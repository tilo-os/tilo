//! The emoji picker: Super+. (rebindable) over any text field.
//!
//! The card opens at the focused field's caret — below it, or above it when
//! the display ends first, kept on the display — from the cursor rectangle
//! the field reports through `zwp_text_input_v3`. A field that reports none
//! gets the card centred on the display.
//!
//! A compact card with the search field focused, the Unicode groups as tabs
//! and a grid that lists this user's recents first, then the selected group
//! — or, while a query is typed, the matches. Everything is reachable from
//! the keyboard: typing searches, the arrows move the highlight, Tab changes
//! the group, Enter inserts, Shift+Enter (or a right click, or a long press)
//! opens the skin-tone row of an emoji that takes one, Escape closes.
//!
//! The picker never writes into an application itself. A pick is queued for
//! the host ([`EmojiPicker::take_commits`]), which commits it into the
//! focused field once the field has the keyboard back. The host reports what
//! arrived, and only then does the emoji join the recents — unless the field
//! was a password or another secret, whose content is never remembered.
//!
//! Recents are private to the user and kept wherever the host's
//! [`RecentsStore`] puts them, the same [`emoji::Recents`] model the
//! on-screen keyboard keeps in its own app state.
//!
//! The catalogue, search, skin tones and names come from the shared
//! [`emoji`] crate in the desktop's language.

use ui::time::{Duration, Instant};

use emoji::{Catalogue, Recents, SkinTone};
use ui::a11y::{AccessNode, AccessState, Politeness, Role};
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::text_selection::Selection;
use ui::theme::Theme;
use ui::widget::{
    ControlSize, SearchField, Segment, SegmentedFace, SegmentedStyle, ToggleGroup, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::i18n::{t, t_args};
use crate::text_entry::{self, Edit};
use ui::TextRole;

/// Matches listed for a query at most.
const SEARCH_RESULTS: usize = 96;
/// A press held this long opens the skin-tone row instead of inserting.
const LONG_PRESS: Duration = Duration::from_millis(500);
/// Rows of emoji the grid shows at once.
const GRID_ROWS: usize = 6;
/// Emoji glyph size relative to its cell.
const GLYPH_SCALE: f64 = 0.6;

/// Where the picker keeps this user's recents between sessions. The host
/// decides the medium (a private file natively); the picker only reads
/// once and writes after each delivered pick.
pub trait RecentsStore {
    /// The stored recents; nothing stored reads as none.
    fn load(&self) -> Recents;

    /// Replace what is stored.
    ///
    /// # Errors
    ///
    /// Returns a description of why the recents could not be kept.
    fn save(&self, recents: &Recents) -> Result<(), String>;
}

/// What a point on the card is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hit {
    Field,
    Tab(usize),
    Emoji(usize),
    Tone(usize),
    Card,
    Outside,
}

/// The skin-tone row of one emoji.
#[derive(Clone, Debug)]
struct Tones {
    /// Index of the emoji in the grid.
    anchor: usize,
    /// Its default form, then each tone lightest first.
    forms: Vec<String>,
    selected: usize,
    rect: Rect,
    cells: Vec<Rect>,
}

#[derive(Clone, Copy, Debug)]
struct Press {
    hit: Hit,
    at: Instant,
}

/// A run of the grid under an optional heading.
#[derive(Clone, Debug)]
struct Section {
    heading: Option<String>,
    /// Range into [`EmojiPicker::items`].
    start: usize,
    end: usize,
}

/// Where the last layout put everything.
#[derive(Clone, Debug, Default)]
struct Geometry {
    card: Rect,
    field: Rect,
    tabs: Vec<Rect>,
    grid: Rect,
    footer: Rect,
    /// Each item's cell, in grid content coordinates (origin at the grid's
    /// top-left, before scrolling).
    cells: Vec<Rect>,
    /// Each heading and its baseline row, in content coordinates.
    headings: Vec<(String, f64)>,
    content_height: f64,
    cell: f64,
}

/// Shaped footer content; geometry uses the same typography as painting.
struct FooterText {
    name: String,
    hint: Vec<String>,
    hint_width: f64,
    hint_height: f64,
    line_height: f64,
    height: f64,
    stacked: bool,
}

/// The picker's state and geometry, routed and painted by the overlay host.
pub struct EmojiPicker {
    open: bool,
    catalogue: &'static Catalogue,
    recents: Recents,
    store: Option<Box<dyn RecentsStore>>,
    recents_loaded: bool,
    query: String,
    caret: Selection,
    search: SearchField,
    group: usize,
    items: Vec<String>,
    sections: Vec<Section>,
    selected: usize,
    scroll: f64,
    tones: Option<Tones>,
    press: Option<Press>,
    hover: Option<Hit>,
    commits: Vec<String>,
    viewport: Rect,
    /// The focused field's caret in desktop coordinates, when it reported
    /// one; the card is placed against it.
    anchor: Option<Rect>,
    geometry: Geometry,
    spacing: ui::tokens::Spacing,
    metrics: ui::tokens::Metrics,
    footer_height: f64,
    group_width: f64,
    group_strip_width: f64,
    tab_columns: usize,
}

impl Default for EmojiPicker {
    fn default() -> Self {
        Self::new()
    }
}

impl EmojiPicker {
    pub fn new() -> Self {
        Self {
            open: false,
            catalogue: Catalogue::english(),
            recents: Recents::default(),
            store: None,
            recents_loaded: false,
            query: String::new(),
            caret: Selection::collapsed(0),
            search: SearchField::new().without_clear(),
            group: 0,
            items: Vec::new(),
            sections: Vec::new(),
            selected: 0,
            scroll: 0.0,
            tones: None,
            press: None,
            hover: None,
            commits: Vec::new(),
            viewport: Rect::ZERO,
            anchor: None,
            geometry: Geometry::default(),
            spacing: ui::tokens::Spacing::BASE,
            metrics: ui::tokens::Metrics::BASE,
            footer_height: f64::from(ui::tokens::Metrics::BASE.bar_height),
            group_width: f64::from(ui::tokens::Metrics::BASE.bar_height),
            group_strip_width: f64::from(ui::tokens::Metrics::BASE.panel_width_sheet),
            tab_columns: 1,
        }
    }

