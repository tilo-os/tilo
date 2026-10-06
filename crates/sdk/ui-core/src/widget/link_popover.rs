//! `LinkPopover` — fuzzy-search popover for `[[Page Title]]` inline
//! page links. Same shape as [`crate::widget::SlashMenu`] but
//! seeded with caller-provided page titles instead of a hard-coded
//! block-kind list.
//!
//! A thin sibling rather than a generic `Popover<Item>`: the slash menu's
//! API is shaped around block-kind conversions, so folding the page-title
//! flow into the same trait object would mean either generic-ifying every
//! field (and paying for the boxing) or collapsing `BlockKind` and
//! `PageEntry` behind an associated type. The duplication is ~150 LOC and
//! a separate widget keeps the test surface small.

use std::collections::HashMap;

use taffy::{NodeId, Style, TaffyTree};
use uuid::Uuid;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke, Vec2};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::query_text::{QueryComposition, QueryEventChange, QueryInk, QueryView};
use crate::text_render::TextRenderer;
use crate::text_selection::Selection;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageEntry {
    pub id: Uuid,
    pub title: String,
}

const MENU_WIDTH: f64 = 220.0;
const ROW_HEIGHT: f64 = 26.0;
const PADDING_Y: f64 = 6.0;
const PADDING_X: f64 = 10.0;
const FILTER_HEIGHT: f64 = 32.0;
const FILTER_GAP: f64 = 4.0;
/// Keeps the popover small for big workspaces; the user can keep typing
/// to narrow the result.
const MAX_VISIBLE_ROWS: usize = 8;

