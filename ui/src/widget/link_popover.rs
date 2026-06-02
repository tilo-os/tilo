//! `LinkPopover` — fuzzy-search popover for `[[Page Title]]` inline
//! page links. Same shape as [`crate::widget::SlashMenu`] but
//! seeded with caller-provided page titles instead of a hard-coded
//! block-kind list.
//!
//! Spec: Phase 4 of the notes app — see
//! `docs/superpowers/plans/2026-06-02-notes-app-mvp-roadmap.md`.
//!
//! ## Why a thin sibling, not a generic `Popover<Item>`?
//!
//! The slash menu's API is shaped around block-kind conversions;
//! folding the page-title flow into the same trait object would mean
//! either generic-ifying every field (and paying for the boxing) or
//! collapsing `BlockKind` and `PageEntry` behind an associated type.
//! Phase 4's scope is "ship the marks demo with a working palette",
//! not "design a generic popover primitive" — the duplication is
//! ~150 LOC and a separate widget keeps the test surface small.

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
use crate::widget::Widget;

/// One row in the popover. Carries a stable id and the page title to
/// display + filter on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageEntry {
    pub id: Uuid,
    pub title: String,
}

/// Width of the popover in CSS-like pixels.
const MENU_WIDTH: f64 = 220.0;
const ROW_HEIGHT: f64 = 26.0;
const PADDING_Y: f64 = 6.0;
const PADDING_X: f64 = 10.0;
/// Cap on rows rendered at once — keeps the popover small for big
/// workspaces; the user can keep typing to narrow the result.
const MAX_VISIBLE_ROWS: usize = 8;

pub struct LinkPopover {
    /// All known pages (set via `set_items`).
    items: Vec<PageEntry>,
    /// Indices into `items` that survive the current filter.
    visible: Vec<usize>,
    /// Cursor into `visible`. Always clamped.
    cursor: usize,
    filter: String,
    visible_flag: bool,
    anchor: Point,
    /// One-shot slot: when the user picks an entry, the chosen
    /// `(PageEntry)` lands here.
    pending: Option<PageEntry>,
}

impl Default for LinkPopover {
    fn default() -> Self {
        Self::new()
    }
}

