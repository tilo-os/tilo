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
//! - `sync`   — seed the CRDT on first frame, bind the notes editor to
//!              the selected item's block list, run the editor's sync.
//! - `build_layout` — register the editor node, return a full-bleed root.
//! - `apply_layout` — compute region rects, place the editor.
//! - `draw`   — paint sidebar + toolbar + the active view + detail panel,
//!              rebuilding the click hit-map.
//! - `handle_event` — route clicks against the hit-map, keyboard
//!              shortcuts, and forward to the editor when notes are active.

use std::collections::HashMap;
use std::path::PathBuf;

use automerge::{AutoCommit, ChangeHash, ObjId};
use chrono::{Datelike, Duration, Local, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use taffy::style_helpers::length;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use uuid::Uuid;
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::Widget;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::BlockEditor;

use crate::account::AccountStore;
use crate::caldav::{self, SyncOutcome};
use crate::date;
use crate::doc::CalDoc;
use crate::ics;
use crate::model::{seed_demo, Event, Id, ListSel, Model, Palette, Priority, Recurrence, View};

const SIDEBAR_W: f64 = 252.0;
const TOPBAR_H: f64 = 60.0;
const DETAIL_PAD: f64 = 22.0;
const NOTES_TOP_OFFSET: f64 = 296.0;
/// First/last hours shown in the week/day timelines.
const DAY_START: i64 = 7;
const DAY_END: i64 = 22;

/// Width at/above which the calendar shows its full side-by-side layout
/// (persistent sidebar + calendar view + detail panel). Below it the app
/// collapses to a narrow single-pane: the sidebar becomes an overlay flap and
/// the detail is pushed over the calendar as its own screen. Driven purely by
/// the allocated window width (see [`CalendarApp::relayout`]).
const WIDE_BREAKPOINT: f64 = 980.0;
/// Detail panel width when it sits beside the calendar (wide layout). In the
/// narrow layout the detail screen takes the whole content width instead.
const DETAIL_W_WIDE: f64 = 392.0;
/// Notes editor column width. Fixed (rather than tracking the detail width) so
/// the taffy-registered editor node has a stable size across frames; the
/// narrow detail screen simply left-aligns the same column.
const NOTES_W: f64 = DETAIL_W_WIDE - 2.0 * DETAIL_PAD;

/// How the calendars/smart-lists sidebar is presented at the current width.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SidebarMode {
    /// Inline, always visible, occupies the left edge (wide windows).
    Persistent,
    /// Floats over the content behind a scrim; toggled open, hidden by default
    /// (narrow windows).
    Overlay,
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
    /// Step the visible month by ±1 from the mini-month navigator.
    ShiftMonth(i32),
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
    /// Pop the narrow detail screen, returning to the calendar.
    BackToCalendar,
    /// Toggle whether a synced remote calendar's events are shown.
    ToggleCalendarVisible(String),
    /// Pull events from the configured CalDAV calendars.
    Sync,
}

pub struct CalendarApp {
    model: Model,
    today: NaiveDate,
    anchor: NaiveDate,
    view: View,
    list_sel: ListSel,
    selection: Option<Sel>,
    detail_open: bool,

    /// CalDAV accounts + their calendars, as configured by OS Settings (the
    /// shared [`AccountStore`]). Read-only here; Settings owns the lifecycle.
    accounts: AccountStore,

    notes_editor: BlockEditor,
    bound: Option<Sel>,
    seeded: bool,

    /// True for the hermetic `--shots` / test build: it seeds sample content
    /// and drives the offline (synthesised) CalDAV path. The live app leaves
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
    /// Recomputed every layout from the allocated width.
    sidebar_mode: SidebarMode,
    /// Whether the overlay flap is open (only meaningful in `Overlay` mode).
    sidebar_open: bool,
    /// Which narrow screen is showing (only meaningful below the breakpoint).
    narrow_screen: NarrowScreen,

    // --- layout caches (filled by `relayout`) ----------------------------
    rect: Rect,
    sidebar_rect: Rect,
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

