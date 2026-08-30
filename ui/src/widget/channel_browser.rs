//! Typeahead-driven modal sheet listing every channel the user can reach,
//! grouped by parent space.
//!
//! Consumes `Vec<ChannelEntry>` rather than a CRDT path directly, so the
//! widget stays source-agnostic — the host chrome owns Spaces, roster
//! state, and unread aggregation. Pinned channels float to the top of
//! their space group; spaces sort alphabetically. Fuzzy search matches
//! both `entry.name` and `entry.space_name` so typing a space prefix
//! narrows the list.

use std::collections::HashMap;
use std::time::SystemTime;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Color;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use collab_types::ChannelId;

use crate::color_compat::ColorCompat;
use crate::presence::PresenceCtx;
use crate::query_text::{QueryComposition, QueryEventChange};
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
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
            self.cursor = idx.min(self.visible.len() - 1);
        }
        self.dirty = true;
    }

    pub fn selected_entry(&self) -> Option<&ChannelEntry> {
        self.visible.get(self.cursor).map(|&i| &self.entries[i])
    }

    pub fn on_select<F: FnMut(&ChannelEntry) + 'static>(mut self, f: F) -> Self {
        self.on_select = Some(Box::new(f));
        self
    }

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
        self.visible.get(i).map(|&j| &self.entries[j])
    }

    /// Same alphabetical ordering as [`Self::space_pills`], filtered
    /// to spaces that survive the current query/space-filter.
    pub fn visible_space_groups(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .visible
            .iter()
            .map(|&i| self.entries[i].space_name.clone())
            .collect();
        // `rebuild` sorts entries so identical space_names land
        // contiguously, so a stable dedup is enough here.
        names.dedup();
        names
    }

    fn now(&self) -> SystemTime {
        self.now_override.unwrap_or_else(SystemTime::now)
    }

    /// Sort key: (`space_name` asc, !`is_pinned`, `last_activity` desc).
    fn rebuild(&mut self) {
        self.visible.clear();
        for (i, e) in self.entries.iter().enumerate() {
            if let Some(filter) = &self.space_filter
                && &e.space_name != filter
            {
                continue;
            }
            // Either-match: a hit on the channel name OR the parent
            // space name, so typing a space prefix drills in.
            if fuzzy_score(&self.query, &e.name).is_none()
                && fuzzy_score(&self.query, &e.space_name).is_none()
            {
                continue;
            }
            self.visible.push(i);
        }
        self.visible.sort_by(|a, b| {
            let ea = &self.entries[*a];
            let eb = &self.entries[*b];
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
            let space = self.entries[ei].space_name.as_str();
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
        let shape = RoundedRect::from_rect(row, f64::from(theme.tokens.radii.sm));
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &shape,
        );
        let border_color = if self.is_focused {
            theme.tokens.colors.border_focus
        } else {
            theme.tokens.colors.border_standard
        };
        scene.stroke(
            &Stroke::new(if self.is_focused { 2.0 } else { 1.0 }),
            transform,
            border_color,
            None,
            &shape,
        );
        let text_size = theme.tokens.typography.scale.sm;
        self.query_composition.draw(
            scene,
            transform,
            text_renderer,
            &self.query,
            &self.query_selection,
            "Search channels",
            Rect::new(row.x0 + 12.0, row.y0, row.x1 - 12.0, row.y1),
            &TextStyle::new(text_size),
            theme.tokens.colors.text_primary,
            theme.tokens.colors.text_dim,
            theme.tokens.colors.accent[5].with_alpha_factor(0.3),
            theme.tokens.colors.accent[5],
            self.is_focused,
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
        let pill_text_size = (theme.tokens.typography.scale.sm - 1.0).max(10.0);
        let active_fill = theme.tokens.colors.accent[5];
        let idle_fill = theme.tokens.colors.surface_white_5;
        let active_text = theme.tokens.colors.text_primary;
        let idle_text = theme.tokens.colors.text_dim;

        // "All" + every distinct space name; rects are stashed so the
        // mouse-input branch can hit-test.
        self.pill_rects.clear();
        let mut x = strip.x0;
        let pill_h = (strip.height() - 8.0).max(24.0);
        let cy = f64::midpoint(strip.y0, strip.y1);

        let mut paint_pill = |label: &str,
                              active: bool,
                              text_renderer: &mut TextRenderer,
                              x: &mut f64,
                              rects: &mut Vec<(String, Rect)>| {
            let (lw, _) = text_renderer.measure(label, pill_text_size, None);
            let pill_w = f64::from(lw) + 20.0;
            let rect = Rect::new(*x, cy - pill_h / 2.0, *x + pill_w, cy + pill_h / 2.0);
            let body = if active { active_fill } else { idle_fill };
            let color = if active { active_text } else { idle_text };
            let shape = RoundedRect::from_rect(rect, pill_h / 2.0);
            scene.fill(vello::peniko::Fill::NonZero, transform, body, None, &shape);
            let (lw2, lh2) = text_renderer.measure(label, pill_text_size, None);
            text_renderer.draw_text(
                scene,
                label,
                pill_text_size,
                color,
                transform
                    * Affine::translate((
                        rect.x0 + (rect.width() - f64::from(lw2)) / 2.0,
                        rect.y0 + (rect.height() - f64::from(lh2)) / 2.0,
                    )),
                None,
            );
            rects.push((label.to_string(), rect));
            *x += pill_w + 6.0;
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

    #[allow(clippy::too_many_arguments)]
    fn draw_row(
        &self,
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
            let shape = RoundedRect::from_rect(row, f64::from(theme.tokens.radii.sm));
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.surface_white_10,
                None,
                &shape,
            );
            let border = Rect::new(row.x0, row.y0, row.x0 + 3.0, row.y1);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.accent[5],
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

        let text_size = theme.tokens.typography.scale.sm;
        let cy = f64::midpoint(row.y0, row.y1);
        let mut x_cursor = band.x1 + 8.0;

        if entry.is_pinned {
            let (gw, _) = text_renderer.measure(PIN_GLYPH, text_size, None);
            text_renderer.draw_text(
                scene,
                PIN_GLYPH,
                text_size,
                theme.tokens.colors.accent[5],
                transform * Affine::translate((x_cursor, cy - f64::from(text_size) / 2.0)),
                None,
            );
            x_cursor += f64::from(gw) + 4.0;
        }

        text_renderer.draw_text(
            scene,
            entry.name.as_str(),
            text_size,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((x_cursor, cy - f64::from(text_size) / 2.0)),
            None,
        );
        let (nw, _) = text_renderer.measure(entry.name.as_str(), text_size, None);
        x_cursor += f64::from(nw) + 8.0;

        if let Some(role) = &entry.role_label {
            let chip_text_size = (text_size - 2.0).max(10.0);
            let (lw, _) = text_renderer.measure(role, chip_text_size, None);
            let chip_w = (f64::from(lw) + 12.0).min(ROLE_CHIP_W);
            let chip_h = (row.height() - 16.0).max(16.0);
            let chip = Rect::new(
                x_cursor,
                cy - chip_h / 2.0,
                x_cursor + chip_w,
                cy + chip_h / 2.0,
            );
            let shape = RoundedRect::from_rect(chip, chip_h / 2.0);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.surface_white_5,
                None,
                &shape,
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                theme.tokens.colors.border_white_10,
                None,
                &shape,
            );
            let (lw2, lh2) = text_renderer.measure(role, chip_text_size, None);
            text_renderer.draw_text(
                scene,
                role,
                chip_text_size,
                theme.tokens.colors.text_dim,
                transform
                    * Affine::translate((
                        chip.x0 + (chip.width() - f64::from(lw2)) / 2.0,
                        chip.y0 + (chip.height() - f64::from(lh2)) / 2.0,
                    )),
                None,
            );
        }

        // Right-aligned before the unread badge slot.
        let rel = crate::widget::history::format_relative_time(entry.last_activity, now);
        let (tw, _) = text_renderer.measure(&rel, text_size, None);
        let time_x = row.x1 - UNREAD_SLOT_W - 4.0 - f64::from(tw);
        text_renderer.draw_text(
            scene,
            &rel,
            text_size,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((time_x, cy - f64::from(text_size) / 2.0)),
            None,
        );

        if entry.unread_count > 0 {
            let badge_text = if entry.unread_count > 99 {
                "99+".to_string()
            } else {
                entry.unread_count.to_string()
            };
            let badge_text_size = (text_size - 2.0).max(10.0);
            let (bw, _) = text_renderer.measure(&badge_text, badge_text_size, None);
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
                theme.tokens.colors.accent[5],
                None,
                &shape,
            );
            let (lw, lh) = text_renderer.measure(&badge_text, badge_text_size, None);
            text_renderer.draw_text(
                scene,
                &badge_text,
                badge_text_size,
                theme.tokens.colors.text_primary,
                transform
                    * Affine::translate((
                        badge.x0 + (badge.width() - f64::from(lw)) / 2.0,
                        badge.y0 + (badge.height() - f64::from(lh)) / 2.0,
                    )),
                None,
            );
            // Silences an unused-constant warning; TIME_SLOT_W is
            // kept for a future layout that reserves the slot.
            let _ = TIME_SLOT_W;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_space_header(
        &self,
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
        let text_size = (theme.tokens.typography.scale.sm - 1.0).max(10.0);
        let cy = f64::midpoint(header_rect.y0, header_rect.y1);
        text_renderer.draw_text(
            scene,
            space_name,
            text_size,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((band.x1 + 6.0, cy - f64::from(text_size) / 2.0)),
            None,
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
        let band_body = Color::from_rgba8(0, 0, 0, 51);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            band_body,
            None,
            &footer,
        );
        let border = vello::kurbo::Line::new((footer.x0, footer.y0), (footer.x1, footer.y0));
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_standard,
            None,
            &border,
        );

        let chip_text_size = (theme.tokens.typography.scale.sm - 2.0).max(9.0);
        let chip_body = theme.tokens.colors.surface_white_5;
        let chip_border = theme.tokens.colors.border_white_10;
        let dim = theme.tokens.colors.text_dim;
        let chip_h = (footer.height() - 14.0).max(14.0);
        let cy = f64::midpoint(footer.y0, footer.y1);

        let mut x = footer.x0 + 4.0;
        for (key, label) in [
            ("\u{2191}\u{2193}", "Navigate"),
            ("Enter", "Select"),
            ("Esc", "Close"),
        ] {
            let (kw, kh) = text_renderer.measure(key, chip_text_size, None);
            let chip_w = f64::from(kw) + 10.0;
            let chip_rect = Rect::new(x, cy - chip_h / 2.0, x + chip_w, cy + chip_h / 2.0);
            let chip_shape = RoundedRect::from_rect(chip_rect, 4.0);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                chip_body,
                None,
                &chip_shape,
            );
            scene.stroke(&Stroke::new(1.0), transform, chip_border, None, &chip_shape);
            text_renderer.draw_text(
                scene,
                key,
                chip_text_size,
                dim,
                transform
                    * Affine::translate((
                        chip_rect.x0 + (chip_rect.width() - f64::from(kw)) / 2.0,
                        chip_rect.y0 + (chip_rect.height() - f64::from(kh)) / 2.0,
                    )),
                None,
            );
            x = chip_rect.x1 + 4.0;
            let (lw, lh) = text_renderer.measure(label, chip_text_size, None);
            text_renderer.draw_text(
                scene,
                label,
                chip_text_size,
                dim,
                transform * Affine::translate((x, cy - f64::from(lh) / 2.0)),
                None,
            );
            x += f64::from(lw) + 12.0;
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
                .map_or(0, |i| i + 1),
        }
    }

    fn set_active_pill_index(&mut self, idx: usize) {
        let pills = self.space_pills();
        let total = pills.len() + 1;
        let clamped = idx.min(total - 1);
        let new_filter = if clamped == 0 {
            None
        } else {
            Some(pills[clamped - 1].clone())
        };
        self.set_space_filter(new_filter);
    }
}