pub struct LinkPopover {
    items: Vec<PageEntry>,
    /// Indices into `items` that survive the current filter.
    visible: Vec<usize>,
    /// Cursor into `visible`. Always clamped.
    cursor: usize,
    filter: String,
    filter_selection: Selection,
    filter_composition: QueryComposition,
    visible_flag: bool,
    anchor: Point,
    /// One-shot: when the user picks an entry, it lands here.
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
            filter_selection: Selection::collapsed(0),
            filter_composition: QueryComposition::new(),
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
            self.filter_selection = Selection::collapsed(0);
            self.filter_composition.clear();
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
        self.filter_selection = Selection::collapsed(self.filter.len());
        self.filter_composition.clear();
        self.cursor = 0;
        self.rebuild_visible();
    }

    pub fn filter(&self) -> &str {
        &self.filter
    }

    pub fn take_selection(&mut self) -> Option<PageEntry> {
        self.pending.take()
    }

    pub fn highlighted(&self) -> Option<&PageEntry> {
        self.visible
            .get(self.cursor)
            .and_then(|&i| self.items.get(i))
    }

    pub fn visible_len(&self) -> usize {
        self.visible.len()
    }

    pub fn width(&self) -> f64 {
        MENU_WIDTH
    }

    pub fn height(&self) -> f64 {
        let rows = self.visible.len().clamp(1, MAX_VISIBLE_ROWS);
        PADDING_Y * 2.0 + FILTER_HEIGHT + FILTER_GAP + ROW_HEIGHT * count_f64(rows)
    }

    pub fn rect(&self) -> Rect {
        Rect::new(
            self.anchor.x,
            self.anchor.y,
            self.anchor.x + self.width(),
            self.anchor.y + self.height(),
        )
    }

    fn rows_top(&self) -> f64 {
        self.rect().y0 + PADDING_Y + FILTER_HEIGHT + FILTER_GAP
    }

    fn window_start(&self) -> usize {
        self.cursor
            .saturating_add(1)
            .saturating_sub(MAX_VISIBLE_ROWS)
    }

    /// Painted rectangle for a result currently inside the eight-row window.
    pub fn row_rect(&self, visible_index: usize) -> Option<Rect> {
        let start = self.window_start();
        if visible_index < start
            || visible_index
                >= self
                    .visible
                    .len()
                    .min(start.saturating_add(MAX_VISIBLE_ROWS))
        {
            return None;
        }
        let slot = visible_index.saturating_sub(start);
        let y0 = self.rows_top() + count_f64(slot) * ROW_HEIGHT;
        let rect = self.rect();
        Some(Rect::new(rect.x0 + 4.0, y0, rect.x1 - 4.0, y0 + ROW_HEIGHT))
    }

    /// The filtered result row under a logical point.  Keeping this in the
    /// popover makes pointer and keyboard selection share the exact same
    /// scrolling window and bounds checks.
    fn row_at(&self, point: Point) -> Option<usize> {
        if !self.visible_flag || !self.rect().contains(point) {
            return None;
        }
        let relative = point.y - self.rows_top();
        if relative < 0.0 {
            return None;
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "relative is non-negative screen geometry and the visible result count is bounded"
        )]
        let slot = (relative / ROW_HEIGHT).floor() as usize;
        let index = self.window_start().saturating_add(slot);
        (index < self.visible.len()).then_some(index)
    }

    /// Highlight the result under the pointer. Returns whether a redraw is
    /// needed; filtering and keyboard navigation use the same cursor.
    pub fn hover_at(&mut self, point: Point) -> bool {
        match self.row_at(point) {
            Some(index) if index != self.cursor => {
                self.cursor = index;
                true
            }
            _ => false,
        }
    }

    /// Pick a row as if Enter had been pressed.  A click outside rows remains
    /// a parent-owned dismiss gesture, mirroring the slash menu.
    pub fn click_at(&mut self, point: Point) -> Option<PageEntry> {
        let index = self.row_at(point)?;
        let entry = self
            .visible
            .get(index)
            .and_then(|&item| self.items.get(item))?
            .clone();
        self.cursor = index;
        self.pending = Some(entry.clone());
        self.set_visible(false);
        Some(entry)
    }

    fn rebuild_visible(&mut self) {
        let needle = self.filter.to_lowercase();
        self.visible.clear();
        for (i, e) in self.items.iter().enumerate() {
            let matches = needle.is_empty() || e.title.to_lowercase().contains(&needle);
            if matches {
                self.visible.push(i);
            }
        }
        if self.cursor >= self.visible.len() {
            self.cursor = self.visible.len().saturating_sub(1);
        }
    }

    pub fn handle_key(&mut self, event: &WindowEvent) -> bool {
        if !self.visible_flag {
            return false;
        }
        if matches!(event, WindowEvent::Ime(_)) {
            let change = self.filter_composition.handle_event(
                event,
                &mut self.filter,
                &mut self.filter_selection,
                false,
                false,
            );
            if change == QueryEventChange::Edited {
                self.cursor = 0;
                self.rebuild_visible();
            }
            return true;
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
            return false;
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
                if let Some(entry) = self
                    .visible
                    .get(self.cursor)
                    .and_then(|&idx| self.items.get(idx))
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
            Key::Named(NamedKey::Backspace)
                if self.filter_selection.is_collapsed() && self.filter_selection.cursor == 0 =>
            {
                self.set_visible(false);
                // Return false so the parent can handle the
                // delete-of-the-`[[` glyphs.
                false
            }
            _ => {
                let change = self.filter_composition.handle_event(
                    event,
                    &mut self.filter,
                    &mut self.filter_selection,
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
        self.draw_chrome_and_filter(scene, transform, text_renderer, theme, rect);
        self.draw_rows(scene, transform, text_renderer, theme, rect);
    }

    /// The card background/border and the filter query box. Split out of
    /// `draw_overlay` to keep that under the line cap.
    fn draw_chrome_and_filter(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
    ) {
        let tokens = &theme.tokens;
        // Panel role (HIG §9.1): panel ground + the derived panel edge as
        // a crisp 1 px inset stroke; the declared `border_standard` vanished
        // against `raised` on the dark preset.
        let radius = 6.0;
        let shape = RoundedRect::from_rect(rect, radius);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            crate::chrome::panel_ground(theme),
            None,
            &shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            crate::chrome::panel_border(theme),
            None,
            &crate::surface::edge_outline(rect, radius),
        );

        let filter_rect = Rect::new(
            rect.x0 + 6.0,
            rect.y0 + PADDING_Y,
            rect.x1 - 6.0,
            rect.y0 + PADDING_Y + FILTER_HEIGHT,
        );
        let filter_shape = RoundedRect::from_rect(filter_rect, 4.0);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            tokens.colors.surfaces.base,
            None,
            &filter_shape,
        );
        // A field reads as a field on every preset: its rest edge derives
        // from its own fill (HIG §9.5), painted as a crisp 1 px inset stroke;
        // on the light preset `base` and the panel ground are a shade apart.
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            crate::surface::edge_on(tokens.colors.surfaces.base),
            None,
            &crate::surface::edge_outline(filter_rect, 4.0),
        );
        self.filter_composition.draw(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            QueryView {
                text: &self.filter,
                selection: &self.filter_selection,
                placeholder: "Find a note…",
                show_caret: true,
            },
            Rect::new(
                filter_rect.x0 + 6.0,
                filter_rect.y0,
                filter_rect.x1 - 6.0,
                filter_rect.y1,
            ),
            &theme.text(TextRole::Caption),
            QueryInk::of(&tokens.colors),
        );
    }

    /// The "No matches" empty state or the scrolled row window. Split out of
    /// `draw_overlay` to keep that under the line cap.
    fn draw_rows(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
    ) {
        let tokens = &theme.tokens;
        let font_style = tokens.typography.style(TextRole::Caption);
        if self.visible.is_empty() {
            let (_, hint_h) = text_renderer.measure_style("No matches", None, &font_style);
            let y = self.rows_top() + (ROW_HEIGHT - f64::from(hint_h)) / 2.0;
            text_renderer.draw_style(
                scene,
                "No matches",
                tokens.colors.text_dim,
                transform.pre_translate(Vec2::new(rect.x0 + PADDING_X, y)),
                None,
                &font_style,
            );
            return;
        }
        let window_start = self.window_start();
        for (i, (visible_index, &idx)) in self
            .visible
            .iter()
            .enumerate()
            .skip(window_start)
            .take(MAX_VISIBLE_ROWS)
            .enumerate()
        {
            let Some(row) = self.items.get(idx) else {
                continue;
            };
            let y0 = self.rows_top() + count_f64(i) * ROW_HEIGHT;
            let row_rect = Rect::new(rect.x0 + 4.0, y0, rect.x1 - 4.0, y0 + ROW_HEIGHT);
            let armed = visible_index == self.cursor;
            // The armed row is the panel's strong lift, as in every menu;
            // its ink is measured on it. Centred on the measured line.
            let ink = if armed {
                crate::widget::floating_surface::paint_armed_row(
                    scene,
                    transform,
                    row_rect,
                    row_rect.x0 - rect.x0,
                    theme,
                )
            } else {
                tokens.colors.text_primary
            };
            let (_, title_h) = text_renderer.measure_style(&row.title, None, &font_style);
            let text_y = row_rect.y0 + (row_rect.height() - f64::from(title_h)) / 2.0;
            text_renderer.draw_style(
                scene,
                &row.title,
                ink,
                transform.pre_translate(Vec2::new(row_rect.x0 + PADDING_X, text_y)),
                None,
                &font_style,
            );
        }
    }
}