    /// Receiver for an in-flight background CalDAV pull. The live sync runs on a
    /// worker thread (the `ui` event loop is synchronous and must never block on
    /// the network); its result is drained here on the next `sync` pass.
    sync_rx: Option<std::sync::mpsc::Receiver<(caldav::SyncResult, SyncOutcome)>>,
    /// True while a background sync is running, which keeps the event loop
    /// ticking (`needs_animation_frame`) so the worker's result lands promptly
    /// without waiting for the next unrelated input.
    syncing: bool,
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
            notes_editor: BlockEditor::new(),
            bound: None,
            seeded: false,
            demo,
            data_path,
            saved_heads: Vec::new(),
            force_persist: false,
            last_persist: std::time::Instant::now(),
            sidebar_mode: SidebarMode::Persistent,
            sidebar_open: false,
            narrow_screen: if detail_open {
                NarrowScreen::Detail
            } else {
                NarrowScreen::Calendar
            },
            rect: Rect::ZERO,
            sidebar_rect: Rect::ZERO,
            content_rect: Rect::ZERO,
            detail_rect: Rect::ZERO,
            notes_origin: Point::new(-10000.0, -10000.0),
            notes_w: DETAIL_W_WIDE - 2.0 * DETAIL_PAD,
            editor_node: None,
            built_notes_w: -1.0,
            hits: Vec::new(),
            last_mouse: Point::ZERO,
            status: None,
            dirty: true,
            sync_rx: None,
            syncing: false,
        }
    }

    // --- shots-only configuration (no effect on live runtime behaviour) --

    /// Force a calendar view for a screenshot.
    pub fn shots_set_view(&mut self, v: View) {
        self.view = v;
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
        self.sidebar_open = true;
        self.dirty = true;
    }

    /// Kick off a CalDAV pull from the configured calendars. The live pull
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

        // Live pull on a background thread; the result lands via `poll_sync`.
        let (tx, rx) = std::sync::mpsc::channel();
        let today = self.today;
        std::thread::spawn(move || {
            let _ = tx.send(caldav::sync_all(&inputs, today));
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
            Ok((result, outcome)) => {
                self.sync_rx = None;
                self.syncing = false;
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

    fn notes_list_for(&self, doc: &mut AutoCommit, sel: Sel) -> Option<ObjId> {
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
                    "Review the latest mockups.",
                    "Open questions:",
                    "Empty states for new users?",
                    "Tighten dense-list spacing.",
                ],
            ),
            (
                "Team standup",
                &[
                    "# Standup",
                    "Wrapped up the import flow.",
                    "Reviewing open PRs today.",
                    "Blockers: none.",
                ],
            ),
        ];
        for (needle, lines) in curated {
            if let Some(e) = self.model.events.iter().find(|e| e.title.starts_with(needle)) {
                if let Some(list) = CalDoc::event_notes(doc, e.id) {
                    CalDoc::seed_notes_if_empty(doc, &list, lines);
                }
            }
        }
    }

    fn ensure_bound(&mut self, doc: &mut AutoCommit) {
        let sel = match self.selection {
            Some(s) if self.detail_open => s,
            _ => {
                if self.bound.is_some() {
                    self.notes_editor = BlockEditor::new();
                    self.bound = None;
                }
                return;
            }
        };
        if self.bound == Some(sel) {
            return;
        }
        if let Some(list) = self.notes_list_for(doc, sel) {
            let mut ed = BlockEditor::new();
            ed.bind_blocks(doc, list);
            // Install the freshly-bound editor through the single notes-editor
            // seam (the swap-point for the shared WYSIWYG editor).
            *self.notes_editor() = ed;
            self.bound = Some(sel);
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

    // --- mutations -------------------------------------------------------

    fn select_event(&mut self, id: Id) {
        self.selection = Some(Sel::Event(id));
        self.open_detail();
    }

    fn select_task(&mut self, id: Id) {
        self.selection = Some(Sel::Task(id));
        self.open_detail();
    }

    /// Open the detail panel for the current selection. In the narrow layout
    /// this pushes the detail screen over the calendar and closes the flap; in
    /// the wide layout the panel just appears beside the calendar.
    fn open_detail(&mut self) {
        self.detail_open = true;
        if matches!(self.sidebar_mode, SidebarMode::Overlay) {
            self.narrow_screen = NarrowScreen::Detail;
            self.sidebar_open = false;
        }
        self.dirty = true;
    }

    fn toggle_task(&mut self, id: Id, doc: &mut AutoCommit) {
        if let Some(t) = self.model.task_mut(id) {
            t.done = !t.done;
            let snap = t.clone();
            CalDoc::update_task(doc, &snap);
            self.dirty = true;
        }
    }

    fn new_event(&mut self, doc: &mut AutoCommit) {
        let start = self
            .anchor
            .and_time(NaiveTime::from_hms_opt(12, 0, 0).unwrap());
        let ev = Event {
            id: Uuid::new_v4(),
            title: "New event".into(),
            start,
            end: start + Duration::hours(1),
            all_day: false,
            location: String::new(),
            color: self.model.next_event_order_color(),
            recurrence: Recurrence::None,
            reminder_min: Some(10),
            project: None,
        };
        let id = ev.id;
        CalDoc::update_event(doc, &ev);
        if let Some(list) = CalDoc::event_notes(doc, id) {
            CalDoc::seed_notes_if_empty(doc, &list, &["Add details…"]);
        }
        self.model.events.push(ev);
        self.model.events.sort_by_key(|e| e.start);
        self.select_event(id);
        self.status = Some("New event created".into());
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
            Ok(_) => {
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
            if let Some(e) = self.model.event_mut(id) {
                f(e);
                let snap = e.clone();
                CalDoc::update_event(doc, &snap);
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
            View::Week => self.anchor + Duration::days(7 * dir as i64),
            View::Day => self.anchor + Duration::days(dir as i64),
            View::Agenda => self.anchor + Duration::days(7 * dir as i64),
        };
        self.dirty = true;
    }

    /// Recompute every layout rect from the allocated window size. Called from
    /// `apply_layout`. This is the one place breakpoints are decided:
    ///
    /// * **wide** (`width >= WIDE_BREAKPOINT`): persistent sidebar on the left,
    ///   calendar view in the middle, detail panel docked on the right.
    /// * **narrow**: the sidebar becomes an overlay flap (hidden by default),
    ///   the calendar view goes full-width, and the detail is its own screen
    ///   pushed over the calendar — only one of {calendar, detail} is laid out
    ///   at a time (the other gets a zero rect and is neither drawn nor hit).
    fn relayout(&mut self) {
        let r = self.rect;
        let wide = r.width() >= WIDE_BREAKPOINT;
        self.sidebar_mode = if wide { SidebarMode::Persistent } else { SidebarMode::Overlay };

        // Sidebar occupies the left edge in both modes; in overlay it floats
        // over the content (so the content starts at the window's left edge).
        self.sidebar_rect = Rect::new(r.x0, r.y0, r.x0 + SIDEBAR_W, r.y1);
        let content_x0 = match self.sidebar_mode {
            SidebarMode::Persistent => r.x0 + SIDEBAR_W,
            SidebarMode::Overlay => r.x0,
        };

        if wide {
            // Detail docked on the right when open; calendar fills the rest.
            let detail_x0 = if self.detail_open { r.x1 - DETAIL_W_WIDE } else { r.x1 };
            self.content_rect = Rect::new(content_x0, r.y0, detail_x0, r.y1);
            self.detail_rect = if self.detail_open {
                Rect::new(detail_x0, r.y0, r.x1, r.y1)
            } else {
                Rect::ZERO
            };
        } else {
            // Single-pane: calendar OR detail occupies the full content width.
            let full = Rect::new(content_x0, r.y0, r.x1, r.y1);
            match (self.detail_open, self.narrow_screen) {
                (true, NarrowScreen::Detail) => {
                    self.content_rect = Rect::ZERO;
                    self.detail_rect = full;
                }
                _ => {
                    self.content_rect = full;
                    self.detail_rect = Rect::ZERO;
                }
            }
        }

        // Notes editor placement, derived from whichever detail rect is active.
        // The column width tracks the detail rect (capped for readability) so
        // the narrow single-pane screen fills its width instead of leaving a
        // fixed-width column stranded against a wide empty margin.
        self.notes_w = self.detail_body_w();
        if self.detail_rect.area() > 0.0 {
            self.notes_origin = Point::new(
                self.detail_rect.x0 + DETAIL_PAD,
                self.detail_rect.y0 + self.notes_top_offset(),
            );
        } else {
            self.notes_origin = Point::new(-10000.0, -10000.0);
        }
    }

    /// Width of the detail body column (fields + notes editor). Fixed in the
    /// wide docked panel; in the narrow single-pane screen it grows with the
    /// window (capped for comfortable line length) so the detail fills the
    /// pane instead of hugging the left edge. Derived from the allocated
    /// window width so `build_layout` and `relayout` agree.
    fn detail_body_w(&self) -> f64 {
        if self.rect.width() >= WIDE_BREAKPOINT {
            NOTES_W
        } else {
            (self.rect.width() - 2.0 * DETAIL_PAD).clamp(240.0, 620.0)
        }
    }

    /// Whether the overlay flap is currently shown (always true for a
    /// persistent sidebar).
    fn sidebar_visible(&self) -> bool {
        match self.sidebar_mode {
            SidebarMode::Persistent => true,
            SidebarMode::Overlay => self.sidebar_open,
        }
    }

    /// Y offset (from the detail rect top) of the NOTES section + editor. The
    /// narrow detail screen carries a back bar above the body, so its notes
    /// start lower. Single source of truth for both `relayout` (editor origin)
    /// and `draw_detail` (the NOTES label/divider).
    fn notes_top_offset(&self) -> f64 {
        if matches!(self.sidebar_mode, SidebarMode::Overlay) {
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
                if matches!(self.sidebar_mode, SidebarMode::Overlay) {
                    self.narrow_screen = NarrowScreen::Calendar;
                    self.sidebar_open = false;
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
                if matches!(self.sidebar_mode, SidebarMode::Overlay) {
                    self.narrow_screen = NarrowScreen::Calendar;
                    self.sidebar_open = false;
                }
            }
            Action::CloseDetail => {
                self.detail_open = false;
                self.selection = None;
                self.narrow_screen = NarrowScreen::Calendar;
            }
            Action::ToggleSidebar => self.sidebar_open = !self.sidebar_open,
            Action::DismissSidebar => self.sidebar_open = false,
            Action::BackToCalendar => self.narrow_screen = NarrowScreen::Calendar,
            Action::ToggleCalendarVisible(id) => {
                for acc in &mut self.accounts.accounts {
                    if let Some(cal) = acc.calendars.iter_mut().find(|c| c.id == id) {
                        cal.visible = !cal.visible;
                    }
                }
            }
            Action::Sync => self.start_sync(doc),
            Action::Export => self.export_ics(),
            Action::Import => self.import_ics(doc),
            Action::ShiftMonth(n) => self.anchor = date::add_months(self.anchor, n),
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
        }
        // Discrete actions are worth an immediate save; if the action didn't
        // touch the document (view/navigation) the head check in `maybe_persist`
        // makes this a no-op.
        self.force_persist = true;
        self.dirty = true;
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
        let c = &theme.tokens.colors;
        let mut hits = Vec::new();
        if !self.sidebar_visible() {
            return hits;
        }
        let overlay = matches!(self.sidebar_mode, SidebarMode::Overlay);
        // The overlay flap dims the content behind a scrim; clicking it
        // dismisses the flap. Drawn before the panel so the panel sits on top.
        if overlay && self.content_rect.area() > 0.0 || (overlay && self.detail_rect.area() > 0.0) {
            let behind = if self.content_rect.area() > 0.0 {
                self.content_rect
            } else {
                self.detail_rect
            };
            fill_rect(scene, t, c.surfaces.overlay, behind);
            hits.push((behind, Action::DismissSidebar));
        }

        let r = self.sidebar_rect;
        fill_rect(scene, t, c.surfaces.raised, r);
        if overlay {
            // Drop shadow along the flap's right edge.
            scene.fill(
                Fill::NonZero,
                t,
                Color::from_rgba8(0, 0, 0, 70),
                None,
                &Rect::new(r.x1, r.y0, r.x1 + 8.0, r.y1),
            );
        }
        stroke_line(
            scene,
            t,
            c.border_standard,
            Point::new(r.x1, r.y0),
            Point::new(r.x1, r.y1),
            1.0,
        );

        let pad = 18.0;
        let mut y = r.y0 + 22.0;
        // App wordmark.
        tr.draw_text(
            scene,
            "Calendar",
            20.0,
            c.text_primary,
            t * Affine::translate((r.x0 + pad, y)),
            None,
        );
        // small accent dot
        scene.fill(
            Fill::NonZero,
            t,
            Palette::Indigo.color(theme),
            None,
            &Circle::new(Point::new(r.x0 + pad + 100.0, y + 9.0), 4.0),
        );
        y += 40.0;

        // View switcher — carried by the flap only in the overlay layout. The
        // wide topbar has its own segmented switch, but on narrow layouts that
        // switch folds away, so without this a pointer/touch user could reach
        // Day/Agenda but never get back to Month. Keeps every view reachable.
        if overlay {
            let seg = Rect::new(r.x0 + 12.0, y, r.x1 - 12.0, y + 36.0);
            fill_round(scene, t, c.surfaces.base, seg, 9.0);
            stroke_round(scene, t, c.border_standard, seg, 9.0, 1.0);
            let cell_w = seg.width() / View::ALL.len() as f64;
            for (i, v) in View::ALL.iter().enumerate() {
                let cell = Rect::new(
                    seg.x0 + cell_w * i as f64,
                    seg.y0,
                    seg.x0 + cell_w * (i as f64 + 1.0),
                    seg.y1,
                );
                if self.view == *v {
                    fill_round(scene, t, c.accent[5], cell.inflate(-3.0, -3.0), 7.0);
                } else if cell.contains(self.last_mouse) {
                    fill_round(scene, t, c.surface_white_5, cell.inflate(-3.0, -3.0), 7.0);
                }
                let col = if self.view == *v { Color::WHITE } else { c.neutral[7] };
                center_text(scene, tr, v.label(), 13.0, col, cell, t);
                hits.push((cell, Action::View(*v)));
            }
            y += 50.0;
        }

        // Export / Import .ics buttons (also bound to `e` / `i`).
        let bw = (r.width() - 24.0 - 8.0) / 2.0;
        let exp = Rect::new(r.x0 + 12.0, y, r.x0 + 12.0 + bw, y + 32.0);
        draw_icon_button(scene, t, theme, exp, exp.contains(self.last_mouse));
        draw_download(scene, t, c.neutral[8], Point::new(exp.x0 + 18.0, exp.center().y));
        tr.draw_text(
            scene,
            "Export",
            13.0,
            c.neutral[8],
            t * Affine::translate((exp.x0 + 32.0, exp.center().y - 8.0)),
            None,
        );
        hits.push((exp, Action::Export));

        let imp = Rect::new(exp.x1 + 8.0, y, r.x1 - 12.0, y + 32.0);
        draw_icon_button(scene, t, theme, imp, imp.contains(self.last_mouse));
        draw_upload(scene, t, c.neutral[8], Point::new(imp.x0 + 18.0, imp.center().y));
        tr.draw_text(
            scene,
            "Import",
            13.0,
            c.neutral[8],
            t * Affine::translate((imp.x0 + 32.0, imp.center().y - 8.0)),
            None,
        );
        hits.push((imp, Action::Import));
        y += 46.0;

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
                Rect::new(row.x0 + 12.0, row.center().y - 5.0, row.x0 + 22.0, row.center().y + 5.0),
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

        // CalDAV calendars from the shared OS accounts store. Each account's
        // calendars are listed with a visibility checkbox; a Sync affordance in
        // the heading pulls events from every visible calendar.
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
                // Account header (label only — management lives in Settings).
                tr.draw_text(
                    scene,
                    &elide(acc.label(), 26),
                    12.0,
                    c.neutral[7],
                    t * Affine::translate((r.x0 + pad, y)),
                    None,
                );
                y += 22.0;
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
                    let txt_color = if cal.visible { c.neutral[8] } else { c.text_dim };
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

        // Reserve the bottom of the sidebar for the mini-month navigator.
        let mini_top = r.y1 - 196.0;
        let rail_tasks = self.model.tasks_for(&self.list_sel, self.today);
        // Mini upcoming-task rail — only when there are tasks to show AND
        // there's vertical room left above the mini-month (the CALENDARS
        // section above may have used it up; the agenda/day views still list
        // the tasks in full either way).
        if !rail_tasks.is_empty() && y + 56.0 < mini_top {
            let list_title = match &self.list_sel {
                ListSel::Inbox => "Inbox",
                ListSel::Today => "Today",
                ListSel::Upcoming => "Upcoming",
                ListSel::Completed => "Completed",
                ListSel::Project(_) => "Tasks",
            };
            y += 10.0;
            stroke_line(scene, t, c.border_standard, Point::new(r.x0 + pad, y), Point::new(r.x1 - pad, y), 1.0);
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

            let box_rect = Rect::new(row.x0 + 4.0, row.center().y - 8.0, row.x0 + 20.0, row.center().y + 8.0);
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
            let label_w = tr.measure(&label, 13.0, None).0 as f64;
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

        // Mini-month navigator pinned to the sidebar bottom — drawn only when
        // the sidebar content above it (lists + projects + CALENDARS) hasn't
        // already grown into its reserved band, so the two never overlap.
        if y < mini_top - 8.0 {
            stroke_line(
                scene,
                t,
                c.border_standard,
                Point::new(r.x0 + pad, mini_top - 8.0),
                Point::new(r.x1 - pad, mini_top - 8.0),
                1.0,
            );
            let mini_region = Rect::new(r.x0, mini_top, r.x1, r.y1 - 12.0);
            hits.extend(self.draw_mini_month(scene, t, tr, theme, mini_region));
        }

        hits
    }

    /// Compact month navigator drawn in the sidebar: a 6×7 day grid with
    /// today highlighted, the anchor ringed, a dot under days that have
    /// events, and prev/next month chevrons. Clicking a day re-anchors the
    /// active view; the chevrons step the visible month.
    fn draw_mini_month(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        region: Rect,
    ) -> Vec<(Rect, Action)> {
        let c = &theme.tokens.colors;
        let mut hits = Vec::new();
        let pad = 18.0;

        tr.draw_text(
            scene,
            &date::month_title(self.anchor),
            13.0,
            c.text_primary,
            t * Affine::translate((region.x0 + pad, region.y0)),
            None,
        );
        let pc = Rect::new(region.x1 - pad - 46.0, region.y0 - 4.0, region.x1 - pad - 24.0, region.y0 + 18.0);
        let nc = Rect::new(region.x1 - pad - 22.0, region.y0 - 4.0, region.x1 - pad, region.y0 + 18.0);
        if pc.contains(self.last_mouse) {
            fill_round(scene, t, c.surface_white_5, pc, 6.0);
        }
        if nc.contains(self.last_mouse) {
            fill_round(scene, t, c.surface_white_5, nc, 6.0);
        }
        draw_chevron(scene, t, c.neutral[7], pc.center(), true);
        draw_chevron(scene, t, c.neutral[7], nc.center(), false);
        hits.push((pc, Action::ShiftMonth(-1)));
        hits.push((nc, Action::ShiftMonth(1)));

        let grid_x0 = region.x0 + pad;
        let grid_w = region.width() - 2.0 * pad;
        let cw = grid_w / 7.0;
        let head_y = region.y0 + 24.0;
        for (i, lbl) in date::WEEKDAY_NARROW.iter().enumerate() {
            center_text(
                scene,
                tr,
                lbl,
                10.0,
                c.text_dim,
                Rect::new(grid_x0 + cw * i as f64, head_y, grid_x0 + cw * (i as f64 + 1.0), head_y + 14.0),
                t,
            );
        }

        let grid = date::month_grid(self.anchor);
        let cell_h = 22.0;
        let top = head_y + 18.0;
        for (idx, day) in grid.iter().enumerate() {
            let row = (idx / 7) as f64;
            let col = (idx % 7) as f64;
            let cell = Rect::new(
                grid_x0 + cw * col,
                top + cell_h * row,
                grid_x0 + cw * (col + 1.0),
                top + cell_h * (row + 1.0),
            );
            let in_month = day.month() == self.anchor.month();
            let is_today = *day == self.today;
            let is_anchor = *day == self.anchor;
            let has_ev = self.model.events.iter().any(|e| e.occurs_on(*day));

            if is_today {
                scene.fill(Fill::NonZero, t, c.accent[5], None, &Circle::new(cell.center(), 10.0));
            } else if is_anchor {
                stroke_round(scene, t, c.accent[5], cell.inflate(-3.0, -2.0), 7.0, 1.2);
            } else if cell.contains(self.last_mouse) {
                fill_round(scene, t, c.surface_white_5, cell.inflate(-3.0, -2.0), 7.0);
            }
            let num_col = if is_today {
                Color::WHITE
            } else if !in_month {
                c.neutral[4]
            } else {
                c.neutral[8]
            };
            center_text(scene, tr, &format!("{}", day.day()), 11.0, num_col, cell, t);
            if has_ev && !is_today {
                let dot = Point::new(cell.center().x, cell.y1 - 2.5);
                scene.fill(Fill::NonZero, t, c.accent[5], None, &Circle::new(dot, 1.6));
            }
            hits.push((cell, Action::PickDate(*day)));
        }

        hits
    }

    /// Candidate period titles for the topbar, widest first. The topbar draws
    /// the widest one that fits the space between the Today pill and the New
    /// button, so the full range still reads when there's room and degrades to
    /// a compact form (still showing both endpoints) when there isn't — rather
    /// than clipping the end date off a single fixed string.
    fn period_title_variants(&self) -> Vec<String> {
        match self.view {
            View::Month => vec![date::month_title(self.anchor)],
            View::Week => {
                let w = date::week_days(self.anchor);
                vec![
                    date::week_range_long(w[0], w[6]),
                    date::week_range_short(w[0], w[6]),
                ]
            }
            View::Day => vec![date::fmt_day_long(self.anchor)],
            View::Agenda => vec!["Upcoming".into()],
        }
    }

    fn draw_topbar(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) -> Vec<(Rect, Action)> {
        let c = &theme.tokens.colors;
        let mut hits = Vec::new();
        let content_x0 = self.content_rect.x0;
        let right_edge = self.content_rect.x1;
        let r = Rect::new(content_x0, self.rect.y0, right_edge, self.rect.y0 + TOPBAR_H);
        fill_rect(scene, t, c.surfaces.base, r);
        stroke_line(scene, t, c.border_standard, Point::new(r.x0, r.y1), Point::new(r.x1, r.y1), 1.0);

        let cy = r.center().y;
        let overlay_sidebar = matches!(self.sidebar_mode, SidebarMode::Overlay);
        // Narrow layouts cannot fit the segmented view switch; it folds away
        // (the view is still switchable by keyboard m/w/d/a).
        let show_segmented = !overlay_sidebar;

        let mut left = r.x0 + 16.0;
        // Hamburger toggle for the overlay flap (narrow only).
        if overlay_sidebar {
            let ham = Rect::new(left, cy - 15.0, left + 30.0, cy + 15.0);
            draw_icon_button(scene, t, theme, ham, ham.contains(self.last_mouse));
            draw_hamburger(scene, t, c.neutral[8], ham.center());
            hits.push((ham, Action::ToggleSidebar));
            left += 38.0;
        }

        // prev / next chevrons
        let prev = Rect::new(left, cy - 15.0, left + 30.0, cy + 15.0);
        let next = Rect::new(left + 34.0, cy - 15.0, left + 64.0, cy + 15.0);
        draw_icon_button(scene, t, theme, prev, prev.contains(self.last_mouse));
        draw_icon_button(scene, t, theme, next, next.contains(self.last_mouse));
        draw_chevron(scene, t, c.neutral[8], prev.center(), true);
        draw_chevron(scene, t, c.neutral[8], next.center(), false);
        hits.push((prev, Action::Prev));
        hits.push((next, Action::Next));
        left += 74.0;

        // Today pill
        let today_btn = Rect::new(left, cy - 15.0, left + 60.0, cy + 15.0);
        draw_icon_button(scene, t, theme, today_btn, today_btn.contains(self.last_mouse));
        center_text(scene, tr, "Today", 13.0, c.neutral[8], today_btn, t);
        hits.push((today_btn, Action::Today));
        let title_x = today_btn.x1 + 14.0;

        // Right cluster: segmented view control (wide only) + New button.
        let new_w = 60.0;
        let seg_x0 = if show_segmented {
            let seg_w = 56.0;
            let seg_total = seg_w * 4.0;
            let seg_x1 = right_edge - 14.0;
            let seg_x0 = seg_x1 - seg_total;
            let seg = Rect::new(seg_x0, cy - 16.0, seg_x1, cy + 16.0);
            fill_round(scene, t, c.surfaces.raised, seg, 9.0);
            stroke_round(scene, t, c.border_standard, seg, 9.0, 1.0);
            for (i, v) in View::ALL.iter().enumerate() {
                let cell = Rect::new(seg_x0 + seg_w * i as f64, seg.y0, seg_x0 + seg_w * (i as f64 + 1.0), seg.y1);
                if self.view == *v {
                    fill_round(scene, t, c.accent[5], cell.inflate(-3.0, -3.0), 7.0);
                } else if cell.contains(self.last_mouse) {
                    fill_round(scene, t, c.surface_white_5, cell.inflate(-3.0, -3.0), 7.0);
                }
                let col = if self.view == *v { Color::WHITE } else { c.neutral[7] };
                center_text(scene, tr, v.label(), 13.0, col, cell, t);
                hits.push((cell, Action::View(*v)));
            }
            seg_x0
        } else {
            right_edge - 14.0
        };

        let new_btn = Rect::new(seg_x0 - new_w - 10.0, cy - 16.0, seg_x0 - 10.0, cy + 16.0);
        fill_round(scene, t, c.accent[5], new_btn, 9.0);
        draw_plus(scene, t, Color::WHITE, Point::new(new_btn.x0 + 16.0, cy));
        tr.draw_text(
            scene,
            "New",
            13.0,
            Color::WHITE,
            t * Affine::translate((new_btn.x0 + 28.0, cy - 8.0)),
            None,
        );
        hits.push((new_btn, Action::NewEvent));

        // Period title — fills the gap between the Today pill and the New
        // button. Draw the widest variant that fits so the full range reads
        // whenever there's room (dropping to a compact both-endpoints form
        // when there isn't); the clip layer is only a last-resort safety net.
        let title_right = new_btn.x0 - 12.0;
        let avail = (title_right - title_x).max(0.0);
        let variants = self.period_title_variants();
        let title = variants
            .iter()
            .find(|v| tr.measure(v, 19.0, None).0 as f64 <= avail)
            .unwrap_or_else(|| variants.last().expect("at least one title variant"));
        let title_clip = Rect::new(title_x - 2.0, r.y0, title_right, r.y1);
        scene.push_layer(Fill::NonZero, vello::peniko::BlendMode::default(), 1.0, t, &title_clip);
        tr.draw_text(
            scene,
            title,
            19.0,
            c.text_primary,
            t * Affine::translate((title_x, cy - 11.0)),
            None,
        );
        scene.pop_layer();

        hits
    }

    fn view_rect(&self) -> Rect {
        Rect::new(
            self.content_rect.x0,
            self.rect.y0 + TOPBAR_H,
            self.content_rect.x1,
            self.rect.y1,
        )
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
        let cell_h = (area.height() - header_h) / rows as f64;

        // Weekday header.
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
            // grid lines
            stroke_round(scene, t, c.border_standard.with_alpha(0.5), cell, 0.0, 0.5);

            // Cell-level click targets are pushed FIRST so the event chips
            // drawn below win the reverse (topmost-last) hit-scan — clicking a
            // chip must open that event, not just re-anchor the day. The
            // date-number strip in turn wins over the empty cell body.
            hits.push((cell, Action::PickDate(*day)));
            hits.push((
                Rect::new(cell.x0, cell.y0, cell.x1, cell.y0 + 28.0),
                Action::OpenDay(*day),
            ));

            let in_month = day.month() == self.anchor.month();
            let is_today = *day == self.today;
            let is_anchor = *day == self.anchor;
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

            // Event chips (clip to cell).
            scene.push_layer(Fill::NonZero, vello::peniko::BlendMode::default(), 1.0, t, &cell);
            let evs: Vec<(Id, String, Palette, bool, NaiveDateTime)> = self
                .model
                .events_on(*day)
                .iter()
                .map(|e| (e.id, e.title.clone(), e.color, e.all_day, e.start))
                .collect();
            let max_chips = ((cell_h - 30.0) / 20.0).floor().max(0.0) as usize;
            let mut cy = cell.y0 + 30.0;
            for (i, (id, title, color, all_day, start)) in evs.iter().enumerate() {
                if i >= max_chips.max(1).min(3) {
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
        hits
    }

    fn draw_timeline(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        days: &[NaiveDate],
    ) -> Vec<(Rect, Action)> {
        let c = &theme.tokens.colors;
        let mut hits = Vec::new();
        fill_rect(scene, t, c.surfaces.base, area);

        let gutter = 56.0;
        let head_h = 56.0;
        let allday_h = 26.0;
        let grid_x0 = area.x0 + gutter;
        let col_w = (area.width() - gutter) / days.len() as f64;
        let hours = (DAY_END - DAY_START) as f64;
        let body_top = area.y0 + head_h + allday_h;
        let body_h = (area.y1 - body_top - 8.0).max(40.0);
        let hour_h = body_h / hours;

        // Day headers + all-day strip.
        for (i, day) in days.iter().enumerate() {
            let x0 = grid_x0 + col_w * i as f64;
            let head = Rect::new(x0, area.y0, x0 + col_w, area.y0 + head_h);
            let is_today = *day == self.today;
            if is_today {
                fill_rect(scene, t, c.accent[5].with_alpha(0.10), head);
            }
            tr.draw_text(
                scene,
                date::WEEKDAY_SHORT[day.weekday().num_days_from_monday() as usize],
                11.0,
                if i >= 5 { c.text_dim } else { c.neutral[7] },
                t * Affine::translate((x0 + 10.0, area.y0 + 8.0)),
                None,
            );
            let num = format!("{}", day.day());
            let ncol = if is_today { c.accent[5] } else { c.neutral[9] };
            tr.draw_text(
                scene,
                &num,
                20.0,
                ncol,
                t * Affine::translate((x0 + 10.0, area.y0 + 24.0)),
                None,
            );
            hits.push((head, Action::OpenDay(*day)));

            // all-day chips
            let allday: Vec<(Id, String, Palette)> = self
                .model
                .events_on(*day)
                .iter()
                .filter(|e| e.all_day)
                .map(|e| (e.id, e.title.clone(), e.color))
                .collect();
            let strip = Rect::new(x0 + 2.0, area.y0 + head_h + 2.0, x0 + col_w - 2.0, area.y0 + head_h + allday_h - 2.0);
            if !allday.is_empty() {
                let (id, title, color) = &allday[0];
                fill_round(scene, t, color.color(theme), strip, 4.0);
                let line = elide_to_width(tr, title, 10.0, strip.width() - 12.0);
                tr.draw_text(
                    scene,
                    &line,
                    10.0,
                    Color::WHITE,
                    t * Affine::translate((strip.x0 + 6.0, strip.y0 + 4.0)),
                    None,
                );
                hits.push((strip, Action::OpenEvent(*id)));
            }
        }

        // Hour gridlines + labels.
        for h in 0..=(hours as i64) {
            let y = body_top + hour_h * h as f64;
            stroke_line(scene, t, c.border_standard.with_alpha(0.5), Point::new(grid_x0, y), Point::new(area.x1, y), 0.5);
            if h < hours as i64 {
                tr.draw_text(
                    scene,
                    &format!("{:02}:00", DAY_START + h),
                    10.0,
                    c.text_dim,
                    t * Affine::translate((area.x0 + 12.0, y + 2.0)),
                    None,
                );
            }
        }
        // vertical column separators
        for i in 0..=days.len() {
            let x = grid_x0 + col_w * i as f64;
            stroke_line(scene, t, c.border_standard.with_alpha(0.5), Point::new(x, body_top), Point::new(x, area.y1 - 8.0), 0.5);
        }

        // "now" line if today is in range.
        let now = Local::now().naive_local();
        if let Some(i) = days.iter().position(|d| *d == self.today) {
            let frac = ((now.hour() as i64 - DAY_START) as f64 + now.minute() as f64 / 60.0) / hours;
            if (0.0..=1.0).contains(&frac) {
                let y = body_top + body_h * frac;
                let x0 = grid_x0 + col_w * i as f64;
                stroke_line(scene, t, c.semantic.danger, Point::new(x0, y), Point::new(x0 + col_w, y), 2.0);
                scene.fill(Fill::NonZero, t, c.semantic.danger, None, &Circle::new(Point::new(x0, y), 3.5));
            }
        }

        // Timed event blocks.
        for (i, day) in days.iter().enumerate() {
            let x0 = grid_x0 + col_w * i as f64;
            let timed: Vec<(Id, String, String, Palette, NaiveDateTime, NaiveDateTime)> = self
                .model
                .events_on(*day)
                .iter()
                .filter(|e| !e.all_day)
                .map(|e| (e.id, e.title.clone(), e.location.clone(), e.color, e.start, e.end))
                .collect();
            for (id, title, loc, color, start, end) in timed {
                let s = (start.hour() as i64 - DAY_START) as f64 + start.minute() as f64 / 60.0;
                let e = (end.hour() as i64 - DAY_START) as f64 + end.minute() as f64 / 60.0;
                let top = body_top + (s.max(0.0)) * hour_h;
                let bottom = body_top + (e.min(hours)) * hour_h;
                let block = Rect::new(x0 + 3.0, top, x0 + col_w - 3.0, (bottom).max(top + 16.0));
                let col = color.color(theme);
                fill_round(scene, t, col.with_alpha(0.22), block, 6.0);
                fill_round(scene, t, col, Rect::new(block.x0, block.y0, block.x0 + 3.0, block.y1), 2.0);
                scene.push_layer(Fill::NonZero, vello::peniko::BlendMode::default(), 1.0, t, &block);
                // Single line, ellipsized to the block width rather than wrapped —
                // a long title must never spill onto the time/location subtitle
                // row below it, and it degrades to "Team stan…" instead of a hard
                // mid-glyph slice.
                let text_w = block.width() - 20.0;
                let title_line = elide_to_width(tr, &title, 11.0, text_w);
                tr.draw_text(
                    scene,
                    &title_line,
                    11.0,
                    c.neutral[9],
                    t * Affine::translate((block.x0 + 10.0, block.y0 + 4.0)),
                    None,
                );
                if block.height() > 34.0 {
                    let sub = format!("{}–{} {}", date::fmt_time(&start), date::fmt_time(&end), loc);
                    // Single ellipsized line too — a short block has only one row
                    // of vertical space for the subtitle, so wrapping the location
                    // would spill past the block bottom.
                    let sub_line = elide_to_width(tr, &sub, 10.0, text_w);
                    tr.draw_text(
                        scene,
                        &sub_line,
                        10.0,
                        c.text_dim,
                        t * Affine::translate((block.x0 + 10.0, block.y0 + 18.0)),
                        None,
                    );
                }
                scene.pop_layer();
                hits.push((block, Action::OpenEvent(id)));
            }
        }

        hits
    }

    fn draw_agenda(
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

        let from = self.anchor;
        let to = self.anchor + Duration::days(21);
        let mut y = area.y0 + 18.0;
        let x0 = area.x0 + 24.0;

        let mut day = from;
        while day <= to && y < area.y1 - 20.0 {
            let evs = self.model.events_on(day);
            let tasks: Vec<_> = self
                .model
                .tasks
                .iter()
                .filter(|tk| tk.due == Some(day) && tk.parent.is_none())
                .collect();
            if evs.is_empty() && tasks.is_empty() {
                day += Duration::days(1);
                continue;
            }

            // Date heading.
            let is_today = day == self.today;
            let head_col = if is_today { c.accent[5] } else { c.neutral[8] };
            tr.draw_text(
                scene,
                &date::relative_label(day, self.today),
                15.0,
                head_col,
                t * Affine::translate((x0, y)),
                None,
            );
            tr.draw_text(
                scene,
                &date::fmt_day_long(day),
                12.0,
                c.text_dim,
                t * Affine::translate((x0 + 120.0, y + 2.0)),
                None,
            );
            y += 26.0;

            for e in &evs {
                if y > area.y1 - 18.0 {
                    break;
                }
                let row = Rect::new(x0, y, area.x1 - 24.0, y + 30.0);
                if row.contains(self.last_mouse) {
                    fill_round(scene, t, c.surface_white_5, row, 6.0);
                }
                let col = e.color.color(theme);
                fill_round(scene, t, col, Rect::new(row.x0, row.y0 + 4.0, row.x0 + 4.0, row.y1 - 4.0), 2.0);
                let time = if e.all_day { "All day".to_string() } else { date::fmt_time(&e.start) };
                tr.draw_text(scene, &time, 12.0, c.text_dim, t * Affine::translate((row.x0 + 16.0, y + 7.0)), None);
                tr.draw_text(
                    scene,
                    &e.title,
                    13.0,
                    c.neutral[9],
                    t * Affine::translate((row.x0 + 90.0, y + 7.0)),
                    Some((row.width() - 220.0) as f32),
                );
                if !e.location.is_empty() {
                    tr.draw_text(
                        scene,
                        &e.location,
                        12.0,
                        c.text_dim,
                        t * Affine::translate((row.x1 - 130.0, y + 7.0)),
                        Some(126.0),
                    );
                }
                hits.push((row, Action::OpenEvent(e.id)));
                y += 32.0;
            }
            for tk in &tasks {
                if y > area.y1 - 18.0 {
                    break;
                }
                let row = Rect::new(x0, y, area.x1 - 24.0, y + 30.0);
                if row.contains(self.last_mouse) {
                    fill_round(scene, t, c.surface_white_5, row, 6.0);
                }
                // Open the task on a plain row click; checkbox + label toggle
                // completion (pushed on top so they win the reverse hit-scan).
                hits.push((row, Action::OpenTask(tk.id)));
                let box_rect = Rect::new(row.x0 + 8.0, row.center().y - 8.0, row.x0 + 24.0, row.center().y + 8.0);
                draw_checkbox(scene, t, theme, box_rect, tk.done);
                let col = if tk.done { c.text_dim } else { c.neutral[9] };
                let label_x0 = row.x0 + 34.0;
                tr.draw_text(
                    scene,
                    &tk.title,
                    13.0,
                    col,
                    t * Affine::translate((label_x0, y + 7.0)),
                    Some((row.width() - 160.0) as f32),
                );
                let label_w = tr.measure(&tk.title, 13.0, None).0 as f64;
                if tk.done {
                    stroke_line(scene, t, c.text_dim, Point::new(label_x0, row.center().y), Point::new((label_x0 + label_w).min(row.x1 - 120.0), row.center().y), 1.0);
                }
                let complete = Rect::new(
                    row.x0,
                    row.y0,
                    (label_x0 + label_w.min(row.width() - 160.0) + 8.0).min(row.x1 - 40.0),
                    row.y1,
                );
                hits.push((complete, Action::ToggleTask(tk.id)));
                if tk.priority != Priority::None {
                    tr.draw_text(scene, tk.priority.flag(), 11.0, tk.priority.color(theme), t * Affine::translate((row.x1 - 30.0, y + 7.0)), None);
                }
                y += 32.0;
            }
            y += 12.0;
            day += Duration::days(1);
        }

        if y <= area.y0 + 18.0 {
            center_text(
                scene,
                tr,
                "Nothing scheduled — press New to add an event",
                14.0,
                c.text_dim,
                area,
                t,
            );
        }
        hits
    }

    fn draw_detail(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) -> Vec<(Rect, Action)> {
        let c = &theme.tokens.colors;
        let mut hits = Vec::new();
        let r = self.detail_rect;
        fill_rect(scene, t, c.surfaces.raised, r);
        stroke_line(scene, t, c.border_standard, Point::new(r.x0, r.y0), Point::new(r.x0, r.y1), 1.0);

        let x = r.x0 + DETAIL_PAD;
        let w = self.notes_w;
        let mut y = r.y0 + 18.0;

        // In the narrow single-pane layout the detail is its own pushed screen,
        // so it gets a back affordance instead of a docked close button.
        if matches!(self.sidebar_mode, SidebarMode::Overlay) {
            let back = Rect::new(r.x0 + 12.0, r.y0 + 12.0, r.x0 + 96.0, r.y0 + 44.0);
            if back.contains(self.last_mouse) {
                fill_round(scene, t, c.surface_white_5, back, 8.0);
            }
            draw_back_arrow(scene, t, c.neutral[8], Point::new(back.x0 + 14.0, back.center().y));
            tr.draw_text(
                scene,
                "Back",
                13.0,
                c.neutral[8],
                t * Affine::translate((back.x0 + 28.0, back.center().y - 8.0)),
                None,
            );
            hits.push((back, Action::BackToCalendar));
            // Push the detail body below the back bar.
            y += 34.0;
        }

        // close button
        let close = Rect::new(r.x1 - 36.0, r.y0 + 14.0, r.x1 - 14.0, r.y0 + 36.0);
        draw_icon_button(scene, t, theme, close, close.contains(self.last_mouse));
        draw_cross(scene, t, c.neutral[8], close.center());
        hits.push((close, Action::CloseDetail));

        match self.selection {
            Some(Sel::Event(id)) => {
                if let Some(e) = self.model.event(id) {
                    // colour dot + kind label
                    scene.fill(Fill::NonZero, t, e.color.color(theme), None, &Circle::new(Point::new(x + 6.0, y + 8.0), 6.0));
                    tr.draw_text(scene, "EVENT", 11.0, c.text_dim, t * Affine::translate((x + 20.0, y + 2.0)), None);
                    y += 26.0;
                    tr.draw_text(scene, &e.title, 22.0, c.text_primary, t * Affine::translate((x, y)), Some(w as f32));
                    y += 40.0;

                    // when
                    let when = if e.all_day {
                        format!("{} · All day", date::fmt_day_long(e.start.date()))
                    } else {
                        format!(
                            "{} · {}–{}",
                            date::fmt_day_long(e.start.date()),
                            date::fmt_time(&e.start),
                            date::fmt_time(&e.end)
                        )
                    };
                    field_row(scene, tr, theme, t, x, y, w, "When", &when);
                    y += 34.0;
                    if !e.location.is_empty() {
                        field_row(scene, tr, theme, t, x, y, w, "Where", &e.location);
                        y += 34.0;
                    }
                    if let Some(p) = e.project.and_then(|pid| self.model.project(pid)) {
                        field_row(scene, tr, theme, t, x, y, w, "Calendar", &p.name);
                        y += 34.0;
                    }

                    // editable chips: recurrence / reminder / colour
                    let rec_label = if e.recurrence == Recurrence::None {
                        "No repeat".to_string()
                    } else {
                        e.recurrence.short().to_string()
                    };
                    let rec_chip = chip(scene, t, tr, theme, x, y, &format!("↻ {rec_label}"), false);
                    hits.push((rec_chip, Action::CycleRecurrence));
                    let rem_label = match e.reminder_min {
                        None => "No reminder".to_string(),
                        Some(m) if m >= 60 => format!("⏰ {}h before", m / 60),
                        Some(m) => format!("⏰ {m}m before"),
                    };
                    let rem_chip = chip(scene, t, tr, theme, rec_chip.x1 + 8.0, y, &rem_label, false);
                    hits.push((rem_chip, Action::CycleReminder));
                    y += 38.0;
                    let col_chip = chip(scene, t, tr, theme, x, y, &format!("● {}", e.color.name()), false);
                    // recolour the bullet
                    scene.fill(Fill::NonZero, t, e.color.color(theme), None, &Circle::new(Point::new(col_chip.x0 + 12.0, col_chip.center().y), 4.0));
                    hits.push((col_chip, Action::CycleColor));
                }
            }
            Some(Sel::Task(id)) => {
                if let Some(tk) = self.model.task(id) {
                    let box_rect = Rect::new(x, y, x + 18.0, y + 18.0);
                    draw_checkbox(scene, t, theme, box_rect, tk.done);
                    hits.push((box_rect, Action::ToggleSelectedTask));
                    tr.draw_text(scene, "TASK", 11.0, c.text_dim, t * Affine::translate((x + 28.0, y + 2.0)), None);
                    y += 26.0;
                    let title_col = if tk.done { c.text_dim } else { c.text_primary };
                    tr.draw_text(scene, &tk.title, 21.0, title_col, t * Affine::translate((x, y)), Some(w as f32));
                    y += 40.0;
                    if let Some(due) = tk.due {
                        field_row(scene, tr, theme, t, x, y, w, "Due", &date::fmt_day_long(due));
                        y += 34.0;
                    }
                    field_row(scene, tr, theme, t, x, y, w, "Priority", tk.priority.label());
                    y += 34.0;
                    if let Some(p) = tk.project.and_then(|pid| self.model.project(pid)) {
                        field_row(scene, tr, theme, t, x, y, w, "Project", &p.name);
                        y += 34.0;
                    }
                    let subs = self.model.subtasks_of(id);
                    if !subs.is_empty() {
                        tr.draw_text(scene, "Subtasks", 12.0, c.text_dim, t * Affine::translate((x, y)), None);
                        y += 22.0;
                        for s in subs {
                            let box_rect = Rect::new(x, y, x + 16.0, y + 16.0);
                            draw_checkbox(scene, t, theme, box_rect, s.done);
                            hits.push((box_rect, Action::ToggleTask(s.id)));
                            tr.draw_text(scene, &s.title, 13.0, c.neutral[8], t * Affine::translate((x + 26.0, y)), None);
                            y += 24.0;
                        }
                    }
                }
            }
            None => {}
        }

        // Notes header (the editor itself is drawn separately in draw()).
        let notes_label_y = r.y0 + self.notes_top_offset() - 26.0;
        tr.draw_text(scene, "NOTES", 11.0, c.text_dim, t * Affine::translate((x, notes_label_y)), None);
        stroke_line(scene, t, c.border_standard, Point::new(x, notes_label_y + 18.0), Point::new(x + w, notes_label_y + 18.0), 1.0);

        hits
    }

    fn draw_status(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, content_x0: f64) {
        if let Some(msg) = &self.status {
            let c = &theme.tokens.colors;
            let wmeas = tr.measure(msg, 12.0, None).0 as f64;
            let pill = Rect::new(content_x0 + 20.0, self.rect.y1 - 44.0, content_x0 + 44.0 + wmeas, self.rect.y1 - 16.0);
            fill_round(scene, t, c.neutral[2], pill, 8.0);
            stroke_round(scene, t, c.border_standard, pill, 8.0, 1.0);
            tr.draw_text(scene, msg, 12.0, c.neutral[9], t * Affine::translate((pill.x0 + 12.0, pill.center().y - 7.0)), None);
        }
    }
}

impl Default for CalendarApp {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for CalendarApp {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        if !self.seeded {
            // Live app: adopt the user's persisted calendar (empty on a fresh
            // machine). Demo/test builds pass no path and stay in-memory.
            if let Some(p) = &self.data_path {
                if let Some(mut loaded) = crate::store::load(p) {
                    let _ = state.merge(&mut loaded);
                }
            }
            // Seed sample content ONLY in the demo build, and only when the
            // document is still empty.
            if self.demo && CalDoc::is_empty(state) {
                CalDoc::seed(state, &self.model);
                self.seed_demo_notes(state);
            }
            // Whatever the document holds is now authoritative for the UI.
            if !CalDoc::is_empty(state) {
                self.model = CalDoc::read_model(state);
            } else {
                self.model = Model::default();
            }
            self.revalidate_selection();
            self.saved_heads = state.get_heads();
            self.seeded = true;
        }
        // Drain a finished background CalDAV pull, if one landed since the last
        // frame, before binding the editor so a freshly-synced selection reads
        // back its notes.
        self.poll_sync(state);
        self.ensure_bound(state);
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
                        left: length(0.0),
                        top: length(0.0),
                        right: taffy::LengthPercentageAuto::auto(),
                        bottom: taffy::LengthPercentageAuto::auto(),
                    },
                    ..existing
                },
            );
        }
        self.editor_node = Some(editor_node);

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
        node_map.insert(root, self as *mut dyn Widget);
        root
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        let new_rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
        if new_rect.width() != self.rect.width() || new_rect.height() != self.rect.height() {
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
            hits.extend(self.draw_topbar(scene, t, text_renderer, theme));
            let area = self.view_rect();
            match self.view {
                View::Month => hits.extend(self.draw_month(scene, t, text_renderer, theme, area)),
                View::Week => {
                    let days = date::week_days(self.anchor);
                    hits.extend(self.draw_timeline(scene, t, text_renderer, theme, area, &days));
                }
                View::Day => {
                    let days = [self.anchor];
                    hits.extend(self.draw_timeline(scene, t, text_renderer, theme, area, &days));
                }
                View::Agenda => hits.extend(self.draw_agenda(scene, t, text_renderer, theme, area)),
            }
            self.draw_status(scene, t, text_renderer, theme, self.content_rect.x0);
        }

        if detail_drawn {
            hits.extend(self.draw_detail(scene, t, text_renderer, theme));
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
            scene.push_layer(Fill::NonZero, vello::peniko::BlendMode::default(), 1.0, t, &clip);
            self.notes_editor.draw(scene, t, text_renderer, theme, presence);
            scene.pop_layer();
            self.notes_editor.draw_overlay(scene, t, text_renderer, theme);
        }

        // Sidebar last so the overlay flap (and its scrim) paint over the
        // calendar/detail content beneath them.
        hits.extend(self.draw_sidebar(scene, t, text_renderer, theme));

        self.hits = hits;
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
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
        let in_notes = editor_active
            && self.last_mouse.x >= self.detail_rect.x0
            && self.last_mouse.y >= self.notes_origin.y - 4.0;

        // Keyboard.
        if let WindowEvent::KeyboardInput {
            event: KeyEvent { logical_key, state: ElementState::Pressed, .. },
            ..
        } = event
        {
            // Escape: dismiss the overlay flap, else pop the narrow detail
            // screen back to the calendar, else close the detail entirely.
            if matches!(logical_key, Key::Named(NamedKey::Escape)) {
                if matches!(self.sidebar_mode, SidebarMode::Overlay) && self.sidebar_open {
                    self.sidebar_open = false;
                    *handled = true;
                    self.dirty = true;
                    return;
                }
                if matches!(self.sidebar_mode, SidebarMode::Overlay)
                    && matches!(self.narrow_screen, NarrowScreen::Detail)
                {
                    self.narrow_screen = NarrowScreen::Calendar;
                    self.notes_editor.set_focus(false);
                    *handled = true;
                    self.dirty = true;
                    return;
                }
                if self.detail_open {
                    self.detail_open = false;
                    self.selection = None;
                    self.narrow_screen = NarrowScreen::Calendar;
                    self.notes_editor.set_focus(false);
                    *handled = true;
                    self.dirty = true;
                    return;
                }
            }
            // When notes are focused, let the editor own the keystrokes.
            if !(editor_active && self.notes_editor.is_focused()) {
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
                        return;
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
                        return;
                    }
                }
            }
        }

        // Mouse click resolution.
        if let WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } = event {
            // Clicks inside the notes region go to the editor (focus + caret).
            if in_notes {
                if let Some(n) = self.editor_node {
                    self.notes_editor.handle_event(event, scale_factor, modifiers, state, path, handled, focus_target, n, taffy);
                }
                self.dirty = true;
                return;
            }
            // Otherwise resolve against the hit-map (topmost last).
            let p = self.last_mouse;
            let mut chosen: Option<Action> = None;
            for (rect, action) in self.hits.iter().rev() {
                if rect.contains(p) {
                    chosen = Some(action.clone());
                    break;
                }
            }
            if let Some(a) = chosen {
                self.notes_editor.set_focus(false);
                self.apply_action(a, state);
                *handled = true;
                return;
            }
        }

        // Forward remaining events (key typing, mouse move) to the editor
        // when the notes panel is active so editing works.
        if editor_active {
            if let Some(n) = self.editor_node {
                self.notes_editor.handle_event(event, scale_factor, modifiers, state, path, handled, focus_target, n, taffy);
            }
        }
        self.dirty = true;
    }

    fn name(&self) -> &str {
        "CalendarApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.notes_editor.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.notes_editor.mark_drawn();
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
        // waiting for an unrelated input event.
        self.syncing
    }

    fn is_layout_dirty(&self) -> bool {
        self.dirty || self.notes_editor.needs_redraw()
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

// ========================================================================
// Drawing primitives (free functions).
// ========================================================================

fn fill_rect(scene: &mut Scene, t: Affine, color: Color, r: Rect) {
    scene.fill(Fill::NonZero, t, color, None, &r);
}

fn fill_round(scene: &mut Scene, t: Affine, color: Color, r: Rect, radius: f64) {
    scene.fill(Fill::NonZero, t, color, None, &RoundedRect::from_rect(r, radius));
}

fn stroke_round(scene: &mut Scene, t: Affine, color: Color, r: Rect, radius: f64, w: f64) {
    scene.stroke(&Stroke::new(w), t, color, None, &RoundedRect::from_rect(r, radius));
}

fn stroke_line(scene: &mut Scene, t: Affine, color: Color, a: Point, b: Point, w: f64) {
    scene.stroke(&Stroke::new(w), t, color, None, &Line::new(a, b));
}

fn center_text(scene: &mut Scene, tr: &mut TextRenderer, text: &str, size: f32, color: Color, r: Rect, t: Affine) {
    let w = tr.measure(text, size, None).0 as f64;
    let x = r.center().x - w / 2.0;
    let y = r.center().y - size as f64 / 2.0;
    tr.draw_text(scene, text, size, color, t * Affine::translate((x, y)), None);
}

fn draw_icon_button(scene: &mut Scene, t: Affine, theme: &Theme, r: Rect, hover: bool) {
    let c = &theme.tokens.colors;
    let bg = if hover { c.surface_white_10 } else { c.surfaces.raised };
    fill_round(scene, t, bg, r, 8.0);
    stroke_round(scene, t, c.border_standard, r, 8.0, 1.0);
}

fn draw_chevron(scene: &mut Scene, t: Affine, color: Color, center: Point, left: bool) {
    // `left == true` draws a "<" (points left); false draws ">".
    let dx = if left { -4.0 } else { 4.0 };
    let mut p = BezPath::new();
    p.move_to(Point::new(center.x - dx, center.y - 5.0));
    p.line_to(Point::new(center.x + dx, center.y));
    p.line_to(Point::new(center.x - dx, center.y + 5.0));
    scene.stroke(&Stroke::new(1.6), t, color, None, &p);
}

fn draw_plus(scene: &mut Scene, t: Affine, color: Color, center: Point) {
    stroke_line(scene, t, color, Point::new(center.x - 5.0, center.y), Point::new(center.x + 5.0, center.y), 1.8);
    stroke_line(scene, t, color, Point::new(center.x, center.y - 5.0), Point::new(center.x, center.y + 5.0), 1.8);
}

fn draw_hamburger(scene: &mut Scene, t: Affine, color: Color, center: Point) {
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
    stroke_line(scene, t, color, Point::new(center.x - 6.0, center.y), Point::new(center.x + 6.0, center.y), 1.7);
    let mut p = BezPath::new();
    p.move_to(Point::new(center.x - 1.0, center.y - 5.0));
    p.line_to(Point::new(center.x - 6.0, center.y));
    p.line_to(Point::new(center.x - 1.0, center.y + 5.0));
    scene.stroke(&Stroke::new(1.7), t, color, None, &p);
}

fn draw_cross(scene: &mut Scene, t: Affine, color: Color, center: Point) {
    stroke_line(scene, t, color, Point::new(center.x - 5.0, center.y - 5.0), Point::new(center.x + 5.0, center.y + 5.0), 1.6);
    stroke_line(scene, t, color, Point::new(center.x + 5.0, center.y - 5.0), Point::new(center.x - 5.0, center.y + 5.0), 1.6);
}

fn draw_check(scene: &mut Scene, t: Affine, color: Color, r: Rect) {
    let mut p = BezPath::new();
    p.move_to(Point::new(r.x0 + r.width() * 0.22, r.center().y));
    p.line_to(Point::new(r.x0 + r.width() * 0.42, r.y1 - r.height() * 0.28));
    p.line_to(Point::new(r.x1 - r.width() * 0.20, r.y0 + r.height() * 0.28));
    scene.stroke(&Stroke::new(1.8), t, color, None, &p);
}

fn draw_checkbox(scene: &mut Scene, t: Affine, theme: &Theme, r: Rect, done: bool) {
    let c = &theme.tokens.colors;
    if done {
        fill_round(scene, t, c.accent[5], r, 4.0);
        draw_check(scene, t, Color::WHITE, r);
    } else {
        stroke_round(scene, t, c.neutral[5], r, 4.0, 1.6);
    }
}

fn draw_count_badge(scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, cx: f64, cy: f64, n: usize) {
    let c = &theme.tokens.colors;
    let s = format!("{n}");
    let w = (tr.measure(&s, 11.0, None).0 as f64 + 12.0).max(20.0);
    let r = Rect::new(cx - w / 2.0, cy - 9.0, cx + w / 2.0, cy + 9.0);
    fill_round(scene, t, c.neutral[3], r, 9.0);
    center_text(scene, tr, &s, 11.0, c.neutral[9], r, t);
}

#[allow(clippy::too_many_arguments)]
fn field_row(scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, x: f64, y: f64, w: f64, label: &str, value: &str) {
    let c = &theme.tokens.colors;
    tr.draw_text(scene, label, 11.0, c.text_dim, t * Affine::translate((x, y)), None);
    tr.draw_text(scene, value, 14.0, c.neutral[9], t * Affine::translate((x + 84.0, y - 2.0)), Some((w - 84.0) as f32));
}

fn draw_download(scene: &mut Scene, t: Affine, color: Color, center: Point) {
    // Down-arrow into a tray.
    stroke_line(scene, t, color, Point::new(center.x, center.y - 6.0), Point::new(center.x, center.y + 2.0), 1.6);
    let mut p = BezPath::new();
    p.move_to(Point::new(center.x - 4.0, center.y - 1.0));
    p.line_to(Point::new(center.x, center.y + 3.0));
    p.line_to(Point::new(center.x + 4.0, center.y - 1.0));
    scene.stroke(&Stroke::new(1.6), t, color, None, &p);
    stroke_line(scene, t, color, Point::new(center.x - 5.0, center.y + 6.0), Point::new(center.x + 5.0, center.y + 6.0), 1.6);
}

fn draw_upload(scene: &mut Scene, t: Affine, color: Color, center: Point) {
    // Up-arrow rising out of a tray (mirror of `draw_download`).
    stroke_line(scene, t, color, Point::new(center.x, center.y - 5.0), Point::new(center.x, center.y + 3.0), 1.6);
    let mut p = BezPath::new();
    p.move_to(Point::new(center.x - 4.0, center.y - 1.0));
    p.line_to(Point::new(center.x, center.y - 5.0));
    p.line_to(Point::new(center.x + 4.0, center.y - 1.0));
    scene.stroke(&Stroke::new(1.6), t, color, None, &p);
    stroke_line(scene, t, color, Point::new(center.x - 5.0, center.y + 6.0), Point::new(center.x + 5.0, center.y + 6.0), 1.6);
}

/// Draw a small pill chip and return its rect.
fn chip(scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, x: f64, y: f64, label: &str, filled: bool) -> Rect {
    let c = &theme.tokens.colors;
    let tw = tr.measure(label, 12.0, None).0 as f64;
    let r = Rect::new(x, y, x + tw + 24.0, y + 28.0);
    if filled {
        fill_round(scene, t, c.accent[5], r, 8.0);
    } else {
        fill_round(scene, t, c.neutral[2], r, 8.0);
        stroke_round(scene, t, c.border_standard, r, 8.0, 1.0);
    }
    tr.draw_text(scene, label, 12.0, c.neutral[9], t * Affine::translate((r.x0 + 12.0, r.center().y - 7.0)), None);
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

/// Truncate `text` with a trailing ellipsis so it renders within `max_px` at
/// `size`, measured against the real text shaper so the cut matches the drawn
/// glyphs (a fixed character budget can't, since glyph widths vary). Returns
/// the whole string when it already fits, and a lone ellipsis when not even one
/// character does. This is what keeps event titles from being hard-sliced
/// mid-glyph in the dense month/week grids.
fn elide_to_width(tr: &mut TextRenderer, text: &str, size: f32, max_px: f64) -> String {
    if max_px <= 0.0 {
        return String::new();
    }
    if tr.measure(text, size, None).0 as f64 <= max_px {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    // Largest prefix length whose "<prefix>…" still fits within `max_px`.
    let mut lo = 0usize;
    let mut hi = chars.len().saturating_sub(1);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let candidate: String = chars[..mid].iter().chain(std::iter::once(&'…')).collect();
        if tr.measure(&candidate, size, None).0 as f64 <= max_px {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let mut out: String = chars[..lo].iter().collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(app.sidebar_mode, SidebarMode::Persistent);
        assert!(app.content_rect.width() > 0.0);
        assert!(app.detail_rect.width() > 0.0);
        let wide_content = app.content_rect;

        // Resize the SAME instance narrow. A cached-from-first-frame mode would
        // stay Persistent; a correctly re-derived one flips to Overlay.
        app.rect = Rect::new(0.0, 0.0, 560.0, 720.0);
        app.relayout();
        assert_eq!(
            app.sidebar_mode,
            SidebarMode::Overlay,
            "breakpoint must re-derive from the current width, not the first frame"
        );
        // Narrow + detail-open default => detail is the full-width pushed
        // screen and the calendar content pane collapses.
        assert_eq!(app.content_rect.width(), 0.0);
        assert!(app.detail_rect.width() > 0.0);
        assert_ne!(app.content_rect, wide_content, "layout actually changed on resize");

        // Back to wide — proves it toggles both directions, not just once.
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.relayout();
        assert_eq!(app.sidebar_mode, SidebarMode::Persistent);
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
        assert_ne!(app.built_notes_w, wide_built, "editor rebuilt at the resized width");
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
        let created = app.model.events.iter().find(|e| e.title == "New event").map(|e| e.id);
        assert!(created.is_some(), "NewEvent added an event to the model");
        // Persist runs on the next sync (writes only when heads changed).
        app.sync(&mut state.doc, &state.root);
        assert!(path.exists(), "document was written to disk");

        // Fresh instance + fresh doc: it must load the event back from disk.
        let mut app2 = CalendarApp::new_with_data_path(path.clone());
        let mut state2 = ui::State::new();
        app2.sync(&mut state2.doc, &state2.root);
        assert!(
            app2.model.events.iter().any(|e| e.title == "New event"),
            "event survived the restart via on-disk persistence"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
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
        assert!(app.model.events.len() > before, "imported events landed in the model");
        assert!(
            app.model.events.iter().any(|e| e.title == "Quarterly all-hands"),
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
    fn wide_window_uses_persistent_sidebar_and_docked_detail() {
        let mut app = CalendarApp::new_demo();
        app.detail_open = true;
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.relayout();
        assert_eq!(app.sidebar_mode, SidebarMode::Persistent);
        // Sidebar is inline (content starts to its right), detail docked right.
        assert!(app.content_rect.x0 >= app.sidebar_rect.x1 - 0.5);
        assert!(app.detail_rect.area() > 0.0);
        assert!(app.content_rect.area() > 0.0);
    }

    #[test]
    fn narrow_window_collapses_sidebar_to_overlay() {
        let mut app = CalendarApp::new_demo();
        app.rect = Rect::new(0.0, 0.0, 700.0, 900.0);
        app.sidebar_open = false;
        app.relayout();
        assert_eq!(app.sidebar_mode, SidebarMode::Overlay);
        // Overlay sidebar floats over the content from the left edge: content
        // starts at the window's left edge, not to the sidebar's right.
        assert_eq!(app.content_rect.x0, app.rect.x0);
        assert!(!app.sidebar_visible(), "overlay flap is closed by default");
    }

    #[test]
    fn narrow_detail_screen_replaces_the_calendar() {
        let mut app = CalendarApp::new_demo();
        app.rect = Rect::new(0.0, 0.0, 700.0, 900.0);
        app.detail_open = true;
        app.narrow_screen = NarrowScreen::Detail;
        app.relayout();
        // The detail is pushed over the calendar: only one pane is laid out.
        assert!(app.detail_rect.area() > 0.0);
        assert_eq!(app.content_rect.area(), 0.0, "calendar hidden behind detail");

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
        app.rect = Rect::new(0.0, 0.0, 700.0, 900.0);
        app.relayout();
        app.sidebar_open = true;
        let id = app.model.events[0].id;
        app.select_event(id);
        assert_eq!(app.narrow_screen, NarrowScreen::Detail);
        assert!(!app.sidebar_open, "opening a detail dismisses the overlay flap");
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
        app.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &PresenceCtx::empty());

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
        assert!(toggle_in_rail, "checkbox+label must offer a completion target");
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
            project: None,
        };
        tx.send((
            caldav::SyncResult { events: vec![synced], tasks: Vec::new() },
            SyncOutcome::Synced { calendars: 1, events: 1 },
        ))
        .unwrap();
        app.sync_rx = Some(rx);
        app.syncing = true;

        app.poll_sync(&mut state);

        assert!(!app.syncing, "drain clears the in-flight flag");
        assert_eq!(app.model.events.len(), before + 1, "synced event landed");
        assert!(
            app.model.events.iter().any(|e| e.title == "Pulled from server"),
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
            let mut hn = Harness { app, doc, taffy: TaffyTree::new(), node: NodeId::new(0), w, h };
            hn.relayout();
            hn
        }

        fn relayout(&mut self) {
            use taffy::prelude::*;
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            self.taffy = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let child = self.app.build_layout(&mut self.taffy, &theme, &mut node_map);
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
            self.app.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &PresenceCtx::empty());
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
    fn clicking_new_button_creates_and_selects_an_event() {
        let mut h = Harness::new(1280, 800);
        let before = h.app.model.events.len();
        let p = h.hit_center(|a| matches!(a, Action::NewEvent)).expect("New button present");
        h.click(p);
        assert_eq!(h.app.model.events.len(), before + 1, "click created an event");
        assert!(matches!(h.app.selection, Some(Sel::Event(_))), "new event is selected");
    }

    #[test]
    fn clicking_a_view_segment_switches_the_view() {
        let mut h = Harness::new(1280, 800);
        assert_eq!(h.app.view, View::Month);
        let p = h
            .hit_center(|a| matches!(a, Action::View(View::Week)))
            .expect("Week segment present");
        h.click(p);
        assert_eq!(h.app.view, View::Week, "clicking the Week segment switched the view");
    }

    #[test]
    fn clicking_an_event_chip_opens_its_detail() {
        // Month view is full of chips; clicking one selects that event.
        let mut h = Harness::new(1280, 800);
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
        assert_eq!(h.app.selection, Some(Sel::Event(id)), "clicked chip is now selected");
        assert!(h.app.detail_open, "detail panel opened");
    }

    #[test]
    fn narrow_flap_offers_a_pointer_reachable_view_switcher() {
        // On narrow layouts the topbar's segmented switch folds away; the
        // overlay flap must carry a pointer-reachable switch so every view stays
        // reachable without a keyboard (the reported dead-end: a mouse user on
        // narrow could reach Day/Agenda but never get back to Month). Drives the
        // real `handle_event` click path, not a synthetic call.
        let mut h = Harness::new(560, 720);
        assert_eq!(h.app.sidebar_mode, SidebarMode::Overlay, "560px is a narrow layout");
        // Open the flap and re-lay so its hit-map is populated.
        h.app.sidebar_open = true;
        h.relayout();
        assert_eq!(h.app.view, View::Month);
        let p = h
            .hit_center(|a| matches!(a, Action::View(View::Week)))
            .expect("the flap carries a Week view target on narrow");
        h.click(p);
        assert_eq!(h.app.view, View::Week, "clicking the flap switcher changed the view");
        assert!(!h.app.sidebar_open, "picking a view dismisses the flap");
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
        let mut h = Harness::new(1280, 800);
        let view_targets = h
            .app
            .hits
            .iter()
            .filter(|(_, a)| matches!(a, Action::View(_)))
            .count();
        assert_eq!(view_targets, View::ALL.len(), "exactly one switcher on wide");
    }

    #[test]
    fn elide_to_width_ellipsizes_and_actually_fits() {
        let mut tr = TextRenderer::new();
        let full = "Team standup with the whole design org";
        // Given room for the whole string, it comes back untouched (no ellipsis).
        let roomy = tr.measure(full, 11.0, None).0 as f64 + 10.0;
        assert_eq!(elide_to_width(&mut tr, full, 11.0, roomy), full);
        // Under a tight budget it must ellipsize AND the result must measure
        // within the budget — proving the cut is width-driven, not a fixed
        // character count that could still overflow.
        let tight = 60.0;
        let cut = elide_to_width(&mut tr, full, 11.0, tight);
        assert!(cut.ends_with('…'), "ellipsized: {cut:?}");
        assert!(cut.chars().count() < full.chars().count());
        assert!(
            tr.measure(&cut, 11.0, None).0 as f64 <= tight,
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

        let theme = Theme::dark();
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
            .find(|tit| tr.measure(tit, 11.0, None).0 as f64 > block_text_w)
            .expect("demo has a title too long for a narrow week column");
        let drawn = elide_to_width(&mut tr, &long, 11.0, block_text_w);
        assert!(drawn.ends_with('…'), "narrow week title ellipsized: {drawn:?}");
        assert_ne!(drawn, long);
    }
}
