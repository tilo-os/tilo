//! `HistoryScrubber` — the per-document version strip.
//!
//! Drag a handle through an artifact's `Vec<HistoryEntry>`. `[Preview]` fires
//! `on_preview`; `[Restore to this version]` fires `on_restore`. The widget
//! owns scrubbing state only — it never touches the CRDT itself, so the same
//! scrubber works across artifact kinds whose restore wiring differs.
//!
//! A track with one tick per change is unusable for a document: a
//! debounced editor produces hundreds of them and they smear into a grey
//! band. Installing groups (`Self::set_groups`) adds a segment band above
//! the track and snaps the handle to boundaries; per-change resolution stays
//! one toggle away (`Self::set_snap_to_groups`). No groups installed is the
//! degenerate case — every change is a tick, nothing snaps — not a separate
//! mode.

use crate::num::f64_of_usize;
use crate::time::SystemTime;
use std::collections::HashMap;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::Color;
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::history::{HistoryEntry, HistoryEntryId, format_relative_time};
use crate::widget::history_group::GroupNameRequest;
use crate::widget::name_field::{NameField, NameFieldOutcome, fit};

/// Must exceed [`HANDLE_RADIUS`] so the handle clears the rect edge.
const TRACK_INSET: f64 = 16.0;
const TRACK_Y_PLAIN: f64 = 56.0;
const TRACK_Y_GROUPED: f64 = 84.0;
const BAND_Y_FROM_TOP: f64 = 50.0;
const BAND_H: f64 = 20.0;
const TRACK_HEIGHT: f64 = 6.0;
const TICK_MARK_H: f64 = 12.0;
/// Below this per-tick spacing, notches become a grey smear; they are
/// dropped and the group band carries the structure instead.
const MIN_TICK_SPACING: f64 = 6.0;
/// Matches `Slider`'s handle radius for visual consistency.
const HANDLE_RADIUS: f64 = 9.0;
/// Larger than [`HANDLE_RADIUS`] so a near-miss still latches the drag.
const HANDLE_HIT_RADIUS: f64 = 14.0;
const PREVIEW_W: f64 = 96.0;
const RESTORE_W: f64 = 160.0;
const BUTTON_H: f64 = 28.0;
/// A version name is a label, not a paragraph — stretching the field across
/// a 1000 px button row would be a metre of empty box for three words.
const NAME_FIELD_W: f64 = 300.0;
const BUTTON_Y_OFFSET: f64 = 24.0;
const POPOVER_H: f64 = 38.0;
/// Between the popover and the band below it.
const POPOVER_BAND_GAP: f64 = crate::tokens::Spacing::BASE.s2 as f64;
/// Breathing room the popover's text stack keeps from its top and bottom
/// edge — a hairline, half the smallest spacing step; a stack that cannot
/// keep it drops its second line.
const POPOVER_PAD_Y: f64 = crate::tokens::Spacing::BASE.s1 as f64 / 2.0;
const NATURAL_H_PLAIN: f64 = 140.0;
const NATURAL_H_GROUPED: f64 = 168.0;

/// The strip paints on its own `surfaces.raised` fill, where `Colors::lift`
/// (base → raised) has nowhere left to go, so every tier inside it —
/// segment, track, button hover — is **one ordered ramp** of translucent
/// steps against that fill, polarity chosen the way `hairline_on` chooses
/// it (HIG §9.3 caveat, §9.5). Ascending: odd segment, even segment, track,
/// the segment under the handle / a hovered button.
const WASH_FAINT: u8 = 20;
const WASH_LOW: u8 = 36;
const WASH_TRACK: u8 = 48;
const WASH_MID: u8 = 64;

/// One step of the strip's own ramp: `alpha` of white or black, whichever
/// moves `surface` further.
fn wash(surface: Color, alpha: u8) -> Color {
    crate::tokens::colors::Colors::hairline_on(surface, alpha)
}

/// Composes a paint transform with a translation. `Affine`'s `Mul` impl is
/// pure `f64` matrix multiply-add with no overflow trap, so it cannot panic
/// regardless of operand magnitude, but going through `pre_translate`
/// keeps that reasoning inside `kurbo` rather than repeating an `#[allow]`
/// at every text transform composed in this file.
fn translated(transform: Affine, offset: (f64, f64)) -> Affine {
    transform.pre_translate(Vec2::new(offset.0, offset.1))
}

/// Turns a `[0.0, 1.0]` drag ratio into a tick index in `0..=max`. The
/// ratio is clamped before rounding and `max` is a bounded entry count, so
/// the result always fits `usize` without wrapping or losing its sign.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "ratio is clamped to [0,1] and max is a bounded entry count, so the rounded product always fits usize"
)]
fn tick_from_ratio(ratio: f64, max: usize) -> usize {
    (ratio.clamp(0.0, 1.0) * f64_of_usize(max)).round() as usize
}

/// `FnMut` so the host can mutate captured state (e.g. flip a
/// preview-mounted flag) without needing interior mutability.
pub type ScrubCallback = Box<dyn FnMut(&HistoryEntryId)>;

pub type NameCallback = Box<dyn FnMut(&GroupNameRequest)>;

pub type VersionCallback = Box<dyn FnMut(&VersionAction)>;

/// `start` / `end` are **inclusive** tick indices. `label` is `None` for a
/// group nobody named, in which case `derived` carries the
/// description the popover shows.
#[derive(Clone, Debug, PartialEq)]
pub struct ScrubberGroup {
    pub start: usize,
    pub end: usize,
    /// The user's name for this run, when it has one.
    pub label: Option<String>,
    /// What the run is, when nobody named it — "12 edits over 4 min".
    pub derived: String,
    /// The naming record, so a rename addresses the right one.
    pub record_id: Option<String>,
    /// Still absorbing new changes.
    pub open: bool,
}

impl ScrubberGroup {
    pub fn title(&self) -> &str {
        self.label.as_deref().unwrap_or(self.derived.as_str())
    }
}

/// Opening or closing a prospective named version — the "I am about to do X"
/// case, as opposed to naming a run after the fact.
#[derive(Clone, Debug, PartialEq)]
pub enum VersionAction {
    /// Start a version at `at` (the newest change), called `label`.
    Open { at: HistoryEntryId, label: String },
    /// Close the version `record_id` names, ending at `at`.
    Close {
        record_id: String,
        at: HistoryEntryId,
    },
}

/// The buttons under the track, in drop order: the last one listed is the first
/// to go when the strip is too narrow to hold them all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScrubButton {
    Restore,
    Preview,
    Name,
    Version,
}

/// Horizontal-slider history scrubber.
// Four independent widget-state flags (snap-to-groups, dragging, hovered,
// dirty) that can co-occur in any combination — not a state machine an
// enum would clarify.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent widget-state flags, not related toggles"
)]
pub struct HistoryScrubber {
    entries: Vec<HistoryEntry>,
    groups: Vec<ScrubberGroup>,
    snap_to_groups: bool,
    rect: Rect,
    /// 0-based index of the currently-selected tick. Always in
    /// `0..entries.len()` (or 0 when entries is empty).
    current_tick: usize,
    /// Lets the widget keep capturing pointer events when the cursor
    /// leaves `rect` mid-drag.
    is_dragging: bool,
    /// Mirrors `Slider`'s hover contract (slight knob-radius scale).
    is_hovered: bool,
    hover_button: Option<ScrubButton>,
    last_cursor: Point,
    on_preview: Option<ScrubCallback>,
    on_restore: Option<ScrubCallback>,
    on_name: Option<NameCallback>,
    on_version: Option<VersionCallback>,
    /// The name being typed, and whether it is for the current group (a rename)
    /// or for a version about to be opened.
    naming: Option<(NameKind, NameField)>,
    /// Pinned by tests so the popover's relative-time label stays stable.
    now_override: Option<SystemTime>,
    dirty: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NameKind {
    /// Naming or renaming the group the handle is in.
    CurrentGroup,
    /// Opening a prospective version at the newest change.
    NewVersion,
}

impl Default for HistoryScrubber {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl HistoryScrubber {
    /// The handle starts pinned at the rightmost tick ("current state"), so
    /// the user has to actively scrub left to view history.
    pub fn new(entries: Vec<HistoryEntry>) -> Self {
        let current = entries.len().saturating_sub(1);
        Self {
            entries,
            groups: Vec::new(),
            snap_to_groups: true,
            rect: Rect::ZERO,
            current_tick: current,
            is_dragging: false,
            is_hovered: false,
            hover_button: None,
            last_cursor: Point::ZERO,
            on_preview: None,
            on_restore: None,
            on_name: None,
            on_version: None,
            naming: None,
            now_override: None,
            dirty: true,
        }
    }

    /// Resets the handle to the rightmost tick (the new "current" state).
    pub fn set_entries(&mut self, entries: Vec<HistoryEntry>) {
        self.entries = entries;
        self.current_tick = self.entries.len().saturating_sub(1);
        self.dirty = true;
    }

    /// Segments outside the entry range are dropped rather than clamped: a
    /// segment that doesn't describe existing ticks would put a label over the
    /// wrong part of the track.
    pub fn set_groups(&mut self, groups: Vec<ScrubberGroup>) {
        let n = self.entries.len();
        let groups: Vec<ScrubberGroup> = groups
            .into_iter()
            .filter(|g| g.start <= g.end && g.end < n)
            .collect();
        if self.groups != groups {
            self.groups = groups;
            self.dirty = true;
        }
    }

    pub fn groups(&self) -> &[ScrubberGroup] {
        &self.groups
    }

    /// Ignored while no groups are installed — there is nothing to snap to.
    pub fn set_snap_to_groups(&mut self, snap: bool) {
        if self.snap_to_groups != snap {
            self.snap_to_groups = snap;
            self.dirty = true;
        }
    }

    pub fn snaps_to_groups(&self) -> bool {
        self.snap_to_groups && !self.groups.is_empty()
    }

    /// For streaming producers.
    pub fn entries_mut(&mut self) -> &mut Vec<HistoryEntry> {
        &mut self.entries
    }

    pub fn entries(&self) -> &[HistoryEntry] {
        &self.entries
    }

    /// For a host that reserves a band out of its own layout rather than
    /// putting the widget in a taffy tree — an editor that opens a history
    /// strip under its canvas, or a compositor pane.
    pub fn set_rect(&mut self, rect: Rect) {
        if self.rect != rect {
            self.rect = rect;
            self.dirty = true;
        }
    }

    pub fn rect(&self) -> Rect {
        self.rect
    }

    pub fn natural_height(&self) -> f64 {
        if self.groups.is_empty() {
            NATURAL_H_PLAIN
        } else {
            NATURAL_H_GROUPED
        }
    }

    /// A host that routes events by hit-test must keep forwarding to the
    /// scrubber while this is set, or a drag that leaves the strip stops
    /// tracking mid-gesture.
    pub fn is_dragging(&self) -> bool {
        self.is_dragging
    }

