//! `FilterMenu` — a compact popover anchored at the caret: a list of rows
//! narrowed by a case-insensitive query, labels that start with it first.
//! The [`SlashMenu`](crate::widget::SlashMenu) (block kinds and commands)
//! and the [`LinkPopover`](crate::widget::LinkPopover) (page links) are this
//! menu over their own row values.
//!
//! The query is either the menu's own filter field, or, for an *inline*
//! menu ([`FilterMenu::set_inline`]), text the parent keeps in its document
//! after the trigger and hands over with [`FilterMenu::set_filter`]: what a
//! person types stays where they typed it, whatever the menu makes of it.
//!
//! - **Arrow Up / Down** — move the highlighted row, wrapping at either end.
//! - **Enter / Tab** — commit the highlighted row; its value is queued for
//!   the parent to drain via [`FilterMenu::take_selection`]. With no row
//!   left, an inline menu closes and leaves the key to the parent.
//! - **Escape** — close without committing.
//! - **Typing** — edits the filter field; an inline menu leaves typing to
//!   the parent.
//! - **Backspace** on an empty filter field closes the menu and is left to
//!   the parent, which deletes the trigger text.
//!
//! The menu has no layout node: the parent calls [`FilterMenu::draw_overlay`]
//! after its main draw pass so the popover lands on top, the same "draw
//! last" convention as [`ContextMenu`](crate::widget::context_menu::ContextMenu).

use std::collections::HashMap;
use tilo_sort::CompactSort as _;

use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke, Vec2};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::icons::{Icon, draw_icon};
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
    /// Fluent key of the menu's accessible name.
    pub access_name: &'static str,
    pub widget_name: &'static str,
    pub test_id: &'static str,
}

/// One row: its label, extra search keywords, the value it commits, and
/// optionally a leading icon and a trailing hint (the Markdown that makes
/// the same block, say).
pub(super) struct MenuEntry<T> {
    pub label: String,
    pub keywords: &'static [&'static str],
    pub value: T,
    pub icon: Option<Icon>,
    pub hint: Option<&'static str>,
}

impl<T> MenuEntry<T> {
    /// A row with a label, keywords and a value, and no icon or hint.
    pub(super) fn plain(label: String, keywords: &'static [&'static str], value: T) -> Self {
        Self {
            label,
            keywords,
            value,
            icon: None,
            hint: None,
        }
    }

    /// How well the row answers `needle` (lowercase), best first: the label
    /// starts with it, a word of the label or a keyword starts with it, or
    /// either merely contains it. `None` when it does not match.
    fn rank(&self, needle: &str) -> Option<u8> {
        if needle.is_empty() {
            return Some(0);
        }
        let label = self.label.to_lowercase();
        if label.starts_with(needle) {
            return Some(0);
        }
        let keywords = || self.keywords.iter().map(|k| k.to_lowercase());
        if label
            .split_whitespace()
            .any(|word| word.starts_with(needle))
            || keywords().any(|k| k.starts_with(needle))
        {
            return Some(1);
        }
        (label.contains(needle) || keywords().any(|k| k.contains(needle))).then_some(2)
    }
}

