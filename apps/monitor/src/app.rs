//! The Monitor window: a two-tab activity monitor (Processes / Resources) over
//! [`MonitorState`], which holds this machine's readings and what the window shows
//! of them. The shared table layout is the document's; the tab, filter and
//! selection belong to this window, and the process list never leaves this machine.

use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::framework::{Dispatcher, Effected, Live, LiveRow, Publishes, Setting, ViewCx};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tilo_resource::{self, Model};
use ui::time::{Duration, Instant};
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::toast::ToastKind;
use ui::widget::{
    AppModel, Element, InspectorPlacement, ModelPath, NavShell, Switch, TaffyTreeExt, Widget,
};

use crate::model::{MonitorModel, ProcessDataStatus, ProcessInfo};
use crate::prefs::{MonitorPreferences, Prefs};
use crate::resource::{Monitor, MonitorEffect};
use crate::sampler::{Sampler, StopKind};
use crate::tree::{FlattenOpts, Forest, Row, SumMode};
use crate::view::{
    Bridge, Cadence, ListOptions, Placeholder, ProcessDetail, ProcessRow, ProcessesPane, Request,
    ResourcesPane, Shared,
};
use ui::charts::RollingSeries;
use ui::widget::DrawChild;

/// The process resource id carries the kernel start tick: a bare PID can be recycled
/// while an action waits to run.
fn process_resource_id(pid: u32, started_at: u64) -> String {
    format!("process:{pid}:started:{started_at}")
}

/// Parse strictly, so a malformed target never falls back to a PID-only action.
pub(crate) fn parse_process_resource_id(id: &str) -> Option<(u32, u64)> {
    let (pid, started_at) = id.strip_prefix("process:")?.split_once(":started:")?;
    Some((pid.parse().ok()?, started_at.parse().ok()?))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Processes,
    Resources,
}

impl Tab {
    const ALL: [Self; 2] = [Self::Processes, Self::Resources];

    fn label(self) -> String {
        crate::resource::CATALOG.t(match self {
            Self::Processes => "monitor-a11y-processes",
            Self::Resources => "monitor-tab-resources",
        })
    }
}

/// The column the process table sorts by.
#[derive(Model, Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortKey {
    Pid,
    Name,
    User,
    #[default]
    Cpu,
    Mem,
    Threads,
}

impl SortKey {
    /// Numeric columns default to descending, text columns to ascending.
    fn default_desc(self) -> bool {
        !matches!(self, SortKey::Name | SortKey::User)
    }
}

/// Published rows stay bounded; other processes resolve on demand through
/// [`Widget::resource`].
const MAX_CONTEXT_PROCESS_RESOURCES: usize = 64;
/// Bound and flatten process names before publishing: they reach global and remote
/// action surfaces.
const MAX_PROCESS_RESOURCE_LABEL_CHARS: usize = 160;

/// A shorter window than the whole-system charts, so the sparkline stays readable.
const PROC_SPARK_CAP: usize = 48;

/// The identity stays in the id; this display label is bounded.
fn process_resource_label(process: &ProcessInfo) -> String {
    let mut label = String::new();
    let mut chars = process.name.chars();
    for _ in 0..MAX_PROCESS_RESOURCE_LABEL_CHARS {
        let Some(ch) = chars.next() else {
            return format!("{} ({})", label, process.pid);
        };
        label.push(if ch.is_control() { ' ' } else { ch });
    }
    if chars.next().is_some() {
        label.push('…');
    }
    format!("{} ({})", label, process.pid)
}

/// One process row, bound to its PID generation.
pub(crate) fn process_row(process: &ProcessInfo) -> LiveRow {
    let id = process_resource_id(process.pid, process.started_at);
    let row = LiveRow::new(
        id.clone(),
        "monitor.process",
        process_resource_label(process),
    );
    [
        MonitorEffect::Inspect {
            process: id.clone(),
        },
        MonitorEffect::CopyPid {
            process: id.clone(),
        },
        MonitorEffect::EndProcess {
            process: id.clone(),
        },
        MonitorEffect::Kill { process: id },
    ]
    .into_iter()
    .chain(process.app.clone().map(|app| MonitorEffect::EndApp { app }))
    .fold(row, |row, effect| row.effect(&effect))
}

fn indexed_text(value: &str) -> String {
    value
        .chars()
        .take(MAX_PROCESS_RESOURCE_LABEL_CHARS)
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect()
}

fn process_search_entry(process: &ProcessInfo) -> ui::tilo_resource::SearchEntry {
    let id = process_resource_id(process.pid, process.started_at);
    ui::tilo_resource::SearchEntry::new(
        process_resource_label(process),
        format!(
            "{} {} {}",
            indexed_text(&process.name),
            indexed_text(&process.user),
            process.pid
        ),
    )
    .anchored(id)
    .grouped("Processes")
    .subtitled(format!(
        "PID {} owned by {}",
        process.pid,
        indexed_text(&process.user)
    ))
}

/// A choice on the row menu, in the order the menu lists them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MenuChoice {
    ShowDetails,
    Expand(bool),
    Copy,
    End,
    Kill,
    EndApp,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Inspected {
    Stale,
    Hidden,
    Nothing,
    Process(u64),
}

fn first_child(taffy: &TaffyTree<()>, node: NodeId) -> Option<NodeId> {
    taffy.children(node).ok()?.first().copied()
}

fn tab_content(shared: &Shared, shown_tab: &Rc<std::cell::Cell<Tab>>) -> Switch<Tab> {
    let shared = Rc::clone(shared);
    let shown_tab = Rc::clone(shown_tab);
    Switch::new(
        move || shown_tab.get(),
        move |tab| match tab {
            Tab::Processes => Box::new(ProcessesPane::new(Rc::clone(&shared))) as Element,
            Tab::Resources => Box::new(ResourcesPane::new(Rc::clone(&shared))),
        },
    )
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent flags: sampling gate, publishing and a capture's reveal"
)]
pub(crate) struct MonitorState {
    model: MonitorModel,
    sampler: Sampler,
    /// Retained search index; its entries carry the same PID-generation ids.
    prefs: Prefs,
    /// The person's preferences when hosted, and the value last stored.
    preferences: Option<(Setting<MonitorPreferences>, MonitorPreferences)>,

    tab: Tab,
    sort_key: SortKey,
    sort_desc: bool,
    tree_mode: bool,
    /// Whether a parent row shows its summed subtree use or only its own.
    sum_mode: SumMode,
    expanded: HashSet<u32>,
    filter: String,
    selected_pid: Option<u32>,
    /// Per-process CPU history, keyed by the kernel process generation.
    proc_cpu_hist: HashMap<(u32, u64), RollingSeries>,

    last_refresh: Instant,
    /// Never sample the live machine: the seeded snapshot is the whole model.
    sample_only: bool,

    shared: Shared,
    shown_tab: Rc<std::cell::Cell<Tab>>,
    stale: bool,
    told: bool,
    metrics_revision: u64,
    published_metrics: Option<u64>,
    published_rows: Option<u64>,
    reveal_detail: bool,
    reveal: bool,
    menu: Option<(u32, Point, Rect)>,
    toasts: Vec<String>,
    /// The door edits and effects go through once the framework hosts the window, so
    /// the host checks permission and confirmation as it does for the palette.
    dispatcher: Option<Dispatcher<Monitor>>,
    #[cfg(test)]
    requested: Vec<MonitorEffect>,
}

pub(crate) type State = Rc<RefCell<MonitorState>>;

/// The window: the shared [`MonitorState`] in a [`NavShell`].
pub struct MonitorApp {
    state: State,
    /// The table layout as the document holds it; `None` outside the framework.
    live: Option<Live<Monitor>>,
    presented: Option<(u64, u64)>,
    context: ContextMenu,
    context_pid: Option<u32>,
    context_choices: Vec<Option<MenuChoice>>,
    rect: Rect,
    shell: NavShell,
    bar: ui::framework::BarCommands<window_bar::BarCommand>,
    inspected: Inspected,
    /// How many layouts the window has had, which a capture's drawer waits on.
    layouts: u32,
    layout_dirty: bool,
    /// Handed to the views since the host last took the structure.
    retold: bool,

    dirty: bool,
}

impl Default for MonitorApp {
    fn default() -> Self {
        Self::new()
    }
}

impl MonitorApp {
    fn over(state: MonitorState) -> Self {
        let shell = NavShell::new()
            .content_kind(ui::widget::ContentKind::List)
            .with_id("monitor.shell")
            .with_window_toolbar()
            .content(Box::new(tab_content(&state.shared, &state.shown_tab)));
        Self {
            dirty: true,
            state: Rc::new(RefCell::new(state)),
            live: None,
            presented: None,
            context: ContextMenu::new(),
            context_pid: None,
            context_choices: Vec::new(),
            rect: Rect::ZERO,
            shell,
            bar: ui::framework::BarCommands::new(),
            inspected: Inspected::Stale,
            layouts: 0,
            layout_dirty: true,
            retold: true,
        }
    }

