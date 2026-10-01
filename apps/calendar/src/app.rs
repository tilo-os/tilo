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
use std::fmt::Write as _;
use std::path::PathBuf;
use tilo_automerge::ApplyIncoming;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ChangeHash, ObjId};
use chrono::{Datelike, Duration, Local, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use taffy::style_helpers::length;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use uuid::Uuid;
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::Widget;
use ui::a11y::{AccessNode, AccessState, Action as A11yAction, Role};
use ui::paint::PaintCtx;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::{
    LocalState,
    widget::{
        Checkbox, CheckboxFace, DatePicker, Dropdown, ExternalDatePickerHit, TextInput, Toggle,
        ToggleFace,
        chip::{Chip, ChipLead, ChipStyle},
    },
};

use tilo_rich_text::{RichTextField, RichTextSurface};

use crate::account::AccountStore;
use crate::agenda::{AgendaAction, AgendaMetrics, AgendaSource, AgendaView};
use crate::caldav::{self, SyncOutcome};
use crate::date;
use crate::doc::CalDoc;
use crate::draw_util::{
    fill_rect, fill_round, hairline, panel, row_ground, selected_row, selection_fill, stroke_line,
    stroke_round,
};
use crate::event_popovers::{
    CalendarChoices, EventPopovers, PopoverAnchors, PopoverEdit, PopoverKind as FormPopover,
};
use crate::ics;
use crate::input_context::InputContext;
use crate::metrics;
use crate::mini_month::{MiniMonth, Outcome as MiniMonthOutcome};
use crate::model::{Event, Id, ListSel, Model, Palette, Priority, Recurrence, View, seed_demo};
use crate::sidebar::{
    Action as SidebarAction, Mode as SidebarMode, Panes as SidebarPanes, Sidebar,
};
use crate::timeline_interaction::{
    TIMELINE_SNAP_MIN, TimelineEffect, TimelineInteraction, TimelineRenderEvent,
    TimelineRenderModel, TimelineSpan,
};
#[cfg(test)]
use crate::timeline_viewport::TIMELINE_MIN_HOUR_H;
use crate::topbar::{Action as TopbarAction, LeadingControl, Topbar};
use ui::surface::{card, separator};
use ui::tokens::colors::over;
use ui::widget::{Button, ButtonFace};

/// Placeholder shown in the empty title field of a new event.
const TITLE_PLACEHOLDER: &str = "New event";
/// Placeholder shown in the empty notes editor of an event (dim hint text that
/// disappears the moment the user types — never seeded as real body text).
const NOTES_PLACEHOLDER: &str = "Add details…";

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

/// First-run onboarding shown centered over an empty calendar (no events):
/// a friendly headline + hint instead of a blank grid.
const EMPTY_ONBOARDING_TITLE: &str = "No events yet";
const EMPTY_ONBOARDING_HINT: &str = "Click a day to add your first event";

const DETAIL_PAD: f64 = 22.0;
const NOTES_TOP_OFFSET: f64 = 296.0;
/// Base label size of a metadata chip (repeat / reminder / colour); the
/// renderer applies the text scale. The chip's height is the theme's compact
/// selector height, see `event_form_chip_h`.
#[cfg(test)]
const EVENT_FORM_CHIP_TEXT: f32 = 12.0;
/// Base size of the "EVENT · SAVES AUTOMATICALLY" / "NOTES" section labels.
const SECTION_LABEL_TEXT: f32 = 11.0;
/// Authored 1× height of the detail header row that carries the kind label
/// and the close / delete controls.
const DETAIL_HEADER_MIN_H: f64 = 22.0;

/// Base size of the month grid's weekday labels and day numbers.
const MONTH_LABEL_TEXT: f32 = 12.0;
/// Base size of a month-grid event chip label and the "+N more" line.
const MONTH_CHIP_TEXT: f32 = 10.0;
/// Authored 1× weekday header height; grows with the measured label line.
const MONTH_HEADER_MIN_H: f64 = 28.0;
/// Authored 1× today-pill radius; grows with the measured day-number line.
const MONTH_TODAY_PILL_R: f64 = 12.0;
/// Authored 1× event chip height; grows with the measured chip label line.
const MONTH_CHIP_MIN_H: f64 = 17.0;
/// Top of the day-number band inside a cell. Deliberately off the spacing
/// scale (`s1 + 2`): the band is sized by the today pill, and `s2` would push
/// the first chip a row further down than the 1× design allows.
const MONTH_NUM_TOP: f64 = 6.0;
/// Horizontal inset of event chips and the today pill inside a cell.
/// Deliberately off-scale (`s1 + 2`) — the authored 1× chip width.
const MONTH_CHIP_INSET: f64 = 6.0;
/// Month-grid geometry for the current text scale, derived once per frame in
/// [`CalendarApp::draw_month`] and shared by the weekday header, the day
/// cells, their chips and the hit rects pushed alongside them — one geometry
/// description for paint and pointer alike.
struct MonthMetrics {
    /// Weekday header row height.
    header_h: f64,
    /// Line height of a weekday label / day number.
    label_h: f64,
    /// Radius of the today pill; every day number is centred on its axis.
    pill_r: f64,
    /// Top of the first event chip inside a cell (also the `OpenDay` strip).
    chips_top: f64,
    /// Event chip height and stacking pitch.
    chip_h: f64,
    chip_pitch: f64,
    /// Line height of a chip label.
    chip_text_h: f64,
}

/// One month-grid paint pass: the paint context, the grid's metrics and row
/// height, and the hit rects its cells push.
struct MonthGridPaint<'a> {
    ctx: PaintCtx<'a>,
    m: &'a MonthMetrics,
    cell_h: f64,
    hits: &'a mut Vec<(Rect, Action)>,
}

impl MonthMetrics {
    fn new(theme: &Theme, text_scale: f32) -> Self {
        let sp = &theme.tokens.spacing;
        let label_h = metrics::line_h(MONTH_LABEL_TEXT, text_scale);
        let chip_text_h = metrics::line_h(MONTH_CHIP_TEXT, text_scale);
        // Every band is the authored 1× height, or its measured line plus the
        // same theme gap the painter leaves — rounded up — once scaled type
        // needs more; at 1× the design is reproduced exactly and every value
        // is a whole pixel at every scale.
        let header_h = metrics::at_least_whole(MONTH_HEADER_MIN_H, label_h + f64::from(sp.s3));
        let pill_r = metrics::at_least_whole(MONTH_TODAY_PILL_R, label_h / 2.0 + f64::from(sp.s1));
        // The first chip starts an `s1` below the day-number band (6 + 24 + 4
        // = 34 at 1×), the gap the painter keeps between pill and chip.
        let chips_top = MONTH_NUM_TOP + 2.0 * pill_r + f64::from(sp.s1);
        let chip_h = metrics::at_least_whole(MONTH_CHIP_MIN_H, chip_text_h + f64::from(sp.s1));
        Self {
            header_h,
            label_h,
            pill_r,
            chips_top,
            chip_h,
            chip_pitch: chip_h + f64::from(sp.s1),
            chip_text_h,
        }
    }
}

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
/// Above this theme scale, an editor needs a wider canvas for its real labels
/// and controls instead of squeezing 2× glyphs into the normal modal grid.
const LARGE_TEXT_SCALE: f32 = 1.35;
/// Metadata chips are compact at the default text size but must grow with
/// text-only accessibility scaling: their border is a container, never a
/// decoration drawn across the label.
const EVENT_FORM_CHIP_H: f64 = 28.0;
const EVENT_FORM_CHIP_GAP: f64 = 6.0;

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
    CycleRecurrenceInterval,
    CycleTimezone,
    CycleReminder,
    ToggleSelectedTask,
    OpenTaskPopover(TaskPopover),
    DismissTaskPopover,
    SetTaskDue(Option<NaiveDate>),
    ShiftTaskDueMonth(i32),
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
    Invite,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TaskPopover {
    Due,
    Priority,
    Project,
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
    /// Read-only scheduling feedback. It is present only when this locally
    /// available event overlaps another saved event, and is deliberately not a
    /// fake remote free/busy claim.
    conflict: Rect,
    location: Rect,
    calendar: Rect,
    invite: Rect,
    chips_y: f64,
    /// Y offset (from the detail rect top) at which the NOTES section starts.
    notes_top: f64,
}

impl FormRects {
    fn offset_y(&mut self, offset: f64) {
        for rect in [
            &mut self.title,
            &mut self.allday,
            &mut self.start_date,
            &mut self.start_time,
            &mut self.end_date,
            &mut self.end_time,
            &mut self.conflict,
            &mut self.location,
            &mut self.calendar,
            &mut self.invite,
        ] {
            *rect = Rect::new(rect.x0, rect.y0 + offset, rect.x1, rect.y1 + offset);
        }
        self.chips_y += offset;
        self.notes_top += offset;
    }
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
    /// Self-contained date/time/calendar picker controller: exclusive open
    /// state, embedded widgets, drawing, hit-testing and event routing.
    event_popovers: EventPopovers,
    task_popover: Option<TaskPopover>,
    /// Shared date-grid widget embedded in the task due-date popover.
    task_due_picker: DatePicker,
    /// Shared searchable/scrollable selector for all Calendar projects.
    task_project_choice: LocalState<String>,
    task_project_dropdown: Dropdown,
    /// Stable dropdown value to project mapping; display labels are owned by
    /// the shared widget and may legitimately be duplicated.
    task_project_values: Vec<(String, Option<Id>)>,
    task_project_last_choice: String,
    task_priority_choice: LocalState<String>,
    task_priority_dropdown: Dropdown,
    task_priority_last_choice: String,
    /// Cached form field geometry (see [`FormRects`]).
    form: FormRects,
    /// Shots-only: spawn a fresh event on the first sync so the offscreen
    /// renderer can capture the New-Event form. No effect on live runtime.
    shots_pending_new_event: bool,
    /// A deterministic visual form state should not include a transient toast
    /// that only exists after an interactive creation action.
    shots_suppress_status: bool,
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
    last_persist: ui::time::Instant,

