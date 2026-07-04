//! `StatusBar` — single-row strip pinned to the bottom of the window.
//!
//! Three slots, left-to-right:
//!
//! - **Left**: save indicator. "Saved" (text_dim) when no writes
//!   pending, "Saving…" (accent_primary) during the debounce window,
//!   "Save failed" (semantic.danger) if the most recent save errored.
//! - **Center**: the current page's breadcrumb path, e.g. `Notes / Welcome`.
//! - **Right**: page count + word count of the selected page, e.g.
//!   `4 pages · 142 words`.
//!
//! The widget owns no CRDT state — `NotesApp` pushes a fresh
//! [`StatusBarState`] every frame via [`StatusBar::set_state`]. Holding
//! the state in a plain struct (vs. polling pointers into the workspace)
//! keeps the rendering deterministic and makes unit tests trivial.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Color, Fill};
use winit::event::WindowEvent;

use ui::Widget;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;

use crate::storage::SaveStatus;

/// Height of the status-bar strip in logical px. Sits between
/// "compact enough not to steal vertical room from the editor" and
/// "tall enough that the text breathes". 26 px reads as one Inter
/// 12-pt line plus 7 px top/bottom inset.
pub const STATUS_BAR_HEIGHT: f64 = 26.0;

/// Snapshot pushed in by `NotesApp` every frame. Cheap to clone
/// (small strings, two numbers, one enum).
#[derive(Clone, Debug, Default)]
pub struct StatusBarState {
    /// Combined save-state — the worse of (workspace saver, page saver)
    /// so the UI surfaces the most actionable signal.
    pub save_status: SaveStatus,
    /// Breadcrumb path of the selected page, e.g. `Notes / Welcome`.
    /// Empty when nothing is selected.
    pub page_path: String,
    /// Total number of pages in the workspace.
    pub page_count: usize,
    /// Word count of the currently-selected page's body. None when
    /// nothing is selected.
    pub word_count: Option<usize>,
}

pub struct StatusBar {
    state: StatusBarState,
    rect: Rect,
    dirty: bool,
}

impl StatusBar {
    pub fn new() -> Self {
        Self {
            state: StatusBarState::default(),
            rect: Rect::ZERO,
            dirty: true,
        }
    }

    /// Replace the snapshot. Diff vs. the previous state to avoid
    /// flagging dirty when nothing observable changed — keeps the
    /// incremental redraw machinery happy.
    pub fn set_state(&mut self, new: StatusBarState) {
        let changed = self.state.save_status != new.save_status
            || self.state.page_path != new.page_path
            || self.state.page_count != new.page_count
            || self.state.word_count != new.word_count;
        if changed {
            self.state = new;
            self.dirty = true;
        }
    }

    /// Read accessor for tests.
    pub fn state(&self) -> &StatusBarState {
        &self.state
    }

    fn save_text(&self) -> &'static str {
        match self.state.save_status {
            SaveStatus::Saved => "Saved",
            SaveStatus::Saving => "Saving…",
            SaveStatus::Failed => "Save failed",
        }
    }

    fn save_color(&self, theme: &Theme) -> Color {
        match self.state.save_status {
            SaveStatus::Saved => theme.tokens.colors.text_dim,
            SaveStatus::Saving => theme.accent_primary,
            SaveStatus::Failed => theme.tokens.colors.semantic.danger,
        }
    }

    fn right_text(&self) -> String {
        let pages = match self.state.page_count {
            1 => "1 page".to_string(),
            n => format!("{} pages", n),
        };
        match self.state.word_count {
            Some(w) => {
                let words = match w {
                    1 => "1 word".to_string(),
                    _ => format!("{} words", w),
                };
                format!("{} · {}", pages, words)
            }
            None => pages,
        }
    }
}

impl Default for StatusBar {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for StatusBar {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(STATUS_BAR_HEIGHT as f32),
                },
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        let new_rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
        if new_rect.width() != self.rect.width() || new_rect.height() != self.rect.height() {
            self.dirty = true;
        }
        self.rect = new_rect;
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let tokens = &theme.tokens;

        // Filled strip background — one notch raised from the editor
        // canvas so the divider reads as a horizontal rule without
        // needing an extra stroke.
        scene.fill(
            Fill::NonZero,
            transform,
            tokens.colors.surfaces.raised,
            None,
            &self.rect,
        );
        // Top hairline — one logical px of border_standard to draw the
        // eye to the seam between content and status bar. Cheaper than
        // a full stroke around the rect and matches typical IDE chrome.
        let hairline = Rect::new(self.rect.x0, self.rect.y0, self.rect.x1, self.rect.y0 + 1.0);
        scene.fill(
            Fill::NonZero,
            transform,
            tokens.colors.border_standard,
            None,
            &hairline,
        );