    /// Keep the recents in `store` from now on, reading what it holds.
    pub fn attach_store(&mut self, store: Box<dyn RecentsStore>) {
        self.store = Some(store);
        self.recents_loaded = false;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Open centred over `viewport` in the desktop's language, search
    /// focused and empty, the first group selected.
    pub fn open(&mut self, viewport: Rect, locale: &str) {
        self.open_at(viewport, None, locale);
    }

    /// [`Self::open`], placed at `caret` — the focused field's cursor
    /// rectangle in desktop coordinates — when there is one.
    pub fn open_at(&mut self, viewport: Rect, caret: Option<Rect>, locale: &str) {
        self.anchor = caret;
        if !self.recents_loaded {
            self.recents_loaded = true;
            if let Some(store) = &self.store {
                self.recents = store.load();
            }
        }
        self.catalogue = Catalogue::for_locale(locale);
        self.open = true;
        self.viewport = viewport;
        self.query.clear();
        self.caret = Selection::collapsed(0);
        self.search.set_query("");
        self.search.set_placeholder(&t("emoji-picker-search"));
        self.search.set_focused(true);
        self.group = 0;
        self.tones = None;
        self.press = None;
        self.hover = None;
        self.rebuild();
    }

    /// Close without inserting anything.
    pub fn close(&mut self) {
        self.open = false;
        self.tones = None;
        self.press = None;
        self.hover = None;
        self.search.set_focused(false);
    }

    /// The emoji picked since the last call, oldest first.
    pub fn take_commits(&mut self) -> Vec<String> {
        std::mem::take(&mut self.commits)
    }

    /// The compositor committed `text` into a field. It joins the recents
    /// unless the field was secret.
    pub fn note_delivered(&mut self, text: &str, sensitive: bool) {
        if sensitive {
            return;
        }
        if !self.recents_loaded {
            self.recents_loaded = true;
            if let Some(store) = &self.store {
                self.recents = store.load();
            }
        }
        self.recents.record(text);
        if let Some(store) = &self.store
            && let Err(error) = store.save(&self.recents)
        {
            tracing::warn!(%error, "emoji recents could not be saved");
        }
        if self.open {
            self.rebuild();
        }
    }

    /// Most recent first.
    pub fn recents(&self) -> &[String] {
        self.recents.items()
    }

    /// The grid's emoji in display order.
    pub fn items(&self) -> &[String] {
        &self.items
    }

    /// The highlighted emoji.
    pub fn selected(&self) -> Option<&str> {
        self.items.get(self.selected).map(String::as_str)
    }

    /// The skin-tone row's forms, when it is open.
    pub fn tone_forms(&self) -> Option<&[String]> {
        self.tones.as_ref().map(|tones| tones.forms.as_slice())
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// The field caret the open card is placed against.
    pub fn anchor(&self) -> Option<Rect> {
        self.anchor
    }

    pub fn card_rect(&self) -> Rect {
        if self.open {
            self.geometry.card
        } else {
            Rect::ZERO
        }
    }

    /// The grid rect of item `index` on screen, if it is scrolled into view.
    pub fn emoji_rect(&self, index: usize) -> Option<Rect> {
        let rect = self.screen_cell(index)?;
        (rect.y0 >= self.geometry.grid.y0 - 0.5 && rect.y1 <= self.geometry.grid.y1 + 0.5)
            .then_some(rect)
    }

    // --- content ---------------------------------------------------------

    fn rebuild(&mut self) {
        let catalogue = self.catalogue;
        self.items.clear();
        self.sections.clear();
        let query = self.query.trim();
        if query.is_empty() {
            if !self.recents.is_empty() {
                self.items.extend(self.recents.items().iter().cloned());
                self.sections.push(Section {
                    heading: Some(t("emoji-picker-recent")),
                    start: 0,
                    end: self.items.len(),
                });
            }
            if let Some(group) = catalogue.groups().get(self.group) {
                let start = self.items.len();
                self.items.extend(
                    catalogue
                        .group_emoji(group)
                        .iter()
                        .map(|emoji| emoji.text().to_owned()),
                );
                self.sections.push(Section {
                    heading: Some(group.label().to_owned()),
                    start,
                    end: self.items.len(),
                });
            }
        } else {
            self.items.extend(
                catalogue
                    .search(query, SEARCH_RESULTS)
                    .into_iter()
                    .map(|emoji| emoji.text().to_owned()),
            );
            self.sections.push(Section {
                heading: None,
                start: 0,
                end: self.items.len(),
            });
        }
        self.selected = 0;
        self.scroll = 0.0;
        self.tones = None;
        self.layout();
    }

    fn select_group(&mut self, group: usize) {
        let count = self.catalogue.groups().len();
        if count == 0 {
            return;
        }
        self.group = group.checked_rem(count).unwrap_or(0);
        if !self.query.is_empty() {
            self.query.clear();
            self.caret = Selection::collapsed(0);
            self.search.set_query("");
        }
        self.rebuild();
        // The group's own section, below the recents.
        if let Some(section) = self.sections.last() {
            self.selected = section.start;
            self.scroll = self
                .geometry
                .headings
                .last()
                .map_or(0.0, |(_, y)| *y)
                .min(self.max_scroll());
        }
    }

    fn takes_tone(&self, text: &str) -> bool {
        self.catalogue
            .lookup(text)
            .is_some_and(|found| found.emoji.takes_skin_tone())
    }

    fn open_tones(&mut self, index: usize) -> bool {
        let Some(text) = self.items.get(index).cloned() else {
            return false;
        };
        let Some(found) = self.catalogue.lookup(&text) else {
            return false;
        };
        if !found.emoji.takes_skin_tone() {
            return false;
        }
        let base = found.emoji.text();
        let forms: Vec<String> = std::iter::once(None)
            .chain(SkinTone::ALL.into_iter().map(Some))
            .filter_map(|tone| self.catalogue.with_skin_tone(base, tone).map(str::to_owned))
            .collect();
        let selected = found
            .tone
            .and_then(|tone| SkinTone::ALL.iter().position(|t| *t == tone))
            .map_or(0, |i| i.saturating_add(1));
        self.selected = index;
        self.tones = Some(Tones {
            anchor: index,
            forms,
            selected,
            rect: Rect::ZERO,
            cells: Vec::new(),
        });
        self.layout_tones();
        true
    }

    fn commit(&mut self, text: String) {
        self.commits.push(text);
        self.close();
    }

    fn commit_selected(&mut self) {
        if let Some(tones) = &self.tones {
            if let Some(text) = tones.forms.get(tones.selected).cloned() {
                self.commit(text);
            }
            return;
        }
        if let Some(text) = self.items.get(self.selected).cloned() {
            self.commit(text);
        }
    }

    // --- geometry --------------------------------------------------------

    fn layout(&mut self) {
        // A new grid must never complete a press on the old geometry/content.
        self.press = None;
        let pad = f64::from(self.spacing.s3);
        let gap = f64::from(self.spacing.s2);
        let edge = f64::from(self.spacing.s4);
        let cell = f64::from(self.metrics.panel_row_height);
        let field_h = f64::from(self.metrics.bar_height);
        let tabs_h = f64::from(self.metrics.bar_height);
        let footer_h = self.footer_height.max(f64::from(self.metrics.bar_height));
        let heading_h = f64::from(self.metrics.selector_height);
        let viewport = self.viewport;
        let width =
            f64::from(self.metrics.panel_width_sheet).min((viewport.width() - edge * 2.0).max(0.0));
        let columns = (((width - pad * 2.0) / cell).floor()).max(1.0);
        let grid_w = columns * cell;
        let group_count = self.catalogue.groups().len().max(1);
        let room = width - pad * 2.0;
        let capacity = usize_from_f64((room / self.group_width.max(1.0)).floor())
            .max(1)
            .min(group_count);
        self.tab_columns = if room >= self.group_strip_width {
            group_count
        } else {
            group_count.div_ceil(group_count.div_ceil(capacity))
        };
        let tabs_h = tabs_h * f64_from_usize(group_count.div_ceil(self.tab_columns));
        let fixed = pad * 2.0 + field_h + gap + tabs_h + gap + gap + footer_h;
        let grid_h = (cell * f64_from_usize(GRID_ROWS))
            .min((viewport.height() - edge * 2.0 - fixed).max(0.0));
        let height = fixed + grid_h;
        let size = (width, height);
        let card = self.anchor.map_or_else(
            || {
                let x0 = viewport.center().x - width / 2.0;
                let y0 = (viewport.y0 + viewport.height() * 0.18)
                    .min(viewport.y1 - height - edge)
                    .max(viewport.y0 + edge);
                Rect::from_origin_size((x0, y0), size)
            },
            |caret| card_at_caret(caret, size, viewport, edge, gap),
        );
        let inner_x0 = card.x0 + pad;
        let inner_x1 = card.x1 - pad;
        let field = Rect::new(inner_x0, card.y0 + pad, inner_x1, card.y0 + pad + field_h);
        let tabs_y0 = field.y1 + gap;
        let tab_w = (inner_x1 - inner_x0) / f64_from_usize(self.tab_columns);
        let tabs = (0..group_count)
            .map(|i| {
                let column = i.checked_rem(self.tab_columns).unwrap_or(0);
                let row = i.checked_div(self.tab_columns).unwrap_or(0);
                let x = inner_x0 + tab_w * f64_from_usize(column);
                let y = tabs_y0 + f64::from(self.metrics.bar_height) * f64_from_usize(row);
                Rect::new(x, y, x + tab_w, y + f64::from(self.metrics.bar_height))
            })
            .collect();
        let grid_x0 = card.center().x - grid_w / 2.0;
        let grid = Rect::new(
            grid_x0,
            tabs_y0 + tabs_h + gap,
            grid_x0 + grid_w,
            tabs_y0 + tabs_h + gap + grid_h,
        );
        let footer = Rect::new(inner_x0, grid.y1 + gap, inner_x1, grid.y1 + gap + footer_h);

        let per_row = usize_from_f64(columns).max(1);
        let mut cells = vec![Rect::ZERO; self.items.len()];
        let mut headings = Vec::new();
        let mut y = 0.0;
        for section in &self.sections {
            if let Some(heading) = &section.heading {
                headings.push((heading.clone(), y));
                y += heading_h;
            }
            for (n, index) in (section.start..section.end).enumerate() {
                let row = f64_from_usize(n.checked_div(per_row).unwrap_or(0));
                let col = f64_from_usize(n.checked_rem(per_row).unwrap_or(0));
                if let Some(slot) = cells.get_mut(index) {
                    *slot = Rect::new(
                        col * cell,
                        y + row * cell,
                        (col + 1.0) * cell,
                        y + (row + 1.0) * cell,
                    );
                }
            }
            let rows = (section.end.saturating_sub(section.start)).div_ceil(per_row);
            y += f64_from_usize(rows) * cell;
        }
        self.geometry = Geometry {
            card,
            field,
            tabs,
            grid,
            footer,
            cells,
            headings,
            content_height: y,
            cell,
        };
        self.search.apply_layout_external(field);
        self.scroll = self.scroll.clamp(0.0, self.max_scroll());
        self.scroll_selected_into_view();
        self.layout_tones();
    }

    fn layout_tones(&mut self) {
        let Some(anchor) = self.tones.as_ref().map(|tones| tones.anchor) else {
            return;
        };
        let Some(anchor_rect) = self.screen_cell(anchor) else {
            return;
        };
        let cell = self.geometry.cell;
        let pad = f64::from(self.spacing.s1);
        let card = self.geometry.card;
        let Some(tones) = self.tones.as_mut() else {
            return;
        };
        let width = cell * f64_from_usize(tones.forms.len()) + pad * 2.0;
        let height = cell + pad * 2.0;
        let x0 = (anchor_rect.center().x - width / 2.0)
            .min(card.x1 - width)
            .max(card.x0);
        // Above the emoji, or below it when the card's top is in the way.
        let y0 = if anchor_rect.y0 - height >= card.y0 {
            anchor_rect.y0 - height
        } else {
            anchor_rect.y1
        };
        tones.rect = Rect::new(x0, y0, x0 + width, y0 + height);
        tones.cells = (0..tones.forms.len())
            .map(|i| {
                let x = x0 + pad + cell * f64_from_usize(i);
                Rect::new(x, y0 + pad, x + cell, y0 + pad + cell)
            })
            .collect();
    }

    fn max_scroll(&self) -> f64 {
        (self.geometry.content_height - self.geometry.grid.height()).max(0.0)
    }

    fn screen_cell(&self, index: usize) -> Option<Rect> {
        let cell = self.geometry.cells.get(index)?;
        let grid = self.geometry.grid;
        Some(cell.with_origin((grid.x0 + cell.x0, grid.y0 + cell.y0 - self.scroll)))
    }

    fn scroll_selected_into_view(&mut self) {
        let Some(cell) = self.geometry.cells.get(self.selected).copied() else {
            return;
        };
        let view = self.geometry.grid.height();
        // A section's first row keeps its heading in view.
        let heading_h = f64::from(self.metrics.selector_height);
        let top = if self
            .sections
            .iter()
            .any(|s| s.start == self.selected && s.heading.is_some())
        {
            cell.y0 - heading_h
        } else {
            cell.y0
        };
        if top < self.scroll {
            self.scroll = top;
        } else if cell.y1 > self.scroll + view {
            self.scroll = cell.y1 - view;
        }
        self.scroll = self.scroll.clamp(0.0, self.max_scroll());
    }

    fn hit(&self, p: Point) -> Hit {
        if let Some(tones) = &self.tones {
            if let Some(i) = tones.cells.iter().position(|r| r.contains(p)) {
                return Hit::Tone(i);
            }
            if tones.rect.contains(p) {
                return Hit::Card;
            }
        }
        let g = &self.geometry;
        if !g.card.contains(p) {
            return Hit::Outside;
        }
        if g.field.contains(p) {
            return Hit::Field;
        }
        if let Some(i) = g.tabs.iter().position(|r| r.contains(p)) {
            return Hit::Tab(i);
        }
        if g.grid.contains(p) {
            let local = Point::new(p.x - g.grid.x0, p.y - g.grid.y0 + self.scroll);
            if let Some(i) = g.cells.iter().position(|r| r.contains(local)) {
                return Hit::Emoji(i);
            }
        }
        Hit::Card
    }

    /// The item in the row above (`down == false`) or below the highlight
    /// whose centre is nearest horizontally.
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

    // --- input -----------------------------------------------------------

    /// Route an event while the picker is open. It owns the keyboard and the
    /// pointer: whatever it does not act on reaches nothing behind it.
    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: ModifiersState,
        cursor: Option<Point>,
        now: Instant,
    ) {
        if !self.open {
            return;
        }
        match event {
            WindowEvent::KeyboardInput { event: key, .. } if key.state == ElementState::Pressed => {
                self.key(&key.logical_key, modifiers);
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if let Some(p) = cursor {
                    self.button(*state, *button, p, now);
                }
            }
            WindowEvent::Touch(touch) => {
                let p = Point::new(
                    touch.location.x / scale_factor,
                    touch.location.y / scale_factor,
                );
                match touch.phase {
                    TouchPhase::Started => {
                        self.button(ElementState::Pressed, MouseButton::Left, p, now);
                    }
                    TouchPhase::Ended => {
                        self.button(ElementState::Released, MouseButton::Left, p, now);
                    }
                    TouchPhase::Cancelled => self.press = None,
                    TouchPhase::Moved => {}
                }
            }
            WindowEvent::CursorMoved { .. } => {
                self.hover = cursor.map(|p| self.hit(p));
            }
            WindowEvent::Focused(false) => {
                self.press = None;
                self.hover = None;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.press = None;
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * self.geometry.cell,
                    MouseScrollDelta::PixelDelta(p) => p.y / scale_factor,
                };
                self.scroll = (self.scroll - dy).clamp(0.0, self.max_scroll());
                self.tones = None;
            }
            _ => {}
        }
    }