/// Where the popover sits.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Placement {
    /// Its top-left corner at a point.
    At(Point),
    /// Under the caret's rect, or over it where there is no room below,
    /// kept inside `bounds` when the parent gave any.
    Caret { caret: Rect, bounds: Option<Rect> },
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
    /// The query is the parent's text, not the menu's own field.
    inline: bool,
    /// Where the popover sits, in logical coords.
    placement: Placement,
    /// The text scale its rows and field grow with.
    scale: f64,
    /// One-shot: the committed row's value, drained by the parent. Hiding
    /// the menu leaves it, as the parent may hide it before draining.
    pending: Option<T>,
    /// Its coming from the caret or point it opens at, and going back
    /// (`motion::Appear`).
    appear: crate::motion::Appearing,
    /// The pose it was last drawn in: rows are hit where they are drawn.
    drawn: crate::motion::Pose,
    /// The filter, rows and highlight it showed when it was hidden, so a
    /// leaving menu fades as it was.
    leaving: Option<(String, Vec<usize>, usize)>,
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
            inline: false,
            placement: Placement::At(Point::ZERO),
            scale: 1.0,
            pending: None,
            appear: crate::motion::Appearing::closed(),
            drawn: crate::motion::Pose::REST,
            leaving: None,
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
        if !visible && self.shows() {
            self.leaving = Some((self.filter.clone(), self.visible.clone(), self.cursor));
        }
        self.visible_flag = visible;
        self.appear.show(visible);
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

    /// Put the popover's top-left corner at `anchor`.
    pub fn set_anchor(&mut self, anchor: Point) {
        self.placement = Placement::At(anchor);
    }

    /// Attach the popover to the caret's rect (the caller leaves the gap it
    /// wants around the caret in it): below it, or above it when
    /// `bounds` (the visible area) has no room below but has some above,
    /// and moved left as far as it must to stay inside `bounds`.
    pub fn set_caret(&mut self, caret: Rect, bounds: Option<Rect>) {
        self.placement = Placement::Caret { caret, bounds };
    }

    /// Whether the query is text the parent keeps in its document (and
    /// passes in with [`Self::set_filter`]) rather than the menu's own
    /// field. An inline menu paints no field and leaves typing to the
    /// parent.
    pub fn set_inline(&mut self, inline: bool) {
        self.inline = inline;
    }

    pub fn is_inline(&self) -> bool {
        self.inline
    }

    /// Grow rows, field and width with the reader's text scale.
    pub fn set_scale(&mut self, scale: f64) {
        self.scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
    }

    /// Replace the filter and rebuild the visible rows; the same filter
    /// again keeps the highlighted row.
    pub fn set_filter(&mut self, filter: &str) {
        if self.filter == filter {
            return;
        }
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
        self.spec.width * self.scale
    }

    fn row_height(&self) -> f64 {
        self.spec.row_height * self.scale
    }

    /// The filter field and the gap under it; an inline menu has none.
    fn field_height(&self) -> f64 {
        if self.inline {
            0.0
        } else {
            (FILTER_HEIGHT + FILTER_GAP) * self.scale
        }
    }

    pub fn height(&self) -> f64 {
        let rows = self.visible.len().clamp(1, self.spec.max_rows);
        PADDING_Y * 2.0 + self.field_height() + self.row_height() * count_f64(rows)
    }

    /// Whether the popover paints at all: an inline menu with nothing to
    /// offer stays out of the way of the text being typed.
    fn shows(&self) -> bool {
        self.visible_flag && !(self.inline && self.visible.is_empty())
    }

    /// Rect the popover paints into at its placement.
    pub fn rect(&self) -> Rect {
        let (width, height) = (self.width(), self.height());
        let origin = match self.placement {
            Placement::At(anchor) => anchor,
            Placement::Caret { caret, bounds } => {
                let below = caret.y1;
                let above = caret.y0 - height;
                let Some(bounds) = bounds else {
                    return Rect::new(caret.x0, below, caret.x0 + width, below + height);
                };
                let room_below = bounds.y1 - below;
                let room_above = caret.y0 - bounds.y0;
                let y = if room_below >= height || room_below >= room_above {
                    below
                } else {
                    above
                };
                let x = caret.x0.min(bounds.x1 - width).max(bounds.x0);
                Point::new(x, y)
            }
        };
        Rect::new(origin.x, origin.y, origin.x + width, origin.y + height)
    }

    fn entry(&self, visible_index: usize) -> Option<&MenuEntry<T>> {
        self.visible
            .get(visible_index)
            .and_then(|&i| self.entries.get(i))
    }

    fn rows_top(&self) -> f64 {
        self.rect().y0 + PADDING_Y + self.field_height()
    }

    fn window_start(&self) -> usize {
        self.cursor
            .saturating_add(1)
            .saturating_sub(self.spec.max_rows)
    }

    fn slot_rect(&self, slot: usize) -> Rect {
        let y0 = self.rows_top() + count_f64(slot) * self.row_height();
        let rect = self.rect();
        Rect::new(rect.x0 + 4.0, y0, rect.x1 - 4.0, y0 + self.row_height())
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
        // Rows are hit where they are drawn while the menu arrives.
        let point = self.drawn.to_local(point);
        if !self.shows() || !self.rect().contains(point) {
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
        let slot = (relative / self.row_height()).floor() as usize;
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
        let needle = self.filter.trim().to_lowercase();
        let mut ranked: Vec<(u8, usize)> = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(i, entry)| entry.rank(&needle).map(|rank| (rank, i)))
            .collect();
        // Stable: within a rank, the menu's own order.
        ranked.compact_sort_by_key(|&(rank, _)| rank);
        self.visible.clear();
        self.visible.extend(ranked.into_iter().map(|(_, i)| i));
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
            if self.inline {
                return false;
            }
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
        let last = self.visible.len().saturating_sub(1);
        match logical_key {
            Key::Named(NamedKey::ArrowDown) => {
                self.cursor = if self.cursor >= last {
                    0
                } else {
                    self.cursor.saturating_add(1)
                };
                true
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.cursor = self.cursor.checked_sub(1).unwrap_or(last);
                true
            }
            Key::Named(NamedKey::Enter | NamedKey::Tab) => {
                let value = self.entry(self.cursor).map(|entry| entry.value.clone());
                let committed = value.is_some();
                self.pending = value;
                self.set_visible(false);
                // An inline menu with nothing to commit gives the key back:
                // Enter still ends the line it was typed in.
                committed || !self.inline
            }
            Key::Named(NamedKey::Escape) => {
                self.set_visible(false);
                true
            }
            _ if self.inline => false,
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
        // It comes from the caret or point it opened at and goes back
        // (`motion::Appear`). A menu that empties as the person types has
        // nothing to fade: it goes at once.
        let shows = self.shows();
        if shows {
            self.leaving = None;
        } else if self.leaving.is_none() {
            self.appear.snap(false);
        }
        self.appear.follow(shows, theme.motion());
        if !self.appear.is_drawn() {
            self.drawn = crate::motion::Pose::REST;
            self.leaving = None;
            return;
        }
        // A leaving menu shows what it showed when it was hidden.
        let restore = self.leaving.clone().map(|(filter, visible, cursor)| {
            (
                std::mem::replace(&mut self.filter, filter),
                std::mem::replace(&mut self.visible, visible),
                std::mem::replace(&mut self.cursor, cursor),
            )
        });
        let rect = self.rect();
        let anchor = match self.placement {
            Placement::At(at) => at,
            Placement::Caret { caret, .. } => caret.center(),
        };
        let pose = self.appear.pose(rect, anchor);
        self.drawn = pose;
        let bounds = crate::widget::floating_surface::painted_bounds(rect, theme);
        if let Some(transform) = pose.begin(scene, transform, bounds) {
            self.draw_chrome_and_filter(scene, transform, text_renderer, theme, rect);
            self.draw_rows(scene, transform, text_renderer, theme, rect);
            pose.end(scene);
        }
        if let Some((filter, visible, cursor)) = restore {
            self.filter = filter;
            self.visible = visible;
            self.cursor = cursor;
        }
    }

    /// Whether it is still arriving or leaving: a parent that draws it asks
    /// for frames until it rests.
    pub fn is_moving(&self) -> bool {
        self.appear.is_moving()
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
        if self.inline {
            return;
        }

        let filter_rect = Rect::new(
            rect.x0 + 6.0,
            rect.y0 + PADDING_Y,
            rect.x1 - 6.0,
            rect.y0 + PADDING_Y + FILTER_HEIGHT * self.scale,
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
                placeholder: &crate::i18n!(self.spec.placeholder),
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

    /// The "No matches" empty state or the scrolled row window: each row's
    /// icon, label and hint.
    fn draw_rows(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
    ) {
        let tokens = &theme.tokens;
        let mut font_style = tokens.typography.style(TextRole::Caption);
        font_style.size *= self.scale as f32;
        let row_height = self.row_height();
        if self.visible.is_empty() {
            let empty = crate::i18n!("filter-menu-no-matches");
            let (_, hint_h) = text_renderer.measure_style(&empty, None, &font_style);
            let y = self.rows_top() + (row_height - f64::from(hint_h)) / 2.0;
            text_renderer.draw_style(
                scene,
                &empty,
                tokens.colors.text_dim,
                transform.pre_translate(Vec2::new(rect.x0 + PADDING_X, y)),
                None,
                &font_style,
            );
            return;
        }
        let icon_size = f64::from(tokens.metrics.icon_sm) * self.scale;
        let icon_gap = f64::from(tokens.spacing.s2) * self.scale;
        let has_icons = self.entries.iter().any(|entry| entry.icon.is_some());
        let window_start = self.window_start();
        for slot in 0..self.spec.max_rows {
            let visible_index = window_start.saturating_add(slot);
            let Some(entry) = self.entry(visible_index) else {
                break;
            };
            let row_rect = self.slot_rect(slot);
            // The armed row is the panel's strong lift, as in every menu;
            // its ink is measured on it.
            let armed = visible_index == self.cursor;
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
            let mut label_x = row_rect.x0 + PADDING_X;
            if has_icons {
                if let Some(icon) = entry.icon {
                    draw_icon(
                        scene,
                        transform,
                        Rect::from_origin_size(
                            (label_x, row_rect.y0 + (row_height - icon_size) / 2.0),
                            (icon_size, icon_size),
                        ),
                        icon,
                        ink,
                        crate::icons::LUCIDE_DEFAULT_STROKE_PX,
                    );
                }
                label_x += icon_size + icon_gap;
            }
            // The hint (the Markdown that makes the same block) in
            // secondary ink against the trailing edge.
            let trailing_x1 = row_rect.x1 - PADDING_X;
            let mut label_x1 = trailing_x1;
            if let Some(hint) = entry.hint {
                let (w, h) = text_renderer.measure_style(hint, None, &font_style);
                let dim = if armed { ink } else { tokens.colors.text_dim };
                let x = trailing_x1 - f64::from(w);
                text_renderer.draw_style(
                    scene,
                    hint,
                    dim,
                    transform.pre_translate(Vec2::new(
                        x,
                        row_rect.y0 + (row_height - f64::from(h)) / 2.0,
                    )),
                    None,
                    &font_style,
                );
                label_x1 = x - icon_gap;
            }
            let label = text_renderer.elide_to_width_style(
                &entry.label,
                (label_x1 - label_x).max(1.0) as f32,
                &font_style,
            );
            // Centre on the measured line, not the font size — the two differ
            // by the face's ascent/descent and the label drifted low.
            let (_, label_h) = text_renderer.measure_style(&label, None, &font_style);
            let text_y = row_rect.y0 + (row_height - f64::from(label_h)) / 2.0;
            text_renderer.draw_style(
                scene,
                &label,
                ink,
                transform.pre_translate(Vec2::new(label_x, text_y)),
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
        (self.visible_flag && !self.inline)
            .then(|| self.filter_composition.ime_cursor_rect())
            .flatten()
    }

    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        self.ime_cursor_rect()?;
        self.filter_composition
            .ime_edit_fingerprint(&self.filter, &self.filter_selection, key)
    }

    fn ime_content(&self) -> Option<crate::ime::InputContent> {
        Some(crate::ime::InputContent::TEXT)
    }

    // A closed menu is hidden so it collapses out of the focus/reading order.
    fn accessibility_hidden(&self) -> bool {
        !self.visible_flag
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // Rows are drawn, not child widgets, so each visible row is a
        // synthetic `MenuItem` selected at the highlight cursor.
        if self.accessibility_hidden() {
            return Some(
                AccessNode::new(Role::Menu)
                    .with_name(crate::i18n!(self.spec.access_name))
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
                .with_name(crate::i18n!(self.spec.access_name))
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
