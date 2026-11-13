//! The emoji picker every app opens: to react to something, to put an emoji
//! in what it writes.
//!
//! A compact panel: the search field (focused), Unicode's groups as tabs,
//! and a grid that lists the workspace's own emoji first, with an "Add
//! emoji" tile for whoever may add them ([`crate::workspace_emoji`]), then
//! the person's recently used emoji, then the chosen group; while a query
//! is typed, the matches (the workspace's first). Everything is reachable
//! from the keyboard: typing searches, the arrows move the highlight, Tab
//! changes the group, Enter picks, Shift+Enter (a right click, a long press)
//! offers the skin tones of an emoji that takes them, Escape closes.
//!
//! A pick is [`PickerEvent::Picked`]: the emoji itself, or a workspace
//! emoji written `:name:`; it joins the recents
//! ([`crate::emoji_catalogue::record_recent`]). The Unicode emoji come from
//! the toolkit's own catalogue ([`crate::emoji_catalogue`]); the workspace's
//! emoji are added to it, and the person's recents come first.
//!
//! Hosts: a retained view puts it in a [`super::Popover`]
//! ([`EmojiPicker::new`] lays out at its own size); a canvas that paints by
//! hand frames it ([`EmojiPicker::framed`], or [`EmojiPicker::sheet`] for the
//! desk's own wider card), places it with [`EmojiPicker::open_at`], paints it
//! with [`Widget::draw`] and routes the window's events to
//! [`Widget::handle_event`] while it is open. A framed card is sized by the
//! viewport it is placed in: on a small display it is narrower, its grid
//! shorter and its group tabs over several rows, and a display too short for
//! all of its chrome drops the hint line and then the tabs, so the card
//! always fits.

use std::collections::HashMap;

use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Vec2};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, MouseButton, TouchPhase, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::a11y::{AccessNode, AccessState, Politeness, Role};
use crate::emoji_catalogue::{self, CatalogueGroup};
use crate::num::{f32_of_f64, f64_of_usize, usize_of_f64};
use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::time::{Duration, Instant};
use crate::tokens::TextRole;
use crate::widget::DrawChild;
use crate::widget::{
    ControlSize, EventCtx, ModelPath, ScrollRegion, SearchField, SegmentedFace, SegmentedStyle,
    TaffyTreeExt, ToggleGroup, Widget,
};
use crate::workspace_emoji::Adding;

/// Matches listed for a query at most.
const SEARCH_RESULTS: usize = 96;
/// A press held this long offers the skin tones instead of picking.
const LONG_PRESS: Duration = Duration::from_millis(500);
/// Rows of the grid shown at once.
const GRID_ROWS: usize = 6;
/// Columns of a popover's grid.
const COLUMNS: usize = 8;

/// What the picker asks of its host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PickerEvent {
    /// The person picked this emoji (a workspace emoji as `:name:`).
    Picked(String),
    /// The person closed it (Escape, a press outside it) without a pick.
    Dismissed,
}

/// One cell of the grid.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Item {
    Emoji(String),
    /// "Add emoji": choose a picture to add to the workspace's emoji.
    AddCustom,
}

/// What a point on the panel is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hit {
    Field,
    Tab(usize),
    Item(usize),
    Tone(usize),
    Panel,
    Outside,
}

/// A run of the grid under a heading.
#[derive(Clone, Debug)]
struct Section {
    heading: Option<String>,
    start: usize,
    end: usize,
}

/// The skin tones of one emoji, over the grid.
#[derive(Clone, Debug)]
struct Tones {
    forms: Vec<String>,
    selected: usize,
    /// Tones to a row: the arrow keys walk them as they are drawn.
    columns: usize,
    rect: Rect,
    cells: Vec<Rect>,
}

/// The picker's geometry in one theme, with the panel's top-left at the
/// origin.
#[derive(Clone, Debug, Default)]
struct Geometry {
    size: (f64, f64),
    field: Rect,
    /// The row the group tabs share, and each tab in it.
    tab_strip: Rect,
    tabs: Vec<Rect>,
    grid: Rect,
    footer: Rect,
    cell: f64,
    /// Columns of the grid.
    columns: usize,
    /// Tabs that share a row of the group strip, and a tab's height.
    tab_columns: usize,
    tab_height: f64,
    /// Each item's cell and each heading's top, in grid content
    /// coordinates.
    cells: Vec<Rect>,
    headings: Vec<(String, f64)>,
    content_height: f64,
}

/// The shared emoji picker. See the [module docs](self).
#[crate::component::not_a_component(
    part,
    "the panel of the `ui.EmojiPicker` button, which opens it in its popover"
)]
pub struct EmojiPicker {
    framed: bool,
    /// Whether a framed card takes a sheet's width instead of a utility
    /// panel's.
    sheet: bool,
    open: bool,
    /// The panel, in the host's coordinates.
    rect: Rect,
    /// The room a framed card is sized and placed in.
    viewport: Rect,
    /// The caret a framed card stands beside, when the field reported one.
    anchor: Option<Rect>,
    search: SearchField,
    query: String,
    group: usize,
    groups: Vec<CatalogueGroup>,
    items: Vec<Item>,
    sections: Vec<Section>,
    selected: usize,
    hover: Option<Hit>,
    press: Option<(Hit, Instant)>,
    tones: Option<Tones>,
    events: Vec<PickerEvent>,
    geometry: Geometry,
    scroll: ScrollRegion,
    theme: Theme,
    /// What the grid was built from: the catalogue's arrival, the
    /// workspace's emoji and the last picture added to them.
    built_from: (u64, u64, u64),
    /// Why the last picture chosen with "Add emoji" was not added, in
    /// place of the footer's name until the person does something else.
    notice: Option<String>,
    /// The count of pictures added ([`crate::workspace_emoji::EmojiSource::last_added`])
    /// last shown.
    shown_added: u64,
    cursor: Point,
    dirty: bool,
}

impl Default for EmojiPicker {
    fn default() -> Self {
        Self::new()
    }
}

impl EmojiPicker {
    /// A picker for a popover: its content, open, at its own size.
    #[must_use]
    pub fn new() -> Self {
        let mut picker = Self {
            framed: false,
            sheet: false,
            open: true,
            rect: Rect::ZERO,
            viewport: Rect::ZERO,
            anchor: None,
            search: SearchField::new().without_clear(),
            query: String::new(),
            group: 0,
            groups: Vec::new(),
            items: Vec::new(),
            sections: Vec::new(),
            selected: 0,
            hover: None,
            press: None,
            tones: None,
            events: Vec::new(),
            geometry: Geometry::default(),
            scroll: ScrollRegion::vertical().with_overlaid_bars(),
            theme: Theme::dark(),
            built_from: (u64::MAX, u64::MAX, u64::MAX),
            notice: None,
            shown_added: 0,
            cursor: Point::ZERO,
            dirty: true,
        };
        picker.reset();
        picker
    }

    /// A picker that paints its own floating panel and is placed by its
    /// host ([`Self::open_at`]); it starts closed.
    #[must_use]
    pub fn framed() -> Self {
        let mut picker = Self::new();
        picker.framed = true;
        picker.open = false;
        picker
    }

