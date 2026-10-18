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

use crate::model::{Drive, Topology};
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
    fn title(self) -> &'static str {
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
    topology: u64,
    names: u64,
    selected: usize,
    partition: Option<usize>,
    tab: Tab,
}

/// Where a [`DisksApp`]'s devices come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// This machine: probed with `lsblk` + `smartctl`, operated through
    /// udisks2.
    System,
    /// A fixed sample machine: operations report what they would run.
    Sample,
}

pub struct DisksApp {
    shell: NavShell,
    intents: Intents,
    /// This machine's block devices, as last probed (or the sample machine).
    topo: Topology,
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
            .sidebar_header("Drives", vec![Box::new(view::rescan_button(&intents))])
            .with_id("disks-shell");
        Self {
            shell,
            intents,
            topo,
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
        }
    }

    /// Hosted: edits and effects go through `dispatcher`. Only a live window
    /// probes this machine; a capture shows `fixture` (or the sample
    /// machine) and a past version shows no devices.
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: Dispatcher<Disks>,
        mode: RunMode,
        fixture: Option<Topology>,
    ) {
        self.dispatcher = Some(dispatcher);
        match mode {
            RunMode::Live => {
                if self.backend == Backend::System {
                    self.replace_topology(crate::probe::probe().unwrap_or_default());
                }
            }
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

    /// Which partition Mount / Unmount act on: the explicitly-selected one,
    /// or — when the whole disk is selected and it carries exactly one
    /// filesystem — that sole partition, so single-fs drives (USB sticks,
    /// data disks) mount without first drilling into the map.
    fn mount_target_part(&self, drive: &Drive) -> Option<usize> {
        if let Some(i) = self.part_index() {
            return Some(i);
        }
        if drive.partitions.len() == 1 {
            Some(0)
        } else {
            None
        }
    }

    /// Whether an action applies to the current selection. Inapplicable
    /// actions render disabled rather than silently no-op'ing.
    fn action_enabled(&self, drive: &Drive, action: ActionKind) -> bool {
        match action {
            ActionKind::Mount => self
                .mount_target_part(drive)
                .and_then(|i| drive.partitions.get(i))
                .is_some_and(|p| p.mountpoint.is_none()),
            ActionKind::Unmount => self
                .mount_target_part(drive)
                .and_then(|i| drive.partitions.get(i))
                .is_some_and(|p| p.mountpoint.is_some()),
            // Format works on either the selected partition or the whole disk.
            ActionKind::Format | ActionKind::Benchmark => true,
            ActionKind::Eject => drive.removable,
        }
    }

    // ---- the shell's content ------------------------------------------

    /// Rebuild the sidebar and the content when what they show changed.
    fn refresh_view(&mut self) {
        let key = ViewKey {
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
            b.topology == key.topology && b.names == key.names && b.selected == key.selected
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
                &self.intents,
            ))));
        }
        let Some(drive) = self.topo.drives.get(self.selected) else {
            self.shell.set_content_title("");
            self.shell.set_content(view::empty_state(&self.intents));
            return;
        };
        let mut enabled = [false; 5];
        for (slot, kind) in enabled.iter_mut().zip(ActionKind::bar()) {
            *slot = self.action_enabled(drive, kind);
        }
        let model = DetailModel {
            drive,
            tab: self.tab,
            partition: key.partition,
            enabled,
        };
        let content = view::detail(&model, &self.intents, &self.scroll);
        let title = self.display_name(drive).to_owned();
        self.shell.set_content_title(title);
        self.shell.set_content(content);
    }

    /// Apply what the window's controls asked for.
    fn drain_intents(&mut self) {
        for intent in self.intents.take() {
            match intent {
                Intent::SelectDrive(index) => self.select(index),
                Intent::SelectPartition(part) => self.select_part(part),
                Intent::SetTab(tab) => self.set_tab(tab),
                Intent::Run(action) => self.trigger_action(action),
                Intent::Rescan => self.rescan(),
            }
        }
    }

    // ---- results ---------------------------------------------------------

    /// Record a result: the status the window announces and the toast that
    /// shows it once.
    fn report(&mut self, text: String, tone: StatusTone) {
        self.status.clone_from(&text);
        self.shell.toast(text, tone.kind());
        self.dirty = true;
    }

    // ---- interaction ---------------------------------------------------

    /// An action button: invoke the operation on the drive or partition it
    /// targets, as that row offers it — formatting asks for the device's
    /// name before anything runs.
    fn trigger_action(&mut self, action: ActionKind) {
        let Some(drive) = self.drive() else { return };
        if !self.action_enabled(drive, action) {
            return; // inapplicable buttons are inert
        }
        // Resolve what the action targets: Mount/Unmount snap to a filesystem;
        // Eject always the whole device; Format/Benchmark follow the selection.
        let part = match action {
            ActionKind::Mount | ActionKind::Unmount => self.mount_target_part(drive),
            ActionKind::Eject => None,
            ActionKind::Format | ActionKind::Benchmark => self.part_index(),
        };
        let (row, leaf) = match part.and_then(|i| drive.partitions.get(i)) {
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
            Some("health") => self.set_tab(Tab::Smart),
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
            DisksEffect::Rescan => {
                self.rescan();
                return Ok(Effected::value(&self.status));
            }
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
        };
        self.run_operation(operation.0, operation.1, cx)
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
        device: String,
        cx: &mut EffectCx<Disks, Self>,
    ) -> EffectAnswer {
        if self.running.is_some() {
            return Err("another disk operation is still running".into());
        }
        // A caller's device name is no authority to touch an arbitrary
        // `/dev` node, and the filesystem type comes from the probe.
        let fstype = self.revalidate_operation_target(kind, &device)?;
        let dev = format!("/dev/{device}");
        if self.backend == Backend::Sample {
            self.report(kind.plan(&dev), StatusTone::Info);
            return Ok(Effected::value(&self.status));
        }
        self.running = Some(kind);
        self.report(format!("{} {dev}…", kind.gerund()), StatusTone::Info);
        Ok(cx.thread(
            move |_| ops::run(kind.op(), &device, fstype.as_deref()),
            move |app: &mut Self, result: Result<String, String>, _| {
                app.running = None;
                if kind.changes_topology() {
                    app.reprobe();
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

    /// Probe this machine again, keeping the selection where it still
    /// exists. The sample machine stays as it is.
    fn reprobe(&mut self) {
        if self.backend == Backend::System {
            self.replace_topology(crate::probe::probe().unwrap_or_default());
        }
    }

    /// Rescan storage devices: the Rescan button, `r`, and the effect.
    fn rescan(&mut self) {
        self.reprobe();
        self.report(found_message(self.topo.drives.len()), StatusTone::Success);
    }

    /// Resolve an operation's target against the latest locally-probed
    /// topology. An invocation may arrive after a search result or an
    /// agent's view has gone stale, so a device name is never authority to
    /// touch an arbitrary `/dev/*` node.
    ///
    /// The return value derives the filesystem type from the current probe
    /// instead of trusting the caller.
    fn revalidate_operation_target(
        &self,
        action: ActionKind,
        leaf: &str,
    ) -> Result<Option<String>, String> {
        for drive in &self.topo.drives {
            if drive.name == leaf {
                return match action {
                    ActionKind::Format | ActionKind::Benchmark => Ok(None),
                    ActionKind::Eject if drive.removable => Ok(None),
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
                return match action {
                    ActionKind::Mount if part.mountpoint.is_none() => Ok(part.fstype.clone()),
                    ActionKind::Unmount if part.mountpoint.is_some() => Ok(part.fstype.clone()),
                    ActionKind::Mount => Err(format!("/dev/{leaf} is already mounted")),
                    ActionKind::Unmount => Err(format!("/dev/{leaf} is not mounted")),
                    ActionKind::Format | ActionKind::Benchmark => Ok(part.fstype.clone()),
                    ActionKind::Eject => Err("eject targets a whole removable drive".into()),
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
        self.drain_intents();
        self.refresh_view();
        self.shell.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.drain_intents();
        self.refresh_view();
        self.shell.update(state, path) || self.dirty
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
            assert!(
                nodes
                    .iter()
                    .any(|(_, node)| node.label() == Some("Rescan drives"))
            );
            assert!(
                nodes
                    .iter()
                    .any(|(_, node)| node.label().is_some_and(|label| label.contains("Samsung"))),
                "the sample drive is accessible"
            );
            assert!(
                nodes
                    .iter()
                    .any(|(_, node)| node.label() == Some("Format…")),
                "the drive's format action is accessible"
            );
            let smart = window
                .harness()
                .a11y_node_named("Health")
                .expect("the drive health tab");
            window
                .harness()
                .perform_a11y_action(smart, &ui::a11y::Action::Activate)
                .unwrap();
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

    /// Operation targets are revalidated against the probed topology.
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
    }

    /// A result shows as a toast, and the window announces it.
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

    /// The sidebar docks beside the detail when wide and becomes a drawer when narrow, and each stays usable.
    #[test]
    fn the_drive_list_docks_when_wide_and_collapses_to_a_drawer_when_narrow() {
        let wide = at_width(1280.0);
        assert_eq!(wide.shell.geometry().sidebar, SidebarPlacement::Docked);
        assert!(wide.shell.is_sidebar_open());

        let narrow = at_width(520.0);
        assert_eq!(narrow.shell.geometry().sidebar, SidebarPlacement::Drawer);
        assert!(
            !narrow.shell.is_sidebar_open(),
            "the drawer starts closed so the detail has the window"
        );
    }

    /// Choosing a drive or a tab from the window's controls repoints the detail.
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

        // --- phase: choosing_a_tab_does_not_retarget_the_drive ---
        {
            let mut app = at_width(1280.0);
            let strip = target(&app, "disks-tabs");
            // The middle of three segments is the Health tab.
            click_at(
                &mut app,
                Point::new(strip.x0 + strip.width() * 0.5, strip.center().y),
            );
            assert_eq!(app.tab, Tab::Smart);
            assert_eq!(app.selected, 0, "changing tabs must not retarget the drive");
        }

        // --- phase: a_disabled_action_does_nothing ---
        {
            let mut app = at_width(1280.0);
            // The whole multi-partition disk has no filesystem to mount.
            Framed::harness_mut(&mut app)
                .click_named("disks-action-mount")
                .unwrap();
            assert!(app.status.is_empty());
        }
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

    /// A real probed topology drives the app end to end; an empty probe shows an honest empty state rather than a sample.
    #[test]
    fn probed_topology_drives_the_app_or_an_honest_empty_state() {
        // --- phase: drives_real_or_sample_topology_end_to_end ---
        {
            let mut app = sample_app();
            if let Some(topo) = crate::probe::probe() {
                app.replace_topology(topo);
            }
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
            // The action is reachable from the keyboard.
            Framed::harness_mut(&mut app).key("Tab");
            Framed::harness_mut(&mut app).key("Enter");
            assert_eq!(
                app.status, "Rescan found 0 drives",
                "the empty state's action reports what the rescan found"
            );
        }
    }
}
