//! The Monitor root widget: a two-tab activity monitor (Processes /
//! Resources) built from the window shell and toolkit widgets.
//!
//! The window is one [`NavShell`]. The window bar holds the tabs as its
//! views, Refresh, the sampling cadence and the list's view options, and its
//! search narrows the process list (`window_bar`). The content is the active tab
//! (`crate::view::ProcessesPane` or `crate::view::ResourcesPane`); the
//! selected process is the inspector. The shared table layout (sort, tree
//! mode) lives on [`crate::resource::Monitor`]; the tab, filter and
//! selection are this window's own; the live process snapshot never leaves
//! this machine, and the person's preferences
//! ([`crate::prefs::MonitorPreferences`]) are a personal settings section.

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

use ui::framework::{Dispatcher, LiveRow, Setting};
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

/// A process is only the same action resource for the lifetime identified by
/// its kernel start-time tick.  In particular, do not use a bare PID here:
/// PIDs can be recycled while a palette entry, history item, or collaborator's
/// action is waiting to execute.
fn process_resource_id(pid: u32, started_at: u64) -> String {
    format!("process:{pid}:started:{started_at}")
}

/// Decode the canonical, wire-safe process resource id. Keep parsing
/// deliberately strict so a malformed target can never silently fall back to
/// a PID-only action.
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

    fn label(self) -> &'static str {
        match self {
            Self::Processes => "Processes",
            Self::Resources => "Resources",
        }
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
    /// Numeric columns default to descending (biggest first); text
    /// columns default to ascending.
    fn default_desc(self) -> bool {
        !matches!(self, SortKey::Name | SortKey::User)
    }
}

/// A process table can contain thousands of kernel entries. Keep the
/// contextual resource projection small; exact process resources remain
/// resolvable on demand through [`Widget::resource`].
const MAX_CONTEXT_PROCESS_RESOURCES: usize = 64;
/// Process names originate with the operating system and are rendered in
/// global/remote action surfaces. Bound and flatten them before publishing so
/// an unusual process name cannot force unbounded text shaping or line breaks.
const MAX_PROCESS_RESOURCE_LABEL_CHARS: usize = 160;

/// Sample depth of the per-process CPU sparkline shown in the inspector —
/// a shorter window than the whole-system charts so the line stays readable.
const PROC_SPARK_CAP: usize = 48;

/// A stable one-line label for a process resource. The identity stays in the
/// opaque PID-generation id; this display label is deliberately bounded.
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

/// One exact, PID-generation-bound process row. This is shared by the
/// bounded contextual catalogue and lazy direct resolution, keeping every
/// visible, searched, and agent-invoked action contract identical.
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

/// `value` as one line of searchable text, bounded like a published label.
fn indexed_text(value: &str) -> String {
    value
        .chars()
        .take(MAX_PROCESS_RESOURCE_LABEL_CHARS)
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect()
}

/// `process` as a search hit: found by its name, user or PID, and opening
/// the exact PID generation it names.
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

/// What the inspector currently shows.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Inspected {
    /// Not built for the current state yet.
    Stale,
    /// Nothing: the Resources tab, or no processes to inspect.
    Hidden,
    /// The "no process selected" state.
    Nothing,
    /// A process, by a fingerprint of everything its detail shows.
    Process(u64),
}

/// The first child of `node`.
fn first_child(taffy: &TaffyTree<()>, node: NodeId) -> Option<NodeId> {
    taffy.children(node).ok()?.first().copied()
}

/// The content column: the active tab's pane.
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
    reason = "independent flags: sampling gate, publishing, layout and a capture's reveal"
)]
pub struct MonitorApp {
    model: MonitorModel,
    sampler: Sampler,
    /// Retained global-search/directory index. Its entries carry the same
    /// PID-generation ids as every resource action and update only per sample.
    prefs: Prefs,
    /// The person's Monitor preferences when hosted, and the value last
    /// stored, which tells a change made here from one made elsewhere.
    preferences: Option<(Setting<MonitorPreferences>, MonitorPreferences)>,

    tab: Tab,
    sort_key: SortKey,
    sort_desc: bool,
    /// Tree view (parent→child with disclosure) vs a flat sorted list.
    tree_mode: bool,
    /// Whether a parent row shows its summed subtree usage or only its own.
    sum_mode: SumMode,
    /// pids the person has expanded in the tree (collapsed otherwise).
    expanded: HashSet<u32>,
    filter: String,
    selected_pid: Option<u32>,
    /// Rolling per-process CPU history, keyed by the kernel process generation,
    /// feeding the inspector's sparkline.
    proc_cpu_hist: HashMap<(u32, u64), RollingSeries>,

