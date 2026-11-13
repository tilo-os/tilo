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
use ui::widget::DrawChild;

pub const EMOJI_SUGGESTIONS: usize = 2;
const SEARCH_RESULTS: usize = 64;
/// Emoji glyph size relative to its cell. Emoji are content: the size follows
/// the cell, not the text scale.
const GLYPH_CONTENT_PX_RATIO: f64 = 0.55;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelMode {
    Closed,
    Browse,
    Search,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PanelHit {
    Search,
    Group(usize),
    Emoji(String),
    Tone(String),
    Letters,
    Delete,
    EndSearch,
    Nothing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub heading: String,
    pub emoji: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PanelBands {
    pub toolbar: Rect,
    pub suggestion: Rect,
    pub keys: Rect,
    pub row_height: f64,
    pub min_cell: f64,
    pub heading_height: f64,
    pub inset: f64,
    pub tab_glyph: f64,
}

impl PanelBands {
    #[must_use]
    pub fn sized_by(mut self, spacing: &Spacing, metrics: &Metrics) -> Self {
        self.min_cell = f64::from(spacing.s12);
        self.heading_height = f64::from(spacing.s6);
        self.inset = f64::from(spacing.s2);
        self.tab_glyph = f64::from(metrics.icon_md);
        self
    }
}

#[derive(Clone, Debug)]
struct Tones {
    base: String,
    anchor: Rect,
}

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
    pub fn new(catalogue: &'static Catalogue) -> Self {
        Self {
            catalogue,
            mode: PanelMode::Closed,
            group: 0,
            scroll: 0.0,
            // The on-screen keyboard has no window bar.
            search: ui::window_toolbar::exempt!(Service, {
                SearchField::new().with_placeholder(&i18n::t("osk-emoji-search"))
            }),
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

    pub fn mode(&self) -> PanelMode {
        self.mode
    }

    pub fn is_open(&self) -> bool {
        self.mode != PanelMode::Closed
    }

    pub fn is_searching(&self) -> bool {
        self.mode == PanelMode::Search
    }

    pub fn group(&self) -> usize {
        self.group
    }

    pub fn query(&self) -> &str {
        self.search.value()
    }

    pub fn catalogue(&self) -> &'static Catalogue {
        self.catalogue
    }

    pub fn tones_for(&self) -> Option<&str> {
        self.tones.as_ref().map(|t| t.base.as_str())
    }

    pub fn open(&mut self) {
        self.mode = PanelMode::Browse;
        self.tones = None;
        self.pressed = None;
    }

    pub fn close(&mut self) {
        self.mode = PanelMode::Closed;
        self.search.set_query("");
        self.search.set_focused(false);
        self.scroll = 0.0;
        self.tones = None;
        self.pressed = None;
    }

    pub fn start_search(&mut self) {
        if self.mode == PanelMode::Closed {
            return;
        }
        self.mode = PanelMode::Search;
        self.tones = None;
        self.search.set_focused(true);
    }

    pub fn end_search(&mut self) {
        if self.mode != PanelMode::Search {
            return;
        }
        self.mode = PanelMode::Browse;
        self.search.set_query("");
        self.search.set_focused(false);
        self.tones = None;
    }

    pub fn select_group(&mut self, index: usize) {
        let last = self.catalogue.groups().len().saturating_sub(1);
        self.group = index.min(last);
        self.scroll = 0.0;
        self.tones = None;
    }

    pub fn type_text(&mut self, text: &str) {
        let mut query = self.search.value().to_owned();
        query.push_str(text);
        self.set_query(&query);
    }

    pub fn backspace(&mut self) {
        let mut query = self.search.value().to_owned();
        query.pop();
        self.set_query(&query);
    }

    pub fn set_query(&mut self, query: &str) {
        // A programmatic query leaves the caret after the text, as typing would.
        self.search.set_query(query);
        let _ = self.search.take_dirty();
        self.tones = None;
    }

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

    pub fn layout(&mut self, bands: PanelBands, recents: &Recents, theme: &Theme) {
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
        self.search.apply_layout_external(self.search_rect, theme);
        self.layout_tones();
    }

    fn edge(&self) -> f64 {
        let row = self
            .bands
            .row_height
            .min(self.bands.keys.height() * 0.3)
            .max(0.0);
        crate::presentation::key_pad_for_height(row)
    }

    fn field_rect(&self) -> Rect {
        let PanelBands {
            suggestion, keys, ..
        } = self.bands;
        let edge = self.edge();
        let v = self.bands.inset / 2.0;
        Rect::new(
            keys.x0 + edge,
            suggestion.y0 + v,
            (keys.x1 - edge).max(keys.x0 + edge),
            (suggestion.y1 - v).max(suggestion.y0 + v),
        )
    }

    fn layout_browse(&mut self, recents: &Recents) {
        let PanelBands {
            keys, row_height, ..
        } = self.bands;
        self.search_rect = self.field_rect();
        let edge = self.edge();
        let bottom_h = row_height.min(keys.height() * 0.3).max(0.0);
        let bottom = Rect::new(keys.x0, keys.y1 - bottom_h, keys.x1, keys.y1);
        let side = (bottom.width() * 0.12).max(bottom_h);
        self.letters_rect = Rect::new(bottom.x0, bottom.y0, bottom.x0 + side, bottom.y1);
        self.delete_rect = Rect::new(bottom.x1 - side, bottom.y0, bottom.x1, bottom.y1);
        let groups = self.catalogue.groups().len().max(1);
        let tab_w = (self.delete_rect.x0 - self.letters_rect.x1).max(0.0) / groups as f64;
        self.tab_rects = (0..groups)
            .map(|i| {
                let x = self.letters_rect.x1 + i as f64 * tab_w;
                Rect::new(x, bottom.y0, x + tab_w, bottom.y1)
            })
            .collect();
        self.grid = Rect::new(keys.x0, keys.y0, keys.x1, bottom.y0);

        // The columns tile so the first emoji's glyph starts on the edge and
        // the last one's ends on the opposite edge: each cell is wider than
        // its glyph by `overhang` of a cell, half of it on either side.
        let left = keys.x0 + edge;
        let width = (keys.x1 - edge - left).max(1.0);
        let overhang = 1.0 - GLYPH_CONTENT_PX_RATIO;
        let cols = ((width / self.bands.min_cell.max(1.0) + overhang).floor() as usize).max(1);
        self.cell = width / (cols as f64 - overhang);
        let cells_x0 = left - overhang / 2.0 * self.cell;
        let sections = self.sections(recents);
        self.content_height = sections
            .iter()
            .map(|s| self.bands.heading_height + s.emoji.len().div_ceil(cols) as f64 * self.cell)
            .sum();
        let max = (self.content_height - self.grid.height()).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
        let mut y = self.grid.y0 - self.scroll;
        for section in sections {
            self.headings.push((section.heading, Point::new(left, y)));
            y += self.bands.heading_height;
            let rows = section.emoji.len().div_ceil(cols) as f64;
            for (i, text) in section.emoji.into_iter().enumerate() {
                let x = cells_x0 + (i % cols) as f64 * self.cell;
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
        let toolbar = self.bands.toolbar;
        self.search_rect = self.field_rect();
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

    pub fn emoji_rect(&self, text: &str) -> Option<Rect> {
        self.cells
            .iter()
            .chain(&self.strip)
            .find(|(t, _)| t == text)
            .map(|(_, r)| *r)
    }

    pub fn over_grid(&self, p: Point) -> bool {
        self.mode == PanelMode::Browse && self.grid.contains(p)
    }

    pub fn press(&mut self, hit: PanelHit) {
        self.pressed = Some(hit);
    }

    pub fn take_pressed(&mut self) -> Option<PanelHit> {
        self.pressed.take()
    }

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
                    .draw_child(scene, transform, text_renderer, theme, presence);
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
                    .draw_child(scene, transform, text_renderer, theme, presence);
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
        p.layout(bands(), &Recents::default(), &Theme::dark());
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
        p.layout(bands(), &recents, &Theme::dark());
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
        p.layout(bands(), &Recents::default(), &Theme::dark());
        let at = p.tone_cells[3].1.center();
        assert_eq!(p.hit(at), Some(PanelHit::Tone("👍🏽".into())));
        assert_eq!(p.hit(Point::new(1.0, 300.0)), Some(PanelHit::Nothing));
    }

    #[test]
    fn browse_layout_hits_tabs_grid_and_bottom_row() {
        let mut p = panel();
        p.open();
        p.layout(bands(), &Recents::default(), &Theme::dark());
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
        p.layout(bands(), &Recents::default(), &Theme::dark());
        let first = p.cells[0].clone();
        p.scroll_by(-50.0);
        p.layout(bands(), &Recents::default(), &Theme::dark());
        assert_eq!(p.cells[0], first, "cannot scroll above the top");
        p.scroll_by(1e9);
        p.layout(bands(), &Recents::default(), &Theme::dark());
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
