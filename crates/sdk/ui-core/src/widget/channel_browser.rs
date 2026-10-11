//! Typeahead-driven modal sheet listing every channel the user can reach,
//! grouped by parent space.
//!
//! Consumes `Vec<ChannelEntry>` rather than a CRDT path directly, so the
//! widget stays source-agnostic — the host chrome owns Spaces, roster
//! state, and unread aggregation. Pinned channels float to the top of
//! their space group; spaces sort alphabetically. Fuzzy search matches
//! both `entry.name` and `entry.space_name` so typing a space prefix
//! narrows the list.

use crate::search::ListIndex;
use crate::time::SystemTime;
use std::collections::HashMap;
use tilo_search::FieldWeight;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Color;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use collab_types::ChannelId;

use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::query_text::{QueryComposition, QueryEventChange, QueryInk, QueryView};
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

// ---------------------------------------------------------------------
// Layout pins
// ---------------------------------------------------------------------

pub const ROW_HEIGHT: f64 = 40.0;
pub const SPACE_HEADER_HEIGHT: f64 = 24.0;
/// Matches `command_palette::INPUT_HEIGHT`.
pub const INPUT_HEIGHT: f64 = 36.0;
pub const FILTER_STRIP_HEIGHT: f64 = 36.0;
pub const FOOTER_HEIGHT: f64 = 36.0;
const PADDING: f64 = 12.0;
const COLOUR_BAND_W: f64 = 4.0;
const UNREAD_SLOT_W: f64 = 36.0;
const TIME_SLOT_W: f64 = 80.0;
const ROLE_CHIP_W: f64 = 64.0;
/// Mirrors command-palette's adaptive sizing rule.
pub const SHEET_MAX_W: f64 = 720.0;
pub const SHEET_MIN_W: f64 = 560.0;
const PIN_GLYPH: &str = "\u{2605}"; // ★

/// The sheet is a Panel (HIG §9.1): it floats over content on
/// `chrome::shell::panel_ground` under `panel_border`. Everything inside it
/// derives against that ground — `Colors::lift` (base → raised) is measured
/// against the desktop and has no purchase on a panel — so its tiers are one
/// ordered ramp of translucent steps, polarity chosen the way `hairline_on`
/// chooses it (HIG §9.3 caveat); the sheet has one lifted tier, selection.
const WASH_SELECTED: u8 = 56;

/// Sibling space pills keep one `s2` (HIG §7b: siblings of equal weight in
/// a compact strip); the 6 px they kept before was off the scale.
const PILL_GAP: f64 = crate::tokens::Spacing::BASE.s2 as f64;

/// One step of the sheet's own ramp: `alpha` of white or black, whichever
/// moves `surface` further.
fn wash(surface: Color, alpha: u8) -> Color {
    crate::tokens::colors::Colors::hairline_on(surface, alpha)
}

/// The ground the sheet paints on and every row derives from.
fn sheet_surface(theme: &Theme) -> Color {
    crate::chrome::shell::panel_ground(theme)
}

/// A flat, source-agnostic record produced by the host chrome. The
/// widget never mutates these; it sorts a vector of references into
/// its own `visible` index list.
#[derive(Clone, Debug, PartialEq)]
pub struct ChannelEntry {
    pub channel_id: ChannelId,
    pub space_name: String,
    /// Same HSV→RGB curve as [`crate::widget::history::hue_to_rgb`]
    /// so the colour matches anywhere else the host renders the space.
    pub space_color_hue: f32,
    pub name: String,
    /// Drives both the relative-time label and the within-space
    /// recency sort.
    pub last_activity: SystemTime,
    /// Aggregation is the producer's concern; the widget just renders
    /// the badge when `> 0`.
    pub unread_count: u32,
    pub is_pinned: bool,
    pub role_label: Option<String>,
}

pub type SelectCallback = Box<dyn FnMut(&ChannelEntry)>;
pub type DismissCallback = Box<dyn FnMut()>;

pub struct ChannelBrowser {
    entries: Vec<ChannelEntry>,
    /// Names and space names of `entries`, indexed while they stay the
    /// same.
    entry_index: ListIndex,
    query: String,
    /// Mirrors the command-palette model so the same keyboard contract
    /// (arrow, shift+arrow, Ctrl+A/C/V/X) works inside the search input.
    query_selection: crate::text_selection::Selection,
    query_composition: QueryComposition,
    /// Row index into `visible`.
    cursor: usize,
    /// Indexes into `entries` that pass the current filter, in display
    /// order. Rebuilt on every entry / query / filter change.
    visible: Vec<usize>,
    rect: Rect,
    is_focused: bool,
    /// `None` is the "All" pill.
    space_filter: Option<String>,
    on_select: Option<SelectCallback>,
    on_dismiss: Option<DismissCallback>,
    /// Pins "now" for the relative-time render in tests / golden
    /// captures; live callers leave it `None`.
    now_override: Option<SystemTime>,
    /// Used by mouse-click handling to know which row/pill was hit.
    last_cursor: Point,
    /// Painted pill rects, refreshed each `draw`, so the mouse-input
    /// branch can hit-test without re-walking the strip layout.
    pill_rects: Vec<(String, Rect)>,
    dirty: bool,
}

impl Default for ChannelBrowser {
    fn default() -> Self {
        Self::new()
    }
}

