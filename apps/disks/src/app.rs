//! Root of the Disks app.
//!
//! [`DisksApp`] is a [`NavShell`] window: the drive list is the sidebar and the
//! selected drive is the content (see [`crate::view`]). This module owns what
//! the shell cannot: this machine's probed [`Topology`], the selection, the
//! tab and the result status. The selection, the tab and the status are this
//! window's own; the devices are live state, published as rows whose
//! effects the app answers ([`crate::resource::DisksEffect`]).

use std::cell::Cell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::mpsc;

use runtime_portal::tools::{ToolIntent, open_tool};
use ui::framework::{Dispatcher, EffectAnswer, EffectCx, Effected, LiveRow, RunMode};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::vello::Scene;
use ui::vello::kurbo::Affine;
use ui::widget::toast::ToastKind;
use ui::widget::{AppModel, ModelPath, NavShell, Widget};
use ui::winit::event::{ElementState, KeyEvent, WindowEvent};
use ui::winit::keyboard::Key;

use crate::model::{Drive, Partition, Topology};
use crate::offer;
#[cfg(test)]
use crate::offer::Offer;
use crate::ops::{self, Op};
use crate::resource::{Disks, DisksEffect};
use crate::view::{self, DetailModel, Intent, Intents};

impl ActionKind {
    /// The real operation this action maps to.
    fn op(self) -> Op {
        match self {
            ActionKind::Mount => Op::Mount,
            ActionKind::Unmount => Op::Unmount,
            ActionKind::Format => Op::Format,
            ActionKind::Benchmark => Op::Benchmark,
            ActionKind::Eject => Op::Eject,
        }
    }
    /// Present-tense verb for the "…in progress" status.
    fn gerund(self) -> &'static str {
        match self {
            ActionKind::Mount => "Mounting",
            ActionKind::Unmount => "Unmounting",
            ActionKind::Format => "Formatting",
            ActionKind::Benchmark => "Benchmarking",
            ActionKind::Eject => "Ejecting",
        }
    }
    /// The action's name, for a status.
    pub(crate) fn title(self) -> &'static str {
        self.label().trim_end_matches('…')
    }
    /// What the action would run on `dev`, reported on the sample machine.
    fn plan(self, dev: &str) -> String {
        match self {
            ActionKind::Mount => {
                format!("Sample machine: would run udisks2 Filesystem.Mount on {dev}")
            }
            ActionKind::Unmount => {
                format!("Sample machine: would run udisks2 Filesystem.Unmount on {dev}")
            }
            ActionKind::Format => {
                format!("Sample machine: would run udisks2 Block.Format on {dev}")
            }
            ActionKind::Benchmark => {
                format!("Sample machine: would run a read/seek benchmark on {dev}")
            }
            ActionKind::Eject => format!("Sample machine: would power off {dev} via udisks2"),
        }
    }
    /// Does completing this action change the mount/partition topology?
    fn changes_topology(self) -> bool {
        matches!(
            self,
            ActionKind::Mount | ActionKind::Unmount | ActionKind::Format | ActionKind::Eject
        )
    }
}

/// The detail tabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Smart,
    Partitions,
}

impl Tab {
    pub(crate) fn all() -> [Tab; 3] {
        [Tab::Overview, Tab::Smart, Tab::Partitions]
    }
    pub(crate) fn key(self) -> &'static str {
        match self {
            Tab::Overview => "overview",
            Tab::Smart => "smart",
            Tab::Partitions => "partitions",
        }
    }
    /// The view's id in the window bar's switcher.
    pub(crate) fn view_id(self) -> &'static str {
        match self {
            Tab::Overview => "disks.view.overview",
            Tab::Smart => "disks.view.smart",
            Tab::Partitions => "disks.view.partitions",
        }
    }
    pub(crate) fn title(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Smart => "Health",
            Tab::Partitions => "Partitions",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionKind {
    Mount,
    Unmount,
    Format,
    Benchmark,
    Eject,
}

impl ActionKind {
    /// The action row, left to right.
    pub(crate) fn bar() -> [ActionKind; 5] {
        [
            ActionKind::Mount,
            ActionKind::Unmount,
            ActionKind::Format,
            ActionKind::Benchmark,
            ActionKind::Eject,
        ]
    }
    pub(crate) fn key(self) -> &'static str {
        match self {
            ActionKind::Mount => "mount",
            ActionKind::Unmount => "unmount",
            ActionKind::Format => "format",
            ActionKind::Benchmark => "benchmark",
            ActionKind::Eject => "eject",
        }
    }
    pub(crate) fn label(self) -> &'static str {
        match self {
            ActionKind::Mount => "Mount",
            ActionKind::Unmount => "Unmount",
            ActionKind::Format => "Format…",
            ActionKind::Benchmark => "Benchmark",
            ActionKind::Eject => "Eject",
        }
    }
    /// What the action does, as the button's tooltip.
    pub(crate) fn describe(self) -> &'static str {
        match self {
            ActionKind::Mount => "Attach the filesystem so its files become reachable",
            ActionKind::Unmount => "Detach the filesystem before removing the device",
            ActionKind::Format => "Erase the selection and create a new filesystem",
            ActionKind::Benchmark => "Measure sequential read speed and seek time",
            ActionKind::Eject => "Power down and safely remove the whole device",
        }
    }
    /// Destructive actions ask the person to type the device's name first.
    pub(crate) fn destructive(self) -> bool {
        matches!(self, ActionKind::Format)
    }
}

/// What a transient result means. The toast takes its colour from this, so
/// nothing reads meaning out of the message text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StatusTone {
    Info,
    Success,
    Danger,
}

impl StatusTone {
    fn kind(self) -> ToastKind {
        match self {
            StatusTone::Info => ToastKind::Info,
            StatusTone::Success => ToastKind::Success,
            StatusTone::Danger => ToastKind::Error,
        }
    }
}

/// What the shell currently shows, so it is rebuilt only when it changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ViewKey {
    loading: bool,
    topology: u64,
    names: u64,
    selected: usize,
    partition: Option<usize>,
    tab: Tab,
}

/// A reading of this machine's storage, or why it could not be read.
type Probed = Result<Topology, String>;

/// The package that shows what is taking up the room.
pub(crate) const DISK_USAGE: &str = "so.tilo.disk-usage";

/// How this window asks the session to open something for it: the tools
/// service, whose round trip blocks, so it is called on a worker.
pub(crate) type ShellDoor = fn(ToolIntent, Option<[u8; 16]>) -> Result<(), String>;

/// The invocation this window is answering, when an agent asked it to open
/// something: the shell admits the open as that invocation's caller.
fn delivered() -> Option<[u8; 16]> {
    ui::invocation_scope::delivered_invocation().map(|id| id.0.into_bytes())
}

