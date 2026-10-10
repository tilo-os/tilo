//! `CalendarApp` — the root widget. Owns the in-memory [`Calendar`], the
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

use automerge::transaction::Transactable;
use std::collections::{HashMap, HashSet};
use ui::widget::TaffyTreeExt;

/// How many attendees the calendar publishes as rows of their own.
const ATTENDEE_ROWS: usize = 256;

/// The files the calendar imports and exports.
fn ics_filter() -> ui::portal::FileFilter {
    ui::portal::FileFilter::new("iCalendar files")
        .mimetype("text/calendar")
        .glob("*.ics")
}
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ChangeHash, ObjId};
use chrono::{Duration, Local, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
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
use ui::widget::{
    DatePicker, Popover, SegmentedTouch, SegmentedTouchGroup, SegmentedTouchResult, TextInput,
};

use tilo_rich_text::{RichTextField, RichTextSurface};

use crate::account::AccountStore;
use crate::agenda::{AgendaAction, AgendaSource, AgendaView};
use crate::caldav::{self, SyncOutcome};
use crate::date;
use crate::event_popovers::{EventPopovers, PopoverEdit, PopoverKind as FormPopover};
use crate::ics;
use crate::input_context::InputContext;
use crate::model::{Calendar, Event, Id, ListSel, Palette, Priority, View};
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

/// First-run onboarding shown centered over an empty calendar (no events):
/// a friendly headline + hint instead of a blank grid.
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
    /// Put the sidebar's task list away, back to the month navigator.
    CloseList,
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
    /// Give the edited event one of the calendar colours.
    SetEventColor(Palette),
    /// Throw away the new event the sheet was opened for, unsaved.
    DiscardDraft(Id),
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
type NotesEditor = RichTextSurface<RichTextField>;

type SyncWorkerResult = (caldav::SyncResult, SyncOutcome);

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent, unrelated flag (demo mode, sidebar state, sync status, form scratch state); grouping them into enums would not express any real invariant between them and would touch every read/write site across this file"
)]
pub struct CalendarApp {
    model: Calendar,
    today: NaiveDate,
    /// The moment shown as now, when a capture pins it; else the clock's.
    pinned_now: Option<chrono::NaiveDateTime>,
    anchor: NaiveDate,
    view: View,
    /// `None` shows every visible collection; a stable local/remote calendar
    /// id is set only by the collection resource's canonical `open` action.
    calendar_filter: Option<String>,
    /// The task list the sidebar shows in place of the month navigator,
    /// when one is chosen.
    list_sel: Option<ListSel>,
    selection: Option<Sel>,
    detail_open: bool,

    /// `CalDAV` accounts + their calendars as this person shows them: the
    /// accounts Settings configured on this device ([`Self::configured`])
    /// with the person's display choices applied.
    accounts: AccountStore,
    /// The accounts as Settings configured them on this device.
    configured: AccountStore,
    /// The same accounts, as the drains of the upload and delete queues
    /// read them.
    device_accounts: crate::outbox::DeviceAccounts,
    /// The person's display choices for their calendars.
    display: crate::display::CalendarDisplay,
    /// Where those choices are kept, following the person across devices.
    display_setting: Option<ui::framework::Setting<crate::display::CalendarDisplay>>,

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
    staged_new_event: bool,
    /// Shots-only: open this popover once the form is bound.
    staged_popover: Option<FormPopover>,
    /// Shots-only: open the calendar dropdown once the form is bound.
    shots_pending_calendar_menu: bool,
    shots_detail_scroll_to_end: bool,
    /// Shots-only: open the sidebar's overflow menu once it is laid out.
    shots_pending_sidebar_menu: bool,

    /// The accounts are a capture's sample ones: their servers answer from
    /// bundled sample calendars, and display choices are not saved.
    sample_accounts: bool,
    /// The person's accounts, which Settings keeps and every device of theirs
    /// shares, followed in a live window; and the revision last shown.
    person_accounts: Option<(
        ui::framework::PersonRecord<accounts::person::PersonAccounts>,
        Option<u64>,
    )>,

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
    editor_node: Option<NodeId>,
    /// Notes-column width the editor node was last built with. When the current
    /// width (from `detail_body_w`) diverges — i.e. a resize crossed the
    /// breakpoint or reshaped the detail pane — `take_structure_dirty` requests
    /// a rebuild so the editor's own text-wrap width tracks the new layout
    /// instead of staying frozen at the width it launched with.
    built_notes_w: f64,

    hits: Vec<(Rect, Action)>,
    last_mouse: Point,
    segmented_touch: SegmentedTouch,
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
    /// A sync effect asked for a pull, started on the next sync with the
    /// document.
    sync_requested: bool,
    /// The caller of the sync effect, answered when its pull lands.
    sync_reply: Option<ui::framework::EffectReply>,
    /// What a search hit or an agent's `open` asked to show, shown on the
    /// next sync.
    opens: std::rc::Rc<std::cell::RefCell<Vec<String>>>,

    /// Where the calendar's edits go: the framework's dispatcher, attached
    /// before the first frame.
    dispatcher: Option<ui::framework::Dispatcher<Calendar>>,
    /// The last edit an on-screen control dispatched, for the tests that
    /// check a control and its published action build the same edit.
    #[cfg(test)]
    last_dispatched: std::cell::RefCell<Option<ui::tilo_resource::Invoke<Calendar>>>,

    /// The document heads the in-memory [`Calendar`] was last derived from. When the
    /// document moves past this out of band — a Ctrl+K palette action, a peer's
    /// change merged in — the next `sync` re-reads the model so those mutations
    /// show up in the views, exactly as an on-screen edit already does.
    model_heads: (Vec<ChangeHash>, usize),
}

impl CalendarApp {
    /// The live app: persisted to disk, no sample content, accounts sourced
    /// from OS Settings (none on a fresh machine).
    pub fn new() -> Self {
        // The launcher's shared store owns loading, autosave and the exit flush.
        // A second widget-owned writer would keep importing stale legacy data
        // and bypass the host's persistence policy.
        Self::build()
    }

    /// An empty calendar whose edits apply to the document its tests hand it,
    /// through a dispatcher of no window.
    #[cfg(test)]
    pub(crate) fn new_detached() -> Self {
        let mut app = Self::build();
        let dispatcher = ui::framework::Dispatcher::detached("so.tilo.calendar", "test-author");
        app.notes_editor = Self::new_notes_editor(None, Some(&dispatcher));
        app.dispatcher = Some(dispatcher);
        app
    }

