//! Storage devices in a retained NavShell. Device state and selection are
//! local; authored names and authorized operations use the resource framework.
use crate::model::{Drive, Partition, Topology};
use crate::ops::{self, Op};
use crate::resource::{Disks, DisksEffect};
use crate::view::{self, DetailModel, Intent, Intents};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::rc::Rc;
use ui::framework::{Dispatcher, EffectAnswer, EffectCx, Effected, LiveRow, RunMode};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::time::{Duration, Instant};
use ui::vello::Scene;
use ui::vello::kurbo::{Affine, Point, Rect};
use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind, paint_transient};
use ui::widget::{AppModel, ModelPath, NavShell, Widget};
use ui::winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use ui::winit::keyboard::Key;
const TOAST_TTL: Duration = Duration::from_secs(5);
impl ActionKind {
    /// The real operation this action maps to, if any (Eject/Mount/… all do;
    /// this is a total map today but kept explicit for clarity).
    fn op(self) -> Op {
        match self {
            ActionKind::Mount => Op::Mount,
            ActionKind::Unmount => Op::Unmount,
            ActionKind::Format => Op::Format,
            ActionKind::Benchmark => Op::Benchmark,
            ActionKind::Eject => Op::Eject,
        }
    }
    /// Present-tense verb for the "…in progress" status line.
    fn gerund(self) -> &'static str {
        match self {
            ActionKind::Mount => "Mounting",
            ActionKind::Unmount => "Unmounting",
            ActionKind::Format => "Formatting",
            ActionKind::Benchmark => "Benchmarking",
            ActionKind::Eject => "Ejecting",
        }
    }
    /// The action's name, for a status line.
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
    pub(crate) fn from_key(k: &str) -> Tab {
        match k {
            "smart" => Tab::Smart,
            "partitions" => Tab::Partitions,
            _ => Tab::Overview,
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
    pub(crate) fn from_key(k: &str) -> Option<ActionKind> {
        match k {
            "mount" => Some(ActionKind::Mount),
            "unmount" => Some(ActionKind::Unmount),
            "format" => Some(ActionKind::Format),
            "benchmark" => Some(ActionKind::Benchmark),
            "eject" => Some(ActionKind::Eject),
            _ => None,
        }
    }
    /// Destructive actions require an explicit confirm dialog.
    pub(crate) fn destructive(self) -> bool {
        matches!(self, ActionKind::Format)
    }
    fn confirm_title(self, target: &str) -> String {
        match self {
            ActionKind::Format => format!("Format {target}?"),
            ActionKind::Unmount => format!("Unmount {target}?"),
            ActionKind::Mount => format!("Mount {target}?"),
            ActionKind::Benchmark => format!("Benchmark {target}?"),
            ActionKind::Eject => format!("Eject {target}?"),
        }
    }
    fn confirm_body(self, target: &str) -> String {
        match self {
            ActionKind::Format => {
                format!("Formatting erases all data on {target}. This cannot be undone.")
            }
            ActionKind::Unmount => {
                format!("Detach {target} from the filesystem. Programs using it may lose access.")
            }
            ActionKind::Mount => format!("Attach {target} so its files become available."),
            ActionKind::Benchmark => {
                format!("Run a read benchmark on {target}. It may briefly slow the system.")
            }
            ActionKind::Eject => format!("Power down and detach {target} so it is safe to unplug."),
        }
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

/// The result toast currently on screen.
struct ToastState {
    text: String,
    tone: StatusTone,
    shown: Instant,
    /// A capture holds it so it is still there when the frame is taken.
    pinned: bool,
    /// The close control, from the last paint.
    close: Rect,
}

/// What the shell currently shows, so it is rebuilt only when it changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ViewKey {
    topology: u64,
    selected: usize,
    partition: Option<usize>,
    tab: Tab,
    running: bool,
}

/// Source of the local machine topology and device operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    System,
    Sample,
}

pub struct DisksApp {
    shell: NavShell,
    intents: Intents,
    topo: Topology,
    backend: Backend,
    names: BTreeMap<String, String>,
    dispatcher: Option<Dispatcher<Disks>>,
    running: Option<ActionKind>,
    selected: usize,
    selected_part: Option<usize>,
    tab: Tab,
    status: String,
    shown_status: String,
    topo_rev: u64,
    dirty: bool,
    scroll: Rc<Cell<(f32, f32)>>,
    built: Option<ViewKey>,
    toast: Option<ToastState>,
    cursor: Point,
}
impl Default for DisksApp {
    fn default() -> Self {
        Self::new()
    }
}
impl DisksApp {
    pub fn new() -> Self {
        Self::with_backend(Backend::System, Topology::default())
    }
    pub fn sample() -> Self {
        Self::with_backend(Backend::Sample, Topology::sample())
    }

    fn with_backend(backend: Backend, topo: Topology) -> Self {
        let intents = Intents::default();
        let shell = NavShell::new()
            .sidebar_header("Drives", vec![Box::new(view::rescan_button(&intents))])
            .with_id("disks-shell");
        Self {
            shell,
            intents,
            topo,
            backend,
            names: BTreeMap::new(),
            dispatcher: None,
            running: None,
            selected: 0,
            selected_part: None,
            tab: Tab::Overview,
            status: String::new(),
            shown_status: String::new(),
            topo_rev: 0,
            dirty: true,
            scroll: Rc::new(Cell::new((0.0, 0.0))),
            built: None,
            toast: None,
            cursor: Point::ZERO,
        }
    }
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
                    self.topo = crate::probe::probe().unwrap_or_default();
                }
            }
            RunMode::Capture => {
                self.backend = Backend::Sample;
                self.topo = fixture.unwrap_or_else(Topology::sample);
            }
            RunMode::Past => {
                self.backend = Backend::Sample;
                self.topo = Topology::default();
            }
        }
        self.clamp_selection();
        self.topo_rev = self.topo_rev.wrapping_add(1);
        self.dirty = true;
    }
    pub(crate) fn adopt_names(&mut self, names: &BTreeMap<String, String>) {
        if self.names != *names {
            self.names.clone_from(names);
            self.topo_rev = self.topo_rev.wrapping_add(1);
            self.dirty = true;
        }
    }
    fn display_name<'a>(&'a self, drive: &'a Drive) -> &'a str {
        drive
            .serial
            .as_ref()
            .and_then(|serial| self.names.get(serial))
            .map_or(drive.model.as_str(), String::as_str)
    }
    fn shown(&self, drive: &Drive) -> Drive {
        let mut shown = drive.clone();
        self.display_name(drive).clone_into(&mut shown.model);
        shown
    }
    pub(crate) fn selected_serial(&self) -> Option<String> {
        self.drive()?.serial.clone()
    }
    pub(crate) fn is_running(&self) -> bool {
        self.running.is_some()
    }
    pub(crate) fn drive_rows(&self) -> Vec<LiveRow> {
        self.topo
            .drives
            .iter()
            .map(|drive| crate::resource::drive_row(drive, &self.names))
            .collect()
    }
    fn clamp_selection(&mut self) {
        self.selected = self.selected.min(self.topo.drives.len().saturating_sub(1));
        self.selected_part = self.part_index();
    }
    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        let usb = self.topo.drives.iter().position(|drive| drive.removable);
        let base = scenario.map(|name| name.strip_prefix("phone-").unwrap_or(name));
        match base {
            Some("smart" | "health") => self.set_tab(Tab::Smart),
            Some("partitions") => self.set_tab(Tab::Partitions),
            Some("partition") => self.select_part(Some(2)),
            Some("removable" | "usb") => {
                if let Some(usb) = usb {
                    self.select(usb);
                }
            }
            Some("status-ok" | "toast") => {
                self.status = "Mounted /dev/sdb1 at /run/media/sdb1".into();
            }
            Some("status-fail" | "toast-error") => {
                self.status = "Format failed — device is busy".into();
            }
            Some("narrow" | "drawer") => self.snap_sidebar(true),
            Some("failing") => {
                self.select(2);
                self.set_tab(Tab::Smart);
            }
            Some("format") => {
                self.select_part(Some(2));
                self.trigger_action(ActionKind::Format);
            }

            _ => {}
        }
        self.update_status_toast(true);
        self.dirty = true;
    }
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
        self.dirty = true;
        if self.backend == Backend::Sample {
            self.status = kind.plan(&dev);
            return Ok(Effected::value(&self.status));
        }
        self.running = Some(kind);
        self.status = format!("{} {dev}…", kind.gerund());
        Ok(cx.thread(
            move |_| ops::run(kind.op(), &device, fstype.as_deref()),
            move |app: &mut Self, result: Result<String, String>, _| {
                app.running = None;
                app.dirty = true;
                if kind.changes_topology() {
                    app.reprobe();
                }
                match result {
                    Ok(message) => {
                        app.status.clone_from(&message);
                        Ok(Effected::value(message))
                    }
                    Err(error) => {
                        app.status = format!("{} failed — {error}", kind.title());
                        Err(error)
                    }
                }
            },
        ))
    }
    fn reprobe(&mut self) {
        if self.backend == Backend::System {
            self.topo = crate::probe::probe().unwrap_or_default();
            self.clamp_selection();
            self.topo_rev = self.topo_rev.wrapping_add(1);
            self.dirty = true;
        }
    }
    fn rescan(&mut self) {
        self.reprobe();
        self.status = format!("Rescanned — {} devices", self.topo.drives.len());
        self.dirty = true;
    }
    fn drive(&self) -> Option<&Drive> {
        self.topo.drives.get(self.selected)
    }
    fn part_index(&self) -> Option<usize> {
        let n = self.drive()?.partitions.len();
        self.selected_part.filter(|&i| i < n)
    }
    fn part(&self) -> Option<&Partition> {
        let d = self.drive()?;
        self.part_index().and_then(|i| d.partitions.get(i))
    }
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
    fn trigger_action(&mut self, action: ActionKind) {
        let Some(drive) = self.drive() else { return };
        if self.is_running() || !self.action_enabled(drive, action) {
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
            // detail + action bar can never point at a stale partition.
            if changed {
                self.selected_part = None;
                self.scroll.set((0.0, 0.0));
            }
            self.dirty = true;
        }
    }
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
    fn refresh_view(&mut self) {
        let key = ViewKey {
            topology: self.topo_rev,
            selected: self.selected,
            partition: self.part_index(),
            tab: self.tab,
            running: self.is_running(),
        };
        if self.built == Some(key) {
            return;
        }
        let listed_before = self
            .built
            .is_some_and(|b| b.topology == key.topology && b.selected == key.selected);
        self.built = Some(key);
        if !listed_before {
            self.shell.set_sidebar(Some(Box::new(view::drive_list(
                &Topology {
                    drives: self.topo.drives.iter().map(|d| self.shown(d)).collect(),
                },
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
            *slot = !self.is_running() && self.action_enabled(drive, kind);
        }
        let drive = self.shown(drive);
        let model = DetailModel {
            drive: &drive,
            tab: self.tab,
            partition: key.partition,
            enabled,
        };
        let content = view::detail(&model, &self.intents, &self.scroll);
        self.shell.set_content_title(drive.model.clone());
        self.shell.set_content(content);
    }

    pub fn snap_sidebar(&mut self, open: bool) {
        if open {
            self.shell.open_sidebar();
        } else {
            self.shell.close_sidebar();
        }
        self.shell.snap();
        self.dirty = true;
    }
    fn drain_intents(&mut self) {
        for intent in self.intents.take() {
            match intent {
                Intent::SelectDrive(i) => self.select(i),
                Intent::SelectPartition(part) => self.select_part(part),
                Intent::SetTab(tab) => self.set_tab(tab),
                Intent::Run(action) => self.trigger_action(action),
                Intent::Rescan => {
                    if let Some(dispatcher) = &self.dispatcher {
                        dispatcher.effect(&DisksEffect::Rescan);
                    }
                }
            }
        }
    }
    fn update_status_toast(&mut self, pinned: bool) {
        if self.status == self.shown_status {
            return;
        }
        self.shown_status.clone_from(&self.status);
        if self.status.is_empty() {
            self.toast = None;
            return;
        }
        let tone = if self.status.contains("failed") {
            StatusTone::Danger
        } else if self.running.is_some() || self.status.starts_with("Sample") {
            StatusTone::Info
        } else {
            StatusTone::Success
        };
        self.toast = Some(ToastState {
            text: self.status.clone(),
            tone,
            shown: ui::time::now(),
            pinned,
            close: Rect::ZERO,
        });
    }
    fn toast_visible(&self) -> bool {
        self.toast
            .as_ref()
            .is_some_and(|t| t.pinned || t.shown.elapsed() < TOAST_TTL)
    }
    fn paint_toast(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        window: Rect,
    ) {
        if !self.toast_visible() {
            self.toast = None;
            return;
        }
        let Some(toast) = self.toast.as_mut() else {
            return;
        };
        let close_hovered = toast.close.contains(self.cursor);
        let card = ToastCard::new(&toast.text, toast.tone.kind())
            .closable(true)
            .close_hovered(close_hovered);
        let hit = paint_transient(
            scene,
            transform,
            text,
            theme,
            window,
            ToastAnchor::BottomCenter,
            &card,
        );
        toast.close = hit.close_rect;
    }
}

impl ui::widget::Decorator for DisksApp {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    type Inner = NavShell;
    fn inner(&self) -> &NavShell {
        &self.shell
    }
    fn inner_mut(&mut self) -> &mut NavShell {
        &mut self.shell
    }
    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        self.drain_intents();
        self.refresh_view();
        self.update_status_toast(false);
        self.shell.sync(state, path);
    }
    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
        let changed = self.shell.update(state, path);
        self.drain_intents();
        self.refresh_view();
        self.update_status_toast(false);
        changed || self.dirty
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        // The toast's close control.
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        ) && self
            .toast
            .as_ref()
            .is_some_and(|t| t.close.contains(self.cursor))
        {
            self.toast = None;
            self.dirty = true;
            *ctx.handled = true;
            return;
        }
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
                self.intents.push(Intent::Rescan);
                self.drain_intents();
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
        let window = presence
            .viewport
            .get()
            .unwrap_or_else(|| self.shell.get_rect());
        self.paint_toast(scene, transform, text, theme, window);
    }

    fn needs_animation_frame(&self) -> bool {
        self.toast
            .as_ref()
            .is_some_and(|t| !t.pinned && t.shown.elapsed() < TOAST_TTL)
            || self.shell.needs_animation_frame()
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
        if let Some(shell) = self.shell.accessibility() {
            root.children.push(shell);
        }
        if !self.status.is_empty() {
            let mut status = AccessNode::new(Role::Status).with_name(&self.status);
            status.live = Some(Politeness::Polite);
            root.children.push(status);
        }
        Some(root)
    }
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        let handled = self
            .shell
            .perform_semantic_action(target, action, state, path);
        self.drain_intents();
        handled
    }
}

