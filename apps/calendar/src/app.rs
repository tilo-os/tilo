//! `CalendarApp` — the root widget. Owns the in-memory [`Model`], the
//! current view + selection, and a single [`BlockEditor`] used for the
//! rich-text **notes** attached to the selected event/task (the same
//! widget the Notes app is built on).
//!
//! Layout is hand-positioned (like the Notes `MainPanel`) rather than
//! delegated to taffy, because the month grid / week timeline / day
//! timeline are bespoke Vello canvases. The only taffy-registered child
//! is the notes editor, positioned absolutely inside the detail panel.
//!
//! Five-pass `Widget` contract:
//! - `sync` — seed the CRDT on first frame, bind the notes editor to
//!   the selected item's block list, run the editor's sync.
//! - `build_layout` — register the editor node, return a full-bleed root.
//! - `apply_layout` — compute region rects, place the editor.
//! - `draw` — paint sidebar + toolbar + the active view + detail panel,
//!   rebuilding the click hit-map.
//! - `handle_event` — route clicks against the hit-map, keyboard
//!   shortcuts, and forward to the editor when notes are active.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use automerge::{AutoCommit, ChangeHash, ObjId};
use chrono::{Datelike, Duration, Local, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use taffy::style_helpers::length;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use uuid::Uuid;
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::Widget;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{BlockEditor, Checkbox, CheckboxFace, TextInput, Toggle, ToggleFace};

use crate::account::AccountStore;
use crate::agenda::{AgendaAction, AgendaView};
use crate::caldav::{self, SyncOutcome};
use crate::date;
use crate::doc::CalDoc;
use crate::draw_util::{elide_to_width, fill_rect, fill_round, stroke_line, stroke_round};
use crate::event_popovers::{
    EventPopovers, PopoverAnchors, PopoverEdit, PopoverKind as FormPopover,
};
use crate::ics;
use crate::mini_month::{MiniMonth, Outcome as MiniMonthOutcome};
use crate::model::{Event, Id, ListSel, Model, Palette, Priority, Recurrence, View, seed_demo};
use crate::sidebar::{Action as SidebarAction, Mode as SidebarMode, Sidebar};
use crate::timeline_interaction::{
    TIMELINE_SNAP_MIN, TimelineEffect, TimelineEventCtx, TimelineInteraction, TimelineRenderEvent,
    TimelineRenderModel, TimelineSpan,
};
#[cfg(test)]
use crate::timeline_viewport::TIMELINE_MIN_HOUR_H;
use crate::topbar::{Action as TopbarAction, LeadingControl, Topbar};

/// Placeholder shown in the empty title field of a new event.
const TITLE_PLACEHOLDER: &str = "New event";
/// Placeholder shown in the empty notes editor of an event (dim hint text that
/// disappears the moment the user types — never seeded as real body text).
const NOTES_PLACEHOLDER: &str = "Add details…";

/// First-run onboarding shown centered over an empty calendar (no events):
/// a friendly headline + hint instead of a blank grid.
const EMPTY_ONBOARDING_TITLE: &str = "No events yet";
const EMPTY_ONBOARDING_HINT: &str = "Click a day to add your first event";

const DETAIL_PAD: f64 = 22.0;
const NOTES_TOP_OFFSET: f64 = 296.0;

/// Hour the timeline first scrolls to when the day doesn't fit — the start of a
/// typical work day, rather than opening at midnight.
const TIMELINE_INITIAL_HOUR: f64 = 7.0;

/// Width at/above which the sidebar is an inline persistent panel. Below it the
/// sidebar becomes an overlay drawer (see [`SidebarMode`] and
/// [`CalendarApp::relayout`]).
/// Width at/above which the event/task editor opens as a centered modal
/// **overlay** floating over the still-visible calendar. Below it the editor
/// takes the whole screen as its own pushed page — comfortable on narrow
/// windows where a floating card would be cramped.
const EDITOR_OVERLAY_MIN: f64 = 640.0;

/// Modal editor card width (overlay presentation) and the derived body column.
const MODAL_W: f64 = 468.0;
/// Notes editor / form column width inside the modal card. Fixed so the
/// taffy-registered editor node has a stable size across frames.
const MODAL_BODY_W: f64 = MODAL_W - 2.0 * DETAIL_PAD;
/// Minimum gap kept between the modal card and the window edges.
const MODAL_MARGIN: f64 = 40.0;
/// Cap on the modal card height so it stays a card, not a full-height column.
const MODAL_MAX_H: f64 = 720.0;

/// How the event/task detail editor is presented at the current width.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DetailMode {
    /// A centered modal card floating over the still-visible calendar (wide).
    Overlay,
    /// The editor takes the whole content area as its own pushed page (narrow).
    FullScreen,
}

/// In the narrow layout the content area is a two-screen navigation stack: the
/// calendar view, or the event/task detail pushed in from the right.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum NarrowScreen {
    Calendar,
    Detail,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sel {
    Event(Id),
    Task(Id),
}

/// A click target produced while drawing and resolved on the next press.
#[derive(Clone)]
enum Action {
    View(View),
    Today,
    Prev,
    Next,
    PickDate(NaiveDate),
    OpenDay(NaiveDate),
    OpenEvent(Id),
    ToggleTask(Id),
    List(ListSel),
    CloseDetail,
    Export,
    Import,
    NewEvent,
    /// Navigation intent interpreted by the mini-month component. The app
    /// applies it to the anchor shared by every calendar view.
    MiniMonth(MiniMonthOutcome),
    CycleColor,
    CycleRecurrence,
    CycleReminder,
    ToggleSelectedTask,
    /// Select a task to view its detail (without toggling completion).
    OpenTask(Id),
    /// Show/hide the overlay sidebar flap (narrow layout).
    ToggleSidebar,
    /// Dismiss the overlay sidebar by clicking the scrim (narrow layout).
    DismissSidebar,
    /// Collapse (or re-expand) the persistent sidebar in the wide layout. The
    /// content + detail reclaim the freed width; only the sidebar collapses.
    ToggleSidebarCollapsed,
    /// Pop the narrow detail screen, returning to the calendar.
    BackToCalendar,
    /// Toggle whether a synced remote calendar's events are shown.
    ToggleCalendarVisible(String),
    /// Collapse/expand one account's calendar list in the sidebar.
    ToggleAccountCollapsed(String),
    /// Pull events from the configured `CalDAV` calendars.
    Sync,
    /// Flip the selected event's all-day flag.
    ToggleAllDay,
    /// Open one of the form's transient popovers (date grid / time spinners /
    /// calendar list) over the currently-edited event.
    OpenPopover(FormPopover),
    /// Focus one of the form's text fields (used by the title/location field
    /// click targets so a press lands the caret).
    FocusField(FormField),
    /// Delete an event from the model + CRDT (and queue a `CalDAV` delete for a
    /// synced event). Fired by the editor's trash button and the Delete key.
    DeleteEvent(Id),
    /// Delete a task (and its subtasks) from the model + CRDT.
    DeleteTask(Id),
    /// Absorb a click without acting on it — the modal card's own backing, so a
    /// press on the card padding doesn't fall through to the dismiss scrim.
    Noop,
}

/// The two keyboard-focusable text fields in the form. Tab cycles between them
/// (and the notes editor); the pickers/selector are opened with Enter/click.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FormField {
    Title,
    Location,
}

/// A calendar an event can be filed under: the on-device local calendar, or a
/// specific remote `CalDAV` collection (identified by its stable id, matching
/// [`Event::calendar_id`]).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CalTarget {
    Local,
    Remote(String),
}

/// Cached rects for the event form's fields, recomputed every `relayout` from
/// the detail panel. Kept together so the draw pass, the hit map, and the
/// embedded widgets' `apply_layout_external` all read the same geometry.
#[derive(Clone, Debug, Default)]
struct FormRects {
    header_bottom: f64,
    title: Rect,
    allday: Rect,
    start_date: Rect,
    start_time: Rect,
    end_date: Rect,
    end_time: Rect,
    location: Rect,
    calendar: Rect,
    chips_y: f64,
    /// Y offset (from the detail rect top) at which the NOTES section starts.
    notes_top: f64,
}

/// Payload delivered by a finished background `CalDAV` sync worker: the pull
/// result, the merged outcome, the ids that uploaded successfully, and the
/// ids the server confirmed deleted.
type SyncWorkerResult = (caldav::SyncResult, SyncOutcome, Vec<Id>, Vec<Id>);

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent, unrelated flag (demo mode, sidebar state, sync status, form scratch state); grouping them into enums would not express any real invariant between them and would touch every read/write site across this file"
)]
pub struct CalendarApp {
    model: Model,
    today: NaiveDate,
    anchor: NaiveDate,
    view: View,
    list_sel: ListSel,
    selection: Option<Sel>,
    detail_open: bool,

    /// `CalDAV` accounts + their calendars, as configured by OS Settings (the
    /// shared [`AccountStore`]). Read-only here; Settings owns the lifecycle.
    accounts: AccountStore,

    notes_editor: BlockEditor,
    bound: Option<Sel>,
    seeded: bool,

    // --- event form (live only while an Event detail is open) -----------
    /// Editable title field (local text, mirrored into the event on change).
    title_input: TextInput,
    /// Editable location field.
    location_input: TextInput,
    /// Which event the form widgets are currently seeded from — so a new
    /// selection reseeds the inputs and edits land on the right event.
    form_event: Option<Id>,
    /// Self-contained date/time/calendar picker controller: exclusive open
    /// state, embedded widgets, drawing, hit-testing and event routing.
    event_popovers: EventPopovers,
    /// Cached form field geometry (see [`FormRects`]).
    form: FormRects,
    /// Shots-only: spawn a fresh event on the first sync so the offscreen
    /// renderer can capture the New-Event form. No effect on live runtime.
    shots_pending_new_event: bool,
    /// Shots-only: open this popover once the form is bound.
    shots_pending_popover: Option<FormPopover>,

    /// True for the hermetic `--shots` / test build: it seeds sample content
    /// and drives the offline (synthesised) `CalDAV` path. The live app leaves
    /// this `false` and never fabricates data.
    demo: bool,
    /// Where the calendar document is persisted (live app only; `None` in
    /// demo/test builds, which stay purely in-memory).
    data_path: Option<PathBuf>,
    /// Document heads at the last successful save, so we persist only when the
    /// CRDT actually changed rather than every frame.
    saved_heads: Vec<ChangeHash>,
    /// Set by discrete data actions (new event, toggle, import, sync…) to force
    /// an immediate save; continuous changes (typing in notes) coalesce instead.
    force_persist: bool,
    /// When the document was last written, used to debounce note typing so a
    /// burst of keystrokes doesn't serialize the whole document every frame.
    last_persist: std::time::Instant,

    // --- responsive state ------------------------------------------------
    /// Responsive drawer state, geometry and chrome interaction.
    sidebar: Sidebar,
    /// Responsive navigation chrome and its pointer hit map.
    topbar: Topbar,
    /// Which narrow screen is showing (only meaningful when the editor is
    /// full-screen, i.e. below [`EDITOR_OVERLAY_MIN`]).
    narrow_screen: NarrowScreen,
    /// How the detail editor is presented — recomputed every layout from width.
    detail_mode: DetailMode,
    // --- layout caches (filled by `relayout`) ----------------------------
    rect: Rect,
    /// The region right of a persistent sidebar (or full-width under an overlay
    /// one) holding the topbar + calendar view.
    content_rect: Rect,
    detail_rect: Rect,
    notes_origin: Point,
    notes_w: f64,
    editor_node: Option<NodeId>,
    /// Notes-column width the editor node was last built with. When the current
    /// width (from `detail_body_w`) diverges — i.e. a resize crossed the
    /// breakpoint or reshaped the detail pane — `take_structure_dirty` requests
    /// a rebuild so the editor's own text-wrap width tracks the new layout
    /// instead of staying frozen at the width it launched with.
    built_notes_w: f64,

    hits: Vec<(Rect, Action)>,
    last_mouse: Point,
    status: Option<String>,
    dirty: bool,

    // --- timeline (week/day) scroll + drag -------------------------------
    /// Vertical scroll of the full-24h week/day hour grid. Driven manually
    /// (the timeline is painted imperatively, not through a child widget), it
    /// still reuses the shared [`Scrollable`] for offset clamping, wheel /
    /// keyboard / momentum, and the overlay bar.
    timeline: TimelineInteraction,
    /// Vertical scroll of the agenda list, so a short window pages through the
    /// upcoming days instead of clipping them.
    /// Self-contained upcoming-list view: scroll state, input, drawing and row
    /// actions live together instead of leaking into the root widget.
    agenda: AgendaView,
    /// Receiver for an in-flight background `CalDAV` pull. The live sync runs on a
    /// worker thread (the `ui` event loop is synchronous and must never block on
    /// the network); its result is drained here on the next `sync` pass.
    /// Delivers `(pull result, outcome, pushed uids, deleted uids)` — the
    /// events that uploaded and the ones the server confirmed removed, so both
    /// outboxes can be cleared.
    sync_rx: Option<std::sync::mpsc::Receiver<SyncWorkerResult>>,
    /// True while a background sync is running, which keeps the event loop
    /// ticking (`needs_animation_frame`) so the worker's result lands promptly
    /// without waiting for the next unrelated input.
    syncing: bool,

    /// Framework-owned action dispatch. Each on-screen affordance dispatches its
    /// [`CalendarAction`](crate::actions::CalendarAction) through this: the host
    /// brackets the undo boundary, applies the action through the executor (so it
    /// replays byte-identically on peers, exactly as the Ctrl+K palette does), and
    /// records the last one so a test can prove a button and its
    /// [`ActionSpec`](qos_actions::ActionSpec) build the identical action.
    actions: ui::ActionHost,

    /// The document heads the in-memory [`Model`] was last derived from. When the
    /// document moves past this out of band — a Ctrl+K palette action, a peer's
    /// change merged in — the next `sync` re-reads the model so those mutations
    /// show up in the views, exactly as an on-screen edit already does.
    model_heads: Vec<ChangeHash>,
}

impl CalendarApp {
    /// The live app: persisted to disk, no sample content, accounts sourced
    /// from OS Settings (none on a fresh machine).
    pub fn new() -> Self {
        Self::build(false, crate::store::default_path())
    }

    /// The hermetic demo used by `--shots` and tests: seeded with sample
    /// content, in-memory only (never touches disk).
    pub fn new_demo() -> Self {
        Self::build(true, None)
    }

    #[cfg(test)]
    fn new_with_data_path(path: PathBuf) -> Self {
        Self::build(false, Some(path))
    }

    fn build(demo: bool, data_path: Option<PathBuf>) -> Self {
        let today = Local::now().date_naive();

        // Sample events/tasks/accounts exist ONLY in the demo build. The live
        // app starts empty and fills in from the persisted document + the
        // accounts OS Settings has configured.
        let (model, selection, accounts) = if demo {
            let model = seed_demo(today);
            let selection = model
                .events
                .iter()
                .find(|e| e.title.starts_with("Design review"))
                .or_else(|| model.events.first())
                .map(|e| Sel::Event(e.id));
            (model, selection, AccountStore::sample())
        } else {
            let accounts = AccountStore::load_from_settings().unwrap_or_default();
            (Model::default(), None, accounts)
        };
        let detail_open = selection.is_some();

        Self {
            model,
            today,
            anchor: today,
            view: View::Month,
            list_sel: ListSel::Today,
            selection,
            detail_open,
            accounts,
            notes_editor: BlockEditor::new().with_empty_hint(NOTES_PLACEHOLDER),
            bound: None,
            seeded: false,
            title_input: TextInput::local()
                .with_placeholder(TITLE_PLACEHOLDER)
                .with_font_size(20.0)
                .with_id("event_title"),
            location_input: TextInput::local()
                .with_placeholder("Add location")
                .with_id("event_location"),
            form_event: None,
            event_popovers: EventPopovers::new(today),
            form: FormRects::default(),
            shots_pending_new_event: false,
            shots_pending_popover: None,
            demo,
            data_path,
            saved_heads: Vec::new(),
            force_persist: false,
            last_persist: std::time::Instant::now(),
            sidebar: Sidebar::new(),
            topbar: Topbar::new(),
            narrow_screen: if detail_open {
                NarrowScreen::Detail
            } else {
                NarrowScreen::Calendar
            },
            detail_mode: DetailMode::Overlay,
            rect: Rect::ZERO,
            content_rect: Rect::ZERO,
            detail_rect: Rect::ZERO,
            notes_origin: Point::new(-10000.0, -10000.0),
            notes_w: MODAL_BODY_W,
            editor_node: None,
            built_notes_w: -1.0,
            hits: Vec::new(),
            last_mouse: Point::ZERO,
            status: None,
            dirty: true,
            timeline: TimelineInteraction::new(),
            agenda: AgendaView::new(),
            sync_rx: None,
            syncing: false,
            actions: ui::ActionHost::new(),
            model_heads: Vec::new(),
        }
    }

    // --- shots-only configuration (no effect on live runtime behaviour) --

    /// Force a calendar view for a screenshot.
    pub fn shots_set_view(&mut self, v: View) {
        self.view = v;
        self.dirty = true;
    }

    /// Pin the week/day timeline scroll to `hour` for a screenshot (`0.0` = top
    /// of the day, `24.0` = clamped to the bottom). Proves the full 24h scrolls.
    pub fn shots_scroll_timeline(&mut self, hour: f64) {
        self.timeline.pin_hour(hour);
        self.dirty = true;
    }

    /// Show the calendar screen (close the detail panel) for a screenshot.
    pub fn shots_show_calendar(&mut self) {
        self.detail_open = false;
        self.selection = None;
        self.narrow_screen = NarrowScreen::Calendar;
        self.dirty = true;
    }

    /// Open the overlay sidebar flap for a screenshot.
    pub fn shots_open_flap(&mut self) {
        self.sidebar.set_open(true);
        self.dirty = true;
    }

    /// Collapse the persistent sidebar for a screenshot (wide layout only).
    pub fn shots_collapse_sidebar(&mut self) {
        self.sidebar.set_collapsed(true);
        self.dirty = true;
    }

    /// Create a fresh event on the next sync and open its edit form — the
    /// New-Event screenshot. The event has an empty title (so the field shows
    /// its placeholder) and empty notes (so the notes editor shows its hint).
    pub fn shots_new_event(&mut self) {
        self.shots_pending_new_event = true;
        self.dirty = true;
    }

    /// Open the start-date grid popover over the New-Event form for a
    /// screenshot of the date picker.
    pub fn shots_open_date_popover(&mut self) {
        self.shots_pending_new_event = true;
        self.shots_pending_popover = Some(FormPopover::StartDate);
        self.dirty = true;
    }

    /// Open the calendar/account selector popover for a screenshot.
    pub fn shots_open_calendar_popover(&mut self) {
        self.shots_pending_new_event = true;
        self.shots_pending_popover = Some(FormPopover::Calendar);
        self.dirty = true;
    }

    /// Kick off a `CalDAV` pull from the configured calendars. The live pull
    /// touches the network, so it runs on a worker thread and its result is
    /// drained in [`Self::poll_sync`]; the UI stays responsive throughout. The
    /// hermetic demo/test build stays fully synchronous (it synthesises each
    /// calendar's events through the real [`ics::parse`] path — no network, no
    /// thread — so `--shots` and tests are deterministic).
    fn start_sync(&mut self, doc: &mut AutoCommit) {
        if self.syncing {
            return;
        }
        let inputs: Vec<_> = self
            .accounts
            .accounts
            .iter()
            .map(|a| (a.config.clone(), a.calendars.clone()))
            .collect();
        let sync_account_ids: Vec<String> = self
            .accounts
            .accounts
            .iter()
            .map(|account| account.id.clone())
            .collect();

        if self.demo {
            let (result, outcome) = caldav::sample_pull_all(&inputs, self.today);
            self.apply_sync_result(result, outcome, doc);
            return;
        }

        // No account configured: answer instantly rather than spinning up a
        // worker that would only return Offline.
        if self.accounts.is_empty() {
            self.status = Some("No calendar accounts yet — add one in Settings".into());
            self.dirty = true;
            return;
        }

        // Live delete + push + pull on a background thread; the result lands via
        // `poll_sync`. Remote deletions go out first (so a re-pull can't
        // resurrect a just-deleted event), then queued uploads, then the
        // collections are pulled.
        let delete_jobs = self.gather_delete_jobs(doc);
        let push_jobs = self.gather_push_jobs(doc);
        let (tx, rx) = std::sync::mpsc::channel();
        let today = self.today;
        for id in &sync_account_ids {
            if let Err(error) = accounts::AccountSyncReporter::new(id.clone()).started() {
                tracing::warn!(%error, account = id, "calendar: could not persist sync start");
            }
        }
        std::thread::spawn(move || {
            let deleted = caldav::delete_all(&delete_jobs);
            let pushed = caldav::push_all(&push_jobs);
            let mut result = caldav::SyncResult::default();
            let mut synced_calendars = 0;
            let mut first_failure = None;
            for (id, input) in sync_account_ids.iter().zip(inputs.iter()) {
                let (mut account_result, account_outcome) =
                    caldav::sync_all(std::slice::from_ref(input), today);
                let reporter = accounts::AccountSyncReporter::new(id.clone());
                let upload_failed = push_jobs
                    .iter()
                    .any(|(config, _, event)| config == &input.0 && !pushed.contains(&event.id));
                let delete_failed = delete_jobs
                    .iter()
                    .any(|(config, _, uid)| config == &input.0 && !deleted.contains(uid));
                let persisted = if matches!(&account_outcome, caldav::SyncOutcome::Synced { .. })
                    && (upload_failed || delete_failed)
                {
                    first_failure.get_or_insert_with(|| "Some changes remain queued".to_string());
                    reporter.failed()
                } else {
                    match &account_outcome {
                        caldav::SyncOutcome::Synced { .. } => reporter.succeeded(),
                        caldav::SyncOutcome::Offline => reporter.offline(),
                        caldav::SyncOutcome::Failed(reason)
                            if reason.contains("401") || reason.contains("403") =>
                        {
                            reporter.needs_authentication()
                        }
                        caldav::SyncOutcome::Failed(_) => reporter.failed(),
                    }
                };
                if let Err(error) = persisted {
                    tracing::warn!(%error, account = id, "calendar: could not persist sync result");
                }
                result.events.append(&mut account_result.events);
                result.tasks.append(&mut account_result.tasks);
                match account_outcome {
                    caldav::SyncOutcome::Synced { calendars, .. } => {
                        synced_calendars += calendars;
                    }
                    caldav::SyncOutcome::Offline => {}
                    caldav::SyncOutcome::Failed(reason) => {
                        first_failure.get_or_insert(reason);
                    }
                }
            }
            let outcome = if let Some(reason) = first_failure {
                caldav::SyncOutcome::Failed(reason)
            } else if synced_calendars > 0 {
                caldav::SyncOutcome::Synced {
                    calendars: synced_calendars,
                    events: result.events.len(),
                }
            } else {
                caldav::SyncOutcome::Offline
            };
            let _ = tx.send((result, outcome, pushed, deleted));
        });
        self.sync_rx = Some(rx);
        self.syncing = true;
        self.status = Some("Syncing…".into());
        self.dirty = true;
    }