/// Open a terminal in `folder`: what the person's press asks the session
/// for when they open a mounted filesystem.
fn ask_terminal(ask: ShellDoor, folder: String) -> Result<(), String> {
    ask(
        ToolIntent::Terminal {
            directory: Some(folder),
        },
        delivered(),
    )
}

/// What this window asks the session to open: Disk Usage, which says what is
/// taking up the room in a window of its own.
fn open_disk_usage_intent() -> ToolIntent {
    ToolIntent::OpenApplication {
        app: DISK_USAGE.to_owned(),
    }
}

/// What a [`DisksApp`]'s devices come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// This machine: described by the session's system information service,
    /// operated through udisks2.
    System,
    /// A fixed sample machine: operations report what they would run.
    Sample,
}

pub struct DisksApp {
    shell: NavShell,
    intents: Intents,
    /// This machine's block devices, as last probed (or the sample machine).
    topo: Topology,
    /// Why the last probe found nothing, when it could not read the machine.
    unreadable: Option<String>,
    /// No reading of this machine has arrived yet, asked then: the window
    /// shows the page the drives will fill.
    loading: Option<ui::time::Instant>,
    /// The reading taken when the window opened, on a worker.
    opening: Option<mpsc::Receiver<Probed>>,
    /// Bumped whenever `topo` is replaced.
    topo_rev: u64,
    /// Where the topology comes from and where operations run.
    backend: Backend,
    /// Drive names from the document, by serial number.
    names: BTreeMap<String, String>,
    /// Bumped whenever `names` changes.
    names_rev: u64,
    /// Where effects and invocations go once hosted.
    dispatcher: Option<Dispatcher<Disks>>,
    /// The device operation running now, if any: one at a time.
    running: Option<ActionKind>,
    selected: usize,
    /// Selected partition within the current drive, or `None` for the whole
    /// disk. Drives every partition-scoped action + detail highlight.
    selected_part: Option<usize>,
    tab: Tab,
    status: String,
    dirty: bool,
    /// The detail page's scroll offset, kept across rebuilds.
    scroll: Rc<Cell<(f32, f32)>>,
    built: Option<ViewKey>,
    /// The slots last declared to the bar.
    declared_bar: Option<ui::window_toolbar::WindowToolbar>,
    /// How this window asks the session to open something for it. A test
    /// swaps in a recorder that answers without a session.
    ask_shell: ShellDoor,
}

impl Default for DisksApp {
    fn default() -> Self {
        Self::new()
    }
}

impl DisksApp {
    /// Disks over this machine's devices.
    pub fn new() -> Self {
        Self::with_backend(Backend::System, Topology::default())
    }

    /// Disks over the sample machine: fully populated, and operations only
    /// report what they would run.
    pub fn sample() -> Self {
        Self::with_backend(Backend::Sample, Topology::sample())
    }

    fn with_backend(backend: Backend, topo: Topology) -> Self {
        let intents = Intents::default();
        let shell = NavShell::new()
            .content_kind(ui::widget::ContentKind::Page)
            .with_window_toolbar()
            .with_id("disks-shell");
        Self {
            shell,
            intents,
            topo,
            unreadable: None,
            loading: None,
            opening: None,
            topo_rev: 0,
            backend,
            names: BTreeMap::new(),
            names_rev: 0,
            dispatcher: None,
            running: None,
            selected: 0,
            selected_part: None,
            tab: Tab::Overview,
            status: String::new(),
            dirty: true,
            scroll: Rc::new(Cell::new((0.0, 0.0))),
            built: None,
            declared_bar: None,
            ask_shell: open_tool,
        }
    }