    /// A host that routes keys by hit-test must send them here regardless
    /// of where the pointer is.
    pub fn wants_keyboard(&self) -> bool {
        self.naming.is_some()
    }

    pub fn naming_buffer(&self) -> Option<&str> {
        self.naming.as_ref().map(|(_, f)| f.text())
    }

    #[must_use]
    pub fn on_preview<F: FnMut(&HistoryEntryId) + 'static>(mut self, f: F) -> Self {
        self.on_preview = Some(Box::new(f));
        self
    }

    #[must_use]
    pub fn on_restore<F: FnMut(&HistoryEntryId) + 'static>(mut self, f: F) -> Self {
        self.on_restore = Some(Box::new(f));
        self
    }

    #[must_use]
    pub fn on_name<F: FnMut(&GroupNameRequest) + 'static>(mut self, f: F) -> Self {
        self.on_name = Some(Box::new(f));
        self
    }

    #[must_use]
    pub fn on_version<F: FnMut(&VersionAction) + 'static>(mut self, f: F) -> Self {
        self.on_version = Some(Box::new(f));
        self
    }

    pub fn set_now_for_test(&mut self, now: SystemTime) {
        self.now_override = Some(now);
    }

    pub fn current_tick(&self) -> usize {
        self.current_tick
    }

    pub fn current_entry(&self) -> Option<&HistoryEntry> {
        self.entries.get(self.current_tick)
    }

    pub fn current_group(&self) -> Option<&ScrubberGroup> {
        self.groups
            .iter()
            .find(|g| self.current_tick >= g.start && self.current_tick <= g.end)
    }

    pub fn open_group(&self) -> Option<&ScrubberGroup> {
        self.groups.iter().find(|g| g.open)
    }

    /// Clamped to `[0, entries.len()-1]`.
    pub fn set_current_tick(&mut self, tick: usize) {
        let new_tick = if self.entries.is_empty() {
            0
        } else {
            tick.min(self.entries.len().saturating_sub(1))
        };
        if new_tick != self.current_tick {
            self.current_tick = new_tick;
            self.dirty = true;
        }
    }

    pub fn tick_count(&self) -> usize {
        self.entries.len()
    }

    fn track_y_from_top(&self) -> f64 {
        if self.groups.is_empty() {
            TRACK_Y_PLAIN
        } else {
            TRACK_Y_GROUPED
        }
    }

    pub fn track_rect(&self) -> Rect {
        let y = self.rect.y0 + self.track_y_from_top();
        Rect::new(
            self.rect.x0 + TRACK_INSET,
            y - TRACK_HEIGHT / 2.0,
            self.rect.x1 - TRACK_INSET,
            y + TRACK_HEIGHT / 2.0,
        )
    }

    /// Rect of the group band above the track. `Rect::ZERO` with no groups.
    pub fn band_rect(&self) -> Rect {
        if self.groups.is_empty() {
            return Rect::ZERO;
        }
        Rect::new(
            self.rect.x0 + TRACK_INSET,
            self.rect.y0 + BAND_Y_FROM_TOP,
            self.rect.x1 - TRACK_INSET,
            self.rect.y0 + BAND_Y_FROM_TOP + BAND_H,
        )
    }

    /// With a single entry, pins to the rightmost track position.
    pub fn tick_x(&self, i: usize) -> f64 {
        let track = self.track_rect();
        let n = self.entries.len();
        if n <= 1 {
            return track.x1;
        }
        let t = f64_of_usize(i) / f64_of_usize(n.saturating_sub(1));
        track.x0 + t * track.width()
    }

    /// Ends of segments (plus tick 0, so the beginning stays reachable), not
    /// starts — "the state at the end of *Intro rewrite*" is the version a
    /// person means when they point at a band.
    pub fn snap_targets(&self) -> Vec<usize> {
        if self.groups.is_empty() {
            return Vec::new();
        }
        let mut out: Vec<usize> = vec![0];
        for g in &self.groups {
            out.push(g.end);
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    pub fn tick_at_x(&self, x: f64) -> usize {
        let track = self.track_rect();
        let n = self.entries.len();
        if n <= 1 {
            return 0;
        }
        let t = ((x - track.x0) / track.width()).clamp(0.0, 1.0);
        let raw = tick_from_ratio(t, n.saturating_sub(1));
        if !self.snaps_to_groups() {
            return raw;
        }
        let targets = self.snap_targets();
        targets
            .iter()
            .copied()
            .min_by_key(|c| c.abs_diff(raw))
            .unwrap_or(raw)
    }

    pub fn handle_center(&self) -> Point {
        Point::new(self.tick_x(self.current_tick), self.track_rect().center().y)
    }

    /// **Below** the track: above is where the segment band and the popover
    /// live, and a marker drawn on top of either is a marker nobody can see.
    pub fn live_state_dot_center(&self) -> Point {
        Point::new(
            self.tick_x(self.entries.len().saturating_sub(1)),
            self.track_rect().center().y + 12.0,
        )
    }

    pub fn popover_rect(&self) -> Rect {
        let centre = self.handle_center();
        // Wide enough for a version name over a change description; the
        // renderer truncates whatever still overflows.
        let want: f64 = if self.groups.is_empty() { 220.0 } else { 260.0 };
        // Clamped to the same gutter as the track and the button row, so a
        // popover pushed to either end lines up with the strip's content
        // edge instead of overhanging it.
        let w = want.min(self.rect.width() - 2.0 * TRACK_INSET).max(80.0);
        let lo = self.rect.x0 + TRACK_INSET;
        let hi = (self.rect.x1 - TRACK_INSET - w).max(lo);
        let x0 = (centre.x - w / 2.0).clamp(lo, hi);
        // A clear gap between the popover and the band under it, so the
        // two read as separate things rather than one stacked block.
        let y1 = if self.groups.is_empty() {
            self.rect.y0 + 8.0 + POPOVER_H
        } else {
            self.band_rect().y0 - POPOVER_BAND_GAP
        };
        Rect::new(x0, y1 - POPOVER_H, x0 + w, y1)
    }

    /// Restore is always there. The rest are dropped from the low-priority
    /// end rather than allowed to overlap — a control half-under another
    /// control is worse than a control that is not offered at this width.
    fn button_layout(&self) -> Vec<(ScrubButton, Rect, &'static str)> {
        let track = self.track_rect();
        let y0 = track.y1 + BUTTON_Y_OFFSET;
        let narrow = self.rect.width() < 560.0;
        let very_narrow = self.rect.width() < 420.0;

        let restore_label = if very_narrow {
            "Restore"
        } else if narrow {
            "Restore version"
        } else {
            "Restore to this version"
        };
        let restore_w = if very_narrow {
            88.0
        } else if narrow {
            124.0
        } else {
            RESTORE_W
        };
        let restore_rect = Rect::new(
            self.rect.x1 - TRACK_INSET - restore_w,
            y0,
            self.rect.x1 - TRACK_INSET,
            y0 + BUTTON_H,
        );
        let mut out: Vec<(ScrubButton, Rect, &'static str)> =
            vec![(ScrubButton::Restore, restore_rect, restore_label)];

        // Preview keeps the leftmost slot so hosts that reserved that
        // geometry stay aligned.
        let mut left: Vec<(ScrubButton, f64, &'static str)> = vec![(
            ScrubButton::Preview,
            if very_narrow { 70.0 } else { PREVIEW_W },
            "Preview",
        )];
        if !self.groups.is_empty() {
            let named = self.current_group().is_some_and(|g| g.label.is_some());
            left.push((
                ScrubButton::Name,
                if narrow { 78.0 } else { 132.0 },
                match (named, narrow) {
                    (true, true) => "Rename",
                    (true, false) => "Rename this version",
                    (false, true) => "Name",
                    (false, false) => "Name this version",
                },
            ));
            let open = self.open_group().is_some();
            left.push((
                ScrubButton::Version,
                if narrow { 74.0 } else { 116.0 },
                match (open, narrow) {
                    (true, true) => "Finish",
                    (true, false) => "Finish version",
                    (false, true) => "Start",
                    (false, false) => "Start a version",
                },
            ));
        }

        let limit = restore_rect.x0 - 8.0;
        let mut x = self.rect.x0 + TRACK_INSET;
        for (kind, w, label) in left {
            if x + w > limit {
                continue;
            }
            out.push((kind, Rect::new(x, y0, x + w, y0 + BUTTON_H), label));
            x += w + 8.0;
        }
        out
    }

    fn button_rect(&self, which: ScrubButton) -> Option<Rect> {
        self.button_layout()
            .into_iter()
            .find(|(k, _, _)| *k == which)
            .map(|(_, r, _)| r)
    }

    /// `Rect::ZERO` when the strip is too narrow to offer it.
    pub fn preview_button_rect(&self) -> Rect {
        self.button_rect(ScrubButton::Preview).unwrap_or(Rect::ZERO)
    }

    pub fn restore_button_rect(&self) -> Rect {
        self.button_rect(ScrubButton::Restore).unwrap_or(Rect::ZERO)
    }

    /// `Rect::ZERO` when no groups are installed (nothing to name) or the
    /// strip is too narrow.
    pub fn name_button_rect(&self) -> Rect {
        self.button_rect(ScrubButton::Name).unwrap_or(Rect::ZERO)
    }

    pub fn version_button_rect(&self) -> Rect {
        self.button_rect(ScrubButton::Version).unwrap_or(Rect::ZERO)
    }

    /// The field grows **on top of** the button row rather than beside it.
    /// Paint and hit-test both read this one rect: a field drawn over
    /// `[Restore to this version]` while the button underneath still took
    /// the click would turn "click in the box to fix a typo" into a
    /// document rewrite.
    pub fn name_field_rect(&self) -> Rect {
        if self.naming.is_none() {
            return Rect::ZERO;
        }
        let layout = self.button_layout();
        let Some((_, first_button, _)) = layout.first() else {
            return Rect::ZERO;
        };
        let first = layout.iter().map(|(_, r, _)| r.x0).fold(f64::MAX, f64::min);
        let last = layout.iter().map(|(_, r, _)| r.x1).fold(0.0_f64, f64::max);
        let y = first_button.y0;
        Rect::new(
            first,
            y,
            first + (last - first).min(NAME_FIELD_W),
            y + BUTTON_H,
        )
    }

    fn now(&self) -> SystemTime {
        self.now_override.unwrap_or_else(crate::time::wall)
    }

    fn invoke_preview(&mut self) {
        let Some(entry) = self.entries.get(self.current_tick) else {
            return;
        };
        let id = entry.id;
        if let Some(cb) = self.on_preview.as_mut() {
            cb(&id);
        }
    }

    /// Redacted entries are non-restorable — "restore to this version" would
    /// re-introduce content the channel owner has explicitly removed by
    /// policy. The button is suppressed in the draw path; this is the
    /// data-layer defence so a future keyboard path that forgets to gate
    /// still cannot undo a redaction.
    fn invoke_restore(&mut self) {
        let Some(entry) = self.entries.get(self.current_tick) else {
            return;
        };
        if entry.redaction.is_some() {
            tracing::debug!("history_scrubber: refusing restore across a redaction boundary");
            return;
        }
        let id = entry.id;
        if let Some(cb) = self.on_restore.as_mut() {
            cb(&id);
        }
    }

    pub fn begin_naming_current_group(&mut self) -> bool {
        let Some(g) = self.current_group() else {
            return false;
        };
        let seed = g.label.clone().unwrap_or_default();
        self.naming = Some((NameKind::CurrentGroup, NameField::new(seed)));
        self.dirty = true;
        true
    }

    pub fn begin_new_version(&mut self) -> bool {
        if self.entries.is_empty() {
            return false;
        }
        self.naming = Some((
            NameKind::NewVersion,
            NameField::new("").with_placeholder("What are you about to do?"),
        ));
        self.dirty = true;
        true
    }

    pub fn cancel_naming(&mut self) {
        if self.naming.take().is_some() {
            self.dirty = true;
        }
    }

    /// Fires `on_name` for a rename, `on_version` for a version being opened.
    pub fn commit_naming(&mut self) -> bool {
        let Some((kind, field)) = self.naming.take() else {
            return false;
        };
        self.dirty = true;
        let label = field.text().trim().to_string();
        match kind {
            NameKind::CurrentGroup => {
                let Some(g) = self.current_group().cloned() else {
                    return false;
                };
                let (Some(first), Some(last)) =
                    (self.entries.get(g.start), self.entries.get(g.end))
                else {
                    return false;
                };
                if label.is_empty() && g.record_id.is_none() {
                    return false;
                }
                let req = GroupNameRequest {
                    record_id: g.record_id,
                    start: first.id,
                    end: last.id,
                    label,
                };
                if let Some(cb) = self.on_name.as_mut() {
                    cb(&req);
                }
                true
            }
            NameKind::NewVersion => {
                if label.is_empty() {
                    return false;
                }
                let Some(last) = self.entries.last() else {
                    return false;
                };
                let action = VersionAction::Open { at: last.id, label };
                if let Some(cb) = self.on_version.as_mut() {
                    cb(&action);
                }
                true
            }
        }
    }

    pub fn finish_open_version(&mut self) -> bool {
        let Some(g) = self.open_group().cloned() else {
            return false;
        };
        let Some(record_id) = g.record_id else {
            return false;
        };
        let Some(last) = self.entries.last() else {
            return false;
        };
        let action = VersionAction::Close {
            record_id,
            at: last.id,
        };
        if let Some(cb) = self.on_version.as_mut() {
            cb(&action);
        }
        self.dirty = true;
        true
    }

    fn draw_ticks(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let track = self.track_rect();
        let n = self.entries.len();
        if n == 0 {
            return;
        }
        let spacing = if n <= 1 {
            track.width()
        } else {
            track.width() / f64_of_usize(n.saturating_sub(1))
        };
        if spacing < MIN_TICK_SPACING {
            return;
        }
        let tick_color = theme.tokens.colors.text_dim;
        for i in 0..n {
            let x = self.tick_x(i);
            let mark = Rect::new(
                x - 0.5,
                track.center().y - TICK_MARK_H / 2.0,
                x + 0.5,
                track.center().y + TICK_MARK_H / 2.0,
            );
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                tick_color,
                None,
                &mark,
            );
        }
    }

    fn draw_band(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let band = self.band_rect();
        if band.width() <= 0.0 || self.groups.is_empty() {
            return;
        }
        let colors = &theme.tokens.colors;
        let track = self.track_rect();
        // `scale.xs` is the floor of the type scale; the band caption was
        // three points under `sm`, which landed below it.
        let style = theme.text(TextRole::Caption);

        // Segments abut at the tick each one ends on so every visible edge is
        // a boundary; spanning only first-tick..last-tick would leave a
        // gap between neighbours — 50 px on a twenty-change document.
        let mut cursor = band.x0;
        let last_index = self.groups.len().saturating_sub(1);
        for (i, g) in self.groups.iter().enumerate() {
            let x0 = cursor;
            let x1 = if i == last_index {
                band.x1
            } else {
                self.tick_x(g.end).clamp(x0, band.x1)
            };
            cursor = x1;
            let seg = Rect::new(x0, band.y0, x1.max(x0 + 2.0).min(band.x1), band.y1);
            if seg.width() <= 0.0 {
                continue;
            }
            let named = g.label.is_some();
            let contains_handle = self.current_tick >= g.start && self.current_tick <= g.end;
            // Unnamed segments step up the strip's own ramp; the declared
            // white veneers they replace were one fixed colour per theme.
            let fill = if named {
                if contains_handle {
                    colors.intent.inverted
                } else {
                    colors.intent.selection
                }
            } else if contains_handle {
                wash(colors.surfaces.raised, WASH_MID)
            } else if i % 2 == 0 {
                wash(colors.surfaces.raised, WASH_LOW)
            } else {
                wash(colors.surfaces.raised, WASH_FAINT)
            };
            let shape = RoundedRect::from_rect(seg, 3.0);
            scene.fill(vello::peniko::Fill::NonZero, transform, fill, None, &shape);

            // Every segment says what it is, even unnamed ones ("8 edits over
            // 2 min", in dim ink) — a blank grey block beside labelled ones
            // reads as a gap in the history rather than as a version. Only
            // shown when it fits: a two-character stub is worse
            // than the block on its own.
            let (label, ink) = match &g.label {
                Some(l) if contains_handle => (l.as_str(), colors.intent.on_inverted),
                Some(l) => (l.as_str(), colors.text_primary),
                None => (g.derived.as_str(), colors.text_dim),
            };
            let budget = seg.width() - 8.0;
            let shown = fit(tr, label, &style, budget);
            let (lw, lh) = tr.measure_style(&shown, None, &style);
            if !shown.is_empty() && f64::from(lw) <= budget && shown.chars().count() > 2 {
                tr.draw_style(
                    scene,
                    &shown,
                    ink,
                    translated(
                        transform,
                        (
                            seg.x0 + (seg.width() - f64::from(lw)) / 2.0,
                            seg.center().y - f64::from(lh) / 2.0,
                        ),
                    ),
                    None,
                    &style,
                );
            }

            // Boundary mark, joining the band to the track so a segment's end and
            // the tick it snaps to read as the same place.
            let bx = self.tick_x(g.end);
            scene.stroke(
                &Stroke::new(if named { 1.5 } else { 1.0 }),
                transform,
                if named {
                    colors.intent.inverted_hover
                } else {
                    wash(colors.surfaces.raised, WASH_MID)
                },
                None,
                &vello::kurbo::Line::new(
                    Point::new(bx, band.y1),
                    Point::new(bx, track.center().y - TICK_MARK_H / 2.0),
                ),
            );
        }
    }

    /// Used for every button so styling stays uniform.
    fn draw_button(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        label: &str,
        hovered: bool,
        primary: bool,
    ) {
        let radius = f64::from(theme.tokens.radii.detail);
        let shape = RoundedRect::from_rect(rect, radius);
        let colors = &theme.tokens.colors;
        // Primary: accent, ink chosen against it; the accent face is its own
        // contrast step, so no edge (HIG §9.5 "selected accent, no edge").
        // Neutral: the Button Neutral rest — the strip's raised fill under a
        // derived edge — and one step up the strip's ramp on hover.
        let (fill, border, text_color) = if primary {
            let fill = if hovered {
                colors.intent.inverted_hover
            } else {
                colors.intent.inverted
            };
            (fill, None, colors.intent.on_inverted)
        } else if hovered {
            let fill = wash(colors.surfaces.raised, WASH_MID);
            (
                fill,
                Some(crate::surface::edge_on(colors.surfaces.raised)),
                colors.text_primary,
            )
        } else {
            (
                colors.surfaces.raised,
                Some(crate::surface::edge_on(colors.surfaces.raised)),
                colors.text_primary,
            )
        };
        scene.fill(vello::peniko::Fill::NonZero, transform, fill, None, &shape);
        if let Some(border) = border {
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                border,
                None,
                &crate::surface::edge_outline(rect, radius),
            );
        }
        let style = theme.text(TextRole::Caption);
        let shown = fit(text_renderer, label, &style, rect.width() - 10.0);
        let (w, h) = text_renderer.measure_style(&shown, None, &style);
        text_renderer.draw_style(
            scene,
            &shown,
            text_color,
            translated(
                transform,
                (
                    rect.x0 + (rect.width() - f64::from(w)) / 2.0,
                    rect.y0 + (rect.height() - f64::from(h)) / 2.0,
                ),
            ),
            None,
            &style,
        );
    }

    /// The whole strip, before any entries have arrived.
    fn draw_empty_state(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let label = "No history yet";
        let style = theme.text(TextRole::Caption);
        let (w, h) = text_renderer.measure_style(label, None, &style);
        text_renderer.draw_style(
            scene,
            label,
            theme.tokens.colors.text_dim,
            translated(
                transform,
                (
                    f64::midpoint(self.rect.x0, self.rect.x1) - f64::from(w) / 2.0,
                    f64::midpoint(self.rect.y0, self.rect.y1) - f64::from(h) / 2.0,
                ),
            ),
            None,
            &style,
        );
    }

    /// The track, its band and ticks, the live-state dot and the drag handle.
    fn draw_track_and_handle(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let track = self.track_rect();
        let track_shape = RoundedRect::from_rect(track, 3.0);
        // The unfilled track is a step of the strip's own ramp, not a ramp
        // slot (`neutral[3]` is a shade the theme author never measured
        // against this fill), under a derived edge like a Slider's.
        let surface = theme.tokens.colors.surfaces.raised;
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            wash(surface, WASH_TRACK),
            None,
            &track_shape,
        );

        let active = Rect::new(track.x0, track.y0, self.handle_center().x, track.y1);
        if active.width() > 0.0 {
            let active_shape = RoundedRect::from_rect(active, 3.0);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.intent.inverted,
                None,
                &active_shape,
            );
        }

        self.draw_band(scene, transform, text_renderer, theme);
        self.draw_ticks(scene, transform, theme);

        // The track's edge goes on last, after the played range and the
        // ticks, so one outline encloses the whole track — played and
        // unplayed alike — instead of stopping where the accent begins.
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            crate::surface::edge_on(surface),
            None,
            &crate::surface::edge_outline(track, 3.0),
        );

        // The live state is marked only while the handle is away from it;
        // under the handle it is a stray dot beside the thumb.
        if self.current_tick + 1 < self.entries.len() {
            let live_dot = Circle::new(self.live_state_dot_center(), 3.0);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.intent.inverted,
                None,
                &live_dot,
            );
        }

        let handle = self.handle_center();
        let radius = if self.is_dragging {
            HANDLE_RADIUS * 1.15
        } else if self.is_hovered {
            HANDLE_RADIUS * 1.08
        } else {
            HANDLE_RADIUS
        };
        let handle_shape = Circle::new(handle, radius);
        let handle_fill = theme.tokens.colors.text_primary;
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            handle_fill,
            None,
            &handle_shape,
        );
        // The ring derives from the handle's own fill, so it seats a light
        // thumb on a dark strip and a dark thumb on a light one alike.
        scene.stroke(
            &Stroke::new(1.5),
            transform,
            crate::surface::edge_on(handle_fill),
            None,
            &handle_shape,
        );
    }

    /// The popover frame and, when there is a current entry, the text inside it.
    fn draw_popover(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let pop = self.popover_rect();
        // Overlay surface under an edge derived from it (HIG §9.5); the
        // white veneer it replaces was invisible on a light overlay.
        crate::surface::card(
            scene,
            transform,
            pop,
            f64::from(theme.tokens.radii.detail),
            theme.tokens.colors.surfaces.overlay,
        );
        let Some(entry) = self.current_entry() else {
            return;
        };
        let now = self.now();
        // The title names the version (or the change, outside one); the
        // caption under it says who and when, and which change within the
        // version — sentences, not a chain of fields.
        let by = (!entry.is_genesis()).then(|| {
            format!(
                "by {}, {}",
                entry.author.name,
                format_relative_time(entry.timestamp, now)
            )
        });
        // An unnamed segment of one change derives its title *from* that
        // change, so a two-line popover would print the same words twice —
        // once as the version and once as the change. Say it once.
        let group_line = self.current_group().and_then(|g| {
            let title = g.title().to_string();
            if g.label.is_none() && !g.open && title == entry.summary {
                return None;
            }
            Some(if g.open {
                format!("{title} (still open)")
            } else {
                title
            })
        });
        let detail = match (&group_line, by) {
            (Some(_), Some(by)) => format!("{} {by}", entry.summary),
            (Some(_) | None, None) => entry.summary.clone(),
            (None, Some(by)) => by,
        };
        let budget = pop.width() - 12.0;
        let head_style = theme.text(TextRole::Caption).strong();
        let sub_style = theme.text(TextRole::Caption);
        let colors = &theme.tokens.colors;
        // Each line is fitted to the width and measured at the size it is
        // painted with; the stack is then centred as one block on the
        // popover, each line on its own measured box rather than at two
        // hand-picked offsets.
        let mut lines: Vec<(String, &TextStyle, Color, f32, f32)> = Vec::new();
        let push = |text_renderer: &mut TextRenderer, text: &str, style: &TextStyle, ink: Color| {
            let drawn = fit(text_renderer, text, style, budget);
            let (w, h) = text_renderer.measure_style(&drawn, None, style);
            (drawn, w, h, ink)
        };
        let head = group_line.unwrap_or_else(|| entry.summary.clone());
        let (d, w, h, ink) = push(text_renderer, &head, &head_style, colors.text_primary);
        lines.push((d, &head_style, ink, w, h));
        if detail != head {
            let (d, w, h, ink) = push(text_renderer, &detail, &sub_style, colors.text_dim);
            lines.push((d, &sub_style, ink, w, h));
        }
        // The popover cannot grow — below it sit the band and the handle's
        // reach — so when the two-line stack no longer fits its box (2×
        // type), the change line yields and the version name stays; the
        // screen reader still gets both. Painting both would have pushed the
        // detail line down over the band.
        let stack_h = |lines: &[(String, &TextStyle, Color, f32, f32)]| -> f64 {
            lines.iter().map(|l| f64::from(l.4)).sum()
        };
        if lines.len() > 1 && stack_h(&lines) > pop.height() - POPOVER_PAD_Y * 2.0 {
            lines.truncate(1);
        }
        let mut top = pop.center().y - stack_h(&lines) / 2.0;
        for (drawn, style, ink, w, h) in &lines {
            text_renderer.draw_style(
                scene,
                drawn,
                *ink,
                translated(
                    transform,
                    (pop.x0 + (pop.width() - f64::from(*w)) / 2.0, top),
                ),
                None,
                style,
            );
            top += f64::from(*h);
        }
    }

    /// The naming field when a name is being typed, otherwise the button row.
    fn draw_controls(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        // A name being typed replaces the row it belongs to, so the field is
        // where the button was.
        let layout = self.button_layout();
        let redacted = self.current_entry().is_some_and(|e| e.redaction.is_some());
        if let Some((_, field)) = self.naming.as_ref() {
            let last = layout.iter().map(|(_, r, _)| r.x1).fold(0.0_f64, f64::max);
            let box_rect = self.name_field_rect();
            let style = theme.text(TextRole::Caption);
            field.draw(scene, transform, text_renderer, theme, box_rect, &style);
            // The field replaced the buttons, so nothing on screen says how to
            // finish. Enter and Escape are the only two things it does; say so
            // where the buttons were, when there is room left over.
            let hint = "Enter saves, Esc cancels";
            let hint_style = theme.text(TextRole::Caption);
            let (hw, hh) = text_renderer.measure_style(hint, None, &hint_style);
            if box_rect.x1 + 10.0 + f64::from(hw) <= last {
                text_renderer.draw_style(
                    scene,
                    hint,
                    theme.tokens.colors.text_dim,
                    translated(
                        transform,
                        (
                            box_rect.x1 + 10.0,
                            box_rect.center().y - f64::from(hh) / 2.0,
                        ),
                    ),
                    None,
                    &hint_style,
                );
            }
        } else {
            for (kind, rect, label) in &layout {
                // Restore is the primary action; and it is not offered at all
                // across a redaction boundary.
                if *kind == ScrubButton::Restore && redacted {
                    continue;
                }
                Self::draw_button(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    *rect,
                    label,
                    self.hover_button == Some(*kind),
                    *kind == ScrubButton::Restore,
                );
            }
        }
    }
}

