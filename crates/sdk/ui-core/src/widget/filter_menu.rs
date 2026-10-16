//! `FilterMenu` — a compact popover anchored at the caret: a filter field
//! over a list of rows narrowed by case-insensitive substring. The
//! [`SlashMenu`](crate::widget::SlashMenu) (block kinds) and the
//! [`LinkPopover`](crate::widget::LinkPopover) (page links) are this menu
//! over their own row values.
//!
//! - **Arrow Up / Down** — move the highlighted row.
//! - **Enter** — commit the highlighted row; its value is queued for the
//!   parent to drain via [`FilterMenu::take_selection`].
//! - **Escape** — close without committing.
//! - **Typing** — edits the filter; the visible rows narrow to those whose
//!   label or a keyword contains it.
//! - **Backspace** on an empty filter closes the menu and is left to the
//!   parent, which deletes the trigger text.
//!
//! The menu has no layout node: the parent calls [`FilterMenu::draw_overlay`]
//! after its main draw pass so the popover lands on top, the same "draw
//! last" convention as [`ContextMenu`](crate::widget::context_menu::ContextMenu).

use std::collections::HashMap;

use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke, Vec2};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::paint::PaintCtx;
use crate::query_text::{QueryComposition, QueryEventChange, QueryInk, QueryView};
use crate::text_render::TextRenderer;
use crate::text_selection::Selection;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::ModelPath;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

const PADDING_Y: f64 = 6.0;
/// Horizontal padding either side of each row label.
const PADDING_X: f64 = 10.0;
const FILTER_HEIGHT: f64 = 32.0;
const FILTER_GAP: f64 = 4.0;

/// What distinguishes one filter menu from another.
pub(super) struct MenuSpec {
    pub width: f64,
    pub row_height: f64,
    /// Rows painted at once; the window scrolls with the cursor.
    pub max_rows: usize,
    pub placeholder: &'static str,
    /// The accessible name of the menu.
    pub access_name: &'static str,
    pub widget_name: &'static str,
    pub test_id: &'static str,
}

/// One row: its label, extra search keywords, and the value it commits.
pub(super) struct MenuEntry<T> {
    pub label: String,
    pub keywords: &'static [&'static str],
    pub value: T,
}

impl<T> MenuEntry<T> {
    fn matches(&self, needle: &str) -> bool {
        needle.is_empty()
            || self.label.to_lowercase().contains(needle)
            || self
                .keywords
                .iter()
                .any(|k| k.to_lowercase().contains(needle))
    }
}

pub struct FilterMenu<T> {
    spec: &'static MenuSpec,
    entries: Vec<MenuEntry<T>>,
    /// Indices into `entries` that survive the current filter.
    visible: Vec<usize>,
    /// Cursor into `visible`. Always clamped.
    cursor: usize,
    filter: String,
    filter_selection: Selection,
    filter_composition: QueryComposition,
    visible_flag: bool,
    /// Top-left of the popover in logical coords.
    anchor: Point,
    /// One-shot: the committed row's value, drained by the parent. Hiding
    /// the menu leaves it, as the parent may hide it before draining.
    pending: Option<T>,
}

impl<T: Clone> FilterMenu<T> {
    pub(super) fn with_spec(spec: &'static MenuSpec, entries: Vec<MenuEntry<T>>) -> Self {
        let mut menu = Self {
            spec,
            entries,
            visible: Vec::new(),
            cursor: 0,
            filter: String::new(),
            filter_selection: Selection::collapsed(0),
            filter_composition: QueryComposition::new(),
            visible_flag: false,
            anchor: Point::ZERO,
            pending: None,
        };
        menu.rebuild_visible();
        menu
    }

    pub(super) fn set_entries(&mut self, entries: Vec<MenuEntry<T>>) {
        self.entries = entries;
        self.rebuild_visible();
    }

    /// Show / hide the menu. Hiding resets the filter and cursor so the
    /// next open lands fresh.
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

    /// Replace the filter and rebuild the visible rows.
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

    /// Drain the committed value: `Some` exactly once per commit.
    pub fn take_selection(&mut self) -> Option<T> {
        self.pending.take()
    }

    /// The highlighted row's value, if any row matches the filter.
    pub fn highlighted(&self) -> Option<&T> {
        self.entry(self.cursor).map(|entry| &entry.value)
    }

    /// Number of rows matching the current filter.
    pub fn visible_len(&self) -> usize {
        self.visible.len()
    }

