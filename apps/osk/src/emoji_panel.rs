//! The emoji panel: group tabs, a scrolling grid with recents first, a search
//! field typed with the keyboard itself, and a skin-tone row.
//!
//! The panel has two faces, and the keyboard's height never changes between
//! them:
//!
//! * **Browse** replaces the suggestion strip with the search field and the
//!   keys with the group tabs, the grid and a row holding the way back to the
//!   letters and Delete. The grid lists this user's recents first, then the
//!   selected group.
//! * **Search** keeps the letter keys so the query can be typed; the search
//!   field sits in the suggestion strip and the toolbar band becomes a strip
//!   of matching emoji. Picking one types it and keeps searching.
//!
//! A long press (the toolkit's secondary click; a right click with a mouse)
//! on an emoji that takes a skin tone opens a row with its default and five
//! toned forms. What a pick types is decided by the caller, which owns the
//! output stream and the recents.
//!
//! The panel is plain state plus geometry, so it is testable without a
//! renderer; [`crate::widget::OskWidget`] routes events to it and draws it.

use emoji::{Catalogue, Recents, SkinTone};
use ui::TextRole;
use ui::a11y::{AccessNode, AccessState, Role};
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextStyle};
use ui::theme::Theme;
use ui::tokens::{Metrics, Spacing};
use ui::widget::{SearchField, SegmentedFace, SegmentedStyle, ToggleGroup, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Fill};

use crate::i18n;

/// How many emoji chips a typed word may add to the suggestion strip.
pub const EMOJI_SUGGESTIONS: usize = 2;
/// Search results listed at most.
const SEARCH_RESULTS: usize = 64;
/// Emoji glyph size relative to its cell. Emoji are content: the size follows
/// the cell, not the text scale.
const GLYPH_CONTENT_PX_RATIO: f64 = 0.55;

/// Which face of the panel is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelMode {
    /// The keyboard shows its keys.
    Closed,
    /// Tabs and the grid replace the keys.
    Browse,
    /// The keys type into the emoji search field.
    Search,
}

/// What a point on the panel is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PanelHit {
    /// The search field; activating it starts a search.
    Search,
    /// A group tab.
    Group(usize),
    /// An emoji in the grid or the search strip.
    Emoji(String),
    /// A form in the open skin-tone row.
    Tone(String),
    /// Back to the letter keys.
    Letters,
    /// Delete before the caret in the edited field.
    Delete,
    /// Leave the search for the grid.
    EndSearch,
    /// Inside the panel, on nothing that acts.
    Nothing,
}

/// One titled run of emoji in the grid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub heading: String,
    pub emoji: Vec<String>,
}

/// The rects the keyboard gives the panel, top to bottom.
#[derive(Clone, Copy, Debug, Default)]
pub struct PanelBands {
    /// The toolbar band (the search strip while searching).
    pub toolbar: Rect,
    /// The suggestion band (the search field).
    pub suggestion: Rect,
    /// The key band (tabs, grid and bottom row while browsing).
    pub keys: Rect,
    /// Height of one key row, for the bottom row.
    pub row_height: f64,
    /// Smallest grid cell, a comfortable touch target.
    pub min_cell: f64,
    /// Height of the group tab row.
    pub tab_height: f64,
    /// Height of a section heading in the grid.
    pub heading_height: f64,
    /// Inset of the search field and the grid's headings from their band.
    pub inset: f64,
    /// The size an emoji group's tab glyph is drawn at.
    pub tab_glyph: f64,
}

impl PanelBands {
    /// Sizes from the theme's spacing scale and metrics.
    #[must_use]
    pub fn sized_by(mut self, spacing: &Spacing, metrics: &Metrics) -> Self {
        self.min_cell = f64::from(spacing.s12);
        self.tab_height = f64::from(metrics.bar_height);
        self.heading_height = f64::from(spacing.s6);
        self.inset = f64::from(spacing.s2);
        self.tab_glyph = f64::from(metrics.icon_md);
        self
    }
}

/// The skin-tone row opened on one emoji.
#[derive(Clone, Debug)]
struct Tones {
    base: String,
    anchor: Rect,
}