impl Widget for HistoryScrubber {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let h = self.natural_height();
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "natural_height is one of two small compile-time constants (140.0 / 168.0), which round-trip exactly through f32"
                )]
                height: Dimension::length(h as f32),
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
        // The strip's own card: raised fill under an edge derived from it.
        // The declared `border_standard` this replaces is three shades from
        // the dark preset's raised surface and vanished there (HIG §9.5).
        crate::surface::card(
            scene,
            transform,
            self.rect,
            f64::from(theme.tokens.radii.control),
            theme.tokens.colors.surfaces.raised,
        );

        if self.entries.is_empty() {
            self.draw_empty_state(scene, transform, text_renderer, theme);
            self.dirty = false;
            return;
        }

        self.draw_track_and_handle(scene, transform, text_renderer, theme);
        self.draw_popover(scene, transform, text_renderer, theme);
        self.draw_controls(scene, transform, text_renderer, theme);

        self.dirty = false;
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if self.handle_naming_event(event, *ctx.modifiers, ctx.handled) {
            return;
        }

        // Empty scrubbers are inert.
        if self.entries.is_empty() {
            return;
        }

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.handle_cursor_moved(position, ctx.scale_factor, ctx.handled);
            }
            WindowEvent::MouseInput {
                state: element_state,
                button: MouseButton::Left,
                ..
            } => {
                if *element_state == ElementState::Pressed {
                    self.handle_mouse_press(ctx.focus_target, ctx.node_id, ctx.handled);
                } else if *element_state == ElementState::Released && self.is_dragging {
                    self.is_dragging = false;
                    self.dirty = true;
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }

    /// Drag must continue tracking the cursor even when it leaves `rect` —
    /// mirrors the slider's contract.
    fn wants_pointer_capture(&self) -> bool {
        self.is_dragging
    }

    fn name(&self) -> &'static str {
        "HistoryScrubber"
    }

    /// `Role::Slider` whose value names the version too when one is
    /// installed: "revision 84 of 190" tells a screen-reader user nothing
    /// they can act on, where "Intro rewrite, revision 84 of 190" does.
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let total = self.entries.len();
        let value = if total == 0 {
            "empty".to_string()
        } else {
            let pos = format!(
                "revision {} of {total}",
                self.current_tick.saturating_add(1)
            );
            match self.current_group() {
                Some(g) => format!("{}, {pos}", g.title()),
                None => pos,
            }
        };
        Some(
            AccessNode::new(Role::Slider)
                .with_name("History scrubber")
                .with_value(value)
                .with_state(AccessState::new().focusable(true)),
        )
    }

    fn test_id(&self) -> Option<&str> {
        Some("history_scrubber")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.naming
            .as_ref()
            .and_then(|(_, field)| field.ime_cursor_rect())
    }

    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        let (kind, field) = self.naming.as_ref()?;
        let scope = format!(
            "{}:{}",
            self.current_tick,
            match kind {
                NameKind::CurrentGroup => "group",
                NameKind::NewVersion => "version",
            }
        );
        Some(crate::ime::scoped_fingerprint(
            scope.as_bytes(),
            field.ime_edit_fingerprint(key)?,
            key,
        ))
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