        // Common typography: 12 pt small text size from the token scale.
        let size = tokens.typography.scale.sm;
        let pad_x = tokens.spacing.s3 as f64;
        let text_y = self.rect.y0 + (self.rect.height() - size as f64) / 2.0 + 1.0;

        // ---- Left: save indicator ----
        let save_label = self.save_text();
        let save_color = self.save_color(theme);
        text_renderer.draw_text(
            scene,
            save_label,
            size,
            save_color,
            transform * Affine::translate((self.rect.x0 + pad_x, text_y)),
            None,
        );

        // ---- Right: page count · word count ----
        let right = self.right_text();
        let (right_w, _) = text_renderer.measure(&right, size, None);
        let right_x = self.rect.x1 - pad_x - right_w as f64;
        text_renderer.draw_text(
            scene,
            &right,
            size,
            tokens.colors.text_dim,
            transform * Affine::translate((right_x, text_y)),
            None,
        );

        // ---- Center: page breadcrumb ----
        if !self.state.page_path.is_empty() {
            let (center_w, _) = text_renderer.measure(&self.state.page_path, size, None);
            let center_x = self.rect.x0 + (self.rect.width() - center_w as f64) / 2.0;
            text_renderer.draw_text(
                scene,
                &self.state.page_path,
                size,
                tokens.colors.text_primary,
                transform * Affine::translate((center_x, text_y)),
                None,
            );
        }
    }

    fn handle_event(
        &mut self,
        _event: &WindowEvent,
        _scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        _handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        // Status bar is purely informational — no interactions in v0.1.
    }

    fn name(&self) -> &str {
        "StatusBar"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
}

/// Compute a word count over a markdown body. Used by `NotesApp` to
/// fill the right-hand status-bar slot for the selected page. We
/// intentionally count via [`str::split_whitespace`] so the result
/// matches what a user thinks of as "words" (consecutive non-whitespace
/// runs) rather than what a markdown tokenizer would say (which would
/// double-count things like `**bold**` as separate tokens).
pub fn count_words(body: &str) -> usize {
    body.split_whitespace().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_indicator_text_per_status() {
        let mut bar = StatusBar::new();
        bar.set_state(StatusBarState {
            save_status: SaveStatus::Saved,
            ..Default::default()
        });
        assert_eq!(bar.save_text(), "Saved");
        bar.set_state(StatusBarState {
            save_status: SaveStatus::Saving,
            ..Default::default()
        });
        assert_eq!(bar.save_text(), "Saving…");
        bar.set_state(StatusBarState {
            save_status: SaveStatus::Failed,
            ..Default::default()
        });
        assert_eq!(bar.save_text(), "Save failed");
    }

    #[test]
    fn right_text_singularizes_page_and_word() {
        let mut bar = StatusBar::new();
        bar.set_state(StatusBarState {
            save_status: SaveStatus::Saved,
            page_path: "Welcome".into(),
            page_count: 1,
            word_count: Some(1),
        });
        assert_eq!(bar.right_text(), "1 page · 1 word");
        bar.set_state(StatusBarState {
            save_status: SaveStatus::Saved,
            page_path: "Welcome".into(),
            page_count: 4,
            word_count: Some(142),
        });
        assert_eq!(bar.right_text(), "4 pages · 142 words");
    }

    #[test]
    fn right_text_drops_word_count_when_none() {
        let mut bar = StatusBar::new();
        bar.set_state(StatusBarState {
            save_status: SaveStatus::Saved,
            page_path: String::new(),
            page_count: 3,
            word_count: None,
        });
        assert_eq!(bar.right_text(), "3 pages");
    }

    #[test]
    fn count_words_handles_punctuation_and_marks() {
        // "**bold** italic and `code`" is 4 word-tokens by
        // whitespace splitting. The status bar uses this so its
        // count matches a human's mental model regardless of inline
        // markdown markers (the markers count as part of the
        // surrounding word).
        assert_eq!(count_words("**bold** italic and `code`"), 4);
        assert_eq!(count_words(""), 0);
        assert_eq!(count_words("   "), 0);
        assert_eq!(count_words("one\ntwo\tthree"), 3);
    }

    #[test]
    fn set_state_no_op_when_identical_does_not_dirty() {
        let mut bar = StatusBar::new();
        bar.set_state(StatusBarState {
            save_status: SaveStatus::Saved,
            page_path: "X".into(),
            page_count: 1,
            word_count: Some(0),
        });
        bar.mark_drawn();
        assert!(!bar.needs_redraw());
        bar.set_state(StatusBarState {
            save_status: SaveStatus::Saved,
            page_path: "X".into(),
            page_count: 1,
            word_count: Some(0),
        });
        assert!(!bar.needs_redraw(), "identical state must not flag dirty");
    }
}