    // --- responsive state ------------------------------------------------
    /// Responsive drawer state, geometry and chrome interaction.
    sidebar: Sidebar,
    /// Responsive navigation chrome and its pointer hit map.
    topbar: Topbar,
    /// Cached from the current theme during layout so bespoke geometry can
    /// make the same accessibility breakpoints as text painting.
    text_scale: f32,
    /// The theme's spacing scale, cached alongside `text_scale` so the
    /// renderer-less layout pass reads the same tokens paint does.
    spacing: ui::tokens::spacing::Spacing,
    /// The theme's density metrics (selector / control heights), cached in
    /// `build_layout` so the renderer-less layout pass reads the same tokens
    /// the painters do.
    tokens_metrics: ui::tokens::Metrics,
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
        let search_provider = crate::search_provider::CalendarSearchProvider::new(&model);

        let task_project_choice = LocalState::new(String::new());
        let task_priority_choice = LocalState::new(String::new());
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
                .with_font_size(20.0)
                .with_id("event_title"),
            location_input: TextInput::local()
                .with_placeholder("Add location")
                .with_id("event_location"),
            invite_input: TextInput::local()
                .with_placeholder("Invite by email — press Enter")
                .with_id("event_invite"),
            form_event: None,
            event_popovers: EventPopovers::new(today),
            task_popover: None,
            task_due_picker: DatePicker::new(today).with_accessible_name("Task due date"),
            task_project_dropdown: Dropdown::from_state(Vec::new(), &task_project_choice)
                .with_accessible_name("Task project")
                .with_placeholder("No project"),
            task_project_choice,
            task_project_values: Vec::new(),
            task_project_last_choice: String::new(),
            task_priority_dropdown: Dropdown::from_state(Vec::new(), &task_priority_choice)
                .with_accessible_name("Task priority"),
            task_priority_choice,
            task_priority_last_choice: String::new(),
            form: FormRects::default(),
            shots_pending_new_event: false,
            shots_suppress_status: false,
            shots_pending_popover: None,
            demo,
            data_path,
            saved_heads: Vec::new(),
            force_persist: false,
            last_persist: ui::time::now(),
            sidebar: Sidebar::new(),
            topbar: Topbar::new(),
            text_scale: 1.0,
            spacing: ui::tokens::spacing::Spacing::default(),
            tokens_metrics: ui::tokens::Metrics::BASE,
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
            detail_scroll_y: 0.0,
            detail_scroll_max: 0.0,
            shots_detail_scroll_to_end: false,
            editor_node: None,
            built_notes_w: -1.0,
            hits: Vec::new(),
            last_mouse: Point::ZERO,
            status: None,
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
        self.narrow_screen = NarrowScreen::Calendar;
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
        let popover = match picker {
            "priority" => Some(TaskPopover::Priority),
            "project" => Some(TaskPopover::Project),
            _ => Some(TaskPopover::Due),
        };
        self.task_popover = popover;
        let due = self
            .selected_task()
            .and_then(|id| self.model.task(id))
            .and_then(|task| task.due)
            .unwrap_or(self.anchor);
        self.task_due_picker.set_selected(due);
        if popover == Some(TaskPopover::Project) {
            self.open_task_project_dropdown();
        } else if popover == Some(TaskPopover::Priority) {
            self.open_task_priority_dropdown();
        }
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

