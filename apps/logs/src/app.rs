//! `LogsApp` — the root widget for the Logs viewer.
//!
//! A graphical systemd-journal viewer. It is a single self-rendering
//! [`Widget`] (the data is read-only and the visuals — severity dots,
//! coloured rows, a detail pane, and a filter bar — are drawn directly into
//! the Vello scene. Shared filter and selection preferences sync through
//! [`crate::resource::Logs`]. Live tail is a local Effect that controls this
//! process's journal poller; journal entries themselves are an external
//! render cache.
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
use ui::time::{Duration, Instant};
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Line, Point, Rect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::a11y::{AccessNode, AccessState, Action as AccessAction, Politeness, Role};
use ui::portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
use ui::presence::PresenceCtx;
use ui::text_render::{FontChoice, TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use ui::tilo_actions::{ActionAddress, ControlInvocation, OperationKind, Resource};
use ui::widget::{TextInput, Widget};

use crate::actions::{self, LogsOperation};
use crate::model::{Filter, LogEntry, Priority};
use crate::resource::LogsAction;
use crate::theme_ext::{ThemeColors, tint};
use ui::paint::{PaintCtx, fill_rect, fill_round, stroke_round};
use ui::widget::{PushNav, PushPane};

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
    let now = ui::time::now();
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

    /// Document actions: local host standalone; framework dispatcher when hosted.
    actions: crate::resource::ActionSink,
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
            last_poll: ui::time::now(),
            actions: crate::resource::ActionSink::default(),
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
        self.push.snap(PushPane::Detail);
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

    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Logs>,
    ) {
        self.actions.attach(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.actions.is_hosted()
    }

    /// Test-only: mark the surface hosted without a real framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.actions.mark_hosted_for_test();
    }

    /// Adopt collaborative view prefs; never clear the local journal cache.
    pub(crate) fn adopt_resource_state(&mut self, state: &crate::resource::LogsState) {
        self.filter = state.filter.clone();
        if self.search_input.value() != self.filter.search {
            self.search_input.set_text(&self.filter.search);
        }
        self.selected_entry = state.selected_entry.clone();
        self.sel_raw = self
            .selected_entry
            .as_ref()
            .and_then(|entry_id| {
                self.entries
                    .iter()
                    .position(|entry| actions::entry_resource_id(entry) == *entry_id)
            })
            .and_then(|index| i64::try_from(index).ok())
            .unwrap_or(-1);
        self.recompute_filtered();
        self.clamp_scroll();
        self.dirty = true;
    }

    pub(crate) fn commit_view(&mut self, doc: &mut AutoCommit, action: &LogsAction) {
        self.apply_logs_action_locally(action);
        self.actions.dispatch(doc, action);
    }

    #[cfg(test)]
    pub(crate) fn last_document_action(&self) -> Option<&ui::tilo_actions::EncodedAction> {
        self.actions.last_dispatched()
    }

    #[cfg(test)]
    pub(crate) fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Apply a document action to the widget caches without writing the doc.
    fn apply_logs_action_locally(&mut self, action: &LogsAction) {
        match action {
            LogsAction::SetPriority { value } => self.filter.min_priority = (*value).clamp(0, 7),
            LogsAction::SetUnit { value } => self.filter.unit = value.clone(),
            LogsAction::SetBoot { value } => self.filter.boot = value.clone(),
            LogsAction::SetSearch { value } => {
                self.filter.search = value.clone();
                self.search_input.set_text(&self.filter.search);
            }
            LogsAction::ClearFilters => {
                self.filter = Filter::default();
                self.search_input.set_text("");
            }
            LogsAction::SelectEntry { entry_id } => {
                self.selected_entry = Some(entry_id.clone());
                self.sel_raw = self
                    .entries
                    .iter()
                    .position(|entry| actions::entry_resource_id(entry) == *entry_id)
                    .and_then(|index| i64::try_from(index).ok())
                    .unwrap_or(-1);
            }
        }
        self.recompute_filtered();
        self.clamp_scroll();
        self.dirty = true;
    }

    /// Standalone / `--shots` / unit tests: one tree from [`tilo_resource::publish`]
    /// with live journal entry children overlaid.
    pub(crate) fn publish_resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        use ui::tilo_resource::Resource as _;
        let state = crate::resource::Logs::read(doc).unwrap_or_default();
        actions::standalone_resources(
            &self.entries,
            &self.filter,
            self.live_tail,
            self.allow_live,
            &state,
        )
    }

    /// Live journal entry rows without a `logs.journal` root.
    pub(crate) fn live_entry_extras(&self) -> Vec<Resource> {
        actions::live_entry_resources(&self.entries, &self.filter)
    }

    /// Local journal Effects. Polling and live-tail are device-local and must
    /// not start work on every peer. Entry select Views also commit the shared
    /// selection into the document.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "Decoded operations are consumed exactly once at the app boundary."
    )]
    fn perform_effect(
        &mut self,
        operation: actions::LogsOperation,
        doc: &mut AutoCommit,
    ) -> Result<(), String> {
        match operation {
            LogsOperation::SelectEntry { entry_id } => {
                self.commit_view(doc, &LogsAction::SelectEntry { entry_id });
            }
            LogsOperation::Refresh => {
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
            LogsOperation::SetLiveTail { enabled } => {
                self.live_tail = enabled;
                self.last_poll = force_immediate_poll_at();
            }
            LogsOperation::ExportFiltered => self.request_export()?,
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
        self.export_rx = Some(request.send());
        Ok(())
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

    /// Move the shared selection by `delta` filtered positions.
    fn move_selection(&mut self, doc: &mut AutoCommit, delta: isize) {
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
        self.commit_view(doc, &LogsAction::SelectEntry { entry_id: id });
        self.scroll_into_view(new_pos);
        self.dirty = true;
    }

    fn select_abs(&mut self, doc: &mut AutoCommit, abs: usize) {
        let Some(entry) = self.entries.get(abs) else {
            return;
        };
        let id = actions::entry_resource_id(entry);
        self.commit_view(doc, &LogsAction::SelectEntry { entry_id: id });
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
        crate::responsive::is_narrow(w / self.m.k.max(f64::from(f32::EPSILON)))
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

/// Theme type sizes (`theme.tokens.typography.scale.*`) already carry the
/// accessibility text scale, and the size-based renderer APIs (`measure`,
/// `draw_style`) apply that scale once more. Every token this app paints
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
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        if !self.filter.is_clear() && reg.clear_btn.width() > 0.0 {
            ctx.fill_round(
                reg.clear_btn,
                f64::from(theme.tokens.radii.md),
                tint(theme.accent_primary, 40),
            );
            ctx.stroke_round(
                reg.clear_btn,
                f64::from(theme.tokens.radii.md),
                theme.accent_primary,
                1.0,
            );
            let bsz = ts(ctx.text, theme.tokens.typography.scale.sm);
            let label = "Clear filters";
            let (bw, _) = ctx
                .text
                .measure_weighted(label, bsz, None, TextWeight::Semibold);
            ctx.text(
                label,
                &TextStyle::new(bsz).with_weight(TextWeight::Semibold),
                theme.accent_primary,
                Point::new(
                    reg.clear_btn.center().x - f64::from(bw) / 2.0,
                    self.m.vcenter(reg.clear_btn, bsz),
                ),
                None,
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
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        let counts = format!(
            "{} shown · {} total",
            self.filtered.len(),
            self.entries.len()
        );
        let (cw, _) = ctx.text.measure(
            &counts,
            ts(ctx.text, theme.tokens.typography.scale.sm),
            None,
        );
        if reg.overflow_trigger.width() > 0.0 {
            ctx.text(
                &counts,
                &TextStyle::new(ts(ctx.text, theme.tokens.typography.scale.sm))
                    .with_weight(TextWeight::Medium),
                theme.text_dim,
                Point::new(
                    reg.header.x0 + PAD,
                    self.m
                        .vcenter(reg.header, ts(ctx.text, theme.tokens.typography.scale.sm)),
                ),
                None,
            );
            return;
        }
        let counts_anchor = if reg.clear_btn.width() > 0.0 {
            reg.clear_btn.x0
        } else {
            reg.refresh_btn.x0
        };
        ctx.text(
            &counts,
            &TextStyle::new(ts(ctx.text, theme.tokens.typography.scale.sm))
                .with_weight(TextWeight::Medium),
            theme.text_dim,
            Point::new(
                counts_anchor - 14.0 - f64::from(cw),
                self.m
                    .vcenter(reg.header, ts(ctx.text, theme.tokens.typography.scale.sm)),
            ),
            None,
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
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        if reg.refresh_btn.width() <= 0.0 {
            return;
        }
        let c = &theme.tokens.colors;
        let hovered = reg.refresh_btn.contains(self.cursor);
        ui::surface::card(
            ctx.scene,
            t,
            reg.refresh_btn,
            f64::from(theme.tokens.radii.full),
            if hovered { c.lift(0.72) } else { c.lift(0.35) },
        );
        let label = "↻";
        let sz = ts(ctx.text, theme.tokens.typography.scale.base);
        let (w, _) = ctx
            .text
            .measure_weighted(label, sz, None, TextWeight::Semibold);
        ctx.text(
            label,
            &TextStyle::new(sz).with_weight(TextWeight::Semibold),
            theme.text_primary,
            Point::new(
                reg.refresh_btn.center().x - f64::from(w) / 2.0,
                self.m.vcenter(reg.refresh_btn, sz) - 1.0,
            ),
            None,
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
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
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
            ctx.fill_round(reg.live_pill, f64::from(theme.tokens.radii.full), pill_bg);
        } else {
            ui::surface::card(
                ctx.scene,
                t,
                reg.live_pill,
                f64::from(theme.tokens.radii.full),
                pill_bg,
            );
        }
        let on_accent = c.on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST);
        let dot_c = if on { on_accent } else { theme.text_dim };
        fill_circle(
            ctx.scene,
            t,
            Point::new(reg.live_pill.x0 + 16.0, reg.live_pill.center().y),
            4.0,
            dot_c,
        );
        let label = "Live tail";
        let lc = if on { on_accent } else { theme.text_dim };
        ctx.text(
            label,
            &TextStyle::new(ts(ctx.text, theme.tokens.typography.scale.sm))
                .with_weight(TextWeight::Semibold),
            lc,
            Point::new(
                reg.live_pill.x0 + 28.0,
                self.m.vcenter(
                    reg.live_pill,
                    ts(ctx.text, theme.tokens.typography.scale.sm),
                ),
            ),
            None,
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
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
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
            ctx.scene,
            t,
            reg.overflow_trigger,
            f64::from(theme.tokens.radii.full),
            trigger_bg,
        );
        if open {
            ctx.stroke_round(
                reg.overflow_trigger,
                f64::from(theme.tokens.radii.full),
                c.border_focus,
                1.5,
            );
        }
        let label = "Actions";
        let sz = ts(ctx.text, theme.tokens.typography.scale.sm);
        ctx.text(
            label,
            &TextStyle::new(sz).with_weight(TextWeight::Semibold),
            theme.text_primary,
            Point::new(
                reg.overflow_trigger.x0 + 12.0,
                self.m.vcenter(reg.overflow_trigger, sz),
            ),
            None,
        );
        let cx = reg.overflow_trigger.x1 - 16.0;
        let cy = reg.overflow_trigger.center().y + 1.0;
        line(
            ctx.scene,
            t,
            cx - 4.0,
            cy - 2.0,
            cx,
            cy + 2.0,
            theme.text_dim,
            1.4,
        );
        line(
            ctx.scene,
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
                f64::from(theme.tokens.radii.md),
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
        self.draw_dropdown_trigger(
            &mut PaintCtx::new(scene, t, tr, theme),
            reg.unit_trigger,
            Popup::Unit,
            ("Unit", &unit_label),
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
        self.draw_dropdown_trigger(
            &mut PaintCtx::new(scene, t, tr, theme),
            reg.boot_trigger,
            Popup::Boot,
            ("Boot", &boot_label),
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
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        let c = &theme.tokens.colors;
        let tc = Self::theme_colors(theme);
        for (i, chip) in reg.chips.iter().enumerate() {
            let Some(p) = Priority::ALL.get(i).copied() else {
                continue;
            };
            let active = p.level() <= self.filter.min_priority;
            let col = p.color(&tc);
            if active {
                ctx.fill_round(*chip, f64::from(theme.tokens.radii.md), tint(col, 235));
            } else {
                ctx.fill_round(*chip, f64::from(theme.tokens.radii.md), c.lift(0.35));
                ctx.stroke_round(*chip, f64::from(theme.tokens.radii.md), tint(col, 90), 1.0);
            }
            let tag = p.tag();
            let label_c = if active {
                // Ink measured against the severity fill with the toolkit's
                // on-fill floor: near-black on amber, white on the dark reds.
                theme.tokens.colors.on_surface(col).ink
            } else {
                theme.text_dim
            };
            let sz = ts(ctx.text, theme.tokens.typography.scale.xs);
            let (lw, _) = ctx.text.measure_weighted(tag, sz, None, TextWeight::Bold);
            ctx.text(
                tag,
                &TextStyle::new(sz).with_weight(TextWeight::Bold),
                label_c,
                Point::new(
                    chip.center().x - f64::from(lw) / 2.0,
                    self.m.vcenter(*chip, sz),
                ),
                None,
            );
        }
    }

    /// The trigger that opens `popup`, captioned `kind` over the current
    /// `label`; `active` marks a filter that narrows the list.
    fn draw_dropdown_trigger(
        &self,
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        popup: Popup,
        (kind, label): (&str, &str),
        active: bool,
    ) {
        let (t, theme, m) = (ctx.transform, ctx.theme, self.m);
        let open = self.popup == Some(popup);
        let c = &theme.tokens.colors;
        ui::surface::card(
            ctx.scene,
            t,
            rect,
            f64::from(theme.tokens.radii.md),
            c.surfaces.raised,
        );
        if open || active {
            ctx.stroke_round(
                rect,
                f64::from(theme.tokens.radii.md),
                if open {
                    c.border_focus
                } else {
                    theme.accent_primary
                },
                1.5,
            );
        }
        let sz = ts(ctx.text, theme.tokens.typography.scale.sm);
        // kind caption
        ctx.text(
            kind,
            &TextStyle::new(ts(ctx.text, theme.tokens.typography.scale.xs))
                .with_weight(TextWeight::Semibold),
            theme.text_dim,
            Point::new(rect.x0 + 12.0, rect.y0 + 4.0),
            None,
        );
        // The value stacks on the caption's measured line (16 px at 1×).
        let value_top =
            rect.y0 + 4.0 + m.line_h(ts(ctx.text, theme.tokens.typography.scale.xs)) - 0.5;
        let val = ctx
            .text
            .elide_to_width(label, sz, ui::num::f32_of_f64(rect.width() - 38.0));
        ctx.text(
            &val,
            &TextStyle::new(sz).with_weight(TextWeight::Medium),
            theme.text_primary,
            Point::new(rect.x0 + 12.0, value_top),
            None,
        );
        // chevron
        let cxp = rect.x1 - 16.0;
        let cyp = rect.center().y + 2.0;
        line(
            ctx.scene,
            t,
            cxp - 4.0,
            cyp - 3.0,
            cxp,
            cyp + 2.0,
            theme.text_dim,
            1.4,
        );
        line(
            ctx.scene,
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
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        let c = &theme.tokens.colors;

        // List frame (header + body) as one card.
        let card = Rect::new(
            reg.list_header.x0,
            reg.list_header.y0,
            reg.list_header.x1,
            reg.list_body.y1,
        );
        ui::surface::card(
            ctx.scene,
            t,
            card,
            f64::from(theme.tokens.radii.lg),
            c.surfaces.raised,
        );

        // Column header.
        ctx.fill_round(
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
        let hsz = ts(ctx.text, theme.tokens.typography.scale.xs);
        let hy = m.vcenter(reg.list_header, hsz);
        ctx.text(
            "TIME",
            &TextStyle::new(hsz).with_weight(TextWeight::Semibold),
            theme.text_dim,
            Point::new(cols.time_x, hy),
            None,
        );
        ctx.text(
            "LEVEL",
            &TextStyle::new(hsz).with_weight(TextWeight::Semibold),
            theme.text_dim,
            Point::new(cols.tag_x, hy),
            None,
        );
        if !cols.compact {
            ctx.text(
                "UNIT",
                &TextStyle::new(hsz).with_weight(TextWeight::Semibold),
                theme.text_dim,
                Point::new(cols.unit_x, hy),
                None,
            );
        }
        ctx.text(
            "MESSAGE",
            &TextStyle::new(hsz).with_weight(TextWeight::Semibold),
            theme.text_dim,
            Point::new(cols.msg_x, hy),
            None,
        );
        line(
            ctx.scene,
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
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
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
            let tsz = ts(ctx.text, theme.tokens.typography.scale.lg);
            let ssz = ts(ctx.text, theme.tokens.typography.scale.sm);
            let title_top = cy - 4.0 - self.m.line_h(tsz);
            let sub_top = cy + 2.0;
            let retry_top = sub_top + self.m.line_h(ssz) + 9.0;
            let (tw, _) = ctx.text.measure(title, tsz, None);
            ctx.text(
                title,
                &TextStyle::new(tsz).with_weight(TextWeight::Semibold),
                c.text_primary,
                Point::new(cx - f64::from(tw) / 2.0, title_top),
                None,
            );
            let (sw, _) = ctx.text.measure(subtitle, ssz, None);
            ctx.text(
                subtitle,
                &TextStyle::new(ssz),
                theme.text_dim,
                Point::new(cx - f64::from(sw) / 2.0, sub_top),
                None,
            );
            if self.source_error.is_some() && self.allow_live {
                let retry = "Use Refresh to try again.";
                let (rw, _) = ctx.text.measure(retry, ssz, None);
                ctx.text(
                    retry,
                    &TextStyle::new(ssz).with_weight(TextWeight::Medium),
                    theme.accent_primary,
                    Point::new(cx - f64::from(rw) / 2.0, retry_top),
                    None,
                );
            }
        } else {
            let msg = "No entries match the current filters.";
            let sz = ts(ctx.text, theme.tokens.typography.scale.base);
            let (mw, _) = ctx.text.measure(msg, sz, None);
            ctx.text(
                msg,
                &TextStyle::new(sz),
                theme.text_dim,
                Point::new(
                    cx - f64::from(mw) / 2.0,
                    cy - self.m.line_h(sz) / 2.0 - 1.25,
                ),
                None,
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
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        let c = &theme.tokens.colors;
        let tc = Self::theme_colors(theme);
        let sel = self.effective_selected();

        ctx.scene
            .push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &reg.list_body);
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
                ctx.fill_rect(band, tint(theme.accent_primary, 46));
                ctx.fill_rect(
                    Rect::new(band.x0, band.y0, band.x0 + 3.0, band.y1),
                    theme.accent_primary,
                );
            } else if is_hover {
                ctx.fill_rect(band, c.lift(0.35));
            } else if pos % 2 == 1 {
                ctx.fill_rect(band, ui::tokens::Colors::hairline_on(c.surfaces.raised, 6));
            }

            let cols = column_x(row, self.m.k);
            // severity dot
            fill_circle(
                ctx.scene,
                t,
                Point::new(cols.dot_cx, row.center().y),
                4.5,
                col,
            );
            // time (mono, dim)
            let tsz = ts(ctx.text, theme.tokens.typography.scale.sm);
            let timestamp = if cols.compact {
                e.time_hms()
            } else {
                e.timestamp()
            };
            ctx.text(
                &timestamp,
                &TextStyle::new(tsz).with_family(FontChoice::Monospace),
                theme.text_dim,
                Point::new(cols.time_x, self.m.vcenter(row, tsz)),
                None,
            );
            // level tag (colored)
            ctx.text(
                e.priority.tag(),
                &TextStyle::new(ts(ctx.text, theme.tokens.typography.scale.xs))
                    .with_weight(TextWeight::Bold)
                    .with_family(FontChoice::Monospace),
                col,
                Point::new(
                    cols.tag_x,
                    self.m
                        .vcenter(row, ts(ctx.text, theme.tokens.typography.scale.xs)),
                ),
                None,
            );
            if !cols.compact {
                // Unit is promoted on desktop; compact rows reserve that
                // width for the message and expose the unit in detail/a11y.
                let unit_shown = ctx.text.elide_to_width(
                    e.unit_short(),
                    tsz,
                    ui::num::f32_of_f64(cols.unit_w - 6.0),
                );
                ctx.text(
                    &unit_shown,
                    &TextStyle::new(tsz).with_weight(TextWeight::Medium),
                    theme.text_primary,
                    Point::new(cols.unit_x, self.m.vcenter(row, tsz)),
                    None,
                );
            }
            // message
            let msg_shown =
                ctx.text
                    .elide_to_width(&e.message, tsz, ui::num::f32_of_f64(cols.msg_w));
            let msg_c = if is_sel {
                theme.text_primary
            } else {
                tint(theme.text_primary, 220)
            };
            ctx.text(
                &msg_shown,
                &TextStyle::new(tsz),
                msg_c,
                Point::new(cols.msg_x, self.m.vcenter(row, tsz)),
                None,
            );
        }
        ctx.scene.pop_layer();
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
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        let c = &theme.tokens.colors;
        let d = reg.detail;
        ui::surface::card(
            ctx.scene,
            t,
            d,
            f64::from(theme.tokens.radii.lg),
            c.surfaces.raised,
        );

        ctx.scene
            .push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &d);
        let mut layout = DetailLayout {
            inner_x: d.x0 + 18.0,
            inner_w: d.width() - 36.0,
            y: d.y0 + 18.0,
            bottom: d.y1,
        };

        // On narrow windows the detail is a pushed pane; draw a Back
        // affordance at the top and inset the field content beneath it.
        if reg.narrow && reg.back_btn.width() > 0.0 {
            self.draw_back_button(ctx.scene, t, ctx.text, theme, reg.back_btn);
            layout.y = reg.back_btn.y1 + 14.0;
        }

        let Some(abs) = self.effective_selected() else {
            let msg = "Select an entry to inspect its fields.";
            let sz = ts(ctx.text, theme.tokens.typography.scale.base);
            ctx.text(
                msg,
                &TextStyle::new(sz),
                theme.text_dim,
                Point::new(layout.inner_x, layout.y.max(d.center().y)),
                None,
            );
            ctx.scene.pop_layer();
            return;
        };
        let Some(e) = self.entries.get(abs) else {
            ctx.scene.pop_layer();
            return;
        };

        Self::draw_detail_header(ctx.scene, t, ctx.text, theme, self.m, &mut layout, e);
        Self::draw_detail_fields(ctx.scene, t, ctx.text, theme, self.m, &mut layout, e);

        ctx.scene.pop_layer();
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
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        let c = &theme.tokens.colors;
        let tc = Self::theme_colors(theme);
        let inner_x = layout.inner_x;
        let inner_w = layout.inner_w;
        let col = e.priority.color(&tc);

        // Priority badge pill.
        let badge_label = e.priority.long().to_uppercase();
        let bsz = ts(ctx.text, theme.tokens.typography.scale.xs);
        let (blw, _) = ctx
            .text
            .measure_weighted(&badge_label, bsz, None, TextWeight::Bold);
        let badge = Rect::new(
            inner_x,
            layout.y,
            inner_x + f64::from(blw) + 24.0,
            layout.y + 22.0,
        );
        ctx.fill_round(badge, f64::from(theme.tokens.radii.full), tint(col, 235));
        ctx.text(
            &badge_label,
            &TextStyle::new(bsz).with_weight(TextWeight::Bold),
            theme.tokens.colors.on_surface(col).ink,
            Point::new(badge.x0 + 12.0, m.vcenter(badge, bsz)),
            None,
        );
        // transport tag to the right of the badge
        if !e.transport.is_empty() {
            ctx.text(
                &e.transport,
                &TextStyle::new(bsz)
                    .with_weight(TextWeight::Medium)
                    .with_family(FontChoice::Monospace),
                theme.text_dim,
                Point::new(badge.x1 + 10.0, m.vcenter(badge, bsz)),
                None,
            );
        }
        layout.y = badge.y1 + 14.0;

        // Unit title.
        let title_sz = ts(ctx.text, theme.tokens.typography.scale.lg);
        let title = ctx
            .text
            .elide_to_width(&e.unit, title_sz, ui::num::f32_of_f64(inner_w));
        ctx.text(
            &title,
            &TextStyle::new(title_sz).with_weight(TextWeight::Semibold),
            theme.text_primary,
            Point::new(inner_x, layout.y),
            None,
        );
        layout.y += m.line_h(title_sz) * 1.2;

        // Timestamp.
        let tssz = ts(ctx.text, theme.tokens.typography.scale.sm);
        ctx.text(
            &e.timestamp_full(),
            &TextStyle::new(tssz).with_family(FontChoice::Monospace),
            theme.text_dim,
            Point::new(inner_x, layout.y),
            None,
        );
        layout.y += m.line_h(tssz) * 1.28 + 6.0;

        // Message (wrapped).
        let msz = ts(ctx.text, theme.tokens.typography.scale.base);
        let (_, mh) = ctx.text.measure(&e.message, msz, Some(inner_w as f32));
        ctx.text.draw_text(
            ctx.scene,
            &e.message,
            msz,
            theme.text_primary,
            t * Affine::translate((inner_x, layout.y)),
            Some(inner_w as f32),
        );
        layout.y += f64::from(mh) + 14.0;

        // Divider.
        line(
            ctx.scene,
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
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
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
        let ksz = ts(ctx.text, theme.tokens.typography.scale.xs);
        let vsz = ts(ctx.text, theme.tokens.typography.scale.sm);
        for (k, v) in fields {
            if layout.y > layout.bottom - 14.0 {
                break;
            }
            if v.trim().is_empty() {
                continue;
            }
            ctx.text(
                &k,
                &TextStyle::new(ksz)
                    .with_weight(TextWeight::Semibold)
                    .with_family(FontChoice::Monospace),
                theme.text_dim,
                Point::new(inner_x, layout.y + 1.0),
                None,
            );
            let val = ctx
                .text
                .elide_to_width(&v, vsz, ui::num::f32_of_f64(inner_w - key_w - 8.0));
            ctx.text(
                &val,
                &TextStyle::new(vsz),
                theme.text_primary,
                Point::new(inner_x + key_w, layout.y),
                None,
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
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        let c = &theme.tokens.colors;
        let hovered = rect.contains(self.cursor);
        let bg = if hovered { c.lift(0.72) } else { c.lift(0.35) };
        ui::surface::card(ctx.scene, t, rect, f64::from(theme.tokens.radii.full), bg);
        // Left chevron.
        let cyp = rect.center().y;
        let cxp = rect.x0 + 18.0;
        line(
            ctx.scene,
            t,
            cxp + 4.0,
            cyp - 5.0,
            cxp - 1.0,
            cyp,
            theme.text_primary,
            1.6,
        );
        line(
            ctx.scene,
            t,
            cxp - 1.0,
            cyp,
            cxp + 4.0,
            cyp + 5.0,
            theme.text_primary,
            1.6,
        );
        let sz = ts(ctx.text, theme.tokens.typography.scale.sm);
        ctx.text(
            "Back",
            &TextStyle::new(sz).with_weight(TextWeight::Semibold),
            theme.text_primary,
            Point::new(rect.x0 + 30.0, self.m.vcenter(rect, sz)),
            None,
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
        if reg.narrow && reg.back_btn.width() > 0.0 && self.push.pane() == PushPane::Detail {
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

impl LogsApp {
    fn answer_resource_operation(
        &mut self,
        kind: OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &mut AutoCommit,
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
        self.perform_effect(operation, doc)?;
        if reveals_entry && self.narrow_at(self.rect.width()) {
            self.push.show_detail();
            self.dirty = true;
        }
        Ok(true)
    }
}

impl Widget for LogsApp {
    ui::provides!(
        dyn ui::history_mode::capability::DocumentHistory,
        dyn ui::widget::controls::ActionControls,
        dyn ui::resources::PublishesResources,
        dyn ui::undo::UndoPoints
    );

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

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        Ok(self
            .answer_resource_operation(request.kind, request.type_id, request.payload, doc, path)?
            .then(|| {
                ui::widget::ResourceOperationStart::Completed(
                    ui::tilo_actions::ActionOutput::default(),
                )
            }))
    }

    fn sync(&mut self, doc: &mut AppModel, root: &ModelPath) {
        let (doc, root) = ui::doc::document(doc, root);
        self.drain_export_portal();
        if let Some(filter) = self.shot_filter.take() {
            self.filter = filter;
            self.search_input.set_text(&self.filter.search);
            self.recompute_filtered();
        }
        self.search_input.sync(doc, root);

        // Optional live tail: throttled re-poll, never under --shots.
        if self.allow_live && self.live_tail && self.last_poll.elapsed() >= POLL_INTERVAL {
            self.last_poll = ui::time::now();
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
        } else if self.push.tick(false) {
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

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if self.route_search_field(event, ctx) {
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
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.focused = true;
                let (doc, _) = ui::doc::document(ctx.state, ctx.path);
                self.on_click(self.cursor, doc, ctx.handled);
            }
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } if key_event.state == ElementState::Pressed => {
                let (doc, _) = ui::doc::document(ctx.state, ctx.path);
                self.on_key(key_event, *ctx.modifiers, doc, ctx.handled);
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
        state: &mut AppModel,
        path: &ModelPath,
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
        let (doc, _) = ui::doc::document(state, path);
        if target == "logs.clear-filters" && activate {
            self.commit_view(doc, &LogsAction::ClearFilters);
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
            self.push.show_master();
            self.dirty = true;
            return true;
        }
        if let Some(value) = target.strip_prefix("logs.priority:")
            && activate
            && let Ok(value) = value.parse::<i64>()
        {
            self.commit_view(doc, &LogsAction::SetPriority { value });
            self.scroll = 0.0;
            self.dirty = true;
            return true;
        }
        if let Some(value) = target.strip_prefix("logs.unit-choice:")
            && activate
        {
            self.commit_view(
                doc,
                &LogsAction::SetUnit {
                    value: value.to_owned(),
                },
            );
            self.popup = None;
            self.scroll = 0.0;
            self.dirty = true;
            return true;
        }
        if let Some(value) = target.strip_prefix("logs.boot-choice:")
            && activate
        {
            self.commit_view(
                doc,
                &LogsAction::SetBoot {
                    value: value.to_owned(),
                },
            );
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
            self.select_abs(doc, abs);
            if self.narrow_at(self.rect.width()) {
                self.push.show_detail();
            }
            return true;
        }
        false
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

impl ui::undo::UndoPoints for LogsApp {
    fn needs_commit(&self) -> bool {
        self.search_input.needs_commit()
    }
    fn on_commit(&mut self) {
        self.search_input.on_commit();
    }
}

impl ui::resources::PublishesResources for LogsApp {
    /// Standalone / `--shots` / unit tests publish the full tree. When the
    /// framework hosts the surface, [`tilo_resource::publish`] already owns
    /// the journal door — keep only live journal entry rows.
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        if self.is_hosted() {
            return crate::resource::live_extras(self);
        }
        self.publish_resources(doc)
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        if self.is_hosted() && id == crate::actions::JOURNAL_RESOURCE_ID {
            return None;
        }
        fn find(resources: &[Resource], id: &str) -> Option<Resource> {
            for resource in resources {
                if resource.id.as_str() == id {
                    return Some(resource.clone());
                }
                if let Some(found) = find(&resource.children, id) {
                    return Some(found);
                }
            }
            None
        }
        find(&self.publish_resources(doc), id)
    }
}

impl ui::widget::controls::ActionControls for LogsApp {
    fn take_control_invocation(&mut self) -> Option<ui::tilo_actions::ControlInvocation> {
        self.pending_control_invocation
            .take()
            .or_else(|| self.search_input.take_control_invocation())
    }
}

impl ui::history_mode::capability::DocumentHistory for LogsApp {
    fn document_history_definition(&self) -> Option<&'static ui::history_mode::HistoryDefinition> {
        Some(&crate::framework_history::HISTORY)
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
    fn route_search_field(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
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
                    && !ctx.modifiers.state().control_key()
                    && !ctx.modifiers.state().super_key()
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
            let (doc, _) = ui::doc::document(ctx.state, ctx.path);
            self.commit_view(
                doc,
                &LogsAction::SetSearch {
                    value: String::new(),
                },
            );
            self.scroll = 0.0;
            self.dirty = true;
            *ctx.handled = true;
            return true;
        }

        let route_to_search = self.search_input.is_focused()
            || matches!(event, WindowEvent::CursorMoved { .. })
            || (left_press && search_rect.contains(self.cursor));
        if route_to_search {
            let mut input_handled = false;
            self.search_input.handle_event(
                event,
                &mut ui::widget::EventCtx {
                    handled: &mut input_handled,
                    ..ctx.reborrow(ctx.path, ctx.node_id)
                },
            );
            if self.search_input.take_dirty().is_some() {
                let (doc, _) = ui::doc::document(ctx.state, ctx.path);
                let query = self.search_input.value().to_owned();
                self.commit_view(doc, &LogsAction::SetSearch { value: query });
                self.scroll = 0.0;
                self.dirty = true;
                input_handled = true;
            }
            if input_handled && !matches!(event, WindowEvent::CursorMoved { .. }) {
                *ctx.handled = true;
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
            f64::from(theme.tokens.radii.md),
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
        let mut ctx = PaintCtx::new(scene, transform, tr, theme);
        let c = &theme.tokens.colors;
        let active_val = match self.popup {
            Some(Popup::Unit) => self.filter.unit.clone(),
            Some(Popup::Boot) => self.filter.boot.clone(),
            Some(Popup::Actions) | None => String::new(),
        };
        let sz = ts(ctx.text, theme.tokens.typography.scale.sm);
        for (irect, val, label) in &reg.dropdown {
            let is_hover = irect.contains(self.cursor);
            let is_active = &active_val == val;
            if is_hover {
                ctx.fill_rect(*irect, c.lift(0.72));
            } else if is_active {
                ctx.fill_rect(*irect, tint(theme.accent_primary, 40));
            }
            let lc = if is_active {
                theme.accent_primary
            } else {
                theme.text_primary
            };
            let shown =
                ctx.text
                    .elide_to_width(label, sz, ui::num::f32_of_f64(irect.width() - 30.0));
            ctx.text(
                &shown,
                &TextStyle::new(sz).with_weight(if is_active {
                    TextWeight::Semibold
                } else {
                    TextWeight::Regular
                }),
                lc,
                Point::new(irect.x0 + 12.0, self.m.vcenter(*irect, sz)),
                None,
            );
            if is_active {
                // check mark
                let cy = irect.center().y;
                line(
                    ctx.scene,
                    transform,
                    irect.x1 - 18.0,
                    cy,
                    irect.x1 - 14.0,
                    cy + 4.0,
                    theme.accent_primary,
                    1.6,
                );
                line(
                    ctx.scene,
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
    fn on_click(&mut self, pt: Point, doc: &mut AutoCommit, handled: &mut bool) {
        // 1. An open dropdown captures the click first.
        if self.popup.is_some() {
            let reg = self.regions();
            for (irect, val, _label) in &reg.dropdown {
                if irect.contains(pt) {
                    match self.popup {
                        Some(Popup::Unit) => {
                            self.commit_view(doc, &LogsAction::SetUnit { value: val.clone() });
                        }
                        Some(Popup::Boot) => {
                            self.commit_view(doc, &LogsAction::SetBoot { value: val.clone() });
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
            self.push.show_master();
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
                self.commit_view(
                    doc,
                    &LogsAction::SetPriority {
                        value: priority.level(),
                    },
                );
                self.scroll = 0.0;
                self.dirty = true;
                *handled = true;
                return;
            }
        }

        // 6. Clear-filters button.
        if !self.filter.is_clear() && reg.clear_btn.contains(pt) {
            self.commit_view(doc, &LogsAction::ClearFilters);
            self.scroll = 0.0;
            self.dirty = true;
            *handled = true;
            return;
        }

        // 7. List rows → select. On a narrow window this also pushes the
        // detail pane in (single-pane master-detail navigation).
        if let Some(abs) = self.row_at(&reg, pt) {
            self.select_abs(doc, abs);
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
        doc: &mut AutoCommit,
        handled: &mut bool,
    ) {
        let ctrl = modifiers.state().control_key();
        match &key_event.logical_key {
            Key::Named(NamedKey::ArrowDown) => {
                self.move_selection(doc, 1);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.move_selection(doc, -1);
                *handled = true;
            }
            Key::Named(NamedKey::PageDown) => {
                self.move_selection(doc, 10);
                *handled = true;
            }
            Key::Named(NamedKey::PageUp) => {
                self.move_selection(doc, -10);
                *handled = true;
            }
            Key::Named(NamedKey::Home) => {
                self.move_selection(doc, isize::MIN / 2);
                *handled = true;
            }
            Key::Named(NamedKey::End) => {
                self.move_selection(doc, isize::MAX / 2);
                *handled = true;
            }
            Key::Named(NamedKey::Escape) => {
                let narrow_in_detail =
                    self.narrow_at(self.rect.width()) && self.push.pane() == PushPane::Detail;
                if self.popup.is_some() {
                    self.popup = None;
                } else if narrow_in_detail {
                    // Back out of the pushed detail pane to the list.
                    self.push.show_master();
                } else if !self.filter.search.is_empty() {
                    self.commit_view(
                        doc,
                        &LogsAction::SetSearch {
                            value: String::new(),
                        },
                    );
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
                            self.commit_view(doc, &LogsAction::ClearFilters);
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

    /// Facets and shared filter actions narrow the entry list and write the
    /// collaborative view document.
    #[test]
    fn shared_filters_narrow_entries_and_write_the_document() {
        // Facets collect units and two boots.
        let app = LogsApp::new(sample_entries(), false);
        assert!(app.units.contains(&"shell.service".to_string()));
        assert_eq!(app.boots.len(), 2);

        // The priority threshold filters entries.
        {
            let (mut app, mut doc) = synced_app();
            let total = app.filtered.len();
            app.commit_view(
                &mut doc,
                &LogsAction::SetPriority {
                    value: Priority::Err.level(),
                },
            );
            let errs = app.filtered.len();
            assert!(errs < total, "errors-only must show fewer than all");
            assert!(errs > 0);
            for &i in &app.filtered {
                assert!(app.entries[i].priority.level() <= 3);
            }
            assert_eq!(crate::doc::ViewDoc::filter(&doc).min_priority, 3);
        }
        // Search narrows results.
        {
            let (mut app, mut doc) = synced_app();
            app.commit_view(
                &mut doc,
                &LogsAction::SetSearch {
                    value: "iroh".to_string(),
                },
            );
            assert_eq!(app.search_input.value(), "iroh");
            assert!(!app.filtered.is_empty());
            for &i in &app.filtered {
                let e = &app.entries[i];
                let hit = e.message.to_lowercase().contains("iroh")
                    || e.unit.to_lowercase().contains("iroh");
                assert!(hit);
            }
            assert_eq!(crate::doc::ViewDoc::filter(&doc).search, "iroh");
        }
        // The unit filter restricts to one unit.
        {
            let (mut app, mut doc) = synced_app();
            app.commit_view(
                &mut doc,
                &LogsAction::SetUnit {
                    value: "kernel".to_string(),
                },
            );
            assert!(
                app.filtered
                    .iter()
                    .all(|&i| app.entries[i].unit == "kernel")
            );
            assert_eq!(crate::doc::ViewDoc::filter(&doc).unit, "kernel");
        }
        // The boot filter syncs.
        {
            let (mut app, mut doc) = synced_app();
            let boot = app.boots[1].0.clone();
            app.commit_view(
                &mut doc,
                &LogsAction::SetBoot {
                    value: boot.clone(),
                },
            );
            assert!(app.filtered.iter().all(|&i| app.entries[i].boot_id == boot));
            assert_eq!(crate::doc::ViewDoc::filter(&doc).boot, boot);
        }
        // A document filter action updates the widget through the ActionSink.
        {
            let (mut app, mut doc) = synced_app();
            app.commit_view(
                &mut doc,
                &LogsAction::SetUnit {
                    value: "kernel".to_string(),
                },
            );
            assert_eq!(app.filter.unit, "kernel");
            assert!(
                app.filtered
                    .iter()
                    .all(|&index| app.entries[index].unit == "kernel")
            );
            assert!(
                doc.keys(&ROOT).next().is_some(),
                "document filter actions must write shared state"
            );
        }
    }

    /// Selection auto-lands on an error or worse and arrow keys move the
    /// shared selection.
    #[test]
    fn selection_auto_lands_on_errors_and_moves_through_the_document() {
        let (mut app, mut doc) = synced_app();
        // Auto-selection lands on an error or worse.
        let sel = app.effective_selected().expect("an entry is auto-selected");
        assert!(app.entries[sel].priority.level() <= Priority::Err.level());

        // Arrow navigation commits the shared selection.
        let before = app.effective_selected().unwrap();
        app.move_selection(&mut doc, 1);
        let after = app.effective_selected().unwrap();
        assert_ne!(before, after);
        assert_eq!(
            app.selected_entry.as_deref(),
            app.entries
                .get(after)
                .map(actions::entry_resource_id)
                .as_deref()
        );
        assert_eq!(
            crate::doc::ViewDoc::selected_entry(&doc).as_deref(),
            app.selected_entry.as_deref()
        );
    }

    /// Region geometry adapts from desktop to medium to phone widths.
    #[test]
    fn regions_adapt_from_desktop_to_medium_to_phone_width() {
        // Content spans to the window bottom.
        {
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
        // Medium-width filter controls wrap without clipping.
        {
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
        // Phone rows prioritize time, level and message.
        {
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
        // The phone actions menu exposes refresh, follow and export.
        {
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
    }

    /// Portal export writes the snapshot chosen before the picker opened and
    /// announces a write failure without path details.
    #[test]
    fn portal_export_writes_the_frozen_snapshot_or_announces_failure() {
        // Success: the snapshot selected before the picker is written.
        {
            let mut app = LogsApp::new(sample_entries().into_iter().take(2).collect(), true);
            let path = std::env::temp_dir().join(format!(
                "tilo-logs-portal-export-{}-{}.jsonl",
                std::process::id(),
                app.entries[0].realtime_us
            ));
            let _ = std::fs::remove_file(&path);
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(PortalOutcome::Opened(path.clone()));
            ui::host::scoped(&services, || {
                app.perform_effect(LogsOperation::ExportFiltered, &mut AutoCommit::new())
                    .unwrap();
            });
            assert_eq!(
                recorder.pick_requests()[0].suggested_name(),
                Some("journal-filtered.jsonl")
            );
            // Entries arriving after the picker opened are not part of the export.
            app.entries.extend(sample_entries().into_iter().skip(2));
            app.drain_export_portal();
            assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 2);
            assert_eq!(
                app.export_status.as_deref(),
                Some("Exported 2 log entries.")
            );
            std::fs::remove_file(path).unwrap();
        }
        // Failure: announced without path details.
        {
            let mut app = LogsApp::new(sample_entries().into_iter().take(1).collect(), true);
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(PortalOutcome::Opened(std::env::temp_dir()));
            ui::host::scoped(&services, || {
                app.perform_effect(LogsOperation::ExportFiltered, &mut AutoCommit::new())
                    .unwrap();
            });
            app.drain_export_portal();
            assert_eq!(app.export_status.as_deref(), Some("Export failed."));
            let tree = app.accessibility_tree();
            assert!(tree.children.iter().any(|node| {
                node.name.as_deref() == Some("Export failed.")
                    && node.live == Some(Politeness::Polite)
            }));
        }
    }

    /// Hand-painted header controls do real work and are exposed to
    /// accessibility: refresh queues the root operation, Clear filters resets,
    /// and unknown targets are rejected.
    #[test]
    fn header_controls_act_through_production_paths_and_accessibility() {
        // Refresh queues the exported root operation.
        {
            let mut app = LogsApp::new(sample_entries(), true);
            app.request_journal_operation("refresh");
            let invocation = ui::widget::controls::take_control_invocation(&mut app)
                .expect("refresh queues an inert control invocation");
            assert_eq!(invocation.address.resource, actions::JOURNAL_RESOURCE_ID);
            assert_eq!(invocation.address.action, "refresh");
            assert!(app.source_loading);
        }
        // Clear filters lives in the header and resets.
        {
            let (mut app, mut doc) = synced_app();
            // A clear filter set means no Clear-filters control is laid out.
            assert!(app.filter.is_clear());
            assert_eq!(app.regions().clear_btn, Rect::ZERO);

            // Activate a filter (errors-or-worse threshold).
            app.commit_view(
                &mut doc,
                &LogsAction::SetPriority {
                    value: Priority::Err.level(),
                },
            );
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
            app.on_click(reg.clear_btn.center(), &mut doc, &mut handled);
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
        // Accessibility exposes hand-painted controls and rejects unknown targets.
        {
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
}