    /// The row menu, mounted in the window's top layer by this widget.
    context: ContextMenu,
    context_pid: Option<u32>,
    context_choices: Vec<Option<MenuChoice>>,

    last_refresh: Instant,
    /// Never sample the live machine — the seeded snapshot is the whole model.
    sample_only: bool,

    rect: Rect,
    shell: NavShell,
    shared: Shared,
    shown_tab: Rc<std::cell::Cell<Tab>>,
    /// Model or view state changed since the panes were last told.
    stale: bool,
    metrics_revision: u64,
    published_metrics: Option<u64>,
    published_rows: Option<u64>,
    inspected: Inspected,
    /// Open the inspector drawer once laid out (`detail` captures).
    reveal_detail: bool,
    /// How many layouts the window has had, so a capture's drawer opens
    /// over content that has settled at its width.
    layouts: u32,
    layout_dirty: bool,
    /// The views were handed something since the host last took the
    /// window's structure ([`Widget::take_structure_dirty`]), which takes it
    /// once: the layout it asks for clears `layout_dirty` on its own.
    retold: bool,
    /// The door edits and effects go through once the framework hosts the
    /// surface: the table layout as Monitor's actions, and every control
    /// that reaches a process as a [`MonitorEffect`], so the host checks its
    /// permission and confirmation as it does for the palette and agents.
    dispatcher: Option<Dispatcher<Monitor>>,
    /// The effects the controls asked for, as a test reads them.
    #[cfg(test)]
    requested: Vec<MonitorEffect>,
}

impl Default for MonitorApp {
    fn default() -> Self {
        Self::new()
    }
}

impl MonitorApp {
    /// Construct with the deterministic, fully-populated sample snapshot. This
    /// is what `--shots` and the tests render: a capture never ticks the
    /// sampler, so the seeded sample is what appears — hermetic and
    /// reproducible offline. The live window uses [`Self::new_live`] instead.
    pub fn new() -> Self {
        Self::sample(MonitorModel::sample())
    }

    /// Construct over `model`, a deterministic snapshot that is never
    /// sampled again: what a screenshot shows.
    pub fn sample(model: MonitorModel) -> Self {
        let mut app = Self::build(model, ui::time::now(), Sampler::inert());
        app.sample_only = true;
        // Select a row from the default (collapsed) tree rather than the
        // globally busiest process, which may be a hidden descendant: the
        // inspector's actions must never refer to a process the person
        // cannot see in the table.
        app.selected_pid = app.busiest_visible_pid();
        app.seed_selected_spark();
        app.publish();
        app
    }

    /// Construct for a past version of the table layout: no processes, and
    /// nothing that reads this machine.
    pub fn past() -> Self {
        let mut app = Self::build(MonitorModel::empty(), ui::time::now(), Sampler::inert());
        app.sample_only = true;
        app.publish();
        app
    }

    /// Construct for the live window: start from an empty model and sample
    /// the machine on the first frame, so the window shows the person's
    /// actual processes rather than fabricated sample rows.
    pub fn new_live() -> Self {
        let stale = ui::time::now()
            .checked_sub(Duration::from_secs(3600))
            .unwrap_or_else(Instant::now);
        let mut app = Self::build(MonitorModel::empty(), stale, Sampler::new());
        app.publish();
        app
    }