    /// A framed picker at a sheet's width ([`crate::tokens::Metrics::BASE`]'s
    /// `panel_width_sheet`): the desk's own card over a whole session, where
    /// a wider grid shows more emoji before it scrolls.
    #[must_use]
    pub fn sheet() -> Self {
        let mut picker = Self::framed();
        picker.sheet = true;
        picker
    }

    /// The panel's size in `theme`.
    #[must_use]
    pub fn size(&self, theme: &Theme) -> (f64, f64) {
        if self.framed {
            self.geometry.size
        } else {
            geometry_of(theme, false, false, Rect::ZERO, 0).size
        }
    }

    /// The caret the card stands beside, when the field reported one.
    #[must_use]
    pub fn anchor(&self) -> Option<Rect> {
        self.anchor
    }

    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Open beside `anchor` (below it, or above where the window ends
    /// first), inside `viewport`; centred a fifth of the way down the
    /// viewport when the field reported no caret. Search empty and focused,
    /// the first group chosen.
    pub fn open_at(&mut self, anchor: Option<Rect>, viewport: Rect, theme: &Theme) {
        self.theme = theme.clone();
        self.viewport = viewport;
        self.anchor = anchor;
        self.open = true;
        self.reset();
    }

    /// Move beside `anchor` inside `viewport`, in `theme`, keeping what is
    /// typed and chosen: a host placing an open picker on every frame, as
    /// its window resizes.
    pub fn place_at(&mut self, anchor: Option<Rect>, viewport: Rect, theme: &Theme) {
        let moved = self.anchor != anchor || self.viewport != viewport || self.theme != *theme;
        self.anchor = anchor;
        self.viewport = viewport;
        self.theme = theme.clone();
        if moved {
            self.layout();
        }
    }

    /// Step a glide after a flick to this frame, once per frame: whether the
    /// open grid moved.
    pub fn advance_scroll(&mut self) -> bool {
        if !self.open {
            return false;
        }
        let moved = self.scroll.advance();
        if moved {
            self.press = None;
            self.dirty = true;
        }
        moved
    }

    /// Open again where it was, as new: search empty and focused, the first
    /// group chosen (a popover showing it again).
    pub fn reopen(&mut self) {
        self.open = true;
        self.reset();
    }

    /// Close without a pick.
    pub fn close(&mut self) {
        self.open = false;
        self.press = None;
        self.hover = None;
        self.tones = None;
        self.scroll.cancel_interaction();
        self.search.set_focused(false);
        self.dirty = true;
    }

    /// What happened since the last call, oldest first.
    pub fn take_events(&mut self) -> Vec<PickerEvent> {
        std::mem::take(&mut self.events)
    }

    /// The panel, in the host's coordinates.
    #[must_use]
    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// What is typed in the search field.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Search for `query`, as typing it does.
    pub fn set_query(&mut self, query: &str) {
        self.search.set_query(query);
        query.clone_into(&mut self.query);
        self.rebuild();
    }

    /// The grid's emoji in order (`None` for the "Add emoji" tile).
    #[must_use]
    pub fn items(&self) -> Vec<Option<&str>> {
        self.items
            .iter()
            .map(|item| match item {
                Item::Emoji(text) => Some(text.as_str()),
                Item::AddCustom => None,
            })
            .collect()
    }

    /// The highlighted emoji.
    #[must_use]
    pub fn selected(&self) -> Option<&str> {
        match self.items.get(self.selected)? {
            Item::Emoji(text) => Some(text),
            Item::AddCustom => None,
        }
    }

    /// The skin tones offered now, when they are.
    #[must_use]
    pub fn tone_forms(&self) -> Option<&[String]> {
        self.tones.as_ref().map(|tones| tones.forms.as_slice())
    }

    /// Where item `index` is drawn on screen, when it is in view.
    #[must_use]
    pub fn item_rect(&self, index: usize) -> Option<Rect> {
        let cell = self.screen_cell(index)?;
        let shown = self.scroll.content_rect();
        (cell.y0 >= shown.y0 - 0.5 && cell.y1 <= shown.y1 + 0.5).then_some(cell)
    }

    /// The catalogue group shown, an index into its groups.
    #[must_use]
    pub fn group(&self) -> usize {
        self.group
    }

    /// Why the last picture chosen with "Add emoji" was not added, and
    /// where the picker says so: for a host that names the picker's parts
    /// to assistive technology itself.
    #[must_use]
    pub fn notice(&self) -> Option<(&str, Rect)> {
        let notice = self.notice.as_deref().filter(|_| self.open)?;
        Some((notice, self.on_screen(self.geometry.footer)))
    }

    /// Where group tab `index` is on screen.
    #[must_use]
    pub fn tab_rect(&self, index: usize) -> Option<Rect> {
        self.geometry
            .tabs
            .get(index)
            .map(|tab| self.on_screen(*tab))
    }

    fn reset(&mut self) {
        self.query.clear();
        self.search.set_query("");
        let _ = self.search.take_dirty();
        self.search
            .set_placeholder(&crate::i18n::widget_format("emoji-chooser-search", &[]));
        self.search.set_focused(true);
        self.group = 0;
        self.tones = None;
        self.press = None;
        self.hover = None;
        self.events.clear();
        self.notice = None;
        self.scroll.set_offset(Vec2::ZERO);
        self.rebuild();
        // What was added before it opened is not news.
        self.shown_added = self.built_from.2;
    }

    fn on_screen(&self, rect: Rect) -> Rect {
        rect + self.rect.origin().to_vec2()
    }

    /// What the grid is built from now: moves when the catalogue arrives,
    /// the workspace's emoji change or a picture chosen to add settled.
    fn sources() -> (u64, u64, u64) {
        let source = crate::workspace_emoji::current();
        (
            emoji_catalogue::CATALOGUE.generation(),
            source.as_ref().map_or(0, |source| source.revision()),
            source.map_or(0, |source| source.last_added().0),
        )
    }

    /// Show what became of a picture chosen to add since the grid was
    /// built: the new emoji selected, or why it was not added.
    fn show_added(&mut self) {
        let Some((_, Some(adding))) =
            crate::workspace_emoji::current().map(|source| source.last_added())
        else {
            return;
        };
        match adding {
            Adding::Added(name) => {
                let written = format!(":{name}:");
                if let Some(at) = self
                    .items
                    .iter()
                    .position(|item| *item == Item::Emoji(written.clone()))
                {
                    self.selected = at;
                    self.scroll_selected_into_view();
                }
                self.notice = None;
            }
            Adding::Failed(reason) => {
                if let Some(at) = self.items.iter().position(|item| *item == Item::AddCustom) {
                    self.selected = at;
                }
                self.notice = Some(crate::i18n::widget_format(
                    "emoji-chooser-add-failed",
                    &[("reason", reason.as_str().into())],
                ));
            }
        }
        self.dirty = true;
    }

    fn rebuild(&mut self) {
        self.built_from = Self::sources();
        let catalogue = emoji_catalogue::catalogue();
        self.groups = catalogue
            .map(|catalogue| catalogue.groups())
            .unwrap_or_default();
        self.group = self.group.min(self.groups.len().saturating_sub(1));
        let source = crate::workspace_emoji::current();
        let workspace = source
            .as_ref()
            .map_or_else(std::rc::Rc::default, |source| source.emoji());
        let can_manage = source.as_ref().is_some_and(|source| source.can_manage());
        let t = |key: &str| crate::i18n::widget_format(key, &[]);
        self.items.clear();
        self.sections.clear();
        let mut section = |items: &mut Vec<Item>, heading: Option<String>, more: Vec<Item>| {
            if more.is_empty() {
                return;
            }
            let start = items.len();
            items.extend(more);
            self.sections.push(Section {
                heading,
                start,
                end: items.len(),
            });
        };
        let query = self.query.trim().to_owned();
        if query.is_empty() {
            let mut custom: Vec<Item> = workspace
                .iter()
                .map(|emoji| Item::Emoji(format!(":{}:", emoji.name)))
                .collect();
            if can_manage {
                custom.push(Item::AddCustom);
            }
            section(&mut self.items, Some(t("emoji-chooser-workspace")), custom);
            section(
                &mut self.items,
                Some(t("emoji-chooser-recent")),
                emoji_catalogue::recents()
                    .into_iter()
                    .map(Item::Emoji)
                    .collect(),
            );
            if let (Some(catalogue), Some(group)) = (catalogue, self.groups.get(self.group)) {
                section(
                    &mut self.items,
                    Some(group.label.to_owned()),
                    catalogue
                        .group_emoji(self.group)
                        .into_iter()
                        .map(|emoji| Item::Emoji(emoji.to_owned()))
                        .collect(),
                );
            }
        } else {
            let found: Vec<Item> = workspace
                .search(&query)
                .into_iter()
                .map(|emoji| Item::Emoji(format!(":{}:", emoji.name)))
                .chain(
                    catalogue
                        .map(|catalogue| catalogue.search(&query, SEARCH_RESULTS))
                        .unwrap_or_default()
                        .into_iter()
                        .map(|emoji| Item::Emoji(emoji.to_owned())),
                )
                .collect();
            section(&mut self.items, None, found);
        }
        // The first emoji, not the add tile: Enter right after opening picks
        // an emoji rather than asking for a file.
        self.selected = self
            .items
            .iter()
            .position(|item| matches!(item, Item::Emoji(_)))
            .unwrap_or(0);
        self.tones = None;
        self.press = None;
        self.scroll.set_offset(Vec2::ZERO);
        self.layout();
    }

    fn layout(&mut self) {
        let mut geometry = geometry_of(
            &self.theme,
            self.framed,
            self.sheet,
            self.viewport,
            self.groups.len(),
        );
        if self.framed {
            // A framed card is its host's to place: beside the caret, and
            // inside the viewport even when it is taller than either side of
            // it.
            self.rect = framed_card(
                self.anchor,
                geometry.size,
                self.viewport,
                f64::from(self.theme.tokens.spacing.s4),
                f64::from(self.theme.tokens.spacing.s2),
            );
        }
        // Every group gets an equal share of its column: their icons are all
        // one glyph wide, and the strip never runs past the panel.
        let strip = geometry.tab_strip;
        let columns = geometry.tab_columns.max(1);
        let share = strip.width() / f64_of_usize(columns);
        geometry.tabs = if strip.height() > 0.0 {
            (0..self.groups.len())
                .map(|i| {
                    let column = f64_of_usize(i % columns);
                    let row = f64_of_usize(i / columns);
                    let x0 = strip.x0 + share * column;
                    let y0 = strip.y0 + geometry.tab_height * row;
                    Rect::new(x0, y0, x0 + share, y0 + geometry.tab_height)
                })
                .collect()
        } else {
            // A card too short for the tabs keeps none: the keys still walk
            // the groups.
            Vec::new()
        };
        let (cells, headings, height) =
            self.layout_cells(geometry.cell, geometry.columns, &self.theme);
        geometry.cells = cells;
        geometry.headings = headings;
        geometry.content_height = height;
        let grid = self.on_screen(geometry.grid);
        self.scroll.set_viewport(grid, &self.theme);
        self.scroll.set_content_size(grid.width(), height);
        let field = self.on_screen(geometry.field);
        let theme = self.theme.clone();
        self.search.apply_layout_external(field, &theme);
        self.geometry = geometry;
        self.dirty = true;
    }

    fn layout_cells(
        &self,
        cell: f64,
        per_row: usize,
        theme: &Theme,
    ) -> (Vec<Rect>, Vec<(String, f64)>, f64) {
        let per_row = per_row.max(1);
        let heading_h = f64::from(theme.tokens.metrics.selector_height);
        let mut cells = vec![Rect::ZERO; self.items.len()];
        let mut headings = Vec::new();
        let mut y = 0.0;
        for section in &self.sections {
            if let Some(heading) = &section.heading {
                headings.push((heading.clone(), y));
                y += heading_h;
            }
            for (n, index) in (section.start..section.end).enumerate() {
                let row = f64_of_usize(n / per_row);
                let col = f64_of_usize(n % per_row);
                if let Some(slot) = cells.get_mut(index) {
                    *slot = Rect::new(
                        col * cell,
                        y + row * cell,
                        (col + 1.0) * cell,
                        y + (row + 1.0) * cell,
                    );
                }
            }
            y += f64_of_usize((section.end - section.start).div_ceil(per_row)) * cell;
        }
        (cells, headings, y)
    }

    /// Item `index`'s cell on screen, scrolled.
    fn screen_cell(&self, index: usize) -> Option<Rect> {
        let cell = self.geometry.cells.get(index)?;
        let shown = self.scroll.content_rect();
        let offset = self.scroll.offset();
        Some(*cell + Vec2::new(shown.x0 - offset.x, shown.y0 - offset.y))
    }

    fn scroll_selected_into_view(&mut self) {
        let Some(cell) = self.geometry.cells.get(self.selected).copied() else {
            return;
        };
        // A section's first row keeps its heading in view.
        let heading = self
            .sections
            .iter()
            .any(|s| s.start == self.selected && s.heading.is_some());
        let top = if heading {
            cell.y0 - f64::from(self.theme.tokens.metrics.selector_height)
        } else {
            cell.y0
        };
        self.scroll
            .scroll_into_view(Rect::new(cell.x0, top, cell.x1, cell.y1));
    }

    /// Show catalogue group `group` (an index into its groups), as its tab
    /// does.
    pub fn select_group(&mut self, group: usize) {
        let count = self.groups.len();
        if count == 0 {
            return;
        }
        self.group = group % count;
        if !self.query.is_empty() {
            self.query.clear();
            self.search.set_query("");
        }
        self.rebuild();
        // The group's own section, below the workspace's and the recents.
        if let Some(section) = self.sections.last() {
            self.selected = section.start;
            let top = self.geometry.headings.last().map_or(0.0, |(_, y)| *y);
            self.scroll.set_offset(Vec2::new(0.0, top));
        }
        self.scroll_selected_into_view();
    }

    fn hit(&self, point: Point) -> Hit {
        if let Some(tones) = &self.tones {
            if let Some(i) = tones.cells.iter().position(|r| r.contains(point)) {
                return Hit::Tone(i);
            }
            if tones.rect.contains(point) {
                return Hit::Panel;
            }
        }
        if !self.rect.contains(point) {
            return Hit::Outside;
        }
        let local = point - self.rect.origin().to_vec2();
        let g = &self.geometry;
        if g.field.contains(local) {
            return Hit::Field;
        }
        if let Some(i) = g.tabs.iter().position(|r| r.contains(local)) {
            return Hit::Tab(i);
        }
        if self.scroll.content_rect().contains(point) {
            let content = self.scroll.to_content(point);
            if let Some(i) = g.cells.iter().position(|r| r.contains(content)) {
                return Hit::Item(i);
            }
        }
        Hit::Panel
    }

    /// The item in the row above or below the highlight whose centre is
    /// nearest.
    fn vertical_neighbour(&self, from: usize, down: bool) -> Option<usize> {
        let cells = &self.geometry.cells;
        let current = cells.get(from)?;
        let row_y = cells
            .iter()
            .map(|r| r.y0)
            .filter(|y| {
                if down {
                    *y > current.y0 + 0.5
                } else {
                    *y < current.y0 - 0.5
                }
            })
            .fold(None, |best: Option<f64>, y| {
                Some(best.map_or(y, |b| if down { b.min(y) } else { b.max(y) }))
            })?;
        cells
            .iter()
            .enumerate()
            .filter(|(_, r)| (r.y0 - row_y).abs() < 0.5)
            .min_by(|(_, a), (_, b)| {
                let da = (a.center().x - current.center().x).abs();
                let db = (b.center().x - current.center().x).abs();
                da.total_cmp(&db)
            })
            .map(|(i, _)| i)
    }

    fn move_selection(&mut self, delta: isize) {
        if self.items.is_empty() {
            return;
        }
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(self.items.len() - 1);
        self.scroll_selected_into_view();
    }

    fn move_vertically(&mut self, down: bool, rows: usize) {
        for _ in 0..rows {
            match self.vertical_neighbour(self.selected, down) {
                Some(next) => self.selected = next,
                None => break,
            }
        }
        self.scroll_selected_into_view();
    }

    fn tones_of(text: &str) -> Vec<String> {
        emoji_catalogue::catalogue()
            .map(|catalogue| catalogue.tones(text))
            .unwrap_or_default()
            .into_iter()
            .map(str::to_owned)
            .collect()
    }

    fn takes_tone(text: &str) -> bool {
        emoji_catalogue::catalogue()
            .and_then(|catalogue| catalogue.info(text))
            .is_some_and(|info| info.takes_tone)
    }

    fn open_tones(&mut self, index: usize) -> bool {
        let Some(Item::Emoji(text)) = self.items.get(index) else {
            return false;
        };
        let forms = Self::tones_of(text);
        if forms.is_empty() {
            return false;
        }
        let Some(anchor) = self.screen_cell(index) else {
            return false;
        };
        let selected = emoji_catalogue::catalogue()
            .and_then(|catalogue| catalogue.info(text))
            .and_then(|info| info.tone)
            .map_or(0, |tone| usize::from(tone) + 1);
        let cell = self.geometry.cell;
        let pad = f64::from(self.theme.tokens.spacing.s1);
        let card = self.rect;
        // The tones wrap inside the card: on a narrow one they take two rows
        // rather than running over its edge.
        let room = (card.width() - pad * 2.0).max(cell);
        let columns = usize_of_f64((room / cell).floor()).clamp(1, forms.len());
        let rows = forms.len().div_ceil(columns);
        let width = cell * f64_of_usize(columns) + pad * 2.0;
        let height = (cell * f64_of_usize(rows) + pad * 2.0).min(card.height());
        let x0 = (anchor.center().x - width / 2.0)
            .min(card.x1 - width)
            .max(card.x0);
        let y0 = if anchor.y0 - height >= card.y0 {
            anchor.y0 - height
        } else {
            anchor.y1
        };
        // Above the cell, else below it, and kept on the card either way.
        let y0 = y0.min(card.y1 - height).max(card.y0);
        let cells = (0..forms.len())
            .map(|i| {
                let x = x0 + pad + cell * f64_of_usize(i % columns);
                let y = y0 + pad + cell * f64_of_usize(i / columns);
                Rect::new(x, y, x + cell, y + cell)
            })
            .collect();
        self.selected = index;
        self.tones = Some(Tones {
            forms,
            selected: selected.min(5),
            columns,
            rect: Rect::new(x0, y0, x0 + width, y0 + height),
            cells,
        });
        self.dirty = true;
        true
    }

    fn pick(&mut self, text: String) {
        emoji_catalogue::record_recent(&text);
        self.events.push(PickerEvent::Picked(text));
        if self.framed {
            self.close();
        } else {
            self.tones = None;
        }
    }

    fn activate(&mut self, index: usize) {
        match self.items.get(index).cloned() {
            Some(Item::Emoji(text)) => self.pick(text),
            Some(Item::AddCustom) => {
                if let Some(source) = crate::workspace_emoji::current() {
                    source.choose_and_add();
                }
            }
            None => {}
        }
    }

    fn activate_selected(&mut self) {
        if let Some(tones) = &self.tones {
            if let Some(text) = tones.forms.get(tones.selected).cloned() {
                self.pick(text);
            }
            return;
        }
        self.activate(self.selected);
    }

    fn dismiss(&mut self) {
        self.events.push(PickerEvent::Dismissed);
        self.close();
    }

    fn key(&mut self, key: &Key, shift: bool) -> bool {
        self.notice = None;
        if let Some(tones) = self.tones.as_mut() {
            let last = tones.forms.len().saturating_sub(1);
            match key {
                Key::Named(NamedKey::Escape) => self.tones = None,
                Key::Named(NamedKey::Enter | NamedKey::Space) => self.activate_selected(),
                Key::Named(NamedKey::ArrowLeft) => {
                    tones.selected = tones.selected.saturating_sub(1);
                }
                Key::Named(NamedKey::ArrowRight | NamedKey::Tab) => {
                    tones.selected = (tones.selected + 1).min(last);
                }
                // The tones are drawn in rows, so up and down follow them.
                Key::Named(NamedKey::ArrowUp) => {
                    tones.selected = tones.selected.saturating_sub(tones.columns);
                }
                Key::Named(NamedKey::ArrowDown) => {
                    tones.selected = (tones.selected + tones.columns).min(last);
                }
                Key::Named(NamedKey::Home) => tones.selected = 0,
                Key::Named(NamedKey::End) => tones.selected = last,
                _ => {}
            }
            self.dirty = true;
            return true;
        }
        match key {
            Key::Named(NamedKey::Escape) => self.dismiss(),
            Key::Named(NamedKey::Enter) if shift => {
                let _ = self.open_tones(self.selected);
            }
            Key::Named(NamedKey::Enter) => self.activate_selected(),
            Key::Named(NamedKey::ArrowLeft) => self.move_selection(-1),
            Key::Named(NamedKey::ArrowRight) => self.move_selection(1),
            Key::Named(NamedKey::ArrowUp) => self.move_vertically(false, 1),
            Key::Named(NamedKey::ArrowDown) => self.move_vertically(true, 1),
            Key::Named(NamedKey::PageUp) => self.move_vertically(false, GRID_ROWS),
            Key::Named(NamedKey::PageDown) => self.move_vertically(true, GRID_ROWS),
            Key::Named(NamedKey::Tab) => {
                let count = self.groups.len().max(1);
                let next = if shift {
                    self.group.checked_sub(1).unwrap_or(count - 1)
                } else {
                    self.group + 1
                };
                self.select_group(next);
            }
            _ => return false,
        }
        self.dirty = true;
        true
    }

    fn button(&mut self, state: ElementState, button: MouseButton, point: Point) -> bool {
        let hit = self.hit(point);
        let now = crate::time::now();
        match (state, button) {
            (ElementState::Pressed, MouseButton::Left) => {
                self.notice = None;
                if hit == Hit::Outside {
                    // A popover around an unframed picker closes it itself.
                    if self.framed {
                        self.dismiss();
                    }
                    return false;
                }
                if self.tones.is_some() && !matches!(hit, Hit::Tone(_) | Hit::Panel) {
                    self.tones = None;
                }
                if let Hit::Tab(i) = hit {
                    self.select_group(i);
                }
                self.press = Some((hit, now));
            }
            (ElementState::Released, MouseButton::Left) => {
                let Some((pressed, at)) = self.press.take() else {
                    return true;
                };
                if pressed != hit {
                    return true;
                }
                match hit {
                    Hit::Item(i) => {
                        let long = now.saturating_duration_since(at) >= LONG_PRESS;
                        if !(long && self.open_tones(i)) {
                            self.activate(i);
                        }
                    }
                    Hit::Tone(i) => {
                        if let Some(tones) = self.tones.as_mut() {
                            tones.selected = i;
                        }
                        self.activate_selected();
                    }
                    _ => {}
                }
            }
            (ElementState::Pressed, MouseButton::Right) => {
                if let Hit::Item(i) = hit {
                    let _ = self.open_tones(i);
                }
            }
            _ => {}
        }
        self.dirty = true;
        true
    }

    /// The name of what item `index` is: an emoji's (with its tone), or
    /// "Add emoji".
    fn item_name(&self, index: usize) -> String {
        match self.items.get(index) {
            Some(Item::Emoji(text)) => emoji_catalogue::emoji_name(text),
            Some(Item::AddCustom) => crate::i18n::widget_format("emoji-chooser-add", &[]),
            None => String::new(),
        }
    }

    fn paint_footer(&self, ctx: &mut PaintCtx<'_>, footer: Rect) {
        if let Some(notice) = &self.notice {
            // Wrapped: a reason may take a second line, which the footer's
            // margin holds.
            let style = ctx.theme.text(TextRole::Caption);
            let width = f32_of_f64(footer.width());
            let (_, h) = ctx.text.measure_style(notice, Some(width), &style);
            let color = ctx.theme.tokens.colors.semantic.danger;
            ctx.text(
                notice,
                &style,
                color,
                Point::new(footer.x0, footer.center().y - f64::from(h) / 2.0),
                Some(width),
            );
            return;
        }
        let announced = self
            .tones
            .as_ref()
            .and_then(|tones| tones.forms.get(tones.selected))
            .map(String::as_str)
            .or_else(|| self.selected());
        let name = match announced {
            // A workspace emoji's name as text: written `:name:`, the line
            // would draw its picture a second time.
            Some(text) => tilo_resource::custom_emoji_name(text)
                .map_or_else(|| emoji_catalogue::emoji_name(text), str::to_owned),
            None if self.items.get(self.selected) == Some(&Item::AddCustom) => {
                crate::i18n::widget_format("emoji-chooser-add", &[])
            }
            None => return,
        };
        let gap = f64::from(ctx.theme.tokens.spacing.s2);
        let glyph = Rect::new(footer.x0, footer.y0, footer.x0 + footer.height(), footer.y1);
        if let Some(text) = announced {
            let caption = ctx.theme.text(TextRole::Caption);
            super::reaction_strip::paint_emoji(
                ctx,
                glyph.inset(-2.0),
                text,
                &caption,
                ctx.theme.text_dim,
            );
        }
        let hints = if self.tones.is_none() && announced.is_some_and(Self::takes_tone) {
            vec![super::KeyHint::new(
                ["Shift", "Enter"],
                crate::i18n::widget_format("emoji-chooser-tone-hint", &[]),
            )]
        } else {
            Vec::new()
        };
        let face = super::KeyHintsFace::new(&hints);
        let hint_w = if hints.is_empty() {
            0.0
        } else {
            face.width(ctx.text, ctx.theme)
        };
        let style = ctx
            .theme
            .text(TextRole::Caption)
            .with_weight(TextWeight::Medium);
        let x = glyph.x1 + gap;
        let room = (footer.x1 - x - if hint_w > 0.0 { hint_w + gap } else { 0.0 }).max(0.0);
        let name = ctx
            .text
            .elide_to_width_style(&name, f32_of_f64(room), &style);
        let (_, h) = ctx.text.measure_style(&name, None, &style);
        ctx.text(
            &name,
            &style,
            ctx.theme.text_primary,
            Point::new(x, footer.center().y - f64::from(h) / 2.0),
            None,
        );
        if hint_w > 0.0 {
            let hint_h = super::KeyHintsFace::height(ctx.theme);
            let y = footer.center().y - hint_h / 2.0;
            face.paint(ctx, Rect::new(footer.x1 - hint_w, y, footer.x1, y + hint_h));
        }
    }

    fn paint_grid(&self, ctx: &mut PaintCtx<'_>) {
        let theme = ctx.theme;
        let g = &self.geometry;
        let shown = self.scroll.content_rect();
        ctx.scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            ctx.transform,
            &shown,
        );
        let content = self.scroll.content_transform(ctx.transform);
        let visible = self.scroll.visible_content();
        let heading_style = theme
            .text(TextRole::Caption)
            .with_weight(TextWeight::Medium);
        let heading_h = f64::from(theme.tokens.metrics.selector_height);
        for (heading, y) in &g.headings {
            if *y + heading_h < visible.y0 || *y > visible.y1 {
                continue;
            }
            let (_, h) = ctx.text.measure_style(heading, None, &heading_style);
            let mut c = ctx.with_transform(content);
            c.text(
                heading,
                &heading_style,
                theme.text_dim,
                Point::new(4.0, y + (heading_h - f64::from(h)) / 2.0),
                Some(f32_of_f64(shown.width())),
            );
        }
        let caption = theme.text(TextRole::Caption);
        let radius = f64::from(theme.tokens.radii.control);
        for (i, item) in self.items.iter().enumerate() {
            let Some(cell) = g.cells.get(i).copied() else {
                continue;
            };
            if cell.y1 < visible.y0 || cell.y0 > visible.y1 {
                continue;
            }
            let shape = RoundedRect::from_rect(cell.inset(-2.0), radius);
            if i == self.selected {
                // The armed cell is the panel's overlay lift, like a menu's
                // armed row (HIG 9.3).
                let (lift, _) = crate::widget::floating_surface::armed_row(theme);
                ctx.scene.fill(Fill::NonZero, content, lift, None, &shape);
            } else if self.hover == Some(Hit::Item(i)) {
                let lift = theme
                    .tokens
                    .colors
                    .lift_on(crate::chrome::panel_ground(theme), 0.5);
                ctx.scene.fill(Fill::NonZero, content, lift, None, &shape);
            }
            let mut c = ctx.with_transform(content);
            match item {
                Item::Emoji(text) => {
                    let side = cell.height() * 0.66;
                    let glyph = Rect::from_center_size(cell.center(), (side, side));
                    if tilo_resource::custom_emoji_name(text).is_some()
                        && crate::workspace_emoji::emoji()
                            .resolve(text)
                            .and_then(|emoji| crate::workspace_emoji::emoji().image(&emoji.name))
                            .is_none()
                    {
                        // No picture yet: its name, as much as the cell holds.
                        let shown = c.text.elide_to_width_style(
                            text,
                            f32_of_f64(cell.width() - 4.0),
                            &caption,
                        );
                        super::reaction_strip::paint_emoji(
                            &mut c,
                            cell,
                            &shown,
                            &caption,
                            theme.text_dim,
                        );
                    } else {
                        super::reaction_strip::paint_emoji(
                            &mut c,
                            glyph,
                            text,
                            &caption,
                            theme.text_dim,
                        );
                    }
                }
                Item::AddCustom => {
                    let side = f64::from(theme.tokens.metrics.icon_sm) + 4.0;
                    crate::icons::draw_icon(
                        c.scene,
                        content,
                        Rect::from_center_size(cell.center(), (side, side)),
                        crate::icons::Icon::Plus,
                        theme.text_dim,
                        crate::icons::LUCIDE_DEFAULT_STROKE_PX,
                    );
                }
            }
        }
        if self.items.is_empty() {
            let key = if emoji_catalogue::catalogue().is_none() {
                "emoji-chooser-arriving"
            } else {
                "emoji-chooser-no-results"
            };
            let message = crate::i18n::widget_format(key, &[]);
            let (w, h) = ctx.text.measure_style(&message, None, &caption);
            let at = shown.center();
            ctx.text(
                &message,
                &caption,
                theme.text_dim,
                Point::new(at.x - f64::from(w) / 2.0, at.y - f64::from(h) / 2.0),
                None,
            );
        }
        ctx.scene.pop_layer();
        self.scroll.paint_bars(ctx.scene, ctx.transform, theme);
    }

