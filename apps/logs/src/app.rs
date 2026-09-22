//! `LogsApp` — the root widget for the Logs viewer.
//!
//! A graphical systemd-journal viewer. It is a single self-rendering
//! [`Widget`] (the data is read-only and the visuals — severity dots,
//! coloured rows, a detail pane, and a filter bar — are drawn directly into
//! the Vello scene. Selection, filters, and search are local view state. Live tail is a local effect that
//! controls this process's journal poller; journal entries themselves are an
//! external render cache.
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

use ui::a11y::{AccessNode, AccessState, Action as AccessAction, Politeness, Role};
use ui::portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use ui::tilo_actions::{ActionAddress, ControlInvocation, OperationKind, Resource};
use ui::widget::{TextInput, Widget};

use crate::actions::{self, LogsOperation};
use crate::doc::LogsDoc;
use crate::model::{Filter, LogEntry, Priority};
use crate::responsive::{Pane, PushNav};
use crate::theme_ext::{ThemeColors, tint};

const PAD: f64 = 16.0;
const HEADER_H: f64 = 60.0;
const FILTER_H: f64 = 64.0;
/// Below this width the filters wrap even though the journal can still keep
/// its list and detail panes docked. The single desktop row needs 1,072px of
/// controls plus outer padding, so waiting for the 760px push breakpoint
/// would clip the severity choices on medium-width windows.
const FILTER_WRAP_BREAKPOINT: f64 = 1_100.0;
const ROW_H: f64 = 30.0;
const LIST_HEADER_H: f64 = 26.0;
const CONTROL_H: f64 = 34.0;
const DROPDOWN_ITEM_H: f64 = 30.0;
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// An `Instant` at least `POLL_INTERVAL` in the past, so writing it into
/// `last_poll` makes the next poll-due check fire immediately. Falls back to
/// `now` itself in the vanishingly rare case the process's monotonic clock
/// has less than `POLL_INTERVAL` of history — that only delays the forced
/// poll by at most `POLL_INTERVAL`, never panics.
fn force_immediate_poll_at() -> Instant {
    let now = Instant::now();
    now.checked_sub(POLL_INTERVAL).unwrap_or(now)
}

/// Which transient popup is open, if any.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Popup {
    Unit,
    Boot,
    Actions,
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
    refresh_btn: Rect,
    overflow_trigger: Rect,
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

/// Running layout cursor threaded through the detail pane's draw helpers.
struct DetailLayout {
    inner_x: f64,
    inner_w: f64,
    y: f64,
    /// The lowest y the field table may draw into before it stops.
    bottom: f64,
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
    /// Stable selected-entry resource id. This survives reordering within the
    /// local journal cache without becoming a collaborator-visible cursor.
    selected_entry: Option<String>,
    live_tail: bool,
    /// A safe classification of the most recent local journal read failure.
    /// Never holds `journalctl` stderr or raw journal content.
    source_error: Option<crate::journal::JournalLoadError>,
    /// A short local status while a resource-routed refresh is being admitted
    /// or completed. Existing rows stay visible; an empty source gets an
    /// honest loading state rather than masquerading as empty.
    source_loading: bool,
    /// Host Save portal receiver plus the immutable filtered snapshot that
    /// was current when export was requested. Neither the chosen path nor log
    /// content is published through resources or collaboration state.
    export_rx: Option<PortalChannel>,
    pending_export: Option<Vec<LogEntry>>,
    #[cfg(test)]
    controlled_export_portal: Option<Box<dyn Fn(PickRequest) -> PortalChannel>>,
    /// Concise local outcome for the portal write, announced through an a11y
    /// live region. It never includes a chosen path or log contents.
    export_status: Option<String>,