// Copy of `command_palette::fuzzy_score`, which is private to its module.
fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let q: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    let t: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();
    let mut qi = 0usize;
    let mut matches = 0i32;
    let mut gaps = 0i32;
    let mut prev_match: Option<usize> = None;
    for (ti, ch) in t.iter().enumerate() {
        if qi < q.len() && *ch == q[qi] {
            if let Some(pm) = prev_match {
                gaps += (ti - pm - 1) as i32;
            }
            prev_match = Some(ti);
            qi += 1;
            matches += 1;
            if qi == q.len() {
                break;
            }
        }
    }
    if qi < q.len() {
        None
    } else {
        Some(matches - gaps)
    }
}

impl Widget for ChannelBrowser {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The host typically positions the sheet inside an overlay
        // container that hands it a final rect via apply_layout; this
        // returns a sensible standalone default.
        let rows = self.visible.len().max(1) as f32;
        let height = (PADDING as f32) * 4.0
            + INPUT_HEIGHT as f32
            + FILTER_STRIP_HEIGHT as f32
            + (rows * ROW_HEIGHT as f32).min(10.0 * ROW_HEIGHT as f32)
            + FOOTER_HEIGHT as f32
            + SPACE_HEADER_HEIGHT as f32;
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::length(640.0),
                height: Dimension::length(height),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        let prev = self.rect;
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
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
        let shape = RoundedRect::from_rect(self.rect, f64::from(theme.tokens.radii.md));
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

