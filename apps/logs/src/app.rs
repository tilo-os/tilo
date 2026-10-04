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

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::Key;

use ui::icons::Icon;
use ui::portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tilo_actions::{ActionAddress, ActionBinding, ControlInvocation, OperationKind, Resource};
use ui::tokens::Spacing;
use ui::widget::{Button, Element, InspectorPlacement, NavShell, TaffyTreeExt, Widget};

use crate::actions::{self, LogsOperation};
use crate::model::{Filter, LogEntry, Priority};
use crate::resource::LogsAction;
use crate::view::{Bridge, JournalPane, JournalRow, Placeholder, Request, Shared};

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

/// The journal's door with the Effects its controls bind to.
fn journal_door(live_tail: bool, allow_live: bool) -> Resource {
    actions::journal_effect_specs(live_tail, allow_live)
        .into_iter()
        .fold(
            Resource::new(
                actions::JOURNAL_RESOURCE_ID,
                "logs.journal",
                "System journal",
            ),
            Resource::with_action,
        )
}

/// The content header's commands: refresh, live tail and export. Live tail
/// is a toggle glyph that stands on the selection lift while the journal is
/// followed.
fn header_actions(door: &Resource, live_tail: bool) -> Vec<Element> {
    let mut actions: Vec<Element> = Vec::new();
    if let Ok(binding) = ActionBinding::unit_for(door, "refresh") {
        actions.push(Box::new(
            Button::icon_only("logs.refresh", Icon::Refresh, "Refresh journal").on_action(binding),
        ));
    }
    let live = Button::icon_only("logs.live-tail", Icon::Play, "Live tail").toggled(live_tail);
    actions.push(Box::new(
        match ActionBinding::unit_for(door, "set-live-tail") {
            Ok(binding) => live.on_action(binding),
            Err(_) => live,
        },
    ));
    if let Ok(binding) = ActionBinding::unit_for(door, "export-filtered-jsonl") {
        actions.push(Box::new(
            Button::icon_only("logs.export", Icon::Download, "Export filtered logs")
                .on_action(binding),
        ));
    }
    actions
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
    /// local journal cache without becoming a collaborator-visible cursor.
    selected_entry: Option<String>,
    live_tail: bool,
    /// A safe classification of the most recent local journal read failure.
    /// Never holds `journalctl` stderr or raw journal content.
    source_error: Option<crate::journal::JournalLoadError>,
    /// Set while a resource-routed refresh is being admitted or completed.
    /// Existing rows stay visible; an empty source shows a loading state
    /// rather than masquerading as empty.
    source_loading: bool,
    /// Host Save portal receiver plus the immutable filtered snapshot that
    /// was current when export was requested. Neither the chosen path nor log
    /// content is published through resources or collaboration state.
    export_rx: Option<PortalChannel>,
    pending_export: Option<Vec<LogEntry>>,
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
    /// One-shot deterministic local view seeding used only by the offscreen
    /// shot factory.
    shot_filter: Option<Filter>,

    // ---- live-tail plumbing (never runs under --shots) ----
    allow_live: bool,
    last_poll: Instant,

    /// Document actions: local host standalone; framework dispatcher when hosted.
    actions: crate::resource::ActionSink,
}