    // ---- ephemeral UI state ----
    rect: Rect,
    node: Option<NodeId>,
    scroll: f32,
    cursor: Point,
    hover_row: Option<usize>,
    popup: Option<Popup>,
    focused: bool,
    /// Local search field. The filter bar keeps its app-specific layout while
    /// text editing remains toolkit-owned.
    search_input: TextInput,
    /// Pointer/keyboard controls publish inert resource addresses; the host
    /// re-resolves current availability and permissions before effects run.
    pending_control_invocation: Option<ControlInvocation>,
    /// One-shot deterministic local view seeding used only by the offscreen
    /// shot factory.
    shot_filter: Option<Filter>,
    dirty: bool,
    /// Text-derived band pitches for this frame (see [`Metrics`]).
    m: Metrics,

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
            selected_entry: None,
            live_tail: false,
            source_error: None,
            source_loading: false,
            export_rx: None,
            pending_export: None,
            #[cfg(test)]
            controlled_export_portal: None,
            export_status: None,
            rect: Rect::ZERO,
            node: None,
            scroll: 0.0,
            cursor: Point::ZERO,
            hover_row: None,
            popup: None,
            focused: true,
            search_input: TextInput::local()
                .with_kind(ui::widget::TextInputKind::Search)
                .with_id("logs.search")
                .with_placeholder("Search messages, units…")
                .with_plain_chrome(),
            pending_control_invocation: None,
            shot_filter: None,
            dirty: true,
            m: Metrics::ONE,
            push: PushNav::default(),
            allow_live,
            last_poll: Instant::now(),
        };
        app.rebuild_facets();
        app
    }

    /// Construct a live-capable viewer whose initial read failed. Keeping the
    /// capability enabled means Refresh remains available as a recovery path;
    /// the error itself is safe, local-only presentation state.
    pub fn new_with_source_error(
        entries: Vec<LogEntry>,
        allow_live: bool,
        error: crate::journal::JournalLoadError,
    ) -> Self {
        let mut app = Self::new(entries, allow_live);
        app.source_error = Some(error);
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

    /// Seed an errors-and-worse state for deterministic visual regression
    /// captures. The value is written into the document on the first sync.
    pub fn shots_filter_errors(&mut self) {
        self.shot_filter = Some(Filter {
            min_priority: Priority::Err.level(),
            ..Filter::default()
        });
        self.dirty = true;
    }

    /// Deterministically capture the empty-source loading state.
    pub fn shots_show_loading(&mut self) {
        self.source_loading = true;
        self.dirty = true;
    }

    /// Deterministically open the compact actions menu for phone captures.
    pub fn shots_show_actions(&mut self) {
        self.popup = Some(Popup::Actions);
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
        // A stable resource selection is explicit even when this replica has
        // not received that record yet; never substitute an unrelated auto
        // selection for a collaborator's chosen entry.
        if self.selected_entry.is_some() {
            let selected = usize::try_from(self.sel_raw).ok()?;
            return self.filtered.contains(&selected).then_some(selected);
        }
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
        self.filtered.len() as f64 * self.m.row_h
    }

    fn clamp_scroll(&mut self) {
        let max = (self.content_h() - self.list_viewport_h()).max(0.0) as f32;
        self.scroll = self.scroll.clamp(0.0, max);
    }

    /// Scroll so the filtered position `pos` is fully visible.
    fn scroll_into_view(&mut self, pos: usize) {
        let body_h = self.list_viewport_h();
        let top = pos as f64 * self.m.row_h;
        let bottom = top + self.m.row_h;
        if (top as f32) < self.scroll {
            self.scroll = top as f32;
        } else if bottom as f32 > self.scroll + body_h as f32 {
            self.scroll = (bottom - body_h) as f32;
        }
        self.clamp_scroll();
    }

    /// Apply one local view operation. Filters and selection intentionally
    /// never cross the Automerge boundary: they describe one person's current
    /// investigation, not a collaborative journal annotation.
    fn apply_view_operation(&mut self, operation: LogsOperation) {
        match operation {
            LogsOperation::SetPriority { value } => self.filter.min_priority = value.clamp(0, 7),
            LogsOperation::SetUnit { value } => self.filter.unit = value,
            LogsOperation::SetBoot { value } => self.filter.boot = value,
            LogsOperation::SetSearch { value } => {
                self.filter.search = value;
                self.search_input.set_text(&self.filter.search);
            }
            LogsOperation::ClearFilters => {
                self.filter = Filter::default();
                self.search_input.set_text("");
            }
            LogsOperation::SelectEntry { entry_id } => {
                self.selected_entry = Some(entry_id.clone());
                self.sel_raw = self
                    .entries
                    .iter()
                    .position(|entry| actions::entry_resource_id(entry) == entry_id)
                    .and_then(|index| i64::try_from(index).ok())
                    .unwrap_or(-1);
            }
            LogsOperation::Refresh
            | LogsOperation::SetLiveTail { .. }
            | LogsOperation::ExportFiltered => return,
        }
        self.recompute_filtered();
        self.clamp_scroll();
        self.dirty = true;
    }

    /// Local journal operations. They intentionally do not enter the shared
    /// document: polling a machine's journal is a device-local capability and
    /// one peer's live-tail preference must not start work on every peer.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "Decoded operations are consumed exactly once at the app boundary."
    )]
    fn perform_effect(&mut self, operation: actions::LogsOperation) -> Result<(), String> {
        match operation {
            operation @ (LogsOperation::SetPriority { .. }
            | LogsOperation::SetUnit { .. }
            | LogsOperation::SetBoot { .. }
            | LogsOperation::SetSearch { .. }
            | LogsOperation::ClearFilters
            | LogsOperation::SelectEntry { .. }) => self.apply_view_operation(operation),
            actions::LogsOperation::Refresh => {
                self.source_loading = false;
                if self.allow_live {
                    match crate::journal::load_live(2000) {
                        Ok(entries) => {
                            self.entries = entries;
                            self.source_error = None;
                            self.rebuild_facets();
                        }
                        Err(error) => self.source_error = Some(error),
                    }
                }
            }
            actions::LogsOperation::SetLiveTail { enabled } => {
                self.live_tail = enabled;
                self.last_poll = force_immediate_poll_at();
            }
            actions::LogsOperation::ExportFiltered => self.request_export()?,
        }
        self.dirty = true;
        Ok(())
    }

    /// Open the host's Save dialog with an immutable filtered snapshot. The
    /// portal is the only source of the destination path, so neither UI nor
    /// agents can point the strict sandbox at an arbitrary filesystem path.
    fn request_export(&mut self) -> Result<(), String> {
        if self.export_rx.is_some() {
            return Err("an export dialog is already open".into());
        }
        let entries = self
            .entries
            .iter()
            .filter(|entry| self.filter.passes(entry))
            .cloned()
            .collect::<Vec<_>>();
        if entries.is_empty() {
            return Err("there are no filtered entries to export".into());
        }
        let request = PickRequest::save("Export filtered logs", "Export", "journal-filtered.jsonl")
            .thread_name("logs-portal-export")
            .filter(
                FileFilter::new("JSON Lines")
                    .mimetype("application/x-ndjson")
                    .glob("*.jsonl"),
            );
        self.pending_export = Some(entries);
        #[cfg(test)]
        let export_rx = if let Some(portal) = &self.controlled_export_portal {
            portal(request)
        } else {
            request.send()
        };
        #[cfg(not(test))]
        let export_rx = request.send();
        self.export_rx = Some(export_rx);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn set_controlled_export_portal(
        &mut self,
        portal: impl Fn(PickRequest) -> PortalChannel + 'static,
    ) {
        self.controlled_export_portal = Some(Box::new(portal));
    }

    /// Complete a non-blocking host Save portal request. No selected path is
    /// retained after the write, and all user-facing errors stay concise.
    fn drain_export_portal(&mut self) {
        let Some(rx) = &self.export_rx else {
            return;
        };
        match rx.try_recv() {
            Ok(PortalOutcome::Opened(path)) => {
                self.export_rx = None;
                if let Some(entries) = self.pending_export.take() {
                    let count = entries.len();
                    let refs = entries.iter().collect::<Vec<_>>();
                    self.export_status = match crate::journal::export_json_lines(&refs, &path) {
                        Ok(()) => Some(format!("Exported {count} log entries.")),
                        Err(_) => Some("Export failed.".to_string()),
                    };
                }
                self.dirty = true;
            }
            Ok(PortalOutcome::Cancelled) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.export_rx = None;
                self.pending_export = None;
                self.dirty = true;
            }
            Ok(PortalOutcome::Failed(_)) => {
                self.export_rx = None;
                self.pending_export = None;
                self.export_status = Some("Export unavailable.".to_string());
                self.dirty = true;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
    }

    /// Queue a root effect through the published resource tree. All visible
    /// local controls use this instead of calling effects directly.
    fn request_journal_operation(&mut self, action: &str) {
        if action == "refresh" {
            self.source_loading = true;
        }
        self.pending_control_invocation = Some(ControlInvocation {
            address: ActionAddress::new(actions::JOURNAL_RESOURCE_ID, action),
            arguments: serde_json::Map::new(),
        });
        self.dirty = true;
    }

    /// Move the local selection by `delta` filtered positions.
    fn move_selection(&mut self, delta: isize) {
        if self.filtered.is_empty() {
            return;
        }
        let cur_pos = self
            .effective_selected()
            .and_then(|abs| self.filtered_pos(abs))
            .unwrap_or(0) as isize;
        let new_pos = (cur_pos + delta).clamp(0, self.filtered.len() as isize - 1) as usize;
        let abs = self.filtered.get(new_pos).copied().unwrap_or(0);
        let Some(entry) = self.entries.get(abs) else {
            return;
        };
        let id = actions::entry_resource_id(entry);
        self.apply_view_operation(LogsOperation::SelectEntry { entry_id: id });
        self.scroll_into_view(new_pos);
        self.dirty = true;
    }

    fn select_abs(&mut self, abs: usize) {
        let Some(entry) = self.entries.get(abs) else {
            return;
        };
        let id = actions::entry_resource_id(entry);
        self.apply_view_operation(LogsOperation::SelectEntry { entry_id: id });
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

        // The compact header remains present on phone widths even in offline
        // shots: it owns the discoverable Actions menu (Refresh, Follow /
        // Pause, Export), which must not disappear with desktop chrome.
        let header_h = self.m.header_h;
        let header = Rect::new(x0, y0, x0 + w, y0 + header_h);
        let filter_h = if Self::filters_wrap(w / self.m.k) {
            self.m.filter_wrap_h()
        } else {
            self.m.filter_h()
        };
        let filter = Rect::new(x0, header.y1, x0 + w, header.y1 + filter_h);
        let content_top = filter.y1;
        // The list/detail content spans all the way to the window bottom —
        // there is no standing legend strip to reserve room for.
        let content_bottom = y0 + h;

        let (search, unit_trigger, boot_trigger, chips) = self.filter_bar_regions(x0, w, filter);
        let (live_pill, refresh_btn, overflow_trigger, clear_btn) =
            self.header_controls(x0, w, header);

        // Content area between the filter bar and the window bottom.
        let cx0 = x0 + PAD;
        let cy0 = content_top + PAD;
        let cx1 = x0 + w - PAD;
        let cy1 = content_bottom - PAD;
        let gap = 14.0;

        let (list_header, list_body, detail, back_btn, narrow) =
            self.content_split(w, cx0, cy0, cx1, cy1, gap);
        // The Actions menu is opened from the compact header's overflow
        // trigger; if the window has since grown past the breakpoint (or a
        // capture requests the menu in a wide window) that trigger no longer
        // exists, so the sheet anchors where the trigger would sit — under the
        // header's trailing controls — instead of at the window origin.
        let overflow_anchor = if overflow_trigger.width() > 0.0 {
            overflow_trigger
        } else {
            let k = self.m.k;
            let ctl_h = 30.0 + (self.m.header_h - HEADER_H);
            let hy = header.y0 + (self.m.header_h - ctl_h) / 2.0;
            Rect::new(x0 + w - PAD - 178.0 * k, hy, x0 + w - PAD, hy + ctl_h)
        };
        let dropdown = self.dropdown_regions(unit_trigger, boot_trigger, overflow_anchor);

        Regions {
            header,
            filter,
            search,
            unit_trigger,
            boot_trigger,
            live_pill,
            refresh_btn,
            overflow_trigger,
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
    fn filter_bar_regions(&self, x0: f64, w: f64, filter: Rect) -> (Rect, Rect, Rect, Vec<Rect>) {
        if Self::filters_wrap(w / self.m.k) {
            let search_y = filter.y0 + 8.0;
            let search = Rect::new(
                x0 + PAD,
                search_y,
                x0 + w - PAD,
                search_y + self.m.control_h,
            );
            let facet_y = search.y1 + 6.0;
            let facet_w = ((w - PAD * 2.0 - 10.0) / 2.0).max(72.0);
            let unit_trigger = Rect::new(
                x0 + PAD,
                facet_y,
                x0 + PAD + facet_w,
                facet_y + self.m.control_h,
            );
            let boot_trigger = Rect::new(
                unit_trigger.x1 + 10.0,
                facet_y,
                x0 + w - PAD,
                facet_y + self.m.control_h,
            );
            let chip_y = facet_y + self.m.control_h + 6.0;
            let chip_w = ((w - PAD * 2.0 - 7.0 * 4.0) / 8.0).max(20.0);
            let chips = (0..8)
                .map(|i| {
                    let x = x0 + PAD + f64::from(i) * (chip_w + 4.0);
                    Rect::new(x, chip_y, x + chip_w, chip_y + self.m.control_h)
                })
                .collect();
            return (search, unit_trigger, boot_trigger, chips);
        }
        // The single-row bar: label-budgeted widths designed at 1× scale with
        // the text scale (the bar itself wraps at the 1×-space breakpoint).
        let k = self.m.k;
        let fy = filter.y0 + (self.m.filter_h() - self.m.control_h) / 2.0;
        let mut fx = x0 + PAD;
        let search = Rect::new(fx, fy, fx + 320.0 * k, fy + self.m.control_h);
        fx = search.x1 + 10.0;
        let unit_trigger = Rect::new(fx, fy, fx + 188.0 * k, fy + self.m.control_h);
        fx = unit_trigger.x1 + 10.0;
        let boot_trigger = Rect::new(fx, fy, fx + 196.0 * k, fy + self.m.control_h);

        // Severity chips, right-aligned in the filter bar.
        let chip_w = 46.0 * k;
        let chip_gap = 4.0;
        let chips_total = chip_w * 8.0 + chip_gap * 7.0;
        let chips_x0 = (x0 + w - PAD - chips_total).max(boot_trigger.x1 + 16.0);
        let chips: Vec<Rect> = (0..8)
            .map(|i| {
                let cx = chips_x0 + f64::from(i) * (chip_w + chip_gap);
                Rect::new(cx, fy, cx + chip_w, fy + self.m.control_h)
            })
            .collect();
        (search, unit_trigger, boot_trigger, chips)
    }

    /// The master-detail push breakpoint, judged in 1× space: a window holds
    /// as much 2× text as a half-width 1× window. One predicate for layout,
    /// paint and the event path, so they never disagree about the pane mode.
    fn narrow_at(&self, w: f64) -> bool {
        PushNav::is_narrow(w / self.m.k.max(f64::from(f32::EPSILON)))
    }

    /// The filters need more width than the docked journal split. Keep this
    /// decision separate from [`PushNav`]'s content-navigation breakpoint so
    /// a medium window preserves both panes without allowing controls to
    /// overlap or clip.
    fn filters_wrap(width: f64) -> bool {
        width < FILTER_WRAP_BREAKPOINT
    }

    /// `regions()`'s header: live pill + clear filters, right-aligned. The
    /// live pill is the rightmost control; the "Clear filters" button sits
    /// just left of it and is only laid out (non-zero) while a filter is
    /// active.
    fn header_controls(&self, x0: f64, w: f64, header: Rect) -> (Rect, Rect, Rect, Rect) {
        // Controls are 30 px (26 px for the clear chip) at 1× and grow by the
        // header's measured line growth; their label-budgeted widths scale
        // with the text scale, and the narrow breakpoint is judged in 1× space.
        let k = self.m.k;
        let grow = self.m.header_h - HEADER_H;
        let ctl_h = 30.0 + grow;
        if self.narrow_at(w) {
            let hy = header.y0 + (self.m.header_h - ctl_h) / 2.0;
            // Keep the menu at least as wide as its three action labels, so
            // the overlay remains inside a 390px phone viewport.
            let overflow = Rect::new(x0 + w - PAD - 178.0 * k, hy, x0 + w - PAD, hy + ctl_h);
            return (Rect::ZERO, Rect::ZERO, overflow, Rect::ZERO);
        }
        let hy = header.y0 + (self.m.header_h - ctl_h) / 2.0;
        let live_pill = Rect::new(x0 + w - PAD - 124.0 * k, hy, x0 + w - PAD, hy + ctl_h);
        let refresh_btn = Rect::new(
            live_pill.x0 - 6.0 - 34.0 * k,
            hy,
            live_pill.x0 - 6.0,
            hy + ctl_h,
        );
        let clear_btn = if self.filter.is_clear() {
            Rect::ZERO
        } else {
            let clear_h = 26.0 + grow;
            let cy = header.y0 + (self.m.header_h - clear_h) / 2.0;
            let x1 = refresh_btn.x0 - 12.0;
            Rect::new(x1 - 104.0 * k, cy, x1, cy + clear_h)
        };
        (live_pill, refresh_btn, Rect::ZERO, clear_btn)
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
        let narrow = self.narrow_at(w);
        let (list_header, list_body, detail, back_btn) = if narrow {
            // Single-pane push: the list and detail each fill the whole
            // content width and slide horizontally as one swaps for the
            // other. `slide` (0 = list, 1 = detail) offsets both slabs.
            let pane_w = cx1 - cx0;
            let slide = self.push.slide();
            let list_x = cx0 - pane_w * slide; // slides off to the left
            let detail_x = cx0 + pane_w * (1.0 - slide); // slides in from the right
            let lh = Rect::new(list_x, cy0, list_x + pane_w, cy0 + self.m.list_header_h);
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
            let lh = Rect::new(cx0, cy0, cx0 + list_w, cy0 + self.m.list_header_h);
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
        overflow_trigger: Rect,
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
                Popup::Actions => (
                    overflow_trigger,
                    vec![
                        ("refresh".to_string(), "Refresh journal".to_string()),
                        (
                            "set-live-tail".to_string(),
                            if self.live_tail {
                                "Pause live tail".to_string()
                            } else {
                                "Follow live tail".to_string()
                            },
                        ),
                        (
                            "export-filtered-jsonl".to_string(),
                            "Export filtered logs…".to_string(),
                        ),
                    ],
                ),
            };
            // Cap the dropdown height; scroll isn't needed for our facet
            // counts but clamp defensively.
            items.truncate(14);
            let dw = (trigger.width()).max(180.0);
            for (i, (val, label)) in items.into_iter().enumerate() {
                let iy = trigger.y1 + 4.0 + i as f64 * self.m.item_h;
                let irect = Rect::new(trigger.x0, iy, trigger.x0 + dw, iy + self.m.item_h);
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

/// Build a single-leaf taffy node. `new_leaf` only fails when the tree
/// itself is corrupted, which cannot happen for a tree we just built in this
/// same function.
#[allow(
    clippy::unwrap_used,
    reason = "new_leaf cannot fail for a freshly-built tree; see doc comment"
)]
fn leaf_node(taffy: &mut TaffyTree<()>, style: Style) -> NodeId {
    taffy.new_leaf(style).unwrap()
}

/// Baseline-free top-left text at `(x,y)`, no wrapping.
#[allow(
    clippy::too_many_arguments,
    reason = "Log text painting needs scene/transform, content, font size/weight/family, color, and top-left coordinates."
)]
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

/// Theme type sizes (`theme.tokens.typography.scale.*`) already carry the
/// accessibility text scale, and the size-based renderer APIs (`measure`,
/// `draw_text_styled`) apply that scale once more. Every token this app paints
/// goes through here so the multiplier lands exactly once — 2× glyphs at
/// 200 %, not 4×. Identity at 1×.
fn ts(tr: &TextRenderer, resolved: f32) -> f32 {
    tr.renderer_input_style(&TextStyle::new(resolved)).size
}

/// Vertical pitches of every text-bearing band, derived once per frame from
/// the measured line growth of the type sizes each band stacks. The `*_H`
/// constants above are the 1× design; each band grows by exactly the extra
/// height its lines gain at the current accessibility scale, so `regions()`
/// (layout + hit-testing) and the painters read one pitch. Glyph-budgeted
/// widths are laid out at 1× and scaled by `k`. At 1× this is
/// [`Metrics::ONE`].
#[derive(Clone, Copy, Debug, PartialEq)]
struct Metrics {
    /// The renderer's accessibility text scale.
    k: f64,
    /// Journal row (`sm` message line).
    row_h: f64,
    /// Table header row (`xs` labels).
    list_header_h: f64,
    /// Filter controls: the dropdown trigger stacks an `xs` caption over an
    /// `sm` value; the search field and the severity chips share the height.
    control_h: f64,
    /// Header band (one `sm` line of controls).
    header_h: f64,
    /// Dropdown row (`sm` label).
    item_h: f64,
    /// Painted line height per renderer-relative pixel of font size (1.25 at
    /// 1×, 2.5 at 200 %) — what `vcenter` centres on and stacked lines step by.
    line_per_px: f64,
}

impl Metrics {
    const ONE: Self = Self {
        k: 1.0,
        row_h: ROW_H,
        list_header_h: LIST_HEADER_H,
        control_h: CONTROL_H,
        header_h: HEADER_H,
        item_h: DROPDOWN_ITEM_H,
        line_per_px: 1.25,
    };

    /// Measure with the renderer that will paint. Needs no theme: in the
    /// renderer's input space the type ramp is the 1× ramp (the renderer
    /// applies the accessibility scale on top), so `Widget::measure` can set
    /// this before the first paint and the event path never sees 1× bands
    /// under a 2× frame.
    fn measure(tr: &mut TextRenderer) -> Self {
        let k = f64::from(tr.text_scale()).max(f64::from(f32::EPSILON));
        let ramp = ui::tokens::TypeScale::default();
        let line = |tr: &mut TextRenderer, size: f32| f64::from(tr.measure("0", size, None).1);
        let grow = |line: f64| (line - line / k).max(0.0);
        let sm_line = line(tr, ramp.sm);
        let xs = grow(line(tr, ramp.xs));
        let sm = grow(sm_line);
        let line_per_px = if sm_line > 0.0 {
            sm_line / f64::from(ramp.sm)
        } else {
            1.25
        };
        Self {
            k,
            row_h: ROW_H + sm,
            list_header_h: LIST_HEADER_H + xs,
            control_h: CONTROL_H + xs + sm,
            header_h: HEADER_H + sm,
            item_h: DROPDOWN_ITEM_H + sm,
            line_per_px,
        }
    }

    /// Height one line of renderer-relative `size` paints at this frame.
    fn line_h(self, size: f32) -> f64 {
        f64::from(size) * self.line_per_px
    }

    /// Vertically-centred Y for a single line of `size` inside `rect`, on the
    /// measured line box rather than the bare font size.
    fn vcenter(self, rect: Rect, size: f32) -> f64 {
        rect.y0 + (rect.height() - self.line_h(size)) / 2.0
    }

    /// Single-row filter bar height (`FILTER_H` at 1×).
    fn filter_h(self) -> f64 {
        FILTER_H + (self.control_h - CONTROL_H)
    }

    /// Wrapped (three-row) filter bar height (128 px at 1×).
    fn filter_wrap_h(self) -> f64 {
        128.0 + 3.0 * (self.control_h - CONTROL_H)
    }
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
            ui::chrome::chrome_hairline(theme),
            1.0,
        );

        // The OS owns the window title/app-name — the header draws only its
        // functional controls (the entry counts, an optional Clear-filters
        // button, and the live-tail pill).
        self.draw_header_clear_button(scene, t, tr, theme, reg);
        self.draw_header_counts(scene, t, tr, theme, reg);
        self.draw_header_refresh_button(scene, t, tr, theme, reg);
        self.draw_header_live_pill(scene, t, tr, theme, reg);
        self.draw_header_overflow_button(scene, t, tr, theme, reg);
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
            let bsz = ts(tr, theme.tokens.typography.scale.sm);
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
                self.m.vcenter(reg.clear_btn, bsz),
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
        let (cw, _) = tr.measure(&counts, ts(tr, theme.tokens.typography.scale.sm), None);
        if reg.overflow_trigger.width() > 0.0 {
            text_at(
                tr,
                scene,
                t,
                &counts,
                ts(tr, theme.tokens.typography.scale.sm),
                theme.text_dim,
                reg.header.x0 + PAD,
                self.m
                    .vcenter(reg.header, ts(tr, theme.tokens.typography.scale.sm)),
                TextWeight::Medium,
                false,
            );
            return;
        }
        let counts_anchor = if reg.clear_btn.width() > 0.0 {
            reg.clear_btn.x0
        } else {
            reg.refresh_btn.x0
        };
        text_at(
            tr,
            scene,
            t,
            &counts,
            ts(tr, theme.tokens.typography.scale.sm),
            theme.text_dim,
            counts_anchor - 14.0 - f64::from(cw),
            self.m
                .vcenter(reg.header, ts(tr, theme.tokens.typography.scale.sm)),
            TextWeight::Medium,
            false,
        );
    }

    /// Compact Refresh control. It queues the exact root resource address,
    /// rather than calling `journalctl` directly, so accessibility, agents,
    /// history and policy all see the same recoverable effect.
    fn draw_header_refresh_button(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        reg: &Regions,
    ) {
        if reg.refresh_btn.width() <= 0.0 {
            return;
        }
        let c = &theme.tokens.colors;
        let hovered = reg.refresh_btn.contains(self.cursor);
        ui::surface::card(
            scene,
            t,
            reg.refresh_btn,
            f64::from(theme.tokens.radii.full),
            if hovered { c.lift(0.72) } else { c.lift(0.35) },
        );
        let label = "↻";
        let sz = ts(tr, theme.tokens.typography.scale.base);
        let (w, _) = tr.measure_weighted(label, sz, None, TextWeight::Semibold);
        text_at(
            tr,
            scene,
            t,
            label,
            sz,
            theme.text_primary,
            reg.refresh_btn.center().x - f64::from(w) / 2.0,
            self.m.vcenter(reg.refresh_btn, sz) - 1.0,
            TextWeight::Semibold,
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
        if reg.live_pill.width() <= 0.0 {
            return;
        }
        let c = &theme.tokens.colors;
        let on = self.live_tail;
        let pill_bg = if on {
            theme.accent_primary
        } else {
            c.lift(0.35)
        };
        if on {
            fill_round(scene, t, reg.live_pill, theme.tokens.radii.full, pill_bg);
        } else {
            ui::surface::card(
                scene,
                t,
                reg.live_pill,
                f64::from(theme.tokens.radii.full),
                pill_bg,
            );
        }
        let on_accent = c.on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST);
        let dot_c = if on { on_accent } else { theme.text_dim };
        fill_circle(
            scene,
            t,
            Point::new(reg.live_pill.x0 + 16.0, reg.live_pill.center().y),
            4.0,
            dot_c,
        );
        let label = "Live tail";
        let lc = if on { on_accent } else { theme.text_dim };
        text_at(
            tr,
            scene,
            t,
            label,
            ts(tr, theme.tokens.typography.scale.sm),
            lc,
            reg.live_pill.x0 + 28.0,
            self.m
                .vcenter(reg.live_pill, ts(tr, theme.tokens.typography.scale.sm)),
            TextWeight::Semibold,
            false,
        );
    }

    /// Phone header's compact, explicit entry point to the shared journal
    /// operations. The menu items themselves are the canonical resource
    /// addresses; this button merely reveals them.
    fn draw_header_overflow_button(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        reg: &Regions,
    ) {
        if reg.overflow_trigger.width() <= 0.0 {
            return;
        }
        let c = &theme.tokens.colors;
        let open = self.popup == Some(Popup::Actions);
        let trigger_bg = if open || reg.overflow_trigger.contains(self.cursor) {
            c.lift(0.72)
        } else {
            c.lift(0.35)
        };
        ui::surface::card(
            scene,
            t,
            reg.overflow_trigger,
            f64::from(theme.tokens.radii.full),
            trigger_bg,
        );
        if open {
            stroke_round(
                scene,
                t,
                reg.overflow_trigger,
                theme.tokens.radii.full,
                c.border_focus,
                1.5,
            );
        }
        let label = "Actions";
        let sz = ts(tr, theme.tokens.typography.scale.sm);
        text_at(
            tr,
            scene,
            t,
            label,
            sz,
            theme.text_primary,
            reg.overflow_trigger.x0 + 12.0,
            self.m.vcenter(reg.overflow_trigger, sz),
            TextWeight::Semibold,
            false,
        );
        let cx = reg.overflow_trigger.x1 - 16.0;
        let cy = reg.overflow_trigger.center().y + 1.0;
        line(
            scene,
            t,
            cx - 4.0,
            cy - 2.0,
            cx,
            cy + 2.0,
            theme.text_dim,
            1.4,
        );
        line(
            scene,
            t,
            cx + 4.0,
            cy - 2.0,
            cx,
            cy + 2.0,
            theme.text_dim,
            1.4,
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
            ui::surface::edge_on(c.surfaces.base),
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
        ui::surface::card(
            scene,
            t,
            reg.search,
            f64::from(theme.tokens.radii.md),
            c.surfaces.raised,
        );
        if focused {
            stroke_round(
                scene,
                t,
                reg.search,
                theme.tokens.radii.md,
                c.border_focus,
                1.5,
            );
        }
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
            self.m,
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
            self.m,
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
                fill_round(scene, t, *chip, theme.tokens.radii.md, c.lift(0.35));
                stroke_round(scene, t, *chip, theme.tokens.radii.md, tint(col, 90), 1.0);
            }
            let tag = p.tag();
            let label_c = if active {
                // Ink measured against the severity fill with the toolkit's
                // on-fill floor: near-black on amber, white on the dark reds.
                theme.tokens.colors.on_surface(col).ink
            } else {
                theme.text_dim
            };
            let sz = ts(tr, theme.tokens.typography.scale.xs);
            let (lw, _) = tr.measure_weighted(tag, sz, None, TextWeight::Bold);
            text_at(
                tr,
                scene,
                t,
                tag,
                sz,
                label_c,
                chip.center().x - f64::from(lw) / 2.0,
                self.m.vcenter(*chip, sz),
                TextWeight::Bold,
                false,
            );
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Filter trigger painting combines render context, measured text metrics, bounds, caption/value, and open/active states."
    )]
    fn draw_dropdown_trigger(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        m: Metrics,
        rect: Rect,
        kind: &str,
        label: &str,
        open: bool,
        active: bool,
    ) {
        let c = &theme.tokens.colors;
        ui::surface::card(
            scene,
            t,
            rect,
            f64::from(theme.tokens.radii.md),
            c.surfaces.raised,
        );
        if open || active {
            stroke_round(
                scene,
                t,
                rect,
                theme.tokens.radii.md,
                if open {
                    c.border_focus
                } else {
                    theme.accent_primary
                },
                1.5,
            );
        }
        let sz = ts(tr, theme.tokens.typography.scale.sm);
        // kind caption
        text_at(
            tr,
            scene,
            t,
            kind,
            ts(tr, theme.tokens.typography.scale.xs),
            theme.text_dim,
            rect.x0 + 12.0,
            rect.y0 + 4.0,
            TextWeight::Semibold,
            false,
        );
        // The value stacks on the caption's measured line (16 px at 1×).
        let value_top = rect.y0 + 4.0 + m.line_h(ts(tr, theme.tokens.typography.scale.xs)) - 0.5;
        let val = elide(tr, label, sz, rect.width() - 38.0, false);
        text_at(
            tr,
            scene,
            t,
            &val,
            sz,
            theme.text_primary,
            rect.x0 + 12.0,
            value_top,
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
        Self::draw_list_frame_and_header(scene, t, tr, theme, self.m, reg);
        if self.draw_list_empty(scene, t, tr, theme, reg) {
            return;
        }
        self.draw_list_rows(scene, t, tr, theme, reg);
        self.draw_list_scrollbar(scene, t, theme, reg);
    }

    /// The list card background plus the TIME/LEVEL/UNIT/MESSAGE column
    /// header row.
    fn draw_list_frame_and_header(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        m: Metrics,
        reg: &Regions,
    ) {
        let c = &theme.tokens.colors;

        // List frame (header + body) as one card.
        let card = Rect::new(
            reg.list_header.x0,
            reg.list_header.y0,
            reg.list_header.x1,
            reg.list_body.y1,
        );
        ui::surface::card(
            scene,
            t,
            card,
            f64::from(theme.tokens.radii.lg),
            c.surfaces.raised,
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
        let cols = column_x(reg.list_header, m.k);
        let hsz = ts(tr, theme.tokens.typography.scale.xs);
        let hy = m.vcenter(reg.list_header, hsz);
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
        if !cols.compact {
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
        }
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
            ui::surface::edge_on(c.surfaces.raised),
            1.0,
        );
    }

    /// Draws the "no journal entries" / "no entries match" placeholder when
    /// there is nothing to list. Returns `true` when it drew one (the caller
    /// must skip the row list + scrollbar in that case).
    fn draw_list_empty(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        reg: &Regions,
    ) -> bool {
        let c = &theme.tokens.colors;
        if !self.filtered.is_empty() {
            return false;
        }
        let cx = reg.list_body.center().x;
        let cy = reg.list_body.center().y;
        if self.entries.is_empty() {
            // Honest zero-state: no readable journal (never the sample). A
            // classified read failure gets an explicit recovery instruction;
            // raw command errors never reach this view.
            let (title, subtitle) = if self.source_loading {
                (
                    "Refreshing journal",
                    "Waiting for the local system journal…",
                )
            } else if let Some(error) = self.source_error {
                ("Journal unavailable", error.message())
            } else if self.allow_live {
                (
                    "No journal entries",
                    "The systemd journal is empty for this session.",
                )
            } else {
                (
                    "No journal entries",
                    "Run where journald is available to view live system logs.",
                )
            };
            // The three lines stack on their measured boxes around `cy`: title
            // above (ending 4 px over centre), subtitle 2 px below, retry 9 px
            // under the subtitle — the 1× offsets (−24 / +2 / +26), scale-proof.
            let tsz = ts(tr, theme.tokens.typography.scale.lg);
            let ssz = ts(tr, theme.tokens.typography.scale.sm);
            let title_top = cy - 4.0 - self.m.line_h(tsz);
            let sub_top = cy + 2.0;
            let retry_top = sub_top + self.m.line_h(ssz) + 9.0;
            let (tw, _) = tr.measure(title, tsz, None);
            text_at(
                tr,
                scene,
                t,
                title,
                tsz,
                c.text_primary,
                cx - f64::from(tw) / 2.0,
                title_top,
                TextWeight::Semibold,
                false,
            );
            let (sw, _) = tr.measure(subtitle, ssz, None);
            text_at(
                tr,
                scene,
                t,
                subtitle,
                ssz,
                theme.text_dim,
                cx - f64::from(sw) / 2.0,
                sub_top,
                TextWeight::Regular,
                false,
            );
            if self.source_error.is_some() && self.allow_live {
                let retry = "Use Refresh to try again.";
                let (rw, _) = tr.measure(retry, ssz, None);
                text_at(
                    tr,
                    scene,
                    t,
                    retry,
                    ssz,
                    theme.accent_primary,
                    cx - f64::from(rw) / 2.0,
                    retry_top,
                    TextWeight::Medium,
                    false,
                );
            }
        } else {
            let msg = "No entries match the current filters.";
            let sz = ts(tr, theme.tokens.typography.scale.base);
            let (mw, _) = tr.measure(msg, sz, None);
            text_at(
                tr,
                scene,
                t,
                msg,
                sz,
                theme.text_dim,
                cx - f64::from(mw) / 2.0,
                cy - self.m.line_h(sz) / 2.0 - 1.25,
                TextWeight::Regular,
                false,
            );
        }
        true
    }

    /// Clips to the list body and draws the scrolled window of visible rows.
    #[allow(
        clippy::too_many_lines,
        reason = "the virtualized row paint keeps its per-column geometry together"
    )]
    fn draw_list_rows(
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

        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &reg.list_body);
        let body = reg.list_body;
        let first = (f64::from(self.scroll) / self.m.row_h).floor().max(0.0) as usize;
        let visible = (body.height() / self.m.row_h).ceil() as usize + 2;
        for pos in first..(first + visible).min(self.filtered.len()) {
            let Some(&abs) = self.filtered.get(pos) else {
                continue;
            };
            let Some(e) = self.entries.get(abs) else {
                continue;
            };
            let row_y0 = body.y0 + pos as f64 * self.m.row_h - f64::from(self.scroll);
            let row = Rect::new(body.x0, row_y0, body.x1, row_y0 + self.m.row_h);
            if row.y1 < body.y0 || row.y0 > body.y1 {
                continue;
            }
            let is_sel = sel == Some(abs);
            let is_hover = self.hover_row == Some(abs);
            let col = e.priority.color(&tc);
            // The painted band sits inside the row pitch, half an `s1` above
            // and below, so neighbouring rows keep a token-step gap (HIG §7)
            // while the pitch stays the paging and hit unit.
            let g = f64::from(theme.tokens.spacing.s1) / 2.0;
            let band = Rect::new(row.x0, row.y0 + g, row.x1, row.y1 - g);
            if is_sel {
                fill_rect(scene, t, band, tint(theme.accent_primary, 46));
                fill_rect(
                    scene,
                    t,
                    Rect::new(band.x0, band.y0, band.x0 + 3.0, band.y1),
                    theme.accent_primary,
                );
            } else if is_hover {
                fill_rect(scene, t, band, c.lift(0.35));
            } else if pos % 2 == 1 {
                fill_rect(
                    scene,
                    t,
                    band,
                    ui::tokens::Colors::hairline_on(c.surfaces.raised, 6),
                );
            }

            let cols = column_x(row, self.m.k);
            // severity dot
            fill_circle(scene, t, Point::new(cols.dot_cx, row.center().y), 4.5, col);
            // time (mono, dim)
            let tsz = ts(tr, theme.tokens.typography.scale.sm);
            let timestamp = if cols.compact {
                e.time_hms()
            } else {
                e.timestamp()
            };
            text_at(
                tr,
                scene,
                t,
                &timestamp,
                tsz,
                theme.text_dim,
                cols.time_x,
                self.m.vcenter(row, tsz),
                TextWeight::Regular,
                true,
            );
            // level tag (colored)
            text_at(
                tr,
                scene,
                t,
                e.priority.tag(),
                ts(tr, theme.tokens.typography.scale.xs),
                col,
                cols.tag_x,
                self.m
                    .vcenter(row, ts(tr, theme.tokens.typography.scale.xs)),
                TextWeight::Bold,
                true,
            );
            if !cols.compact {
                // Unit is promoted on desktop; compact rows reserve that
                // width for the message and expose the unit in detail/a11y.
                let unit_shown = elide(tr, e.unit_short(), tsz, cols.unit_w - 6.0, false);
                text_at(
                    tr,
                    scene,
                    t,
                    &unit_shown,
                    tsz,
                    theme.text_primary,
                    cols.unit_x,
                    self.m.vcenter(row, tsz),
                    TextWeight::Medium,
                    false,
                );
            }
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
                self.m.vcenter(row, tsz),
                TextWeight::Regular,
                false,
            );
        }
        scene.pop_layer();
    }

    /// The list body's vertical scrollbar thumb, when the content overflows.
    fn draw_list_scrollbar(&self, scene: &mut Scene, t: Affine, theme: &Theme, reg: &Regions) {
        let body = reg.list_body;
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
                ui::tokens::Colors::hairline_on(theme.tokens.colors.surfaces.raised, 70),
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
        let d = reg.detail;
        ui::surface::card(
            scene,
            t,
            d,
            f64::from(theme.tokens.radii.lg),
            c.surfaces.raised,
        );

        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &d);
        let mut layout = DetailLayout {
            inner_x: d.x0 + 18.0,
            inner_w: d.width() - 36.0,
            y: d.y0 + 18.0,
            bottom: d.y1,
        };

        // On narrow windows the detail is a pushed pane; draw a Back
        // affordance at the top and inset the field content beneath it.
        if reg.narrow && reg.back_btn.width() > 0.0 {
            self.draw_back_button(scene, t, tr, theme, reg.back_btn);
            layout.y = reg.back_btn.y1 + 14.0;
        }

        let Some(abs) = self.effective_selected() else {
            let msg = "Select an entry to inspect its fields.";
            let sz = ts(tr, theme.tokens.typography.scale.base);
            text_at(
                tr,
                scene,
                t,
                msg,
                sz,
                theme.text_dim,
                layout.inner_x,
                layout.y.max(d.center().y),
                TextWeight::Regular,
                false,
            );
            scene.pop_layer();
            return;
        };
        let Some(e) = self.entries.get(abs) else {
            scene.pop_layer();
            return;
        };

        Self::draw_detail_header(scene, t, tr, theme, self.m, &mut layout, e);
        Self::draw_detail_fields(scene, t, tr, theme, self.m, &mut layout, e);

        scene.pop_layer();
    }

    /// Badge, transport tag, title, timestamp, message and the divider that
    /// follows it. Advances `layout.y` past everything it draws.
    fn draw_detail_header(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        m: Metrics,
        layout: &mut DetailLayout,
        e: &LogEntry,
    ) {
        let c = &theme.tokens.colors;
        let tc = Self::theme_colors(theme);
        let inner_x = layout.inner_x;
        let inner_w = layout.inner_w;
        let col = e.priority.color(&tc);

        // Priority badge pill.
        let badge_label = e.priority.long().to_uppercase();
        let bsz = ts(tr, theme.tokens.typography.scale.xs);
        let (blw, _) = tr.measure_weighted(&badge_label, bsz, None, TextWeight::Bold);
        let badge = Rect::new(
            inner_x,
            layout.y,
            inner_x + f64::from(blw) + 24.0,
            layout.y + 22.0,
        );
        fill_round(scene, t, badge, theme.tokens.radii.full, tint(col, 235));
        text_at(
            tr,
            scene,
            t,
            &badge_label,
            bsz,
            theme.tokens.colors.on_surface(col).ink,
            badge.x0 + 12.0,
            m.vcenter(badge, bsz),
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
                m.vcenter(badge, bsz),
                TextWeight::Medium,
                true,
            );
        }
        layout.y = badge.y1 + 14.0;

        // Unit title.
        let title_sz = ts(tr, theme.tokens.typography.scale.lg);
        let title = elide(tr, &e.unit, title_sz, inner_w, false);
        text_at(
            tr,
            scene,
            t,
            &title,
            title_sz,
            theme.text_primary,
            inner_x,
            layout.y,
            TextWeight::Semibold,
            false,
        );
        layout.y += m.line_h(title_sz) * 1.2;

        // Timestamp.
        let tssz = ts(tr, theme.tokens.typography.scale.sm);
        text_at(
            tr,
            scene,
            t,
            &e.timestamp_full(),
            tssz,
            theme.text_dim,
            inner_x,
            layout.y,
            TextWeight::Regular,
            true,
        );
        layout.y += m.line_h(tssz) * 1.28 + 6.0;

        // Message (wrapped).
        let msz = ts(tr, theme.tokens.typography.scale.base);
        let (_, mh) = tr.measure(&e.message, msz, Some(inner_w as f32));
        tr.draw_text(
            scene,
            &e.message,
            msz,
            theme.text_primary,
            t * Affine::translate((inner_x, layout.y)),
            Some(inner_w as f32),
        );
        layout.y += f64::from(mh) + 14.0;

        // Divider.
        line(
            scene,
            t,
            inner_x,
            layout.y,
            inner_x + inner_w,
            layout.y,
            ui::surface::edge_on(c.surfaces.raised),
            1.0,
        );
        layout.y += 12.0;
    }

    /// The key/value field table beneath the header, stopping once it would
    /// run past `layout.bottom`.
    fn draw_detail_fields(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        m: Metrics,
        layout: &mut DetailLayout,
        e: &LogEntry,
    ) {
        let inner_x = layout.inner_x;
        let inner_w = layout.inner_w;

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

        // The key column is a glyph budget (118 px at 1×) scaled with the text,
        // but never more than 45 % of the pane so the values keep their room.
        let key_w = (118.0_f64 * m.k).min(inner_w * 0.45);
        let ksz = ts(tr, theme.tokens.typography.scale.xs);
        let vsz = ts(tr, theme.tokens.typography.scale.sm);
        for (k, v) in fields {
            if layout.y > layout.bottom - 14.0 {
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
                layout.y + 1.0,
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
                layout.y,
                TextWeight::Regular,
                false,
            );
            // Rows step by the value line's measured box (22 px at 1×).
            layout.y += m.line_h(vsz) + 7.0;
        }
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
        let bg = if hovered { c.lift(0.72) } else { c.lift(0.35) };
        ui::surface::card(scene, t, rect, f64::from(theme.tokens.radii.full), bg);
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
        let sz = ts(tr, theme.tokens.typography.scale.sm);
        text_at(
            tr,
            scene,
            t,
            "Back",
            sz,
            theme.text_primary,
            rect.x0 + 30.0,
            self.m.vcenter(rect, sz),
            TextWeight::Semibold,
            false,
        );
    }

    #[allow(
        clippy::too_many_lines,
        reason = "The hand-painted journal has one matching retained semantic tree."
    )]
    fn accessibility_tree(&self) -> AccessNode {
        let reg = self.regions();
        let mut filter_children = vec![
            self.search_input.accessibility().map_or_else(
                || AccessNode::new(Role::TextInput).with_name("Search journal"),
                |node| node.with_action_target("logs.search"),
            ),
            AccessNode::new(Role::ComboBox)
                .with_name(if self.filter.unit.is_empty() {
                    "All units"
                } else {
                    &self.filter.unit
                })
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .expanded(self.popup == Some(Popup::Unit)),
                )
                .with_bounds(reg.unit_trigger)
                .with_action_target("logs.unit"),
            AccessNode::new(Role::ComboBox)
                .with_name(if self.filter.boot.is_empty() {
                    "All boots"
                } else {
                    "Selected boot"
                })
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .expanded(self.popup == Some(Popup::Boot)),
                )
                .with_bounds(reg.boot_trigger)
                .with_action_target("logs.boot"),
        ];
        for (index, chip) in reg.chips.iter().enumerate() {
            if let Some(priority) = Priority::ALL.get(index) {
                filter_children.push(
                    AccessNode::new(Role::ToggleButton)
                        .with_name(format!("Show {} and more severe", priority.long()))
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .checked(self.filter.min_priority == priority.level()),
                        )
                        .with_bounds(*chip)
                        .with_action_target(format!("logs.priority:{}", priority.level())),
                );
            }
        }
        if let Some(popup) = self.popup {
            for (rect, value, label) in &reg.dropdown {
                let target = match popup {
                    Popup::Unit => format!("logs.unit-choice:{value}"),
                    Popup::Boot => format!("logs.boot-choice:{value}"),
                    Popup::Actions => format!("logs.action-choice:{value}"),
                };
                filter_children.push(
                    AccessNode::new(Role::MenuItem)
                        .with_name(label)
                        .with_bounds(*rect)
                        .with_action_target(target),
                );
            }
        }
        let mut children = Vec::new();
        if reg.header.height() > 0.0 {
            if reg.overflow_trigger.width() > 0.0 {
                children.push(
                    AccessNode::new(Role::Button)
                        .with_name("Journal actions")
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .expanded(self.popup == Some(Popup::Actions)),
                        )
                        .with_bounds(reg.overflow_trigger)
                        .with_action_target("logs.actions"),
                );
            }
            children.push(
                AccessNode::new(Role::Button)
                    .with_name("Refresh journal")
                    .with_state(
                        AccessState::new()
                            .focusable(self.allow_live)
                            .disabled(!self.allow_live),
                    )
                    .with_bounds(reg.refresh_btn)
                    .with_action_target("logs.refresh"),
            );
            children.push(
                AccessNode::new(Role::Switch)
                    .with_name("Live tail")
                    .with_state(
                        AccessState::new()
                            .focusable(self.allow_live)
                            .checked(self.live_tail)
                            .disabled(!self.allow_live),
                    )
                    .with_bounds(reg.live_pill)
                    .with_action_target("logs.live-tail"),
            );
        }
        children.push(
            AccessNode::new(Role::Group)
                .with_name("Journal filters")
                .with_bounds(reg.filter)
                .with_hashed_key("logs.filters")
                .with_children(filter_children),
        );
        if !self.filter.is_clear() {
            children.push(
                AccessNode::new(Role::Button)
                    .with_name("Clear filters")
                    .with_state(AccessState::new().focusable(true))
                    .with_bounds(reg.clear_btn)
                    .with_action_target("logs.clear-filters"),
            );
        }
        let mut rows = Vec::new();
        let first = (f64::from(self.scroll) / self.m.row_h).floor().max(0.0) as usize;
        let count = (reg.list_body.height() / self.m.row_h).ceil().max(1.0) as usize + 1;
        for (offset, abs) in self.filtered.iter().skip(first).take(count).enumerate() {
            let Some(entry) = self.entries.get(*abs) else {
                continue;
            };
            let rect = Rect::new(
                reg.list_body.x0,
                reg.list_body.y0 + offset as f64 * self.m.row_h,
                reg.list_body.x1,
                reg.list_body.y0 + (offset + 1) as f64 * self.m.row_h,
            );
            let id = actions::entry_resource_id(entry);
            rows.push(
                AccessNode::new(Role::ListItem)
                    .with_name(format!(
                        "{} {} {}",
                        entry.priority.long(),
                        entry.unit,
                        entry.message
                    ))
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .selected(self.effective_selected() == Some(*abs)),
                    )
                    .with_bounds(rect)
                    .with_action_target(format!("logs.entry:{id}")),
            );
        }
        children.push(
            AccessNode::new(Role::List)
                .with_name("Journal entries")
                .with_bounds(reg.list_body)
                .with_hashed_key("logs.entries")
                .with_children(rows),
        );
        if reg.narrow && reg.back_btn.width() > 0.0 && self.push.pane() == Pane::Detail {
            children.push(
                AccessNode::new(Role::Button)
                    .with_name("Back to entries")
                    .with_state(AccessState::new().focusable(true))
                    .with_bounds(reg.back_btn)
                    .with_action_target("logs.back"),
            );
        }
        if let Some(status) = &self.export_status {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(status)
                    .with_live(Politeness::Polite)
                    .with_hashed_key("logs.export-status"),
            );
        }
        AccessNode::new(Role::Window)
            .with_name("System logs")
            .with_state(AccessState::new().focusable(true).focused(self.focused))
            .with_bounds(self.rect)
            .with_children(children)
    }
}

