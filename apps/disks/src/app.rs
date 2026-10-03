//! Root of the Disks app.
//!
//! [`DisksApp`] is a [`NavShell`] window: the drive list is the sidebar and the
//! selected drive is the content (see [`crate::view`]). This module owns what
//! the shell cannot: the parsed topology, the selection, the confirm sheet for
//! destructive actions and the transient result toast. All mutable UI state
//! (selection, active tab, pending confirmation) is written back into the
//! Automerge document so it is undoable and collaborative.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::mpsc;
use std::thread;

use ui::automerge::{AutoCommit, ObjId};
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
#[cfg(test)]
use ui::tilo_actions::{ConfirmationPolicy, ExecutionMode, ResultSpec};
use ui::tilo_actions::{OperationKind, Resource};
use ui::time::{Duration, Instant};
use ui::tokens::TextRole;
use ui::vello::Scene;
use ui::vello::kurbo::{Affine, Point, Rect};
use ui::widget::modal_dialog::{self, ModalAction, ModalHit, ModalLayout, ModalSpec};
use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind, paint_transient};
use ui::widget::{AppModel, ControlSize, ModelPath, NavShell, TextInput, Widget};
use ui::winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use ui::winit::keyboard::{Key, NamedKey};

use crate::doc::DisksDoc;
use crate::model::{Drive, Topology};
use crate::ops::{self, Op};
use crate::resource::DisksAction;
use crate::view::{self, DetailModel, Intent, Intents};

/// How long a result toast stays before it hides itself.
const TOAST_TTL: Duration = Duration::from_secs(5);