impl LogsApp {
    /// `allow_live` is true only for a viewer that may read the journal;
    /// it gates the refresh, live tail and export Effects.
    pub fn new(entries: Vec<LogEntry>, allow_live: bool) -> Self {
        let shared: Shared = Rc::new(RefCell::new(Bridge::default()));
        let door = journal_door(false, allow_live);
        if let Ok(refresh) = ActionBinding::unit_for(&door, "refresh") {
            shared.borrow_mut().refresh = Some(refresh);
        }
        let shell = NavShell::new()
            .with_id("logs.shell")
            .content(Box::new(JournalPane::new(Rc::clone(&shared))))
            .content_header("", header_actions(&door, false))
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
            export_rx: None,
            pending_export: None,
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
            shot_filter: None,
            allow_live,
            last_poll: ui::time::now(),
            actions: crate::resource::ActionSink::default(),
        };
        app.rebuild_facets();
        app.recompute_filtered();
        app.publish();
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
        app.publish();
        app
    }

    /// Show the selected entry for a capture: on a narrow window the
    /// inspector is a drawer, which this opens once the window is laid out.
    pub fn shots_show_detail(&mut self) {
        self.reveal_detail = true;
        self.layout_dirty = true;
    }

    /// Seed an errors-and-worse state for deterministic visual regression
    /// captures.
    pub fn shots_filter_errors(&mut self) {
        self.shot_filter = Some(Filter {
            min_priority: Priority::Err.level(),
            ..Filter::default()
        });
        self.layout_dirty = true;
    }

    /// Seed a free-text filter for a capture (`--no-matches` captures the
    /// state where nothing matches).
    pub fn shots_search(&mut self, query: &str) {
        self.shot_filter = Some(Filter {
            search: query.to_owned(),
            ..self.shot_filter.take().unwrap_or_default()
        });
        self.layout_dirty = true;
    }

    /// Deterministically capture the empty-source loading state.
    pub fn shots_show_loading(&mut self) {
        self.source_loading = true;
        self.publish();
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
        if self.shown_live_tail != self.live_tail {
            self.shown_live_tail = self.live_tail;
            let door = journal_door(self.live_tail, self.allow_live);
            let shell = std::mem::take(&mut self.shell);
            self.shell = shell.content_header("", header_actions(&door, self.live_tail));
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
        let shell = std::mem::take(&mut self.shell);
        self.shell = shell.inspector_header(title, Vec::new());
    }

    /// Turn the views' requests into document actions.
    fn apply_requests(&mut self, doc: &mut AutoCommit) {
        let requests = self.shared.borrow_mut().take_requests();
        for request in requests {
            let action = match request {
                Request::Search(value) => LogsAction::SetSearch { value },
                Request::Unit(value) => LogsAction::SetUnit { value },
                Request::Boot(value) => LogsAction::SetBoot { value },
                Request::Priority(value) => LogsAction::SetPriority { value },
                Request::ClearFilters => LogsAction::ClearFilters,
                Request::Select(entry_id) => {
                    self.reveal_inspector();
                    LogsAction::SelectEntry { entry_id }
                }
            };
            self.commit_view(doc, &action);
        }
        self.publish();
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
        self.selected_entry.clone_from(&state.selected_entry);
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
        self.publish();
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
            LogsAction::SetUnit { value } => self.filter.unit.clone_from(value),
            LogsAction::SetBoot { value } => self.filter.boot.clone_from(value),
            LogsAction::SetSearch { value } => self.filter.search.clone_from(value),
            LogsAction::ClearFilters => self.filter = Filter::default(),
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
        self.publish();
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
                self.reveal_inspector();
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
                self.recompute_filtered();
            }
            LogsOperation::SetLiveTail { enabled } => {
                self.live_tail = enabled;
                self.last_poll = force_immediate_poll_at();
            }
            LogsOperation::ExportFiltered => self.request_export()?,
        }
        self.publish();
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
            }
            Ok(PortalOutcome::Cancelled) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.export_rx = None;
                self.pending_export = None;
            }
            Ok(PortalOutcome::Failed(_)) => {
                self.export_rx = None;
                self.pending_export = None;
                self.export_status = Some("Export unavailable.".to_string());
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
    }

    /// Queue a root effect through the published resource tree, so
    /// accessibility, agents, history and policy see the same effect a
    /// keyboard shortcut starts.
    fn request_journal_operation(&mut self, action: &str) {
        if action == "refresh" {
            self.source_loading = true;
        }
        self.pending_control_invocation = Some(ControlInvocation {
            address: ActionAddress::new(actions::JOURNAL_RESOURCE_ID, action),
            arguments: serde_json::Map::new(),
        });
    }

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
        self.perform_effect(operation, doc)?;
        Ok(true)
    }
}

/// The shell's node: the one child of the app's node.
fn shell_node(taffy: &TaffyTree<()>, node: NodeId) -> Option<NodeId> {
    taffy.children(node).ok()?.first().copied()
}

/// Build a single-child parent node that fills the window.
fn fill_node(taffy: &mut TaffyTree<()>, child: NodeId) -> NodeId {
    taffy.parent_node(
        Style {
            display: taffy::Display::Flex,
            flex_grow: 1.0,
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        },
        &[child],
    )
}

impl Widget for LogsApp {
    ui::provides!(
        dyn ui::history_mode::capability::DocumentHistory,
        dyn ui::widget::controls::ActionControls,
        dyn ui::resources::PublishesResources
    );

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

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        {
            let (doc, _) = ui::doc::document(state, path);
            self.drain_export_portal();
            if let Some(filter) = self.shot_filter.take() {
                self.filter = filter;
            }

            // Optional live tail: throttled re-poll, never under --shots.
            if self.allow_live && self.live_tail && self.last_poll.elapsed() >= POLL_INTERVAL {
                self.last_poll = ui::time::now();
                match crate::journal::load_live(2000) {
                    Ok(fresh) => {
                        self.entries = fresh;
                        self.source_error = None;
                        self.rebuild_facets();
                    }
                    Err(error) => self.source_error = Some(error),
                }
            }
            self.recompute_filtered();
            self.apply_requests(doc);
        }
        self.shell.sync(state, path);
    }

    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
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
        let node = fill_node(taffy, shell);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
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
        let (doc, _) = ui::doc::document(ctx.state, ctx.path);
        self.apply_requests(doc);
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
        (self.allow_live && self.live_tail) || self.export_rx.is_some()
    }

    fn needs_animation_frame(&self) -> bool {
        (self.allow_live && self.live_tail)
            || self.export_rx.is_some()
            || self.shell.needs_animation_frame()
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
        if self.is_hosted() && id == crate::actions::JOURNAL_RESOURCE_ID {
            return None;
        }
        find(&self.publish_resources(doc), id)
    }
}

