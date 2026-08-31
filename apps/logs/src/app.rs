//! `LogsApp` — the root widget for the Logs viewer.
//!
//! A graphical systemd-journal viewer. It is a single self-rendering
//! [`Widget`] (the data is read-only and the visuals — severity dots,
//! coloured rows, a detail pane, a filter bar — are bespoke, exactly
//! the case the toolkit calls out for "draw into the Vello Scene
//! yourself"). All *view state* (selection, filters, search,
//! live-tail) lives in the Automerge document via [`LogsDoc`]; the
//! journal entries themselves are an external render cache.
//!
//! Layout (top → bottom):
//! ```text
//! ┌──────────────────────────────────────────────────────────────┐
//! │ Header:              [counts]  [Clear filters]  [● Live tail] │
//! ├──────────────────────────────────────────────────────────────┤
//! │ Filter:  [search] [unit ▾] [boot ▾]         [severity chips]  │
//! ├───────────────────────────────────┬──────────────────────────┤
//! │ Entry list (scrollable)           │ Detail pane              │
//! │  ● time  TAG  unit  message       │  big badge / unit        │
//! │  ● …                              │  message (wrapped)       │
//! │                                   │  field table             │
//! └───────────────────────────────────┴──────────────────────────┘
//! ```
//!
//! Per-row severity is colour-coded (and named in the level tag), so no
//! standing colour key is drawn — the "Clear filters" control that used
//! to share the old bottom legend now lives in the header, appearing
//! only while a filter is active.

#![allow(
    clippy::many_single_char_names,
    reason = "t (Affine), c (Colors), r/d (Rect), x/y/w/h (geometry) and their \
              close relatives are the standard vocabulary shared by every \
              draw helper in this file"
)]

use std::collections::HashMap;
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::ColorCompat;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{TextInput, Widget};

use crate::doc::LogsDoc;
use crate::model::{Filter, LogEntry, Priority};
use crate::responsive::{Pane, PushNav};
use crate::theme_ext::{ThemeColors, tint};

const PAD: f64 = 16.0;
const HEADER_H: f64 = 60.0;
const FILTER_H: f64 = 64.0;
const ROW_H: f64 = 30.0;
const LIST_HEADER_H: f64 = 26.0;
const CONTROL_H: f64 = 34.0;
const DROPDOWN_ITEM_H: f64 = 30.0;
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Which transient popup is open, if any.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Popup {
    Unit,
    Boot,
}

/// Computed screen rectangles for one frame. Built from `self.rect` plus
/// current state; consumed by both `draw` and `handle_event` so the two
/// can never disagree about where a control is.
struct Regions {
    header: Rect,
    filter: Rect,
    search: Rect,
    unit_trigger: Rect,
    boot_trigger: Rect,
    live_pill: Rect,
    clear_btn: Rect,
    chips: Vec<Rect>,
    list_header: Rect,
    list_body: Rect,
    detail: Rect,
    /// `true` when the window is narrow enough that the list and detail are
    /// single-pane push slabs instead of a side-by-side split.
    narrow: bool,
    /// Back affordance shown in the detail pane header on narrow windows
    /// (returns to the list). `Rect::ZERO` when wide or showing the list.
    back_btn: Rect,
    /// (rect, value) for the currently-open dropdown's rows. `value` is
    /// the canonical filter string ("" = the "All" row).
    dropdown: Vec<(Rect, String, String)>, // (rect, value, label)
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent, unrelated flag (live-tail state, \
              focus, repaint gate, live-binary vs. --shots mode); grouping \
              them into enums would not express any real invariant between \
              them and would touch every read/write site across this file"
)]
pub struct LogsApp {
    /// Full journal — external read-only data, not CRDT state.
    entries: Vec<LogEntry>,
    /// Distinct units present, in first-seen order (for the dropdown).
    units: Vec<String>,
    /// Distinct boots: (`boot_id`, human label).
    boots: Vec<(String, String)>,

    // ---- per-frame caches, refreshed from the doc on sync ----
    filter: Filter,
    /// Indices into `entries` that pass `filter`, in display order.
    filtered: Vec<usize>,
    /// Stored selection (absolute index into `entries`, or -1 = auto).
    sel_raw: i64,
    live_tail: bool,

    // ---- ephemeral UI state ----
    rect: Rect,
    node: Option<NodeId>,
    scroll: f32,
    cursor: Point,
    hover_row: Option<usize>,
    popup: Option<Popup>,
    focused: bool,
    /// Shared CRDT-bound field for the collaborative search query. The filter
    /// bar keeps its app-specific layout; text editing is toolkit-owned.
    search_input: TextInput,
    dirty: bool,

    // ---- responsiveness (master-detail push on narrow windows) ----
    /// Drives the single-pane list↔detail push slide below the width
    /// breakpoint. Inert on wide windows, where both panes are docked.
    push: PushNav,

    // ---- live-tail plumbing (never runs under --shots) ----
    allow_live: bool,
    last_poll: Instant,
}

impl LogsApp {
    /// `allow_live` is true only for the live binary; `--shots` passes
    /// `false` so we never shell out to `journalctl` during a headless
    /// render.
    pub fn new(entries: Vec<LogEntry>, allow_live: bool) -> Self {
        let mut app = Self {
            entries,
            units: Vec::new(),
            boots: Vec::new(),
            filter: Filter::default(),
            filtered: Vec::new(),
            sel_raw: -1,
            live_tail: false,
            rect: Rect::ZERO,
            node: None,
            scroll: 0.0,
            cursor: Point::ZERO,
            hover_row: None,
            popup: None,
            focused: true,
            search_input: TextInput::new(Vec::new())
                .with_id("logs.search")
                .with_on_change_path(crate::doc::K_SEARCH)
                .with_placeholder("Search messages, units…")
                .with_plain_chrome(),
            dirty: true,
            push: PushNav::default(),
            allow_live,
            last_poll: Instant::now(),
        };
        app.rebuild_facets();
        app
    }

    /// Deterministically snap the narrow single-pane push flow to the
    /// detail pane (with its Back affordance) for `--shots`, skipping the
    /// eased slide so a headless frame captures a settled pane. A no-op
    /// effect on wide windows, where both panes are docked anyway.
    pub fn shots_show_detail(&mut self) {
        self.push.snap(Pane::Detail);
        self.dirty = true;
    }

    /// Recompute the distinct-unit and distinct-boot lists from entries.
    fn rebuild_facets(&mut self) {
        let mut units: Vec<String> = Vec::new();
        let mut boots: Vec<(String, String)> = Vec::new();
        for e in &self.entries {
            if !units.contains(&e.unit) {
                units.push(e.unit.clone());
            }
            if !boots.iter().any(|(b, _)| b == &e.boot_id) {
                boots.push((e.boot_id.clone(), String::new()));
            }
        }
        units.sort();
        // Label boots newest-first as "current", "previous", … with a
        // short id suffix so the dropdown is legible.
        boots.sort_by_key(|(b, _)| {
            self.entries
                .iter()
                .filter(|e| &e.boot_id == b)
                .map(|e| e.realtime_us)
                .max()
                .unwrap_or(0)
        });
        boots.reverse();
        let n = boots.len();
        for (i, (id, label)) in boots.iter_mut().enumerate() {
            let short = id.chars().take(8).collect::<String>();
            *label = match i {
                0 if n > 1 => format!("Current boot · {short}"),
                0 => format!("Boot · {short}"),
                1 => format!("Previous boot · {short}"),
                _ => format!("Boot −{i} · {short}"),
            };
        }
        self.units = units;
        self.boots = boots;
    }

    fn theme_colors(theme: &Theme) -> ThemeColors {
        ThemeColors::from_theme(theme)
    }