/// Result of a background action thread, drained in `sync`.
struct ActionResult {
    text: String,
    error: Option<String>,
    /// Whether the topology changed and should be re-probed.
    refresh: bool,
}

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
    fn confirm_body(self, target: &str, live: bool) -> String {
        match self {
            // In the live binary Format really erases the device; only the
            // hermetic build is a no-op, so the reassurance that nothing is
            // written must never appear when we can actually destroy data.
            ActionKind::Format if live => {
                format!("Formatting erases all data on {target}. This cannot be undone.")
            }
            ActionKind::Format => format!(
                "Formatting erases all data on {target}. This cannot be undone. \
                 Live disk access is off, so nothing will be written."
            ),
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

#[derive(Clone, Copy, Debug)]
struct Pending {
    action: ActionKind,
    drive: usize,
    /// Partition the action targets, or `None` for the whole disk.
    part: Option<usize>,
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

/// The confirm sheet's geometry from the last paint, for hit-testing.
struct ModalState {
    layout: ModalLayout,
    input: Rect,
    ready: bool,
}

/// What the shell currently shows, so it is rebuilt only when it changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ViewKey {
    topology: u64,
    selected: usize,
    partition: Option<usize>,
    tab: Tab,
}

pub struct DisksApp {
    shell: NavShell,
    intents: Intents,
    topo: Topology,
    topo_json_cache: String,
    /// Bumped whenever `topo` is replaced.
    topo_rev: u64,
    selected: usize,
    /// Selected partition within the current drive, or `None` for the whole
    /// disk. Drives every partition-scoped action + detail highlight.
    selected_part: Option<usize>,
    tab: Tab,
    pending: Option<Pending>,
    /// Typed confirmation text for a destructive (Format) action. The Erase
    /// button only unlocks once this matches the target device name.
    confirm_input: TextInput,
    status: String,
    /// True in the live binary (`DISKS_LIVE`): actions actually execute.
    /// False for `--shots` and unit tests, which stay inert.
    live: bool,
    /// Receiver for the currently-running background action, if any.
    action_rx: Option<mpsc::Receiver<ActionResult>>,
    action_invocation: Option<tilo_actions::InvocationId>,
    completed_action: Option<tilo_actions::InvocationUpdate>,
    seeded: bool,
    dirty: bool,
    /// The detail page's scroll offset, kept across rebuilds.
    scroll: Rc<Cell<(f32, f32)>>,
    built: Option<ViewKey>,
    toast: Option<ToastState>,
    modal: Option<ModalState>,
    /// The pointer, for the sheet's hover states and the toast's close control.
    cursor: Point,
    shared: crate::resource::ActionSink,
}

impl Default for DisksApp {
    fn default() -> Self {
        Self::new()
    }
}

impl DisksApp {
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Disks>,
    ) {
        self.shared.attach(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.shared.is_hosted()
    }

    /// Test-only: mark the surface hosted without a real framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.shared.mark_hosted_for_test();
    }

    /// Standalone / `--shots` / unit tests: one tree from [`tilo_resource::publish`].
    pub(crate) fn publish_resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        use ui::tilo_resource::Resource as _;
        let mut state = crate::resource::Disks::read(doc).unwrap_or_default();
        // Prefer the widget's live topology when the document has not been
        // seeded yet (first-frame tests before sync).
        if state.topology.drives.is_empty() && !self.topo.drives.is_empty() {
            state.topology = self.topo.clone();
        }
        state.live = self.live;
        vec![ui::tilo_resource::publish::<crate::resource::Disks>(
            "disks",
            "Storage devices",
            &state,
        )]
    }

    pub(crate) fn adopt_shared(&mut self, state: &crate::resource::DisksState) {
        if let Ok(json) = serde_json::to_string(&state.topology)
            && json != self.topo_json_cache
        {
            self.topo = state.topology.clone();
            self.topo_json_cache = json;
            self.topo_rev += 1;
            self.dirty = true;
        }
        if self.selected != state.selected {
            self.selected = state.selected;
            self.dirty = true;
        }
        if self.selected_part != state.selected_part {
            self.selected_part = state.selected_part;
            self.dirty = true;
        }
        let tab = Tab::from_key(&state.tab);
        if self.tab != tab {
            self.tab = tab;
            self.dirty = true;
        }
        if self.status != state.status {
            self.status.clone_from(&state.status);
            self.dirty = true;
        }
    }

    fn commit_shared(&mut self, doc: &mut AutoCommit, action: &DisksAction) {
        self.shared.dispatch(doc, action);
    }

    fn commit_topology(&mut self, doc: &mut AutoCommit, topo: &Topology) {
        let json = serde_json::to_string(topo).unwrap_or_else(|_| "{\"drives\":[]}".into());
        self.commit_shared(doc, &DisksAction::WriteTopology { json });
    }

    pub fn new() -> Self {
        let intents = Intents::default();
        let shell = NavShell::new()
            .sidebar_header("Drives", vec![Box::new(view::rescan_button(&intents))])
            .with_id("disks-shell");
        Self {
            shell,
            intents,
            topo: Topology::default(),
            topo_json_cache: String::new(),
            topo_rev: 0,
            selected: 0,
            selected_part: None,
            tab: Tab::Overview,
            pending: None,
            confirm_input: TextInput::local(),
            status: String::new(),
            live: std::env::var("DISKS_LIVE").is_ok(),
            action_rx: None,
            action_invocation: None,
            completed_action: None,
            seeded: false,
            dirty: true,
            scroll: Rc::new(Cell::new((0.0, 0.0))),
            built: None,
            toast: None,
            modal: None,
            cursor: Point::ZERO,
            shared: crate::resource::ActionSink::default(),
        }
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
            selected: self.selected,
            partition: self.part_index(),
            tab: self.tab,
        };
        if self.built == Some(key) {
            return;
        }
        let listed_before = self
            .built
            .is_some_and(|b| b.topology == key.topology && b.selected == key.selected);
        self.built = Some(key);
        let Some(drive) = self.topo.drives.get(self.selected) else {
            self.shell.set_sidebar(None);
            self.shell.set_content_title("");
            self.shell.set_content(view::empty_state(&self.intents));
            return;
        };
        if !listed_before {
            self.shell.set_sidebar(Some(Box::new(view::drive_list(
                &self.topo,
                self.selected,
                &self.intents,
            ))));
        }
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
        self.shell.set_content_title(drive.model.clone());
        self.shell.set_content(content);
    }

    /// Apply what the window's controls asked for.
    fn drain_intents(&mut self, doc: &mut AutoCommit) {
        for intent in self.intents.take() {
            match intent {
                Intent::SelectDrive(index) => self.select(doc, index),
                Intent::SelectPartition(part) => self.select_part(doc, part),
                Intent::SetTab(tab) => self.set_tab(doc, tab),
                Intent::Run(action) => self.trigger_action(doc, action),
                Intent::Rescan => self.do_refresh(doc),
            }
        }
    }

    // ---- results ---------------------------------------------------------

    /// Record a result: the status line the document carries and the toast
    /// that shows it once.
    fn report(&mut self, doc: &mut AutoCommit, text: String, tone: StatusTone) {
        self.status.clone_from(&text);
        self.commit_shared(
            doc,
            &DisksAction::SetStatus {
                status: text.clone(),
            },
        );
        self.toast = Some(ToastState {
            text,
            tone,
            shown: ui::time::now(),
            pinned: false,
            close: Rect::ZERO,
        });
        self.dirty = true;
    }

    /// Show the document's status as a held toast. Captures stage with it.
    pub(crate) fn pin_status_toast(&mut self, tone_of: impl Fn(&str) -> StatusTone) {
        if self.status.is_empty() {
            return;
        }
        let tone = tone_of(&self.status);
        self.toast = Some(ToastState {
            text: self.status.clone(),
            tone,
            shown: ui::time::now(),
            pinned: true,
            close: Rect::ZERO,
        });
        self.dirty = true;
    }

    fn toast_visible(&self) -> bool {
        self.toast
            .as_ref()
            .is_some_and(|t| t.pinned || t.shown.elapsed() < TOAST_TTL)
    }

    // ---- confirm sheet ---------------------------------------------------

    /// The `/dev/…` name a *pending* action targets (partition if one was
    /// captured with the action, else the whole disk).
    fn pending_target_name(&self, p: Pending) -> String {
        format!("/dev/{}", self.pending_target_leaf(p))
    }

    /// The bare kernel leaf name (`nvme0n1p3`, `sdc`) a pending action targets.
    /// This is also the string the user must type to unlock a destructive op.
    fn pending_target_leaf(&self, p: Pending) -> String {
        let Some(d) = self.topo.drives.get(p.drive) else {
            return String::new();
        };
        match p.part.and_then(|i| d.partitions.get(i)) {
            Some(part) => part.name.clone(),
            None => d.name.clone(),
        }
    }

    /// True once the typed confirmation matches the target — gates Erase.
    fn confirm_ready(&self) -> bool {
        match self.pending {
            Some(p) => {
                !self.confirm_input.value().trim().is_empty()
                    && self.confirm_input.value().trim() == self.pending_target_leaf(p)
            }
            None => false,
        }
    }

    /// Paint the confirm sheet and remember where its parts landed.
    fn paint_sheet(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        window: Rect,
    ) {
        let Some(pending) = self.pending else {
            self.modal = None;
            return;
        };
        let target = self.pending_target_name(pending);
        let leaf = self.pending_target_leaf(pending);
        let title = pending.action.confirm_title(&target);
        let body = pending.action.confirm_body(&target, self.live);
        let danger = pending.action.destructive();
        let ready = self.confirm_ready();
        let spacing = &theme.tokens.spacing;
        let gap = f64::from(spacing.rhythm.sibling);

        // The body: the explanation, then (for a destructive action) the
        // name to type and its field.
        let body_style = theme.text(TextRole::Body);
        let body_width = modal_dialog::body_width(window, modal_dialog::MODAL_MAX_WIDTH, theme);
        let (_, body_h) = text.measure_style(&body, Some(body_width as f32), &body_style);
        let prompt = format!("Type {leaf} to confirm");
        let label_h = f64::from(
            text.measure_style(&prompt, None, &theme.text(TextRole::Label))
                .1,
        );
        let field_h = ControlSize::Regular.height(theme);
        let mut body_height = f64::from(body_h);
        if danger {
            body_height += gap + label_h + f64::from(spacing.rhythm.tight) + field_h;
        }

        let confirm_label = if danger { "Erase" } else { "Confirm" };
        let hovered = |layout: &ModalLayout, i: usize| {
            layout
                .actions
                .get(i)
                .is_some_and(|r| r.contains(self.cursor))
        };
        let actions = |layout: Option<&ModalLayout>| {
            let cancel = ModalAction::secondary("Cancel");
            let confirm = if danger {
                ModalAction::danger(confirm_label)
            } else {
                ModalAction::primary(confirm_label)
            }
            .enabled(!danger || ready);
            match layout {
                Some(l) => [
                    cancel.hovered(hovered(l, 0)),
                    confirm.hovered(hovered(l, 1)),
                ],
                None => [cancel, confirm],
            }
        };
        let first = actions(None);
        let spec = ModalSpec::new(&title)
            .actions(&first)
            .body_height(body_height)
            .closable(false);
        let layout = modal_dialog::layout(window, &spec, text, theme);
        let shown = actions(Some(&layout));
        let spec = ModalSpec::new(&title)
            .actions(&shown)
            .body_height(body_height)
            .closable(false);
        modal_dialog::paint_backdrop(scene, transform, text, theme, &layout, &spec);

        let (ink, dim) = modal_dialog::ink(theme);
        let mut ctx = PaintCtx::new(scene, transform, text, theme);
        let origin = layout.body.origin();
        ctx.text_role(&body, TextRole::Body, dim, origin, Some(body_width as f32));
        let mut input = Rect::ZERO;
        if danger {
            let label_y = origin.y + f64::from(body_h) + gap;
            ctx.text_role(
                &prompt,
                TextRole::Label,
                ink,
                Point::new(origin.x, label_y),
                None,
            );
            let field_y = label_y + label_h + f64::from(spacing.rhythm.tight);
            input = Rect::new(layout.body.x0, field_y, layout.body.x1, field_y + field_h);
            self.confirm_input.set_placeholder(&leaf);
            self.confirm_input.apply_layout_external(input);
            self.confirm_input
                .draw(scene, transform, text, theme, presence);
        }
        modal_dialog::paint_actions(scene, transform, text, theme, &layout, &spec);
        self.modal = Some(ModalState {
            layout,
            input,
            ready,
        });
    }

    /// Paint the result toast, if one is showing.
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

    // ---- interaction ---------------------------------------------------

    fn trigger_action(&mut self, state: &mut AutoCommit, action: ActionKind) {
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

        if action.destructive() {
            // Open the type-to-confirm sheet — nothing executes until the user
            // types the device name and confirms.
            self.pending = Some(Pending {
                action,
                drive: self.selected,
                part,
            });
            self.confirm_input.set_text("");
            self.confirm_input.set_focus(true);
            self.commit_shared(
                state,
                &DisksAction::SetPending {
                    pending: format!(
                        "{}:{}:{}",
                        action.key(),
                        self.selected,
                        part.map_or(-1, |i| i as i64)
                    ),
                },
            );
            self.dirty = true;
        } else {
            self.dispatch_action(state, action, part);
        }
    }

    /// Actually perform (or, without live access, say why not) an action. Live
    /// runs spawn a worker thread whose result is drained in `sync`.
    fn dispatch_action(&mut self, state: &mut AutoCommit, action: ActionKind, part: Option<usize>) {
        // Snapshot the target as owned strings so the drive borrow ends before
        // we mutate `self` / spawn a thread.
        let (leaf, fstype) = {
            let Some(drive) = self.drive() else { return };
            let p = part.and_then(|i| drive.partitions.get(i));
            let leaf = p.map_or_else(|| drive.name.clone(), |p| p.name.clone());
            let fstype = p.and_then(|p| p.fstype.clone());
            (leaf, fstype)
        };
        let dev = format!("/dev/{leaf}");

        if self.live {
            let (tx, rx) = mpsc::channel();
            let op = action.op();
            let refresh = action.changes_topology();
            let done_label = action.label().trim_end_matches('…').to_string();
            thread::spawn(move || {
                let (text, error) = match ops::run(op, &leaf, fstype.as_deref()) {
                    Ok(msg) => (msg, None),
                    Err(e) => {
                        let text = format!("{done_label} failed: {e}");
                        (text, Some(e))
                    }
                };
                let _ = tx.send(ActionResult {
                    text,
                    error,
                    refresh,
                });
            });
            self.action_rx = Some(rx);
            self.report(
                state,
                format!("{} {dev}…", action.gerund()),
                StatusTone::Info,
            );
        } else {
            self.report(
                state,
                format!("Live disk access is off, so nothing was changed on {dev}."),
                StatusTone::Info,
            );
        }
        self.dirty = true;
    }

    fn confirm_pending(&mut self, state: &mut AutoCommit) {
        // Locked until the typed name matches the target device.
        if !self.confirm_ready() {
            return;
        }
        let Some(p) = self.pending.take() else { return };
        self.confirm_input.set_text("");
        self.confirm_input.set_focus(false);
        self.commit_shared(
            state,
            &DisksAction::SetPending {
                pending: String::new(),
            },
        );
        self.dispatch_action(state, p.action, p.part);
    }

    fn cancel_pending(&mut self, state: &mut AutoCommit) {
        self.pending = None;
        self.confirm_input.set_text("");
        self.confirm_input.set_focus(false);
        self.commit_shared(
            state,
            &DisksAction::SetPending {
                pending: String::new(),
            },
        );
    }

    fn select(&mut self, state: &mut AutoCommit, idx: usize) {
        if idx < self.topo.drives.len() {
            let changed = idx != self.selected;
            self.selected = idx;
            self.commit_shared(state, &DisksAction::SetSelected { index: idx as u64 });
            // A new drive resets the partition focus to the whole disk so the
            // detail and its actions can never point at a stale partition.
            if changed {
                self.selected_part = None;
                self.commit_shared(state, &DisksAction::SetSelectedPart { part: -1 });
                self.scroll.set((0.0, 0.0));
            }
            self.dirty = true;
        }
    }

    /// Select a partition within the current drive, or the whole disk (`None`).
    /// Choosing the already-selected partition goes back to the whole disk.
    fn select_part(&mut self, state: &mut AutoCommit, part: Option<usize>) {
        let next = match part {
            Some(i) if self.selected_part == Some(i) => None,
            other => other,
        };
        self.selected_part = next;
        self.commit_shared(
            state,
            &DisksAction::SetSelectedPart {
                part: next.map_or(-1, |index| index as i64),
            },
        );
        self.dirty = true;
    }

    fn set_tab(&mut self, state: &mut AutoCommit, tab: Tab) {
        self.tab = tab;
        self.commit_shared(
            state,
            &DisksAction::SetTab {
                tab: tab.key().to_string(),
            },
        );
        self.scroll.set((0.0, 0.0));
        self.dirty = true;
    }

    /// Resolve an operation payload against the latest locally-probed
    /// topology. Resource invocations may arrive after a collaborator's
    /// document snapshot or a search result has gone stale, so a leaf name in
    /// a payload is never authority to touch an arbitrary `/dev/*` node.
    ///
    /// The return value deliberately derives the filesystem type from the
    /// current probe instead of trusting the serialized payload.
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

    /// Re-probe the live system. Without live access the topology stays as it
    /// is and the toast says so.
    fn do_refresh(&mut self, state: &mut AutoCommit) {
        if std::env::var("DISKS_LIVE").is_err() {
            let n = self.topo.drives.len();
            self.report(state, found_message(n), StatusTone::Success);
            return;
        }
        let topo = crate::probe::probe().unwrap_or_else(|| self.topo.clone());
        self.commit_topology(state, &topo);
        self.report(state, found_message(topo.drives.len()), StatusTone::Success);
    }

    /// Stage view state for a screenshot scenario.
    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        if scenario == Some("toast") {
            self.pin_status_toast(|_| StatusTone::Success);
        }
        if scenario == Some("toast-error") {
            self.pin_status_toast(|_| StatusTone::Danger);
        }
        if scenario.is_some_and(|name| name.ends_with("drawer")) {
            self.shell.open_sidebar();
            self.shell.snap();
        }
    }
}

