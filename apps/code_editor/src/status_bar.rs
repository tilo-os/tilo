//! The bottom status bar: language, caret position (line:col), selection size,
//! indentation, problem count, and a branch-ish project label. Drawn directly
//! into the scene as a single accent-tinted strip, the way an IDE status line
//! reads.

use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Fill;
use winit::event::WindowEvent;

pub const STATUS_HEIGHT: f64 = 26.0;

/// Snapshot of everything the status bar shows. The app refreshes this each
/// frame; the bar diffs to avoid needless redraws.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatusState {
    pub language: String,
    pub line: usize,
    pub col: usize,
    pub selection: usize,
    pub caret_count: usize,
    pub indent: String,
    pub problems: usize,
    pub project: String,
    /// What the language server is doing — "rust-analyzer: indexing 42%".
    /// Empty when there is no server.
    pub lsp: String,
    /// True while the server is starting or busy, which draws the activity dot.
    pub lsp_working: bool,
    /// The last file-operation message ("Saved main.rs", "Could not save …").
    pub notice: String,
    pub notice_is_error: bool,
    /// True when the active buffer has unsaved edits.
    pub dirty: bool,
}

pub struct StatusBar {
    rect: Rect,
    state: StatusState,
    dirty: bool,
}

impl StatusBar {
    pub fn new() -> Self {
        StatusBar {
            rect: Rect::ZERO,
            state: StatusState::default(),
            dirty: true,
        }
    }

    pub fn set_state(&mut self, s: StatusState) {
        if s != self.state {
            self.state = s;
            self.dirty = true;
        }
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
                    height: Dimension::length(STATUS_HEIGHT as f32),
                },
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
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
        let c = &theme.tokens.colors;
        // Accent-tinted bar. Use accent[3] (darker indigo) so light foreground
        // text reads against it across themes.
        scene.fill(Fill::NonZero, transform, c.accent[3], None, &self.rect);

        let fg = vello::peniko::Color::WHITE;
        let y = self.rect.y0 + 5.0;

        // Left cluster: project name.
        let mut x = self.rect.x0 + 12.0;
        if !self.state.project.is_empty() {
            let label = format!("\u{2387} {}", self.state.project);
            text_renderer.draw_text_weighted(
                scene,
                &label,
                12.0,
                fg,
                transform * Affine::translate((x, y)),
                None,
                TextWeight::Medium,
            );
            let (w, _) = text_renderer.measure(&label, 12.0, None);
            x += w as f64 + 18.0;
        }

        // Problems indicator.
        let problems = if self.state.problems == 0 {
            "\u{2713} No problems".to_string()
        } else {
            format!("\u{26A0} {} problems", self.state.problems)
        };
        text_renderer.draw_text(
            scene,
            &problems,
            12.0,
            fg,
            transform * Affine::translate((x, y)),
            None,
        );
        let (pw, _) = text_renderer.measure(&problems, 12.0, None);
        x += pw as f64 + 18.0;

        // Language-server state. This is the difference between "the editor is
        // thinking" and "the editor is broken" during a multi-minute index, so
        // it sits in the left cluster where it is read, not tucked away.
        if !self.state.lsp.is_empty() {
            if self.state.lsp_working {
                // A small filled dot as the activity mark. The bar redraws only
                // while the server reports progress, so this does not spin the
                // frame loop when nothing is happening.
                let dot = Rect::from_center_size(
                    Point::new(x + 4.0, self.rect.center().y),
                    (6.0, 6.0),
                );
                scene.fill(
                    Fill::NonZero,
                    transform,
                    fg.multiply_alpha(0.85),
                    None,
                    &vello::kurbo::Circle::new(dot.center(), 3.0),
                );
                x += 14.0;
            }
            text_renderer.draw_text(
                scene,
                &self.state.lsp,
                12.0,
                fg.multiply_alpha(0.9),
                transform * Affine::translate((x, y)),
                None,
            );
            let (w, _) = text_renderer.measure(&self.state.lsp, 12.0, None);
            x += w as f64 + 18.0;
        }

        // The last file-operation message, so a save is visibly a save and a
        // failed one is visibly a failure.
        if !self.state.notice.is_empty() {
            let color = if self.state.notice_is_error {
                c.semantic.danger
            } else {
                fg.multiply_alpha(0.9)
            };
            text_renderer.draw_text(
                scene,
                &self.state.notice,
                12.0,
                color,
                transform * Affine::translate((x, y)),
                Some((self.rect.width() * 0.4) as f32),
            );
        }

        // Right cluster: assembled and right-aligned.
        let mut right: Vec<String> = Vec::new();
        if self.state.dirty {
            right.push("\u{25CF} Unsaved".to_string());
        }
        if self.state.selection > 0 {
            right.push(format!("{} selected", self.state.selection));
        }
        if self.state.caret_count > 1 {
            right.push(format!("{} cursors", self.state.caret_count));
        }
        right.push(format!("Ln {}, Col {}", self.state.line, self.state.col));
        if !self.state.indent.is_empty() {
            right.push(self.state.indent.clone());
        }
        if !self.state.language.is_empty() {
            right.push(self.state.language.clone());
        }

        // Lay the right cluster out from the right edge leftward.
        let mut rx = self.rect.x1 - 12.0;
        for seg in right.iter() {
            let (w, _) = text_renderer.measure(seg, 12.0, None);
            rx -= w as f64;
            text_renderer.draw_text(
                scene,
                seg,
                12.0,
                fg,
                transform * Affine::translate((rx, y)),
                None,
            );
            rx -= 18.0;
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