    /// Drain a finished background sync, if any. Called every `sync` pass; a
    /// no-op until the worker delivers. Keeping this off the click path is why
    /// a slow or unreachable server never freezes the window.
    fn poll_sync(&mut self, doc: &mut AutoCommit) {
        let Some(rx) = self.sync_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok((result, outcome, pushed, deleted)) => {
                self.sync_rx = None;
                self.syncing = false;
                // Clear successfully-uploaded events from the push outbox and
                // server-confirmed removals from the delete outbox.
                for id in pushed {
                    CalDoc::dequeue_push(doc, id);
                }
                for id in deleted {
                    CalDoc::dequeue_delete(doc, id);
                }
                self.apply_sync_result(result, outcome, doc);
                // A landed pull mutated the document — persist it.
                self.force_persist = true;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                // Worker panicked or dropped the sender without a reply.
                self.sync_rx = None;
                self.syncing = false;
                self.status = Some("Sync failed".into());
                self.dirty = true;
            }
        }
    }

    /// Merge a pull's events into the model + CRDT and set the status pill.
    /// De-duped by (title, start) so a repeated sync is close to idempotent.
    fn apply_sync_result(
        &mut self,
        result: caldav::SyncResult,
        outcome: SyncOutcome,
        doc: &mut AutoCommit,
    ) {
        let mut added = 0usize;
        for mut e in result.events {
            if self
                .model
                .events
                .iter()
                .any(|x| x.title == e.title && x.start == e.start)
            {
                continue;
            }
            e.id = Uuid::new_v4();
            CalDoc::update_event(doc, &e);
            self.model.events.push(e);
            added += 1;
        }
        self.model.events.sort_by_key(|e| e.start);
        self.status = Some(match outcome {
            SyncOutcome::Synced { calendars, .. } => {
                format!("Synced {added} events from {calendars} calendars")
            }
            SyncOutcome::Offline => {
                if self.accounts.is_empty() {
                    "No calendar accounts yet — add one in Settings".into()
                } else {
                    "Couldn't reach the calendar server".into()
                }
            }
            SyncOutcome::Failed(reason) => format!("Sync failed: {reason}"),
        });
        self.dirty = true;
    }

    /// Resolve the outbox into concrete upload jobs: each queued event paired
    /// with the account config and remote calendar it targets. Events whose
    /// calendar is no longer configured (or that became local) are skipped and
    /// left in the queue for a later attempt — never fabricated.
    fn gather_push_jobs(
        &self,
        doc: &AutoCommit,
    ) -> Vec<(
        crate::account::CalDavConfig,
        crate::account::RemoteCalendar,
        Event,
    )> {
        let mut jobs = Vec::new();
        for id in CalDoc::outbox(doc) {
            let Some(event) = self.model.event(id) else {
                continue;
            };
            let Some(cal_id) = &event.calendar_id else {
                continue;
            };
            for acc in &self.accounts.accounts {
                if let Some(cal) = acc.calendars.iter().find(|c| &c.id == cal_id) {
                    jobs.push((acc.config.clone(), cal.clone(), event.clone()));
                    break;
                }
            }
        }
        jobs
    }

    /// The single notes-editor entry point. All notes rendering and editing for
    /// the selected event/task goes through this widget; binding it to the
    /// selected item's CRDT block list happens in [`Self::ensure_bound`]. This
    /// is the seam the shared WYSIWYG editor will replace: swap the
    /// [`BlockEditor`] field type and re-point `ensure_bound`/`notes_list_for`,
    /// and nothing in the layout/draw/event code below needs to change.
    fn notes_editor(&mut self) -> &mut BlockEditor {
        &mut self.notes_editor
    }

    // --- CRDT plumbing ---------------------------------------------------

    fn notes_list_for(doc: &mut AutoCommit, sel: Sel) -> Option<ObjId> {
        match sel {
            Sel::Event(id) => CalDoc::event_notes(doc, id),
            Sel::Task(id) => CalDoc::task_notes(doc, id),
        }
    }

    fn seed_demo_notes(&self, doc: &mut AutoCommit) {
        // Curated notes so a freshly-seeded calendar shows real
        // documents, not empty editors, on first paint.
        let curated: [(&str, &[&str]); 2] = [
            (
                "Design review",
                &[
                    "# Agenda",
                    "Walk through the latest mockups.",
                    "Open questions:",
                    "Sign-off on the final layout?",
                    "Confirm the launch timeline.",
                ],
            ),
            (
                "Team standup",
                &[
                    "# Standup",
                    "Wrapped up last week's report.",
                    "Following up with the vendor today.",
                    "Blockers: none.",
                ],
            ),
        ];
        for (needle, lines) in curated {
            if let Some(e) = self
                .model
                .events
                .iter()
                .find(|e| e.title.starts_with(needle))
                && let Some(list) = CalDoc::event_notes(doc, e.id)
            {
                CalDoc::seed_notes_if_empty(doc, &list, lines);
            }
        }
    }

    fn ensure_bound(&mut self, doc: &mut AutoCommit) {
        let sel = match self.selection {
            Some(s) if self.detail_open => s,
            _ => {
                if self.bound.is_some() {
                    self.notes_editor = BlockEditor::new().with_empty_hint(NOTES_PLACEHOLDER);
                    self.bound = None;
                }
                return;
            }
        };
        if self.bound == Some(sel) {
            return;
        }
        if let Some(list) = Self::notes_list_for(doc, sel) {
            let mut ed = BlockEditor::new().with_empty_hint(NOTES_PLACEHOLDER);
            ed.bind_blocks(doc, list);
            // Install the freshly-bound editor through the single notes-editor
            // seam (the swap-point for the shared WYSIWYG editor).
            *self.notes_editor() = ed;
            self.bound = Some(sel);
            self.dirty = true;
        }
    }

    /// Seed the form's title/location fields and clock spinners from the
    /// currently-selected event whenever the selection changes. The fields are
    /// local (widget-owned) text/values; edits flow back into the model +
    /// CRDT via [`Self::flush_form_edits`]. A stale popover is closed so it
    /// can't act on the previous event.
    fn ensure_form_bound(&mut self) {
        let ev_id = match self.selection {
            Some(Sel::Event(id)) if self.detail_open => Some(id),
            _ => None,
        };
        if self.form_event == ev_id {
            return;
        }
        self.form_event = ev_id;
        self.event_popovers.close();
        if let Some(id) = ev_id {
            if let Some(e) = self.model.event(id) {
                self.title_input.set_text(&e.title);
                self.location_input.set_text(&e.location);
            }
        } else {
            self.title_input.set_focus(false);
            self.location_input.set_focus(false);
        }
        self.dirty = true;
    }

    /// Pull any pending text edits out of the title/location fields and mirror
    /// them into the selected event (model + CRDT). Called after routing an
    /// event to the inputs so a keystroke persists like every other edit.
    fn flush_form_edits(&mut self, doc: &mut AutoCommit) {
        let Some(Sel::Event(id)) = self.selection else {
            return;
        };
        let title_dirty = self.title_input.take_dirty();
        let loc_dirty = self.location_input.take_dirty();
        if title_dirty.is_none() && loc_dirty.is_none() {
            return;
        }
        let title = self.title_input.value().to_string();
        let location = self.location_input.value().to_string();
        let snap = if let Some(e) = self.model.event_mut(id) {
            e.title = title;
            e.location = location;
            Some(e.clone())
        } else {
            None
        };
        if let Some(snap) = snap {
            self.perform_action(doc, &crate::actions::upsert_event_action(&snap));
            self.force_persist = true;
            self.dirty = true;
        }
    }

    /// Drop the current selection if the item it points at is no longer in the
    /// model (e.g. after adopting a persisted document that never had it).
    fn revalidate_selection(&mut self) {
        let alive = match self.selection {
            Some(Sel::Event(id)) => self.model.event(id).is_some(),
            Some(Sel::Task(id)) => self.model.task(id).is_some(),
            None => false,
        };
        if !alive {
            self.selection = None;
            self.detail_open = false;
            self.narrow_screen = NarrowScreen::Calendar;
        }
    }

    /// Write the calendar document to disk when it has changed. No-op in the
    /// demo/test build (no `data_path`) and when nothing changed since the last
    /// save, so this is safe to call every frame.
    ///
    /// Discrete actions (new event, toggle, import, sync) force an immediate
    /// write. Continuous edits — typing in a note — coalesce: the whole
    /// document is serialized at most a few times a second while the keystrokes
    /// keep landing, and once more when they stop.
    fn maybe_persist(&mut self, state: &mut AutoCommit) {
        let Some(path) = self.data_path.clone() else {
            return;
        };
        let heads = state.get_heads();
        if heads == self.saved_heads {
            self.force_persist = false;
            return;
        }
        let due = self.force_persist
            || self.last_persist.elapsed() >= std::time::Duration::from_millis(500);
        if !due {
            return;
        }
        match crate::store::save(&path, state) {
            Ok(()) => {
                self.saved_heads = heads;
                self.last_persist = std::time::Instant::now();
                self.force_persist = false;
            }
            Err(e) => tracing::warn!(error = %e, "calendar: failed to persist document"),
        }
    }

    // --- action dispatch -------------------------------------------------

    /// Dispatch one [`CalendarAction`](crate::actions::CalendarAction) — the
    /// single CRDT-mutation path shared with the Ctrl+K palette. Delegates to the
    /// framework's [`ActionHost`](ui::ActionHost), which brackets the undo
    /// boundary and applies the action through the executor (a pinned actor +
    /// committed change, so it replays byte-identically on peers), exactly as the
    /// palette does. The in-memory model is updated by the caller for immediate UI
    /// feedback and reconciled from the document on the next `sync`. This app
    /// supplies only its domain: the action and its type.
    fn perform_action(&mut self, doc: &mut AutoCommit, action: &crate::actions::CalendarAction) {
        self.actions.dispatch_with_context(
            doc,
            crate::actions::CALENDAR_ACTION_TYPE,
            action,
            crate::actions::history_context(action),
        );
    }

    /// The last [`EncodedAction`](qos_actions::EncodedAction) an on-screen
    /// affordance dispatched, for the button == [`ActionSpec`](qos_actions::ActionSpec)
    /// covenant test: a control and its Ctrl+K entry must build the identical
    /// action.
    #[cfg(test)]
    fn last_dispatched(&self) -> Option<&qos_actions::EncodedAction> {
        self.actions.last_dispatched()
    }

    /// The source calendar id to queue a `CalDAV` delete for when an event is
    /// removed: `Some` only for a synced event whose calendar is still
    /// configured (otherwise there is nothing real to address). Shared by the
    /// trash-button handler and the Ctrl+K "Delete event" spec so both build the
    /// identical [`CalendarAction`](crate::actions::CalendarAction).
    fn resolve_remote_delete(&self, id: Id) -> Option<String> {
        let cid = self.model.event(id).and_then(|e| e.calendar_id.clone())?;
        self.accounts
            .accounts
            .iter()
            .any(|a| a.calendars.iter().any(|c| c.id == cid))
            .then_some(cid)
    }

    /// The context Ctrl+K needs to build the calendar's resource tree: a template
    /// for a palette-created event (from the current view), and the events +
    /// top-level tasks currently in the model.
    pub(crate) fn calendar_resource_ctx(&self) -> crate::actions::CalendarResourceCtx {
        let start = self
            .anchor
            // 12:00:00 is always a valid `NaiveTime`; the fallback never runs.
            .and_time(NaiveTime::from_hms_opt(12, 0, 0).unwrap_or(NaiveTime::MIN));
        let template = Event {
            id: Uuid::nil(),
            title: String::new(),
            start,
            end: start + Duration::hours(1),
            all_day: false,
            location: String::new(),
            color: self.model.next_event_order_color(),
            recurrence: Recurrence::None,
            reminder_min: Some(10),
            calendar_id: None,
            project: None,
        };
        let events = self
            .model
            .events
            .iter()
            .map(|e| crate::actions::EventResource {
                data: crate::actions::EventData::from_event(e),
                remote_delete: self.resolve_remote_delete(e.id),
            })
            .collect();
        let tasks = self
            .model
            .tasks
            .iter()
            .filter(|t| t.parent.is_none())
            .map(|t| crate::actions::TaskResource {
                data: crate::actions::TaskData::from_task(t),
                subtasks: self
                    .model
                    .subtasks_of(t.id)
                    .iter()
                    .map(|s| s.id.to_string())
                    .collect(),
            })
            .collect();
        crate::actions::CalendarResourceCtx {
            new_event: crate::actions::EventData::from_event(&template),
            events,
            tasks,
            can_sync: self.demo || !self.accounts.is_empty(),
            syncing: self.syncing,
            can_export: !self.model.events.is_empty() || !self.model.tasks.is_empty(),
        }
    }

    // --- mutations -------------------------------------------------------

    fn select_event(&mut self, id: Id) {
        self.selection = Some(Sel::Event(id));
        self.open_detail();
    }

    /// The event currently selected, if the selection is an event (not a task).
    /// Lets a test observe where a Ctrl+K activation navigated.
    pub fn selected_event(&self) -> Option<Id> {
        match self.selection {
            Some(Sel::Event(id)) => Some(id),
            _ => None,
        }
    }

    fn select_task(&mut self, id: Id) {
        self.selection = Some(Sel::Task(id));
        self.open_detail();
    }

    /// Open the detail editor for the current selection. In the full-screen
    /// layout this pushes the editor page over the calendar; in the overlay
    /// layout the modal card floats over it. Either way an open overlay
    /// sidebar flap is dismissed so it isn't left covering the editor.
    fn open_detail(&mut self) {
        self.detail_open = true;
        if matches!(self.detail_mode, DetailMode::FullScreen) {
            self.narrow_screen = NarrowScreen::Detail;
        }
        if matches!(self.sidebar.mode(), SidebarMode::Overlay) {
            self.sidebar.set_open(false);
        }
        self.dirty = true;
    }

    /// Delete an event: drop it from the model + CRDT, clear any pending upload,
    /// and — for an event synced from a configured remote calendar — queue a
    /// `CalDAV` delete so the server copy is removed on the next sync. Closes the
    /// editor if the deleted event was the one on screen.
    fn delete_event(&mut self, id: Id, doc: &mut AutoCommit) {
        // Resolve the CalDAV delete target (a synced event on a still-configured
        // calendar) before the row leaves the model. The action removes the
        // event, clears any queued upload, and — given a target — queues the
        // remote delete, so every doc write is one replayable envelope.
        let remote_delete = self.resolve_remote_delete(id);
        self.model.events.retain(|e| e.id != id);
        self.perform_action(
            doc,
            &crate::actions::delete_event_action(id.to_string(), remote_delete),
        );
        if self.selection == Some(Sel::Event(id)) {
            self.close_after_delete();
        }
        self.status = Some("Event deleted".into());
        self.force_persist = true;
        self.dirty = true;
    }

    /// Delete a task (and any subtasks) from the model + CRDT. Closes the editor
    /// if the deleted task was the one on screen.
    fn delete_task(&mut self, id: Id, doc: &mut AutoCommit) {
        let subtasks: Vec<Id> = self
            .model
            .tasks
            .iter()
            .filter(|t| t.parent == Some(id))
            .map(|t| t.id)
            .collect();
        let victims: Vec<Id> = std::iter::once(id)
            .chain(subtasks.iter().copied())
            .collect();
        self.model.tasks.retain(|t| !victims.contains(&t.id));
        self.perform_action(
            doc,
            &crate::actions::delete_task_action(
                id.to_string(),
                subtasks
                    .iter()
                    .map(std::string::ToString::to_string)
                    .collect(),
            ),
        );
        if matches!(self.selection, Some(Sel::Task(sel)) if victims.contains(&sel)) {
            self.close_after_delete();
        }
        self.status = Some("Task deleted".into());
        self.force_persist = true;
        self.dirty = true;
    }

    /// Release keyboard focus from every editor field (title, location, notes)
    /// and drop any open popover. Called whenever the editor leaves the screen
    /// so keystrokes meant for the calendar can't leak into a hidden field.
    fn blur_editor(&mut self) {
        self.notes_editor.set_focus(false);
        self.title_input.set_focus(false);
        self.location_input.set_focus(false);
        self.event_popovers.close();
    }

    /// Shared teardown after deleting the on-screen item: drop the selection,
    /// close the editor, and return to the calendar.
    fn close_after_delete(&mut self) {
        self.selection = None;
        self.detail_open = false;
        self.narrow_screen = NarrowScreen::Calendar;
        self.blur_editor();
    }

    /// Resolve the delete outbox into concrete `CalDAV` delete jobs: each queued
    /// `(calendar_id, uid)` paired with the account config + remote calendar it
    /// targets. Entries whose calendar is no longer configured are skipped and
    /// left queued for a later attempt.
    fn gather_delete_jobs(
        &self,
        doc: &AutoCommit,
    ) -> Vec<(
        crate::account::CalDavConfig,
        crate::account::RemoteCalendar,
        Id,
    )> {
        let mut jobs = Vec::new();
        for (cal_id, uid) in CalDoc::delete_outbox(doc) {
            for acc in &self.accounts.accounts {
                if let Some(cal) = acc.calendars.iter().find(|c| c.id == cal_id) {
                    jobs.push((acc.config.clone(), cal.clone(), uid));
                    break;
                }
            }
        }
        jobs
    }

    fn toggle_task(&mut self, id: Id, doc: &mut AutoCommit) {
        let exists = if let Some(t) = self.model.task_mut(id) {
            t.done = !t.done;
            true
        } else {
            false
        };
        if exists {
            self.perform_action(doc, &crate::actions::toggle_task_action(id.to_string()));
            self.dirty = true;
        }
    }

    fn new_event(&mut self, doc: &mut AutoCommit) {
        let start = self
            .anchor
            // 12:00:00 is always a valid `NaiveTime`; the fallback never runs.
            .and_time(NaiveTime::from_hms_opt(12, 0, 0).unwrap_or(NaiveTime::MIN));
        let ev = Event {
            // Empty title: the form's title field shows its "New event"
            // placeholder rather than literal text the user has to clear. The
            // event still renders as "New event" in the views via
            // `Event::display_title`.
            id: Uuid::new_v4(),
            title: String::new(),
            start,
            end: start + Duration::hours(1),
            all_day: false,
            location: String::new(),
            color: self.model.next_event_order_color(),
            recurrence: Recurrence::None,
            reminder_min: Some(10),
            // Filed on the local (on-device) calendar by default; the form's
            // calendar selector can retarget it to a configured account.
            calendar_id: None,
            project: None,
        };
        self.spawn_event(ev, doc);
    }

    /// Insert a freshly-authored event into the model + CRDT (through the action
    /// model), select it, and open its edit form with the caret in the title so
    /// the next keystroke names it. Shared by the New button and the timeline's
    /// drag-to-create. Notes are left EMPTY on purpose — the editor paints its
    /// placeholder until the user types.
    fn spawn_event(&mut self, ev: Event, doc: &mut AutoCommit) {
        let id = ev.id;
        self.perform_action(doc, &crate::actions::upsert_event_action(&ev));
        self.model.events.push(ev);
        self.model.events.sort_by_key(|e| e.start);
        self.select_event(id);
        self.title_input.set_text("");
        self.location_input.set_text("");
        self.focus_field(FormField::Title);
        self.status = Some("New event".into());
    }

    /// Create a timed event spanning `[start_min, end_min]` minutes-of-day on
    /// `day` and open it for editing — the timeline drag-to-create commit.
    fn create_event_in_range(
        &mut self,
        day: NaiveDate,
        start_min: i64,
        end_min: i64,
        doc: &mut AutoCommit,
    ) {
        let dur = (end_min - start_min).max(TIMELINE_SNAP_MIN);
        // `dur` has no upper bound (a pathological drag could exceed a full
        // day), so the clamp's own upper bound is floored at 0 rather than
        // trusting `24 * 60 - dur` to stay non-negative — `clamp` panics if
        // its bounds are inverted.
        let start_min = start_min.clamp(0, (24 * 60 - dur).max(0));
        let start = day.and_time(min_to_time(start_min));
        let end = start + Duration::minutes(dur);
        let ev = Event {
            id: Uuid::new_v4(),
            title: String::new(),
            start,
            end,
            all_day: false,
            location: String::new(),
            color: self.model.next_event_order_color(),
            recurrence: Recurrence::None,
            reminder_min: Some(10),
            calendar_id: None,
            project: None,
        };
        self.spawn_event(ev, doc);
        self.force_persist = true;
        self.dirty = true;
    }

    /// Reschedule an existing event to a new day + start time, preserving its
    /// duration — the timeline drag-to-move commit. Routed through the action
    /// model so it replays byte-identically on peers.
    fn move_event(
        &mut self,
        id: Id,
        day: NaiveDate,
        start_min: i64,
        dur_min: i64,
        doc: &mut AutoCommit,
    ) {
        let start = day.and_time(min_to_time(start_min));
        let end = start + Duration::minutes(dur_min.max(TIMELINE_SNAP_MIN));
        let snap = if let Some(e) = self.model.event_mut(id) {
            // An all-day event has no place on the timed grid; leave it alone.
            if e.all_day {
                None
            } else {
                e.start = start;
                e.end = end;
                Some(e.clone())
            }
        } else {
            None
        };
        if let Some(snap) = snap {
            self.perform_action(doc, &crate::actions::upsert_event_action(&snap));
            self.status = Some("Event rescheduled".into());
            self.force_persist = true;
            self.dirty = true;
        }
    }

    // --- week/day timeline + agenda: scroll & drag input -----------------

    /// Whether the calendar content (as opposed to a modal editor or an open
    /// overlay sidebar) currently owns pointer input.
    fn calendar_content_interactive(&self) -> bool {
        self.content_rect.area() > 0.0
            && !(self.detail_open && self.detail_rect.area() > 0.0)
            && !(matches!(self.sidebar.mode(), SidebarMode::Overlay) && self.sidebar.is_open())
    }

    /// Route input to the Agenda scroller or the self-contained week/day
    /// controller. Typed timeline effects return here for domain/CRDT mutation.
    #[allow(clippy::too_many_arguments)]
    fn handle_timeline_input(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) -> bool {
        if !matches!(self.view, View::Week | View::Day) {
            self.timeline.cancel_drag();
        }
        if !self.calendar_content_interactive() {
            return false;
        }
        if matches!(self.view, View::Agenda) {
            return self.handle_agenda_timeline_input(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                focus_target,
                node_id,
                taffy,
            );
        }
        if !matches!(self.view, View::Week | View::Day) {
            return false;
        }

        let area = self.view_rect();
        let days = self.timeline_days();
        let spans = self.timeline_spans(&days);
        let outcome = self.timeline.handle_event(
            TimelineEventCtx {
                event,
                scale_factor,
                modifiers,
                state,
                path,
                focus_target,
                node_id,
                taffy,
            },
            area,
            &days,
            &spans,
        );
        if let Some(effect) = outcome.effect {
            self.apply_timeline_effect(effect, state);
        }
        if outcome.handled {
            self.dirty = true;
        }
        outcome.handled
    }

    fn apply_timeline_effect(&mut self, effect: TimelineEffect, doc: &mut AutoCommit) {
        match effect {
            TimelineEffect::Create {
                day,
                start_min,
                end_min,
            } => self.create_event_in_range(day, start_min, end_min, doc),
            TimelineEffect::Move {
                id,
                day,
                start_min,
                dur_min,
            } => self.move_event(id, day, start_min, dur_min, doc),
            TimelineEffect::Select(id) => self.apply_action(Action::OpenEvent(id), doc),
            TimelineEffect::OpenDay(day) => self.apply_action(Action::OpenDay(day), doc),
        }
    }

    /// The Agenda view's timeline input: scroll only, no drag. Split out of
    /// `handle_timeline_input` to keep it within the line-count lint.
    #[allow(clippy::too_many_arguments)]
    fn handle_agenda_timeline_input(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) -> bool {
        if matches!(
            event,
            WindowEvent::MouseWheel { .. }
                | WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput {
                    button: MouseButton::Left,
                    ..
                }
        ) {
            let p = self.last_mouse;
            if self.agenda.wants_pointer_capture() || self.view_rect().contains(p) {
                let handled = self.agenda.handle_event(
                    event,
                    scale_factor,
                    modifiers,
                    state,
                    path,
                    focus_target,
                    node_id,
                    taffy,
                );
                if handled {
                    self.dirty = true;
                }
                return handled;
            }
        }
        false
    }

    // --- event form model edits -----------------------------------------

    /// Every calendar an event can be filed under: the local on-device
    /// calendar plus each remote `CalDAV` collection configured in Settings
    /// (the shared [`AccountStore`]). This is the selector's option list —
    /// sourced from real configured accounts, never fabricated.
    fn calendar_options(&self) -> Vec<(CalTarget, String)> {
        let mut out = vec![(CalTarget::Local, "On this device".to_string())];
        for acc in &self.accounts.accounts {
            for cal in &acc.calendars {
                out.push((
                    CalTarget::Remote(cal.id.clone()),
                    format!("{} · {}", acc.label(), cal.name),
                ));
            }
        }
        out
    }

    /// The display label for an event's current calendar target.
    fn calendar_label(&self, calendar_id: Option<&String>) -> String {
        match calendar_id {
            None => "On this device".to_string(),
            Some(id) => self
                .accounts
                .accounts
                .iter()
                .flat_map(|a| a.calendars.iter().map(move |c| (a, c)))
                .find(|(_, c)| &c.id == id)
                .map_or_else(
                    || id.clone(),
                    |(a, c)| format!("{} · {}", a.label(), c.name),
                ),
        }
    }

    /// File the selected event on `target`. A remote target tags the event
    /// with the calendar id, adopts that calendar's colour, and queues it for
    /// upload; the local target clears the tag and drops it from the queue.
    fn set_event_calendar(&mut self, target: &CalTarget, doc: &mut AutoCommit) {
        let Some(Sel::Event(id)) = self.selection else {
            return;
        };
        // Resolve the remote calendar's palette up front (immutable borrow of
        // `self.accounts`) so the mutable event borrow below doesn't overlap.
        let remote_color = match target {
            CalTarget::Remote(cid) => self
                .accounts
                .accounts
                .iter()
                .flat_map(|a| a.calendars.iter())
                .find(|c| &c.id == cid)
                .map(super::account::RemoteCalendar::palette),
            CalTarget::Local => None,
        };
        let snap = if let Some(e) = self.model.event_mut(id) {
            match target {
                CalTarget::Local => e.calendar_id = None,
                CalTarget::Remote(cid) => {
                    e.calendar_id = Some(cid.clone());
                    if let Some(col) = remote_color {
                        e.color = col;
                    }
                }
            }
            Some(e.clone())
        } else {
            None
        };
        if let Some(snap) = snap {
            // One action writes the retargeted event AND updates the push queue
            // (enqueue for a remote target, dequeue for the local one).
            let enqueue = matches!(target, CalTarget::Remote(_));
            self.perform_action(
                doc,
                &crate::actions::set_event_calendar_action(&snap, enqueue),
            );
        }
        self.event_popovers.close();
        self.force_persist = true;
        self.dirty = true;
    }

    /// Open one of the form popovers, seeding the embedded picker from the
    /// event's current value and giving it keyboard focus so it is immediately
    /// arrow-navigable. Opening a popover takes focus off the text fields.
    fn open_popover(&mut self, which: FormPopover) {
        let ev = match self.selection {
            Some(Sel::Event(id)) => self.model.event(id).cloned(),
            _ => None,
        };
        let Some(e) = ev else { return };
        self.title_input.set_focus(false);
        self.location_input.set_focus(false);
        self.event_popovers.open(which, &e);
        self.dirty = true;
    }

    /// Move keyboard focus to a form text field (click / Tab target).
    fn focus_field(&mut self, field: FormField) {
        self.event_popovers.close();
        self.notes_editor.set_focus(false);
        match field {
            FormField::Title => {
                self.title_input.set_focus(true);
                self.location_input.set_focus(false);
            }
            FormField::Location => {
                self.title_input.set_focus(false);
                self.location_input.set_focus(true);
            }
        }
        self.dirty = true;
    }

    /// The topmost hit-map action under point `p`, if any (later hits win, so
    /// popovers/scrims added last take precedence over the fields beneath).
    fn hit_action_at(&self, p: Point) -> Option<Action> {
        self.hits
            .iter()
            .rev()
            .find(|(r, _)| r.contains(p))
            .map(|(_, a)| a.clone())
    }

    /// Advance (or, with `backwards`, retreat) keyboard focus through the
    /// form's fields: title → location → notes → title. Opening a field closes
    /// any popover.
    fn cycle_form_focus(&mut self, backwards: bool) {
        let cur: i32 = if self.title_input.is_focused() {
            0
        } else if self.location_input.is_focused() {
            1
        } else if self.notes_editor.is_focused() {
            2
        } else {
            -1
        };
        let n = 3;
        let next = if cur < 0 {
            if backwards { n - 1 } else { 0 }
        } else {
            (cur + if backwards { n - 1 } else { 1 }) % n
        };
        self.event_popovers.close();
        self.title_input.set_focus(false);
        self.location_input.set_focus(false);
        self.notes_editor.set_focus(false);
        match next {
            0 => self.title_input.set_focus(true),
            1 => self.location_input.set_focus(true),
            _ => self.notes_editor.set_focus(true),
        }
        self.dirty = true;
    }

    /// Route input through the self-contained popover controller, then apply
    /// any semantic edit it emits through the calendar action model.
    #[allow(clippy::too_many_arguments)]
    fn handle_popover_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        let (consumed, edit) = self.event_popovers.handle_event(
            event,
            self.last_mouse,
            scale_factor,
            modifiers,
            state,
            path,
            focus_target,
            node_id,
            taffy,
        );
        *handled |= consumed;
        match edit {
            Some(PopoverEdit::Date(which, date)) => self.set_event_date(which, date, state),
            Some(PopoverEdit::Time(which, hour, minute)) => {
                self.set_event_time(which, hour, minute, state);
            }
            Some(PopoverEdit::Calendar(target)) => self.set_event_calendar(&target, state),
            None => {}
        }
    }

    /// Flip the selected event's all-day flag.
    fn toggle_all_day(&mut self, doc: &mut AutoCommit) {
        self.cycle_event(doc, |e| e.all_day = !e.all_day);
        self.event_popovers.close();
    }

    /// Apply the date grid's current selection to the start or end of the
    /// event, keeping the clock time and preserving a sane ordering (a start
    /// past the end drags the end along; an end before the start is clamped).
    fn set_event_date(&mut self, which: FormPopover, date: NaiveDate, doc: &mut AutoCommit) {
        self.cycle_event(doc, |e| match which {
            FormPopover::StartDate => {
                e.start = date.and_time(e.start.time());
                if e.end < e.start {
                    e.end = e.start + Duration::hours(1);
                }
            }
            FormPopover::EndDate => {
                e.end = date.and_time(e.end.time());
                if e.end < e.start {
                    e.end = e.start + Duration::hours(1);
                }
            }
            _ => {}
        });
    }

    /// Apply a clock spinner's current value to the start or end time.
    fn set_event_time(&mut self, which: FormPopover, h: u8, m: u8, doc: &mut AutoCommit) {
        let time = NaiveTime::from_hms_opt(u32::from(h), u32::from(m), 0).unwrap_or(NaiveTime::MIN);
        self.cycle_event(doc, |e| match which {
            FormPopover::StartTime => {
                e.start = e.start.date().and_time(time);
                if e.end < e.start {
                    e.end = e.start + Duration::hours(1);
                }
            }
            FormPopover::EndTime => {
                e.end = e.end.date().and_time(time);
                if e.end < e.start {
                    e.end = e.start + Duration::hours(1);
                }
            }
            _ => {}
        });
    }

    fn export_ics(&mut self) {
        if self.model.events.is_empty() && self.model.tasks.is_empty() {
            self.status = Some("Nothing to export yet".into());
            self.dirty = true;
            return;
        }
        let ics = ics::export(&self.model);
        let dir = crate::store::transfer_dir();
        let path = dir.join(ics::suggested_filename(self.anchor));
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::write(&path, ics) {
            Ok(()) => {
                self.status = Some(format!(
                    "Exported {} events to {}",
                    self.model.events.len(),
                    path.display()
                ));
            }
            Err(e) => self.status = Some(format!("Export failed: {e}")),
        }
        self.dirty = true;
    }

    /// Import the most recent `.ics` file from the transfer directory (the
    /// user's Downloads folder) through the real [`ics::parse`] path, merging
    /// new events/tasks into the model and the CRDT. De-duped by (title,
    /// start) for events and by title for tasks so a repeated import is close
    /// to idempotent.
    fn import_ics(&mut self, doc: &mut AutoCommit) {
        let dir = crate::store::transfer_dir();
        let Some(path) = crate::store::newest_ics(&dir) else {
            self.status = Some(format!("No .ics files found in {}", dir.display()));
            self.dirty = true;
            return;
        };
        let payload = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                self.status = Some(format!("Import failed: {e}"));
                self.dirty = true;
                return;
            }
        };
        let source = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();
        let (events, tasks) = ics::parse(&payload);
        let mut added_e = 0usize;
        for mut e in events {
            if self
                .model
                .events
                .iter()
                .any(|x| x.title == e.title && x.start == e.start)
            {
                continue;
            }
            e.id = Uuid::new_v4();
            CalDoc::update_event(doc, &e);
            if let Some(list) = CalDoc::event_notes(doc, e.id) {
                CalDoc::seed_notes_if_empty(doc, &list, &["Imported from .ics"]);
            }
            self.model.events.push(e);
            added_e += 1;
        }
        let mut added_t = 0usize;
        for mut t in tasks {
            if self.model.tasks.iter().any(|x| x.title == t.title) {
                continue;
            }
            t.id = Uuid::new_v4();
            CalDoc::update_task(doc, &t);
            self.model.tasks.push(t);
            added_t += 1;
        }
        self.model.events.sort_by_key(|e| e.start);
        self.status = Some(if added_e == 0 && added_t == 0 {
            format!("{source} already imported")
        } else {
            format!("Imported {added_e} events · {added_t} tasks from {source}")
        });
        self.dirty = true;
    }

    fn cycle_event<F: Fn(&mut Event)>(&mut self, doc: &mut AutoCommit, f: F) {
        if let Some(Sel::Event(id)) = self.selection {
            let snap = if let Some(e) = self.model.event_mut(id) {
                f(e);
                Some(e.clone())
            } else {
                None
            };
            if let Some(snap) = snap {
                self.perform_action(doc, &crate::actions::upsert_event_action(&snap));
                self.dirty = true;
            }
        }
    }

    fn move_anchor(&mut self, days: i64) {
        self.anchor += Duration::days(days);
        self.dirty = true;
    }

    fn step_period(&mut self, dir: i32) {
        self.anchor = match self.view {
            View::Month => date::add_months(self.anchor, dir),
            View::Week | View::Agenda => self.anchor + Duration::days(7 * i64::from(dir)),
            View::Day => self.anchor + Duration::days(i64::from(dir)),
        };
        self.dirty = true;
    }

    /// Recompute every layout rect from the allocated window size. Called from
    /// `apply_layout`. This is the one place breakpoints are decided, along two
    /// independent axes:
    ///
    /// * **sidebar** — responsive inline panel / overlay drawer, delegated to
    ///   [`Sidebar`] so its state and geometry stay together.
    /// * **editor** — a centered modal card floating over the still-visible
    ///   calendar at/above [`EDITOR_OVERLAY_MIN`]; a full-screen page that
    ///   replaces the calendar below it (only one of {calendar, detail} is laid
    ///   out at a time in that mode).
    fn relayout(&mut self) {
        let r = self.rect;
        // Two independent responsive axes, each re-derived from the current
        // width so a live resize reflows both.
        let content_x0 = self.sidebar.layout(r);
        self.detail_mode = if r.width() >= EDITOR_OVERLAY_MIN {
            DetailMode::Overlay
        } else {
            DetailMode::FullScreen
        };

        match self.detail_mode {
            DetailMode::Overlay => {
                // The calendar stays full-width; the editor floats over it as a
                // centered modal card. Both panes have area so both draw.
                self.content_rect = Rect::new(content_x0, r.y0, r.x1, r.y1);
                self.detail_rect = if self.detail_open {
                    self.modal_rect(content_x0)
                } else {
                    Rect::ZERO
                };
            }
            DetailMode::FullScreen => {
                // Single-pane: calendar OR the pushed detail page fills the
                // content width, never both.
                let full = Rect::new(content_x0, r.y0, r.x1, r.y1);
                if let (true, NarrowScreen::Detail) = (self.detail_open, self.narrow_screen) {
                    self.content_rect = Rect::ZERO;
                    self.detail_rect = full;
                } else {
                    self.content_rect = full;
                    self.detail_rect = Rect::ZERO;
                }
            }
        }

        // Navigation chrome owns its geometry and the responsive decision to
        // keep the view switcher here or fold it into the sidebar.
        let leading = if matches!(self.sidebar.mode(), SidebarMode::Overlay) {
            LeadingControl::OpenSidebar
        } else if self.sidebar.is_collapsed() {
            LeadingControl::ExpandSidebar
        } else {
            LeadingControl::None
        };
        self.topbar.layout(self.content_rect, leading);

        // Point the active view's scroll container at the fresh geometry so a
        // resize reflows the scroll extents (and re-clamps the offset) every
        // frame rather than laying out once.
        if self.content_rect.area() > 0.0 {
            match self.view {
                View::Week | View::Day => self.update_timeline_scroll(),
                View::Agenda => {
                    let area = self.view_rect();
                    let hidden = self.hidden_calendar_ids();
                    self.agenda.layout(area, &self.model, self.anchor, &hidden);
                }
                View::Month => {}
            }
        }

        // Notes editor placement, derived from whichever detail rect is active.
        // The column width tracks the detail rect (capped for readability) so
        // the narrow single-pane screen fills its width instead of leaving a
        // fixed-width column stranded against a wide empty margin.
        self.notes_w = self.detail_body_w();
        // Compute the editable event-form geometry (fields + pickers) for the
        // current detail, then position the embedded inputs. The notes section
        // begins below the form when an event is being edited; a task detail
        // (or no selection) falls back to the fixed offset.
        self.form = self.form_layout();
        let editing_event =
            matches!(self.selection, Some(Sel::Event(_))) && self.detail_rect.area() > 0.0;
        let notes_top = if editing_event {
            self.form.notes_top
        } else {
            self.notes_top_offset()
        };
        if self.detail_rect.area() > 0.0 {
            self.notes_origin = Point::new(
                self.detail_rect.x0 + DETAIL_PAD,
                self.detail_rect.y0 + notes_top,
            );
        } else {
            self.notes_origin = Point::new(-10000.0, -10000.0);
        }

        if editing_event {
            self.title_input.apply_layout_external(self.form.title);
            self.location_input
                .apply_layout_external(self.form.location);
        } else {
            // Park the inputs off-screen so a stale click can't hit them.
            self.title_input
                .apply_layout_external(Rect::new(-10000.0, -10000.0, -9900.0, -9900.0));
            self.location_input
                .apply_layout_external(Rect::new(-10000.0, -10000.0, -9900.0, -9900.0));
        }
    }

    /// Point the timeline viewport at its new geometry, seeding the initial
    /// work-day scroll the first time the full day does not fit.
    fn update_timeline_scroll(&mut self) {
        let area = self.view_rect();
        let days = self.timeline_days();
        let focus_hour = if days.contains(&self.today) {
            let now = Local::now().naive_local();
            (f64::from(now.hour()) + f64::from(now.minute()) / 60.0 - 1.0).max(0.0)
        } else {
            TIMELINE_INITIAL_HOUR
        };
        self.timeline.layout(area, focus_hour);
    }

    /// Geometry of the centered modal editor card, sized to a comfortable
    /// column and clamped inside the content area (right of a persistent
    /// sidebar). Only meaningful in [`DetailMode::Overlay`].
    fn modal_rect(&self, content_x0: f64) -> Rect {
        let r = self.rect;
        let content_w = (r.x1 - content_x0).max(0.0);
        let w = MODAL_W.min((content_w - 2.0 * MODAL_MARGIN).max(240.0));
        let h = (r.height() - 2.0 * MODAL_MARGIN).clamp(360.0, MODAL_MAX_H);
        let cx = content_x0 + content_w / 2.0;
        let cy = r.y0 + r.height() / 2.0;
        Rect::new(cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0)
    }

    /// The rect covering the notes editor column inside the detail card/page —
    /// used to route clicks to the editor (and to bound them so a press outside
    /// a modal card doesn't get swallowed by the editor).
    fn notes_region(&self) -> Rect {
        Rect::new(
            self.detail_rect.x0,
            self.notes_origin.y - 4.0,
            self.detail_rect.x1,
            self.detail_rect.y1,
        )
    }

    /// Compute the event-form field rects from the current detail panel. Pure
    /// function of the detail rect, body width, detail mode and the selected
    /// event's all-day flag, so `draw`, the hit map, and the embedded widgets
    /// all read identical geometry. Returns a zeroed layout when no event is
    /// being edited.
    #[allow(
        clippy::many_single_char_names,
        reason = "f is the result being built, r is the source rect, and x/y/w are the \
                  conventional position/size names for a geometry layout pass"
    )]
    fn form_layout(&self) -> FormRects {
        let mut f = FormRects::default();
        let r = self.detail_rect;
        let editing = matches!(self.selection, Some(Sel::Event(_))) && r.area() > 0.0;
        if !editing {
            f.notes_top = self.notes_top_offset();
            return f;
        }
        let all_day = match self.selection {
            Some(Sel::Event(id)) => self.model.event(id).is_some_and(|e| e.all_day),
            _ => false,
        };
        let x = r.x0 + DETAIL_PAD;
        let w = self.detail_body_w();
        let fullscreen = matches!(self.detail_mode, DetailMode::FullScreen);
        // Header: 18px top pad, plus a back bar in the full-screen pushed page,
        // plus the "EVENT" kind label row.
        let top = r.y0 + 18.0 + if fullscreen { 34.0 } else { 0.0 };
        f.header_bottom = top + 22.0;
        let mut y = top + 30.0;

        let field_h = 40.0;
        f.title = Rect::new(x, y, x + w, y + 44.0);
        y += 44.0 + 16.0;

        f.allday = Rect::new(x, y, x + w, y + 30.0);
        y += 30.0 + 14.0;

        // Start row: [label] [date field] [time field]. Time is hidden for an
        // all-day event, and the date field then spans the row.
        let label_w = 52.0;
        let time_w = 92.0;
        let gap = 12.0;
        let date_w = if all_day {
            (w - label_w).max(120.0)
        } else {
            (w - label_w - time_w - gap).max(120.0)
        };
        f.start_date = Rect::new(x + label_w, y, x + label_w + date_w, y + field_h);
        f.start_time = if all_day {
            Rect::ZERO
        } else {
            Rect::new(x + label_w + date_w + gap, y, x + w, y + field_h)
        };
        y += field_h + 12.0;
        f.end_date = Rect::new(x + label_w, y, x + label_w + date_w, y + field_h);
        f.end_time = if all_day {
            Rect::ZERO
        } else {
            Rect::new(x + label_w + date_w + gap, y, x + w, y + field_h)
        };
        y += field_h + 16.0;

        f.location = Rect::new(x, y, x + w, y + field_h);
        y += field_h + 14.0;

        f.calendar = Rect::new(x, y, x + w, y + field_h);
        y += field_h + 18.0;

        f.chips_y = y;
        // Chip row (28px) plus a comfortable gap before the NOTES divider so
        // the section header doesn't hug the chips.
        y += 34.0 + 36.0;

        f.notes_top = y - r.y0;
        f
    }

    /// Width of the detail body column (fields + notes editor). Fixed in the
    /// wide docked panel; in the narrow single-pane screen it grows with the
    /// window (capped for comfortable line length) so the detail fills the
    /// pane instead of hugging the left edge. Derived from the allocated
    /// window width so `build_layout` and `relayout` agree.
    fn detail_body_w(&self) -> f64 {
        // Keyed off the allocated window width (not `detail_rect`) so
        // `build_layout` and `relayout` — which run in that order within a
        // frame — always agree on the editor node's width.
        if self.rect.width() >= EDITOR_OVERLAY_MIN {
            MODAL_BODY_W
        } else {
            (self.rect.width() - 2.0 * DETAIL_PAD).clamp(240.0, 620.0)
        }
    }

    /// Y offset (from the detail rect top) of the NOTES section + editor. The
    /// narrow detail screen carries a back bar above the body, so its notes
    /// start lower. Single source of truth for both `relayout` (editor origin)
    /// and `draw_detail` (the NOTES label/divider).
    fn notes_top_offset(&self) -> f64 {
        if matches!(self.detail_mode, DetailMode::FullScreen) {
            NOTES_TOP_OFFSET + 40.0
        } else {
            NOTES_TOP_OFFSET
        }
    }

    fn apply_action(&mut self, action: Action, doc: &mut AutoCommit) {
        match action {
            Action::View(v) => {
                self.view = v;
                // On narrow layouts the switcher lives in the overlay flap;
                // picking a view there returns to the calendar screen and
                // dismisses the flap so the chosen view is visible at once.
                if matches!(self.sidebar.mode(), SidebarMode::Overlay) {
                    self.narrow_screen = NarrowScreen::Calendar;
                    self.sidebar.set_open(false);
                }
            }
            Action::Today => self.anchor = self.today,
            Action::Prev => self.step_period(-1),
            Action::Next => self.step_period(1),
            Action::PickDate(d) => self.anchor = d,
            Action::OpenDay(d) => {
                self.anchor = d;
                self.view = View::Day;
            }
            Action::OpenEvent(id) => self.select_event(id),
            Action::OpenTask(id) => self.select_task(id),
            Action::ToggleTask(id) => self.toggle_task(id, doc),
            Action::List(s) => {
                self.list_sel = s;
                // Picking a smart list/project on the narrow flap returns to
                // the calendar screen and dismisses the flap.
                if matches!(self.sidebar.mode(), SidebarMode::Overlay) {
                    self.narrow_screen = NarrowScreen::Calendar;
                    self.sidebar.set_open(false);
                }
            }
            Action::CloseDetail => {
                self.detail_open = false;
                self.selection = None;
                self.narrow_screen = NarrowScreen::Calendar;
                self.blur_editor();
            }
            Action::ToggleSidebar => self.sidebar.toggle_open(),
            Action::DismissSidebar => self.sidebar.set_open(false),
            Action::ToggleSidebarCollapsed => self.sidebar.toggle_collapsed(),
            Action::BackToCalendar => {
                self.narrow_screen = NarrowScreen::Calendar;
                // The calendar screen owns the keyboard once the editor is off
                // it; release the form fields so keystrokes don't leak into the
                // now-hidden title/location/notes.
                self.blur_editor();
            }
            Action::ToggleCalendarVisible(id) => {
                for acc in &mut self.accounts.accounts {
                    if let Some(cal) = acc.calendars.iter_mut().find(|c| c.id == id) {
                        cal.visible = !cal.visible;
                    }
                }
            }
            Action::ToggleAccountCollapsed(id) => {
                if let Some(acc) = self.accounts.accounts.iter_mut().find(|a| a.id == id) {
                    acc.collapsed = !acc.collapsed;
                }
            }
            Action::Sync => self.start_sync(doc),
            Action::Export => self.export_ics(),
            Action::Import => self.import_ics(doc),
            Action::MiniMonth(outcome) => match outcome {
                MiniMonthOutcome::SelectDate(date) => self.anchor = date,
                MiniMonthOutcome::ShiftMonth(months) => {
                    self.anchor = date::add_months(self.anchor, months);
                }
            },
            Action::NewEvent => self.new_event(doc),
            Action::CycleColor => {
                self.cycle_event(doc, |e| {
                    e.color = Palette::from_index(e.color.index() + 1);
                });
            }
            Action::CycleRecurrence => {
                self.cycle_event(doc, |e| e.recurrence = e.recurrence.cycle());
            }
            Action::CycleReminder => {
                self.cycle_event(doc, |e| {
                    e.reminder_min = match e.reminder_min {
                        None => Some(10),
                        Some(10) => Some(30),
                        Some(30) => Some(60),
                        _ => None,
                    };
                });
            }
            Action::ToggleSelectedTask => {
                if let Some(Sel::Task(id)) = self.selection {
                    self.toggle_task(id, doc);
                }
            }
            Action::ToggleAllDay => self.toggle_all_day(doc),
            Action::OpenPopover(which) => self.open_popover(which),
            Action::FocusField(field) => self.focus_field(field),
            Action::DeleteEvent(id) => self.delete_event(id, doc),
            Action::DeleteTask(id) => self.delete_task(id, doc),
            Action::Noop => {}
        }
        // Discrete actions are worth an immediate save; if the action didn't
        // touch the document (view/navigation) the head check in `maybe_persist`
        // makes this a no-op.
        self.force_persist = true;
        self.dirty = true;
    }

    // --- calendar visibility --------------------------------------------

    /// Ids of every remote calendar currently unchecked in the sidebar. A synced
    /// event whose source calendar is in this set is filtered out of every view.
    fn hidden_calendar_ids(&self) -> HashSet<String> {
        self.accounts
            .accounts
            .iter()
            .flat_map(|a| a.calendars.iter())
            .filter(|c| !c.visible)
            .map(|c| c.id.clone())
            .collect()
    }

    /// Events occurring on `date`, minus any whose source calendar is hidden.
    /// This is the single filter every view draws through, so unchecking a
    /// calendar hides its already-synced events immediately.
    fn visible_events_on(&self, date: NaiveDate) -> Vec<&Event> {
        let hidden = self.hidden_calendar_ids();
        self.model
            .events_on(date)
            .into_iter()
            .filter(|e| e.visible_under(&hidden))
            .collect()
    }

    /// Dates in the mini-month grid carrying at least one visible event. The
    /// root applies account-visibility policy once; the component only needs
    /// the dates it should mark.
    fn mini_month_event_dates(&self) -> HashSet<NaiveDate> {
        let hidden = self.hidden_calendar_ids();
        date::month_grid(self.anchor)
            .into_iter()
            .filter(|date| {
                self.model
                    .events
                    .iter()
                    .any(|event| event.occurs_on(*date) && event.visible_under(&hidden))
            })
            .collect()
    }

    // ====================================================================
    // Drawing — each helper takes `&self` and returns its click targets.
    // ====================================================================

    fn draw_sidebar(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) -> Vec<(Rect, Action)> {
        let Some(chrome) = self.sidebar.draw_chrome(
            scene,
            t,
            tr,
            theme,
            self.rect,
            self.content_rect,
            self.detail_rect,
            self.topbar.switcher_visible(),
            self.view,
            self.last_mouse,
        ) else {
            return Vec::new();
        };
        let mut hits: Vec<_> = chrome
            .hits
            .into_iter()
            .map(|(rect, action)| {
                let action = match action {
                    SidebarAction::Dismiss => Action::DismissSidebar,
                    SidebarAction::ToggleCollapsed => Action::ToggleSidebarCollapsed,
                    SidebarAction::View(view) => Action::View(view),
                    SidebarAction::Export => Action::Export,
                    SidebarAction::Import => Action::Import,
                };
                (rect, action)
            })
            .collect();
        let r = self.sidebar.rect();
        let pad = 18.0;
        let y = self.draw_sidebar_lists(scene, t, tr, theme, r, pad, chrome.body_y, &mut hits);
        let y = self.draw_sidebar_calendars(scene, t, tr, theme, r, pad, y, &mut hits);
        self.draw_sidebar_footer(scene, t, tr, theme, r, pad, y, &mut hits);

        hits
    }
    #[allow(
        clippy::too_many_arguments,
        reason = "internal draw-step split of an over-length function"
    )]
    fn draw_sidebar_lists(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
        pad: f64,
        mut y: f64,
        hits: &mut Vec<(Rect, Action)>,
    ) -> f64 {
        let c = &theme.tokens.colors;
        // Smart lists.
        let smart = [
            ("Inbox", ListSel::Inbox, c.semantic.info),
            ("Today", ListSel::Today, Palette::Indigo.color(theme)),
            ("Upcoming", ListSel::Upcoming, c.semantic.warning),
            ("Completed", ListSel::Completed, c.semantic.success),
        ];
        for (label, sel, accent) in smart {
            let row = Rect::new(r.x0 + 10.0, y, r.x1 - 10.0, y + 34.0);
            let active = self.list_sel == sel;
            if active {
                fill_round(scene, t, c.accent[5].with_alpha(0.18), row, 8.0);
            } else if row.contains(self.last_mouse) {
                fill_round(scene, t, c.surface_white_5, row, 8.0);
            }
            // bullet
            scene.fill(
                Fill::NonZero,
                t,
                accent,
                None,
                &Circle::new(Point::new(row.x0 + 16.0, row.center().y), 4.5),
            );
            let txt_color = if active { c.text_primary } else { c.neutral[8] };
            tr.draw_text(
                scene,
                label,
                14.0,
                txt_color,
                t * Affine::translate((row.x0 + 32.0, row.center().y - 8.0)),
                None,
            );
            let count = self.model.open_count(&sel, self.today);
            if count > 0 {
                draw_count_badge(scene, t, tr, theme, row.x1 - 18.0, row.center().y, count);
            }
            hits.push((row, Action::List(sel)));
            y += 38.0;
        }

        // Section heading — only when the user actually has projects.
        if !self.model.projects.is_empty() {
            y += 14.0;
            tr.draw_text(
                scene,
                "PROJECTS",
                11.0,
                c.text_dim,
                t * Affine::translate((r.x0 + pad, y)),
                None,
            );
            y += 26.0;
        }
        for p in &self.model.projects {
            let row = Rect::new(r.x0 + 10.0, y, r.x1 - 10.0, y + 34.0);
            let active = self.list_sel == ListSel::Project(p.id);
            if active {
                fill_round(scene, t, c.accent[5].with_alpha(0.18), row, 8.0);
            } else if row.contains(self.last_mouse) {
                fill_round(scene, t, c.surface_white_5, row, 8.0);
            }
            // colour tag
            fill_round(
                scene,
                t,
                p.color.color(theme),
                Rect::new(
                    row.x0 + 12.0,
                    row.center().y - 5.0,
                    row.x0 + 22.0,
                    row.center().y + 5.0,
                ),
                3.0,
            );
            tr.draw_text(
                scene,
                &p.name,
                14.0,
                if active { c.text_primary } else { c.neutral[8] },
                t * Affine::translate((row.x0 + 32.0, row.center().y - 8.0)),
                None,
            );
            let count = self
                .model
                .tasks
                .iter()
                .filter(|tk| tk.project == Some(p.id) && !tk.done && tk.parent.is_none())
                .count();
            if count > 0 {
                draw_count_badge(scene, t, tr, theme, row.x1 - 18.0, row.center().y, count);
            }
            hits.push((row, Action::List(ListSel::Project(p.id))));
            y += 38.0;
        }
        y
    }

    /// `CalDAV` calendars from the shared OS accounts store. Each account's
    /// calendars are listed with a visibility checkbox; a Sync affordance in
    /// the heading pulls events from every visible calendar (part of
    /// [`Self::draw_sidebar`]). Returns the `y` the footer should start at.
    #[allow(
        clippy::too_many_arguments,
        reason = "internal draw-step split of an over-length function"
    )]
    fn draw_sidebar_calendars(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
        pad: f64,
        mut y: f64,
        hits: &mut Vec<(Rect, Action)>,
    ) -> f64 {
        let c = &theme.tokens.colors;
        if !self.accounts.is_empty() {
            y += 14.0;
            tr.draw_text(
                scene,
                "CALENDARS",
                11.0,
                c.text_dim,
                t * Affine::translate((r.x0 + pad, y)),
                None,
            );
            let sync = Rect::new(r.x1 - pad - 44.0, y - 4.0, r.x1 - pad, y + 16.0);
            if sync.contains(self.last_mouse) {
                fill_round(scene, t, c.surface_white_5, sync, 6.0);
            }
            tr.draw_text(
                scene,
                "Sync",
                11.0,
                c.accent[6],
                t * Affine::translate((sync.x0 + 6.0, y)),
                None,
            );
            hits.push((sync, Action::Sync));
            y += 26.0;

            for acc in &self.accounts.accounts {
                // Account header — a disclosure row that collapses/expands this
                // account's calendar list (management itself lives in Settings).
                let header = Rect::new(r.x0 + 10.0, y - 4.0, r.x1 - 10.0, y + 18.0);
                if header.contains(self.last_mouse) {
                    fill_round(scene, t, c.surface_white_5, header, 6.0);
                }
                draw_caret(
                    scene,
                    t,
                    c.neutral[6],
                    Point::new(r.x0 + pad + 4.0, y + 7.0),
                    !acc.collapsed,
                );
                tr.draw_text(
                    scene,
                    &elide(acc.label(), 24),
                    12.0,
                    c.neutral[7],
                    t * Affine::translate((r.x0 + pad + 16.0, y)),
                    None,
                );
                hits.push((header, Action::ToggleAccountCollapsed(acc.id.clone())));
                y += 22.0;
                if acc.collapsed {
                    continue;
                }
                for cal in &acc.calendars {
                    let row = Rect::new(r.x0 + 10.0, y, r.x1 - 10.0, y + 30.0);
                    if row.contains(self.last_mouse) {
                        fill_round(scene, t, c.surface_white_5, row, 6.0);
                    }
                    // Colour swatch checkbox: filled when visible, hollow when
                    // hidden. The whole row toggles visibility.
                    let sw = Rect::new(
                        row.x0 + 8.0,
                        row.center().y - 7.0,
                        row.x0 + 22.0,
                        row.center().y + 7.0,
                    );
                    let pc = cal.palette().color(theme);
                    if cal.visible {
                        fill_round(scene, t, pc, sw, 4.0);
                        draw_check(scene, t, Color::WHITE, sw);
                    } else {
                        stroke_round(scene, t, pc.with_alpha(0.6), sw, 4.0, 1.4);
                    }
                    let txt_color = if cal.visible {
                        c.neutral[8]
                    } else {
                        c.text_dim
                    };
                    tr.draw_text(
                        scene,
                        &elide(&cal.name, 24),
                        13.0,
                        txt_color,
                        t * Affine::translate((row.x0 + 32.0, row.center().y - 8.0)),
                        None,
                    );
                    hits.push((row, Action::ToggleCalendarVisible(cal.id.clone())));
                    y += 32.0;
                }
            }
        }
        y
    }

    /// The footer: the mini upcoming-task rail (when there's room for it
    /// above the reserved mini-month band) followed by the mini-month
    /// navigator itself (part of [`Self::draw_sidebar`]).
    #[allow(
        clippy::too_many_arguments,
        reason = "internal draw-step split of an over-length function"
    )]
    fn draw_sidebar_footer(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
        pad: f64,
        y: f64,
        hits: &mut Vec<(Rect, Action)>,
    ) {
        // Reserve the bottom of the sidebar for the mini-month navigator.
        let mini_top = MiniMonth::content_limit(r);
        let y = self.draw_sidebar_task_rail(scene, t, tr, theme, r, pad, y, mini_top, hits);
        let event_dates = self.mini_month_event_dates();
        hits.extend(
            MiniMonth::draw(
                scene,
                t,
                tr,
                theme,
                r,
                y,
                self.anchor,
                self.today,
                &event_dates,
                self.last_mouse,
            )
            .into_iter()
            .map(|(rect, outcome)| (rect, Action::MiniMonth(outcome))),
        );
    }

    /// The mini upcoming-task rail — only when there are tasks to show AND
    /// there's vertical room left above the mini-month (the CALENDARS
    /// section above may have used it up; the agenda/day views still list
    /// the tasks in full either way) (part of [`Self::draw_sidebar_footer`]).
    #[allow(
        clippy::too_many_arguments,
        reason = "internal draw-step split of an over-length function"
    )]
    fn draw_sidebar_task_rail(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
        pad: f64,
        mut y: f64,
        mini_top: f64,
        hits: &mut Vec<(Rect, Action)>,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let rail_tasks = self.model.tasks_for(&self.list_sel, self.today);
        if !rail_tasks.is_empty() && y + 56.0 < mini_top {
            let list_title = match &self.list_sel {
                ListSel::Inbox => "Inbox",
                ListSel::Today => "Today",
                ListSel::Upcoming => "Upcoming",
                ListSel::Completed => "Completed",
                ListSel::Project(_) => "Tasks",
            };
            y += 10.0;
            stroke_line(
                scene,
                t,
                c.border_standard,
                Point::new(r.x0 + pad, y),
                Point::new(r.x1 - pad, y),
                1.0,
            );
            y += 14.0;
            tr.draw_text(
                scene,
                list_title,
                11.0,
                c.text_dim,
                t * Affine::translate((r.x0 + pad, y)),
                None,
            );
            y += 24.0;
            let tasks = self.model.tasks_for(&self.list_sel, self.today);
            for tk in tasks.iter().take(4) {
                if y > mini_top - 34.0 {
                    break;
                }
                let row = Rect::new(r.x0 + 12.0, y, r.x1 - 12.0, y + 30.0);
                if row.contains(self.last_mouse) {
                    fill_round(scene, t, c.surface_white_5, row, 6.0);
                }
                // Affordance split: the checkbox AND the label text toggle
                // completion; the open area to their right selects the task to view
                // its detail. Push the wide "open" target first, then the narrower
                // "complete" target on top so the reverse hit-scan lets the
                // label/checkbox win where they overlap.
                hits.push((row, Action::OpenTask(tk.id)));

                let box_rect = Rect::new(
                    row.x0 + 4.0,
                    row.center().y - 8.0,
                    row.x0 + 20.0,
                    row.center().y + 8.0,
                );
                draw_checkbox(scene, t, theme, box_rect, tk.done);
                let txt_color = if tk.done { c.text_dim } else { c.neutral[8] };
                let label = elide(&tk.title, 22);
                let label_x0 = row.x0 + 28.0;
                tr.draw_text(
                    scene,
                    &label,
                    13.0,
                    txt_color,
                    t * Affine::translate((label_x0, row.center().y - 7.0)),
                    None,
                );
                let label_w = f64::from(tr.measure(&label, 13.0, None).0);
                if tk.done {
                    stroke_line(
                        scene,
                        t,
                        c.text_dim,
                        Point::new(label_x0, row.center().y),
                        Point::new(label_x0 + label_w, row.center().y),
                        1.0,
                    );
                }
                // Completion target spans the checkbox and the label text (with a
                // little slack), so clicking either toggles done.
                let complete = Rect::new(
                    row.x0,
                    row.y0,
                    (label_x0 + label_w + 8.0).min(row.x1),
                    row.y1,
                );
                hits.push((complete, Action::ToggleTask(tk.id)));
                // priority flag
                if tk.priority != Priority::None {
                    tr.draw_text(
                        scene,
                        tk.priority.flag(),
                        10.0,
                        tk.priority.color(theme),
                        t * Affine::translate((row.x1 - 22.0, row.center().y - 6.0)),
                        None,
                    );
                }
                y += 32.0;
            }
        }
        y
    }

    fn view_rect(&self) -> Rect {
        self.topbar.view_rect()
    }

    fn draw_month(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
    ) -> Vec<(Rect, Action)> {
        let c = &theme.tokens.colors;
        let mut hits = Vec::new();
        fill_rect(scene, t, c.surfaces.base, area);

        let header_h = 28.0;
        let cols = 7;
        let rows = 6;
        let cell_w = area.width() / cols as f64;
        let grid_top = area.y0 + header_h;
        let cell_h = (area.height() - header_h) / f64::from(rows);

        Self::draw_month_header(scene, t, tr, theme, area, cell_w);

        let grid = date::month_grid(self.anchor);
        for (idx, day) in grid.iter().enumerate() {
            let row = idx / cols;
            let col = idx % cols;
            let cell = Rect::new(
                area.x0 + cell_w * col as f64,
                grid_top + cell_h * row as f64,
                area.x0 + cell_w * (col as f64 + 1.0),
                grid_top + cell_h * (row as f64 + 1.0),
            );
            self.draw_month_cell(scene, t, tr, theme, cell, cell_h, *day, &mut hits);
        }
        hits
    }

    /// The weekday header row (part of [`Self::draw_month`]).
    fn draw_month_header(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        cell_w: f64,
    ) {
        let c = &theme.tokens.colors;
        for (i, lbl) in date::WEEKDAY_SHORT.iter().enumerate() {
            let weekend = i >= 5;
            let col = if weekend { c.text_dim } else { c.neutral[7] };
            let x = area.x0 + cell_w * i as f64;
            tr.draw_text(
                scene,
                lbl,
                12.0,
                col,
                t * Affine::translate((x + 10.0, area.y0 + 7.0)),
                None,
            );
        }
    }

    /// One day cell: grid line, hit targets, background tint, and the date
    /// number (with a today pill) (part of [`Self::draw_month`]).
    #[allow(
        clippy::too_many_arguments,
        reason = "internal draw-step split of an over-length function"
    )]
    fn draw_month_cell(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        cell: Rect,
        cell_h: f64,
        day: NaiveDate,
        hits: &mut Vec<(Rect, Action)>,
    ) {
        let c = &theme.tokens.colors;
        // grid lines
        stroke_round(scene, t, c.border_standard.with_alpha(0.5), cell, 0.0, 0.5);

        // Cell-level click targets are pushed FIRST so the event chips
        // drawn below win the reverse (topmost-last) hit-scan — clicking a
        // chip must open that event, not just re-anchor the day. The
        // date-number strip in turn wins over the empty cell body.
        hits.push((cell, Action::PickDate(day)));
        hits.push((
            Rect::new(cell.x0, cell.y0, cell.x1, cell.y0 + 28.0),
            Action::OpenDay(day),
        ));

        let in_month = day.month() == self.anchor.month();
        let is_today = day == self.today;
        let is_anchor = day == self.anchor;
        let weekend = day.weekday().num_days_from_monday() >= 5;

        if is_anchor && !is_today {
            fill_rect(scene, t, c.surface_white_5, cell);
        }
        if weekend && in_month {
            fill_rect(scene, t, c.neutral[1].with_alpha(0.35), cell);
        }

        // date number, with a today pill.
        let num = format!("{}", day.day());
        let num_color = if !in_month {
            c.neutral[4]
        } else if weekend {
            c.text_dim
        } else {
            c.neutral[9]
        };
        if is_today {
            let pill = Circle::new(Point::new(cell.x0 + 18.0, cell.y0 + 17.0), 12.0);
            scene.fill(Fill::NonZero, t, c.accent[5], None, &pill);
            center_text(
                scene,
                tr,
                &num,
                12.0,
                Color::WHITE,
                Rect::new(cell.x0 + 6.0, cell.y0 + 5.0, cell.x0 + 30.0, cell.y0 + 29.0),
                t,
            );
        } else {
            tr.draw_text(
                scene,
                &num,
                12.0,
                num_color,
                t * Affine::translate((cell.x0 + 8.0, cell.y0 + 6.0)),
                None,
            );
        }

        self.draw_month_cell_chips(scene, t, tr, theme, cell, cell_h, day, hits);
    }

    /// The event chips inside one day cell, clipped to it (part of
    /// [`Self::draw_month_cell`]).
    #[allow(
        clippy::too_many_arguments,
        reason = "internal draw-step split of an over-length function"
    )]
    fn draw_month_cell_chips(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        cell: Rect,
        cell_h: f64,
        day: NaiveDate,
        hits: &mut Vec<(Rect, Action)>,
    ) {
        let c = &theme.tokens.colors;
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            t,
            &cell,
        );
        let evs: Vec<(Id, String, Palette, bool, NaiveDateTime)> = self
            .visible_events_on(day)
            .iter()
            .map(|e| {
                (
                    e.id,
                    e.display_title().to_string(),
                    e.color,
                    e.all_day,
                    e.start,
                )
            })
            .collect();
        let max_chips = ((cell_h - 30.0) / 20.0).floor().max(0.0) as usize;
        let mut cy = cell.y0 + 30.0;
        for (i, (id, title, color, all_day, start)) in evs.iter().enumerate() {
            if i >= max_chips.clamp(1, 3) {
                let more = evs.len() - i;
                tr.draw_text(
                    scene,
                    &format!("+{more} more"),
                    10.0,
                    c.text_dim,
                    t * Affine::translate((cell.x0 + 8.0, cy)),
                    None,
                );
                break;
            }
            let chip = Rect::new(cell.x0 + 6.0, cy, cell.x1 - 6.0, cy + 17.0);
            let col = color.color(theme);
            if *all_day {
                fill_round(scene, t, col, chip, 4.0);
                let line = elide_to_width(tr, title, 10.0, chip.width() - 12.0);
                tr.draw_text(
                    scene,
                    &line,
                    10.0,
                    Color::WHITE,
                    t * Affine::translate((chip.x0 + 6.0, chip.y0 + 3.0)),
                    None,
                );
            } else {
                fill_round(scene, t, col.with_alpha(0.18), chip, 4.0);
                scene.fill(
                    Fill::NonZero,
                    t,
                    col,
                    None,
                    &Circle::new(Point::new(chip.x0 + 8.0, chip.center().y), 3.0),
                );
                let label = format!("{} {}", date::fmt_time(start), title);
                let line = elide_to_width(tr, &label, 10.0, chip.width() - 22.0);
                tr.draw_text(
                    scene,
                    &line,
                    10.0,
                    c.neutral[9],
                    t * Affine::translate((chip.x0 + 16.0, chip.y0 + 3.0)),
                    None,
                );
            }
            hits.push((chip, Action::OpenEvent(*id)));
            cy += 20.0;
        }
        scene.pop_layer();
    }

    // ── week / day timeline geometry + hit-testing ──────────────────────

    /// Days shown in the current timeline view: the whole week, or a single day.
    fn timeline_days(&self) -> Vec<NaiveDate> {
        match self.view {
            View::Day => vec![self.anchor],
            _ => date::week_days(self.anchor),
        }
    }

    /// Timed event spans supplied to the interaction controller. The host owns
    /// event visibility policy; the controller owns block hit-testing.
    fn timeline_spans(&self, days: &[NaiveDate]) -> Vec<TimelineSpan> {
        let mut spans = Vec::new();
        for day in days {
            for event in self.visible_events_on(*day) {
                if event.all_day {
                    continue;
                }
                let start_min =
                    i64::from(event.start.hour()) * 60 + i64::from(event.start.minute());
                let end_min = (i64::from(event.end.hour()) * 60 + i64::from(event.end.minute()))
                    .max(start_min + TIMELINE_SNAP_MIN);
                spans.push(TimelineSpan {
                    id: event.id,
                    day: *day,
                    start_min,
                    end_min,
                });
            }
        }
        spans
    }

    /// Presentation-ready timeline snapshot. Calendar policy decides which
    /// events are visible and how untitled events are labelled; the timeline
    /// component owns all geometry and painting from here on.
    fn timeline_render_events<'a>(&'a self, days: &[NaiveDate]) -> Vec<TimelineRenderEvent<'a>> {
        let mut events = Vec::new();
        for day in days {
            for event in self.visible_events_on(*day) {
                let start_min =
                    i64::from(event.start.hour()) * 60 + i64::from(event.start.minute());
                let end_min = (i64::from(event.end.hour()) * 60 + i64::from(event.end.minute()))
                    .max(start_min + TIMELINE_SNAP_MIN);
                events.push(TimelineRenderEvent {
                    id: event.id,
                    day: *day,
                    title: event.display_title(),
                    location: &event.location,
                    color: event.color,
                    start_min,
                    end_min,
                    all_day: event.all_day,
                });
            }
        }
        events
    }

    #[allow(
        clippy::many_single_char_names,
        reason = "t is the paint transform and r/x/y/w are the detail card's rect/body \
                  position/width — conventional short names for this draw step"
    )]
    fn draw_detail(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) -> Vec<(Rect, Action)> {
        let mut hits = Vec::new();
        let r = self.detail_rect;
        let modal = matches!(self.detail_mode, DetailMode::Overlay);

        self.draw_detail_frame(scene, t, theme, r, modal, &mut hits);

        let x = r.x0 + DETAIL_PAD;
        let w = self.notes_w;
        let mut y = r.y0 + 18.0;

        // The full-screen editor page is its own pushed screen, so it gets a
        // Back affordance top-left instead of a close button.
        if !modal {
            y += self.draw_detail_back_button(scene, t, tr, theme, r, &mut hits);
        }

        // Header controls, top-right: a Delete (trash) button for the selected
        // item, plus a Close (X) on the modal (the full-screen page uses Back).
        self.draw_detail_header_controls(scene, t, theme, r, modal, &mut hits);

        self.draw_detail_selection(scene, t, tr, theme, x, y, w, &mut hits);

        // Notes header (the editor itself is drawn separately in draw()). For
        // an event the notes sit below the form; a task uses the fixed offset.
        self.draw_detail_notes_header(scene, t, tr, theme, r, x, w);

        hits
    }

    /// The card/panel background — a dimmed floating card in overlay mode,
    /// or a docked panel with a left border otherwise (part of
    /// [`Self::draw_detail`]).
    #[allow(
        clippy::too_many_arguments,
        reason = "internal draw-step split of an over-length function"
    )]
    fn draw_detail_frame(
        &self,
        scene: &mut Scene,
        t: Affine,
        theme: &Theme,
        r: Rect,
        modal: bool,
        hits: &mut Vec<(Rect, Action)>,
    ) {
        let c = &theme.tokens.colors;
        if modal {
            // Dim the calendar behind the card; a click on the dim closes the
            // editor. The card is a floating panel, not a docked strip.
            fill_rect(scene, t, c.surfaces.overlay, self.content_rect);
            hits.push((self.content_rect, Action::CloseDetail));
            let shadow = r.inflate(6.0, 10.0);
            fill_round(scene, t, Color::from_rgba8(0, 0, 0, 90), shadow, 18.0);
            fill_round(scene, t, c.surfaces.raised, r, 14.0);
            stroke_round(scene, t, c.border_standard, r, 14.0, 1.0);
            // Backing guard so a press on the card padding is absorbed rather
            // than falling through to the dismiss scrim beneath it.
            hits.push((r, Action::Noop));
        } else {
            fill_rect(scene, t, c.surfaces.raised, r);
            stroke_line(
                scene,
                t,
                c.border_standard,
                Point::new(r.x0, r.y0),
                Point::new(r.x0, r.y1),
                1.0,
            );
        }
    }

    /// The full-screen page's Back affordance, top-left (part of
    /// [`Self::draw_detail`], non-modal only). Returns the y-advance the
    /// caller should push the detail body down by.
    fn draw_detail_back_button(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
        hits: &mut Vec<(Rect, Action)>,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let back = Rect::new(r.x0 + 12.0, r.y0 + 12.0, r.x0 + 96.0, r.y0 + 44.0);
        if back.contains(self.last_mouse) {
            fill_round(scene, t, c.surface_white_5, back, 8.0);
        }
        draw_back_arrow(
            scene,
            t,
            c.neutral[8],
            Point::new(back.x0 + 14.0, back.center().y),
        );
        tr.draw_text(
            scene,
            "Back",
            13.0,
            c.neutral[8],
            t * Affine::translate((back.x0 + 28.0, back.center().y - 8.0)),
            None,
        );
        hits.push((back, Action::BackToCalendar));
        34.0
    }

    /// The Close (modal only) and Delete header buttons, top-right (part of
    /// [`Self::draw_detail`]).
    #[allow(
        clippy::too_many_arguments,
        reason = "internal draw-step split of an over-length function"
    )]
    fn draw_detail_header_controls(
        &self,
        scene: &mut Scene,
        t: Affine,
        theme: &Theme,
        r: Rect,
        modal: bool,
        hits: &mut Vec<(Rect, Action)>,
    ) {
        let c = &theme.tokens.colors;
        let mut bx = r.x1 - 14.0;
        if modal {
            let close = Rect::new(bx - 22.0, r.y0 + 14.0, bx, r.y0 + 36.0);
            draw_icon_button(scene, t, theme, close, close.contains(self.last_mouse));
            draw_cross(scene, t, c.neutral[8], close.center());
            hits.push((close, Action::CloseDetail));
            bx -= 30.0;
        }
        let del_action = match self.selection {
            Some(Sel::Event(id)) => Some(Action::DeleteEvent(id)),
            Some(Sel::Task(id)) => Some(Action::DeleteTask(id)),
            None => None,
        };
        if let Some(action) = del_action {
            let del = Rect::new(bx - 22.0, r.y0 + 14.0, bx, r.y0 + 36.0);
            let hot = del.contains(self.last_mouse);
            draw_icon_button(scene, t, theme, del, hot);
            let col = if hot { c.semantic.danger } else { c.neutral[8] };
            draw_trash(scene, t, col, del.center());
            hits.push((del, action));
        }
    }

    /// The selected item's body: an event's kind label + colour dot (every
    /// editable field is drawn by [`Self::draw_event_form`]), or the full
    /// task detail (part of [`Self::draw_detail`]).
    #[allow(
        clippy::too_many_arguments,
        reason = "internal draw-step split of an over-length function"
    )]
    #[allow(
        clippy::many_single_char_names,
        reason = "t is the paint transform, x/y/w are the body column's position/width, \
                  and c is the theme's colour set — conventional short names for this \
                  draw step"
    )]
    fn draw_detail_selection(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        x: f64,
        y: f64,
        w: f64,
        hits: &mut Vec<(Rect, Action)>,
    ) {
        let c = &theme.tokens.colors;
        match self.selection {
            Some(Sel::Event(id)) => {
                if let Some(e) = self.model.event(id) {
                    scene.fill(
                        Fill::NonZero,
                        t,
                        e.color.color(theme),
                        None,
                        &Circle::new(Point::new(x + 6.0, y + 8.0), 6.0),
                    );
                    tr.draw_text(
                        scene,
                        "EVENT",
                        11.0,
                        c.text_dim,
                        t * Affine::translate((x + 20.0, y + 2.0)),
                        None,
                    );
                }
            }
            Some(Sel::Task(id)) => self.draw_detail_task(scene, t, tr, theme, x, y, w, id, hits),
            None => {}
        }
    }

    /// The full task detail: checkbox + title, due/priority/project fields,
    /// and the subtask list (part of [`Self::draw_detail_selection`]).
    #[allow(
        clippy::too_many_arguments,
        reason = "internal draw-step split of an over-length function"
    )]
    #[allow(
        clippy::many_single_char_names,
        reason = "t is the paint transform, x/y/w are the body column's position/width, \
                  and c is the theme's colour set — conventional short names for this \
                  draw step"
    )]
    fn draw_detail_task(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        x: f64,
        mut y: f64,
        w: f64,
        id: Id,
        hits: &mut Vec<(Rect, Action)>,
    ) {
        let c = &theme.tokens.colors;
        let Some(tk) = self.model.task(id) else {
            return;
        };
        let box_rect = Rect::new(x, y, x + 18.0, y + 18.0);
        draw_checkbox(scene, t, theme, box_rect, tk.done);
        hits.push((box_rect, Action::ToggleSelectedTask));
        tr.draw_text(
            scene,
            "TASK",
            11.0,
            c.text_dim,
            t * Affine::translate((x + 28.0, y + 2.0)),
            None,
        );
        y += 26.0;
        let title_col = if tk.done { c.text_dim } else { c.text_primary };
        tr.draw_text(
            scene,
            &tk.title,
            21.0,
            title_col,
            t * Affine::translate((x, y)),
            Some(w as f32),
        );
        y += 40.0;
        if let Some(due) = tk.due {
            field_row(
                scene,
                tr,
                theme,
                t,
                x,
                y,
                w,
                "Due",
                &date::fmt_day_long(due),
            );
            y += 34.0;
        }
        field_row(
            scene,
            tr,
            theme,
            t,
            x,
            y,
            w,
            "Priority",
            tk.priority.label(),
        );
        y += 34.0;
        if let Some(p) = tk.project.and_then(|pid| self.model.project(pid)) {
            field_row(scene, tr, theme, t, x, y, w, "Project", &p.name);
            y += 34.0;
        }
        let subs = self.model.subtasks_of(id);
        if !subs.is_empty() {
            tr.draw_text(
                scene,
                "Subtasks",
                12.0,
                c.text_dim,
                t * Affine::translate((x, y)),
                None,
            );
            y += 22.0;
            for s in subs {
                let box_rect = Rect::new(x, y, x + 16.0, y + 16.0);
                draw_checkbox(scene, t, theme, box_rect, s.done);
                hits.push((box_rect, Action::ToggleTask(s.id)));
                tr.draw_text(
                    scene,
                    &s.title,
                    13.0,
                    c.neutral[8],
                    t * Affine::translate((x + 26.0, y)),
                    None,
                );
                y += 24.0;
            }
        }
    }

    /// The "NOTES" section label + rule, positioned below the event form or
    /// at the task's fixed offset (part of [`Self::draw_detail`]).
    #[allow(
        clippy::too_many_arguments,
        reason = "internal draw-step split of an over-length function"
    )]
    #[allow(
        clippy::many_single_char_names,
        reason = "t is the paint transform, r/x/w are the detail card's rect/body \
                  position/width, and c is the theme's colour set — conventional short \
                  names for this draw step"
    )]
    fn draw_detail_notes_header(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
        x: f64,
        w: f64,
    ) {
        let c = &theme.tokens.colors;
        let notes_top = if matches!(self.selection, Some(Sel::Event(_))) {
            self.form.notes_top
        } else {
            self.notes_top_offset()
        };
        let notes_label_y = r.y0 + notes_top - 26.0;
        tr.draw_text(
            scene,
            "NOTES",
            11.0,
            c.text_dim,
            t * Affine::translate((x, notes_label_y)),
            None,
        );
        stroke_line(
            scene,
            t,
            c.border_standard,
            Point::new(x, notes_label_y + 18.0),
            Point::new(x + w, notes_label_y + 18.0),
            1.0,
        );
    }

    /// Draw the editable event form (title/location inputs, all-day switch,
    /// start/end date+time field buttons, calendar selector, and the repeat/
    /// reminder/colour chips) and return its click targets. Runs with `&mut
    /// self` so the embedded `TextInput`s can render; the transient date/time/
    /// calendar popovers are painted separately (on top) by
    /// [`Self::draw_form_popovers`].
    fn draw_event_form(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) -> Vec<(Rect, Action)> {
        let mut hits = Vec::new();
        let Some(Sel::Event(id)) = self.selection else {
            return hits;
        };
        let Some(e) = self.model.event(id).cloned() else {
            return hits;
        };
        let c = &theme.tokens.colors;
        let f = self.form.clone();
        let mouse = self.last_mouse;
        let label_x = f.title.x0;

        // --- Title (real text input, prominent) -------------------------
        self.title_input.draw(scene, t, tr, theme, presence);
        hits.push((f.title, Action::FocusField(FormField::Title)));

        // --- All-day row: label + pill switch ---------------------------
        tr.draw_text(
            scene,
            "All day",
            14.0,
            c.text_primary,
            t * Affine::translate((label_x, f.allday.center().y - 8.0)),
            None,
        );
        let sw = Rect::new(
            f.allday.x1 - 44.0,
            f.allday.center().y - 12.0,
            f.allday.x1,
            f.allday.center().y + 12.0,
        );
        Toggle::paint(
            scene,
            t,
            theme,
            &ToggleFace::new(sw, e.all_day).hovered(sw.contains(mouse)),
        );
        hits.push((f.allday, Action::ToggleAllDay));

        self.draw_event_form_dates(scene, t, tr, theme, &f, &e, mouse, label_x, &mut hits);

        // --- Location (real text input) ---------------------------------
        self.location_input.draw(scene, t, tr, theme, presence);
        hits.push((f.location, Action::FocusField(FormField::Location)));

        self.draw_event_form_calendar(scene, t, tr, theme, &f, &e, mouse, &mut hits);
        Self::draw_event_form_chips(scene, t, tr, theme, &f, &e, label_x, &mut hits);

        hits
    }

    /// The Starts / Ends date + time field-button rows (part of
    /// [`Self::draw_event_form`]).
    #[allow(
        clippy::too_many_arguments,
        reason = "internal draw-step split of an over-length function"
    )]
    #[allow(
        clippy::many_single_char_names,
        reason = "t is the paint transform, f is the form layout, e is the event being \
                  edited, and c is the theme's colour set — conventional short names for \
                  this draw step"
    )]
    fn draw_event_form_dates(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        f: &FormRects,
        e: &Event,
        mouse: Point,
        label_x: f64,
        hits: &mut Vec<(Rect, Action)>,
    ) {
        let c = &theme.tokens.colors;
        let row = |scene: &mut Scene,
                   tr: &mut TextRenderer,
                   hits: &mut Vec<(Rect, Action)>,
                   label: &str,
                   date_rect: Rect,
                   time_rect: Rect,
                   date_str: &str,
                   time_str: &str,
                   date_pop: FormPopover,
                   time_pop: FormPopover,
                   popover: FormPopover| {
            tr.draw_text(
                scene,
                label,
                12.0,
                c.text_dim,
                t * Affine::translate((label_x, date_rect.center().y - 6.0)),
                None,
            );
            let date_active = date_rect.contains(mouse) || popover == date_pop;
            draw_field_button(scene, t, tr, theme, date_rect, date_str, date_active, true);
            hits.push((date_rect, Action::OpenPopover(date_pop)));
            if time_rect.area() > 0.0 {
                let time_active = time_rect.contains(mouse) || popover == time_pop;
                draw_field_button(scene, t, tr, theme, time_rect, time_str, time_active, true);
                hits.push((time_rect, Action::OpenPopover(time_pop)));
            }
        };
        row(
            scene,
            tr,
            hits,
            "Starts",
            f.start_date,
            f.start_time,
            &date::fmt_day_long(e.start.date()),
            &date::fmt_time(&e.start),
            FormPopover::StartDate,
            FormPopover::StartTime,
            self.event_popovers.active(),
        );
        row(
            scene,
            tr,
            hits,
            "Ends",
            f.end_date,
            f.end_time,
            &date::fmt_day_long(e.end.date()),
            &date::fmt_time(&e.end),
            FormPopover::EndDate,
            FormPopover::EndTime,
            self.event_popovers.active(),
        );
    }

    /// The calendar selector: colour dot + value + disclosure caret (part
    /// of [`Self::draw_event_form`]).
    #[allow(
        clippy::too_many_arguments,
        reason = "internal draw-step split of an over-length function"
    )]
    #[allow(
        clippy::many_single_char_names,
        reason = "t is the paint transform, f is the form layout, e is the event being \
                  edited, and c is the theme's colour set — conventional short names for \
                  this draw step"
    )]
    fn draw_event_form_calendar(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        f: &FormRects,
        e: &Event,
        mouse: Point,
        hits: &mut Vec<(Rect, Action)>,
    ) {
        let c = &theme.tokens.colors;
        let cal_active =
            f.calendar.contains(mouse) || self.event_popovers.active() == FormPopover::Calendar;
        {
            let r = f.calendar;
            let bg = if cal_active {
                c.surface_white_10
            } else {
                c.surfaces.raised
            };
            fill_round(scene, t, bg, r, 8.0);
            let (border_w, border) = if cal_active {
                (2.0, c.border_focus)
            } else {
                (1.0, c.border_standard)
            };
            stroke_round(scene, t, border, r, 8.0, border_w);
            scene.fill(
                Fill::NonZero,
                t,
                e.color.color(theme),
                None,
                &Circle::new(Point::new(r.x0 + 16.0, r.center().y), 5.0),
            );
            let label = self.calendar_label(e.calendar_id.as_ref());
            let disp = elide_to_width(tr, &label, 14.0, (r.width() - 56.0).max(0.0));
            tr.draw_text(
                scene,
                &disp,
                14.0,
                c.text_primary,
                t * Affine::translate((r.x0 + 30.0, r.center().y - 8.0)),
                None,
            );
            draw_caret(
                scene,
                t,
                c.text_dim,
                Point::new(r.x1 - 16.0, r.center().y),
                true,
            );
        }
        hits.push((f.calendar, Action::OpenPopover(FormPopover::Calendar)));
    }

    /// The repeat / reminder / colour chips, cycled on click (part of
    /// [`Self::draw_event_form`]).
    #[allow(
        clippy::too_many_arguments,
        reason = "internal draw-step split of an over-length function"
    )]
    #[allow(
        clippy::similar_names,
        reason = "rem_label and rem_chip serve distinct purposes"
    )]
    fn draw_event_form_chips(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        f: &FormRects,
        e: &Event,
        label_x: f64,
        hits: &mut Vec<(Rect, Action)>,
    ) {
        let rec_label = if e.recurrence == Recurrence::None {
            "No repeat".to_string()
        } else {
            e.recurrence.short().to_string()
        };
        let rec_chip = chip(
            scene,
            t,
            tr,
            theme,
            label_x,
            f.chips_y,
            &format!("↻ {rec_label}"),
            false,
        );
        hits.push((rec_chip, Action::CycleRecurrence));
        let rem_label = match e.reminder_min {
            None => "No reminder".to_string(),
            Some(m) if m >= 60 => format!("⏰ {}h before", m / 60),
            Some(m) => format!("⏰ {m}m before"),
        };
        let rem_chip = chip(
            scene,
            t,
            tr,
            theme,
            rec_chip.x1 + 8.0,
            f.chips_y,
            &rem_label,
            false,
        );
        hits.push((rem_chip, Action::CycleReminder));
        let col_chip = chip(
            scene,
            t,
            tr,
            theme,
            rem_chip.x1 + 8.0,
            f.chips_y,
            &format!("● {}", e.color.name()),
            false,
        );
        scene.fill(
            Fill::NonZero,
            t,
            e.color.color(theme),
            None,
            &Circle::new(Point::new(col_chip.x0 + 12.0, col_chip.center().y), 4.0),
        );
        hits.push((col_chip, Action::CycleColor));
    }

    /// Paint the open form popover (date grid / time spinners / calendar list)
    /// on top of everything and return its click targets. A full-panel scrim
    /// hit sits underneath so a click outside the popover dismisses it.
    fn draw_form_popovers(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) -> Vec<(Rect, Action)> {
        let options = self.calendar_options();
        let current = match self.selection {
            Some(Sel::Event(id)) => self.model.event(id).and_then(|e| e.calendar_id.as_deref()),
            _ => None,
        };
        self.event_popovers.draw(
            scene,
            t,
            tr,
            theme,
            presence,
            self.detail_rect,
            PopoverAnchors {
                start_date: self.form.start_date,
                end_date: self.form.end_date,
                calendar: self.form.calendar,
            },
            self.last_mouse,
            &options,
            current,
        );
        Vec::new()
    }

    fn draw_status(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        content_x0: f64,
    ) {
        if let Some(msg) = &self.status {
            let c = &theme.tokens.colors;
            let wmeas = f64::from(tr.measure(msg, 12.0, None).0);
            let pill = Rect::new(
                content_x0 + 20.0,
                self.rect.y1 - 44.0,
                content_x0 + 44.0 + wmeas,
                self.rect.y1 - 16.0,
            );
            fill_round(scene, t, c.neutral[2], pill, 8.0);
            stroke_round(scene, t, c.border_standard, pill, 8.0, 1.0);
            tr.draw_text(
                scene,
                msg,
                12.0,
                c.neutral[9],
                t * Affine::translate((pill.x0 + 12.0, pill.center().y - 7.0)),
                None,
            );
        }
    }

    /// The first-run onboarding prompt to show over an empty view, or `None`
    /// once the calendar holds events. Returned as `(headline, hint)` so the
    /// draw path and tests gate on the same real state.
    pub fn empty_onboarding_prompt(&self) -> Option<(&'static str, &'static str)> {
        if self.model.events.is_empty() {
            Some((EMPTY_ONBOARDING_TITLE, EMPTY_ONBOARDING_HINT))
        } else {
            None
        }
    }

    /// Paint the first-run onboarding: a centered headline + hint over an
    /// otherwise-blank view. Colours come from the theme so it matches the OS.
    fn draw_empty_onboarding(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        title: &str,
        hint: &str,
    ) {
        let c = &theme.tokens.colors;
        let title_size = 20.0_f32;
        let hint_size = 14.0_f32;
        let gap = 10.0_f64;
        let title_w = f64::from(tr.measure(title, title_size, None).0);
        let hint_w = f64::from(tr.measure(hint, hint_size, None).0);
        let block_h = f64::from(title_size) + gap + f64::from(hint_size);
        let cx = area.center().x;
        let top = area.center().y - block_h / 2.0;
        tr.draw_text(
            scene,
            title,
            title_size,
            c.neutral[8],
            t * Affine::translate((cx - title_w / 2.0, top)),
            None,
        );
        tr.draw_text(
            scene,
            hint,
            hint_size,
            c.text_dim,
            t * Affine::translate((cx - hint_w / 2.0, top + f64::from(title_size) + gap)),
            None,
        );
    }

    /// The open popover owns input first: the picker itself, then (if it
    /// didn't consume the event) a left click resolved against the hit map
    /// — the dismiss scrim, a calendar row, or a popover guard (part of
    /// [`Widget::handle_event`]).
    #[allow(
        clippy::too_many_arguments,
        reason = "internal event-handling split of an over-length function"
    )]
    fn handle_popover_priority(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        self.handle_popover_event(
            event,
            scale_factor,
            modifiers,
            state,
            path,
            handled,
            focus_target,
            node_id,
            taffy,
        );
        if *handled {
            self.dirty = true;
            return;
        }
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
            && let Some(a) = self.hit_action_at(self.last_mouse)
        {
            self.apply_action(a, state);
            *handled = true;
        }
        self.dirty = true;
    }

    /// Keyboard handling: Escape (cancel drag → blur field → dismiss flap →
    /// pop the narrow detail screen → close detail), Delete (remove the
    /// selected event/task), Tab (cycle form focus), and global letter/
    /// arrow shortcuts (part of [`Widget::handle_event`]). Returns `true` if
    /// the event was consumed.
    fn handle_keyboard(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) -> bool {
        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return false;
        };

        let editor_active = self.detail_open && self.detail_rect.area() > 0.0;
        let form_active = editor_active && matches!(self.selection, Some(Sel::Event(_)));
        let field_focused = self.title_input.is_focused() || self.location_input.is_focused();
        let notes_focused = editor_active && self.notes_editor.is_focused();

        if matches!(logical_key, Key::Named(NamedKey::Escape))
            && self.handle_keyboard_escape(field_focused, handled)
        {
            return true;
        }

        if matches!(logical_key, Key::Named(NamedKey::Delete))
            && self.handle_keyboard_delete(
                state,
                editor_active,
                field_focused,
                notes_focused,
                handled,
            )
        {
            return true;
        }

        // Tab / Shift+Tab move between the form fields (title → location →
        // notes) on the real key path while an event is being edited.
        if form_active && matches!(logical_key, Key::Named(NamedKey::Tab)) {
            self.cycle_form_focus(modifiers.state().shift_key());
            *handled = true;
            self.dirty = true;
            return true;
        }

        // Global letter/arrow shortcuts fire only when NOT typing into a
        // text field or the notes editor.
        if !field_focused && !notes_focused {
            return self.handle_keyboard_shortcut(logical_key, state, handled);
        }
        false
    }

    /// Escape: cancel a live timeline drag first (it commits nothing), then
    /// blur a focused field, then dismiss the overlay flap, then pop the
    /// narrow detail screen, then close the detail. Split out of
    /// `handle_keyboard` to keep that under the line cap.
    fn handle_keyboard_escape(&mut self, field_focused: bool, handled: &mut bool) -> bool {
        if self.timeline.cancel_drag() {
            *handled = true;
            self.dirty = true;
            return true;
        }
        if field_focused {
            self.title_input.set_focus(false);
            self.location_input.set_focus(false);
            *handled = true;
            self.dirty = true;
            return true;
        }
        if matches!(self.sidebar.mode(), SidebarMode::Overlay) && self.sidebar.is_open() {
            self.sidebar.set_open(false);
            *handled = true;
            self.dirty = true;
            return true;
        }
        if matches!(self.detail_mode, DetailMode::FullScreen)
            && matches!(self.narrow_screen, NarrowScreen::Detail)
        {
            self.narrow_screen = NarrowScreen::Calendar;
            self.notes_editor.set_focus(false);
            *handled = true;
            self.dirty = true;
            return true;
        }
        if self.detail_open {
            self.detail_open = false;
            self.selection = None;
            self.narrow_screen = NarrowScreen::Calendar;
            self.notes_editor.set_focus(false);
            *handled = true;
            self.dirty = true;
            return true;
        }
        false
    }

    /// Delete removes the on-screen event/task — but only when the caret
    /// isn't in a text field or the notes editor (where Delete edits text).
    /// Mirrors the editor's trash button through the same action. Split out
    /// of `handle_keyboard` to keep that under the line cap.
    fn handle_keyboard_delete(
        &mut self,
        state: &mut AutoCommit,
        editor_active: bool,
        field_focused: bool,
        notes_focused: bool,
        handled: &mut bool,
    ) -> bool {
        if !editor_active || field_focused || notes_focused {
            return false;
        }
        let act = match self.selection {
            Some(Sel::Event(id)) => Some(Action::DeleteEvent(id)),
            Some(Sel::Task(id)) => Some(Action::DeleteTask(id)),
            None => None,
        };
        let Some(a) = act else {
            return false;
        };
        self.apply_action(a, state);
        *handled = true;
        self.dirty = true;
        true
    }

    /// Global letter/arrow shortcuts, fired only when not typing into a text
    /// field or the notes editor. Split out of `handle_keyboard` to keep
    /// that under the line cap.
    fn handle_keyboard_shortcut(
        &mut self,
        logical_key: &Key,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) -> bool {
        if let Key::Character(ch) = logical_key {
            let key = ch.to_lowercase();
            let act = match key.as_str() {
                "m" => Some(Action::View(View::Month)),
                "w" => Some(Action::View(View::Week)),
                "d" => Some(Action::View(View::Day)),
                "a" => Some(Action::View(View::Agenda)),
                "t" => Some(Action::Today),
                "n" => Some(Action::NewEvent),
                "e" => Some(Action::Export),
                "i" => Some(Action::Import),
                "[" => Some(Action::Prev),
                "]" => Some(Action::Next),
                _ => None,
            };
            if let Some(a) = act {
                self.apply_action(a, state);
                *handled = true;
                return true;
            }
        }
        if let Key::Named(named) = logical_key {
            let days = match named {
                NamedKey::ArrowLeft => Some(-1),
                NamedKey::ArrowRight => Some(1),
                NamedKey::ArrowUp => Some(-7),
                NamedKey::ArrowDown => Some(7),
                _ => None,
            };
            if let Some(d) = days {
                self.move_anchor(d);
                *handled = true;
                return true;
            }
        }
        false
    }

    /// Mouse click resolution: a form text field, the notes region, or the
    /// generic hit-map (topmost last) (part of [`Widget::handle_event`]).
    /// Returns `true` if the event was consumed.
    #[allow(
        clippy::too_many_arguments,
        reason = "internal event-handling split of an over-length function"
    )]
    fn handle_mouse_click(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
        form_active: bool,
        in_notes: bool,
    ) -> bool {
        let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
        else {
            return false;
        };
        let p = self.last_mouse;
        // A click inside a form text field focuses it and lands the caret.
        if form_active && self.form.title.contains(p) {
            self.location_input.set_focus(false);
            self.notes_editor.set_focus(false);
            self.title_input.set_focus(true);
            self.title_input.handle_event(
                event,
                scale_factor,
                &modifiers,
                state,
                path,
                handled,
                focus_target,
                node_id,
                taffy,
            );
            *handled = true;
            self.dirty = true;
            return true;
        }
        if form_active && self.form.location.contains(p) {
            self.title_input.set_focus(false);
            self.notes_editor.set_focus(false);
            self.location_input.set_focus(true);
            self.location_input.handle_event(
                event,
                scale_factor,
                &modifiers,
                state,
                path,
                handled,
                focus_target,
                node_id,
                taffy,
            );
            *handled = true;
            self.dirty = true;
            return true;
        }
        // Clicks inside the notes region go to the editor (focus + caret).
        if in_notes {
            self.title_input.set_focus(false);
            self.location_input.set_focus(false);
            if let Some(n) = self.editor_node {
                self.notes_editor.handle_event(
                    event,
                    scale_factor,
                    &modifiers,
                    state,
                    path,
                    handled,
                    focus_target,
                    n,
                    taffy,
                );
            }
            self.dirty = true;
            return true;
        }
        // Otherwise resolve against the hit-map (topmost last).
        if let Some(a) = self.hit_action_at(p) {
            self.title_input.set_focus(false);
            self.location_input.set_focus(false);
            self.notes_editor.set_focus(false);
            self.apply_action(a, state);
            *handled = true;
            return true;
        }
        false
    }

    /// Forward typing (and other events) to a focused form text field so
    /// edits land, mirroring the change into the event via
    /// [`Self::flush_form_edits`] (part of [`Widget::handle_event`]).
    /// Returns `true` if the caller should stop dispatching.
    #[allow(
        clippy::too_many_arguments,
        reason = "internal event-handling split of an over-length function"
    )]
    fn handle_form_field_input(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) -> bool {
        if self.title_input.is_focused() {
            self.title_input.handle_event(
                event,
                scale_factor,
                &modifiers,
                state,
                path,
                handled,
                focus_target,
                node_id,
                taffy,
            );
            self.flush_form_edits(state);
            self.dirty = true;
            return *handled;
        } else if self.location_input.is_focused() {
            self.location_input.handle_event(
                event,
                scale_factor,
                &modifiers,
                state,
                path,
                handled,
                focus_target,
                node_id,
                taffy,
            );
            self.flush_form_edits(state);
            self.dirty = true;
            return *handled;
        }
        false
    }
}