fn found_message(n: usize) -> String {
    format!(
        "Rescan found {n} {}",
        if n == 1 { "drive" } else { "drives" }
    )
}

fn parse_pending(s: &str) -> Option<Pending> {
    // Format: "<action>:<drive>[:<part|-1>]". The partition field is optional
    // so older docs (drive-only) still parse.
    let mut it = s.split(':');
    let action = ActionKind::from_key(it.next()?)?;
    let drive = it.next()?.parse::<usize>().ok()?;
    let part = match it.next() {
        Some(p) => match p.parse::<i64>().ok()? {
            n if n < 0 => None,
            n => Some(n as usize),
        },
        None => None,
    };
    Some(Pending {
        action,
        drive,
        part,
    })
}

impl ui::widget::Decorator for DisksApp {
    type Inner = NavShell;

    fn inner(&self) -> &NavShell {
        &self.shell
    }

    fn inner_mut(&mut self) -> &mut NavShell {
        &mut self.shell
    }

    fn provide<'a>(&'a self, request: &mut ui::widget::capability::Request<'a, '_>) {
        if request.wants::<dyn ui::resources::PublishesResources>() {
            request.provide::<dyn ui::resources::PublishesResources>(self);
            return;
        }
        self.shell.provide(request);
    }

    fn provide_mut<'a>(&'a mut self, request: &mut ui::widget::capability::RequestMut<'a, '_>) {
        if request.wants::<dyn ui::resources::PublishesResources>() {
            request.provide::<dyn ui::resources::PublishesResources>(self);
            return;
        }
        self.shell.provide_mut(request);
    }

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &["disks.operation"]
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return ui::widget::start_in_children(&mut self.shell, op, doc, path);
        };
        if let Some(started) = ui::widget::start_in_children(&mut self.shell, op, doc, path)? {
            return Ok(Some(started));
        }
        self.admit_resource_operation(
            request.invocation,
            request.kind,
            request.type_id,
            request.payload,
            doc,
            path,
        )
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        _cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if self
            .completed_action
            .as_ref()
            .is_some_and(|update| update.invocation() == invocation)
        {
            return self.completed_action.take();
        }
        None
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one ordered reconciliation pass keeps the CRDT projection and operation lifecycle atomic"
    )]
    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        {
            let (doc, _) = ui::doc::document(state, path);
            self.reconcile(doc);
        }
        self.refresh_view();
        self.shell.sync(state, path);
    }

    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
        {
            let (doc, _) = ui::doc::document(state, path);
            self.drain_intents(doc);
        }
        self.shell.update(state, path) || self.dirty
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        if self.pending.is_some() {
            self.handle_sheet_event(event, ctx);
            return;
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
        self.drain_intents(ctx.state);
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
                self.set_tab(ctx.state, tab);
                *ctx.handled = true;
            } else if matches!(key.as_str(), "r" | "R") {
                self.do_refresh(ctx.state);
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
        self.paint_sheet(scene, transform, text, theme, presence, window);
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep frames (and thus `sync` polling) coming while a background
        // action is in flight, and while a toast counts down.
        self.action_rx.is_some()
            || self.toast.as_ref().is_some_and(|t| !t.pinned)
            || self.shell.needs_animation_frame()
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.toast.is_some() || self.pending.is_some() || self.shell.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.shell.mark_drawn();
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.modal.as_ref().map(|modal| modal.layout.card)
    }

    fn traps_focus(&self) -> bool {
        self.pending.is_some()
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the semantic tree mirrors the sheet and the status beside the shell's own nodes"
    )]
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Politeness, Role};
        let mut root = AccessNode::new(Role::Window)
            .with_name("Disks")
            .with_bounds(self.shell.get_rect());
        // The confirm sheet is painted in the top layer, so it has no widget
        // of its own: mirror it so keyboard users, screen readers and agents
        // reach the same controls as the pointer.
        if let (Some(pending), Some(modal)) = (self.pending, &self.modal) {
            let expected = self.pending_target_leaf(pending);
            let target = format!("/dev/{expected}");
            let mut dialog = AccessNode::new(Role::Dialog)
                .with_name(pending.action.confirm_title(&target))
                .with_description(pending.action.confirm_body(&target, self.live))
                .with_bounds(modal.layout.card)
                .with_hashed_key("disks.confirmation-dialog")
                .with_state(AccessState::new().modal(true));
            if pending.action.destructive() {
                dialog.children.push(
                    AccessNode::new(Role::TextInput)
                        .with_name(format!("Type {expected} to confirm"))
                        .with_description("The erase button unlocks only after an exact match.")
                        .with_value(self.confirm_input.value())
                        .with_bounds(modal.input)
                        .with_action_target("disks.modal.confirm-input")
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .focused(self.confirm_input.is_focused()),
                        ),
                );
            }
            let (cancel, confirm) = (
                modal.layout.actions.first().copied().unwrap_or(Rect::ZERO),
                modal.layout.actions.get(1).copied().unwrap_or(Rect::ZERO),
            );
            dialog.children.push(
                AccessNode::new(Role::Button)
                    .with_name("Cancel")
                    .with_bounds(cancel)
                    .with_action_target("disks.modal.cancel")
                    .with_state(AccessState::new().focusable(true)),
            );
            dialog.children.push(
                AccessNode::new(Role::Button)
                    .with_name(if pending.action.destructive() {
                        "Erase"
                    } else {
                        "Confirm"
                    })
                    .with_description(if modal.ready {
                        "Confirmation text matches the target device."
                    } else {
                        "Type the exact device name to enable this action."
                    })
                    .with_bounds(confirm)
                    .with_action_target("disks.modal.confirm")
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .disabled(pending.action.destructive() && !modal.ready),
                    ),
            );
            root.children.push(dialog);
        }
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

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        let (state, _) = ui::doc::document(state, path);
        if let ui::a11y::Action::SetValue(value) = action
            && target == "disks.modal.confirm-input"
            && self.pending.is_some()
        {
            self.confirm_input.set_text(value);
            self.confirm_input.set_focus(true);
            self.dirty = true;
            return true;
        }
        if !matches!(action, ui::a11y::Action::Activate) {
            return matches!(action, ui::a11y::Action::Focus);
        }
        match target {
            "disks.modal.cancel" if self.pending.is_some() => self.cancel_pending(state),
            "disks.modal.confirm" if self.pending.is_some() => self.confirm_pending(state),
            _ => return false,
        }
        self.dirty = true;
        true
    }
}