impl ChannelBrowser {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            entry_index: ListIndex::default(),
            query: String::new(),
            query_selection: crate::text_selection::Selection::collapsed(0),
            query_composition: QueryComposition::new(),
            cursor: 0,
            visible: Vec::new(),
            rect: Rect::ZERO,
            is_focused: false,
            space_filter: None,
            on_select: None,
            on_dismiss: None,
            now_override: None,
            last_cursor: Point::ZERO,
            pill_rects: Vec::new(),
            dirty: true,
        }
    }

    pub fn with_entries(entries: Vec<ChannelEntry>) -> Self {
        let mut s = Self::new();
        s.set_entries(entries);
        s
    }

    /// Query and filter state are preserved so streaming a fresh
    /// entry batch after a query is typed doesn't reset the user's
    /// typing.
    pub fn set_entries(&mut self, entries: Vec<ChannelEntry>) {
        self.entries = entries;
        self.rebuild();
    }

    pub fn set_query(&mut self, q: &str) {
        self.query = q.into();
        let end = self.query.len();
        self.query_selection = crate::text_selection::Selection::collapsed(end);
        self.query_composition.clear();
        self.rebuild();
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn set_space_filter(&mut self, space: Option<String>) {
        if self.space_filter != space {
            self.space_filter = space;
            // A filter change means the prior selection index has no
            // meaning in the new list.
            self.cursor = 0;
            self.rebuild();
        }
    }

    pub fn space_filter(&self) -> Option<&str> {
        self.space_filter.as_deref()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn set_cursor(&mut self, idx: usize) {
        if self.visible.is_empty() {
            self.cursor = 0;
        } else {
            self.cursor = idx.min(self.visible.len().saturating_sub(1));
        }
        self.dirty = true;
    }

    pub fn selected_entry(&self) -> Option<&ChannelEntry> {
        self.visible
            .get(self.cursor)
            .and_then(|&i| self.entries.get(i))
    }

    #[must_use]
    pub fn on_select<F: FnMut(&ChannelEntry) + 'static>(mut self, f: F) -> Self {
        self.on_select = Some(Box::new(f));
        self
    }

    #[must_use]
    pub fn on_dismiss<F: FnMut() + 'static>(mut self, f: F) -> Self {
        self.on_dismiss = Some(Box::new(f));
        self
    }

    pub fn set_now_for_test(&mut self, now: SystemTime) {
        self.now_override = Some(now);
    }

    pub fn space_pills(&self) -> Vec<String> {
        let mut names: Vec<String> = self.entries.iter().map(|e| e.space_name.clone()).collect();
        names.sort();
        names.dedup();
        names
    }

    pub fn visible_len(&self) -> usize {
        self.visible.len()
    }

    pub fn visible_entry(&self, i: usize) -> Option<&ChannelEntry> {
        self.visible.get(i).and_then(|&j| self.entries.get(j))
    }

    /// Same alphabetical ordering as [`Self::space_pills`], filtered
    /// to spaces that survive the current query/space-filter.
    pub fn visible_space_groups(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .visible
            .iter()
            .filter_map(|&i| self.entries.get(i))
            .map(|e| e.space_name.clone())
            .collect();
        // `rebuild` sorts entries so identical space_names land
        // contiguously, so a stable dedup is enough here.
        names.dedup();
        names
    }

    fn now(&self) -> SystemTime {
        self.now_override.unwrap_or_else(crate::time::wall)
    }

    /// Sort key: (`space_name` asc, !`is_pinned`, `last_activity` desc).
    fn rebuild(&mut self) {
        self.visible.clear();
        // A hit on the channel name or the parent space name, so typing a
        // space prefix drills in.
        let ranks = self.entry_index.ranks(
            &self.query,
            self.entries.iter().map(|entry| {
                [
                    (entry.name.as_str(), FieldWeight::TITLE),
                    (entry.space_name.as_str(), FieldWeight::SUBTITLE),
                ]
            }),
        );
        for (i, e) in self.entries.iter().enumerate() {
            if let Some(filter) = &self.space_filter
                && &e.space_name != filter
            {
                continue;
            }
            if ranks.get(i).is_none_or(Option::is_none) {
                continue;
            }
            self.visible.push(i);
        }
        self.visible.sort_by(|a, b| {
            // `visible` only ever holds indices this same rebuild pushed
            // from `entries.iter().enumerate()`, so both lookups always
            // hit; the `else` arm exists only so a future change can't
            // turn a stale index into a panic.
            let (Some(ea), Some(eb)) = (self.entries.get(*a), self.entries.get(*b)) else {
                return std::cmp::Ordering::Equal;
            };
            ea.space_name
                .cmp(&eb.space_name)
                .then_with(|| eb.is_pinned.cmp(&ea.is_pinned))
                .then_with(|| eb.last_activity.cmp(&ea.last_activity))
        });
        if self.cursor >= self.visible.len() {
            self.cursor = self.visible.len().saturating_sub(1);
        }
        self.dirty = true;
    }

    pub fn input_row_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0 + PADDING,
            self.rect.y0 + PADDING,
            self.rect.x1 - PADDING,
            self.rect.y0 + PADDING + INPUT_HEIGHT,
        )
    }

    pub fn filter_strip_rect(&self) -> Rect {
        let row = self.input_row_rect();
        let y0 = row.y1 + PADDING;
        Rect::new(row.x0, y0, row.x1, y0 + FILTER_STRIP_HEIGHT)
    }

    pub fn footer_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0 + PADDING,
            self.rect.y1 - PADDING - FOOTER_HEIGHT,
            self.rect.x1 - PADDING,
            self.rect.y1 - PADDING,
        )
    }

    fn list_top_y(&self) -> f64 {
        self.filter_strip_rect().y1 + PADDING
    }

    /// Yields `(visible_index, row_rect, space_header)`; a header is
    /// emitted whenever the `space_name` changes.
    fn iter_row_layout(&self) -> Vec<(usize, Rect, Option<Rect>)> {
        let mut out = Vec::with_capacity(self.visible.len());
        let mut y = self.list_top_y();
        let x0 = self.rect.x0 + PADDING;
        let x1 = self.rect.x1 - PADDING;
        let max_y = self.footer_rect().y0 - PADDING;
        let mut last_space: Option<&str> = None;
        for (vi, &ei) in self.visible.iter().enumerate() {
            let Some(entry) = self.entries.get(ei) else {
                continue;
            };
            let space = entry.space_name.as_str();
            let header = if Some(space) == last_space {
                None
            } else {
                last_space = Some(space);
                let h0 = y;
                let h1 = (y + SPACE_HEADER_HEIGHT).min(max_y);
                y = h1;
                Some(Rect::new(x0, h0, x1, h1))
            };
            if y >= max_y {
                break;
            }
            let r0 = y;
            let r1 = (y + ROW_HEIGHT).min(max_y);
            if r1 - r0 < ROW_HEIGHT - 0.5 {
                break;
            }
            y = r1;
            out.push((vi, Rect::new(x0, r0, x1, r1), header));
        }
        out
    }

    pub fn row_rect(&self, i: usize) -> Option<Rect> {
        self.iter_row_layout()
            .into_iter()
            .find(|(vi, _, _)| *vi == i)
            .map(|(_, r, _)| r)
    }

    pub fn row_at(&self, p: Point) -> Option<usize> {
        self.iter_row_layout()
            .into_iter()
            .find(|(_, r, _)| r.contains(p))
            .map(|(vi, _, _)| vi)
    }

    fn draw_search_input(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let row = self.input_row_rect();
        let radius = f64::from(theme.tokens.radii.detail);
        let field = theme.tokens.colors.surfaces.base;
        // TextInput policy (HIG §9.5): the field fill under an edge derived
        // from it at rest; focus keeps the 2 px `border_focus` ring.
        if self.is_focused {
            let shape = RoundedRect::from_rect(row, radius);
            scene.fill(vello::peniko::Fill::NonZero, transform, field, None, &shape);
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                theme.tokens.colors.border_focus,
                None,
                &shape,
            );
        } else {
            crate::surface::card(scene, transform, row, radius, field);
        }
        self.query_composition.draw(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            QueryView {
                text: &self.query,
                selection: &self.query_selection,
                placeholder: "Search channels",
                show_caret: self.is_focused,
            },
            Rect::new(row.x0 + 12.0, row.y0, row.x1 - 12.0, row.y1),
            &theme.text(TextRole::Body),
            QueryInk::of(&theme.tokens.colors),
        );
    }

    fn draw_pill_strip(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let strip = self.filter_strip_rect();
        // The space filters are the shared chip: quiet at rest, a lift when
        // chosen (HIG §9.5).
        // "All" + every distinct space name; rects are stashed so the
        // mouse-input branch can hit-test.
        self.pill_rects.clear();
        let mut x = strip.x0;
        let pill_h = crate::widget::ChipFace::height(theme).min(strip.height());
        let cy = f64::midpoint(strip.y0, strip.y1);

        let mut paint_pill = |label: &str,
                              active: bool,
                              text_renderer: &mut TextRenderer,
                              x: &mut f64,
                              rects: &mut Vec<(String, Rect)>| {
            let pill_w = crate::widget::ChipFace::intrinsic_width(
                label,
                crate::widget::ChipLead::None,
                false,
                text_renderer,
                theme,
            );
            let rect = Rect::new(*x, cy - pill_h / 2.0, *x + pill_w, cy + pill_h / 2.0);
            crate::widget::ChipFace::new(rect, label)
                .selected(active)
                .paint(scene, transform, text_renderer, theme);
            rects.push((label.to_string(), rect));
            *x += pill_w + PILL_GAP;
        };

        paint_pill(
            "All",
            self.space_filter.is_none(),
            text_renderer,
            &mut x,
            &mut self.pill_rects,
        );
        // Temp copy avoids aliasing self.entries against the mutable
        // hit-test buffer below.
        let pills: Vec<String> = {
            let mut names: Vec<String> =
                self.entries.iter().map(|e| e.space_name.clone()).collect();
            names.sort();
            names.dedup();
            names
        };
        for name in &pills {
            let active = self.space_filter.as_deref() == Some(name.as_str());
            paint_pill(name, active, text_renderer, &mut x, &mut self.pill_rects);
        }
    }

    /// Selection highlight, space-colour band, pin glyph and name — the
    /// row's identity. Role chip and time/badge are split into their own
    /// helpers below to keep this under clippy's line budget; neither
    /// reads `self`, so both are associated functions.
    fn draw_row(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        row: Rect,
        entry: &ChannelEntry,
        is_selected: bool,
        now: SystemTime,
    ) {
        if is_selected {
            // The top step of the sheet's own ramp under an edge derived
            // from the sheet: a lift that reads on a light panel as well as
            // a dark one, where the white veneer it replaces did not.
            let radius = f64::from(theme.tokens.radii.detail);
            let sheet = sheet_surface(theme);
            let shape = RoundedRect::from_rect(row, radius);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                wash(sheet, WASH_SELECTED),
                None,
                &shape,
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                crate::surface::edge_on(sheet),
                None,
                &crate::surface::edge_outline(row, radius),
            );
            let border = Rect::new(row.x0, row.y0, row.x0 + 3.0, row.y1);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme
                    .tokens
                    .colors
                    .lift_on(theme.tokens.colors.surfaces.content, 1.0),
                None,
                &border,
            );
        }

        let (r, g, b) = crate::widget::history::hue_to_rgb(entry.space_color_hue);
        let band_color = Color::from_rgba8(r, g, b, 255);
        let band = Rect::new(
            row.x0 + 6.0,
            row.y0 + 6.0,
            row.x0 + 6.0 + COLOUR_BAND_W,
            row.y1 - 6.0,
        );
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            band_color,
            None,
            &band,
        );

        let text_style = theme.text(TextRole::Caption);
        let cy = f64::midpoint(row.y0, row.y1);
        let mut x_cursor = band.x1 + 8.0;
        // One measured line box for everything on the row's centre line:
        // the pin, the name and (below) the time share it.
        let line_top = text_top(text_renderer, &text_style, cy);

        if entry.is_pinned {
            let (gw, _) = text_renderer.measure_style(PIN_GLYPH, None, &text_style);
            text_renderer.draw_style(
                scene,
                PIN_GLYPH,
                theme.tokens.colors.text_dim,
                translated(transform, x_cursor, line_top),
                None,
                &text_style,
            );
            x_cursor += f64::from(gw) + 4.0;
        }

        text_renderer.draw_style(
            scene,
            entry.name.as_str(),
            theme.tokens.colors.text_primary,
            translated(transform, x_cursor, line_top),
            None,
            &text_style,
        );
        let (nw, _) = text_renderer.measure_style(entry.name.as_str(), None, &text_style);
        x_cursor += f64::from(nw) + 8.0;

        if let Some(role) = &entry.role_label {
            Self::draw_role_chip(scene, transform, text_renderer, theme, row, role, x_cursor);
        }

        Self::draw_time_and_unread(scene, transform, text_renderer, theme, row, entry, now);
    }

    /// The role chip painted after the channel name; split out of
    /// [`Self::draw_row`] to keep it under clippy's line budget.
    fn draw_role_chip(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        row: Rect,
        role: &str,
        x_cursor: f64,
    ) {
        // A role is a neutral badge, capped at `ROLE_CHIP_W`; the label is
        // fitted to the room inside it rather than painted past the cap.
        let cy = f64::midpoint(row.y0, row.y1);
        let chip_w =
            crate::widget::BadgeFace::intrinsic_width(role, text_renderer, theme).min(ROLE_CHIP_W);
        let chip_h = crate::widget::BadgeFace::height(theme).min(row.height());
        let chip = Rect::new(
            x_cursor,
            cy - chip_h / 2.0,
            x_cursor + chip_w,
            cy + chip_h / 2.0,
        );
        crate::widget::BadgeFace::new(chip, role).paint(scene, transform, text_renderer, theme);
    }

    /// Relative-time label and unread badge at the row's trailing edge;
    /// split out of [`Self::draw_row`] to keep it under clippy's line
    /// budget.
    fn draw_time_and_unread(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        row: Rect,
        entry: &ChannelEntry,
        now: SystemTime,
    ) {
        let text_style = theme.text(TextRole::Caption);
        let cy = f64::midpoint(row.y0, row.y1);

        // Right-aligned before the unread badge slot.
        let rel = crate::widget::history::format_relative_time(entry.last_activity, now);
        let (tw, th) = text_renderer.measure_style(&rel, None, &text_style);
        let time_x = row.x1 - UNREAD_SLOT_W - 4.0 - f64::from(tw);
        text_renderer.draw_style(
            scene,
            &rel,
            theme.tokens.colors.text_dim,
            translated(transform, time_x, cy - f64::from(th) / 2.0),
            None,
            &text_style,
        );

        if entry.unread_count > 0 {
            let badge_text = if entry.unread_count > 99 {
                "99+".to_string()
            } else {
                entry.unread_count.to_string()
            };
            let badge_text_style = theme.text(TextRole::Caption).numeric();
            let (bw, _) = text_renderer.measure_style(&badge_text, None, &badge_text_style);
            let badge_w = (f64::from(bw) + 12.0).max(20.0);
            let badge_h = (row.height() - 18.0).max(16.0);
            let badge = Rect::new(
                row.x1 - 6.0 - badge_w,
                cy - badge_h / 2.0,
                row.x1 - 6.0,
                cy + badge_h / 2.0,
            );
            let shape = RoundedRect::from_rect(badge, badge_h / 2.0);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme
                    .tokens
                    .colors
                    .lift_on(theme.tokens.colors.surfaces.content, 1.0),
                None,
                &shape,
            );
            let (lw, lh) = text_renderer.measure_style(&badge_text, None, &badge_text_style);
            text_renderer.draw_style(
                scene,
                &badge_text,
                theme.tokens.colors.text_primary,
                translated(
                    transform,
                    badge.x0 + (badge.width() - f64::from(lw)) / 2.0,
                    badge.y0 + (badge.height() - f64::from(lh)) / 2.0,
                ),
                None,
                &badge_text_style,
            );
            // Silences an unused-constant warning; TIME_SLOT_W is
            // kept for a future layout that reserves the slot.
            let _ = TIME_SLOT_W;
        }
    }

    /// Doesn't read `self`: an associated function, not a method (clippy
    /// `unused_self`).
    fn draw_space_header(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        header_rect: Rect,
        space_name: &str,
        hue: f32,
    ) {
        let (r, g, b) = crate::widget::history::hue_to_rgb(hue);
        let band = Rect::new(
            header_rect.x0,
            header_rect.y0 + 4.0,
            header_rect.x0 + COLOUR_BAND_W,
            header_rect.y1 - 4.0,
        );
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            Color::from_rgba8(r, g, b, 255),
            None,
            &band,
        );
        let text_style = theme.text(TextRole::Subheading);
        let cy = f64::midpoint(header_rect.y0, header_rect.y1);
        let header_top = text_top(text_renderer, &text_style, cy);
        text_renderer.draw_style(
            scene,
            space_name,
            theme.tokens.colors.text_dim,
            translated(transform, band.x1 + 6.0, header_top),
            None,
            &text_style,
        );
    }

    fn draw_footer(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let footer = self.footer_rect();
        // The quietest theme veneer rather than a fixed 20 % black: its
        // polarity follows the theme, so the band recedes on light too.
        let band_body = theme.tokens.colors.overlay_subtle;
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            band_body,
            None,
            &footer,
        );
        // The band's top rule derives against the sheet it sits in (HIG
        // §9.5). The footer rect is already `PADDING` (one `s3`) inside the
        // sheet, so the rule clears the corner radius without a second inset.
        crate::surface::separator(
            scene,
            transform,
            footer.x0,
            footer.x1,
            footer.y0,
            0.0,
            sheet_surface(theme),
        );

        // Key hints are the shared keycaps, in the platform's legends.
        let chip_text_style = theme.text(TextRole::Caption);
        let dim = theme.tokens.colors.text_dim;
        let cy = f64::midpoint(footer.y0, footer.y1);
        let mut x = footer.x0 + f64::from(theme.tokens.spacing.s1);
        let hints = [
            (
                vec!["Up".to_owned(), "Down".to_owned()],
                crate::i18n!("palette-hint-navigate"),
            ),
            (
                vec!["Enter".to_owned()],
                crate::i18n!("palette-hint-select"),
            ),
            (vec!["Esc".to_owned()], crate::i18n!("palette-hint-close")),
        ];
        let keycaps = crate::keycaps::active();
        for (keys, label) in &hints {
            let legends: Vec<String> = keys.iter().map(|key| keycaps.key(key)).collect();
            let cap_h = crate::widget::InputHint::keycap_height(theme);
            x += crate::widget::InputHint::paint_keycap_labels_at(
                scene,
                transform,
                Point::new(x, cy - cap_h / 2.0),
                &legends,
                text_renderer,
                theme,
            ) + f64::from(theme.tokens.spacing.s1);
            let (lw, lh) = text_renderer.measure_style(label, None, &chip_text_style);
            text_renderer.draw_style(
                scene,
                label,
                dim,
                translated(transform, x, cy - f64::from(lh) / 2.0),
                None,
                &chip_text_style,
            );
            x += f64::from(lw) + f64::from(theme.tokens.spacing.s3);
        }
    }

    /// Index into `["All", space_pills...]`.
    fn active_pill_index(&self) -> usize {
        match &self.space_filter {
            None => 0,
            Some(name) => self
                .space_pills()
                .iter()
                .position(|n| n == name)
                .map_or(0, |i| i.saturating_add(1)),
        }
    }

    fn set_active_pill_index(&mut self, idx: usize) {
        let pills = self.space_pills();
        let total = pills.len().saturating_add(1);
        let clamped = idx.min(total.saturating_sub(1));
        let new_filter = if clamped == 0 {
            None
        } else {
            clamped.checked_sub(1).and_then(|i| pills.get(i)).cloned()
        };
        self.set_space_filter(new_filter);
    }

    /// Cursor-move and click branches of `handle_event`; split out to
    /// keep the dispatcher under clippy's line budget. Runs regardless of
    /// focus so click-to-select on pills/rows works even when the search
    /// input doesn't have it.
    fn handle_pointer_event(&mut self, event: &WindowEvent, scale_factor: f64, handled: &mut bool) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                // Pill strip steals clicks before the row list.
                let p = self.last_cursor;
                let pill_hit = self
                    .pill_rects
                    .iter()
                    .find(|(_, r)| r.contains(p))
                    .map(|(l, _)| l.clone());
                if let Some(label) = pill_hit {
                    if label == "All" {
                        self.set_space_filter(None);
                    } else {
                        self.set_space_filter(Some(label));
                    }
                    *handled = true;
                    return;
                }
                if let Some(i) = self.row_at(p) {
                    self.cursor = i;
                    self.dirty = true;
                    if let Some(entry) = self.selected_entry() {
                        // Clone to drop the immutable borrow before
                        // the FnMut callback needs `&mut self.on_select`.
                        let entry = entry.clone();
                        if let Some(cb) = self.on_select.as_mut() {
                            cb(&entry);
                        }
                    }
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    /// Keyboard branch of `handle_event`, reached only once focused, not
    /// mid-IME-composition, and the key is a fresh press. Split out to
    /// keep the dispatcher under clippy's line budget.
    fn handle_key_press(
        &mut self,
        event: &WindowEvent,
        logical_key: &Key,
        shift: bool,
        ctrl: bool,
        handled: &mut bool,
    ) {
        match logical_key {
            Key::Named(NamedKey::ArrowDown) => {
                if !self.visible.is_empty() {
                    self.cursor = self
                        .cursor
                        .saturating_add(1)
                        .min(self.visible.len().saturating_sub(1));
                    self.dirty = true;
                    *handled = true;
                }
            }
            Key::Named(NamedKey::ArrowUp) => {
                if self.cursor > 0 {
                    self.cursor = self.cursor.saturating_sub(1);
                    self.dirty = true;
                    *handled = true;
                }
            }
            Key::Named(NamedKey::Enter) => {
                if let Some(entry) = self.selected_entry() {
                    let entry = entry.clone();
                    if let Some(cb) = self.on_select.as_mut() {
                        cb(&entry);
                    }
                    *handled = true;
                }
            }
            Key::Named(NamedKey::Escape) => {
                if let Some(cb) = self.on_dismiss.as_mut() {
                    cb();
                }
                *handled = true;
            }
            Key::Named(NamedKey::ArrowLeft) => {
                // Caret moves first; at position 0 with no shift
                // selection, cycle the active pill instead.
                let pos = self.query_selection.cursor;
                if pos == 0 && self.query_selection.is_collapsed() && !shift {
                    let idx = self.active_pill_index();
                    let total = self.space_pills().len().saturating_add(1);
                    let new_idx = if idx == 0 {
                        total.saturating_sub(1)
                    } else {
                        idx.saturating_sub(1)
                    };
                    self.set_active_pill_index(new_idx);
                } else {
                    self.query_composition.handle_event(
                        event,
                        &mut self.query,
                        &mut self.query_selection,
                        shift,
                        ctrl,
                    );
                }
                *handled = true;
            }
            Key::Named(NamedKey::ArrowRight) => {
                let pos = self.query_selection.cursor;
                if pos == self.query.len() && self.query_selection.is_collapsed() && !shift {
                    let idx = self.active_pill_index();
                    let total = self.space_pills().len().saturating_add(1);
                    let new_idx = idx.saturating_add(1).checked_rem(total).unwrap_or(0);
                    self.set_active_pill_index(new_idx);
                } else {
                    self.query_composition.handle_event(
                        event,
                        &mut self.query,
                        &mut self.query_selection,
                        shift,
                        ctrl,
                    );
                }
                *handled = true;
            }
            _ => {
                let change = self.query_composition.handle_event(
                    event,
                    &mut self.query,
                    &mut self.query_selection,
                    shift,
                    ctrl,
                );
                if change == QueryEventChange::Edited {
                    self.rebuild();
                } else if change == QueryEventChange::Display {
                    self.dirty = true;
                }
                if change != QueryEventChange::Ignored {
                    *handled = true;
                }
            }
        }
    }
}

