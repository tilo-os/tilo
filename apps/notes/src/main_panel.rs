//! `MainPanel` — the right-hand pane that displays the currently
//! selected page.
//!
//! Phase 1 shows just the title as a large heading. Phase 2 will
//! replace this with the `BlockEditor` widget for the page body.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use ui::Widget;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;

/// Heading point size. Reads as a "page title" rather than body
/// copy; intentionally well above the 16 px body baseline so the
/// document hierarchy is obvious at a glance.
const HEADING_SIZE: f32 = 32.0;
/// Inner padding around the heading inside the panel rect.
const PADDING_PX: f64 = 32.0;
/// Body color of the empty-state hint ("No page selected"). Slightly
/// dimmer than the heading so it doesn't compete visually.
const HINT_SIZE: f32 = 14.0;

pub struct MainPanel {
    title: Option<String>,
    rect: Rect,
    /// True until the next `mark_drawn`. Flipped by `set_title`
    /// (title changed → redraw needed) and on rect resize.
    dirty: bool,
}

impl MainPanel {
    pub fn new() -> Self {
        Self {
            title: None,
            rect: Rect::ZERO,
            dirty: true,
        }
    }

    /// Update the heading. `None` clears it back to the empty-state hint.
    /// No-op if the value hasn't changed (we don't want to invalidate the
    /// scene cache on every parent sync pass).
    pub fn set_title(&mut self, title: Option<String>) {
        if self.title != title {
            self.title = title;
            self.dirty = true;
        }
    }

    /// Current heading text, for tests.
    #[cfg(test)]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }
}

impl Default for MainPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for MainPanel {
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
                    width: Dimension::auto(),
                    height: Dimension::auto(),
                },
                flex_grow: 1.0,
                flex_basis: Dimension::length(0.0),
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        let new_rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
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

        // Background fill so the panel reads as its own surface, not
        // a transparent gap against the window's clear color.
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            tokens.colors.surfaces.base,
            None,
            &self.rect,
        );

        let inner_x = self.rect.x0 + PADDING_PX;
        let inner_y = self.rect.y0 + PADDING_PX;
        let inner_w = (self.rect.width() - 2.0 * PADDING_PX).max(0.0) as f32;

        match &self.title {
            Some(t) => {
                text_renderer.draw_text(
                    scene,
                    t,
                    HEADING_SIZE,
                    tokens.colors.text_primary,
                    transform * Affine::translate((inner_x, inner_y)),
                    Some(inner_w),
                );
            }
            None => {
                text_renderer.draw_text(
                    scene,
                    "No page selected",
                    HINT_SIZE,
                    tokens.colors.text_dim,
                    transform * Affine::translate((inner_x, inner_y)),
                    Some(inner_w),
                );
            }
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
        "MainPanel"
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_title_marks_dirty_when_changed() {
        let mut p = MainPanel::new();
        p.mark_drawn();
        assert!(!p.needs_redraw());
        p.set_title(Some("Hello".into()));
        assert!(p.needs_redraw());
        p.mark_drawn();
        // Same value → no new dirty.
        p.set_title(Some("Hello".into()));
        assert!(!p.needs_redraw());
    }
}