    /// Recompute `filtered` from the current `filter`.
    fn recompute_filtered(&mut self) {
        self.filtered = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| self.filter.passes(e))
            .map(|(i, _)| i)
            .collect();
    }

    /// Resolve the effective selection to an absolute entry index that is
    /// actually in the filtered set. Honors an explicit pick; otherwise
    /// auto-selects the first error-or-worse (falling back to the first
    /// visible entry).
    fn effective_selected(&self) -> Option<usize> {
        if self.sel_raw >= 0 {
            let s = self.sel_raw as usize;
            if self.filtered.contains(&s) {
                return Some(s);
            }
            // Explicitly selected but filtered out → nothing selected.
            if self.sel_raw >= 0 && (s) < self.entries.len() {
                return None;
            }
        }
        // Auto.
        self.filtered
            .iter()
            .copied()
            .find(|&i| {
                self.entries
                    .get(i)
                    .is_some_and(|e| e.priority.level() <= Priority::Err.level())
            })
            .or_else(|| self.filtered.first().copied())
    }

    /// Position of an absolute entry index within the filtered list.
    fn filtered_pos(&self, abs: usize) -> Option<usize> {
        self.filtered.iter().position(|&i| i == abs)
    }

    fn list_viewport_h(&self) -> f64 {
        self.regions().list_body.height()
    }

    fn content_h(&self) -> f64 {
        self.filtered.len() as f64 * ROW_H
    }

    fn clamp_scroll(&mut self) {
        let max = (self.content_h() - self.list_viewport_h()).max(0.0) as f32;
        self.scroll = self.scroll.clamp(0.0, max);
    }

    /// Scroll so the filtered position `pos` is fully visible.
    fn scroll_into_view(&mut self, pos: usize) {
        let body_h = self.list_viewport_h();
        let top = pos as f64 * ROW_H;
        let bottom = top + ROW_H;
        if (top as f32) < self.scroll {
            self.scroll = top as f32;
        } else if bottom as f32 > self.scroll + body_h as f32 {
            self.scroll = (bottom - body_h) as f32;
        }
        self.clamp_scroll();
    }

    /// Move the selection by `delta` filtered positions and persist it.
    fn move_selection(&mut self, delta: isize, doc: &mut AutoCommit, root: &ObjId) {
        if self.filtered.is_empty() {
            return;
        }
        let cur_pos = self
            .effective_selected()
            .and_then(|abs| self.filtered_pos(abs))
            .unwrap_or(0) as isize;
        let new_pos = (cur_pos + delta).clamp(0, self.filtered.len() as isize - 1) as usize;
        let abs = self.filtered.get(new_pos).copied().unwrap_or(0);
        self.sel_raw = abs as i64;
        LogsDoc::set_selected(doc, root, abs as i64);
        self.scroll_into_view(new_pos);
        self.dirty = true;
    }

    fn select_abs(&mut self, abs: usize, doc: &mut AutoCommit, root: &ObjId) {
        self.sel_raw = abs as i64;
        LogsDoc::set_selected(doc, root, abs as i64);
        if let Some(pos) = self.filtered_pos(abs) {
            self.scroll_into_view(pos);
        }
        self.dirty = true;
    }

    // ---------------------------------------------------------------
    // Layout
    // ---------------------------------------------------------------

    fn regions(&self) -> Regions {
        let r = self.rect;
        let (x0, y0, w, h) = (r.x0, r.y0, r.width(), r.height());

        let header = Rect::new(x0, y0, x0 + w, y0 + HEADER_H);
        let filter = Rect::new(x0, header.y1, x0 + w, header.y1 + FILTER_H);
        let content_top = filter.y1;
        // The list/detail content spans all the way to the window bottom —
        // there is no standing legend strip to reserve room for.
        let content_bottom = y0 + h;

        let (search, unit_trigger, boot_trigger, chips) = Self::filter_bar_regions(x0, w, filter);
        let (live_pill, clear_btn) = self.header_controls(x0, w, header);

        // Content area between the filter bar and the window bottom.
        let cx0 = x0 + PAD;
        let cy0 = content_top + PAD;
        let cx1 = x0 + w - PAD;
        let cy1 = content_bottom - PAD;
        let gap = 14.0;

        let (list_header, list_body, detail, back_btn, narrow) =
            self.content_split(w, cx0, cy0, cx1, cy1, gap);
        let dropdown = self.dropdown_regions(unit_trigger, boot_trigger);

        Regions {
            header,
            filter,
            search,
            unit_trigger,
            boot_trigger,
            live_pill,
            clear_btn,
            chips,
            list_header,
            list_body,
            detail,
            narrow,
            back_btn,
            dropdown,
        }
    }

    /// `regions()`'s filter-bar controls: search box, unit/boot dropdown
    /// triggers, and the right-aligned severity chips.
    fn filter_bar_regions(x0: f64, w: f64, filter: Rect) -> (Rect, Rect, Rect, Vec<Rect>) {
        let fy = filter.y0 + (FILTER_H - CONTROL_H) / 2.0;
        let mut fx = x0 + PAD;
        let search = Rect::new(fx, fy, fx + 320.0, fy + CONTROL_H);
        fx = search.x1 + 10.0;
        let unit_trigger = Rect::new(fx, fy, fx + 188.0, fy + CONTROL_H);
        fx = unit_trigger.x1 + 10.0;
        let boot_trigger = Rect::new(fx, fy, fx + 196.0, fy + CONTROL_H);

        // Severity chips, right-aligned in the filter bar.
        let chip_w = 46.0;
        let chip_gap = 4.0;
        let chips_total = chip_w * 8.0 + chip_gap * 7.0;
        let chips_x0 = (x0 + w - PAD - chips_total).max(boot_trigger.x1 + 16.0);
        let chips: Vec<Rect> = (0..8)
            .map(|i| {
                let cx = chips_x0 + f64::from(i) * (chip_w + chip_gap);
                Rect::new(cx, fy, cx + chip_w, fy + CONTROL_H)
            })
            .collect();
        (search, unit_trigger, boot_trigger, chips)
    }

    /// `regions()`'s header: live pill + clear filters, right-aligned. The
    /// live pill is the rightmost control; the "Clear filters" button sits
    /// just left of it and is only laid out (non-zero) while a filter is
    /// active.
    fn header_controls(&self, x0: f64, w: f64, header: Rect) -> (Rect, Rect) {
        let hy = header.y0 + (HEADER_H - 30.0) / 2.0;
        let live_pill = Rect::new(x0 + w - PAD - 124.0, hy, x0 + w - PAD, hy + 30.0);
        let clear_btn = if self.filter.is_clear() {
            Rect::ZERO
        } else {
            let cy = header.y0 + (HEADER_H - 26.0) / 2.0;
            let x1 = live_pill.x0 - 12.0;
            Rect::new(x1 - 104.0, cy, x1, cy + 26.0)
        };
        (live_pill, clear_btn)
    }

    /// `regions()`'s responsive split: side-by-side when wide, single-pane
    /// push when narrow. The choice is driven purely from the allocated
    /// width. Returns `(list_header, list_body, detail, back_btn, narrow)`.
    #[allow(
        clippy::too_many_arguments,
        reason = "one regions() section threading the shared content-area \
                  geometry (width + the four content corners + pane gap) \
                  through the list/detail split"
    )]
    fn content_split(
        &self,
        w: f64,
        cx0: f64,
        cy0: f64,
        cx1: f64,
        cy1: f64,
        gap: f64,
    ) -> (Rect, Rect, Rect, Rect, bool) {
        let narrow = PushNav::is_narrow(w);
        let (list_header, list_body, detail, back_btn) = if narrow {
            // Single-pane push: the list and detail each fill the whole
            // content width and slide horizontally as one swaps for the
            // other. `slide` (0 = list, 1 = detail) offsets both slabs.
            let pane_w = cx1 - cx0;
            let slide = self.push.slide();
            let list_x = cx0 - pane_w * slide; // slides off to the left
            let detail_x = cx0 + pane_w * (1.0 - slide); // slides in from the right
            let lh = Rect::new(list_x, cy0, list_x + pane_w, cy0 + LIST_HEADER_H);
            let lb = Rect::new(list_x, lh.y1, list_x + pane_w, cy1);
            // Reserve a Back affordance strip at the top of the detail pane.
            let detail_rect = Rect::new(detail_x, cy0, detail_x + pane_w, cy1);
            let bb = Rect::new(
                detail_rect.x0 + 12.0,
                detail_rect.y0 + 10.0,
                detail_rect.x0 + 92.0,
                detail_rect.y0 + 10.0 + 26.0,
            );
            (lh, lb, detail_rect, bb)
        } else {
            // Wide: the classic list (left) + detail (right) split.
            let list_w = ((cx1 - cx0) - gap) * 0.58;
            let lh = Rect::new(cx0, cy0, cx0 + list_w, cy0 + LIST_HEADER_H);
            let lb = Rect::new(cx0, lh.y1, cx0 + list_w, cy1);
            let detail_rect = Rect::new(cx0 + list_w + gap, cy0, cx1, cy1);
            (lh, lb, detail_rect, Rect::ZERO)
        };
        (list_header, list_body, detail, back_btn, narrow)
    }

    /// `regions()`'s open dropdown rows (empty when no popup is open).
    fn dropdown_regions(
        &self,
        unit_trigger: Rect,
        boot_trigger: Rect,
    ) -> Vec<(Rect, String, String)> {
        let mut dropdown = Vec::new();
        if let Some(p) = self.popup {
            let (trigger, mut items) = match p {
                Popup::Unit => {
                    let mut v = vec![(String::new(), "All units".to_string())];
                    for u in &self.units {
                        v.push((u.clone(), u.clone()));
                    }
                    (unit_trigger, v)
                }
                Popup::Boot => {
                    let mut v = vec![(String::new(), "All boots".to_string())];
                    for (id, label) in &self.boots {
                        v.push((id.clone(), label.clone()));
                    }
                    (boot_trigger, v)
                }
            };
            // Cap the dropdown height; scroll isn't needed for our facet
            // counts but clamp defensively.
            items.truncate(14);
            let dw = (trigger.width()).max(180.0);
            for (i, (val, label)) in items.into_iter().enumerate() {
                let iy = trigger.y1 + 4.0 + i as f64 * DROPDOWN_ITEM_H;
                let irect = Rect::new(trigger.x0, iy, trigger.x0 + dw, iy + DROPDOWN_ITEM_H);
                dropdown.push((irect, val, label));
            }
        }
        dropdown
    }
}