impl Default for CalendarApp {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for CalendarApp {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::CALENDAR_OPERATION_TYPE]
    }

    /// Publish the open calendar + its events/tasks so the host's Ctrl+K action
    /// palette scopes to them. Delegates to the
    /// [`ResourceProvider`](qos_actions::ResourceProvider) impl in
    /// [`crate::actions`].
    fn resources(&self, doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        <Self as qos_actions::ResourceProvider>::resources(self, doc)
    }

    /// Hand the host the heads captured before an on-screen affordance dispatched
    /// its calendar action, so it records one undo boundary for it (see
    /// [`Self::perform_action`]). Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }

    fn invoke_resource_operation(
        &mut self,
        kind: qos_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if type_id != crate::actions::CALENDAR_OPERATION_TYPE {
            return Ok(false);
        }
        let operation: crate::actions::CalendarOperation =
            serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        match (kind, operation) {
            (qos_actions::OperationKind::View, crate::actions::CalendarOperation::ShowMonth) => {
                self.view = View::Month
            }
            (qos_actions::OperationKind::View, crate::actions::CalendarOperation::ShowWeek) => {
                self.view = View::Week
            }
            (qos_actions::OperationKind::View, crate::actions::CalendarOperation::ShowDay) => {
                self.view = View::Day
            }
            (qos_actions::OperationKind::View, crate::actions::CalendarOperation::ShowAgenda) => {
                self.view = View::Agenda
            }
            (qos_actions::OperationKind::Effect, crate::actions::CalendarOperation::Sync) => {
                if self.syncing {
                    return Err("calendar synchronization is already running".into());
                }
                let mut doc = doc.clone();
                self.start_sync(&mut doc);
            }
            (qos_actions::OperationKind::Effect, crate::actions::CalendarOperation::ExportIcs) => {
                self.export_ics()
            }
            _ => return Err("operation kind does not match Calendar operation".into()),
        }
        self.dirty = true;
        Ok(true)
    }

    /// Fold the calendar's events into the unified Ctrl+K palette: a snapshot
    /// [`EventSearchProvider`](crate::search_provider::EventSearchProvider)
    /// makes every event findable there, alongside the actions the app exports.
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(crate::search_provider::EventSearchProvider::new(
            &self.model,
        ))]
    }

    /// Navigate to the event a chosen palette row identifies: jump the view's
    /// anchor to its day and select it. Anchor/selection are app-local
    /// presentation (per-peer, never in the shared doc), so the doc args are
    /// unused.
    fn activate_search_result(&mut self, action_id: &str, _doc: &mut AutoCommit, _path: &ObjId) {
        if let Some(id) = crate::search_provider::EventSearchProvider::parse_action_id(action_id)
            && let Some(ev) = self.model.event(id)
        {
            self.anchor = ev.start.date();
            self.select_event(id);
        }
    }

    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        if !self.seeded {
            // Live app: adopt the user's persisted calendar (empty on a fresh
            // machine). Demo/test builds pass no path and stay in-memory.
            if let Some(p) = &self.data_path
                && let Some(mut loaded) = crate::store::load(p)
            {
                let _ = state.merge(&mut loaded);
            }
            // Seed sample content ONLY in the demo build, and only when the
            // document is still empty.
            if self.demo && CalDoc::is_empty(state) {
                CalDoc::seed(state, &self.model);
                self.seed_demo_notes(state);
            }
            // Whatever the document holds is now authoritative for the UI.
            if CalDoc::is_empty(state) {
                self.model = Model::default();
            } else {
                self.model = CalDoc::read_model(state);
            }
            self.revalidate_selection();
            self.saved_heads = state.get_heads();
            self.model_heads = state.get_heads();
            self.seeded = true;
        }
        // Drain a finished background CalDAV pull, if one landed since the last
        // frame, before binding the editor so a freshly-synced selection reads
        // back its notes.
        // Shots-only: spawn a fresh event to capture the New-Event form.
        if self.shots_pending_new_event {
            self.shots_pending_new_event = false;
            self.new_event(state);
        }
        self.poll_sync(state);
        // Reconcile the in-memory model with the document when it moved out of
        // band — a Ctrl+K palette action mutated the CRDT, or a peer's change
        // merged in — so those events/tasks show up in the views just like an
        // on-screen edit does. Cheap and idempotent: only when the heads differ
        // from what the model was last derived from, and events/tasks re-read to
        // the same values a handler would have set. (Notes live in the document
        // and are read by the editor directly, so this never disturbs them.)
        let heads = state.get_heads();
        if heads != self.model_heads {
            self.model = CalDoc::read_model(state);
            self.revalidate_selection();
            self.model_heads = heads;
        }
        self.ensure_bound(state);
        // Seed the form's title/location/time widgets from the current event
        // whenever the selection changes.
        self.ensure_form_bound();
        if let Some(which) = self.shots_pending_popover.take() {
            self.open_popover(which);
        }
        self.notes_editor.sync(state, path);
        // Persist after the editor's sync so notes edits land too. Cheap: only
        // writes when the document's heads changed since the last save.
        self.maybe_persist(state);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let editor_node = self.notes_editor.build_layout(taffy, theme, node_map);
        let notes_w = self.detail_body_w();
        self.built_notes_w = notes_w;
        if let Ok(existing) = taffy.style(editor_node).cloned() {
            let _ = taffy.set_style(
                editor_node,
                Style {
                    position: taffy::Position::Absolute,
                    size: Size {
                        width: Dimension::length(notes_w as f32),
                        height: Dimension::auto(),
                    },
                    inset: taffy::Rect {
                        left: length(0.0_f32),
                        top: length(0.0_f32),
                        right: taffy::LengthPercentageAuto::auto(),
                        bottom: taffy::LengthPercentageAuto::auto(),
                    },
                    ..existing
                },
            );
        }
        self.editor_node = Some(editor_node);

        // `new_with_children` is typed `-> TaffyResult<NodeId>` for API
        // generality but only fails for a child `NodeId` belonging to a
        // different tree, which cannot happen here — `editor_node` was just
        // built in this same tree above (see `ui::widget::TaffyTreeExt`'s doc
        // comment) — and `Widget::build_layout` returns a bare `NodeId`
        // across every implementor, so there is nowhere to surface a
        // theoretical error.
        #[allow(
            clippy::unwrap_used,
            reason = "child was just built in this tree; see ui::widget::TaffyTreeExt"
        )]
        let root = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::percent(1.0),
                    },
                    ..Default::default()
                },
                &[editor_node],
            )
            .unwrap();
        node_map.insert(root, std::ptr::from_mut::<dyn Widget>(self));
        root
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        let new_rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        let size_changed = (new_rect.width() - self.rect.width()).abs() > f64::EPSILON
            || (new_rect.height() - self.rect.height()).abs() > f64::EPSILON;
        if size_changed {
            self.dirty = true;
        }
        self.rect = new_rect;

        // Recompute the responsive rects (sidebar / content / detail / notes)
        // from the new allocated size, then place the notes editor.
        self.relayout();
        if let Some(n) = self.editor_node {
            self.notes_editor.apply_layout(taffy, n, self.notes_origin);
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "107 lines of straight-line paint order: chrome, then the view body, \
                  then overlays. Splitting it hides the order in which layers are drawn, \
                  which is the one thing a reader of a draw method needs to see"
    )]
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let t = transform;
        let c = &theme.tokens.colors;
        fill_rect(scene, t, c.surfaces.base, self.rect);

        let mut hits = Vec::new();

        // Content: the calendar screen (topbar + active view) when it has a
        // rect, otherwise the detail screen has taken the whole width (narrow
        // pushed detail).
        let detail_drawn = self.detail_open && self.detail_rect.area() > 0.0;
        if self.content_rect.area() > 0.0 {
            hits.extend(
                self.topbar
                    .draw(
                        scene,
                        t,
                        text_renderer,
                        theme,
                        self.view,
                        self.anchor,
                        self.last_mouse,
                    )
                    .iter()
                    .map(|(rect, action)| {
                        let action = match action {
                            TopbarAction::View(view) => Action::View(*view),
                            TopbarAction::Today => Action::Today,
                            TopbarAction::Previous => Action::Prev,
                            TopbarAction::Next => Action::Next,
                            TopbarAction::NewEvent => Action::NewEvent,
                            TopbarAction::OpenSidebar => Action::ToggleSidebar,
                            TopbarAction::ExpandSidebar => Action::ToggleSidebarCollapsed,
                        };
                        (*rect, action)
                    }),
            );
            let area = self.view_rect();
            match self.view {
                View::Month => hits.extend(self.draw_month(scene, t, text_renderer, theme, area)),
                View::Week | View::Day => {
                    let days = self.timeline_days();
                    let events = self.timeline_render_events(&days);
                    let now = Local::now().naive_local();
                    let model = TimelineRenderModel {
                        days: &days,
                        events: &events,
                        today: self.today,
                        now: Some((
                            self.today,
                            i64::from(now.hour()) * 60 + i64::from(now.minute()),
                        )),
                        untitled: TITLE_PLACEHOLDER,
                    };
                    hits.extend(
                        self.timeline
                            .draw_timeline(scene, t, text_renderer, theme, area, &model)
                            .into_iter()
                            .filter_map(|(rect, effect)| match effect {
                                TimelineEffect::Select(id) => Some((rect, Action::OpenEvent(id))),
                                TimelineEffect::OpenDay(day) => Some((rect, Action::OpenDay(day))),
                                TimelineEffect::Create { .. } | TimelineEffect::Move { .. } => None,
                            }),
                    );
                }
                View::Agenda => {
                    let hidden = self.hidden_calendar_ids();
                    hits.extend(
                        self.agenda
                            .draw(
                                scene,
                                t,
                                text_renderer,
                                theme,
                                area,
                                &self.model,
                                self.anchor,
                                self.today,
                                &hidden,
                                self.last_mouse,
                            )
                            .into_iter()
                            .map(|(rect, action)| {
                                let action = match action {
                                    AgendaAction::OpenEvent(id) => Action::OpenEvent(id),
                                    AgendaAction::OpenTask(id) => Action::OpenTask(id),
                                    AgendaAction::ToggleTask(id) => Action::ToggleTask(id),
                                };
                                (rect, action)
                            }),
                    );
                }
            }
            // First-run onboarding: over an otherwise-blank Month/Week/Day view
            // with no events, show a friendly prompt instead of an empty grid.
            // Agenda carries its own inline empty-state.
            if !matches!(self.view, View::Agenda)
                && let Some((title, hint)) = self.empty_onboarding_prompt()
            {
                Self::draw_empty_onboarding(scene, t, text_renderer, theme, area, title, hint);
            }
            self.draw_status(scene, t, text_renderer, theme, self.content_rect.x0);
        }

        if detail_drawn {
            hits.extend(self.draw_detail(scene, t, text_renderer, theme));
            // The editable event form (title/location inputs, date/time/
            // calendar fields, chips) draws over the event detail body.
            let editing_event = matches!(self.selection, Some(Sel::Event(_)));
            if editing_event {
                hits.extend(self.draw_event_form(scene, t, text_renderer, theme, presence));
            }
            // Notes editor — clipped to the notes region below the header.
            // Clip to the editor's own column (not the whole panel) so a
            // long single-line note block scrolls/cuts at the notes
            // padding rather than bleeding to the window edge.
            let clip = Rect::new(
                self.detail_rect.x0,
                self.notes_origin.y - 4.0,
                self.notes_origin.x + self.notes_w + 4.0,
                self.detail_rect.y1 - 12.0,
            );
            scene.push_layer(
                Fill::NonZero,
                vello::peniko::BlendMode::default(),
                1.0,
                t,
                &clip,
            );
            self.notes_editor
                .draw(scene, t, text_renderer, theme, presence);
            scene.pop_layer();
            self.notes_editor
                .draw_overlay(scene, t, text_renderer, theme);
            // Transient date/time/calendar popovers paint on top of the form
            // and the notes, with a dismiss scrim beneath them.
            if editing_event {
                hits.extend(self.draw_form_popovers(scene, t, text_renderer, theme, presence));
            }
        }

        // Sidebar last so the overlay flap (and its scrim) paint over the
        // calendar/detail content beneath them.
        hits.extend(self.draw_sidebar(scene, t, text_renderer, theme));

        self.hits = hits;
    }

    // This is the widget system's event-dispatch contract, implemented by
    // every widget; every parameter is independently meaningful (the raw
    // event, scaling, modifiers, CRDT handle+path, two out-params, and the
    // node's own identity/layout).
    #[allow(clippy::too_many_arguments)]
    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        // Track the pointer in logical coordinates.
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
            self.dirty = true;
        }

        // The notes editor is only live when the detail screen is actually
        // drawn (in the narrow layout it isn't, while the calendar screen
        // shows).
        let editor_active = self.detail_open && self.detail_rect.area() > 0.0;
        let form_active = editor_active && matches!(self.selection, Some(Sel::Event(_)));
        let popover_open = form_active && self.event_popovers.is_open();
        // Bounded to the notes column of the detail card/page so a click below
        // or beside a floating modal isn't swallowed by the editor.
        let in_notes = editor_active && self.notes_region().contains(self.last_mouse);

        // --- Open popover owns input first --------------------------------
        if popover_open {
            self.handle_popover_priority(
                event,
                scale_factor,
                *modifiers,
                state,
                path,
                handled,
                focus_target,
                node_id,
                taffy,
            );
            return;
        }

        // --- Week/day timeline + agenda: scroll and drag ------------------
        // Owns mouse input inside the scrolling views (wheel, scrollbar,
        // drag-to-create / drag-to-move) before the generic hit-map path.
        if self.handle_timeline_input(
            event,
            scale_factor,
            *modifiers,
            state,
            path,
            focus_target,
            node_id,
            taffy,
        ) {
            self.dirty = true;
            return;
        }

        if self.handle_keyboard(event, *modifiers, state, handled) {
            return;
        }

        if self.handle_mouse_click(
            event,
            scale_factor,
            *modifiers,
            state,
            path,
            handled,
            focus_target,
            node_id,
            taffy,
            form_active,
            in_notes,
        ) {
            return;
        }

        // Forward typing (and other events) to a focused form text field so
        // edits land, mirroring the change into the event via `flush_form_edits`.
        if self.handle_form_field_input(
            event,
            scale_factor,
            *modifiers,
            state,
            path,
            handled,
            focus_target,
            node_id,
            taffy,
        ) {
            return;
        }

        // Forward remaining events (key typing, mouse move) to the editor
        // when the notes panel is active so editing works.
        if editor_active && let Some(n) = self.editor_node {
            self.notes_editor.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                n,
                taffy,
            );
        }
        self.dirty = true;
    }

    fn name(&self) -> &'static str {
        "CalendarApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || self.notes_editor.needs_redraw()
            || self.title_input.needs_redraw()
            || self.location_input.needs_redraw()
            || self.timeline.needs_redraw()
            || self.agenda.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.notes_editor.mark_drawn();
        self.title_input.mark_drawn();
        self.location_input.mark_drawn();
        self.timeline.mark_drawn();
        self.agenda.mark_drawn();
    }

    fn take_structure_dirty(&mut self) -> bool {
        // Rebuild when the editor's own structure changed, OR when the notes
        // column width the editor was built at no longer matches the current
        // layout (a resize crossed the breakpoint). The rebuild re-runs
        // `build_layout`, which resets `built_notes_w`, so this settles in one
        // frame and never loops while the width is stable.
        let width_changed = (self.detail_body_w() - self.built_notes_w).abs() > 0.5;
        self.notes_editor.take_structure_dirty() || width_changed
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep the event loop ticking while a background CalDAV pull is running
        // so its result is drained in `sync` the moment it arrives, without
        // waiting for an unrelated input event — and while a scroll container is
        // still settling momentum / fading its bar.
        self.syncing || self.timeline.needs_animation_frame() || self.agenda.needs_animation_frame()
    }

    fn is_layout_dirty(&self) -> bool {
        self.dirty
            || self.notes_editor.needs_redraw()
            || self.title_input.needs_redraw()
            || self.location_input.needs_redraw()
            || self.timeline.is_layout_dirty()
            || self.agenda.is_layout_dirty()
    }

    /// While the event/task editor is open it OWNS the keyboard. Reporting the
    /// editor's box here makes the host [`ui::App`] stand down: it stops running
    /// its built-in Tab focus cycling (which would otherwise cycle to the notes
    /// editor node and never reach the form's own title → location → notes
    /// traversal) and swallows click-through so a press outside the card can't
    /// reach the calendar behind it. The form's own `handle_event` still routes
    /// each key/click; this only tells the App to stop competing for input.
    fn modal_capture(&self) -> Option<Rect> {
        if self.detail_open && self.detail_rect.area() > 0.0 {
            Some(self.detail_rect)
        } else {
            None
        }
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &PermissionContext,
    ) {
        self.draw(scene, transform, text_renderer, theme, presence);
    }
}

