//! The Logs window: one [`NavShell`] whose content is the journal table
//! with its filters (`JournalPane`) and whose inspector shows every field of
//! the selected entry. The window bar holds the journal's commands: refresh,
//! live tail and export.
//!
//! The view owns the [`Journal`] (an external, read-only render cache read
//! off the UI thread) and answers its effects; the [`LogsWindow`] lays it
//! out. The filters sync as document edits on [`crate::resource::Logs`];
//! the selected entry and live tail are this window's own.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use ui::time::{Duration, Instant};
use ui::widget::{AppModel, ModelPath};

use std::sync::mpsc::{Receiver, TryRecvError};
use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::Key;

use ui::framework::{
    Actions, Dispatcher, EffectAnswer, EffectCx, EffectStatus, Effected, Live, LiveRow, Publishes,
    RunMode, Submit, ViewCx,
};
use ui::icons::Icon;
use ui::portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::Spacing;
use ui::widget::{Element, InspectorPlacement, NavShell, TaffyTreeExt, Widget};

use crate::actions::{self, JournalEffect};
use crate::journal::{JournalLoadError, Page};
use crate::model::{Filter, LogEntry, Priority};
use crate::resource::{JournalFixture, Logs};
use crate::view::{Bridge, JournalPane, JournalRow, Placeholder, Request, Shared};
use ui::window_toolbar::{ToolbarAction, ToolbarEvent};

const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// The most recent journal records read.
const JOURNAL_LIMIT: usize = 2000;

/// A finished read of the journal.
#[derive(Debug)]
enum Read {
    /// The most recent records, which replace what the window shows.
    Whole(Page),
    /// What live tail found after the newest record shown.
    Tail(Page),
}

type JournalRead = Result<Read, JournalLoadError>;

/// An `Instant` at least `POLL_INTERVAL` in the past, so writing it into
/// `last_poll` makes the next poll-due check fire immediately. Falls back to
/// `now` itself in the vanishingly rare case the process's monotonic clock
/// has less than `POLL_INTERVAL` of history — that only delays the forced
/// poll by at most `POLL_INTERVAL`, never panics.
fn force_immediate_poll_at() -> Instant {
    let now = ui::time::now();
    now.checked_sub(POLL_INTERVAL).unwrap_or(now)
}

/// The journal's commands in the window bar: refresh, live tail and
/// export (in its menu), each the effect it runs. Live tail is a toggle
/// that stands on while the journal is followed.
const WINDOW_ACTIONS: [(&str, Icon, &str); 3] = [
    ("logs.refresh", Icon::Refresh, "Refresh journal"),
    ("logs.live-tail", Icon::Play, "Live tail"),
    ("logs.export", Icon::Download, "Export filtered logs"),
];

/// The effect a window bar action of [`WINDOW_ACTIONS`] runs.
fn window_effect(id: &str) -> Option<JournalEffect> {
    match id {
        "logs.refresh" => Some(JournalEffect::Refresh),
        "logs.live-tail" => Some(JournalEffect::SetLiveTail { enabled: None }),
        "logs.export" => Some(JournalEffect::ExportFilteredJsonl),
        _ => None,
    }
}

/// What the inspector currently shows.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Inspected {
    /// Not built for the current state yet.
    Stale,
    /// The "no entry selected" state.
    Nothing,
    /// The entry with this resource id.
    Entry(String),
}

/// The journal this window shows, shared by the window and the view's
/// effect handler: what was read, the filter the document holds, and the
/// selection and live tail that are this window's own.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent flag (live tail, loading, reading, \
              exporting, a reveal asked for, dirtiness)"
)]
pub(crate) struct Journal {
    /// Full journal — external read-only data, not CRDT state.
    entries: Vec<LogEntry>,
    /// Distinct units present, sorted (for the unit filter).
    units: Vec<String>,
    /// Distinct boots: (`boot_id`, label), newest first.
    boots: Vec<(String, String)>,

    filter: Filter,
    /// Indices into `entries` that pass `filter`, in display order.
    filtered: Vec<usize>,
    /// Stored selection (absolute index into `entries`, or -1 = auto).
    sel_raw: i64,
    /// Stable selected-entry resource id. This survives reordering within the
    /// local journal cache; it is this window's, never a collaborator's.
    selected_entry: Option<String>,
    live_tail: bool,
    /// A safe classification of the most recent local journal read failure.
    /// Never holds `journalctl` stderr or raw journal content.
    source_error: Option<crate::journal::JournalLoadError>,
    /// Set while a resource-routed refresh is being admitted or completed.
    /// Existing rows stay visible; an empty source shows a loading state
    /// rather than masquerading as empty.
    source_loading: bool,
    /// The journal read in flight off the UI thread, if any.
    reading: Option<Receiver<JournalRead>>,
    /// Where the newest record shown sits in the journal: live tail reads
    /// only what was written after it. `None` reads the whole page again.
    tail_cursor: Option<String>,
    /// Whether an export's Save dialog is open. Neither the chosen path nor
    /// log content is published through resources or collaboration state.
    exporting: bool,
    /// Concise local outcome for the portal write, announced through an a11y
    /// live region. It never includes a chosen path or log contents.
    export_status: Option<String>,

    shared: Shared,
    /// The fingerprints of the state and the rows last handed to the views.
    published: Option<u64>,
    published_rows: Option<u64>,
    /// The selection changed since the window last showed it.
    republished: bool,
    /// The entries changed: the inspector shows the selected one again.
    inspector_stale: bool,
    /// Show the selected entry where the inspector is a closed sheet: it
    /// was chosen.
    reveal: bool,
    /// Open the inspector drawer once laid out (`--detail` captures).
    reveal_detail: bool,

    /// Whether this window reads the journal: a live window, never a
    /// capture or a past version.
    reads: bool,
    last_poll: Instant,
    /// Wakes the window when a read finishes.
    waker: Option<ui::WakeHandle>,
    dirty: bool,
}

pub(crate) type JournalRef = Rc<RefCell<Journal>>;

impl Journal {
    fn with_entries(entries: Vec<LogEntry>) -> Self {
        let mut journal = Self {
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
            reading: None,
            tail_cursor: None,
            exporting: false,
            export_status: None,
            shared: Rc::new(RefCell::new(Bridge::default())),
            published: None,
            published_rows: None,
            republished: false,
            inspector_stale: false,
            reveal: false,
            reveal_detail: false,
            reads: false,
            last_poll: ui::time::now(),
            waker: None,
            dirty: true,
        };
        journal.rebuild_facets();
        journal.recompute_filtered();
        journal.publish();
        journal
    }