// ===================================================================
// Drawing helpers (free functions so borrows stay simple)
// ===================================================================

fn fill_rect(scene: &mut Scene, t: Affine, r: Rect, c: Color) {
    scene.fill(Fill::NonZero, t, c, None, &r);
}
fn fill_round(scene: &mut Scene, t: Affine, r: Rect, radius: impl Into<f64>, c: Color) {
    scene.fill(
        Fill::NonZero,
        t,
        c,
        None,
        &RoundedRect::from_rect(r, radius.into()),
    );
}
fn stroke_round(scene: &mut Scene, t: Affine, r: Rect, radius: impl Into<f64>, c: Color, w: f64) {
    scene.stroke(
        &Stroke::new(w),
        t,
        c,
        None,
        &RoundedRect::from_rect(r, radius.into()),
    );
}
fn fill_circle(scene: &mut Scene, t: Affine, center: Point, radius: f64, c: Color) {
    scene.fill(Fill::NonZero, t, c, None, &Circle::new(center, radius));
}

/// Baseline-free top-left text at `(x,y)`, no wrapping.
#[allow(clippy::too_many_arguments)]
fn text_at(
    tr: &mut TextRenderer,
    scene: &mut Scene,
    transform: Affine,
    text: &str,
    size: f32,
    color: Color,
    x: f64,
    y: f64,
    weight: TextWeight,
    mono: bool,
) {
    tr.draw_text_styled(
        scene,
        text,
        size,
        color,
        transform * Affine::translate((x, y)),
        None,
        weight,
        false,
        mono,
    );
}

/// Vertically-centred Y for a single line of `size` inside `rect`.
fn vcenter(rect: Rect, size: f32) -> f64 {
    rect.y0 + (rect.height() - f64::from(size) * 1.25) / 2.0
}