/// The panel's state and last laid-out geometry.
pub struct EmojiPanel {
    catalogue: &'static Catalogue,
    mode: PanelMode,
    group: usize,
    scroll: f64,
    search: SearchField,
    tones: Option<Tones>,
    bands: PanelBands,
    search_rect: Rect,
    tab_rects: Vec<Rect>,
    grid: Rect,
    content_height: f64,
    cell: f64,
    cells: Vec<(String, Rect)>,
    headings: Vec<(String, Point)>,
    letters_rect: Rect,
    delete_rect: Rect,
    end_search_rect: Rect,
    strip: Vec<(String, Rect)>,
    tone_cells: Vec<(String, Rect)>,
    tone_rect: Rect,
    pressed: Option<PanelHit>,
}

impl EmojiPanel {
    /// A closed panel over `catalogue`.
    pub fn new(catalogue: &'static Catalogue) -> Self {
        Self {
            catalogue,
            mode: PanelMode::Closed,
            group: 0,
            scroll: 0.0,
            search: SearchField::new().with_placeholder(&i18n::t("osk-emoji-search")),
            tones: None,
            bands: PanelBands::default(),
            search_rect: Rect::ZERO,
            tab_rects: Vec::new(),
            grid: Rect::ZERO,
            content_height: 0.0,
            cell: 0.0,
            cells: Vec::new(),
            headings: Vec::new(),
            letters_rect: Rect::ZERO,
            delete_rect: Rect::ZERO,
            end_search_rect: Rect::ZERO,
            strip: Vec::new(),
            tone_cells: Vec::new(),
            tone_rect: Rect::ZERO,
            pressed: None,
        }
    }

    // --- state ----------------------------------------------------------

    pub fn mode(&self) -> PanelMode {
        self.mode
    }

    pub fn is_open(&self) -> bool {
        self.mode != PanelMode::Closed
    }

    pub fn is_searching(&self) -> bool {
        self.mode == PanelMode::Search
    }

    /// The selected group's index into [`Catalogue::groups`].
    pub fn group(&self) -> usize {
        self.group
    }

    pub fn query(&self) -> &str {
        self.search.value()
    }