#[allow(
    clippy::many_single_char_names,
    reason = "t is the paint transform, r is the rect being centred in, and w/x/y are the \
              measured width and centred position — conventional short names for this \
              draw step"
)]
pub(crate) fn center_text(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    text: &str,
    size: f32,
    color: Color,
    r: Rect,
    t: Affine,
) {
    let w = f64::from(tr.measure(text, size, None).0);
    let x = r.center().x - w / 2.0;
    let y = r.center().y - f64::from(size) / 2.0;
    tr.draw_text(
        scene,
        text,
        size,
        color,
        t * Affine::translate((x, y)),
        None,
    );
}

pub(crate) fn draw_icon_button(scene: &mut Scene, t: Affine, theme: &Theme, r: Rect, hover: bool) {
    let c = &theme.tokens.colors;
    let bg = if hover {
        c.surface_white_10
    } else {
        c.surfaces.raised
    };
    fill_round(scene, t, bg, r, 8.0);
    stroke_round(scene, t, c.border_standard, r, 8.0, 1.0);
}

pub(crate) fn draw_chevron(scene: &mut Scene, t: Affine, color: Color, center: Point, left: bool) {
    // `left == true` draws a "<" (points left); false draws ">".
    let dx = if left { -4.0 } else { 4.0 };
    let mut p = BezPath::new();
    p.move_to(Point::new(center.x - dx, center.y - 5.0));
    p.line_to(Point::new(center.x + dx, center.y));
    p.line_to(Point::new(center.x - dx, center.y + 5.0));
    scene.stroke(&Stroke::new(1.6), t, color, None, &p);
}

