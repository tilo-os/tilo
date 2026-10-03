//! `CalendarApp` — the root widget. Owns the in-memory [`Model`], the
//! current view + selection, and a single SDK rich-text editor
//! ([`RichTextSurface`]) for the **notes** attached to the selected
//! event/task.
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

/// A user-named text file (an `.ics` in the transfer folder), whole.
fn read_text(path: &std::path::Path) -> Result<String, tilo_files::FsError> {
    let (dir, name) = tilo_files::file_at(path)?;
    let bytes = tilo_files::blocking::read(&dir, &name)?;
    String::from_utf8(bytes)
        .map_err(|_| tilo_files::FsError::Io("the file is not UTF-8 text".into()))
}

#[cfg(test)]
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use tilo_automerge::ApplyIncoming;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ChangeHash, ObjId};
use chrono::{Duration, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use taffy::style_helpers::length;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use uuid::Uuid;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::Widget;
use ui::a11y::{AccessNode, AccessState, Action as A11yAction, Role};
use ui::paint::PaintCtx;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::modal_dialog::ModalLayout;
use ui::widget::{DatePicker, Popover, TextInput};

use tilo_rich_text::{RichTextField, RichTextSurface};

use crate::account::AccountStore;
use crate::agenda::{AgendaAction, AgendaSource, AgendaView};
use crate::caldav::{self, SyncOutcome};
use crate::date;
use crate::doc::CalDoc;
use crate::event_popovers::{EventPopovers, PopoverEdit, PopoverKind as FormPopover};
use crate::ics;
use crate::input_context::InputContext;
use crate::model::{Event, Id, ListSel, Model, Priority, Recurrence, View, seed_demo};
use crate::sidebar::{Mode as SidebarMode, Sidebar};
use crate::timeline_interaction::{
    TIMELINE_SNAP_MIN, TimelineEffect, TimelineInteraction, TimelineRenderEvent,
    TimelineRenderModel, TimelineSpan,
};
#[cfg(test)]
use crate::timeline_viewport::TIMELINE_MIN_HOUR_H;
use crate::topbar::{Action as TopbarAction, Topbar};

mod editor;
mod month_view;
mod sidebar_view;
mod status;

use editor::{Choices, FormRects};

/// Placeholder shown in the empty title field of a new event.
const TITLE_PLACEHOLDER: &str = "New event";
/// Placeholder shown in the empty notes editor of an event (dim hint text that
/// disappears the moment the user types — never seeded as real body text).
const NOTES_PLACEHOLDER: &str = "Add notes";

/// Compact, derived stable-id positions over Calendar's authoritative model.
///
/// This deliberately contains identities and positions only: unlike the old
/// retained resource context it never duplicates `EventData`/`TaskData` for
/// the whole document. Exact resource requests validate the position against
/// the live model before materialising the one addressed record.
#[derive(Default)]
struct CalendarResourceIndex {
    events: HashMap<String, usize>,
    tasks: HashMap<String, usize>,
    projects: HashMap<String, usize>,
    calendars: HashMap<String, usize>,
    subtasks: HashMap<String, BoundedSubtasks>,
    projects_in_use: HashSet<String>,
}

#[derive(Default)]
struct BoundedSubtasks {
    ids: Vec<String>,
    overflow: usize,
}

/// The empty state of a calendar with no events: one sentence and the
/// action that fills it.
const EMPTY_ONBOARDING_TITLE: &str = "No events yet";
const EMPTY_ONBOARDING_HINT: &str = "Select New event to add your first one.";

/// The hour a working day starts at: the week and day views open on it.
const WORKING_DAY_START: u8 = 7;

/// Navigation intent of the sidebar's month navigator. The app applies it to
/// the anchor shared by every calendar view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MiniMonthOutcome {
    SelectDate(NaiveDate),
    ShiftMonth(i32),
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
    ToggleSelectedTask,
    OpenTaskPopover(TaskPopover),
    DismissTaskPopover,
    SetTaskDue(Option<NaiveDate>),
    ShiftTaskDueMonth(i32),
    /// Select a task to view its detail (without toggling completion).
    OpenTask(Id),
    /// Show or hide the sidebar: the docked column when the window is wide,
    /// the floating drawer when it is narrow.
    ToggleSidebar,
    /// Dismiss the drawer by pressing the scrim behind it.
    DismissSidebar,
    /// Open the sidebar's overflow menu (export, import).
    OpenSidebarMenu,
    /// Open the platform search, narrowed to this app.
    OpenSearch,
    /// Set whether a synced remote calendar's events are shown. This carries
    /// the resulting absolute value, never a replay-unsafe toggle.
    SetCalendarVisible {
        id: String,
        visible: bool,
    },
    /// Collapse/expand one account's calendar list in the sidebar.
    ToggleAccountCollapsed(String),
    /// Pull events from the configured `CalDAV` calendars.
    Sync,
    /// Flip the selected event's all-day flag.
    ToggleAllDay,
    /// Open one of the form's date or time pickers over the edited event.
    OpenPopover(FormPopover),
    /// Focus one of the form's text fields (used by the title/location field
    /// click targets so a press lands the caret).
    FocusField(FormField),
    /// Delete an event from the model + CRDT (and queue a `CalDAV` delete for a
    /// synced event). Fired by the editor's trash button and the Delete key.
    DeleteEvent(Id),
    /// Delete a task (and its subtasks) from the model + CRDT.
    DeleteTask(Id),
    /// Close the status toast.
    DismissStatus,
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
    Invite,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TaskPopover {
    Due,
}

/// A calendar an event can be filed under: the on-device local calendar, or a
/// specific remote `CalDAV` collection (identified by its stable id, matching
/// [`Event::calendar_id`]).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CalTarget {
    Local,
    Remote(String),
}

/// Payload delivered by a finished background `CalDAV` sync worker: the pull
/// result, the merged outcome, the ids that uploaded successfully, and the
/// ids the server confirmed deleted.
/// The notes of the selected event or task, edited as calendar actions.
type NotesEditor = RichTextSurface<RichTextField<crate::actions::CalendarAction>>;

type SyncWorkerResult = (
    caldav::SyncResult,
    SyncOutcome,
    Vec<Id>,
    Vec<Id>,
    Vec<String>,
    Vec<caldav::RemoteRevision>,
);

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent, unrelated flag (demo mode, sidebar state, sync status, form scratch state); grouping them into enums would not express any real invariant between them and would touch every read/write site across this file"
)]
pub struct CalendarApp {
    model: Model,
    /// Plain-text projection of each rich note for global search. The CRDT
    /// remains canonical; this is a refreshed, local indexing cache only.
    note_search: HashMap<String, String>,
    /// Complete durable search namespace retained across palette openings.
    /// It is reconciled after each observed document revision, rather than
    /// rebuilding a private corpus while a user is typing.
    search_provider: crate::search_provider::CalendarSearchProvider,
    search_catalog_heads: Vec<ChangeHash>,
    /// Monotonic generation of the non-CRDT calendar collection projection.
    /// Account configuration belongs to Settings, so collection names,
    /// identities, colours, and visibility can change without advancing the
    /// Automerge heads that protect events and notes.
    calendar_projection_revision: u64,
    /// Last observed collection projection. Comparing this small configuration
    /// snapshot on sync catches account-state replacements as well as
    /// Calendar's own visibility toggles, without walking the event/task note
    /// corpus.
    calendar_projection_snapshot: Vec<crate::actions::CalendarCollectionResource>,
    /// Projection generation included in the retained catalogue currently
    /// published to Ctrl+K.
    search_catalog_projection_revision: u64,
    /// Compact position indices over the authoritative model. Rebuilt when a
    /// document/account projection changes; this is intentionally not a
    /// second event/task resource corpus.
    resource_index: CalendarResourceIndex,
    #[cfg(test)]
    note_projection_refreshes: usize,
    #[cfg(test)]
    search_catalog_refreshes: usize,
    #[cfg(test)]
    pub(crate) resource_snapshot_inspected: Cell<usize>,
    #[cfg(test)]
    pub(crate) resource_exact_inspected: Cell<usize>,
    today: NaiveDate,
    anchor: NaiveDate,
    view: View,
    /// `None` shows every visible collection; a stable local/remote calendar
    /// id is set only by the collection resource's canonical `open` action.
    calendar_filter: Option<String>,
    list_sel: ListSel,
    selection: Option<Sel>,
    detail_open: bool,

    /// `CalDAV` accounts + their calendars, as configured by OS Settings (the
    /// shared [`AccountStore`]). Read-only here; Settings owns the lifecycle.
    accounts: AccountStore,

    notes_editor: NotesEditor,
    bound: Option<Sel>,
    seeded: bool,

    // --- event form (live only while an Event detail is open) -----------
    /// Editable title field (local text, mirrored into the event on change).
    title_input: TextInput,
    /// Editable location field.
    location_input: TextInput,
    invite_input: TextInput,
    /// Which event the form widgets are currently seeded from — so a new
    /// selection reseeds the inputs and edits land on the right event.
    form_event: Option<Id>,
    /// Self-contained date/time picker controller: exclusive open state,
    /// embedded widgets, drawing, hit-testing and event routing.
    event_popovers: EventPopovers,
    task_popover: Option<TaskPopover>,
    /// Shared date-grid widget embedded in the task due-date popover.
    task_due_picker: DatePicker,
    /// The editor's dropdowns (calendar, repeat, alert, time zone, colour;
    /// a task's priority and project).
    choices: Choices,
    /// Cached form field geometry (see [`FormRects`]).
    form: FormRects,
    /// The modal sheet the editor is presented in, laid out against the
    /// window; `None` while no editor is open.
    modal: Option<ModalLayout>,
    /// Height of the notes editor's content at its last layout, so the sheet
    /// grows with the notes.
    notes_content_h: f64,
    /// The sidebar's overflow menu (export, import).
    sidebar_menu: Popover,
    /// Opens the platform search from the header's search button.
    search_opener: ui::search::SearchOpener,
    /// The month navigator at the bottom of the sidebar.
    mini_month: DatePicker,
    /// Shots-only: spawn a fresh event on the first sync so the offscreen
    /// renderer can capture the New-Event form. No effect on live runtime.
    shots_pending_new_event: bool,
    /// A deterministic visual form state should not include a transient toast
    /// that only exists after an interactive creation action.
    shots_suppress_status: bool,
    /// Shots-only: open this popover once the form is bound.
    shots_pending_popover: Option<FormPopover>,
    /// Shots-only: open the calendar dropdown once the form is bound.
    shots_pending_calendar_menu: bool,
    /// Shots-only: open the sidebar's overflow menu once it is laid out.
    shots_pending_sidebar_menu: bool,

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
    last_persist: ui::time::Instant,

    // --- responsive state ------------------------------------------------
    /// Responsive drawer state, geometry and chrome interaction.
    sidebar: Sidebar,
    /// Responsive navigation chrome and its pointer hit map.
    topbar: Topbar,
    /// The theme of the last layout pass, so the renderer-less layout reads
    /// the same tokens and text roles the painters do.
    layout_theme: Theme,
    // --- layout caches (filled by `relayout`) ----------------------------
    rect: Rect,
    /// The region right of a docked sidebar (or the whole window under a
    /// drawer) holding the header and the calendar view.
    content_rect: Rect,
    /// The editor sheet's card while it is open.
    detail_rect: Rect,
    notes_origin: Point,
    notes_w: f64,
    detail_scroll_y: f64,
    detail_scroll_max: f64,
    shots_detail_scroll_to_end: bool,
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
    /// The status toast's close button as last painted.
    status_close: Rect,
    /// A just-created blank event is a provisional draft until the user edits
    /// any event field or adds notes. Back/Escape may discard *only* this exact
    /// pristine object; every other event follows normal autosave semantics.
    fresh_draft: Option<Event>,
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
    resource_sync_invocation: Option<tilo_actions::InvocationId>,
    resource_sync_result: Option<tilo_actions::InvocationUpdate>,
    /// A resource import is parsed on explicit invocation, then dispatched on
    /// the next mutable document sync. Resource-operation handlers receive an
    /// immutable snapshot, so this bridge prevents a false successful import
    /// against a throwaway clone.
    pending_resource_import: Option<crate::actions::CalendarAction>,
    /// Device-private launch input, never published in resource metadata.
    pending_launch_ics: Option<String>,
    launch_import_emitted: bool,
    resource_import_invocation: Option<tilo_actions::InvocationId>,
    resource_import_result: Option<tilo_actions::InvocationUpdate>,

    /// Framework-owned action dispatch. Hosted via
    /// [`crate::resource::ActionSink`]; standalone tests use the local host.
    actions: crate::resource::ActionSink,