    fn paint_tones(&self, ctx: &mut PaintCtx<'_>) {
        let Some(tones) = &self.tones else {
            return;
        };
        let theme = ctx.theme;
        let colors = &theme.tokens.colors;
        let radius = f64::from(theme.tokens.radii.control);
        crate::surface::shadow(
            ctx.scene,
            ctx.transform,
            tones.rect,
            radius,
            &theme.tokens.elevation.floating,
        );
        let shape = RoundedRect::from_rect(tones.rect, radius);
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            colors.surfaces.raised,
            None,
            &shape,
        );
        ctx.scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            ctx.transform,
            crate::surface::edge_on(colors.surfaces.raised),
            None,
            &shape,
        );
        let caption = theme.text(TextRole::Caption);
        for (i, (text, cell)) in tones.forms.iter().zip(&tones.cells).enumerate() {
            if i == tones.selected {
                let lift = colors.lift_on(
                    colors.surfaces.raised,
                    crate::widget::floating_surface::OVERLAY_LIFT,
                );
                ctx.scene.fill(
                    Fill::NonZero,
                    ctx.transform,
                    lift,
                    None,
                    &RoundedRect::from_rect(cell.inset(-2.0), radius),
                );
            }
            let side = cell.height() * 0.66;
            super::reaction_strip::paint_emoji(
                ctx,
                Rect::from_center_size(cell.center(), (side, side)),
                text,
                &caption,
                theme.text_dim,
            );
        }
    }

    /// The picker as assistive technology sees it: a dialog with the
    /// search field, the group tabs, a group per grid section (its items in
    /// view) and the skin tones when offered; the highlight is announced.
    fn access_tree(&self) -> AccessNode {
        let t = |key: &str| crate::i18n::widget_format(key, &[]);
        let mut dialog = AccessNode::new(Role::Dialog)
            .with_name(t("emoji-chooser-title"))
            .with_bounds(self.rect)
            .with_state(AccessState::new().modal(self.framed));
        dialog.children.push(
            AccessNode::new(Role::TextInput)
                .with_name(t("emoji-chooser-search"))
                .with_value(self.query.clone())
                .with_bounds(self.on_screen(self.geometry.field))
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .focused(self.tones.is_none()),
                ),
        );
        let tabs = self
            .groups
            .iter()
            .zip(&self.geometry.tabs)
            .enumerate()
            .map(|(i, (group, rect))| {
                AccessNode::new(Role::Tab)
                    .with_name(group.label)
                    .with_bounds(self.on_screen(*rect))
                    .with_hashed_key(("emoji-group", group.id))
                    .with_state(AccessState::new().selected(i == self.group))
            })
            .collect();
        dialog.children.push(
            AccessNode::new(Role::TabBar)
                .with_name(t("emoji-chooser-groups"))
                .with_children(tabs),
        );
        for section in &self.sections {
            let buttons = (section.start..section.end)
                .filter_map(|i| {
                    let rect = self.item_rect(i)?;
                    let key = match self.items.get(i)? {
                        Item::Emoji(text) => ("emoji", text.clone()),
                        Item::AddCustom => ("emoji-add", String::new()),
                    };
                    Some(
                        AccessNode::new(Role::Button)
                            .with_name(self.item_name(i))
                            .with_bounds(rect)
                            .with_hashed_key(key)
                            .with_state(AccessState::new().selected(i == self.selected)),
                    )
                })
                .collect();
            dialog.children.push(
                AccessNode::new(Role::Group)
                    .with_name(
                        section
                            .heading
                            .clone()
                            .unwrap_or_else(|| t("emoji-chooser-results")),
                    )
                    .with_children(buttons),
            );
        }
        if self.items.is_empty() {
            dialog.children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(t("emoji-chooser-no-results"))
                    .with_bounds(self.scroll.content_rect()),
            );
        }
        if let Some(tones) = &self.tones {
            let buttons = tones
                .forms
                .iter()
                .zip(&tones.cells)
                .enumerate()
                .map(|(i, (text, rect))| {
                    AccessNode::new(Role::Button)
                        .with_name(emoji_catalogue::emoji_name(text))
                        .with_bounds(*rect)
                        .with_hashed_key(("emoji-tone", text))
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .focused(i == tones.selected),
                        )
                })
                .collect();
            dialog.children.push(
                AccessNode::new(Role::Group)
                    .with_name(t("emoji-chooser-tones"))
                    .with_bounds(tones.rect)
                    .with_children(buttons),
            );
        }
        if let Some(notice) = &self.notice {
            dialog.children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(notice.clone())
                    .with_bounds(self.on_screen(self.geometry.footer))
                    .with_live(Politeness::Assertive),
            );
        } else if !self.items.is_empty() {
            dialog.children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(self.item_name(self.selected))
                    .with_bounds(self.on_screen(self.geometry.footer))
                    .with_live(Politeness::Polite),
            );
        }
        dialog
    }
}