impl ui::widget::controls::ActionControls for LogsApp {
    fn take_control_invocation(&mut self) -> Option<ui::tilo_actions::ControlInvocation> {
        self.pending_control_invocation
            .take()
            .or_else(|| ui::widget::controls::take_control_invocation(&mut self.shell))
    }
}

impl ui::history_mode::capability::DocumentHistory for LogsApp {
    fn document_history_definition(&self) -> Option<&'static ui::history_mode::HistoryDefinition> {
        Some(&crate::framework_history::HISTORY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::sample_entries;
    use automerge::{ROOT, ReadDoc};

    fn synced_app() -> (LogsApp, AutoCommit) {
        let mut app = LogsApp::new(sample_entries(), false);
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        (app, doc)
    }

    fn harness(app: LogsApp, viewport: (u32, u32)) -> ui::headless::FrameHarness {
        let mut harness =
            ui::headless::FrameHarness::for_root(ui::State::new(), app, viewport, Theme::dark());
        harness.send(&WindowEvent::RedrawRequested);
        harness
    }

    fn named(harness: &ui::headless::FrameHarness, name: &str) -> bool {
        harness
            .a11y_nodes()
            .iter()
            .any(|(_, node)| node.label() == Some(name))
    }

    /// Live tail is a toggle glyph in the header that reads as on while the
    /// journal is followed.
    #[test]
    fn live_tail_is_a_header_toggle_that_follows_the_state() {
        fn live_tail(harness: &ui::headless::FrameHarness) -> Option<ui::accesskit::Toggled> {
            harness
                .a11y_nodes()
                .into_iter()
                .find(|(_, node)| node.label() == Some("Live tail"))
                .and_then(|(_, node)| node.toggled())
        }
        let mut window = harness(LogsApp::new(sample_entries(), false), (1280, 800));
        assert_eq!(live_tail(&window), Some(ui::accesskit::Toggled::False));

        let mut doc = AutoCommit::new();
        window
            .root_mut::<LogsApp>()
            .perform_effect(LogsOperation::SetLiveTail { enabled: true }, &mut doc)
            .unwrap();
        window.send(&WindowEvent::RedrawRequested);
        assert_eq!(live_tail(&window), Some(ui::accesskit::Toggled::True));
    }

    /// Facets and shared filter actions narrow the entry list and write the
    /// collaborative view document.
    #[test]
    fn shared_filters_narrow_entries_and_write_the_document() {
        // Facets collect units and two boots, newest first.
        let app = LogsApp::new(sample_entries(), false);
        assert!(app.units.contains(&"shell.service".to_string()));
        let boots: Vec<&str> = app.boots.iter().map(|(_, label)| label.as_str()).collect();
        assert_eq!(boots, ["Current boot", "Previous boot"]);

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
            assert_eq!(app.shared.borrow().rows.len(), errs);
            assert_eq!(app.shared.borrow().total, total);
        }
        // Search narrows results, and the field's text follows the document.
        {
            let (mut app, mut doc) = synced_app();
            app.shared
                .borrow_mut()
                .push(Request::Search("iroh".to_string()));
            app.apply_requests(&mut doc);
            assert!(!app.filtered.is_empty());
            for &i in &app.filtered {
                let e = &app.entries[i];
                let hit = e.message.to_lowercase().contains("iroh")
                    || e.unit.to_lowercase().contains("iroh");
                assert!(hit);
            }
            assert_eq!(crate::doc::ViewDoc::filter(&doc).search, "iroh");
            assert_eq!(app.shared.borrow().filter.search, "iroh");
        }
        // The unit and boot filters restrict to one unit and one boot.
        {
            let (mut app, mut doc) = synced_app();
            app.shared
                .borrow_mut()
                .push(Request::Unit("kernel".to_string()));
            let boot = app.boots[0].0.clone();
            app.shared.borrow_mut().push(Request::Boot(boot.clone()));
            app.apply_requests(&mut doc);
            assert!(
                app.filtered
                    .iter()
                    .all(|&i| app.entries[i].unit == "kernel" && app.entries[i].boot_id == boot)
            );
            let stored = crate::doc::ViewDoc::filter(&doc);
            assert_eq!(stored.unit, "kernel");
            assert_eq!(stored.boot, boot);
            assert!(
                doc.keys(&ROOT).next().is_some(),
                "document filter actions must write shared state"
            );
        }
    }