    pub fn catalogue(&self) -> &'static Catalogue {
        self.catalogue
    }

    /// The emoji whose skin-tone row is open.
    pub fn tones_for(&self) -> Option<&str> {
        self.tones.as_ref().map(|t| t.base.as_str())
    }

    /// Show the grid.
    pub fn open(&mut self) {
        self.mode = PanelMode::Browse;
        self.tones = None;
        self.pressed = None;
    }

    /// Back to the keys. The query and scroll position go; the group stays.
    pub fn close(&mut self) {
        self.mode = PanelMode::Closed;
        self.search.set_query("");
        self.search.set_focused(false);
        self.scroll = 0.0;
        self.tones = None;
        self.pressed = None;
    }

    /// Type into the search field with the keys.
    pub fn start_search(&mut self) {
        if self.mode == PanelMode::Closed {
            return;
        }
        self.mode = PanelMode::Search;
        self.tones = None;
        self.search.set_focused(true);
    }

    /// Leave the search for the grid, dropping the query.
    pub fn end_search(&mut self) {
        if self.mode != PanelMode::Search {
            return;
        }
        self.mode = PanelMode::Browse;
        self.search.set_query("");
        self.search.set_focused(false);
        self.tones = None;
    }

    /// Show group `index` (clamped) from its top.
    pub fn select_group(&mut self, index: usize) {
        let last = self.catalogue.groups().len().saturating_sub(1);
        self.group = index.min(last);
        self.scroll = 0.0;
        self.tones = None;
    }

    /// Append `text` to the query.
    pub fn type_text(&mut self, text: &str) {
        let mut query = self.search.value().to_owned();
        query.push_str(text);
        self.set_query(&query);
    }

    /// Delete the query's last character.
    pub fn backspace(&mut self) {
        let mut query = self.search.value().to_owned();
        query.pop();
        self.set_query(&query);
    }

    /// Replace the query.
    pub fn set_query(&mut self, query: &str) {
        self.search.set_query(query);
        let _ = self.search.take_dirty();
        self.tones = None;
    }

    /// The best matches for the query, best first.
    pub fn results(&self) -> Vec<String> {
        let query = self.search.value().trim();
        if query.is_empty() {
            return Vec::new();
        }
        self.catalogue
            .search(query, SEARCH_RESULTS)
            .into_iter()
            .map(|e| e.text().to_owned())
            .collect()
    }

    /// The grid's sections: this user's recents, then the selected group.
    pub fn sections(&self, recents: &Recents) -> Vec<Section> {
        let mut sections = Vec::new();
        if !recents.is_empty() {
            sections.push(Section {
                heading: i18n::t("osk-emoji-recent"),
                emoji: recents.items().to_vec(),
            });
        }
        if let Some(group) = self.catalogue.groups().get(self.group) {
            sections.push(Section {
                heading: group.label().to_owned(),
                emoji: self
                    .catalogue
                    .group_emoji(group)
                    .iter()
                    .map(|e| e.text().to_owned())
                    .collect(),
            });
        }
        sections
    }

    /// The emoji the search strip shows: matches, or the recents before a
    /// query is typed.
    fn strip_emoji(&self, recents: &Recents) -> Vec<String> {
        if self.search.value().trim().is_empty() {
            recents.items().to_vec()
        } else {
            self.results()
        }
    }

    /// Open the skin-tone row on `text` (shown by `anchor`). Only emoji that
    /// take a tone have one; returns whether it opened.
    pub fn open_tones(&mut self, text: &str, anchor: Rect) -> bool {
        let Some(found) = self.catalogue.lookup(text) else {
            return false;
        };
        if !found.emoji.takes_skin_tone() {
            return false;
        }
        self.tones = Some(Tones {
            base: found.emoji.text().to_owned(),
            anchor,
        });
        true
    }

    pub fn close_tones(&mut self) {
        self.tones = None;
    }

    /// The open row's choices: the default form, then each tone lightest
    /// first.
    pub fn tone_options(&self) -> Vec<String> {
        let Some(tones) = &self.tones else {
            return Vec::new();
        };
        tone_forms(self.catalogue, &tones.base)
    }

    /// Scroll the grid by `dy` logical px, within its content.
    pub fn scroll_by(&mut self, dy: f64) {
        let max = (self.content_height - self.grid.height()).max(0.0);
        self.scroll = (self.scroll + dy).clamp(0.0, max);
        self.tones = None;
    }

    // --- geometry -------------------------------------------------------

    /// Lay the current face out in `bands`, reading `recents` for the grid.
    pub fn layout(&mut self, bands: PanelBands, recents: &Recents) {
        self.bands = bands;
        self.cells.clear();
        self.headings.clear();
        self.strip.clear();
        self.tab_rects.clear();
        self.tone_cells.clear();
        self.tone_rect = Rect::ZERO;
        self.letters_rect = Rect::ZERO;
        self.delete_rect = Rect::ZERO;
        self.end_search_rect = Rect::ZERO;
        self.grid = Rect::ZERO;
        match self.mode {
            PanelMode::Closed => {
                self.search_rect = Rect::ZERO;
                return;
            }
            PanelMode::Browse => self.layout_browse(recents),
            PanelMode::Search => self.layout_search(recents),
        }
        self.search.apply_layout_external(self.search_rect);
        self.layout_tones();
    }

    fn layout_browse(&mut self, recents: &Recents) {
        let PanelBands {
            suggestion,
            keys,
            row_height,
            ..
        } = self.bands;
        self.search_rect = suggestion.inset(-self.bands.inset / 2.0);
        let tab_h = self.bands.tab_height.min(keys.height() * 0.25);
        let bottom_h = row_height.min(keys.height() * 0.3).max(0.0);
        let groups = self.catalogue.groups().len().max(1);
        let tab_w = keys.width() / groups as f64;
        self.tab_rects = (0..groups)
            .map(|i| {
                let x = keys.x0 + i as f64 * tab_w;
                Rect::new(x, keys.y0, x + tab_w, keys.y0 + tab_h)
            })
            .collect();
        let bottom = Rect::new(keys.x0, keys.y1 - bottom_h, keys.x1, keys.y1);
        let side = (bottom.width() * 0.18).max(bottom_h);
        self.letters_rect = Rect::new(bottom.x0, bottom.y0, bottom.x0 + side, bottom.y1);
        self.delete_rect = Rect::new(bottom.x1 - side, bottom.y0, bottom.x1, bottom.y1);
        self.grid = Rect::new(keys.x0, keys.y0 + tab_h, keys.x1, bottom.y0);

        let cols = ((self.grid.width() / self.bands.min_cell.max(1.0)).floor() as usize).max(1);
        self.cell = self.grid.width() / cols as f64;
        let sections = self.sections(recents);
        self.content_height = sections
            .iter()
            .map(|s| self.bands.heading_height + s.emoji.len().div_ceil(cols) as f64 * self.cell)
            .sum();
        let max = (self.content_height - self.grid.height()).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
        let mut y = self.grid.y0 - self.scroll;
        for section in sections {
            self.headings.push((
                section.heading,
                Point::new(self.grid.x0 + self.bands.inset, y),
            ));
            y += self.bands.heading_height;
            let rows = section.emoji.len().div_ceil(cols) as f64;
            for (i, text) in section.emoji.into_iter().enumerate() {
                let x = self.grid.x0 + (i % cols) as f64 * self.cell;
                let top = y + (i / cols) as f64 * self.cell;
                let rect = Rect::new(x, top, x + self.cell, top + self.cell);
                if rect.y1 > self.grid.y0 && rect.y0 < self.grid.y1 {
                    self.cells.push((text, rect));
                }
            }
            y += rows * self.cell;
        }
    }

    fn layout_search(&mut self, recents: &Recents) {
        let PanelBands {
            toolbar,
            suggestion,
            ..
        } = self.bands;
        self.search_rect = suggestion.inset(-self.bands.inset / 2.0);
        let side = toolbar.height();
        self.end_search_rect = Rect::new(toolbar.x0, toolbar.y0, toolbar.x0 + side, toolbar.y1);
        let cell = side.max(1.0);
        let mut x = self.end_search_rect.x1;
        for text in self.strip_emoji(recents) {
            if x + cell > toolbar.x1 + 0.5 {
                break;
            }
            self.strip
                .push((text, Rect::new(x, toolbar.y0, x + cell, toolbar.y1)));
            x += cell;
        }
        self.cell = cell;
    }

    fn layout_tones(&mut self) {
        let Some(tones) = self.tones.clone() else {
            return;
        };
        let forms = tone_forms(self.catalogue, &tones.base);
        let cell = self.cell.max(self.bands.min_cell);
        let width = cell * forms.len() as f64;
        let bounds = self
            .bands
            .toolbar
            .union(self.bands.suggestion)
            .union(self.bands.keys);
        let x0 = (tones.anchor.center().x - width * 0.5)
            .clamp(bounds.x0, (bounds.x1 - width).max(bounds.x0));
        // Above the pressed emoji when there is room, as a finger covers it.
        let y0 = if tones.anchor.y0 - cell >= bounds.y0 {
            tones.anchor.y0 - cell
        } else {
            (tones.anchor.y1).min(bounds.y1 - cell)
        };
        self.tone_rect = Rect::new(x0, y0, x0 + width, y0 + cell);
        self.tone_cells = forms
            .into_iter()
            .enumerate()
            .map(|(i, text)| {
                let x = x0 + i as f64 * cell;
                (text, Rect::new(x, y0, x + cell, y0 + cell))
            })
            .collect();
    }

    /// What is at `p`; `None` outside the panel.
    pub fn hit(&self, p: Point) -> Option<PanelHit> {
        if !self.is_open() {
            return None;
        }
        if self.tones.is_some() {
            if let Some((text, _)) = self.tone_cells.iter().find(|(_, r)| r.contains(p)) {
                return Some(PanelHit::Tone(text.clone()));
            }
            // Anywhere else only dismisses the row.
            return Some(PanelHit::Nothing);
        }
        if self.search_rect.contains(p) {
            return Some(PanelHit::Search);
        }
        match self.mode {
            PanelMode::Browse => {
                if let Some(i) = self.tab_rects.iter().position(|r| r.contains(p)) {
                    return Some(PanelHit::Group(i));
                }
                if self.letters_rect.contains(p) {
                    return Some(PanelHit::Letters);
                }
                if self.delete_rect.contains(p) {
                    return Some(PanelHit::Delete);
                }
                if self.grid.contains(p) {
                    return Some(
                        self.cells
                            .iter()
                            .find(|(_, r)| r.contains(p))
                            .map_or(PanelHit::Nothing, |(t, _)| PanelHit::Emoji(t.clone())),
                    );
                }
                self.bands.keys.contains(p).then_some(PanelHit::Nothing)
            }
            PanelMode::Search => {
                if self.end_search_rect.contains(p) {
                    return Some(PanelHit::EndSearch);
                }
                if let Some((text, _)) = self.strip.iter().find(|(_, r)| r.contains(p)) {
                    return Some(PanelHit::Emoji(text.clone()));
                }
                self.bands.toolbar.contains(p).then_some(PanelHit::Nothing)
            }
            PanelMode::Closed => None,
        }
    }

    /// The rect an emoji was last laid out at, in the grid or the strip.
    pub fn emoji_rect(&self, text: &str) -> Option<Rect> {
        self.cells
            .iter()
            .chain(&self.strip)
            .find(|(t, _)| t == text)
            .map(|(_, r)| *r)
    }

    /// Whether `p` is over the scrolling grid.
    pub fn over_grid(&self, p: Point) -> bool {
        self.mode == PanelMode::Browse && self.grid.contains(p)
    }

    /// Latch the target a press landed on.
    pub fn press(&mut self, hit: PanelHit) {
        self.pressed = Some(hit);
    }

    /// Take the target the last press latched, if any.
    pub fn take_pressed(&mut self) -> Option<PanelHit> {
        self.pressed.take()
    }

    // --- drawing --------------------------------------------------------

    /// Paint the current face.
    #[allow(
        clippy::too_many_lines,
        reason = "one paint pass keeps the panel's layer order visible: field, tabs, grid, bottom row, tone row"
    )]
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let colors = &theme.tokens.colors;
        match self.mode {
            PanelMode::Closed => return,
            PanelMode::Browse => {
                self.search
                    .draw(scene, transform, text_renderer, theme, presence);
                self.draw_tabs(scene, transform, text_renderer, theme);
                scene.push_layer(
                    Fill::NonZero,
                    BlendMode::default(),
                    1.0,
                    transform,
                    &self.grid,
                );
                {
                    let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
                    for (heading, at) in &self.headings {
                        let band =
                            Rect::new(at.x, at.y, self.grid.x1, at.y + self.bands.heading_height);
                        ctx.text_role_in_band(
                            heading,
                            TextRole::Subheading,
                            colors.text_dim,
                            at.x,
                            band,
                            None,
                        );
                    }
                }
                for (text, rect) in &self.cells {
                    let pressed = self.pressed.as_ref() == Some(&PanelHit::Emoji(text.clone()));
                    draw_emoji(scene, transform, text_renderer, theme, *rect, text, pressed);
                }
                scene.pop_layer();
                let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
                crate::presentation::draw_cell(
                    &mut ctx,
                    self.letters_rect,
                    crate::presentation::CellFace::new(None, &i18n::t("osk-emoji-letters")),
                    crate::presentation::CellState {
                        pressed: self.pressed == Some(PanelHit::Letters),
                        tone: crate::presentation::CellTone::Modifier,
                        ..Default::default()
                    },
                );
                crate::presentation::draw_cell(
                    &mut ctx,
                    self.delete_rect,
                    crate::presentation::CellFace::Icon(ui::icons::Icon::Delete),
                    crate::presentation::CellState {
                        pressed: self.pressed == Some(PanelHit::Delete),
                        tone: crate::presentation::CellTone::Modifier,
                        ..Default::default()
                    },
                );
            }
            PanelMode::Search => {
                self.search
                    .draw(scene, transform, text_renderer, theme, presence);
                let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
                crate::presentation::draw_cell(
                    &mut ctx,
                    self.end_search_rect,
                    crate::presentation::CellFace::Icon(ui::icons::Icon::ArrowLeft),
                    crate::presentation::CellState {
                        pressed: self.pressed == Some(PanelHit::EndSearch),
                        tone: crate::presentation::CellTone::Flat,
                        ..Default::default()
                    },
                );
                if self.strip.is_empty() && !self.search.value().trim().is_empty() {
                    let hint = i18n::t("osk-emoji-no-results");
                    ctx.text_role_in_band(
                        &hint,
                        TextRole::Caption,
                        colors.text_dim,
                        self.end_search_rect.x1 + self.bands.inset,
                        self.bands.toolbar,
                        None,
                    );
                }
                for (text, rect) in &self.strip {
                    let pressed = self.pressed.as_ref() == Some(&PanelHit::Emoji(text.clone()));
                    draw_emoji(scene, transform, text_renderer, theme, *rect, text, pressed);
                }
            }
        }
        if !self.tone_cells.is_empty() {
            let radius = f64::from(theme.tokens.radii.panel);
            ui::surface::shadow(
                scene,
                transform,
                self.tone_rect,
                radius,
                &theme.tokens.elevation.floating,
            );
            ui::surface::panel(scene, transform, self.tone_rect, radius, theme);
            for (text, rect) in &self.tone_cells {
                let pressed = self.pressed.as_ref() == Some(&PanelHit::Tone(text.clone()));
                draw_emoji(scene, transform, text_renderer, theme, *rect, text, pressed);
            }
        }
    }

    /// The group tabs: the toolkit's underline tab strip, each tab marked by
    /// its group's emoji (content, drawn at the icon size).
    fn draw_tabs(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let blanks: Vec<&str> = self.tab_rects.iter().map(|_| "").collect();
        ToggleGroup::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &SegmentedFace::new(&self.tab_rects, &blanks, self.group).style(SegmentedStyle::Tabs),
        );
        let style = TextStyle::content(self.bands.tab_glyph as f32);
        for (group, rect) in self.catalogue.groups().iter().zip(&self.tab_rects) {
            let icon = group.icon();
            let (w, h) = text_renderer.measure_style(icon, None, &style);
            let origin = Point::new(
                rect.center().x - f64::from(w) * 0.5,
                rect.center().y - f64::from(h) * 0.5,
            );
            PaintCtx::new(scene, transform, text_renderer, theme).text(
                icon,
                &style,
                theme.tokens.colors.text_primary,
                origin,
                None,
            );
        }
    }

    // --- accessibility --------------------------------------------------

    /// The panel's semantic nodes: every control it paints, each with the
    /// action target [`crate::widget::OskWidget`] resolves.
    pub fn accessibility(&self) -> Vec<AccessNode> {
        let mut nodes = Vec::new();
        if !self.is_open() {
            return nodes;
        }
        if let Some(field) = Widget::accessibility(&self.search) {
            nodes.push(
                field
                    .with_name(i18n::t("osk-emoji-search"))
                    .with_bounds(self.search_rect)
                    .with_hashed_key("emoji-search")
                    .with_action_target("osk:emoji-search"),
            );
        }
        let button = |name: String, rect: Rect, target: &str| {
            AccessNode::new(Role::Button)
                .with_name(name)
                .with_bounds(rect)
                .with_state(AccessState::new().focusable(true))
                .with_hashed_key(target.to_owned())
                .with_action_target(target.to_owned())
        };
        match self.mode {
            PanelMode::Browse => {
                let tabs: Vec<AccessNode> = self
                    .catalogue
                    .groups()
                    .iter()
                    .zip(&self.tab_rects)
                    .enumerate()
                    .map(|(i, (group, rect))| {
                        AccessNode::new(Role::Tab)
                            .with_name(group.label())
                            .with_bounds(*rect)
                            .with_state(
                                AccessState::new().focusable(true).selected(i == self.group),
                            )
                            .with_position(i + 1, self.tab_rects.len())
                            .with_hashed_key(format!("emoji-group:{}", group.id()))
                            .with_action_target(format!("osk:emoji-group:{i}"))
                    })
                    .collect();
                nodes.push(
                    AccessNode::new(Role::TabBar)
                        .with_name(i18n::t("osk-emoji-panel"))
                        .with_children(tabs),
                );
                for (text, rect) in &self.cells {
                    nodes.push(self.emoji_node(text, *rect));
                }
                nodes.push(button(
                    i18n::t("osk-emoji-letters-name"),
                    self.letters_rect,
                    "osk:emoji-letters",
                ));
                nodes.push(button(
                    i18n::t("osk-emoji-delete"),
                    self.delete_rect,
                    "osk:emoji-delete",
                ));
            }
            PanelMode::Search => {
                nodes.push(button(
                    i18n::t("osk-emoji-end-search"),
                    self.end_search_rect,
                    "osk:emoji-end-search",
                ));
                for (text, rect) in &self.strip {
                    nodes.push(self.emoji_node(text, *rect));
                }
            }
            PanelMode::Closed => {}
        }
        for (text, rect) in &self.tone_cells {
            nodes.push(
                AccessNode::new(Role::Button)
                    .with_name(self.spoken_name(text))
                    .with_bounds(*rect)
                    .with_state(AccessState::new().focusable(true))
                    .with_hashed_key(format!("emoji-tone:{text}"))
                    .with_action_target(format!("osk:emoji-tone:{text}")),
            );
        }
        nodes
    }

    fn emoji_node(&self, text: &str, rect: Rect) -> AccessNode {
        let takes_tone = self
            .catalogue
            .lookup(text)
            .is_some_and(|f| f.emoji.takes_skin_tone());
        let node = AccessNode::new(Role::Button)
            .with_name(self.spoken_name(text))
            .with_bounds(rect)
            .with_state(AccessState::new().focusable(true))
            .with_hashed_key(format!("emoji:{text}"))
            .with_action_target(format!("osk:emoji:{text}"));
        if takes_tone {
            node.with_description(i18n::t("osk-emoji-tones-hint"))
        } else {
            node
        }
    }

    /// The localised name of `text`, with its tone when it carries one.
    pub fn spoken_name(&self, text: &str) -> String {
        let Some(found) = self.catalogue.lookup(text) else {
            return text.to_owned();
        };
        let name = found.emoji.name().to_owned();
        match found.tone {
            None => name,
            Some(tone) => i18n::t_args(
                "osk-emoji-toned",
                &[
                    ("name", name.into()),
                    ("tone", i18n::t(tone_key(tone)).into()),
                ],
            ),
        }
    }
}