/// The picker's geometry in `theme`, before its items are laid out: a popover
/// at its own size, or a framed card fitted to `viewport` — narrower than its
/// width class when the viewport is, and with a shorter grid when the height
/// is, so the card always fits the display it opens on.
fn geometry_of(
    theme: &Theme,
    framed: bool,
    sheet: bool,
    viewport: Rect,
    groups: usize,
) -> Geometry {
    let spacing = &theme.tokens.spacing;
    let metrics = &theme.tokens.metrics;
    let pad = if framed { f64::from(spacing.s3) } else { 0.0 };
    let gap = f64::from(spacing.s2);
    // The overlaid scroll bar gets a lane of its own at the grid's right, so
    // it never covers the last column.
    let lane = f64::from(spacing.s2);
    let edge = f64::from(spacing.s4);
    let field_h = super::ButtonFace::height(ControlSize::Regular, theme);
    let tab_height = SegmentedFace::height(ControlSize::Regular, theme);
    let hint_h = f64::from(metrics.selector_height);
    let wanted = f64::from(if sheet {
        metrics.panel_width_sheet
    } else {
        metrics.panel_width_utility
    });
    let (width, available) = if framed {
        (
            wanted.min((viewport.width() - edge * 2.0).max(0.0)),
            (viewport.height() - edge * 2.0).max(0.0),
        )
    } else {
        (wanted, f64::INFINITY)
    };
    let body = (width - pad * 2.0 - lane).max(1.0);
    let cell = if framed {
        f64::from(metrics.panel_row_height).max(1.0)
    } else {
        (body / f64_of_usize(COLUMNS)).floor().max(1.0)
    };
    let columns = if framed {
        usize_of_f64((body / cell).floor()).max(1)
    } else {
        COLUMNS
    };
    let tab_columns = if framed {
        group_columns(body, groups.max(1), tab_height, wanted)
    } else {
        groups.max(1)
    };
    let mut tabs_h = tab_height * f64_of_usize(groups.max(1).div_ceil(tab_columns).max(1));
    let grid_w = cell * f64_of_usize(columns);
    let chrome = |tabs: f64, footer: f64| pad * 2.0 + field_h + gap + tabs + gap + gap + footer;
    // A display too short for all of the card's chrome still holds the field
    // and a row of emoji: the hint line goes first, then the group tabs.
    let mut footer_h = hint_h;
    if chrome(tabs_h, footer_h) + cell > available {
        footer_h = 0.0;
    }
    if chrome(tabs_h, footer_h) + cell > available {
        tabs_h = 0.0;
    }
    let fixed = chrome(tabs_h, footer_h);
    let grid_h = if framed {
        // One row at least: a display too short for the card's chrome still
        // shows emoji, and the card scrolls.
        (cell * f64_of_usize(GRID_ROWS)).min((available - fixed).max(cell))
    } else {
        cell * f64_of_usize(GRID_ROWS)
    };
    let height = if framed {
        (fixed + grid_h).min(available)
    } else {
        fixed + grid_h
    };
    let field = Rect::new(pad, pad, width - pad, pad + field_h);
    let tabs_y = field.y1 + gap;
    let grid_y = tabs_y + tabs_h + gap;
    // Never past the card: on a display shorter than the chrome, the grid
    // holds what is left of it and the person scrolls.
    let reserved = pad + if footer_h > 0.0 { gap + footer_h } else { 0.0 };
    let grid_h = grid_h.min((height - grid_y - reserved).max(0.0));
    let grid_x0 = pad + (body - grid_w) / 2.0;
    let grid = Rect::new(grid_x0, grid_y, grid_x0 + grid_w + lane, grid_y + grid_h);
    let footer = Rect::new(pad, grid.y1 + gap, width - pad, grid.y1 + gap + footer_h);
    Geometry {
        size: (width, height),
        field,
        tab_strip: Rect::new(pad, tabs_y, width - pad, tabs_y + tabs_h),
        tabs: Vec::new(),
        grid,
        footer,
        cell,
        columns,
        tab_columns,
        tab_height,
        cells: Vec::new(),
        headings: Vec::new(),
        content_height: 0.0,
    }
}

