//! `PageJumpPalette` — Cmd-K / Ctrl-K fullscreen-ish page jump
//! affordance used by the notes app.
//!
//! Conceptually a thinner cousin of [`crate::widget::CommandPalette`]
//! — the CommandPalette has a concept-locked filter strip + footer
//! row that doesn't fit the notes app's "just jump to a page" mental
//! model. PageJumpPalette ships only the input row + the filtered
//! result list, sized to look like a fullscreen-ish dialog when
//! anchored at a generous offset from the window's top-center.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, Style, TaffyTree};
use uuid::Uuid;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::link_popover::PageEntry;
use crate::widget::Widget;

const WIDTH: f64 = 480.0;
const ROW_HEIGHT: f64 = 36.0;
const INPUT_HEIGHT: f64 = 44.0;
const PADDING: f64 = 12.0;
const MAX_VISIBLE_ROWS: usize = 10;

pub struct PageJumpPalette {
    items: Vec<PageEntry>,
    visible: Vec<usize>,
    cursor: usize,
    query: String,
    visible_flag: bool,
    /// Top-left anchor in logical coords. Set by the host when
    /// opening the palette — typically `(window_width/2 - WIDTH/2,
    /// 80.0)` for a fullscreen-ish appearance.
    anchor: Point,
    pending: Option<PageEntry>,
}

impl Default for PageJumpPalette {
    fn default() -> Self {
        Self::new()
    }
}

impl PageJumpPalette {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            visible: Vec::new(),
            cursor: 0,
            query: String::new(),
            visible_flag: false,
            anchor: Point::ZERO,
            pending: None,
        }
    }

    pub fn set_items(&mut self, items: Vec<PageEntry>) {
        self.items = items;
        self.rebuild_visible();
    }

    pub fn set_visible(&mut self, visible: bool) {
        self.visible_flag = visible;
        if !visible {
            self.query.clear();
            self.cursor = 0;
            self.rebuild_visible();
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible_flag
    }

    pub fn set_anchor(&mut self, p: Point) {
        self.anchor = p;
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn visible_len(&self) -> usize {
        self.visible.len()
    }

    pub fn highlighted(&self) -> Option<&PageEntry> {
        self.visible.get(self.cursor).and_then(|&i| self.items.get(i))
    }

    pub fn take_selection(&mut self) -> Option<PageEntry> {
        self.pending.take()
    }

    pub fn width(&self) -> f64 {
        WIDTH
    }

    pub fn height(&self) -> f64 {
        let rows = self.visible.len().min(MAX_VISIBLE_ROWS).max(1) as f64;
        PADDING * 2.0 + INPUT_HEIGHT + PADDING + ROW_HEIGHT * rows
    }

    pub fn rect(&self) -> Rect {
        Rect::new(
            self.anchor.x,
            self.anchor.y,
            self.anchor.x + self.width(),
            self.anchor.y + self.height(),
        )
    }

    fn rebuild_visible(&mut self) {
        let needle = self.query.to_lowercase();
        self.visible.clear();
        for (i, e) in self.items.iter().enumerate() {
            if needle.is_empty() || e.title.to_lowercase().contains(&needle) {
                self.visible.push(i);
            }
        }
        if self.cursor >= self.visible.len() {
            self.cursor = self.visible.len().saturating_sub(1);
        }
    }

    /// Handle a key event when the palette is visible. Returns true
    /// if consumed.
    pub fn handle_key(&mut self, event: &WindowEvent) -> bool {
        if !self.visible_flag {
            return false;
        }
        let WindowEvent::KeyboardInput {
            event: KeyEvent { logical_key, state: ElementState::Pressed, text, .. },
            ..
        } = event
        else {
            return false;
        };
        match logical_key {
            Key::Named(NamedKey::ArrowDown) => {
                if !self.visible.is_empty() {
                    self.cursor = (self.cursor + 1).min(self.visible.len() - 1);
                }
                true
            }
            Key::Named(NamedKey::ArrowUp) => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                }
                true
            }
            Key::Named(NamedKey::Enter) => {
                if let Some(&idx) = self.visible.get(self.cursor) {
                    self.pending = Some(self.items[idx].clone());
                }
                self.set_visible(false);
                true
            }
            Key::Named(NamedKey::Escape) => {
                self.set_visible(false);
                true
            }
            Key::Named(NamedKey::Backspace) => {
                if self.query.pop().is_some() {
                    self.rebuild_visible();
                }
                true
            }
            Key::Named(NamedKey::Space) => {
                self.query.push(' ');
                self.rebuild_visible();
                true
            }
            Key::Character(_) => {
                let mut pushed = false;
                if let Some(t) = text {
                    for c in t.chars() {
                        if !c.is_control() {
                            self.query.push(c);
                            pushed = true;
                        }
                    }
                }
                if pushed {
                    self.rebuild_visible();
                }
                pushed
            }
            _ => false,
        }
    }

    pub fn draw_overlay(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if !self.visible_flag {
            return;
        }
        let tokens = &theme.tokens;
        let rect = self.rect();
        let shape = RoundedRect::from_rect(rect, 12.0);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            tokens.colors.surfaces.raised,
            None,
            &shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            tokens.colors.border_standard,
            None,
            &shape,
        );

        // Input row.
        let input_rect = Rect::new(
            rect.x0 + PADDING,
            rect.y0 + PADDING,
            rect.x1 - PADDING,
            rect.y0 + PADDING + INPUT_HEIGHT,
        );
        let input_shape =
            RoundedRect::from_rect(input_rect, theme.tokens.radii.md as f64);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            tokens.colors.surfaces.base,
            None,
            &input_shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            tokens.colors.border_focus,
            None,
            &input_shape,
        );

        let text_size = tokens.typography.scale.base;
        let placeholder = "Jump to page…";
        let display = if self.query.is_empty() { placeholder } else { &self.query };
        let text_color = if self.query.is_empty() {
            tokens.colors.text_dim
        } else {
            tokens.colors.text_primary
        };
        let ty =
            input_rect.y0 + (input_rect.height() - text_size as f64) / 2.0;
        text_renderer.draw_text(
            scene,
            display,
            text_size,
            text_color,
            transform * Affine::translate((input_rect.x0 + 12.0, ty)),
            None,
        );

        // Results list.
        let list_top = input_rect.y1 + PADDING;
        let row_size = tokens.typography.scale.sm;
        if self.visible.is_empty() {
            let label = "No pages match";
            let y = list_top + (ROW_HEIGHT - row_size as f64) / 2.0;
            text_renderer.draw_text(
                scene,
                label,
                row_size,
                tokens.colors.text_dim,
                transform * Affine::translate((rect.x0 + PADDING + 12.0, y)),
                None,
            );
            return;
        }

        for (i, &idx) in self.visible.iter().take(MAX_VISIBLE_ROWS).enumerate() {
            let row = &self.items[idx];
            let y0 = list_top + i as f64 * ROW_HEIGHT;
            let row_rect = Rect::new(
                rect.x0 + PADDING,
                y0,
                rect.x1 - PADDING,
                y0 + ROW_HEIGHT,
            );
            if i == self.cursor {
                let hl =
                    RoundedRect::from_rect(row_rect, theme.tokens.radii.sm as f64);
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    tokens.colors.accent[5],
                    None,
                    &hl,
                );
            }
            let ty =
                row_rect.y0 + (row_rect.height() - row_size as f64) / 2.0;
            text_renderer.draw_text(
                scene,
                &row.title,
                row_size,
                tokens.colors.text_primary,
                transform * Affine::translate((row_rect.x0 + 12.0, ty)),
                None,
            );
        }
    }

    /// Static page-id getter for tests that don't want to fish
    /// through `take_selection`.
    pub fn highlighted_id(&self) -> Option<Uuid> {
        self.highlighted().map(|e| e.id)
    }
}