    fn build(model: MonitorModel, last_refresh: Instant, sampler: Sampler) -> Self {
        let shared: Shared = Rc::new(RefCell::new(Bridge::default()));
        let shown_tab = Rc::new(std::cell::Cell::new(Tab::Processes));
        let prefs = Prefs::default();
        let shell = NavShell::new()
            .content_kind(ui::widget::ContentKind::List)
            .with_id("monitor.shell")
            .with_window_toolbar()
            .content(Box::new(tab_content(&shared, &shown_tab)));
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
            context: ContextMenu::new(),
            context_pid: None,
            context_choices: Vec::new(),
            last_refresh,
            sample_only: false,
            rect: Rect::ZERO,
            shell,
            shared,
            shown_tab,
            stale: true,
            metrics_revision: 0,
            published_metrics: None,
            published_rows: None,
            inspected: Inspected::Stale,
            reveal_detail: false,
            layouts: 0,
            layout_dirty: true,
            retold: true,
            dispatcher: None,
            #[cfg(test)]
            requested: Vec::new(),
        }
    }

    /// Take the machine's newest reading. Where the session pushes them
    /// (Linux), each arrival wakes the window and is taken here; elsewhere
    /// the machine is read at most once per cadence. A capture never trips
    /// this, so `--shots` shows the sample snapshot.
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
                // The first frame waits for the first reading, which the
                // session sends at once: the window opens on the machine,
                // not on a loading state.
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

    /// Read the machine now, as the person (or an agent) asked.
    fn refresh_live_snapshot(&mut self) {
        let selected_before = self.selected_identity();
        self.sampler.refresh_now(&mut self.model);
        self.after_refresh(selected_before);
    }

    /// A new reading landed: keep the identity boundary of the selected
    /// detail. If the OS recycled a selected PID between readings, leave no
    /// action surface silently pointed at the replacement process.
    fn after_refresh(&mut self, selected_before: Option<(u32, u64)>) {
        self.last_refresh = ui::time::now();
        self.clear_recycled_selection(selected_before);
        self.record_proc_cpu();
        self.metrics_changed();
        self.clear_selection_if_filtered_out();
    }

    /// Wake the window with `waker` when a pushed reading arrives.
    pub(crate) fn wake_with(&mut self, waker: ui::WakeHandle) {
        if !self.sample_only {
            self.sampler.set_waker(waker);
        }
    }

    /// The live window over `sampler`'s readings.
    #[cfg(test)]
    pub(crate) fn live_over(sampler: Sampler) -> Self {
        let stale = ui::time::now()
            .checked_sub(Duration::from_secs(3600))
            .unwrap_or_else(Instant::now);
        let mut app = Self::build(MonitorModel::empty(), stale, sampler);
        app.publish();
        app
    }

    fn metrics_changed(&mut self) {
        self.metrics_revision = self.metrics_revision.wrapping_add(1);
        self.stale = true;
    }

    /// Clear a selection when the process snapshot no longer contains the
    /// exact lifetime that was selected before it was refreshed.
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

    /// Fold this refresh's per-process CPU into the sparkline histories, then
    /// prune rings for processes that have since exited.
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

    /// Seed the selected process's CPU ring with a wave around its present
    /// load, so the sample snapshot's inspector has a history to draw.
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
                // Wander around the current reading in proportion to it.
                (base * (1.0 + 0.35 * (t * 0.31).sin() + 0.15 * (t * 0.73).cos())).max(0.0)
            }),
        );
        self.proc_cpu_hist.insert((pid, p.started_at), ring);
        self.stale = true;
    }

    /// The filtered and sorted flat process list.
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

    /// The rows the process table lists. In tree mode this is the flattened
    /// parent→child forest (honouring collapse state, the sum mode, sort,
    /// kernel preference and filter); in flat mode the sorted list at depth 0.
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

    /// How many processes the list can show before the filter narrows it.
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

    /// Identity of the current selection at this instant.
    fn selected_identity(&self) -> Option<(u32, u64)> {
        self.selected_process().map(|p| (p.pid, p.started_at))
    }

    /// A filter or a collapse is a view boundary: never leave an actionable
    /// inspector for a process that is no longer listed.
    /// The listed rows changed, so the views must hear of it.
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

    /// The busiest process currently listed.
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

    /// Follow the person's Monitor preferences: this window starts from
    /// them, stores what it changes, and takes what another window or device
    /// changed.
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

    /// Whether the stored preferences moved since this window last looked:
    /// another window or device changed them, or this one did.
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

    /// Adopt the shared table layout; never touch the process catalogue.
    pub(crate) fn adopt_resource_state(&mut self, state: &Monitor) {
        let layout = (state.sort, !state.ascending, !state.flat);
        if (self.sort_key, self.sort_desc, self.tree_mode) == layout {
            return;
        }
        (self.sort_key, self.sort_desc, self.tree_mode) = layout;
        self.clear_selection_if_filtered_out();
    }

    /// Submit edits of the shared table layout, as one step.
    fn commit_layout(&self, edits: Vec<ui::tilo_resource::Edit<Monitor>>) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.group(|| {
                for edit in edits {
                    dispatcher.submit(edit);
                }
            });
        }
    }

    /// What a search hit or an agent's `open` runs: inspect the process
    /// row its anchor names.
    pub(crate) fn opener(&self) -> impl FnMut(&str) -> Result<(), &'static str> + 'static {
        let shared = Rc::clone(&self.shared);
        move |anchor| {
            parse_process_resource_id(anchor).ok_or("not a process of this machine")?;
            shared.borrow_mut().push(Request::Open(anchor.to_owned()));
            Ok(())
        }
    }

    /// Ask the host to run `effect` through the same door as the palette
    /// and agents: its permission and confirmation are checked before the
    /// surface answers it.
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

    /// The bounded contextual page of live process rows; any other process
    /// resolves by id ([`Self::live_process_row`]).
    pub(crate) fn live_process_rows(&self) -> Vec<LiveRow> {
        self.model
            .processes
            .iter()
            .take(MAX_CONTEXT_PROCESS_RESOURCES)
            .map(process_row)
            .collect()
    }

    /// Every process running here as a search hit.
    pub(crate) fn process_search_entries(&self) -> Vec<ui::tilo_resource::SearchEntry> {
        self.model
            .processes
            .iter()
            .map(process_search_entry)
            .collect()
    }

    /// The live row of the exact process generation `id` names.
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
                    // Resuming starts a fresh cadence window rather than an
                    // immediate surprise sample.
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

    /// Ask for the effect `control` builds on the current lifetime of `pid`,
    /// the same address the palette and agents reach it at.
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

    /// The live process `id` names, if it still runs.
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

    /// Answer `effect`, however it was invoked: a control here, the
    /// palette, an agent. The host has already checked its permission and
    /// had a destructive one confirmed.
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
                self.reveal_inspector();
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

    /// End every process of `app` the person may end.
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

    /// Ask for End app on the app `pid` belongs to.
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

    /// Dispatch the real signal and reflect it in the local model. Live: the
    /// sampler sends the actual SIGTERM/SIGKILL, then rebuilds the snapshot;
    /// dispatch acceptance is not proof that a process has exited, so only
    /// that fresh snapshot may remove the row. Sample: the pid isn't a real
    /// process, so drop the seeded row locally.
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

    /// Complete the clipboard effect after the action broker has resolved
    /// the current process resource and its availability contract.
    fn copy_pid(&mut self, pid: u32) {
        ui::clipboard_copy(&pid.to_string());
        self.shell
            .toast(format!("Copied PID {pid}"), ToastKind::Success);
        self.selected_pid = Some(pid);
        self.stale = true;
    }

    /// Open the row menu for `pid` at `at`, marking its row.
    fn open_context_menu(&mut self, pid: u32, at: Point, row: Rect) {
        let mut menu = ContextMenu::new();
        let mut choices = Vec::new();
        let mut push = |item: MenuItem, choice: Option<MenuChoice>| {
            menu.push(item);
            choices.push(choice);
        };
        // "Show details" only earns a place where the inspector is a closed
        // drawer; beside the table the right-click already showed them.
        if self.shell.geometry().inspector == InspectorPlacement::Drawer
            && !self.shell.is_inspector_open()
        {
            push(
                MenuItem::selection("Show details"),
                Some(MenuChoice::ShowDetails),
            );
        }
        if self.tree_mode && self.row_has_children(pid) {
            let open = !self.expanded.contains(&pid);
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
        if self
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
        match choice {
            MenuChoice::ShowDetails => {
                self.commit_selected(Some(pid));
                self.reveal_inspector();
            }
            MenuChoice::Expand(open) => self.set_expanded(pid, open),
            MenuChoice::Copy => {
                self.request_process_control(pid, |process| MonitorEffect::CopyPid { process });
            }
            MenuChoice::End => {
                self.request_process_control(pid, |process| MonitorEffect::EndProcess { process });
            }
            MenuChoice::Kill => {
                self.request_process_control(pid, |process| MonitorEffect::Kill { process });
            }
            MenuChoice::EndApp => self.request_end_app(pid),
        }
        self.context_pid = None;
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

    /// Hand the current model, view state and selection to the views.
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
        self.show_inspector();
        self.layout_dirty = true;
        self.retold = true;
    }

    /// The inspector follows the selection on the Processes tab; it is gone
    /// on the Resources tab and when there is nothing to list.
    fn show_inspector(&mut self) {
        let wanted = self.tab == Tab::Processes
            && self.model.process_status == ProcessDataStatus::Ready
            && !self.shared.borrow().rows.is_empty();
        if !wanted {
            if self.inspected != Inspected::Hidden {
                self.inspected = Inspected::Hidden;
                self.shell.set_inspector(None);
            }
            return;
        }
        let Some(process) = self.selected_process().cloned() else {
            if self.inspected != Inspected::Nothing {
                self.inspected = Inspected::Nothing;
                self.shell
                    .set_inspector(Some(crate::view::process_detail(None)));
                self.shell.set_inspector_title("");
            }
            return;
        };
        let history: Vec<f32> = self
            .proc_cpu_hist
            .get(&(process.pid, process.started_at))
            .map(|ring| ring.as_slice().to_vec())
            .unwrap_or_default();
        let parent = process.ppid.and_then(|ppid| {
            self.model
                .processes
                .iter()
                .find(|p| p.pid == ppid)
                .map(|p| p.name.clone())
        });
        // A summed row reads more than the process alone; the inspector names
        // both, so the row and the detail never seem to disagree.
        let with_children = if self.tree_mode && self.sum_mode == SumMode::SumSubtree {
            self.shared
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
            requests: Rc::clone(&self.shared),
        };
        self.shell
            .set_inspector(Some(crate::view::process_detail(Some(&detail))));
        self.shell.set_inspector_title(process.name.clone());
    }

    /// Open the inspector where it is a closed drawer, so choosing a process
    /// on a narrow window shows it.
    fn reveal_inspector(&mut self) {
        if self.shell.geometry().inspector == InspectorPlacement::Drawer
            && !self.shell.is_inspector_open()
        {
            self.shell.toggle_inspector();
        }
    }

    /// Turn the views' requests into view changes, layout edits and effects.
    /// The views hear of them in the next `sync` or layout: telling them
    /// rebuilds the inspector, which must never happen while
    /// the host is routing an event through the tree it holds.
    fn apply_requests(&mut self) {
        let requests = self.shared.borrow_mut().take_requests();
        for request in requests {
            match request {
                Request::Tab(tab) => self.commit_tab(tab),
                Request::Sort(key) => self.set_sort(key),
                Request::Filter(query) => self.commit_filter(query),
                Request::Select(pid) => {
                    self.commit_selected(Some(pid));
                    self.reveal_inspector();
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
                        self.reveal_inspector();
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
                    self.open_context_menu(pid, at, row);
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

    /// Stage the view state of screenshot `scenario` over the sample
    /// machine its fixture provides ([`MonitorModel::scenario`]) and the
    /// table layout its actions set ([`crate::resource::MonitorShell`]): a
    /// scene a single captured frame cannot reach through live input (an
    /// open menu, the inspector drawer, an expanded tree). Unknown names
    /// leave the default frame.
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
    /// Keyboard commands of the window: Ctrl+R refreshes, Ctrl+1 and Ctrl+2
    /// switch tabs, Delete kills the selected process (through its confirmed
    /// action). Returns whether the key was consumed.
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
        // The preferences follow the person's settings, never the shared
        // document.
        self.sync_preferences();
        self.maybe_refresh();
        self.take_window_toolbar_events();
        self.apply_requests();
        self.publish();
        self.declare_window_toolbar();
        self.shell.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        // The window bar's slots are pressed between events: carry out what
        // they asked.
        let asked = self.take_window_toolbar_events();
        if asked {
            self.apply_requests();
        }
        let changed = self.stale;
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
        // Open the drawer once the content behind it has settled at its
        // width, so the window under the drawer is the one the person sees.
        if self.reveal_detail
            && self.shell.geometry().inspector != InspectorPlacement::None
            && self.layouts > 2
        {
            self.reveal_detail = false;
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
            .draw(scene, transform, text_renderer, theme, presence);
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
            self.handle_shortcut(event, ctx);
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
        self.layout_dirty
            || self.stale
            || self.preferences_changed()
            || self.shell.is_layout_dirty()
    }

    fn layout_needs_measure(&self) -> bool {
        self.is_layout_dirty() || self.shell.layout_needs_measure()
    }

    fn take_structure_dirty(&mut self) -> bool {
        // What changed since the last frame reaches the views with the next
        // tree; the shell reports its own changes.
        // A sync that rebuilt the inspector (`retold`, taken here)
        // and preferences another window or device changed count too.
        let retold = std::mem::take(&mut self.retold);
        let pending = self.stale || retold || self.preferences_changed();
        self.shell.take_structure_dirty() || pending
    }

    fn needs_animation_frame(&self) -> bool {
        self.shell.needs_animation_frame()
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        let mut next = self.shell.next_wake_deadline();
        let mut consider = |deadline: Instant| {
            next = Some(next.map_or(deadline, |current: Instant| current.min(deadline)));
        };
        // Pushed readings wake the window themselves; only a machine read
        // on the window's own cadence needs a deadline.
        if !self.sample_only && self.prefs.auto_refresh && !self.sampler.is_pushed() {
            consider(
                self.last_refresh
                    .checked_add(Duration::from_millis(self.prefs.interval_ms))
                    .unwrap_or_else(Instant::now),
            );
        }
        next
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

    /// The effect the app's controls last asked the host for.
    fn taken(a: &mut MonitorApp) -> Option<MonitorEffect> {
        a.requested.pop()
    }

    fn pid_named(a: &MonitorApp, name: &str) -> u32 {
        a.model
            .processes
            .iter()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("sample process {name}"))
            .pid
    }

    fn resource_id_for(a: &MonitorApp, pid: u32) -> String {
        let process = a
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

    /// The centre of the first widget whose test id or accessible name is
    /// `name`, or of the first accessible node naming it or holding it as
    /// its value (a table cell), among those on screen.
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

    /// The tree folds, sums and filters; the flat list sorts; kernel threads
    /// are hidden until asked for.
    #[test]
    fn process_list_sorts_filters_and_folds_the_tree() {
        {
            let a = app();
            let rows = a.visible_rows();
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
            let mut a = app();
            let shell = pid_named(&a, "shell");
            let summed = a.visible_rows()[0].cpu;
            a.toggle_expand(shell);
            let rows = a.visible_rows();
            assert!(rows[0].expanded);
            assert!(rows[1].depth == 1, "the shell's children follow it");
            a.sum_mode = SumMode::PerProcess;
            let own = a
                .visible_rows()
                .into_iter()
                .find(|row| row.pid == shell)
                .unwrap()
                .cpu;
            assert!(own < summed, "a parent shows only its own load");
        }

        {
            let mut a = app();
            a.filter = "rustc".into();
            let rows = a.visible_rows();
            assert_eq!(names(&rows), ["shell", "code", "cargo", "rustc"]);
        }

        {
            let mut a = app();
            a.commit_tree_mode(false);
            a.set_sort(SortKey::Name);
            let rows = a.visible_rows();
            assert!(!a.sort_desc, "text sorts ascending first");
            assert!(
                rows.windows(2)
                    .all(|pair| { pair[0].name.to_lowercase() <= pair[1].name.to_lowercase() })
            );
            a.prefs.show_kernel_threads = true;
            assert!(
                names(&a.visible_rows())
                    .iter()
                    .any(|name| name.starts_with("kworker"))
            );
        }
    }

    /// A selection names one listed process lifetime; filtering it away or
    /// recycling its PID clears it.
    #[test]
    fn selection_follows_the_listed_process_lifetime() {
        let mut a = app();
        let selected = a.selected_pid.expect("the sample selects a process");
        assert!(a.visible_rows().iter().any(|row| row.pid == selected));

        a.commit_filter("printer queue".into());
        assert!(a.selected_pid.is_none(), "filtered away");
        a.publish();
        assert_eq!(a.shared.borrow().placeholder, Some(Placeholder::NoMatches));
        assert_eq!(
            a.inspected,
            Inspected::Hidden,
            "nothing listed, nothing inspected"
        );

        let mut a = app();
        let pid = a.selected_pid.unwrap();
        let before = a.selected_identity();
        if let Some(process) = a.model.processes.iter_mut().find(|p| p.pid == pid) {
            process.started_at += 1;
        }
        a.clear_recycled_selection(before);
        assert!(
            a.selected_pid.is_none(),
            "a recycled PID is not the selection"
        );
    }

    /// What the panes ask for becomes view state, layout edits and effects.
    #[test]
    fn requests_from_the_panes_change_the_view() {
        let mut a = app();
        let shell = pid_named(&a, "shell");
        let push = |a: &MonitorApp, request: Request| a.shared.borrow_mut().push(request);

        push(&a, Request::Sort(SortKey::Mem));
        push(&a, Request::SetExpanded(shell, true));
        push(&a, Request::KernelThreads(true));
        a.apply_requests();
        assert_eq!(a.sort_key, SortKey::Mem);
        assert!(a.sort_desc);
        assert!(a.expanded.contains(&shell));
        assert!(a.prefs.show_kernel_threads);

        push(&a, Request::SumChildren(false));
        push(&a, Request::TreeMode(false));
        a.apply_requests();
        assert_eq!(a.sum_mode, SumMode::PerProcess);
        assert!(!a.tree_mode);

        push(&a, Request::ShowStat(Stat::Network));
        a.apply_requests();
        a.publish();
        assert_eq!(a.tab, Tab::Resources);
        assert_eq!(a.shared.borrow().reveal, Some(Stat::Network));
        assert!(
            a.inspected == Inspected::Hidden,
            "no inspector on Resources"
        );

        push(&a, Request::Tab(Tab::Processes));
        push(&a, Request::Filter("pipe".into()));
        a.apply_requests();
        a.publish();
        assert_eq!(a.tab, Tab::Processes);
        assert_eq!(a.filter, "pipe");
        assert_eq!(a.shared.borrow().filter, "pipe");

        a.set_cadence(Cadence::Paused);
        assert!(!a.prefs.auto_refresh);
        a.set_cadence(Cadence::Every(3000));
        assert!(a.prefs.auto_refresh);
        assert_eq!(a.prefs.interval_ms, 3000);

        push(&a, Request::Refresh);
        a.apply_requests();
        assert_eq!(taken(&mut a), Some(MonitorEffect::Refresh));

        let busiest = a.busiest_visible_pid().expect("a listed process");
        push(&a, Request::End(busiest));
        a.apply_requests();
        assert_eq!(
            taken(&mut a),
            Some(MonitorEffect::EndProcess {
                process: resource_id_for(&a, busiest)
            }),
            "the inspector's End process asks the host, which confirms it"
        );
    }

    /// The row menu offers the process's actions and asks for their
    /// effects; it never signals by itself.
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
        let before = a.model.processes.len();
        a.apply_menu_choice(kill);
        assert_eq!(
            a.model.processes.len(),
            before,
            "no signal without the host's confirmation"
        );
        assert_eq!(
            taken(&mut a),
            Some(MonitorEffect::Kill {
                process: resource_id_for(&a, shell)
            })
        );
    }

    /// Process rows are generation-exact, bounded and contract-confirmed.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn process_rows_are_bounded_generation_exact_and_local() {
        {
            let mut a = app();
            let pid = a.selected_pid.expect("sample selects a process");
            let process = a
                .live_process_row(&resource_id_for(&a, pid))
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
            a.answer(MonitorEffect::Kill {
                process: resource_id_for(&a, pid),
            })
            .expect("the confirmed kill executes");
            assert!(a.model.processes.iter().all(|process| process.pid != pid));
            assert!(a.selected_pid.is_none());
        }

        {
            let mut a = app();
            let template = a.model.processes[0].clone();
            for offset in 0..=MAX_CONTEXT_PROCESS_RESOURCES {
                let mut process = template.clone();
                process.pid = 90_000 + u32::try_from(offset).unwrap();
                process.started_at = 7_000 + u64::try_from(offset).unwrap();
                process.name = format!("worker-{offset}");
                a.model.processes.push(process);
            }
            let target = a.model.processes.last().unwrap().clone();
            let rows = a.live_process_rows();
            assert_eq!(rows.len(), MAX_CONTEXT_PROCESS_RESOURCES);
            let target_id = process_resource_id(target.pid, target.started_at);
            assert!(
                !rows.iter().any(|row| row.id() == target_id),
                "a tail process is left off the eager page"
            );
            let resolved = a
                .live_process_row(&target_id)
                .expect("an omitted exact PID generation stays resolvable")
                .into_resource();
            assert!(resolved.actions.iter().any(|action| action.id == "kill"));
        }

        {
            let mut a = app();
            let mut tail = a.model.processes[0].clone();
            tail.pid = 99_991;
            tail.started_at = 88_881;
            tail.name = "searchable-tail-worker".into();
            a.model.processes.push(tail.clone());

            let provider = ui::framework::testing::search_source::<crate::resource::Monitor>(
                a.process_search_entries(),
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
                a.live_process_row(&hit.action_id).is_some(),
                "the search address materializes the exact resource, not a PID-only fallback"
            );
        }

        {
            let mut process = app().model.processes[0].clone();
            process.name = format!(
                "line\nbreak\t{}",
                "x".repeat(MAX_PROCESS_RESOURCE_LABEL_CHARS + 8)
            );
            let label = process_resource_label(&process);
            assert!(!label.contains('\n') && !label.contains('\t'));
            assert!(label.chars().count() <= MAX_PROCESS_RESOURCE_LABEL_CHARS + 16);
        }

        {
            let mut a = app();
            let process = a.model.processes[0].clone();
            let result = a.answer(MonitorEffect::Kill {
                process: process_resource_id(process.pid, process.started_at + 1),
            });
            assert!(result.is_err());
            assert!(a.model.processes.iter().any(|p| p.pid == process.pid));
        }

        {
            let mut a = app();
            let process = a.model.processes[3].clone();
            a.answer(MonitorEffect::CopyPid {
                process: process_resource_id(process.pid, process.started_at),
            })
            .unwrap();
            assert_eq!(a.selected_pid, Some(process.pid));
            let copied = format!("Copied PID {}", process.pid);
            assert_eq!(a.shell.toast_text(), Some(copied.as_str()));
        }

        {
            let mut a = app();
            a.commit_tab(Tab::Resources);
            let process = a.model.processes[3].clone();
            a.answer(MonitorEffect::Inspect {
                process: process_resource_id(process.pid, process.started_at),
            })
            .unwrap();
            assert_eq!(a.selected_pid, Some(process.pid));
            assert_eq!(a.tab, Tab::Processes);
            assert!(
                a.answer(MonitorEffect::ShowTab {
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
            first.set_cadence(Cadence::Paused);
            first.prefs.show_kernel_threads = true;
            first.commit_filter("shell".into());
            first.set_sort(SortKey::Name);
            first.sync(&doc, &automerge::ROOT);
            second.sync(&doc, &automerge::ROOT);
            assert_eq!(second.prefs, Prefs::default());
            assert_eq!(
                doc.get_heads(),
                heads,
                "nothing unhosted reaches the document"
            );
        }
    }

    /// The window lays out at desktop, narrow and phone sizes: the inspector
    /// docks where it leaves the table its room and is a drawer below that.
    #[test]
    fn window_lays_out_at_every_size() {
        for (viewport, placement) in [
            ((1280, 800), InspectorPlacement::Docked),
            ((800, 600), InspectorPlacement::Docked),
            ((480, 820), InspectorPlacement::Drawer),
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

    /// Press the window bar's slot `id`, as the desktop's bar does.
    fn press(harness: &mut FrameHarness, id: &str) {
        harness.press_window_slot(ui::window_toolbar::ToolbarEvent::Action { id: id.into() });
    }

    fn bar() -> ui::window_toolbar::WindowToolbar {
        ui::window_toolbar::declared().expect("the window declares its slots")
    }

    /// The tabs are the bar's views, the cadence its menu button, the
    /// list's options its actions; the window draws none of them, and the
    /// bar's search narrows the list.
    #[test]
    fn the_window_bar_switches_views_and_holds_the_lists_options() {
        let mut harness = window(app(), (390, 844));
        harness.present_in_window_bar();
        harness.relayout();
        for drawn in ["monitor.tabs", "monitor.cadence", "monitor.filter"] {
            assert!(
                ui::widget::find_test_id(harness.root::<MonitorApp>(), drawn).is_none(),
                "the window draws no {drawn} of its own"
            );
        }
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
        assert_eq!(harness.root::<MonitorApp>().tab, Tab::Resources);
        assert_eq!(bar().views.unwrap().chosen, Tab::Resources.view_id());
        assert!(
            bar()
                .actions
                .iter()
                .all(|action| action.id != window_bar::TREE),
            "the list's options leave with the list"
        );
        press(&mut harness, Tab::Processes.view_id());
        assert_eq!(harness.root::<MonitorApp>().tab, Tab::Processes);

        let cadence = bar()
            .actions
            .into_iter()
            .find(|action| action.id == window_bar::CADENCE)
            .expect("the update frequency is a menu button");
        assert_eq!(cadence.items.len(), Cadence::all().len());
        press(&mut harness, &Cadence::Paused.item_id());
        assert!(!harness.root::<MonitorApp>().prefs.auto_refresh);

        let tree = harness.root::<MonitorApp>().tree_mode;
        press(&mut harness, window_bar::TREE);
        assert_eq!(harness.root::<MonitorApp>().tree_mode, !tree);
        press(&mut harness, window_bar::KERNEL_THREADS);
        assert!(harness.root::<MonitorApp>().prefs.show_kernel_threads);

        ui::widget::apply_filter(harness.root_mut::<MonitorApp>(), "cargo");
        harness.relayout();
        assert_eq!(harness.root::<MonitorApp>().filter, "cargo");
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
        assert_eq!(harness.root::<MonitorApp>().tab, Tab::Resources);
        assert!(harness.inspection_json().unwrap().contains("Host name"));
        press(&mut harness, Tab::Processes.view_id());
        assert_eq!(harness.root::<MonitorApp>().tab, Tab::Processes);

        press(&mut harness, &Cadence::Paused.item_id());
        assert!(!harness.root::<MonitorApp>().prefs.auto_refresh);

        let (x, y) = center_of(&harness, "tilo-fs").expect("tilo-fs row");
        harness.send(&ui::synthetic_input::cursor_moved(x, y));
        harness.send(&ui::synthetic_input::mouse_input(
            ElementState::Pressed,
            winit::event::MouseButton::Right,
        ));
        harness.relayout();
        let tilo_fs = pid_named(harness.root::<MonitorApp>(), "tilo-fs");
        assert_eq!(harness.root::<MonitorApp>().selected_pid, Some(tilo_fs));
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
            harness.root::<MonitorApp>().requested.last(),
            Some(&copy_pid),
            "the window asks for the copy through the process's effect"
        );

        // ← and → fold the selected parent while the table has focus (the
        // row is where it was; the inspector now carries its name too).
        harness.click_at(x, y);
        harness.relayout();
        harness.key("ArrowRight");
        harness.relayout();
        assert!(harness.root::<MonitorApp>().expanded.contains(&tilo_fs));
        harness.key("ArrowLeft");
        harness.relayout();
        assert!(!harness.root::<MonitorApp>().expanded.contains(&tilo_fs));

        click(&mut harness, "Memory");
        assert_eq!(harness.root::<MonitorApp>().tab, Tab::Resources);
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
            assert_eq!(monitor.model.process_status, ProcessDataStatus::Ready);
            assert_eq!(monitor.model.metrics.host, "ada-laptop");
            assert_eq!(monitor.model.metrics.cores, vec![70.0, 50.0, 30.0, 10.0]);
            assert_eq!(monitor.model.metrics.mem_used, 12 << 30);
            let notes = monitor
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
                .answer(MonitorEffect::EndApp {
                    app: "so.tilo.notes".into(),
                })
                .is_err(),
            "an app that is not running is refused"
        );
    }
}
