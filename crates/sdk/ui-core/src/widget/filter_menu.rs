//! `FilterMenu` — a compact picker anchored at the caret: a
//! [`SuggestionList`] of rows narrowed by a query, ranked the way every list
//! search ranks ([`crate::search::ranked_rows`]). The
//! [`SlashMenu`](crate::widget::SlashMenu) (block kinds and commands) and
//! the [`LinkPopover`](crate::widget::LinkPopover) (page links) are this
//! menu over their own row values.
//!
//! The query is either the menu's own filter field, painted in the list's
//! header band, or, for an *inline* menu ([`FilterMenu::set_inline`]), text
//! the parent keeps in its document after the trigger and hands over with
//! [`FilterMenu::set_filter`]: what a person types stays where they typed
//! it, whatever the menu makes of it.
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

use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::icons::Icon;
use crate::paint::PaintCtx;
use crate::query_text::{QueryComposition, QueryEventChange, QueryInk, QueryView};
use crate::search::{FieldWeight, ListIndex};
use crate::text_render::TextRenderer;
use crate::text_selection::Selection;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::ModelPath;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::autocomplete::{Suggestion, SuggestionList};

const FILTER_HEIGHT: f64 = 32.0;
const FILTER_GAP: f64 = 4.0;

/// What distinguishes one filter menu from another.
pub(super) struct MenuSpec {
    /// The narrowest the popover is.
    pub width: f64,
    /// Rows painted at once; the rest scroll with the highlight.
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

    /// The row as the list paints it; its value is its index in the menu.
    fn suggestion(&self, index: usize) -> Suggestion {
        let row = Suggestion::labelled(index.to_string(), self.label.clone());
        let row = match self.icon {
            Some(icon) => row.with_icon(icon),
            None => row,
        };
        match self.hint {
            Some(hint) => row.with_detail(hint),
            None => row,
        }
    }
}

pub struct FilterMenu<T> {
    spec: &'static MenuSpec,
    entries: Vec<MenuEntry<T>>,
    /// Labels and keywords of `entries`, indexed while they stay the same.
    index: ListIndex,
    /// Indices into `entries` that survive the current filter, in the
    /// list's order.
    visible: Vec<usize>,
    /// The rows: their placement at the caret, painting, scrolling and
    /// highlight.
    list: SuggestionList,
    filter: String,
    filter_selection: Selection,
    filter_composition: QueryComposition,
    visible_flag: bool,
    /// The query is the parent's text, not the menu's own field.
    inline: bool,
    /// The text scale its field and width grow with.
    scale: f64,
    /// One-shot: the committed row's value, drained by the parent. Hiding
    /// the menu leaves it, as the parent may hide it before draining.
    pending: Option<T>,
    /// Its coming from the caret or point it opens at, and going back
    /// (`motion::Appear`).
    appear: crate::motion::Appearing,
    /// The pose it was last drawn in: rows are hit where they are drawn.
    drawn: crate::motion::Pose,
    /// The filter it showed when it was hidden, so a leaving menu fades as
    /// it was; its rows stay in the list until it opens again.
    leaving: Option<String>,
}

