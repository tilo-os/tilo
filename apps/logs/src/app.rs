//! `LogsApp` — the root widget for the Logs viewer.
//!
//! A graphical systemd-journal viewer, composed from the toolkit: one
//! [`NavShell`] whose content is the journal table with its filters
//! ([`JournalPane`]) and whose inspector shows every field of the selected
//! entry. The content header holds the journal's commands: refresh, live
//! tail and export.
//!
//! `LogsApp` owns the journal (an external, read-only render cache) and the
//! shared view state: filters and the selected entry sync as document
//! actions on [`crate::resource::Logs`]. Live tail is a local Effect that
//! controls this process's journal poller.

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
    Dispatcher, EffectCx, EffectStatus, Effected, LiveRow, RunMode, Submit, SurfaceCx,
};
use ui::icons::Icon;
use ui::portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tilo_actions::ControlInvocation;
use ui::tilo_resource::Atom;
use ui::tokens::Spacing;
use ui::widget::{
    Button, Element, FilterCount, InspectorPlacement, NavShell, SearchField, TaffyTreeExt, Widget,
};

use crate::actions::{self, JournalEffect};
use crate::journal::JournalLoadError;
use crate::model::{Filter, LogEntry, Priority};
use crate::resource::{JournalFixture, Logs};
use crate::view::{Bridge, JournalPane, JournalRow, Placeholder, Request, Shared};

const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// The most recent journal records read.
const JOURNAL_LIMIT: usize = 2000;

/// One read of the journal.
type JournalRead = Result<Vec<LogEntry>, JournalLoadError>;

/// An `Instant` at least `POLL_INTERVAL` in the past, so writing it into
/// `last_poll` makes the next poll-due check fire immediately. Falls back to
/// `now` itself in the vanishingly rare case the process's monotonic clock
/// has less than `POLL_INTERVAL` of history — that only delays the forced
/// poll by at most `POLL_INTERVAL`, never panics.
fn force_immediate_poll_at() -> Instant {
    let now = ui::time::now();
    now.checked_sub(POLL_INTERVAL).unwrap_or(now)
}

/// The content header's commands: refresh, live tail and export. Live tail
/// is a toggle glyph that stands on the selection lift while the journal is
/// followed.
fn header_actions(actions: Option<&ui::framework::Actions<Logs>>, live_tail: bool) -> Vec<Element> {
    use ui::framework::ActionControl;
    let controls = [
        (
            Button::icon_only("logs.refresh", Icon::Refresh, "Refresh journal"),
            JournalEffect::Refresh,
        ),
        (
            Button::icon_only("logs.live-tail", Icon::Play, "Live tail").toggled(live_tail),
            JournalEffect::SetLiveTail { enabled: None },
        ),
        (
            Button::icon_only("logs.export", Icon::Download, "Export filtered logs"),
            JournalEffect::ExportFilteredJsonl,
        ),
    ];
    controls
        .into_iter()
        .map(|(button, effect)| {
            Box::new(match actions {
                Some(actions) => actions.bind_effect_invocation(button, &effect),
                None => button.unavailable(),
            }) as Element
        })
        .collect()
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

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent flag (live tail, loading, live access, \
              inspector presence, a capture's reveal, layout dirtiness)"
)]
pub struct LogsApp {
    /// Full journal — external read-only data, not CRDT state.
    entries: Vec<LogEntry>,
    /// Distinct units present, sorted (for the unit filter).
    units: Vec<String>,
    /// Distinct boots: (`boot_id`, label), newest first.
    boots: Vec<(String, String)>,

    // ---- caches, refreshed from the doc on sync ----
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
    /// Whether an export's Save dialog is open. Neither the chosen path nor
    /// log content is published through resources or collaboration state.
    exporting: bool,
    /// Concise local outcome for the portal write, announced through an a11y
    /// live region. It never includes a chosen path or log contents.
    export_status: Option<String>,