    pub(crate) fn state(&self) -> std::cell::RefMut<'_, MonitorState> {
        self.state.borrow_mut()
    }

    /// The sample snapshot: a capture never ticks the sampler.
    pub fn new() -> Self {
        Self::sample(MonitorModel::sample())
    }

    /// Over `model`, a snapshot that is never sampled again.
    pub fn sample(model: MonitorModel) -> Self {
        let mut state = MonitorState::build(model, ui::time::now(), Sampler::inert());
        state.sample_only = true;
        // Select a row from the default collapsed tree, so the inspector never names
        // a hidden process.
        state.selected_pid = state.busiest_visible_pid();
        state.seed_selected_spark();
        Self::over(state).published()
    }

    pub fn past() -> Self {
        let mut state =
            MonitorState::build(MonitorModel::empty(), ui::time::now(), Sampler::inert());
        state.sample_only = true;
        Self::over(state).published()
    }

    /// The live window: an empty model, sampled on the first frame.
    pub fn new_live() -> Self {
        let stale = ui::time::now()
            .checked_sub(Duration::from_secs(3600))
            .unwrap_or_else(Instant::now);
        Self::over(MonitorState::build(
            MonitorModel::empty(),
            stale,
            Sampler::new(),
        ))
        .published()
    }

    fn published(mut self) -> Self {
        self.publish();
        self
    }

    #[cfg(test)]
    pub(crate) fn live_over(sampler: Sampler) -> Self {
        let stale = ui::time::now()
            .checked_sub(Duration::from_secs(3600))
            .unwrap_or_else(Instant::now);
        Self::over(MonitorState::build(MonitorModel::empty(), stale, sampler)).published()
    }
}

impl MonitorState {
    fn build(model: MonitorModel, last_refresh: Instant, sampler: Sampler) -> Self {
        let shared: Shared = Rc::new(RefCell::new(Bridge::default()));
        let shown_tab = Rc::new(std::cell::Cell::new(Tab::Processes));
        let prefs = Prefs::default();
        Self {
            model,
            sampler,
            prefs,
            preferences: None,
            tab: Tab::Processes,
            sort_key: SortKey::Cpu,
            sort_desc: true,
            tree_mode: true,
            sum_mode: SumMode::SumSubtree,
            expanded: HashSet::new(),
            filter: String::new(),
            selected_pid: None,
            proc_cpu_hist: HashMap::new(),
            last_refresh,
            sample_only: false,
            shared,
            shown_tab,
            stale: true,
            metrics_revision: 0,
            published_metrics: None,
            published_rows: None,
            told: false,
            reveal_detail: false,
            reveal: false,
            menu: None,
            toasts: Vec::new(),
            dispatcher: None,
            #[cfg(test)]
            requested: Vec::new(),
        }
    }

    /// Take the machine's newest reading: pushed by the session on Linux, read at most
    /// once per cadence elsewhere. A capture never trips this.
    fn maybe_refresh(&mut self) {
        if self.sample_only {
            return;
        }
        if self.sampler.is_pushed() {
            self.sampler.set_interval(
                self.prefs
                    .auto_refresh
                    .then(|| u32::try_from(self.prefs.interval_ms).unwrap_or(u32::MAX)),
            );
            if self.model.process_status == ProcessDataStatus::Loading {
                // The first frame waits for the first reading, which the session sends
                // at once.
                self.refresh_live_snapshot();
                return;
            }
            let selected_before = self.selected_identity();
            if self.sampler.refresh(&mut self.model) {
                self.after_refresh(selected_before);
            }
        } else if self.prefs.auto_refresh
            && self.last_refresh.elapsed() >= Duration::from_millis(self.prefs.interval_ms)
        {
            let selected_before = self.selected_identity();
            self.sampler.refresh(&mut self.model);
            self.after_refresh(selected_before);
        }
    }

    fn refresh_live_snapshot(&mut self) {
        let selected_before = self.selected_identity();
        self.sampler.refresh_now(&mut self.model);
        self.after_refresh(selected_before);
    }

    /// Keep the selected lifetime across a reading; a recycled PID leaves no action
    /// pointed at its replacement.
    fn after_refresh(&mut self, selected_before: Option<(u32, u64)>) {
        self.last_refresh = ui::time::now();
        self.clear_recycled_selection(selected_before);
        self.record_proc_cpu();
        self.metrics_changed();
        self.clear_selection_if_filtered_out();
    }

    pub(crate) fn wake_with(&mut self, waker: ui::WakeHandle) {
        if !self.sample_only {
            self.sampler.set_waker(waker);
        }
    }

    fn metrics_changed(&mut self) {
        self.metrics_revision = self.metrics_revision.wrapping_add(1);
        self.stale = true;
    }

    /// Clear a selection the new snapshot no longer holds.
    fn clear_recycled_selection(&mut self, selected_before: Option<(u32, u64)>) {
        if selected_before.is_some_and(|(pid, started_at)| {
            !self
                .model
                .processes
                .iter()
                .any(|p| p.pid == pid && p.started_at == started_at)
        }) {
            self.selected_pid = None;
            self.stale = true;
        }
    }

    /// Fold this reading's per-process CPU into the sparklines, then prune rings for
    /// processes that exited.
    fn record_proc_cpu(&mut self) {
        for p in &self.model.processes {
            self.proc_cpu_hist
                .entry((p.pid, p.started_at))
                .or_insert_with(|| RollingSeries::new(PROC_SPARK_CAP))
                .push(p.cpu);
        }
        let live: HashSet<(u32, u64)> = self
            .model
            .processes
            .iter()
            .map(|p| (p.pid, p.started_at))
            .collect();
        self.proc_cpu_hist
            .retain(|identity, _| live.contains(identity));
    }

    fn seed_selected_spark(&mut self) {
        let Some(pid) = self.selected_pid else { return };
        let Some(p) = self.model.processes.iter().find(|p| p.pid == pid) else {
            return;
        };
        let base = p.cpu;
        let ring = RollingSeries::from_samples(
            PROC_SPARK_CAP,
            (0..PROC_SPARK_CAP).map(|i| {
                let t = i as f32;
                (base * (1.0 + 0.35 * (t * 0.31).sin() + 0.15 * (t * 0.73).cos())).max(0.0)
            }),
        );
        self.proc_cpu_hist.insert((pid, p.started_at), ring);
        self.stale = true;
    }

    fn visible_processes(&self) -> Vec<ProcessInfo> {
        let f = self.filter.to_lowercase();
        let mut v: Vec<ProcessInfo> = self
            .model
            .processes
            .iter()
            .filter(|p| self.prefs.show_kernel_threads || !p.kernel)
            .filter(|p| {
                f.is_empty()
                    || p.name.to_lowercase().contains(&f)
                    || p.user.to_lowercase().contains(&f)
                    || p.pid.to_string().contains(&f)
            })
            .cloned()
            .collect();
        v.sort_by(|a, b| {
            let o = match self.sort_key {
                SortKey::Pid => a.pid.cmp(&b.pid),
                SortKey::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                SortKey::User => a.user.to_lowercase().cmp(&b.user.to_lowercase()),
                SortKey::Cpu => a.cpu.total_cmp(&b.cpu),
                SortKey::Mem => a.mem_bytes.cmp(&b.mem_bytes),
                SortKey::Threads => a.threads.cmp(&b.threads),
            };
            if self.sort_desc { o.reverse() } else { o }
        });
        v
    }

    /// The rows the table lists: the flattened forest in tree mode, the sorted list at
    /// depth 0 in flat mode.
    fn visible_rows(&self) -> Vec<Row> {
        if self.tree_mode {
            let forest = Forest::build(&self.model.processes);
            let filter = self.filter.to_lowercase();
            forest.flatten(&FlattenOpts {
                sum_mode: self.sum_mode,
                sort_key: self.sort_key,
                sort_desc: self.sort_desc,
                filter: &filter,
                expanded: &self.expanded,
                show_kernel: self.prefs.show_kernel_threads,
            })
        } else {
            self.visible_processes()
                .into_iter()
                .map(|p| Row {
                    pid: p.pid,
                    name: p.name,
                    user: p.user,
                    cpu: p.cpu,
                    mem_bytes: p.mem_bytes,
                    threads: p.threads,
                    depth: 0,
                    has_children: false,
                    expanded: false,
                })
                .collect()
        }
    }

    fn listable_count(&self) -> usize {
        self.model
            .processes
            .iter()
            .filter(|p| self.prefs.show_kernel_threads || !p.kernel)
            .count()
    }

    fn selected_process(&self) -> Option<&ProcessInfo> {
        let pid = self.selected_pid?;
        self.model.processes.iter().find(|p| p.pid == pid)
    }

    fn selected_identity(&self) -> Option<(u32, u64)> {
        self.selected_process().map(|p| (p.pid, p.started_at))
    }

    /// A filter or a collapse never leaves an actionable inspector for a process that
    /// is no longer listed.
    fn clear_selection_if_filtered_out(&mut self) {
        self.stale = true;
        let Some(pid) = self.selected_pid else {
            return;
        };
        if !self.visible_rows().iter().any(|row| row.pid == pid) {
            self.selected_pid = None;
        }
    }

    fn row_has_children(&self, pid: u32) -> bool {
        self.model.processes.iter().any(|c| {
            c.ppid == Some(pid) && c.pid != pid && (self.prefs.show_kernel_threads || !c.kernel)
        })
    }

    fn set_expanded(&mut self, pid: u32, open: bool) {
        let changed = if open {
            self.expanded.insert(pid)
        } else {
            self.expanded.remove(&pid)
        };
        if changed {
            self.clear_selection_if_filtered_out();
        }
    }

    fn toggle_expand(&mut self, pid: u32) {
        let open = !self.expanded.contains(&pid);
        self.set_expanded(pid, open);
    }