// Column geometry inside a list row / header.
struct Cols {
    compact: bool,
    dot_cx: f64,
    time_x: f64,
    tag_x: f64,
    unit_x: f64,
    unit_w: f64,
    msg_x: f64,
    msg_w: f64,
}
/// Column edges of one journal row. The time / level / unit widths are glyph
/// budgets designed at 1×, so they scale with the text scale `k`, and the
/// compact breakpoint is judged in 1× space — a 200 % row folds exactly like
/// a half-width 1× row. Header and cells share this, so they always agree.
fn column_x(row: Rect, k: f64) -> Cols {
    let k = k.max(f64::from(f32::EPSILON));
    let rpad = 12.0;
    let mut cur = row.x0 + rpad;
    let dot_cx = cur + 4.0;
    cur += 16.0;
    // Phone-width master panes cannot faithfully carry four desktop columns.
    // Preserve chronology + severity and give the message the remaining
    // space; the selected entry's unit remains immediately available in the
    // pushed detail pane and in each row's accessible name.
    if row.width() / k < 480.0 {
        let time_x = cur;
        cur += 62.0 * k;
        let tag_x = cur;
        cur += 42.0 * k;
        let msg_x = cur;
        return Cols {
            compact: true,
            dot_cx,
            time_x,
            tag_x,
            unit_x: msg_x,
            unit_w: 0.0,
            msg_x,
            msg_w: (row.x1 - rpad - msg_x).max(24.0),
        };
    }
    let time_x = cur;
    cur += 138.0 * k + 8.0;
    let tag_x = cur;
    cur += 50.0 * k + 8.0;
    let unit_x = cur;
    let unit_w = 132.0 * k;
    cur += unit_w + 10.0;
    let msg_x = cur;
    let msg_w = (row.x1 - rpad - msg_x).max(40.0);
    Cols {
        compact: false,
        dot_cx,
        time_x,
        tag_x,
        unit_x,
        unit_w,
        msg_x,
        msg_w,
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "Line painting passes the scene transform, both endpoint coordinates, stroke color, and stroke width."
)]
fn line(scene: &mut Scene, t: Affine, x0: f64, y0: f64, x1: f64, y1: f64, c: Color, w: f64) {
    scene.stroke(
        &Stroke::new(w),
        t,
        c,
        None,
        &Line::new(Point::new(x0, y0), Point::new(x1, y1)),
    );
}