    fn key(&mut self, key: &Key, modifiers: ModifiersState) {
        let shift = modifiers.shift_key();
        if self.tones.is_some() {
            self.tone_key(key);
            return;
        }
        match key {
            Key::Named(NamedKey::Escape) => self.close(),
            Key::Named(NamedKey::Enter) if shift => {
                let _ = self.open_tones(self.selected);
            }
            Key::Named(NamedKey::Enter) => self.commit_selected(),
            Key::Named(NamedKey::ArrowLeft) => self.move_selection(-1),
            Key::Named(NamedKey::ArrowRight) => self.move_selection(1),
            Key::Named(NamedKey::ArrowUp) => self.move_vertically(false, 1),
            Key::Named(NamedKey::ArrowDown) => self.move_vertically(true, 1),
            Key::Named(NamedKey::PageUp) => self.move_vertically(false, GRID_ROWS),
            Key::Named(NamedKey::PageDown) => self.move_vertically(true, GRID_ROWS),
            Key::Named(NamedKey::Tab) => {
                let count = self.catalogue.groups().len();
                let next = if shift {
                    self.group
                        .checked_sub(1)
                        .unwrap_or_else(|| count.saturating_sub(1))
                } else {
                    self.group.saturating_add(1)
                };
                self.select_group(next);
            }
            _ => {
                let edit = text_entry::apply(
                    key,
                    text_entry::Mods::from_state(modifiers),
                    &mut self.query,
                    &mut self.caret,
                );
                if edit == Edit::Changed {
                    self.search.set_query(&self.query);
                    self.rebuild();
                }
            }
        }
    }