    pub fn width(&self) -> f64 {
        self.spec.width
    }

    pub fn height(&self) -> f64 {
        let rows = self.visible.len().clamp(1, self.spec.max_rows);
        PADDING_Y * 2.0 + FILTER_HEIGHT + FILTER_GAP + self.spec.row_height * count_f64(rows)
    }

    /// Rect the popover paints into at the current anchor.
    pub fn rect(&self) -> Rect {
        Rect::new(
            self.anchor.x,
            self.anchor.y,
            self.anchor.x + self.width(),
            self.anchor.y + self.height(),
        )
    }

    fn entry(&self, visible_index: usize) -> Option<&MenuEntry<T>> {
        self.visible
            .get(visible_index)
            .and_then(|&i| self.entries.get(i))
    }

    fn rows_top(&self) -> f64 {
        self.rect().y0 + PADDING_Y + FILTER_HEIGHT + FILTER_GAP
    }

    fn window_start(&self) -> usize {
        self.cursor
            .saturating_add(1)
            .saturating_sub(self.spec.max_rows)
    }

    fn slot_rect(&self, slot: usize) -> Rect {
        let y0 = self.rows_top() + count_f64(slot) * self.spec.row_height;
        let rect = self.rect();
        Rect::new(rect.x0 + 4.0, y0, rect.x1 - 4.0, y0 + self.spec.row_height)
    }

    /// Painted rectangle for a visible row inside the scrolled window. Hosts
    /// use this for pointer automation rather than duplicating the geometry.
    pub fn row_rect(&self, visible_index: usize) -> Option<Rect> {
        let start = self.window_start();
        let end = self
            .visible
            .len()
            .min(start.saturating_add(self.spec.max_rows));
        (start..end)
            .contains(&visible_index)
            .then(|| self.slot_rect(visible_index.saturating_sub(start)))
    }

    /// The visible row under a logical point, sharing the scrolled window
    /// and bounds checks with keyboard selection.
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
            reason = "relative is non-negative screen geometry and the visible row count is bounded"
        )]
        let slot = (relative / self.spec.row_height).floor() as usize;
        let index = self.window_start().saturating_add(slot);
        (index < self.visible.len()).then_some(index)
    }

    /// Highlight the row under the pointer. Hovering the padding leaves the
    /// last row lit. Returns whether the highlight moved.
    pub fn hover_at(&mut self, point: Point) -> bool {
        match self.row_at(point) {
            Some(index) if index != self.cursor => {
                self.cursor = index;
                true
            }
            _ => false,
        }
    }

    /// Commit the row under `point` as if Enter were pressed on it: its value
    /// is returned and queued, and the menu closes. `None` (menu left open)
    /// off the rows, so the parent can treat the click as a dismiss.
    pub fn click_at(&mut self, point: Point) -> Option<T> {
        let index = self.row_at(point)?;
        let value = self.entry(index)?.value.clone();
        self.cursor = index;
        self.pending = Some(value.clone());
        self.set_visible(false);
        Some(value)
    }

    fn rebuild_visible(&mut self) {
        let needle = self.filter.to_lowercase();
        self.visible.clear();
        for (i, entry) in self.entries.iter().enumerate() {
            if entry.matches(&needle) {
                self.visible.push(i);
            }
        }
        if self.cursor >= self.visible.len() {
            self.cursor = self.visible.len().saturating_sub(1);
        }
    }

    /// Feed `event` to the filter field; an edit restarts at the top row.
    fn edit_filter(&mut self, event: &WindowEvent) -> QueryEventChange {
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
        change
    }

    /// Handle a key or IME event while the menu is open. Returns whether it
    /// was consumed; the parent routes keystrokes here while it is visible.
    pub fn handle_key(&mut self, event: &WindowEvent) -> bool {
        if !self.visible_flag {
            return false;
        }
        if matches!(event, WindowEvent::Ime(_)) {
            self.edit_filter(event);
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
                if let Some(entry) = self.entry(self.cursor) {
                    self.pending = Some(entry.value.clone());
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
                // Close, but leave the keystroke to the parent so it deletes
                // the trigger text (`/`, `[[`).
                self.set_visible(false);
                false
            }
            _ => self.edit_filter(event) != QueryEventChange::Ignored,
        }
    }

    /// Paint the popover. Called by the parent after its main draw pass.
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

    /// The card background/border and the filter query box.
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
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            crate::chrome::panel_ground(theme),
            None,
            &RoundedRect::from_rect(rect, radius),
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
        let filter_radius = 4.0;
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            tokens.colors.surfaces.base,
            None,
            &RoundedRect::from_rect(filter_rect, filter_radius),
        );
        // A field reads as a field on every preset: its rest edge derives
        // from its own fill (HIG §9.5), painted as a crisp 1 px inset stroke;
        // on the light preset `base` and the panel ground are a shade apart.
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            crate::surface::edge_on(tokens.colors.surfaces.base),
            None,
            &crate::surface::edge_outline(filter_rect, filter_radius),
        );
        self.filter_composition.draw(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            QueryView {
                text: &self.filter,
                selection: &self.filter_selection,
                placeholder: self.spec.placeholder,
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

    /// The "No matches" empty state or the scrolled row window.
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
            let y = self.rows_top() + (self.spec.row_height - f64::from(hint_h)) / 2.0;
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
        for slot in 0..self.spec.max_rows {
            let visible_index = window_start.saturating_add(slot);
            let Some(entry) = self.entry(visible_index) else {
                break;
            };
            let row_rect = self.slot_rect(slot);
            // The armed row is the panel's strong lift, as in every menu;
            // its ink is measured on it.
            let ink = if visible_index == self.cursor {
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
            // Centre on the measured line, not the font size — the two differ
            // by the face's ascent/descent and the label drifted low.
            let (_, label_h) = text_renderer.measure_style(&entry.label, None, &font_style);
            let text_y = row_rect.y0 + (row_rect.height() - f64::from(label_h)) / 2.0;
            text_renderer.draw_style(
                scene,
                &entry.label,
                ink,
                transform.pre_translate(Vec2::new(row_rect.x0 + PADDING_X, text_y)),
                None,
                &font_style,
            );
        }
    }
}

/// Widens a small on-screen row count/index to `f64`. Always far below
/// f64's exact-integer range.
fn count_f64(v: usize) -> f64 {
    #[allow(
        clippy::cast_precision_loss,
        reason = "v is a painted row count/index, far below f64's exact-integer range"
    )]
    let out = v as f64;
    out
}