// ===================================================================
// Widget impl
// ===================================================================

impl Widget for LogsApp {
    fn document_history_definition(&self) -> Option<&'static ui::history_mode::HistoryDefinition> {
        Some(&crate::framework_history::HISTORY)
    }

    fn measure(
        &mut self,
        _known: taffy::Size<Option<f32>>,
        _available: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        // The band pitches follow the renderer that will paint; measuring
        // here means the event path (`regions()`, hit-testing) never sees 1×
        // bands under a frame that will paint at 200 %.
        self.m = Metrics::measure(text_renderer);
        taffy::Size::ZERO
    }

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[actions::LOGS_OPERATION_TYPE]
    }

    fn take_control_invocation(&mut self) -> Option<ui::tilo_actions::ControlInvocation> {
        self.pending_control_invocation
            .take()
            .or_else(|| self.search_input.take_control_invocation())
    }

    fn resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
        actions::resources(&self.entries, &self.filter, self.live_tail, self.allow_live)
    }

    fn invoke_resource_operation(
        &mut self,
        kind: OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if !matches!(kind, OperationKind::Effect | OperationKind::View) {
            return Ok(false);
        }
        let Some(operation) = actions::decode_operation(type_id, payload)? else {
            return Ok(false);
        };
        if kind == OperationKind::Effect && !self.allow_live {
            return Err("live journal access is disabled".into());
        }
        // A resource-level entry selection is navigation, not merely a row
        // highlight. Reveal the selected detail slab for an agent or palette
        // invocation just as a pointer selection does on a narrow viewport.
        let reveals_entry = matches!(operation, LogsOperation::SelectEntry { .. });
        self.perform_effect(operation)?;
        if reveals_entry && self.narrow_at(self.rect.width()) {
            self.push.show_detail();
            self.dirty = true;
        }
        Ok(true)
    }

    fn sync(&mut self, doc: &mut AutoCommit, root: &ObjId) {
        // Upgrade old shared documents by removing the transient keys written
        // by previous Logs versions. Nothing below reads or writes view state
        // in the document.
        LogsDoc::prune_legacy_view_state(doc, root);
        self.drain_export_portal();
        if let Some(filter) = self.shot_filter.take() {
            self.filter = filter;
            self.search_input.set_text(&self.filter.search);
            self.recompute_filtered();
        }
        self.search_input.sync(doc, root);

        // Optional live tail: throttled re-poll, never under --shots.
        if self.allow_live && self.live_tail && self.last_poll.elapsed() >= POLL_INTERVAL {
            self.last_poll = Instant::now();
            match crate::journal::load_live(2000) {
                Ok(fresh) => {
                    self.entries = fresh;
                    self.source_error = None;
                    self.rebuild_facets();
                    self.dirty = true;
                }
                Err(error) => {
                    self.source_error = Some(error);
                    self.dirty = true;
                }
            }
        }

        self.recompute_filtered();
        self.clamp_scroll();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = leaf_node(
            taffy,
            Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                flex_grow: 1.0,
                ..Default::default()
            },
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        self.node = Some(node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(l) = taffy.layout(node) else {
            return;
        };
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
                let top = pos as f64 * self.m.row_h;
                if top + self.m.row_h > body_h {
                    self.scroll = ((top + self.m.row_h) - body_h) as f32 + 4.0;
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
        // The host deliberately stops animation pumping for a reduced-motion
        // theme. Snap here as well so the first redraw after a navigation
        // lands on its final pane rather than leaving a half-open slab.
        // Every text-bearing band takes its pitch from this frame's measured
        // lines; `regions()` (layout and hit-testing) reads the same values.
        self.m = Metrics::measure(tr);

        if theme.reduce_motion {
            self.push.snap(self.push.pane());
        } else if self.push.tick() {
            self.dirty = true;
        }

        let reg = self.regions();
        let c = &theme.tokens.colors;
        // App background.
        fill_rect(scene, transform, self.rect, c.surfaces.base);

        if reg.header.height() > 0.0 {
            self.draw_header(scene, transform, tr, theme, &reg);
        }
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
        // Keep the canvas-owned popup in the primary draw pass as well as the
        // normal native overlay pass. The deterministic FrameHarness renders
        // only this pass, while the actual app still gets the same z-order.
        self.draw_popup(scene, transform, tr, theme);
    }

    fn draw_overlay(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _tr: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // `draw` paints the custom popup so headless captures and native
        // windows share one result; do not paint a second copy here.
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
        if self.route_search_field(
            event,
            scale_factor,
            *modifiers,
            doc,
            root,
            handled,
            focus_target,
            node_id,
            taffy,
        ) {
            return;
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
                self.on_click(self.cursor, handled);
            }
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } if key_event.state == ElementState::Pressed => {
                self.on_key(key_event, *modifiers, handled);
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
    fn accessibility(&self) -> Option<AccessNode> {
        Some(self.accessibility_tree())
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
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &AccessAction,
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> bool {
        if target == "logs.search" {
            return self.search_input.perform_action(action, state, path);
        }
        let activate = matches!(action, AccessAction::Activate | AccessAction::ToggleCheck);
        if target == "logs.actions" && activate {
            self.popup = Some(Popup::Actions);
            self.dirty = true;
            return true;
        }
        if target == "logs.refresh" && activate && self.allow_live {
            self.request_journal_operation("refresh");
            return true;
        }
        if target == "logs.live-tail" && activate && self.allow_live {
            self.request_journal_operation("set-live-tail");
            return true;
        }
        if target == "logs.clear-filters" && activate {
            self.apply_view_operation(LogsOperation::ClearFilters);
            self.scroll = 0.0;
            self.dirty = true;
            return true;
        }
        if target == "logs.unit" && activate {
            self.popup = Some(Popup::Unit);
            self.dirty = true;
            return true;
        }
        if target == "logs.boot" && activate {
            self.popup = Some(Popup::Boot);
            self.dirty = true;
            return true;
        }
        if target == "logs.back" && activate {
            self.push.show_list();
            self.dirty = true;
            return true;
        }
        if let Some(value) = target.strip_prefix("logs.priority:")
            && activate
            && let Ok(value) = value.parse::<i64>()
        {
            self.apply_view_operation(LogsOperation::SetPriority { value });
            self.scroll = 0.0;
            self.dirty = true;
            return true;
        }
        if let Some(value) = target.strip_prefix("logs.unit-choice:")
            && activate
        {
            self.apply_view_operation(LogsOperation::SetUnit {
                value: value.to_owned(),
            });
            self.popup = None;
            self.scroll = 0.0;
            self.dirty = true;
            return true;
        }
        if let Some(value) = target.strip_prefix("logs.boot-choice:")
            && activate
        {
            self.apply_view_operation(LogsOperation::SetBoot {
                value: value.to_owned(),
            });
            self.popup = None;
            self.scroll = 0.0;
            self.dirty = true;
            return true;
        }
        if let Some(action_id) = target.strip_prefix("logs.action-choice:")
            && activate
            && matches!(
                action_id,
                "refresh" | "set-live-tail" | "export-filtered-jsonl"
            )
        {
            self.popup = None;
            self.request_journal_operation(action_id);
            return true;
        }
        if let Some(id) = target.strip_prefix("logs.entry:")
            && activate
            && let Some(abs) = self
                .entries
                .iter()
                .position(|entry| actions::entry_resource_id(entry) == id)
        {
            self.select_abs(abs);
            if self.narrow_at(self.rect.width()) {
                self.push.show_detail();
            }
            return true;
        }
        false
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
        (self.allow_live && self.live_tail) || self.export_rx.is_some()
    }
    fn needs_animation_frame(&self) -> bool {
        // Keep pumping frames while the push slide eases into place, or
        // while live tail is on.
        self.push.is_animating() || (self.allow_live && self.live_tail) || self.export_rx.is_some()
    }
}

impl LogsApp {
    fn draw_popup(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.popup.is_none() {
            return;
        }
        let reg = self.regions();
        let Some(first) = reg.dropdown.first().map(|d| d.0) else {
            return;
        };
        let Some(last) = reg.dropdown.last().map(|d| d.0) else {
            return;
        };
        let panel = Rect::new(first.x0, first.y0 - 4.0, first.x1, last.y1 + 4.0);
        Self::draw_overlay_panel_bg(scene, transform, theme, panel);
        self.draw_overlay_rows(scene, transform, tr, theme, &reg);
    }

    /// Search-field layout, focus transfer, Escape-to-clear, and routing
    /// into the shared field (part of [`Widget::handle_event`]). Returns
    /// whether the caller should return immediately — either because
    /// Escape cleared the search here, or because the field consumed the
    /// event.
    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors Widget::handle_event's own signature, which it routes"
    )]
    fn route_search_field(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        doc: &mut AutoCommit,
        root: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) -> bool {
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
            self.apply_view_operation(LogsOperation::SetSearch {
                value: String::new(),
            });
            self.scroll = 0.0;
            self.dirty = true;
            *handled = true;
            return true;
        }

        let route_to_search = self.search_input.is_focused()
            || matches!(event, WindowEvent::CursorMoved { .. })
            || (left_press && search_rect.contains(self.cursor));
        if route_to_search {
            let mut input_handled = false;
            self.search_input.handle_event(
                event,
                scale_factor,
                &modifiers,
                doc,
                root,
                &mut input_handled,
                focus_target,
                node_id,
                taffy,
            );
            if self.search_input.take_dirty().is_some() {
                self.filter.search = self.search_input.value().to_owned();
                self.recompute_filtered();
                self.scroll = 0.0;
                self.dirty = true;
                input_handled = true;
            }
            if input_handled && !matches!(event, WindowEvent::CursorMoved { .. }) {
                *handled = true;
                return true;
            }
        }
        false
    }

    /// The dropdown panel's shadow, fill and border.
    fn draw_overlay_panel_bg(scene: &mut Scene, transform: Affine, theme: &Theme, panel: Rect) {
        // Panel role (HIG §9.5): the shared floating-panel shadow, ground and
        // derived border, so the list matches every other menu on the desktop.
        ui::surface::shadow(
            scene,
            transform,
            panel,
            f64::from(theme.tokens.radii.md),
            &theme.tokens.elevation.e3,
        );
        fill_round(
            scene,
            transform,
            panel,
            theme.tokens.radii.md,
            ui::chrome::panel_ground(theme),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            ui::chrome::panel_border(theme),
            None,
            &ui::surface::edge_outline(panel, f64::from(theme.tokens.radii.md)),
        );
    }

    /// Each dropdown row: hover/active tint, label and (for the active row) a
    /// check mark.
    fn draw_overlay_rows(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        reg: &Regions,
    ) {
        let c = &theme.tokens.colors;
        let active_val = match self.popup {
            Some(Popup::Unit) => self.filter.unit.clone(),
            Some(Popup::Boot) => self.filter.boot.clone(),
            Some(Popup::Actions) | None => String::new(),
        };
        let sz = ts(tr, theme.tokens.typography.scale.sm);
        for (irect, val, label) in &reg.dropdown {
            let is_hover = irect.contains(self.cursor);
            let is_active = &active_val == val;
            if is_hover {
                fill_rect(scene, transform, *irect, c.lift(0.72));
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
                self.m.vcenter(*irect, sz),
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
}

impl LogsApp {
    /// Filtered-list hit test: which absolute entry index sits under
    /// `pt` in the list body, if any.
    fn row_at(&self, reg: &Regions, pt: Point) -> Option<usize> {
        if !reg.list_body.contains(pt) {
            return None;
        }
        let rel = pt.y - reg.list_body.y0 + f64::from(self.scroll);
        let pos = (rel / self.m.row_h).floor();
        if pos < 0.0 {
            return None;
        }
        let pos = pos as usize;
        self.filtered.get(pos).copied()
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one ordered hit-test preserves the popup-to-row interaction priority"
    )]
    fn on_click(&mut self, pt: Point, handled: &mut bool) {
        // 1. An open dropdown captures the click first.
        if self.popup.is_some() {
            let reg = self.regions();
            for (irect, val, _label) in &reg.dropdown {
                if irect.contains(pt) {
                    match self.popup {
                        Some(Popup::Unit) => {
                            self.apply_view_operation(LogsOperation::SetUnit {
                                value: val.clone(),
                            });
                        }
                        Some(Popup::Boot) => {
                            self.apply_view_operation(LogsOperation::SetBoot {
                                value: val.clone(),
                            });
                        }
                        Some(Popup::Actions) => self.request_journal_operation(val),
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
            let on_trigger = reg.unit_trigger.contains(pt)
                || reg.boot_trigger.contains(pt)
                || reg.overflow_trigger.contains(pt);
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

        // 2. Header: refresh and live-tail controls.
        if reg.refresh_btn.contains(pt) && self.allow_live {
            self.request_journal_operation("refresh");
            *handled = true;
            return;
        }
        if reg.live_pill.contains(pt) {
            if self.allow_live {
                self.request_journal_operation("set-live-tail");
            }
            *handled = true;
            return;
        }
        if reg.overflow_trigger.contains(pt) {
            self.popup = if self.popup == Some(Popup::Actions) {
                None
            } else {
                Some(Popup::Actions)
            };
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
                let Some(priority) = Priority::ALL.get(i) else {
                    continue;
                };
                self.apply_view_operation(LogsOperation::SetPriority {
                    value: priority.level(),
                });
                self.scroll = 0.0;
                self.dirty = true;
                *handled = true;
                return;
            }
        }

        // 6. Clear-filters button.
        if !self.filter.is_clear() && reg.clear_btn.contains(pt) {
            self.apply_view_operation(LogsOperation::ClearFilters);
            self.scroll = 0.0;
            self.dirty = true;
            *handled = true;
            return;
        }

        // 7. List rows → select. On a narrow window this also pushes the
        // detail pane in (single-pane master-detail navigation).
        if let Some(abs) = self.row_at(&reg, pt) {
            self.select_abs(abs);
            if reg.narrow {
                self.push.show_detail();
            }
            *handled = true;
        }
    }

    fn on_key(
        &mut self,
        key_event: &winit::event::KeyEvent,
        modifiers: winit::event::Modifiers,
        handled: &mut bool,
    ) {
        let ctrl = modifiers.state().control_key();
        match &key_event.logical_key {
            Key::Named(NamedKey::ArrowDown) => {
                self.move_selection(1);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.move_selection(-1);
                *handled = true;
            }
            Key::Named(NamedKey::PageDown) => {
                self.move_selection(10);
                *handled = true;
            }
            Key::Named(NamedKey::PageUp) => {
                self.move_selection(-10);
                *handled = true;
            }
            Key::Named(NamedKey::Home) => {
                self.move_selection(isize::MIN / 2);
                *handled = true;
            }
            Key::Named(NamedKey::End) => {
                self.move_selection(isize::MAX / 2);
                *handled = true;
            }
            Key::Named(NamedKey::Escape) => {
                let narrow_in_detail =
                    self.narrow_at(self.rect.width()) && self.push.pane() == Pane::Detail;
                if self.popup.is_some() {
                    self.popup = None;
                } else if narrow_in_detail {
                    // Back out of the pushed detail pane to the list.
                    self.push.show_list();
                } else if !self.filter.search.is_empty() {
                    self.apply_view_operation(LogsOperation::SetSearch {
                        value: String::new(),
                    });
                    self.scroll = 0.0;
                }
                self.dirty = true;
                *handled = true;
            }
            Key::Character(s) => {
                if ctrl {
                    match s.as_str() {
                        "l" | "L" => {
                            self.request_journal_operation("set-live-tail");
                            *handled = true;
                        }
                        "r" | "R" => {
                            self.apply_view_operation(LogsOperation::ClearFilters);
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
    use automerge::{ROOT, ReadDoc};

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
        let (mut app, _doc) = synced_app();
        let total = app.filtered.len();
        app.apply_view_operation(LogsOperation::SetPriority {
            value: Priority::Err.level(),
        });
        let errs = app.filtered.len();
        assert!(errs < total, "errors-only must show fewer than all");
        assert!(errs > 0);
        for &i in &app.filtered {
            assert!(app.entries[i].priority.level() <= 3);
        }
    }

    #[test]
    fn search_narrows_results() {
        let (mut app, _doc) = synced_app();
        app.apply_view_operation(LogsOperation::SetSearch {
            value: "iroh".to_string(),
        });
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
        let (mut app, _doc) = synced_app();
        app.apply_view_operation(LogsOperation::SetUnit {
            value: "kernel".to_string(),
        });
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
    fn arrow_navigation_moves_only_local_selection() {
        let (mut app, doc) = synced_app();
        let before = app.effective_selected().unwrap();
        app.move_selection(1);
        let after = app.effective_selected().unwrap();
        assert_ne!(before, after);
        assert_eq!(
            app.selected_entry.as_deref(),
            app.entries
                .get(after)
                .map(actions::entry_resource_id)
                .as_deref()
        );
        assert!(
            doc.keys(&ROOT).next().is_none(),
            "selection must not touch the shared doc"
        );
    }

    #[test]
    fn boot_filter_is_local() {
        let (mut app, _doc) = synced_app();
        let boot = app.boots[1].0.clone();
        app.apply_view_operation(LogsOperation::SetBoot {
            value: boot.clone(),
        });
        assert!(app.filtered.iter().all(|&i| app.entries[i].boot_id == boot));
    }

    #[test]
    fn agent_filter_view_operation_never_changes_the_shared_document() {
        let (mut app, doc) = synced_app();
        let encoded = tilo_actions::EncodedAction::encode_operation(
            actions::LOGS_OPERATION_TYPE,
            &LogsOperation::SetUnit {
                value: "kernel".to_string(),
            },
        )
        .unwrap();

        assert!(
            app.invoke_resource_operation(
                OperationKind::View,
                &encoded.type_id,
                &encoded.payload,
                &doc,
                &ROOT,
            )
            .unwrap()
        );
        assert_eq!(app.filter.unit, "kernel");
        assert!(
            app.filtered
                .iter()
                .all(|&index| app.entries[index].unit == "kernel")
        );
        assert!(
            doc.keys(&ROOT).next().is_none(),
            "agent view actions must not write shared state"
        );
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
    fn medium_width_filter_controls_wrap_without_clipping() {
        let (mut app, _doc) = synced_app();
        app.rect = Rect::new(0.0, 0.0, 1024.0, 720.0);
        let reg = app.regions();

        assert!(
            (reg.filter.height() - 128.0).abs() < f64::EPSILON,
            "medium-width filters should use the compact three-row height"
        );
        assert_eq!(reg.chips.len(), 8);
        assert!(
            reg.chips.iter().all(|chip| chip.x1 <= app.rect.x1 - PAD),
            "every severity choice must stay inside the viewport"
        );
        assert!(
            reg.chips.iter().all(|chip| chip.y0 > reg.unit_trigger.y1),
            "severity choices belong on their own compact row"
        );
    }

    #[test]
    fn phone_rows_prioritize_time_level_and_message() {
        let row = Rect::new(0.0, 0.0, 358.0, ROW_H);
        let cols = column_x(row, 1.0);
        assert!(cols.compact, "phone rows use compact column geometry");
        assert!(
            cols.unit_w.abs() < f64::EPSILON,
            "unit yields space to the message"
        );
        assert!(cols.msg_w >= 200.0, "message remains meaningfully readable");
        assert!(cols.msg_x < row.x1);
    }

    #[test]
    fn refresh_control_queues_the_exported_root_operation() {
        let mut app = LogsApp::new(sample_entries(), true);
        app.request_journal_operation("refresh");
        let invocation = app
            .take_control_invocation()
            .expect("refresh queues an inert control invocation");
        assert_eq!(invocation.address.resource, actions::JOURNAL_RESOURCE_ID);
        assert_eq!(invocation.address.action, "refresh");
        assert!(app.source_loading);
    }

    #[test]
    fn phone_actions_menu_exposes_refresh_follow_and_export() {
        let (mut app, _doc) = synced_app();
        app.allow_live = true;
        app.rect = Rect::new(0.0, 0.0, 390.0, 844.0);
        app.shots_show_actions();
        let reg = app.regions();
        assert!(reg.overflow_trigger.width() > 0.0);
        assert_eq!(reg.dropdown.len(), 3);
        let tree = app.accessibility_tree();
        assert!(
            tree.children
                .iter()
                .any(|node| node.name.as_deref() == Some("Journal actions"))
        );
        let filters = tree
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some("Journal filters"))
            .unwrap();
        assert!(filters.children.iter().any(|node| {
            node.action_target.as_deref() == Some("logs.action-choice:export-filtered-jsonl")
        }));
    }

    #[test]
    fn portal_export_writes_the_snapshot_selected_before_the_picker() {
        let mut app = LogsApp::new(sample_entries(), true);
        let path = std::env::temp_dir().join(format!(
            "tilo-logs-portal-export-{}-{}.jsonl",
            std::process::id(),
            app.entries[0].realtime_us
        ));
        let _ = std::fs::remove_file(&path);
        app.pending_export = Some(app.entries.iter().take(2).cloned().collect());
        app.export_rx = Some(ui::portal::inject_for_test(PortalOutcome::Opened(
            path.clone(),
        )));
        app.drain_export_portal();
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 2);
        assert_eq!(
            app.export_status.as_deref(),
            Some("Exported 2 log entries.")
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn portal_export_write_failure_is_announced_without_path_details() {
        let mut app = LogsApp::new(sample_entries(), true);
        app.pending_export = Some(app.entries.iter().take(1).cloned().collect());
        app.export_rx = Some(ui::portal::inject_for_test(PortalOutcome::Opened(
            std::env::temp_dir(),
        )));
        app.drain_export_portal();
        assert_eq!(app.export_status.as_deref(), Some("Export failed."));
        let tree = app.accessibility_tree();
        assert!(tree.children.iter().any(|node| {
            node.name.as_deref() == Some("Export failed.") && node.live == Some(Politeness::Polite)
        }));
    }

    #[test]
    fn clear_filters_control_lives_in_header_and_resets() {
        let (mut app, mut doc) = synced_app();
        // A clear filter set means no Clear-filters control is laid out.
        assert!(app.filter.is_clear());
        assert_eq!(app.regions().clear_btn, Rect::ZERO);

        // Activate a filter (errors-or-worse threshold).
        app.apply_view_operation(LogsOperation::SetPriority {
            value: Priority::Err.level(),
        });
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
        app.on_click(reg.clear_btn.center(), &mut handled);
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

    #[test]
    fn accessibility_exposes_hand_painted_controls_and_rejects_unknown_targets() {
        let (mut app, mut doc) = synced_app();
        let tree = app.accessibility_tree();
        assert!(tree.children.iter().any(|node| node.role == Role::Switch));
        let filters = tree
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some("Journal filters"))
            .expect("filter group is exposed");
        assert!(
            filters
                .children
                .iter()
                .any(|node| node.role == Role::ToggleButton)
        );
        let list = tree
            .children
            .iter()
            .find(|node| node.role == Role::List)
            .expect("entry list is exposed");
        assert!(list.children.iter().all(|node| node.state.focusable));
        assert!(
            !Widget::perform_semantic_action(
                &mut app,
                "logs.unknown",
                &AccessAction::Focus,
                &mut doc,
                &ROOT,
            ),
            "unknown targets must not claim an accessibility action"
        );
        assert!(
            !Widget::perform_semantic_action(
                &mut app,
                "logs.live-tail",
                &AccessAction::Activate,
                &mut doc,
                &ROOT,
            ),
            "unavailable local journal access must reject live-tail activation"
        );
    }
}