impl DisksApp {
    /// Fold the shared document into the widget's own state, drain a finished
    /// background action, and apply what the window's controls asked for.
    #[allow(
        clippy::too_many_lines,
        reason = "one ordered reconciliation pass keeps the CRDT projection and operation lifecycle atomic"
    )]
    fn reconcile(&mut self, state: &mut AutoCommit) {
        if !self.seeded {
            if DisksDoc::is_empty(state) {
                // Live: reflect the real system only. If the probe finds
                // nothing (or the tools are missing) we seed an *empty*
                // topology and show an honest empty state — never the curated
                // sample, which would fabricate devices that don't exist.
                // The sample is reserved for `--shots`/demo (non-live) builds.
                let topo = if std::env::var("DISKS_LIVE").is_ok() {
                    crate::probe::probe().unwrap_or_default()
                } else {
                    Topology::sample()
                };
                self.commit_topology(state, &topo);
                self.commit_shared(
                    state,
                    &DisksAction::SetTab {
                        tab: "overview".into(),
                    },
                );
                self.commit_shared(state, &DisksAction::SetSelected { index: 0 });
                self.commit_shared(state, &DisksAction::SetSelectedPart { part: -1 });
                self.commit_shared(
                    state,
                    &DisksAction::SetPending {
                        pending: String::new(),
                    },
                );
                self.commit_shared(
                    state,
                    &DisksAction::SetStatus {
                        status: String::new(),
                    },
                );
            }
            self.seeded = true;
        }

        if let Some(json) = DisksDoc::topology_json(state)
            && json != self.topo_json_cache
            && let Ok(t) = serde_json::from_str::<Topology>(&json)
        {
            self.topo = t;
            self.topo_json_cache = json;
            self.topo_rev += 1;
            self.dirty = true;
        }

        let sel = DisksDoc::selected(state).min(self.topo.drives.len().saturating_sub(1));
        if sel != self.selected {
            self.selected = sel;
            self.dirty = true;
        }
        // Partition selection, clamped against the current drive's partitions.
        let part = DisksDoc::selected_part(state)
            .filter(|&i| self.drive().is_some_and(|d| i < d.partitions.len()));
        if part != self.selected_part {
            self.selected_part = part;
            self.dirty = true;
        }
        let tab = Tab::from_key(&DisksDoc::tab(state));
        if tab != self.tab {
            self.tab = tab;
            self.dirty = true;
        }
        let pending = DisksDoc::pending(state);
        let parsed = parse_pending(&pending);
        if parsed.is_some() != self.pending.is_some() {
            if parsed.is_some() {
                self.confirm_input.set_text("");
                self.confirm_input.set_focus(true);
            } else {
                self.confirm_input.set_focus(false);
                self.modal = None;
            }
            self.pending = parsed;
            self.dirty = true;
        }
        let status = DisksDoc::status(state);
        if status != self.status {
            self.status = status;
            self.dirty = true;
        }

        // Drain a completed background action (the root widget's `update` is
        // not pumped by the runtime, so we poll here — `sync` runs every frame
        // while `needs_animation_frame` holds).
        let mut result = None;
        let mut clear = false;
        if let Some(rx) = &self.action_rx {
            match rx.try_recv() {
                Ok(res) => {
                    result = Some(res);
                    clear = true;
                }
                Err(mpsc::TryRecvError::Disconnected) => clear = true,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        let had_result = result.is_some();
        if let Some(res) = result {
            if let Some(invocation) = self.action_invocation.take() {
                self.completed_action = Some(match &res.error {
                    Some(error) => tilo_actions::InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
                            code: "disks.operation_failed".into(),
                            message: error.clone(),
                            details: None,
                            retryable: false,
                        },
                    },
                    None => tilo_actions::InvocationUpdate::Succeeded {
                        invocation,
                        output: tilo_actions::ActionOutput::default(),
                    },
                });
            }
            let tone = if res.error.is_some() {
                StatusTone::Danger
            } else {
                StatusTone::Success
            };
            self.report(state, res.text.clone(), tone);
            // A mount/format/unmount/eject changed the on-disk topology.
            if res.refresh
                && self.live
                && let Some(topo) = crate::probe::probe()
            {
                self.commit_topology(state, &topo);
            }
            self.dirty = true;
        }
        if clear {
            self.action_rx = None;
            if !had_result && let Some(invocation) = self.action_invocation.take() {
                self.completed_action = Some(tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "disks.worker_disconnected".into(),
                        message: "disk operation worker disconnected without a result".into(),
                        details: None,
                        retryable: false,
                    },
                });
            }
        }
        self.drain_intents(state);
    }

    /// The confirm sheet is fully blocking: it takes every event, routing the
    /// editing ones to the shared field.
    fn handle_sheet_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // The confirmation text is deliberately local-only: it gates a
        // machine-local destructive operation and must never enter the CRDT.
        // Forward the complete event stream so selection, clipboard, IME and
        // drag behaviour come from the shared editor rather than a key shim.
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
            && matches!(logical_key, Key::Named(NamedKey::Escape | NamedKey::Enter))
        {
            match logical_key {
                Key::Named(NamedKey::Escape) => self.cancel_pending(ctx.state),
                _ => self.confirm_pending(ctx.state),
            }
            *ctx.handled = true;
            self.dirty = true;
            return;
        }
        self.confirm_input.handle_event(event, ctx);
        if self.confirm_input.take_dirty().is_some() {
            self.dirty = true;
        }
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        ) {
            let hit = self
                .modal
                .as_ref()
                .and_then(|modal| modal.layout.hit(self.cursor));
            match hit {
                Some(ModalHit::Action(1)) => self.confirm_pending(ctx.state),
                Some(ModalHit::Action(_) | ModalHit::Close | ModalHit::Scrim) => {
                    self.cancel_pending(ctx.state);
                }
                Some(ModalHit::Card) | None => {}
            }
        }
        // The open sheet is fully blocking, including IME events and
        // clicks/releases that the editor itself did not consume.
        *ctx.handled = true;
        self.dirty = true;
    }

    fn admit_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if kind != OperationKind::Effect || type_id != "disks.operation" {
            return Ok(None);
        }
        if self.action_rx.is_some() {
            return Err("another disk operation is still running".into());
        }
        let args: Vec<String> = serde_json::from_slice(payload).map_err(|e| e.to_string())?;
        let action = ActionKind::from_key(args.first().map_or("", String::as_str))
            .ok_or_else(|| "unknown disk operation".to_string())?;
        let leaf = args
            .get(1)
            .ok_or_else(|| "missing device".to_string())?
            .clone();
        if !self.live {
            return Err("live disk access is disabled".into());
        }
        // Re-check against the locally probed inventory immediately before
        // execution. This rejects stale remote/search invocations and ignores
        // a caller-supplied filesystem type in favour of current system data.
        let fstype = self.revalidate_operation_target(action, &leaf)?;
        let (tx, rx) = mpsc::channel();
        let refresh = action.changes_topology();
        thread::spawn(move || {
            let (text, error) = match ops::run(action.op(), &leaf, fstype.as_deref()) {
                Ok(text) => (text, None),
                Err(error) => (format!("{} failed: {error}", action.label()), Some(error)),
            };
            let _ = tx.send(ActionResult {
                text,
                error,
                refresh,
            });
        });
        self.action_rx = Some(rx);
        self.action_invocation = Some(invocation);
        self.dirty = true;
        Ok(Some(ui::widget::ResourceOperationStart::Accepted))
    }
}

