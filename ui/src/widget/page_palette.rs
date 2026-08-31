//! `PageJumpPalette` — Cmd-K / Ctrl-K fullscreen-ish page jump
//! affordance used by the notes app.
//!
//! Conceptually a thinner cousin of [`crate::widget::CommandPalette`]
//! — the `CommandPalette` has a concept-locked filter strip + footer
//! row that doesn't fit the notes app's "just jump to a page" mental
//! model. `PageJumpPalette` ships only the input row + the filtered
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

use crate::color_compat::ColorCompat;
use crate::presence::PresenceCtx;
use crate::query_text::{QueryComposition, QueryEventChange};
use crate::text_render::TextRenderer;
use crate::text_selection::Selection;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::link_popover::PageEntry;

const WIDTH: f64 = 480.0;
const ROW_HEIGHT: f64 = 36.0;
const INPUT_HEIGHT: f64 = 44.0;
const PADDING: f64 = 12.0;
const MAX_VISIBLE_ROWS: usize = 10;

/// Narrows a visible-row count/slot index (always `<= MAX_VISIBLE_ROWS`)
/// to `f64` for layout math. Bounded far below `f64`'s precision limit,
/// so this never loses precision in practice.
#[allow(
    clippy::cast_precision_loss,
    reason = "row counts/slots are bounded to MAX_VISIBLE_ROWS (10)"
)]
fn row_f64(v: usize) -> f64 {
    v as f64
}

/// Composes `base` with a translation for text placement. Affine
/// composition is floating-point matrix multiply — it cannot overflow or
/// panic the way integer arithmetic can.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine `*` composes floating-point transform matrices, not integer arithmetic"
)]
fn translated(base: Affine, dx: f64, dy: f64) -> Affine {
    base * Affine::translate((dx, dy))
}