    /// Hosted: edits and effects go through `dispatcher`. Only a live window
    /// reads this machine, on a worker that wakes the window with the
    /// result; a capture shows `fixture` (or the sample machine) and a past
    /// version shows no devices.
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: Dispatcher<Disks>,
        mode: RunMode,
        fixture: Option<Topology>,
        waker: ui::WakeHandle,
    ) {
        self.dispatcher = Some(dispatcher);
        match mode {
            RunMode::Live if self.backend == Backend::System => self.start_reading(waker),
            RunMode::Live => {}
            RunMode::Capture => {
                self.backend = Backend::Sample;
                self.replace_topology(fixture.unwrap_or_else(Topology::sample));
            }
            RunMode::Past => {
                self.backend = Backend::Sample;
                self.replace_topology(Topology::default());
            }
        }
    }

    pub(crate) fn adopt_names(&mut self, names: &BTreeMap<String, String>) {
        if self.names != *names {
            self.names.clone_from(names);
            self.names_rev += 1;
            self.dirty = true;
        }
    }

    /// The name the person gave `drive`, else its model.
    fn display_name<'a>(&'a self, drive: &'a Drive) -> &'a str {
        drive
            .serial
            .as_ref()
            .and_then(|serial| self.names.get(serial))
            .map_or(drive.model.as_str(), String::as_str)
    }

    pub(crate) fn is_running(&self) -> bool {
        self.running.is_some()
    }

    /// Every drive, with its partitions, as published rows.
    pub(crate) fn drive_rows(&self) -> Vec<LiveRow> {
        self.topo
            .drives
            .iter()
            .map(|drive| crate::resource::drive_row(drive, &self.names))
            .collect()
    }

    /// Show `topo`, keeping the selection on a drive and partition that
    /// still exist.
    fn replace_topology(&mut self, topo: Topology) {
        self.topo = topo;
        self.topo_rev += 1;
        self.selected = self.selected.min(self.topo.drives.len().saturating_sub(1));
        self.selected_part = self.part_index();
        self.dirty = true;
    }

    fn drive(&self) -> Option<&Drive> {
        self.topo.drives.get(self.selected)
    }

    /// The selected partition index, validated against the current drive so a
    /// stale index (after switching drives) never points out of bounds.
    fn part_index(&self) -> Option<usize> {
        let n = self.drive()?.partitions.len();
        self.selected_part.filter(|&i| i < n)
    }

    /// The selected partition of `drive`, or `None` for the whole drive.
    fn selected_partition<'a>(&self, drive: &'a Drive) -> Option<&'a Partition> {
        self.part_index().and_then(|i| drive.partitions.get(i))
    }

    /// What `action` can do for the current selection.
    #[cfg(test)]
    fn offer(&self, drive: &Drive, action: ActionKind) -> Offer {
        offer::offer(drive, self.selected_partition(drive), action)
    }

    /// Whether an action can run on the current selection now.
    #[cfg(test)]
    fn action_enabled(&self, drive: &Drive, action: ActionKind) -> bool {
        self.offer(drive, action).is_available()
    }

    /// Rebuild the sidebar and the content when what they show changed.
    fn refresh_view(&mut self) {
        let key = ViewKey {
            loading: self.loading.is_some(),
            topology: self.topo_rev,
            names: self.names_rev,
            selected: self.selected,
            partition: self.part_index(),
            tab: self.tab,
        };
        if self.built == Some(key) {
            return;
        }
        let listed_before = self.built.is_some_and(|b| {
            b.loading == key.loading
                && b.topology == key.topology
                && b.names == key.names
                && b.selected == key.selected
        });
        self.built = Some(key);
        if !listed_before {
            let drives: Vec<_> = self
                .topo
                .drives
                .iter()
                .map(|drive| (drive, self.display_name(drive).to_owned()))
                .collect();
            self.shell.set_sidebar(Some(Box::new(view::drive_list(
                &drives,
                self.selected,
                self.loading.is_some(),
                &self.intents,
            ))));
        }
        let Some(drive) = self.topo.drives.get(self.selected) else {
            self.shell.set_content_title("");
            self.shell.set_content(match self.loading {
                Some(since) => view::loading(&self.scroll, since),
                None => view::empty_state(self.unreadable.as_deref(), &self.intents),
            });
            return;
        };
        let model = DetailModel {
            drive,
            tab: self.tab,
            partition: key.partition,
        };
        let content = view::detail(&model, &self.intents, &self.scroll);
        self.shell.set_content(content);
    }

    /// What the window shows as where it is: the drive, and the partition
    /// when one is chosen.
    fn location(&self, drive: &Drive) -> view::Location {
        view::Location {
            drive: self.display_name(drive).to_owned(),
            partition: self.selected_partition(drive).map(Partition::display_label),
        }
    }

    /// Apply what the person did in the window's bar: the shell's own
    /// slots, the location's drive level, Rescan and the selection's
    /// operations. Returns whether anything changed.
    fn take_window_toolbar_events(&mut self) -> bool {
        let mut changed = false;
        for event in ui::window_toolbar::take_events() {
            if self.shell.window_toolbar_event(&event) {
                changed = true;
                continue;
            }
            match event {
                // The location's drive level: back to the whole drive.
                ui::window_toolbar::ToolbarEvent::Commit { text } if text == view::WHOLE_DRIVE => {
                    self.select_part(None);
                }
                ui::window_toolbar::ToolbarEvent::Action { id } => {
                    if id == view::RESCAN {
                        self.rescan();
                    } else if let Some(tab) = Tab::all().into_iter().find(|tab| tab.view_id() == id)
                    {
                        self.set_tab(tab);
                    } else if let Some(action) = ActionKind::bar()
                        .into_iter()
                        .find(|action| action.key() == id)
                    {
                        self.trigger_action(action, self.part_index());
                    }
                }
                _ => continue,
            }
            changed = true;
        }
        changed
    }

    /// Declare the window bar's slots: the shell's own, where the window
    /// is (the drive, and the partition when one is chosen), and the
    /// selection's operations followed by Rescan.
    fn declare_window_toolbar(&mut self) {
        let bar = view::window_toolbar(
            self.shell.window_toolbar(),
            self.drive().map(|drive| {
                (
                    self.location(drive),
                    offer::shown(drive, self.selected_partition(drive)),
                )
            }),
            self.tab,
        );
        if self.declared_bar.as_ref() != Some(&bar) {
            ui::window_toolbar::declare(Some(bar.clone()));
            self.declared_bar = Some(bar);
        }
    }

    /// Apply what the window's controls asked for.
    fn drain_intents(&mut self) {
        for intent in self.intents.take() {
            match intent {
                Intent::SelectDrive(index) => self.select(index),
                Intent::SelectPartition(part) => self.select_part(part),
                Intent::RunOn { action, partition } => self.trigger_action(action, partition),
                Intent::Rescan => self.rescan(),
                Intent::OpenTerminal(partition) => self.request_terminal(partition),
                Intent::OpenDiskUsage => self.ask_disk_usage(),
            }
        }
    }

    /// The window's own buttons ask through the published effects, so a
    /// person's press and an agent's invocation take one path.
    fn request_terminal(&mut self, partition: usize) {
        let Some(drive) = self.drive() else { return };
        let Some(part) = drive.partitions.get(partition) else {
            return;
        };
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.effect_on(
                &crate::resource::partition_row_id(&part.name),
                &DisksEffect::OpenInTerminal {
                    device: part.name.clone(),
                },
            );
        }
        self.dirty = true;
    }

    fn ask_disk_usage(&mut self) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.effect(&DisksEffect::OpenDiskUsage);
        }
        self.dirty = true;
    }

    /// Record a result: the status the window announces and the toast that
    /// shows it once.
    fn report(&mut self, text: String, tone: StatusTone) {
        self.status.clone_from(&text);
        self.shell.toast(text, tone.kind());
        self.dirty = true;
    }

    /// An action from the bar, a header button or a row's menu: invoke the
    /// operation on partition `partition` of the selected drive (`None`: the
    /// whole drive), as that row offers it. Formatting asks for the device's
    /// name before anything runs.
    fn trigger_action(&mut self, action: ActionKind, partition: Option<usize>) {
        let Some(drive) = self.drive() else { return };
        let chosen = partition.and_then(|i| drive.partitions.get(i));
        if !offer::offer(drive, chosen, action).is_available() {
            return; // unavailable actions are inert
        }
        // Resolve what the action targets: Mount/Unmount snap to a filesystem;
        // Eject always the whole device; Format/Benchmark follow the choice.
        let part = match action {
            ActionKind::Mount | ActionKind::Unmount => offer::mount_target(drive, chosen),
            ActionKind::Eject => None,
            ActionKind::Format | ActionKind::Benchmark => chosen,
        };
        let (row, leaf) = match part {
            Some(part) => (
                crate::resource::partition_row_id(&part.name),
                part.name.clone(),
            ),
            None => (
                crate::resource::drive_row_id(&drive.name),
                drive.name.clone(),
            ),
        };
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.effect_on(&row, &DisksEffect::operation(action, &leaf));
        }
        self.dirty = true;
    }

    fn select(&mut self, idx: usize) {
        if idx < self.topo.drives.len() {
            let changed = idx != self.selected;
            self.selected = idx;
            // A new drive resets the partition focus to the whole disk so the
            // detail and its actions can never point at a stale partition.
            if changed {
                self.selected_part = None;
                self.scroll.set((0.0, 0.0));
            }
            self.dirty = true;
        }
    }

    /// Select a partition within the current drive, or the whole disk (`None`).
    /// Choosing the already-selected partition goes back to the whole disk.
    fn select_part(&mut self, part: Option<usize>) {
        let next = match part {
            Some(i) if self.selected_part == Some(i) => None,
            other => other,
        };
        self.selected_part = next;
        self.dirty = true;
    }

    fn set_tab(&mut self, tab: Tab) {
        self.tab = tab;
        self.scroll.set((0.0, 0.0));
        self.dirty = true;
    }

    /// Stage a screenshot scenario over the sample machine. A phone-sized
    /// capture stages the same state as its wide twin.
    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        let base = scenario.map(|name| name.strip_prefix("phone-").unwrap_or(name));
        match base {
            Some("health" | "laptop-health") => self.set_tab(Tab::Smart),
            Some("partitions") => {
                self.set_tab(Tab::Partitions);
                self.select_part(Some(1));
            }
            Some("partition") => self.select_part(Some(1)),
            Some("usb") => {
                if let Some(usb) = self.topo.drives.iter().position(|drive| drive.removable) {
                    self.select(usb);
                }
            }
            Some("failing") => {
                self.select(2);
                self.set_tab(Tab::Smart);
            }
            Some("laptop-partitions") => {
                // The LUKS2 home, unlocked.
                self.set_tab(Tab::Partitions);
                self.select_part(Some(3));
            }
            // The window as it opens on a slow read of this machine's
            // storage: past its frame budget, so the page the drives will
            // fill stands in.
            Some("loading") => {
                self.loading = ui::time::now().checked_sub(ui::time::Duration::from_millis(500));
            }
            Some("toast") => self.report("Mounted /dev/sdc1".into(), StatusTone::Success),
            Some("toast-error") => {
                self.report(
                    "Mount failed: the device is busy".into(),
                    StatusTone::Danger,
                );
            }
            _ => {}
        }
        if scenario.is_some_and(|name| name.ends_with("drawer")) {
            self.shell.open_sidebar();
            self.shell.snap();
        }
        self.dirty = true;
    }

    /// Answer `effect`, from a control, the palette, an agent or a script.
    pub(crate) fn answer_effect(
        &mut self,
        effect: DisksEffect,
        cx: &mut EffectCx<Disks, Self>,
    ) -> EffectAnswer {
        let operation = match effect {
            DisksEffect::Rescan => return Ok(self.read_again(cx)),
            DisksEffect::Select { device } => {
                self.select_device(&device)?;
                return Ok(Effected::done());
            }
            DisksEffect::ShowTab { tab } => {
                let tab = Tab::all()
                    .into_iter()
                    .find(|known| known.key() == tab)
                    .ok_or_else(|| format!("there is no tab `{tab}`"))?;
                self.set_tab(tab);
                return Ok(Effected::done());
            }
            DisksEffect::Mount { device } => (ActionKind::Mount, device),
            DisksEffect::Unmount { device } => (ActionKind::Unmount, device),
            DisksEffect::Format { device } => (ActionKind::Format, device),
            DisksEffect::Benchmark { device } => (ActionKind::Benchmark, device),
            DisksEffect::Eject { device } => (ActionKind::Eject, device),
            DisksEffect::OpenInTerminal { device } => return self.open_terminal(&device, cx),
            DisksEffect::OpenDiskUsage => return Ok(self.open_disk_usage(cx)),
        };
        self.run_operation(operation.0, &operation.1, cx)
    }

    /// The folder a mounted partition is reachable at, or why it is not.
    fn mounted_folder(&self, device: &str) -> Result<String, String> {
        for drive in &self.topo.drives {
            for part in &drive.partitions {
                if part.name != device {
                    continue;
                }
                return part.mounted_folder().map(str::to_owned).ok_or_else(|| {
                    format!("/dev/{device} is not mounted; mount it to open a terminal in it")
                });
            }
        }
        Err(format!(
            "/dev/{device} is no longer present; rescan storage and try again"
        ))
    }

    /// Open a terminal in the folder of mounted partition `device`.
    ///
    /// The session's tools service is the door to another app, and the only
    /// one that reaches a mounted folder: this app is granted no filesystem
    /// paths, so the chooser's open has none of them to hand on.
    fn open_terminal(&mut self, device: &str, cx: &mut EffectCx<Disks, Self>) -> EffectAnswer {
        let folder = self.mounted_folder(device)?;
        if self.backend == Backend::Sample {
            self.report(
                format!("Sample machine: would open a terminal in {folder}"),
                StatusTone::Info,
            );
            return Ok(Effected::value(&self.status));
        }
        let ask = self.ask_shell;
        let asked = folder.clone();
        Ok(cx.thread(
            move |_| ask_terminal(ask, asked),
            move |app: &mut Self, result: Result<(), String>, _| match result {
                Ok(()) => {
                    app.report(
                        format!("Opened a terminal in {folder}"),
                        StatusTone::Success,
                    );
                    Ok(Effected::value(&app.status))
                }
                Err(error) => {
                    app.report(error.clone(), StatusTone::Danger);
                    Err(error)
                }
            },
        ))
    }

    /// Ask the session to open Disk Usage, which says what is taking up the
    /// room on these drives: a different job from partitioning, in its own
    /// window.
    fn open_disk_usage(&mut self, cx: &mut EffectCx<Disks, Self>) -> Effected {
        if self.backend == Backend::Sample {
            self.report(
                "Sample machine: would open Disk Usage".to_owned(),
                StatusTone::Info,
            );
            return Effected::value(&self.status);
        }
        let ask = self.ask_shell;
        cx.thread(
            move |_| ask(open_disk_usage_intent(), delivered()),
            |app: &mut Self, result: Result<(), String>, _| match result {
                Ok(()) => {
                    app.report("Opening Disk Usage…".to_owned(), StatusTone::Info);
                    Ok(Effected::value(&app.status))
                }
                Err(error) => {
                    app.report(error.clone(), StatusTone::Danger);
                    Err(error)
                }
            },
        )
    }

    /// Select the drive or partition named `device`.
    fn select_device(&mut self, device: &str) -> Result<(), String> {
        for (index, drive) in self.topo.drives.iter().enumerate() {
            if drive.name == device {
                self.select(index);
                self.selected_part = None;
                return Ok(());
            }
            if let Some(part) = drive.partitions.iter().position(|part| part.name == device) {
                self.select(index);
                self.selected_part = Some(part);
                return Ok(());
            }
        }
        Err(format!("/dev/{device} is not present"))
    }

    /// Run `kind` on `device`: checked against the latest probe, then on
    /// this machine in the background, or described on the sample machine.
    fn run_operation(
        &mut self,
        kind: ActionKind,
        device: &str,
        cx: &mut EffectCx<Disks, Self>,
    ) -> EffectAnswer {
        if self.running.is_some() {
            return Err("another disk operation is still running".into());
        }
        // A caller's device name is no authority to touch an arbitrary
        // `/dev` node, and the filesystem type comes from the probe.
        let (device, fstype) = self.revalidate_operation_target(kind, device)?;
        let dev = format!("/dev/{device}");
        if self.backend == Backend::Sample {
            self.report(kind.plan(&dev), StatusTone::Info);
            return Ok(Effected::value(&self.status));
        }
        self.running = Some(kind);
        self.report(format!("{} {dev}…", kind.gerund()), StatusTone::Info);
        Ok(cx.thread(
            move |_| {
                let result = ops::run(kind.op(), &device, fstype.as_deref());
                // What changed is read again here, on the worker.
                let probed = kind.changes_topology().then(crate::probe::probe);
                (result, probed)
            },
            move |app: &mut Self, (result, probed): (Result<String, String>, Option<Probed>), _| {
                app.running = None;
                if let Some(probed) = probed {
                    app.show_reading(probed);
                }
                match result {
                    Ok(message) => {
                        app.report(message.clone(), StatusTone::Success);
                        Ok(Effected::value(message))
                    }
                    Err(error) => {
                        app.report(
                            format!("{} failed: {error}", kind.title()),
                            StatusTone::Danger,
                        );
                        Err(error)
                    }
                }
            },
        ))
    }

    /// Read this machine's storage on a worker, which wakes the window
    /// with the result; until it arrives the window shows the page the
    /// drives will fill.
    fn start_reading(&mut self, waker: ui::WakeHandle) {
        let (sender, receiver) = mpsc::channel();
        drop(ui::rt::app().spawn_blocking(move || {
            let _ = sender.send(crate::probe::probe());
            waker.wake();
        }));
        self.opening = Some(receiver);
        self.loading = Some(ui::time::now());
        self.dirty = true;
    }

    /// Show the opening reading, if it arrived.
    fn take_opening_reading(&mut self) {
        let Some(receiver) = &self.opening else {
            return;
        };
        match receiver.try_recv() {
            Ok(probed) => {
                self.opening = None;
                self.show_reading(probed);
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.opening = None;
                self.show_reading(Err("Storage could not be read.".into()));
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }

    /// Show a reading of this machine, keeping the selection where it still
    /// exists. It is newer than one still being taken at opening, which is
    /// dropped.
    fn show_reading(&mut self, probed: Probed) {
        self.opening = None;
        self.loading = None;
        self.unreadable = probed.as_ref().err().cloned();
        self.replace_topology(probed.unwrap_or_default());
    }

    /// Rescan storage devices: the Rescan button, `r`, and the bar. This
    /// machine is read again through the Rescan effect, on a worker; the
    /// sample machine has nothing to read again.
    fn rescan(&mut self) {
        match (&self.dispatcher, self.backend) {
            (Some(dispatcher), Backend::System) => dispatcher.effect(&DisksEffect::Rescan),
            _ => self.report_scan(),
        }
    }

    /// The Rescan effect: read this machine again on a worker, then show
    /// and report what it found.
    fn read_again(&mut self, cx: &mut EffectCx<Disks, Self>) -> Effected {
        if self.backend != Backend::System {
            self.report_scan();
            return Effected::value(&self.status);
        }
        cx.thread(
            |_| crate::probe::probe(),
            |app: &mut Self, probed: Probed, _| {
                app.show_reading(probed);
                app.report_scan();
                Ok(Effected::value(&app.status))
            },
        )
    }

    /// Report what the last reading found.
    fn report_scan(&mut self) {
        match self.unreadable.clone() {
            Some(reason) => self.report(reason, StatusTone::Danger),
            None => self.report(found_message(self.topo.drives.len()), StatusTone::Success),
        }
    }

    /// Resolve an operation's target against the latest locally-probed
    /// topology. An invocation may arrive after a search result or an
    /// agent's view has gone stale, so a device name is never authority to
    /// touch an arbitrary `/dev/*` node.
    ///
    /// The return value is the device the operation runs on (an unlocked
    /// encrypted partition mounts and unmounts through its mapping) and the
    /// filesystem type from the current probe, never the caller's word.
    fn revalidate_operation_target(
        &self,
        action: ActionKind,
        leaf: &str,
    ) -> Result<(String, Option<String>), String> {
        for drive in &self.topo.drives {
            if drive.name == leaf {
                if let (ActionKind::Format, Some(reason)) = (action, drive.system_use()) {
                    return Err(format!("Can't format /dev/{leaf}: {reason}"));
                }
                let whole = || (leaf.to_owned(), None);
                return match action {
                    ActionKind::Format | ActionKind::Benchmark => Ok(whole()),
                    ActionKind::Eject if drive.removable => Ok(whole()),
                    ActionKind::Eject => Err(format!("/dev/{leaf} is not removable")),
                    ActionKind::Mount | ActionKind::Unmount => Err(format!(
                        "/dev/{leaf} is a whole disk; select one of its filesystems"
                    )),
                };
            }
            for part in &drive.partitions {
                if part.name != leaf {
                    continue;
                }
                // What the running system stands on is never formatted or
                // detached, whoever asks.
                if let (ActionKind::Format | ActionKind::Unmount, Some(reason)) =
                    (action, drive.partition_use(part))
                {
                    return Err(format!("Can't {} /dev/{leaf}: {reason}", action.key()));
                }
                let filesystem = part
                    .encryption
                    .as_ref()
                    .map_or(Some(leaf), |encryption| encryption.unlocked_as.as_deref());
                return match (action, filesystem) {
                    (ActionKind::Mount | ActionKind::Unmount, None) => Err(format!(
                        "/dev/{leaf} is encrypted and locked; unlock it first"
                    )),
                    (ActionKind::Mount, Some(device)) if part.mountpoint.is_none() => {
                        Ok((device.to_owned(), part.fstype.clone()))
                    }
                    (ActionKind::Unmount, Some(device)) if part.mountpoint.is_some() => {
                        Ok((device.to_owned(), part.fstype.clone()))
                    }
                    (ActionKind::Mount, _) => Err(format!("/dev/{leaf} is already mounted")),
                    (ActionKind::Unmount, _) => Err(format!("/dev/{leaf} is not mounted")),
                    // Formatting re-creates the filesystem the probe found;
                    // on an encrypted partition that would silently drop its
                    // encryption, so it is not offered there.
                    (ActionKind::Format, _) if part.encryption.is_some() => Err(format!(
                        "/dev/{leaf} is encrypted; formatting it here would remove its encryption"
                    )),
                    (ActionKind::Format | ActionKind::Benchmark, _) => {
                        Ok((leaf.to_owned(), part.fstype.clone()))
                    }
                    (ActionKind::Eject, _) => Err("eject targets a whole removable drive".into()),
                };
            }
        }
        Err(format!(
            "/dev/{leaf} is no longer present; rescan storage and try again"
        ))
    }
}

fn found_message(n: usize) -> String {
    format!(
        "Rescan found {n} {}",
        if n == 1 { "drive" } else { "drives" }
    )
}

impl ui::widget::Decorator for DisksApp {
    type Inner = NavShell;

    fn inner(&self) -> &NavShell {
        &self.shell
    }

    fn inner_mut(&mut self) -> &mut NavShell {
        &mut self.shell
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.take_opening_reading();
        self.take_window_toolbar_events();
        self.drain_intents();
        self.refresh_view();
        self.declare_window_toolbar();
        self.shell.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.take_opening_reading();
        let bar = self.take_window_toolbar_events();
        self.drain_intents();
        self.refresh_view();
        self.declare_window_toolbar();
        self.shell.update(state, path) || self.dirty || bar
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.shell.handle_event(event, ctx);
        self.drain_intents();
        if !*ctx.handled
            && let WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Character(key),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } = event
            && !ctx.modifiers.state().control_key()
        {
            let tab = match key.as_str() {
                "1" => Some(Tab::Overview),
                "2" => Some(Tab::Smart),
                "3" => Some(Tab::Partitions),
                _ => None,
            };
            if let Some(tab) = tab {
                self.set_tab(tab);
                *ctx.handled = true;
            } else if matches!(key.as_str(), "r" | "R") {
                self.rescan();
                *ctx.handled = true;
            }
        }
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.shell
            .draw_overlay(scene, transform, text, theme, presence);
    }

    fn needs_animation_frame(&self) -> bool {
        self.shell.needs_animation_frame()
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.shell.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.shell.mark_drawn();
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, Politeness, Role};
        let mut root = AccessNode::new(Role::Window)
            .with_name("Disks")
            .with_bounds(self.shell.get_rect());
        if !self.status.is_empty() {
            root.children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(self.status.clone())
                    .with_hashed_key("disks.status")
                    .with_live(Politeness::Polite),
            );
        }
        Some(root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::headless::Framed;
    use ui::vello::kurbo::{Point, Rect};
    use ui::widget::SidebarPlacement;

    /// A recorder for the session's tools door: what this window asked for,
    /// without a session to ask.
    static ASKED: std::sync::Mutex<Vec<ToolIntent>> = std::sync::Mutex::new(Vec::new());

    fn record(intent: ToolIntent, _on_behalf_of: Option<[u8; 16]>) -> Result<(), String> {
        ASKED.lock().unwrap().push(intent);
        Ok(())
    }

    fn asked() -> Vec<ToolIntent> {
        std::mem::take(&mut *ASKED.lock().unwrap())
    }

    /// A mounted filesystem leads to what is in it: the row offers a
    /// terminal there, and the person's press asks the session for exactly
    /// that folder. An unmounted partition offers no such thing.
    #[test]
    fn a_mounted_filesystem_leads_to_its_folder() {
        assert_eq!(
            ask_terminal(record, "/media/you/MEDIA".to_owned()),
            Ok(()),
            "the ask reaches the session"
        );
        assert_eq!(
            asked(),
            [ToolIntent::Terminal {
                directory: Some("/media/you/MEDIA".to_owned()),
            }]
        );

        let app = sample_app();
        let drive = app.drive().unwrap();
        let mounted = drive
            .partitions
            .iter()
            .find(|part| part.mountpoint.is_some())
            .expect("the sample machine has a mounted partition");
        assert_eq!(
            app.mounted_folder(&mounted.name),
            Ok(mounted.mountpoint.clone().unwrap())
        );
        let unmounted = crate::model::Partition {
            mountpoint: None,
            ..mounted.clone()
        };
        let mut app = app;
        app.topo.drives[app.selected].partitions = vec![unmounted.clone()];
        assert!(
            app.mounted_folder(&unmounted.name)
                .unwrap_err()
                .contains("is not mounted"),
            "an unmounted partition has no folder to open"
        );
        assert!(
            app.mounted_folder("gone1")
                .unwrap_err()
                .contains("no longer"),
            "a partition the probe no longer lists opens nothing"
        );
    }

    /// The partition's own page offers the terminal in its folder, and the
    /// sentence says where that is.
    #[test]
    fn the_partition_page_offers_a_terminal_in_its_folder() {
        let mut app = at_width(1500.0);
        app.select_part(Some(2)); // the sample home filesystem, at /home
        app.refresh_view();
        Framed::harness_mut(&mut app).poll_frame().unwrap();
        let page = Framed::harness(&app).inspection_json().unwrap();
        assert!(page.contains("Open a Terminal Here"), "{page}");
        assert!(page.contains("/home"), "the folder is named: {page}");
    }

    /// Opening Disk Usage asks the session for that app: measuring is its
    /// job, partitioning is this window's.
    #[test]
    fn disk_usage_is_asked_for_by_name() {
        assert_eq!(
            open_disk_usage_intent(),
            ToolIntent::OpenApplication {
                app: "so.tilo.disk-usage".to_owned()
            }
        );
    }

    fn sample_app() -> DisksApp {
        let mut app = DisksApp::sample();
        app.refresh_view();
        app
    }

    fn framed(app: DisksApp, w: f64, h: f64) -> Framed<DisksApp> {
        Framed::new(
            ui::State::new(),
            app,
            ui::headless::viewport((w, h)),
            Theme::dark(),
        )
    }

    fn at_width(width: f64) -> Framed<DisksApp> {
        framed(sample_app(), width, 800.0)
    }

    fn target(app: &Framed<DisksApp>, id: &str) -> Rect {
        Framed::harness(app)
            .targets()
            .get(id)
            .copied()
            .unwrap_or_else(|| panic!("no widget {id}"))
    }

    /// Where the partition map stands, as assistive technology finds it.
    fn partition_map(app: &Framed<DisksApp>) -> Rect {
        let harness = Framed::harness(app);
        let id = harness
            .a11y_node_named("Partition map")
            .expect("the page shows the partition map");
        harness
            .a11y_nodes()
            .into_iter()
            .find(|(node, _)| *node == id)
            .and_then(|(_, node)| node.bounds())
            .map(|bounds| Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1))
            .expect("the partition map has bounds")
    }

    fn click_at(app: &mut Framed<DisksApp>, point: Point) {
        Framed::harness_mut(app).click_at(point.x, point.y);
    }

    /// The centre of a drive's row in the sidebar list.
    fn drive_row(app: &Framed<DisksApp>, index: usize) -> Point {
        let list = target(app, "NavShell[0]/NavList");
        let row = f64::from(Theme::dark().tokens.metrics.nav_row_height);
        Point::new(list.x0 + 120.0, list.y0 + row * (index as f64 + 0.5))
    }

    /// The sample machine starts on its first drive, whole, on the overview.
    #[test]
    fn accessibility_names_and_routes_canvas_controls() {
        // --- phase: accessibility_exposes_and_routes_canvas_controls ---
        {
            let mut window =
                ui::framework::testing::TestApp::<crate::resource::DisksShell>::capture(None)
                    .unwrap();
            let nodes = window.harness().a11y_nodes();
            let bar = ui::window_toolbar::declared().expect("the window declares its slots");
            let named = |id: &str| {
                bar.actions
                    .iter()
                    .find(|action| action.id == id)
                    .map(|action| action.name.clone())
            };
            assert_eq!(named(view::RESCAN).as_deref(), Some("Rescan drives"));
            assert!(
                nodes
                    .iter()
                    .any(|(_, node)| node.label().is_some_and(|label| label.contains("Samsung"))),
                "the sample drive is accessible"
            );
            assert!(
                named(ActionKind::Format.key()).is_some(),
                "the drive's format action is in the bar"
            );
            let health = bar
                .views
                .as_ref()
                .and_then(|views| views.choices.iter().find(|view| view.label == "Health"))
                .expect("the drive's health is a view in the bar")
                .id
                .clone();
            window
                .harness()
                .press_window_slot(ui::window_toolbar::ToolbarEvent::Action { id: health });
            window.settle();
            assert_eq!(
                window.with_surface::<DisksApp, _>(|app| app.tab).unwrap(),
                Tab::Smart
            );
        }

        // --- phase: icon_only_hamburger_face_has_a_spoken_name ---
        {
            let face = ui::widget::ButtonFace::new(Rect::ZERO, "")
                .icon(ui::icons::Icon::Menu)
                .accessible_name("Show drives");
            assert_eq!(face.spoken_name(), "Show drives");
            assert_eq!(
                ui::widget::ButtonFace::new(Rect::ZERO, "Rescan")
                    .icon(ui::icons::Icon::Refresh)
                    .spoken_name(),
                "Rescan"
            );
        }
    }

    /// A drive's name from the document replaces its model.
    #[test]
    fn a_named_drive_shows_its_name() {
        let mut app = sample_app();
        let serial = app
            .drive()
            .and_then(|drive| drive.serial.clone())
            .expect("the sample nvme has a serial");
        app.adopt_names(&BTreeMap::from([(serial, "Workstation".to_owned())]));
        app.refresh_view();
        let drive = app.drive().unwrap();
        assert_eq!(app.display_name(drive), "Workstation");
    }

    #[test]
    fn operation_targets_are_revalidated_against_the_topology() {
        let app = sample_app();
        assert!(
            app.revalidate_operation_target(ActionKind::Benchmark, "nvme0n1")
                .is_ok()
        );
        assert!(
            app.revalidate_operation_target(ActionKind::Mount, "nvme0n1")
                .unwrap_err()
                .contains("whole disk")
        );
        assert!(
            app.revalidate_operation_target(ActionKind::Format, "no-longer-present")
                .unwrap_err()
                .contains("no longer present")
        );

        // What the running system stands on is refused whoever asks: an
        // agent's invocation reaches this check as the window's buttons do.
        let mut app = sample_app();
        let yoga = crate::probe::captured::yoga().unwrap();
        app.replace_topology(yoga.clone());
        for (action, leaf, reason) in [
            (ActionKind::Format, "nvme0n1", crate::model::HOLDS_SYSTEM),
            (ActionKind::Unmount, "nvme0n1p4", "Holds the home folders"),
            (ActionKind::Format, "nvme0n1p4", "Holds the home folders"),
            (ActionKind::Unmount, "nvme0n1p7", crate::model::HOLDS_SYSTEM),
            (ActionKind::Format, "nvme0n1p6", "Starts this device"),
        ] {
            let refusal = app.revalidate_operation_target(action, leaf).unwrap_err();
            assert!(refusal.contains(reason), "{action:?} {leaf}: {refusal}");
        }

        // An unlocked LUKS2 volume that serves no running system unmounts
        // through its mapping, and is never formatted out of its encryption.
        let mut data = yoga;
        data.drives[0].partitions[3].mountpoint = Some("/mnt/data".into());
        app.replace_topology(data);
        assert_eq!(
            app.revalidate_operation_target(ActionKind::Unmount, "nvme0n1p4"),
            Ok(("dm-1".to_owned(), Some("ext4".to_owned())))
        );
        assert!(
            app.revalidate_operation_target(ActionKind::Format, "nvme0n1p4")
                .unwrap_err()
                .contains("encrypted")
        );
        app.selected_part = Some(3);
        let drive = app.drive().unwrap().clone();
        assert!(!app.action_enabled(&drive, ActionKind::Format));
        assert!(app.action_enabled(&drive, ActionKind::Unmount));
    }

    #[test]
    fn results_toast_and_are_announced() {
        let mut app = sample_app();
        app.report("Mounted /dev/sda1".into(), StatusTone::Success);
        assert_eq!(app.shell.toast_text(), Some("Mounted /dev/sda1"));
        app.report("Mount failed: busy".into(), StatusTone::Danger);
        assert_eq!(app.shell.toast_text(), Some("Mount failed: busy"));
        let node = ui::widget::Decorator::accessibility(&app).expect("disks exports a window");
        assert_eq!(node.role, ui::a11y::Role::Window);
        assert!(
            node.children
                .iter()
                .any(|child| child.name.as_deref() == Some("Mount failed: busy"))
        );
    }

    /// The drive list docks beside the detail while the detail keeps its
    /// minimum width, and is a screen of its own when narrower.
    #[test]
    fn the_drive_list_docks_when_wide_and_is_a_screen_when_narrow() {
        let wide = at_width(1280.0);
        assert_eq!(wide.shell.geometry().sidebar, SidebarPlacement::Docked);
        assert!(wide.shell.is_sidebar_open());

        let narrow = at_width(520.0);
        assert_eq!(narrow.shell.geometry().sidebar, SidebarPlacement::Screen);
    }

    /// Choosing a drive or a tab from the window's controls repoints the detail.
    /// The window opens before this machine's storage is read (that reading
    /// is a worker's), on the detail page's own layout, and nothing moves
    /// when the drives arrive.
    #[test]
    fn the_opening_page_is_the_layout_the_drives_fill() {
        let mut opening = DisksApp::new();
        // A read that missed its frame budget: the page stands in.
        opening.loading = ui::time::now().checked_sub(ui::time::Duration::from_millis(500));
        let mut app = framed(opening, 1280.0, 800.0);
        Framed::harness_mut(&mut app).poll_frame().unwrap();
        let map = partition_map(&app);
        assert!(
            Framed::harness_mut(&mut app)
                .inspection_json()
                .unwrap()
                .contains(&ui::i18n::I18nManager::lookup_plain("arriving-loading")),
            "the opening page says the drives are on their way"
        );
        app.show_reading(Ok(Topology::sample()));
        Framed::harness_mut(&mut app).poll_frame().unwrap();
        assert!(app.drive().is_some(), "the reading fills the page");
        assert_eq!(
            partition_map(&app),
            map,
            "the partition map stays where the opening page put it"
        );
    }

    #[test]
    fn pointer_controls_repoint_the_detail() {
        // --- phase: choosing_a_drive_selects_it_and_resets_the_partition ---
        {
            let mut app = at_width(1280.0);
            app.select_part(Some(1));
            let point = drive_row(&app, 2);
            click_at(&mut app, point);
            assert_eq!(app.selected, 2, "the chosen row becomes the selection");
            assert_eq!(app.selected_part, None, "a fresh drive is the whole disk");
        }

        // --- phase: choosing_a_view_does_not_retarget_the_drive ---
        {
            let mut app = at_width(1280.0);
            ui::window_toolbar::deliver(ui::window_toolbar::ToolbarEvent::Action {
                id: Tab::Smart.view_id().to_owned(),
            });
            app.take_window_toolbar_events();
            assert_eq!(app.tab, Tab::Smart);
            assert_eq!(
                app.selected, 0,
                "changing views must not retarget the drive"
            );
        }

        // --- phase: a_disabled_action_does_nothing ---
        {
            let mut app = at_width(1280.0);
            ui::window_toolbar::deliver(ui::window_toolbar::ToolbarEvent::Action {
                id: ActionKind::Format.key().to_owned(),
            });
            app.take_window_toolbar_events();
            assert!(app.status.is_empty());
            assert!(app.running.is_none());
        }
    }

    /// The drive's views are the window bar's switcher: the bar shows
    /// Overview, Health and Partitions with the open one chosen, choosing
    /// one there shows it, and the page draws no tabs of its own.
    #[test]
    fn the_drives_views_are_the_window_bars_switcher() {
        let mut app = at_width(1280.0);
        Framed::harness_mut(&mut app).poll_frame().unwrap();
        let views = ui::window_toolbar::declared()
            .and_then(|bar| bar.views)
            .expect("the bar switches the drive's views");
        let labels: Vec<&str> = views
            .choices
            .iter()
            .map(|view| view.label.as_str())
            .collect();
        assert_eq!(labels, ["Overview", "Health", "Partitions"]);
        assert_eq!(views.chosen, Tab::Overview.view_id());
        assert!(
            !Framed::harness(&app).targets().contains_key("disks-tabs"),
            "the page draws no tabs of its own"
        );

        Framed::harness_mut(&mut app).press_window_slot(ui::window_toolbar::ToolbarEvent::Action {
            id: Tab::Partitions.view_id().to_owned(),
        });
        Framed::harness_mut(&mut app).poll_frame().unwrap();
        assert_eq!(app.tab, Tab::Partitions);
        assert_eq!(
            ui::window_toolbar::declared()
                .and_then(|bar| bar.views)
                .map(|views| views.chosen),
            Some(Tab::Partitions.view_id().to_owned()),
            "the switcher shows the view it chose"
        );
    }

    /// Partition selection scopes Mount and Unmount to the filesystem it names.
    #[test]
    fn partition_selection_scopes_the_actions() {
        // --- phase: choosing_a_partition_enables_what_applies ---
        {
            let mut app = sample_app();
            let drive = app.drive().unwrap();
            assert!(
                !app.action_enabled(drive, ActionKind::Mount),
                "no partition chosen: Mount is inert"
            );
            assert!(
                !app.action_enabled(drive, ActionKind::Unmount),
                "no partition chosen: Unmount is inert"
            );
            app.intents.push(Intent::SelectPartition(Some(1)));
            app.drain_intents();
            assert_eq!(app.selected_part, Some(1));
            let drive = app.drive().unwrap();
            assert_eq!(
                app.offer(drive, ActionKind::Unmount),
                Offer::Unavailable(crate::model::HOLDS_SYSTEM),
                "the root filesystem stays mounted"
            );
            app.select(1); // SATA SSD
            app.intents.push(Intent::SelectPartition(Some(0)));
            app.drain_intents();
            let drive = app.drive().unwrap();
            assert!(
                app.action_enabled(drive, ActionKind::Unmount),
                "mounted: Unmount applies"
            );
            assert!(
                !app.action_enabled(drive, ActionKind::Mount),
                "already mounted"
            );
        }

        // --- phase: choosing_the_selected_partition_again_returns_to_the_whole_disk ---
        {
            let mut app = sample_app();
            app.select_part(Some(1));
            app.select_part(Some(1));
            assert_eq!(app.selected_part, None);
            app.select_part(Some(2));
            app.intents.push(Intent::SelectPartition(None));
            app.drain_intents();
            assert_eq!(app.selected_part, None, "Whole disk clears the selection");
        }

        // --- phase: single_filesystem_disk_targets_its_sole_partition ---
        {
            let mut app = sample_app();
            app.select(3); // USB stick: one mounted partition, removable
            let d = app.drive().unwrap();
            assert!(
                app.action_enabled(d, ActionKind::Unmount),
                "sole fs is the implicit target"
            );
            assert!(
                app.action_enabled(d, ActionKind::Eject),
                "removable: Eject applies"
            );
            // A fixed, non-removable disk cannot be ejected.
            app.select(1); // SATA SSD
            assert!(!app.action_enabled(app.drive().unwrap(), ActionKind::Eject));
        }
    }

    /// A real machine's storage drives the app end to end; an empty probe shows an honest empty state rather than a sample.
    #[test]
    fn probed_topology_drives_the_app_or_an_honest_empty_state() {
        // --- phase: drives_real_or_sample_topology_end_to_end ---
        {
            let mut app = sample_app();
            app.replace_topology(crate::probe::captured::yoga().unwrap());
            for i in 0..app.topo.drives.len() {
                app.select(i);
                let d = app.drive().unwrap().clone();
                assert!(!d.name.is_empty(), "no nameless devices");
                assert!(d.size > 0, "no zero-size phantom devices");
                for tab in Tab::all() {
                    app.set_tab(tab);
                    app.refresh_view();
                }
                // Mount/Unmount enablement mirrors the real mount state, and
                // Eject only applies where the device is removable.
                if d.partitions.len() == 1 {
                    let mounted = d.partitions[0].mountpoint.is_some();
                    let dr = app.drive().unwrap();
                    assert_eq!(app.action_enabled(dr, ActionKind::Unmount), mounted);
                    assert_eq!(app.action_enabled(dr, ActionKind::Mount), !mounted);
                }
                assert_eq!(
                    app.action_enabled(app.drive().unwrap(), ActionKind::Eject),
                    d.removable
                );
            }
        }

        // --- phase: empty_probe_shows_the_empty_state_not_a_sample ---
        {
            let mut app = sample_app();
            app.replace_topology(Topology::default());
            app.refresh_view();
            assert!(app.drive().is_none());
            assert!(
                app.shell.has_sidebar(),
                "the drive list stays, empty, so the window keeps its shape"
            );
        }

        // --- phase: the_empty_states_rescan_action_rescans ---
        {
            let mut app = sample_app();
            app.replace_topology(Topology::default());
            let mut app = framed(app, 1280.0, 800.0);
            assert!(app.drive().is_none());
            Framed::harness_mut(&mut app).key("Tab");
            Framed::harness_mut(&mut app).key("Enter");
            assert_eq!(
                app.status, "Rescan found 0 drives",
                "the empty state's action reports what the rescan found"
            );
        }
    }
}