    fn busiest_visible_pid(&self) -> Option<u32> {
        self.visible_rows()
            .into_iter()
            .max_by(|a, b| a.cpu.total_cmp(&b.cpu))
            .map(|r| r.pid)
    }

    fn busiest_pid_named(&self, name: &str) -> Option<u32> {
        self.model
            .processes
            .iter()
            .filter(|p| p.name == name)
            .max_by(|a, b| a.cpu.total_cmp(&b.cpu))
            .map(|p| p.pid)
    }

    fn expand_by_name(&mut self, names: &[&str]) {
        let pids: Vec<u32> = self
            .model
            .processes
            .iter()
            .filter(|p| names.contains(&p.name.as_str()))
            .map(|p| p.pid)
            .collect();
        self.expanded.extend(pids);
        self.stale = true;
    }

    pub(crate) fn attach_actions(&self, actions: ui::framework::Actions<Monitor>) {
        let mut shared = self.shared.borrow_mut();
        shared.actions = Some(actions);
        shared.revision += 1;
    }

    pub(crate) fn attach_framework(&mut self, dispatcher: Dispatcher<Monitor>) {
        self.dispatcher = Some(dispatcher);
    }

    /// Follow the person's preferences: start from them, store changes, take another
    /// window's.
    pub(crate) fn follow_preferences(&mut self, setting: Setting<MonitorPreferences>) {
        let stored = setting.get();
        self.adopt_preferences(&stored);
        self.preferences = Some((setting, stored));
    }

    fn adopt_preferences(&mut self, personal: &MonitorPreferences) {
        if self.prefs.personal() == *personal {
            return;
        }
        self.prefs.adopt(personal);
        self.clear_selection_if_filtered_out();
    }

    /// Whether the stored preferences moved since this window last looked.
    fn preferences_changed(&self) -> bool {
        self.preferences
            .as_ref()
            .is_some_and(|(setting, last)| setting.get() != *last || self.prefs.personal() != *last)
    }

    fn sync_preferences(&mut self) {
        let Some((setting, last)) = &self.preferences else {
            return;
        };
        let stored = setting.get();
        if stored != *last {
            self.adopt_preferences(&stored);
            if let Some((_, last)) = &mut self.preferences {
                *last = stored;
            }
            return;
        }
        let here = self.prefs.personal();
        if here == *last {
            return;
        }
        match setting.set(here.clone()) {
            Ok(()) => {
                if let Some((_, last)) = &mut self.preferences {
                    *last = here;
                }
            }
            Err(error) => tracing::warn!(%error, "could not keep the Monitor preferences"),
        }
    }

    /// Adopt the shared table layout.
    pub(crate) fn adopt_resource_state(&mut self, state: &Monitor) {
        let layout = (state.sort, !state.ascending, !state.flat);
        if (self.sort_key, self.sort_desc, self.tree_mode) == layout {
            return;
        }
        (self.sort_key, self.sort_desc, self.tree_mode) = layout;
        self.clear_selection_if_filtered_out();
    }