impl HistoryScrubber {
    /// A name field owns the keyboard while it is up. Returns `true` when it
    /// consumed the event, meaning `handle_event` should not process it any
    /// further.
    fn handle_naming_event(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
        handled: &mut bool,
    ) -> bool {
        if !(self.naming.is_some()
            && matches!(
                event,
                WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
            ))
        {
            return false;
        }
        let outcome = self
            .naming
            .as_mut()
            .map_or(NameFieldOutcome::Ignored, |(_, f)| {
                f.handle_event(event, &modifiers)
            });
        match outcome {
            NameFieldOutcome::Commit(_) => {
                self.commit_naming();
                *handled = true;
            }
            NameFieldOutcome::Cancel => {
                self.cancel_naming();
                *handled = true;
            }
            NameFieldOutcome::Edited => {
                self.dirty = true;
                *handled = true;
            }
            NameFieldOutcome::Ignored => {}
        }
        true
    }

    fn handle_cursor_moved(
        &mut self,
        position: &winit::dpi::PhysicalPosition<f64>,
        scale_factor: f64,
        handled: &mut bool,
    ) {
        let p = Point::new(position.x / scale_factor, position.y / scale_factor);
        self.last_cursor = p;

        let handle = self.handle_center();
        let dx = p.x - handle.x;
        let dy = p.y - handle.y;
        let hovered = (dx * dx + dy * dy).sqrt() <= HANDLE_HIT_RADIUS;
        if hovered != self.is_hovered {
            self.is_hovered = hovered;
            self.dirty = true;
            *handled = true;
        }

        let over = self
            .button_layout()
            .into_iter()
            .find(|(_, r, _)| r.contains(p))
            .map(|(k, _, _)| k);
        if over != self.hover_button {
            self.hover_button = over;
            self.dirty = true;
            *handled = true;
        }

        if self.is_dragging {
            let tick = self.tick_at_x(p.x);
            if tick != self.current_tick {
                self.current_tick = tick;
                self.dirty = true;
                *handled = true;
            }
        }
    }