    fn tone_key(&mut self, key: &Key) {
        let Some(tones) = self.tones.as_mut() else {
            return;
        };
        let last = tones.forms.len().saturating_sub(1);
        match key {
            Key::Named(NamedKey::Escape) => self.tones = None,
            Key::Named(NamedKey::Enter | NamedKey::Space) => self.commit_selected(),
            Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowUp) => {
                tones.selected = tones.selected.saturating_sub(1);
            }
            Key::Named(NamedKey::ArrowRight | NamedKey::ArrowDown | NamedKey::Tab) => {
                tones.selected = tones.selected.saturating_add(1).min(last);
            }
            Key::Named(NamedKey::Home) => tones.selected = 0,
            Key::Named(NamedKey::End) => tones.selected = last,
            _ => {}
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.items.is_empty() {
            return;
        }
        let last = self.items.len().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(delta).min(last);
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

    fn button(&mut self, state: ElementState, button: MouseButton, p: Point, now: Instant) {
        let hit = self.hit(p);
        match (state, button) {
            (ElementState::Pressed, MouseButton::Left) => {
                if hit == Hit::Outside {
                    self.close();
                    return;
                }
                if self.tones.is_some() && !matches!(hit, Hit::Tone(_) | Hit::Card) {
                    self.tones = None;
                }
                if let Hit::Tab(i) = hit {
                    self.select_group(i);
                }
                self.press = Some(Press { hit, at: now });
            }
            (ElementState::Released, MouseButton::Left) => {
                let Some(press) = self.press.take() else {
                    return;
                };
                if press.hit != hit {
                    return;
                }
                match hit {
                    Hit::Emoji(i) => {
                        let long = now.saturating_duration_since(press.at) >= LONG_PRESS;
                        if !(long && self.open_tones(i))
                            && let Some(text) = self.items.get(i).cloned()
                        {
                            self.commit(text);
                        }
                    }
                    Hit::Tone(i) => {
                        if let Some(tones) = self.tones.as_mut() {
                            tones.selected = i;
                        }
                        self.commit_selected();
                    }
                    _ => {}
                }
            }
            (ElementState::Pressed, MouseButton::Right) => {
                if let Hit::Emoji(i) = hit {
                    let _ = self.open_tones(i);
                }
            }
            _ => {}
        }
    }

    // --- words -----------------------------------------------------------

    /// The localised name of `text`, with its tone when it carries one.
    pub fn spoken_name(&self, text: &str) -> String {
        let Some(found) = self.catalogue.lookup(text) else {
            return text.to_owned();
        };
        let name = found.emoji.name().to_owned();
        match found.tone {
            None => name,
            Some(tone) => t_args(
                "emoji-picker-toned",
                &[("name", name.into()), ("tone", t(tone_key(tone)).into())],
            ),
        }
    }

    fn hint(&self) -> String {
        match self.selected() {
            Some(text) if self.takes_tone(text) => t("emoji-picker-hint-tones"),
            _ => t("emoji-picker-hint"),
        }
    }

    // --- accessibility ---------------------------------------------------

    /// The picker as assistive technology sees it: a modal dialog with the
    /// search field focused, the group tabs, the grid (the highlighted emoji
    /// selected) and the skin-tone row when open. The footer announces the
    /// highlight politely as it moves.
    pub fn accessibility(&self) -> Option<AccessNode> {
        if !self.open {
            return None;
        }
        let g = &self.geometry;
        let mut dialog = AccessNode::new(Role::Dialog)
            .with_name(t("emoji-picker-title"))
            .with_description(t("emoji-picker-a11y-description"))
            .with_bounds(g.card)
            .with_state(AccessState::new().modal(true));
        dialog.children.push(
            AccessNode::new(Role::TextInput)
                .with_name(t("emoji-picker-search"))
                .with_value(self.query.clone())
                .with_bounds(g.field)
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .focused(self.tones.is_none()),
                ),
        );
        let tabs = self
            .catalogue
            .groups()
            .iter()
            .zip(&g.tabs)
            .enumerate()
            .map(|(i, (group, rect))| {
                AccessNode::new(Role::Tab)
                    .with_name(group.label())
                    .with_bounds(*rect)
                    .with_hashed_key(format!("emoji-group:{}", group.id()))
                    .with_state(AccessState::new().selected(i == self.group))
            })
            .collect();
        dialog.children.push(
            AccessNode::new(Role::TabBar)
                .with_name(t("emoji-picker-groups"))
                .with_children(tabs),
        );
        dialog.children.extend(self.grid_accessibility());
        dialog.children.extend(self.tones_accessibility());
        let announced = self
            .tones
            .as_ref()
            .and_then(|tones| tones.forms.get(tones.selected))
            .map(String::as_str)
            .or_else(|| self.selected());
        if let Some(text) = announced {
            dialog.children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(self.spoken_name(text))
                    .with_bounds(g.footer)
                    .with_live(Politeness::Polite),
            );
        }
        Some(dialog)
    }

    /// One group per grid section, holding the emoji scrolled into view (the
    /// highlight moves the view), or the empty-search message.
    fn grid_accessibility(&self) -> Vec<AccessNode> {
        let mut nodes: Vec<AccessNode> = self
            .sections
            .iter()
            .map(|section| {
                let name = section
                    .heading
                    .clone()
                    .unwrap_or_else(|| t("emoji-picker-results"));
                let buttons = (section.start..section.end)
                    .filter_map(|i| {
                        let text = self.items.get(i)?;
                        let visible = self.emoji_rect(i)?;
                        let node = AccessNode::new(Role::Button)
                            .with_name(self.spoken_name(text))
                            .with_bounds(visible)
                            .with_hashed_key(format!("emoji:{i}:{text}"))
                            .with_state(AccessState::new().selected(i == self.selected));
                        Some(if self.takes_tone(text) {
                            node.with_description(t("emoji-picker-tones-hint"))
                        } else {
                            node
                        })
                    })
                    .collect();
                AccessNode::new(Role::Group)
                    .with_name(name)
                    .with_children(buttons)
            })
            .collect();
        if self.items.is_empty() {
            nodes.push(
                AccessNode::new(Role::StaticText)
                    .with_name(t("emoji-picker-no-results"))
                    .with_bounds(self.geometry.grid),
            );
        }
        nodes
    }

    /// The skin-tone row, when open, with the chosen tone focused.
    fn tones_accessibility(&self) -> Option<AccessNode> {
        let tones = self.tones.as_ref()?;
        let buttons = tones
            .forms
            .iter()
            .zip(&tones.cells)
            .enumerate()
            .map(|(i, (text, rect))| {
                AccessNode::new(Role::Button)
                    .with_name(self.spoken_name(text))
                    .with_bounds(*rect)
                    .with_hashed_key(format!("emoji-tone:{text}"))
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(i == tones.selected),
                    )
            })
            .collect();
        Some(
            AccessNode::new(Role::Group)
                .with_name(t("emoji-picker-tones"))
                .with_bounds(tones.rect)
                .with_children(buttons),
        )
    }

    /// Where the node at `path` inside [`Self::accessibility`] is, so an
    /// assistive client's click lands on the control it named.
    pub fn accessibility_rect_for_path(&self, path: &[usize]) -> Option<Rect> {
        let mut node = self.accessibility()?;
        for &index in path {
            node = node.children.into_iter().nth(index)?;
        }
        node.bounds
            .map(|b| Rect::new(b.x0, b.y0, b.x1, b.y1))
            .filter(|rect| rect.area() > 0.0)
    }

    fn measure_footer(&self, text: &mut TextRenderer, theme: &Theme) -> Option<FooterText> {
        let selected = self.selected()?;
        let name = self.spoken_name(selected);
        let hint = self.hint();
        let gap = f64::from(self.spacing.s2);
        let width =
            (self.geometry.footer.width() - f64::from(self.metrics.bar_height) - gap).max(0.0);
        let style = theme.tokens.typography.style(TextRole::Caption);
        let name_style = style.clone().with_weight(TextWeight::Medium);
        let name_width = f64::from(text.measure_style(&name, None, &name_style).0);
        let (hint_width, hint_height) = text.measure_style(&hint, None, &style);
        let stacked = name_width + gap + f64::from(hint_width) > width;
        let line_height = f64::from(style.line_height_px());
        let lines = if stacked {
            text.clamp_to_lines_style(&hint, f64_to_f32(width), usize::MAX, &style)
        } else {
            vec![hint]
        };
        let height = if stacked {
            f64::from(self.metrics.bar_height).max(line_height)
                + gap
                + line_height * f64_from_usize(lines.len())
        } else {
            f64::from(self.metrics.bar_height)
        };
        Some(FooterText {
            name: text.elide_to_width_style(&name, f64_to_f32(width), &name_style),
            hint: lines,
            hint_width: f64::from(hint_width),
            hint_height: f64::from(hint_height),
            line_height,
            height,
            stacked,
        })
    }

    fn draw_footer(&self, ctx: &mut PaintCtx<'_>, footer: &FooterText) {
        let Some(selected) = self.selected() else {
            return;
        };
        let rect = self.geometry.footer;
        let edge = f64::from(self.metrics.bar_height);
        let glyph = Rect::new(rect.x0, rect.y0, rect.x0 + edge, rect.y0 + edge);
        draw_emoji(
            ctx.scene,
            ctx.transform,
            ctx.text,
            ctx.theme,
            glyph,
            selected,
        );
        let x = glyph.x1 + f64::from(self.spacing.s2);
        let style = ctx.theme.tokens.typography.style(TextRole::Caption);
        let name_style = style.clone().with_weight(TextWeight::Medium);
        let hint_x = if footer.stacked {
            x
        } else {
            (rect.x1 - footer.hint_width).max(x)
        };
        let name_y = if footer.stacked {
            glyph.center().y - f64::from(ctx.theme.tokens.typography.size(TextRole::Caption)) * 0.7
        } else {
            rect.center().y - f64::from(ctx.theme.tokens.typography.size(TextRole::Caption)) * 0.7
        };
        let name_width = if footer.stacked {
            rect.x1 - x
        } else {
            hint_x - x - f64::from(self.spacing.s2)
        };
        ctx.text.draw_style(
            ctx.scene,
            &footer.name,
            ctx.theme.tokens.colors.text_primary,
            ctx.transform.pre_translate((x, name_y).into()),
            Some(f64_to_f32(name_width.max(0.0))),
            &name_style,
        );
        let hint_y = if footer.stacked {
            rect.y0 + edge.max(footer.line_height) + f64::from(self.spacing.s2)
        } else {
            rect.center().y - footer.hint_height / 2.0
        };
        for (index, line) in footer.hint.iter().enumerate() {
            ctx.text.draw_style(
                ctx.scene,
                line,
                ctx.theme.tokens.colors.text_dim,
                ctx.transform.pre_translate(
                    (hint_x, hint_y + footer.line_height * f64_from_usize(index)).into(),
                ),
                None,
                &style,
            );
        }
    }

    // --- paint -----------------------------------------------------------

    /// Paint over `viewport`. Geometry follows the theme's spacing and
    /// metrics, re-laid out when either (or the viewport) changed.
    #[allow(
        clippy::too_many_lines,
        reason = "one paint pass keeps the card's layer order visible: card, field, tabs, grid, footer, tone row"
    )]
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        viewport: Rect,
    ) {
        if !self.open {
            return;
        }
        let renderer_scale = text_renderer.text_scale();
        text_renderer.set_text_scale(1.0);
        if self.spacing != theme.tokens.spacing
            || self.metrics != theme.tokens.metrics
            || self.viewport != viewport
        {
            self.spacing = theme.tokens.spacing;
            self.metrics = theme.tokens.metrics;
            self.viewport = viewport;
            self.layout();
        }
        let group_width = self
            .catalogue
            .groups()
            .iter()
            .map(|group| {
                SegmentedFace::intrinsic_width(
                    &[group.icon()],
                    ControlSize::Regular,
                    text_renderer,
                    theme,
                )
            })
            .fold(f64::from(self.metrics.bar_height), f64::max);
        let segments: Vec<Segment> = self
            .catalogue
            .groups()
            .iter()
            .map(|group| Segment::new(group.icon()))
            .collect();
        let group_strip_width = SegmentedFace::partition_segments(
            Rect::ZERO,
            &segments,
            SegmentedStyle::Tabs,
            ControlSize::Regular,
            text_renderer,
            theme,
        )
        .iter()
        .map(Rect::width)
        .fold(0.0_f64, f64::max)
            * f64_from_usize(segments.len());
        if self.group_width.to_bits() != group_width.to_bits()
            || self.group_strip_width.to_bits() != group_strip_width.to_bits()
        {
            self.group_width = group_width;
            self.group_strip_width = group_strip_width;
            self.layout();
        }
        let footer = self.measure_footer(text_renderer, theme);
        let footer_height = footer.as_ref().map_or_else(
            || f64::from(self.metrics.bar_height),
            |footer| footer.height,
        );
        if self.footer_height.to_bits() != footer_height.to_bits() {
            self.footer_height = footer_height;
            self.layout();
        }
        let colors = &theme.tokens.colors;
        let type_scale = &theme.tokens.typography;
        let g = self.geometry.clone();

        let radius = ui::chrome::ChromeRole::Panel.radius(theme, g.card.height());
        ui::chrome::shell::panel_shadow(scene, transform, g.card, radius, theme, 1.0);
        let card_shape = RoundedRect::from_rect(g.card, radius);
        scene.fill(
            Fill::NonZero,
            transform,
            ui::chrome::shell::panel_ground(theme),
            None,
            &card_shape,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            ui::chrome::shell::panel_border(theme),
            None,
            &card_shape,
        );

        self.search
            .draw(scene, transform, text_renderer, theme, presence);

        let icons: Vec<&str> = self
            .catalogue
            .groups()
            .iter()
            .map(emoji::Group::icon)
            .collect();
        let hovered_tab = match self.hover {
            Some(Hit::Tab(i)) => Some(i),
            _ => None,
        };
        for (row_index, (rects, labels)) in g
            .tabs
            .chunks(self.tab_columns)
            .zip(icons.chunks(self.tab_columns))
            .enumerate()
        {
            let first = row_index.saturating_mul(self.tab_columns);
            let selection = self
                .group
                .checked_sub(first)
                .filter(|index| *index < rects.len());
            let hover = hovered_tab
                .and_then(|index| index.checked_sub(first))
                .filter(|index| *index < rects.len());
            ToggleGroup::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &SegmentedFace::new(rects, labels, usize::MAX)
                    .selection(selection)
                    .style(SegmentedStyle::Tabs)
                    .hovered(hover),
            );
        }

        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &g.grid);
        for (heading, y) in &g.headings {
            text_renderer.draw_style(
                scene,
                heading,
                colors.text_dim,
                transform
                    .pre_translate((g.grid.x0 + 4.0, g.grid.y0 + y - self.scroll + 6.0).into()),
                Some(f64_to_f32(g.grid.width())),
                &type_scale
                    .style(TextRole::Caption)
                    .with_weight(TextWeight::Medium),
            );
        }
        for (i, text) in self.items.iter().enumerate() {
            let Some(rect) = self.screen_cell(i) else {
                continue;
            };
            if rect.y1 < g.grid.y0 || rect.y0 > g.grid.y1 {
                continue;
            }
            let shape =
                RoundedRect::from_rect(rect.inset(-2.0), f64::from(theme.tokens.radii.control));
            if i == self.selected {
                let lift = colors.lift_on(ui::chrome::shell::panel_ground(theme), 1.0);
                scene.fill(Fill::NonZero, transform, lift, None, &shape);
                scene.stroke(
                    &vello::kurbo::Stroke::new(1.5),
                    transform,
                    colors.border_focus,
                    None,
                    &shape,
                );
            } else if self.hover == Some(Hit::Emoji(i)) {
                let lift = colors.lift_on(ui::chrome::shell::panel_ground(theme), 0.5);
                scene.fill(Fill::NonZero, transform, lift, None, &shape);
            }
            draw_emoji(scene, transform, text_renderer, theme, rect, text);
        }
        if self.items.is_empty() {
            let message = t("emoji-picker-no-results");
            let (w, h) =
                text_renderer.measure_style(&message, None, &type_scale.style(TextRole::Caption));
            text_renderer.draw_style(
                scene,
                &message,
                colors.text_dim,
                transform.pre_translate(
                    (
                        g.grid.center().x - f64::from(w) / 2.0,
                        g.grid.center().y - f64::from(h) / 2.0,
                    )
                        .into(),
                ),
                None,
                &type_scale.style(TextRole::Caption),
            );
        }
        scene.pop_layer();

        if let Some(footer) = &footer {
            self.draw_footer(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                footer,
            );
        }

        if let Some(tones) = &self.tones {
            let radius = f64::from(theme.tokens.radii.control);
            ui::surface::shadow(
                scene,
                transform,
                tones.rect,
                radius,
                &theme.tokens.elevation.floating,
            );
            let shape = RoundedRect::from_rect(tones.rect, radius);
            scene.fill(
                Fill::NonZero,
                transform,
                colors.surfaces.raised,
                None,
                &shape,
            );
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                ui::chrome::shell::panel_border(theme),
                None,
                &shape,
            );
            for (i, (text, rect)) in tones.forms.iter().zip(&tones.cells).enumerate() {
                if i == tones.selected {
                    let cell_shape = RoundedRect::from_rect(
                        rect.inset(-2.0),
                        f64::from(theme.tokens.radii.control),
                    );
                    let lift = colors.lift_on(colors.surfaces.raised, 1.0);
                    scene.fill(Fill::NonZero, transform, lift, None, &cell_shape);
                    scene.stroke(
                        &vello::kurbo::Stroke::new(1.5),
                        transform,
                        colors.border_focus,
                        None,
                        &cell_shape,
                    );
                }
                draw_emoji(scene, transform, text_renderer, theme, *rect, text);
            }
        }
        text_renderer.set_text_scale(renderer_scale);
    }
}