    // ---- the window ----
    rect: Rect,
    shell: NavShell,
    shared: Shared,
    /// The live-tail state the header's toggle shows.
    shown_live_tail: bool,
    /// The spacing the inspector was laid out with.
    spacing: Spacing,
    /// The fingerprints of the state and the rows last handed to the views.
    published: Option<u64>,
    published_rows: Option<u64>,
    inspected: Inspected,
    has_inspector: bool,
    /// Open the inspector drawer once laid out (`--detail` captures).
    reveal_detail: bool,
    layout_dirty: bool,
    /// Pointer/keyboard controls publish inert resource addresses; the host
    /// re-resolves current availability and permissions before effects run.
    pending_control_invocation: Option<ControlInvocation>,

    // ---- journal source ----
    /// Whether the journal controls are offered: everywhere but a past
    /// version.
    allow_live: bool,
    /// Whether this window reads the journal: a live window, never a
    /// capture or a past version.
    reads: bool,
    last_poll: Instant,
    /// Wakes the window when a read finishes.
    waker: Option<ui::WakeHandle>,

    /// Where filter edits and journal effects go.
    dispatcher: Option<Dispatcher<Logs>>,
    chrome_actions: Option<ui::framework::Actions<Logs>>,
    dirty: bool,
}

impl Default for LogsApp {
    fn default() -> Self {
        Self::new()
    }
}