    fn commit_layout(&self, edits: Vec<ui::tilo_resource::Edit<Monitor>>) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.group(|| {
                for edit in edits {
                    dispatcher.submit(edit);
                }
            });
        }
    }

    pub(crate) fn opener(&self) -> impl FnMut(&str) -> Result<(), &'static str> + 'static {
        let shared = Rc::clone(&self.shared);
        move |anchor| {
            parse_process_resource_id(anchor).ok_or("not a process of this machine")?;
            shared.borrow_mut().push(Request::Open(anchor.to_owned()));
            Ok(())
        }
    }

    /// Run `effect` through the same door as the palette and agents, which checks
    /// permission and confirmation.
    #[cfg_attr(
        not(test),
        allow(
            clippy::needless_pass_by_ref_mut,
            clippy::needless_pass_by_value,
            reason = "a test also keeps what was asked for"
        )
    )]
    fn request(&mut self, effect: MonitorEffect) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.effect(&effect);
        }
        #[cfg(test)]
        self.requested.push(effect);
    }

    /// The bounded page of live rows; any other process resolves by id.
    pub(crate) fn live_process_rows(&self) -> Vec<LiveRow> {
        self.model
            .processes
            .iter()
            .take(MAX_CONTEXT_PROCESS_RESOURCES)
            .map(process_row)
            .collect()
    }

    pub(crate) fn process_search_entries(&self) -> Vec<ui::tilo_resource::SearchEntry> {
        self.model
            .processes
            .iter()
            .map(process_search_entry)
            .collect()
    }

    pub(crate) fn live_process_row(&self, id: &str) -> Option<LiveRow> {
        let (pid, started_at) = parse_process_resource_id(id)?;
        self.model
            .processes
            .iter()
            .find(|process| process.pid == pid && process.started_at == started_at)
            .map(process_row)
    }

    fn commit_tab(&mut self, tab: Tab) {
        if self.tab == tab {
            return;
        }
        self.tab = tab;
        self.stale = true;
    }

    fn commit_tree_mode(&mut self, tree: bool) {
        if self.tree_mode == tree {
            return;
        }
        self.tree_mode = tree;
        self.stale = true;
        self.commit_layout(vec![Monitor::paths().flat.set(!tree)]);
    }

    fn commit_selected(&mut self, pid: Option<u32>) {
        if self.selected_pid == pid {
            return;
        }
        self.selected_pid = pid;
        self.stale = true;
    }

    fn commit_filter(&mut self, query: String) {
        if self.filter == query {
            return;
        }
        self.filter = query;
        self.clear_selection_if_filtered_out();
    }

    fn set_sort(&mut self, key: SortKey) {
        let paths = Monitor::paths();
        let mut edits = Vec::new();
        if self.sort_key == key {
            self.sort_desc = !self.sort_desc;
        } else {
            self.sort_key = key;
            self.sort_desc = key.default_desc();
            edits.push(paths.sort.set(key));
        }
        edits.push(paths.ascending.set(!self.sort_desc));
        self.stale = true;
        self.commit_layout(edits);
    }

    fn set_cadence(&mut self, cadence: Cadence) {
        match cadence {
            Cadence::Every(ms) => {
                if !self.prefs.auto_refresh {
                    // Resuming starts a fresh cadence window.
                    self.last_refresh = ui::time::now();
                }
                self.prefs.interval_ms = ms;
                self.prefs.auto_refresh = true;
            }
            Cadence::Paused => self.prefs.auto_refresh = false,
        }
        self.stale = true;
    }

    fn cadence(&self) -> Cadence {
        if self.prefs.auto_refresh {
            Cadence::Every(self.prefs.interval_ms)
        } else {
            Cadence::Paused
        }
    }

    fn request_process_control(&mut self, pid: u32, control: fn(String) -> MonitorEffect) {
        if let Some(process) = self
            .model
            .processes
            .iter()
            .find(|process| process.pid == pid)
        {
            let effect = control(process_resource_id(pid, process.started_at));
            self.request(effect);
        }
    }

    fn current_process(&self, id: &str) -> Result<ProcessInfo, String> {
        let (pid, started_at) =
            parse_process_resource_id(id).ok_or_else(|| format!("`{id}` names no process"))?;
        self.model
            .processes
            .iter()
            .find(|process| process.pid == pid && process.started_at == started_at)
            .cloned()
            .ok_or_else(|| "process is no longer running".into())
    }

    /// Answer `effect`, however it was invoked; the host has already checked its
    /// permission and had a destructive one confirmed.
    pub(crate) fn answer(&mut self, effect: MonitorEffect) -> Result<(), String> {
        match effect {
            MonitorEffect::Refresh => {
                self.refresh_live_snapshot();
                Ok(())
            }
            MonitorEffect::ShowTab { tab } => {
                let tab = match tab.as_str() {
                    "processes" => Tab::Processes,
                    "resources" => Tab::Resources,
                    other => return Err(format!("there is no `{other}` tab")),
                };
                self.commit_tab(tab);
                Ok(())
            }
            MonitorEffect::Filter { query } => {
                self.commit_filter(query.unwrap_or_default());
                Ok(())
            }
            MonitorEffect::Inspect { process } => {
                let process = self.current_process(&process)?;
                self.commit_selected(Some(process.pid));
                self.commit_tab(Tab::Processes);
                self.reveal = true;
                Ok(())
            }
            MonitorEffect::CopyPid { process } => {
                let process = self.current_process(&process)?;
                self.copy_pid(process.pid);
                Ok(())
            }
            MonitorEffect::EndProcess { process } => self.signal(&process, StopKind::Term),
            MonitorEffect::Kill { process } => self.signal(&process, StopKind::Kill),
            MonitorEffect::EndApp { app } => self.end_app(&app),
        }
    }

    fn end_app(&mut self, app: &str) -> Result<(), String> {
        if !self
            .model
            .processes
            .iter()
            .any(|process| process.app.as_deref() == Some(app))
        {
            return Err(format!("`{app}` is not running"));
        }
        if self.sample_only {
            self.model
                .processes
                .retain(|process| process.app.as_deref() != Some(app));
            self.selected_pid = None;
            self.metrics_changed();
            return Ok(());
        }
        self.sampler.end_app(app)?;
        self.refresh_live_snapshot();
        Ok(())
    }

    fn request_end_app(&mut self, pid: u32) {
        if let Some(app) = self
            .model
            .processes
            .iter()
            .find(|process| process.pid == pid)
            .and_then(|process| process.app.clone())
        {
            self.request(MonitorEffect::EndApp { app });
        }
    }

    fn signal(&mut self, id: &str, kind: StopKind) -> Result<(), String> {
        let process = self.current_process(id)?;
        if self.do_stop(process.pid, process.started_at, kind) {
            Ok(())
        } else {
            Err("the operating system refused the process signal".into())
        }
    }

    /// Send the signal, then let the next snapshot remove the row: dispatch acceptance
    /// is not proof that a process exited. A sample drops the seeded row locally.
    fn do_stop(&mut self, pid: u32, started_at: u64, kind: StopKind) -> bool {
        if !self.sample_only && !self.sampler.signal_if_current(pid, started_at, kind) {
            return false;
        }
        if self.sample_only {
            self.model
                .processes
                .retain(|p| p.pid != pid || p.started_at != started_at);
            if self.selected_pid == Some(pid) {
                self.selected_pid = None;
            }
            self.metrics_changed();
        } else {
            self.refresh_live_snapshot();
        }
        true
    }

    fn copy_pid(&mut self, pid: u32) {
        ui::clipboard_copy(&pid.to_string());
        self.toasts.push(format!("Copied PID {pid}"));
        self.selected_pid = Some(pid);
        self.stale = true;
    }

    fn rows_fingerprint(rows: &[ProcessRow]) -> u64 {
        let mut hasher = DefaultHasher::new();
        for row in rows {
            row.pid.hash(&mut hasher);
            row.name.hash(&mut hasher);
            row.user.hash(&mut hasher);
            row.cpu.to_bits().hash(&mut hasher);
            row.mem_bytes.hash(&mut hasher);
            row.threads.hash(&mut hasher);
            row.depth.hash(&mut hasher);
            row.has_children.hash(&mut hasher);
            row.expanded.hash(&mut hasher);
        }
        hasher.finish()
    }

    fn placeholder(&self, rows: usize) -> Option<Placeholder> {
        let listed = if rows > 0 {
            None
        } else if self.filter.is_empty() {
            Some(Placeholder::Empty)
        } else {
            Some(Placeholder::NoMatches)
        };
        Placeholder::of(self.model.process_status).or(listed)
    }

    fn publish(&mut self) {
        if !self.stale {
            return;
        }
        self.stale = false;
        let rows: Vec<ProcessRow> = self
            .visible_rows()
            .into_iter()
            .map(|row| ProcessRow {
                pid: row.pid,
                name: row.name,
                user: row.user,
                cpu: row.cpu,
                mem_bytes: row.mem_bytes,
                threads: row.threads,
                depth: row.depth,
                has_children: row.has_children,
                expanded: row.expanded,
            })
            .collect();
        let selected = self
            .selected_pid
            .and_then(|pid| rows.iter().position(|row| row.pid == pid));
        let placeholder = self.placeholder(rows.len());
        let fingerprint = Self::rows_fingerprint(&rows);
        {
            let mut bridge = self.shared.borrow_mut();
            bridge.revision = bridge.revision.wrapping_add(1);
            bridge.total = self.listable_count();
            if self.published_rows != Some(fingerprint) {
                self.published_rows = Some(fingerprint);
                bridge.rows = rows;
                bridge.rows_revision = bridge.rows_revision.wrapping_add(1);
            }
            bridge.selected = selected;
            bridge.options = Some(ListOptions {
                sort: self.sort_key,
                descending: self.sort_desc,
                tree: self.tree_mode,
                sum: self.sum_mode,
                kernel_threads: self.prefs.show_kernel_threads,
            });
            bridge.filter.clone_from(&self.filter);
            bridge.placeholder = placeholder;
            if self.published_metrics != Some(self.metrics_revision) {
                self.published_metrics = Some(self.metrics_revision);
                bridge.metrics = self
                    .model
                    .metrics
                    .measured()
                    .then(|| self.model.metrics.clone());
                bridge.process_count = self.model.processes.len();
                bridge.thread_count = self.model.processes.iter().map(|p| p.threads).sum();
                bridge.metrics_revision = bridge.metrics_revision.wrapping_add(1);
            }
        }
        self.shown_tab.set(self.tab);
        self.told = true;
    }

    /// Turning requests into view changes never rebuilds the inspector while the host
    /// routes an event through the tree it holds.
    fn apply_requests(&mut self) {
        let requests = self.shared.borrow_mut().take_requests();
        for request in requests {
            match request {
                Request::Tab(tab) => self.commit_tab(tab),
                Request::Sort(key) => self.set_sort(key),
                Request::Filter(query) => self.commit_filter(query),
                Request::Select(pid) => {
                    self.commit_selected(Some(pid));
                    self.reveal = true;
                }
                Request::Open(process) => {
                    if let Err(error) = self.answer(MonitorEffect::Inspect { process }) {
                        tracing::debug!(%error, "nothing to open");
                    }
                }
                Request::Activate(pid) => {
                    if self.tree_mode && self.row_has_children(pid) {
                        self.toggle_expand(pid);
                    } else {
                        self.commit_selected(Some(pid));
                        self.reveal = true;
                    }
                }
                Request::SetExpanded(pid, open) => self.set_expanded(pid, open),
                Request::TreeMode(tree) => self.commit_tree_mode(tree),
                Request::SumChildren(sum) => {
                    self.sum_mode = if sum {
                        SumMode::SumSubtree
                    } else {
                        SumMode::PerProcess
                    };
                    self.stale = true;
                }
                Request::KernelThreads(show) => {
                    self.prefs.show_kernel_threads = show;
                    self.clear_selection_if_filtered_out();
                }
                Request::ShowStat(stat) => {
                    self.commit_tab(Tab::Resources);
                    self.shared.borrow_mut().reveal = Some(stat);
                    self.stale = true;
                }
                Request::ContextMenu { pid, at, row } => {
                    self.commit_selected(Some(pid));
                    self.menu = Some((pid, at, row));
                }
                Request::Refresh => self.request(MonitorEffect::Refresh),
                Request::End(pid) => {
                    self.request_process_control(pid, |process| MonitorEffect::EndProcess {
                        process,
                    });
                }
                Request::Kill(pid) => {
                    self.request_process_control(pid, |process| MonitorEffect::Kill { process });
                }
                Request::EndApp(pid) => self.request_end_app(pid),
            }
        }
    }

    /// Stage the view state a capture cannot reach through input: an open menu, the
    /// drawer, an expanded tree.
    pub(crate) fn stage_scenario(&mut self, scenario: &str) {
        match scenario {
            "detail" => self.reveal_detail = true,
            "resources" | "resources-many" => self.tab = Tab::Resources,
            "no-results" => {
                self.filter = "printer queue".into();
                self.clear_selection_if_filtered_out();
            }
            "tree-expanded" => {
                self.expand_by_name(&["shell", "browser", "code", "cargo", "systemd"]);
            }
            "per-process" => {
                self.sum_mode = SumMode::PerProcess;
                self.expand_by_name(&["shell", "browser", "code", "cargo", "systemd"]);
            }
            "kernel-threads" => {
                self.prefs.show_kernel_threads = true;
                self.filter = "k".into();
                self.selected_pid = self.busiest_pid_named("kworker/u32:3");
            }
            "context-menu" => {
                self.expand_by_name(&["shell", "browser", "code", "cargo"]);
                let pid = self
                    .busiest_pid_named("rustc")
                    .or_else(|| self.busiest_visible_pid());
                self.selected_pid = pid;
                self.shared.borrow_mut().menu_seed = pid;
                self.seed_selected_spark();
            }
            "interval" => self.set_cadence(Cadence::Every(2000)),
            _ => {}
        }
        self.stale = true;
        self.publish();
    }
}

impl MonitorApp {
    fn publish(&mut self) {
        let told = {
            let mut state = self.state.borrow_mut();
            state.publish();
            std::mem::take(&mut state.told)
        };
        if told {
            self.show_inspector();
            self.layout_dirty = true;
            self.retold = true;
        }
    }

    fn follow_document(&mut self) {
        let Some(live) = &self.live else {
            return;
        };
        let Some(shown) = live.changed_since(&mut self.presented) else {
            return;
        };
        self.state.borrow_mut().adopt_resource_state(&shown);
    }

    fn answer_state(&mut self) {
        let (toasts, menu, reveal) = {
            let mut state = self.state.borrow_mut();
            (
                std::mem::take(&mut state.toasts),
                state.menu.take(),
                std::mem::take(&mut state.reveal),
            )
        };
        for toast in toasts {
            self.shell.toast(toast, ToastKind::Success);
        }
        if let Some((pid, at, row)) = menu {
            self.open_context_menu(pid, at, row);
        }
        if reveal {
            self.reveal_inspector();
        }
    }

    fn apply_requests(&mut self) {
        self.state.borrow_mut().apply_requests();
        self.answer_state();
    }