/// Widens a small on-screen row count/index (bounded by
/// `MAX_VISIBLE_ROWS`, 8) to `f64`. Always far below f64's
/// exact-integer range.
fn count_f64(v: usize) -> f64 {
    #[allow(
        clippy::cast_precision_loss,
        reason = "v is bounded by MAX_VISIBLE_ROWS (8), far below f64's exact-integer range"
    )]
    let out = v as f64;
    out
}

// LinkPopover lives in overlay space so most methods no-op.
impl Widget for LinkPopover {
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
    fn name(&self) -> &'static str {
        "LinkPopover"
    }
    fn test_id(&self) -> Option<&str> {
        Some("link_popover")
    }
    fn get_rect(&self) -> Rect {
        self.rect()
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.visible_flag
            .then(|| self.filter_composition.ime_cursor_rect())
            .flatten()
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        Some(winit::window::ImePurpose::Normal)
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        if !self.visible_flag {
            return Some(
                AccessNode::new(Role::Menu)
                    .with_name("Link notes")
                    .with_state(AccessState {
                        expanded: Some(false),
                        hidden: true,
                        ..Default::default()
                    }),
            );
        }
        let count = self.visible.len();
        let items = self
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
            AccessNode::new(Role::Menu)
                .with_name("Link notes")
                .with_optional_value((!self.filter.is_empty()).then(|| self.filter.clone()))
                .with_state(AccessState {
                    expanded: Some(true),
                    ..Default::default()
                })
                .with_children(items),
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

    fn seed_three() -> LinkPopover {
        let mut p = LinkPopover::new();
        p.set_items(vec![
            PageEntry {
                id: Uuid::from_u128(1),
                title: "Welcome".into(),
            },
            PageEntry {
                id: Uuid::from_u128(2),
                title: "Roadmap".into(),
            },
            PageEntry {
                id: Uuid::from_u128(3),
                title: "Inbox".into(),
            },
        ]);
        p
    }

    #[test]
    fn link_popover_items_filter_and_empty_hint() {
        fn new_starts_empty_and_hidden() {
            let p = LinkPopover::new();
            assert!(!p.is_visible());
            assert!(p.items().is_empty());
        }

        fn set_items_seeds_visible_list() {
            let p = seed_three();
            assert_eq!(p.visible_len(), 3);
        }

        fn filter_narrows_results() {
            let mut p = seed_three();
            p.set_filter("wel");
            assert_eq!(p.visible_len(), 1);
            assert_eq!(p.highlighted().unwrap().title, "Welcome");
        }

        fn empty_results_renders_no_matches_hint() {
            let mut p = seed_three();
            p.set_visible(true);
            p.set_filter("zzz");
            assert_eq!(p.visible_len(), 0);
            assert!(p.highlighted().is_none());
        }

        new_starts_empty_and_hidden();
        set_items_seeds_visible_list();
        filter_narrows_results();
        empty_results_renders_no_matches_hint();
    }

    #[test]
    fn link_popover_keyboard_and_pointer_select_commit_or_escape() {
        fn arrow_keys_move_cursor() {
            let mut p = seed_three();
            p.set_visible(true);
            assert_eq!(p.highlighted().unwrap().title, "Welcome");
            p.handle_key(&key_press(Key::Named(NamedKey::ArrowDown), None));
            assert_eq!(p.highlighted().unwrap().title, "Roadmap");
            p.handle_key(&key_press(Key::Named(NamedKey::ArrowUp), None));
            assert_eq!(p.highlighted().unwrap().title, "Welcome");
        }

        fn keyboard_highlight_keeps_scrolling_through_large_page_sets() {
            let mut p = LinkPopover::new();
            p.set_items(
                (0..12)
                    .map(|index| PageEntry {
                        id: Uuid::from_u128(index + 1),
                        title: format!("Page {index}"),
                    })
                    .collect(),
            );
            p.set_visible(true);
            for _ in 0..9 {
                p.handle_key(&key_press(Key::Named(NamedKey::ArrowDown), None));
            }
            assert_eq!(p.cursor, 9);
            assert_eq!(p.window_start(), 2);
            assert_eq!(p.highlighted().unwrap().title, "Page 9");
        }

        fn enter_commits_selection_and_hides() {
            let mut p = seed_three();
            p.set_visible(true);
            p.handle_key(&key_press(Key::Named(NamedKey::Enter), None));
            let chosen = p.take_selection().unwrap();
            assert_eq!(chosen.title, "Welcome");
            assert!(!p.is_visible());
        }

        fn pointer_hover_and_click_select_the_visible_row() {
            let mut p = seed_three();
            p.set_anchor(Point::new(20.0, 30.0));
            p.set_visible(true);
            let row = p.row_rect(1).expect("second row is painted");
            assert!(p.hover_at(row.center()));
            assert_eq!(
                p.highlighted().map(|entry| entry.title.as_str()),
                Some("Roadmap")
            );
            let chosen = p.click_at(row.center()).expect("click selects row");
            assert_eq!(chosen.title, "Roadmap");
            assert_eq!(p.take_selection(), Some(chosen));
            assert!(!p.is_visible());
        }

        fn escape_hides_without_committing() {
            let mut p = seed_three();
            p.set_visible(true);
            p.handle_key(&key_press(Key::Named(NamedKey::Escape), None));
            assert!(!p.is_visible());
            assert!(p.take_selection().is_none());
        }

        arrow_keys_move_cursor();
        keyboard_highlight_keeps_scrolling_through_large_page_sets();
        enter_commits_selection_and_hides();
        pointer_hover_and_click_select_the_visible_row();
        escape_hides_without_committing();
    }

    #[test]
    fn link_popover_filter_typing_ime_and_backspace() {
        fn typing_pushes_filter() {
            let mut p = seed_three();
            p.set_visible(true);
            p.handle_key(&char_press("r"));
            assert_eq!(p.filter(), "r");
            assert_eq!(p.visible_len(), 1);
            assert_eq!(p.highlighted().unwrap().title, "Roadmap");
        }

        fn ime_preedit_is_visible_but_filters_only_after_commit() {
            let mut p = LinkPopover::new();
            p.set_items(vec![
                PageEntry {
                    id: Uuid::from_u128(1),
                    title: "東京".into(),
                },
                PageEntry {
                    id: Uuid::from_u128(2),
                    title: "London".into(),
                },
            ]);
            p.set_visible(true);
            assert!(p.handle_key(&WindowEvent::Ime(winit::event::Ime::Preedit(
                "東京".into(),
                Some(("東".len(), "東".len())),
            ))));
            assert_eq!(p.filter(), "");
            assert_eq!(p.visible_len(), 2);
            assert_eq!(
                Widget::accessibility(&p).unwrap().value,
                None,
                "preedit is not exposed as committed menu state"
            );

            let mut scene = Scene::new();
            let mut renderer = TextRenderer::new();
            p.draw_overlay(&mut scene, Affine::IDENTITY, &mut renderer, &Theme::dark());
            assert!(Widget::ime_cursor_rect(&p).is_some());

            assert!(p.handle_key(&WindowEvent::Ime(winit::event::Ime::Commit("東京".into()))));
            assert_eq!(p.filter(), "東京");
            assert_eq!(p.visible_len(), 1);
            assert_eq!(p.highlighted().unwrap().title, "東京");
            let access = Widget::accessibility(&p).unwrap();
            assert_eq!(access.name.as_deref(), Some("Link notes"));
            assert_eq!(access.value.as_deref(), Some("東京"));
            assert_eq!(access.children.len(), 1);
        }

        fn backspace_removes_one_extended_grapheme() {
            let mut p = seed_three();
            p.set_visible(true);
            p.set_filter("👩🏽‍💻");
            assert!(p.handle_key(&key_press(Key::Named(NamedKey::Backspace), None)));
            assert_eq!(p.filter(), "");
            assert!(p.is_visible());
        }

        fn backspace_pops_filter_then_closes() {
            let mut p = seed_three();
            p.set_visible(true);
            p.set_filter("r");
            let consumed = p.handle_key(&key_press(Key::Named(NamedKey::Backspace), None));
            assert!(consumed);
            assert_eq!(p.filter(), "");
            let consumed = p.handle_key(&key_press(Key::Named(NamedKey::Backspace), None));
            assert!(!consumed);
            assert!(!p.is_visible());
        }

        typing_pushes_filter();
        ime_preedit_is_visible_but_filters_only_after_commit();
        backspace_removes_one_extended_grapheme();
        backspace_pops_filter_then_closes();
    }
}