/// How many tabs share a row: all of them while the strip fits the card's
/// width class, else as many as fit with the rows kept even.
fn group_columns(room: f64, groups: usize, tab_width: f64, strip_width: f64) -> usize {
    if room >= strip_width {
        return groups;
    }
    let fits = usize_of_f64((room / tab_width.max(1.0)).floor())
        .max(1)
        .min(groups);
    groups.div_ceil(groups.div_ceil(fits))
}

/// Where a framed card of `size` stands: its leading edge on `anchor`'s,
/// `gap` below the caret, or `gap` above it where the viewport ends first;
/// with no anchor, centred a fifth of the way down the viewport. Either way
/// the card stays `edge` inside `viewport`, sliding rather than running off
/// it.
fn framed_card(
    anchor: Option<Rect>,
    (width, height): (f64, f64),
    viewport: Rect,
    edge: f64,
    gap: f64,
) -> Rect {
    let (left, right) = (viewport.x0 + edge, viewport.x1 - edge);
    let (top, bottom) = (viewport.y0 + edge, viewport.y1 - edge);
    let inside = |y0: f64| y0.min(bottom - height).max(top);
    if let Some(caret) = anchor {
        let x0 = caret.x0.min(right - width).max(left);
        let below = caret.y1 + gap;
        let above = caret.y0 - gap - height;
        let y0 = if below + height <= bottom {
            below
        } else if above >= top {
            above
        } else if bottom - caret.y1 >= caret.y0 - top {
            bottom - height
        } else {
            top
        };
        Rect::from_origin_size((x0, inside(y0)), (width, height))
    } else {
        let x0 = (viewport.center().x - width / 2.0)
            .min(right - width)
            .max(left);
        let y0 = inside(viewport.y0 + viewport.height() * 0.18);
        Rect::from_origin_size((x0, y0), (width, height))
    }
}