    /// The sample calendar tests start from: the sample events, tasks and
    /// accounts around today, the "Design review" event open.
    #[cfg(test)]
    pub(crate) fn new_demo() -> Self {
        let mut app = Self::new_detached();
        app.model = crate::model::seed_demo(app.today);
        app.configured = AccountStore::sample();
        app.sample_accounts = true;
        app.show_accounts();
        app.open_sample_event();
        app
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the constructor deliberately keeps every per-widget local state default adjacent to its field"
    )]
    fn build() -> Self {
        let today = ui::clock::today();
        let model = Calendar::default();
        let selection = None;
        let accounts = AccountStore::default();
        let detail_open = false;
        Self {
            model,
            today,
            anchor: today,
            pinned_now: None,
            view: View::Month,
            calendar_filter: None,
            list_sel: None,
            selection,
            detail_open,
            accounts: accounts.clone(),
            device_accounts: std::rc::Rc::new(std::cell::RefCell::new(accounts.clone())),
            configured: accounts,
            display: crate::display::CalendarDisplay::default(),
            display_setting: None,
            notes_editor: Self::new_notes_editor(None, None),
            bound: None,
            seeded: false,
            title_input: TextInput::local()
                .with_placeholder(TITLE_PLACEHOLDER)
                .with_text_role(ui::tokens::TextRole::Heading)
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
            task_due_picker: DatePicker::new(today)
                .with_today(today)
                .with_accessible_name("Task due date"),
            choices: Choices::new(),
            form: FormRects::default(),
            modal: None,
            notes_content_h: 0.0,
            sidebar_menu: Popover::new()
                .with_items(["Export calendar", "Import calendar"])
                .with_label("Calendar file"),
            mini_month: DatePicker::new(today)
                .with_today(today)
                .navigator()
                .with_accessible_name("Month"),
            search_opener: ui::search::SearchOpener::new(),
            staged_new_event: false,
            staged_popover: None,
            shots_pending_calendar_menu: false,
            shots_detail_scroll_to_end: false,
            shots_pending_sidebar_menu: false,
            sidebar: Sidebar::new(),
            topbar: Topbar::new(),
            layout_theme: Theme::dark(),
            sample_accounts: false,
            person_accounts: None,
            rect: Rect::ZERO,
            content_rect: Rect::ZERO,
            detail_rect: Rect::ZERO,
            notes_origin: Point::new(-10000.0, -10000.0),
            notes_w: 0.0,
            detail_scroll_y: 0.0,
            detail_scroll_max: 0.0,
            editor_node: None,
            built_notes_w: -1.0,
            hits: Vec::new(),
            last_mouse: Point::ZERO,
            segmented_touch: SegmentedTouch::default(),
            status: None,
            status_close: Rect::ZERO,
            fresh_draft: None,
            dirty: true,
            timeline: TimelineInteraction::new(),
            agenda: AgendaView::new(),
            sync_rx: None,
            syncing: false,
            sync_requested: false,
            sync_reply: None,
            opens: std::rc::Rc::default(),
            dispatcher: None,
            #[cfg(test)]
            last_dispatched: std::cell::RefCell::default(),
            model_heads: (Vec::new(), 0),
        }
    }

    /// Adopt the framework's live model projection.
    pub(crate) fn adopt_resource_state(&mut self, state: &std::rc::Rc<crate::model::Calendar>) {
        self.model = (**state).clone();
        self.dirty = true;
    }

    /// Attach the framework dispatcher, with the accounts the window shows
    /// (`None` keeps none); `sample` when they are a capture's sample ones.
    /// Show `now` as the present: a capture's fixed day and time, the same
    /// on every run.
    pub(crate) fn pin_now(&mut self, now: chrono::NaiveDateTime) {
        let today = now.date();
        self.pinned_now = Some(now);
        self.today = today;
        self.anchor = today;
        self.event_popovers = EventPopovers::new(today);
        self.task_due_picker = DatePicker::new(today).with_accessible_name("Task due date");
        self.dirty = true;
    }

    /// The present: the pinned moment of a capture, else the clock's.
    fn now(&self) -> chrono::NaiveDateTime {
        self.pinned_now
            .unwrap_or_else(|| Local::now().naive_local())
    }

    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<Calendar>,
        accounts: Option<AccountStore>,
        sample: bool,
    ) {
        self.notes_editor = Self::new_notes_editor(self.bound, Some(&dispatcher));
        self.dispatcher = Some(dispatcher);
        if let Some(accounts) = accounts {
            self.configured = accounts;
            self.sample_accounts = sample;
        }
        self.show_accounts();
        self.dirty = true;
    }

    /// Open the sample calendar's "Design review" event, as a populated
    /// capture shows it.
    fn open_sample_event(&mut self) {
        let event = self
            .model
            .events
            .iter()
            .find(|event| event.title.starts_with("Design review"))
            .or_else(|| self.model.events.first())
            .map(|event| event.id);
        if let Some(id) = event {
            self.select_event(id);
        }
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
    }

    /// Stage the view state of the screenshot scenario `scenario` (`None`
    /// for a plain capture) over the resource its actions filled.
    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        let name = scenario.unwrap_or("event_editor");
        let view = match name {
            "month" => Some(View::Month),
            "week" | "empty_week" => Some(View::Week),
            "day" => Some(View::Day),
            "agenda" | "empty_agenda" => Some(View::Agenda),
            _ => None,
        };
        if let Some(view) = view {
            self.view = view;
        }
        if name.starts_with("empty") {
            self.dirty = true;
            return;
        }
        match name {
            "event_editor" | "narrow_editor" => self.open_sample_event(),
            "new_event" => self.staged_new_event = true,
            "calendar_popover" => self.shots_open_calendar_popover(),
            "date_popover" | "time_popover" => {
                self.staged_new_event = true;
                self.staged_popover = Some(match name {
                    "date_popover" => FormPopover::StartDate,
                    _ => FormPopover::StartTime,
                });
            }
            "task_detail" => self.shots_show_task_detail(),
            "task_due_picker" => self.shots_open_task_picker("due"),
            "task_priority_picker" => self.shots_open_task_picker("priority"),
            "task_project_picker" => self.shots_open_task_picker("project"),
            "sync_syncing" | "sync_offline" | "sync_conflict" | "sync_error" => {
                self.shots_set_sync_state(name.strip_prefix("sync_").unwrap_or(name));
            }
            "collapsed_sidebar" => self.sidebar.set_collapsed(true),
            "narrow_drawer" => self.sidebar.set_open(true),
            _ => {}
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

    /// Open the calendar dropdown of a new event's form for a screenshot.
    pub fn shots_open_calendar_popover(&mut self) {
        self.staged_new_event = true;
        self.shots_pending_calendar_menu = true;
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
    fn start_sync(&mut self, doc: &mut ui::widget::Edits<'_>) {
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

        if self.sample_accounts {
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

        // A live pull as a task on the app runtime; the result lands via
        // `poll_sync`. Uploads and deletes are drained as they are queued
        // (`crate::outbox`); an item still waiting for its upload is never
        // overwritten or removed by what the server sends meanwhile.
        let runtime = match ui::rt::try_app() {
            Ok(runtime) => runtime,
            Err(error) => {
                self.status = Some(format!("Sync unavailable: {error}"));
                self.dirty = true;
                return;
            }
        };
        let current = Calendar::read_whole(doc.doc()).unwrap_or_default();
        let protected_hrefs: Vec<(String, String)> = crate::outbox::pending_copies(&current)
            .into_iter()
            .map(|copy| (copy.calendar.clone(), copy.href.clone()))
            .collect();
        let sync_tokens: Vec<(String, String)> = current
            .sync_tokens
            .iter()
            .map(|(calendar, token)| (calendar.clone(), token.clone()))
            .collect();
        let (tx, rx) = std::sync::mpsc::channel();
        let today = self.today;
        for id in &sync_account_ids {
            if let Err(error) = accounts::AccountSyncReporter::new(id.clone()).started() {
                tracing::warn!(%error, account = id, "calendar: could not persist sync start");
            }
        }
        drop(runtime.spawn(async move {
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
                let persisted = match &account_outcome {
                    caldav::SyncOutcome::Synced { .. } => reporter.succeeded(),
                    caldav::SyncOutcome::Offline => reporter.offline(),
                    caldav::SyncOutcome::Failed(reason)
                        if reason.contains("401") || reason.contains("403") =>
                    {
                        reporter.needs_authentication()
                    }
                    caldav::SyncOutcome::Failed(_) => reporter.failed(),
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
            // Do not overwrite a locally edited object still waiting for its
            // upload (or whose conditional upload conflicted). It stays
            // queued; the next upload can be resolved without destroying
            // either copy.
            result.resources.retain(|resource| {
                !protected_hrefs.iter().any(|(calendar_id, href)| {
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
            let _ = tx.send((result, outcome));
        }));
        self.sync_rx = Some(rx);
        self.syncing = true;
        self.status = Some("Syncing…".into());
        self.dirty = true;
    }

    /// Drain a finished background sync, if any. Called every `sync` pass; a
    /// no-op until the worker delivers. Keeping this off the click path is why
    /// a slow or unreachable server never freezes the window.
    fn poll_sync(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let Some(rx) = self.sync_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok((result, outcome)) => {
                self.sync_rx = None;
                self.syncing = false;
                let failure = match &outcome {
                    SyncOutcome::Failed(reason) => Some(reason.clone()),
                    SyncOutcome::Offline => Some("calendar service is offline".to_string()),
                    SyncOutcome::Synced { .. } => None,
                };
                self.apply_sync_result(result, outcome, doc);
                if let Some(reply) = self.sync_reply.take() {
                    reply.finish(
                        failure.map_or_else(|| Ok(tilo_actions::ActionOutput::default()), Err),
                    );
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                // Worker panicked or dropped the sender without a reply.
                self.sync_rx = None;
                self.syncing = false;
                self.status = Some("Sync failed".into());
                if let Some(reply) = self.sync_reply.take() {
                    reply.finish(Err(
                        "calendar synchronization stopped without a result".into()
                    ));
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
        doc: &mut ui::widget::Edits<'_>,
    ) {
        let mut added = 0usize;
        let mut updated = 0usize;
        let mut removed = 0usize;
        let current = Calendar::read_whole(doc.doc()).unwrap_or_default();
        // Where each item a server holds lives there, as the calendar knows
        // it and as this pull adds to it.
        let mut known: Vec<(Id, String, String)> = current
            .remote
            .iter()
            .filter_map(|(id, copy)| {
                Some((id.parse().ok()?, copy.calendar.clone(), copy.href.clone()))
            })
            .collect();
        let find = |known: &[(Id, String, String)], calendar: &str, href: &str| {
            known
                .iter()
                .find(|(_, held_calendar, held_href)| {
                    held_calendar == calendar && caldav::hrefs_equal(held_href, href)
                })
                .map(|(id, _, _)| *id)
        };

        // Sample results carry no server address of their own.
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
            let at = crate::resource::RemoteAt {
                calendar: e.calendar_id.clone().unwrap_or_else(|| "sample".into()),
                href: String::new(),
                etag: None,
            };
            self.adopt_event(doc, &e, at);
            self.model.events.push(e);
            added += 1;
        }
        for resource in result.resources {
            for (index, mut event) in resource.events.into_iter().enumerate() {
                // Current parser represents recurrence as one event. If a
                // server returns multiple components in one object, retain
                // stable identities for all by an internal component suffix.
                let binding_href = if index == 0 {
                    resource.href.clone()
                } else {
                    format!("{}#tilo-component-{index}", resource.href)
                };
                let id = find(&known, &resource.calendar_id, &binding_href);
                // An item deleted here keeps what its server held until the
                // server deleted it too: a late pull still carrying it does
                // not bring it back.
                if id.is_some_and(|id| current.event(id).is_none()) {
                    continue;
                }
                let id = id.unwrap_or_else(Uuid::new_v4);
                event.id = id;
                event.calendar_id = Some(resource.calendar_id.clone());
                if let Some(slot) = self.model.events.iter_mut().find(|item| item.id == id) {
                    *slot = event.clone();
                    updated += 1;
                } else {
                    self.model.events.push(event.clone());
                    added += 1;
                }
                known.push((id, resource.calendar_id.clone(), binding_href.clone()));
                self.adopt_event(
                    doc,
                    &event,
                    crate::resource::RemoteAt {
                        calendar: resource.calendar_id.clone(),
                        href: binding_href,
                        etag: resource.etag.clone(),
                    },
                );
            }
            for (index, mut task) in resource.tasks.into_iter().enumerate() {
                let binding_href = format!("{}#tilo-task-{index}", resource.href);
                let id = find(&known, &resource.calendar_id, &binding_href);
                if id.is_some_and(|id| current.task(id).is_none()) {
                    continue;
                }
                let id = id.unwrap_or_else(Uuid::new_v4);
                task.id = id;
                if let Some(slot) = self.model.tasks.iter_mut().find(|item| item.id == id) {
                    *slot = task.clone();
                    updated += 1;
                } else {
                    self.model.tasks.push(task.clone());
                    added += 1;
                }
                known.push((id, resource.calendar_id.clone(), binding_href.clone()));
                self.submit(
                    doc,
                    Calendar::ADOPT_REMOTE_TASK.with(crate::resource::calendar::AdoptRemoteTask {
                        task: crate::actions::TaskData::from_task(&task),
                        at: crate::resource::RemoteAt {
                            calendar: resource.calendar_id.clone(),
                            href: binding_href,
                            etag: resource.etag.clone(),
                        },
                    }),
                );
            }
        }
        let held_in = |calendar: &str| -> Vec<(Id, String)> {
            current
                .remote
                .iter()
                .filter(|(_, copy)| copy.calendar == calendar && !copy.href.is_empty())
                .filter_map(|(id, copy)| Some((id.parse().ok()?, copy.href.clone())))
                .collect()
        };
        for deletion in result.deletions {
            let victims: Vec<Id> = held_in(&deletion.calendar_id)
                .into_iter()
                .filter(|(_, href)| {
                    caldav::hrefs_equal(href.split("#tilo-").next().unwrap_or(href), &deletion.href)
                })
                .map(|(id, _)| id)
                .collect();
            for id in victims {
                self.drop_remote(doc, id);
                removed += 1;
            }
        }
        // A successful full query is authoritative for that collection. This
        // is also the invalid-token recovery path.
        for checkpoint in &result.checkpoints {
            if checkpoint.full {
                let seen: HashSet<&str> =
                    checkpoint.seen_hrefs.iter().map(String::as_str).collect();
                for (id, href) in held_in(&checkpoint.calendar_id) {
                    let base = href.split("#tilo-").next().unwrap_or(&href);
                    if !seen
                        .iter()
                        .any(|seen_href| caldav::hrefs_equal(base, seen_href))
                    {
                        self.drop_remote(doc, id);
                        removed += 1;
                    }
                }
            }
        }
        // Commit markers are deliberately last: a crash before here repeats a
        // response idempotently; a crash after here sees fully reconciled data.
        for checkpoint in result.checkpoints {
            self.submit(
                doc,
                Calendar::PULLED.with(crate::resource::calendar::Pulled {
                    calendar: checkpoint.calendar_id.clone(),
                    token: checkpoint.token.clone(),
                }),
            );
        }
        self.model.events.settle();
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

    /// Write `event` as its server holds it, at `at`.
    fn adopt_event(
        &self,
        doc: &mut ui::widget::Edits<'_>,
        event: &Event,
        at: crate::resource::RemoteAt,
    ) {
        self.submit(
            doc,
            Calendar::ADOPT_REMOTE_EVENT.with(crate::resource::calendar::AdoptRemoteEvent {
                event: crate::actions::EventData::from_event(event),
                at,
            }),
        );
    }

    /// Drop the item `id` its server no longer holds.
    fn drop_remote(&mut self, doc: &mut ui::widget::Edits<'_>, id: Id) {
        self.submit(
            doc,
            Calendar::DROP_REMOTE_ITEM.with(crate::resource::calendar::DropRemoteItem { item: id }),
        );
        self.model.events.retain(|event| event.id != id);
        self.model.tasks.retain(|task| task.id != id);
    }

    /// The notes editor, bound to `owner`'s notes: every edit is the
    /// calendar's field edit at their path, through the notes' child of the
    /// calendar's dispatcher. Until the calendar is hosted there is none, and
    /// the notes are shown, not edited.
    fn new_notes_editor(
        owner: Option<Sel>,
        calendar: Option<&ui::framework::Dispatcher<Calendar>>,
    ) -> NotesEditor {
        let id = match owner {
            Some(Sel::Event(id) | Sel::Task(id)) => Some(id),
            None => None,
        };
        let notes = id.map(crate::resource::notes_path);
        let mut field =
            RichTextField::view_only(move |doc| notes.as_ref().and_then(|notes| notes.object(doc)));
        if let (Some(calendar), Some(id)) = (calendar, id) {
            field.attach(crate::resource::notes_dispatcher(calendar, id));
        }
        RichTextSurface::field(field).with_placeholder(NOTES_PLACEHOLDER)
    }

    // --- CRDT plumbing ---------------------------------------------------

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

    fn ensure_bound(&mut self, doc: &AutoCommit) {
        let sel = match self.selection {
            Some(s) if self.detail_open => s,
            _ => {
                if self.bound.is_some() {
                    self.notes_editor = Self::new_notes_editor(None, self.dispatcher.as_ref());
                    self.bound = None;
                }
                return;
            }
        };
        if self.bound == Some(sel) {
            return;
        }
        // Every action that creates an event or a task starts its notes.
        let (Sel::Event(id) | Sel::Task(id)) = sel;
        if crate::resource::notes_path(id).object(doc).is_some() {
            self.notes_editor = Self::new_notes_editor(Some(sel), self.dispatcher.as_ref());
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
    fn flush_form_edits(&mut self, doc: &mut ui::widget::Edits<'_>) {
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
        let Some(e) = self.model.event_mut(id) else {
            return;
        };
        let before = e.clone();
        if title_dirty.is_some() {
            e.title = title;
        }
        if loc_dirty.is_some() {
            e.location = location;
        }
        let after = e.clone();
        if self.submit_event_change(doc, &before, &after) {
            self.dirty = true;
        }
    }

    fn submit_invite(&mut self, doc: &mut ui::widget::Edits<'_>) {
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
        self.submit(
            doc,
            Calendar::INVITE.with((crate::model::key(event_id), email.clone(), String::new())),
        );
        if let Ok(read) = Calendar::read_whole(doc.doc()) {
            self.model = read;
        }
        self.invite_input.set_text("");
        self.status = Some(format!(
            "Added {email}. Prepare the invitation in Mail to send it."
        ));
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

    // --- submitting edits ------------------------------------------------

    /// Submit one edit or verb through the calendar's dispatcher — the one
    /// write path, shared with the palette, agents and scripts: checked
    /// against the participant's permissions, attributed and recorded in
    /// history. The in-memory calendar is updated by the caller for
    /// immediate feedback and re-read from the document on the next `sync`.
    fn submit(
        &self,
        doc: &mut ui::widget::Edits<'_>,
        submit: impl ui::framework::Submit<Calendar>,
    ) {
        if let Err(error) = self.try_submit(doc, submit) {
            tracing::debug!(%error, "calendar: an edit was refused");
        }
    }

    /// [`Self::submit`], reporting a refusal.
    fn try_submit(
        &self,
        doc: &mut ui::widget::Edits<'_>,
        submit: impl ui::framework::Submit<Calendar>,
    ) -> Result<(), tilo_actions::ActionError> {
        let invoke = submit
            .into_invoke()
            .map_err(tilo_actions::ActionError::Refused)?;
        #[cfg(test)]
        {
            *self.last_dispatched.borrow_mut() = Some(invoke.clone());
        }
        let Some(dispatcher) = &self.dispatcher else {
            return Err(tilo_actions::ActionError::Refused(
                "the calendar is not attached to a window".into(),
            ));
        };
        dispatcher.submit_now(doc, invoke)
    }

    /// Submit the field edits that make `before` the event `after`, one by
    /// one.
    fn submit_event_change(
        &self,
        doc: &mut ui::widget::Edits<'_>,
        before: &Event,
        after: &Event,
    ) -> bool {
        let edits = Calendar::paths().events.upsert(Some(before), after);
        let changed = !edits.is_empty();
        for edit in edits {
            self.submit(doc, edit);
        }
        changed
    }

    /// The last invocation an on-screen affordance submitted, for the tests
    /// that check a control and its published action build the same edit.
    #[cfg(test)]
    #[allow(dead_code, reason = "test helper")]
    pub(crate) fn last_dispatched(&self) -> Option<ui::tilo_resource::Invoke<Calendar>> {
        self.last_dispatched.borrow().clone()
    }

    /// The calendar collections as live rows of the calendar.
    pub(crate) fn collection_rows(&self) -> Vec<ui::framework::LiveRow> {
        self.calendar_collections()
            .iter()
            .map(crate::actions::calendar_collection_row)
            .collect()
    }

    /// Each event's attendees as rows offering to prepare their invitation
    /// in Mail, as many as [`ATTENDEE_ROWS`].
    pub(crate) fn attendee_rows(&self) -> Vec<ui::framework::LiveRow> {
        self.model
            .events
            .iter()
            .flat_map(|event| {
                event.attendees.iter().map(move |attendee| {
                    ui::framework::LiveRow::new(
                        crate::actions::attendee_resource_id(event.id, attendee.id),
                        "calendar.attendee",
                        attendee.email.clone(),
                    )
                    .effect(&crate::resource::CalendarEffect::PrepareInMail {
                        event_id: event.id.to_string(),
                        attendee_id: attendee.id.to_string(),
                    })
                })
            })
            .take(ATTENDEE_ROWS)
            .collect()
    }

    fn calendar_for_resource(
        &self,
        id: Option<&str>,
    ) -> Option<crate::actions::CalendarCollectionResource> {
        let id = id.unwrap_or("local");
        self.calendar_collections()
            .into_iter()
            .find(|calendar| calendar.id == id)
    }

    /// Invoke one of the calendar's effects through the door, as the palette
    /// or an agent does.
    fn invoke_effect(&self, effect: &crate::resource::CalendarEffect) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.effect(effect);
        }
    }

    // --- mutations -------------------------------------------------------

    fn select_event(&mut self, id: Id) {
        self.selection = Some(Sel::Event(id));
        self.detail_scroll_y = 0.0;
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

    /// Where the sidebar's overflow menu may open: the window, up to the
    /// trailing edge of the button that opens it, so the menu hangs under
    /// the button and stays over the sidebar.
    fn sidebar_menu_viewport(&self) -> Rect {
        let button = self.sidebar.menu_rect(&self.layout_theme);
        Rect::new(self.rect.x0, self.rect.y0, button.x1, self.rect.y1)
    }

    /// Delete an event and its notes; a server holding it deletes it next.
    /// Closes the editor if the deleted event was the one on screen.
    fn delete_event(&mut self, id: Id, doc: &mut ui::widget::Edits<'_>) {
        self.model.events.retain(|e| e.id != id);
        self.submit(doc, Calendar::DELETE_EVENT.with(crate::model::key(id)));
        if self.selection == Some(Sel::Event(id)) {
            self.close_after_delete();
        }
        self.status = Some("Event deleted".into());
        self.dirty = true;
    }

    /// Delete a task (and any subtasks) from the model + CRDT. Closes the editor
    /// if the deleted task was the one on screen.
    fn delete_task(&mut self, id: Id, doc: &mut ui::widget::Edits<'_>) {
        let victims: Vec<Id> = std::iter::once(id)
            .chain(
                self.model
                    .tasks
                    .iter()
                    .filter(|t| t.parent == Some(id))
                    .map(|t| t.id),
            )
            .collect();
        self.model.tasks.retain(|t| !victims.contains(&t.id));
        self.submit(doc, Calendar::DELETE_TASK.with(crate::model::key(id)));
        if matches!(self.selection, Some(Sel::Task(sel)) if victims.contains(&sel)) {
            self.close_after_delete();
        }
        self.status = Some("Task deleted".into());
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

    fn toggle_task(&mut self, id: Id, doc: &mut ui::widget::Edits<'_>) {
        let done = if let Some(t) = self.model.task_mut(id) {
            t.done = !t.done;
            Some(t.done)
        } else {
            None
        };
        if let Some(done) = done {
            self.submit(
                doc,
                Calendar::paths()
                    .tasks
                    .item(&crate::model::key(id))
                    .done
                    .set(done),
            );
            self.dirty = true;
        }
    }

    fn set_selected_task_due(&mut self, due: Option<NaiveDate>, doc: &mut ui::widget::Edits<'_>) {
        let Some(Sel::Task(id)) = self.selection else {
            return;
        };
        if let Some(task) = self.model.task_mut(id) {
            task.due = due;
            self.submit(
                doc,
                Calendar::paths()
                    .tasks
                    .item(&crate::model::key(id))
                    .due
                    .set(due),
            );
        }
        self.task_popover = None;
    }

    fn set_selected_task_priority(&mut self, priority: Priority, doc: &mut ui::widget::Edits<'_>) {
        let Some(Sel::Task(id)) = self.selection else {
            return;
        };
        if let Some(task) = self.model.task_mut(id) {
            task.priority = priority;
            self.submit(
                doc,
                Calendar::paths()
                    .tasks
                    .item(&crate::model::key(id))
                    .priority
                    .set(priority),
            );
        }
        self.task_popover = None;
    }

    fn set_selected_task_project(&mut self, project: Option<Id>, doc: &mut ui::widget::Edits<'_>) {
        let Some(Sel::Task(id)) = self.selection else {
            return;
        };
        if let Some(task) = self.model.task_mut(id) {
            task.project = project;
            self.submit(
                doc,
                Calendar::paths()
                    .tasks
                    .item(&crate::model::key(id))
                    .project
                    .set(project),
            );
        }
        self.task_popover = None;
        self.choices.close_all();
    }

    fn new_event(&mut self, doc: &mut ui::widget::Edits<'_>) {
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
            color: self.model.next_event_order_color(),
            reminder_min: Some(10),
            // Filed on the local (on-device) calendar by default; the form's
            // calendar selector can retarget it to a configured account.
            ..Event::default()
        };
        self.spawn_event(ev, doc);
    }

    /// Insert a freshly-authored event into the model + CRDT (through the action
    /// model), select it, and open its edit form with the caret in the title so
    /// the next keystroke names it. Shared by the New button and the timeline's
    /// drag-to-create. Notes are left EMPTY on purpose — the editor paints its
    /// placeholder until the user types.
    fn spawn_event(&mut self, ev: Event, doc: &mut ui::widget::Edits<'_>) {
        let id = ev.id;
        self.submit(
            doc,
            Calendar::NEW_EVENT.with(crate::resource::calendar::NewEvent {
                event: crate::actions::EventData::from_event(&ev),
            }),
        );
        self.fresh_draft = Some(ev.clone());
        self.model.events.push(ev);
        self.model.events.settle();
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
    fn discard_pristine_draft(&mut self, doc: &mut ui::widget::Edits<'_>) -> bool {
        let Some(initial) = self.fresh_draft.take() else {
            return false;
        };
        let id = initial.id;
        let Some(current) = self.model.event(id).cloned() else {
            return false;
        };
        let has_notes = Calendar::read_whole(doc.doc())
            .is_ok_and(|calendar| !calendar.notes_text(id).trim().is_empty());
        if initial != current || has_notes {
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
        doc: &mut ui::widget::Edits<'_>,
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
            color: self.model.next_event_order_color(),
            reminder_min: Some(10),
            ..Event::default()
        };
        self.spawn_event(ev, doc);
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
        doc: &mut ui::widget::Edits<'_>,
    ) {
        let start = day.and_time(min_to_time(start_min));
        let end = start + Duration::minutes(dur_min.max(TIMELINE_SNAP_MIN));
        let moved = match self.model.event_mut(id) {
            // An all-day event has no place on the timed grid; leave it alone.
            Some(e) if !e.all_day && (e.start != start || e.end != end) => {
                e.start = start;
                e.end = end;
                true
            }
            _ => false,
        };
        if moved {
            self.submit(
                doc,
                Calendar::RESCHEDULE.with((crate::model::key(id), start, end)),
            );
            self.status = Some("Event rescheduled".into());
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
            self.apply_timeline_effect(effect, &mut input.state);
        }
        if outcome.handled {
            *input.handled = true;
            self.dirty = true;
        }
        outcome.handled
    }

    fn apply_timeline_effect(&mut self, effect: TimelineEffect, doc: &mut ui::widget::Edits<'_>) {
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

    /// The accounts the upload and delete drains reach.
    pub(crate) fn device_accounts(&self) -> &crate::outbox::DeviceAccounts {
        &self.device_accounts
    }

    /// Attach where the person's display choices are kept, and show the
    /// accounts as they choose.
    pub(crate) fn follow_display(
        &mut self,
        setting: ui::framework::Setting<crate::display::CalendarDisplay>,
    ) {
        self.display = setting.get();
        self.display_setting = Some(setting);
        self.show_accounts();
    }

    /// The configured accounts with the person's display choices applied.
    fn show_accounts(&mut self) {
        self.device_accounts.replace(self.configured.clone());
        self.accounts = self.display.applied_to(&self.configured);
        self.dirty = true;
    }

    /// The accounts as configured, before display choices apply.
    #[cfg(test)]
    pub(crate) fn configured_for_test(&self) -> &AccountStore {
        &self.configured
    }

    /// Follow the person's accounts, which Settings keeps, in this window.
    pub(crate) fn follow_person_accounts(
        &mut self,
        accounts: ui::framework::PersonRecord<accounts::person::PersonAccounts>,
    ) {
        self.person_accounts = Some((accounts, None));
        self.adopt_person_accounts();
    }

    /// Show the person's `CalDAV` accounts when their record moved: each
    /// account's settings from the record, its password from this device's
    /// vault. An empty record leaves the accounts this device's file holds.
    fn adopt_person_accounts(&mut self) {
        let Some((record, seen)) = &mut self.person_accounts else {
            return;
        };
        let revision = record.revision();
        if *seen == Some(revision) {
            return;
        }
        *seen = Some(revision);
        let person = record.get();
        if person.accounts.is_empty() {
            return;
        }
        let adopted = AccountStore::from_store(&person.store(), &accounts::SecretVault::open());
        if adopted != self.configured {
            self.configured = adopted;
            self.show_accounts();
        }
    }

    /// Pick up display choices made in another window or on another device.
    fn refresh_display(&mut self) {
        if let Some(setting) = &self.display_setting {
            let current = setting.get();
            if current != self.display {
                self.display = current;
                self.show_accounts();
            }
        }
    }

    /// Change the person's display choices and keep them.
    fn change_display(
        &mut self,
        change: impl FnOnce(&mut crate::display::CalendarDisplay),
    ) -> Result<(), String> {
        let mut display = self.display.clone();
        change(&mut display);
        if let Some(setting) = &self.display_setting {
            setting
                .set(display.clone())
                .map_err(|error| format!("Couldn't save the calendar display: {error}"))?;
        }
        self.display = display;
        self.show_accounts();
        Ok(())
    }

    /// Show or hide a calendar — an *absolute* state, so the sidebar row, a
    /// screen reader, the palette and an agent retry cannot invert a newer
    /// choice by replaying a toggle.
    pub(crate) fn set_calendar_visibility(
        &mut self,
        id: &str,
        visible: bool,
    ) -> Result<(), String> {
        if !self
            .configured
            .all_calendars()
            .any(|(_, calendar)| calendar.id == id)
        {
            return Err("That calendar is no longer configured".into());
        }
        self.change_display(|display| {
            display.visible.insert(id.to_owned(), visible);
        })
    }

    /// Draw a calendar in palette slot `color`.
    fn set_calendar_color(&mut self, id: &str, color: Palette) -> Result<(), String> {
        if !self
            .configured
            .all_calendars()
            .any(|(_, calendar)| calendar.id == id)
        {
            return Err("That calendar is no longer configured".into());
        }
        let slot = u8::try_from(color.index()).unwrap_or_default();
        self.change_display(|display| {
            display.colors.insert(id.to_owned(), slot);
        })
    }

    /// Fold or unfold an account's calendars in the sidebar.
    fn toggle_account_collapsed(&mut self, id: &str) {
        let Some(collapsed) = self
            .accounts
            .accounts
            .iter()
            .find(|account| account.id == id)
            .map(|account| account.collapsed)
        else {
            self.status = Some("That calendar account is no longer configured".into());
            return;
        };
        let result = self.change_display(|display| {
            display.collapsed.insert(id.to_owned(), !collapsed);
        });
        if let Err(error) = result {
            self.status = Some(error);
        }
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
    /// with the calendar id and adopts that calendar's colour, and its server
    /// takes it next; the local target clears the tag.
    fn set_event_calendar(&mut self, target: &CalTarget, doc: &mut ui::widget::Edits<'_>) {
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
            self.submit(
                doc,
                Calendar::MOVE_TO_CALENDAR.with((
                    crate::model::key(id),
                    snap.calendar_id.clone(),
                    snap.color,
                )),
            );
        }
        self.event_popovers.close();
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
                self.set_event_date(which, date, &mut input.state);
            }
            Some(PopoverEdit::Time(which, hour, minute)) => {
                self.set_event_time(which, hour, minute, &mut input.state);
            }
            None => {}
        }
    }

    /// Flip the selected event's all-day flag.
    fn toggle_all_day(&mut self, doc: &mut ui::widget::Edits<'_>) {
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
    fn set_event_date(
        &mut self,
        which: FormPopover,
        date: NaiveDate,
        doc: &mut ui::widget::Edits<'_>,
    ) {
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
    fn set_event_time(
        &mut self,
        which: FormPopover,
        h: u8,
        m: u8,
        doc: &mut ui::widget::Edits<'_>,
    ) {
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

    /// Every event and task as an iCalendar file, and the name to suggest
    /// for it.
    fn ics_export(&mut self) -> Result<(String, String), String> {
        self.dirty = true;
        if self.model.events.is_empty() && self.model.tasks.is_empty() {
            self.status = Some("Nothing to export yet".into());
            return Err("There are no events or tasks to export".into());
        }
        Ok((
            ics::export(&self.model),
            ics::suggested_filename(self.anchor),
        ))
    }

    /// Save every event and task to an iCalendar file the person chooses.
    fn export_ics(
        &mut self,
        cx: &mut ui::framework::EffectCx<Calendar, Self>,
    ) -> ui::framework::EffectAnswer {
        let (ics, name) = self.ics_export()?;
        let request =
            ui::portal::PickRequest::save("Export calendar", "Export", &name).filter(ics_filter());
        Ok(
            cx.save_file(request, ics.into_bytes(), |app: &mut Self, saved, _| {
                app.status = Some(if saved {
                    format!(
                        "Exported {} events · {} tasks",
                        app.model.events.len(),
                        app.model.tasks.len()
                    )
                } else {
                    "Export cancelled".into()
                });
                app.dirty = true;
                Ok(ui::framework::Effected::done())
            }),
        )
    }

    /// Import the events and tasks of an iCalendar file the person chooses
    /// through the real [`ics::parse`] path, as one action. Items already in
    /// the calendar are not added twice.
    fn import_ics(cx: &mut ui::framework::EffectCx<Calendar, Self>) -> ui::framework::Effected {
        let request = ui::portal::PickRequest::open("Import a calendar", "Import")
            .filter(ics_filter())
            .all_files();
        cx.choose_file(request, |app: &mut Self, chosen, cx| {
            let Some(file) = chosen else {
                return Ok(ui::framework::Effected::done());
            };
            let payload = String::from_utf8(file.bytes)
                .map_err(|_| format!("{} is not an iCalendar file", file.name))?;
            let import = crate::resource::import(&payload)
                .ok_or_else(|| format!("{} holds no events or tasks", file.name))?;
            cx.submit(import);
            app.status = Some(format!("Imported calendar items from {}", file.name));
            app.dirty = true;
            Ok(ui::framework::Effected::done())
        })
    }

    fn cycle_event<F: Fn(&mut Event)>(&mut self, doc: &mut ui::widget::Edits<'_>, f: F) {
        if let Some(Sel::Event(id)) = self.selection
            && let Some(e) = self.model.event_mut(id)
        {
            let before = e.clone();
            f(e);
            let after = e.clone();
            if self.submit_event_change(doc, &before, &after) {
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
            self.sidebar_menu.set_viewport(self.sidebar_menu_viewport());
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
            let now = self.now();
            (f64::from(now.hour()) + f64::from(now.minute()) / 60.0 - 1.0).max(0.0)
        } else {
            working
        };
        self.timeline.layout(area, focus_hour, days.len());
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

    fn segmented_rects(&self) -> (Vec<Rect>, Vec<Rect>) {
        if self.detail_open || self.sidebar_menu.is_open() {
            return (Vec::new(), Vec::new());
        }
        let sidebar = self.sidebar.rect();
        self.hits
            .iter()
            .filter_map(|(rect, action)| matches!(action, Action::View(_)).then_some(*rect))
            .partition(|rect| !sidebar.contains(rect.center()))
    }

    fn handle_segmented_touch(
        &mut self,
        touch: &winit::event::Touch,
        scale: f64,
        edits: &mut ui::widget::Edits<'_>,
    ) -> bool {
        let (header, sidebar) = self.segmented_rects();
        let selected = View::ALL
            .iter()
            .position(|view| *view == self.view)
            .unwrap_or(0);
        let groups = [
            SegmentedTouchGroup::new(0, &header, selected),
            SegmentedTouchGroup::new(1, &sidebar, selected),
        ];
        let result = self.segmented_touch.handle(touch, scale, &groups);
        if let SegmentedTouchResult::Selected { index, .. } = result {
            let Some(&view) = View::ALL.get(index) else {
                return true;
            };
            if self.dispatcher.is_some() {
                self.invoke_effect(&crate::resource::CalendarEffect::ShowView {
                    view: view.label().to_lowercase(),
                });
            } else {
                self.apply_action(Action::View(view), edits);
            }
            self.dirty = true;
        }
        result.handled()
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the complete local action routing table remains intentionally centralized so every hit and semantic action shares one persistence boundary"
    )]
    fn apply_action(&mut self, action: Action, doc: &mut ui::widget::Edits<'_>) {
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
                // The list's tasks show in the sidebar, under the lists, so
                // the drawer stays open; choosing the shown list again puts
                // it away.
                self.list_sel = (self.list_sel != Some(s)).then_some(s);
            }
            Action::CloseList => self.list_sel = None,
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
                self.sidebar_menu.set_viewport(self.sidebar_menu_viewport());
                self.sidebar_menu.open_at(anchor, None);
            }
            Action::SetCalendarVisible { id, visible } => {
                self.status = Some(match self.set_calendar_visibility(&id, visible) {
                    Ok(()) => Self::visibility_status(visible).into(),
                    Err(error) => error,
                });
            }
            Action::ToggleAccountCollapsed(id) => {
                self.toggle_account_collapsed(&id);
            }
            Action::Sync => self.start_sync(doc),
            Action::Export => self.invoke_effect(&crate::resource::CalendarEffect::ExportIcs),
            Action::Import => self.invoke_effect(&crate::resource::CalendarEffect::ImportIcs),
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
            Action::SetEventColor(palette) => self.cycle_event(doc, |e| e.color = palette),
            Action::DiscardDraft(id) => {
                self.fresh_draft = None;
                self.delete_event(id, doc);
                self.status = None;
            }
            Action::DeleteTask(id) => self.delete_task(id, doc),
            Action::DismissStatus => self.status = None,
            Action::Noop => {}
        }
        // Discrete actions are worth an immediate save; if the action didn't
        // touch the document (view/navigation) the head check in `maybe_persist`
        // makes this a no-op.
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
                        .checked(self.list_sel == Some(*selection)),
                )
            }
            Action::CloseList => (
                "calendar.list:close".into(),
                "Close task list".into(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
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
            Action::SetEventColor(palette) => (
                format!("calendar.event:colour:{}", palette.index()),
                palette.name().to_string(),
                Role::RadioButton,
                AccessState::new().focusable(true).checked(matches!(
                    self.selection,
                    Some(Sel::Event(id)) if self.model.event(id).is_some_and(|event| event.color == *palette)
                )),
            ),
            Action::DiscardDraft(id) => (
                format!("{}:discard", crate::actions::event_resource_id(*id)),
                "Discard new event".into(),
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
            self.commit_choices(&mut ctx.state, ctx.path);
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
            self.apply_action(a, &mut input.state);
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
        state: &mut ui::widget::Edits<'_>,
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
        state: &mut ui::widget::Edits<'_>,
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
        state: &mut ui::widget::Edits<'_>,
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
        state: &mut ui::widget::Edits<'_>,
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
            self.title_input
                .handle_event(input.event, &mut input.event_ctx());
            *input.handled = true;
            self.dirty = true;
            return true;
        }
        if form_active && self.form.location.contains(p) {
            self.title_input.set_focus(false);
            self.invite_input.set_focus(false);
            self.notes_editor.set_focus(false);
            self.location_input.set_focus(true);
            self.location_input
                .handle_event(input.event, &mut input.event_ctx());
            *input.handled = true;
            self.dirty = true;
            return true;
        }
        if form_active && self.form.invite.contains(p) {
            self.title_input.set_focus(false);
            self.location_input.set_focus(false);
            self.notes_editor.set_focus(false);
            self.invite_input.set_focus(true);
            self.invite_input
                .handle_event(input.event, &mut input.event_ctx());
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
                self.notes_editor
                    .handle_event(input.event, &mut input.event_ctx_at(n));
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
            self.apply_action(a, &mut input.state);
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
            self.title_input
                .handle_event(input.event, &mut input.event_ctx());
            self.flush_form_edits(&mut input.state);
            self.dirty = true;
            return *input.handled;
        } else if self.location_input.is_focused() {
            self.location_input
                .handle_event(input.event, &mut input.event_ctx());
            self.flush_form_edits(&mut input.state);
            self.dirty = true;
            return *input.handled;
        } else if self.invite_input.is_focused() {
            self.invite_input
                .handle_event(input.event, &mut input.event_ctx());
            self.submit_invite(&mut input.state);
            self.dirty = true;
            return *input.handled;
        }
        false
    }
}

impl Default for CalendarApp {
    fn default() -> Self {
        Self::new()
    }
}

/// What an item's `open` shows, by the kind its resource id names.
enum Open<'a> {
    Event {
        id: &'a str,
    },
    Occurrence {
        event_id: &'a str,
        original_date: &'a str,
    },
    Task {
        id: &'a str,
    },
    Project {
        id: &'a str,
    },
    Calendar {
        id: &'a str,
    },
}

impl Widget for CalendarApp {
    ui::provides!(dyn ui::widget::controls::ActionControls);

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let (state, path) = ui::doc::document_ref(state, path);
        self.adopt_person_accounts();
        if !self.seeded {
            // Whatever the document holds is now authoritative for the UI.
            self.model = Calendar::read_whole(state).unwrap_or_default();
            self.revalidate_selection();
            self.model_heads = (state.base_heads(), state.pending_ops());
            self.seeded = true;
        }
        self.refresh_display();
        // Reconcile the in-memory model with the document when it moved out of
        // band — a Ctrl+K palette action mutated the CRDT, or a peer's change
        // merged in — so those events/tasks show up in the views just like an
        // on-screen edit does. Cheap and idempotent: only when the heads differ
        // from what the model was last derived from, and events/tasks re-read to
        // the same values a handler would have set. (Notes live in the document
        // and are read by the editor directly, so this never disturbs them.)
        let heads = (state.base_heads(), state.pending_ops());
        if heads != self.model_heads {
            self.model = Calendar::read_whole(state).unwrap_or_default();
            self.revalidate_selection();
            self.model_heads.clone_from(&heads);
        }
        let opens = std::mem::take(&mut *self.opens.borrow_mut());
        for item in opens {
            if let Err(error) = self.open_item(&item) {
                tracing::debug!(%error, "calendar: an item to open is gone");
            }
        }
        self.ensure_bound(state);
        // Seed the form's title/location/time widgets from the current event
        // whenever the selection changes.
        self.ensure_form_bound();
        if let Some(which) = self.staged_popover.take() {
            self.open_popover(which);
        }
        if self.shots_pending_calendar_menu && self.form_event.is_some() {
            self.shots_pending_calendar_menu = false;
            self.seed_choices();
            self.choices.calendar.dropdown.open_menu();
        }
        self.notes_editor.sync(state, path);
    }

    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, path: &ObjId) {
        if std::mem::take(&mut self.sync_requested) {
            self.start_sync(edits);
            if !self.syncing
                && let Some(reply) = self.sync_reply.take()
            {
                reply.finish(Ok(tilo_actions::ActionOutput::default()));
            }
        }
        // Drain a finished background CalDAV pull, if one landed since the last
        // frame, before binding the editor so a freshly-synced selection reads
        // back its notes.
        // A scenario's fresh event, to capture the New-Event form without the
        // toast an interactive creation shows.
        if std::mem::take(&mut self.staged_new_event) {
            self.new_event(edits);
            self.status = None;
        }
        self.poll_sync(edits);
        self.commit_choices(edits, path);
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
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        };
        taffy.widget_node(style, &[editor_node], node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        let new_rect = ui::widget::placed(&layout, origin);
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
                    let now = self.now();
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
            self.sidebar_menu.set_viewport(self.sidebar_menu_viewport());
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

        if let WindowEvent::Touch(touch) = event
            && self.handle_segmented_touch(touch, ctx.scale_factor, &mut ctx.state)
        {
            *ctx.handled = true;
            return;
        }

        // The sidebar's overflow menu owns input while it is open.
        if self.sidebar_menu.is_open() {
            Widget::handle_event(&mut self.sidebar_menu, event, ctx);
            match self.sidebar_menu.take_selection() {
                Some(0) => self.apply_action(Action::Export, &mut ctx.state),
                Some(1) => self.apply_action(Action::Import, &mut ctx.state),
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
                state: ctx.state.reborrow(),
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
            state: ctx.state.reborrow(),
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

        if self.handle_keyboard(event, *ctx.modifiers, &mut ctx.state, ctx.handled) {
            return;
        }

        if self.handle_mouse_click(
            &mut InputContext {
                event,
                scale_factor: ctx.scale_factor,
                modifiers: *ctx.modifiers,
                state: ctx.state.reborrow(),
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
            state: ctx.state.reborrow(),
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

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn claims_raw_touch(&self, point: Point) -> bool {
        let (header, sidebar) = self.segmented_rects();
        header
            .iter()
            .chain(&sidebar)
            .any(|rect| rect.contains(point))
    }

    fn wants_pointer_capture(&self) -> bool {
        self.segmented_touch.is_active() || self.agenda.wants_pointer_capture()
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
        state: &mut ui::widget::Edits<'_>,
        path: &ui::widget::ModelPath,
    ) -> bool {
        let _ = path;
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
        // layout (a resize crossed the breakpoint). Taking the change adopts
        // the width the rebuild will use: a host may update the tree again
        // before it lays it out, and must not be asked for the same rebuild
        // twice.
        let notes_w = self.detail_body_w();
        let width_changed = (notes_w - self.built_notes_w).abs() > 0.5;
        if width_changed {
            self.built_notes_w = notes_w;
        }
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

impl ui::widget::controls::ActionControls for CalendarApp {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        if let Some(search) = self.search_opener.take() {
            return Some(search);
        }
        self.children_mut()
            .into_iter()
            .find_map(ui::widget::controls::take_control_invocation)
    }
}

impl CalendarApp {
    /// Where a search hit or an agent's `open` queues the item to show.
    pub(crate) fn opens(&self) -> std::rc::Rc<std::cell::RefCell<Vec<String>>> {
        std::rc::Rc::clone(&self.opens)
    }

    /// Open an item — an event, an occurrence, a task, a project or a
    /// calendar — by its resource id, as its `open` asks.
    pub(crate) fn open_item(&mut self, item: &str) -> Result<(), String> {
        let of_calendar = item
            .strip_prefix(<Calendar as ui::tilo_resource::Resource>::KIND)
            .and_then(|rest| rest.strip_prefix('/'));
        let open = if let Some(id) = item.strip_prefix("calendar.collection:") {
            Open::Calendar { id }
        } else if let Some(rest) = of_calendar.and_then(|rest| rest.strip_prefix("events/")) {
            match rest.split_once("/occurrences/") {
                Some((event_id, original_date)) => Open::Occurrence {
                    event_id,
                    original_date,
                },
                None => Open::Event { id: rest },
            }
        } else if let Some(id) = of_calendar.and_then(|rest| rest.strip_prefix("tasks/")) {
            Open::Task { id }
        } else if let Some(id) = of_calendar.and_then(|rest| rest.strip_prefix("projects/")) {
            Open::Project { id }
        } else {
            return Err(format!("`{item}` is no event, task, project or calendar"));
        };
        match open {
            Open::Event { id } => {
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
            Open::Occurrence {
                event_id,
                original_date,
            } => {
                let id = event_id
                    .parse::<Id>()
                    .map_err(|_| "invalid event id".to_string())?;
                let original_date = NaiveDate::parse_from_str(original_date, "%Y-%m-%d")
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
            Open::Task { id } => {
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
            Open::Project { id } => {
                let id = id
                    .parse::<Id>()
                    .map_err(|_| "invalid project id".to_string())?;
                if self.model.project(id).is_none() {
                    return Err("project no longer exists".to_string());
                }
                self.list_sel = Some(ListSel::Project(id));
                // Opening a project is presentation-only. It must not route
                // through Back/Close because those deliberately discard a
                // pristine new-event draft via a durable delete action.
                self.detail_open = false;
                self.selection = None;
                self.blur_editor();
            }
            Open::Calendar { id } => {
                let calendar = self
                    .calendar_for_resource(Some(id))
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
        }
        self.dirty = true;
        Ok(())
    }

    /// Show every calendar again, after one calendar's events were shown.
    pub(crate) fn show_all_calendars(&mut self) {
        self.calendar_filter = None;
        self.detail_open = false;
        self.selection = None;
        self.detail_open = false;
        self.blur_editor();
        self.status = Some("Showing all calendars".into());
        self.dirty = true;
    }

    fn show_view(&mut self, view: View) {
        self.view = view;
        self.dirty = true;
    }

    /// Answer one of the calendar's effects, invoked by a control, the
    /// palette, an agent or a script.
    pub(crate) fn answer_effect(
        &mut self,
        effect: crate::resource::CalendarEffect,
        cx: &mut ui::framework::EffectCx<Calendar, Self>,
    ) -> ui::framework::EffectAnswer {
        use crate::resource::CalendarEffect;
        use ui::framework::Effected;
        match effect {
            CalendarEffect::Open { item } => {
                self.open_item(&item)?;
                Ok(Effected::done())
            }
            CalendarEffect::ShowAllCalendars => {
                self.show_all_calendars();
                Ok(Effected::done())
            }
            CalendarEffect::ShowView { view } => {
                self.show_view(match view.as_str() {
                    "month" => View::Month,
                    "week" => View::Week,
                    "day" => View::Day,
                    "agenda" => View::Agenda,
                    other => return Err(format!("there is no `{other}` view")),
                });
                Ok(Effected::done())
            }
            CalendarEffect::ShowMonth => {
                self.show_view(View::Month);
                Ok(Effected::done())
            }
            CalendarEffect::ShowWeek => {
                self.show_view(View::Week);
                Ok(Effected::done())
            }
            CalendarEffect::ShowDay => {
                self.show_view(View::Day);
                Ok(Effected::done())
            }
            CalendarEffect::ShowAgenda => {
                self.show_view(View::Agenda);
                Ok(Effected::done())
            }
            CalendarEffect::Sync => {
                if self.syncing || self.sync_requested {
                    return Err("calendar synchronization is already running".into());
                }
                self.sync_requested = true;
                self.sync_reply = Some(cx.reply());
                self.dirty = true;
                Ok(Effected::Pending)
            }
            CalendarEffect::ExportIcs => self.export_ics(cx),
            CalendarEffect::ImportIcs => Ok(Self::import_ics(cx)),
            CalendarEffect::PrepareInMail {
                event_id,
                attendee_id,
            } => self
                .prepare_in_mail(&event_id, &attendee_id)
                .map(Effected::value),
            CalendarEffect::SetColor { id, color } => {
                let color = crate::model::Palette::from_name(&color)
                    .ok_or_else(|| format!("no colour is called {color}"))?;
                self.set_calendar_color(&id, color)?;
                Ok(Effected::done())
            }
            CalendarEffect::SetVisibility { id, visible } => {
                self.set_calendar_visibility(&id, visible)?;
                self.status = Some(Self::visibility_status(visible).into());
                self.dirty = true;
                Ok(Effected::done())
            }
        }
    }

    /// The effects that cannot run right now, and why.
    pub(crate) fn effect_statuses(&self) -> Vec<ui::framework::EffectStatus> {
        use crate::resource::CalendarEffect;
        use ui::framework::EffectStatus;
        let mut statuses = Vec::new();
        if self.syncing || self.sync_requested {
            statuses.push(EffectStatus::unavailable(
                CalendarEffect::SYNC,
                "already_running",
                "Calendar synchronization is already running",
            ));
        } else if self.accounts.is_empty() && !self.sample_accounts {
            statuses.push(EffectStatus::unavailable(
                CalendarEffect::SYNC,
                "no_accounts",
                "Add a calendar account before synchronizing",
            ));
        }
        if self.model.events.is_empty() && self.model.tasks.is_empty() {
            statuses.push(EffectStatus::unavailable(
                CalendarEffect::EXPORT_ICS,
                "calendar_empty",
                "There are no events or tasks to export",
            ));
        }
        statuses
    }

    fn visibility_status(visible: bool) -> &'static str {
        if visible {
            "Calendar is now visible"
        } else {
            "Calendar is now hidden"
        }
    }

    /// The arguments of Mail's invitation draft for one attendee of an
    /// event: a reviewable draft, never a claim that anything was sent.
    fn prepare_in_mail(
        &mut self,
        event_id: &str,
        attendee_id: &str,
    ) -> Result<serde_json::Value, String> {
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
        self.status = Some("Invitation prepared for Mail — review it there before sending".into());
        self.dirty = true;
        Ok(serde_json::json!({
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
        }))
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