impl ui::resources::PublishesResources for DisksApp {
    /// Standalone / `--shots` / unit tests publish the full tree. When the
    /// framework hosts the surface, [`tilo_resource::publish`] already owns
    /// the topology door — keep only live extras (none today).
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
        if self.is_hosted() && id == "disks" {
            return None;
        }
        find(&self.publish_resources(doc), id)
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;
    use ui::headless::Framed;
    use ui::widget::SidebarPlacement;

    fn seeded_app() -> (DisksApp, AutoCommit) {
        let mut app = DisksApp::new();
        let mut doc = AutoCommit::new();
        Widget::sync(&mut app, &mut doc, &ui::automerge::ROOT);
        (app, doc)
    }

    fn framed((app, doc): (DisksApp, AutoCommit), w: f64, h: f64) -> Framed<DisksApp> {
        let mut state = ui::State::new();
        assert!(state.adopt_doc(doc));
        Framed::new(state, app, ui::headless::viewport((w, h)), Theme::dark())
    }

    fn at_width(width: f64) -> Framed<DisksApp> {
        framed(seeded_app(), width, 800.0)
    }

    fn doc_of(app: &Framed<DisksApp>) -> &AutoCommit {
        &Framed::harness(app).state().doc
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

    /// Sync seeds the sample topology and drive selection round-trips through the CRDT.
    #[test]
    fn sync_and_selection_persist_through_the_document() {
        // --- phase: sync_seeds_sample_topology ---
        {
            let (app, _doc) = seeded_app();
            assert_eq!(app.topo.drives.len(), 4);
            assert_eq!(app.selected, 0);
            assert_eq!(app.tab, Tab::Overview);
        }

        // --- phase: select_persists_to_doc_and_back ---
        {
            let (mut app, mut doc) = seeded_app();
            app.select(&mut doc, 2);
            assert_eq!(DisksDoc::selected(&doc), 2);
            // A fresh app syncing the same doc adopts the selection.
            let mut app2 = DisksApp::new();
            Widget::sync(&mut app2, &mut doc, &ui::automerge::ROOT);
            assert_eq!(app2.selected, 2);
        }
    }

    /// A disk operation from request (sheet or direct) through confirmation and the worker result to the reported status.
    #[test]
    fn disk_operation_lifecycle_from_request_to_status() {
        // --- phase: destructive_action_opens_the_confirm_sheet ---
        {
            let (mut app, mut doc) = seeded_app();
            app.trigger_action(&mut doc, ActionKind::Format);
            assert!(app.pending.is_some());
            assert!(DisksDoc::pending(&doc).starts_with("format:"));
        }

        // --- phase: non_destructive_action_reports_with_a_toast_and_no_sheet ---
        {
            let (mut app, mut doc) = seeded_app();
            // Benchmark always applies, so it reports on the toast with no sheet.
            app.trigger_action(&mut doc, ActionKind::Benchmark);
            assert!(app.pending.is_none());
            assert!(!DisksDoc::status(&doc).is_empty());
            let toast = app.toast.as_ref().expect("the result shows as a toast");
            assert_eq!(toast.tone, StatusTone::Info);
        }

        // --- phase: confirm_requires_typed_name_then_clears_pending ---
        {
            let (mut app, mut doc) = seeded_app();
            app.trigger_action(&mut doc, ActionKind::Format); // whole nvme disk
            // The Erase gate is locked until the exact device name is typed.
            assert!(!app.confirm_ready(), "empty input does not unlock");
            app.confirm_pending(&mut doc);
            assert!(app.pending.is_some(), "confirm is a no-op while locked");

            app.confirm_input.set_text("wrong");
            assert!(!app.confirm_ready());
            app.confirm_pending(&mut doc);
            assert!(app.pending.is_some(), "a mismatched name stays locked");

            app.confirm_input.set_text("nvme0n1"); // the whole-disk device name
            assert!(app.confirm_ready(), "the exact name unlocks Erase");
            app.confirm_pending(&mut doc);
            assert!(app.pending.is_none(), "confirming clears the sheet");
            assert_eq!(DisksDoc::pending(&doc), "");
            // Without live access nothing runs, and the result says so.
            assert!(DisksDoc::status(&doc).contains("nothing was changed"));
        }

        // --- phase: worker_results_become_terminal_updates_and_toasts ---
        {
            let (mut app, mut doc) = seeded_app();
            let invocation = tilo_actions::InvocationId::new();
            let (tx, rx) = mpsc::channel();
            app.action_invocation = Some(invocation);
            app.action_rx = Some(rx);
            tx.send(ActionResult {
                text: "Mounted /dev/sda1".into(),
                error: None,
                refresh: false,
            })
            .unwrap();
            Widget::sync(&mut app, &mut doc, &ui::automerge::ROOT);
            assert!(matches!(
                app.poll_resource_operation(invocation, false, &doc, &ui::automerge::ROOT),
                Some(tilo_actions::InvocationUpdate::Succeeded { .. })
            ));
            assert_eq!(
                app.toast.as_ref().map(|t| t.tone),
                Some(StatusTone::Success)
            );

            let (tx, rx) = mpsc::channel();
            app.action_rx = Some(rx);
            tx.send(ActionResult {
                text: "Mount failed: busy".into(),
                error: Some("busy".into()),
                refresh: false,
            })
            .unwrap();
            Widget::sync(&mut app, &mut doc, &ui::automerge::ROOT);
            assert_eq!(
                app.toast.as_ref().map(|t| t.tone),
                Some(StatusTone::Danger),
                "a failure's tone comes from the result, not from its wording"
            );
        }

        // --- phase: root_accessibility_reports_the_status ---
        {
            let (mut app, _doc) = seeded_app();
            app.status = "Rescan found 4 drives".into();
            let node = ui::widget::Decorator::accessibility(&app).expect("disks exports a window");
            assert_eq!(node.role, ui::a11y::Role::Window);
            assert!(
                node.children
                    .iter()
                    .any(|child| child.name.as_deref() == Some("Rescan found 4 drives"))
            );
        }
    }

    /// Exported disk resources publish their operations as background jobs whose targets are revalidated against the live topology.
    #[test]
    fn exported_disk_resources_declare_revalidated_background_actions() {
        // --- phase: exported_disk_actions_declare_background_jobs ---
        {
            let (app, doc) = seeded_app();
            for action in ui::resources::resources(&app, &doc)
                .iter()
                .flat_map(|root| root.children.iter())
                .flat_map(|drive| {
                    drive
                        .actions
                        .iter()
                        .chain(drive.children.iter().flat_map(|p| p.actions.iter()))
                })
                .filter(|action| action.kind == OperationKind::Effect)
            {
                assert!(matches!(
                    action.contract.execution,
                    ExecutionMode::Background { cancellable: false }
                ));
                assert_eq!(action.contract.result, ResultSpec::Job);
            }
        }

        // --- phase: disk_resources_publish_whole_drive_format_and_benchmark ---
        {
            let (app, doc) = seeded_app();
            let resources = ui::resources::resources(&app, &doc);
            let disk = resources
                .first()
                .and_then(|root| root.children.first())
                .expect("sample exposes its first disk resource");
            for id in ["format", "benchmark"] {
                assert!(
                    disk.actions.iter().any(|action| action.id == id),
                    "whole-drive resource must publish {id}"
                );
            }
            let format = disk
                .actions
                .iter()
                .find(|action| action.id == "format")
                .expect("format action");
            assert!(matches!(
                format.contract.confirmation,
                ConfirmationPolicy::Phrase { .. }
            ));
        }

        // --- phase: resource_operation_targets_are_revalidated_against_current_topology ---
        {
            let (app, _doc) = seeded_app();
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

    /// Choosing a drive, a tab or an action from the window's controls repoints the detail and scopes what applies.
    #[test]
    fn pointer_controls_repoint_the_detail_and_its_actions() {
        // --- phase: choosing_a_drive_selects_it_and_resets_the_partition ---
        {
            let mut app = at_width(1280.0);
            let point = drive_row(&app, 2);
            click_at(&mut app, point);
            assert_eq!(app.selected, 2, "the chosen row becomes the selection");
            assert_eq!(app.selected_part, None, "a fresh drive is the whole disk");
            assert_eq!(DisksDoc::selected(doc_of(&app)), 2, "persisted to the CRDT");
        }

        // --- phase: choosing_a_tab_persists_without_retargeting_the_drive ---
        {
            let mut app = at_width(1280.0);
            let strip = target(&app, "disks-tabs");
            // The middle of three segments is the Health tab.
            click_at(
                &mut app,
                Point::new(strip.x0 + strip.width() * 0.5, strip.center().y),
            );
            assert_eq!(app.tab, Tab::Smart);
            assert_eq!(DisksDoc::tab(doc_of(&app)), Tab::Smart.key());
            assert_eq!(app.selected, 0, "changing tabs must not retarget the drive");
        }

        // --- phase: a_disabled_action_does_nothing ---
        {
            let mut app = at_width(1280.0);
            // The whole multi-partition disk has no filesystem to mount.
            Framed::harness_mut(&mut app)
                .click_named("disks-action-mount")
                .unwrap();
            assert!(DisksDoc::status(doc_of(&app)).is_empty());
            assert!(app.pending.is_none());
        }

        // --- phase: format_opens_the_sheet_and_the_typed_name_unlocks_it ---
        {
            let mut app = at_width(1280.0);
            Framed::harness_mut(&mut app)
                .click_named("disks-action-format")
                .unwrap();
            assert!(app.pending.is_some(), "Format asks for confirmation");
            assert_eq!(DisksDoc::pending(doc_of(&app)), "format:0:-1");

            // The sheet is fully blocking, and Escape dismisses it.
            Framed::harness_mut(&mut app).key("Escape");
            assert!(app.pending.is_none());
            assert_eq!(DisksDoc::pending(doc_of(&app)), "");

            Framed::harness_mut(&mut app)
                .click_named("disks-action-format")
                .unwrap();
            Framed::harness_mut(&mut app).key("Enter");
            assert!(
                app.pending.is_some(),
                "Enter stays locked until the name matches"
            );
            Framed::harness_mut(&mut app).type_text("nvme0n1");
            assert!(app.confirm_ready());
            Framed::harness_mut(&mut app).key("Enter");
            assert!(app.pending.is_none(), "an exact match confirms");
            assert!(DisksDoc::status(doc_of(&app)).contains("nothing was changed"));
        }
    }

    /// Partition selection scopes Mount and Unmount to the filesystem it names.
    #[test]
    fn partition_selection_scopes_the_actions() {
        // --- phase: choosing_a_partition_enables_what_applies ---
        {
            let (mut app, mut doc) = seeded_app();
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
            app.drain_intents(&mut doc);
            assert_eq!(app.selected_part, Some(1));
            assert_eq!(
                DisksDoc::selected_part(&doc),
                Some(1),
                "persisted to the CRDT"
            );
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
            let (mut app, mut doc) = seeded_app();
            app.select_part(&mut doc, Some(1));
            app.select_part(&mut doc, Some(1));
            assert_eq!(app.selected_part, None);
            app.select_part(&mut doc, Some(2));
            app.intents.push(Intent::SelectPartition(None));
            app.drain_intents(&mut doc);
            assert_eq!(app.selected_part, None, "Whole disk clears the selection");
        }

        // --- phase: single_filesystem_disk_targets_its_sole_partition ---
        {
            let (mut app, mut doc) = seeded_app();
            app.select(&mut doc, 3); // USB stick: one mounted partition, removable
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
            app.select(&mut doc, 1); // SATA SSD
            assert!(!app.action_enabled(app.drive().unwrap(), ActionKind::Eject));
        }
    }

    /// The confirm sheet names the target, is honest about live access and gates on the typed name.
    #[test]
    fn the_confirm_sheet_is_honest_about_live_access() {
        let demo = ActionKind::Format.confirm_body("/dev/sdz1", false);
        assert!(
            demo.contains("nothing will be written"),
            "without live access the sheet says nothing is written: {demo}"
        );
        let live = ActionKind::Format.confirm_body("/dev/sdz1", true);
        assert!(
            !live.contains("nothing will be written"),
            "the live sheet must not claim the format is inert: {live}"
        );
        assert!(
            live.contains("erases all data") && live.contains("cannot be undone"),
            "the live sheet still warns the format is destructive: {live}"
        );
    }

    /// A real probed topology drives the app end to end; an empty probe shows an honest empty state rather than a sample.
    #[test]
    fn probed_topology_drives_the_app_or_an_honest_empty_state() {
        // --- phase: drives_real_or_sample_topology_end_to_end ---
        {
            let (mut app, mut doc) = seeded_app();
            if let Some(topo) = crate::probe::probe() {
                DisksDoc::write_topology(&mut doc, &topo);
                Widget::sync(&mut app, &mut doc, &ui::automerge::ROOT);
            }
            assert!(
                !app.topo.drives.is_empty(),
                "there is always a device to drive"
            );
            for i in 0..app.topo.drives.len() {
                app.select(&mut doc, i);
                let d = app.drive().unwrap().clone();
                assert!(!d.name.is_empty(), "no nameless devices");
                assert!(d.size > 0, "no zero-size phantom devices");
                for tab in [Tab::Overview, Tab::Smart, Tab::Partitions] {
                    app.set_tab(&mut doc, tab);
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
            // The live path finding nothing must NOT fall back to fabricated
            // devices: the list goes away and the empty state offers Rescan.
            let (mut app, mut doc) = seeded_app();
            DisksDoc::write_topology(&mut doc, &Topology::default());
            Widget::sync(&mut app, &mut doc, &ui::automerge::ROOT);
            assert!(
                app.topo.drives.is_empty(),
                "empty topology stays empty: no sample"
            );
            assert!(app.drive().is_none());
            assert!(
                !app.shell.has_sidebar(),
                "no drive list when there are no drives"
            );
        }
    }
}