impl Widget for EmojiPicker {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let (width, height) = self.size(theme);
        let style = Style {
            size: taffy::Size {
                width: taffy::style_helpers::length(f32_of_f64(width)),
                height: taffy::style_helpers::length(f32_of_f64(height)),
            },
            flex_shrink: 0.0,
            ..Style::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let rect = crate::widget::layout_rect(taffy, node, origin);
        if rect != self.rect {
            self.rect = rect;
            self.layout();
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if !self.open {
            return;
        }
        let theme_changed = self.theme != *theme;
        if theme_changed {
            self.theme = theme.clone();
            self.search
                .set_placeholder(&crate::i18n::widget_format("emoji-chooser-search", &[]));
        }
        if Self::sources() != self.built_from {
            let selected = self.selected;
            self.rebuild();
            self.selected = selected.min(self.items.len().saturating_sub(1));
            if self.built_from.2 != self.shown_added {
                self.shown_added = self.built_from.2;
                self.show_added();
            }
        } else if theme_changed || self.geometry.size != self.size(theme) {
            self.layout();
        }
        let local;
        let theme = if self.framed {
            crate::widget::floating_surface::paint(scene, transform, self.rect, theme);
            local = theme.on_ground(crate::chrome::panel_ground(theme));
            &local
        } else {
            theme
        };
        self.search
            .draw_child(scene, transform, text, theme, presence);

        // The group tabs.
        let g = self.geometry.clone();
        if !g.tabs.is_empty() {
            let icons: Vec<&str> = self.groups.iter().map(|group| group.icon).collect();
            let rects: Vec<Rect> = g.tabs.iter().map(|tab| self.on_screen(*tab)).collect();
            let selection = self.query.trim().is_empty().then_some(self.group);
            let hovered = match self.hover {
                Some(Hit::Tab(i)) => Some(i),
                _ => None,
            };
            ToggleGroup::paint(
                scene,
                transform,
                text,
                theme,
                &SegmentedFace::new(&rects, &icons, usize::MAX)
                    .selection(selection)
                    .style(SegmentedStyle::Tabs)
                    .hovered(hovered),
            );
        }
        let mut ctx = PaintCtx::new(scene, transform, text, theme);
        self.paint_grid(&mut ctx);
        let footer = self.on_screen(g.footer);
        if footer.height() > 0.0 {
            self.paint_footer(&mut ctx, footer);
        }
        self.paint_tones(&mut ctx);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        if !self.open {
            return;
        }
        let mut handled = false;
        if !matches!(event, WindowEvent::KeyboardInput { .. }) {
            let theme = self.theme.clone();
            self.scroll.handle_window_event(
                event,
                ctx.scale_factor,
                ctx.modifiers.state(),
                &mut handled,
                &theme,
            );
        }
        if handled {
            self.press = None;
            self.tones = None;
            self.dirty = true;
            *ctx.handled = true;
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                let hover = Some(self.hit(self.cursor));
                if hover != self.hover {
                    self.hover = hover;
                    self.dirty = true;
                }
                if self.hover == Some(Hit::Field) {
                    self.search.handle_event(event, ctx);
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let hit = self.hit(self.cursor);
                if hit == Hit::Field {
                    self.search.handle_event(event, ctx);
                }
                *ctx.handled |= self.button(*state, *button, self.cursor);
                if *ctx.handled {
                    *ctx.focus_target = Some(ctx.node_id);
                }
            }
            WindowEvent::KeyboardInput { event: key, .. } if key.state == ElementState::Pressed => {
                let shift = ctx.modifiers.state().shift_key();
                if self.key(&key.logical_key, shift) {
                    *ctx.handled = true;
                    return;
                }
                self.search.set_focused(true);
                self.search.handle_event(event, ctx);
                *ctx.handled = true;
            }
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_) => {
                self.search.handle_event(event, ctx);
                *ctx.handled = true;
            }
            // A touchscreen presses and picks where a mouse does.
            WindowEvent::Touch(touch) => {
                self.cursor = Point::new(
                    touch.location.x / ctx.scale_factor,
                    touch.location.y / ctx.scale_factor,
                );
                match touch.phase {
                    TouchPhase::Started => {
                        *ctx.handled |=
                            self.button(ElementState::Pressed, MouseButton::Left, self.cursor);
                    }
                    TouchPhase::Ended => {
                        *ctx.handled |=
                            self.button(ElementState::Released, MouseButton::Left, self.cursor);
                    }
                    TouchPhase::Cancelled => {
                        self.press = None;
                        self.dirty = true;
                    }
                    TouchPhase::Moved => {}
                }
                if *ctx.handled {
                    *ctx.focus_target = Some(ctx.node_id);
                }
            }
            // A press held when the window lost the keyboard is no press.
            WindowEvent::Focused(false) => {
                self.press = None;
                self.hover = None;
                self.dirty = true;
            }
            _ => {}
        }
        if self.search.take_dirty().is_some() {
            self.query = self.search.value().to_owned();
            self.rebuild();
        }
    }