    /// Deterministic transport-state presentation for visual regression shots.
    pub fn shots_set_sync_state(&mut self, state: &str) {
        self.syncing = state == "syncing";
        self.status = Some(match state {
            "syncing" => "Syncing calendars…".into(),
            "offline" => "Offline · changes will sync when you reconnect".into(),
            "conflict" => {
                "1 change also changed on the server; your version is safe and still queued".into()
            }
            "error" => "Sync failed · check the account in Settings and retry".into(),
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

    /// Open the calendar/account selector popover for a screenshot.
    pub fn shots_open_calendar_popover(&mut self) {
        self.shots_pending_new_event = true;
        self.shots_pending_popover = Some(FormPopover::Calendar);
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
            self.status = Some("No calendar accounts yet — add one in Settings".into());
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
                    "Synced {calendars} calendars · {added} new, {updated} updated, {removed} removed"
                )
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
                label: format!("{} · {}", account.label(), calendar.name),
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
                self.invite_input
                    .set_placeholder(if e.attendees.is_empty() {
                        "Invite by email — press Enter"
                    } else {
                        "Invite another person by email"
                    });
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
            "Added {email} — prepare the invitation in Mail to send it"
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
        self.narrow_screen = NarrowScreen::Calendar;
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
        self.task_project_dropdown.close();
        self.task_priority_dropdown.close();
    }

    fn open_task_project_dropdown(&mut self) {
        self.task_project_values = std::iter::once((String::new(), None))
            .chain(
                self.model
                    .projects
                    .iter()
                    .map(|project| (project.id.to_string(), Some(project.id))),
            )
            .collect();
        let selected = self
            .selected_task()
            .and_then(|id| self.model.task(id))
            .and_then(|task| task.project);
        let choice = self
            .task_project_values
            .iter()
            .find(|(_, project)| *project == selected)
            .map_or_else(String::new, |(value, _)| value.clone());
        self.task_project_choice.set(choice.clone());
        self.task_project_last_choice.clone_from(&choice);
        self.task_project_dropdown.set_value_options(
            std::iter::once((String::new(), "No project".to_string()))
                .chain(
                    self.model
                        .projects
                        .iter()
                        .map(|project| (project.id.to_string(), project.name.clone())),
                )
                .collect(),
        );
        self.task_project_dropdown.set_selected(choice);
        self.task_project_dropdown.open_menu();
    }

    fn commit_task_project_dropdown(&mut self, doc: &mut AutoCommit, path: &ObjId) {
        self.task_project_dropdown.sync(doc, path);
        let choice = self.task_project_choice.get();
        if self.task_popover != Some(TaskPopover::Project)
            || choice == self.task_project_last_choice
        {
            return;
        }
        self.task_project_last_choice.clone_from(&choice);
        if let Some((_, project)) = self
            .task_project_values
            .iter()
            .find(|(value, _)| *value == choice)
        {
            self.set_selected_task_project(*project, doc);
        }
    }

    fn open_task_priority_dropdown(&mut self) {
        let priority = self
            .selected_task()
            .and_then(|id| self.model.task(id))
            .map_or(Priority::None, |task| task.priority);
        let value = match priority {
            Priority::High => "p1",
            Priority::Medium => "p2",
            Priority::Low => "p3",
            Priority::None => "none",
        }
        .to_string();
        self.task_priority_choice.set(value.clone());
        self.task_priority_last_choice.clone_from(&value);
        self.task_priority_dropdown.set_value_options(vec![
            ("p1".into(), "P1 · High".into()),
            ("p2".into(), "P2 · Medium".into()),
            ("p3".into(), "P3 · Low".into()),
            ("none".into(), "No priority".into()),
        ]);
        self.task_priority_dropdown.set_selected(value);
        self.task_priority_dropdown.open_menu();
    }

    fn commit_task_priority_dropdown(&mut self, doc: &mut AutoCommit, path: &ObjId) {
        self.task_priority_dropdown.sync(doc, path);
        let value = self.task_priority_choice.get();
        if self.task_popover != Some(TaskPopover::Priority)
            || value == self.task_priority_last_choice
        {
            return;
        }
        self.task_priority_last_choice.clone_from(&value);
        let priority = match value.as_str() {
            "p1" => Some(Priority::High),
            "p2" => Some(Priority::Medium),
            "p3" => Some(Priority::Low),
            "none" => Some(Priority::None),
            _ => None,
        };
        if let Some(priority) = priority {
            self.set_selected_task_priority(priority, doc);
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
        self.status = Some("New event".into());
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
            Some(PopoverEdit::Calendar(target)) => {
                self.set_event_calendar(&target, input.state);
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
                    "Exported {} events · {} tasks to {}",
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
            "Imported {added_e} events · {added_t} tasks from {source}"
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
    /// `apply_layout`. This is the one place breakpoints are decided, along two
    /// independent axes:
    ///
    /// * **sidebar** — responsive inline panel / overlay drawer, delegated to
    ///   [`Sidebar`] (backed by the shared [`ui::widget::SidebarDrawer`]).
    /// * **editor** — a centered modal card floating over the still-visible
    ///   calendar at/above [`EDITOR_OVERLAY_MIN`]; a full-screen page that
    ///   replaces the calendar below it (only one of {calendar, detail} is laid
    ///   out at a time in that mode).
    #[allow(
        clippy::too_many_lines,
        reason = "one responsive layout pass keeps sidebar, editor, embedded fields, and scroll geometry synchronized"
    )]
    fn relayout(&mut self) {
        let r = self.rect;
        // Two independent responsive axes, each re-derived from the current
        // width so a live resize reflows both.
        self.sidebar.set_text_scale(self.text_scale);
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
        self.topbar.set_text_scale(self.text_scale);
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
                    self.agenda.layout(
                        area,
                        &AgendaSource {
                            model: &self.model,
                            anchor: self.anchor,
                            hidden_calendars: &hidden,
                            calendar_filter: self.calendar_filter.as_deref(),
                        },
                        &AgendaMetrics::new(&self.spacing, self.text_scale),
                    );
                }
                View::Month => {}
            }
        }

        // Notes editor placement, derived from whichever detail rect is active.
        // The column width tracks the detail rect (capped for readability) so
        // the narrow single-pane screen fills its width instead of leaving a
        // fixed-width column stranded against a wide empty margin.
        self.notes_w = self.detail_body_w();
        if matches!(self.selection, Some(Sel::Event(_))) {
            let compact_invite = self.text_scale >= LARGE_TEXT_SCALE && self.notes_w < 440.0;
            let has_guests = self
                .selection
                .and_then(|selection| match selection {
                    Sel::Event(id) => self.model.event(id),
                    Sel::Task(_) => None,
                })
                .is_some_and(|event| !event.attendees.is_empty());
            self.invite_input.set_placeholder(if compact_invite {
                "Invite email · Enter"
            } else if has_guests {
                "Invite another person by email"
            } else {
                "Invite by email — press Enter"
            });
        }
        // Compute the editable event-form geometry (fields + pickers) for the
        // current detail, then position the embedded inputs. The notes section
        // begins below the form when an event is being edited; a task detail
        // (or no selection) falls back to the fixed offset.
        self.form = self.form_layout();
        let editing_event =
            matches!(self.selection, Some(Sel::Event(_))) && self.detail_rect.area() > 0.0;
        if editing_event {
            self.form.offset_y(-self.detail_scroll_y);
        }
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
            self.invite_input.apply_layout_external(self.form.invite);
        } else {
            // Park the inputs off-screen so a stale click can't hit them.
            self.title_input
                .apply_layout_external(Rect::new(-10000.0, -10000.0, -9900.0, -9900.0));
            self.location_input
                .apply_layout_external(Rect::new(-10000.0, -10000.0, -9900.0, -9900.0));
            self.invite_input
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
        let w = (self.modal_body_width() + 2.0 * DETAIL_PAD)
            .min((content_w - 2.0 * MODAL_MARGIN).max(240.0));
        let desired_h = match self.selection {
            Some(Sel::Task(id)) => {
                let subtask_rows = self.model.subtasks_of(id).len().min(8) as f64;
                430.0 + subtask_rows * 24.0
            }
            // At large text, use the available vertical space: the form grows
            // substantially and the notes editor must remain meaningfully
            // visible rather than being clipped into a token strip.
            Some(Sel::Event(_)) if self.text_scale >= LARGE_TEXT_SCALE => {
                (r.height() - 2.0 * MODAL_MARGIN).max(360.0)
            }
            Some(Sel::Event(_)) => 680.0,
            None => 440.0,
        };
        let h = desired_h.min((r.height() - 2.0 * MODAL_MARGIN).max(360.0));
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
        // plus the "EVENT" kind label row, whose height follows the measured
        // label line so scaled type never runs into the title field.
        let top = r.y0 + 18.0 + if fullscreen { 34.0 } else { 0.0 };
        let s2 = f64::from(self.spacing.s2);
        f.header_bottom = top + self.detail_header_h();
        let mut y = f.header_bottom + s2;

        let large_text = self.text_scale >= LARGE_TEXT_SCALE;
        let field_h = if large_text { 52.0 } else { 40.0 };
        let title_h = if large_text { 54.0 } else { 44.0 };
        let all_day_h = if large_text { 38.0 } else { 30.0 };
        f.title = Rect::new(x, y, x + w, y + title_h);
        y += title_h + 16.0;

        f.allday = Rect::new(x, y, x + w, y + all_day_h);
        y += all_day_h + 14.0;

        // Start row: [label] [date field] [time field]. Time is hidden for an
        // all-day event, and the date field then spans the row.
        let label_w = if large_text { 90.0 } else { 52.0 };
        let time_w = if large_text { 150.0 } else { 92.0 };
        let gap = if large_text { 14.0 } else { 12.0 };
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

        let has_schedule_conflict = self
            .selection
            .and_then(|selection| match selection {
                Sel::Event(id) => self.model.event(id),
                Sel::Task(_) => None,
            })
            .is_some_and(|event| !self.local_schedule_conflicts(event).is_empty());
        if has_schedule_conflict {
            // The notice carries two independently meaningful lines. Give
            // scaled glyphs their own vertical lanes instead of allowing the
            // count line to touch the warning border at 200% text.
            let conflict_h = if large_text { 72.0 } else { 40.0 };
            f.conflict = Rect::new(x, y, x + w, y + conflict_h);
            y += conflict_h + 14.0;
        }

        f.location = Rect::new(x, y, x + w, y + field_h);
        y += field_h + 14.0;

        f.calendar = Rect::new(x, y, x + w, y + field_h);
        y += field_h + 12.0;

        f.invite = Rect::new(x, y, x + w, y + field_h);
        y += field_h + 16.0;

        f.chips_y = y;
        // Metadata chips flow to additional rows on a narrow sheet. Reserve
        // those rows in the same breakpoint domain as the painter so NOTES
        // never collides with a wrapped reminder/colour/timezone control.
        let chip_rows = if w < 440.0 {
            // At 200% each of the six metadata controls can require its own
            // row. Reserve the worst case; the detail pane scrolls when the
            // resulting form is taller than the phone viewport.
            if large_text { 6.0 } else { 3.0 }
        } else if large_text {
            3.0
        } else {
            2.0
        };
        y += chip_rows * event_form_chip_row_pitch(self.text_scale) + 36.0;

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
            self.modal_body_width()
        } else {
            (self.rect.width() - 2.0 * DETAIL_PAD).clamp(240.0, 620.0)
        }
    }

    /// Width reserved for the modal's form and notes column.  The value is
    /// derived from the current viewport as `build_layout` precedes the final
    /// detail rect in a frame.
    fn modal_body_width(&self) -> f64 {
        if self.text_scale < LARGE_TEXT_SCALE {
            return MODAL_BODY_W;
        }
        let desired = MODAL_BODY_W * f64::from(self.text_scale);
        let available = self.rect.width() - 2.0 * MODAL_MARGIN - 2.0 * DETAIL_PAD;
        desired.min(available.max(240.0))
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

    /// Height of the detail header row (kind label + close / delete controls):
    /// the authored 22px at 1×, or the measured label line plus a token gap
    /// once scaled type needs more. Layout and paint both read this.
    fn detail_header_h(&self) -> f64 {
        metrics::at_least_whole(
            DETAIL_HEADER_MIN_H,
            metrics::line_h(SECTION_LABEL_TEXT, self.text_scale) + f64::from(self.spacing.s2),
        )
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
                self.discard_pristine_draft(doc);
                self.detail_open = false;
                self.selection = None;
                self.narrow_screen = NarrowScreen::Calendar;
                self.blur_editor();
            }
            Action::ToggleSidebar => self.sidebar.toggle_open(),
            Action::DismissSidebar => self.sidebar.set_open(false),
            Action::ToggleSidebarCollapsed => self.sidebar.toggle_collapsed(),
            Action::BackToCalendar => {
                self.discard_pristine_draft(doc);
                self.narrow_screen = NarrowScreen::Calendar;
                // The calendar screen owns the keyboard once the editor is off
                // it; release the form fields so keystrokes don't leak into the
                // now-hidden title/location/notes.
                self.blur_editor();
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
            Action::CycleColor => {
                self.cycle_event(doc, |e| {
                    e.color = Palette::from_index(e.color.index() + 1);
                });
            }
            Action::CycleRecurrence => {
                self.cycle_event(doc, |e| e.recurrence = e.recurrence.cycle());
            }
            Action::CycleRecurrenceInterval => {
                self.cycle_event(doc, |e| {
                    e.recurrence_interval = match e.recurrence_interval {
                        1 => 2,
                        2 => 4,
                        _ => 1,
                    };
                });
            }
            Action::CycleTimezone => {
                self.cycle_event(doc, |e| {
                    e.timezone = match e.timezone.as_str() {
                        "floating" => "UTC",
                        "UTC" => "Europe/Berlin",
                        "Europe/Berlin" => "America/New_York",
                        "America/New_York" => "Asia/Tokyo",
                        _ => "floating",
                    }
                    .into();
                });
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
            Action::OpenTaskPopover(popover) => {
                self.task_popover = Some(popover);
                let due = self
                    .selected_task()
                    .and_then(|id| self.model.task(id))
                    .and_then(|task| task.due)
                    .unwrap_or(self.anchor);
                self.task_due_picker.set_selected(due);
                if popover == TaskPopover::Project {
                    self.open_task_project_dropdown();
                } else if popover == TaskPopover::Priority {
                    self.open_task_priority_dropdown();
                }
            }
            Action::DismissTaskPopover => {
                self.task_popover = None;
                self.task_project_dropdown.close();
                self.task_priority_dropdown.close();
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
                "Show calendars sidebar".into(),
                Role::ToggleButton,
                AccessState::new()
                    .focusable(true)
                    .checked(self.sidebar.is_open()),
            ),
            Action::ToggleSidebarCollapsed => (
                "calendar.sidebar-width".into(),
                if self.sidebar.is_collapsed() {
                    "Expand calendars sidebar".into()
                } else {
                    "Collapse calendars sidebar".into()
                },
                Role::ToggleButton,
                AccessState::new()
                    .focusable(true)
                    .checked(!self.sidebar.is_collapsed()),
            ),
            Action::DismissSidebar => (
                "calendar.sidebar.dismiss".into(),
                "Dismiss calendars sidebar".into(),
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
            Action::BackToCalendar | Action::CloseDetail => (
                "calendar.back".into(),
                "Back to calendar".into(),
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
            Action::CycleRecurrence => (
                "calendar.event:repeat".into(),
                "Change repeat frequency".into(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Action::CycleRecurrenceInterval => (
                "calendar.event:repeat-interval".into(),
                "Change repeat interval".into(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Action::CycleTimezone => (
                "calendar.event:timezone".into(),
                "Change event time zone".into(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Action::CycleReminder => (
                "calendar.event:reminder".into(),
                "Change event reminder".into(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Action::CycleColor => (
                "calendar.event:color".into(),
                "Change event color".into(),
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
            Action::OpenPopover(FormPopover::Calendar) => (
                "calendar.event:calendar".into(),
                "Choose calendar".into(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Action::OpenTaskPopover(TaskPopover::Due) => (
                "calendar.task.due".into(),
                "Choose task due date".into(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Action::OpenTaskPopover(TaskPopover::Priority) => (
                "calendar.task.priority".into(),
                "Choose task priority".into(),
                Role::Button,
                AccessState::new().focusable(true),
            ),
            Action::OpenTaskPopover(TaskPopover::Project) => (
                "calendar.task.project".into(),
                "Choose task project".into(),
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

    /// Dates in the mini-month grid carrying at least one visible event. The
    /// root applies account-visibility policy once; the component only needs
    /// the dates it should mark.
    fn mini_month_event_dates(&self) -> HashSet<NaiveDate> {
        let hidden = self.hidden_calendar_ids();
        date::month_grid(self.anchor)
            .into_iter()
            .filter(|date| {
                self.model.events.iter().any(|event| {
                    event.occurs_on(*date)
                        && self.event_in_selected_calendar(event)
                        && event.visible_under(&hidden)
                })
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
        let ctx = &mut PaintCtx::new(scene, t, tr, theme);
        let Some(chrome) = self.sidebar.draw_chrome(
            ctx,
            SidebarPanes {
                window: self.rect,
                content: self.content_rect,
                detail: self.detail_rect,
            },
            (!self.topbar.switcher_visible()).then_some(self.view),
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
        // Inset of the sidebar's section headings from its edges.
        let pad = 18.0;
        let y = self.draw_sidebar_lists(ctx, chrome.body_y, pad, &mut hits);
        let y = self.draw_sidebar_calendars(ctx, y, pad, &mut hits);
        self.draw_sidebar_footer(ctx, y, pad, &mut hits);

        hits
    }
    #[expect(
        clippy::too_many_lines,
        reason = "the smart lists and project lists are painted in one top-to-bottom pass sharing the running y cursor"
    )]
    fn draw_sidebar_lists(
        &self,
        ctx: &mut PaintCtx<'_>,
        mut y: f64,
        pad: f64,
        hits: &mut Vec<(Rect, Action)>,
    ) -> f64 {
        let (scene, t, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let r = self.sidebar.rect();
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
            let hover = row.contains(self.last_mouse);
            selected_row(
                scene,
                t,
                theme,
                c.surfaces.raised,
                row,
                f64::from(theme.tokens.radii.md),
                active,
                hover,
            );
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
                draw_count_badge(
                    scene,
                    t,
                    tr,
                    theme,
                    Point::new(row.x1 - 18.0, row.center().y),
                    count,
                    row_ground(theme, c.surfaces.raised, active, hover),
                );
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
            let hover = row.contains(self.last_mouse);
            selected_row(
                scene,
                t,
                theme,
                c.surfaces.raised,
                row,
                f64::from(theme.tokens.radii.md),
                active,
                hover,
            );
            // colour tag
            fill_round(
                scene,
                t,
                Rect::new(
                    row.x0 + 12.0,
                    row.center().y - 5.0,
                    row.x0 + 22.0,
                    row.center().y + 5.0,
                ),
                3.0,
                p.color.color(theme),
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
                draw_count_badge(
                    scene,
                    t,
                    tr,
                    theme,
                    Point::new(row.x1 - 18.0, row.center().y),
                    count,
                    row_ground(theme, c.surfaces.raised, active, hover),
                );
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
    fn draw_sidebar_calendars(
        &self,
        ctx: &mut PaintCtx<'_>,
        mut y: f64,
        pad: f64,
        hits: &mut Vec<(Rect, Action)>,
    ) -> f64 {
        let (scene, t, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let r = self.sidebar.rect();
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
                fill_round(scene, t, sync, 6.0, c.lift(0.35));
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
                    fill_round(scene, t, header, 6.0, c.lift(0.35));
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
                        fill_round(scene, t, row, 6.0, c.lift(0.35));
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
                        fill_round(scene, t, sw, 4.0, pc);
                        draw_check(scene, t, ui::widget::Avatar::initial_ink(pc), sw);
                    } else {
                        stroke_round(scene, t, sw, 4.0, pc.with_alpha(0.6), 1.4);
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
                    hits.push((
                        row,
                        Action::SetCalendarVisible {
                            id: cal.id.clone(),
                            visible: !cal.visible,
                        },
                    ));
                    y += 32.0;
                }
            }
        }
        y
    }

    /// The footer: the mini upcoming-task rail (when there's room for it
    /// above the reserved mini-month band) followed by the mini-month
    /// navigator itself (part of [`Self::draw_sidebar`]).
    fn draw_sidebar_footer(
        &self,
        ctx: &mut PaintCtx<'_>,
        y: f64,
        pad: f64,
        hits: &mut Vec<(Rect, Action)>,
    ) {
        let y = self.draw_sidebar_task_rail(ctx, y, pad, hits);
        let event_dates = self.mini_month_event_dates();
        let mini_month = MiniMonth {
            anchor: self.anchor,
            today: self.today,
            event_dates: &event_dates,
        };
        hits.extend(
            mini_month
                .draw(ctx, self.sidebar.rect(), y, self.last_mouse)
                .into_iter()
                .map(|(rect, outcome)| (rect, Action::MiniMonth(outcome))),
        );
    }

    /// The mini upcoming-task rail — only when there are tasks to show AND
    /// there's vertical room left above the mini-month (the CALENDARS
    /// section above may have used it up; the agenda/day views still list
    /// the tasks in full either way) (part of [`Self::draw_sidebar_footer`]).
    fn draw_sidebar_task_rail(
        &self,
        ctx: &mut PaintCtx<'_>,
        mut y: f64,
        pad: f64,
        hits: &mut Vec<(Rect, Action)>,
    ) -> f64 {
        let (scene, t, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let r = self.sidebar.rect();
        // Reserve the bottom of the sidebar for the mini-month navigator.
        let mini_top = MiniMonth::content_limit(r);
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
            separator(scene, t, r.x0, r.x1, y, pad, c.surfaces.raised);
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
                    fill_round(scene, t, row, 6.0, c.lift(0.35));
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
        fill_rect(scene, t, area, c.surfaces.base);

        let m = MonthMetrics::new(theme, tr.text_scale());
        let cols = 7;
        let rows = 6;
        let cell_w = area.width() / cols as f64;
        let grid_top = area.y0 + m.header_h;
        let cell_h = (area.height() - m.header_h) / f64::from(rows);

        Self::draw_month_header(scene, t, tr, theme, area, cell_w, &m);

        let grid = date::month_grid(self.anchor);
        let mut grid_paint = MonthGridPaint {
            ctx: PaintCtx::new(scene, t, tr, theme),
            m: &m,
            cell_h,
            hits: &mut hits,
        };
        for (idx, day) in grid.iter().enumerate() {
            let row = idx / cols;
            let col = idx % cols;
            let cell = Rect::new(
                area.x0 + cell_w * col as f64,
                grid_top + cell_h * row as f64,
                area.x0 + cell_w * (col as f64 + 1.0),
                grid_top + cell_h * (row as f64 + 1.0),
            );
            self.draw_month_cell(&mut grid_paint, cell, *day);
        }
        // Grid rules once, on top of the cell tints, derived against the
        // month's own ground (HIG §9.5) — a half-alpha `border_standard` at
        // half a pixel vanished on the dark preset.
        for col in 0..=cols {
            // The closing rule is the last pixel column inside the grid.
            let x = if col == cols {
                area.x1 - 1.0
            } else {
                area.x0 + cell_w * col as f64
            };
            hairline(
                scene,
                t,
                c.surfaces.base,
                Point::new(x, grid_top),
                Point::new(x, area.y1),
            );
        }
        for row in 0..=rows {
            let y = if row == rows {
                area.y1 - 1.0
            } else {
                grid_top + cell_h * f64::from(row)
            };
            hairline(
                scene,
                t,
                c.surfaces.base,
                Point::new(area.x0, y),
                Point::new(area.x1, y),
            );
        }
        hits
    }

    /// The weekday header row (part of [`Self::draw_month`]).
    #[allow(
        clippy::many_single_char_names,
        reason = "t is the paint transform, c the colour set, m the month metrics and x/y the \
                  label origin — conventional short names for this draw step"
    )]
    fn draw_month_header(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        cell_w: f64,
        m: &MonthMetrics,
    ) {
        let c = &theme.tokens.colors;
        // Labels share the day numbers' left inset and are centred on the
        // measured header row, so column heads and dates form one left edge.
        let inset = f64::from(theme.tokens.spacing.s2);
        let y = area.y0 + (m.header_h - m.label_h) / 2.0;
        for (i, lbl) in date::WEEKDAY_SHORT.iter().enumerate() {
            let weekend = i >= 5;
            let col = if weekend { c.text_dim } else { c.neutral[7] };
            let x = area.x0 + cell_w * i as f64;
            tr.draw_text(
                scene,
                lbl,
                MONTH_LABEL_TEXT,
                col,
                t * Affine::translate((x + inset, y)),
                None,
            );
        }
    }

    /// One day cell: grid line, hit targets, background tint, and the date
    /// number (with a today pill) (part of [`Self::draw_month`]).
    #[allow(
        clippy::many_single_char_names,
        reason = "t is the paint transform, c the colour set and m the month metrics — \
                  conventional short names for this draw step"
    )]
    fn draw_month_cell(&self, grid: &mut MonthGridPaint<'_>, cell: Rect, day: NaiveDate) {
        let ctx = &mut grid.ctx;
        let (scene, t, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let (m, hits) = (grid.m, &mut *grid.hits);
        let c = &theme.tokens.colors;

        // Cell-level click targets are pushed FIRST so the event chips
        // drawn below win the reverse (topmost-last) hit-scan — clicking a
        // chip must open that event, not just re-anchor the day. The
        // date-number strip in turn wins over the empty cell body; it ends
        // exactly where the first chip starts, so the two never overlap.
        hits.push((cell, Action::PickDate(day)));
        hits.push((
            Rect::new(cell.x0, cell.y0, cell.x1, cell.y0 + m.chips_top),
            Action::OpenDay(day),
        ));

        let in_month = day.month() == self.anchor.month();
        let is_today = day == self.today;
        let is_anchor = day == self.anchor;
        let weekend = day.weekday().num_days_from_monday() >= 5;

        // The cell's own ground: the weekend and anchor tints are opaque
        // lifts off the month's base (the weekend wash wins where both
        // apply), and a timed chip's wash below is composited onto whichever
        // of them is actually under it.
        let cell_tint = if weekend && in_month {
            Some(c.lift(0.18))
        } else if is_anchor && !is_today {
            Some(c.lift(0.35))
        } else {
            None
        };
        if let Some(tint) = cell_tint {
            fill_rect(scene, t, cell, tint);
        }
        let cell_fill = cell_tint.unwrap_or(c.surfaces.base);

        // date number, with a today pill.
        let num = format!("{}", day.day());
        let num_color = if !in_month {
            c.neutral[4]
        } else if weekend {
            c.text_dim
        } else {
            c.neutral[9]
        };
        // Every date sits on the same centre line as the today pill, so the
        // highlighted number no longer rides lower than its neighbours.
        let pill_c = Point::new(
            cell.x0 + MONTH_CHIP_INSET + m.pill_r,
            cell.y0 + MONTH_NUM_TOP + m.pill_r,
        );
        if is_today {
            let pill = Circle::new(pill_c, m.pill_r);
            scene.fill(Fill::NonZero, t, c.accent[5], None, &pill);
            center_text(
                scene,
                tr,
                &num,
                MONTH_LABEL_TEXT,
                c.on_accent(4.5),
                Rect::from_center_size(pill_c, (2.0 * m.pill_r, 2.0 * m.pill_r)),
                t,
            );
        } else {
            tr.draw_text(
                scene,
                &num,
                MONTH_LABEL_TEXT,
                num_color,
                t * Affine::translate((
                    cell.x0 + f64::from(theme.tokens.spacing.s2),
                    pill_c.y - m.label_h / 2.0,
                )),
                None,
            );
        }

        self.draw_month_cell_chips(grid, cell, cell_fill, day);
    }

    /// The visible events of one month cell as the chip painter needs them:
    /// id, display title, palette, all-day flag and start.
    fn month_cell_events(&self, day: NaiveDate) -> Vec<(Id, String, Palette, bool, NaiveDateTime)> {
        self.visible_events_on(day)
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
            .collect()
    }

    /// The event chips inside one day cell, clipped to it (part of
    /// [`Self::draw_month_cell`]). `cell_fill` is the opaque ground the cell
    /// was painted with (base, or its weekend / anchor lift), which a timed
    /// chip's translucent wash is composited onto.
    #[allow(
        clippy::many_single_char_names,
        reason = "t is the paint transform, c the colour set and m the month metrics — \
                  conventional short names for this draw step"
    )]
    fn draw_month_cell_chips(
        &self,
        grid: &mut MonthGridPaint<'_>,
        cell: Rect,
        cell_fill: Color,
        day: NaiveDate,
    ) {
        let ctx = &mut grid.ctx;
        let (scene, t, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let (m, cell_h, hits) = (grid.m, grid.cell_h, &mut *grid.hits);
        let c = &theme.tokens.colors;
        let sp = &theme.tokens.spacing;
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            t,
            &cell,
        );
        let evs = self.month_cell_events(day);
        // Slots that fit below the day number, at most three. When the day
        // has more events than slots, the last slot carries the "+N more"
        // line instead of a chip, so the overflow note is never itself
        // clipped by the cell — but at least one chip is always drawn and
        // clickable, so a crowded day in a short window still opens its
        // events from the grid.
        let slots = (((cell_h - m.chips_top) / m.chip_pitch).floor().max(0.0) as usize).clamp(1, 3);
        let max_chips = if evs.len() > slots {
            slots.saturating_sub(1).max(1)
        } else {
            evs.len()
        };
        let mut cy = cell.y0 + m.chips_top;
        let radius = f64::from(theme.tokens.radii.sm);
        let text_y = (m.chip_h - m.chip_text_h) / 2.0;
        // The timed-event marker scales with the chip label so it stays a dot
        // beside the text rather than a speck under it.
        let dot_r = (m.chip_text_h / 4.0).round();
        let dot_x = f64::from(sp.s2);
        let timed_text_x = dot_x + 2.0 * dot_r + f64::from(sp.s1);
        for (i, (id, title, color, all_day, start)) in evs.iter().enumerate() {
            if i >= max_chips {
                let more = evs.len() - i;
                tr.draw_text(
                    scene,
                    &format!("+{more} more"),
                    MONTH_CHIP_TEXT,
                    c.text_dim,
                    t * Affine::translate((cell.x0 + f64::from(sp.s2), cy)),
                    None,
                );
                // The overflow line is itself a way into the hidden events:
                // it opens the day, with the same rect it is painted in.
                hits.push((
                    Rect::new(
                        cell.x0 + MONTH_CHIP_INSET,
                        cy,
                        cell.x1 - MONTH_CHIP_INSET,
                        (cy + m.chip_h).min(cell.y1),
                    ),
                    Action::OpenDay(day),
                ));
                break;
            }
            let chip = Rect::new(
                cell.x0 + MONTH_CHIP_INSET,
                cy,
                cell.x1 - MONTH_CHIP_INSET,
                cy + m.chip_h,
            );
            let col = color.color(theme);
            if *all_day {
                card(scene, t, chip, radius, col);
                let line = tr.elide_to_width(
                    title,
                    MONTH_CHIP_TEXT,
                    ui::num::f32_of_f64(chip.width() - 2.0 * MONTH_CHIP_INSET),
                );
                tr.draw_text(
                    scene,
                    &line,
                    MONTH_CHIP_TEXT,
                    ui::widget::Avatar::initial_ink(col),
                    t * Affine::translate((chip.x0 + MONTH_CHIP_INSET, chip.y0 + text_y)),
                    None,
                );
            } else {
                card(scene, t, chip, radius, over(cell_fill, col, 0.18));
                scene.fill(
                    Fill::NonZero,
                    t,
                    col,
                    None,
                    &Circle::new(Point::new(chip.x0 + dot_x + dot_r, chip.center().y), dot_r),
                );
                let label = format!("{} {}", date::fmt_time(start), title);
                let line = tr.elide_to_width(
                    &label,
                    MONTH_CHIP_TEXT,
                    ui::num::f32_of_f64(chip.width() - timed_text_x - MONTH_CHIP_INSET),
                );
                tr.draw_text(
                    scene,
                    &line,
                    MONTH_CHIP_TEXT,
                    c.neutral[9],
                    t * Affine::translate((chip.x0 + timed_text_x, chip.y0 + text_y)),
                    None,
                );
            }
            hits.push((chip, Action::OpenEvent(*id)));
            cy += m.chip_pitch;
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

    /// The surface the detail editor's content sits on: the Panel ground
    /// when it floats as a modal card, the raised pane otherwise. Every rule
    /// and chip edge inside the editor is derived against this.
    fn detail_ground(&self, theme: &Theme) -> Color {
        if matches!(self.detail_mode, DetailMode::Overlay) {
            ui::chrome::panel_ground(theme)
        } else {
            theme.tokens.colors.surfaces.raised
        }
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

        self.draw_detail_selection(
            &mut PaintCtx::new(scene, t, tr, theme),
            Rect::new(x, y, x + w, r.y1),
            &mut hits,
        );

        // Notes header (the editor itself is drawn separately in draw()). For
        // an event the notes sit below the form; a task uses the fixed offset.
        self.draw_detail_notes_header(scene, t, tr, theme, r, x, w);

        hits
    }

    /// The card/panel background — a dimmed floating card in overlay mode,
    /// or a docked panel with a left border otherwise (part of
    /// [`Self::draw_detail`]).
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
            // Dim the calendar behind the card — the shared modal scrim at the
            // toolkit's modal strength, so the grid stays visible beneath the
            // editor instead of vanishing under an opaque popover surface. A
            // click on the dim closes the editor. The card is a floating
            // panel, not a docked strip.
            ui::chrome::fill_scrim(
                scene,
                t,
                self.content_rect,
                ui::widget::modal_frame::MODAL_SCRIM_ALPHA,
                theme,
            );
            hits.push((self.content_rect, Action::CloseDetail));
            // Panel role (HIG §9.1): elevation shadow, Panel ground and the
            // derived Panel border, like every shell flyout.
            panel(
                scene,
                t,
                theme,
                r,
                ui::chrome::ChromeRole::Panel.radius(theme, r.height()),
            );
            // Backing guard so a press on the card padding is absorbed rather
            // than falling through to the dismiss scrim beneath it.
            hits.push((r, Action::Noop));
        } else {
            fill_rect(scene, t, r, c.surfaces.raised);
            hairline(
                scene,
                t,
                c.surfaces.raised,
                Point::new(r.x0, r.y0),
                Point::new(r.x0, r.y1),
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
            fill_round(scene, t, back, 8.0, c.lift(0.35));
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
        // In the modal card the controls sit on the kind-label header row and
        // share its centre line; the pushed page keeps them on the back bar.
        let mid = if modal {
            r.y0 + 18.0 + self.detail_header_h() / 2.0
        } else {
            r.y0 + 25.0
        };
        if modal {
            let close = Rect::new(bx - 22.0, mid - 11.0, bx, mid + 11.0);
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
            let del = Rect::new(bx - 22.0, mid - 11.0, bx, mid + 11.0);
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
        clippy::many_single_char_names,
        reason = "t is the paint transform, x/y/w are the body column's position/width, \
                  and c is the theme's colour set — conventional short names for this \
                  draw step"
    )]
    fn draw_detail_selection(
        &self,
        ctx: &mut PaintCtx<'_>,
        body: Rect,
        hits: &mut Vec<(Rect, Action)>,
    ) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        match self.selection {
            Some(Sel::Event(id)) => {
                if let Some(e) = self.model.event(id) {
                    let (scene, t, tr) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text);
                    let (x, y) = (body.x0, body.y0);
                    // Dot and kind label share the header row's centre line,
                    // whatever height the measured label gives that row.
                    let header_h = self.detail_header_h();
                    let mid = y + header_h / 2.0;
                    let label = "EVENT · SAVES AUTOMATICALLY";
                    let label_h = f64::from(tr.measure(label, SECTION_LABEL_TEXT, None).1);
                    scene.fill(
                        Fill::NonZero,
                        t,
                        e.color.color(theme),
                        None,
                        &Circle::new(Point::new(x + 6.0, mid), 6.0),
                    );
                    tr.draw_text(
                        scene,
                        label,
                        SECTION_LABEL_TEXT,
                        c.text_dim,
                        t * Affine::translate((x + 20.0, mid - label_h / 2.0)),
                        None,
                    );
                }
            }
            Some(Sel::Task(id)) => self.draw_detail_task(ctx, body, id, hits),
            None => {}
        }
    }

    /// The full task detail: checkbox + title, due/priority/project fields,
    /// and the subtask list (part of [`Self::draw_detail_selection`]).
    #[allow(
        clippy::many_single_char_names,
        reason = "t is the paint transform, x/y/w are the body column's position/width, \
                  and c is the theme's colour set — conventional short names for this \
                  draw step"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "the task card keeps its direct editable fields, subtasks, and hit targets co-located for responsive layout correctness"
    )]
    fn draw_detail_task(
        &self,
        ctx: &mut PaintCtx<'_>,
        body: Rect,
        id: Id,
        hits: &mut Vec<(Rect, Action)>,
    ) {
        let (scene, t, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let (x, mut y, w) = (body.x0, body.y0, body.width());
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
        let due = tk.due.map_or_else(
            || format!("Due · none — set {}", date::fmt_day_long(self.anchor)),
            |date| format!("Due · {}", date::fmt_day_long(date)),
        );
        let due_rect = Rect::new(x, y, x + w, y + 32.0);
        draw_field_button(
            scene,
            t,
            tr,
            theme,
            due_rect,
            &due,
            due_rect.contains(self.last_mouse),
            true,
        );
        hits.push((due_rect, Action::OpenTaskPopover(TaskPopover::Due)));
        y += 40.0;
        let priority = format!("Priority · {}", tk.priority.label());
        let priority_rect = Rect::new(x, y, x + w, y + 32.0);
        draw_field_button(
            scene,
            t,
            tr,
            theme,
            priority_rect,
            &priority,
            priority_rect.contains(self.last_mouse),
            true,
        );
        hits.push((
            priority_rect,
            Action::OpenTaskPopover(TaskPopover::Priority),
        ));
        y += 40.0;
        let project = tk
            .project
            .and_then(|pid| self.model.project(pid))
            .map_or_else(
                || "Project · none".to_string(),
                |project| format!("Project · {}", project.name),
            );
        let project_rect = Rect::new(x, y, x + w, y + 32.0);
        draw_field_button(
            scene,
            t,
            tr,
            theme,
            project_rect,
            &project,
            project_rect.contains(self.last_mouse),
            true,
        );
        hits.push((project_rect, Action::OpenTaskPopover(TaskPopover::Project)));
        y += 40.0;
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
        // Walk back up from the editor origin with the same terms
        // `notes_header_h` reserved: rule→editor gap, label→rule gap, label
        // line. The rule can then never cross a scaled label.
        let label_h = f64::from(tr.measure("NOTES", SECTION_LABEL_TEXT, None).1);
        let rule_y = r.y0 + notes_top - f64::from(theme.tokens.spacing.s2);
        let notes_label_y = rule_y - f64::from(theme.tokens.spacing.s1) - label_h;
        tr.draw_text(
            scene,
            "NOTES",
            SECTION_LABEL_TEXT,
            c.text_dim,
            t * Affine::translate((x, notes_label_y)),
            None,
        );
        hairline(
            scene,
            t,
            self.detail_ground(theme),
            Point::new(x, rule_y),
            Point::new(x + w, rule_y),
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
        let all_day_h = f64::from(tr.measure("All day", 14.0, None).1);
        tr.draw_text(
            scene,
            "All day",
            14.0,
            c.text_primary,
            t * Affine::translate((label_x, f.allday.center().y - all_day_h / 2.0)),
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

        self.draw_event_form_dates(&mut PaintCtx::new(scene, t, tr, theme), &f, &e, &mut hits);
        if f.conflict.area() > 0.0 {
            Self::draw_event_conflict_notice(
                &mut PaintCtx::new(scene, t, tr, theme),
                f.conflict,
                self.local_schedule_conflicts(&e).len(),
            );
        }

        // --- Location (real text input) ---------------------------------
        self.location_input.draw(scene, t, tr, theme, presence);
        hits.push((f.location, Action::FocusField(FormField::Location)));

        self.draw_event_form_calendar(&mut PaintCtx::new(scene, t, tr, theme), &f, &e, &mut hits);
        Self::draw_event_form_chips(scene, t, tr, theme, &f, &e, label_x, &mut hits);

        hits
    }

    /// The Starts / Ends date + time field-button rows (part of
    /// [`Self::draw_event_form`]).
    #[allow(
        clippy::many_single_char_names,
        reason = "t is the paint transform, f is the form layout, e is the event being \
                  edited, and c is the theme's colour set — conventional short names for \
                  this draw step"
    )]
    fn draw_event_form_dates(
        &self,
        ctx: &mut PaintCtx<'_>,
        f: &FormRects,
        e: &Event,
        hits: &mut Vec<(Rect, Action)>,
    ) {
        let (scene, t, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let (mouse, label_x) = (self.last_mouse, f.title.x0);
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
            let label_h = f64::from(tr.measure(label, 12.0, None).1);
            tr.draw_text(
                scene,
                label,
                12.0,
                c.text_dim,
                t * Affine::translate((label_x, date_rect.center().y - label_h / 2.0)),
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

    /// Compact, read-only feedback for an overlap discovered in the local
    /// model. The count is intentionally kept short enough to remain legible
    /// in the 200% form; the complete event names are exposed in the matching
    /// accessibility status node.
    fn draw_event_conflict_notice(paint: &mut PaintCtx<'_>, rect: Rect, conflict_count: usize) {
        let scene = &mut *paint.scene;
        let t = paint.transform;
        let tr = &mut *paint.text;
        let theme = paint.theme;
        let c = &theme.tokens.colors;
        let large_text = theme.text_scale >= LARGE_TEXT_SCALE;
        fill_round(scene, t, rect, 8.0, c.semantic.warning.with_alpha(0.14));
        stroke_round(
            scene,
            t,
            rect,
            8.0,
            c.semantic.warning.with_alpha(0.75),
            1.0,
        );
        scene.fill(
            Fill::NonZero,
            t,
            c.semantic.warning,
            None,
            &Circle::new(Point::new(rect.x0 + 15.0, rect.y0 + 15.0), 4.0),
        );
        tr.draw_text(
            scene,
            "Schedule overlap",
            11.5,
            c.semantic.warning,
            t * Affine::translate((
                rect.x0 + 27.0,
                rect.y0 + if large_text { 10.0 } else { 7.0 },
            )),
            None,
        );
        let label = format!(
            "{conflict_count} saved event{} overlaps this time",
            if conflict_count == 1 { "" } else { "s" }
        );
        tr.draw_text(
            scene,
            &label,
            10.0,
            c.text_primary,
            t * Affine::translate((
                rect.x0 + 12.0,
                if large_text {
                    rect.y0 + 43.0
                } else {
                    rect.y1 - 17.0
                },
            )),
            Some((rect.width() - 24.0).max(0.0) as f32),
        );
    }

    /// The calendar selector: colour dot + value + disclosure caret (part
    /// of [`Self::draw_event_form`]).
    #[allow(
        clippy::many_single_char_names,
        reason = "t is the paint transform, f is the form layout, e is the event being \
                  edited, and c is the theme's colour set — conventional short names for \
                  this draw step"
    )]
    fn draw_event_form_calendar(
        &self,
        ctx: &mut PaintCtx<'_>,
        f: &FormRects,
        e: &Event,
        hits: &mut Vec<(Rect, Action)>,
    ) {
        let (scene, t, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let mouse = self.last_mouse;
        let c = &theme.tokens.colors;
        let cal_active =
            f.calendar.contains(mouse) || self.event_popovers.active() == FormPopover::Calendar;
        {
            let r = f.calendar;
            let bg = if cal_active {
                c.lift(0.72)
            } else {
                c.surfaces.raised
            };
            let radius = f64::from(theme.tokens.radii.md);
            if cal_active {
                fill_round(scene, t, r, radius, bg);
                stroke_round(scene, t, r, radius, c.border_focus, 2.0);
            } else {
                card(scene, t, r, radius, bg);
            }
            scene.fill(
                Fill::NonZero,
                t,
                e.color.color(theme),
                None,
                &Circle::new(Point::new(r.x0 + 16.0, r.center().y), 5.0),
            );
            let label = self.calendar_label(e.calendar_id.as_ref());
            let disp = tr.elide_to_width(
                &label,
                14.0,
                ui::num::f32_of_f64((r.width() - 56.0).max(0.0)),
            );
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
        clippy::many_single_char_names,
        reason = "t/f/e and x/y are conventional paint, form, event, and flow geometry names"
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
        let mut recurrence_label = if e.recurrence == Recurrence::None {
            "No repeat".to_string()
        } else {
            e.recurrence.short().to_string()
        };
        if let Some(count) = e.recurrence_count {
            let _ = write!(recurrence_label, " · {count} times");
        } else if let Some(until) = e.recurrence_until {
            let _ = write!(recurrence_label, " · to {}", date::fmt_day_short(until));
        }
        let reminder_label = match e.reminder_min {
            None => "No reminder".to_string(),
            Some(m) if m >= 60 => format!("⏰ {}h before", m / 60),
            Some(m) => format!("⏰ {m}m before"),
        };
        let guest_label = match e.attendees.len() {
            0 => "No guests yet".to_string(),
            1 => "1 guest".to_string(),
            count => format!("{count} guests"),
        };
        let specs = [
            (format!("↻ {recurrence_label}"), Action::CycleRecurrence),
            (
                format!("Every {}×", e.recurrence_interval.max(1)),
                Action::CycleRecurrenceInterval,
            ),
            (reminder_label, Action::CycleReminder),
            (e.color.name().to_string(), Action::CycleColor),
            (format!("◷ {}", e.timezone), Action::CycleTimezone),
            (guest_label, Action::FocusField(FormField::Invite)),
        ];
        let max_x = f.title.x1;
        let height = event_form_chip_height(theme.text_scale);
        let gap = EVENT_FORM_CHIP_GAP * f64::from(theme.text_scale).max(1.0);
        let mut x = label_x;
        let mut y = f.chips_y;
        for (index, (label, action)) in specs.into_iter().enumerate() {
            let lead = if index == 3 {
                ChipLead::Dot(e.color.color(theme))
            } else {
                ChipLead::None
            };
            let width = Chip::natural_width_at(&label, height, lead, theme, tr)
                .min((max_x - label_x).max(1.0));
            if x > label_x && x + width > max_x {
                x = label_x;
                y += event_form_chip_row_pitch(theme.text_scale);
            }
            let rect = Rect::new(x, y, x + width, y + height);
            Chip::draw_at(scene, t, rect, &label, lead, ChipStyle::quiet(), theme, tr);
            x = rect.x1 + gap;
            hits.push((rect, action));
        }
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
            &mut PaintCtx::new(scene, t, tr, theme),
            presence,
            PopoverAnchors {
                start_date: self.form.start_date,
                end_date: self.form.end_date,
                start_time: self.form.start_time,
                end_time: self.form.end_time,
                calendar: self.form.calendar,
                panel: self.detail_rect,
            },
            self.last_mouse,
            CalendarChoices {
                options: &options,
                current,
            },
        );
        Vec::new()
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the three direct task pickers share one overlay and hit-map boundary so their selection actions cannot diverge"
    )]
    fn draw_task_popover(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) -> Vec<(Rect, Action)> {
        let Some(popover) = self.task_popover else {
            return Vec::new();
        };
        // This backing target sits below every option row, so pointer and
        // keyboard users can dismiss a picker without accidentally invoking a
        // hidden detail-card action.
        let mut hits = vec![(self.detail_rect, Action::DismissTaskPopover)];
        let popover_height = match popover {
            TaskPopover::Due => 256.0,
            TaskPopover::Priority => self.task_priority_dropdown.open_overlay_height() + 26.0,
            TaskPopover::Project => self.task_project_dropdown.open_overlay_height() + 26.0,
        };
        let r = Rect::new(
            self.detail_rect.x0 + 24.0,
            self.detail_rect.y0 + 176.0,
            self.detail_rect.x1 - 24.0,
            self.detail_rect.y0 + 176.0 + popover_height,
        );
        panel(scene, t, theme, r, f64::from(theme.tokens.radii.lg));
        match popover {
            TaskPopover::Due => {
                let picker_rect = Rect::new(r.x0 + 12.0, r.y0 + 10.0, r.x1 - 12.0, r.y1 - 46.0);
                self.task_due_picker.apply_layout_external(picker_rect);
                Widget::draw(
                    &mut self.task_due_picker,
                    scene,
                    t,
                    tr,
                    theme,
                    &PresenceCtx::empty(),
                );
                for (hit, rect) in self.task_due_picker.hit_regions_external() {
                    let action = match hit {
                        ExternalDatePickerHit::PreviousMonth => Action::ShiftTaskDueMonth(-1),
                        ExternalDatePickerHit::NextMonth => Action::ShiftTaskDueMonth(1),
                        ExternalDatePickerHit::Date(day) => Action::SetTaskDue(Some(day)),
                    };
                    hits.push((rect, action));
                }
                let clear = Rect::new(r.x0 + 12.0, r.y1 - 38.0, r.x1 - 12.0, r.y1 - 10.0);
                draw_field_button(scene, t, tr, theme, clear, "Clear due date", false, false);
                hits.push((clear, Action::SetTaskDue(None)));
            }
            TaskPopover::Priority => {
                let dropdown = Rect::new(r.x0 + 12.0, r.y0 + 14.0, r.x1 - 12.0, r.y0 + 54.0);
                self.task_priority_dropdown.apply_layout_external(dropdown);
                Widget::draw(
                    &mut self.task_priority_dropdown,
                    scene,
                    t,
                    tr,
                    theme,
                    &PresenceCtx::empty(),
                );
                Widget::draw_overlay(
                    &mut self.task_priority_dropdown,
                    scene,
                    t,
                    tr,
                    theme,
                    &PresenceCtx::empty(),
                );
            }
            TaskPopover::Project => {
                let dropdown = Rect::new(r.x0 + 12.0, r.y0 + 14.0, r.x1 - 12.0, r.y0 + 54.0);
                self.task_project_dropdown.apply_layout_external(dropdown);
                Widget::draw(
                    &mut self.task_project_dropdown,
                    scene,
                    t,
                    tr,
                    theme,
                    &PresenceCtx::empty(),
                );
                Widget::draw_overlay(
                    &mut self.task_project_dropdown,
                    scene,
                    t,
                    tr,
                    theme,
                    &PresenceCtx::empty(),
                );
            }
        }
        hits
    }

    fn draw_status(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        content_x0: f64,
        hits: &mut Vec<(Rect, Action)>,
    ) {
        if let Some(msg) = &self.status {
            let c = &theme.tokens.colors;
            let sp = &theme.tokens.spacing;
            let (inset, pad_x, gap) = (f64::from(sp.s4), f64::from(sp.s3), f64::from(sp.s2));
            let (wmeas, hmeas) = tr.measure(msg, 12.0, None);
            let (wmeas, hmeas) = (f64::from(wmeas), f64::from(hmeas));
            // The pill is the theme's compact selector height, or its measured
            // line plus the `s2` it keeps above and below once type scales.
            let pill_h = metrics::at_least_whole(
                f64::from(theme.tokens.metrics.selector_height),
                hmeas + 2.0 * gap,
            );
            let pill = Rect::new(
                content_x0 + inset,
                self.rect.y1 - inset - pill_h,
                content_x0 + inset + wmeas + 2.0 * pad_x,
                self.rect.y1 - inset,
            );
            card(
                scene,
                t,
                pill,
                f64::from(theme.tokens.radii.md),
                c.surfaces.raised,
            );
            tr.draw_text(
                scene,
                msg,
                12.0,
                c.text_primary,
                t * Affine::translate((pill.x0 + pad_x, pill.center().y - hmeas / 2.0)),
                None,
            );
            // An error must offer its recovery at the point where it is
            // explained. Reusing the canonical Sync action keeps pointer,
            // keyboard, screen-reader and resource-operation retry on one
            // execution path instead of presenting inert advice.
            if msg.starts_with("Sync failed") {
                let retry_w = f64::from(tr.measure("Retry", 12.0, None).0) + 2.0 * pad_x;
                let retry = Rect::new(pill.x1 + gap, pill.y0, pill.x1 + gap + retry_w, pill.y1);
                let hot = retry.contains(self.last_mouse);
                Button::paint(
                    scene,
                    t,
                    tr,
                    theme,
                    &ButtonFace::new(retry, "Retry")
                        .primary()
                        .text_size(12.0)
                        .hovered(hot),
                );
                hits.push((retry, Action::Sync));
            }
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
        let sp = &theme.tokens.spacing;
        let title_size = 20.0_f32;
        let hint_size = 14.0_f32;
        // Related title and hint sit one small gap apart (HIG §7a); the plate
        // around them takes the larger padding so the group reads as a unit.
        let gap = f64::from(sp.s2);
        let (title_w, title_h) = tr.measure(title, title_size, None);
        let (hint_w, hint_h) = tr.measure(hint, hint_size, None);
        let block_h = f64::from(title_h) + gap + f64::from(hint_h);
        let block_w = f64::from(title_w.max(hint_w));
        let cx = area.center().x;
        let top = area.center().y - block_h / 2.0;
        // The prompt floats over the day grid, so it needs its own surface:
        // without one the hint line lands on whichever date numbers happen to
        // sit in the middle row (visible at phone widths).
        let plate = Rect::new(
            cx - block_w / 2.0 - f64::from(sp.s6),
            top - f64::from(sp.s4),
            cx + block_w / 2.0 + f64::from(sp.s6),
            top + block_h + f64::from(sp.s4),
        );
        let radius = f64::from(theme.tokens.radii.lg);
        card(scene, t, plate, radius, c.surfaces.raised);
        tr.draw_text(
            scene,
            title,
            title_size,
            c.neutral[8],
            t * Affine::translate((cx - f64::from(title_w) / 2.0, top)),
            None,
        );
        tr.draw_text(
            scene,
            hint,
            hint_size,
            c.text_dim,
            t * Affine::translate((cx - f64::from(hint_w) / 2.0, top + f64::from(title_h) + gap)),
            None,
        );
    }

    /// The open popover owns input first: the picker itself, then (if it
    /// didn't consume the event) a left click resolved against the hit map
    /// — the dismiss scrim, a calendar row, or a popover guard (part of
    /// [`Widget::handle_event`]).
    #[allow(
        clippy::too_many_lines,
        reason = "internal event-handling split of an over-length function"
    )]
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
        if matches!(self.detail_mode, DetailMode::FullScreen)
            && matches!(self.narrow_screen, NarrowScreen::Detail)
        {
            self.discard_pristine_draft(state);
            self.narrow_screen = NarrowScreen::Calendar;
            self.notes_editor.set_focus(false);
            *handled = true;
            self.dirty = true;
            return true;
        }
        if self.detail_open {
            self.discard_pristine_draft(state);
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
        self.commit_task_project_dropdown(state, path);
        self.commit_task_priority_dropdown(state, path);
        if let Some(which) = self.shots_pending_popover.take() {
            self.open_popover(which);
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
        self.text_scale = theme.text_scale;
        self.spacing = theme.tokens.spacing;
        self.tokens_metrics = theme.tokens.metrics;
        self.timeline
            .set_layout_tokens(theme.text_scale, &theme.tokens.spacing);
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
            let content_height = taffy
                .layout(n)
                .map_or(0.0, |layout| f64::from(layout.size.height));
            let content_bottom = self.notes_origin.y + self.detail_scroll_y + content_height + 12.0;
            self.detail_scroll_max = if matches!(self.selection, Some(Sel::Event(_))) {
                (content_bottom - self.detail_rect.y1).max(0.0)
            } else {
                0.0
            };
            let next_scroll = if self.shots_detail_scroll_to_end {
                self.detail_scroll_max
            } else {
                self.detail_scroll_y.clamp(0.0, self.detail_scroll_max)
            };
            if (next_scroll - self.detail_scroll_y).abs() > f64::EPSILON {
                self.detail_scroll_y = next_scroll;
                self.relayout();
            }
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
        // Calendar is a custom canvas-heavy app, so its direct text renderer
        // calls need the same accessibility multiplier as toolkit widgets.
        text_renderer.set_text_scale(theme.text_scale);
        let c = &theme.tokens.colors;
        fill_rect(scene, t, self.rect, c.surfaces.base);

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
                            .draw_timeline(
                                &mut PaintCtx::new(scene, t, text_renderer, theme),
                                area,
                                &model,
                            )
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
                                &mut PaintCtx::new(scene, t, text_renderer, theme),
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
            // First-run onboarding: over an otherwise-blank Month/Week/Day view
            // with no events, show a friendly prompt instead of an empty grid.
            // Agenda carries its own inline empty-state.
            if !matches!(self.view, View::Agenda)
                && let Some((title, hint)) = self.empty_onboarding_prompt()
            {
                Self::draw_empty_onboarding(scene, t, text_renderer, theme, area, title, hint);
            }
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
                .draw_overlay(scene, t, text_renderer, theme, presence);
            // Transient date/time/calendar popovers paint on top of the form
            // and the notes, with a dismiss scrim beneath them.
            if editing_event {
                hits.extend(self.draw_form_popovers(scene, t, text_renderer, theme, presence));
            }
            if matches!(self.selection, Some(Sel::Task(_))) {
                hits.extend(self.draw_task_popover(scene, t, text_renderer, theme));
            }
        }

        // Transport feedback is painted after the optional editor card. A
        // sync can finish while someone is editing; leaving the message under
        // the modal would make an offline/conflict/error state invisible at
        // precisely the moment it needs acknowledgement and recovery.
        if self.status.is_some() {
            let status_x0 = if self.content_rect.area() > 0.0 {
                self.content_rect.x0
            } else {
                self.detail_rect.x0
            };
            self.draw_status(scene, t, text_renderer, theme, status_x0, &mut hits);
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

        if self.task_popover == Some(TaskPopover::Project) {
            Widget::handle_event(&mut self.task_project_dropdown, event, ctx);
            self.commit_task_project_dropdown(ctx.state, ctx.path);
            if *ctx.handled {
                self.dirty = true;
                return;
            }
        }

        if self.task_popover == Some(TaskPopover::Priority) {
            Widget::handle_event(&mut self.task_priority_dropdown, event, ctx);
            self.commit_task_priority_dropdown(ctx.state, ctx.path);
            if *ctx.handled {
                self.dirty = true;
                return;
            }
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
                    Some("Invitation prepared for Mail — review it there before sending".into());
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
                self.narrow_screen = NarrowScreen::Calendar;
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
                self.narrow_screen = NarrowScreen::Calendar;
                self.blur_editor();
                self.status = Some(if calendar.visible {
                    format!("Showing {}", calendar.label)
                } else {
                    format!(
                        "{} is hidden — enable it in the sidebar to show its events",
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
                self.narrow_screen = NarrowScreen::Calendar;
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
    // Centre on the measured line box, not the nominal glyph size: the
    // renderer scales the text it draws, so only the measurement it reports
    // tells us how tall the line really is at the current text scale.
    let (w, h) = tr.measure(text, size, None);
    let x = r.center().x - f64::from(w) / 2.0;
    let y = r.center().y - f64::from(h) / 2.0;
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
        c.lift(0.72)
    } else {
        c.surfaces.raised
    };
    card(scene, t, r, f64::from(theme.tokens.radii.md), bg);
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
    at: Point,
    n: usize,
    ground: Color,
) {
    let c = &theme.tokens.colors;
    let s = format!("{n}");
    let w = (f64::from(tr.measure(&s, 11.0, None).0) + 12.0).max(20.0);
    let r = Rect::new(at.x - w / 2.0, at.y - 9.0, at.x + w / 2.0, at.y + 9.0);
    // A pill on `ground` — the sidebar's raised pane, or the lifted row it
    // sits in: whichever tier moves furthest from that ground, so the badge
    // body registers on an idle row and on the selected one alike (a fixed
    // `base` fill vanished on a selected row whose lift is `base`).
    card(scene, t, r, r.height() / 2.0, selection_fill(theme, ground));
    center_text(scene, tr, &s, 11.0, c.text_primary, r, t);
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
        c.lift(0.72)
    } else {
        c.surfaces.raised
    };
    let radius = f64::from(theme.tokens.radii.md);
    if active {
        fill_round(scene, t, r, radius, bg);
        stroke_round(scene, t, r, radius, c.border_focus, 2.0);
    } else {
        card(scene, t, r, radius, bg);
    }
    let text_max = (r.width() - if trailing_caret { 40.0 } else { 24.0 }).max(0.0);
    let disp = tr.elide_to_width(label, 14.0, ui::num::f32_of_f64(text_max));
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

/// Height of a metadata chip for the current text scale: the theme's compact
/// selector height (28px at 1×), or the measured label line plus the `s3`
/// pad the painter leaves — rounded up — once scaled type needs more. Shared
/// by `form_layout` (no renderer; reads the cached theme tokens) and `chip`
/// (renderer) so the reserved row and the painted pill agree.
#[cfg(test)]
fn event_form_chip_h(selector_h: f64, pad_y: f64, text_scale: f32) -> f64 {
    metrics::at_least_whole(
        selector_h,
        metrics::line_h(EVENT_FORM_CHIP_TEXT, text_scale) + pad_y,
    )
}

/// The immediate-mode shared chip measures its label against the renderer's
/// scaled font. Keep this geometry in one helper so the form reserve, hit
/// targets, and painted border agree at every accessibility text size.
fn event_form_chip_height(text_scale: f32) -> f64 {
    EVENT_FORM_CHIP_H * f64::from(text_scale).max(1.0)
}

fn event_form_chip_row_pitch(text_scale: f32) -> f64 {
    event_form_chip_height(text_scale) + EVENT_FORM_CHIP_GAP * f64::from(text_scale).max(1.0)
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
#[path = "app/tests.rs"]
mod tests;