impl Widget for PageJumpPalette {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.new_leaf(Style::default()).unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }
    fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, _origin: Point) {}
    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
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
        "PageJumpPalette"
    }
    fn test_id(&self) -> Option<&str> {
        Some("page_jump_palette")
    }
    fn get_rect(&self) -> Rect {
        self.rect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::event::{ElementState, KeyEvent};
    use winit::keyboard::{KeyLocation, PhysicalKey, SmolStr};

    fn key_press(logical_key: Key, text: Option<&str>) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = logical_key;
        ev.physical_key = PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = text.map(SmolStr::new);
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }

    fn char_press(c: &str) -> WindowEvent {
        key_press(Key::Character(SmolStr::new(c)), Some(c))
    }

    fn seed_two() -> PageJumpPalette {
        let mut p = PageJumpPalette::new();
        p.set_items(vec![
            PageEntry { id: Uuid::from_u128(1), title: "Welcome".into() },
            PageEntry { id: Uuid::from_u128(2), title: "Roadmap".into() },
        ]);
        p
    }

    #[test]
    fn new_starts_empty_and_hidden() {
        let p = PageJumpPalette::new();
        assert!(!p.is_visible());
        assert_eq!(p.visible_len(), 0);
    }

    #[test]
    fn typing_filters() {
        let mut p = seed_two();
        p.set_visible(true);
        p.handle_key(&char_press("r"));
        assert_eq!(p.query(), "r");
        assert_eq!(p.visible_len(), 1);
        assert_eq!(p.highlighted().unwrap().title, "Roadmap");
    }

    #[test]
    fn enter_commits_and_closes() {
        let mut p = seed_two();
        p.set_visible(true);
        p.handle_key(&key_press(Key::Named(NamedKey::Enter), None));
        let chosen = p.take_selection().unwrap();
        assert_eq!(chosen.title, "Welcome");
        assert!(!p.is_visible());
    }

    #[test]
    fn escape_hides_without_committing() {
        let mut p = seed_two();
        p.set_visible(true);
        p.handle_key(&key_press(Key::Named(NamedKey::Escape), None));
        assert!(!p.is_visible());
        assert!(p.take_selection().is_none());
    }

    #[test]
    fn arrow_keys_move_cursor() {
        let mut p = seed_two();
        p.set_visible(true);
        assert_eq!(p.highlighted().unwrap().title, "Welcome");
        p.handle_key(&key_press(Key::Named(NamedKey::ArrowDown), None));
        assert_eq!(p.highlighted().unwrap().title, "Roadmap");
        p.handle_key(&key_press(Key::Named(NamedKey::ArrowUp), None));
        assert_eq!(p.highlighted().unwrap().title, "Welcome");
    }
}