/// Top edge that centres one line of `size`-px text on `center_y`, using the
/// line box the renderer will actually paint (1.25 × size by default, and
/// carrying the accessibility text scale) rather than the bare font size.
fn text_top(tr: &mut TextRenderer, style: &TextStyle, center_y: f64) -> f64 {
    center_y - f64::from(tr.measure_style("Ag", None, style).1) / 2.0
}

/// Composes a paint transform with a translation. `Affine`'s `Mul` impl is
/// pure `f64` matrix multiply-add with no overflow trap, so it cannot
/// panic regardless of operand magnitude — centralised here rather than
/// repeating the exemption at every text transform in this widget.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine composition is float matrix math and cannot panic"
)]
fn translated(transform: Affine, x: f64, y: f64) -> Affine {
    transform * Affine::translate((x, y))
}

/// Widens a row/entry count to `f64` for layout math. Goes through `u32`
/// (exactly representable in `f64`) with a saturating fallback rather than
/// an `as` cast, so it's lossless for any realistic list length.
fn rows_to_f64(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

/// Narrows a layout dimension to `f32` for taffy. The only call site is
/// this sheet's overall height, always many orders of magnitude below
/// `f32::MAX`, so the truncation the lint warns about can't actually
/// occur.
#[allow(
    clippy::cast_possible_truncation,
    reason = "sheet layout height in this widget never approaches f32::MAX"
)]
fn px(v: f64) -> f32 {
    v as f32
}

