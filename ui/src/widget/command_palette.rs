//! `CommandPalette` primitive — a fuzzy-matching searchable command list.
//!
//! Spec: `docs/superpowers/specs/2026-04-18-nirios-shell-design.md` — Phase A
//! step 9 (`CommandPalette` primitive + `GlobalSearch` using it).
//!
//! The palette holds a list of [`CommandEntry`] rows, supports a query string
//! and an optional composable filter predicate, and exposes the currently
//! visible entries via [`CommandPalette::visible_entries`]. Enter invokes the
//! selected entry's `on_select` callback with the CRDT handle and root path,
//! matching `quick-ui`'s direct-mutation event model.

use automerge::{AutoCommit, ObjId};
use std::sync::Arc;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;

type SelectFn = dyn Fn(&mut AutoCommit, &ObjId) + Send + Sync;

/// One row in the palette.
#[derive(Clone)]
pub struct CommandEntry {
    pub id: String,
    pub label: String,
    pub on_select: Option<Arc<SelectFn>>,
}

impl std::fmt::Debug for CommandEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandEntry")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("on_select", &self.on_select.as_ref().map(|_| "<fn>"))
            .finish()
    }
}

impl CommandEntry {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self { id: id.into(), label: label.into(), on_select: None }
    }

    /// Attach a callback invoked when Enter is pressed on this entry.
    pub fn on_select<F>(mut self, f: F) -> Self
    where
        F: Fn(&mut AutoCommit, &ObjId) + Send + Sync + 'static,
    {
        self.on_select = Some(Arc::new(f));
        self
    }
}

/// Subsequence fuzzy score: `Some(match_count - gap_penalty)` when `query`
/// matches `text` as a (case-insensitive) subsequence; `None` otherwise.
/// Empty queries match everything with score `0`.
fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    if query.is_empty() { return Some(0); }
    let q: Vec<char> = query.chars().flat_map(|c| c.to_lowercase()).collect();
    let t: Vec<char> = text.chars().flat_map(|c| c.to_lowercase()).collect();
    let mut qi = 0usize;
    let mut matches = 0i32;
    let mut gaps = 0i32;
    let mut prev_match: Option<usize> = None;
    for (ti, ch) in t.iter().enumerate() {
        if qi < q.len() && *ch == q[qi] {
            if let Some(pm) = prev_match { gaps += (ti - pm - 1) as i32; }
            prev_match = Some(ti);
            qi += 1;
            matches += 1;
            if qi == q.len() { break; }
        }
    }
    if qi < q.len() { None } else { Some(matches - gaps) }
}

pub struct CommandPalette {
    entries: Vec<CommandEntry>,
    query: String,
    filter: Option<Box<dyn Fn(&CommandEntry) -> bool + Send + Sync>>,
    visible: Vec<usize>,
    cursor: usize,
    rect: Rect,
    is_focused: bool,
}

impl Default for CommandPalette {
    fn default() -> Self { Self::new() }
}