impl LogsApp {
    /// `allow_live` is true only for a viewer that may read the journal;
    /// it gates the refresh, live tail and export Effects.
    pub fn with_entries(entries: Vec<LogEntry>, allow_live: bool) -> Self {
        let shared: Shared = Rc::new(RefCell::new(Bridge::default()));
        let shell = NavShell::new()
            .with_id("logs.shell")
            .content(Box::new(JournalPane::new(Rc::clone(&shared))))
            .with_filter(
                SearchField::filter()
                    .with_id("logs.filter")
                    .with_placeholder("Filter messages and units"),
            )
            .content_header("", header_actions(None, false))
            .inspector(crate::view::entry_detail(None, &Spacing::default()));
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
            reading: None,
            exporting: false,
            export_status: None,
            rect: Rect::ZERO,
            shell,
            shared,
            shown_live_tail: false,
            spacing: Spacing::default(),
            published: None,
            published_rows: None,
            inspected: Inspected::Stale,
            has_inspector: true,
            reveal_detail: false,
            layout_dirty: true,
            pending_control_invocation: None,
            allow_live,
            last_poll: ui::time::now(),
            reads: false,
            waker: None,
            dispatcher: None,
            chrome_actions: None,
            dirty: true,
        };
        app.rebuild_facets();
        app.recompute_filtered();
        app.publish();
        app
    }

    /// Show the selected entry for a capture: on a narrow window the
    /// inspector is a drawer, which this opens once the window is laid out.
    pub fn shots_show_detail(&mut self) {
        self.reveal_detail = true;
        self.layout_dirty = true;
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
        }
        if let Some(field) = self.shell.filter_mut() {
            if field.value() != self.filter.search {
                field.set_query(&self.filter.search);
            }
            // "12 of 96" while any filter narrows the journal.
            field.set_count(
                (!self.filter.is_clear())
                    .then(|| FilterCount::new(self.filtered.len(), self.entries.len())),
            );
        }
        if self.shown_live_tail != self.live_tail {
            self.shown_live_tail = self.live_tail;
            self.shell.set_content_header(
                "",
                header_actions(self.chrome_actions.as_ref(), self.live_tail),
            );
        }
        self.show_inspector(selected);
        self.layout_dirty = true;
    }

    /// The inspector follows the selection; when the journal lists nothing
    /// it collapses, leaving the one empty state.
    fn show_inspector(&mut self, selected: Option<usize>) {
        let wanted = !self.filtered.is_empty();
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
        let entry = selected.and_then(|abs| self.entries.get(abs));
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

    /// Turn the views' requests into document actions.
    fn apply_requests(&self) {
        let requests = self.shared.borrow_mut().take_requests();
        for request in requests {
            match request {
                Request::Search(value) => self.edit(Logs::paths().search.replace(value)),
                Request::Unit(value) => self.edit(Logs::paths().unit.set(Atom(value))),
                Request::Boot(value) => self.edit(Logs::paths().boot.set(Atom(value))),
                Request::Priority(value) => {
                    self.edit(Logs::paths().severity.set(Priority::from_level(value)));
                }
                Request::ClearFilters => self.edit(Logs::CLEAR_FILTERS.with(())),
                Request::Select(entry_id) => self.request(&JournalEffect::SelectEntry { entry_id }),
            }
        }
    }

    /// Open the inspector where it is a closed drawer, so choosing an entry
    /// on a narrow window shows it.
    fn reveal_inspector(&mut self) {
        if self.shell.geometry().inspector == InspectorPlacement::Drawer
            && !self.shell.is_inspector_open()
        {
            self.shell.toggle_inspector();
        }
    }

    pub(crate) fn attach_framework(&mut self, cx: &SurfaceCx<Logs>) {
        self.dispatcher = Some(cx.dispatcher());
        self.chrome_actions = Some(cx.actions());
        self.shared.borrow_mut().refresh = cx.actions().effect(&JournalEffect::Refresh);
        self.shell.set_content_header(
            "",
            header_actions(self.chrome_actions.as_ref(), self.live_tail),
        );
        match cx.mode() {
            RunMode::Live => {
                self.allow_live = true;
                self.reads = true;
                self.waker = Some(cx.waker());
                self.start_read();
            }
            RunMode::Capture => {
                self.allow_live = true;
                match cx.fixture::<JournalFixture>().as_deref() {
                    Some(JournalFixture::Entries(entries)) => self.show_entries(entries.clone()),
                    Some(JournalFixture::Failed(error)) => self.source_error = Some(*error),
                    Some(JournalFixture::Loading) => self.source_loading = true,
                    None => {}
                }
            }
            RunMode::Past => {}
        }
    }

    /// Show the presented filter.
    pub(crate) fn adopt_filter(&mut self, filter: Filter) {
        self.filter = filter;
        self.recompute_filtered();
        self.publish();
        self.dirty = true;
    }

    /// Submit a filter edit; the document presents its result.
    fn edit(&self, edit: impl Submit<Logs>) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.submit(edit);
        }
    }

    /// Invoke a journal effect through the same door as the palette and
    /// agents, which checks its availability first.
    fn request(&self, effect: &JournalEffect) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.effect(effect);
        }
    }

    /// Read the journal off the UI thread, unless a read is in flight or
    /// this window does not read it.
    fn start_read(&mut self) {
        if !self.reads || self.reading.is_some() {
            return;
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let waker = self.waker.clone();
        drop(ui::rt::app().spawn_blocking(move || {
            let _ = sender.send(crate::journal::load_live(JOURNAL_LIMIT));
            if let Some(waker) = waker {
                waker.wake();
            }
        }));
        self.reading = Some(receiver);
        if self.entries.is_empty() {
            self.source_loading = true;
        }
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

    /// Answer a journal effect, from a control, the palette or an agent.
    pub(crate) fn answer_effect(
        &mut self,
        effect: JournalEffect,
        cx: &mut EffectCx<Logs, Self>,
    ) -> Result<Effected, String> {
        self.dirty = true;
        match effect {
            JournalEffect::Refresh => {
                self.source_loading = true;
                Ok(cx.thread(
                    |_| crate::journal::load_live(JOURNAL_LIMIT),
                    |app: &mut Self, read: JournalRead, _| {
                        let failed = read.as_ref().err().copied();
                        app.show_read(read);
                        match failed {
                            None => Ok(Effected::value(app.entries.len())),
                            Some(error) => Err(error.message().to_owned()),
                        }
                    },
                ))
            }
            JournalEffect::SetLiveTail { enabled } => {
                self.live_tail = enabled.unwrap_or(!self.live_tail);
                self.last_poll = force_immediate_poll_at();
                Ok(Effected::done())
            }
            JournalEffect::ExportFilteredJsonl => {
                let entries = self.export_snapshot()?;
                let picked = export_request().send();
                self.exporting = true;
                Ok(cx.thread(
                    move |_| export_picked(&picked, &entries),
                    |app: &mut Self, status: Option<String>, _| {
                        app.exporting = false;
                        app.export_status.clone_from(&status);
                        app.dirty = true;
                        Ok(Effected::value(status))
                    },
                ))
            }
            JournalEffect::SelectEntry { entry_id } => {
                let abs = self
                    .entries
                    .iter()
                    .position(|entry| actions::entry_resource_id(entry) == entry_id)
                    .ok_or_else(|| "this device's journal has no such entry".to_owned())?;
                self.sel_raw = i64::try_from(abs).unwrap_or(i64::MAX);
                self.selected_entry = Some(entry_id);
                self.publish();
                self.reveal_inspector();
                // Showing an entry is navigation: reveal the detail slab on a
                // narrow window, as a pointer selection does.

                Ok(Effected::done())
            }
        }
    }

    /// How the journal effects differ from their declarations now.
    pub(crate) fn effect_statuses(&self) -> Vec<EffectStatus> {
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
    pub(crate) fn entry_rows(&self) -> Vec<LiveRow> {
        actions::entry_rows(&self.entries, &self.filter)
    }

    /// Any entry of this device's journal, by id.
    pub(crate) fn entry_row(&self, id: &str) -> Option<LiveRow> {
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

    /// Queue a root effect through the published resource tree, so
    /// accessibility, agents, history and policy see the same effect a
    /// keyboard shortcut starts.
    fn request_journal_operation(&self, action: &str) {
        let effect = match action {
            "refresh" => JournalEffect::Refresh,
            "set-live-tail" => JournalEffect::SetLiveTail { enabled: None },
            "export-filtered-jsonl" => JournalEffect::ExportFilteredJsonl,
            _ => return,
        };
        self.request(&effect);
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
impl Widget for LogsApp {
    ui::provides!(dyn ui::widget::controls::ActionControls);

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.drain_read();
        if self.reads && self.live_tail && self.last_poll.elapsed() >= POLL_INTERVAL {
            self.last_poll = ui::time::now();
            self.start_read();
        }
        self.publish();
        self.shell.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.shell.update(state, path)
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
            self.show_inspector(self.effective_selected());
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
        if self.reveal_detail && self.shell.geometry().inspector != InspectorPlacement::None {
            self.reveal_detail = false;
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

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        permissions: &ui::permissions::PermissionContext,
    ) {
        self.shell
            .draw_with_presence(scene, transform, tr, theme, presence, permissions);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // Ctrl+L follows or pauses the journal, like a terminal's `-f`.
        if let WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == ElementState::Pressed
            && ctx.modifiers.state().control_key()
            && matches!(&key.logical_key, Key::Character(c) if c.eq_ignore_ascii_case("l"))
        {
            if self.allow_live {
                self.request_journal_operation("set-live-tail");
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
        "LogsApp"
    }

    fn test_id(&self) -> Option<&str> {
        Some("logs_app")
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        let status = self.export_status.as_ref()?;
        Some(
            ui::a11y::AccessNode::new(ui::a11y::Role::StaticText)
                .with_name(status)
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
        (self.reads && self.live_tail) || self.reading.is_some()
    }

    fn needs_animation_frame(&self) -> bool {
        (self.allow_live && self.live_tail) || self.exporting || self.shell.needs_animation_frame()
    }
}

impl ui::widget::controls::ActionControls for LogsApp {
    fn take_control_invocation(&mut self) -> Option<ui::tilo_actions::ControlInvocation> {
        self.pending_control_invocation
            .take()
            .or_else(|| ui::widget::controls::take_control_invocation(&mut self.shell))
    }
}

impl LogsApp {
    pub fn new() -> Self {
        Self::with_entries(Vec::new(), false)
    }
    fn show_entries(&mut self, entries: Vec<LogEntry>) {
        self.entries = entries;
        self.source_error = None;
        self.rebuild_facets();
        self.recompute_filtered();
        self.publish();
        self.dirty = true;
    }
    fn show_read(&mut self, read: JournalRead) {
        self.source_loading = false;
        match read {
            Ok(entries) => self.show_entries(entries),
            Err(error) => {
                self.source_error = Some(error);
                self.publish();
                self.dirty = true;
            }
        }
    }
    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        if scenario == Some("detail") {
            self.shots_show_detail();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::sample_entries;
    use crate::resource::LogsShell;
    use ui::prelude::{Invoke, TestApp};

    fn captured(viewport: (u32, u32)) -> TestApp<LogsShell> {
        let mut app = TestApp::new().unwrap();
        surface(&mut app, |viewer| {
            // Exercise live effect admission with a deterministic local journal.
            viewer.reads = false;
            viewer.reading = None;
            viewer.source_loading = false;
            viewer.show_entries(sample_entries());
        });
        app.harness().resize(viewport).unwrap();
        app.settle();
        app
    }
    fn surface<T>(app: &mut TestApp<LogsShell>, visit: impl FnOnce(&mut LogsApp) -> T) -> T {
        app.with_surface(visit)
            .expect("the journal surface is mounted")
    }
    fn request(app: &mut TestApp<LogsShell>, request: Request) {
        surface(app, |viewer| {
            viewer.shared.borrow_mut().push(request);
            viewer.apply_requests();
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
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            app.settle();
            if surface(app, |viewer| !viewer.exporting) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the admitted export job completes"
            );
            std::thread::park_timeout(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn live_tail_is_a_header_toggle_that_follows_the_state() {
        fn live_tail(app: &mut TestApp<LogsShell>) -> Option<ui::accesskit::Toggled> {
            app.harness()
                .a11y_nodes()
                .into_iter()
                .find(|(_, node)| node.label() == Some("Live tail"))
                .and_then(|(_, node)| node.toggled())
        }
        let mut app = captured((1280, 800));
        assert_eq!(live_tail(&mut app), Some(ui::accesskit::Toggled::False));
        app.effect(&JournalEffect::SetLiveTail {
            enabled: Some(true),
        })
        .unwrap();
        app.settle();
        assert_eq!(live_tail(&mut app), Some(ui::accesskit::Toggled::True));
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
        request(&mut app, Request::Unit("kernel".to_string()));
        let boot = surface(&mut app, |viewer| viewer.boots[0].0.clone());
        request(&mut app, Request::Boot(boot.clone()));
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
        request(&mut app, Request::Priority(Priority::Err.level()));
        request(&mut app, Request::Unit("kernel".to_owned()));
        request(&mut app, Request::Search("iroh".to_owned()));
        app.invoke(Logs::CLEAR_FILTERS.with(())).unwrap();
        let catalogue =
            tilo_history::Catalogue::new("en-US", &[include_str!("../locales/en-US.ftl")]);
        let reader = tilo_history::Reader::new("en-US").with_app(&catalogue);
        let sentences: Vec<String> = app
            .versions()
            .into_iter()
            .filter_map(|version| version.context)
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
            assert_eq!(viewer.inspected, Inspected::Entry(next.entry_id));
        });
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
            surface(&mut desktop, |viewer| viewer.shell.geometry().inspector),
            InspectorPlacement::Docked
        );
        for name in [
            "Journal entries",
            "Refresh journal",
            "Live tail",
            "Export filtered logs",
        ] {
            assert!(named(&mut desktop, name), "{name} is exposed");
        }
        let mut phone = captured((390, 844));
        surface(&mut phone, |viewer| {
            assert_eq!(
                viewer.shell.geometry().inspector,
                InspectorPlacement::Drawer
            );
            assert!(!viewer.shell.is_inspector_open());
        });
        let entry = surface(&mut phone, |viewer| {
            viewer.shared.borrow().rows[0].entry_id.clone()
        });
        request(&mut phone, Request::Select(entry));
        assert!(surface(&mut phone, |viewer| viewer
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
        let node = surface(&mut app, |viewer| {
            viewer.accessibility().expect("the outcome is announced")
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
            assert!(!viewer.has_inspector, "the detail pane collapses");
            assert!(!viewer.shared.borrow().has_entries, "no filters to offer");
        });
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