#[cfg(test)]
mod retained_tests {
    use super::*;
    use ui::headless::Driver;
    use ui::widget::SidebarPlacement;
    fn mounted(app: DisksApp, width: u32, height: u32) -> Driver {
        Driver::for_root(ui::State::new(), app, (width, height), Theme::dark())
    }
    fn activate(driver: &mut Driver, label: &str) {
        let id = driver
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| node.label() == Some(label))
            .map(|(id, _)| id)
            .unwrap_or_else(|| panic!("missing control {label}"));
        driver
            .perform_a11y_action(id, &ui::a11y::Action::Activate)
            .unwrap();
    }
    #[test]
    fn retained_navigation_scopes_actions_and_keeps_tab_selection() {
        let mut driver = mounted(DisksApp::sample(), 1280, 800);
        let topology = Topology::sample();
        for drive in &topology.drives {
            activate(&mut driver, &drive.model);
            let app = driver.root_mut::<DisksApp>();
            assert_eq!(app.drive().unwrap().name, drive.name);
            assert_eq!(app.selected_part, None);
            assert_eq!(
                app.action_enabled(app.drive().unwrap(), ActionKind::Eject),
                drive.removable
            );
            if drive.partitions.len() == 1 {
                let mounted = drive.partitions[0].mountpoint.is_some();
                assert_eq!(
                    app.action_enabled(app.drive().unwrap(), ActionKind::Unmount),
                    mounted
                );
                assert_eq!(
                    app.action_enabled(app.drive().unwrap(), ActionKind::Mount),
                    !mounted
                );
            }
        }
        activate(&mut driver, &topology.drives[0].model);
        let selected = driver.root_mut::<DisksApp>().selected;
        activate(&mut driver, "Health");
        assert_eq!(driver.root_mut::<DisksApp>().tab, Tab::Smart);
        assert_eq!(driver.root_mut::<DisksApp>().selected, selected);
        activate(&mut driver, "Partitions");
        assert_eq!(driver.root_mut::<DisksApp>().tab, Tab::Partitions);
        assert!(!driver.render_rgba().is_empty());
    }
    fn click_label(driver: &mut Driver, label: &str) {
        let bounds = driver
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| node.label() == Some(label))
            .and_then(|(_, node)| node.bounds())
            .unwrap_or_else(|| panic!("missing bounds for {label}"));
        driver.click_at((bounds.x0 + bounds.x1) / 2.0, (bounds.y0 + bounds.y1) / 2.0);
    }
    #[test]
    fn partition_map_rows_and_whole_disk_change_the_real_action_target() {
        let mut driver = mounted(DisksApp::sample(), 1280, 800);
        click_label(&mut driver, "Select partition nvme0n1p2");
        assert_eq!(driver.root_mut::<DisksApp>().part_index(), Some(1));
        assert!(
            driver
                .root_mut::<DisksApp>()
                .action_enabled(&Topology::sample().drives[0], ActionKind::Unmount)
        );
        activate(&mut driver, "Partitions");
        driver.click_named("disks-partition-2").unwrap();
        assert_eq!(driver.root_mut::<DisksApp>().part_index(), Some(2));
        activate(&mut driver, "Overview");
        click_label(&mut driver, "Select whole disk");
        assert_eq!(driver.root_mut::<DisksApp>().part_index(), None);
        assert!(
            !driver
                .root_mut::<DisksApp>()
                .action_enabled(&Topology::sample().drives[0], ActionKind::Unmount)
        );
    }
    #[test]
    fn sidebar_wheel_keeps_long_device_lists_reachable_at_large_text() {
        let mut topology = Topology::sample();
        let template = topology.drives[0].clone();
        topology.drives = (0..30)
            .map(|index| {
                let mut drive = template.clone();
                drive.name = format!("test{index}");
                drive.model = format!("Drive {index}");
                drive
            })
            .collect();
        let mut driver = Driver::for_root(
            ui::State::new(),
            DisksApp::with_backend(Backend::Sample, topology),
            (1280, 600),
            Theme::dark().with_text_scale(2.0),
        );
        let sidebar = driver
            .root_mut::<DisksApp>()
            .shell
            .geometry()
            .sidebar_column;
        driver.click_at(sidebar.center().x, sidebar.center().y);
        driver.scroll(0.0, -100.0);
        let last = driver
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| node.label() == Some("Drive 29"))
            .and_then(|(_, node)| node.bounds())
            .unwrap();
        assert!(
            last.y0 >= sidebar.y0 && last.y1 <= sidebar.y1,
            "last drive scrolls into the sidebar viewport"
        );
        driver.click_at((last.x0 + last.x1) / 2.0, (last.y0 + last.y1) / 2.0);
        assert_eq!(driver.root_mut::<DisksApp>().selected, 29);
    }
    #[test]
    fn retained_sidebar_resolves_wide_and_narrow_windows() {
        let mut wide = mounted(DisksApp::sample(), 1280, 800);
        assert_eq!(
            wide.root_mut::<DisksApp>().shell.geometry().sidebar,
            SidebarPlacement::Docked
        );
        assert!(wide.root_mut::<DisksApp>().shell.is_sidebar_open());
        let mut narrow = mounted(DisksApp::sample(), 520, 420);
        assert_eq!(
            narrow.root_mut::<DisksApp>().shell.geometry().sidebar,
            SidebarPlacement::Drawer
        );
        assert!(!narrow.root_mut::<DisksApp>().shell.is_sidebar_open());
        narrow.root_mut::<DisksApp>().snap_sidebar(true);
        narrow.relayout();
        assert!(narrow.root_mut::<DisksApp>().shell.is_sidebar_open());
    }
    #[test]
    fn status_and_latest_target_validation_are_preserved() {
        let mut app = DisksApp::sample();
        app.status = "Rescan complete".into();
        let node = app.accessibility().unwrap();
        assert_eq!(node.role, ui::a11y::Role::Window);
        assert!(
            node.children
                .iter()
                .any(|n| n.name.as_deref() == Some("Rescan complete") && n.live.is_some())
        );
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
        assert!(!app.action_enabled(app.drive().unwrap(), ActionKind::Mount));
        assert!(!app.action_enabled(app.drive().unwrap(), ActionKind::Unmount));
        app.select_part(Some(0));
        assert!(app.action_enabled(app.drive().unwrap(), ActionKind::Unmount));
        assert!(!app.action_enabled(app.drive().unwrap(), ActionKind::Mount));
        app.select(1);
        assert_eq!(app.selected_part, None);
        let mut empty = mounted(
            DisksApp::with_backend(Backend::Sample, Topology::default()),
            1280,
            800,
        );
        assert!(empty.root_mut::<DisksApp>().topo.drives.is_empty());
        assert!(
            empty
                .a11y_nodes()
                .iter()
                .any(|(_, n)| n.label() == Some("Rescan drives") || n.label() == Some("Rescan"))
        );
    }
}