impl CommandPalette {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            query: String::new(),
            filter: None,
            visible: Vec::new(),
            cursor: 0,
            rect: Rect::ZERO,
            is_focused: false,
        }
    }

    pub fn push(&mut self, e: CommandEntry) {
        self.entries.push(e);
        self.rebuild();
    }

    pub fn set_query(&mut self, q: &str) {
        self.query = q.into();
        self.rebuild();
    }

    pub fn query(&self) -> &str { &self.query }

    pub fn set_filter(&mut self, f: Box<dyn Fn(&CommandEntry) -> bool + Send + Sync>) {
        self.filter = Some(f);
        self.rebuild();
    }

    pub fn visible_entries(&self) -> Vec<&CommandEntry> {
        self.visible.iter().map(|&i| &self.entries[i]).collect()
    }

    pub fn cursor(&self) -> usize { self.cursor }

    /// Move the cursor to `index` within the visible window. Silently
    /// clamps to the last visible row so overlay callers that do hit-
    /// testing on the post-layout rects don't have to bounds-check.
    pub fn set_cursor(&mut self, index: usize) {
        if self.visible.is_empty() {
            self.cursor = 0;
        } else {
            self.cursor = index.min(self.visible.len() - 1);
        }
    }

    /// Id of the currently-selected visible entry, or `None` if the
    /// filter matched no rows.
    pub fn selected_id(&self) -> Option<&str> {
        self.visible
            .get(self.cursor)
            .map(|&i| self.entries[i].id.as_str())
    }

    /// Rect of the `i`th *visible* row, in the same coordinate space as
    /// `get_rect()`. Returns `None` if `i` is past the end of the
    /// rendered window (the palette caps at 10 rows in `build_layout`).
    pub fn row_rect(&self, i: usize) -> Option<Rect> {
        if i >= self.visible.len() {
            return None;
        }
        let list_top = self.rect.y0 + PADDING + INPUT_HEIGHT + PADDING;
        let y0 = list_top + (i as f64) * ROW_HEIGHT;
        let y1 = y0 + ROW_HEIGHT;
        if y1 > self.rect.y1 - PADDING {
            return None;
        }
        Some(Rect::new(self.rect.x0 + PADDING, y0, self.rect.x1 - PADDING, y1))
    }

    /// Index of the visible row whose rect contains `p`, if any. Used by
    /// overlay callers that accept a click inside the palette rect and
    /// want to move the cursor to the clicked row.
    pub fn row_at(&self, p: Point) -> Option<usize> {
        (0..self.visible.len()).find(|&i| {
            self.row_rect(i).map(|r| r.contains(p)).unwrap_or(false)
        })
    }

    fn rebuild(&mut self) {
        self.visible.clear();
        for (i, e) in self.entries.iter().enumerate() {
            if let Some(f) = &self.filter {
                if !f(e) { continue; }
            }
            if fuzzy_score(&self.query, &e.label).is_none() { continue; }
            self.visible.push(i);
        }
        if self.cursor >= self.visible.len() {
            self.cursor = self.visible.len().saturating_sub(1);
        }
    }
}

const ROW_HEIGHT: f64 = 32.0;
const INPUT_HEIGHT: f64 = 36.0;
const PADDING: f64 = 8.0;