impl LinkPopover {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            visible: Vec::new(),
            cursor: 0,
            filter: String::new(),
            visible_flag: false,
            anchor: Point::ZERO,
            pending: None,
        }
    }

    pub fn set_items(&mut self, items: Vec<PageEntry>) {
        self.items = items;
        self.rebuild_visible();
    }

    pub fn items(&self) -> &[PageEntry] {
        &self.items
    }

    pub fn set_visible(&mut self, visible: bool) {
        self.visible_flag = visible;
        if !visible {
            self.filter.clear();
            self.cursor = 0;
            self.rebuild_visible();
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible_flag
    }

    pub fn set_anchor(&mut self, anchor: Point) {
        self.anchor = anchor;
    }

    pub fn set_filter(&mut self, filter: &str) {
        self.filter = filter.to_string();
        self.cursor = 0;
        self.rebuild_visible();
    }

    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// Drain the pending selection from the most-recent Enter press.
    pub fn take_selection(&mut self) -> Option<PageEntry> {
        self.pending.take()
    }

    pub fn highlighted(&self) -> Option<&PageEntry> {
        self.visible.get(self.cursor).and_then(|&i| self.items.get(i))
    }

    pub fn visible_len(&self) -> usize {
        self.visible.len()
    }

    pub fn width(&self) -> f64 {
        MENU_WIDTH
    }

    pub fn height(&self) -> f64 {
        PADDING_Y * 2.0
            + ROW_HEIGHT * self.visible.len().min(MAX_VISIBLE_ROWS).max(1) as f64
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
        let needle = self.filter.to_lowercase();
        self.visible.clear();
        for (i, e) in self.items.iter().enumerate() {
            let matches = needle.is_empty()
                || e.title.to_lowercase().contains(&needle);
            if matches {
                self.visible.push(i);
            }
        }
        if self.cursor >= self.visible.len() {
            self.cursor = self.visible.len().saturating_sub(1);
        }
    }

    /// Handle a key event. Returns true if consumed.
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
                if self.filter.pop().is_some() {
                    self.rebuild_visible();
                    true
                } else {
                    self.set_visible(false);
                    // Return false so the parent can handle the
                    // delete-of-the-`[[` glyphs.
                    false
                }
            }
            Key::Named(NamedKey::Space) => {
                self.filter.push(' ');
                self.rebuild_visible();
                true
            }
            Key::Character(_) => {
                let mut pushed = false;
                if let Some(t) = text {
                    for c in t.chars() {
                        if !c.is_control() {
                            self.filter.push(c);
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
        let shape = RoundedRect::from_rect(rect, 6.0);
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

        let font_size = tokens.typography.scale.sm;
        // Empty-state hint.
        if self.visible.is_empty() {
            let y = rect.y0 + PADDING_Y + (ROW_HEIGHT - font_size as f64) / 2.0;
            text_renderer.draw_text(
                scene,
                "No matches",
                font_size,
                tokens.colors.text_dim,
                transform * Affine::translate((rect.x0 + PADDING_X, y)),
                None,
            );
            return;
        }
        for (i, &idx) in self.visible.iter().take(MAX_VISIBLE_ROWS).enumerate() {
            let row = &self.items[idx];
            let y0 = rect.y0 + PADDING_Y + i as f64 * ROW_HEIGHT;
            let row_rect = Rect::new(
                rect.x0 + 4.0,
                y0,
                rect.x1 - 4.0,
                y0 + ROW_HEIGHT,
            );
            if i == self.cursor {
                let hl = RoundedRect::from_rect(row_rect, 4.0);
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    tokens.colors.accent[5],
                    None,
                    &hl,
                );
            }
            let text_y = row_rect.y0
                + (row_rect.height() - font_size as f64) / 2.0;
            text_renderer.draw_text(
                scene,
                &row.title,
                font_size,
                tokens.colors.text_primary,
                transform * Affine::translate((row_rect.x0 + PADDING_X, text_y)),
                None,
            );
        }
    }
}

// `Widget` impl for compatibility with the `Widget`-trait surface;
// LinkPopover lives in overlay space so most methods no-op.
impl Widget for LinkPopover {
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
        "LinkPopover"
    }
    fn test_id(&self) -> Option<&str> {
        Some("link_popover")
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

    fn seed_three() -> LinkPopover {
        let mut p = LinkPopover::new();
        p.set_items(vec![
            PageEntry { id: Uuid::from_u128(1), title: "Welcome".into() },
            PageEntry { id: Uuid::from_u128(2), title: "Roadmap".into() },
            PageEntry { id: Uuid::from_u128(3), title: "Inbox".into() },
        ]);
        p
    }

    #[test]
    fn new_starts_empty_and_hidden() {
        let p = LinkPopover::new();
        assert!(!p.is_visible());
        assert!(p.items().is_empty());
    }

    #[test]
    fn set_items_seeds_visible_list() {
        let p = seed_three();
        assert_eq!(p.visible_len(), 3);
    }

    #[test]
    fn filter_narrows_results() {
        let mut p = seed_three();
        p.set_filter("wel");
        assert_eq!(p.visible_len(), 1);
        assert_eq!(p.highlighted().unwrap().title, "Welcome");
    }

    #[test]
    fn arrow_keys_move_cursor() {
        let mut p = seed_three();
        p.set_visible(true);
        assert_eq!(p.highlighted().unwrap().title, "Welcome");
        p.handle_key(&key_press(Key::Named(NamedKey::ArrowDown), None));
        assert_eq!(p.highlighted().unwrap().title, "Roadmap");
        p.handle_key(&key_press(Key::Named(NamedKey::ArrowUp), None));
        assert_eq!(p.highlighted().unwrap().title, "Welcome");
    }

    #[test]
    fn enter_commits_selection_and_hides() {
        let mut p = seed_three();
        p.set_visible(true);
        p.handle_key(&key_press(Key::Named(NamedKey::Enter), None));
        let chosen = p.take_selection().unwrap();
        assert_eq!(chosen.title, "Welcome");
        assert!(!p.is_visible());
    }

    #[test]
    fn escape_hides_without_committing() {
        let mut p = seed_three();
        p.set_visible(true);
        p.handle_key(&key_press(Key::Named(NamedKey::Escape), None));
        assert!(!p.is_visible());
        assert!(p.take_selection().is_none());
    }

    #[test]
    fn typing_pushes_filter() {
        let mut p = seed_three();
        p.set_visible(true);
        p.handle_key(&char_press("r"));
        assert_eq!(p.filter(), "r");
        assert_eq!(p.visible_len(), 1);
        assert_eq!(p.highlighted().unwrap().title, "Roadmap");
    }

    #[test]
    fn backspace_pops_filter_then_closes() {
        let mut p = seed_three();
        p.set_visible(true);
        p.set_filter("r");
        let consumed = p.handle_key(&key_press(Key::Named(NamedKey::Backspace), None));
        assert!(consumed);
        assert_eq!(p.filter(), "");
        // Another backspace with empty filter closes.
        let consumed = p.handle_key(&key_press(Key::Named(NamedKey::Backspace), None));
        assert!(!consumed);
        assert!(!p.is_visible());
    }

    #[test]
    fn empty_results_renders_no_matches_hint() {
        // We don't have a real renderer here, but we can at least
        // sanity-check the state: filter that matches nothing leaves
        // visible_len 0 and is_visible true.
        let mut p = seed_three();
        p.set_visible(true);
        p.set_filter("zzz");
        assert_eq!(p.visible_len(), 0);
        assert!(p.highlighted().is_none());
    }
}