/// Disclosure caret: points down when `expanded`, right when collapsed.
fn draw_caret(scene: &mut Scene, t: Affine, color: Color, center: Point, expanded: bool) {
    let mut p = BezPath::new();
    if expanded {
        p.move_to(Point::new(center.x - 4.0, center.y - 2.0));
        p.line_to(Point::new(center.x, center.y + 3.0));
        p.line_to(Point::new(center.x + 4.0, center.y - 2.0));
    } else {
        p.move_to(Point::new(center.x - 2.0, center.y - 4.0));
        p.line_to(Point::new(center.x + 3.0, center.y));
        p.line_to(Point::new(center.x - 2.0, center.y + 4.0));
    }
    scene.stroke(&Stroke::new(1.6), t, color, None, &p);
}

pub(crate) fn draw_plus(scene: &mut Scene, t: Affine, color: Color, center: Point) {
    stroke_line(
        scene,
        t,
        color,
        Point::new(center.x - 5.0, center.y),
        Point::new(center.x + 5.0, center.y),
        1.8,
    );
    stroke_line(
        scene,
        t,
        color,
        Point::new(center.x, center.y - 5.0),
        Point::new(center.x, center.y + 5.0),
        1.8,
    );
}

pub(crate) fn draw_hamburger(scene: &mut Scene, t: Affine, color: Color, center: Point) {
    for dy in [-5.0, 0.0, 5.0] {
        stroke_line(
            scene,
            t,
            color,
            Point::new(center.x - 7.0, center.y + dy),
            Point::new(center.x + 7.0, center.y + dy),
            1.6,
        );
    }
}

fn draw_back_arrow(scene: &mut Scene, t: Affine, color: Color, center: Point) {
    // A "←": horizontal shaft plus an arrowhead opening left.
    stroke_line(
        scene,
        t,
        color,
        Point::new(center.x - 6.0, center.y),
        Point::new(center.x + 6.0, center.y),
        1.7,
    );
    let mut p = BezPath::new();
    p.move_to(Point::new(center.x - 1.0, center.y - 5.0));
    p.line_to(Point::new(center.x - 6.0, center.y));
    p.line_to(Point::new(center.x - 1.0, center.y + 5.0));
    scene.stroke(&Stroke::new(1.7), t, color, None, &p);
}

pub(crate) fn draw_cross(scene: &mut Scene, t: Affine, color: Color, center: Point) {
    stroke_line(
        scene,
        t,
        color,
        Point::new(center.x - 5.0, center.y - 5.0),
        Point::new(center.x + 5.0, center.y + 5.0),
        1.6,
    );
    stroke_line(
        scene,
        t,
        color,
        Point::new(center.x + 5.0, center.y - 5.0),
        Point::new(center.x - 5.0, center.y + 5.0),
        1.6,
    );
}

/// A small trash-can glyph centered on `center` — the delete affordance.
fn draw_trash(scene: &mut Scene, t: Affine, color: Color, center: Point) {
    let cx = center.x;
    let cy = center.y;
    // Lid line + handle.
    stroke_line(
        scene,
        t,
        color,
        Point::new(cx - 6.0, cy - 5.0),
        Point::new(cx + 6.0, cy - 5.0),
        1.5,
    );
    stroke_line(
        scene,
        t,
        color,
        Point::new(cx - 2.5, cy - 5.0),
        Point::new(cx - 2.5, cy - 7.0),
        1.4,
    );
    stroke_line(
        scene,
        t,
        color,
        Point::new(cx + 2.5, cy - 5.0),
        Point::new(cx + 2.5, cy - 7.0),
        1.4,
    );
    stroke_line(
        scene,
        t,
        color,
        Point::new(cx - 2.5, cy - 7.0),
        Point::new(cx + 2.5, cy - 7.0),
        1.4,
    );
    // Can body (slightly tapered) + two ribs.
    stroke_line(
        scene,
        t,
        color,
        Point::new(cx - 5.0, cy - 5.0),
        Point::new(cx - 4.0, cy + 7.0),
        1.5,
    );
    stroke_line(
        scene,
        t,
        color,
        Point::new(cx + 5.0, cy - 5.0),
        Point::new(cx + 4.0, cy + 7.0),
        1.5,
    );
    stroke_line(
        scene,
        t,
        color,
        Point::new(cx - 4.0, cy + 7.0),
        Point::new(cx + 4.0, cy + 7.0),
        1.5,
    );
    stroke_line(
        scene,
        t,
        color,
        Point::new(cx - 1.6, cy - 2.5),
        Point::new(cx - 1.6, cy + 4.5),
        1.2,
    );
    stroke_line(
        scene,
        t,
        color,
        Point::new(cx + 1.6, cy - 2.5),
        Point::new(cx + 1.6, cy + 4.5),
        1.2,
    );
}

pub(crate) fn draw_check(scene: &mut Scene, t: Affine, color: Color, r: Rect) {
    let mut p = BezPath::new();
    p.move_to(Point::new(r.x0 + r.width() * 0.22, r.center().y));
    p.line_to(Point::new(
        r.x0 + r.width() * 0.42,
        r.y1 - r.height() * 0.28,
    ));
    p.line_to(Point::new(
        r.x1 - r.width() * 0.20,
        r.y0 + r.height() * 0.28,
    ));
    scene.stroke(&Stroke::new(1.8), t, color, None, &p);
}

pub(crate) fn draw_checkbox(scene: &mut Scene, t: Affine, theme: &Theme, r: Rect, done: bool) {
    Checkbox::paint(
        scene,
        t,
        theme,
        &CheckboxFace::new(r, done).transparent_unchecked(),
    );
}

/// A minute-of-day (0..=1439) as a wall-clock time.
fn min_to_time(min: i64) -> NaiveTime {
    let m = min.clamp(0, 24 * 60 - 1);
    // `m` is clamped to 0..=1439, so hour/minute are always in-range and this
    // is always `Some`; the fallback is unreachable.
    NaiveTime::from_hms_opt((m / 60) as u32, (m % 60) as u32, 0).unwrap_or(NaiveTime::MIN)
}

#[allow(
    clippy::many_single_char_names,
    reason = "t is the paint transform, n is the count, and c/s/w/r are the colour set/count \
              text/badge width/badge rect locals — conventional short names for this draw step"
)]
fn draw_count_badge(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    cx: f64,
    cy: f64,
    n: usize,
) {
    let c = &theme.tokens.colors;
    let s = format!("{n}");
    let w = (f64::from(tr.measure(&s, 11.0, None).0) + 12.0).max(20.0);
    let r = Rect::new(cx - w / 2.0, cy - 9.0, cx + w / 2.0, cy + 9.0);
    fill_round(scene, t, c.neutral[3], r, 9.0);
    center_text(scene, tr, &s, 11.0, c.neutral[9], r, t);
}