    /// The journal a window in `mode` shows: a live window reads this
    /// device's, a capture shows its fixture's, a past version none.
    fn for_window(cx: &ViewCx<Logs>) -> Self {
        let mut journal = Self::with_entries(Vec::new());
        match cx.mode() {
            RunMode::Live => {
                journal.reads = true;
                journal.waker = Some(cx.waker());
                journal.start_read();
            }
            RunMode::Capture => match cx.fixture::<JournalFixture>().as_deref() {
                Some(JournalFixture::Entries(entries)) => journal.show_entries(entries.clone()),
                Some(JournalFixture::Failed(error)) => journal.source_error = Some(*error),
                Some(JournalFixture::Loading) => journal.source_loading = true,
                None => {}
            },
            RunMode::Past => {}
        }
        journal
    }

    /// Recompute the distinct-unit and distinct-boot lists from entries.
    fn rebuild_facets(&mut self) {
        let mut units: Vec<String> = Vec::new();
        let mut boots: Vec<(String, u64)> = Vec::new();
        for e in &self.entries {
            if !units.contains(&e.unit) {
                units.push(e.unit.clone());
            }
            match boots.iter_mut().find(|(b, _)| b == &e.boot_id) {
                Some((_, latest)) => *latest = (*latest).max(e.realtime_us),
                None => boots.push((e.boot_id.clone(), e.realtime_us)),
            }
        }
        units.sort();
        // Newest boot first, named by how far back it is.
        boots.sort_by_key(|(_, latest)| std::cmp::Reverse(*latest));
        self.boots = boots
            .into_iter()
            .enumerate()
            .map(|(i, (id, _))| {
                let label = match i {
                    0 => "Current boot".to_owned(),
                    1 => "Previous boot".to_owned(),
                    n => format!("{n} boots ago"),
                };
                (id, label)
            })
            .collect();
        self.units = units;
    }

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
        if let Ok(s) = usize::try_from(self.sel_raw) {
            if self.filtered.contains(&s) {
                return Some(s);
            }
            // Explicitly selected but filtered out → nothing selected.
            if s < self.entries.len() {
                return None;
            }
        }
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

    /// The selected entry, as the inspector shows it.
    fn selected(&self) -> Option<&LogEntry> {
        self.effective_selected()
            .and_then(|abs| self.entries.get(abs))
    }

    /// What the journal shows in place of rows, if anything.
    fn placeholder(&self) -> Option<Placeholder> {
        if self.entries.is_empty() {
            Some(if self.source_loading {
                Placeholder::Loading
            } else if let Some(error) = self.source_error {
                Placeholder::Unavailable(error)
            } else {
                Placeholder::Empty
            })
        } else if self.filtered.is_empty() {
            Some(Placeholder::NoMatches)
        } else {
            None
        }
    }

    /// A fingerprint of the rows the table lists.
    fn rows_fingerprint(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.entries.len().hash(&mut hasher);
        for e in self.entries.first().into_iter().chain(self.entries.last()) {
            e.realtime_us.hash(&mut hasher);
            e.message.hash(&mut hasher);
        }
        self.filter.min_priority.hash(&mut hasher);
        self.filter.unit.hash(&mut hasher);
        self.filter.boot.hash(&mut hasher);
        self.filter.search.hash(&mut hasher);
        hasher.finish()
    }

    /// A fingerprint of everything the views show, so they are refreshed
    /// only when it changes.
    fn fingerprint(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.rows_fingerprint().hash(&mut hasher);
        self.effective_selected().hash(&mut hasher);
        format!("{:?}", self.placeholder()).hash(&mut hasher);
        self.live_tail.hash(&mut hasher);
        hasher.finish()
    }

    /// Hand the current journal, filters and selection to the views.
    fn publish(&mut self) {
        let fingerprint = self.fingerprint();
        if self.published == Some(fingerprint) {
            return;
        }
        self.published = Some(fingerprint);
        let selected = self.effective_selected();
        let rows = self.rows_fingerprint();
        let rows = (self.published_rows != Some(rows)).then(|| {
            self.published_rows = Some(rows);
            self.filtered
                .iter()
                .filter_map(|&i| self.entries.get(i))
                .map(|e| JournalRow::of(e, actions::entry_resource_id(e)))
                .collect::<Vec<_>>()
        });
        {
            let mut bridge = self.shared.borrow_mut();
            bridge.revision = bridge.revision.wrapping_add(1);
            if let Some(rows) = rows {
                bridge.rows = rows;
                bridge.rows_revision = bridge.rows_revision.wrapping_add(1);
            }
            bridge.selected = selected.and_then(|abs| self.filtered_pos(abs));
            bridge.filter = self.filter.clone();
            bridge.units.clone_from(&self.units);
            bridge.boots.clone_from(&self.boots);
            bridge.placeholder = self.placeholder();
            bridge.has_entries = !self.entries.is_empty();
            bridge.total = self.entries.len();
            bridge.follow = self.live_tail;
        }
        self.republished = true;
    }

    /// Show the filter the document holds.
    fn adopt_filter(&mut self, filter: Filter) {
        self.filter = filter;
        self.recompute_filtered();
        self.publish();
        self.dirty = true;
    }