impl<T: Clone> FilterMenu<T> {
    pub(super) fn with_spec(spec: &'static MenuSpec, entries: Vec<MenuEntry<T>>) -> Self {
        let mut menu = Self {
            spec,
            entries,
            index: ListIndex::default(),
            visible: Vec::new(),
            list: SuggestionList::new()
                .with_max_visible(spec.max_rows)
                .with_min_width(spec.width),
            filter: String::new(),
            filter_selection: Selection::collapsed(0),
            filter_composition: QueryComposition::new(),
            visible_flag: false,
            inline: false,
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
            self.leaving = Some(self.filter.clone());
        }
        self.visible_flag = visible;
        self.appear.show(visible);
        if visible {
            self.leaving = None;
            self.rebuild_visible();
        } else {
            self.filter.clear();
            self.filter_selection = Selection::collapsed(0);
            self.filter_composition.clear();
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible_flag
    }

    /// Put the popover's top-left corner at `anchor`.
    pub fn set_anchor(&mut self, anchor: Point) {
        self.list.set_anchor(Rect::from_points(anchor, anchor));
    }

    /// Attach the popover to the caret's rect: below it, or above it when
    /// `bounds` (the visible area) has more room there, and moved along it
    /// as far as it must to stay inside `bounds`
    /// ([`place_anchored`](crate::widget::floating_surface::place_anchored)).
    pub fn set_caret(&mut self, caret: Rect, bounds: Option<Rect>) {
        self.list.set_anchor(caret);
        if let Some(bounds) = bounds {
            self.list.set_viewport(bounds);
        }
    }

    /// Whether the query is text the parent keeps in its document (and
    /// passes in with [`Self::set_filter`]) rather than the menu's own
    /// field. An inline menu paints no field and leaves typing to the
    /// parent.
    pub fn set_inline(&mut self, inline: bool) {
        self.inline = inline;
        self.sync_list_chrome();
    }

    pub fn is_inline(&self) -> bool {
        self.inline
    }

    /// Grow the field and width with the reader's text scale; the rows
    /// grow with the theme's type.
    pub fn set_scale(&mut self, scale: f64) {
        self.scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        self.list.set_min_width(self.width());
        self.sync_list_chrome();
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
        self.entry(self.list.highlight()?).map(|entry| &entry.value)
    }

    /// Number of rows matching the current filter.
    pub fn visible_len(&self) -> usize {
        self.visible.len()
    }

    pub fn width(&self) -> f64 {
        self.spec.width * self.scale
    }

    /// The filter field and the gap under it; an inline menu has none.
    fn field_height(&self) -> f64 {
        if self.inline {
            0.0
        } else {
            (FILTER_HEIGHT + FILTER_GAP) * self.scale
        }
    }

    /// The header band and the "No matches" line follow whether the menu
    /// has its own field: an inline menu with nothing to offer shows
    /// nothing at all.
    fn sync_list_chrome(&mut self) {
        self.list.set_header(self.field_height());
        let empty = (!self.inline && self.visible.is_empty())
            .then(|| crate::i18n!("filter-menu-no-matches"));
        self.list.set_status(empty);
    }

    pub fn height(&self) -> f64 {
        self.list.rect().map_or(0.0, |rect| rect.height())
    }

    /// Whether the popover paints at all: an inline menu with nothing to
    /// offer stays out of the way of the text being typed.
    fn shows(&self) -> bool {
        self.visible_flag && !(self.inline && self.visible.is_empty())
    }

    /// Rect the popover paints into at its placement.
    pub fn rect(&self) -> Rect {
        self.list.rect().unwrap_or(Rect::ZERO)
    }

    fn entry(&self, visible_index: usize) -> Option<&MenuEntry<T>> {
        self.visible
            .get(visible_index)
            .and_then(|&i| self.entries.get(i))
    }

    /// Painted rectangle for a visible row inside the scrolled window. Hosts
    /// use this for pointer automation rather than duplicating the geometry.
    pub fn row_rect(&self, visible_index: usize) -> Option<Rect> {
        self.list.row_rect(visible_index)
    }

    /// The visible row under a logical point, sharing the scrolled window
    /// and bounds checks with keyboard selection.
    fn row_at(&self, point: Point) -> Option<usize> {
        // Rows are hit where they are drawn while the menu arrives.
        if !self.shows() {
            return None;
        }
        self.list.row_at(self.drawn.to_local(point))
    }

    /// Highlight the row under the pointer. Hovering the padding leaves the
    /// last row lit. Returns whether the highlight moved.
    pub fn hover_at(&mut self, point: Point) -> bool {
        match self.row_at(point) {
            Some(index) if Some(index) != self.list.highlight() => {
                self.list.set_highlight(Some(index));
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
        self.list.set_highlight(Some(index));
        self.pending = Some(value.clone());
        self.set_visible(false);
        Some(value)
    }

    /// Rank the rows against the filter and start at the top one.
    fn rebuild_visible(&mut self) {
        self.visible = crate::search::ranked_rows(
            &self.index,
            self.filter.trim(),
            self.entries.iter().map(|entry| {
                std::iter::once((entry.label.as_str(), FieldWeight::TITLE)).chain(
                    entry
                        .keywords
                        .iter()
                        .map(|keyword| (*keyword, FieldWeight::KEYWORD)),
                )
            }),
        );
        let rows = self
            .visible
            .iter()
            .filter_map(|&i| self.entries.get(i).map(|entry| entry.suggestion(i)))
            .collect();
        self.list.set_suggestions(rows);
        self.list
            .set_highlight((!self.visible.is_empty()).then_some(0));
        self.sync_list_chrome();
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
        match logical_key {
            Key::Named(key @ (NamedKey::ArrowDown | NamedKey::ArrowUp)) => {
                self.list.move_highlight(*key == NamedKey::ArrowDown);
                true
            }
            Key::Named(NamedKey::Enter | NamedKey::Tab) => {
                let value = self
                    .list
                    .highlight()
                    .and_then(|at| self.entry(at))
                    .map(|entry| entry.value.clone());
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
        let Some(rect) = self.list.rect() else {
            return;
        };
        let anchor = self.list.anchor().center();
        let pose = self.appear.pose(rect, anchor);
        self.drawn = pose;
        let bounds = crate::widget::floating_surface::painted_bounds(rect, theme);
        if let Some(transform) = pose.begin(scene, transform, bounds) {
            self.list.paint(scene, transform, text_renderer, theme);
            self.draw_filter(scene, transform, text_renderer, theme);
            pose.end(scene);
        }
    }

    /// Whether it is still arriving or leaving: a parent that draws it asks
    /// for frames until it rests.
    pub fn is_moving(&self) -> bool {
        self.appear.is_moving()
    }

    /// The filter field in the list's header band; a leaving menu shows the
    /// filter it had.
    fn draw_filter(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(header) = self.list.header_rect() else {
            return;
        };
        let tokens = &theme.tokens;
        let filter_rect = Rect::new(
            header.x0,
            header.y0,
            header.x1,
            header.y0 + FILTER_HEIGHT * self.scale,
        );
        let filter_radius = f64::from(tokens.radii.control);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            tokens.colors.surfaces.base,
            None,
            &RoundedRect::from_rect(filter_rect, filter_radius),
        );
        // A field reads as a field on every preset: its rest edge derives
        // from its own fill (HIG §9.5), painted as a crisp 1 px inset stroke.
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            crate::surface::edge_on(tokens.colors.surfaces.base),
            None,
            &crate::surface::edge_outline(filter_rect, filter_radius),
        );
        let leaving = self.leaving.clone();
        let text = leaving.as_deref().unwrap_or(&self.filter);
        let selection = if leaving.is_some() {
            Selection::collapsed(text.len())
        } else {
            self.filter_selection
        };
        self.filter_composition.draw(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            QueryView {
                text,
                selection: &selection,
                placeholder: &crate::i18n!(self.spec.placeholder),
                show_caret: leaving.is_none(),
            },
            filter_rect.inset((-6.0, 0.0)),
            &theme.text(TextRole::Caption),
            QueryInk::of(&tokens.colors),
        );
    }
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
                        .with_state(
                            AccessState::new().selected(Some(position) == self.list.highlight()),
                        )
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