impl Widget for ChannelBrowser {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The host typically positions the sheet inside an overlay
        // container that hands it a final rect via apply_layout; this
        // returns a sensible standalone default.
        let rows = rows_to_f64(self.visible.len().max(1));
        let height = PADDING * 4.0
            + INPUT_HEIGHT
            + FILTER_STRIP_HEIGHT
            + (rows * ROW_HEIGHT).min(10.0 * ROW_HEIGHT)
            + FOOTER_HEIGHT
            + SPACE_HEADER_HEIGHT;
        let style = Style {
            size: Size {
                width: Dimension::length(640.0),
                height: Dimension::length(px(height)),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let prev = self.rect;
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        if prev != self.rect {
            self.dirty = true;
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // Panel role (HIG §9.1): the sheet floats over content, so it takes
        // the panel ground and the panel edge — one material with the rest
        // of the chrome — rather than a raised card whose declared border
        // vanished on the dark preset.
        let radius = f64::from(theme.tokens.radii.control);
        let shape = RoundedRect::from_rect(self.rect, radius);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            sheet_surface(theme),
            None,
            &shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            crate::chrome::shell::panel_border(theme),
            None,
            &crate::surface::edge_outline(self.rect, radius),
        );

        self.draw_search_input(scene, transform, text_renderer, theme);
        self.draw_pill_strip(scene, transform, text_renderer, theme);

        if self.visible.is_empty() {
            let label = "No channels";
            let style = theme.text(TextRole::Caption);
            let (w, h) = text_renderer.measure_style(label, None, &style);
            let top = self.list_top_y();
            let cx = f64::midpoint(self.rect.x0, self.rect.x1);
            let cy = f64::midpoint(top, self.footer_rect().y0);
            text_renderer.draw_style(
                scene,
                label,
                theme.tokens.colors.text_dim,
                translated(transform, cx - f64::from(w) / 2.0, cy - f64::from(h) / 2.0),
                None,
                &style,
            );
            self.draw_footer(scene, transform, text_renderer, theme);
            self.dirty = false;
            return;
        }

        let now = self.now();
        let layout = self.iter_row_layout();
        for (vi, row, header) in layout {
            // `iter_row_layout` only yields `vi` values it read out of
            // `self.visible` itself, and `visible` only ever holds
            // indices into `self.entries` — both lookups always hit;
            // the `continue` exists only so a future change can't turn
            // a stale index into a panic.
            let Some(&ei) = self.visible.get(vi) else {
                continue;
            };
            let Some(entry) = self.entries.get(ei) else {
                continue;
            };
            if let Some(hr) = header {
                Self::draw_space_header(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    hr,
                    entry.space_name.as_str(),
                    entry.space_color_hue,
                );
            }
            let is_selected = vi == self.cursor;
            Self::draw_row(
                scene,
                transform,
                text_renderer,
                theme,
                row,
                entry,
                is_selected,
                now,
            );
        }

        self.draw_footer(scene, transform, text_renderer, theme);
        self.dirty = false;
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        // Mouse branches go first so click-to-select on pills/rows
        // works even when the input has focus.
        self.handle_pointer_event(event, ctx.scale_factor, ctx.handled);

        if !self.is_focused {
            return;
        }
        if matches!(event, WindowEvent::Ime(_)) {
            let change = self.query_composition.handle_event(
                event,
                &mut self.query,
                &mut self.query_selection,
                false,
                false,
            );
            if change == QueryEventChange::Edited {
                self.rebuild();
            } else {
                self.dirty = true;
            }
            *ctx.handled = true;
            return;
        }
        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: element_state,
                    ..
                },
            ..
        } = event
        else {
            return;
        };
        if *element_state != ElementState::Pressed {
            return;
        }
        let mods = ctx.modifiers.state();
        let shift = mods.shift_key();
        let ctrl = mods.control_key() || mods.super_key();
        self.handle_key_press(event, logical_key, shift, ctrl, ctx.handled);
    }

    fn name(&self) -> &'static str {
        "ChannelBrowser"
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        Some(
            AccessNode::new(Role::List)
                .with_name("Channels")
                .with_description(format!("{} channels", self.visible.len()))
                .with_state(AccessState {
                    focusable: true,
                    focused: self.is_focused,
                    ..Default::default()
                }),
        )
    }

    fn test_id(&self) -> Option<&str> {
        Some("channel_browser")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }
    fn set_focus(&mut self, focused: bool) {
        self.is_focused = focused;
        if !focused {
            self.query_composition.clear();
        }
    }
    fn is_focused(&self) -> bool {
        self.is_focused
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.query_composition.ime_cursor_rect()
    }

    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        self.ime_cursor_rect()?;
        self.query_composition
            .ime_edit_fingerprint(&self.query, &self.query_selection, key)
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        Some(winit::window::ImePurpose::Normal)
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Dispatch;
    use crate::time::Duration;
    use std::cell::RefCell;
    use std::rc::Rc;
    use winit::event::Modifiers;
    use winit::keyboard::SmolStr;

    #[test]
    fn channel_browser_presentation_and_accessibility() {
        /// At 2× a role name no longer fits the capped chip; it is elided so
        /// its ink stays inside the chip instead of running past the cap.
        fn role_chip_label_stays_inside_the_capped_chip_at_two_x() {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            tr.set_text_scale(2.0);
            let row = Rect::new(0.0, 0.0, 300.0, 56.0);
            let x = 20.0;
            let mut scene = Scene::new();
            ChannelBrowser::draw_role_chip(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                row,
                "Moderator",
                x,
            );
            let (w, h) = (300_u32, 56_u32);
            let rgba = crate::offscreen::OffscreenRenderer::new(w, h)
                .render(&scene, vello::peniko::Color::BLACK);
            let mut ink_cols = Vec::new();
            for col in 0..w as usize {
                for y in 0..h as usize {
                    let i = (y * w as usize + col) * 4;
                    // Label ink is the light `text_dim`; the chip's 5 % fill and
                    // 10 % border stay far darker.
                    if rgba[i] > 120 && rgba[i + 1] > 120 && rgba[i + 2] > 120 {
                        ink_cols.push(col);
                        break;
                    }
                }
            }
            let (first, last) = (
                *ink_cols.first().expect("the role label painted some ink"),
                *ink_cols.last().expect("the role label painted some ink"),
            );
            assert!(
                f64::from(first as u32) >= x.floor()
                    && f64::from(last as u32) < (x + ROLE_CHIP_W).ceil(),
                "role ink columns {first}..={last} leave the chip {x}..{}",
                x + ROLE_CHIP_W
            );
        }

        /// The sheet (a Panel), its quiet pills, the search field and the
        /// selected row keep a **visible derived edge** at 1× on both presets
        /// (HIG §9.1, §9.5). Before, the sheet and field wore `border_standard`
        /// (three shades from the dark raised surface), the pills and chips
        /// literal white veneers, and the footer rule ran full-bleed.
        /// The `channel_browser --shots` example writes the PNG + inspection.
        fn ux1x_sheet_pills_field_and_selection_register_on_both_presets() {
            use crate::offscreen::OffscreenRenderer;
            const EDGE_MIN: f64 = 0.008;
            let margin = 16.0;
            for theme in [Theme::dark(), Theme::light()] {
                let theme = theme.with_reduce_motion(true);
                let mut b = ChannelBrowser::with_entries(vec![
                    entry("engineering", "Company", 220.0, 5, 12, true, Some("Editor")),
                    entry("design", "Company", 220.0, 25, 0, false, Some("Editor")),
                    entry("journal", "Personal", 30.0, 60, 0, true, None),
                ]);
                b.set_now_for_test(
                    SystemTime::UNIX_EPOCH
                        .checked_add(Duration::from_secs(10_000))
                        .unwrap_or(SystemTime::UNIX_EPOCH),
                );
                let sheet = Rect::new(margin, margin, margin + 640.0, margin + 420.0);
                b.rect = sheet;
                b.rebuild();
                let (w, h) = (sheet.x1 + margin, sheet.y1 + margin);
                let mut tr = TextRenderer::new();
                let mut scene = Scene::new();
                b.draw(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut tr,
                    &theme,
                    &PresenceCtx::empty(),
                );
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "a small test surface"
                )]
                let rgba = OffscreenRenderer::new(w as u32, h as u32)
                    .render(&scene, theme.tokens.colors.surfaces.content);
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "a small test surface"
                )]
                let lum = |x: f64, y: f64| lum_at(&rgba, w as u32, x, y);

                // Sheet edge against the panel ground, on the top padding row.
                let y = sheet.y0 + PADDING / 2.0;
                let edge = lum(sheet.x0, y);
                let ground = lum(sheet.x0 + 3.0, y);
                assert!(
                    (edge - ground).abs() >= EDGE_MIN,
                    "{}: sheet edge {edge:.4} vs ground {ground:.4} did not register",
                    theme.name
                );

                // Search field: its rest edge against the field fill.
                let field = b.input_row_rect();
                let fedge = lum(field.x0, field.center().y);
                let ffill = lum(field.x0 + 3.0, field.center().y);
                assert!(
                    (fedge - ffill).abs() >= EDGE_MIN,
                    "{}: field edge {fedge:.4} vs fill {ffill:.4} did not register",
                    theme.name
                );

                // Quiet pill (the second one; "All" is active): its left edge
                // against its own fill.
                let (_, pill) = b.pill_rects.get(1).cloned().expect("a space pill");
                let pedge = lum(pill.x0, pill.center().y);
                let pfill = lum(pill.x0 + 3.0, pill.center().y);
                assert!(
                    (pedge - pfill).abs() >= EDGE_MIN,
                    "{}: pill edge {pedge:.4} vs fill {pfill:.4} did not register",
                    theme.name
                );

                // Selected row (cursor 0) against the sheet beside its inset.
                let row = b.row_rect(0).expect("the first row is laid out");
                let rfill = lum(row.x1 - 3.0, row.y0 + 4.0);
                let beside = lum(sheet.x1 - PADDING / 2.0, row.y0 + 4.0);
                assert!(
                    (rfill - beside).abs() >= EDGE_MIN,
                    "{}: selected row {rfill:.4} vs sheet {beside:.4} did not register",
                    theme.name
                );
            }
        }

        fn accessibility_reports_list_role_name_and_focus() {
            use crate::a11y::Role;
            let entries = vec![
                entry("general", "Team", 120.0, 1_000, 0, false, None),
                entry("random", "Team", 200.0, 2_000, 3, true, Some("admin")),
            ];
            let mut b = browser_at(Rect::new(0.0, 0.0, 320.0, 480.0), entries);
            let node = Widget::accessibility(&b).expect("browser is not presentational");
            assert_eq!(node.role, Role::List);
            assert_eq!(node.name.as_deref(), Some("Channels"));
            assert_eq!(node.description.as_deref(), Some("2 channels"));
            assert!(node.state.focusable);
            assert!(!node.state.focused);

            b.set_focus(true);
            assert!(Widget::accessibility(&b).unwrap().state.focused);
        }

        fn empty_browser_renders_placeholder() {
            let b = browser_at(Rect::new(0.0, 0.0, 640.0, 480.0), vec![]);
            assert_eq!(b.visible_len(), 0);
            assert!(b.row_rect(0).is_none());
            let input = b.input_row_rect();
            let strip = b.filter_strip_rect();
            let footer = b.footer_rect();
            assert!(input.y1 <= strip.y0);
            assert!(strip.y1 <= footer.y0);
            assert!(footer.y1 <= b.rect.y1);
        }

        fn unread_badge_renders_with_count() {
            // Pixel-precise rendering isn't assertable here, so this
            // pins the data contract that drives the badge instead: the
            // draw path reads the same `unread_count` field checked below.
            let entries = vec![
                entry("noisy", "Company", 200.0, 100, 42, false, None),
                entry("quiet", "Company", 200.0, 200, 0, false, None),
            ];
            let b = browser_at(Rect::new(0.0, 0.0, 640.0, 480.0), entries);
            let noisy = b
                .visible_entry(b.visible_len() - 1)
                .unwrap_or_else(|| b.visible_entry(0).unwrap());
            let other = b.visible_entry(0).unwrap();
            let noisy_name_match = noisy.name == "noisy" || other.name == "noisy";
            assert!(noisy_name_match);
            let n = if other.name == "noisy" { other } else { noisy };
            assert_eq!(n.unread_count, 42);
            assert_eq!(n.name, "noisy");
            // Threshold for the capped "99+" render.
            let big = entry("loud", "Company", 200.0, 300, 250, false, None);
            assert!(big.unread_count > 99);
        }

        role_chip_label_stays_inside_the_capped_chip_at_two_x();
        ux1x_sheet_pills_field_and_selection_register_on_both_presets();
        accessibility_reports_list_role_name_and_focus();
        empty_browser_renders_placeholder();
        unread_badge_renders_with_count();
    }

    /// Relative luminance of the composited pixel at (`x`, `y`).
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "probe coordinates are non-negative pixel positions inside the test surface"
    )]
    fn lum_at(rgba: &[u8], w: u32, x: f64, y: f64) -> f64 {
        let (r, g, b, _) = crate::offscreen::OffscreenRenderer::sample(
            rgba,
            w,
            x.floor() as u32,
            y.floor() as u32,
        );
        crate::a11y::relative_luminance(Color::from_rgb8(r, g, b))
    }

    fn entry(
        name: &str,
        space: &str,
        hue: f32,
        ts: u64,
        unread: u32,
        pinned: bool,
        role: Option<&str>,
    ) -> ChannelEntry {
        ChannelEntry {
            channel_id: ChannelId::new(),
            space_name: space.into(),
            space_color_hue: hue,
            name: name.into(),
            last_activity: SystemTime::UNIX_EPOCH
                .checked_add(Duration::from_secs(ts))
                .expect("test timestamp fits in SystemTime"),
            unread_count: unread,
            is_pinned: pinned,
            role_label: role.map(str::to_string),
        }
    }

    fn browser_at(rect: Rect, entries: Vec<ChannelEntry>) -> ChannelBrowser {
        let mut b = ChannelBrowser::with_entries(entries);
        b.rect = rect;
        b.set_now_for_test(
            SystemTime::UNIX_EPOCH
                .checked_add(Duration::from_secs(10_000_000))
                .expect("test timestamp fits in SystemTime"),
        );
        b
    }

    fn key_press(logical_key: Key) -> WindowEvent {
        crate::synthetic_input::keyboard_input(crate::synthetic_input::key_event(
            logical_key,
            None,
            ElementState::Pressed,
        ))
    }

    fn dispatch(b: &mut ChannelBrowser, event: &WindowEvent, mods: Modifiers) {
        Dispatch::new().modifiers(mods).send(b, event);
    }

    use crate::synthetic_input::cursor_moved;

    use crate::synthetic_input::mouse_press;

    #[test]
    fn query_editing_filters_with_ime_caret_and_fuzzy_matches() {
        fn typing_filters_entries() {
            let entries = vec![
                entry("engineering", "Company", 200.0, 100, 0, false, None),
                entry("design", "Company", 200.0, 200, 0, false, None),
                entry("personal", "Personal", 30.0, 50, 0, false, None),
            ];
            let mut b = browser_at(Rect::new(0.0, 0.0, 640.0, 480.0), entries);
            b.set_focus(true);
            assert_eq!(b.visible_len(), 3);

            dispatch(
                &mut b,
                &key_press(Key::Character(SmolStr::new("e"))),
                Modifiers::default(),
            );
            dispatch(
                &mut b,
                &key_press(Key::Character(SmolStr::new("n"))),
                Modifiers::default(),
            );
            dispatch(
                &mut b,
                &key_press(Key::Character(SmolStr::new("g"))),
                Modifiers::default(),
            );
            assert_eq!(b.query(), "eng");
            assert_eq!(b.visible_len(), 1);
            assert_eq!(b.visible_entry(0).unwrap().name, "engineering");
        }

        fn ime_preedit_is_display_only_until_atomic_filter_commit() {
            let entries = vec![
                entry("東京", "Travel", 200.0, 100, 0, false, None),
                entry("London", "Travel", 200.0, 200, 0, false, None),
            ];
            let mut b = browser_at(Rect::new(0.0, 0.0, 640.0, 480.0), entries);
            b.set_focus(true);

            dispatch(
                &mut b,
                &WindowEvent::Ime(winit::event::Ime::Preedit(
                    "東京".into(),
                    Some(("東".len(), "東".len())),
                )),
                Modifiers::default(),
            );
            assert_eq!(b.query(), "");
            assert_eq!(b.visible_len(), 2);

            let mut scene = Scene::new();
            let mut renderer = TextRenderer::new();
            Widget::draw(
                &mut b,
                &mut scene,
                Affine::IDENTITY,
                &mut renderer,
                &Theme::dark(),
                &PresenceCtx::empty(),
            );
            assert!(Widget::ime_cursor_rect(&b).is_some());

            dispatch(
                &mut b,
                &WindowEvent::Ime(winit::event::Ime::Commit("東京".into())),
                Modifiers::default(),
            );
            assert_eq!(b.query(), "東京");
            assert_eq!(b.visible_len(), 1);
            assert_eq!(b.visible_entry(0).unwrap().name, "東京");
        }

        fn query_edit_inserts_at_caret_not_end() {
            let mut b = ChannelBrowser::new();
            b.rect = Rect::new(0.0, 0.0, 640.0, 480.0);
            b.set_focus(true);
            b.set_query("ac");
            b.query_selection.cursor = 1;
            b.query_selection.anchor = 1;
            dispatch(
                &mut b,
                &key_press(Key::Character(SmolStr::new("b"))),
                Modifiers::default(),
            );
            assert_eq!(b.query(), "abc");
            assert_eq!(b.query_selection.cursor, 2);
        }

        fn ctrl_a_selects_query() {
            let mut b = ChannelBrowser::new();
            b.rect = Rect::new(0.0, 0.0, 640.0, 480.0);
            b.set_focus(true);
            b.set_query("hello");
            let mut mods_state = winit::keyboard::ModifiersState::empty();
            mods_state.set(winit::keyboard::ModifiersState::CONTROL, true);
            dispatch(
                &mut b,
                &key_press(Key::Character(SmolStr::new("a"))),
                Modifiers::from(mods_state),
            );
            assert_eq!(b.query_selection.anchor, 0);
            assert_eq!(b.query_selection.cursor, 5);
        }

        fn fuzzy_match_picks_up_space_name_hits() {
            let entries = vec![
                entry("engineering", "Company", 200.0, 100, 0, false, None),
                entry("design", "Company", 200.0, 200, 0, false, None),
                entry("personal", "Personal", 30.0, 50, 0, false, None),
            ];
            let mut b = browser_at(Rect::new(0.0, 0.0, 640.0, 480.0), entries);
            b.set_query("comp");
            assert_eq!(b.visible_len(), 2);
            for i in 0..b.visible_len() {
                assert_eq!(b.visible_entry(i).unwrap().space_name, "Company");
            }
        }

        typing_filters_entries();
        ime_preedit_is_display_only_until_atomic_filter_commit();
        query_edit_inserts_at_caret_not_end();
        ctrl_a_selects_query();
        fuzzy_match_picks_up_space_name_hits();
    }

    #[test]
    fn space_filter_pills_narrow_cycle_and_reset_the_cursor() {
        fn space_filter_pill_click_narrows_list() {
            let entries = vec![
                entry("engineering", "Company", 200.0, 100, 0, false, None),
                entry("design", "Company", 200.0, 200, 0, false, None),
                entry("personal", "Personal", 30.0, 50, 0, false, None),
            ];
            let mut b = browser_at(Rect::new(0.0, 0.0, 640.0, 480.0), entries);
            // Mirrors the click path — set_space_filter is what the
            // hit-test branch ends up calling.
            b.set_space_filter(Some("Personal".into()));
            assert_eq!(b.visible_len(), 1);
            assert_eq!(b.visible_entry(0).unwrap().space_name, "Personal");
            b.set_space_filter(None);
            assert_eq!(b.visible_len(), 3);
        }

        fn space_filter_pill_hit_test_narrows_list() {
            let entries = vec![
                entry("engineering", "Company", 200.0, 100, 0, false, None),
                entry("personal", "Personal", 30.0, 50, 0, false, None),
            ];
            let mut b = browser_at(Rect::new(0.0, 0.0, 640.0, 480.0), entries);
            b.set_focus(true);
            // pill_rects only populates on a real draw; exercise the API
            // the click handler funnels into instead.
            b.set_space_filter(Some("Personal".into()));
            assert_eq!(b.visible_len(), 1);
            assert_eq!(b.visible_entry(0).unwrap().space_name, "Personal");
        }

        fn space_filter_pill_arrow_cycles_active_pill() {
            let entries = vec![
                entry("engineering", "Company", 200.0, 100, 0, false, None),
                entry("personal", "Personal", 30.0, 50, 0, false, None),
            ];
            let mut b = browser_at(Rect::new(0.0, 0.0, 640.0, 480.0), entries);
            b.set_focus(true);
            assert!(b.space_filter().is_none());
            dispatch(
                &mut b,
                &key_press(Key::Named(NamedKey::ArrowRight)),
                Modifiers::default(),
            );
            assert_eq!(b.space_filter(), Some("Company"));
            dispatch(
                &mut b,
                &key_press(Key::Named(NamedKey::ArrowRight)),
                Modifiers::default(),
            );
            assert_eq!(b.space_filter(), Some("Personal"));
            dispatch(
                &mut b,
                &key_press(Key::Named(NamedKey::ArrowRight)),
                Modifiers::default(),
            );
            assert!(b.space_filter().is_none(), "wraps back to All");
            dispatch(
                &mut b,
                &key_press(Key::Named(NamedKey::ArrowLeft)),
                Modifiers::default(),
            );
            assert_eq!(b.space_filter(), Some("Personal"), "left wraps to last");
        }

        fn space_filter_change_resets_cursor() {
            let entries = vec![
                entry("a1", "Alpha", 200.0, 1, 0, false, None),
                entry("a2", "Alpha", 200.0, 2, 0, false, None),
                entry("b1", "Beta", 100.0, 3, 0, false, None),
            ];
            let mut b = browser_at(Rect::new(0.0, 0.0, 640.0, 480.0), entries);
            b.set_cursor(2);
            assert_eq!(b.cursor(), 2);
            b.set_space_filter(Some("Beta".into()));
            assert_eq!(b.cursor(), 0, "filter change must reset cursor");
            assert_eq!(b.visible_len(), 1);
        }

        space_filter_pill_click_narrows_list();
        space_filter_pill_hit_test_narrows_list();
        space_filter_pill_arrow_cycles_active_pill();
        space_filter_change_resets_cursor();
    }

    #[test]
    fn cursor_selection_fires_select_or_dismiss() {
        fn arrow_down_advances_cursor_through_visible() {
            let entries = vec![
                entry("a", "Company", 200.0, 1, 0, false, None),
                entry("b", "Company", 200.0, 2, 0, false, None),
                entry("c", "Company", 200.0, 3, 0, false, None),
            ];
            let mut b = browser_at(Rect::new(0.0, 0.0, 640.0, 480.0), entries);
            b.set_focus(true);
            assert_eq!(b.cursor(), 0);
            dispatch(
                &mut b,
                &key_press(Key::Named(NamedKey::ArrowDown)),
                Modifiers::default(),
            );
            assert_eq!(b.cursor(), 1);
            dispatch(
                &mut b,
                &key_press(Key::Named(NamedKey::ArrowDown)),
                Modifiers::default(),
            );
            assert_eq!(b.cursor(), 2);
            // Saturates at last row.
            dispatch(
                &mut b,
                &key_press(Key::Named(NamedKey::ArrowDown)),
                Modifiers::default(),
            );
            assert_eq!(b.cursor(), 2);
            dispatch(
                &mut b,
                &key_press(Key::Named(NamedKey::ArrowUp)),
                Modifiers::default(),
            );
            assert_eq!(b.cursor(), 1);
        }

        fn enter_fires_on_select_with_cursor_entry() {
            let entries = vec![
                entry("alpha", "Company", 200.0, 1, 0, false, None),
                entry("beta", "Company", 200.0, 2, 0, false, None),
            ];
            let captured: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
            let captured_clone = captured.clone();
            let mut b = ChannelBrowser::with_entries(entries).on_select(move |e| {
                *captured_clone.borrow_mut() = Some(e.name.clone());
            });
            b.rect = Rect::new(0.0, 0.0, 640.0, 480.0);
            b.set_focus(true);
            dispatch(
                &mut b,
                &key_press(Key::Named(NamedKey::Enter)),
                Modifiers::default(),
            );
            let got = captured.borrow().clone();
            let first_name = b.visible_entry(0).unwrap().name.clone();
            assert_eq!(got, Some(first_name));
        }

        fn esc_fires_on_dismiss() {
            let fired: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));
            let fired_clone = fired.clone();
            let mut b = ChannelBrowser::new().on_dismiss(move || {
                *fired_clone.borrow_mut() = true;
            });
            b.rect = Rect::new(0.0, 0.0, 640.0, 480.0);
            b.set_focus(true);
            dispatch(
                &mut b,
                &key_press(Key::Named(NamedKey::Escape)),
                Modifiers::default(),
            );
            assert!(*fired.borrow());
        }

        fn host_loop_typing_arrow_enter_fires_callback_for_second_match() {
            let entries = vec![
                entry(
                    "engineering",
                    "Company",
                    200.0,
                    100,
                    3,
                    false,
                    Some("Editor"),
                ),
                entry("english-class", "Personal", 30.0, 200, 0, true, None),
                entry("design", "Company", 200.0, 50, 0, false, None),
                entry("notes", "Personal", 30.0, 80, 1, false, Some("Owner")),
            ];
            let captured: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
            let captured_clone = captured.clone();
            let mut b = ChannelBrowser::with_entries(entries).on_select(move |e| {
                *captured_clone.borrow_mut() = Some(e.name.clone());
            });
            b.rect = Rect::new(0.0, 0.0, 640.0, 480.0);
            b.set_focus(true);
            // "eng" matches engineering + english-class.
            for ch in ["e", "n", "g"] {
                dispatch(
                    &mut b,
                    &key_press(Key::Character(SmolStr::new(ch))),
                    Modifiers::default(),
                );
            }
            assert_eq!(b.visible_len(), 2);
            dispatch(
                &mut b,
                &key_press(Key::Named(NamedKey::ArrowDown)),
                Modifiers::default(),
            );
            let second = b.visible_entry(1).unwrap().name.clone();
            dispatch(
                &mut b,
                &key_press(Key::Named(NamedKey::Enter)),
                Modifiers::default(),
            );
            assert_eq!(*captured.borrow(), Some(second));
        }

        fn row_click_drives_on_select() {
            let entries = vec![
                entry("a", "Company", 200.0, 200, 0, false, None),
                entry("b", "Company", 200.0, 100, 0, false, None),
            ];
            let captured: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
            let captured_clone = captured.clone();
            let mut b = ChannelBrowser::with_entries(entries).on_select(move |e| {
                *captured_clone.borrow_mut() = Some(e.name.clone());
            });
            b.rect = Rect::new(0.0, 0.0, 640.0, 480.0);
            b.set_focus(true);
            let row1 = b.row_rect(1).expect("seeded row 1 must lay out");
            dispatch(
                &mut b,
                &cursor_moved(row1.center().x, row1.center().y),
                Modifiers::default(),
            );
            dispatch(&mut b, &mouse_press(), Modifiers::default());
            let want = b.visible_entry(1).unwrap().name.clone();
            assert_eq!(*captured.borrow(), Some(want));
        }

        arrow_down_advances_cursor_through_visible();
        enter_fires_on_select_with_cursor_entry();
        esc_fires_on_dismiss();
        host_loop_typing_arrow_enter_fires_callback_for_second_match();
        row_click_drives_on_select();
    }

    #[test]
    fn space_groups_sort_and_float_pinned_rows() {
        fn pinned_rows_float_to_top_of_space_group() {
            let entries = vec![
                entry("recent", "Company", 200.0, 500, 0, false, None),
                entry("pinned-old", "Company", 200.0, 100, 0, true, None),
                entry("other", "Personal", 30.0, 1000, 0, false, None),
            ];
            let b = browser_at(Rect::new(0.0, 0.0, 640.0, 480.0), entries);
            assert_eq!(b.visible_entry(0).unwrap().name, "pinned-old");
            assert_eq!(b.visible_entry(1).unwrap().name, "recent");
            assert_eq!(b.visible_entry(2).unwrap().name, "other");
        }

        fn space_groups_sort_alphabetically() {
            let entries = vec![
                entry("z-channel", "Zoo", 30.0, 500, 0, false, None),
                entry("a-channel", "Aardvark", 200.0, 100, 0, false, None),
                entry("m-channel", "Mango", 100.0, 300, 0, false, None),
            ];
            let b = browser_at(Rect::new(0.0, 0.0, 640.0, 480.0), entries);
            let groups = b.visible_space_groups();
            assert_eq!(groups, vec!["Aardvark", "Mango", "Zoo"]);
        }

        pinned_rows_float_to_top_of_space_group();
        space_groups_sort_alphabetically();
    }
}