    /// A left-button press. Handles the naming field's click-outside-to-abandon
    /// behaviour, the button row, the drag handle and a click on the band or
    /// track.
    fn handle_mouse_press(
        &mut self,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        handled: &mut bool,
    ) {
        // The field is drawn over the button row, so buttons under it do not
        // get the click: pressing in the field does nothing (no caret to
        // place), pressing elsewhere on the strip abandons the name like
        // Escape does. A press outside the strip abandons it too but is left
        // for whatever the user actually clicked on.
        if self.naming.is_some() {
            let field = self.name_field_rect();
            if field.contains(self.last_cursor) {
                *handled = true;
                return;
            }
            self.cancel_naming();
            if self.rect.contains(self.last_cursor) {
                *handled = true;
            }
            return;
        }
        if let Some((kind, _, _)) = self
            .button_layout()
            .into_iter()
            .find(|(_, r, _)| r.contains(self.last_cursor))
        {
            match kind {
                ScrubButton::Preview => self.invoke_preview(),
                ScrubButton::Restore => self.invoke_restore(),
                ScrubButton::Name => {
                    self.begin_naming_current_group();
                }
                ScrubButton::Version => {
                    if self.open_group().is_some() {
                        self.finish_open_version();
                    } else {
                        self.begin_new_version();
                    }
                }
            }
            *handled = true;
            return;
        }

        let handle = self.handle_center();
        let dx = self.last_cursor.x - handle.x;
        let dy = self.last_cursor.y - handle.y;
        if (dx * dx + dy * dy).sqrt() <= HANDLE_HIT_RADIUS {
            self.is_dragging = true;
            *focus_target = Some(node_id);
            *handled = true;
            return;
        }

        // The band is part of the same control: clicking a version's block is
        // the most direct way to say "take me there".
        let track = self.track_rect();
        let band = self.band_rect();
        let on_track_y = (self.last_cursor.y - track.center().y).abs() <= TICK_MARK_H;
        let in_band = band.area() > 0.0 && band.contains(self.last_cursor);
        let on_track_x = self.last_cursor.x >= track.x0 && self.last_cursor.x <= track.x1;
        if on_track_x && (on_track_y || in_band) {
            // A click in a segment goes to that segment's end (the version it
            // names); a click on the track snaps normally.
            let tick = if in_band {
                let t = ((self.last_cursor.x - track.x0) / track.width()).clamp(0.0, 1.0);
                let raw = tick_from_ratio(t, self.entries.len().max(2).saturating_sub(1));
                self.groups
                    .iter()
                    .find(|g| raw >= g.start && raw <= g.end)
                    .map_or_else(|| self.tick_at_x(self.last_cursor.x), |g| g.end)
            } else {
                self.tick_at_x(self.last_cursor.x)
            };
            self.current_tick = tick.min(self.entries.len().saturating_sub(1));
            self.is_dragging = !in_band;
            *focus_target = Some(node_id);
            self.dirty = true;
            *handled = true;
        }
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic_input::{cursor_moved, mouse_press, mouse_release};
    use crate::testing::dispatch;
    use crate::time::Duration;
    use crate::widget::history::HistoryAuthor;
    use collab_types::{ArtifactKind, OpClass};
    use std::cell::RefCell;
    use std::rc::Rc;

    fn entry(id: u8) -> HistoryEntry {
        let mut bytes = [0u8; 16];
        bytes[0] = id;
        let secs = u64::from(id).saturating_mul(60);
        HistoryEntry {
            id: HistoryEntryId(bytes),
            author: HistoryAuthor::new("Alice", 120.0, "deadbeef"),
            op_class: OpClass::Splice,
            artifact_kind: ArtifactKind::Note,
            timestamp: SystemTime::UNIX_EPOCH
                .checked_add(Duration::from_secs(secs))
                .unwrap_or(SystemTime::UNIX_EPOCH),
            summary: format!("op {id}"),
            text_delta: Some((1, 0)),
            diff_payload: None,
            redaction: None,
            context: None,
        }
    }

    fn scrubber_at(rect: Rect, n: usize) -> HistoryScrubber {
        let entries: Vec<HistoryEntry> = (0..u8::try_from(n)
            .expect("test history size fits in u8"))
            .map(entry)
            .collect();
        let mut s = HistoryScrubber::new(entries);
        s.rect = rect;
        s.set_now_for_test(
            SystemTime::UNIX_EPOCH
                .checked_add(Duration::from_secs(3600))
                .unwrap_or(SystemTime::UNIX_EPOCH),
        );
        s
    }

    #[test]
    fn scrubber_track_drag_click_and_buttons_move_and_fire() {
        fn accessibility_reports_slider_role_and_revision_value() {
            use crate::a11y::Role;
            let mut s = scrubber_at(Rect::new(0.0, 0.0, 400.0, 40.0), 5);
            let node = Widget::accessibility(&s).expect("scrubber is not presentational");
            assert_eq!(node.role, Role::Slider);
            assert_eq!(node.name.as_deref(), Some("History scrubber"));
            assert_eq!(node.value.as_deref(), Some("revision 5 of 5"));
            assert!(node.state.focusable);

            s = scrubber_at(Rect::new(0.0, 0.0, 400.0, 40.0), 0);
            let node = Widget::accessibility(&s).unwrap();
            assert_eq!(node.value.as_deref(), Some("empty"));
        }

        fn handle_drag_advances_tick() {
            let mut s = scrubber_at(Rect::new(0.0, 0.0, 400.0, 140.0), 11);
            s.set_current_tick(0);
            let track = s.track_rect();
            let start_x = s.tick_x(0);

            let handle_y = track.center().y;
            dispatch(&mut s, &cursor_moved(start_x, handle_y));
            dispatch(&mut s, &mouse_press());
            assert!(s.is_dragging, "press on handle must start drag");

            let target_x = s.tick_x(5);
            dispatch(&mut s, &cursor_moved(target_x, handle_y));
            assert_eq!(
                s.current_tick(),
                5,
                "drag must advance tick (got {})",
                s.current_tick()
            );

            dispatch(&mut s, &mouse_release());
            assert!(!s.is_dragging, "release must end drag");
        }

        fn click_on_track_jumps_to_tick() {
            let mut s = scrubber_at(Rect::new(0.0, 0.0, 400.0, 140.0), 11);
            s.set_current_tick(0);
            let track = s.track_rect();
            let target = s.tick_x(7);

            dispatch(&mut s, &cursor_moved(target, track.center().y));
            dispatch(&mut s, &mouse_press());
            assert_eq!(s.current_tick(), 7);
            dispatch(&mut s, &mouse_release());
        }

        fn restore_callback_fires_on_button_click() {
            let mut s = scrubber_at(Rect::new(0.0, 0.0, 400.0, 140.0), 5);
            s.set_current_tick(2);
            let captured: Rc<RefCell<Option<HistoryEntryId>>> = Rc::new(RefCell::new(None));
            let captured_clone = captured.clone();
            s.on_restore = Some(Box::new(move |id| {
                *captured_clone.borrow_mut() = Some(*id);
            }));

            let btn = s.restore_button_rect();
            dispatch(&mut s, &cursor_moved(btn.center().x, btn.center().y));
            dispatch(&mut s, &mouse_press());

            let mut id_bytes = [0u8; 16];
            id_bytes[0] = 2;
            assert_eq!(*captured.borrow(), Some(HistoryEntryId(id_bytes)));
        }

        fn preview_callback_fires_on_button_click() {
            let mut s = scrubber_at(Rect::new(0.0, 0.0, 400.0, 140.0), 5);
            s.set_current_tick(3);
            let captured: Rc<RefCell<Option<HistoryEntryId>>> = Rc::new(RefCell::new(None));
            let captured_clone = captured.clone();
            s.on_preview = Some(Box::new(move |id| {
                *captured_clone.borrow_mut() = Some(*id);
            }));

            let btn = s.preview_button_rect();
            dispatch(&mut s, &cursor_moved(btn.center().x, btn.center().y));
            dispatch(&mut s, &mouse_press());

            let mut id_bytes = [0u8; 16];
            id_bytes[0] = 3;
            assert_eq!(*captured.borrow(), Some(HistoryEntryId(id_bytes)));
        }

        fn scrubber_with_no_entries_is_a_no_op() {
            let mut s = scrubber_at(Rect::new(0.0, 0.0, 400.0, 140.0), 0);
            assert_eq!(s.tick_count(), 0);
            assert_eq!(s.current_tick(), 0);
            dispatch(&mut s, &cursor_moved(100.0, 60.0));
            dispatch(&mut s, &mouse_press());
            dispatch(&mut s, &mouse_release());
            assert_eq!(s.current_tick(), 0);
            s.set_current_tick(999);
            assert_eq!(s.current_tick(), 0);
        }

        fn drag_saturates_at_endpoints() {
            // Important so a fast cursor swipe doesn't bypass the rightmost entry.
            let mut s = scrubber_at(Rect::new(0.0, 0.0, 400.0, 140.0), 11);
            s.set_current_tick(5);
            let track = s.track_rect();
            let start_x = s.tick_x(5);
            dispatch(&mut s, &cursor_moved(start_x, track.center().y));
            dispatch(&mut s, &mouse_press());
            dispatch(&mut s, &cursor_moved(10_000.0, track.center().y));
            assert_eq!(
                s.current_tick(),
                10,
                "drag past right must saturate at last tick"
            );
            dispatch(&mut s, &cursor_moved(-10_000.0, track.center().y));
            assert_eq!(
                s.current_tick(),
                0,
                "drag past left must saturate at tick 0"
            );
        }

        accessibility_reports_slider_role_and_revision_value();
        handle_drag_advances_tick();
        click_on_track_jumps_to_tick();
        restore_callback_fires_on_button_click();
        preview_callback_fires_on_button_click();
        scrubber_with_no_entries_is_a_no_op();
        drag_saturates_at_endpoints();
    }
}

// =====================================================================
// Tests: versions on the track
// =====================================================================

#[cfg(test)]
mod version_tests {
    use super::*;
    use crate::testing::dispatch;
    use crate::time::Duration;
    use crate::widget::history::HistoryAuthor;
    use collab_types::{ArtifactKind, OpClass};
    use std::cell::RefCell;
    use std::rc::Rc;