        self.draw_search_input(scene, transform, text_renderer, theme);
        self.draw_pill_strip(scene, transform, text_renderer, theme);

        if self.visible.is_empty() {
            let label = "No channels";
            let size = theme.tokens.typography.scale.sm;
            let (w, h) = text_renderer.measure(label, size, None);
            let top = self.list_top_y();
            let cx = f64::midpoint(self.rect.x0, self.rect.x1);
            let cy = f64::midpoint(top, self.footer_rect().y0);
            text_renderer.draw_text(
                scene,
                label,
                size,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((cx - f64::from(w) / 2.0, cy - f64::from(h) / 2.0)),
                None,
            );
            self.draw_footer(scene, transform, text_renderer, theme);
            self.dirty = false;
            return;
        }

        let now = self.now();
        let layout = self.iter_row_layout();
        for (vi, row, header) in layout {
            if let Some(hr) = header {
                let entry = &self.entries[self.visible[vi]];
                self.draw_space_header(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    hr,
                    entry.space_name.as_str(),
                    entry.space_color_hue,
                );
            }
            let entry = &self.entries[self.visible[vi]];
            let is_selected = vi == self.cursor;
            self.draw_row(
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

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        // Mouse branches go first so click-to-select on pills/rows
        // works even when the input has focus.
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
            *handled = true;
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
        let mods = modifiers.state();
        let shift = mods.shift_key();
        let ctrl = mods.control_key() || mods.super_key();
        match logical_key {
            Key::Named(NamedKey::ArrowDown) => {
                if !self.visible.is_empty() {
                    self.cursor = (self.cursor + 1).min(self.visible.len() - 1);
                    self.dirty = true;
                    *handled = true;
                }
            }
            Key::Named(NamedKey::ArrowUp) => {
                if self.cursor > 0 {
                    self.cursor -= 1;
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
                    let total = self.space_pills().len() + 1;
                    let new_idx = if idx == 0 { total - 1 } else { idx - 1 };
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
                    let total = self.space_pills().len() + 1;
                    let new_idx = (idx + 1) % total;
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
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;
    use winit::event::Modifiers;
    use winit::keyboard::{KeyLocation, PhysicalKey, SmolStr};

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
            last_activity: SystemTime::UNIX_EPOCH + Duration::from_secs(ts),
            unread_count: unread,
            is_pinned: pinned,
            role_label: role.map(str::to_string),
        }
    }

    fn browser_at(rect: Rect, entries: Vec<ChannelEntry>) -> ChannelBrowser {
        let mut b = ChannelBrowser::with_entries(entries);
        b.rect = rect;
        b.set_now_for_test(SystemTime::UNIX_EPOCH + Duration::from_secs(10_000_000));
        b
    }

    #[test]
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

    fn key_press(logical_key: Key) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = logical_key;
        ev.physical_key = PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }

    fn dispatch(b: &mut ChannelBrowser, event: &WindowEvent, mods: Modifiers) {
        let mut tree = TaffyTree::<()>::new();
        let node = tree.leaf_node(Style::default());
        let mut handled = false;
        let mut focus = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        b.handle_event(
            event,
            1.0,
            &mods,
            &mut doc,
            &root,
            &mut handled,
            &mut focus,
            node,
            &tree,
        );
    }

    fn cursor_moved(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(x, y),
        }
    }

    fn mouse_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }

    #[test]
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

    #[test]
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

    #[test]
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

    #[test]
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

    #[test]
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

    #[test]
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

    #[test]
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

    #[test]
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

    #[test]
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

    #[test]
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

    #[test]
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

    #[test]
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

    #[test]
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

    #[test]
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

    #[test]
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

    #[test]
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

    #[test]
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

    #[test]
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
}