    fn needs_redraw(&self) -> bool {
        // A closed picker paints nothing: what it owes is the frame that takes it down, and
        // neither its field nor what its grid is built from shows until it opens again, which
        // rebuilds the grid.
        self.dirty
            || (self.open
                && (self.search.needs_redraw()
                    || self.search.needs_animation_frame()
                    || Self::sources() != self.built_from))
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.search.mark_drawn();
    }

    fn needs_animation_frame(&self) -> bool {
        self.open
            && (self.scroll.needs_animation_frame(&self.theme)
                || self.search.needs_animation_frame())
    }

    /// A glide after a flick scrolls the grid as the wheel does.
    fn update(&mut self, _state: &crate::widget::AppModel, _path: &ModelPath) -> bool {
        self.advance_scroll()
    }

    fn is_focusable(&self) -> bool {
        self.open
    }

    fn set_focus(&mut self, focused: bool) {
        self.search.set_focused(focused && self.tones.is_none());
        self.dirty = true;
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.search.ime_cursor_rect()
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        match self.hit(point) {
            Hit::Field => Some(winit::window::CursorIcon::Text),
            Hit::Tab(_) | Hit::Item(_) | Hit::Tone(_) => Some(winit::window::CursorIcon::Pointer),
            Hit::Panel | Hit::Outside => None,
        }
    }

    fn modal_capture(&self) -> Option<Rect> {
        (self.framed && self.open).then_some(self.rect)
    }

    fn accessibility(&self) -> Option<AccessNode> {
        self.open.then(|| self.access_tree())
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        if !matches!(action, crate::a11y::Action::Activate) {
            return false;
        }
        let Some(key) = key_path.last() else {
            return false;
        };
        let item = self.items.iter().position(|item| {
            let id = match item {
                Item::Emoji(text) => ("emoji", text.clone()),
                Item::AddCustom => ("emoji-add", String::new()),
            };
            crate::a11y::hash_key(&id) == *key
        });
        if let Some(index) = item {
            self.selected = index;
            self.activate(index);
            return true;
        }
        if let Some(group) = self
            .groups
            .iter()
            .position(|group| crate::a11y::hash_key(&("emoji-group", group.id)) == *key)
        {
            self.select_group(group);
            return true;
        }
        false
    }

    fn name(&self) -> &'static str {
        "EmojiPicker"
    }
}

#[cfg(test)]
#[path = "emoji_picker_tests.rs"]
mod tests;