    fn entry(id: u8) -> HistoryEntry {
        let mut bytes = [0u8; 16];
        bytes[0] = id;
        let secs = u64::from(id).saturating_mul(60);
        let len = usize::from(id).saturating_add(1);
        HistoryEntry {
            id: HistoryEntryId(bytes),
            author: HistoryAuthor::new("Alice", 120.0, "deadbeef"),
            op_class: OpClass::Splice,
            artifact_kind: ArtifactKind::Note,
            timestamp: SystemTime::UNIX_EPOCH
                .checked_add(Duration::from_secs(secs))
                .unwrap_or(SystemTime::UNIX_EPOCH),
            summary: format!("+{len} -0 chars"),
            text_delta: Some((len, 0)),
            diff_payload: None,
            redaction: None,
            context: None,
        }
    }

    fn scrubber(w: f64, n: usize) -> HistoryScrubber {
        let entries: Vec<HistoryEntry> = (0..u8::try_from(n)
            .expect("test history size fits in u8"))
            .map(entry)
            .collect();
        let mut s = HistoryScrubber::new(entries);
        s.set_rect(Rect::new(0.0, 0.0, w, s.natural_height()));
        s.set_now_for_test(
            SystemTime::UNIX_EPOCH
                .checked_add(Duration::from_secs(100_000))
                .unwrap_or(SystemTime::UNIX_EPOCH),
        );
        s
    }

    fn group(start: usize, end: usize, label: Option<&str>) -> ScrubberGroup {
        ScrubberGroup {
            start,
            end,
            label: label.map(str::to_string),
            derived: format!("{} edits", end.saturating_add(1).saturating_sub(start)),
            record_id: label.map(|_| format!("tilo:group:{start}")),
            open: false,
        }
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

    /// `$UX1X_F4_OUT/<theme>/<name>` when the 1× review asked for evidence.
    fn ux1x_out(theme: &Theme, name: &str) -> Option<std::path::PathBuf> {
        let dir = std::path::PathBuf::from(std::env::var_os("UX1X_F4_OUT")?)
            .join(theme.name.to_lowercase());
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir.join(name))
    }

    #[test]
    fn version_strip_installs_snaps_and_navigates_versions() {
        /// The strip, its neutral buttons and its track keep a **visible derived
        /// edge** at 1× on both presets (HIG §9.5). Before, the strip wore
        /// `border_standard` (three shades from the dark raised surface), the
        /// buttons and segments literal white veneers, the track `neutral[3]`.
        ///
        /// Writes `$UX1X_F4_OUT/<theme>/history_scrubber.png` plus the harness
        /// inspection when that variable is set; asserts regardless.
        fn ux1x_strip_buttons_and_track_edges_register_on_both_presets() {
            use crate::offscreen::OffscreenRenderer;
            const EDGE_MIN: f64 = 0.008;
            let margin = 16.0;
            // Evidence first, for both presets, so a failing assertion on one
            // preset still leaves the other's PNG and inspection on disk.
            for theme in [Theme::dark(), Theme::light()] {
                let theme = theme.with_reduce_motion(true);
                if let Some(png) = ux1x_out(&theme, "history_scrubber.png") {
                    let mut mounted = with_versions(1000.0);
                    mounted.set_current_tick(19);
                    let h = mounted.natural_height() + 2.0 * margin;
                    #[allow(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "a small test viewport"
                    )]
                    let harness = crate::headless::FrameHarness::new_with_theme(
                        crate::State::new(),
                        Box::new(mounted),
                        (1000, h as u32),
                        theme.clone(),
                    );
                    harness.render_png(&png);
                    if let Ok(json) = harness.inspection_json() {
                        std::fs::write(png.with_extension("inspection.json"), json)
                            .expect("write inspection");
                    }
                }
            }
            for theme in [Theme::dark(), Theme::light()] {
                let theme = theme.with_reduce_motion(true);
                let mut s = with_versions(1000.0);
                s.set_current_tick(19);
                let strip = Rect::new(margin, margin, margin + 1000.0, margin + s.natural_height());
                s.set_rect(strip);
                let (w, h) = (strip.x1 + margin, strip.y1 + margin);
                let mut tr = TextRenderer::new();
                let mut scene = Scene::new();
                s.draw(
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

                // Strip edge against its own fill, on the track's centre line
                // (clear of the corner arcs; the track starts one `TRACK_INSET`
                // in, so the columns beside the edge are bare fill).
                let y = s.track_rect().center().y;
                let edge = lum(strip.x0, y);
                let fill = lum(strip.x0 + 3.0, y);
                assert!(
                    (edge - fill).abs() >= EDGE_MIN,
                    "{}: strip edge {edge:.4} vs fill {fill:.4} did not register",
                    theme.name
                );

                // Neutral button (Preview): its left edge against its face.
                let preview = s
                    .button_rect(ScrubButton::Preview)
                    .expect("the wide strip offers Preview");
                let bedge = lum(preview.x0, preview.center().y);
                let face = lum(preview.x0 + 3.0, preview.center().y);
                assert!(
                    (bedge - face).abs() >= EDGE_MIN,
                    "{}: button edge {bedge:.4} vs face {face:.4} did not register",
                    theme.name
                );

                // Track: the unfilled track does not exist at tick 19 (fully
                // played), so probe the band's odd segment against the strip.
                let band = s.band_rect();
                let seg = lum(band.x0 + 8.0, band.center().y + 6.0);
                let beside = lum(band.x0 + 8.0, band.y1 + 3.0);
                assert!(
                    (seg - beside).abs() >= EDGE_MIN,
                    "{}: band segment {seg:.4} vs strip {beside:.4} did not register",
                    theme.name
                );
            }
        }

        fn installing_versions_grows_the_strip_and_makes_room_for_the_band() {
            let plain = scrubber(900.0, 20);
            assert_eq!(plain.natural_height(), 140.0);
            assert_eq!(plain.band_rect(), Rect::ZERO, "no versions, no band");
            let plain_track_y = plain.track_rect().center().y;

            let s = with_versions(900.0);
            assert_eq!(s.natural_height(), 168.0);
            let band = s.band_rect();
            assert!(band.height() > 0.0, "the band is on screen");
            assert!(
                band.y1 < s.track_rect().y0,
                "and it sits above the track rather than over it"
            );
            assert!(
                s.track_rect().center().y > plain_track_y,
                "the track moved down to make room, it did not lose height"
            );
            assert!(
                s.restore_button_rect().y1 <= s.rect().y1,
                "the button row must stay inside the taller strip"
            );
        }

        fn dragging_snaps_to_version_boundaries() {
            let mut s = with_versions(900.0);
            assert!(s.snaps_to_groups());
            assert_eq!(s.snap_targets(), vec![0, 4, 9, 14, 19]);

            s.set_current_tick(0);
            let y = s.track_rect().center().y;
            let x0 = s.tick_x(0);
            dispatch(&mut s, &cursor_moved(x0, y));
            dispatch(&mut s, &mouse_press());
            assert!(s.is_dragging());

            let target = { s.tick_x(11) };
            dispatch(&mut s, &cursor_moved(target, y));
            assert_eq!(
                s.current_tick(),
                9,
                "snapped to the nearest version boundary, not to keystroke 11"
            );

            let target = { s.tick_x(13) };
            dispatch(&mut s, &cursor_moved(target, y));
            assert_eq!(s.current_tick(), 14);
            dispatch(&mut s, &mouse_release());
        }

        fn per_change_resolution_is_one_toggle_away() {
            let mut s = with_versions(900.0);
            s.set_snap_to_groups(false);
            assert!(!s.snaps_to_groups());
            s.set_current_tick(0);
            let y = s.track_rect().center().y;
            let (x0, x11) = (s.tick_x(0), s.tick_x(11));
            dispatch(&mut s, &cursor_moved(x0, y));
            dispatch(&mut s, &mouse_press());
            dispatch(&mut s, &cursor_moved(x11, y));
            assert_eq!(s.current_tick(), 11, "every change is reachable again");
        }

        fn a_strip_with_no_versions_snaps_to_nothing() {
            let mut s = scrubber(900.0, 20);
            assert!(!s.snaps_to_groups());
            assert!(s.snap_targets().is_empty());
            s.set_current_tick(0);
            let y = s.track_rect().center().y;
            let (x0, x7) = (s.tick_x(0), s.tick_x(7));
            dispatch(&mut s, &cursor_moved(x0, y));
            dispatch(&mut s, &mouse_press());
            dispatch(&mut s, &cursor_moved(x7, y));
            assert_eq!(s.current_tick(), 7);
        }

        fn clicking_a_version_block_goes_to_the_end_of_that_version() {
            let mut s = with_versions(900.0);
            s.set_current_tick(19);
            // The middle of the "Intro rewrite" block (changes 10..=14).
            let band = s.band_rect();
            let x = f64::midpoint(s.tick_x(10), s.tick_x(14));
            click(&mut s, Point::new(x, band.center().y));
            assert_eq!(
                s.current_tick(),
                14,
                "the version's end is the state that version produced"
            );
            assert_eq!(
                s.current_group().map(ScrubberGroup::title),
                Some("Intro rewrite"),
                "and the handle is inside it"
            );
            assert!(
                !s.is_dragging(),
                "a click on the band is a jump, not the start of a drag"
            );
        }

        fn an_empty_history_says_so_and_ignores_every_gesture() {
            let mut s = scrubber(900.0, 0);
            assert_eq!(s.tick_count(), 0);
            assert!(s.current_group().is_none());
            s.set_groups(vec![group(0, 3, Some("Impossible"))]);
            assert!(s.groups().is_empty(), "no changes, no segments");
            dispatch(&mut s, &cursor_moved(100.0, 60.0));
            dispatch(&mut s, &mouse_press());
            dispatch(&mut s, &mouse_release());
            assert_eq!(s.current_tick(), 0);
            assert!(!s.wants_keyboard());
        }

        ux1x_strip_buttons_and_track_edges_register_on_both_presets();
        installing_versions_grows_the_strip_and_makes_room_for_the_band();
        dragging_snaps_to_version_boundaries();
        per_change_resolution_is_one_toggle_away();
        a_strip_with_no_versions_snaps_to_nothing();
        clicking_a_version_block_goes_to_the_end_of_that_version();
        an_empty_history_says_so_and_ignores_every_gesture();
    }

    /// Twenty changes cut into four versions, two of them named.
    fn with_versions(w: f64) -> HistoryScrubber {
        let mut s = scrubber(w, 20);
        s.set_groups(vec![
            group(0, 4, Some("Outline")),
            group(5, 9, None),
            group(10, 14, Some("Intro rewrite")),
            group(15, 19, None),
        ]);
        // Re-place: installing versions changes the height the strip wants.
        s.set_rect(Rect::new(0.0, 0.0, w, s.natural_height()));
        s
    }

    use crate::synthetic_input::cursor_moved;

    use crate::synthetic_input::mouse_press;

    use crate::synthetic_input::mouse_release;