    /// The exact encoded non-document operation an on-screen Calendar control
    /// sent through the resource-operation boundary. This is test-only because
    /// effect/view operation history is owned by the invocation host, rather
    /// than by Calendar's shared-document [`ActionHost`](ui::ActionHost).
    #[cfg(test)]
    last_local_resource_operation: Option<tilo_actions::EncodedAction>,

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
        // The launcher's shared store owns loading, autosave and the exit flush.
        // A second widget-owned writer would keep importing stale legacy data
        // and bypass the host's persistence policy.
        Self::build(false, None)
    }

    /// Stage a local ICS launch target without changing the calendar. The host
    /// admits a zero-argument effect after loading the current document.
    ///
    /// # Errors
    /// Rejects non-local URLs, duplicate pending imports, unreadable/non-UTF-8
    /// input, and files exceeding 8 MiB.
    pub fn queue_launch_ics(&mut self, target: &str) -> Result<(), String> {
        const LIMIT: u64 = 8 * 1024 * 1024;
        if self.pending_launch_ics.is_some() {
            return Err("A calendar launch import is already pending".into());
        }
        let path = if target.to_ascii_lowercase().starts_with("file:") {
            let url =
                url::Url::parse(target).map_err(|error| format!("Invalid file URL: {error}"))?;
            if url
                .host_str()
                .is_some_and(|host| !host.is_empty() && host != "localhost")
            {
                return Err("Calendar launch requires a local file URL".into());
            }
            PathBuf::from(ui::uri::decode(url.path()))
        } else if target.contains("://") {
            return Err("Calendar launch requires a local file".into());
        } else {
            PathBuf::from(target)
        };
        let (dir, name) =
            tilo_files::file_at(&path).map_err(|error| format!("Import failed: {error}"))?;
        let meta = tilo_files::blocking::metadata(&dir, &name)
            .map_err(|error| format!("Import failed: {error}"))?;
        if meta.kind != tilo_files::Kind::File {
            return Err("Calendar launch requires a regular file".into());
        }
        if meta.len > LIMIT {
            return Err("Calendar import exceeds 8 MiB".into());
        }
        let bytes = tilo_files::blocking::read(&dir, &name).map_err(|error| error.to_string())?;
        if bytes.len() as u64 > LIMIT {
            return Err("Calendar import exceeds 8 MiB".into());
        }
        let payload =
            String::from_utf8(bytes).map_err(|_| "Calendar import must be UTF-8".to_string())?;
        let (events, tasks) = ics::parse(&payload);
        if events.is_empty() && tasks.is_empty() {
            return Err("The file contains no supported calendar items".into());
        }
        self.pending_launch_ics = Some(payload);
        self.launch_import_emitted = false;
        self.dirty = true;
        Ok(())
    }

    fn pending_launch_resource(&self) -> Option<tilo_actions::Resource> {
        if !self.seeded || self.pending_launch_ics.is_none() {
            return None;
        }
        let action = tilo_actions::ActionSpec::operation(
            "finalize",
            "Import calendar file",
            tilo_actions::OperationKind::Effect,
            crate::actions::CALENDAR_OPERATION_TYPE,
            &crate::actions::CalendarOperation::FinalizeLaunchImport,
        )
        .ok()?
        .retry_policy(tilo_actions::RetryPolicy::AtMostOnce);
        Some(
            tilo_actions::Resource::new(
                "calendar.pending-import",
                "calendar.import",
                "Import calendar file",
            )
            .with_action(action),
        )
    }

    /// The hermetic demo used by `--shots` and tests: seeded with sample
    /// content, in-memory only (never touches disk).
    pub fn new_demo() -> Self {
        Self::build(true, None)
    }

    /// A hermetic first-run instance used only by the offscreen visual suite.
    /// Unlike [`Self::new`], it deliberately does not open the user's real
    /// saved calendar: a “No events yet” shot must prove empty-state UX rather
    /// than accidentally rendering whatever a previous local run persisted.
    pub fn new_empty_for_shots() -> Self {
        Self::build(false, None)
    }

    #[cfg(test)]
    fn new_with_data_path(path: PathBuf) -> Self {
        Self::build(false, Some(path))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the constructor deliberately keeps every per-widget local state default adjacent to its field"
    )]
    fn build(demo: bool, data_path: Option<PathBuf>) -> Self {
        let today = ui::clock::today();

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
        let search_provider = crate::search_provider::CalendarSearchProvider::new(&model);

        let mut app = Self {
            model,
            note_search: HashMap::new(),
            search_provider,
            search_catalog_heads: Vec::new(),
            calendar_projection_revision: 0,
            calendar_projection_snapshot: Vec::new(),
            search_catalog_projection_revision: 0,
            resource_index: CalendarResourceIndex::default(),
            #[cfg(test)]
            note_projection_refreshes: 0,
            #[cfg(test)]
            search_catalog_refreshes: 0,
            #[cfg(test)]
            resource_snapshot_inspected: Cell::new(0),
            #[cfg(test)]
            resource_exact_inspected: Cell::new(0),
            today,
            anchor: today,
            view: View::Month,
            calendar_filter: None,
            list_sel: ListSel::Today,
            selection,
            detail_open,
            accounts,
            notes_editor: Self::new_notes_editor(None),
            bound: None,
            seeded: false,
            title_input: TextInput::local()
                .with_placeholder(TITLE_PLACEHOLDER)
                .with_text_role(ui::tokens::TextRole::Heading)
                .with_persistent_placeholder()
                .with_id("event_title"),
            location_input: TextInput::local()
                .with_placeholder("Add location")
                .with_id("event_location"),
            invite_input: TextInput::local()
                .with_placeholder("Invite by email")
                .with_id("event_invite"),
            form_event: None,
            event_popovers: EventPopovers::new(today),
            task_popover: None,
            task_due_picker: DatePicker::new(today).with_accessible_name("Task due date"),
            choices: Choices::new(),
            form: FormRects::default(),
            modal: None,
            notes_content_h: 0.0,
            sidebar_menu: Popover::new()
                .with_items(["Export calendar", "Import calendar"])
                .with_label("Calendar file"),
            mini_month: DatePicker::new(today).with_accessible_name("Month"),
            search_opener: ui::search::SearchOpener::new(),
            shots_pending_new_event: false,
            shots_suppress_status: false,
            shots_pending_popover: None,
            shots_pending_calendar_menu: false,
            shots_pending_sidebar_menu: false,
            demo,
            data_path,
            saved_heads: Vec::new(),
            force_persist: false,
            last_persist: ui::time::now(),
            sidebar: Sidebar::new(),
            topbar: Topbar::new(),
            layout_theme: Theme::dark(),
            rect: Rect::ZERO,
            content_rect: Rect::ZERO,
            detail_rect: Rect::ZERO,
            notes_origin: Point::new(-10000.0, -10000.0),
            notes_w: 0.0,
            detail_scroll_y: 0.0,
            detail_scroll_max: 0.0,
            shots_detail_scroll_to_end: false,
            editor_node: None,
            built_notes_w: -1.0,
            hits: Vec::new(),
            last_mouse: Point::ZERO,
            status: None,
            status_close: Rect::ZERO,
            fresh_draft: None,
            dirty: true,
            timeline: TimelineInteraction::new(),
            agenda: AgendaView::new(),
            sync_rx: None,
            syncing: false,
            resource_sync_invocation: None,
            resource_sync_result: None,
            pending_resource_import: None,
            pending_launch_ics: None,
            launch_import_emitted: false,
            resource_import_invocation: None,
            resource_import_result: None,
            actions: crate::resource::ActionSink::default(),
            #[cfg(test)]
            last_local_resource_operation: None,
            model_heads: Vec::new(),
        };
        app.observe_calendar_projection();
        app.rebuild_resource_index();
        app
    }

    /// Adopt the framework's live model projection.
    pub(crate) fn adopt_resource_state(&mut self, state: &std::rc::Rc<crate::model::Model>) {
        self.model = (**state).clone();
        self.dirty = true;
        self.rebuild_resource_index();
    }

    /// Attach the framework dispatcher when hosted as a surface.
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Calendar>,
    ) {
        self.actions.attach(dispatcher);
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
        self.dirty = true;
    }

    /// Open a representative task detail for deterministic visual review of
    /// the compact task editor controls.
    pub fn shots_show_task_detail(&mut self) {
        if let Some(id) = self.model.tasks.first().map(|task| task.id) {
            self.select_task(id);
        }
        self.dirty = true;
    }

    pub fn shots_open_task_picker(&mut self, picker: &str) {
        self.shots_show_task_detail();
        self.seed_choices();
        match picker {
            "priority" => self.choices.priority.dropdown.open_menu(),
            "project" => self.choices.project.dropdown.open_menu(),
            _ => {
                self.task_popover = Some(TaskPopover::Due);
                self.task_due_picker.set_selected(self.task_due_seed());
            }
        }
        self.dirty = true;
    }

    /// Open the sidebar's overflow menu for a screenshot.
    pub fn shots_open_sidebar_menu(&mut self) {
        self.shots_pending_sidebar_menu = true;
        self.dirty = true;
    }

    /// Open the sidebar drawer for a screenshot of a narrow window.
    pub fn shots_open_flap(&mut self) {
        self.sidebar.open_after_layout();
        self.dirty = true;
    }

    /// Collapse the persistent sidebar for a screenshot (wide layout only).
    pub fn shots_collapse_sidebar(&mut self) {
        self.sidebar.set_collapsed(true);
        self.dirty = true;
    }

    /// Deterministic transport-state presentation for visual regression shots.
    pub fn shots_set_sync_state(&mut self, state: &str) {
        self.syncing = state == "syncing";
        self.status = Some(match state {
            "syncing" => "Syncing calendars…".into(),
            "offline" => "Offline. Changes sync when you reconnect.".into(),
            "conflict" => {
                "1 change also changed on the server; your version is safe and still queued".into()
            }
            "error" => "Sync failed. Check the account in Settings, then sync again.".into(),
            _ => return,
        });
        self.dirty = true;
    }

    /// Add a deterministic overlapping event to the demo model for visual
    /// review of the editor's local scheduling warning. This is screenshot
    /// staging only: the live app never fabricates a conflicting booking.
    pub fn shots_show_schedule_conflict(&mut self) {
        let Some(Sel::Event(id)) = self.selection else {
            return;
        };
        let Some(edited) = self.model.event(id).cloned() else {
            return;
        };
        let mut overlapping = edited.clone();
        overlapping.id = Uuid::new_v4();
        overlapping.title = "Design review follow-up".into();
        overlapping.start = edited.start + Duration::minutes(15);
        overlapping.end = edited.end + Duration::minutes(30);
        overlapping.recurrence = Recurrence::Weekly;
        self.model.events.push(overlapping);
        self.detail_open = true;
        self.dirty = true;
    }

    /// Create a fresh event on the next sync and open its edit form — the
    /// New-Event screenshot. The event has an empty title (so the field shows
    /// its placeholder) and empty notes (so the notes editor shows its hint).
    pub fn shots_new_event(&mut self) {
        self.shots_pending_new_event = true;
        self.shots_suppress_status = true;
        self.dirty = true;
    }

    /// Open the start-date grid popover over the New-Event form for a
    /// screenshot of the date picker.
    pub fn shots_open_date_popover(&mut self) {
        self.shots_pending_new_event = true;
        self.shots_pending_popover = Some(FormPopover::StartDate);
        self.dirty = true;
    }

    /// Open the start-time spinner popover over the New-Event form for a
    /// screenshot.
    pub fn shots_open_time_popover(&mut self) {
        self.shots_pending_new_event = true;
        self.shots_pending_popover = Some(FormPopover::StartTime);
        self.dirty = true;
    }

    /// Open the calendar dropdown of a new event's form for a screenshot.
    pub fn shots_open_calendar_popover(&mut self) {
        self.shots_pending_new_event = true;
        self.shots_pending_calendar_menu = true;
        self.dirty = true;
    }

    /// Scroll the event sheet to its lower bound after layout, for a truthful
    /// visual proof that large-text content below the fold remains reachable.
    pub fn shots_scroll_detail_to_end(&mut self) {
        self.shots_detail_scroll_to_end = true;
        self.dirty = true;
    }

    /// Kick off a `CalDAV` pull from the configured calendars. The live pull
    /// touches the network, so it runs on a worker thread and its result is
    /// drained in [`Self::poll_sync`]; the UI stays responsive throughout. The
    /// hermetic demo/test build stays fully synchronous (it synthesises each
    /// calendar's events through the real [`ics::parse`] path — no network, no
    /// thread — so `--shots` and tests are deterministic).
    #[allow(
        clippy::too_many_lines,
        reason = "sync orchestration keeps worker admission and failure contracts together"
    )]
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
            self.status = Some("No calendar accounts yet. Add one in Settings.".into());
            self.dirty = true;
            return;
        }

        // Live delete + push + pull as a task on the app runtime; the result
        // lands via `poll_sync`. Remote deletions go out first (so a re-pull can't
        // resurrect a just-deleted event), then queued uploads, then the
        // collections are pulled.
        let runtime = match ui::rt::try_app() {
            Ok(runtime) => runtime,
            Err(error) => {
                self.status = Some(format!("Sync unavailable: {error}"));
                self.dirty = true;
                return;
            }
        };
        let delete_jobs = self.gather_delete_jobs(doc);
        let push_jobs = self.gather_push_jobs(doc);
        let sync_tokens = CalDoc::sync_tokens(doc);
        let (tx, rx) = std::sync::mpsc::channel();
        let today = self.today;
        for id in &sync_account_ids {
            if let Err(error) = accounts::AccountSyncReporter::new(id.clone()).started() {
                tracing::warn!(%error, account = id, "calendar: could not persist sync start");
            }
        }
        drop(runtime.spawn(async move {
            let delete_result = caldav::remove_jobs(&delete_jobs).await;
            let push_result = caldav::push_jobs(&push_jobs).await;
            let deleted = delete_result.confirmed;
            let pushed = push_result.confirmed;
            let revisions = push_result.revisions;
            let protected_hrefs: Vec<(String, String)> = push_jobs
                .iter()
                .filter(|job| !pushed.contains(&job.event.id))
                .filter_map(|job| job.href.clone().map(|href| (job.calendar.id.clone(), href)))
                .collect();
            let conflicts: Vec<String> = delete_result
                .conflicts
                .into_iter()
                .chain(push_result.conflicts)
                .map(|(_, href)| href)
                .collect();
            let mut result = caldav::SyncResult::default();
            let mut synced_calendars = 0;
            let mut first_failure = None;
            for (id, input) in sync_account_ids.iter().zip(inputs.iter()) {
                let (mut account_result, account_outcome) = caldav::sync_all_with_checkpoints(
                    std::slice::from_ref(input),
                    today,
                    &sync_tokens,
                )
                .await;
                let reporter = accounts::AccountSyncReporter::new(id.clone());
                let upload_failed = push_jobs
                    .iter()
                    .any(|job| job.config == input.0 && !pushed.contains(&job.event.id));
                let delete_failed = delete_jobs
                    .iter()
                    .any(|job| job.config == input.0 && !deleted.contains(&job.id));
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
                result.resources.append(&mut account_result.resources);
                result.deletions.append(&mut account_result.deletions);
                result.checkpoints.append(&mut account_result.checkpoints);
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
            // Do not overwrite a locally edited object after its conditional
            // upload conflicted. It stays queued; the next retry can be
            // resolved without destroying either copy.
            result.resources.retain(|resource| {
                !conflicts
                    .iter()
                    .any(|href| caldav::hrefs_equal(href, &resource.href))
                    && !protected_hrefs.iter().any(|(calendar_id, href)| {
                        calendar_id == &resource.calendar_id
                            && caldav::hrefs_equal(href, &resource.href)
                    })
            });
            // A failed/conflicted local edit wins neither side automatically.
            // Suppress an incremental tombstone for it and treat it as seen in
            // a full fallback, leaving the local copy and outbox intact for an
            // explicit retry/resolution.
            result.deletions.retain(|deletion| {
                !protected_hrefs.iter().any(|(calendar_id, href)| {
                    calendar_id == &deletion.calendar_id
                        && caldav::hrefs_equal(href, &deletion.href)
                })
            });
            for checkpoint in &mut result.checkpoints {
                if checkpoint.full {
                    checkpoint.seen_hrefs.extend(
                        protected_hrefs
                            .iter()
                            .filter(|(calendar_id, _)| calendar_id == &checkpoint.calendar_id)
                            .map(|(_, href)| href.clone()),
                    );
                }
            }
            let _ = tx.send((result, outcome, pushed, deleted, conflicts, revisions));
        }));
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
            Ok((result, mut outcome, pushed, deleted, conflicts, revisions)) => {
                self.sync_rx = None;
                self.syncing = false;
                // Clear successfully-uploaded events from the push outbox and
                // server-confirmed removals from the delete outbox.
                for revision in revisions {
                    self.record_remote_binding(
                        doc,
                        revision.id,
                        &revision.calendar_id,
                        &revision.href,
                        revision.etag.as_deref(),
                    );
                }
                for id in pushed {
                    self.perform_action(
                        doc,
                        &crate::actions::CalendarAction::AcknowledgeRemotePush {
                            id: id.to_string(),
                        },
                    );
                }
                for id in deleted {
                    self.perform_action(
                        doc,
                        &crate::actions::CalendarAction::AcknowledgeRemoteDelete {
                            id: id.to_string(),
                        },
                    );
                }
                if !conflicts.is_empty() {
                    outcome = SyncOutcome::Failed(format!(
                        "{} change{} also changed on the server; your version is safe and still queued",
                        conflicts.len(),
                        if conflicts.len() == 1 { "" } else { "s" }
                    ));
                }
                let failure = match &outcome {
                    SyncOutcome::Failed(reason) => Some(reason.clone()),
                    SyncOutcome::Offline => Some("calendar service is offline".to_string()),
                    SyncOutcome::Synced { .. } => None,
                };
                self.apply_sync_result(result, outcome, doc);
                if let Some(invocation) = self.resource_sync_invocation.take() {
                    self.resource_sync_result = Some(if let Some(message) = failure {
                        tilo_actions::InvocationUpdate::Failed {
                            invocation,
                            error: tilo_actions::ActionFailure {
                                code: "calendar.sync_failed".into(),
                                message,
                                details: None,
                                retryable: true,
                            },
                        }
                    } else {
                        tilo_actions::InvocationUpdate::Succeeded {
                            invocation,
                            output: tilo_actions::ActionOutput::default(),
                        }
                    });
                }
                // A landed pull mutated the document — persist it.
                self.force_persist = true;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                // Worker panicked or dropped the sender without a reply.
                self.sync_rx = None;
                self.syncing = false;
                self.status = Some("Sync failed".into());
                if let Some(invocation) = self.resource_sync_invocation.take() {
                    self.resource_sync_result = Some(tilo_actions::InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
                            code: "calendar.sync_worker_lost".into(),
                            message: "calendar synchronization worker stopped without a result"
                                .into(),
                            details: None,
                            retryable: true,
                        },
                    });
                }
                self.dirty = true;
            }
        }
    }

    /// Atomically reconcile server resources by stable collection href. A
    /// title/date is presentation, never identity; edits update the same local
    /// UUID across pulls and server deletions remove it without resurrection.
    #[allow(
        clippy::too_many_lines,
        reason = "remote reconciliation is deliberately kept in one transactional merge routine"
    )]
    fn apply_sync_result(
        &mut self,
        result: caldav::SyncResult,
        outcome: SyncOutcome,
        doc: &mut AutoCommit,
    ) {
        let mut added = 0usize;
        let mut updated = 0usize;
        let mut removed = 0usize;

        // Demo/import results have no DAV resource envelope.
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
            self.perform_action(
                doc,
                &crate::actions::CalendarAction::AdoptRemoteEvent(
                    crate::actions::EventData::from_event(&e),
                ),
            );
            self.model.events.push(e);
            added += 1;
        }
        for resource in result.resources {
            let existing = CalDoc::remote_id_for_href(doc, &resource.calendar_id, &resource.href);
            for (index, mut event) in resource.events.into_iter().enumerate() {
                // Current parser represents recurrence as one event. If a
                // server returns multiple components in one object, retain
                // stable identities for all by an internal component suffix.
                let binding_href = if index == 0 {
                    resource.href.clone()
                } else {
                    format!("{}#tilo-component-{index}", resource.href)
                };
                let id = if index == 0 {
                    existing
                } else {
                    CalDoc::remote_id_for_href(doc, &resource.calendar_id, &binding_href)
                }
                .unwrap_or_else(Uuid::new_v4);
                // A locally deleted ID is permanent. A late server pull may
                // still carry its old href, but it must not repopulate the
                // transient model while the durable tombstone hides it from
                // every other replica.
                if CalDoc::event_tombstoned(doc, id) {
                    continue;
                }
                event.id = id;
                event.calendar_id = Some(resource.calendar_id.clone());
                if let Some(slot) = self.model.events.iter_mut().find(|item| item.id == id) {
                    *slot = event.clone();
                    updated += 1;
                } else {
                    self.model.events.push(event.clone());
                    added += 1;
                }
                self.perform_action(
                    doc,
                    &crate::actions::CalendarAction::AdoptRemoteEvent(
                        crate::actions::EventData::from_event(&event),
                    ),
                );
                self.record_remote_binding(
                    doc,
                    id,
                    &resource.calendar_id,
                    &binding_href,
                    resource.etag.as_deref(),
                );
            }
            for (index, mut task) in resource.tasks.into_iter().enumerate() {
                let binding_href = format!("{}#tilo-task-{index}", resource.href);
                let id = CalDoc::remote_id_for_href(doc, &resource.calendar_id, &binding_href)
                    .unwrap_or_else(Uuid::new_v4);
                if CalDoc::task_tombstoned(doc, id) {
                    continue;
                }
                task.id = id;
                if let Some(slot) = self.model.tasks.iter_mut().find(|item| item.id == id) {
                    *slot = task.clone();
                    updated += 1;
                } else {
                    self.model.tasks.push(task.clone());
                    added += 1;
                }
                self.perform_action(
                    doc,
                    &crate::actions::CalendarAction::AdoptRemoteTask(
                        crate::actions::TaskData::from_task(&task),
                    ),
                );
                self.record_remote_binding(
                    doc,
                    id,
                    &resource.calendar_id,
                    &binding_href,
                    resource.etag.as_deref(),
                );
            }
        }
        for deletion in result.deletions {
            let victims: Vec<Id> = CalDoc::remote_bindings_for_calendar(doc, &deletion.calendar_id)
                .into_iter()
                .filter(|(_, href)| {
                    caldav::hrefs_equal(href.split("#tilo-").next().unwrap_or(href), &deletion.href)
                })
                .map(|(id, _)| id)
                .collect();
            for id in victims {
                self.perform_action(
                    doc,
                    &crate::actions::CalendarAction::DropRemoteItem { id: id.to_string() },
                );
                self.model.events.retain(|event| event.id != id);
                self.model.tasks.retain(|task| task.id != id);
                removed += 1;
            }
        }
        // A successful full query is authoritative for that collection. This
        // is also the invalid-token recovery path.
        for checkpoint in &result.checkpoints {
            if checkpoint.full {
                let seen: HashSet<&str> =
                    checkpoint.seen_hrefs.iter().map(String::as_str).collect();
                for (id, href) in CalDoc::remote_bindings_for_calendar(doc, &checkpoint.calendar_id)
                {
                    let base = href.split("#tilo-").next().unwrap_or(&href);
                    if !seen
                        .iter()
                        .any(|seen_href| caldav::hrefs_equal(base, seen_href))
                    {
                        self.perform_action(
                            doc,
                            &crate::actions::CalendarAction::DropRemoteItem { id: id.to_string() },
                        );
                        self.model.events.retain(|event| event.id != id);
                        self.model.tasks.retain(|task| task.id != id);
                        removed += 1;
                    }
                }
            }
        }
        // Commit markers are deliberately last: a crash before here repeats a
        // response idempotently; a crash after here sees fully reconciled data.
        for checkpoint in result.checkpoints {
            self.perform_action(
                doc,
                &crate::actions::CalendarAction::SetSyncToken {
                    calendar_id: checkpoint.calendar_id.clone(),
                    token: checkpoint.token.clone(),
                },
            );
        }
        self.model.events.sort_by_key(|e| e.start);
        self.status = Some(match outcome {
            SyncOutcome::Synced { calendars, .. } => {
                format!(
                    "Synced {calendars} calendars. {added} new, {updated} updated, {removed} removed."
                )
            }
            SyncOutcome::Offline => {
                if self.accounts.is_empty() {
                    "No calendar accounts yet. Add one in Settings.".into()
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
    fn gather_push_jobs(&self, doc: &AutoCommit) -> Vec<caldav::UploadJob> {
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
                    let binding = CalDoc::remote_binding(doc, event.id);
                    jobs.push(caldav::UploadJob {
                        config: acc.config.clone(),
                        calendar: cal.clone(),
                        event: event.clone(),
                        href: binding.as_ref().map(|(_, href, _)| href.clone()),
                        etag: binding.and_then(|(_, _, etag)| etag),
                    });
                    break;
                }
            }
        }
        jobs
    }

    /// The notes editor, bound to `owner`'s notes: every edit is the
    /// calendar's own [`EditNotes`](crate::actions::CalendarAction::EditNotes)
    /// action on that event or task.
    fn new_notes_editor(owner: Option<Sel>) -> NotesEditor {
        let (event, id) = match owner {
            Some(Sel::Event(id)) => (true, Some(id)),
            Some(Sel::Task(id)) => (false, Some(id)),
            None => (true, None),
        };
        let note_owner = {
            let id = id.map(|id| id.to_string()).unwrap_or_default();
            if event {
                crate::actions::NoteOwner::Event { id }
            } else {
                crate::actions::NoteOwner::Task { id }
            }
        };
        let context = crate::actions::history_context(&crate::actions::CalendarAction::EditNotes {
            owner: note_owner.clone(),
            edit: tilo_document::RichTextAction::AppendParagraph {
                text: String::new(),
            },
        });
        let field = RichTextField::new(
            crate::actions::CALENDAR_ACTION_TYPE,
            move |doc| id.and_then(|id| CalDoc::notes(doc, event, id)),
            move |edit| crate::actions::CalendarAction::EditNotes {
                owner: note_owner.clone(),
                edit,
            },
        )
        .with_context(context);
        RichTextSurface::field(field).with_placeholder(NOTES_PLACEHOLDER)
    }

    // --- CRDT plumbing ---------------------------------------------------

    fn refresh_note_search(&mut self, doc: &AutoCommit) {
        #[cfg(test)]
        {
            self.note_projection_refreshes = self.note_projection_refreshes.saturating_add(1);
        }
        let mut notes = HashMap::new();
        for event in &self.model.events {
            notes.insert(event.id.to_string(), CalDoc::note_text(doc, true, event.id));
        }
        for task in &self.model.tasks {
            notes.insert(task.id.to_string(), CalDoc::note_text(doc, false, task.id));
        }
        self.note_search = notes;
    }

    /// The configuration-only portion of Calendar's resource/search surface.
    /// It deliberately excludes account collapse (pure sidebar presentation)
    /// but includes every property that can alter a collection result or its
    /// canonical target.
    fn calendar_collections(&self) -> Vec<crate::actions::CalendarCollectionResource> {
        let mut calendars = vec![crate::actions::CalendarCollectionResource {
            id: "local".to_string(),
            label: "On this device".to_string(),
            color: None,
            visible: true,
        }];
        calendars.extend(self.accounts.all_calendars().map(|(account, calendar)| {
            crate::actions::CalendarCollectionResource {
                id: calendar.id.clone(),
                label: format!("{} ({})", calendar.name, account.label()),
                color: Some(calendar.palette().index()),
                visible: calendar.visible,
            }
        }));
        calendars
    }

    /// Advance the calendar-projection generation only when Settings-owned
    /// collection data actually differs. This catches an in-memory account
    /// replacement or a direct non-CRDT configuration change while keeping an
    /// unchanged frame from rebuilding the search catalogue (or reading any
    /// notes).
    fn observe_calendar_projection(&mut self) {
        let collections = self.calendar_collections();
        if collections != self.calendar_projection_snapshot {
            self.calendar_projection_snapshot = collections;
            self.calendar_projection_revision = self.calendar_projection_revision.wrapping_add(1);
        }
    }

    /// Rebuild the retained catalogue for a document revision and/or the
    /// Settings-owned collection projection. Notes are projected only when the
    /// document changed; a collection rename or visibility toggle reuses the
    /// already-current note projection.
    fn refresh_search_catalog(&mut self, heads: &[ChangeHash]) {
        let calendars = self.calendar_projection_snapshot.clone();
        match self.search_provider.refresh_with_calendars(
            &self.model,
            &self.note_search,
            &calendars,
        ) {
            Ok(()) => {
                self.search_catalog_heads = heads.to_vec();
                self.search_catalog_projection_revision = self.calendar_projection_revision;
            }
            Err(error) => {
                // Keep the last complete revision searchable, but make a
                // failed catalogue rebuild visible rather than silently
                // advertising a partial/stale corpus as current.
                self.status = Some(format!("Calendar search update failed: {error}"));
            }
        }
        #[cfg(test)]
        {
            self.search_catalog_refreshes = self.search_catalog_refreshes.saturating_add(1);
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
            {
                CalDoc::seed_notes_if_empty(doc, true, e.id, lines);
            }
        }
    }

    fn ensure_bound(&mut self, doc: &mut AutoCommit) {
        let sel = match self.selection {
            Some(s) if self.detail_open => s,
            _ => {
                if self.bound.is_some() {
                    self.notes_editor = Self::new_notes_editor(None);
                    self.bound = None;
                }
                return;
            }
        };
        if self.bound == Some(sel) {
            return;
        }
        let exists = match sel {
            Sel::Event(id) => CalDoc::ensure_notes(doc, true, id),
            Sel::Task(id) => CalDoc::ensure_notes(doc, false, id),
        };
        if exists.is_some() {
            self.notes_editor = Self::new_notes_editor(Some(sel));
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
                self.invite_input.set_text("");
            }
        } else {
            self.title_input.set_focus(false);
            self.location_input.set_focus(false);
            self.invite_input.set_focus(false);
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
        let patch = if let Some(e) = self.model.event_mut(id) {
            let mut patch = crate::actions::EventPatch::default();
            if title_dirty.is_some() && e.title != title {
                e.title = title;
                patch.title = Some(e.title.clone());
            }
            if loc_dirty.is_some() && e.location != location {
                e.location = location;
                patch.location = Some(e.location.clone());
            }
            (!patch.is_empty()).then_some(patch)
        } else {
            None
        };
        if let Some(patch) = patch {
            self.perform_action(doc, &crate::actions::patch_event_action(id, patch));
            self.force_persist = true;
            self.dirty = true;
        }
    }

    fn submit_invite(&mut self, doc: &mut AutoCommit) {
        if !self.invite_input.take_submit() {
            return;
        }
        let Some(Sel::Event(event_id)) = self.selection else {
            return;
        };
        let email = self.invite_input.value().trim().to_string();
        let valid = email
            .split_once('@')
            .is_some_and(|(local, domain)| !local.is_empty() && domain.contains('.'));
        if !valid {
            self.status = Some("Enter a complete email address, such as name@example.com".into());
            return;
        }
        if self
            .model
            .event(event_id)
            .is_some_and(|event| event.attendees.iter().any(|item| item.email == email))
        {
            self.status = Some(format!("{email} is already invited"));
            return;
        }
        let attendee = crate::model::Attendee {
            id: Uuid::new_v4(),
            name: String::new(),
            email: email.clone(),
            status: crate::model::InviteStatus::NeedsAction,
        };
        self.perform_action(
            doc,
            &crate::actions::CalendarAction::UpsertAttendee {
                event_id: event_id.to_string(),
                attendee: crate::actions::AttendeeData {
                    id: attendee.id.to_string(),
                    name: attendee.name.clone(),
                    email: attendee.email.clone(),
                    status: attendee.status.as_ical().into(),
                },
            },
        );
        if let Some(event) = self.model.event_mut(event_id) {
            event.attendees.push(attendee);
        }
        self.invite_input.set_text("");
        self.status = Some(format!(
            "Added {email}. Prepare the invitation in Mail to send it."
        ));
        self.force_persist = true;
        self.dirty = true;
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
                self.last_persist = ui::time::now();
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
        self.actions.dispatch(doc, action);
    }

    fn record_remote_binding(
        &mut self,
        doc: &mut AutoCommit,
        id: Id,
        calendar_id: &str,
        href: &str,
        etag: Option<&str>,
    ) {
        self.perform_action(
            doc,
            &crate::actions::CalendarAction::SetRemoteBinding {
                id: id.to_string(),
                calendar_id: calendar_id.to_string(),
                href: href.to_string(),
                etag: etag.unwrap_or_default().to_string(),
            },
        );
    }

    /// Send a local effect through the same typed resource-operation protocol
    /// that agents, search, history, and collaboration-aware hosts invoke.
    ///
    /// Device-private operations such as collection visibility intentionally
    /// do not enter the shared event CRDT, but must still have one canonical
    /// encoded envelope and one handler. Otherwise a pointer retry could drift
    /// from an agent or screen-reader invocation of the same resource action.
    fn invoke_local_calendar_effect(
        &mut self,
        operation: &crate::actions::CalendarOperation,
        doc: &AutoCommit,
    ) -> Result<(), String> {
        let encoded = tilo_actions::EncodedAction::encode_operation(
            crate::actions::CALENDAR_OPERATION_TYPE,
            operation,
        )
        .map_err(|error| error.to_string())?;
        #[cfg(test)]
        {
            self.last_local_resource_operation = Some(encoded.clone());
        }
        let handled = self.answer_resource_operation(
            tilo_actions::OperationKind::Effect,
            &encoded.type_id,
            &encoded.payload,
            doc,
            &automerge::ROOT,
        )?;
        if handled {
            Ok(())
        } else {
            Err("Calendar does not handle its advertised visibility operation".into())
        }
    }

    /// The last [`EncodedAction`](tilo_actions::EncodedAction) an on-screen
    /// affordance dispatched, for the button == [`ActionSpec`](tilo_actions::ActionSpec)
    /// covenant test: a control and its Ctrl+K entry must build the identical
    /// action.
    #[cfg(test)]
    #[allow(dead_code, reason = "framework migration test helper")]
    fn last_dispatched(&self) -> Option<&tilo_actions::EncodedAction> {
        self.actions.last_dispatched()
    }

    #[cfg(test)]
    fn last_local_resource_operation(&self) -> Option<&tilo_actions::EncodedAction> {
        self.last_local_resource_operation.as_ref()
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

    /// Build compact stable-id indices over the authoritative model and the
    /// small Settings-owned calendar projection. No event/task payload is
    /// copied here; resource requests materialise only a bounded snapshot or
    /// the one exact record they address.
    fn rebuild_resource_index(&mut self) {
        let mut index = CalendarResourceIndex::default();
        for (position, event) in self.model.events.iter().enumerate() {
            index.events.insert(event.id.to_string(), position);
            if let Some(project) = event.project {
                index.projects_in_use.insert(project.to_string());
            }
        }
        for (position, task) in self.model.tasks.iter().enumerate() {
            index.tasks.insert(task.id.to_string(), position);
            if let Some(project) = task.project {
                index.projects_in_use.insert(project.to_string());
            }
            if let Some(parent) = task.parent {
                let children = index.subtasks.entry(parent.to_string()).or_default();
                if children.ids.len() < crate::actions::TASK_DELETE_FANOUT_LIMIT {
                    children.ids.push(task.id.to_string());
                } else {
                    children.overflow = children.overflow.saturating_add(1);
                }
            }
        }
        for (position, project) in self.model.projects.iter().enumerate() {
            index.projects.insert(project.id.to_string(), position);
        }
        for (position, calendar) in self.calendar_projection_snapshot.iter().enumerate() {
            index.calendars.insert(calendar.id.clone(), position);
        }
        self.resource_index = index;
    }

    fn resource_template(&self) -> crate::actions::EventData {
        let start = self
            .anchor
            .and_time(NaiveTime::from_hms_opt(12, 0, 0).unwrap_or(NaiveTime::MIN));
        crate::actions::EventData::from_event(&Event {
            id: Uuid::nil(),
            title: String::new(),
            start,
            end: start + Duration::hours(1),
            all_day: false,
            location: String::new(),
            color: self.model.next_event_order_color(),
            recurrence: Recurrence::None,
            recurrence_interval: 1,
            recurrence_until: None,
            recurrence_count: None,
            recurrence_exdates: Vec::new(),
            occurrence_overrides: Vec::new(),
            timezone: "floating".into(),
            organizer: None,
            attendees: Vec::new(),
            reminder_min: Some(10),
            calendar_id: None,
            project: None,
        })
    }

    fn resource_ctx_base(&self) -> crate::actions::CalendarResourceCtx {
        crate::actions::CalendarResourceCtx::new(
            self.resource_template(),
            self.demo || !self.accounts.is_empty(),
            self.syncing,
            !self.model.events.is_empty() || !self.model.tasks.is_empty(),
            self.model
                .projects
                .last()
                .map_or(1, |project| project.order.saturating_add(1)),
        )
    }

    /// A bounded contextual snapshot for the root directory. This is the only
    /// path `Widget::resources` uses, so opening the local resource directory
    /// never clones or projects an unbounded Calendar corpus.
    #[allow(
        clippy::too_many_lines,
        reason = "one bounded projection keeps resource ownership and editor destinations consistent"
    )]
    pub(crate) fn calendar_resource_ctx(&self) -> crate::actions::CalendarResourceCtx {
        #[cfg(test)]
        self.resource_snapshot_inspected.set(0);
        let mut ctx = self.resource_ctx_base();
        let limit = crate::actions::EAGER_RESOURCE_SNAPSHOT_LIMIT;
        ctx.calendars = self
            .calendar_projection_snapshot
            .iter()
            .take(limit)
            .cloned()
            .collect();
        ctx.projects = self
            .model
            .projects
            .iter()
            .take(limit)
            .map(|project| {
                #[cfg(test)]
                self.resource_snapshot_inspected
                    .set(self.resource_snapshot_inspected.get().saturating_add(1));
                crate::actions::ProjectResource {
                    data: crate::actions::ProjectData::from_project(project),
                    in_use: self
                        .resource_index
                        .projects_in_use
                        .contains(&project.id.to_string()),
                }
            })
            .collect();
        ctx.events = self
            .model
            .events
            .iter()
            .take(limit)
            .map(|event| {
                #[cfg(test)]
                self.resource_snapshot_inspected
                    .set(self.resource_snapshot_inspected.get().saturating_add(1));
                crate::actions::EventResource {
                    data: crate::actions::EventData::from_event(event),
                    remote_delete: self.resolve_remote_delete(event.id),
                }
            })
            .collect();
        ctx.tasks = self
            .model
            .tasks
            .iter()
            .take(limit)
            .map(|task| {
                #[cfg(test)]
                self.resource_snapshot_inspected
                    .set(self.resource_snapshot_inspected.get().saturating_add(1));
                crate::actions::TaskResource {
                    data: crate::actions::TaskData::from_task(task),
                    subtasks: self
                        .resource_index
                        .subtasks
                        .get(&task.id.to_string())
                        .map_or_else(Vec::new, |children| children.ids.clone()),
                    subtask_overflow: self
                        .resource_index
                        .subtasks
                        .get(&task.id.to_string())
                        .map_or(0, |children| children.overflow),
                }
            })
            .collect();
        ctx
    }

    /// Materialise a one-record context for a canonical exact resource id.
    /// Each entity position comes from `resource_index`, then is checked
    /// against the authoritative current model to make stale positions/IDs
    /// harmless. Descendants share their event's one-record context.
    pub(crate) fn exact_resource_ctx(
        &self,
        id: &str,
    ) -> Option<crate::actions::CalendarResourceCtx> {
        #[cfg(test)]
        self.resource_exact_inspected.set(0);
        let mut ctx = self.resource_ctx_base();
        let event_id = id
            .strip_prefix("calendar.event:")
            .and_then(|value| value.strip_suffix(":notes").or(Some(value)))
            .or_else(|| {
                id.strip_prefix("calendar.attendee:")
                    .and_then(|value| value.split_once(':').map(|(event, _)| event))
            })
            .or_else(|| {
                id.strip_prefix("calendar.reminder:")
                    .and_then(|value| value.strip_suffix(":primary"))
            })
            .or_else(|| {
                id.strip_prefix("calendar.occurrence:")
                    .and_then(|value| value.split_once(':').map(|(event, _)| event))
            });
        if let Some(event_id) = event_id {
            let position = *self.resource_index.events.get(event_id)?;
            let event = self
                .model
                .events
                .get(position)
                .filter(|event| event.id.to_string() == event_id)?;
            #[cfg(test)]
            self.resource_exact_inspected.set(1);
            ctx.events.push(crate::actions::EventResource {
                data: crate::actions::EventData::from_event(event),
                remote_delete: self.resolve_remote_delete(event.id),
            });
            if let Some(calendar) = self.calendar_for_resource(event.calendar_id.as_deref()) {
                ctx.calendars.push(calendar);
            }
            return Some(ctx);
        }
        if let Some(raw) = id.strip_prefix("calendar.task:") {
            let task_id = raw.strip_suffix(":notes").unwrap_or(raw);
            let position = *self.resource_index.tasks.get(task_id)?;
            let task = self
                .model
                .tasks
                .get(position)
                .filter(|task| task.id.to_string() == task_id)?;
            #[cfg(test)]
            self.resource_exact_inspected.set(1);
            ctx.tasks.push(crate::actions::TaskResource {
                data: crate::actions::TaskData::from_task(task),
                subtasks: self
                    .resource_index
                    .subtasks
                    .get(task_id)
                    .map_or_else(Vec::new, |children| children.ids.clone()),
                subtask_overflow: self
                    .resource_index
                    .subtasks
                    .get(task_id)
                    .map_or(0, |children| children.overflow),
            });
            if let Some(project) = task
                .project
                .and_then(|project_id| self.project_resource(project_id))
            {
                ctx.projects.push(project);
            }
            return Some(ctx);
        }
        if let Some(raw) = id.strip_prefix("calendar.project:") {
            let parsed = raw.parse::<Id>().ok()?;
            if crate::actions::project_resource_id(parsed) != id {
                return None;
            }
            let project = self.project_resource(parsed)?;
            #[cfg(test)]
            self.resource_exact_inspected.set(1);
            ctx.projects.push(project);
            return Some(ctx);
        }
        if let Some(raw) = id.strip_prefix("calendar.collection:") {
            if crate::actions::calendar_resource_id(raw) != id {
                return None;
            }
            let position = *self.resource_index.calendars.get(raw)?;
            let calendar = self
                .calendar_projection_snapshot
                .get(position)
                .filter(|calendar| calendar.id == raw)?
                .clone();
            #[cfg(test)]
            self.resource_exact_inspected.set(1);
            ctx.calendars.push(calendar);
            return Some(ctx);
        }
        (id == "calendar.calendar").then_some(ctx)
    }

    fn calendar_for_resource(
        &self,
        id: Option<&str>,
    ) -> Option<crate::actions::CalendarCollectionResource> {
        let id = id.unwrap_or("local");
        let position = *self.resource_index.calendars.get(id)?;
        self.calendar_projection_snapshot
            .get(position)
            .filter(|calendar| calendar.id == id)
            .cloned()
    }

    fn project_resource(&self, id: Id) -> Option<crate::actions::ProjectResource> {
        let raw = id.to_string();
        let position = *self.resource_index.projects.get(&raw)?;
        let project = self
            .model
            .projects
            .get(position)
            .filter(|project| project.id == id)?;
        Some(crate::actions::ProjectResource {
            data: crate::actions::ProjectData::from_project(project),
            in_use: self.resource_index.projects_in_use.contains(&raw),
        })
    }

    /// Convert parsed ICS records into one deterministic action. Tilo's UUID
    /// UIDs survive parsing, so a repeated import resolves to the same task;
    /// missing or external non-UUID identities are minted during parsing.
    /// Replays carry this exact payload and never consult the filesystem or
    /// clock.
    fn import_action_from_payload(&self, payload: &str) -> Option<crate::actions::CalendarAction> {
        let (events, tasks) = ics::parse(payload);
        let events: Vec<_> = events
            .into_iter()
            .filter(|event| {
                !self
                    .model
                    .events
                    .iter()
                    .any(|current| current.title == event.title && current.start == event.start)
            })
            .map(|mut event| {
                event.id = Uuid::new_v4();
                crate::actions::EventData::from_event(&event)
            })
            .collect();
        let tasks: Vec<_> = tasks
            .into_iter()
            .filter(|task| {
                !self.model.tasks.iter().any(|current| {
                    current.id == task.id
                        && current.title == task.title
                        && current.done == task.done
                        && current.due == task.due
                        && current.priority == task.priority
                })
            })
            .map(|task| crate::actions::TaskData::from_task(&task))
            .collect();
        (!events.is_empty() || !tasks.is_empty())
            .then_some(crate::actions::CalendarAction::ImportIcs { events, tasks })
    }

    /// Read the transfer directory only for an explicitly invoked import.
    fn latest_ics_import_action(&self) -> Result<crate::actions::CalendarAction, String> {
        let dir = crate::store::transfer_dir();
        let path = crate::store::newest_ics(&dir)
            .ok_or_else(|| format!("No .ics files found in {}", dir.display()))?;
        let payload = read_text(&path).map_err(|error| format!("Import failed: {error}"))?;
        self.import_action_from_payload(&payload).ok_or_else(|| {
            format!(
                "{} contains no new supported calendar items",
                path.display()
            )
        })
    }

    // --- mutations -------------------------------------------------------

    fn select_event(&mut self, id: Id) {
        self.selection = Some(Sel::Event(id));
        self.detail_scroll_y = 0.0;
        self.shots_detail_scroll_to_end = false;
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
        self.detail_scroll_y = 0.0;
        self.shots_detail_scroll_to_end = false;
        self.open_detail();
    }

    /// The task currently selected, if the selection is a task (not an event).
    pub fn selected_task(&self) -> Option<Id> {
        match self.selection {
            Some(Sel::Task(id)) => Some(id),
            _ => None,
        }
    }

    /// Open the editor sheet for the current selection. An open sidebar
    /// drawer is dismissed so it isn't left under the sheet.
    fn open_detail(&mut self) {
        self.detail_open = true;
        self.detail_scroll_y = 0.0;
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

    /// Release keyboard focus from every editor field.
    /// and drop any open popover. Called whenever the editor leaves the screen
    /// so keystrokes meant for the calendar can't leak into a hidden field.
    fn blur_editor(&mut self) {
        self.notes_editor.set_focus(false);
        self.title_input.set_focus(false);
        self.location_input.set_focus(false);
        self.invite_input.set_focus(false);
        self.event_popovers.close();
    }

    /// Shared teardown after deleting the on-screen item: drop the selection,
    /// close the editor, and return to the calendar.
    fn close_after_delete(&mut self) {
        self.selection = None;
        self.detail_open = false;
        self.blur_editor();
    }

    /// Resolve the delete outbox into concrete `CalDAV` delete jobs: each queued
    /// `(calendar_id, uid)` paired with the account config + remote calendar it
    /// targets. Entries whose calendar is no longer configured are skipped and
    /// left queued for a later attempt.
    fn gather_delete_jobs(&self, doc: &AutoCommit) -> Vec<caldav::RemoveJob> {
        let mut jobs = Vec::new();
        for (cal_id, uid) in CalDoc::delete_outbox(doc) {
            for acc in &self.accounts.accounts {
                if let Some(cal) = acc.calendars.iter().find(|c| c.id == cal_id) {
                    let binding = CalDoc::remote_binding(doc, uid);
                    jobs.push(caldav::RemoveJob {
                        config: acc.config.clone(),
                        calendar: cal.clone(),
                        id: uid,
                        href: binding.as_ref().map(|(_, href, _)| href.clone()),
                        etag: binding.and_then(|(_, _, etag)| etag),
                    });
                    break;
                }
            }
        }
        jobs
    }

    fn toggle_task(&mut self, id: Id, doc: &mut AutoCommit) {
        let done = if let Some(t) = self.model.task_mut(id) {
            t.done = !t.done;
            Some(t.done)
        } else {
            None
        };
        if let Some(done) = done {
            self.perform_action(
                doc,
                &crate::actions::set_task_done_action(id.to_string(), done),
            );
            self.dirty = true;
        }
    }

    fn set_selected_task_due(&mut self, due: Option<NaiveDate>, doc: &mut AutoCommit) {
        let Some(Sel::Task(id)) = self.selection else {
            return;
        };
        if let Some(task) = self.model.task_mut(id) {
            task.due = due;
            self.perform_action(
                doc,
                &crate::actions::patch_task_action(
                    id,
                    crate::actions::TaskPatch {
                        due: Some(due.map(|date| date.to_string()).unwrap_or_default()),
                        ..crate::actions::TaskPatch::default()
                    },
                ),
            );
        }
        self.task_popover = None;
    }

    fn set_selected_task_priority(&mut self, priority: Priority, doc: &mut AutoCommit) {
        let Some(Sel::Task(id)) = self.selection else {
            return;
        };
        if let Some(task) = self.model.task_mut(id) {
            task.priority = priority;
            self.perform_action(
                doc,
                &crate::actions::patch_task_action(
                    id,
                    crate::actions::TaskPatch {
                        priority: Some(priority.rank()),
                        ..crate::actions::TaskPatch::default()
                    },
                ),
            );
        }
        self.task_popover = None;
    }

    fn set_selected_task_project(&mut self, project: Option<Id>, doc: &mut AutoCommit) {
        let Some(Sel::Task(id)) = self.selection else {
            return;
        };
        if let Some(task) = self.model.task_mut(id) {
            task.project = project;
            self.perform_action(
                doc,
                &crate::actions::patch_task_action(
                    id,
                    crate::actions::TaskPatch {
                        project: Some(project.map(|value| value.to_string()).unwrap_or_default()),
                        ..crate::actions::TaskPatch::default()
                    },
                ),
            );
        }
        self.task_popover = None;
        self.choices.close_all();
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
            recurrence_interval: 1,
            recurrence_until: None,
            recurrence_count: None,
            recurrence_exdates: Vec::new(),
            occurrence_overrides: Vec::new(),
            timezone: "floating".into(),
            organizer: None,
            attendees: Vec::new(),
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
        self.fresh_draft = Some(ev.clone());
        self.model.events.push(ev);
        self.model.events.sort_by_key(|e| e.start);
        self.select_event(id);
        self.title_input.set_text("");
        self.location_input.set_text("");
        self.invite_input.set_text("");
        self.focus_field(FormField::Title);
    }

    /// Delete an untouched new draft when its editor is dismissed. This is
    /// deliberately narrower than “cancel event”: a user-created event with a
    /// title, any changed field, or notes is already useful work and remains
    /// autosaved. The delete travels through the normal replayable action path,
    /// so collaborators and history see the same discard.
    fn discard_pristine_draft(&mut self, doc: &mut AutoCommit) -> bool {
        let Some(initial) = self.fresh_draft.take() else {
            return false;
        };
        let id = initial.id;
        let Some(current) = self.model.event(id).cloned() else {
            return false;
        };
        if !crate::actions::EventPatch::between(&initial, &current).is_empty()
            || CalDoc::event_has_notes(doc, id)
        {
            return false;
        }
        self.delete_event(id, doc);
        self.status = Some("Discarded empty draft".into());
        true
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
            recurrence_interval: 1,
            recurrence_until: None,
            recurrence_count: None,
            recurrence_exdates: Vec::new(),
            occurrence_overrides: Vec::new(),
            timezone: "floating".into(),
            organizer: None,
            attendees: Vec::new(),
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
        let patch = if let Some(e) = self.model.event_mut(id) {
            // An all-day event has no place on the timed grid; leave it alone.
            if e.all_day {
                None
            } else {
                let before = e.clone();
                e.start = start;
                e.end = end;
                let patch = crate::actions::EventPatch::between(&before, e);
                (!patch.is_empty()).then_some(patch)
            }
        } else {
            None
        };
        if let Some(patch) = patch {
            self.perform_action(doc, &crate::actions::patch_event_action(id, patch));
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
    fn handle_timeline_input(&mut self, input: &mut InputContext<'_>) -> bool {
        if !matches!(self.view, View::Week | View::Day) {
            self.timeline.cancel_drag();
        }
        if !self.calendar_content_interactive() {
            return false;
        }
        if matches!(self.view, View::Agenda) {
            return self.handle_agenda_timeline_input(input);
        }
        if !matches!(self.view, View::Week | View::Day) {
            return false;
        }

        let area = self.view_rect();
        let days = self.timeline_days();
        let spans = self.timeline_spans(&days);
        let outcome = self.timeline.handle_event(input, area, &days, &spans);
        if let Some(effect) = outcome.effect {
            self.apply_timeline_effect(effect, input.state);
        }
        if outcome.handled {
            *input.handled = true;
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
    fn handle_agenda_timeline_input(&mut self, input: &mut InputContext<'_>) -> bool {
        if matches!(
            input.event,
            WindowEvent::MouseWheel { .. }
                | WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput {
                    button: MouseButton::Left,
                    ..
                }
        ) {
            let p = self.last_mouse;
            if self.agenda.wants_pointer_capture() || self.view_rect().contains(p) {
                let handled = self.agenda.handle_event(input);
                if handled {
                    self.dirty = true;
                }
                return handled;
            }
        }
        false
    }

    // --- event form model edits -----------------------------------------

    /// Commit an *absolute* collection visibility state to the Settings-owned
    /// account store. Absolute intent means the screen-reader row, Ctrl+K
    /// resource action, and an agent retry cannot accidentally invert a newer
    /// user choice by replaying a toggle.
    fn set_calendar_visibility(&mut self, id: &str, visible: bool) -> Result<(), String> {
        let mut updated = self.accounts.clone();
        let Some(calendar) = updated
            .accounts
            .iter_mut()
            .flat_map(|account| account.calendars.iter_mut())
            .find(|calendar| calendar.id == id)
        else {
            return Err("That calendar is no longer configured".into());
        };
        if calendar.visible == visible {
            return Ok(());
        }
        calendar.visible = visible;

        if !self.demo
            && let Err(error) = updated.persist()
        {
            return Err(format!("Couldn't save calendar visibility: {error}"));
        }

        self.accounts = updated;
        // Make the generation visibly stale now; `sync` will rebuild the
        // catalogue on its next pass without projecting notes again.
        self.observe_calendar_projection();
        self.dirty = true;
        Ok(())
    }

    /// Persist a sidebar disclosure preference through the same shared account
    /// store that owns visibility. This keeps a collapse/expand choice stable
    /// across restarts, while never claiming success if Settings' store cannot
    /// be written.
    fn toggle_account_collapsed(&mut self, id: &str) {
        let mut updated = self.accounts.clone();
        let Some(account) = updated.accounts.iter_mut().find(|account| account.id == id) else {
            self.status = Some("That calendar account is no longer configured".into());
            return;
        };
        account.collapsed = !account.collapsed;

        if !self.demo
            && let Err(error) = updated.persist()
        {
            self.status = Some(format!("Couldn't save account sidebar state: {error}"));
            return;
        }

        self.accounts = updated;
        self.dirty = true;
    }

    /// Every calendar an event can be filed under: the local on-device
    /// calendar plus each remote `CalDAV` collection configured in Settings
    /// (the shared [`AccountStore`]). This is the selector's option list —
    /// sourced from real configured accounts, never fabricated.
    fn calendar_options(&self) -> Vec<(CalTarget, String)> {
        let mut out = vec![(CalTarget::Local, "On this device".to_string())];
        for acc in &self.accounts.accounts {
            for cal in &acc.calendars {
                out.push((CalTarget::Remote(cal.id.clone()), cal.name.clone()));
            }
        }
        out
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
        self.invite_input.set_focus(false);
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
                self.invite_input.set_focus(false);
            }
            FormField::Location => {
                self.title_input.set_focus(false);
                self.location_input.set_focus(true);
                self.invite_input.set_focus(false);
            }
            FormField::Invite => {
                self.title_input.set_focus(false);
                self.location_input.set_focus(false);
                self.invite_input.set_focus(true);
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
        } else if self.invite_input.is_focused() {
            2
        } else if self.notes_editor.is_focused() {
            3
        } else {
            -1
        };
        let n = 4;
        let next = if cur < 0 {
            if backwards { n - 1 } else { 0 }
        } else {
            (cur + if backwards { n - 1 } else { 1 }) % n
        };
        self.event_popovers.close();
        self.title_input.set_focus(false);
        self.location_input.set_focus(false);
        self.invite_input.set_focus(false);
        self.notes_editor.set_focus(false);
        match next {
            0 => self.title_input.set_focus(true),
            1 => self.location_input.set_focus(true),
            2 => self.invite_input.set_focus(true),
            _ => self.notes_editor.set_focus(true),
        }
        self.dirty = true;
    }

    /// Route input through the self-contained popover controller, then apply
    /// any semantic edit it emits through the calendar action model.
    fn handle_popover_event(&mut self, input: &mut InputContext<'_>) {
        let edit = self.event_popovers.handle_event(input, self.last_mouse);
        match edit {
            Some(PopoverEdit::Date(which, date)) => {
                self.set_event_date(which, date, input.state);
            }
            Some(PopoverEdit::Time(which, hour, minute)) => {
                self.set_event_time(which, hour, minute, input.state);
            }
            None => {}
        }
    }

    /// Flip the selected event's all-day flag.
    fn toggle_all_day(&mut self, doc: &mut AutoCommit) {
        self.cycle_event(doc, |e| {
            if e.all_day {
                // Preserve the date range when returning to a timed event.
                // A midnight exclusive end is a useful all-day boundary but
                // reads as an accidental overnight event on the timed grid.
                let last = e.all_day_last_date();
                e.all_day = false;
                e.end = last.and_time(NaiveTime::from_hms_opt(17, 0, 0).unwrap_or(NaiveTime::MIN));
            } else {
                // Calendar's editor chooses inclusive days; the durable
                // all-day representation is [start, end), as required by
                // RFC 5545.  A timed event ending on its start date therefore
                // becomes one full all-day date, not a zero-length event.
                let last = if e.end.date() > e.start.date() {
                    if e.end.time() == NaiveTime::MIN {
                        e.end.date() - Duration::days(1)
                    } else {
                        e.end.date()
                    }
                } else {
                    e.start.date()
                };
                e.start = e.start.date().and_time(NaiveTime::MIN);
                e.end = (last + Duration::days(1)).and_time(NaiveTime::MIN);
                e.all_day = true;
            }
        });
        self.event_popovers.close();
    }

    /// Apply the date grid's current selection to the start or end of the
    /// event, keeping the clock time and preserving a sane ordering (a start
    /// past the end drags the end along; an end before the start is clamped).
    fn set_event_date(&mut self, which: FormPopover, date: NaiveDate, doc: &mut AutoCommit) {
        self.cycle_event(doc, |e| match which {
            FormPopover::StartDate => {
                if e.all_day {
                    let days = (e.all_day_end_exclusive() - e.start.date())
                        .num_days()
                        .max(1);
                    e.start = date.and_time(NaiveTime::MIN);
                    e.end = (date + Duration::days(days)).and_time(NaiveTime::MIN);
                } else {
                    e.start = date.and_time(e.start.time());
                    if e.end < e.start {
                        e.end = e.start + Duration::hours(1);
                    }
                }
            }
            FormPopover::EndDate => {
                if e.all_day {
                    e.end = (date.max(e.start.date()) + Duration::days(1)).and_time(NaiveTime::MIN);
                } else {
                    e.end = date.and_time(e.end.time());
                    if e.end < e.start {
                        e.end = e.start + Duration::hours(1);
                    }
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
        let written = tilo_files::file_at(&path).and_then(|(dir, name)| {
            tilo_files::blocking::save_document(&dir, &name, ics.as_bytes())
        });
        match written {
            Ok(()) => {
                self.status = Some(format!(
                    "Exported {} events and {} tasks to {}",
                    self.model.events.len(),
                    self.model.tasks.len(),
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
        let payload = match read_text(&path) {
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
        let Some(action) = self.import_action_from_payload(&payload) else {
            self.status = Some(format!("{source} already imported"));
            self.dirty = true;
            return;
        };
        let (added_e, added_t) = match &action {
            crate::actions::CalendarAction::ImportIcs { events, tasks } => {
                (events.len(), tasks.len())
            }
            _ => (0, 0),
        };
        self.perform_action(doc, &action);
        self.model = CalDoc::read_model(doc);
        self.model_heads = doc.get_heads();
        self.model.events.sort_by_key(|e| e.start);
        self.status = Some(format!(
            "Imported {added_e} events and {added_t} tasks from {source}"
        ));
        self.dirty = true;
    }

    fn cycle_event<F: Fn(&mut Event)>(&mut self, doc: &mut AutoCommit, f: F) {
        if let Some(Sel::Event(id)) = self.selection {
            let patch = if let Some(e) = self.model.event_mut(id) {
                let before = e.clone();
                f(e);
                let patch = crate::actions::EventPatch::between(&before, e);
                (!patch.is_empty()).then_some(patch)
            } else {
                None
            };
            if let Some(patch) = patch {
                self.perform_action(doc, &crate::actions::patch_event_action(id, patch));
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
    /// `apply_layout`: the sidebar (docked column or drawer, from the shared
    /// [`ui::widget::SidebarDrawer`]), the header and the active view below
    /// it, and the editor sheet over the window.
    fn relayout(&mut self) {
        let r = self.rect;
        let theme = self.layout_theme.clone();
        let content_x0 = self.sidebar.layout(r, &theme);
        self.content_rect = Rect::new(content_x0, r.y0, r.x1, r.y1);
        let show_toggle = !self.sidebar.visible() || self.sidebar.mode() == SidebarMode::Overlay;
        self.topbar.layout(
            self.content_rect,
            show_toggle,
            self.view,
            self.anchor,
            &theme,
        );

        // Point the active view's scroll container at the fresh geometry so a
        // resize reflows the scroll extents (and re-clamps the offset) every
        // frame rather than laying out once.
        match self.view {
            View::Week | View::Day => self.update_timeline_scroll(),
            View::Agenda => {
                let area = self.view_rect();
                let hidden = self.hidden_calendar_ids();
                self.agenda.layout(
                    area,
                    &AgendaSource {
                        model: &self.model,
                        anchor: self.anchor,
                        hidden_calendars: &hidden,
                        calendar_filter: self.calendar_filter.as_deref(),
                    },
                    &theme,
                );
            }
            View::Month => {}
        }

        self.seed_choices();
        self.layout_sheet();
        if std::mem::take(&mut self.shots_pending_sidebar_menu) && self.sidebar.visible() {
            let anchor = self.sidebar.menu_rect(&theme);
            self.sidebar_menu.open_at(anchor, None);
        }
    }

    /// Point the timeline viewport at its new geometry, seeding the initial
    /// work-day scroll the first time the full day does not fit.
    fn update_timeline_scroll(&mut self) {
        let area = self.view_rect();
        let days = self.timeline_days();
        // Open on the working day; when the current time is past what that
        // shows, open an hour before it instead.
        let working = f64::from(WORKING_DAY_START);
        let focus_hour = if days.contains(&self.today) {
            let now = ui::clock::local_now().naive_local();
            let hour = f64::from(now.hour());
            if hour < working + 8.0 {
                working
            } else {
                hour - 1.0
            }
        } else {
            working
        };
        self.timeline.layout(area, focus_hour);
    }

    /// Names of locally-known events that overlap the edited event's first
    /// occurrence. Calendar does not have a free/busy endpoint, so this is
    /// intentionally limited to the events already saved in this device's
    /// model; it never claims that a remote participant is available.
    fn local_schedule_conflicts(&self, edited: &Event) -> Vec<String> {
        let (start, end) = Self::event_interval(edited, edited.start.date());
        let window_start = start.date() - Duration::days(1);
        let window_end = end.date();
        let mut conflicts = Vec::new();

        for candidate in &self.model.events {
            if candidate.id == edited.id {
                continue;
            }

            let overlaps = if candidate.all_day {
                // An all-day range can start before a timed event's date, so
                // ask the model's recurrence-aware predicate about every day
                // in the edited range instead of comparing only seed values.
                let mut day = start.date();
                let mut found = false;
                while day <= end.date() {
                    if candidate.occurs_on(day) {
                        found = true;
                        break;
                    }
                    day += Duration::days(1);
                }
                found
            } else {
                candidate
                    .occurrences_in(window_start, window_end)
                    .into_iter()
                    .any(|date| {
                        let (other_start, other_end) = Self::event_interval(candidate, date);
                        start < other_end && other_start < end
                    })
            };
            if overlaps {
                conflicts.push(candidate.display_title().to_string());
            }
        }
        conflicts.sort_unstable();
        conflicts.dedup();
        conflicts
    }

    /// Bounds one materialized occurrence in the editor's wall-time domain.
    /// Overrides already moved by CalDAV/ICS are respected; a base recurring
    /// event keeps its duration while moving to the requested occurrence date.
    fn event_interval(event: &Event, occurrence_date: NaiveDate) -> (NaiveDateTime, NaiveDateTime) {
        if event.all_day {
            let start = occurrence_date.and_time(NaiveTime::MIN);
            return (start, start + Duration::days(event.all_day_span_days()));
        }
        let override_for_date = event.occurrence_overrides.iter().find(|override_| {
            override_.original_date == occurrence_date
                || override_
                    .start
                    .is_some_and(|start| start.date() == occurrence_date)
        });
        let start = override_for_date
            .and_then(|override_| override_.start)
            .unwrap_or_else(|| occurrence_date.and_time(event.start.time()));
        let duration = (event.end - event.start).max(Duration::minutes(1));
        let end = override_for_date
            .and_then(|override_| override_.end)
            .unwrap_or(start + duration)
            .max(start + Duration::minutes(1));
        (start, end)
    }

    /// Complete schedule-conflict statement for assistive technology. The
    /// paint surface uses a compact count to retain its geometry at 200% text,
    /// while this semantic value retains every affected event name.
    fn local_schedule_conflict_summary(&self) -> Option<String> {
        let Sel::Event(id) = self.selection? else {
            return None;
        };
        let event = self.model.event(id)?;
        let conflicts = self.local_schedule_conflicts(event);
        (!conflicts.is_empty()).then(|| {
            format!(
                "Schedule conflict: this event overlaps {}.",
                conflicts.join(", ")
            )
        })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the complete local action routing table remains intentionally centralized so every hit and semantic action shares one persistence boundary"
    )]
    fn apply_action(&mut self, action: Action, doc: &mut AutoCommit) {
        match action {
            Action::View(v) => {
                self.view = v;
                // On narrow layouts the switcher lives in the overlay flap;
                // picking a view there returns to the calendar screen and
                // dismisses the flap so the chosen view is visible at once.
                if matches!(self.sidebar.mode(), SidebarMode::Overlay) {
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
                    self.sidebar.set_open(false);
                }
            }
            Action::CloseDetail => {
                self.discard_pristine_draft(doc);
                self.detail_open = false;
                self.selection = None;
                self.blur_editor();
            }
            Action::ToggleSidebar => self.sidebar.toggle_open(),
            Action::DismissSidebar => self.sidebar.set_open(false),
            Action::OpenSearch => self
                .search_opener
                .open(ui::search::SearchRequest::this_app()),
            Action::OpenSidebarMenu => {
                let anchor = self.sidebar.menu_rect(&self.layout_theme);
                self.sidebar_menu.open_at(anchor, None);
            }
            Action::SetCalendarVisible { id, visible } => {
                if let Err(error) = self.invoke_local_calendar_effect(
                    &crate::actions::CalendarOperation::SetCalendarVisible { id, visible },
                    doc,
                ) {
                    self.status = Some(error);
                }
            }
            Action::ToggleAccountCollapsed(id) => {
                self.toggle_account_collapsed(&id);
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
            Action::ToggleSelectedTask => {
                if let Some(Sel::Task(id)) = self.selection {
                    self.toggle_task(id, doc);
                }
            }
            Action::OpenTaskPopover(popover) => {
                self.task_popover = Some(popover);
                self.task_due_picker.set_selected(self.task_due_seed());
            }
            Action::DismissTaskPopover => {
                self.task_popover = None;
                self.choices.close_all();
            }
            Action::SetTaskDue(due) => self.set_selected_task_due(due, doc),
            Action::ShiftTaskDueMonth(delta) => {
                if delta.is_negative() {
                    self.task_due_picker.retreat_month();
                } else {
                    self.task_due_picker.advance_month();
                }
            }
            Action::ToggleAllDay => self.toggle_all_day(doc),
            Action::OpenPopover(which) => self.open_popover(which),
            Action::FocusField(field) => self.focus_field(field),
            Action::DeleteEvent(id) => self.delete_event(id, doc),
            Action::DeleteTask(id) => self.delete_task(id, doc),
            Action::DismissStatus => self.status = None,
            Action::Noop => {}
        }
        // Discrete actions are worth an immediate save; if the action didn't
        // touch the document (view/navigation) the head check in `maybe_persist`
        // makes this a no-op.
        self.force_persist = true;
        self.dirty = true;
    }

    /// A compact semantic projection for the global controls in the painted
    /// hit map. Event cells keep their existing keyboard/search routes; this
    /// adapter covers the shared navigation and creation controls that every
    /// view paints itself.
    #[allow(
        clippy::too_many_lines,
        reason = "one exhaustive projection keeps painted hit actions and their accessible names, roles, and selection state in lockstep"
    )]
    fn a11y_control(&self, action: &Action) -> Option<(String, String, Role, AccessState)> {
        let (target, name, role, state) = match action {
            Action::View(view) => (
                format!("calendar.view:{}", view.label().to_lowercase()),
                format!("{} view", view.label()),
                Role::RadioButton,
                AccessState::new()
                    .focusable(true)
                    .checked(self.view == *view),
            ),
            Action::Today => (
                "calendar.today".into(),
                "Go to today".into(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Action::Prev => (
                "calendar.previous".into(),
                "Previous period".into(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Action::Next => (
                "calendar.next".into(),
                "Next period".into(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Action::NewEvent => (
                "calendar.new-event".into(),
                "New event".into(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Action::ToggleSidebar => (
                "calendar.sidebar".into(),
                if self.sidebar.visible() {
                    "Hide sidebar".into()
                } else {
                    "Show sidebar".into()
                },
                Role::ToggleButton,
                AccessState::new()
                    .focusable(true)
                    .checked(self.sidebar.visible()),
            ),
            Action::OpenSearch => (
                "calendar.search".into(),
                "Search calendar".into(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Action::OpenSidebarMenu => (
                "calendar.sidebar.menu".into(),
                "More".into(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Action::DismissSidebar => (
                "calendar.sidebar.dismiss".into(),
                "Hide sidebar".into(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Action::List(selection) => {
                let (target, label) = match selection {
                    ListSel::Inbox => ("calendar.list:inbox".to_string(), "Inbox".to_string()),
                    ListSel::Today => ("calendar.list:today".to_string(), "Today".to_string()),
                    ListSel::Upcoming => {
                        ("calendar.list:upcoming".to_string(), "Upcoming".to_string())
                    }
                    ListSel::Completed => (
                        "calendar.list:completed".to_string(),
                        "Completed".to_string(),
                    ),
                    ListSel::Project(id) => (
                        format!("{}:open", crate::actions::project_resource_id(*id)),
                        self.model
                            .projects
                            .iter()
                            .find(|project| project.id == *id)
                            .map_or_else(|| "project".to_string(), |project| project.name.clone()),
                    ),
                };
                (
                    target,
                    format!("Show tasks in {label}"),
                    Role::RadioButton,
                    AccessState::new()
                        .focusable(true)
                        .checked(self.list_sel == *selection),
                )
            }
            Action::SetCalendarVisible { id, visible: _ } => {
                let calendar = self
                    .accounts
                    .all_calendars()
                    .find_map(|(_, calendar)| (calendar.id == *id).then_some(calendar));
                let (name, current_visible) = calendar.map_or_else(
                    || ("calendar".to_string(), false),
                    |calendar| (calendar.name.clone(), calendar.visible),
                );
                (
                    format!("{}:visibility", crate::actions::calendar_resource_id(id)),
                    format!("Show calendar {name}"),
                    Role::Checkbox,
                    AccessState::new().focusable(true).checked(current_visible),
                )
            }
            Action::ToggleAccountCollapsed(id) => {
                let account = self
                    .accounts
                    .accounts
                    .iter()
                    .find(|account| account.id == *id);
                let (name, expanded) = account.map_or_else(
                    || ("calendar account".to_string(), false),
                    |account| (account.label().to_string(), !account.collapsed),
                );
                (
                    format!("calendar.account:{id}:expanded"),
                    format!("{name} calendars"),
                    Role::Button,
                    AccessState::new().focusable(true).expanded(expanded),
                )
            }
            Action::CloseDetail => (
                "calendar.back".into(),
                "Done".into(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Action::Sync => (
                "calendar.sync".into(),
                if self
                    .status
                    .as_deref()
                    .is_some_and(|status| status.starts_with("Sync failed"))
                {
                    "Retry calendar synchronization".into()
                } else {
                    "Sync calendars".into()
                },
                Role::Button,
                AccessState::new().focusable(true).disabled(self.syncing),
            ),
            Action::Export => (
                "calendar.export".into(),
                "Export calendar".into(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Action::Import => (
                "calendar.import".into(),
                "Import calendar".into(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Action::PickDate(day) | Action::OpenDay(day) => (
                format!("calendar.date:{day}:open"),
                format!("Open {}", date::fmt_day_long(*day)),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Action::MiniMonth(MiniMonthOutcome::SelectDate(day)) => (
                format!("calendar.mini-date:{day}"),
                format!("Select {}", date::fmt_day_long(*day)),
                Role::Button,
                AccessState::new()
                    .focusable(true)
                    .selected(self.anchor == *day),
            ),
            Action::MiniMonth(MiniMonthOutcome::ShiftMonth(months)) => (
                format!("calendar.mini-month:{months}"),
                if *months < 0 {
                    "Previous month".into()
                } else {
                    "Next month".into()
                },
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Action::OpenEvent(id) => {
                let label = self.model.event(*id).map_or_else(
                    || "event".to_string(),
                    |event| event.display_title().to_string(),
                );
                (
                    format!("{}:open", crate::actions::event_resource_id(*id)),
                    format!("Open event {label}"),
                    Role::Button,
                    AccessState::new().focusable(true),
                )
            }
            Action::OpenTask(id) => {
                let label = self
                    .model
                    .task(*id)
                    .map_or_else(|| "task".to_string(), |task| task.title.clone());
                (
                    format!("{}:open", crate::actions::task_resource_id(*id)),
                    format!("Open task {label}"),
                    Role::Button,
                    AccessState::new().focusable(true),
                )
            }
            Action::ToggleTask(_) | Action::ToggleSelectedTask => {
                let id = match action {
                    Action::ToggleTask(id) => Some(*id),
                    Action::ToggleSelectedTask => self.selected_task(),
                    _ => None,
                };
                let id = id?;
                let task = self.model.task(id);
                let (label, done) = task.map_or_else(
                    || ("task".to_string(), false),
                    |task| (task.title.clone(), task.done),
                );
                (
                    format!("{}:toggle", crate::actions::task_resource_id(id)),
                    format!(
                        "Mark task {label} {}",
                        if done { "incomplete" } else { "complete" }
                    ),
                    Role::Checkbox,
                    AccessState::new().focusable(true).checked(done),
                )
            }
            Action::DeleteEvent(id) => (
                format!("{}:delete", crate::actions::event_resource_id(*id)),
                "Delete event".into(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Action::DeleteTask(id) => (
                format!("{}:delete", crate::actions::task_resource_id(*id)),
                "Delete task".into(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Action::ToggleAllDay => (
                "calendar.event:all-day".into(),
                "All-day event".into(),
                Role::Switch,
                AccessState::new().focusable(true).checked(matches!(
                    self.selection,
                    Some(Sel::Event(id)) if self.model.event(id).is_some_and(|event| event.all_day)
                )),
            ),
            Action::FocusField(FormField::Title) => (
                "calendar.event:title".into(),
                "Edit event title".into(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Action::FocusField(FormField::Location) => (
                "calendar.event:location".into(),
                "Edit event location".into(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Action::FocusField(FormField::Invite) => (
                "calendar.event:invite".into(),
                "Invite attendee by email".into(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Action::OpenPopover(FormPopover::StartDate) => (
                "calendar.event:start-date".into(),
                "Choose event start date".into(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Action::OpenPopover(FormPopover::EndDate) => (
                "calendar.event:end-date".into(),
                "Choose event end date".into(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Action::OpenPopover(FormPopover::StartTime) => (
                "calendar.event:start-time".into(),
                "Choose event start time".into(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Action::OpenPopover(FormPopover::EndTime) => (
                "calendar.event:end-time".into(),
                "Choose event end time".into(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Action::OpenTaskPopover(TaskPopover::Due) => (
                "calendar.task.due".into(),
                "Choose task due date".into(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Action::DismissTaskPopover => (
                "calendar.task.picker.dismiss".into(),
                "Dismiss task picker".into(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Action::ShiftTaskDueMonth(delta) => (
                format!("calendar.task.due.month:{delta}"),
                if *delta < 0 {
                    "Previous due-date month".into()
                } else {
                    "Next due-date month".into()
                },
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Action::SetTaskDue(due) => (
                format!(
                    "calendar.task.due:{}",
                    due.map_or_else(|| "none".into(), |day| day.to_string())
                ),
                due.map_or_else(
                    || "Clear task due date".into(),
                    |day| format!("Set task due date to {}", date::fmt_day_long(day)),
                ),
                Role::RadioButton,
                AccessState::new().focusable(true).checked(
                    self.selected_task()
                        .and_then(|id| self.model.task(id))
                        .and_then(|task| task.due)
                        == *due,
                ),
            ),
            _ => return None,
        };
        Some((target, name, role, state))
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

    fn event_in_selected_calendar(&self, event: &Event) -> bool {
        self.calendar_filter.as_ref().is_none_or(|calendar| {
            if calendar == "local" {
                event.calendar_id.is_none()
            } else {
                event.calendar_id.as_deref() == Some(calendar)
            }
        })
    }

    /// Events occurring on `date`, minus any whose source calendar is hidden.
    /// This is the single filter every view draws through, so unchecking a
    /// calendar hides its already-synced events immediately.
    fn visible_events_on(&self, date: NaiveDate) -> Vec<&Event> {
        let hidden = self.hidden_calendar_ids();
        self.model
            .events_on(date)
            .into_iter()
            .filter(|event| self.event_in_selected_calendar(event) && event.visible_under(&hidden))
            .collect()
    }

    // ====================================================================
    // Drawing — each helper takes `&self` and returns its click targets.
    // ====================================================================

    fn view_rect(&self) -> Rect {
        self.topbar.view_rect()
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

    /// The open popover owns input first: the picker itself, then (if it
    /// didn't consume the event) a left click resolved against the hit map
    /// — the dismiss scrim, a calendar row, or a popover guard (part of
    /// [`Widget::handle_event`]).
    #[allow(
        clippy::too_many_lines,
        reason = "internal event-handling split of an over-length function"
    )]
    /// Route input to the editor's dropdowns: an open list owns every
    /// event; a press on a closed dropdown opens it. Picks are applied at
    /// once. Returns whether the event was consumed.
    fn handle_choice_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        let task = matches!(self.selection, Some(Sel::Task(_)));
        let pressed = matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            }
        );
        let pointer = self.last_mouse;
        let body = self.modal.as_ref().map_or(Rect::ZERO, |m| m.body);
        let mut consumed = false;
        let open = self
            .choices
            .shown_mut(task)
            .into_iter()
            .any(|c| c.dropdown.is_open());
        for choice in self.choices.shown_mut(task) {
            let route = if open {
                choice.dropdown.is_open()
            } else {
                matches!(event, WindowEvent::CursorMoved { .. })
                    || (pressed && choice.rect.contains(pointer) && body.contains(pointer))
            };
            if !route {
                continue;
            }
            Widget::handle_event(&mut choice.dropdown, event, ctx);
            if open || pressed {
                consumed = true;
                *ctx.handled = true;
            }
        }
        if consumed {
            self.commit_choices(ctx.state, ctx.path);
        }
        consumed
    }

    fn handle_popover_priority(&mut self, input: &mut InputContext<'_>) {
        self.handle_popover_event(input);
        if *input.handled {
            self.dirty = true;
            return;
        }
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = input.event
            && let Some(a) = self.hit_action_at(self.last_mouse)
        {
            self.apply_action(a, input.state);
            *input.handled = true;
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
        let field_focused = self.title_input.is_focused()
            || self.location_input.is_focused()
            || self.invite_input.is_focused();
        let notes_focused = editor_active && self.notes_editor.is_focused();

        if matches!(logical_key, Key::Named(NamedKey::Escape))
            && self.handle_keyboard_escape(state, field_focused, notes_focused, handled)
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

    fn handle_detail_scroll(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        taffy: &TaffyTree<()>,
    ) -> bool {
        let WindowEvent::MouseWheel { delta, .. } = event else {
            return false;
        };
        if self.detail_scroll_max <= 0.0 || !self.detail_rect.contains(self.last_mouse) {
            return false;
        }
        let delta = match delta {
            MouseScrollDelta::LineDelta(_, y) => f64::from(-y * 48.0),
            MouseScrollDelta::PixelDelta(point) => -point.y / scale_factor,
        };
        let before = self.detail_scroll_y;
        self.detail_scroll_y = (before + delta).clamp(0.0, self.detail_scroll_max);
        if (before - self.detail_scroll_y).abs() <= f64::EPSILON {
            return false;
        }
        self.shots_detail_scroll_to_end = false;
        self.relayout();
        if let Some(node) = self.editor_node {
            self.notes_editor
                .apply_layout(taffy, node, self.notes_origin);
        }
        self.dirty = true;
        true
    }

    /// Escape: cancel a live timeline drag first (it commits nothing), then
    /// blur a focused text control, then dismiss the overlay flap, then pop the
    /// narrow detail screen, then close the detail. Split out of
    /// `handle_keyboard` to keep that under the line cap.
    fn handle_keyboard_escape(
        &mut self,
        state: &mut AutoCommit,
        field_focused: bool,
        notes_focused: bool,
        handled: &mut bool,
    ) -> bool {
        if self.timeline.cancel_drag() {
            *handled = true;
            self.dirty = true;
            return true;
        }
        if field_focused || notes_focused {
            self.title_input.set_focus(false);
            self.location_input.set_focus(false);
            self.invite_input.set_focus(false);
            self.notes_editor.set_focus(false);
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
        if self.detail_open {
            self.discard_pristine_draft(state);
            self.detail_open = false;
            self.selection = None;
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
        clippy::too_many_lines,
        reason = "internal event-handling split of an over-length function"
    )]
    fn handle_mouse_click(
        &mut self,
        input: &mut InputContext<'_>,
        form_active: bool,
        in_notes: bool,
    ) -> bool {
        let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = input.event
        else {
            return false;
        };
        let p = self.last_mouse;
        // A click inside a form text field focuses it and lands the caret.
        if form_active && self.form.title.contains(p) {
            self.location_input.set_focus(false);
            self.invite_input.set_focus(false);
            self.notes_editor.set_focus(false);
            self.title_input.set_focus(true);
            self.title_input.handle_event(
                input.event,
                &mut ui::widget::EventCtx {
                    scale_factor: input.scale_factor,
                    modifiers: &input.modifiers,
                    state: input.state,
                    path: input.path,
                    handled: input.handled,
                    focus_target: input.focus_target,
                    node_id: input.node_id,
                    taffy: input.taffy,
                },
            );
            *input.handled = true;
            self.dirty = true;
            return true;
        }
        if form_active && self.form.location.contains(p) {
            self.title_input.set_focus(false);
            self.invite_input.set_focus(false);
            self.notes_editor.set_focus(false);
            self.location_input.set_focus(true);
            self.location_input.handle_event(
                input.event,
                &mut ui::widget::EventCtx {
                    scale_factor: input.scale_factor,
                    modifiers: &input.modifiers,
                    state: input.state,
                    path: input.path,
                    handled: input.handled,
                    focus_target: input.focus_target,
                    node_id: input.node_id,
                    taffy: input.taffy,
                },
            );
            *input.handled = true;
            self.dirty = true;
            return true;
        }
        if form_active && self.form.invite.contains(p) {
            self.title_input.set_focus(false);
            self.location_input.set_focus(false);
            self.notes_editor.set_focus(false);
            self.invite_input.set_focus(true);
            self.invite_input.handle_event(
                input.event,
                &mut ui::widget::EventCtx {
                    scale_factor: input.scale_factor,
                    modifiers: &input.modifiers,
                    state: input.state,
                    path: input.path,
                    handled: input.handled,
                    focus_target: input.focus_target,
                    node_id: input.node_id,
                    taffy: input.taffy,
                },
            );
            *input.handled = true;
            self.dirty = true;
            return true;
        }
        // Clicks inside the notes region go to the editor (focus + caret).
        if in_notes {
            self.title_input.set_focus(false);
            self.location_input.set_focus(false);
            self.invite_input.set_focus(false);
            if let Some(n) = self.editor_node {
                self.notes_editor.handle_event(
                    input.event,
                    &mut ui::widget::EventCtx {
                        scale_factor: input.scale_factor,
                        modifiers: &input.modifiers,
                        state: input.state,
                        path: input.path,
                        handled: input.handled,
                        focus_target: input.focus_target,
                        node_id: n,
                        taffy: input.taffy,
                    },
                );
            }
            self.dirty = true;
            return true;
        }
        // Otherwise resolve against the hit-map (topmost last).
        if let Some(a) = self.hit_action_at(p) {
            self.title_input.set_focus(false);
            self.location_input.set_focus(false);
            self.invite_input.set_focus(false);
            self.notes_editor.set_focus(false);
            self.apply_action(a, input.state);
            *input.handled = true;
            return true;
        }
        false
    }

    /// Forward typing (and other events) to a focused form text field so
    /// edits land, mirroring the change into the event via
    /// [`Self::flush_form_edits`] (part of [`Widget::handle_event`]).
    /// Returns `true` if the caller should stop dispatching.
    fn handle_form_field_input(&mut self, input: &mut InputContext<'_>) -> bool {
        if self.title_input.is_focused() {
            self.title_input.handle_event(
                input.event,
                &mut ui::widget::EventCtx {
                    scale_factor: input.scale_factor,
                    modifiers: &input.modifiers,
                    state: input.state,
                    path: input.path,
                    handled: input.handled,
                    focus_target: input.focus_target,
                    node_id: input.node_id,
                    taffy: input.taffy,
                },
            );
            self.flush_form_edits(input.state);
            self.dirty = true;
            return *input.handled;
        } else if self.location_input.is_focused() {
            self.location_input.handle_event(
                input.event,
                &mut ui::widget::EventCtx {
                    scale_factor: input.scale_factor,
                    modifiers: &input.modifiers,
                    state: input.state,
                    path: input.path,
                    handled: input.handled,
                    focus_target: input.focus_target,
                    node_id: input.node_id,
                    taffy: input.taffy,
                },
            );
            self.flush_form_edits(input.state);
            self.dirty = true;
            return *input.handled;
        } else if self.invite_input.is_focused() {
            self.invite_input.handle_event(
                input.event,
                &mut ui::widget::EventCtx {
                    scale_factor: input.scale_factor,
                    modifiers: &input.modifiers,
                    state: input.state,
                    path: input.path,
                    handled: input.handled,
                    focus_target: input.focus_target,
                    node_id: input.node_id,
                    taffy: input.taffy,
                },
            );
            self.submit_invite(input.state);
            self.dirty = true;
            return *input.handled;
        }
        false
    }

    /// Apply a resource-authored import once we have the mutable document that
    /// the widget operation hook intentionally does not receive. The completion
    /// update is deferred until this write has crossed `ActionHost`, so callers
    /// never observe a successful import that only touched an ephemeral clone.
    fn flush_pending_resource_import(&mut self, state: &mut AutoCommit) {
        let Some(action) = self.pending_resource_import.take() else {
            return;
        };
        let result = self.actions.try_dispatch_with_context(
            state,
            crate::actions::CALENDAR_ACTION_TYPE,
            &action,
            crate::actions::history_context(&action),
        );
        if result.is_ok() {
            self.force_persist = true;
            self.status = Some("Imported calendar items from the latest ICS file".to_string());
        } else if let Err(error) = &result {
            self.status = Some(format!("Import failed: {error}"));
        }
        self.dirty = true;
        if let Some(invocation) = self.resource_import_invocation.take() {
            self.resource_import_result = Some(match result {
                Ok(()) => tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput::default(),
                },
                Err(error) => tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "calendar.import_failed".into(),
                        message: error.to_string(),
                        details: None,
                        retryable: false,
                    },
                },
            });
        }
    }
}

impl Default for CalendarApp {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for CalendarApp {
    ui::provides!(
        dyn ui::undo::CommittedActions,
        dyn ui::widget::controls::ActionControls,
        dyn ui::resources::PublishesResources,
        dyn ui::search::capability::SearchSource
    );

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return ui::widget::start_in_children(self, op, doc, path);
        };
        if let Some(started) = self.admit_resource_operation_with_document(
            request.invocation,
            request.kind,
            request.type_id,
            request.payload,
            doc,
            path,
        )? {
            return Ok(Some(started));
        }
        Ok(self
            .answer_resource_operation_v2(
                request.kind,
                request.type_id,
                request.payload,
                doc,
                path,
            )?
            .map(ui::widget::ResourceOperationStart::Completed))
    }

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::CALENDAR_OPERATION_TYPE]
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        _cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if self
            .resource_sync_result
            .as_ref()
            .is_some_and(|update| update.invocation() == invocation)
        {
            self.resource_sync_result.take()
        } else if self
            .resource_import_result
            .as_ref()
            .is_some_and(|update| update.invocation() == invocation)
        {
            self.resource_import_result.take()
        } else {
            None
        }
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, path) = ui::doc::document(state, path);
        if !self.seeded {
            // Live app: adopt the user's persisted calendar (empty on a fresh
            // machine). Demo/test builds pass no path and stay in-memory.
            if let Some(p) = &self.data_path
                && let Some(mut loaded) = crate::store::load(p)
            {
                let _ = state.merge_incoming(&mut loaded);
            }
            // Seed sample content ONLY in the demo build, and only when the
            // document is still empty. This stays a direct `CalDoc::seed` plus
            // `seed_demo_notes`, not an action. `AdoptRemoteEvent` round-trips
            // through `EventData`, which keeps only reminders of -1, 10, 30, or
            // 60 minutes; the selected "Design review" event and "Quarterly
            // planning" use 15, and the detail chip renders that. `UpsertEvent`
            // also enqueues a push when `calendar_id` is `Some`. The curated
            // notes are heading blocks filled into empty note maps.
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
        self.flush_pending_resource_import(state);
        // Drain a finished background CalDAV pull, if one landed since the last
        // frame, before binding the editor so a freshly-synced selection reads
        // back its notes.
        // Shots-only: spawn a fresh event to capture the New-Event form.
        if self.shots_pending_new_event {
            self.shots_pending_new_event = false;
            self.new_event(state);
            if self.shots_suppress_status {
                self.status = None;
            }
        }
        self.poll_sync(state);
        // Account/calendar configuration is Settings-owned rather than CRDT
        // state. Observe it independently so a rename, visibility change, or
        // collection replacement invalidates Ctrl+K even when document heads
        // stay put.
        self.observe_calendar_projection();
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
            self.model_heads.clone_from(&heads);
        }
        self.ensure_bound(state);
        // Seed the form's title/location/time widgets from the current event
        // whenever the selection changes.
        self.ensure_form_bound();
        self.commit_choices(state, path);
        if let Some(which) = self.shots_pending_popover.take() {
            self.open_popover(which);
        }
        if self.shots_pending_calendar_menu && self.form_event.is_some() {
            self.shots_pending_calendar_menu = false;
            self.seed_choices();
            self.choices.calendar.dropdown.open_menu();
        }
        self.notes_editor.sync(state, path);
        // Binding an editor, applying a dropdown selection, or accepting a
        // note delta can hydrate the document during this same frame. Re-read
        // heads after those paths before deriving search state, then project
        // every note only when the revision really advanced. An idle frame is
        // therefore O(1), not a corpus walk over every event/task note.
        let post_hydration_heads = state.get_heads();
        if post_hydration_heads != self.model_heads {
            self.model = CalDoc::read_model(state);
            self.revalidate_selection();
            self.model_heads.clone_from(&post_hydration_heads);
        }
        let document_changed = self.search_catalog_heads != post_hydration_heads;
        let calendar_projection_changed =
            self.search_catalog_projection_revision != self.calendar_projection_revision;
        if document_changed || calendar_projection_changed {
            self.rebuild_resource_index();
        }
        if document_changed {
            self.refresh_note_search(state);
        }
        if document_changed || calendar_projection_changed {
            self.refresh_search_catalog(&post_hydration_heads);
        }
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
        self.layout_theme = theme.clone();
        self.timeline.set_layout_theme(theme);
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
        if let Some(n) = self.editor_node {
            // The sheet grows with the notes: take their height from this
            // layout before the sheet is solved.
            self.notes_content_h = taffy
                .layout(n)
                .map_or(0.0, |layout| f64::from(layout.size.height));
        }
        self.relayout();
        if let Some(n) = self.editor_node {
            self.notes_editor.apply_layout(taffy, n, self.notes_origin);
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
        let t = transform;
        // No ground of our own: the framework painted the window ground.
        let mut hits = Vec::new();
        if self.content_rect.area() > 0.0 {
            let drawer = self.sidebar.drawer().clone();
            hits.extend(
                self.topbar
                    .draw(
                        &mut PaintCtx::new(scene, t, text_renderer, theme),
                        (self.view, self.anchor),
                        self.last_mouse,
                        &drawer,
                    )
                    .iter()
                    .map(|(rect, action)| {
                        let action = match action {
                            TopbarAction::View(view) => Action::View(*view),
                            TopbarAction::Today => Action::Today,
                            TopbarAction::Previous => Action::Prev,
                            TopbarAction::Next => Action::Next,
                            TopbarAction::NewEvent => Action::NewEvent,
                            TopbarAction::ShowSidebar => Action::ToggleSidebar,
                            TopbarAction::Search => Action::OpenSearch,
                        };
                        (*rect, action)
                    }),
            );
            let area = self.view_rect();
            let mut ctx = PaintCtx::new(scene, t, text_renderer, theme);
            match self.view {
                View::Month => hits.extend(self.draw_month(&mut ctx, area)),
                View::Week | View::Day => {
                    let days = self.timeline_days();
                    let events = self.timeline_render_events(&days);
                    let now = ui::clock::local_now().naive_local();
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
                            .draw_timeline(&mut ctx, area, &model)
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
                                &mut ctx,
                                area,
                                &AgendaSource {
                                    model: &self.model,
                                    anchor: self.anchor,
                                    hidden_calendars: &hidden,
                                    calendar_filter: self.calendar_filter.as_deref(),
                                },
                                self.today,
                                self.last_mouse,
                            )
                            .into_iter()
                            .map(|(rect, action)| {
                                let action = match action {
                                    AgendaAction::OpenEvent(id) => Action::OpenEvent(id),
                                    AgendaAction::OpenTask(id) => Action::OpenTask(id),
                                    AgendaAction::ToggleTask(id) => Action::ToggleTask(id),
                                    AgendaAction::NewEvent => Action::NewEvent,
                                };
                                (rect, action)
                            }),
                    );
                }
            }
        }

        // The sidebar last, so a drawer and its scrim float over the view.
        hits.extend(self.draw_sidebar(&mut PaintCtx::new(scene, t, text_renderer, theme)));
        self.hits = hits;
    }

    /// The window's top layer: the editor sheet and its pickers, the
    /// sidebar's overflow menu, and transient status.
    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.draw_sheet(scene, transform, text_renderer, theme, presence);
        if self.sidebar_menu.is_open() {
            self.sidebar_menu.set_viewport(self.rect);
            Widget::draw_overlay(
                &mut self.sidebar_menu,
                scene,
                transform,
                text_renderer,
                theme,
                presence,
            );
        }
        self.draw_status(scene, transform, text_renderer, theme);
    }

    // This is the widget system's event-dispatch contract, implemented by
    // every widget; every parameter is independently meaningful (the raw
    // event, scaling, modifiers, CRDT handle+path, two out-params, and the
    // node's own identity/layout).
    #[allow(
        clippy::too_many_lines,
        reason = "the widget-system dispatcher deliberately keeps input precedence visible: modal popovers, shared task controls, timeline, keyboard, hit map, fields, then notes"
    )]
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // Track the pointer in logical coordinates.
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_mouse =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            self.dirty = true;
        }

        // The sidebar's overflow menu owns input while it is open.
        if self.sidebar_menu.is_open() {
            Widget::handle_event(&mut self.sidebar_menu, event, ctx);
            match self.sidebar_menu.take_selection() {
                Some(0) => self.apply_action(Action::Export, ctx.state),
                Some(1) => self.apply_action(Action::Import, ctx.state),
                _ => {}
            }
            self.dirty = true;
            return;
        }

        // The notes editor is only live while the editor sheet is open.
        let editor_active = self.detail_open && self.detail_rect.area() > 0.0;
        let form_active = editor_active && matches!(self.selection, Some(Sel::Event(_)));
        let popover_open = form_active && self.event_popovers.is_open();
        // Bounded to the notes column of the detail card/page so a click below
        // or beside a floating modal isn't swallowed by the editor.
        let in_notes = editor_active && self.notes_region().contains(self.last_mouse);

        // --- Open popover owns input first --------------------------------
        if popover_open {
            self.handle_popover_priority(&mut InputContext {
                event,
                scale_factor: ctx.scale_factor,
                modifiers: *ctx.modifiers,
                state: ctx.state,
                path: ctx.path,
                handled: ctx.handled,
                focus_target: ctx.focus_target,
                node_id: ctx.node_id,
                taffy: ctx.taffy,
            });
            return;
        }

        if editor_active && self.handle_choice_event(event, ctx) {
            self.dirty = true;
            return;
        }

        if editor_active && self.handle_detail_scroll(event, ctx.scale_factor, ctx.taffy) {
            *ctx.handled = true;
            return;
        }

        // --- Week/day timeline + agenda: scroll and drag ------------------
        // Owns mouse input inside the scrolling views (wheel, scrollbar,
        // drag-to-create / drag-to-move) before the generic hit-map path.
        let timeline_handled = self.handle_timeline_input(&mut InputContext {
            event,
            scale_factor: ctx.scale_factor,
            modifiers: *ctx.modifiers,
            state: ctx.state,
            path: ctx.path,
            handled: ctx.handled,
            focus_target: ctx.focus_target,
            node_id: ctx.node_id,
            taffy: ctx.taffy,
        });
        if timeline_handled {
            self.dirty = true;
            return;
        }

        if self.handle_keyboard(event, *ctx.modifiers, ctx.state, ctx.handled) {
            return;
        }

        if self.handle_mouse_click(
            &mut InputContext {
                event,
                scale_factor: ctx.scale_factor,
                modifiers: *ctx.modifiers,
                state: ctx.state,
                path: ctx.path,
                handled: ctx.handled,
                focus_target: ctx.focus_target,
                node_id: ctx.node_id,
                taffy: ctx.taffy,
            },
            form_active,
            in_notes,
        ) {
            return;
        }

        // Forward typing (and other events) to a focused form text field so
        // edits land, mirroring the change into the event via `flush_form_edits`.
        if self.handle_form_field_input(&mut InputContext {
            event,
            scale_factor: ctx.scale_factor,
            modifiers: *ctx.modifiers,
            state: ctx.state,
            path: ctx.path,
            handled: ctx.handled,
            focus_target: ctx.focus_target,
            node_id: ctx.node_id,
            taffy: ctx.taffy,
        }) {
            return;
        }

        // Forward remaining events (key typing, mouse move) to the editor
        // when the notes panel is active so editing works.
        if editor_active && let Some(n) = self.editor_node {
            self.notes_editor
                .handle_event(event, &mut ctx.reborrow(ctx.path, n));
        }
        self.dirty = true;
    }

    fn name(&self) -> &'static str {
        "CalendarApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<AccessNode> {
        let mut seen = HashSet::new();
        let controls = self
            .hits
            .iter()
            .filter_map(|(rect, action)| {
                let (target, name, role, state) = self.a11y_control(action)?;
                seen.insert(target.clone()).then(|| {
                    AccessNode::new(role)
                        .with_name(name)
                        .with_state(state)
                        .with_bounds(*rect)
                        .with_action_target(target)
                })
            })
            .collect();
        let mut root = AccessNode::new(Role::Window)
            .with_name("Calendar")
            .with_bounds(self.rect)
            .with_children(controls);
        if self.syncing {
            root.children.push(
                AccessNode::new(Role::StaticText)
                    .with_name("Synchronizing calendars")
                    .with_live(ui::a11y::Politeness::Polite),
            );
        }
        if let Some(status) = &self.status {
            root.children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(status.clone())
                    .with_live(ui::a11y::Politeness::Polite),
            );
        }
        if self.detail_open && matches!(self.selection, Some(Sel::Event(_))) {
            root.children.push(
                AccessNode::new(Role::StaticText)
                    .with_name("Event editor. Changes save automatically."),
            );
            if let Some(conflict) = self.local_schedule_conflict_summary() {
                root.children
                    .push(AccessNode::new(Role::StaticText).with_name(conflict));
            }
        }
        Some(root)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &A11yAction,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        let (state, _) = ui::doc::document(state, path);
        if !matches!(action, A11yAction::Activate | A11yAction::ToggleCheck) {
            return false;
        }
        let hit = self.hits.iter().find_map(|(_, candidate)| {
            self.a11y_control(candidate)
                .filter(|(candidate_target, _, _, _)| candidate_target == target)
                .map(|_| candidate.clone())
        });
        if let Some(hit) = hit {
            self.apply_action(hit, state);
            true
        } else {
            false
        }
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || self.notes_editor.needs_redraw()
            || self.title_input.needs_redraw()
            || self.location_input.needs_redraw()
            || self.invite_input.needs_redraw()
            || self.timeline.needs_redraw()
            || self.agenda.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.notes_editor.mark_drawn();
        self.title_input.mark_drawn();
        self.location_input.mark_drawn();
        self.invite_input.mark_drawn();
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
        self.syncing
            || (self.pending_launch_ics.is_some() && !self.launch_import_emitted)
            || self.timeline.needs_animation_frame()
            || self.agenda.needs_animation_frame()
    }

    fn is_layout_dirty(&self) -> bool {
        self.dirty
            || self.notes_editor.needs_redraw()
            || self.title_input.needs_redraw()
            || self.location_input.needs_redraw()
            || self.invite_input.needs_redraw()
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

impl ui::search::capability::SearchSource for CalendarApp {
    /// Fold the calendar's events and tasks into the unified Ctrl+K palette.
    /// Every hit addresses the matching resource's exported `open` view action.
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(self.search_provider.clone())]
    }
}

impl ui::resources::PublishesResources for CalendarApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        if self.actions.is_hosted() {
            return self.pending_launch_resource().into_iter().collect();
        }
        let mut resources = self.publish_resources(doc);
        resources.extend(self.pending_launch_resource());
        resources
    }
    fn action_directory_resource_page(
        &self,
        doc: &AutoCommit,
        cursor: &[u8],
        max_results: usize,
    ) -> Option<ui::widget::ActionDirectoryResourcePage> {
        // The framework root already publishes the calendar. The directory page
        // keeps the same pending-import extra as `resources`, never a second root.
        if self.actions.is_hosted() {
            let resources = if cursor.is_empty() && max_results > 0 {
                self.pending_launch_resource().into_iter().collect()
            } else {
                Vec::new()
            };
            return Some(ui::widget::ActionDirectoryResourcePage {
                resources,
                next_cursor: Vec::new(),
                exhausted: true,
            });
        }
        // The pull directory is the published calendar root. Event and task
        // rows stay on the revisioned search provider; walking them here would
        // materialise the whole history just to list stable root actions.
        let mut root = ui::tilo_resource::publish::<crate::resource::Calendar>(
            "calendar.calendar",
            "Calendar",
            &crate::doc::CalDoc::read_model(doc),
        );
        root.children.clear();
        Some(ui::widget::ActionDirectoryResourcePage {
            resources: (cursor.is_empty() && max_results > 0)
                .then_some(root)
                .into_iter()
                .collect(),
            next_cursor: Vec::new(),
            exhausted: true,
        })
    }
    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        if id == "calendar.pending-import" {
            return self.pending_launch_resource();
        }
        if self.actions.is_hosted() && id == "calendar.calendar" {
            return None;
        }
        self.publish_resource(doc, id)
    }
}

impl ui::widget::controls::ActionControls for CalendarApp {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        if let Some(search) = self.search_opener.take() {
            return Some(search);
        }
        if !self.launch_import_emitted
            && let Some(resource) = self.pending_launch_resource()
            && let Some(spec) = resource.actions.first()
            && let Ok(binding) =
                tilo_actions::ActionBinding::new(&resource, spec, |()| serde_json::Map::new())
            && let Ok(intent) = binding.invoke(())
        {
            self.launch_import_emitted = true;
            return Some(intent);
        }
        self.children_mut()
            .into_iter()
            .find_map(ui::widget::controls::take_control_invocation)
    }
    fn on_control_invocation_result(
        &mut self,
        address: &tilo_actions::ActionAddress,
        result: &Result<ui::widget::ResourceOperationStart, tilo_actions::DoorError>,
    ) {
        if address.resource == "calendar.pending-import"
            && address.action == "finalize"
            && let Err(error) = result
        {
            self.pending_launch_ics = None;
            self.launch_import_emitted = false;
            self.status = Some(format!("Import failed: {error}"));
            self.dirty = true;
        }
        for child in self.children_mut() {
            ui::widget::controls::on_control_invocation_result(child, address, result);
        }
    }
}

impl ui::undo::CommittedActions for CalendarApp {
    /// Hand the host the heads captured before an on-screen affordance dispatched
    /// its calendar action, so it records one undo boundary for it (see
    /// `Self::perform_action`). Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        // A notes edit dispatches through the editor's field; everything else
        // through the calendar's own host. One event does one or the other.
        let notes = ui::undo::CommittedActions::take_committed_action(&mut self.notes_editor);
        let calendar = self.actions.take_committed();
        notes.or(calendar)
    }
    /// Typing a word in the notes is one undo step.
    fn keeps_undo_step_open(&self) -> bool {
        ui::undo::CommittedActions::keeps_undo_step_open(&self.notes_editor)
    }
}

impl CalendarApp {
    fn admit_resource_operation_with_document(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if type_id == crate::actions::CALENDAR_OPERATION_TYPE
            && serde_json::from_slice::<crate::actions::CalendarOperation>(payload)
                .map_err(|error| error.to_string())?
                == crate::actions::CalendarOperation::FinalizeLaunchImport
        {
            if kind != tilo_actions::OperationKind::Effect {
                return Err("Calendar import completion requires an Effect invocation".into());
            }
            let source = self
                .pending_launch_ics
                .take()
                .ok_or("No calendar launch import is pending")?;
            self.launch_import_emitted = false;
            // Admission is fresh and the mutable host document is authoritative,
            // including edits received since the last widget sync.
            self.model = CalDoc::read_model(doc);
            if let Some(action) = self.import_action_from_payload(&source) {
                self.actions
                    .try_dispatch_with_context(
                        doc,
                        crate::actions::CALENDAR_ACTION_TYPE,
                        &action,
                        crate::actions::history_context(&action),
                    )
                    .map_err(|error| error.to_string())?;
            }
            self.status = Some("Imported calendar file".into());
            self.dirty = true;
            return Ok(Some(ui::widget::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            )));
        }
        self.admit_resource_operation(invocation, kind, type_id, payload, doc, path)
    }

    fn answer_resource_operation_v2(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<tilo_actions::ActionOutput>, String> {
        if kind == tilo_actions::OperationKind::Effect
            && type_id == crate::actions::CALENDAR_OPERATION_TYPE
        {
            let operation: crate::actions::CalendarOperation =
                serde_json::from_slice(payload).map_err(|error| error.to_string())?;
            if let crate::actions::CalendarOperation::PrepareInvitationInMail {
                event_id,
                attendee_id,
            } = operation
            {
                let event_id = event_id
                    .parse::<Id>()
                    .map_err(|_| "invalid event id".to_string())?;
                let attendee_id = attendee_id
                    .parse::<Id>()
                    .map_err(|_| "invalid attendee id".to_string())?;
                let event = self
                    .model
                    .event(event_id)
                    .ok_or_else(|| "event no longer exists".to_string())?;
                let attendee = event
                    .attendees
                    .iter()
                    .find(|attendee| attendee.id == attendee_id)
                    .ok_or_else(|| "attendee no longer exists".to_string())?;
                let subject = format!("Invitation: {}", event.display_title());
                let body = format!(
                    "You’re invited to {} on {} at {} ({}).\n\nReview the attached calendar invitation for details.",
                    event.display_title(),
                    event.start.format("%A, %d %B %Y"),
                    event.start.format("%H:%M"),
                    event.timezone
                );
                let calendar_ics = crate::ics::invitation_request(event);
                self.status =
                    Some("Invitation prepared in Mail. Review it there before sending.".into());
                self.dirty = true;
                return Ok(Some(tilo_actions::ActionOutput {
                    value: Some(serde_json::json!({
                        "delivery": "not_sent",
                        "target": {
                            "resource": "mail.mailbox",
                            "action": "prepare-calendar-invitation"
                        },
                        "arguments": {
                            "to": attendee.email,
                            "subject": subject,
                            "body": body,
                            "calendar_ics": calendar_ics
                        },
                        "next": "Invoke the target through the action broker; Mail may refuse with account_offline or invite permission denied."
                    })),
                    resources: Vec::new(),
                }));
            }
        }
        self.answer_resource_operation(kind, type_id, payload, doc, path)
            .map(|handled| handled.then(tilo_actions::ActionOutput::default))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the complete operation dispatch keeps every canonical Calendar view/effect route and its truthful state transition in one auditable match"
    )]
    fn answer_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
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
            (
                tilo_actions::OperationKind::View,
                crate::actions::CalendarOperation::OpenEvent { id },
            ) => {
                let id = id
                    .parse::<Id>()
                    .map_err(|_| "invalid event id".to_string())?;
                let event = self
                    .model
                    .event(id)
                    .ok_or_else(|| "event no longer exists".to_string())?;
                self.anchor = event.start.date();
                self.select_event(id);
            }
            (
                tilo_actions::OperationKind::View,
                crate::actions::CalendarOperation::OpenOccurrence {
                    event_id,
                    original_date,
                },
            ) => {
                let id = event_id
                    .parse::<Id>()
                    .map_err(|_| "invalid event id".to_string())?;
                let original_date = NaiveDate::parse_from_str(&original_date, "%Y-%m-%d")
                    .map_err(|_| "invalid occurrence date".to_string())?;
                let event = self
                    .model
                    .event(id)
                    .ok_or_else(|| "event no longer exists".to_string())?;
                self.anchor = event
                    .occurrence_overrides
                    .iter()
                    .find(|edit| edit.original_date == original_date)
                    .and_then(|edit| edit.start)
                    .map_or(original_date, |start| start.date());
                self.select_event(id);
            }
            (
                tilo_actions::OperationKind::View,
                crate::actions::CalendarOperation::OpenTask { id },
            ) => {
                let id = id
                    .parse::<Id>()
                    .map_err(|_| "invalid task id".to_string())?;
                let task = self
                    .model
                    .task(id)
                    .ok_or_else(|| "task no longer exists".to_string())?;
                if let Some(due) = task.due {
                    self.anchor = due;
                }
                self.select_task(id);
            }
            (
                tilo_actions::OperationKind::View,
                crate::actions::CalendarOperation::OpenProject { id },
            ) => {
                let id = id
                    .parse::<Id>()
                    .map_err(|_| "invalid project id".to_string())?;
                if self.model.project(id).is_none() {
                    return Err("project no longer exists".to_string());
                }
                self.list_sel = ListSel::Project(id);
                // Opening a project is presentation-only. It must not route
                // through Back/Close because those deliberately discard a
                // pristine new-event draft via a durable delete action.
                self.detail_open = false;
                self.selection = None;
                self.blur_editor();
            }
            (
                tilo_actions::OperationKind::View,
                crate::actions::CalendarOperation::OpenCalendar { id },
            ) => {
                let calendar = self
                    .calendar_for_resource(Some(&id))
                    .ok_or_else(|| "calendar no longer exists".to_string())?;
                self.calendar_filter = Some(calendar.id);
                self.detail_open = false;
                self.selection = None;
                self.blur_editor();
                self.status = Some(if calendar.visible {
                    format!("Showing {}", calendar.label)
                } else {
                    format!(
                        "{} is hidden. Turn it on in the sidebar to show its events.",
                        calendar.label
                    )
                });
            }
            (
                tilo_actions::OperationKind::View,
                crate::actions::CalendarOperation::ShowAllCalendars,
            ) => {
                self.calendar_filter = None;
                self.detail_open = false;
                self.selection = None;
                self.blur_editor();
                self.status = Some("Showing all calendars".into());
            }
            (tilo_actions::OperationKind::View, crate::actions::CalendarOperation::ShowMonth) => {
                self.view = View::Month;
            }
            (tilo_actions::OperationKind::View, crate::actions::CalendarOperation::ShowWeek) => {
                self.view = View::Week;
            }
            (tilo_actions::OperationKind::View, crate::actions::CalendarOperation::ShowDay) => {
                self.view = View::Day;
            }
            (tilo_actions::OperationKind::View, crate::actions::CalendarOperation::ShowAgenda) => {
                self.view = View::Agenda;
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::CalendarOperation::SetCalendarVisible { id, visible },
            ) => {
                self.set_calendar_visibility(&id, visible)?;
                self.status = Some(if visible {
                    "Calendar is now visible".into()
                } else {
                    "Calendar is now hidden".into()
                });
            }
            (tilo_actions::OperationKind::Effect, crate::actions::CalendarOperation::Sync) => {
                if self.syncing {
                    return Err("calendar synchronization is already running".into());
                }
                let mut doc = doc.clone();
                self.start_sync(&mut doc);
            }
            (tilo_actions::OperationKind::Effect, crate::actions::CalendarOperation::ExportIcs) => {
                self.export_ics();
            }
            (tilo_actions::OperationKind::Effect, crate::actions::CalendarOperation::ImportIcs) => {
                return Err("calendar import requires the lifecycle-capable operation host".into());
            }
            _ => return Err("operation kind does not match Calendar operation".into()),
        }
        self.dirty = true;
        Ok(true)
    }

    fn admit_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let operation = (type_id == crate::actions::CALENDAR_OPERATION_TYPE)
            .then(|| serde_json::from_slice::<crate::actions::CalendarOperation>(payload))
            .transpose()
            .map_err(|error| error.to_string())?;
        if kind != tilo_actions::OperationKind::Effect {
            return self
                .answer_resource_operation_v2(kind, type_id, payload, doc, path)
                .map(|result| result.map(ui::widget::ResourceOperationStart::Completed));
        }
        if operation == Some(crate::actions::CalendarOperation::ImportIcs) {
            if self.resource_import_invocation.is_some() || self.pending_resource_import.is_some() {
                return Err("calendar import is already waiting to be applied".into());
            }
            self.pending_resource_import = Some(self.latest_ics_import_action()?);
            self.resource_import_invocation = Some(invocation);
            self.dirty = true;
            return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
        }
        if operation != Some(crate::actions::CalendarOperation::Sync) {
            return self
                .answer_resource_operation_v2(kind, type_id, payload, doc, path)
                .map(|result| result.map(ui::widget::ResourceOperationStart::Completed));
        }
        if self.resource_sync_invocation.is_some() || self.syncing {
            return Err("calendar synchronization is already running".into());
        }
        if !self.answer_resource_operation(kind, type_id, payload, doc, path)? {
            return Ok(None);
        }
        if self.syncing {
            self.resource_sync_invocation = Some(invocation);
            Ok(Some(ui::widget::ResourceOperationStart::Accepted))
        } else {
            Ok(Some(ui::widget::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            )))
        }
    }
}

/// A minute-of-day (0..=1439) as a wall-clock time.
fn min_to_time(min: i64) -> NaiveTime {
    let m = min.clamp(0, 24 * 60 - 1);
    // `m` is clamped to 0..=1439, so hour/minute are always in-range and this
    // is always `Some`; the fallback is unreachable.
    NaiveTime::from_hms_opt((m / 60) as u32, (m % 60) as u32, 0).unwrap_or(NaiveTime::MIN)
}

#[cfg(test)]
#[path = "app/tests.rs"]
mod tests;