fn tone_key(tone: SkinTone) -> &'static str {
    match tone {
        SkinTone::Light => "emoji-picker-tone-light",
        SkinTone::MediumLight => "emoji-picker-tone-medium-light",
        SkinTone::Medium => "emoji-picker-tone-medium",
        SkinTone::MediumDark => "emoji-picker-tone-medium-dark",
        SkinTone::Dark => "emoji-picker-tone-dark",
    }
}

/// One emoji centred in its cell.
fn draw_emoji(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    text: &str,
) {
    let size = f64_to_f32((rect.height().min(rect.width()) * GLYPH_SCALE).clamp(12.0, 48.0));
    let (w, h) = text_renderer.measure(text, size, None);
    text_renderer.draw_text(
        scene,
        text,
        size,
        theme.tokens.colors.text_primary,
        transform.pre_translate(
            (
                rect.center().x - f64::from(w) / 2.0,
                rect.center().y - f64::from(h) / 2.0,
            )
                .into(),
        ),
        None,
    );
}

#[allow(
    clippy::cast_precision_loss,
    reason = "grid counts are a few hundred at most"
)]
fn f64_from_usize(n: usize) -> f64 {
    n as f64
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a column count of a card-sized grid, already floored and at least one"
)]
fn usize_from_f64(v: f64) -> usize {
    v.max(0.0) as usize
}