    fn open_context_menu(&mut self, pid: u32, at: Point, row: Rect) {
        let state = self.state.borrow();
        let shell = &self.shell;
        let mut menu = ContextMenu::new();
        let mut choices = Vec::new();
        let mut push = |item: MenuItem, choice: Option<MenuChoice>| {
            menu.push(item);
            choices.push(choice);
        };
        // "Show details" only earns a place where the inspector is a put-away sheet.
        if shell.geometry().inspector == InspectorPlacement::Sheet && !shell.is_inspector_open() {
            push(
                MenuItem::selection("Show details"),
                Some(MenuChoice::ShowDetails),
            );
        }
        if state.tree_mode && state.row_has_children(pid) {
            let open = !state.expanded.contains(&pid);
            let label = if open { "Expand" } else { "Collapse" };
            push(MenuItem::selection(label), Some(MenuChoice::Expand(open)));
        }
        push(MenuItem::selection("Copy PID"), Some(MenuChoice::Copy));
        push(MenuItem::separator(), None);
        push(MenuItem::selection("End process"), Some(MenuChoice::End));
        push(
            MenuItem::destructive_selection("Kill process"),
            Some(MenuChoice::Kill),
        );
        if state
            .model
            .processes
            .iter()
            .any(|process| process.pid == pid && process.app.is_some())
        {
            push(
                MenuItem::destructive_selection("End app"),
                Some(MenuChoice::EndApp),
            );
        }
        drop(state);
        menu.open_anchored(Rect::from_origin_size(at, (0.0, 0.0)), self.rect);
        menu.anchor_row(row);
        self.context = menu;
        self.context_choices = choices;
        self.context_pid = Some(pid);
    }

    fn apply_menu_choice(&mut self, index: usize) {
        let (Some(pid), Some(Some(choice))) =
            (self.context_pid, self.context_choices.get(index).copied())
        else {
            return;
        };
        let mut state = self.state.borrow_mut();
        match choice {
            MenuChoice::ShowDetails => {
                state.commit_selected(Some(pid));
                state.reveal = true;
            }
            MenuChoice::Expand(open) => state.set_expanded(pid, open),
            MenuChoice::Copy => {
                state.request_process_control(pid, |process| MonitorEffect::CopyPid { process });
            }
            MenuChoice::End => {
                state.request_process_control(pid, |process| MonitorEffect::EndProcess { process });
            }
            MenuChoice::Kill => {
                state.request_process_control(pid, |process| MonitorEffect::Kill { process });
            }
            MenuChoice::EndApp => state.request_end_app(pid),
        }
        drop(state);
        self.context_pid = None;
    }

    /// The inspector follows the selection on the Processes tab.
    fn show_inspector(&mut self) {
        let state = Rc::clone(&self.state);
        let state = state.borrow();
        let wanted = state.tab == Tab::Processes
            && state.model.process_status == ProcessDataStatus::Ready
            && !state.shared.borrow().rows.is_empty();
        if !wanted {
            if self.inspected != Inspected::Hidden {
                self.inspected = Inspected::Hidden;
                self.shell.set_inspector(None);
            }
            return;
        }
        let Some(process) = state.selected_process().cloned() else {
            if self.inspected != Inspected::Nothing {
                self.inspected = Inspected::Nothing;
                self.shell
                    .set_inspector(Some(crate::view::process_detail(None)));
                self.shell.set_inspector_title("");
            }
            return;
        };
        let history: Vec<f32> = state
            .proc_cpu_hist
            .get(&(process.pid, process.started_at))
            .map(|ring| ring.as_slice().to_vec())
            .unwrap_or_default();
        let parent = process.ppid.and_then(|ppid| {
            state
                .model
                .processes
                .iter()
                .find(|p| p.pid == ppid)
                .map(|p| p.name.clone())
        });
        // A summed row reads more than the process alone; the inspector names both.
        let with_children = if state.tree_mode && state.sum_mode == SumMode::SumSubtree {
            state
                .shared
                .borrow()
                .rows
                .iter()
                .find(|row| row.pid == process.pid && row.has_children)
                .map(|row| (row.cpu, row.mem_bytes))
        } else {
            None
        };
        let fingerprint = {
            let mut hasher = DefaultHasher::new();
            format!("{process:?}").hash(&mut hasher);
            parent.hash(&mut hasher);
            with_children
                .map(|(cpu, memory)| (cpu.to_bits(), memory))
                .hash(&mut hasher);
            for sample in &history {
                sample.to_bits().hash(&mut hasher);
            }
            hasher.finish()
        };
        if self.inspected == Inspected::Process(fingerprint) {
            return;
        }
        self.inspected = Inspected::Process(fingerprint);
        let detail = ProcessDetail {
            process: &process,
            parent,
            cpu_history: history,
            with_children,
            requests: Rc::clone(&state.shared),
        };
        self.shell
            .set_inspector(Some(crate::view::process_detail(Some(&detail))));
        self.shell.set_inspector_title(process.name.clone());
    }

    fn reveal_inspector(&mut self) {
        if self.shell.geometry().inspector == InspectorPlacement::Sheet
            && !self.shell.is_inspector_open()
        {
            self.shell.toggle_inspector();
        }
    }
}

/// The view of the table layout; [`MonitorState`] answers the effects and publishes
/// this machine's processes.
pub(crate) fn window(cx: &ViewCx<Monitor>, mut window: MonitorApp) -> Element {
    window.live = Some(cx.live().clone());
    let state = Rc::clone(&window.state);
    {
        let mut state = state.borrow_mut();
        state.wake_with(cx.waker());
        state.follow_preferences(cx.settings());
        state.attach_framework(cx.dispatcher());
        state.attach_actions(cx.actions());
    }
    cx.on_open(state.borrow().opener());
    {
        let answer = Rc::clone(&state);
        cx.effects(move |effect: MonitorEffect, _| {
            answer
                .borrow_mut()
                .answer(effect)
                .map(|()| Effected::done())
        });
    }
    {
        let state = Rc::clone(&state);
        cx.on_stage(move |scenario| {
            if let Some(scenario) = scenario {
                state.borrow_mut().stage_scenario(scenario);
            }
        });
    }
    let (rows, row, entries) = (Rc::clone(&state), Rc::clone(&state), state);
    cx.publishing(
        Publishes::new()
            // The processes on this machine: inspect, copy the PID, stop or kill one.
            .live_rows(move || rows.borrow().live_process_rows())
            .live_row(move |id| row.borrow().live_process_row(id))
            // Every process here, also beyond the published rows.
            .search_entries(move || entries.borrow().process_search_entries())
            // A closed Monitor leaves nothing in the search index to open into a process
            // that ended.
            .search_entries_while_running(),
        Box::new(window),
    )
}

impl MonitorState {
    /// Ctrl+R refreshes, Ctrl+1 and Ctrl+2 switch tabs, Delete kills the selection
    /// through its confirmed action.
    fn handle_shortcut(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) -> bool {
        let WindowEvent::KeyboardInput { event: key, .. } = event else {
            return false;
        };
        if key.state != ElementState::Pressed {
            return false;
        }
        let command = ctx.modifiers.state().control_key() || ctx.modifiers.state().super_key();
        match &key.logical_key {
            Key::Character(c) if command && c.eq_ignore_ascii_case("r") => {
                self.request(MonitorEffect::Refresh);
            }
            Key::Character(c) if command && c.as_str() == "1" => {
                self.commit_tab(Tab::Processes);
            }
            Key::Character(c) if command && c.as_str() == "2" => {
                self.commit_tab(Tab::Resources);
            }
            Key::Named(NamedKey::Delete) if self.tab == Tab::Processes => {
                let Some(pid) = self.selected_pid else {
                    return false;
                };
                self.request_process_control(pid, |process| MonitorEffect::Kill { process });
            }
            _ => return false,
        }
        *ctx.handled = true;
        true
    }
}