// The popover lives in overlay space, so the layout and main draw passes
// see a zero-size leaf; it is still addressable for focus, IME and
// assistive technology.
impl<T: Clone + 'static> Widget for FilterMenu<T> {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(Style::default(), node_map, self)
    }
    fn name(&self) -> &'static str {
        self.spec.widget_name
    }
    fn test_id(&self) -> Option<&str> {
        Some(self.spec.test_id)
    }
    fn get_rect(&self) -> Rect {
        self.rect()
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.visible_flag
            .then(|| self.filter_composition.ime_cursor_rect())
            .flatten()
    }

    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        self.ime_cursor_rect()?;
        self.filter_composition
            .ime_edit_fingerprint(&self.filter, &self.filter_selection, key)
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        Some(winit::window::ImePurpose::Normal)
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // Rows are drawn, not child widgets, so each visible row is a
        // synthetic `MenuItem` selected at the highlight cursor. A closed
        // menu is hidden so it collapses out of the focus/reading order.
        if !self.visible_flag {
            return Some(
                AccessNode::new(Role::Menu)
                    .with_name(self.spec.access_name)
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
            .filter_map(|(position, &entry_index)| {
                self.entries.get(entry_index).map(|entry| {
                    AccessNode::new(Role::MenuItem)
                        .with_action_target(format!("row:{entry_index}"))
                        .with_name(entry.label.clone())
                        .with_state(AccessState::new().selected(position == self.cursor))
                        .with_position(position.saturating_add(1), count)
                })
            })
            .collect();
        Some(
            AccessNode::new(Role::Menu)
                .with_name(self.spec.access_name)
                .with_optional_value((!self.filter.is_empty()).then(|| self.filter.clone()))
                .with_state(AccessState::new().expanded(true))
                .with_children(items),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        let Some(entry_index) = target
            .strip_prefix("row:")
            .and_then(|value| value.parse::<usize>().ok())
        else {
            return false;
        };
        if !self.visible_flag
            || !matches!(action, crate::a11y::Action::Activate)
            || !self.visible.contains(&entry_index)
        {
            return false;
        }
        let Some(value) = self.entries.get(entry_index).map(|e| e.value.clone()) else {
            return false;
        };
        self.pending = Some(value);
        self.set_visible(false);
        true
    }
}