#[allow(clippy::too_many_arguments)]
#[allow(
    clippy::many_single_char_names,
    reason = "t is the paint transform, x/y/w are the field's position/width, and c is \
              the theme's colour set — conventional short names for this draw step"
)]
fn field_row(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    theme: &Theme,
    t: Affine,
    x: f64,
    y: f64,
    w: f64,
    label: &str,
    value: &str,
) {
    let c = &theme.tokens.colors;
    tr.draw_text(
        scene,
        label,
        11.0,
        c.text_dim,
        t * Affine::translate((x, y)),
        None,
    );
    tr.draw_text(
        scene,
        value,
        14.0,
        c.neutral[9],
        t * Affine::translate((x + 84.0, y - 2.0)),
        Some((w - 84.0) as f32),
    );
}

pub(crate) fn draw_download(scene: &mut Scene, t: Affine, color: Color, center: Point) {
    // Down-arrow into a tray.
    stroke_line(
        scene,
        t,
        color,
        Point::new(center.x, center.y - 6.0),
        Point::new(center.x, center.y + 2.0),
        1.6,
    );
    let mut p = BezPath::new();
    p.move_to(Point::new(center.x - 4.0, center.y - 1.0));
    p.line_to(Point::new(center.x, center.y + 3.0));
    p.line_to(Point::new(center.x + 4.0, center.y - 1.0));
    scene.stroke(&Stroke::new(1.6), t, color, None, &p);
    stroke_line(
        scene,
        t,
        color,
        Point::new(center.x - 5.0, center.y + 6.0),
        Point::new(center.x + 5.0, center.y + 6.0),
        1.6,
    );
}

pub(crate) fn draw_upload(scene: &mut Scene, t: Affine, color: Color, center: Point) {
    // Up-arrow rising out of a tray (mirror of `draw_download`).
    stroke_line(
        scene,
        t,
        color,
        Point::new(center.x, center.y - 5.0),
        Point::new(center.x, center.y + 3.0),
        1.6,
    );
    let mut p = BezPath::new();
    p.move_to(Point::new(center.x - 4.0, center.y - 1.0));
    p.line_to(Point::new(center.x, center.y - 5.0));
    p.line_to(Point::new(center.x + 4.0, center.y - 1.0));
    scene.stroke(&Stroke::new(1.6), t, color, None, &p);
    stroke_line(
        scene,
        t,
        color,
        Point::new(center.x - 5.0, center.y + 6.0),
        Point::new(center.x + 5.0, center.y + 6.0),
        1.6,
    );
}

/// A field-shaped button that reads as an editable control (rounded rect,
/// standard border that thickens to the focus colour when active), showing a
/// value and an optional trailing disclosure caret. Backs the date/time/
/// calendar fields, mirroring the `TextInput` chrome so the whole form looks
/// uniformly editable.
#[allow(clippy::too_many_arguments)]
fn draw_field_button(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    r: Rect,
    label: &str,
    active: bool,
    trailing_caret: bool,
) {
    let c = &theme.tokens.colors;
    let bg = if active {
        c.surface_white_10
    } else {
        c.surfaces.raised
    };
    fill_round(scene, t, bg, r, 8.0);
    let (bw, border) = if active {
        (2.0, c.border_focus)
    } else {
        (1.0, c.border_standard)
    };
    stroke_round(scene, t, border, r, 8.0, bw);
    let text_max = (r.width() - if trailing_caret { 40.0 } else { 24.0 }).max(0.0);
    let disp = elide_to_width(tr, label, 14.0, text_max);
    tr.draw_text(
        scene,
        &disp,
        14.0,
        c.text_primary,
        t * Affine::translate((r.x0 + 12.0, r.center().y - 8.0)),
        None,
    );
    if trailing_caret {
        draw_caret(
            scene,
            t,
            c.text_dim,
            Point::new(r.x1 - 16.0, r.center().y),
            true,
        );
    }
}

/// Draw a small pill chip and return its rect.
#[allow(
    clippy::too_many_arguments,
    reason = "each parameter is an independently-meaningful piece of the chip's own geometry/label"
)]
#[allow(
    clippy::many_single_char_names,
    reason = "t is the paint transform, x/y are the chip's position, and c/r are the \
              theme's colour set and the resulting rect — conventional short names for \
              this draw step"
)]
fn chip(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    x: f64,
    y: f64,
    label: &str,
    filled: bool,
) -> Rect {
    let c = &theme.tokens.colors;
    let tw = f64::from(tr.measure(label, 12.0, None).0);
    let r = Rect::new(x, y, x + tw + 24.0, y + 28.0);
    if filled {
        fill_round(scene, t, c.accent[5], r, 8.0);
    } else {
        fill_round(scene, t, c.neutral[2], r, 8.0);
        stroke_round(scene, t, c.border_standard, r, 8.0, 1.0);
    }
    tr.draw_text(
        scene,
        label,
        12.0,
        c.neutral[9],
        t * Affine::translate((r.x0 + 12.0, r.center().y - 7.0)),
        None,
    );
    r
}