    /// Selection auto-lands on an error or worse; choosing a row commits the
    /// shared selection and the inspector follows it.
    #[test]
    fn selection_auto_lands_on_errors_and_moves_through_the_document() {
        let (mut app, mut doc) = synced_app();
        let sel = app.effective_selected().expect("an entry is auto-selected");
        assert!(app.entries[sel].priority.level() <= Priority::Err.level());
        let shown = app.shared.borrow().selected;
        assert_eq!(shown, app.filtered_pos(sel));

        let next = app.shared.borrow().rows[shown.unwrap() + 1].clone();
        app.shared
            .borrow_mut()
            .push(Request::Select(next.entry_id.clone()));
        app.apply_requests(&mut doc);
        let after = app.effective_selected().unwrap();
        assert_ne!(sel, after);
        assert_eq!(app.selected_entry.as_deref(), Some(next.entry_id.as_str()));
        assert_eq!(
            crate::doc::ViewDoc::selected_entry(&doc).as_deref(),
            Some(next.entry_id.as_str())
        );
        assert_eq!(app.inspected, Inspected::Entry(next.entry_id));
    }

    /// The journal is the content and the entry the inspector: docked beside
    /// the table on a desktop window, a drawer on a phone that a chosen row
    /// opens. The header carries the journal's commands.
    #[test]
    fn window_docks_the_inspector_on_desktop_and_draws_it_on_a_phone() {
        let desktop = harness(LogsApp::new(sample_entries(), true), (1280, 800));
        assert_eq!(
            desktop.root::<LogsApp>().shell.geometry().inspector,
            InspectorPlacement::Docked
        );
        for name in [
            "Journal entries",
            "Refresh journal",
            "Live tail",
            "Export filtered logs",
        ] {
            assert!(named(&desktop, name), "{name} is exposed");
        }

        let mut phone = harness(LogsApp::new(sample_entries(), true), (390, 844));
        assert_eq!(
            phone.root::<LogsApp>().shell.geometry().inspector,
            InspectorPlacement::Drawer
        );
        assert!(!phone.root::<LogsApp>().shell.is_inspector_open());
        let entry = phone.root::<LogsApp>().shared.borrow().rows[0]
            .entry_id
            .clone();
        phone
            .root_mut::<LogsApp>()
            .shared
            .borrow_mut()
            .push(Request::Select(entry));
        phone.send(&WindowEvent::RedrawRequested);
        assert!(phone.root::<LogsApp>().shell.is_inspector_open());
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
            let node = app.accessibility().expect("the outcome is announced");
            assert_eq!(node.name.as_deref(), Some("Export failed."));
            assert_eq!(node.live, Some(ui::a11y::Politeness::Polite));
        }
    }

    /// Refresh queues the exported root operation; clearing the filters from
    /// the no-matches state resets them; an unreadable journal shows one
    /// empty state and no inspector.
    #[test]
    fn journal_commands_and_empty_states_act_through_production_paths() {
        {
            let mut app = LogsApp::new(sample_entries(), true);
            app.request_journal_operation("refresh");
            let invocation = ui::widget::controls::take_control_invocation(&mut app)
                .expect("refresh queues an inert control invocation");
            assert_eq!(invocation.address.resource, actions::JOURNAL_RESOURCE_ID);
            assert_eq!(invocation.address.action, "refresh");
            assert!(app.source_loading);
        }
        {
            let (mut app, mut doc) = synced_app();
            app.shared
                .borrow_mut()
                .push(Request::Search("no such message anywhere".to_string()));
            app.apply_requests(&mut doc);
            assert_eq!(app.placeholder(), Some(Placeholder::NoMatches));
            app.shared.borrow_mut().push(Request::ClearFilters);
            app.apply_requests(&mut doc);
            assert!(app.filter.is_clear());
            assert_eq!(app.placeholder(), None);
        }
        {
            let app = LogsApp::new_with_source_error(
                Vec::new(),
                true,
                crate::journal::JournalLoadError::AccessDenied,
            );
            assert_eq!(
                app.placeholder(),
                Some(Placeholder::Unavailable(
                    crate::journal::JournalLoadError::AccessDenied
                ))
            );
            assert!(!app.has_inspector, "the detail pane collapses");
            assert!(!app.shared.borrow().has_entries, "no filters to offer");
        }
    }
}