    fn key(k: winit::keyboard::Key) -> WindowEvent {
        crate::synthetic_input::keyboard_input(crate::synthetic_input::key_event(
            k,
            None,
            ElementState::Pressed,
        ))
    }

    #[test]
    fn naming_versions_opens_renames_or_abandons() {
        fn naming_field_forwards_ime_preedit_and_candidate_geometry() {
            let mut scrubber = with_versions(900.0);
            scrubber.set_current_tick(12);
            assert!(scrubber.begin_naming_current_group());
            let committed = scrubber.naming_buffer().unwrap().to_string();
            dispatch(
                &mut scrubber,
                &WindowEvent::Ime(winit::event::Ime::Preedit(
                    "漢".into(),
                    Some(("漢".len(), "漢".len())),
                )),
            );
            assert_eq!(scrubber.naming_buffer(), Some(committed.as_str()));

            let mut scene = Scene::new();
            let mut renderer = TextRenderer::new();
            Widget::draw(
                &mut scrubber,
                &mut scene,
                Affine::IDENTITY,
                &mut renderer,
                &Theme::dark(),
                &PresenceCtx::empty(),
            );
            let candidate = Widget::ime_cursor_rect(&scrubber)
                .expect("history wrapper forwards its naming field caret");
            assert!(scrubber.name_field_rect().contains(candidate.center()));
        }

        fn naming_the_current_version_emits_that_versions_whole_range() {
            let captured: Rc<RefCell<Vec<GroupNameRequest>>> = Rc::new(RefCell::new(Vec::new()));
            let c = captured.clone();
            let mut s = with_versions(900.0);
            s.on_name = Some(Box::new(move |r| c.borrow_mut().push(r.clone())));
            s.set_current_tick(7); // inside the unnamed 5..=9 run

            let btn = s.name_button_rect();
            assert!(btn.area() > 0.0, "the strip offers a name button");
            click(&mut s, btn.center());
            assert!(s.wants_keyboard());
            type_str(&mut s, "Second pass");
            dispatch(
                &mut s,
                &key(winit::keyboard::Key::Named(
                    winit::keyboard::NamedKey::Enter,
                )),
            );

            let reqs = captured.borrow();
            assert_eq!(reqs.len(), 1);
            let mut start = [0u8; 16];
            start[0] = 5;
            let mut end = [0u8; 16];
            end[0] = 9;
            assert_eq!(reqs[0].start, HistoryEntryId(start));
            assert_eq!(reqs[0].end, HistoryEntryId(end));
            assert_eq!(reqs[0].label, "Second pass");
            assert_eq!(reqs[0].record_id, None);
            assert!(!s.wants_keyboard());
        }

        fn renaming_a_named_version_addresses_its_existing_record() {
            let captured: Rc<RefCell<Vec<GroupNameRequest>>> = Rc::new(RefCell::new(Vec::new()));
            let c = captured.clone();
            let mut s = with_versions(900.0);
            s.on_name = Some(Box::new(move |r| c.borrow_mut().push(r.clone())));
            s.set_current_tick(12); // inside "Intro rewrite"

            let btn = s.name_button_rect();
            click(&mut s, btn.center());
            assert_eq!(
                s.naming_buffer(),
                Some("Intro rewrite"),
                "a rename starts from the current name"
            );
            type_str(&mut s, "!");
            dispatch(
                &mut s,
                &key(winit::keyboard::Key::Named(
                    winit::keyboard::NamedKey::Enter,
                )),
            );
            let reqs = captured.borrow();
            assert_eq!(reqs[0].label, "Intro rewrite!");
            assert_eq!(reqs[0].record_id.as_deref(), Some("tilo:group:10"));
        }

        fn starting_a_version_opens_one_and_finishing_closes_it() {
            let captured: Rc<RefCell<Vec<VersionAction>>> = Rc::new(RefCell::new(Vec::new()));
            let c = captured.clone();
            let mut s = with_versions(900.0);
            s.on_version = Some(Box::new(move |a| c.borrow_mut().push(a.clone())));

            let btn = s.version_button_rect();
            assert!(btn.area() > 0.0);
            click(&mut s, btn.center());
            assert!(s.wants_keyboard(), "it asks what you are about to do");
            type_str(&mut s, "Rework the intro");
            dispatch(
                &mut s,
                &key(winit::keyboard::Key::Named(
                    winit::keyboard::NamedKey::Enter,
                )),
            );
            {
                let acts = captured.borrow();
                assert_eq!(acts.len(), 1);
                let mut at = [0u8; 16];
                at[0] = 19;
                assert_eq!(
                    acts[0],
                    VersionAction::Open {
                        at: HistoryEntryId(at),
                        label: "Rework the intro".to_string()
                    },
                    "a prospective version starts at the newest change"
                );
            }

            // The host writes it and hands the groups back with one still open.
            let mut groups = s.groups().to_vec();
            groups.last_mut().unwrap().label = Some("Rework the intro".into());
            groups.last_mut().unwrap().record_id = Some("tilo:group:15".into());
            groups.last_mut().unwrap().open = true;
            s.set_groups(groups);
            assert!(s.open_group().is_some());

            let btn = s.version_button_rect();
            click(&mut s, btn.center());
            let acts = captured.borrow();
            assert_eq!(acts.len(), 2);
            let mut at = [0u8; 16];
            at[0] = 19;
            assert_eq!(
                acts[1],
                VersionAction::Close {
                    record_id: "tilo:group:15".to_string(),
                    at: HistoryEntryId(at)
                },
                "finishing closes the open one at the newest change — no typing needed"
            );
        }

        fn an_empty_name_neither_opens_a_version_nor_writes_one() {
            let names: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));
            let versions: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));
            let n = names.clone();
            let v = versions.clone();
            let mut s = with_versions(900.0);
            s.on_name = Some(Box::new(move |_| *n.borrow_mut() += 1));
            s.on_version = Some(Box::new(move |_| *v.borrow_mut() += 1));

            s.set_current_tick(7); // an unnamed run: nothing to remove
            let at = s.name_button_rect().center();
            click(&mut s, at);
            dispatch(
                &mut s,
                &key(winit::keyboard::Key::Named(
                    winit::keyboard::NamedKey::Enter,
                )),
            );
            assert_eq!(*names.borrow(), 0, "nothing to write and nothing to remove");

            let at = s.version_button_rect().center();
            click(&mut s, at);
            dispatch(
                &mut s,
                &key(winit::keyboard::Key::Named(
                    winit::keyboard::NamedKey::Enter,
                )),
            );
            assert_eq!(*versions.borrow(), 0, "an unnamed version is not a version");
        }

        fn escape_abandons_a_name_and_leaves_the_version_alone() {
            let names: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));
            let n = names.clone();
            let mut s = with_versions(900.0);
            s.on_name = Some(Box::new(move |_| *n.borrow_mut() += 1));
            s.set_current_tick(12); // inside "Intro rewrite"

            let at = s.name_button_rect().center();
            click(&mut s, at);
            type_str(&mut s, "half typed");
            assert_eq!(s.naming_buffer(), Some("Intro rewritehalf typed"));

            dispatch(
                &mut s,
                &key(winit::keyboard::Key::Named(
                    winit::keyboard::NamedKey::Escape,
                )),
            );
            assert!(!s.wants_keyboard(), "the field is gone");
            assert_eq!(*names.borrow(), 0, "and nothing was written");
            assert_eq!(
                s.current_group().and_then(|g| g.label.as_deref()),
                Some("Intro rewrite"),
                "the name it started from is untouched"
            );
        }

        naming_field_forwards_ime_preedit_and_candidate_geometry();
        naming_the_current_version_emits_that_versions_whole_range();
        renaming_a_named_version_addresses_its_existing_record();
        starting_a_version_opens_one_and_finishing_closes_it();
        an_empty_name_neither_opens_a_version_nor_writes_one();
        escape_abandons_a_name_and_leaves_the_version_alone();
    }

    fn type_str(s: &mut HistoryScrubber, text: &str) {
        use winit::keyboard::{Key, NamedKey};
        for ch in text.chars() {
            if ch == ' ' {
                dispatch(s, &key(Key::Named(NamedKey::Space)));
            } else {
                dispatch(s, &key(Key::Character(ch.to_string().into())));
            }
        }
    }

    fn click(s: &mut HistoryScrubber, p: Point) {
        dispatch(s, &cursor_moved(p.x, p.y));
        dispatch(s, &mouse_press());
    }

    #[test]
    fn version_segments_and_names_describe_real_changes() {
        fn the_popover_and_the_screen_reader_both_name_the_version() {
            let mut s = with_versions(900.0);
            s.set_current_tick(12);
            assert_eq!(
                s.current_group().map(ScrubberGroup::title),
                Some("Intro rewrite")
            );
            let node = Widget::accessibility(&s).unwrap();
            assert_eq!(
                node.value.as_deref(),
                Some("Intro rewrite, revision 13 of 20"),
                "'revision 13 of 20' alone is not something a user can act on"
            );

            // An unnamed version falls back to its derived description.
            s.set_current_tick(7);
            let node = Widget::accessibility(&s).unwrap();
            assert_eq!(node.value.as_deref(), Some("5 edits, revision 8 of 20"));
        }

        fn a_segment_that_does_not_describe_real_changes_is_dropped() {
            let mut s = scrubber(900.0, 10);
            s.set_groups(vec![
                group(0, 4, Some("Real")),
                group(8, 40, Some("Runs past the end")),
                group(6, 5, Some("Backwards")),
            ]);
            let titles: Vec<&str> = s.groups().iter().map(ScrubberGroup::title).collect();
            assert_eq!(titles, vec!["Real"]);
        }

        fn a_one_change_version_still_gets_a_visible_block() {
            let mut s = scrubber(900.0, 20);
            s.set_groups(vec![group(7, 7, Some("One change"))]);
            let g = &s.groups()[0];
            assert_eq!((g.start, g.end), (7, 7));
            assert_eq!(s.tick_x(7), s.tick_x(g.end));
            assert_eq!(s.snap_targets(), vec![0, 7]);
        }

        fn a_version_nobody_named_still_says_what_it_is() {
            let mut s = with_versions(900.0);
            let unnamed = segment_interior(&s, 1); // the 5..=9 run, which nobody named
            assert!(
                unnamed.width() > 60.0,
                "the fixture's block has room: {unnamed:?}"
            );
            assert!(
                ink_in(&mut s, unnamed) > 0,
                "the unnamed block is blank grey"
            );

            // It is the segment's own text: blank the description and
            // the same block goes back to being a flat fill.
            let mut groups = s.groups().to_vec();
            groups[1].derived = String::new();
            s.set_groups(groups);
            assert_eq!(
                ink_in(&mut s, unnamed),
                0,
                "something other than `derived` is being drawn in there"
            );
        }

        fn a_dense_track_drops_the_per_change_notches_rather_than_smearing() {
            // 200 changes across 900 px is a notch every 4.4 px — a grey texture,
            // not marks. Asserts on the same predicate the paint path branches on.
            let dense = scrubber(900.0, 200);
            let track = dense.track_rect();
            let spacing = track.width() / 199.0;
            assert!(spacing < MIN_TICK_SPACING, "spacing is {spacing}");

            let sparse = scrubber(900.0, 20);
            let spacing = sparse.track_rect().width() / 19.0;
            assert!(spacing >= MIN_TICK_SPACING, "spacing is {spacing}");
        }

        the_popover_and_the_screen_reader_both_name_the_version();
        a_segment_that_does_not_describe_real_changes_is_dropped();
        a_one_change_version_still_gets_a_visible_block();
        a_version_nobody_named_still_says_what_it_is();
        a_dense_track_drops_the_per_change_notches_rather_than_smearing();
    }

    #[test]
    fn button_row_and_name_field_never_overlap_their_targets() {
        fn the_button_row_drops_buttons_rather_than_overlapping_them() {
            for w in [900.0, 620.0, 520.0, 420.0, 360.0, 300.0] {
                let s = with_versions(w);
                let mut rects: Vec<Rect> = vec![
                    s.restore_button_rect(),
                    s.preview_button_rect(),
                    s.name_button_rect(),
                    s.version_button_rect(),
                ]
                .into_iter()
                .filter(|r| r.area() > 0.0)
                .collect();
                assert!(
                    !rects.is_empty(),
                    "at {w} px the strip must still offer something"
                );
                assert!(
                    s.restore_button_rect().area() > 0.0,
                    "restore is the primary action and is never dropped ({w} px)"
                );
                for r in &rects {
                    assert!(
                        r.x0 >= s.rect().x0 && r.x1 <= s.rect().x1,
                        "a button escaped the strip at {w} px: {r:?}"
                    );
                }
                rects.sort_by(|a, b| a.x0.partial_cmp(&b.x0).unwrap());
                for pair in rects.windows(2) {
                    assert!(
                        pair[0].x1 <= pair[1].x0,
                        "buttons overlap at {w} px: {:?} then {:?}",
                        pair[0],
                        pair[1]
                    );
                }
            }
        }

        fn a_click_in_the_name_field_cannot_reach_the_button_underneath_it() {
            let restores: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));
            let versions: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));
            let r = restores.clone();
            let v = versions.clone();
            let mut s = with_versions(900.0);
            s.on_restore = Some(Box::new(move |_| *r.borrow_mut() += 1));
            s.on_version = Some(Box::new(move |_| *v.borrow_mut() += 1));
            s.set_current_tick(12);

            let at = s.name_button_rect().center();
            click(&mut s, at);
            type_str(&mut s, "Intro");
            let field = s.name_field_rect();
            assert!(field.area() > 0.0, "the field has a rect while it is up");

            for btn in [
                s.preview_button_rect(),
                s.name_button_rect(),
                s.version_button_rect(),
            ] {
                if btn.area() > 0.0 && field.contains(btn.center()) {
                    click(&mut s, btn.center());
                    assert!(
                        s.wants_keyboard(),
                        "the field was dismissed by a click inside it"
                    );
                    assert_eq!(
                        s.naming_buffer(),
                        Some("Intro rewriteIntro"),
                        "a button under the field restarted the name"
                    );
                }
            }
            assert_eq!(*versions.borrow(), 0, "no version was opened");

            let restore = s.restore_button_rect();
            assert!(!field.contains(restore.center()));
            click(&mut s, restore.center());
            assert!(!s.wants_keyboard(), "clicking off the field abandons it");
            assert_eq!(
                *restores.borrow(),
                0,
                "the click that dismissed the field must not also restore"
            );

            let at = s.restore_button_rect().center();
            click(&mut s, at);
            assert_eq!(*restores.borrow(), 1);
        }

        fn the_name_field_is_a_label_box_not_the_whole_button_row() {
            let mut s = with_versions(1000.0);
            s.set_current_tick(12);
            let at = s.name_button_rect().center();
            click(&mut s, at);
            let field = s.name_field_rect();
            assert!(field.width() <= NAME_FIELD_W, "got {}", field.width());
            assert!(
                field.x1 < s.restore_button_rect().x0,
                "the field must not reach the destructive button: {field:?}"
            );
        }

        fn the_live_state_dot_does_not_hide_under_the_band_or_the_popover() {
            let s = with_versions(900.0);
            let dot_y = s.live_state_dot_center().y;
            assert!(
                dot_y > s.band_rect().y1,
                "the dot is inside the segment band"
            );
            assert!(dot_y > s.popover_rect().y1, "the dot is behind the popover");
            assert!(
                dot_y < s.restore_button_rect().y0,
                "the dot collides with the button row"
            );
        }

        the_button_row_drops_buttons_rather_than_overlapping_them();
        a_click_in_the_name_field_cannot_reach_the_button_underneath_it();
        the_name_field_is_a_label_box_not_the_whole_button_row();
        the_live_state_dot_does_not_hide_under_the_band_or_the_popover();
    }

    /// Rounds a rect coordinate into a device-pixel index for offscreen
    /// sampling. Test geometry here is always non-negative and far below
    /// `u32::MAX`, so this never loses the sign or truncates in practice.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "test rect coordinates are non-negative and well under u32::MAX"
    )]
    fn px_u32(v: f64) -> u32 {
        v.round() as u32
    }

    /// Render the strip and report how many pixels inside `r` differ from the
    /// pixel in its top-left corner. A flat fill scores 0; ink drawn on top of it
    /// scores. Used to assert that a label reached the scene, rather than that a
    /// helper returned a string the paint path might not use.
    fn ink_in(scrubber: &mut HistoryScrubber, r: Rect) -> usize {
        let width = px_u32(scrubber.rect().width());
        let height = px_u32(scrubber.rect().height());
        let theme = Theme::dark();
        let mut scene = Scene::new();
        let mut tr = TextRenderer::new();
        scrubber.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &PresenceCtx::empty(),
        );
        let rgba = crate::offscreen::OffscreenRenderer::new(width, height)
            .render(&scene, theme.tokens.colors.surfaces.base);
        let base =
            crate::offscreen::OffscreenRenderer::sample(&rgba, width, px_u32(r.x0), px_u32(r.y0));
        let mut diff: usize = 0;
        for y in px_u32(r.y0)..px_u32(r.y1).min(height) {
            for x in px_u32(r.x0)..px_u32(r.x1).min(width) {
                if crate::offscreen::OffscreenRenderer::sample(&rgba, width, x, y) != base {
                    diff = diff.saturating_add(1);
                }
            }
        }
        diff
    }

    /// The interior of segment `i`'s block, inset clear of the boundary strokes
    /// on either edge.
    fn segment_interior(s: &HistoryScrubber, i: usize) -> Rect {
        let band = s.band_rect();
        let x0 = if i == 0 {
            band.x0
        } else {
            s.tick_x(s.groups()[i.saturating_sub(1)].end)
        };
        let x1 = if i.saturating_add(1) == s.groups().len() {
            band.x1
        } else {
            s.tick_x(s.groups()[i].end)
        };
        Rect::new(x0 + 5.0, band.y0 + 3.0, x1 - 5.0, band.y1 - 3.0)
    }

    #[test]
    fn popover_stays_inside_the_track_at_every_width_and_scale() {
        /// The popover is clamped to the same gutter as the track and the
        /// button row, so pushed to either end it lines up with the strip's
        /// content edge instead of overhanging it by the old 8 px.
        fn popover_never_overhangs_the_track_gutter() {
            let mut s = scrubber(1000.0, 20);
            let track = s.track_rect();

            s.set_current_tick(19);
            let at_end = s.popover_rect();
            assert!(
                at_end.x1 <= track.x1 + 1e-6,
                "popover right edge {} overhangs the track end {}",
                at_end.x1,
                track.x1
            );

            s.set_current_tick(0);
            let at_start = s.popover_rect();
            assert!(
                at_start.x0 >= track.x0 - 1e-6,
                "popover left edge {} overhangs the track start {}",
                at_start.x0,
                track.x0
            );
        }

        /// The popover cannot grow (the band and the handle sit right under
        /// it), so at 2× the two-line stack yields its change line and the
        /// version name stays inside the box — before, the detail line ran
        /// down over the band. At 1× both lines still paint, inside.
        fn popover_text_stays_inside_its_box_at_every_text_scale() {
            for (scale, expected_lines) in [(1.0_f32, 2_usize), (2.0, 1)] {
                // A matched host: the theme's type steps carry the scale the
                // renderer is set to, so the glyphs paint at 2x, not 4x (X1).
                let theme = Theme::dark().with_text_scale(scale);
                let mut s = scrubber(1000.0, 20);
                s.set_groups(vec![
                    group(0, 9, Some("Costs section")),
                    group(10, 19, Some("Headcount pass")),
                ]);
                s.set_current_tick(15);
                let mut tr = TextRenderer::new();
                tr.set_text_scale(scale);
                let mut scene = Scene::new();
                s.draw_popover(&mut scene, Affine::IDENTITY, &mut tr, &theme);
                assert_eq!(
                    scene.encoding().resources.glyph_runs.len(),
                    expected_lines,
                    "at {scale}x the popover painted the wrong number of lines"
                );
                let pop = s.popover_rect();
                let (w, h) = (1000_u32, 200_u32);
                let rgba =
                    crate::offscreen::OffscreenRenderer::new(w, h).render(&scene, Color::BLACK);
                let mut ink_rows = Vec::new();
                for y in 0..h as usize {
                    let row = &rgba[y * w as usize * 4..(y + 1) * w as usize * 4];
                    if row
                        .chunks_exact(4)
                        .any(|px| px[0] > 160 && px[1] > 160 && px[2] > 160)
                    {
                        ink_rows.push(y);
                    }
                }
                let (top, bottom) = (
                    *ink_rows.first().expect("the popover painted a label"),
                    *ink_rows.last().expect("the popover painted a label"),
                );
                assert!(
                    f64::from(top as u32) >= pop.y0.floor()
                        && f64::from(bottom as u32) < pop.y1.ceil(),
                    "at {scale}x label ink rows {top}..={bottom} leave the popover {pop:?}"
                );
            }
        }

        /// A strip narrower than its own gutters plus the popover's floor width
        /// must degrade, not panic in `clamp`.
        fn popover_survives_a_strip_narrower_than_its_floor_width() {
            let s = scrubber(60.0, 2);
            let pop = s.popover_rect();
            assert!(pop.width() > 0.0);
            assert!((pop.x0 - TRACK_INSET).abs() < 1e-6);
        }

        popover_never_overhangs_the_track_gutter();
        popover_text_stays_inside_its_box_at_every_text_scale();
        popover_survives_a_strip_narrower_than_its_floor_width();
    }
}