/// Where a card of `size` goes for a field whose caret is `caret`: its left
/// edge on the caret, `gap` below it, or `gap` above it when the display
/// ends first; when neither side has room, on whichever side has more, and
/// always kept `edge` inside `viewport` (the display holding the caret).
pub fn card_at_caret(
    caret: Rect,
    (width, height): (f64, f64),
    viewport: Rect,
    edge: f64,
    gap: f64,
) -> Rect {
    let (left, right) = (viewport.x0 + edge, viewport.x1 - edge);
    let (top, bottom) = (viewport.y0 + edge, viewport.y1 - edge);
    let x0 = caret.x0.min(right - width).max(left);
    let below = caret.y1 + gap;
    let above = caret.y0 - gap - height;
    let y0 = if below + height <= bottom {
        below
    } else if above >= top {
        above
    } else if bottom - caret.y1 >= caret.y0 - top {
        (bottom - height).max(top)
    } else {
        top
    };
    Rect::from_origin_size((x0, y0), (width, height))
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "card-sized lengths and glyph sizes fit f32"
)]
fn f64_to_f32(v: f64) -> f32 {
    v as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEWPORT: Rect = Rect::new(0.0, 0.0, 1280.0, 800.0);

    #[test]
    fn the_card_sits_below_the_caret_flips_above_it_and_stays_on_the_display() {
        const EDGE: f64 = 16.0;
        const GAP: f64 = 8.0;
        let size = (400.0, 300.0);
        let caret = |x: f64, y: f64| Rect::new(x, y, x + 2.0, y + 20.0);

        let card = card_at_caret(caret(300.0, 100.0), size, VIEWPORT, EDGE, GAP);
        assert_eq!(
            card,
            Rect::new(300.0, 128.0, 700.0, 428.0),
            "below the caret"
        );

        let card = card_at_caret(caret(300.0, 700.0), size, VIEWPORT, EDGE, GAP);
        assert_eq!(card.y1, 692.0, "above a caret near the bottom");

        let card = card_at_caret(caret(1200.0, 100.0), size, VIEWPORT, EDGE, GAP);
        assert_eq!(card.x1, VIEWPORT.x1 - EDGE, "kept inside the right edge");
        let card = card_at_caret(caret(-50.0, 100.0), size, VIEWPORT, EDGE, GAP);
        assert_eq!(card.x0, VIEWPORT.x0 + EDGE, "kept inside the left edge");

        // A display too short for either side: the roomier side, on screen.
        let short = Rect::new(0.0, 0.0, 1280.0, 420.0);
        let card = card_at_caret(caret(300.0, 150.0), size, short, EDGE, GAP);
        assert_eq!((card.y0, card.y1), (104.0, 404.0));
        let card = card_at_caret(caret(300.0, 260.0), size, short, EDGE, GAP);
        assert_eq!((card.y0, card.y1), (16.0, 316.0));

        // Another display of the desktop: its own edges apply.
        let right = Rect::new(1280.0, 0.0, 2560.0, 800.0);
        let card = card_at_caret(caret(2500.0, 100.0), size, right, EDGE, GAP);
        assert_eq!(card.x1, right.x1 - EDGE);
        assert!(card.x0 >= right.x0 + EDGE);
    }

    fn opened() -> EmojiPicker {
        let mut picker = EmojiPicker::new();
        picker.open(VIEWPORT, "en-US");
        picker
    }

    fn press(picker: &mut EmojiPicker, key: &Key, mods: ModifiersState) {
        picker.key(key, mods);
    }

    fn typed(picker: &mut EmojiPicker, text: &str) {
        for c in text.chars() {
            let key = if c == ' ' {
                Key::Named(NamedKey::Space)
            } else {
                Key::Character(c.to_string().into())
            };
            press(picker, &key, ModifiersState::empty());
        }
    }

    fn pointer_event(picker: &mut EmojiPicker, state: ElementState, point: Point) {
        picker.handle_event(
            &WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state,
                button: MouseButton::Left,
            },
            1.0,
            ModifiersState::empty(),
            Some(point),
            Instant::now(),
        );
    }

    #[test]
    fn interrupted_pointer_pick_does_not_insert_after_focus_loss() {
        let mut picker = opened();
        typed(&mut picker, "pizza");
        let cell = picker.emoji_rect(0).expect("visible match").center();
        pointer_event(&mut picker, ElementState::Pressed, cell);
        picker.handle_event(
            &WindowEvent::Focused(false),
            1.0,
            ModifiersState::empty(),
            Some(cell),
            Instant::now(),
        );
        pointer_event(&mut picker, ElementState::Released, cell);
        assert!(
            picker.take_commits().is_empty(),
            "focus loss cancels the pick"
        );
        assert!(picker.is_open(), "cancelled pick keeps the search");
        pointer_event(&mut picker, ElementState::Pressed, cell);
        pointer_event(&mut picker, ElementState::Released, cell);
        assert_eq!(picker.take_commits(), ["🍕"], "a fresh click still works");
        assert!(
            picker.take_commits().is_empty(),
            "one completed click inserts once"
        );
    }

    #[test]
    fn interrupted_pointer_pick_does_not_insert_a_replaced_search_result() {
        let mut picker = opened();
        typed(&mut picker, "pizza");
        let cell = picker.emoji_rect(0).expect("visible match").center();
        pointer_event(&mut picker, ElementState::Pressed, cell);
        picker.handle_event(
            &WindowEvent::Ime(winit::event::Ime::Disabled),
            1.0,
            ModifiersState::empty(),
            Some(cell),
            Instant::now(),
        );
        // Search correction through the production keyboard editing path.
        press(
            &mut picker,
            &Key::Character("a".into()),
            ModifiersState::CONTROL,
        );
        typed(&mut picker, "thumbs up");
        assert_eq!(picker.selected(), Some("👍"));
        pointer_event(&mut picker, ElementState::Released, cell);
        assert!(
            picker.take_commits().is_empty(),
            "the old press cannot pick a new result"
        );
        assert!(picker.is_open());
        press(
            &mut picker,
            &Key::Named(NamedKey::Enter),
            ModifiersState::empty(),
        );
        assert_eq!(
            picker.take_commits(),
            ["👍"],
            "explicit keyboard pick works"
        );
    }

    #[test]
    fn interrupted_pointer_pick_cancels_scroll_resize_and_touch() {
        #[derive(Clone, Copy, Debug)]
        enum Interruption {
            Scroll,
            Resize,
            TouchCancel,
        }
        for interruption in [
            Interruption::Scroll,
            Interruption::Resize,
            Interruption::TouchCancel,
        ] {
            let mut picker = opened();
            let expected = picker.selected().expect("first emoji").to_owned();
            let cell = picker.emoji_rect(0).expect("visible first emoji").center();
            pointer_event(&mut picker, ElementState::Pressed, cell);
            match interruption {
                Interruption::Scroll => {
                    for delta in [-1.0, 1.0] {
                        picker.handle_event(
                            &WindowEvent::MouseWheel {
                                device_id: winit::event::DeviceId::dummy(),
                                delta: MouseScrollDelta::LineDelta(0.0, delta),
                                phase: TouchPhase::Moved,
                            },
                            1.0,
                            ModifiersState::empty(),
                            Some(cell),
                            Instant::now(),
                        );
                    }
                }
                Interruption::Resize => {
                    picker.draw(
                        &mut Scene::new(),
                        Affine::IDENTITY,
                        &mut TextRenderer::new(),
                        &Theme::dark(),
                        &PresenceCtx::default(),
                        Rect::new(0.0, 0.0, 1260.0, 800.0),
                    );
                }
                Interruption::TouchCancel => {
                    picker.handle_event(
                        &WindowEvent::Touch(winit::event::Touch {
                            device_id: winit::event::DeviceId::dummy(),
                            phase: TouchPhase::Cancelled,
                            location: winit::dpi::PhysicalPosition::new(cell.x, cell.y),
                            force: None,
                            id: 1,
                        }),
                        1.0,
                        ModifiersState::empty(),
                        Some(cell),
                        Instant::now(),
                    );
                }
            }
            pointer_event(&mut picker, ElementState::Released, cell);
            assert!(
                picker.take_commits().is_empty(),
                "{interruption:?} cancels the pick"
            );
            assert!(picker.is_open());
            let fresh = picker
                .emoji_rect(0)
                .expect("first emoji remains visible")
                .center();
            pointer_event(&mut picker, ElementState::Pressed, fresh);
            pointer_event(&mut picker, ElementState::Released, fresh);
            assert_eq!(
                picker.take_commits(),
                [expected],
                "next click works after {interruption:?}"
            );
        }
    }

    #[test]
    fn enlarged_footer_and_narrow_groups_reserve_their_measured_room() {
        let mut picker = opened();
        typed(&mut picker, "thumbs up");
        let theme = Theme::dark().with_text_scale(2.0);
        let mut text = TextRenderer::new();
        picker.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut text,
            &theme,
            &PresenceCtx::default(),
            Rect::new(0.0, 0.0, 320.0, 700.0),
        );
        let name = picker.spoken_name(picker.selected().expect("match"));
        let style = theme.tokens.typography.style(TextRole::Caption);
        let name_width = f64::from(text.measure_style(&name, None, &style).0);
        let hint_width = f64::from(text.measure_style(&picker.hint(), None, &style).0);
        assert!(
            picker.geometry.footer.height() >= 2.0 * f64::from(style.line_height_px())
                || name_width + hint_width + f64::from(picker.metrics.bar_height)
                    < picker.geometry.footer.width(),
            "footer text must fit or stack"
        );
        let labels: Vec<&str> = picker
            .catalogue
            .groups()
            .iter()
            .map(emoji::Group::icon)
            .collect();
        let required = labels
            .iter()
            .map(|label| {
                ui::widget::SegmentedFace::intrinsic_width(
                    &[*label],
                    ui::widget::ControlSize::Regular,
                    &mut text,
                    &theme,
                )
            })
            .fold(0.0_f64, f64::max);
        assert!(
            picker
                .geometry
                .tabs
                .iter()
                .all(|rect| rect.width() >= required),
            "group icons need the shared control's measured room"
        );
    }

    #[test]
    fn narrow_groups_keep_shared_intrinsic_targets() {
        let mut picker = opened();
        let theme = Theme::light().with_text_scale(2.0);
        let mut text = TextRenderer::new();
        picker.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut text,
            &theme,
            &PresenceCtx::default(),
            Rect::new(0.0, 0.0, 320.0, 700.0),
        );
        let required = picker
            .catalogue
            .groups()
            .iter()
            .map(|group| {
                ui::widget::SegmentedFace::intrinsic_width(
                    &[group.icon()],
                    ui::widget::ControlSize::Regular,
                    &mut text,
                    &theme,
                )
            })
            .fold(0.0_f64, f64::max);
        assert!(
            picker
                .geometry
                .tabs
                .iter()
                .all(|rect| rect.width() >= required),
            "each visible group needs the shared face's measured glyph room"
        );
    }

    #[test]
    fn wrapped_groups_keep_keyboard_pointer_and_accessible_selection_on_resize() {
        let mut picker = opened();
        let theme = Theme::light().with_text_scale(2.0);
        let mut text = TextRenderer::new();
        let mut paint = |picker: &mut EmojiPicker, height: f64| {
            picker.draw(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut text,
                &theme,
                &PresenceCtx::default(),
                Rect::new(0.0, 0.0, 320.0, height),
            );
        };
        paint(&mut picker, 700.0);
        let group_count = picker.catalogue.groups().len();
        for _ in 0..group_count.saturating_sub(1) {
            press(
                &mut picker,
                &Key::Named(NamedKey::Tab),
                ModifiersState::empty(),
            );
        }
        paint(&mut picker, 700.0);
        assert_eq!(picker.group, group_count.saturating_sub(1));
        let accessible = picker.accessibility().expect("open picker");
        let tabs = accessible
            .children
            .iter()
            .find(|node| node.role == Role::TabBar)
            .expect("shared group semantics")
            .children
            .as_slice();
        assert_eq!(tabs.len(), group_count);
        assert!(tabs.last().expect("last group").state.selected);
        for node in tabs {
            let bounds = node.bounds.expect("every group has painted bounds");
            assert!(bounds.x0 >= picker.geometry.card.x0 && bounds.x1 <= picker.geometry.card.x1);
        }
        let cursor = picker.geometry.tabs.first().expect("first group").center();
        for state in [ElementState::Pressed, ElementState::Released] {
            picker.handle_event(
                &WindowEvent::MouseInput {
                    device_id: winit::event::DeviceId::dummy(),
                    state,
                    button: MouseButton::Left,
                },
                1.0,
                ModifiersState::empty(),
                Some(cursor),
                Instant::now(),
            );
        }
        assert_eq!(picker.group, 0, "pointer chooses the original stable group");
        for _ in 0..picker.items.len().saturating_sub(1) {
            press(
                &mut picker,
                &Key::Named(NamedKey::ArrowRight),
                ModifiersState::empty(),
            );
        }
        paint(&mut picker, 480.0);
        assert!(
            picker.emoji_rect(picker.selected).is_some(),
            "resized highlight stays visible"
        );
        assert!(
            picker.geometry.card.y1 <= 480.0,
            "measured footer stays on screen"
        );
        press(
            &mut picker,
            &Key::Named(NamedKey::Escape),
            ModifiersState::empty(),
        );
        assert!(!picker.is_open());
        assert!(
            picker.take_commits().is_empty(),
            "group navigation/cancel inserts nothing"
        );
    }

    #[test]
    fn typing_searches_and_enter_picks_the_first_match() {
        let mut picker = opened();
        typed(&mut picker, "thumbs");
        assert_eq!(picker.query(), "thumbs");
        assert_eq!(picker.selected(), Some("👍"));
        press(
            &mut picker,
            &Key::Named(NamedKey::Enter),
            ModifiersState::empty(),
        );
        assert!(!picker.is_open());
        assert_eq!(picker.take_commits(), ["👍".to_string()]);
        assert!(picker.take_commits().is_empty(), "one pick, one commit");
    }

    #[test]
    fn escape_closes_without_picking() {
        let mut picker = opened();
        typed(&mut picker, "pizza");
        press(
            &mut picker,
            &Key::Named(NamedKey::Escape),
            ModifiersState::empty(),
        );
        assert!(!picker.is_open());
        assert!(picker.take_commits().is_empty());
    }

    #[test]
    fn recents_come_first_most_recent_first_and_secrets_are_not_remembered() {
        let mut picker = opened();
        picker.note_delivered("👍", false);
        picker.note_delivered("🍕", false);
        picker.note_delivered("🔑", true);
        assert_eq!(picker.recents(), ["🍕".to_string(), "👍".to_string()]);
        assert_eq!(&picker.items()[..2], ["🍕".to_string(), "👍".to_string()]);
        assert_eq!(picker.selected(), Some("🍕"));
        // A toned pick replaces the plain one rather than adding a second.
        picker.note_delivered("👍🏽", false);
        assert_eq!(picker.recents(), ["👍🏽".to_string(), "🍕".to_string()]);
    }

    #[test]
    fn arrows_move_through_the_grid_and_tab_changes_group() {
        let mut picker = opened();
        let first = picker.selected().map(str::to_owned);
        press(
            &mut picker,
            &Key::Named(NamedKey::ArrowRight),
            ModifiersState::empty(),
        );
        assert_eq!(picker.selected, 1);
        press(
            &mut picker,
            &Key::Named(NamedKey::ArrowDown),
            ModifiersState::empty(),
        );
        let first_row = picker.geometry.cells[0].y0;
        let columns = picker
            .geometry
            .cells
            .iter()
            .take_while(|r| (r.y0 - first_row).abs() < 0.5)
            .count();
        assert_eq!(picker.selected, 1 + columns, "one row down, same column");
        press(
            &mut picker,
            &Key::Named(NamedKey::ArrowUp),
            ModifiersState::empty(),
        );
        press(
            &mut picker,
            &Key::Named(NamedKey::ArrowLeft),
            ModifiersState::empty(),
        );
        assert_eq!(picker.selected().map(str::to_owned), first);

        press(
            &mut picker,
            &Key::Named(NamedKey::Tab),
            ModifiersState::empty(),
        );
        assert_eq!(picker.group, 1);
        let people = picker.catalogue.groups()[1].clone();
        assert_eq!(
            picker.selected(),
            picker
                .catalogue
                .group_emoji(&people)
                .first()
                .map(emoji::Emoji::text)
        );
        press(
            &mut picker,
            &Key::Named(NamedKey::Tab),
            ModifiersState::SHIFT,
        );
        assert_eq!(picker.group, 0);
        press(
            &mut picker,
            &Key::Named(NamedKey::Tab),
            ModifiersState::SHIFT,
        );
        assert_eq!(picker.group, picker.catalogue.groups().len() - 1, "wraps");
    }

    #[test]
    fn shift_enter_offers_skin_tones_and_enter_picks_one() {
        let mut picker = opened();
        typed(&mut picker, "thumbs up");
        assert_eq!(picker.selected(), Some("👍"));
        press(
            &mut picker,
            &Key::Named(NamedKey::Enter),
            ModifiersState::SHIFT,
        );
        let forms = picker.tone_forms().expect("tone row").to_vec();
        assert_eq!(forms.len(), 6, "default plus five tones: {forms:?}");
        press(
            &mut picker,
            &Key::Named(NamedKey::ArrowRight),
            ModifiersState::empty(),
        );
        press(
            &mut picker,
            &Key::Named(NamedKey::ArrowRight),
            ModifiersState::empty(),
        );
        press(
            &mut picker,
            &Key::Named(NamedKey::Enter),
            ModifiersState::empty(),
        );
        assert_eq!(picker.take_commits(), [forms[2].clone()]);

        // Escape in the tone row closes only the row.
        let mut picker = opened();
        typed(&mut picker, "thumbs up");
        press(
            &mut picker,
            &Key::Named(NamedKey::Enter),
            ModifiersState::SHIFT,
        );
        press(
            &mut picker,
            &Key::Named(NamedKey::Escape),
            ModifiersState::empty(),
        );
        assert!(picker.is_open() && picker.tone_forms().is_none());

        // An emoji without tones opens no row.
        let mut picker = opened();
        typed(&mut picker, "pizza");
        press(
            &mut picker,
            &Key::Named(NamedKey::Enter),
            ModifiersState::SHIFT,
        );
        assert!(picker.tone_forms().is_none());
    }

    #[test]
    fn a_click_picks_a_right_click_offers_tones_and_outside_closes() {
        let t0 = Instant::now();
        let mut picker = opened();
        typed(&mut picker, "thumbs up");
        let cell = picker.emoji_rect(0).expect("first match visible");
        picker.button(ElementState::Pressed, MouseButton::Right, cell.center(), t0);
        assert!(picker.tone_forms().is_some());

        let mut picker = opened();
        typed(&mut picker, "thumbs up");
        let cell = picker.emoji_rect(0).expect("first match visible");
        picker.button(ElementState::Pressed, MouseButton::Left, cell.center(), t0);
        let later = t0 + LONG_PRESS + Duration::from_millis(10);
        picker.button(
            ElementState::Released,
            MouseButton::Left,
            cell.center(),
            later,
        );
        assert!(
            picker.tone_forms().is_some(),
            "a long press opens the tones"
        );

        let mut picker = opened();
        typed(&mut picker, "pizza");
        let cell = picker.emoji_rect(0).expect("first match visible");
        picker.button(ElementState::Pressed, MouseButton::Left, cell.center(), t0);
        picker.button(ElementState::Released, MouseButton::Left, cell.center(), t0);
        assert_eq!(picker.take_commits(), ["🍕".to_string()]);

        let mut picker = opened();
        picker.button(
            ElementState::Pressed,
            MouseButton::Left,
            Point::new(2.0, 2.0),
            t0,
        );
        assert!(!picker.is_open());
        assert!(picker.take_commits().is_empty());
    }

    #[test]
    fn nothing_found_says_so() {
        let mut picker = opened();
        typed(&mut picker, "zzzzqqq");
        assert!(picker.items().is_empty());
        press(
            &mut picker,
            &Key::Named(NamedKey::Enter),
            ModifiersState::empty(),
        );
        assert!(
            picker.is_open(),
            "Enter with nothing highlighted keeps it open"
        );
        let node = picker.accessibility().expect("open");
        assert!(
            node.children
                .iter()
                .any(|c| c.name.as_deref() == Some(t("emoji-picker-no-results").as_str()))
        );
    }

    #[test]
    fn the_dialog_announces_the_highlight() {
        // German names and searches in German.
        let mut picker = EmojiPicker::new();
        picker.open(VIEWPORT, "de-DE");
        typed(&mut picker, "daumen");
        assert_eq!(picker.selected(), Some("👍"));
        let node = picker.accessibility().expect("open");
        assert_eq!(node.role, Role::Dialog);
        assert!(node.state.modal);
        let spoken = picker.spoken_name("👍");
        assert!(
            node.children
                .iter()
                .any(|c| c.role == Role::StaticText && c.name.as_deref() == Some(spoken.as_str())),
            "the footer announces {spoken}"
        );
    }

    #[derive(Clone, Default)]
    struct MemoryStore(std::rc::Rc<std::cell::RefCell<Option<Recents>>>);

    impl RecentsStore for MemoryStore {
        fn load(&self) -> Recents {
            self.0.borrow().clone().unwrap_or_default()
        }
        fn save(&self, recents: &Recents) -> Result<(), String> {
            *self.0.borrow_mut() = Some(recents.clone());
            Ok(())
        }
    }

    #[test]
    fn recents_persist_through_the_store_and_skip_secrets() {
        let store = MemoryStore::default();
        let mut picker = EmojiPicker::new();
        picker.attach_store(Box::new(store.clone()));
        picker.note_delivered("👍", false);
        picker.note_delivered("🎉", false);
        picker.note_delivered("🔒", true);

        let mut restarted = EmojiPicker::new();
        restarted.attach_store(Box::new(store));
        restarted.open(VIEWPORT, "en-US");
        assert_eq!(restarted.recents(), ["🎉".to_string(), "👍".to_string()]);
    }
}