pub struct PageJumpPalette {
    items: Vec<PageEntry>,
    visible: Vec<usize>,
    cursor: usize,
    query: String,
    query_selection: Selection,
    query_composition: QueryComposition,
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
            query_selection: Selection::collapsed(0),
            query_composition: QueryComposition::new(),
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
            self.query_selection = Selection::collapsed(0);
            self.query_composition.clear();
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
        self.visible
            .get(self.cursor)
            .and_then(|&i| self.items.get(i))
    }

    pub fn take_selection(&mut self) -> Option<PageEntry> {
        self.pending.take()
    }

    pub fn width(&self) -> f64 {
        WIDTH
    }

    pub fn height(&self) -> f64 {
        let rows = row_f64(self.visible.len().clamp(1, MAX_VISIBLE_ROWS));
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

    /// First result kept in the painted window. Keyboard movement can reach
    /// every result without letting the highlighted row disappear below the
    /// fixed-height palette.
    fn window_start(&self) -> usize {
        self.cursor
            .saturating_add(1)
            .saturating_sub(MAX_VISIBLE_ROWS)
    }

    pub fn handle_key(&mut self, event: &WindowEvent) -> bool {
        if !self.visible_flag {
            return false;
        }
        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            let change = self.query_composition.handle_event(
                event,
                &mut self.query,
                &mut self.query_selection,
                false,
                false,
            );
            if change == QueryEventChange::Edited {
                self.cursor = 0;
                self.rebuild_visible();
            }
            return change != QueryEventChange::Ignored;
        };
        match logical_key {
            Key::Named(NamedKey::ArrowDown) => {
                if !self.visible.is_empty() {
                    self.cursor = self
                        .cursor
                        .saturating_add(1)
                        .min(self.visible.len().saturating_sub(1));
                }
                true
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.cursor = self.cursor.saturating_sub(1);
                true
            }
            Key::Named(NamedKey::Enter) => {
                if let Some(&idx) = self.visible.get(self.cursor)
                    && let Some(entry) = self.items.get(idx)
                {
                    self.pending = Some(entry.clone());
                }
                self.set_visible(false);
                true
            }
            Key::Named(NamedKey::Escape) => {
                self.set_visible(false);
                true
            }
            _ => {
                let change = self.query_composition.handle_event(
                    event,
                    &mut self.query,
                    &mut self.query_selection,
                    false,
                    false,
                );
                if change == QueryEventChange::Edited {
                    self.cursor = 0;
                    self.rebuild_visible();
                }
                change != QueryEventChange::Ignored
            }
        }
    }

    pub fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if !self.visible_flag {
            return;
        }
        let rect = self.rect();
        Self::draw_card_chrome(scene, transform, theme, rect);
        let input_rect = self.draw_input_field(scene, transform, text_renderer, theme, rect);
        self.draw_result_rows(scene, transform, text_renderer, theme, rect, input_rect);
    }

    /// The palette card's background fill and border.
    fn draw_card_chrome(scene: &mut Scene, transform: Affine, theme: &Theme, rect: Rect) {
        let shape = RoundedRect::from_rect(rect, 12.0);
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
    }

    /// The query input box and its composed text. Returns the input's
    /// `Rect` so the result list can lay out below it.
    fn draw_input_field(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
    ) -> Rect {
        let tokens = &theme.tokens;
        let input_rect = Rect::new(
            rect.x0 + PADDING,
            rect.y0 + PADDING,
            rect.x1 - PADDING,
            rect.y0 + PADDING + INPUT_HEIGHT,
        );
        let input_shape = RoundedRect::from_rect(input_rect, f64::from(theme.tokens.radii.md));
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

        self.query_composition.draw(
            scene,
            transform,
            text_renderer,
            &self.query,
            &self.query_selection,
            "Jump to page…",
            Rect::new(
                input_rect.x0 + 12.0,
                input_rect.y0,
                input_rect.x1 - 12.0,
                input_rect.y1,
            ),
            &tokens.typography.style(TextRole::Body),
            tokens.colors.text_primary,
            tokens.colors.text_dim,
            tokens.colors.accent[5].with_alpha_factor(0.3),
            tokens.colors.accent[5],
            true,
        );
        input_rect
    }

    /// Either the "No pages match" placeholder, or the visible-and-windowed
    /// slice of result rows with the cursor highlight.
    fn draw_result_rows(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        input_rect: Rect,
    ) {
        let tokens = &theme.tokens;
        let list_top = input_rect.y1 + PADDING;
        let row_size = tokens.typography.scale.sm;
        if self.visible.is_empty() {
            let label = "No pages match";
            let y = list_top + (ROW_HEIGHT - f64::from(row_size)) / 2.0;
            text_renderer.draw_text(
                scene,
                label,
                row_size,
                tokens.colors.text_dim,
                translated(transform, rect.x0 + PADDING + 12.0, y),
                None,
            );
            return;
        }

        for (slot, (visible_index, &idx)) in self
            .visible
            .iter()
            .enumerate()
            .skip(self.window_start())
            .take(MAX_VISIBLE_ROWS)
            .enumerate()
        {
            // `visible` only ever holds indices produced by `rebuild_visible`
            // enumerating `self.items`, so this is structurally in range —
            // checked rather than indexed so that invariant can't silently
            // regress into a panic.
            let Some(row) = self.items.get(idx) else {
                continue;
            };
            let y0 = list_top + row_f64(slot) * ROW_HEIGHT;
            let row_rect = Rect::new(rect.x0 + PADDING, y0, rect.x1 - PADDING, y0 + ROW_HEIGHT);
            if visible_index == self.cursor {
                let hl = RoundedRect::from_rect(row_rect, f64::from(theme.tokens.radii.sm));
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    tokens.colors.accent[5],
                    None,
                    &hl,
                );
            }
            let ty = row_rect.y0 + (row_rect.height() - f64::from(row_size)) / 2.0;
            text_renderer.draw_text(
                scene,
                &row.title,
                row_size,
                tokens.colors.text_primary,
                translated(transform, row_rect.x0 + 12.0, ty),
                None,
            );
        }
    }

    /// Page-id getter for tests that don't want to fish through
    /// `take_selection`.
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
        let node = taffy.leaf_node(Style::default());
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
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
    fn name(&self) -> &'static str {
        "PageJumpPalette"
    }
    fn test_id(&self) -> Option<&str> {
        Some("page_jump_palette")
    }
    fn get_rect(&self) -> Rect {
        self.rect()
    }
    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.query_composition.ime_cursor_rect()
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        Some(winit::window::ImePurpose::Normal)
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        if !self.visible_flag {
            return Some(
                AccessNode::new(Role::Dialog)
                    .with_name("Jump to page")
                    .with_state(AccessState {
                        hidden: true,
                        modal: true,
                        ..Default::default()
                    }),
            );
        }
        let count = self.visible.len();
        let children = self
            .visible
            .iter()
            .enumerate()
            .filter_map(|(position, &item_index)| {
                self.items.get(item_index).map(|item| {
                    AccessNode::new(Role::MenuItem)
                        .with_name(item.title.clone())
                        .with_state(AccessState {
                            selected: position == self.cursor,
                            ..Default::default()
                        })
                        .with_position(position.saturating_add(1), count)
                })
            })
            .collect();
        Some(
            AccessNode::new(Role::Dialog)
                .with_name("Jump to page")
                .with_optional_value((!self.query.is_empty()).then(|| self.query.clone()))
                .with_state(AccessState {
                    modal: true,
                    ..Default::default()
                })
                .with_children(children),
        )
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
        ev.physical_key = PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
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
            PageEntry {
                id: Uuid::from_u128(1),
                title: "Welcome".into(),
            },
            PageEntry {
                id: Uuid::from_u128(2),
                title: "Roadmap".into(),
            },
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
    fn typing_replaces_selection_at_the_caret_instead_of_appending() {
        let mut p = seed_two();
        p.set_visible(true);
        p.query = "wxyz".into();
        p.query_selection = Selection {
            anchor: 1,
            cursor: 3,
        };
        assert!(p.handle_key(&char_press("a")));
        assert_eq!(p.query(), "waz");
        assert_eq!(p.query_selection, Selection::collapsed(2));
    }

    #[test]
    fn ime_preedit_draws_without_filtering_until_commit() {
        let mut palette = PageJumpPalette::new();
        palette.set_items(vec![
            PageEntry {
                id: Uuid::from_u128(1),
                title: "東京".into(),
            },
            PageEntry {
                id: Uuid::from_u128(2),
                title: "London".into(),
            },
        ]);
        palette.set_visible(true);
        palette.set_anchor(Point::new(20.0, 20.0));
        assert!(
            palette.handle_key(&WindowEvent::Ime(winit::event::Ime::Preedit(
                "東京".into(),
                Some(("東".len(), "東".len())),
            )))
        );
        assert_eq!(palette.query(), "");
        assert_eq!(palette.visible_len(), 2);

        let mut scene = Scene::new();
        let mut renderer = TextRenderer::new();
        palette.draw_overlay(&mut scene, Affine::IDENTITY, &mut renderer, &Theme::dark());
        assert!(Widget::ime_cursor_rect(&palette).is_some());

        assert!(palette.handle_key(&WindowEvent::Ime(winit::event::Ime::Commit("東京".into(),))));
        assert_eq!(palette.query(), "東京");
        assert_eq!(palette.visible_len(), 1);
        assert_eq!(palette.highlighted().unwrap().title, "東京");
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

    #[test]
    fn long_result_lists_keep_the_keyboard_highlight_in_the_painted_window() {
        let mut p = PageJumpPalette::new();
        p.set_items(
            (0..14)
                .map(|i| PageEntry {
                    id: Uuid::from_u128(i + 1),
                    title: format!("Page {i}"),
                })
                .collect(),
        );
        p.set_visible(true);
        for _ in 0..12 {
            p.handle_key(&key_press(Key::Named(NamedKey::ArrowDown), None));
        }
        assert_eq!(p.cursor, 12);
        assert_eq!(p.window_start(), 3);
        assert!(p.cursor < p.window_start() + MAX_VISIBLE_ROWS);
    }

    #[test]
    fn accessibility_exposes_committed_query_and_selected_result() {
        let mut p = seed_two();
        let hidden = Widget::accessibility(&p).unwrap();
        assert!(hidden.state.hidden);
        p.set_visible(true);
        p.handle_key(&char_press("r"));
        let node = Widget::accessibility(&p).unwrap();
        assert_eq!(node.role, crate::a11y::Role::Dialog);
        assert_eq!(node.value.as_deref(), Some("r"));
        assert_eq!(node.children.len(), 1);
        assert!(node.children[0].state.selected);
    }
}