/// `base` in its default form, then in each skin tone lightest first.
fn tone_forms(catalogue: &Catalogue, base: &str) -> Vec<String> {
    std::iter::once(None)
        .chain(SkinTone::ALL.into_iter().map(Some))
        .filter_map(|tone| catalogue.with_skin_tone(base, tone).map(str::to_owned))
        .collect()
}

fn tone_key(tone: SkinTone) -> &'static str {
    match tone {
        SkinTone::Light => "osk-emoji-tone-light",
        SkinTone::MediumLight => "osk-emoji-tone-medium-light",
        SkinTone::Medium => "osk-emoji-tone-medium",
        SkinTone::MediumDark => "osk-emoji-tone-medium-dark",
        SkinTone::Dark => "osk-emoji-tone-dark",
    }
}

/// One emoji centred in its cell, over a pressed veneer when held.
fn draw_emoji(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    text: &str,
    pressed: bool,
) {
    let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
    if pressed {
        let grow = f64::from(theme.tokens.spacing.s1) / 2.0;
        let radius = f64::from(theme.tokens.radii.control);
        ctx.fill_round(
            rect.inset(-grow),
            radius,
            theme.tokens.colors.intent.selection,
        );
    }
    let size = (rect.height().min(rect.width()) * GLYPH_CONTENT_PX_RATIO) as f32;
    let style = TextStyle::content(size.max(theme.text(TextRole::Label).size));
    let (w, h) = ctx.text.measure_style(text, None, &style);
    let origin = Point::new(
        rect.center().x - f64::from(w) * 0.5,
        rect.center().y - f64::from(h) * 0.5,
    );
    ctx.text(text, &style, theme.tokens.colors.text_primary, origin, None);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panel() -> EmojiPanel {
        EmojiPanel::new(Catalogue::english())
    }

    fn bands() -> PanelBands {
        PanelBands {
            toolbar: Rect::new(0.0, 0.0, 720.0, 48.0),
            suggestion: Rect::new(0.0, 48.0, 720.0, 88.0),
            keys: Rect::new(0.0, 88.0, 720.0, 312.0),
            row_height: 56.0,
            ..PanelBands::default()
        }
        .sized_by(&Spacing::BASE, &Metrics::BASE)
    }

    #[test]
    fn opens_browses_searches_and_closes() {
        let mut p = panel();
        assert_eq!(p.mode(), PanelMode::Closed);
        p.start_search();
        assert_eq!(p.mode(), PanelMode::Closed, "a closed panel cannot search");
        p.open();
        assert_eq!(p.mode(), PanelMode::Browse);
        p.select_group(3);
        assert_eq!(p.group(), 3);
        p.start_search();
        p.type_text("piz");
        p.type_text("za");
        assert_eq!(p.query(), "pizza");
        p.backspace();
        assert_eq!(p.query(), "pizz");
        p.end_search();
        assert_eq!(p.mode(), PanelMode::Browse);
        assert_eq!(p.query(), "", "leaving the search drops the query");
        assert_eq!(p.group(), 3, "the group survives a search");
        p.close();
        assert!(!p.is_open());
    }

    #[test]
    fn group_index_is_clamped() {
        let mut p = panel();
        p.select_group(usize::MAX);
        assert_eq!(p.group(), Catalogue::english().groups().len() - 1);
    }

    #[test]
    fn recents_come_first_in_the_grid() {
        let p = panel();
        let mut recents = Recents::default();
        assert_eq!(p.sections(&recents).len(), 1, "no recents, no section");
        recents.record("🍕");
        let sections = p.sections(&recents);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].emoji, vec!["🍕".to_owned()]);
        assert_eq!(sections[0].heading, i18n::t("osk-emoji-recent"));
    }

    #[test]
    fn search_ranks_matches_and_fills_the_strip() {
        let mut p = panel();
        p.open();
        p.start_search();
        p.type_text("pizza");
        assert_eq!(p.results().first().map(String::as_str), Some("🍕"));
        p.layout(bands(), &Recents::default());
        assert!(p.emoji_rect("🍕").is_some(), "the match is in the strip");
        let at = p.emoji_rect("🍕").unwrap().center();
        assert_eq!(p.hit(at), Some(PanelHit::Emoji("🍕".into())));
    }

    #[test]
    fn empty_search_strip_offers_recents() {
        let mut p = panel();
        p.open();
        p.start_search();
        let mut recents = Recents::default();
        recents.record("🎉");
        p.layout(bands(), &recents);
        assert!(p.emoji_rect("🎉").is_some());
    }

    #[test]
    fn skin_tone_row_only_for_emoji_that_take_a_tone() {
        let mut p = panel();
        p.open();
        assert!(!p.open_tones("🍕", Rect::ZERO));
        assert!(p.open_tones("👍🏽", Rect::new(100.0, 200.0, 144.0, 244.0)));
        assert_eq!(p.tones_for(), Some("👍"), "the row is on the base form");
        let options = p.tone_options();
        assert_eq!(options.len(), 6);
        assert_eq!(options[0], "👍");
        assert_eq!(options[3], "👍🏽");
        p.layout(bands(), &Recents::default());
        let at = p.tone_cells[3].1.center();
        assert_eq!(p.hit(at), Some(PanelHit::Tone("👍🏽".into())));
        // Anywhere else dismisses rather than acting.
        assert_eq!(p.hit(Point::new(1.0, 300.0)), Some(PanelHit::Nothing));
    }

    #[test]
    fn browse_layout_hits_tabs_grid_and_bottom_row() {
        let mut p = panel();
        p.open();
        p.layout(bands(), &Recents::default());
        let tab = p.tab_rects[2].center();
        assert_eq!(p.hit(tab), Some(PanelHit::Group(2)));
        let (first, rect) = p.cells[0].clone();
        assert_eq!(p.hit(rect.center()), Some(PanelHit::Emoji(first)));
        assert_eq!(p.hit(p.letters_rect.center()), Some(PanelHit::Letters));
        assert_eq!(p.hit(p.delete_rect.center()), Some(PanelHit::Delete));
        assert_eq!(p.hit(p.search_rect.center()), Some(PanelHit::Search));
    }

    #[test]
    fn scrolling_stays_within_the_content() {
        let mut p = panel();
        p.open();
        p.layout(bands(), &Recents::default());
        let first = p.cells[0].clone();
        p.scroll_by(-50.0);
        p.layout(bands(), &Recents::default());
        assert_eq!(p.cells[0], first, "cannot scroll above the top");
        p.scroll_by(1e9);
        p.layout(bands(), &Recents::default());
        assert!(
            p.cells.iter().all(|(t, _)| *t != first.0),
            "scrolled to the end"
        );
    }

    #[test]
    fn toned_emoji_are_named_with_their_tone() {
        let p = panel();
        let plain = p.spoken_name("👍");
        let toned = p.spoken_name("👍🏽");
        assert!(toned.starts_with(&plain), "{toned}");
        assert_ne!(toned, plain);
    }
}