/// Trim `s` with a trailing ellipsis until it fits `max_w` px at `size`.
fn elide(tr: &mut TextRenderer, s: &str, size: f32, max_w: f64, mono: bool) -> String {
    if s.is_empty() {
        return String::new();
    }
    let measure = |tr: &mut TextRenderer, t: &str| {
        if mono {
            tr.measure_styled(t, size, None, TextWeight::Regular, false, true)
                .0
        } else {
            tr.measure(t, size, None).0
        }
    };
    if f64::from(measure(tr, s)) <= max_w {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let full = f64::from(measure(tr, s));
    let mut keep = (((max_w / full) * chars.len() as f64) as usize).min(chars.len());
    loop {
        if keep == 0 {
            return "…".to_string();
        }
        let cand: String = chars.get(..keep).unwrap_or(&[]).iter().collect::<String>() + "…";
        if f64::from(measure(tr, &cand)) <= max_w {
            return cand;
        }
        keep -= 1;
    }
}

impl LogsApp {
    fn draw_header(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        reg: &Regions,
    ) {
        let c = &theme.tokens.colors;
        fill_rect(scene, t, reg.header, c.surfaces.raised);
        // bottom hairline
        line(
            scene,
            t,
            reg.header.x0,
            reg.header.y1,
            reg.header.x1,
            reg.header.y1,
            c.border_standard,
            1.0,
        );

        // The OS owns the window title/app-name — the header draws only its
        // functional controls (the entry counts, an optional Clear-filters
        // button, and the live-tail pill).
        self.draw_header_clear_button(scene, t, tr, theme, reg);
        self.draw_header_counts(scene, t, tr, theme, reg);
        self.draw_header_live_pill(scene, t, tr, theme, reg);
    }

    /// `draw_header`'s Clear-filters button — right-aligned, just left of the
    /// live pill, shown only while a filter is active (its rect is zero
    /// otherwise).
    fn draw_header_clear_button(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        reg: &Regions,
    ) {
        if !self.filter.is_clear() && reg.clear_btn.width() > 0.0 {
            fill_round(
                scene,
                t,
                reg.clear_btn,
                theme.tokens.radii.md,
                tint(theme.accent_primary, 40),
            );
            stroke_round(
                scene,
                t,
                reg.clear_btn,
                theme.tokens.radii.md,
                theme.accent_primary,
                1.0,
            );
            let bsz = theme.tokens.typography.scale.sm;
            let label = "Clear filters";
            let (bw, _) = tr.measure_weighted(label, bsz, None, TextWeight::Semibold);
            text_at(
                tr,
                scene,
                t,
                label,
                bsz,
                theme.accent_primary,
                reg.clear_btn.center().x - f64::from(bw) / 2.0,
                vcenter(reg.clear_btn, bsz),
                TextWeight::Semibold,
                false,
            );
        }
    }

    /// `draw_header`'s entry counts, just left of whichever right-aligned
    /// control is present.
    fn draw_header_counts(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        reg: &Regions,
    ) {
        let counts = format!(
            "{} shown · {} total",
            self.filtered.len(),
            self.entries.len()
        );
        let (cw, _) = tr.measure(&counts, theme.tokens.typography.scale.sm, None);
        let counts_anchor = if reg.clear_btn.width() > 0.0 {
            reg.clear_btn.x0
        } else {
            reg.live_pill.x0
        };
        text_at(
            tr,
            scene,
            t,
            &counts,
            theme.tokens.typography.scale.sm,
            theme.text_dim,
            counts_anchor - 14.0 - f64::from(cw),
            vcenter(reg.header, theme.tokens.typography.scale.sm),
            TextWeight::Medium,
            false,
        );
    }

    /// `draw_header`'s live-tail pill.
    fn draw_header_live_pill(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        reg: &Regions,
    ) {
        let c = &theme.tokens.colors;
        let on = self.live_tail;
        let pill_bg = if on {
            theme.accent_primary
        } else {
            c.surface_white_5
        };
        fill_round(scene, t, reg.live_pill, theme.tokens.radii.full, pill_bg);
        if !on {
            stroke_round(
                scene,
                t,
                reg.live_pill,
                theme.tokens.radii.full,
                c.border_standard,
                1.0,
            );
        }
        let dot_c = if on { Color::WHITE } else { theme.text_dim };
        fill_circle(
            scene,
            t,
            Point::new(reg.live_pill.x0 + 16.0, reg.live_pill.center().y),
            4.0,
            dot_c,
        );
        let label = "Live tail";
        let lc = if on { Color::WHITE } else { theme.text_dim };
        text_at(
            tr,
            scene,
            t,
            label,
            theme.tokens.typography.scale.sm,
            lc,
            reg.live_pill.x0 + 28.0,
            vcenter(reg.live_pill, theme.tokens.typography.scale.sm),
            TextWeight::Semibold,
            false,
        );
    }

    fn draw_filter_bar(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        reg: &Regions,
    ) {
        let c = &theme.tokens.colors;
        fill_rect(scene, t, reg.filter, c.surfaces.base);
        line(
            scene,
            t,
            reg.filter.x0,
            reg.filter.y1,
            reg.filter.x1,
            reg.filter.y1,
            c.border_standard,
            1.0,
        );
        self.draw_filter_search_box(scene, t, tr, theme, reg);
        self.draw_filter_dropdown_triggers(scene, t, tr, theme, reg);
        self.draw_filter_severity_chips(scene, t, tr, theme, reg);
    }

    /// `draw_filter_bar`'s search box: field background, focus ring, search
    /// glyph, and the shared [`TextInput`] itself.
    fn draw_filter_search_box(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        reg: &Regions,
    ) {
        let c = &theme.tokens.colors;
        let focused = self.search_input.is_focused();
        fill_round(
            scene,
            t,
            reg.search,
            theme.tokens.radii.md,
            c.surfaces.raised,
        );
        let border = if focused {
            c.border_focus
        } else {
            c.border_standard
        };
        stroke_round(
            scene,
            t,
            reg.search,
            theme.tokens.radii.md,
            border,
            if focused { 1.5 } else { 1.0 },
        );
        // search glyph
        let gy = reg.search.center().y;
        fill_circle(
            scene,
            t,
            Point::new(reg.search.x0 + 16.0, gy - 1.0),
            4.5,
            Color::TRANSPARENT,
        );
        stroke_round(
            scene,
            t,
            Rect::from_center_size(Point::new(reg.search.x0 + 15.0, gy - 1.0), (8.0, 8.0)),
            4.0,
            theme.text_dim,
            1.4,
        );
        line(
            scene,
            t,
            reg.search.x0 + 18.5,
            gy + 2.5,
            reg.search.x0 + 22.0,
            gy + 6.0,
            theme.text_dim,
            1.4,
        );
        self.search_input.apply_layout_external(Rect::new(
            reg.search.x0 + 28.0,
            reg.search.y0,
            reg.search.x1 - 8.0,
            reg.search.y1,
        ));
        let presence = PresenceCtx::empty();
        self.search_input.draw(scene, t, tr, theme, &presence);
    }

    /// `draw_filter_bar`'s unit + boot dropdown triggers.
    fn draw_filter_dropdown_triggers(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        reg: &Regions,
    ) {
        let unit_label = if self.filter.unit.is_empty() {
            "All units".to_string()
        } else {
            self.filter.unit.clone()
        };
        Self::draw_dropdown_trigger(
            scene,
            t,
            tr,
            theme,
            reg.unit_trigger,
            "Unit",
            &unit_label,
            self.popup == Some(Popup::Unit),
            !self.filter.unit.is_empty(),
        );

        let boot_label = if self.filter.boot.is_empty() {
            "All boots".to_string()
        } else {
            self.boots
                .iter()
                .find(|(id, _)| id == &self.filter.boot)
                .map_or_else(
                    || self.filter.boot.chars().take(8).collect(),
                    |(_, l)| l.clone(),
                )
        };
        Self::draw_dropdown_trigger(
            scene,
            t,
            tr,
            theme,
            reg.boot_trigger,
            "Boot",
            &boot_label,
            self.popup == Some(Popup::Boot),
            !self.filter.boot.is_empty(),
        );
    }

    /// `draw_filter_bar`'s severity threshold chips.
    fn draw_filter_severity_chips(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        reg: &Regions,
    ) {
        let c = &theme.tokens.colors;
        let tc = Self::theme_colors(theme);
        for (i, chip) in reg.chips.iter().enumerate() {
            let Some(p) = Priority::ALL.get(i).copied() else {
                continue;
            };
            let active = p.level() <= self.filter.min_priority;
            let col = p.color(&tc);
            if active {
                fill_round(scene, t, *chip, theme.tokens.radii.md, tint(col, 235));
            } else {
                fill_round(scene, t, *chip, theme.tokens.radii.md, c.surface_white_5);
                stroke_round(scene, t, *chip, theme.tokens.radii.md, tint(col, 90), 1.0);
            }
            let tag = p.tag();
            let label_c = if active {
                contrast_on(col)
            } else {
                theme.text_dim
            };
            let sz = theme.tokens.typography.scale.xs;
            let (lw, _) = tr.measure_weighted(tag, sz, None, TextWeight::Bold);
            text_at(
                tr,
                scene,
                t,
                tag,
                sz,
                label_c,
                chip.center().x - f64::from(lw) / 2.0,
                vcenter(*chip, sz),
                TextWeight::Bold,
                false,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_dropdown_trigger(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        kind: &str,
        label: &str,
        open: bool,
        active: bool,
    ) {
        let c = &theme.tokens.colors;
        fill_round(scene, t, rect, theme.tokens.radii.md, c.surfaces.raised);
        let border = if open {
            c.border_focus
        } else if active {
            theme.accent_primary
        } else {
            c.border_standard
        };
        stroke_round(
            scene,
            t,
            rect,
            theme.tokens.radii.md,
            border,
            if open || active { 1.5 } else { 1.0 },
        );
        let sz = theme.tokens.typography.scale.sm;
        // kind caption
        text_at(
            tr,
            scene,
            t,
            kind,
            theme.tokens.typography.scale.xs,
            theme.text_dim,
            rect.x0 + 12.0,
            rect.y0 + 4.0,
            TextWeight::Semibold,
            false,
        );
        let val = elide(tr, label, sz, rect.width() - 38.0, false);
        text_at(
            tr,
            scene,
            t,
            &val,
            sz,
            theme.text_primary,
            rect.x0 + 12.0,
            rect.y0 + 16.0,
            TextWeight::Medium,
            false,
        );
        // chevron
        let cxp = rect.x1 - 16.0;
        let cyp = rect.center().y + 2.0;
        line(
            scene,
            t,
            cxp - 4.0,
            cyp - 3.0,
            cxp,
            cyp + 2.0,
            theme.text_dim,
            1.4,
        );
        line(
            scene,
            t,
            cxp + 4.0,
            cyp - 3.0,
            cxp,
            cyp + 2.0,
            theme.text_dim,
            1.4,
        );
    }

    fn draw_list(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        reg: &Regions,
    ) {
        let c = &theme.tokens.colors;
        let tc = Self::theme_colors(theme);
        let sel = self.effective_selected();

        // List frame (header + body) as one card.
        let card = Rect::new(
            reg.list_header.x0,
            reg.list_header.y0,
            reg.list_header.x1,
            reg.list_body.y1,
        );
        fill_round(scene, t, card, theme.tokens.radii.lg, c.surfaces.raised);
        stroke_round(
            scene,
            t,
            card,
            theme.tokens.radii.lg,
            c.border_standard,
            1.0,
        );

        // Column header.
        fill_round(
            scene,
            t,
            Rect::new(
                reg.list_header.x0,
                reg.list_header.y0,
                reg.list_header.x1,
                reg.list_header.y1,
            ),
            0.0,
            c.surfaces.base,
        );
        let cols = column_x(reg.list_header);
        let hsz = theme.tokens.typography.scale.xs;
        let hy = vcenter(reg.list_header, hsz);
        text_at(
            tr,
            scene,
            t,
            "TIME",
            hsz,
            theme.text_dim,
            cols.time_x,
            hy,
            TextWeight::Semibold,
            false,
        );
        text_at(
            tr,
            scene,
            t,
            "LEVEL",
            hsz,
            theme.text_dim,
            cols.tag_x,
            hy,
            TextWeight::Semibold,
            false,
        );
        text_at(
            tr,
            scene,
            t,
            "UNIT",
            hsz,
            theme.text_dim,
            cols.unit_x,
            hy,
            TextWeight::Semibold,
            false,
        );
        text_at(
            tr,
            scene,
            t,
            "MESSAGE",
            hsz,
            theme.text_dim,
            cols.msg_x,
            hy,
            TextWeight::Semibold,
            false,
        );
        line(
            scene,
            t,
            reg.list_header.x0 + 8.0,
            reg.list_header.y1,
            reg.list_header.x1 - 8.0,
            reg.list_header.y1,
            c.border_standard,
            1.0,
        );

        // Empty state.
        if self.filtered.is_empty() {
            let cx = reg.list_body.center().x;
            let cy = reg.list_body.center().y;
            if self.entries.is_empty() {
                // Honest zero-state: no readable journal (never the sample).
                let title = "No journal entries";
                let subtitle = if self.allow_live {
                    "The systemd journal is empty or not readable from this session."
                } else {
                    "Run where journald is available to view live system logs."
                };
                let tsz = theme.tokens.typography.scale.lg;
                let (tw, _) = tr.measure(title, tsz, None);
                text_at(
                    tr,
                    scene,
                    t,
                    title,
                    tsz,
                    c.text_primary,
                    cx - f64::from(tw) / 2.0,
                    cy - 24.0,
                    TextWeight::Semibold,
                    false,
                );
                let ssz = theme.tokens.typography.scale.sm;
                let (sw, _) = tr.measure(subtitle, ssz, None);
                text_at(
                    tr,
                    scene,
                    t,
                    subtitle,
                    ssz,
                    theme.text_dim,
                    cx - f64::from(sw) / 2.0,
                    cy + 2.0,
                    TextWeight::Regular,
                    false,
                );
            } else {
                let msg = "No entries match the current filters.";
                let sz = theme.tokens.typography.scale.base;
                let (mw, _) = tr.measure(msg, sz, None);
                text_at(
                    tr,
                    scene,
                    t,
                    msg,
                    sz,
                    theme.text_dim,
                    cx - f64::from(mw) / 2.0,
                    cy - 10.0,
                    TextWeight::Regular,
                    false,
                );
            }
            return;
        }

        // Clip + draw rows.
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &reg.list_body);
        let body = reg.list_body;
        let first = (f64::from(self.scroll) / ROW_H).floor().max(0.0) as usize;
        let visible = (body.height() / ROW_H).ceil() as usize + 2;
        for pos in first..(first + visible).min(self.filtered.len()) {
            let Some(&abs) = self.filtered.get(pos) else {
                continue;
            };
            let Some(e) = self.entries.get(abs) else {
                continue;
            };
            let row_y0 = body.y0 + pos as f64 * ROW_H - f64::from(self.scroll);
            let row = Rect::new(body.x0, row_y0, body.x1, row_y0 + ROW_H);
            if row.y1 < body.y0 || row.y0 > body.y1 {
                continue;
            }
            let is_sel = sel == Some(abs);
            let is_hover = self.hover_row == Some(abs);
            let col = e.priority.color(&tc);
            if is_sel {
                fill_rect(scene, t, row, tint(theme.accent_primary, 46));
                fill_rect(
                    scene,
                    t,
                    Rect::new(row.x0, row.y0, row.x0 + 3.0, row.y1),
                    theme.accent_primary,
                );
            } else if is_hover {
                fill_rect(scene, t, row, c.surface_white_5);
            } else if pos % 2 == 1 {
                fill_rect(scene, t, row, tint(Color::WHITE, 6));
            }

            let cols = column_x(row);
            // severity dot
            fill_circle(scene, t, Point::new(cols.dot_cx, row.center().y), 4.5, col);
            // time (mono, dim)
            let tsz = theme.tokens.typography.scale.sm;
            text_at(
                tr,
                scene,
                t,
                &e.timestamp(),
                tsz,
                theme.text_dim,
                cols.time_x,
                vcenter(row, tsz),
                TextWeight::Regular,
                true,
            );
            // level tag (colored)
            text_at(
                tr,
                scene,
                t,
                e.priority.tag(),
                theme.tokens.typography.scale.xs,
                col,
                cols.tag_x,
                vcenter(row, theme.tokens.typography.scale.xs),
                TextWeight::Bold,
                true,
            );
            // unit (accent-tinted)
            let unit_shown = elide(tr, e.unit_short(), tsz, cols.unit_w - 6.0, false);
            text_at(
                tr,
                scene,
                t,
                &unit_shown,
                tsz,
                theme.text_primary,
                cols.unit_x,
                vcenter(row, tsz),
                TextWeight::Medium,
                false,
            );
            // message
            let msg_shown = elide(tr, &e.message, tsz, cols.msg_w, false);
            let msg_c = if is_sel {
                theme.text_primary
            } else {
                tint(theme.text_primary, 220)
            };
            text_at(
                tr,
                scene,
                t,
                &msg_shown,
                tsz,
                msg_c,
                cols.msg_x,
                vcenter(row, tsz),
                TextWeight::Regular,
                false,
            );
        }
        scene.pop_layer();

        // Scrollbar.
        let content = self.content_h();
        if content > body.height() {
            let track_x = body.x1 - 5.0;
            let frac_v = body.height() / content;
            let thumb_h = (frac_v * body.height()).max(28.0);
            let max_scroll = (content - body.height()).max(1.0);
            let ty0 = body.y0 + (f64::from(self.scroll) / max_scroll) * (body.height() - thumb_h);
            fill_round(
                scene,
                t,
                Rect::new(track_x, ty0, track_x + 3.0, ty0 + thumb_h),
                1.5,
                tint(Color::WHITE, 70),
            );
        }
    }

    fn draw_detail(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        reg: &Regions,
    ) {
        let c = &theme.tokens.colors;
        let tc = Self::theme_colors(theme);
        let d = reg.detail;
        fill_round(scene, t, d, theme.tokens.radii.lg, c.surfaces.raised);
        stroke_round(scene, t, d, theme.tokens.radii.lg, c.border_standard, 1.0);

        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &d);
        let inner_x = d.x0 + 18.0;
        let inner_w = d.width() - 36.0;
        let mut y = d.y0 + 18.0;

        // On narrow windows the detail is a pushed pane; draw a Back
        // affordance at the top and inset the field content beneath it.
        if reg.narrow && reg.back_btn.width() > 0.0 {
            self.draw_back_button(scene, t, tr, theme, reg.back_btn);
            y = reg.back_btn.y1 + 14.0;
        }

        let Some(abs) = self.effective_selected() else {
            let msg = "Select an entry to inspect its fields.";
            let sz = theme.tokens.typography.scale.base;
            text_at(
                tr,
                scene,
                t,
                msg,
                sz,
                theme.text_dim,
                inner_x,
                y.max(d.center().y),
                TextWeight::Regular,
                false,
            );
            scene.pop_layer();
            return;
        };
        let e = &self.entries[abs];
        let col = e.priority.color(&tc);

        // Priority badge pill.
        let badge_label = e.priority.long().to_uppercase();
        let bsz = theme.tokens.typography.scale.xs;
        let (blw, _) = tr.measure_weighted(&badge_label, bsz, None, TextWeight::Bold);
        let badge = Rect::new(inner_x, y, inner_x + f64::from(blw) + 24.0, y + 22.0);
        fill_round(scene, t, badge, theme.tokens.radii.full, tint(col, 235));
        text_at(
            tr,
            scene,
            t,
            &badge_label,
            bsz,
            contrast_on(col),
            badge.x0 + 12.0,
            vcenter(badge, bsz),
            TextWeight::Bold,
            false,
        );
        // transport tag to the right of the badge
        if !e.transport.is_empty() {
            text_at(
                tr,
                scene,
                t,
                &e.transport,
                bsz,
                theme.text_dim,
                badge.x1 + 10.0,
                vcenter(badge, bsz),
                TextWeight::Medium,
                true,
            );
        }
        y = badge.y1 + 14.0;

        // Unit title.
        let title_sz = theme.tokens.typography.scale.lg;
        let title = elide(tr, &e.unit, title_sz, inner_w, false);
        text_at(
            tr,
            scene,
            t,
            &title,
            title_sz,
            theme.text_primary,
            inner_x,
            y,
            TextWeight::Semibold,
            false,
        );
        y += f64::from(title_sz) * 1.5;

        // Timestamp.
        let tssz = theme.tokens.typography.scale.sm;
        text_at(
            tr,
            scene,
            t,
            &e.timestamp_full(),
            tssz,
            theme.text_dim,
            inner_x,
            y,
            TextWeight::Regular,
            true,
        );
        y += f64::from(tssz) * 1.6 + 6.0;

        // Message (wrapped).
        let msz = theme.tokens.typography.scale.base;
        let (_, mh) = tr.measure(&e.message, msz, Some(inner_w as f32));
        tr.draw_text(
            scene,
            &e.message,
            msz,
            theme.text_primary,
            t * Affine::translate((inner_x, y)),
            Some(inner_w as f32),
        );
        y += f64::from(mh) + 14.0;

        // Divider.
        line(
            scene,
            t,
            inner_x,
            y,
            inner_x + inner_w,
            y,
            c.border_standard,
            1.0,
        );
        y += 12.0;

        // Field table.
        let mut fields: Vec<(String, String)> = vec![
            (
                "PRIORITY".into(),
                format!("{} ({})", e.priority.tag(), e.priority.level()),
            ),
            ("UNIT".into(), e.unit.clone()),
            ("IDENTIFIER".into(), e.identifier.clone()),
            (
                "PID".into(),
                e.pid.map_or_else(|| "—".into(), |p| p.to_string()),
            ),
            ("HOSTNAME".into(), e.hostname.clone()),
            ("TRANSPORT".into(), e.transport.clone()),
            ("BOOT_ID".into(), e.boot_id.clone()),
        ];
        for (k, v) in &e.extra {
            fields.push((k.clone(), v.clone()));
        }

        let key_w = 118.0_f64;
        let ksz = theme.tokens.typography.scale.xs;
        let vsz = theme.tokens.typography.scale.sm;
        for (k, v) in fields {
            if y > d.y1 - 14.0 {
                break;
            }
            if v.trim().is_empty() {
                continue;
            }
            text_at(
                tr,
                scene,
                t,
                &k,
                ksz,
                theme.text_dim,
                inner_x,
                y + 1.0,
                TextWeight::Semibold,
                true,
            );
            let val = elide(tr, &v, vsz, inner_w - key_w - 8.0, false);
            text_at(
                tr,
                scene,
                t,
                &val,
                vsz,
                theme.text_primary,
                inner_x + key_w,
                y,
                TextWeight::Regular,
                false,
            );
            y += 22.0;
        }

        scene.pop_layer();
    }

    /// The Back affordance at the top of the pushed detail pane (narrow
    /// windows only): a pill with a left chevron + "Back" that returns to
    /// the list.
    fn draw_back_button(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
    ) {
        let c = &theme.tokens.colors;
        let hovered = rect.contains(self.cursor);
        let bg = if hovered {
            c.surface_white_10
        } else {
            c.surface_white_5
        };
        fill_round(scene, t, rect, theme.tokens.radii.full, bg);
        stroke_round(
            scene,
            t,
            rect,
            theme.tokens.radii.full,
            c.border_standard,
            1.0,
        );
        // Left chevron.
        let cyp = rect.center().y;
        let cxp = rect.x0 + 18.0;
        line(
            scene,
            t,
            cxp + 4.0,
            cyp - 5.0,
            cxp - 1.0,
            cyp,
            theme.text_primary,
            1.6,
        );
        line(
            scene,
            t,
            cxp - 1.0,
            cyp,
            cxp + 4.0,
            cyp + 5.0,
            theme.text_primary,
            1.6,
        );
        let sz = theme.tokens.typography.scale.sm;
        text_at(
            tr,
            scene,
            t,
            "Back",
            sz,
            theme.text_primary,
            rect.x0 + 30.0,
            vcenter(rect, sz),
            TextWeight::Semibold,
            false,
        );
    }
}

// Column geometry inside a list row / header.
struct Cols {
    dot_cx: f64,
    time_x: f64,
    tag_x: f64,
    unit_x: f64,
    unit_w: f64,
    msg_x: f64,
    msg_w: f64,
}
fn column_x(row: Rect) -> Cols {
    let rpad = 12.0;
    let mut cur = row.x0 + rpad;
    let dot_cx = cur + 4.0;
    cur += 16.0;
    let time_x = cur;
    cur += 138.0 + 8.0;
    let tag_x = cur;
    cur += 50.0 + 8.0;
    let unit_x = cur;
    let unit_w = 132.0;
    cur += unit_w + 10.0;
    let msg_x = cur;
    let msg_w = (row.x1 - rpad - msg_x).max(40.0);
    Cols {
        dot_cx,
        time_x,
        tag_x,
        unit_x,
        unit_w,
        msg_x,
        msg_w,
    }
}

fn line(scene: &mut Scene, t: Affine, x0: f64, y0: f64, x1: f64, y1: f64, c: Color, w: f64) {
    scene.stroke(
        &Stroke::new(w),
        t,
        c,
        None,
        &Line::new(Point::new(x0, y0), Point::new(x1, y1)),
    );
}

/// Pick a readable foreground for text sitting on a saturated severity
/// fill: white over the dark dangers, near-black over amber.
fn contrast_on(bg: Color) -> Color {
    let lum = 0.299 * f64::from(bg.r()) + 0.587 * f64::from(bg.g()) + 0.114 * f64::from(bg.b());
    if lum > 150.0 {
        Color::from_rgb8(20, 20, 25)
    } else {
        Color::WHITE
    }
}

// ===================================================================
// Widget impl
// ===================================================================

impl Widget for LogsApp {
    fn sync(&mut self, doc: &mut AutoCommit, root: &ObjId) {
        LogsDoc::ensure_defaults(doc, root);
        self.search_input.sync(doc, root);

        // Optional live tail: throttled re-poll, never under --shots.
        if self.allow_live
            && LogsDoc::live_tail(doc, root)
            && self.last_poll.elapsed() >= POLL_INTERVAL
        {
            self.last_poll = Instant::now();
            let fresh = crate::journal::load_live(2000);
            if !fresh.is_empty() {
                self.entries = fresh;
                self.rebuild_facets();
                self.dirty = true;
            }
        }

        let new_filter = LogsDoc::filter(doc, root);
        let new_sel = LogsDoc::selected(doc, root);
        let new_live = LogsDoc::live_tail(doc, root);
        let changed = new_filter.min_priority != self.filter.min_priority
            || new_filter.unit != self.filter.unit
            || new_filter.boot != self.filter.boot
            || new_filter.search != self.filter.search
            || new_sel != self.sel_raw
            || new_live != self.live_tail;
        self.filter = new_filter;
        self.sel_raw = new_sel;
        self.live_tail = new_live;
        self.recompute_filtered();
        if changed {
            self.dirty = true;
        }
        self.clamp_scroll();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                flex_grow: 1.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        self.node = Some(node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        self.clamp_scroll();
        // Seed the shot's scroll so the auto-selected entry is visible.
        if let Some(abs) = self.effective_selected()
            && let Some(pos) = self.filtered_pos(abs)
        {
            // Only auto-center on the very first layout (scroll == 0
            // and nothing has been interacted with yet).
            if self.scroll == 0.0 {
                let body_h = self.list_viewport_h();
                let top = pos as f64 * ROW_H;
                if top + ROW_H > body_h {
                    self.scroll = ((top + ROW_H) - body_h) as f32 + 4.0;
                    self.clamp_scroll();
                }
            }
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // Advance the master↔detail push slide. `draw` is the per-frame
        // animation tick for a self-rendering widget (the live loop keeps
        // redrawing while `needs_animation_frame` is true); ticking here
        // keeps the eased slide in step with the frame it's drawn into.
        if self.push.tick() {
            self.dirty = true;
        }

        let reg = self.regions();
        let c = &theme.tokens.colors;
        // App background.
        fill_rect(scene, transform, self.rect, c.surfaces.base);

        self.draw_header(scene, transform, tr, theme, &reg);
        self.draw_filter_bar(scene, transform, tr, theme, &reg);
        if reg.narrow {
            // Single-pane push: clip both slabs to the content area so the
            // off-screen pane (sliding in/out) never bleeds past the edges,
            // and skip whichever pane is fully off-screen.
            let content = Rect::new(self.rect.x0, reg.filter.y1, self.rect.x1, self.rect.y1);
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                1.0,
                transform,
                &content,
            );
            let slide = self.push.slide();
            if slide < 0.999 {
                self.draw_list(scene, transform, tr, theme, &reg);
            }
            if slide > 0.001 {
                self.draw_detail(scene, transform, tr, theme, &reg);
            }
            scene.pop_layer();
        } else {
            self.draw_list(scene, transform, tr, theme, &reg);
            self.draw_detail(scene, transform, tr, theme, &reg);
        }
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if self.popup.is_none() {
            return;
        }
        let reg = self.regions();
        let c = &theme.tokens.colors;
        if reg.dropdown.is_empty() {
            return;
        }
        let first = reg.dropdown.first().unwrap().0;
        let last = reg.dropdown.last().unwrap().0;
        let panel = Rect::new(first.x0, first.y0 - 4.0, first.x1, last.y1 + 4.0);
        // soft shadow
        fill_round(
            scene,
            transform,
            panel.inflate(2.0, 2.0),
            theme.tokens.radii.md,
            tint(Color::BLACK, 80),
        );
        fill_round(
            scene,
            transform,
            panel,
            theme.tokens.radii.md,
            c.surfaces.overlay,
        );
        stroke_round(
            scene,
            transform,
            panel,
            theme.tokens.radii.md,
            c.border_standard,
            1.0,
        );

        let active_val = match self.popup {
            Some(Popup::Unit) => self.filter.unit.clone(),
            Some(Popup::Boot) => self.filter.boot.clone(),
            None => String::new(),
        };
        let sz = theme.tokens.typography.scale.sm;
        for (irect, val, label) in &reg.dropdown {
            let is_hover = irect.contains(self.cursor);
            let is_active = &active_val == val;
            if is_hover {
                fill_rect(scene, transform, *irect, c.surface_white_10);
            } else if is_active {
                fill_rect(scene, transform, *irect, tint(theme.accent_primary, 40));
            }
            let lc = if is_active {
                theme.accent_primary
            } else {
                theme.text_primary
            };
            let shown = elide(tr, label, sz, irect.width() - 30.0, false);
            text_at(
                tr,
                scene,
                transform,
                &shown,
                sz,
                lc,
                irect.x0 + 12.0,
                vcenter(*irect, sz),
                if is_active {
                    TextWeight::Semibold
                } else {
                    TextWeight::Regular
                },
                false,
            );
            if is_active {
                // check mark
                let cy = irect.center().y;
                line(
                    scene,
                    transform,
                    irect.x1 - 18.0,
                    cy,
                    irect.x1 - 14.0,
                    cy + 4.0,
                    theme.accent_primary,
                    1.6,
                );
                line(
                    scene,
                    transform,
                    irect.x1 - 14.0,
                    cy + 4.0,
                    irect.x1 - 8.0,
                    cy - 4.0,
                    theme.accent_primary,
                    1.6,
                );
            }
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        doc: &mut AutoCommit,
        root: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        let search_rect = self.regions().search;
        self.search_input.apply_layout_external(Rect::new(
            search_rect.x0 + 28.0,
            search_rect.y0,
            search_rect.x1 - 8.0,
            search_rect.y1,
        ));

        // This field is embedded in a canvas-style parent rather than mounted
        // as a child node, so the parent explicitly owns focus transfer while
        // still routing every editing event through the shared text core.
        let left_press = matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        );
        if left_press {
            if search_rect.contains(self.cursor) {
                self.search_input.set_focus(true);
            } else {
                self.search_input.set_focus(false);
            }
        }

        // Preserve Logs' convenient type-to-filter behaviour: a printable key
        // (or Backspace) transfers focus into the real field before routing,
        // instead of maintaining a second append/pop editor in the app.
        let starts_search = matches!(
            event,
            WindowEvent::KeyboardInput { event: key, .. }
                if key.state == ElementState::Pressed
                    && !modifiers.state().control_key()
                    && !modifiers.state().super_key()
                    && (matches!(key.logical_key, Key::Character(_))
                        || matches!(key.logical_key, Key::Named(NamedKey::Backspace)))
        );
        if starts_search && !self.search_input.is_focused() {
            self.search_input.set_focus(true);
            self.search_input
                .set_cursor_pos(self.search_input.value().len());
        }

        if self.search_input.is_focused()
            && matches!(
                event,
                WindowEvent::KeyboardInput { event: key, .. }
                    if key.state == ElementState::Pressed
                        && matches!(key.logical_key, Key::Named(NamedKey::Escape))
            )
        {
            LogsDoc::search_set(doc, root, "");
            self.search_input.set_text("");
            self.scroll = 0.0;
            self.dirty = true;
            *handled = true;
            return;
        }

        let route_to_search = self.search_input.is_focused()
            || matches!(event, WindowEvent::CursorMoved { .. })
            || (left_press && search_rect.contains(self.cursor));
        if route_to_search {
            let mut input_handled = false;
            self.search_input.handle_event(
                event,
                scale_factor,
                modifiers,
                doc,
                root,
                &mut input_handled,
                focus_target,
                node_id,
                taffy,
            );
            if self.search_input.take_dirty().is_some() {
                self.scroll = 0.0;
                self.dirty = true;
                input_handled = true;
            }
            if input_handled && !matches!(event, WindowEvent::CursorMoved { .. }) {
                *handled = true;
                return;
            }
        }

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = Point::new(position.x, position.y);
                // Hover row tracking.
                let reg = self.regions();
                let prev = self.hover_row;
                self.hover_row = self.row_at(&reg, self.cursor);
                if self.hover_row != prev {
                    self.dirty = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let reg = self.regions();
                if reg.list_body.contains(self.cursor) || reg.list_header.contains(self.cursor) {
                    let dy = match delta {
                        MouseScrollDelta::LineDelta(_, y) => -*y * 60.0,
                        MouseScrollDelta::PixelDelta(p) => -p.y as f32,
                    };
                    self.scroll += dy;
                    self.clamp_scroll();
                    self.dirty = true;
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.focused = true;
                self.on_click(self.cursor, doc, root, handled);
            }
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } if key_event.state == ElementState::Pressed => {
                self.on_key(key_event, modifiers, doc, root, handled);
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "LogsApp"
    }
    fn test_id(&self) -> Option<&str> {
        Some("logs_app")
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn is_focusable(&self) -> bool {
        true
    }
    fn set_focus(&mut self, f: bool) {
        if self.focused != f {
            self.dirty = true;
        }
        self.focused = f;
        if !f {
            self.search_input.set_focus(false);
        }
    }
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.search_input.ime_cursor_rect()
    }
    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        self.search_input.ime_purpose()
    }
    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.search_input.cursor_shape(point)
    }
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.search_input
            .is_focused()
            .then(|| self.search_input.accessibility())
            .flatten()
    }
    fn needs_commit(&self) -> bool {
        self.search_input.needs_commit()
    }
    fn on_commit(&mut self) {
        self.search_input.on_commit();
    }
    fn needs_redraw(&self) -> bool {
        self.dirty
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
    fn is_layout_dirty(&self) -> bool {
        self.dirty
    }
    fn needs_idle_sync(&self) -> bool {
        self.allow_live && self.live_tail
    }
    fn needs_animation_frame(&self) -> bool {
        // Keep pumping frames while the push slide eases into place, or
        // while live tail is on.
        self.push.is_animating() || (self.allow_live && self.live_tail)
    }
}

impl LogsApp {
    /// Filtered-list hit test: which absolute entry index sits under
    /// `pt` in the list body, if any.
    fn row_at(&self, reg: &Regions, pt: Point) -> Option<usize> {
        if !reg.list_body.contains(pt) {
            return None;
        }
        let rel = pt.y - reg.list_body.y0 + f64::from(self.scroll);
        let pos = (rel / ROW_H).floor();
        if pos < 0.0 {
            return None;
        }
        let pos = pos as usize;
        self.filtered.get(pos).copied()
    }

    fn on_click(&mut self, pt: Point, doc: &mut AutoCommit, root: &ObjId, handled: &mut bool) {
        // 1. An open dropdown captures the click first.
        if self.popup.is_some() {
            let reg = self.regions();
            for (irect, val, _label) in &reg.dropdown {
                if irect.contains(pt) {
                    match self.popup {
                        Some(Popup::Unit) => LogsDoc::set_unit(doc, root, val),
                        Some(Popup::Boot) => LogsDoc::set_boot(doc, root, val),
                        None => {}
                    }
                    self.popup = None;
                    self.scroll = 0.0;
                    self.dirty = true;
                    *handled = true;
                    return;
                }
            }
            // Click outside the open list closes it.
            let on_trigger = reg.unit_trigger.contains(pt) || reg.boot_trigger.contains(pt);
            self.popup = None;
            self.dirty = true;
            *handled = true;
            if !on_trigger {
                return;
            }
            // fall through so re-clicking a trigger can re-open it below
        }

        let reg = self.regions();

        // Narrow-window Back affordance: when the detail pane is pushed in,
        // tapping Back slides the list back.
        if reg.narrow && reg.back_btn.width() > 0.0 && reg.back_btn.contains(pt) {
            self.push.show_list();
            self.dirty = true;
            *handled = true;
            return;
        }

        // 2. Header: live-tail pill.
        if reg.live_pill.contains(pt) {
            let v = !self.live_tail;
            LogsDoc::set_live_tail(doc, root, v);
            self.last_poll = Instant::now().checked_sub(POLL_INTERVAL).unwrap(); // force an immediate poll
            self.dirty = true;
            *handled = true;
            return;
        }

        // 3. Search box → focus is transferred to the embedded TextInput by
        // handle_event; this keeps the region consumed by the filter bar.
        if reg.search.contains(pt) {
            self.focused = true;
            *handled = true;
            return;
        }

        // 4. Dropdown triggers.
        if reg.unit_trigger.contains(pt) {
            self.popup = if self.popup == Some(Popup::Unit) {
                None
            } else {
                Some(Popup::Unit)
            };
            self.dirty = true;
            *handled = true;
            return;
        }
        if reg.boot_trigger.contains(pt) {
            self.popup = if self.popup == Some(Popup::Boot) {
                None
            } else {
                Some(Popup::Boot)
            };
            self.dirty = true;
            *handled = true;
            return;
        }

        // 5. Severity chips → set threshold.
        for (i, chip) in reg.chips.iter().enumerate() {
            if chip.contains(pt) {
                let level = Priority::ALL[i].level();
                LogsDoc::set_min_priority(doc, root, level);
                self.scroll = 0.0;
                self.dirty = true;
                *handled = true;
                return;
            }
        }

        // 6. Clear-filters button.
        if !self.filter.is_clear() && reg.clear_btn.contains(pt) {
            LogsDoc::reset_filters(doc, root);
            self.scroll = 0.0;
            self.dirty = true;
            *handled = true;
            return;
        }

        // 7. List rows → select. On a narrow window this also pushes the
        // detail pane in (single-pane master-detail navigation).
        if let Some(abs) = self.row_at(&reg, pt) {
            self.select_abs(abs, doc, root);
            if reg.narrow {
                self.push.show_detail();
            }
            *handled = true;
        }
    }

    fn on_key(
        &mut self,
        key_event: &winit::event::KeyEvent,
        modifiers: &winit::event::Modifiers,
        doc: &mut AutoCommit,
        root: &ObjId,
        handled: &mut bool,
    ) {
        let ctrl = modifiers.state().control_key();
        match &key_event.logical_key {
            Key::Named(NamedKey::ArrowDown) => {
                self.move_selection(1, doc, root);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.move_selection(-1, doc, root);
                *handled = true;
            }
            Key::Named(NamedKey::PageDown) => {
                self.move_selection(10, doc, root);
                *handled = true;
            }
            Key::Named(NamedKey::PageUp) => {
                self.move_selection(-10, doc, root);
                *handled = true;
            }
            Key::Named(NamedKey::Home) => {
                self.move_selection(isize::MIN / 2, doc, root);
                *handled = true;
            }
            Key::Named(NamedKey::End) => {
                self.move_selection(isize::MAX / 2, doc, root);
                *handled = true;
            }
            Key::Named(NamedKey::Escape) => {
                let narrow_in_detail =
                    PushNav::is_narrow(self.rect.width()) && self.push.pane() == Pane::Detail;
                if self.popup.is_some() {
                    self.popup = None;
                } else if narrow_in_detail {
                    // Back out of the pushed detail pane to the list.
                    self.push.show_list();
                } else if !self.filter.search.is_empty() {
                    LogsDoc::search_set(doc, root, "");
                    self.scroll = 0.0;
                }
                self.dirty = true;
                *handled = true;
            }
            Key::Character(s) => {
                if ctrl {
                    match s.as_str() {
                        "l" | "L" => {
                            let v = !self.live_tail;
                            LogsDoc::set_live_tail(doc, root, v);
                            self.last_poll = Instant::now().checked_sub(POLL_INTERVAL).unwrap();
                            *handled = true;
                        }
                        "r" | "R" => {
                            LogsDoc::reset_filters(doc, root);
                            self.scroll = 0.0;
                            *handled = true;
                        }
                        _ => {}
                    }
                }
                self.dirty = true;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::sample_entries;
    use automerge::ROOT;

    fn synced_app() -> (LogsApp, AutoCommit) {
        let mut app = LogsApp::new(sample_entries(), false);
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        app.apply_layout_noop();
        (app, doc)
    }

    impl LogsApp {
        // Test helper: emulate apply_layout's scroll seeding without taffy.
        fn apply_layout_noop(&mut self) {
            self.recompute_filtered();
        }
    }

    #[test]
    fn facets_collect_units_and_two_boots() {
        let app = LogsApp::new(sample_entries(), false);
        assert!(app.units.contains(&"shell.service".to_string()));
        assert_eq!(app.boots.len(), 2);
    }

    #[test]
    fn priority_threshold_filters_entries() {
        let (mut app, mut doc) = synced_app();
        let total = app.filtered.len();
        LogsDoc::set_min_priority(&mut doc, &ROOT, Priority::Err.level());
        app.sync(&mut doc, &ROOT);
        let errs = app.filtered.len();
        assert!(errs < total, "errors-only must show fewer than all");
        assert!(errs > 0);
        for &i in &app.filtered {
            assert!(app.entries[i].priority.level() <= 3);
        }
    }

    #[test]
    fn search_narrows_results() {
        let (mut app, mut doc) = synced_app();
        LogsDoc::search_set(&mut doc, &ROOT, "iroh");
        app.sync(&mut doc, &ROOT);
        assert_eq!(app.search_input.value(), "iroh");
        assert!(!app.filtered.is_empty());
        for &i in &app.filtered {
            let e = &app.entries[i];
            let hit =
                e.message.to_lowercase().contains("iroh") || e.unit.to_lowercase().contains("iroh");
            assert!(hit);
        }
    }

    #[test]
    fn unit_filter_restricts_to_one_unit() {
        let (mut app, mut doc) = synced_app();
        LogsDoc::set_unit(&mut doc, &ROOT, "kernel");
        app.sync(&mut doc, &ROOT);
        assert!(
            app.filtered
                .iter()
                .all(|&i| app.entries[i].unit == "kernel")
        );
    }

    #[test]
    fn auto_selection_lands_on_error_or_worse() {
        let (app, _doc) = synced_app();
        let sel = app.effective_selected().expect("an entry is auto-selected");
        assert!(app.entries[sel].priority.level() <= Priority::Err.level());
    }

    #[test]
    fn arrow_navigation_moves_and_persists_selection() {
        let (mut app, mut doc) = synced_app();
        let before = app.effective_selected().unwrap();
        app.move_selection(1, &mut doc, &ROOT);
        let after = app.effective_selected().unwrap();
        assert_ne!(before, after);
        assert_eq!(LogsDoc::selected(&doc, &ROOT), after as i64);
    }

    #[test]
    fn boot_filter_round_trips_through_doc() {
        let (mut app, mut doc) = synced_app();
        let boot = app.boots[1].0.clone();
        LogsDoc::set_boot(&mut doc, &ROOT, &boot);
        app.sync(&mut doc, &ROOT);
        assert!(app.filtered.iter().all(|&i| app.entries[i].boot_id == boot));
    }

    #[test]
    fn content_spans_to_window_bottom() {
        // With the bottom legend strip gone, the list/detail content must
        // reach the window's bottom edge (minus the usual content padding),
        // not stop short to reserve a legend band.
        let (app, _doc) = synced_app();
        let reg = app.regions();
        assert!(
            reg.list_body.y1 >= app.rect.y1 - PAD - 1.0,
            "list body {} should reach near the window bottom {}",
            reg.list_body.y1,
            app.rect.y1
        );
        assert!(reg.detail.y1 >= app.rect.y1 - PAD - 1.0);
    }

    #[test]
    fn clear_filters_control_lives_in_header_and_resets() {
        let (mut app, mut doc) = synced_app();
        // A clear filter set means no Clear-filters control is laid out.
        assert!(app.filter.is_clear());
        assert_eq!(app.regions().clear_btn, Rect::ZERO);

        // Activate a filter (errors-or-worse threshold).
        LogsDoc::set_min_priority(&mut doc, &ROOT, Priority::Err.level());
        app.sync(&mut doc, &ROOT);
        assert!(!app.filter.is_clear());

        // The Clear-filters control now sits in the header, just left of the
        // live-tail pill — not in any bottom strip.
        let reg = app.regions();
        assert!(reg.clear_btn.width() > 0.0, "clear button must be laid out");
        assert!(
            reg.header.contains(reg.clear_btn.center()),
            "clear button center {:?} must be inside the header {:?}",
            reg.clear_btn.center(),
            reg.header
        );
        assert!(reg.clear_btn.x1 <= reg.live_pill.x0);

        // Clicking it clears the filter through the production on_click path.
        let mut handled = false;
        app.on_click(reg.clear_btn.center(), &mut doc, &ROOT, &mut handled);
        assert!(
            handled,
            "the header Clear-filters button must be hit-tested"
        );
        app.sync(&mut doc, &ROOT);
        assert!(
            app.filter.is_clear(),
            "clicking Clear filters must reset the filter"
        );
    }
}