    /// Read the journal off the UI thread, unless a read is in flight or
    /// this window does not read it.
    fn start_read(&mut self) {
        if !self.reads || self.reading.is_some() {
            return;
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let waker = self.waker.clone();
        let after = self.tail_after();
        drop(ui::rt::app().spawn_blocking(move || {
            let read = match after {
                Some(cursor) => crate::journal::load_after(&cursor, JOURNAL_LIMIT).map(Read::Tail),
                None => crate::journal::load_live(JOURNAL_LIMIT).map(Read::Whole),
            };
            let _ = sender.send(read);
            if let Some(waker) = waker {
                waker.wake();
            }
        }));
        self.reading = Some(receiver);
        if self.entries.is_empty() {
            self.source_loading = true;
        }
    }

    /// The cursor the next read starts after: live tail reads only what is
    /// new once the window shows a page, and the whole page otherwise.
    fn tail_after(&self) -> Option<String> {
        self.tail_cursor
            .clone()
            .filter(|_| !self.entries.is_empty())
    }

    /// Show a finished read, if one finished.
    fn drain_read(&mut self) {
        let Some(receiver) = &self.reading else {
            return;
        };
        match receiver.try_recv() {
            Ok(read) => {
                self.reading = None;
                self.show_read(read);
            }
            Err(TryRecvError::Disconnected) => {
                self.reading = None;
                self.source_loading = false;
                self.dirty = true;
            }
            Err(TryRecvError::Empty) => {}
        }
    }

    /// Read again when live tail is due.
    fn poll(&mut self) {
        self.drain_read();
        if self.reads && self.live_tail && self.last_poll.elapsed() >= POLL_INTERVAL {
            self.last_poll = ui::time::now();
            self.start_read();
        }
    }

    /// When live tail reads next: a deadline, not a frame pump.
    fn next_poll(&self) -> Option<Instant> {
        (self.reads && self.live_tail && self.reading.is_none())
            .then(|| self.last_poll.checked_add(POLL_INTERVAL))
            .flatten()
    }

    /// How the journal effects differ from their declarations now.
    fn effect_statuses(&self) -> Vec<EffectStatus> {
        let mut statuses = Vec::new();
        if self.live_tail {
            statuses.push(EffectStatus::labelled(
                JournalEffect::SET_LIVE_TAIL,
                "Stop live tail",
            ));
        }
        if self.exporting {
            statuses.push(EffectStatus::unavailable(
                JournalEffect::EXPORT_FILTERED_JSONL,
                "export_open",
                "An export dialog is already open",
            ));
        } else if !self.entries.iter().any(|entry| self.filter.passes(entry)) {
            statuses.push(EffectStatus::unavailable(
                JournalEffect::EXPORT_FILTERED_JSONL,
                "no_matching_entries",
                "There are no filtered entries to export",
            ));
        }
        statuses
    }

    /// The entries the filter shows, as live rows.
    fn entry_rows(&self) -> Vec<LiveRow> {
        actions::entry_rows(&self.entries, &self.filter)
    }

    /// Any entry of this device's journal, by id.
    fn entry_row(&self, id: &str) -> Option<LiveRow> {
        self.entries
            .iter()
            .find(|entry| actions::entry_resource_id(entry) == id)
            .map(actions::entry_row)
    }

    /// The entries an export writes: those the filter shows now. Entries
    /// arriving while the Save dialog is open are not part of it.
    fn export_snapshot(&self) -> Result<Vec<LogEntry>, String> {
        if self.exporting {
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
        Ok(entries)
    }

    /// Show the entry `entry_id` names, chosen in the table, by the palette
    /// or by an agent.
    fn select(&mut self, entry_id: String) -> Result<(), String> {
        let abs = self
            .entries
            .iter()
            .position(|entry| actions::entry_resource_id(entry) == entry_id)
            .ok_or_else(|| "this device's journal has no such entry".to_owned())?;
        self.sel_raw = i64::try_from(abs).unwrap_or(i64::MAX);
        self.selected_entry = Some(entry_id);
        self.publish();
        // Showing an entry is navigation: reveal the detail slab on a
        // narrow window, as a pointer selection does.
        self.reveal = true;
        Ok(())
    }

    fn show_entries(&mut self, entries: Vec<LogEntry>) {
        if self.entries != entries {
            // Equal counts and endpoints do not imply equal journal contents.
            // Show the inspector again too: metadata can change without
            // changing the entry's resource id.
            self.published = None;
            self.published_rows = None;
            self.inspector_stale = true;
        }
        self.entries = entries;
        if let Some(id) = &self.selected_entry {
            self.sel_raw = self
                .entries
                .iter()
                .position(|entry| actions::entry_resource_id(entry) == *id)
                .and_then(|index| i64::try_from(index).ok())
                .unwrap_or(-1);
        }
        self.source_error = None;
        self.rebuild_facets();
        self.recompute_filtered();
        self.publish();
        self.dirty = true;
    }
    fn show_read(&mut self, read: JournalRead) {
        self.source_loading = false;
        match read {
            Ok(Read::Whole(page)) => {
                // A tail read still in flight started from the page this
                // replaces: its records would be shown twice.
                self.reading = None;
                self.tail_cursor = page.cursor;
                self.show_entries(page.entries);
            }
            Ok(Read::Tail(page)) => self.show_tail(page),
            Err(error) => {
                self.tail_cursor = None;
                self.source_error = Some(error);
                self.publish();
                self.dirty = true;
            }
        }
    }
    /// Add what live tail found to the page, keeping its newest
    /// `JOURNAL_LIMIT` records. A quiet journal changes nothing: no rows are
    /// rebuilt and nothing is published again.
    fn show_tail(&mut self, page: Page) {
        // A full answer may have left records behind it: the next read takes
        // the newest page whole rather than falling further behind.
        let caught_up = page.entries.len() < JOURNAL_LIMIT;
        if caught_up {
            if let Some(cursor) = page.cursor {
                self.tail_cursor = Some(cursor);
            }
        } else {
            self.tail_cursor = None;
        }
        if page.entries.is_empty() {
            if self.source_error.take().is_some() {
                self.publish();
                self.dirty = true;
            }
            return;
        }
        let mut entries = std::mem::take(&mut self.entries);
        entries.extend(page.entries);
        // Nearly sorted already: records arrive in the journal's order.
        entries.sort_by_key(|entry| entry.realtime_us);
        let excess = entries.len().saturating_sub(JOURNAL_LIMIT);
        entries.drain(..excess);
        self.show_entries(entries);
    }
}

/// The Logs window: the journal and its filters in a [`NavShell`], the
/// selected entry in its inspector, the journal's commands in the window
/// bar.
pub(crate) fn window(cx: &ViewCx<Logs>) -> Element {
    let journal: JournalRef = Rc::new(RefCell::new(Journal::for_window(cx)));
    let actions = cx.actions();
    journal.borrow().shared.borrow_mut().refresh = actions.effect(&JournalEffect::Refresh);
    answer_effects(cx, &journal);
    {
        let journal = Rc::clone(&journal);
        cx.on_stage(move |scenario| {
            if scenario == Some("detail") {
                journal.borrow_mut().reveal_detail = true;
            }
        });
    }
    let (rows, row) = (Rc::clone(&journal), Rc::clone(&journal));
    cx.publishing(
        Publishes::new()
            .live_rows(move || rows.borrow().entry_rows())
            .live_row(move |id| row.borrow().entry_row(id)),
        Box::new(LogsWindow::new(
            journal,
            cx.live().clone(),
            cx.dispatcher(),
            actions,
            cx.mode() != RunMode::Past,
        )),
    )
}

/// Answer the journal's effects, whoever asks: the window bar, the table,
/// the palette, an agent.
fn answer_effects(cx: &ViewCx<Logs>, journal: &JournalRef) {
    let (answer, status) = (Rc::clone(journal), Rc::clone(journal));
    cx.effects(move |effect: JournalEffect, cx| answer_effect(&answer, effect, cx))
        .status(move || status.borrow().effect_statuses());
}

fn answer_effect(
    journal: &JournalRef,
    effect: JournalEffect,
    cx: &mut EffectCx<Logs>,
) -> EffectAnswer {
    let mut shown = journal.borrow_mut();
    shown.dirty = true;
    match effect {
        JournalEffect::Refresh => {
            shown.source_loading = true;
            let journal = Rc::clone(journal);
            Ok(cx.thread(
                |_| crate::journal::load_live(JOURNAL_LIMIT).map(Read::Whole),
                move |(), read: JournalRead, _| {
                    let failed = read.as_ref().err().copied();
                    let mut shown = journal.borrow_mut();
                    shown.show_read(read);
                    match failed {
                        None => Ok(Effected::value(shown.entries.len())),
                        Some(error) => Err(error.message().to_owned()),
                    }
                },
            ))
        }
        JournalEffect::SetLiveTail { enabled } => {
            shown.live_tail = enabled.unwrap_or(!shown.live_tail);
            shown.last_poll = force_immediate_poll_at();
            shown.publish();
            Ok(Effected::done())
        }
        JournalEffect::ExportFilteredJsonl => {
            let entries = shown.export_snapshot()?;
            let picked = export_request().send();
            shown.exporting = true;
            let journal = Rc::clone(journal);
            Ok(cx.thread(
                move |_| export_picked(&picked, &entries),
                move |(), status: Option<String>, _| {
                    let mut shown = journal.borrow_mut();
                    shown.exporting = false;
                    shown.export_status.clone_from(&status);
                    shown.dirty = true;
                    Ok(Effected::value(status))
                },
            ))
        }
        JournalEffect::SelectEntry { entry_id } => {
            shown.select(entry_id)?;
            Ok(Effected::done())
        }
    }
}

/// The window's widget: lays the journal out and turns what the person
/// does in it into filter edits and journal effects.
pub(crate) struct LogsWindow {
    journal: JournalRef,
    shared: Shared,
    /// The filter as the document holds it, shown as it changes.
    live: Live<Logs>,
    presented: Option<(u64, u64)>,
    /// Where filter edits and journal effects go.
    dispatcher: Dispatcher<Logs>,
    actions: Actions<Logs>,
    /// Whether the journal controls are offered: everywhere but a past
    /// version.
    allow_live: bool,

    rect: Rect,
    shell: NavShell,
    /// The spacing the inspector was laid out with.
    spacing: Spacing,
    inspected: Inspected,
    has_inspector: bool,
    layout_dirty: bool,
}

impl LogsWindow {
    fn new(
        journal: JournalRef,
        live: Live<Logs>,
        dispatcher: Dispatcher<Logs>,
        actions: Actions<Logs>,
        allow_live: bool,
    ) -> Self {
        let shared = Rc::clone(&journal.borrow().shared);
        let shell = NavShell::new()
            .content_kind(ui::widget::ContentKind::List)
            .with_id("logs.shell")
            .content(Box::new(JournalPane::new(Rc::clone(&shared), &actions)))
            .with_window_toolbar()
            .inspector(crate::view::entry_detail(None, &Spacing::default()));
        let mut window = Self {
            journal,
            shared,
            live,
            presented: None,
            dispatcher,
            actions,
            allow_live,
            rect: Rect::ZERO,
            shell,
            spacing: Spacing::default(),
            inspected: Inspected::Stale,
            has_inspector: true,
            layout_dirty: true,
        };
        window.follow_document();
        window.show_journal();
        window
    }

    /// Show the filter the document holds when it changed.
    fn follow_document(&mut self) {
        let Some(shown) = self.live.changed_since(&mut self.presented) else {
            return;
        };
        let filter = Logs::filter(&shown);
        self.journal.borrow_mut().adopt_filter(filter);
    }

    /// Bring the inspector and the layout in step with what the journal
    /// published since.
    fn show_journal(&mut self) {
        let (republished, stale, reveal, reveal_detail) = {
            let mut journal = self.journal.borrow_mut();
            journal.publish();
            (
                std::mem::take(&mut journal.republished),
                std::mem::take(&mut journal.inspector_stale),
                std::mem::take(&mut journal.reveal),
                journal.reveal_detail,
            )
        };
        if stale {
            self.inspected = Inspected::Stale;
        }
        if republished || stale {
            self.show_inspector();
            self.layout_dirty = true;
        }
        if reveal {
            self.reveal_inspector();
        }
        if reveal_detail {
            self.layout_dirty = true;
        }
    }

    /// The inspector follows the selection; when the journal lists nothing
    /// it collapses, leaving the one empty state.
    fn show_inspector(&mut self) {
        let journal = Rc::clone(&self.journal);
        let journal = journal.borrow();
        let wanted = !journal.filtered.is_empty();
        if wanted != self.has_inspector {
            self.has_inspector = wanted;
            self.inspected = Inspected::Stale;
            if !wanted {
                self.shell.set_inspector(None);
                return;
            }
        }
        if !wanted {
            return;
        }
        let entry = journal.selected();
        let shows = entry.map_or(Inspected::Nothing, |e| {
            Inspected::Entry(actions::entry_resource_id(e))
        });
        if self.inspected == shows {
            return;
        }
        self.inspected = shows;
        self.shell
            .set_inspector(Some(crate::view::entry_detail(entry, &self.spacing)));
        let title = entry.map_or_else(String::new, |e| e.unit.clone());
        self.shell.set_inspector_title(title);
    }

    /// Turn the views' requests into document edits and journal effects.
    fn apply_requests(&self) {
        let requests = self.shared.borrow_mut().take_requests();
        for request in requests {
            match request {
                Request::Search(value) => self.edit(Logs::paths().search.replace(value)),
                Request::ClearFilters => self.edit(Logs::CLEAR_FILTERS.with(())),
                Request::Select(entry_id) => self.request(&JournalEffect::SelectEntry { entry_id }),
            }
        }
    }

    /// Open the inspector where it is a closed sheet, so choosing an entry
    /// on a narrow window shows it.
    fn reveal_inspector(&mut self) {
        if self.shell.geometry().inspector == InspectorPlacement::Sheet
            && !self.shell.is_inspector_open()
        {
            self.shell.toggle_inspector();
        }
    }

    /// Submit a filter edit; the document presents its result.
    fn edit(&self, edit: impl Submit<Logs>) {
        self.dispatcher.submit(edit);
    }

    /// Invoke a journal effect through the same door as the palette and
    /// agents, which checks its availability first.
    fn request(&self, effect: &JournalEffect) {
        self.dispatcher.effect(effect);
    }

    /// Run what the person did in the window's bar: the shell's own slots,
    /// then the journal's commands.
    fn take_window_toolbar_events(&mut self) {
        for event in ui::window_toolbar::take_events() {
            if self.shell.window_toolbar_event(&event) {
                continue;
            }
            if let ToolbarEvent::Action { id } = &event
                && let Some(effect) = window_effect(id)
                && self.can(&effect)
            {
                self.request(&effect);
            }
        }
    }

    /// Whether this window may run `effect` now.
    fn can(&self, effect: &JournalEffect) -> bool {
        self.actions.effect(effect).is_some()
    }

    /// Declare the window bar's slots: the shell's own (the entry's
    /// details) and the journal's commands.
    fn declare_window_toolbar(&self) {
        let live_tail = self.journal.borrow().live_tail;
        let mut toolbar = self.shell.window_toolbar();
        let actions = WINDOW_ACTIONS.into_iter().map(|(id, icon, name)| {
            let enabled = window_effect(id).is_some_and(|effect| self.can(&effect));
            let action = ToolbarAction::new(id, icon.name(), name)
                .enabled(enabled)
                .on(id == "logs.live-tail" && live_tail);
            // Exporting is occasional: the bar's menu, which leaves the
            // row to Refresh, Live tail and the details' toggle.
            if id == "logs.export" {
                action.in_menu()
            } else {
                action
            }
        });
        NavShell::push_window_actions(&mut toolbar, actions);
        ui::window_toolbar::declare(Some(toolbar));
    }
}

/// The shell's node: the one child of the app's node.
fn shell_node(taffy: &TaffyTree<()>, node: NodeId) -> Option<NodeId> {
    taffy.children(node).ok()?.first().copied()
}

fn export_request() -> PickRequest {
    PickRequest::save("Export filtered logs", "Export", "journal-filtered.jsonl")
        .thread_name("logs-portal-export")
        .filter(
            FileFilter::new("JSON Lines")
                .mimetype("application/x-ndjson")
                .glob("*.jsonl"),
        )
}
fn export_picked(picked: &PortalChannel, entries: &[LogEntry]) -> Option<String> {
    match picked.recv() {
        Ok(PortalOutcome::Opened(path)) => {
            let refs = entries.iter().collect::<Vec<_>>();
            Some(match crate::journal::export_json_lines(&refs, &path) {
                Ok(()) => format!("Exported {} log entries.", entries.len()),
                Err(_) => "Export failed.".to_owned(),
            })
        }
        Ok(PortalOutcome::Cancelled) | Err(_) => None,
        Ok(PortalOutcome::Failed(_)) => Some("Export unavailable.".to_owned()),
    }
}
impl Widget for LogsWindow {
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.follow_document();
        self.journal.borrow_mut().poll();
        self.take_window_toolbar_events();
        self.show_journal();
        self.declare_window_toolbar();
        self.shell.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.layout_dirty = false;
        if self.spacing != theme.tokens.spacing {
            // The inspector's content lines up with its property list in
            // this theme's spacing.
            self.spacing = theme.tokens.spacing;
            self.inspected = Inspected::Stale;
            self.show_inspector();
        }
        let shell = self.shell.build_layout(taffy, theme, node_map);
        let node = taffy.fill_node(shell);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        if let Some(shell) = shell_node(taffy, node) {
            self.shell
                .apply_layout(taffy, shell, Point::new(self.rect.x0, self.rect.y0));
        }
        if self.journal.borrow().reveal_detail
            && self.shell.geometry().inspector != InspectorPlacement::None
        {
            self.journal.borrow_mut().reveal_detail = false;
            self.reveal_inspector();
            self.shell.snap();
            self.layout_dirty = true;
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.shell.draw(scene, transform, tr, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // Ctrl+L follows or pauses the journal, like a terminal's `-f`.
        if let WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == ElementState::Pressed
            && ctx.modifiers.state().control_key()
            && matches!(&key.logical_key, Key::Character(c) if c.eq_ignore_ascii_case("l"))
        {
            if self.allow_live {
                self.request(&JournalEffect::SetLiveTail { enabled: None });
            }
            *ctx.handled = true;
            return;
        }
        if let Some(shell) = shell_node(ctx.taffy, ctx.node_id) {
            self.shell
                .handle_event(event, &mut ctx.reborrow(ctx.path, shell));
        }
        self.apply_requests();
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.shell]
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        visitor(&self.shell);
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.shell]
    }

    fn wants_pointer_capture(&self) -> bool {
        self.shell.wants_pointer_capture()
    }

    fn name(&self) -> &'static str {
        "LogsWindow"
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn test_id(&self) -> Option<&str> {
        Some("logs_app")
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        let status = self.journal.borrow().export_status.clone()?;
        Some(
            ui::a11y::AccessNode::new(ui::a11y::Role::StaticText)
                .with_name(&status)
                .with_live(ui::a11y::Politeness::Polite)
                .with_hashed_key("logs.export-status"),
        )
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_layout_dirty(&self) -> bool {
        self.layout_dirty || self.shell.is_layout_dirty()
    }

    fn layout_needs_measure(&self) -> bool {
        self.layout_dirty || self.shell.layout_needs_measure()
    }

    fn needs_idle_sync(&self) -> bool {
        let journal = self.journal.borrow();
        (journal.reads && journal.live_tail) || journal.reading.is_some()
    }

    /// Live tail reads the journal once a `POLL_INTERVAL`: a deadline,
    /// not a frame pump. A read and an export wake the app when they finish.
    fn next_wake_deadline(&self) -> Option<Instant> {
        let poll = self.journal.borrow().next_poll();
        poll.into_iter()
            .chain(self.shell.next_wake_deadline())
            .min()
    }

    fn needs_animation_frame(&self) -> bool {
        self.shell.needs_animation_frame()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::sample_entries;
    use crate::resource::LogsShell;
    use ui::prelude::{Invoke, TestApp};
    use ui::tilo_resource::Atom;

    fn captured(viewport: (u32, u32)) -> TestApp<LogsShell> {
        let mut app = TestApp::new().unwrap();
        surface(&mut app, |viewer| {
            viewer.reads = false;
            viewer.reading = None;
            viewer.source_loading = false;
            viewer.show_entries(sample_entries());
        });
        app.settle();
        app.harness().resize(viewport).unwrap();
        app.settle();
        app
    }
    /// A whole-journal read of `entries`, with no cursor to tail from.
    fn whole(entries: Vec<LogEntry>) -> Read {
        Read::Whole(Page {
            entries,
            cursor: None,
        })
    }
    /// The journal the window shows.
    fn surface<T>(app: &mut TestApp<LogsShell>, visit: impl FnOnce(&mut Journal) -> T) -> T {
        window(app, |window| visit(&mut window.journal.borrow_mut()))
    }
    fn window<T>(app: &mut TestApp<LogsShell>, visit: impl FnOnce(&mut LogsWindow) -> T) -> T {
        app.with_widget(visit)
            .expect("the journal window is mounted")
    }
    /// Make the filter edit a picker makes, through the window's door.
    fn edit(app: &mut TestApp<LogsShell>, edit: &ui::tilo_resource::Edit<Logs>) {
        app.invoke(ui::tilo_resource::Invoke::edit(edit).unwrap())
            .unwrap();
        app.settle();
    }

    fn request(app: &mut TestApp<LogsShell>, request: Request) {
        window(app, |window| {
            window.shared.borrow_mut().push(request);
            window.apply_requests();
        });
        app.settle();
    }
    fn named(app: &mut TestApp<LogsShell>, name: &str) -> bool {
        app.harness()
            .a11y_nodes()
            .iter()
            .any(|(_, node)| node.label() == Some(name))
    }
    fn finish_job(app: &mut TestApp<LogsShell>) {
        tilo_test_env::wait_until("the admitted export job to complete", || {
            app.settle();
            surface(app, |viewer| !viewer.exporting)
        });
    }

    #[test]
    fn refresh_republishes_changed_middle_records_and_facets() {
        let mut app = captured((1280, 800));
        let (mut entries, revision) = surface(&mut app, |viewer| {
            (viewer.entries.clone(), viewer.shared.borrow().rows_revision)
        });
        entries[1].message = "Updated middle record".into();
        entries[1].unit = "updated.service".into();
        entries[1].priority = Priority::Crit;
        surface(&mut app, |viewer| {
            viewer.show_read(Ok(whole(entries.clone())));
        });
        app.settle();
        surface(&mut app, |viewer| {
            let bridge = viewer.shared.borrow();
            assert_ne!(bridge.rows_revision, revision);
            assert_eq!(bridge.rows[1].message, "Updated middle record");
            assert_eq!(bridge.rows[1].unit, "updated");
            assert_eq!(bridge.rows[1].priority, Priority::Crit);
            assert!(bridge.units.contains(&"updated.service".to_owned()));
        });
        let revision = surface(&mut app, |viewer| viewer.shared.borrow().rows_revision);
        surface(&mut app, |viewer| viewer.show_read(Ok(whole(entries))));
        assert_eq!(
            surface(&mut app, |viewer| viewer.shared.borrow().rows_revision),
            revision,
            "an unchanged poll does not rebuild the table"
        );
    }

    /// Live tail reads only what was written after the newest record shown
    /// and adds it to the page; a quiet journal rebuilds and republishes
    /// nothing, and the page keeps its newest records.
    #[test]
    fn live_tail_adds_what_is_new_and_a_quiet_journal_changes_nothing() {
        let mut app = captured((1280, 800));
        let entries = surface(&mut app, |viewer| viewer.entries.clone());
        let newest = entries.last().unwrap().realtime_us;
        surface(&mut app, |viewer| {
            viewer.show_read(Ok(Read::Whole(Page {
                entries: entries.clone(),
                cursor: Some("s=x;i=1".into()),
            })));
            assert_eq!(viewer.tail_after().as_deref(), Some("s=x;i=1"));
        });
        app.settle();
        let revision = surface(&mut app, |viewer| viewer.shared.borrow().rows_revision);
        surface(&mut app, |viewer| {
            viewer.dirty = false;
            viewer.show_read(Ok(Read::Tail(Page::default())));
            assert!(!viewer.dirty, "a quiet journal asks for no frame");
            assert_eq!(viewer.tail_after().as_deref(), Some("s=x;i=1"));
        });
        app.settle();
        assert_eq!(
            surface(&mut app, |viewer| viewer.shared.borrow().rows_revision),
            revision,
            "a quiet journal rebuilds no rows"
        );

        let mut fresh = entries[0].clone();
        fresh.realtime_us = newest + 1;
        fresh.message = "Written just now".into();
        surface(&mut app, |viewer| {
            viewer.show_read(Ok(Read::Tail(Page {
                entries: vec![fresh.clone()],
                cursor: Some("s=x;i=2".into()),
            })));
            assert_eq!(viewer.entries.len(), entries.len() + 1);
            assert_eq!(viewer.entries.last(), Some(&fresh));
            assert_eq!(viewer.tail_after().as_deref(), Some("s=x;i=2"));
        });
        app.settle();
        surface(&mut app, |viewer| {
            assert_ne!(viewer.shared.borrow().rows_revision, revision);
        });

        // A full answer may have left records behind: the next read is the
        // newest page whole, and the window never holds more than a page.
        let burst: Vec<LogEntry> = (0..JOURNAL_LIMIT as u64)
            .map(|i| {
                let mut entry = fresh.clone();
                entry.realtime_us = newest + 2 + i;
                entry
            })
            .collect();
        surface(&mut app, |viewer| {
            viewer.show_read(Ok(Read::Tail(Page {
                entries: burst,
                cursor: Some("s=x;i=3".into()),
            })));
            assert_eq!(viewer.entries.len(), JOURNAL_LIMIT);
            assert_eq!(
                viewer.entries.first().map(|entry| entry.realtime_us),
                Some(newest + 2),
                "the oldest records leave the page"
            );
            assert_eq!(viewer.tail_after(), None);
        });

        // A failed read starts again from the whole page.
        surface(&mut app, |viewer| {
            viewer.tail_cursor = Some("s=x;i=4".into());
            viewer.show_read(Err(JournalLoadError::Failed));
            assert_eq!(viewer.tail_after(), None);
        });
    }

    #[test]
    fn refresh_keeps_selection_on_its_record_or_shows_no_selection() {
        let mut app = captured((1280, 800));
        let (mut entries, id) = surface(&mut app, |viewer| {
            (
                viewer.entries.clone(),
                actions::entry_resource_id(&viewer.entries[1]),
            )
        });
        app.effect(&JournalEffect::SelectEntry {
            entry_id: id.clone(),
        })
        .unwrap();
        entries.remove(0);
        surface(&mut app, |viewer| {
            viewer.show_read(Ok(whole(entries.clone())));
        });
        app.settle();
        surface(&mut app, |viewer| {
            assert_eq!(viewer.effective_selected(), Some(0));
            assert_eq!(viewer.shared.borrow().selected, Some(0));
        });
        assert_eq!(
            window(&mut app, |window| window.inspected.clone()),
            Inspected::Entry(id.clone())
        );
        entries.remove(0);
        surface(&mut app, |viewer| viewer.show_read(Ok(whole(entries))));
        app.settle();
        surface(&mut app, |viewer| {
            assert_eq!(viewer.selected_entry.as_deref(), Some(id.as_str()));
            assert_eq!(viewer.effective_selected(), None);
            assert_eq!(viewer.shared.borrow().selected, None);
        });
        assert_eq!(
            window(&mut app, |window| window.inspected.clone()),
            Inspected::Nothing
        );
    }

    #[test]
    fn live_tail_is_a_window_bar_toggle_that_follows_the_state() {
        fn live_tail() -> Option<bool> {
            ui::window_toolbar::declared()?
                .actions
                .into_iter()
                .find(|action| action.name == "Live tail")
                .map(|action| action.on)
        }
        let mut app = captured((1280, 800));
        app.settle();
        assert_eq!(live_tail(), Some(false));
        app.effect(&JournalEffect::SetLiveTail {
            enabled: Some(true),
        })
        .unwrap();
        app.settle();
        assert_eq!(live_tail(), Some(true));
        // Pressed in the bar, it stops following the journal.
        app.harness().present_in_window_bar();
        app.harness()
            .press_window_slot(ui::window_toolbar::ToolbarEvent::Action {
                id: "logs.live-tail".into(),
            });
        app.settle();
        assert_eq!(live_tail(), Some(false));
    }

    #[test]
    fn shared_filters_narrow_entries_and_write_the_document() {
        let mut app = captured((1280, 800));
        let initial_heads = app.read_document(|doc| doc.clone().get_heads());
        surface(&mut app, |viewer| {
            assert!(viewer.units.contains(&"shell.service".to_string()));
            let boots: Vec<&str> = viewer
                .boots
                .iter()
                .map(|(_, label)| label.as_str())
                .collect();
            assert_eq!(boots, ["Current boot", "Previous boot"]);
        });
        let total = surface(&mut app, |viewer| viewer.filtered.len());
        app.invoke(Invoke::edit(&Logs::paths().severity.set(Priority::Err)).unwrap())
            .unwrap();
        surface(&mut app, |viewer| {
            let errs = viewer.filtered.len();
            assert!(errs < total, "errors-only must show fewer than all");
            assert!(errs > 0);
            assert!(
                viewer
                    .filtered
                    .iter()
                    .all(|&i| viewer.entries[i].priority.level() <= 3)
            );
            assert_eq!(viewer.shared.borrow().rows.len(), errs);
            assert_eq!(viewer.shared.borrow().total, total);
        });
        assert_eq!(app.state().filter().min_priority, 3);
        app.invoke(Logs::CLEAR_FILTERS.with(())).unwrap();
        request(&mut app, Request::Search("iroh".to_string()));
        surface(&mut app, |viewer| {
            assert!(!viewer.filtered.is_empty());
            assert!(viewer.filtered.iter().all(|&i| {
                let entry = &viewer.entries[i];
                entry.message.to_lowercase().contains("iroh")
                    || entry.unit.to_lowercase().contains("iroh")
            }));
            assert_eq!(viewer.shared.borrow().filter.search, "iroh");
        });
        assert_eq!(app.state().filter().search, "iroh");
        app.invoke(Logs::CLEAR_FILTERS.with(())).unwrap();
        edit(&mut app, &Logs::paths().unit.set(Atom("kernel".to_owned())));
        let boot = surface(&mut app, |viewer| viewer.boots[0].0.clone());
        edit(&mut app, &Logs::paths().boot.set(Atom(boot.clone())));
        surface(&mut app, |viewer| {
            assert!(
                viewer
                    .filtered
                    .iter()
                    .all(|&i| viewer.entries[i].unit == "kernel"
                        && viewer.entries[i].boot_id == boot)
            );
        });
        let stored = app.state().filter();
        assert_eq!(stored.unit, "kernel");
        assert_eq!(stored.boot, boot);
        assert_ne!(
            app.read_document(|doc| doc.clone().get_heads()),
            initial_heads,
            "document filter actions must write shared state"
        );
    }

    #[test]
    fn history_says_how_the_filter_changed() {
        let mut app = captured((1280, 800));
        edit(&mut app, &Logs::paths().severity.set(Priority::Err));
        edit(&mut app, &Logs::paths().unit.set(Atom("kernel".to_owned())));
        request(&mut app, Request::Search("iroh".to_owned()));
        app.invoke(Logs::CLEAR_FILTERS.with(())).unwrap();
        let catalogue =
            tilo_history::Catalogue::new("en-US", &[include_str!("../locales/en-US.ftl")]);
        let reader = tilo_history::Reader::new("en-US").with_app(&catalogue);
        let sentences: Vec<String> = app
            .history()
            .into_iter()
            .filter(|entry| entry.what.is_some())
            .map(|entry| reader.sentence(&entry))
            .collect();
        assert_eq!(
            sentences,
            [
                "Showed errors and more severe entries",
                "Showed the entries of unit kernel",
                "Searched the journal",
                "Cleared the journal filters",
            ]
        );
    }

    #[test]
    fn selection_auto_lands_on_errors_and_moves_through_the_document() {
        let mut app = captured((1280, 800));
        let (sel, next) = surface(&mut app, |viewer| {
            let sel = viewer
                .effective_selected()
                .expect("an entry is auto-selected");
            assert!(viewer.entries[sel].priority.level() <= Priority::Err.level());
            let shown = viewer.shared.borrow().selected;
            assert_eq!(shown, viewer.filtered_pos(sel));
            let next = viewer.shared.borrow().rows[shown.unwrap() + 1].clone();
            (sel, next)
        });
        let before = app.read_document(|doc| doc.clone().get_heads());
        request(&mut app, Request::Select(next.entry_id.clone()));
        surface(&mut app, |viewer| {
            assert_ne!(sel, viewer.effective_selected().unwrap());
            assert_eq!(
                viewer.selected_entry.as_deref(),
                Some(next.entry_id.as_str())
            );
        });
        assert_eq!(
            window(&mut app, |window| window.inspected.clone()),
            Inspected::Entry(next.entry_id)
        );
        assert_eq!(
            app.read_document(|doc| doc.clone().get_heads()),
            before,
            "entry navigation is device-local; shared filters are unchanged"
        );
    }

    #[test]
    fn window_docks_the_inspector_on_desktop_and_draws_it_on_a_phone() {
        let mut desktop = captured((1280, 800));
        assert_eq!(
            window(&mut desktop, |window| window.shell.geometry().inspector),
            InspectorPlacement::Docked
        );
        assert!(
            named(&mut desktop, "Journal entries"),
            "the entries are exposed"
        );
        let bar: Vec<String> = ui::window_toolbar::declared()
            .map(|bar| bar.actions)
            .unwrap_or_default()
            .into_iter()
            .map(|action| action.name)
            .collect();
        for name in ["Refresh journal", "Live tail", "Export filtered logs"] {
            assert!(
                bar.iter().any(|action| action == name),
                "{name} is in the window bar: {bar:?}"
            );
        }
        let mut phone = captured((390, 844));
        window(&mut phone, |window| {
            assert_eq!(window.shell.geometry().inspector, InspectorPlacement::Sheet);
            assert!(!window.shell.is_inspector_open());
        });
        let entry = surface(&mut phone, |viewer| {
            viewer.shared.borrow().rows[0].entry_id.clone()
        });
        request(&mut phone, Request::Select(entry));
        assert!(window(&mut phone, |window| window
            .shell
            .is_inspector_open()));
    }

    #[test]
    fn portal_export_writes_the_frozen_snapshot_or_announces_failure() {
        let mut app = captured((1280, 800));
        surface(&mut app, |viewer| {
            viewer.show_entries(sample_entries().into_iter().take(2).collect());
        });
        let path = std::env::temp_dir().join(format!(
            "tilo-logs-portal-export-{}.jsonl",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (services, recorder) = ui::host::HostServices::recording();
        recorder.respond_to_pick(PortalOutcome::Opened(path.clone()));
        ui::host::scoped(&services, || {
            app.start_effect(&JournalEffect::ExportFilteredJsonl)
                .unwrap()
        });
        assert_eq!(
            recorder.pick_requests()[0].suggested_name(),
            Some("journal-filtered.jsonl")
        );
        surface(&mut app, |viewer| {
            viewer.entries.extend(sample_entries().into_iter().skip(2));
        });
        finish_job(&mut app);
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 2);
        assert_eq!(
            surface(&mut app, |viewer| viewer.export_status.clone()).as_deref(),
            Some("Exported 2 log entries.")
        );
        std::fs::remove_file(path).unwrap();

        let mut app = captured((1280, 800));
        surface(&mut app, |viewer| {
            viewer.show_entries(sample_entries().into_iter().take(1).collect());
        });
        let (services, recorder) = ui::host::HostServices::recording();
        recorder.respond_to_pick(PortalOutcome::Opened(std::env::temp_dir()));
        ui::host::scoped(&services, || {
            app.start_effect(&JournalEffect::ExportFilteredJsonl)
                .unwrap()
        });
        finish_job(&mut app);
        assert_eq!(
            surface(&mut app, |viewer| viewer.export_status.clone()).as_deref(),
            Some("Export failed.")
        );
        let node = window(&mut app, |window| {
            window.accessibility().expect("the outcome is announced")
        });
        assert_eq!(node.name.as_deref(), Some("Export failed."));
        assert_eq!(node.live, Some(ui::a11y::Politeness::Polite));
    }

    #[test]
    fn journal_commands_and_empty_states_act_through_production_paths() {
        let mut app = captured((1280, 800));
        let invocation = app
            .start_effect(&JournalEffect::Refresh)
            .expect("the canonical refresh effect admits a journal read job");
        assert!(
            surface(&mut app, |viewer| viewer.source_loading),
            "admitted journal read {invocation:?} presents its pending state"
        );
        app.settle();
        let mut app = captured((1280, 800));
        request(
            &mut app,
            Request::Search("no such message anywhere".to_string()),
        );
        assert_eq!(
            surface(&mut app, |viewer| viewer.placeholder()),
            Some(Placeholder::NoMatches)
        );
        request(&mut app, Request::ClearFilters);
        surface(&mut app, |viewer| {
            assert!(viewer.filter.is_clear());
            assert_eq!(viewer.placeholder(), None);
        });
        let mut unavailable = TestApp::<LogsShell>::capture(Some("error")).unwrap();
        surface(&mut unavailable, |viewer| {
            assert_eq!(
                viewer.placeholder(),
                Some(Placeholder::Unavailable(
                    crate::journal::JournalLoadError::AccessDenied
                ))
            );
            assert!(!viewer.shared.borrow().has_entries, "no filters to offer");
        });
        assert!(
            !window(&mut unavailable, |window| window.has_inspector),
            "the detail pane collapses"
        );
        let before = app.agent_surface();
        assert_eq!(
            effect(&before, "set-live-tail").unwrap()["label"],
            "Start live tail"
        );
        app.effect(&JournalEffect::SetLiveTail {
            enabled: Some(true),
        })
        .unwrap();
        app.settle();
        assert_eq!(
            effect(&app.agent_surface(), "set-live-tail").unwrap()["label"],
            "Stop live tail"
        );
        assert!(
            app.effect(&JournalEffect::SelectEntry {
                entry_id: "logs.entry.none".into()
            })
            .is_err()
        );
    }
    fn effect(surface: &serde_json::Value, id: &str) -> Option<serde_json::Value> {
        surface
            .as_array()?
            .iter()
            .find(|action| action["action"] == id)
            .cloned()
    }
}