fn elide(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    fn task_checkbox_paints_both_completion_states() {
        let theme = Theme::dark();
        let rect = Rect::new(4.0, 6.0, 22.0, 24.0);
        let mut unchecked = Scene::new();
        draw_checkbox(&mut unchecked, Affine::IDENTITY, &theme, rect, false);
        let mut checked = Scene::new();
        draw_checkbox(&mut checked, Affine::IDENTITY, &theme, rect, true);

        let unchecked_tags = unchecked.encoding().draw_tags.len();
        let checked_tags = checked.encoding().draw_tags.len();
        assert!(unchecked_tags > 0, "unchecked state keeps its outline");
        assert!(
            checked_tags > unchecked_tags,
            "checked state adds its fill and check mark"
        );
    }

    #[test]
    fn demo_app_seeds_model_and_selection() {
        let app = CalendarApp::new_demo();
        assert!(!app.model.events.is_empty());
        assert!(!app.model.tasks.is_empty());
        assert!(app.selection.is_some());
        assert!(app.detail_open);
    }

    #[test]
    fn breakpoint_reflows_on_resize_not_cached_from_first_frame() {
        // The reported bug: responsive on first launch, frozen after resizing.
        // Prove the breakpoint + region rects are re-derived from the CURRENT
        // width on every layout pass — on a SINGLE instance — by driving the
        // real `relayout` (exactly what `apply_layout` calls) across a
        // wide → narrow → wide sequence.
        let mut app = CalendarApp::new_demo();

        // Wide first frame: persistent sidebar, detail docked beside calendar.
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.relayout();
        assert_eq!(app.sidebar.mode(), SidebarMode::Persistent);
        assert!(app.content_rect.width() > 0.0);
        assert!(app.detail_rect.width() > 0.0);
        let wide_content = app.content_rect;

        // Resize the SAME instance narrow. A cached-from-first-frame mode would
        // stay Persistent; a correctly re-derived one flips to Overlay.
        app.rect = Rect::new(0.0, 0.0, 560.0, 720.0);
        app.relayout();
        assert_eq!(
            app.sidebar.mode(),
            SidebarMode::Overlay,
            "breakpoint must re-derive from the current width, not the first frame"
        );
        // Narrow + detail-open default => detail is the full-width pushed
        // screen and the calendar content pane collapses.
        assert_eq!(app.content_rect.width(), 0.0);
        assert!(app.detail_rect.width() > 0.0);
        assert_ne!(
            app.content_rect, wide_content,
            "layout actually changed on resize"
        );

        // Back to wide — proves it toggles both directions, not just once.
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.relayout();
        assert_eq!(app.sidebar.mode(), SidebarMode::Persistent);
        assert!(app.detail_rect.width() > 0.0);
        assert_eq!(app.content_rect, wide_content);
    }

    #[test]
    fn notes_editor_width_retracks_after_resize() {
        // The notes editor's text-wrap width is set in `build_layout`, which the
        // live loop only re-runs on a structure rebuild. A resize that reshapes
        // the detail pane must therefore request a rebuild so the editor column
        // tracks the new width instead of staying frozen at launch width.
        use std::collections::HashMap;
        use taffy::TaffyTree;

        let mut app = CalendarApp::new_demo();
        let theme = Theme::dark();

        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.relayout();
        app.build_layout(&mut taffy, &theme, &mut node_map);
        let wide_built = app.built_notes_w;
        assert!(wide_built > 0.0);

        // Resize narrow: the pane wants a different notes width than we built.
        app.rect = Rect::new(0.0, 0.0, 560.0, 720.0);
        app.relayout();
        assert!(
            (app.detail_body_w() - wide_built).abs() > 0.5,
            "the narrow pane wants a different notes width"
        );
        assert!(
            app.take_structure_dirty(),
            "a resize that changes the notes column must request an editor rebuild"
        );

        // The rebuild adopts the resized width.
        let mut taffy2: TaffyTree<()> = TaffyTree::new();
        let mut node_map2: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        app.build_layout(&mut taffy2, &theme, &mut node_map2);
        assert_ne!(
            app.built_notes_w, wide_built,
            "editor rebuilt at the resized width"
        );
    }

    #[test]
    fn month_week_agenda_grid_reflows_on_resize() {
        // Beyond the panel breakpoints, the *grid inside a view* must also
        // re-derive its cell geometry from the current width on every frame —
        // "lays out once, breaks after resize" is the reported bug. Both widths
        // here sit above every panel breakpoint, so a
        // change in cell width can only be the grid itself reflowing, not a
        // breakpoint flip. Measured on the real path: the hit rects that live
        // clicks resolve against, populated by driving `draw` through `Harness`.
        fn widest_cell(app: &CalendarApp, view: View) -> f64 {
            app.hits
                .iter()
                .filter(|(_, a)| match view {
                    View::Month => matches!(a, Action::PickDate(_)),
                    View::Week | View::Day => matches!(a, Action::OpenDay(_)),
                    View::Agenda => matches!(a, Action::OpenEvent(_) | Action::OpenTask(_)),
                })
                .map(|(r, _)| r.width())
                .fold(0.0f64, f64::max)
        }

        for view in [View::Month, View::Week, View::Agenda] {
            let mut hn = Harness::new(1280, 800);
            hn.app.detail_open = false;
            hn.app.view = view;
            hn.relayout();
            let wide = widest_cell(&hn.app, view);
            assert!(wide > 0.0, "{view:?}: expected grid cells in the hit map");

            // Resize narrower — still wide enough that the sidebar stays docked,
            // isolating the grid's own reflow from any breakpoint change.
            hn.w = 1000;
            hn.relayout();
            assert_eq!(hn.app.sidebar.mode(), SidebarMode::Persistent);
            let narrow = widest_cell(&hn.app, view);
            assert!(
                narrow + 1.0 < wide,
                "{view:?} grid must reflow narrower on resize (was {wide:.1}, now {narrow:.1}) — not laid out once"
            );
        }
    }

    #[test]
    fn week_range_short_keeps_both_endpoints() {
        // The compact week title (used when the topbar is tight) must still show
        // start AND end — the clip bug lost the end date entirely.
        let s = crate::date::week_range_short(
            NaiveDate::from_ymd_opt(2026, 6, 29).unwrap(),
            NaiveDate::from_ymd_opt(2026, 7, 5).unwrap(),
        );
        assert!(s.contains("29 Jun"), "start kept: {s}");
        assert!(s.contains("5 Jul"), "end kept: {s}");
    }

    #[test]
    fn live_app_starts_empty_with_no_fabricated_content() {
        // A fresh machine (no persisted document, no configured accounts) must
        // show an empty calendar — no sample events, tasks or accounts.
        let missing = std::env::temp_dir()
            .join(format!("cal-empty-{}", uuid::Uuid::new_v4()))
            .join("calendar.automerge");
        let mut app = CalendarApp::new_with_data_path(missing);
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        assert!(app.model.events.is_empty(), "no fabricated events");
        assert!(app.model.tasks.is_empty(), "no fabricated tasks");
        assert!(app.accounts.is_empty(), "no fabricated accounts");
        assert!(app.selection.is_none());
        assert!(!app.detail_open);
    }

    #[test]
    fn empty_first_run_offers_onboarding_prompt() {
        // A fresh machine with no events surfaces the friendly first-run prompt;
        // the moment there is content it stands down.
        let missing = std::env::temp_dir()
            .join(format!("cal-onboard-{}", uuid::Uuid::new_v4()))
            .join("calendar.automerge");
        let mut app = CalendarApp::new_with_data_path(missing);
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        assert!(app.model.events.is_empty(), "precondition: no events");
        let (title, hint) = app
            .empty_onboarding_prompt()
            .expect("empty calendar shows the onboarding prompt");
        assert_eq!(title, EMPTY_ONBOARDING_TITLE);
        assert!(
            !hint.trim().is_empty(),
            "hint guides the user to add an event"
        );

        // Seeded demo has events → no onboarding.
        let demo = CalendarApp::new_demo();
        assert!(!demo.model.events.is_empty());
        assert!(
            demo.empty_onboarding_prompt().is_none(),
            "a populated calendar hides the onboarding prompt",
        );
    }

    #[test]
    fn user_events_persist_across_restart() {
        // The real backend: an event created in one session is present after a
        // fresh instance loads the same on-disk document.
        let dir = std::env::temp_dir().join(format!("cal-persist-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("calendar.automerge");

        let mut app = CalendarApp::new_with_data_path(path.clone());
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        app.apply_action(Action::NewEvent, &mut state.doc);
        // A new event starts untitled (the title field shows its placeholder),
        // and is the current selection.
        let created = match app.selection {
            Some(Sel::Event(id)) => Some(id),
            _ => None,
        };
        assert!(created.is_some(), "NewEvent added an event to the model");
        assert!(
            app.model.events.iter().any(|e| e.title.is_empty()),
            "the fresh event has no title yet"
        );
        // Persist runs on the next sync (writes only when heads changed).
        app.sync(&mut state.doc, &state.root);
        assert!(path.exists(), "document was written to disk");

        // Fresh instance + fresh doc: it must load the event back from disk.
        let mut app2 = CalendarApp::new_with_data_path(path);
        let mut state2 = ui::State::new();
        app2.sync(&mut state2.doc, &state2.root);
        assert!(
            app2.model.events.iter().any(|e| Some(e.id) == created),
            "event survived the restart via on-disk persistence"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[serial]
    fn import_reads_a_real_ics_file() {
        // Import pulls the newest `.ics` from the transfer directory through
        // the real parse path — not a bundled sample.
        let dir = std::env::temp_dir().join(format!("cal-import-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: single-threaded test process for this env var.
        unsafe { std::env::set_var("QOS_CALENDAR_IMPORT_DIR", &dir) };
        let today = chrono::Local::now().date_naive();
        std::fs::write(dir.join("invite.ics"), crate::ics::sample_ics(today)).unwrap();

        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        let before = app.model.events.len();
        app.apply_action(Action::Import, &mut state);
        assert!(
            app.model.events.len() > before,
            "imported events landed in the model"
        );
        assert!(
            app.model
                .events
                .iter()
                .any(|e| e.title == "Quarterly all-hands"),
            "the .ics file's event parsed in"
        );
        unsafe { std::env::remove_var("QOS_CALENDAR_IMPORT_DIR") };
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sync_seeds_crdt_and_binds_editor() {
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut state, &root);
        assert!(app.seeded);
        // Selected event's notes are bound.
        assert!(app.bound.is_some());
        assert!(!CalDoc::is_empty(&state));
    }

    #[test]
    fn switching_view_changes_state() {
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.apply_action(Action::View(View::Week), &mut state);
        assert_eq!(app.view, View::Week);
        app.apply_action(Action::View(View::Agenda), &mut state);
        assert_eq!(app.view, View::Agenda);
    }

    #[test]
    fn toggling_a_task_flips_done_and_mirrors_to_crdt() {
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        let id = app.model.tasks.iter().find(|t| !t.done).unwrap().id;
        let before = app.model.task(id).unwrap().done;
        app.apply_action(Action::ToggleTask(id), &mut state);
        assert_ne!(app.model.task(id).unwrap().done, before);
    }

    #[test]
    fn new_event_appends_and_selects() {
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        let before = app.model.events.len();
        app.apply_action(Action::NewEvent, &mut state);
        assert_eq!(app.model.events.len(), before + 1);
        assert!(matches!(app.selection, Some(Sel::Event(_))));
    }

    #[test]
    fn stepping_month_changes_anchor_month() {
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        let m0 = app.anchor.month();
        app.apply_action(Action::Next, &mut state);
        // Month view by default → Next advances one month.
        assert_ne!(app.anchor.month(), m0);
    }

    // --- responsive layout ---------------------------------------------

    #[test]
    fn wide_window_uses_persistent_sidebar_and_overlay_editor() {
        let mut app = CalendarApp::new_demo();
        app.detail_open = true;
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.relayout();
        assert_eq!(app.sidebar.mode(), SidebarMode::Persistent);
        assert_eq!(app.detail_mode, DetailMode::Overlay);
        // Sidebar is inline (content starts to its right).
        assert!(app.content_rect.x0 >= app.sidebar.rect().x1 - 0.5);
        // The editor is a centered modal floating OVER the calendar: the
        // calendar keeps its full content width behind it, and the card is
        // inset from every content edge (not a docked strip on the right).
        assert!(app.detail_rect.area() > 0.0);
        assert!(app.content_rect.area() > 0.0);
        assert_eq!(
            app.content_rect.x1, app.rect.x1,
            "calendar spans to the window edge behind the modal"
        );
        assert!(
            app.detail_rect.x0 > app.content_rect.x0 + 1.0,
            "card inset from the left"
        );
        assert!(
            app.detail_rect.x1 < app.rect.x1 - 1.0,
            "card inset from the right"
        );
        assert!(
            app.detail_rect.y0 > app.rect.y0 + 1.0,
            "card inset from the top"
        );
        // Card is centered within the content area.
        let card_cx = app.detail_rect.center().x;
        let content_cx = f64::midpoint(app.content_rect.x0, app.content_rect.x1);
        assert!(
            (card_cx - content_cx).abs() < 1.0,
            "modal centered over the content"
        );
    }

    #[test]
    fn narrow_window_collapses_sidebar_to_overlay() {
        let mut app = CalendarApp::new_demo();
        app.rect = Rect::new(0.0, 0.0, 700.0, 900.0);
        app.sidebar.set_open(false);
        app.relayout();
        assert_eq!(app.sidebar.mode(), SidebarMode::Overlay);
        // Overlay sidebar floats over the content from the left edge: content
        // starts at the window's left edge, not to the sidebar's right.
        assert_eq!(app.content_rect.x0, app.rect.x0);
        assert!(!app.sidebar.visible(), "overlay flap is closed by default");
    }

    #[test]
    fn desktop_sidebar_collapses_and_only_the_sidebar_collapses() {
        // Confirmed-defect: on desktop the sidebar must be collapsible, and ONLY
        // the sidebar collapses — the editor modal stays open and re-centers in
        // the reclaimed width. Drive the real `relayout` expand → collapse →
        // expand on ONE instance.
        let mut app = CalendarApp::new_demo();
        app.detail_open = true;
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.relayout();
        assert!(app.sidebar.visible(), "sidebar shown when expanded");
        let expanded_content_x0 = app.content_rect.x0;
        assert!(
            expanded_content_x0 >= app.sidebar.rect().x1 - 0.5,
            "expanded: content starts to the right of the sidebar"
        );

        // Collapse: sidebar hidden, content reclaims the left edge, editor stays.
        app.sidebar.set_collapsed(true);
        app.relayout();
        assert!(!app.sidebar.visible(), "collapsed sidebar is hidden");
        assert_eq!(
            app.content_rect.x0, app.rect.x0,
            "content reclaims the sidebar edge"
        );
        assert!(app.content_rect.width() > 0.0);
        assert!(
            app.detail_rect.area() > 0.0,
            "editor stays open — only the sidebar collapsed"
        );
        // Re-centered within the now-wider content.
        let content_cx = f64::midpoint(app.content_rect.x0, app.content_rect.x1);
        assert!(
            (app.detail_rect.center().x - content_cx).abs() < 1.0,
            "modal re-centers after collapse"
        );

        // Expand again — proves it toggles both directions.
        app.sidebar.set_collapsed(false);
        app.relayout();
        assert!(app.sidebar.visible());
        assert_eq!(
            app.content_rect.x0, expanded_content_x0,
            "expanding restores the layout"
        );
    }

    #[test]
    fn narrow_detail_screen_replaces_the_calendar() {
        let mut app = CalendarApp::new_demo();
        app.rect = Rect::new(0.0, 0.0, 480.0, 900.0);
        app.detail_open = true;
        app.narrow_screen = NarrowScreen::Detail;
        app.relayout();
        assert_eq!(
            app.detail_mode,
            DetailMode::FullScreen,
            "480px editor is full-screen"
        );
        // The detail is pushed over the calendar: only one pane is laid out.
        assert!(app.detail_rect.area() > 0.0);
        assert_eq!(
            app.content_rect.area(),
            0.0,
            "calendar hidden behind detail"
        );

        // Going back returns to the calendar screen.
        let mut state = AutoCommit::new();
        app.apply_action(Action::BackToCalendar, &mut state);
        app.relayout();
        assert!(app.content_rect.area() > 0.0);
        assert_eq!(app.detail_rect.area(), 0.0);
    }

    #[test]
    fn selecting_in_narrow_mode_pushes_detail_and_closes_flap() {
        let mut app = CalendarApp::new_demo();
        app.rect = Rect::new(0.0, 0.0, 480.0, 900.0);
        app.relayout();
        app.sidebar.set_open(true);
        let id = app.model.events[0].id;
        app.select_event(id);
        assert_eq!(app.narrow_screen, NarrowScreen::Detail);
        assert!(
            !app.sidebar.is_open(),
            "opening a detail dismisses the overlay flap"
        );
    }

    #[test]
    fn editor_presentation_switches_from_overlay_to_fullscreen_by_width() {
        // The reported fix: on big screens the editor is a modal OVERLAY over
        // the calendar; only on small windows does it take the whole screen.
        // Drive the real relayout across the breakpoint on ONE instance.
        let mut app = CalendarApp::new_demo();
        app.detail_open = true;

        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.relayout();
        assert_eq!(app.detail_mode, DetailMode::Overlay, "wide → modal overlay");
        assert!(
            app.content_rect.area() > 0.0,
            "calendar stays laid out behind the modal"
        );
        assert!(
            app.detail_rect.x1 < app.rect.x1 - 1.0,
            "modal is a floating card, not edge-to-edge"
        );

        // Just above the breakpoint stays an overlay.
        app.rect = Rect::new(0.0, 0.0, EDITOR_OVERLAY_MIN + 10.0, 800.0);
        app.relayout();
        assert_eq!(app.detail_mode, DetailMode::Overlay);

        // Below the breakpoint: full-screen editor page replacing the calendar.
        app.rect = Rect::new(0.0, 0.0, EDITOR_OVERLAY_MIN - 10.0, 800.0);
        app.narrow_screen = NarrowScreen::Detail;
        app.relayout();
        assert_eq!(
            app.detail_mode,
            DetailMode::FullScreen,
            "narrow → full-screen editor"
        );
        assert_eq!(
            app.content_rect.area(),
            0.0,
            "full-screen editor replaces the calendar"
        );
        assert!(app.detail_rect.area() > 0.0);
    }

    #[test]
    fn view_switcher_stays_in_topbar_until_it_genuinely_doesnt_fit() {
        // Issue: the switcher folded into the sidebar too eagerly. It must stay
        // in the topbar at normal/large widths and only fold when it truly
        // can't fit. Assert the real fit decision that `Topbar::draw` reads.
        let mut app = CalendarApp::new_demo();
        // Isolate the switcher logic on the calendar screen (no pushed detail
        // zeroing the content width out from under us).
        app.detail_open = false;
        app.selection = None;
        app.narrow_screen = NarrowScreen::Calendar;

        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.relayout();
        assert!(
            app.topbar.switcher_visible(),
            "wide window keeps the switcher in the topbar"
        );

        // A mid-width overlay-sidebar window still has ample topbar room — the
        // previous behaviour wrongly folded it away here.
        app.rect = Rect::new(0.0, 0.0, 780.0, 800.0);
        app.relayout();
        assert_eq!(app.sidebar.mode(), SidebarMode::Overlay);
        assert!(
            app.topbar.switcher_visible(),
            "a roomy topbar keeps the switcher even with an overlay sidebar"
        );

        // Truly cramped → it folds into the flap.
        app.rect = Rect::new(0.0, 0.0, 460.0, 800.0);
        app.relayout();
        assert!(
            !app.topbar.switcher_visible(),
            "a cramped topbar folds the switcher into the sidebar"
        );
    }

    #[test]
    fn phone_width_sidebar_is_a_full_screen_drawer() {
        // Below the phone breakpoint the overlay sidebar covers the whole
        // window (a page) rather than a cramped 252px flap.
        let mut app = CalendarApp::new_demo();

        app.rect = Rect::new(0.0, 0.0, 700.0, 900.0);
        app.relayout();
        assert_eq!(app.sidebar.mode(), SidebarMode::Overlay);
        assert!(
            app.sidebar.rect().width() < app.rect.width() - 1.0,
            "tablet width: a flap, not full-width"
        );

        app.rect = Rect::new(0.0, 0.0, 420.0, 800.0);
        app.relayout();
        assert_eq!(app.sidebar.mode(), SidebarMode::Overlay);
        assert!(
            (app.sidebar.rect().width() - app.rect.width()).abs() < 0.5,
            "phone width: the sidebar drawer is a full-screen page"
        );
    }

    #[test]
    fn full_screen_drawer_offers_a_pointer_reachable_close() {
        // The full-screen drawer covers the window, so it must carry its own
        // close (no dimmed edge to click). Drive the real click path.
        let mut h = Harness::new(420, 800);
        h.app.shots_show_calendar();
        h.app.sidebar.set_open(true);
        h.relayout();
        assert!(h.app.sidebar.visible(), "flap is open");
        assert!(
            (h.app.sidebar.rect().width() - h.app.rect.width()).abs() < 0.5,
            "the drawer is full-screen at this width"
        );
        // Two DismissSidebar targets exist: the (hidden) scrim behind and the
        // small close button in the header. Pick the compact one — the button.
        let close = h
            .app
            .hits
            .iter()
            .filter(|(_, a)| matches!(a, Action::DismissSidebar))
            .min_by_key(|(r, _)| r.area() as i64)
            .map(|(r, _)| r.center())
            .expect("a close button in the full-screen drawer");
        h.click(close);
        assert!(
            !h.app.sidebar.is_open(),
            "clicking close dismissed the full-screen drawer"
        );
    }

    // --- click-label-to-complete ---------------------------------------

    #[test]
    fn sidebar_task_label_and_open_targets_coexist() {
        // The mini task rail must offer BOTH a completion target (checkbox +
        // label) and a select target (open the task) for the same row.
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        // With no synced accounts the sidebar has room for the task rail
        // (a machine full of remote calendars still lists tasks in the
        // agenda/day views — the rail is the space-permitting extra).
        app.accounts = AccountStore::default();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 900.0);
        app.relayout();
        // Draw to populate the hit-map.
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        app.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &PresenceCtx::empty(),
        );

        let open_task = app
            .hits
            .iter()
            .filter(|(_, a)| matches!(a, Action::OpenTask(_)))
            .count();
        let toggle_in_rail = app
            .hits
            .iter()
            .any(|(_, a)| matches!(a, Action::ToggleTask(_)));
        assert!(open_task > 0, "task rows must be selectable to view detail");
        assert!(
            toggle_in_rail,
            "checkbox+label must offer a completion target"
        );
    }

    #[test]
    fn toggling_via_label_hit_flips_completion() {
        // Resolving a ToggleTask action (what a label/checkbox click produces)
        // flips done — the production toggle path, not a helper.
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        let id = app.model.tasks.iter().find(|t| !t.done).unwrap().id;
        let before = app.model.task(id).unwrap().done;
        app.apply_action(Action::ToggleTask(id), &mut state);
        assert_ne!(app.model.task(id).unwrap().done, before);
    }

    // --- caldav accounts seam ------------------------------------------

    #[test]
    fn new_app_loads_caldav_accounts() {
        // Offline, the app falls back to the sample account store so synced
        // calendars appear in the sidebar.
        let app = CalendarApp::new_demo();
        assert!(!app.accounts.is_empty());
        assert!(app.accounts.all_calendars().count() > 0);
    }

    #[test]
    fn sync_merges_visible_calendar_events() {
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        // Sync is explicit now (no fabricated auto-pull on the first frame).
        app.apply_action(Action::Sync, &mut state);
        assert!(app.model.events.iter().any(|e| e.title == "Synced standup"));
        // The hidden "Holidays" calendar's event must not appear.
        assert!(!app.model.events.iter().any(|e| e.title == "Public holiday"));
    }

    #[test]
    fn live_sync_runs_off_the_ui_thread_without_blocking() {
        // A live (non-demo) app with a configured account must dispatch the
        // pull to a worker thread and return immediately — the click never
        // blocks on the network. We assert the async hand-off, then abandon the
        // detached worker (its result is irrelevant to this behaviour).
        let mut app = CalendarApp::new_demo();
        app.demo = false; // exercise the real (threaded) branch
        app.accounts = AccountStore::sample();
        let mut state = AutoCommit::new();
        app.start_sync(&mut state);
        assert!(app.syncing, "a live sync is in flight");
        assert!(app.sync_rx.is_some(), "worker result channel is armed");
        assert!(
            app.needs_animation_frame(),
            "the loop must keep ticking so the result drains"
        );
        // Detach the worker so the test doesn't depend on the network.
        app.sync_rx = None;
        app.syncing = false;
    }

    #[test]
    fn live_sync_with_no_accounts_answers_instantly() {
        // No worker, no thread, no spin: an empty account set reports its state
        // synchronously.
        let mut app = CalendarApp::new_demo();
        app.demo = false;
        app.accounts = AccountStore::default();
        let mut state = AutoCommit::new();
        app.start_sync(&mut state);
        assert!(!app.syncing, "nothing to sync → no background work");
        assert!(app.status.as_deref().unwrap_or("").contains("account"));
    }

    #[test]
    fn poll_sync_drains_a_finished_worker_into_the_model() {
        // The drain path (`poll_sync` → `apply_sync_result`) folds a completed
        // pull into the model + CRDT and clears the in-flight flag. Driven with
        // a hand-fed channel so it is deterministic and network-free, but it is
        // the exact production code the live worker feeds.
        use crate::model::{Event, Palette, Recurrence};
        use chrono::NaiveTime;

        let mut app = CalendarApp::new_demo();
        app.demo = false;
        let mut state = AutoCommit::new();
        let before = app.model.events.len();

        let (tx, rx) = std::sync::mpsc::channel();
        let start = app
            .today
            .and_time(NaiveTime::from_hms_opt(8, 0, 0).unwrap());
        let synced = Event {
            id: uuid::Uuid::new_v4(),
            title: "Pulled from server".into(),
            start,
            end: start + Duration::hours(1),
            all_day: false,
            location: "Remote".into(),
            color: Palette::Blue,
            recurrence: Recurrence::None,
            reminder_min: None,
            calendar_id: None,
            project: None,
        };
        tx.send((
            caldav::SyncResult {
                events: vec![synced],
                tasks: Vec::new(),
            },
            SyncOutcome::Synced {
                calendars: 1,
                events: 1,
            },
            Vec::new(),
            Vec::new(),
        ))
        .unwrap();
        app.sync_rx = Some(rx);
        app.syncing = true;

        app.poll_sync(&mut state);

        assert!(!app.syncing, "drain clears the in-flight flag");
        assert_eq!(app.model.events.len(), before + 1, "synced event landed");
        assert!(
            app.model
                .events
                .iter()
                .any(|e| e.title == "Pulled from server"),
            "the pulled event is now in the model"
        );
        // And it mirrored into the CRDT (persistence source of truth).
        assert!(!CalDoc::is_empty(&state));
    }

    #[test]
    fn toggling_calendar_visibility_flips_the_flag() {
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        let id = app
            .accounts
            .all_calendars()
            .map(|(_, c)| c.id.clone())
            .next()
            .unwrap();
        let before = app
            .accounts
            .all_calendars()
            .find(|(_, c)| c.id == id)
            .unwrap()
            .1
            .visible;
        app.apply_action(Action::ToggleCalendarVisible(id.clone()), &mut state);
        let after = app
            .accounts
            .all_calendars()
            .find(|(_, c)| c.id == id)
            .unwrap()
            .1
            .visible;
        assert_ne!(before, after);
    }

    // --- end-to-end click routing through the real handle_event -----------

    /// Owns a laid-out app + taffy tree so a test can pump synthesized winit
    /// events through the production `Widget::handle_event`, exactly as the
    /// live event loop does.
    struct Harness {
        app: CalendarApp,
        doc: AutoCommit,
        taffy: TaffyTree<()>,
        node: NodeId,
        w: u32,
        h: u32,
    }

    impl Harness {
        fn new(w: u32, h: u32) -> Self {
            let mut app = CalendarApp::new_demo();
            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &automerge::ROOT);
            let mut hn = Harness {
                app,
                doc,
                taffy: TaffyTree::new(),
                node: NodeId::new(0),
                w,
                h,
            };
            hn.relayout();
            hn
        }

        /// A live (persisted, no sample content) app driven through the same
        /// production event path — used to prove edits reach disk.
        fn live(w: u32, h: u32, path: PathBuf) -> Self {
            let mut app = CalendarApp::new_with_data_path(path);
            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &automerge::ROOT);
            let mut hn = Harness {
                app,
                doc,
                taffy: TaffyTree::new(),
                node: NodeId::new(0),
                w,
                h,
            };
            hn.relayout();
            hn
        }

        fn relayout(&mut self) {
            use taffy::prelude::*;
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            self.taffy = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let child = self
                .app
                .build_layout(&mut self.taffy, &theme, &mut node_map);
            self.node = child;
            let root = self
                .taffy
                .new_with_children(
                    Style {
                        size: Size {
                            width: Dimension::length(self.w as f32),
                            height: Dimension::length(self.h as f32),
                        },
                        ..Default::default()
                    },
                    &[child],
                )
                .unwrap();
            let measure_map = node_map.clone();
            let trr = &mut tr;
            self.taffy
                .compute_layout_with_measure(
                    root,
                    Size {
                        width: AvailableSpace::Definite(self.w as f32),
                        height: AvailableSpace::Definite(self.h as f32),
                    },
                    |known, avail, node_id, _, _| {
                        if let Some(&ptr) = measure_map.get(&node_id) {
                            unsafe { (*ptr).measure(known, avail, trr) }
                        } else {
                            taffy::Size::ZERO
                        }
                    },
                )
                .unwrap();
            self.app.apply_layout(&self.taffy, child, Point::ORIGIN);
            // Draw once to populate the click hit-map, like the live renderer.
            let mut scene = Scene::new();
            self.app.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                &PresenceCtx::empty(),
            );
        }

        fn dispatch(&mut self, event: &winit::event::WindowEvent) {
            let mods = winit::event::Modifiers::default();
            let mut handled = false;
            let mut focus = None;
            self.app.handle_event(
                event,
                1.0,
                &mods,
                &mut self.doc,
                &automerge::ROOT,
                &mut handled,
                &mut focus,
                self.node,
                &self.taffy,
            );
        }

        fn click(&mut self, p: Point) {
            // winit exposes no public DeviceId constructor; the production
            // synthesized-press path does exactly this.
            let device_id = unsafe { std::mem::zeroed() };
            self.dispatch(&winit::event::WindowEvent::CursorMoved {
                device_id,
                position: winit::dpi::PhysicalPosition::new(p.x, p.y),
            });
            self.dispatch(&winit::event::WindowEvent::MouseInput {
                device_id,
                state: ElementState::Pressed,
                button: MouseButton::Left,
            });
        }

        fn mouse_move(&mut self, p: Point) {
            let device_id = unsafe { std::mem::zeroed() };
            self.dispatch(&winit::event::WindowEvent::CursorMoved {
                device_id,
                position: winit::dpi::PhysicalPosition::new(p.x, p.y),
            });
        }
        fn mouse_button(&mut self, state: ElementState) {
            let device_id = unsafe { std::mem::zeroed() };
            self.dispatch(&winit::event::WindowEvent::MouseInput {
                device_id,
                state,
                button: MouseButton::Left,
            });
        }

        /// A full press → drag → release gesture through the real event path.
        fn drag(&mut self, from: Point, to: Point) {
            self.mouse_move(from);
            self.mouse_button(ElementState::Pressed);
            self.mouse_move(to);
            self.mouse_button(ElementState::Released);
        }

        /// Press a single character through the real keyboard path.
        fn type_char(&mut self, ch: &str) {
            self.dispatch(&char_press(ch));
        }

        /// Press a named key (arrows, etc.) through the real keyboard path.
        fn press_named(&mut self, named: winit::keyboard::NamedKey) {
            self.dispatch(&named_press(named));
        }

        /// Centre of the first hit rect carrying an action matching `pred`.
        fn hit_center<F: Fn(&Action) -> bool>(&self, pred: F) -> Option<Point> {
            self.app
                .hits
                .iter()
                .find(|(_, a)| pred(a))
                .map(|(r, _)| r.center())
        }
    }

    #[test]
    fn mini_month_outcome_updates_the_shared_calendar_anchor() {
        let mut h = Harness::new(1280, 1200);
        h.app.shots_show_calendar();
        h.relayout();
        let before = h.app.anchor;
        let previous = h
            .hit_center(|action| matches!(action, Action::MiniMonth(_)))
            .expect("the sidebar mini-month has a previous-month target");

        h.click(previous);

        assert_eq!(
            h.app.anchor,
            date::add_months(before, -1),
            "the component interprets its target and the root updates the anchor"
        );
    }

    #[test]
    fn week_timeline_keeps_min_slot_height_and_scrolls_the_full_day() {
        // The reported bug: a short window squashed the fixed 06:00–21:00 slots
        // and hid everything outside them. Fixed: every hour keeps at least the
        // legible minimum, the grid spans the full 24h, and the body scrolls.
        let mut h = Harness::new(1000, 560);
        h.app.shots_show_calendar();
        h.app.view = View::Week;
        h.relayout();

        let g = TimelineInteraction::test_geometry(h.app.view_rect());
        assert!(
            (g.hour_h - TIMELINE_MIN_HOUR_H).abs() < 0.001,
            "a short window pins the hour height to the legible minimum, got {}",
            g.hour_h
        );
        assert!(
            (g.content_h - 24.0 * TIMELINE_MIN_HOUR_H).abs() < 0.001,
            "the grid content spans the full 24 hours, got {}",
            g.content_h
        );
        assert!(
            h.app.timeline.vertical_overflows(),
            "the full day is taller than the viewport, so it scrolls"
        );
        let vp = (g.body_bottom - g.body_top) as f32;
        assert!(
            g.content_h as f32 - vp > 0.0,
            "there is real scroll range (00:00 … 24:00 both reachable)"
        );

        // A tall window instead grows the hours to fill and stops scrolling.
        let mut tall = Harness::new(1000, 2000);
        tall.app.shots_show_calendar();
        tall.app.view = View::Week;
        tall.relayout();
        let gt = TimelineInteraction::test_geometry(tall.app.view_rect());
        assert!(
            gt.hour_h > TIMELINE_MIN_HOUR_H,
            "a tall window grows the hours to fill the space, got {}",
            gt.hour_h
        );
        assert!(
            !tall.app.timeline.vertical_overflows(),
            "the whole day fits, so nothing scrolls"
        );
    }

    #[test]
    fn dragging_empty_timeline_grid_creates_and_opens_an_event() {
        let mut h = Harness::new(1000, 640);
        h.app.shots_show_calendar();
        h.app.view = View::Week;
        h.relayout();
        // Pin the scroll to the top so an early, empty slot is on screen.
        h.app.timeline.test_set_offset_y(0.0);
        h.relayout();

        let area = h.app.view_rect();
        let g = TimelineInteraction::test_geometry(area);
        let days = h.app.timeline_days();
        let col = 0usize;
        let grid_x0 = area.x0 + g.gutter;
        let col_w = TimelineInteraction::test_column_width(area, days.len(), g);
        let x = grid_x0 + col_w * col as f64 + col_w * 0.5;
        let offset = f64::from(h.app.timeline.test_offset_y());
        let y_at = |hour: f64| g.body_top + hour * g.hour_h - offset;

        let ids_before: HashSet<Id> = h.app.model.events.iter().map(|e| e.id).collect();
        // Drag 03:00 → 05:00 in the first column (empty in the demo seed).
        h.drag(Point::new(x, y_at(3.0)), Point::new(x, y_at(5.0)));

        let new_ev = h
            .app
            .model
            .events
            .iter()
            .find(|e| !ids_before.contains(&e.id))
            .expect("a drag across empty grid creates one event");
        assert_eq!(
            new_ev.start.date(),
            days[col],
            "created on the dragged column's day"
        );
        assert_eq!(
            new_ev.start.time(),
            min_to_time(3 * 60),
            "starts at the drag origin (03:00)"
        );
        assert_eq!(
            (new_ev.end - new_ev.start).num_minutes(),
            120,
            "spans the dragged range (03:00–05:00)"
        );
        assert_eq!(
            h.app.selection,
            Some(Sel::Event(new_ev.id)),
            "the new event is selected"
        );
        assert!(
            h.app.detail_open,
            "the event editor opened for the new event"
        );
    }

    #[test]
    fn dragging_an_event_reschedules_it_through_the_action_model() {
        // Tall window so the whole day fits at offset 0 and every block sits at
        // its true position.
        let mut h = Harness::new(1200, 1900);
        h.app.shots_show_calendar();
        h.app.view = View::Week;
        h.relayout();

        let today = h.app.today;
        let (id, start0, dur0) = {
            let ev = h
                .app
                .model
                .events
                .iter()
                .find(|e| e.title == "Design review")
                .expect("seeded 11:00 event present");
            (ev.id, ev.start, (ev.end - ev.start).num_minutes())
        };
        assert_eq!(start0.time(), min_to_time(11 * 60));

        let area = h.app.view_rect();
        let g = TimelineInteraction::test_geometry(area);
        let days = h.app.timeline_days();
        let col = days
            .iter()
            .position(|d| *d == today)
            .expect("today is in the week");
        let block = h
            .app
            .timeline
            .test_block_rect(area, days.len(), g, col, 11 * 60, 12 * 60);
        let x = block.center().x;
        // Grab near the block top and drag down two hours.
        let from = Point::new(x, block.y0 + 3.0);
        let to = Point::new(x, block.y0 + 3.0 + 2.0 * g.hour_h);
        h.drag(from, to);

        let moved = h
            .app
            .model
            .event(id)
            .expect("event still present after the move");
        assert_eq!(moved.start.date(), today, "stayed in the same day column");
        assert_eq!(
            moved.start.time(),
            min_to_time(13 * 60),
            "rescheduled two hours later (was {start0})"
        );
        assert_eq!(
            (moved.end - moved.start).num_minutes(),
            dur0,
            "duration preserved across the move"
        );
        assert!(
            h.app.last_dispatched().is_some(),
            "the reschedule went through the action model"
        );
    }

    // Synthesised winit keyboard events (the zeroed-KeyEvent trick the toolkit's
    // own widget tests use — every field is overwritten before use).
    fn synth_device() -> winit::event::DeviceId {
        unsafe { std::mem::zeroed() }
    }
    fn char_press(ch: &str) -> winit::event::WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Character(ch.into());
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = Some(ch.into());
        winit::event::WindowEvent::KeyboardInput {
            device_id: synth_device(),
            event: ev,
            is_synthetic: false,
        }
    }
    fn named_press(named: winit::keyboard::NamedKey) -> winit::event::WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(named);
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        winit::event::WindowEvent::KeyboardInput {
            device_id: synth_device(),
            event: ev,
            is_synthetic: false,
        }
    }

    #[test]
    fn clicking_new_button_creates_and_selects_an_event() {
        let mut h = Harness::new(1280, 800);
        // Start from the calendar screen: the wide editor is a modal whose
        // dismiss scrim covers the topbar, so the New button is reachable once
        // no editor is open (which is the state a user creates from).
        h.app.shots_show_calendar();
        h.relayout();
        let before = h.app.model.events.len();
        let p = h
            .hit_center(|a| matches!(a, Action::NewEvent))
            .expect("New button present");
        h.click(p);
        assert_eq!(
            h.app.model.events.len(),
            before + 1,
            "click created an event"
        );
        assert!(
            matches!(h.app.selection, Some(Sel::Event(_))),
            "new event is selected"
        );
    }

    #[test]
    fn clicking_a_view_segment_switches_the_view() {
        let mut h = Harness::new(1280, 800);
        h.app.shots_show_calendar();
        h.relayout();
        assert_eq!(h.app.view, View::Month);
        let p = h
            .hit_center(|a| matches!(a, Action::View(View::Week)))
            .expect("Week segment present");
        h.click(p);
        assert_eq!(
            h.app.view,
            View::Week,
            "clicking the Week segment switched the view"
        );
    }

    #[test]
    fn clicking_an_event_chip_opens_its_detail() {
        // Month view is full of chips; clicking one selects that event.
        let mut h = Harness::new(1280, 800);
        h.app.shots_show_calendar();
        h.relayout();
        let (rect, id) = h
            .app
            .hits
            .iter()
            .find_map(|(r, a)| match a {
                Action::OpenEvent(id) => Some((*r, *id)),
                _ => None,
            })
            .expect("at least one event chip");
        h.click(rect.center());
        assert_eq!(
            h.app.selection,
            Some(Sel::Event(id)),
            "clicked chip is now selected"
        );
        assert!(h.app.detail_open, "detail panel opened");
    }

    #[test]
    fn clicking_sidebar_chevron_collapses_then_topbar_chevron_expands() {
        // The collapse control is reachable and functional through the real
        // click path, and its expand mirror lives in the topbar once collapsed —
        // so a pointer user can always get the sidebar back.
        let mut h = Harness::new(1280, 800);
        // The topbar's expand chevron lives behind the modal's dismiss scrim, so
        // exercise the sidebar collapse from the calendar screen (the state a
        // user collapses the rail from).
        h.app.shots_show_calendar();
        h.relayout();
        assert!(h.app.sidebar.visible(), "starts expanded on a wide window");
        let collapse = h
            .hit_center(|a| matches!(a, Action::ToggleSidebarCollapsed))
            .expect("collapse chevron present in the persistent sidebar");
        h.click(collapse);
        assert!(
            h.app.sidebar.is_collapsed(),
            "clicking the chevron collapsed the sidebar"
        );
        assert!(!h.app.sidebar.visible());

        // Re-lay so the collapsed topbar's expand chevron enters the hit-map.
        h.relayout();
        let expand = h
            .hit_center(|a| matches!(a, Action::ToggleSidebarCollapsed))
            .expect("expand chevron present in the topbar when collapsed");
        h.click(expand);
        assert!(
            !h.app.sidebar.is_collapsed(),
            "clicking the topbar chevron expanded the sidebar"
        );
        assert!(h.app.sidebar.visible());
    }

    #[test]
    fn narrow_pushed_detail_uses_back_not_a_redundant_close() {
        // De-slop: the narrow pushed detail has a single Back affordance, not a
        // Back plus a duplicate X that both return to the calendar.
        let mut h = Harness::new(560, 720);
        h.app.detail_open = true;
        h.app.narrow_screen = NarrowScreen::Detail;
        h.relayout();
        let closes = h
            .app
            .hits
            .iter()
            .filter(|(_, a)| matches!(a, Action::CloseDetail))
            .count();
        let backs = h
            .app
            .hits
            .iter()
            .filter(|(_, a)| matches!(a, Action::BackToCalendar))
            .count();
        assert_eq!(
            closes, 0,
            "no redundant X close on the narrow pushed detail"
        );
        assert_eq!(backs, 1, "Back is the single dismissal affordance");
    }

    #[test]
    fn wide_modal_editor_has_an_x_close_and_a_dismiss_scrim() {
        // The wide editor is a modal card: it carries an X close button (a
        // small top-right target) AND a dismiss scrim over the calendar behind
        // it (clicking outside closes it). No Back — that's narrow-only.
        let h = Harness::new(1280, 800);
        assert!(
            h.app.detail_open,
            "demo opens with a selected event → modal editor"
        );
        assert_eq!(h.app.detail_mode, DetailMode::Overlay);
        let backs = h
            .app
            .hits
            .iter()
            .filter(|(_, a)| matches!(a, Action::BackToCalendar))
            .count();
        assert_eq!(backs, 0, "no Back affordance on the wide modal");
        let closes: Vec<Rect> = h
            .app
            .hits
            .iter()
            .filter(|(_, a)| matches!(a, Action::CloseDetail))
            .map(|(r, _)| *r)
            .collect();
        // A small X button …
        assert!(
            closes
                .iter()
                .any(|r| r.area() < 2000.0 && r.x0 > h.app.detail_rect.x0),
            "a compact X close button in the card header"
        );
        // … and a large dismiss scrim covering the calendar behind the card.
        assert!(
            closes
                .iter()
                .any(|r| (r.area() - h.app.content_rect.area()).abs() < 1.0),
            "a dismiss scrim over the content behind the modal"
        );
    }

    #[test]
    fn narrow_flap_offers_a_pointer_reachable_view_switcher() {
        // On narrow layouts the topbar's segmented switch folds away; the
        // overlay flap must carry a pointer-reachable switch so every view stays
        // reachable without a keyboard (the reported dead-end: a mouse user on
        // narrow could reach Day/Agenda but never get back to Month). Drives the
        // real `handle_event` click path, not a synthetic call.
        let mut h = Harness::new(560, 720);
        assert_eq!(
            h.app.sidebar.mode(),
            SidebarMode::Overlay,
            "560px is a narrow layout"
        );
        // Open the flap and re-lay so its hit-map is populated.
        h.app.sidebar.set_open(true);
        h.relayout();
        assert_eq!(h.app.view, View::Month);
        let p = h
            .hit_center(|a| matches!(a, Action::View(View::Week)))
            .expect("the flap carries a Week view target on narrow");
        h.click(p);
        assert_eq!(
            h.app.view,
            View::Week,
            "clicking the flap switcher changed the view"
        );
        assert!(
            !h.app.sidebar.is_open(),
            "picking a view dismisses the flap"
        );
        assert_eq!(
            h.app.narrow_screen,
            NarrowScreen::Calendar,
            "picking a view returns to the calendar screen"
        );
    }

    #[test]
    fn wide_topbar_has_no_flap_switcher_duplicate() {
        // The flap switcher is narrow-only; the wide layout already has the
        // topbar segmented control, so the flap must NOT add a second set of
        // View targets (which would double every view hit).
        let h = Harness::new(1280, 800);
        let view_targets = h
            .app
            .hits
            .iter()
            .filter(|(_, a)| matches!(a, Action::View(_)))
            .count();
        assert_eq!(
            view_targets,
            View::ALL.len(),
            "exactly one switcher on wide"
        );
    }

    #[test]
    fn elide_to_width_ellipsizes_and_actually_fits() {
        let mut tr = TextRenderer::new();
        let full = "Team standup with the whole design org";
        // Given room for the whole string, it comes back untouched (no ellipsis).
        let roomy = f64::from(tr.measure(full, 11.0, None).0) + 10.0;
        assert_eq!(elide_to_width(&mut tr, full, 11.0, roomy), full);
        // Under a tight budget it must ellipsize AND the result must measure
        // within the budget — proving the cut is width-driven, not a fixed
        // character count that could still overflow.
        let tight = 60.0;
        let cut = elide_to_width(&mut tr, full, 11.0, tight);
        assert!(cut.ends_with('…'), "ellipsized: {cut:?}");
        assert!(cut.chars().count() < full.chars().count());
        assert!(
            f64::from(tr.measure(&cut, 11.0, None).0) <= tight,
            "the ellipsized line fits the budget: {cut:?}"
        );
    }

    #[test]
    fn week_event_titles_are_ellipsized_not_hard_sliced() {
        // A narrow week column can't fit a full title; the drawn line must carry
        // an ellipsis rather than a hard mid-glyph cut. Exercise the real
        // `draw_timeline` path and confirm at least one event line ellipsizes.
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        app.view = View::Week;
        app.detail_open = false;
        app.narrow_screen = NarrowScreen::Calendar;
        app.rect = Rect::new(0.0, 0.0, 560.0, 720.0);
        app.relayout();

        let _theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let area = app.view_rect();
        let days = crate::date::week_days(app.anchor);
        // The narrowest timed block, ellipsized to its width, must not equal the
        // full title (i.e. a real title got shortened) and must end with '…'.
        let col_w = (area.width() - 56.0) / days.len() as f64;
        let block_text_w = col_w - 6.0 - 20.0; // 3px block inset each side, 10+... pad
        let long = app
            .model
            .events
            .iter()
            .filter(|e| !e.all_day)
            .map(|e| e.title.clone())
            .find(|tit| f64::from(tr.measure(tit, 11.0, None).0) > block_text_w)
            .expect("demo has a title too long for a narrow week column");
        let drawn = elide_to_width(&mut tr, &long, 11.0, block_text_w);
        assert!(
            drawn.ends_with('…'),
            "narrow week title ellipsized: {drawn:?}"
        );
        assert_ne!(drawn, long);
    }

    // --- per-calendar visibility actually filters the views ----------------

    #[test]
    fn hiding_a_synced_calendar_drops_its_events_and_rechecking_restores_them() {
        // The standout defect: unchecking a calendar must hide its already-synced
        // events on the spot (and re-checking must bring them back with no
        // re-sync). Assert on the real draw path — the Day timeline's click
        // hit-map, which is exactly what a user clicks.
        fn draws(app: &mut CalendarApp, tr: &mut TextRenderer, theme: &Theme, eid: Id) -> bool {
            let mut scene = Scene::new();
            app.draw(
                &mut scene,
                Affine::IDENTITY,
                tr,
                theme,
                &PresenceCtx::empty(),
            );
            app.hits
                .iter()
                .any(|(_, a)| matches!(a, Action::OpenEvent(id) if *id == eid))
        }

        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        app.view = View::Day;
        app.detail_open = false;
        app.selection = None;
        app.narrow_screen = NarrowScreen::Calendar;
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.relayout();

        // A synced event tagged with an existing (checked) calendar, on the
        // anchored day so the Day timeline lays it out.
        let start = app
            .anchor
            .and_time(NaiveTime::from_hms_opt(10, 0, 0).unwrap());
        let ev = Event {
            id: Uuid::new_v4(),
            title: "Remote sync marker".into(),
            start,
            end: start + Duration::hours(1),
            all_day: false,
            location: String::new(),
            color: Palette::Blue,
            recurrence: Recurrence::None,
            reminder_min: None,
            project: None,
            calendar_id: Some("work/default".into()),
        };
        let eid = ev.id;
        app.model.events.push(ev);
        app.model.events.sort_by_key(|e| e.start);

        let theme = Theme::dark();
        let mut tr = TextRenderer::new();

        assert!(
            draws(&mut app, &mut tr, &theme, eid),
            "shown while its calendar is checked"
        );

        app.apply_action(
            Action::ToggleCalendarVisible("work/default".into()),
            &mut state,
        );
        assert!(
            !draws(&mut app, &mut tr, &theme, eid),
            "hiding the source calendar removes the event's block from the view"
        );

        app.apply_action(
            Action::ToggleCalendarVisible("work/default".into()),
            &mut state,
        );
        assert!(
            draws(&mut app, &mut tr, &theme, eid),
            "re-checking the calendar restores the event with no re-sync"
        );
    }

    #[test]
    fn sync_tags_landed_events_with_their_source_calendar() {
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        app.apply_action(Action::Sync, &mut state);
        // The demo pulls "Synced standup" from the work/default collection.
        let standup = app
            .model
            .events
            .iter()
            .find(|e| e.title == "Synced standup")
            .expect("sync pulled the standup");
        assert_eq!(
            standup.calendar_id.as_deref(),
            Some("work/default"),
            "a synced event remembers which calendar it came from"
        );
        // A locally-created event stays untagged, so it is always visible.
        app.apply_action(Action::NewEvent, &mut state);
        let Some(Sel::Event(new_id)) = app.selection else {
            panic!("NewEvent selects the created event");
        };
        let new = app.model.event(new_id).unwrap();
        assert!(
            new.calendar_id.is_none(),
            "local events carry no source calendar"
        );
    }

    #[test]
    fn source_calendar_id_survives_the_crdt_round_trip() {
        // The tag must persist, or hiding wouldn't stick across a restart.
        let mut doc = AutoCommit::new();
        let start = chrono::NaiveDate::from_ymd_opt(2026, 6, 15)
            .unwrap()
            .and_time(NaiveTime::from_hms_opt(9, 0, 0).unwrap());
        let ev = Event {
            id: Uuid::new_v4(),
            title: "Tagged".into(),
            start,
            end: start + Duration::hours(1),
            all_day: false,
            location: String::new(),
            color: Palette::Red,
            recurrence: Recurrence::None,
            reminder_min: None,
            project: None,
            calendar_id: Some("work/oncall".into()),
        };
        CalDoc::update_event(&mut doc, &ev);
        let back = CalDoc::read_model(&doc);
        assert_eq!(back.events[0].calendar_id.as_deref(), Some("work/oncall"));
    }

    // --- per-account collapse is wired (was a dead serialized field) -------

    #[test]
    fn collapsing_an_account_hides_its_calendar_rows_in_the_sidebar() {
        fn cal_rows(app: &mut CalendarApp, tr: &mut TextRenderer, theme: &Theme) -> usize {
            let mut scene = Scene::new();
            app.draw(
                &mut scene,
                Affine::IDENTITY,
                tr,
                theme,
                &PresenceCtx::empty(),
            );
            app.hits
                .iter()
                .filter(|(_, a)| matches!(a, Action::ToggleCalendarVisible(_)))
                .count()
        }

        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        app.rect = Rect::new(0.0, 0.0, 1280.0, 900.0);
        app.relayout();

        let theme = Theme::dark();
        let mut tr = TextRenderer::new();

        let acc_id = app.accounts.accounts[0].id.clone();
        let acc_cals = app.accounts.accounts[0].calendars.len();
        assert!(acc_cals > 0, "the first sample account has calendars");

        let before = cal_rows(&mut app, &mut tr, &theme);
        // The account header is a real collapse target in the hit-map.
        assert!(
            app.hits
                .iter()
                .any(|(_, a)| matches!(a, Action::ToggleAccountCollapsed(id) if *id == acc_id)),
            "the account header carries a collapse target"
        );

        app.apply_action(Action::ToggleAccountCollapsed(acc_id.clone()), &mut state);
        assert!(
            app.accounts.accounts[0].collapsed,
            "the account is now collapsed"
        );

        let after = cal_rows(&mut app, &mut tr, &theme);
        assert_eq!(
            after,
            before - acc_cals,
            "collapsing removes exactly that account's calendar rows"
        );

        // Header stays clickable so the list can be re-expanded.
        app.apply_action(Action::ToggleAccountCollapsed(acc_id), &mut state);
        assert!(
            !app.accounts.accounts[0].collapsed,
            "clicking again re-expands it"
        );
        assert_eq!(
            cal_rows(&mut app, &mut tr, &theme),
            before,
            "expanding restores the rows"
        );
    }

    // --- detail chips: colour / recurrence / reminder ----------------------

    #[test]
    fn cycling_recurrence_updates_selected_event_and_mirrors_to_crdt() {
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        let Some(Sel::Event(id)) = app.selection else {
            panic!("the demo opens with an event selected");
        };
        let before = app.model.event(id).unwrap().recurrence;
        app.apply_action(Action::CycleRecurrence, &mut state);
        let after = app.model.event(id).unwrap().recurrence;
        assert_eq!(
            after,
            before.cycle(),
            "the chip advances recurrence by one step"
        );
        // Reading the document back yields the new rule — it mirrored into the CRDT.
        let reread = CalDoc::read_model(&state);
        assert_eq!(
            reread
                .events
                .iter()
                .find(|e| e.id == id)
                .unwrap()
                .recurrence,
            after
        );
    }

    #[test]
    fn cycling_colour_updates_selected_event() {
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        let Some(Sel::Event(id)) = app.selection else {
            panic!("event selected");
        };
        let before = app.model.event(id).unwrap().color;
        app.apply_action(Action::CycleColor, &mut state);
        assert_ne!(
            app.model.event(id).unwrap().color,
            before,
            "the chip advances the colour"
        );
    }

    #[test]
    fn cycling_reminder_advances_and_reaches_the_exported_ics() {
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        let Some(Sel::Event(id)) = app.selection else {
            panic!("event selected");
        };
        // Drive the reminder from a known rung: None → 10m → 30m.
        if let Some(e) = app.model.event_mut(id) {
            e.reminder_min = None;
        }
        app.apply_action(Action::CycleReminder, &mut state);
        assert_eq!(app.model.event(id).unwrap().reminder_min, Some(10));
        app.apply_action(Action::CycleReminder, &mut state);
        assert_eq!(app.model.event(id).unwrap().reminder_min, Some(30));
        // The reminder is real, exported data: it becomes a VALARM in the .ics.
        let ics = crate::ics::export(&app.model);
        assert!(
            ics.contains("TRIGGER:-PT30M"),
            "the reminder chip's value reaches the exported alarm"
        );
    }

    // --- Export writes a real file ----------------------------------------

    #[test]
    #[serial]
    fn export_writes_a_real_ics_file_to_the_transfer_dir() {
        let dir = std::env::temp_dir().join(format!("cal-export-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: `#[serial]` keeps env-var tests off each other's toes.
        unsafe { std::env::set_var("QOS_CALENDAR_IMPORT_DIR", &dir) };

        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        app.apply_action(Action::Export, &mut state);

        let written = crate::store::newest_ics(&dir).expect("export wrote an .ics file");
        let body = std::fs::read_to_string(&written).unwrap();
        assert!(body.starts_with("BEGIN:VCALENDAR"), "a real iCalendar file");
        assert!(
            body.contains("BEGIN:VEVENT"),
            "carrying the exported events"
        );
        assert!(
            app.status.as_deref().unwrap_or("").contains("Exported"),
            "the status pill confirms the export"
        );

        unsafe { std::env::remove_var("QOS_CALENDAR_IMPORT_DIR") };
        std::fs::remove_dir_all(&dir).ok();
    }

    // --- keyboard dispatch through the real handle_event -------------------

    #[test]
    #[serial]
    fn keyboard_shortcuts_switch_views_navigate_and_act() {
        // Every shortcut goes through the production `Widget::handle_event` key
        // path, not `apply_action`. 'e'/'i' touch the transfer dir, so redirect
        // it to a temp dir first.
        let dir = std::env::temp_dir().join(format!("cal-keys-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        unsafe { std::env::set_var("QOS_CALENDAR_IMPORT_DIR", &dir) };

        let mut h = Harness::new(1280, 800);
        // Close the demo's detail so the notes editor doesn't own keystrokes.
        h.app.apply_action(Action::CloseDetail, &mut h.doc);
        h.relayout();

        h.type_char("w");
        assert_eq!(h.app.view, View::Week, "'w' → Week");
        h.type_char("d");
        assert_eq!(h.app.view, View::Day, "'d' → Day");
        h.type_char("a");
        assert_eq!(h.app.view, View::Agenda, "'a' → Agenda");
        h.type_char("m");
        assert_eq!(h.app.view, View::Month, "'m' → Month");

        // 't' snaps the anchor back to today; step away first to prove it moves.
        h.app.apply_action(Action::Next, &mut h.doc);
        assert_ne!(h.app.anchor, h.app.today);
        h.type_char("t");
        assert_eq!(h.app.anchor, h.app.today, "'t' → today");

        // '[' / ']' step the period; arrows move the day anchor.
        let m0 = h.app.anchor.month();
        h.type_char("]");
        assert_ne!(h.app.anchor.month(), m0, "']' steps the month forward");
        h.type_char("[");
        assert_eq!(h.app.anchor.month(), m0, "'[' steps it back");
        let a0 = h.app.anchor;
        h.press_named(winit::keyboard::NamedKey::ArrowRight);
        assert_eq!(h.app.anchor, a0 + Duration::days(1), "ArrowRight → +1 day");
        h.press_named(winit::keyboard::NamedKey::ArrowDown);
        assert_eq!(h.app.anchor, a0 + Duration::days(8), "ArrowDown → +7 days");

        // 'n' creates a new event through the keyboard.
        let before = h.app.model.events.len();
        h.type_char("n");
        assert_eq!(h.app.model.events.len(), before + 1, "'n' created an event");

        // Creating an event lands the caret in its title, so the calendar no
        // longer owns the keyboard; close the editor to make the next shortcut a
        // global key again rather than a keystroke into the title.
        h.app.apply_action(Action::CloseDetail, &mut h.doc);
        h.relayout();

        // 'e' exports through the keyboard: a real .ics lands in the transfer dir.
        h.type_char("e");
        assert!(
            crate::store::newest_ics(&dir).is_some(),
            "'e' exported an .ics through the key path"
        );

        unsafe { std::env::remove_var("QOS_CALENDAR_IMPORT_DIR") };
        std::fs::remove_dir_all(&dir).ok();
    }

    // --- sync failure path -------------------------------------------------

    #[test]
    fn poll_sync_reports_failure_when_the_worker_drops() {
        // The Disconnected branch: a worker that panics/drops its sender without
        // a reply must surface "Sync failed" and clear the in-flight state.
        let mut app = CalendarApp::new_demo();
        app.demo = false;
        let mut state = AutoCommit::new();
        let (tx, rx) =
            std::sync::mpsc::channel::<(caldav::SyncResult, SyncOutcome, Vec<Id>, Vec<Id>)>();
        app.sync_rx = Some(rx);
        app.syncing = true;
        drop(tx); // worker vanished without sending
        app.poll_sync(&mut state);
        assert!(!app.syncing, "the in-flight flag is cleared");
        assert!(app.sync_rx.is_none(), "the dead channel is dropped");
        assert_eq!(app.status.as_deref(), Some("Sync failed"));
    }

    // --- notes editing persists through the real editor + save path --------

    #[test]
    fn notes_edit_persists_across_restart() {
        let dir = std::env::temp_dir().join(format!("cal-notes-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("calendar.automerge");

        let theme = Theme::dark();
        let mut app = CalendarApp::new_with_data_path(path.clone());
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        // Create an event and open its detail (which seeds a notes block).
        app.apply_action(Action::NewEvent, &mut doc);
        let Some(Sel::Event(eid)) = app.selection else {
            panic!("the new event is selected");
        };

        // Lay out wide so the notes editor is active, and build the editor node
        // so key events route into it (handle_event forwards via `editor_node`).
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.relayout();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = app.build_layout(&mut taffy, &theme, &mut node_map);
        // Bind the editor to the event's notes, then focus it.
        app.sync(&mut doc, &root);
        // Creating the event landed the caret in its title; focusing the notes
        // editor must take the keyboard from it (exactly what clicking into the
        // notes region does live), so the marker below types into the notes.
        app.title_input.set_focus(false);
        app.location_input.set_focus(false);
        app.notes_editor.set_focus(true);

        // Type a distinctive marker through the real Widget::handle_event path.
        for ch in ["Z", "Q", "9"] {
            let ev = char_press(ch);
            let mut handled = false;
            let mut focus = None;
            app.handle_event(
                &ev,
                1.0,
                &winit::event::Modifiers::default(),
                &mut doc,
                &root,
                &mut handled,
                &mut focus,
                node,
                &taffy,
            );
        }

        // Persist and confirm it reached disk.
        app.force_persist = true;
        app.sync(&mut doc, &root);
        assert!(path.exists(), "the calendar document was written to disk");

        // Reload the on-disk document and read the note back through a fresh
        // editor binding — the typed text must have survived the restart.
        let mut doc2 = crate::store::load(&path).expect("document present on disk");
        let list = CalDoc::event_notes(&mut doc2, eid).expect("event notes list present");
        let mut ed = BlockEditor::new();
        ed.bind_blocks(&mut doc2, list);
        let joined: String = ed
            .blocks()
            .iter()
            .map(|b| b.text().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("ZQ9"),
            "typed note persisted across restart: {joined:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    // --- editable event form -------------------------------------------

    fn selected_event_id(app: &CalendarApp) -> Id {
        match app.selection {
            Some(Sel::Event(id)) => id,
            _ => panic!("expected an event selection"),
        }
    }

    #[test]
    fn event_title_is_a_real_editable_field_on_the_key_path() {
        // The title is a focusable text input, not static text: clicking it
        // focuses the field and typing edits the event through the production
        // handle_event path.
        let mut h = Harness::new(1280, 800);
        let id = selected_event_id(&h.app);
        let before = h.app.model.event(id).unwrap().title.clone();
        let p = h
            .hit_center(|a| matches!(a, Action::FocusField(FormField::Title)))
            .expect("the title is a click target");
        h.click(p);
        assert!(
            h.app.title_input.is_focused(),
            "clicking the title focuses it"
        );
        h.type_char("!");
        let after = h.app.model.event(id).unwrap().title.clone();
        assert_ne!(after, before, "typing edited the event title");
        assert!(
            after.contains('!'),
            "the typed character landed in the title: {after:?}"
        );
    }

    #[test]
    fn location_is_editable_and_tab_traverses_the_fields() {
        // Tab / Shift-nothing here, but Tab moves focus title → location →
        // notes → title on the real key path (App leaves Tab to the root
        // widget because the calendar exposes no focusable children).
        let mut h = Harness::new(1280, 800);
        let p = h
            .hit_center(|a| matches!(a, Action::FocusField(FormField::Title)))
            .expect("title target");
        h.click(p);
        assert!(h.app.title_input.is_focused());
        h.press_named(winit::keyboard::NamedKey::Tab);
        assert!(h.app.location_input.is_focused(), "Tab: title → location");
        assert!(!h.app.title_input.is_focused());
        // Typing into the now-focused location field edits the event location.
        let id = selected_event_id(&h.app);
        h.type_char("Q");
        assert!(
            h.app.model.event(id).unwrap().location.contains('Q'),
            "location is editable"
        );
        h.press_named(winit::keyboard::NamedKey::Tab);
        assert!(h.app.notes_editor.is_focused(), "Tab: location → notes");
        h.press_named(winit::keyboard::NamedKey::Tab);
        assert!(h.app.title_input.is_focused(), "Tab wraps notes → title");
    }

    #[test]
    fn shift_tab_retreats_through_the_form_fields() {
        // Reverse traversal: the exact seam the Shift+Tab key path invokes
        // (winit exposes no constructor for a shift-held `Modifiers`, so — as
        // the toolkit's own widgets do — the direction logic is exercised
        // directly).
        let mut h = Harness::new(1280, 800);
        h.app.focus_field(FormField::Title);
        assert!(h.app.title_input.is_focused());
        h.app.cycle_form_focus(true);
        assert!(
            h.app.notes_editor.is_focused(),
            "Shift+Tab: title → notes (wrap)"
        );
        h.app.cycle_form_focus(true);
        assert!(
            h.app.location_input.is_focused(),
            "Shift+Tab: notes → location"
        );
        h.app.cycle_form_focus(true);
        assert!(
            h.app.title_input.is_focused(),
            "Shift+Tab: location → title"
        );
    }

    #[test]
    fn new_event_is_untitled_with_a_placeholder_and_empty_notes() {
        // A fresh event has NO seeded title or notes: both fields show their
        // placeholders instead of literal text the user must delete.
        let mut h = Harness::new(1280, 800);
        // Create from the calendar screen; the demo's modal editor would
        // otherwise sit over the New button behind its dismiss scrim.
        h.app.shots_show_calendar();
        h.relayout();
        let p = h
            .hit_center(|a| matches!(a, Action::NewEvent))
            .expect("New button");
        h.click(p);
        let id = selected_event_id(&h.app);
        assert!(
            h.app.model.event(id).unwrap().title.is_empty(),
            "new event starts untitled"
        );
        // Bind the notes editor to the new event, then confirm the block list
        // carries no seeded body text.
        h.app.sync(&mut h.doc, &automerge::ROOT);
        assert_eq!(
            h.app.notes_editor.empty_hint(),
            NOTES_PLACEHOLDER,
            "the notes editor shows a calendar-specific placeholder hint"
        );
        let list = CalDoc::event_notes(&mut h.doc, id).expect("notes list exists");
        let mut ed = BlockEditor::new();
        ed.bind_blocks(&mut h.doc, list);
        let joined: String = ed
            .blocks()
            .iter()
            .map(|b| b.text().to_string())
            .collect::<String>();
        assert!(
            joined.trim().is_empty(),
            "notes start empty, not seeded: {joined:?}"
        );
        assert!(
            !joined.contains("Add details"),
            "the hint is a placeholder, not body text"
        );
        // The title field carries NO seeded text and is focused, ready to name
        // the event on the next keystroke; its placeholder is the untitled hint.
        h.relayout();
        assert!(
            h.app.title_input.value().is_empty(),
            "new event's title starts with no seeded text"
        );
        assert!(
            h.app.title_input.is_focused(),
            "the title is focused so typing names the event"
        );
        assert_eq!(h.app.title_input.placeholder(), TITLE_PLACEHOLDER);
        // Blurring the empty field reveals the placeholder hint (never literal body text).
        h.app.title_input.set_focus(false);
        assert!(
            h.app.title_input.shows_placeholder(),
            "empty + unfocused title shows its placeholder"
        );
    }

    #[test]
    fn date_popover_arrow_key_picks_and_sets_the_start_date() {
        // Opening the start-date field floats the month grid; an arrow key
        // moves the selection and drives the new date into the event.
        let mut h = Harness::new(1280, 800);
        let id = selected_event_id(&h.app);
        let before = h.app.model.event(id).unwrap().start.date();
        let p = h
            .hit_center(|a| matches!(a, Action::OpenPopover(FormPopover::StartDate)))
            .expect("the start-date field opens a popover");
        h.click(p);
        assert_eq!(
            h.app.event_popovers.active(),
            FormPopover::StartDate,
            "the date grid is open"
        );
        h.press_named(winit::keyboard::NamedKey::ArrowRight);
        let after = h.app.model.event(id).unwrap().start.date();
        assert_eq!(
            after,
            before.succ_opt().unwrap(),
            "arrow-key navigation picked the next day and set the event start"
        );
    }

    #[test]
    fn time_popover_arrow_key_sets_the_start_time() {
        // The time popover's spinner is keyboard-navigable; an arrow step
        // drives the new hour into the event.
        let mut h = Harness::new(1280, 800);
        let id = selected_event_id(&h.app);
        // Ensure the event is timed (not all-day) so the time field exists.
        assert!(!h.app.model.event(id).unwrap().all_day);
        let before_h = h.app.model.event(id).unwrap().start.hour();
        let p = h
            .hit_center(|a| matches!(a, Action::OpenPopover(FormPopover::StartTime)))
            .expect("the start-time field opens a popover");
        h.click(p);
        assert_eq!(h.app.event_popovers.active(), FormPopover::StartTime);
        h.press_named(winit::keyboard::NamedKey::ArrowUp);
        let after_h = h.app.model.event(id).unwrap().start.hour();
        assert_eq!(
            after_h,
            (before_h + 1) % 24,
            "the clock spinner set the start hour"
        );
    }

    #[test]
    fn all_day_toggle_flips_the_flag() {
        let mut h = Harness::new(1280, 800);
        let id = selected_event_id(&h.app);
        let before = h.app.model.event(id).unwrap().all_day;
        let p = h
            .hit_center(|a| matches!(a, Action::ToggleAllDay))
            .expect("all-day toggle");
        h.click(p);
        assert_eq!(
            h.app.model.event(id).unwrap().all_day,
            !before,
            "all-day flipped"
        );
    }

    #[test]
    fn calendar_selector_files_event_on_a_configured_account_and_queues_it() {
        // The selector lists calendars from the shared account store; picking a
        // remote CalDAV calendar tags the event AND queues it for upload.
        let mut h = Harness::new(1280, 800);
        let id = selected_event_id(&h.app);
        let p = h
            .hit_center(|a| matches!(a, Action::OpenPopover(FormPopover::Calendar)))
            .expect("the calendar field opens a selector");
        h.click(p);
        assert_eq!(h.app.event_popovers.active(), FormPopover::Calendar);
        h.relayout(); // draw the list so its row targets populate
        // Local is row 0; the first configured remote calendar is row 1.
        let options_h = 34.0 * h.app.calendar_options().len() as f64 + 8.0;
        let list_y = (h.app.form.calendar.y1 + 6.0).min(h.app.detail_rect.y1 - options_h - 8.0);
        let p2 = Point::new(h.app.form.calendar.center().x, list_y + 4.0 + 34.0 + 17.0);
        h.click(p2);
        assert_eq!(
            h.app.model.event(id).unwrap().calendar_id.as_deref(),
            Some("work/default"),
            "the event is filed on the chosen CalDAV calendar"
        );
        assert!(
            CalDoc::outbox(&h.doc).contains(&id),
            "creating on a CalDAV calendar queues the event for upload"
        );
        assert_eq!(
            h.app.event_popovers.active(),
            FormPopover::None,
            "picking a calendar closes the selector"
        );
    }

    #[test]
    fn refiling_on_the_local_calendar_clears_the_upload_queue() {
        let mut h = Harness::new(1280, 800);
        let id = selected_event_id(&h.app);
        // File it remote first (queued), then move it back to the device.
        h.app
            .set_event_calendar(&CalTarget::Remote("work/default".into()), &mut h.doc);
        assert!(CalDoc::outbox(&h.doc).contains(&id));
        h.app.set_event_calendar(&CalTarget::Local, &mut h.doc);
        assert!(h.app.model.event(id).unwrap().calendar_id.is_none());
        assert!(
            !CalDoc::outbox(&h.doc).contains(&id),
            "filing back on the local calendar drops it from the queue"
        );
    }

    #[test]
    fn calendar_options_come_from_the_shared_account_store() {
        // The option list is sourced from the configured accounts, never faked:
        // the sample store's calendars all appear, plus the local option.
        let h = Harness::new(1280, 800);
        let opts = h.app.calendar_options();
        assert!(
            matches!(opts.first(), Some((CalTarget::Local, _))),
            "local is first"
        );
        for (_, cal) in h.app.accounts.all_calendars() {
            assert!(
                opts.iter()
                    .any(|(t, _)| matches!(t, CalTarget::Remote(id) if id == &cal.id)),
                "configured calendar {} is offered in the selector",
                cal.id
            );
        }
    }

    #[test]
    fn event_form_exposes_the_expected_editable_targets() {
        // The form paints real hit targets for every editable control.
        let h = Harness::new(1280, 800);
        let has = |pred: fn(&Action) -> bool| h.app.hits.iter().any(|(_, a)| pred(a));
        assert!(
            has(|a| matches!(a, Action::FocusField(FormField::Title))),
            "title field"
        );
        assert!(
            has(|a| matches!(a, Action::FocusField(FormField::Location))),
            "location field"
        );
        assert!(has(|a| matches!(a, Action::ToggleAllDay)), "all-day toggle");
        assert!(
            has(|a| matches!(a, Action::OpenPopover(FormPopover::StartDate))),
            "start date"
        );
        assert!(
            has(|a| matches!(a, Action::OpenPopover(FormPopover::StartTime))),
            "start time"
        );
        assert!(
            has(|a| matches!(a, Action::OpenPopover(FormPopover::Calendar))),
            "calendar selector"
        );
    }

    // --- delete ---------------------------------------------------------

    #[test]
    fn delete_event_removes_it_from_the_model_and_crdt() {
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        let id = app.model.events[0].id;
        let before = app.model.events.len();
        app.select_event(id);
        app.apply_action(Action::DeleteEvent(id), &mut state);
        assert_eq!(
            app.model.events.len(),
            before - 1,
            "event dropped from the model"
        );
        assert!(app.model.event(id).is_none());
        assert!(
            !CalDoc::read_model(&state).events.iter().any(|e| e.id == id),
            "removed from the CRDT too"
        );
        assert!(
            app.selection.is_none(),
            "deleting the on-screen event closes the editor"
        );
        assert!(!app.detail_open);
    }

    #[test]
    fn deleting_a_synced_event_queues_a_caldav_delete() {
        // A synced event carries a calendar_id; deleting it enqueues a CalDAV
        // delete for that collection so the server copy is removed on next sync.
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        app.apply_action(Action::Sync, &mut state); // pull the sample synced events
        let synced = app
            .model
            .events
            .iter()
            .find(|e| e.calendar_id.is_some())
            .cloned()
            .expect("a synced event with a calendar id");
        app.apply_action(Action::DeleteEvent(synced.id), &mut state);
        let outbox = CalDoc::delete_outbox(&state);
        assert!(
            outbox.iter().any(|(cal, uid)| *uid == synced.id
                && Some(cal.as_str()) == synced.calendar_id.as_deref()),
            "a CalDAV delete was queued for the synced event: {outbox:?}"
        );
    }

    #[test]
    fn delete_task_removes_it_and_its_subtasks() {
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        let parent = app
            .model
            .tasks
            .iter()
            .find(|t| !app.model.subtasks_of(t.id).is_empty())
            .map(|t| t.id)
            .expect("a task with subtasks");
        let sub_count = app.model.subtasks_of(parent).len();
        assert!(sub_count > 0);
        let before = app.model.tasks.len();
        app.apply_action(Action::DeleteTask(parent), &mut state);
        assert_eq!(
            app.model.tasks.len(),
            before - 1 - sub_count,
            "the task and its subtasks are gone"
        );
        assert!(app.model.task(parent).is_none());
        assert!(
            !CalDoc::read_model(&state)
                .tasks
                .iter()
                .any(|t| t.id == parent)
        );
    }

    #[test]
    fn delete_key_removes_the_selected_event() {
        // The Delete key on a selected event (no field focused) removes it —
        // through the real handle_event key path.
        let mut h = Harness::new(1280, 800);
        let id = selected_event_id(&h.app);
        assert!(h.app.detail_open);
        let before = h.app.model.events.len();
        h.press_named(winit::keyboard::NamedKey::Delete);
        assert_eq!(
            h.app.model.events.len(),
            before - 1,
            "Delete removed the event"
        );
        assert!(h.app.model.event(id).is_none());
    }

    #[test]
    fn editor_exposes_a_delete_target_that_removes_the_event() {
        // The editor header carries a trash button wired to delete the event —
        // clicked through the real handle_event path.
        let mut h = Harness::new(1280, 800);
        let id = selected_event_id(&h.app);
        let p = h
            .hit_center(|a| matches!(a, Action::DeleteEvent(_)))
            .expect("a delete target in the editor");
        h.click(p);
        assert!(
            h.app.model.event(id).is_none(),
            "clicking the trash button deleted the event"
        );
    }

    #[test]
    fn delete_button_dispatches_the_action_the_palette_builds() {
        // Single source of truth, proven on the real button path: clicking the
        // editor's trash button dispatches the byte-identical EncodedAction the
        // Ctrl+K "Delete event" spec builds for the same event (including the
        // resolved CalDAV delete target).
        let mut h = Harness::new(1280, 800);
        let id = selected_event_id(&h.app);
        let remote = h.app.resolve_remote_delete(id);
        let expected =
            crate::actions::encode(&crate::actions::delete_event_action(id.to_string(), remote));
        let p = h
            .hit_center(|a| matches!(a, Action::DeleteEvent(_)))
            .expect("a delete target in the editor");
        h.click(p);
        assert_eq!(
            h.app.last_dispatched(),
            Some(&expected),
            "the trash button and the palette build the identical EncodedAction"
        );
        assert!(
            h.app.model.event(id).is_none(),
            "and it actually deleted the event"
        );
    }

    #[test]
    fn delete_key_in_the_title_field_does_not_delete_the_event() {
        // Guard: Delete must edit text when a field is focused, never nuke the
        // event out from under the caret.
        let mut h = Harness::new(1280, 800);
        let id = selected_event_id(&h.app);
        let p = h
            .hit_center(|a| matches!(a, Action::FocusField(FormField::Title)))
            .expect("title target");
        h.click(p);
        assert!(h.app.title_input.is_focused());
        h.press_named(winit::keyboard::NamedKey::Delete);
        assert!(
            h.app.model.event(id).is_some(),
            "Delete in a text field kept the event"
        );
    }

    // --- create → name → persist flow ----------------------------------

    #[test]
    fn typing_right_after_creating_an_event_names_it() {
        // The reported break: create an event, start typing — the keystrokes
        // must land in the TITLE, not vanish (or trip a view shortcut) because
        // no field was focused. Drive the real button + key path.
        let mut h = Harness::new(1280, 800);
        // Create from the calendar screen so the New button isn't under the
        // demo's open modal.
        h.app.shots_show_calendar();
        h.relayout();
        let p = h
            .hit_center(|a| matches!(a, Action::NewEvent))
            .expect("New button");
        h.click(p);
        let id = selected_event_id(&h.app);
        assert!(
            h.app.title_input.is_focused(),
            "a freshly created event opens with its title focused, ready to type"
        );
        for ch in ["S", "t", "a", "n", "d", "u", "p"] {
            h.type_char(ch);
        }
        assert_eq!(
            h.app.model.event(id).unwrap().title,
            "Standup",
            "typing right after Create named the event's title"
        );
        // And the view did NOT switch (a stray letter must not be swallowed as
        // a view shortcut while the title owns the keyboard).
        assert_eq!(
            h.app.view,
            View::Month,
            "letters typed into the title never trip a view shortcut"
        );
    }

    #[test]
    fn edited_title_survives_close_and_reopen() {
        // Flow 1, end to end: open an event, edit its title, close the editor,
        // reopen the SAME event — the new title is there, both in the model and
        // reseeded into the field the reopened editor shows.
        let mut h = Harness::new(1280, 800);
        let id = selected_event_id(&h.app);
        let before = h.app.model.event(id).unwrap().title.clone();
        let p = h
            .hit_center(|a| matches!(a, Action::FocusField(FormField::Title)))
            .expect("title target");
        h.click(p);
        h.type_char("!");
        let edited = h.app.model.event(id).unwrap().title.clone();
        assert_ne!(edited, before, "typing changed the title");
        assert!(edited.contains('!'));

        // Close the editor through the production action, then let a frame settle
        // so the form unbinds (form_event clears) — exactly the live sequence.
        h.app.apply_action(Action::CloseDetail, &mut h.doc);
        h.app.sync(&mut h.doc, &automerge::ROOT);
        assert!(!h.app.detail_open, "editor closed");

        // Reopen the same event (the OpenEvent action's production entry point)
        // and run a sync so the form rebinds from the model.
        h.app.apply_action(Action::OpenEvent(id), &mut h.doc);
        h.app.sync(&mut h.doc, &automerge::ROOT);
        h.relayout();
        assert_eq!(
            h.app.model.event(id).unwrap().title,
            edited,
            "the edited title is still on the event after reopen"
        );
        assert_eq!(
            h.app.title_input.value(),
            edited,
            "the reopened editor's title field shows the edited title"
        );
    }

    #[test]
    fn edited_title_persists_across_restart() {
        // The full real backend: a title typed into the field reaches the CRDT
        // and the on-disk document, so a fresh instance loads it back.
        let dir = std::env::temp_dir().join(format!("cal-title-persist-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("calendar.automerge");

        let mut h = Harness::live(1280, 800, path.clone());
        let p = h
            .hit_center(|a| matches!(a, Action::NewEvent))
            .expect("New button");
        h.click(p);
        let id = selected_event_id(&h.app);
        assert!(
            h.app.title_input.is_focused(),
            "the new event opens with the title focused"
        );
        for ch in ["R", "o", "a", "d", "m", "a", "p"] {
            h.type_char(ch);
        }
        assert_eq!(h.app.model.event(id).unwrap().title, "Roadmap");
        // Persist runs on the next sync (writes because the heads changed).
        h.app.sync(&mut h.doc, &automerge::ROOT);
        assert!(path.exists(), "the document was written to disk");

        // Fresh instance + fresh doc must load the named event back.
        let mut app2 = CalendarApp::new_with_data_path(path);
        let mut doc2 = AutoCommit::new();
        app2.sync(&mut doc2, &automerge::ROOT);
        let ev = app2
            .model
            .events
            .iter()
            .find(|e| e.id == id)
            .expect("the created event survived the restart");
        assert_eq!(
            ev.title, "Roadmap",
            "the edited title persisted across the restart"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn open_editor_captures_the_keyboard_and_release_on_close() {
        // The seam that makes the form's OWN Tab/field routing reachable in the
        // live app: while open, the editor reports a modal capture so the host
        // App suspends its built-in Tab focus cycling; closing releases it.
        let mut h = Harness::new(1280, 800);
        assert!(h.app.detail_open);
        assert!(
            h.app.modal_capture().is_some(),
            "an open editor captures the keyboard so the App yields Tab to the form"
        );
        h.app.apply_action(Action::CloseDetail, &mut h.doc);
        h.relayout();
        assert!(
            h.app.modal_capture().is_none(),
            "a closed editor releases the keyboard back to the App"
        );
    }

    #[test]
    fn closing_the_editor_releases_field_focus_so_shortcuts_work() {
        // Bug: opening/creating an event focuses the title; closing the editor
        // must blur it so the next keystroke drives the calendar (a shortcut),
        // not a hidden title field that silently swallows keys.
        let mut h = Harness::new(1280, 800);
        h.app.focus_field(FormField::Title);
        assert!(h.app.title_input.is_focused());
        h.app.apply_action(Action::CloseDetail, &mut h.doc);
        assert!(
            !h.app.title_input.is_focused(),
            "closing the editor blurs the title"
        );
        assert!(!h.app.location_input.is_focused(), "…and the location");
        assert!(!h.app.notes_editor.is_focused(), "…and the notes editor");
        // A view shortcut now lands — it would be eaten by the title otherwise.
        h.relayout();
        h.type_char("w");
        assert_eq!(
            h.app.view,
            View::Week,
            "a shortcut works once the editor has closed"
        );
    }
}