impl Widget for CommandPalette {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let rows = self.visible.len().min(10).max(1) as f32;
        let height = (INPUT_HEIGHT as f32) + rows * (ROW_HEIGHT as f32) + (PADDING as f32) * 2.0;
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::length(360.0),
                    height: Dimension::length(height),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // Popover-shaped surface with a soft border.
        let shape = RoundedRect::from_rect(self.rect, theme.tokens.radii.md as f64);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_standard,
            None,
            &shape,
        );

        // Query input row at the top.
        let input_rect = Rect::new(
            self.rect.x0 + PADDING,
            self.rect.y0 + PADDING,
            self.rect.x1 - PADDING,
            self.rect.y0 + PADDING + INPUT_HEIGHT,
        );
        let input_shape = RoundedRect::from_rect(input_rect, theme.tokens.radii.sm as f64);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &input_shape,
        );
        let border_color = if self.is_focused {
            theme.tokens.colors.border_focus
        } else {
            theme.tokens.colors.border_standard
        };
        scene.stroke(
            &Stroke::new(if self.is_focused { 2.0 } else { 1.0 }),
            transform,
            border_color,
            None,
            &input_shape,
        );
        let display = if self.query.is_empty() { "Search…" } else { &self.query };
        let text_color = if self.query.is_empty() {
            theme.tokens.colors.text_dim
        } else {
            theme.tokens.colors.text_primary
        };
        let text_size = theme.tokens.typography.scale.sm;
        let text_y = input_rect.y0 + (input_rect.height() - text_size as f64) / 2.0;
        text_renderer.draw_text(
            scene,
            display,
            text_size,
            text_color,
            transform * Affine::translate((input_rect.x0 + 8.0, text_y)),
            None,
        );

        // Vertical list of visible entries.
        let list_top = input_rect.y1 + PADDING;
        for (i, &entry_idx) in self.visible.iter().enumerate() {
            let y0 = list_top + (i as f64) * ROW_HEIGHT;
            let y1 = y0 + ROW_HEIGHT;
            if y1 > self.rect.y1 - PADDING {
                break;
            }
            let row_rect = Rect::new(self.rect.x0 + PADDING, y0, self.rect.x1 - PADDING, y1);
            if i == self.cursor {
                let highlight = RoundedRect::from_rect(row_rect, theme.tokens.radii.sm as f64);
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    theme.tokens.colors.neutral[4],
                    None,
                    &highlight,
                );
            }
            let label = &self.entries[entry_idx].label;
            let label_y = row_rect.y0 + (row_rect.height() - text_size as f64) / 2.0;
            text_renderer.draw_text(
                scene,
                label,
                text_size,
                theme.tokens.colors.text_primary,
                transform * Affine::translate((row_rect.x0 + 8.0, label_y)),
                None,
            );
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        _scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        if !self.is_focused { return; }
        if let WindowEvent::KeyboardInput {
            event: KeyEvent { logical_key, state: element_state, .. },
            ..
        } = event
        {
            if *element_state != ElementState::Pressed { return; }
            match logical_key {
                Key::Named(NamedKey::ArrowDown) => {
                    if !self.visible.is_empty() {
                        self.cursor = (self.cursor + 1).min(self.visible.len() - 1);
                        *handled = true;
                    }
                }
                Key::Named(NamedKey::ArrowUp) => {
                    if self.cursor > 0 {
                        self.cursor -= 1;
                        *handled = true;
                    }
                }
                Key::Named(NamedKey::Enter) => {
                    if let Some(&idx) = self.visible.get(self.cursor) {
                        if let Some(cb) = self.entries[idx].on_select.clone() {
                            cb(state, path);
                        }
                        *handled = true;
                    }
                }
                Key::Named(NamedKey::Escape) => {
                    self.is_focused = false;
                    *handled = true;
                }
                _ => {}
            }
        }
    }

    fn name(&self) -> &str { "CommandPalette" }

    fn get_rect(&self) -> Rect { self.rect }

    fn is_focusable(&self) -> bool { true }
    fn set_focus(&mut self, focused: bool) { self.is_focused = focused; }
    fn is_focused(&self) -> bool { self.is_focused }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_score_empty_query_matches_everything() {
        assert_eq!(fuzzy_score("", "anything"), Some(0));
    }

    #[test]
    fn fuzzy_score_subsequence_matches() {
        assert!(fuzzy_score("ren", "Rename Workspace").is_some());
    }

    #[test]
    fn fuzzy_score_non_subsequence_is_none() {
        assert_eq!(fuzzy_score("xyz", "Rename Workspace"), None);
    }

    #[test]
    fn set_cursor_clamps_to_visible_len() {
        let mut p = CommandPalette::new();
        p.push(CommandEntry::new("a", "Alpha"));
        p.push(CommandEntry::new("b", "Bravo"));
        p.set_cursor(99);
        assert_eq!(p.cursor(), 1);
    }

    #[test]
    fn selected_id_reflects_cursor_position() {
        let mut p = CommandPalette::new();
        p.push(CommandEntry::new("a", "Alpha"));
        p.push(CommandEntry::new("b", "Bravo"));
        assert_eq!(p.selected_id(), Some("a"));
        p.set_cursor(1);
        assert_eq!(p.selected_id(), Some("b"));
    }

    #[test]
    fn selected_id_is_none_when_filter_removes_everything() {
        let mut p = CommandPalette::new();
        p.push(CommandEntry::new("a", "Alpha"));
        p.set_filter(Box::new(|_| false));
        assert_eq!(p.selected_id(), None);
    }

    #[test]
    fn row_at_finds_visible_row_inside_the_post_layout_rect() {
        let mut p = CommandPalette::new();
        p.push(CommandEntry::new("a", "Alpha"));
        p.push(CommandEntry::new("b", "Bravo"));
        // Seat the palette rect so row_rect has meaningful coordinates.
        p.rect = Rect::new(0.0, 0.0, 360.0, 200.0);
        let mid_row_1 = p.row_rect(1).unwrap().center();
        assert_eq!(p.row_at(mid_row_1), Some(1));
        // Clicks outside the rect return None.
        assert!(p.row_at(Point::new(1000.0, 1000.0)).is_none());
    }
}