impl Widget for MonitorApp {
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.dirty = true;
        self.follow_document();
        {
            // The preferences follow the person's settings, never the shared document.
            let mut monitor = self.state.borrow_mut();
            monitor.sync_preferences();
            monitor.maybe_refresh();
        }
        self.take_window_toolbar_events();
        self.apply_requests();
        self.publish();
        self.declare_window_toolbar();
        self.shell.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        // The window bar's slots are pressed between events: carry out what they asked.
        let asked = self.take_window_toolbar_events();
        if asked {
            self.apply_requests();
        }
        let changed = self.state.borrow().stale;
        self.publish();
        self.declare_window_toolbar();
        self.shell.update(state, path) || asked || changed
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.publish();
        self.layout_dirty = false;
        let shell = self.shell.build_layout(taffy, theme, node_map);
        let node = taffy.fill_node(shell);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(l) = taffy.layout(node) else {
            return;
        };
        self.rect = ui::widget::placed(l, origin);
        if let Some(shell) = first_child(taffy, node) {
            self.shell
                .apply_layout(taffy, shell, Point::new(self.rect.x0, self.rect.y0));
        }
        self.layouts = self.layouts.saturating_add(1);
        // Open the drawer once the content behind it has settled at its width.
        if self.state.borrow().reveal_detail
            && self.shell.geometry().inspector != InspectorPlacement::None
            && self.layouts > 2
        {
            self.state().reveal_detail = false;
            self.reveal_inspector();
            self.shell.snap();
            self.layout_dirty = true;
            self.retold = true;
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
        self.shell
            .draw_child(scene, transform, text_renderer, theme, presence);
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.shell
            .draw_overlay(scene, transform, text_renderer, theme, presence);
        if self.context.is_open() {
            self.context
                .draw_overlay(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.dirty = true;
        if self.context.is_open() {
            self.context.handle_event(event, ctx);
            if let Some(index) = self.context.take_selection() {
                self.apply_menu_choice(index);
            }
            if *ctx.handled || !matches!(event, WindowEvent::CursorMoved { .. }) {
                *ctx.handled = true;
                return;
            }
        }
        if let Some(shell) = first_child(ctx.taffy, ctx.node_id) {
            self.shell
                .handle_event(event, &mut ctx.reborrow(ctx.path, shell));
        }
        if !*ctx.handled {
            self.state().handle_shortcut(event, ctx);
        }
        self.apply_requests();
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut children: Vec<&dyn Widget> = vec![&self.shell];
        if self.context.is_open() {
            children.push(&self.context);
        }
        children
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut children: Vec<&mut (dyn Widget + '_)> = vec![&mut self.shell];
        if self.context.is_open() {
            children.push(&mut self.context);
        }
        children
    }

    fn wants_pointer_capture(&self) -> bool {
        self.context.is_open() || self.shell.wants_pointer_capture()
    }

    fn is_layout_dirty(&self) -> bool {
        let state = self.state.borrow();
        self.layout_dirty
            || state.stale
            || state.preferences_changed()
            || self.shell.is_layout_dirty()
    }

    fn layout_needs_measure(&self) -> bool {
        self.is_layout_dirty() || self.shell.layout_needs_measure()
    }

    fn take_structure_dirty(&mut self) -> bool {
        // What changed since the last frame reaches the views with the next tree; a
        // rebuilt inspector and preferences changed elsewhere count too.
        let retold = std::mem::take(&mut self.retold);
        let state = self.state.borrow();
        let pending = state.stale || retold || state.preferences_changed();
        self.shell.take_structure_dirty() || pending
    }

    fn needs_animation_frame(&self) -> bool {
        self.shell.needs_animation_frame()
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        let mut next = self.shell.next_wake_deadline();
        let state = self.state.borrow();
        let mut consider = |deadline: Instant| {
            next = Some(next.map_or(deadline, |current: Instant| current.min(deadline)));
        };
        // Pushed readings wake the window themselves; only a machine read needs a
        // deadline.
        if !state.sample_only && state.prefs.auto_refresh && !state.sampler.is_pushed() {
            consider(
                state
                    .last_refresh
                    .checked_add(Duration::from_millis(state.prefs.interval_ms))
                    .unwrap_or_else(Instant::now),
            );
        }
        next
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn needs_redraw(&self) -> bool {
        ui::widget::redraw_or_children(self.dirty, self)
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
        ui::widget::Widget::mark_children_drawn(self);
    }

    fn name(&self) -> &'static str {
        "MonitorApp"
    }

    fn test_id(&self) -> Option<&str> {
        Some("monitor_app")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

pub(crate) mod window_bar;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::Stat;
    use automerge::AutoCommit;
    use ui::headless::FrameHarness;

    fn app() -> MonitorApp {
        MonitorApp::new()
    }

    fn taken(a: &MonitorApp) -> Option<MonitorEffect> {
        a.state().requested.pop()
    }

    fn pid_named(a: &MonitorApp, name: &str) -> u32 {
        a.state()
            .model
            .processes
            .iter()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("sample process {name}"))
            .pid
    }

    fn resource_id_for(a: &MonitorApp, pid: u32) -> String {
        let state = a.state();
        let process = state
            .model
            .processes
            .iter()
            .find(|p| p.pid == pid)
            .expect("process");
        process_resource_id(pid, process.started_at)
    }

    fn names(rows: &[Row]) -> Vec<&str> {
        rows.iter().map(|row| row.name.as_str()).collect()
    }

    fn window(a: MonitorApp, viewport: (u32, u32)) -> FrameHarness {
        let mut harness = FrameHarness::for_root(ui::State::new(), a, viewport, Theme::dark());
        for _ in 0..4 {
            harness.relayout();
        }
        harness
    }

    /// The centre of the first widget or accessible node named `name`.
    fn center_of(harness: &FrameHarness, name: &str) -> Option<(f64, f64)> {
        fn number(value: &serde_json::Value, key: &str) -> Option<f64> {
            value.get(key)?.as_f64()
        }
        fn find(value: &serde_json::Value, name: &str, window: Rect) -> Option<(f64, f64)> {
            let on_screen =
                |(x, y): (f64, f64)| window.contains(Point::new(x, y)).then_some((x, y));
            let text = |node: &serde_json::Value, key: &str| {
                node.get(key)
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            };
            if let (Some(rect), Some(semantics)) = (value.get("rect"), value.get("semantics"))
                && (text(value, "test_id").as_deref() == Some(name)
                    || text(semantics, "name").as_deref() == Some(name))
                && let Some([x, y, w, h]) = rect.as_array().and_then(|r| {
                    let r: Vec<f64> = r.iter().filter_map(serde_json::Value::as_f64).collect();
                    <[f64; 4]>::try_from(r).ok()
                })
                && let Some(center) = on_screen((x + w / 2.0, y + h / 2.0))
            {
                return Some(center);
            }
            if (text(value, "name").as_deref() == Some(name)
                || text(value, "value").as_deref() == Some(name))
                && let Some(bounds) = value.get("bounds").filter(|b| !b.is_null())
                && let Some(center) = on_screen((
                    f64::midpoint(number(bounds, "x0")?, number(bounds, "x1")?),
                    f64::midpoint(number(bounds, "y0")?, number(bounds, "y1")?),
                ))
            {
                return Some(center);
            }
            match value {
                serde_json::Value::Array(values) => {
                    values.iter().find_map(|v| find(v, name, window))
                }
                serde_json::Value::Object(values) => {
                    values.values().find_map(|v| find(v, name, window))
                }
                _ => None,
            }
        }
        let inspection = harness.inspection_json().ok()?;
        let window = harness.root::<MonitorApp>().get_rect();
        find(&serde_json::from_str(&inspection).ok()?, name, window)
    }

    fn click(harness: &mut FrameHarness, name: &str) {
        let (x, y) = center_of(harness, name).unwrap_or_else(|| panic!("{name} is on screen"));
        harness.click_at(x, y);
        harness.relayout();
    }

    #[test]
    fn process_list_sorts_filters_and_folds_the_tree() {
        {
            let a = app();
            let rows = a.state().visible_rows();
            assert!(rows.iter().all(|row| row.depth == 0), "collapsed tree");
            assert_eq!(rows[0].name, "shell", "the session sums the most CPU");
            assert!(
                rows.windows(2).all(|pair| pair[0].cpu >= pair[1].cpu),
                "descending CPU"
            );
            assert!(
                !names(&rows).iter().any(|name| name.starts_with("kworker")),
                "kernel threads are hidden by default"
            );
        }

        {
            let a = app();
            let shell = pid_named(&a, "shell");
            let summed = a.state().visible_rows()[0].cpu;
            a.state().toggle_expand(shell);
            let rows = a.state().visible_rows();
            assert!(rows[0].expanded);
            assert!(rows[1].depth == 1, "the shell's children follow it");
            a.state().sum_mode = SumMode::PerProcess;
            let own = a
                .state()
                .visible_rows()
                .into_iter()
                .find(|row| row.pid == shell)
                .unwrap()
                .cpu;
            assert!(own < summed, "a parent shows only its own load");
        }

        {
            let a = app();
            a.state().filter = "rustc".into();
            let rows = a.state().visible_rows();
            assert_eq!(names(&rows), ["shell", "code", "cargo", "rustc"]);
        }

        {
            let a = app();
            a.state().commit_tree_mode(false);
            a.state().set_sort(SortKey::Name);
            let rows = a.state().visible_rows();
            assert!(!a.state().sort_desc, "text sorts ascending first");
            assert!(
                rows.windows(2)
                    .all(|pair| { pair[0].name.to_lowercase() <= pair[1].name.to_lowercase() })
            );
            a.state().prefs.show_kernel_threads = true;
            assert!(
                names(&a.state().visible_rows())
                    .iter()
                    .any(|name| name.starts_with("kworker"))
            );
        }
    }

    #[test]
    fn selection_follows_the_listed_process_lifetime() {
        let mut a = app();
        let selected = a
            .state()
            .selected_pid
            .expect("the sample selects a process");
        assert!(
            a.state()
                .visible_rows()
                .iter()
                .any(|row| row.pid == selected)
        );

        a.state().commit_filter("printer queue".into());
        assert!(a.state().selected_pid.is_none(), "filtered away");
        a.publish();
        assert_eq!(
            a.state().shared.borrow().placeholder,
            Some(Placeholder::NoMatches)
        );
        assert_eq!(
            a.inspected,
            Inspected::Hidden,
            "nothing listed, nothing inspected"
        );

        let a = app();
        let pid = a.state().selected_pid.unwrap();
        let before = a.state().selected_identity();
        if let Some(process) = a.state().model.processes.iter_mut().find(|p| p.pid == pid) {
            process.started_at += 1;
        }
        a.state().clear_recycled_selection(before);
        assert!(
            a.state().selected_pid.is_none(),
            "a recycled PID is not the selection"
        );
    }

    #[test]
    fn requests_from_the_panes_change_the_view() {
        let mut a = app();
        let shell = pid_named(&a, "shell");
        let push = |a: &MonitorApp, request: Request| a.state().shared.borrow_mut().push(request);

        push(&a, Request::Sort(SortKey::Mem));
        push(&a, Request::SetExpanded(shell, true));
        push(&a, Request::KernelThreads(true));
        a.apply_requests();
        assert_eq!(a.state().sort_key, SortKey::Mem);
        assert!(a.state().sort_desc);
        assert!(a.state().expanded.contains(&shell));
        assert!(a.state().prefs.show_kernel_threads);

        push(&a, Request::SumChildren(false));
        push(&a, Request::TreeMode(false));
        a.apply_requests();
        assert_eq!(a.state().sum_mode, SumMode::PerProcess);
        assert!(!a.state().tree_mode);

        push(&a, Request::ShowStat(Stat::Network));
        a.apply_requests();
        a.publish();
        assert_eq!(a.state().tab, Tab::Resources);
        assert_eq!(a.state().shared.borrow().reveal, Some(Stat::Network));
        assert!(
            a.inspected == Inspected::Hidden,
            "no inspector on Resources"
        );

        push(&a, Request::Tab(Tab::Processes));
        push(&a, Request::Filter("pipe".into()));
        a.apply_requests();
        a.publish();
        assert_eq!(a.state().tab, Tab::Processes);
        assert_eq!(a.state().filter, "pipe");
        assert_eq!(a.state().shared.borrow().filter, "pipe");

        a.state().set_cadence(Cadence::Paused);
        assert!(!a.state().prefs.auto_refresh);
        a.state().set_cadence(Cadence::Every(3000));
        assert!(a.state().prefs.auto_refresh);
        assert_eq!(a.state().prefs.interval_ms, 3000);

        push(&a, Request::Refresh);
        a.apply_requests();
        assert_eq!(taken(&a), Some(MonitorEffect::Refresh));

        let busiest = a.state().busiest_visible_pid().expect("a listed process");
        push(&a, Request::End(busiest));
        a.apply_requests();
        assert_eq!(
            taken(&a),
            Some(MonitorEffect::EndProcess {
                process: resource_id_for(&a, busiest)
            }),
            "the inspector's End process asks the host, which confirms it"
        );
    }

    #[test]
    fn row_menu_asks_for_the_process_effects() {
        let mut a = app();
        let shell = pid_named(&a, "shell");
        a.open_context_menu(
            shell,
            Point::new(100.0, 100.0),
            Rect::new(0.0, 84.0, 600.0, 116.0),
        );
        assert!(a.context.is_open());
        let labels: Vec<&str> = a
            .context
            .items()
            .iter()
            .filter_map(MenuItem::label)
            .collect();
        assert_eq!(
            labels,
            ["Expand", "Copy PID", "End process", "Kill process"],
            "a parent can be expanded from its menu"
        );
        let kill = a
            .context_choices
            .iter()
            .position(|choice| *choice == Some(MenuChoice::Kill))
            .unwrap();
        let before = a.state().model.processes.len();
        a.apply_menu_choice(kill);
        assert_eq!(
            a.state().model.processes.len(),
            before,
            "no signal without the host's confirmation"
        );
        assert_eq!(
            taken(&a),
            Some(MonitorEffect::Kill {
                process: resource_id_for(&a, shell)
            })
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn process_rows_are_bounded_generation_exact_and_local() {
        {
            let a = app();
            let pid = a.state().selected_pid.expect("sample selects a process");
            let id = resource_id_for(&a, pid);
            let process = a
                .state()
                .live_process_row(&id)
                .expect("selected process is a stable row")
                .into_resource();
            let kill = process
                .actions
                .iter()
                .find(|action| action.id == "kill")
                .expect("kill is exported");
            assert_eq!(
                kill.contract.confirmation,
                ui::tilo_actions::ConfirmationPolicy::Confirm
            );
            a.state()
                .answer(MonitorEffect::Kill { process: id })
                .expect("the confirmed kill executes");
            assert!(
                a.state()
                    .model
                    .processes
                    .iter()
                    .all(|process| process.pid != pid)
            );
            assert!(a.state().selected_pid.is_none());
        }

        {
            let a = app();
            let template = a.state().model.processes[0].clone();
            for offset in 0..=MAX_CONTEXT_PROCESS_RESOURCES {
                let mut process = template.clone();
                process.pid = 90_000 + u32::try_from(offset).unwrap();
                process.started_at = 7_000 + u64::try_from(offset).unwrap();
                process.name = format!("worker-{offset}");
                a.state().model.processes.push(process);
            }
            let target = a.state().model.processes.last().unwrap().clone();
            let rows = a.state().live_process_rows();
            assert_eq!(rows.len(), MAX_CONTEXT_PROCESS_RESOURCES);
            let target_id = process_resource_id(target.pid, target.started_at);
            assert!(
                !rows.iter().any(|row| row.id() == target_id),
                "a tail process is left off the eager page"
            );
            let resolved = a
                .state()
                .live_process_row(&target_id)
                .expect("an omitted exact PID generation stays resolvable")
                .into_resource();
            assert!(resolved.actions.iter().any(|action| action.id == "kill"));
        }

        {
            let a = app();
            let mut tail = a.state().model.processes[0].clone();
            tail.pid = 99_991;
            tail.started_at = 88_881;
            tail.name = "searchable-tail-worker".into();
            a.state().model.processes.push(tail.clone());

            let provider = ui::framework::testing::search_source::<crate::resource::Monitor>(
                a.state().process_search_entries(),
            );
            let hit = ui::search::SearchProvider::search_limited(&provider, "tail-worker", 1)
                .pop()
                .expect("the omitted catalogue tail remains globally searchable");
            assert_eq!(
                hit.action_id,
                process_resource_id(tail.pid, tail.started_at),
                "a hit is anchored at the process row"
            );
            assert!(
                a.state().live_process_row(&hit.action_id).is_some(),
                "the search address materializes the exact resource, not a PID-only fallback"
            );
        }

        {
            let mut process = app().state().model.processes[0].clone();
            process.name = format!(
                "line\nbreak\t{}",
                "x".repeat(MAX_PROCESS_RESOURCE_LABEL_CHARS + 8)
            );
            let label = process_resource_label(&process);
            assert!(!label.contains('\n') && !label.contains('\t'));
            assert!(label.chars().count() <= MAX_PROCESS_RESOURCE_LABEL_CHARS + 16);
        }

        {
            let a = app();
            let process = a.state().model.processes[0].clone();
            let result = a.state().answer(MonitorEffect::Kill {
                process: process_resource_id(process.pid, process.started_at + 1),
            });
            assert!(result.is_err());
            assert!(
                a.state()
                    .model
                    .processes
                    .iter()
                    .any(|p| p.pid == process.pid)
            );
        }

        {
            let mut a = app();
            let process = a.state().model.processes[3].clone();
            a.state()
                .answer(MonitorEffect::CopyPid {
                    process: process_resource_id(process.pid, process.started_at),
                })
                .unwrap();
            assert_eq!(a.state().selected_pid, Some(process.pid));
            a.answer_state();
            let copied = format!("Copied PID {}", process.pid);
            assert_eq!(a.shell.toast_text(), Some(copied.as_str()));
        }

        {
            let a = app();
            a.state().commit_tab(Tab::Resources);
            let process = a.state().model.processes[3].clone();
            a.state()
                .answer(MonitorEffect::Inspect {
                    process: process_resource_id(process.pid, process.started_at),
                })
                .unwrap();
            assert_eq!(a.state().selected_pid, Some(process.pid));
            assert_eq!(a.state().tab, Tab::Processes);
            assert!(
                a.state()
                    .answer(MonitorEffect::ShowTab {
                        tab: "graphs".into()
                    })
                    .is_err()
            );
        }

        {
            let mut first = app();
            let mut second = app();
            let mut doc = AutoCommit::new();
            let heads = doc.get_heads();
            first.state().set_cadence(Cadence::Paused);
            first.state().prefs.show_kernel_threads = true;
            first.state().commit_filter("shell".into());
            first.state().set_sort(SortKey::Name);
            first.sync(&doc, &automerge::ROOT);
            second.sync(&doc, &automerge::ROOT);
            assert_eq!(second.state().prefs, Prefs::default());
            assert_eq!(
                doc.get_heads(),
                heads,
                "nothing unhosted reaches the document"
            );
        }
    }

    #[test]
    fn window_lays_out_at_every_size() {
        for (viewport, placement) in [
            ((1280, 800), InspectorPlacement::Docked),
            ((800, 600), InspectorPlacement::Docked),
            ((480, 820), InspectorPlacement::Sheet),
        ] {
            let harness = window(app(), viewport);
            let monitor = harness.root::<MonitorApp>();
            assert_eq!(
                monitor.shell.geometry().inspector,
                placement,
                "inspector at {viewport:?}"
            );
            assert!(
                center_of(&harness, "tilo-fs").is_some(),
                "the process table lists rows at {viewport:?}"
            );
        }
    }

    fn press(harness: &mut FrameHarness, id: &str) {
        harness.press_window_slot(ui::window_toolbar::ToolbarEvent::Action { id: id.into() });
    }

    fn press_named(harness: &mut FrameHarness, name: &str) {
        harness.press_window_action(name).unwrap();
    }

    fn bar() -> ui::window_toolbar::WindowToolbar {
        ui::window_toolbar::declared().expect("the window declares its slots")
    }

    #[test]
    fn the_window_bar_switches_views_and_holds_the_lists_options() {
        let mut harness = window(app(), (390, 844));
        harness.present_in_window_bar();
        harness.relayout();
        let views = bar().views.expect("the tabs are the bar's views");
        assert_eq!(
            views
                .choices
                .iter()
                .map(|view| view.label.as_str())
                .collect::<Vec<_>>(),
            ["Processes", "Resources"]
        );
        assert_eq!(views.chosen, Tab::Processes.view_id());

        press(&mut harness, Tab::Resources.view_id());
        assert_eq!(harness.root::<MonitorApp>().state().tab, Tab::Resources);
        assert_eq!(bar().views.unwrap().chosen, Tab::Resources.view_id());
        assert!(
            bar()
                .actions
                .iter()
                .all(|action| action.name != "Tree view"),
            "the list's options leave with the list"
        );
        press(&mut harness, Tab::Processes.view_id());
        assert_eq!(harness.root::<MonitorApp>().state().tab, Tab::Processes);

        let cadence = bar()
            .actions
            .into_iter()
            .find(|action| action.name.starts_with("Update frequency"))
            .expect("the update frequency is a menu button");
        assert_eq!(cadence.items.len(), Cadence::all().len());
        press_named(&mut harness, "Paused");
        assert!(!harness.root::<MonitorApp>().state().prefs.auto_refresh);

        let tree = harness.root::<MonitorApp>().state().tree_mode;
        press_named(&mut harness, "Tree view");
        assert_eq!(harness.root::<MonitorApp>().state().tree_mode, !tree);
        press_named(&mut harness, "Kernel threads");
        assert!(
            harness
                .root::<MonitorApp>()
                .state()
                .prefs
                .show_kernel_threads
        );

        ui::widget::apply_filter(&mut *harness.root_mut::<MonitorApp>(), "cargo");
        harness.relayout();
        assert_eq!(harness.root::<MonitorApp>().state().filter, "cargo");
        assert!(
            center_of(&harness, "cargo").is_some(),
            "the match is listed"
        );
        assert!(center_of(&harness, "tilo-fs").is_none(), "the rest is not");
    }

    /// The window's controls act through the panes: the bar's views and
    /// cadence, a summary tile, the row menu and the tree keys.
    #[test]
    fn window_controls_change_the_view() {
        let mut harness = window(app(), (1280, 800));

        press(&mut harness, Tab::Resources.view_id());
        assert_eq!(harness.root::<MonitorApp>().state().tab, Tab::Resources);
        assert!(harness.inspection_json().unwrap().contains("Host name"));
        press(&mut harness, Tab::Processes.view_id());
        assert_eq!(harness.root::<MonitorApp>().state().tab, Tab::Processes);

        press_named(&mut harness, "Paused");
        assert!(!harness.root::<MonitorApp>().state().prefs.auto_refresh);

        let (x, y) = center_of(&harness, "tilo-fs").expect("tilo-fs row");
        harness.send(&ui::synthetic_input::cursor_moved(x, y));
        harness.send(&ui::synthetic_input::mouse_input(
            ElementState::Pressed,
            winit::event::MouseButton::Right,
        ));
        harness.relayout();
        let tilo_fs = pid_named(harness.root::<MonitorApp>(), "tilo-fs");
        assert_eq!(
            harness.root::<MonitorApp>().state().selected_pid,
            Some(tilo_fs)
        );
        assert!(harness.root::<MonitorApp>().context.is_open());
        let copy = {
            let monitor = harness.root::<MonitorApp>();
            let index = monitor
                .context_choices
                .iter()
                .position(|choice| *choice == Some(MenuChoice::Copy))
                .expect("the menu offers Copy PID");
            monitor.context.item_rect(index).expect("laid out")
        };
        harness.click_at(copy.center().x, copy.center().y);
        harness.relayout();
        let copy_pid = MonitorEffect::CopyPid {
            process: resource_id_for(harness.root::<MonitorApp>(), tilo_fs),
        };
        assert_eq!(
            harness.root::<MonitorApp>().state().requested.last(),
            Some(&copy_pid),
            "the window asks for the copy through the process's effect"
        );

        // ← and → fold the selected parent while the table has focus (the
        // row is where it was; the inspector now carries its name too).
        harness.click_at(x, y);
        harness.relayout();
        harness.key("ArrowRight");
        harness.relayout();
        assert!(
            harness
                .root::<MonitorApp>()
                .state()
                .expanded
                .contains(&tilo_fs)
        );
        harness.key("ArrowLeft");
        harness.relayout();
        assert!(
            !harness
                .root::<MonitorApp>()
                .state()
                .expanded
                .contains(&tilo_fs)
        );

        click(&mut harness, "Memory in use");
        assert_eq!(harness.root::<MonitorApp>().state().tab, Tab::Resources);
    }

    /// A machine as the session's service would describe it.
    #[cfg(target_os = "linux")]
    fn served_machine(notes_running: bool) -> tilo_system_stats::protocol::Snapshot {
        use tilo_system_stats::protocol::{
            CpuStats, HostInfo, MemoryStats, ProcessState, ProcessStats, Snapshot, SystemStats,
        };
        let process =
            |pid: u32, name: &str, app: Option<&str>, user: &str, cpu: f32| ProcessStats {
                pid,
                ppid: None,
                started_at: u64::from(pid) * 7,
                name: name.into(),
                app: app.map(str::to_owned),
                uid: if user == "root" { 0 } else { 1000 },
                user: user.into(),
                cpu,
                memory_bytes: 300 << 20,
                virtual_bytes: 900 << 20,
                threads: 4,
                state: ProcessState::Running,
                run_secs: 60,
                kernel_thread: false,
                command: None,
                endable: app.is_some(),
            };
        let mut processes = vec![process(700, "sshd", None, "root", 0.5)];
        if notes_running {
            processes.push(process(4242, "notes", Some("so.tilo.notes"), "ada", 12.0));
            processes.push(process(
                4250,
                "notes-renderer",
                Some("so.tilo.notes"),
                "ada",
                30.0,
            ));
        }
        Snapshot {
            taken_ms: 1,
            window_ms: 1500,
            system: Some(SystemStats {
                host: HostInfo {
                    hostname: "ada-laptop".into(),
                    kernel: "7.0.9".into(),
                    cpu_model: Some("Test CPU 9000".into()),
                    logical_cpus: 4,
                    uptime_secs: 3600,
                },
                cpu: Some(CpuStats {
                    total: 40.0,
                    per_core: vec![70.0, 50.0, 30.0, 10.0],
                }),
                load_average: [1.5, 1.0, 0.5],
                memory: MemoryStats {
                    total: 16 << 30,
                    available: 4 << 30,
                    swap_total: 2 << 30,
                    swap_free: 1 << 30,
                },
                ..SystemStats::default()
            }),
            apps: Vec::new(),
            processes,
            processes_included: true,
        }
    }

    /// A service on the far end of a socket pair: every subscription is
    /// answered with the machine, and End app ends the app.
    #[cfg(target_os = "linux")]
    fn fake_service() -> tilo_system_stats::client::Client {
        use tilo_system_stats::protocol::{Reply, Request, read_message, write_message};
        let (app, mut service) = std::os::unix::net::UnixStream::pair().unwrap();
        std::thread::spawn(move || {
            let mut notes_running = true;
            while let Ok(Some(request)) = read_message::<_, Request>(&mut service) {
                let reply = match request {
                    Request::Subscribe { subscription, .. } => Reply::Snapshot {
                        subscription,
                        snapshot: Box::new(served_machine(notes_running)),
                    },
                    Request::EndApp { request, app, .. } if app == "so.tilo.notes" => {
                        notes_running = false;
                        Reply::Ended { request, ended: 2 }
                    }
                    Request::EndApp { request, .. } | Request::EndProcess { request, .. } => {
                        Reply::Refused {
                            request,
                            refusal: tilo_system_stats::protocol::Refusal::NotAnApp,
                        }
                    }
                    _ => continue,
                };
                if write_message(&mut service, &reply).is_err() {
                    break;
                }
            }
        });
        tilo_system_stats::client::Client::from_stream(app).unwrap()
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_monitor_shows_the_processes_and_stats_the_session_serves() {
        let live = MonitorApp::live_over(Sampler::with_client(fake_service()));
        let mut harness = window(live, (1280, 800));
        assert!(
            center_of(&harness, "notes-renderer").is_some(),
            "the served process list is on screen"
        );
        assert!(
            center_of(&harness, "sshd").is_some(),
            "with the system's services"
        );
        {
            let monitor = harness.root::<MonitorApp>();
            assert_eq!(
                monitor.state().model.process_status,
                ProcessDataStatus::Ready
            );
            assert_eq!(monitor.state().model.metrics.host, "ada-laptop");
            assert_eq!(
                monitor.state().model.metrics.cores,
                vec![70.0, 50.0, 30.0, 10.0]
            );
            assert_eq!(monitor.state().model.metrics.mem_used, 12 << 30);
            let state = monitor.state();
            let notes = state
                .model
                .processes
                .iter()
                .find(|p| p.name == "notes")
                .expect("notes is listed");
            assert_eq!(notes.app.as_deref(), Some("so.tilo.notes"));
            assert_eq!(notes.user, "ada");
        }

        press(&mut harness, Tab::Resources.view_id());
        let resources = harness.inspection_json().unwrap();
        assert!(resources.contains("ada-laptop"), "the served host");
        assert!(resources.contains("Test CPU 9000"), "the served processor");
        assert!(resources.contains("Linux 7.0.9"), "the served kernel");
        press(&mut harness, Tab::Processes.view_id());

        harness
            .root_mut::<MonitorApp>()
            .state()
            .answer(MonitorEffect::EndApp {
                app: "so.tilo.notes".into(),
            })
            .expect("the service ended it");
        harness.relayout();
        assert!(center_of(&harness, "notes-renderer").is_none(), "ended");
        assert!(center_of(&harness, "sshd").is_some());
        assert!(
            harness
                .root_mut::<MonitorApp>()
                .state()
                .answer(MonitorEffect::EndApp {
                    app: "so.tilo.notes".into(),
                })
                .is_err(),
            "an app that is not running is refused"
        );
    }
}
