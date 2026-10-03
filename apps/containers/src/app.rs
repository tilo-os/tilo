//! Root widget for the container manager.
//!
//! [`ContainersApp`] owns the engine snapshot (containers, images, volumes
//! and distroboxes mirrored from rootless podman) and presents it in a
//! [`NavShell`]: the sections are the sidebar, the container list is the
//! overview and the chosen container is the content (see [`crate::view`]).
//! Dialogs (confirming a removal, asking for a name, the new-container form,
//! pull progress) are [`ModalDialog`]s mounted beside the shell.
//!
//! Navigation, selection and the new-container draft live in the shared
//! document (see [`crate::doc`]) so undo, History and collaboration use the
//! same state; the engine projection is host data and stays in memory.

#[cfg(test)]
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender};

use automerge::{AutoCommit, ObjId};
use taffy::prelude::*;
use ui::a11y::{AccessNode, Politeness, Role};
use ui::control_custody::RawControlSlot;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tilo_actions::{ActionAddress, ControlInvocation};
use ui::time::Instant;
use ui::tokens::TextRole;
use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind, paint_transient};
use ui::widget::{
    AppModel, Column, ModalDialog, ModelPath, ModelView, NavPane, PromptBody, Text, TextInput,
    Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::doc::{CreateField, DetailTab, Section, ViewDoc};
use crate::model::Engine;
use crate::podman::{self, Action, StreamMsg};
use crate::view::{self, Intent, Intents};

/// How long a result toast stays on screen.
const TOAST_MS: u128 = 4000;

#[derive(Clone)]
pub(crate) struct ContinuationRuntimeIdentity {
    pub(crate) runtime_epoch: u64,
    pub(crate) target_epoch: u64,
    pub(crate) target_id: String,
}

#[cfg(test)]
fn next_continuation_epoch() -> Option<u64> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_update(
        std::sync::atomic::Ordering::Relaxed,
        std::sync::atomic::Ordering::Relaxed,
        |value| value.checked_add(1),
    )
    .ok()
}

/// What a destructive confirm targets — a container, an image, or a volume.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ConfirmTarget {
    Container { id: String, action: Action },
    Image { reference: String },
    Volume { name: String },
}

/// A pending destructive step, asked about before it runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Confirm {
    /// What is removed, as the person knows it.
    pub(crate) name: String,
    pub(crate) target: ConfirmTarget,
}

impl Confirm {
    fn title(&self) -> String {
        format!("Remove {}?", self.name)
    }

    fn body(&self) -> &'static str {
        match self.target {
            ConfirmTarget::Container { .. } => {
                "The container and the changes made inside it are deleted. Its volumes stay."
            }
            ConfirmTarget::Image { .. } => {
                "The image is deleted from this computer. You can pull it again later."
            }
            ConfirmTarget::Volume { .. } => "The volume and the data stored in it are deleted.",
        }
    }

    fn verb(&self) -> &'static str {
        match self.target {
            ConfirmTarget::Container { .. } => "Remove container",
            ConfirmTarget::Image { .. } => "Remove image",
            ConfirmTarget::Volume { .. } => "Remove volume",
        }
    }
}

/// Kind of text the prompt dialog collects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PromptKind {
    /// Create a new named volume.
    NewVolume,
    /// Tag an existing image (the source is in `Prompt::context`).
    TagImage,
    /// Pull an image by reference.
    PullImage,
}

/// A single-line text prompt (new volume name, image tag, image to pull).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Prompt {
    pub(crate) kind: PromptKind,
    pub(crate) value: String,
    /// The image being tagged.
    pub(crate) context: String,
}

impl Prompt {
    fn title(&self) -> String {
        match self.kind {
            PromptKind::NewVolume => "New volume".into(),
            PromptKind::TagImage => format!("Add a tag to {}", self.context),
            PromptKind::PullImage => "Pull an image".into(),
        }
    }

    fn label(&self) -> &'static str {
        match self.kind {
            PromptKind::NewVolume => "Name",
            PromptKind::TagImage => "New tag",
            PromptKind::PullImage => "Image",
        }
    }

    fn placeholder(&self) -> &'static str {
        match self.kind {
            PromptKind::NewVolume => "backups",
            PromptKind::TagImage => "registry.example.test/api:edge",
            PromptKind::PullImage => "docker.io/library/alpine:3.20",
        }
    }

    fn primary(&self) -> &'static str {
        match self.kind {
            PromptKind::NewVolume => "Create volume",
            PromptKind::TagImage => "Add tag",
            PromptKind::PullImage => "Pull image",
        }
    }
}

/// Live progress of a streaming `podman pull`.
pub(crate) struct PullProgress {
    pub(crate) reference: String,
    /// Most recent progress lines (tail-bounded).
    pub(crate) lines: Vec<String>,
    pub(crate) done: bool,
    pub(crate) failed: Option<String>,
}

/// A transient status toast.
pub(crate) struct Toast {
    pub(crate) message: String,
    pub(crate) error: bool,
    pub(crate) born: Instant,
}

/// Background-thread message: a fresh engine probe arrived, or a streaming
/// op (pull) emitted a line / finished.
pub(crate) enum Bg {
    Snapshot(Engine),
    Pull(StreamMsg),
}

#[cfg(test)]
pub enum ControlledRuntimeRequest {
    Refresh(ControlledRefreshRequest),
    SelectedDetails {
        target: String,
    },
    Immediate {
        operation: String,
        target: String,
        arguments: Vec<String>,
    },
    Pull(ControlledPullRequest),
    Create(ControlledCreateRequest),
}

#[cfg(test)]
pub struct ControlledRefreshRequest {
    events: Sender<Bg>,
}

#[cfg(test)]
impl ControlledRefreshRequest {
    /// # Errors
    ///
    /// Returns an error when the retained application event receiver has closed.
    pub fn snapshot(&self, engine: Engine) -> Result<(), String> {
        self.events
            .send(Bg::Snapshot(engine))
            .map_err(|_| "controlled refresh receiver disconnected".into())
    }
}

#[cfg(test)]
pub struct ControlledPullRequest {
    pub reference: String,
    pub arguments: Vec<String>,
    events: Sender<Bg>,
}

#[cfg(test)]
impl ControlledPullRequest {
    /// # Errors
    ///
    /// Returns an error when the retained application event receiver has closed.
    pub fn line(&self, line: impl Into<String>) -> Result<(), String> {
        self.events
            .send(Bg::Pull(StreamMsg::Line(line.into())))
            .map_err(|_| "controlled pull receiver disconnected".into())
    }

    /// # Errors
    ///
    /// Returns an error when the retained application event receiver has closed.
    pub fn done(&self) -> Result<(), String> {
        self.events
            .send(Bg::Pull(StreamMsg::Done))
            .map_err(|_| "controlled pull receiver disconnected".into())
    }

    /// # Errors
    ///
    /// Returns an error when the retained application event receiver has closed.
    pub fn fail(&self, error: impl Into<String>) -> Result<(), String> {
        self.events
            .send(Bg::Pull(StreamMsg::Failed(error.into())))
            .map_err(|_| "controlled pull receiver disconnected".into())
    }
}

#[cfg(test)]
pub struct ControlledCreateRequest {
    pub arguments: Vec<String>,
    events: Sender<Bg>,
}

#[cfg(test)]
impl ControlledCreateRequest {
    /// Deliver the existing post-run refresh event. The current create worker
    /// exposes no classified create success/failure result.
    ///
    /// # Errors
    ///
    /// Returns an error when the retained application event receiver has closed.
    pub fn post_run_snapshot(&self, engine: Engine) -> Result<(), String> {
        self.events
            .send(Bg::Snapshot(engine))
            .map_err(|_| "controlled create receiver disconnected".into())
    }
}

/// The armed retention fixture: the app, its runtime request and result
/// channels, the retention observer, the restart-arm flag, and the runtime
/// replacement request channel and trigger.
#[cfg(test)]
pub type ArmedRetentionRoot = (
    ContainersApp,
    Receiver<ControlledRuntimeRequest>,
    Sender<Result<(), String>>,
    Rc<RefCell<Option<HistoryRetentionSnapshot>>>,
    Rc<Cell<bool>>,
    Receiver<ControlledRuntimeRequest>,
    Rc<Cell<bool>>,
);

/// What the retention fixture observes of the live surface on every sync.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryRetentionSnapshot {
    pub input_instances: Vec<usize>,
    pub input_values: Vec<String>,
    pub input_cursors: Vec<usize>,
    pub input_selections: Vec<Option<(usize, usize)>>,
    pub list_offset: (f32, f32),
    pub detail_offset: (f32, f32),
    pub engine_connection: String,
    pub engine_counts: (usize, usize, usize, usize),
    pub last_refresh: Instant,
    pub pending_control: Option<(String, String)>,
    pub background_channels: (bool, bool),
}

/// The view state the document holds, cached for the paths that have no
/// document access.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ViewState {
    pub section: Section,
    pub selected: Option<String>,
    pub tab: DetailTab,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            section: Section::Containers,
            selected: None,
            tab: DetailTab::Logs,
        }
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent flags: live engine vs. sample data, one-shot demo \
              seeding, an open form and a dirty structure"
)]
pub struct ContainersApp {
    /// The live (or sample) engine projection.
    pub(crate) engine: Engine,
    /// Bumped whenever `engine` changes.
    engine_rev: u64,
    /// Retained, revision-gated search projection.
    pub(crate) search_index: crate::search_provider::ContainerSearchIndex,
    rect: Rect,
    /// The window, rebuilt part by part from the published model.
    window: ModelView<(Section, bool, bool)>,
    /// What the window's parts are built from: the model, the narrow
    /// window's front screen, the intent queue and the scroll positions.
    parts: view::Parts,
    /// The engine revision and view last published to the model.
    published: Option<(u64, ViewState)>,
    intents: Intents,
    /// The new-container form. It is retained while closed so its fields
    /// keep their caret and binding.
    create: ModalDialog<Column>,
    pub(crate) create_open: bool,
    pub(crate) confirm: Option<Confirm>,
    confirm_dialog: Option<ModalDialog<Text>>,
    pub(crate) prompt: Option<Prompt>,
    prompt_dialog: Option<ModalDialog<PromptBody>>,
    /// Live pull progress, if a pull is running or just finished.
    pub(crate) pull: Option<PullProgress>,
    pull_dialog: Option<ModalDialog<Column>>,
    pull_dirty: bool,
    /// Selection requested through the resource/action door. Applied during
    /// `sync`, where the framework provides document access.
    pub(crate) pending_resource_selection: Option<String>,
    pub(crate) pending_resource_section: Option<Section>,
    pub(crate) pending_resource_tab: Option<DetailTab>,
    /// In-app lifecycle controls emit the same inert resource intent as
    /// search and agents. The host re-resolves authority, records the
    /// Effect, then calls `Widget::start_operation`.
    pub(crate) pending_control_invocation: RawControlSlot,
    pub(crate) toast: Option<Toast>,
    /// Background refresh plumbing. `None` in headless/shots mode.
    bg_rx: Option<Receiver<Bg>>,
    bg_tx: Option<Sender<Bg>>,
    #[cfg(test)]
    controlled_runtime: Option<Sender<ControlledRuntimeRequest>>,
    #[cfg(test)]
    controlled_immediate_results: Option<Receiver<Result<(), String>>>,
    pub(crate) continuation_runtime_identity: Option<ContinuationRuntimeIdentity>,
    #[cfg(test)]
    history_retention_observer: Option<Rc<RefCell<Option<HistoryRetentionSnapshot>>>>,
    #[cfg(test)]
    history_arm_restart: Option<Rc<Cell<bool>>>,
    #[cfg(test)]
    history_runtime_replacement: Option<(
        Rc<Cell<bool>>,
        Sender<ControlledRuntimeRequest>,
        ContinuationRuntimeIdentity,
    )>,
    pub(crate) last_refresh: Instant,
    pub(crate) live: bool,
    /// The scene a demo or screenshot session opens on, staged on the
    /// first sync.
    scenario: Option<String>,
    seed_demo: bool,
    seeded: bool,
    /// The structure changed (a dialog opened or closed, the shell was
    /// rebuilt): the host lays the tree out again.
    structure_dirty: bool,
    /// Cached snapshot of the document's view state.
    pub(crate) view: ViewState,
    motion_reduced: bool,
    /// Document actions: local host for standalone / `--shots` / unit tests;
    /// framework dispatcher when [`ResourceSurface::attach`] runs.
    pub(crate) actions: crate::resource::ActionSink,
    /// The History button sits in the window's header row. The History
    /// fixtures leave it to the window's own History bar, which the shared
    /// adapter conformance suite drives.
    history_in_header: bool,
}

fn controlled_input() -> TextInput {
    TextInput::local_with_language("en_US")
        .with_spellcheck(false)
        .with_autocorrect(false)
}

impl ContainersApp {
    /// Construct the manager. `live` enables the background podman poller and
    /// real lifecycle actions; `false` (used by `--shots`) renders the
    /// bundled sample snapshot deterministically with no process spawning.
    pub fn new(live: bool) -> Self {
        let engine = if live {
            podman::probe().engine
        } else {
            crate::model::sample_engine()
        };
        let (bg_tx, bg_rx) = if live {
            let (tx, rx) = std::sync::mpsc::channel();
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };
        let mut app = Self::with_engine(engine, live, !live, bg_tx, bg_rx, TextInput::local);
        app.history_in_header = true;
        app
    }

    fn with_engine(
        engine: Engine,
        live: bool,
        seed_demo: bool,
        bg_tx: Option<Sender<Bg>>,
        bg_rx: Option<Receiver<Bg>>,
        make_input: fn() -> TextInput,
    ) -> Self {
        let search_index = crate::search_provider::ContainerSearchIndex::new(&engine);
        let intents = Intents::default();
        let create = Self::create_dialog(&intents, make_input);
        let parts = view::Parts {
            model: ui::LocalState::new(view::Model::default()),
            pane: ui::LocalState::new(NavPane::Overview),
            intents: intents.clone(),
            offsets: view::Offsets::default(),
        };
        let window = view::window(&parts);
        Self {
            engine,
            engine_rev: 0,
            search_index,
            rect: Rect::ZERO,
            window,
            parts,
            published: None,
            intents,
            create,
            create_open: false,
            confirm: None,
            confirm_dialog: None,
            prompt: None,
            prompt_dialog: None,
            pull: None,
            pull_dialog: None,
            pull_dirty: false,
            pending_resource_selection: None,
            pending_resource_section: None,
            pending_resource_tab: None,
            pending_control_invocation: RawControlSlot::replace_one(),
            toast: None,
            bg_rx,
            bg_tx,
            #[cfg(test)]
            controlled_runtime: None,
            #[cfg(test)]
            controlled_immediate_results: None,
            continuation_runtime_identity: None,
            #[cfg(test)]
            history_retention_observer: None,
            #[cfg(test)]
            history_arm_restart: None,
            #[cfg(test)]
            history_runtime_replacement: None,
            last_refresh: ui::time::now(),
            live,
            scenario: None,
            seed_demo,
            seeded: false,
            structure_dirty: true,
            view: ViewState::default(),
            motion_reduced: false,
            actions: crate::resource::ActionSink::default(),
            history_in_header: false,
        }
    }

    fn create_dialog(intents: &Intents, make_input: fn() -> TextInput) -> ModalDialog<Column> {
        let submit = intents.clone();
        let cancel = intents.clone();
        let dismiss = intents.clone();
        ModalDialog::new(
            "New container",
            crate::create_form::form(make_input, intents),
        )
        .with_test_id("containers-create-dialog")
        .primary_action("Create container", move || {
            submit.push(Intent::SubmitCreate);
        })
        .secondary_action("Cancel", move || cancel.push(Intent::CloseCreate))
        .on_dismiss(move || dismiss.push(Intent::CloseCreate))
    }

    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Containers>,
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

    /// Adopt collaborative view prefs; never clear the local engine catalogue.
    pub(crate) fn adopt_resource_state(&mut self, state: &crate::resource::ContainersState) {
        self.view.section = state.section;
        self.view.selected.clone_from(&state.selected);
        self.view.tab = state.tab;
    }

    pub(crate) fn commit_view(
        &mut self,
        doc: &mut AutoCommit,
        action: &crate::resource::ContainersAction,
    ) {
        self.actions.dispatch(doc, action);
    }

    /// View edits go through the action sink. Hosted, that is the framework
    /// dispatcher. Standalone, `--shots`, and unit tests use the local host.
    pub(crate) fn apply_view(
        &mut self,
        doc: &mut AutoCommit,
        action: &crate::resource::ContainersAction,
    ) {
        self.commit_view(doc, action);
    }

    #[cfg(test)]
    pub(crate) fn last_document_action(&self) -> Option<&ui::tilo_actions::EncodedAction> {
        self.actions.last_dispatched()
    }

    /// The engine changed: the search index and the shell follow it.
    pub(crate) fn engine_changed(&mut self) {
        self.engine_rev = self.engine_rev.wrapping_add(1);
        self.search_index.sync(&self.engine);
    }

    pub(crate) fn show_toast(&mut self, message: impl Into<String>, error: bool) {
        self.toast = Some(Toast {
            message: message.into(),
            error,
            born: ui::time::now(),
        });
    }

    /// Whether lifecycle affordances are live. The deterministic sample
    /// stays interactive for demos and tests; a disconnected engine does not
    /// advertise controls that can only fail.
    pub(crate) fn engine_controls_available(&self) -> bool {
        self.engine.live || !self.live
    }

    /// Whether the sidebar reports the engine as reachable. The sample shown
    /// by demos and screenshots stands in for a connected engine.
    fn connected(&self) -> bool {
        self.engine.live || !self.live
    }

    // ---- background work ------------------------------------------------

    /// Kick a background podman re-probe so stats stay fresh without blocking
    /// the UI thread. No-op offline.
    pub(crate) fn spawn_refresh(&mut self) {
        if !self.live {
            return;
        }
        let Some(tx) = self.bg_tx.clone() else { return };
        self.last_refresh = ui::time::now();
        #[cfg(test)]
        if let Some(controlled) = &self.controlled_runtime {
            let _ = controlled.send(ControlledRuntimeRequest::Refresh(
                ControlledRefreshRequest { events: tx },
            ));
            return;
        }
        std::thread::spawn(move || {
            let engine = podman::probe().engine;
            let _ = tx.send(Bg::Snapshot(engine));
        });
    }

    #[cfg(test)]
    pub(crate) fn controlled_immediate(
        &self,
        operation: impl Into<String>,
        target: impl Into<String>,
        arguments: Vec<String>,
    ) -> Option<Result<(), String>> {
        let controlled = self.controlled_runtime.as_ref()?;
        if controlled
            .send(ControlledRuntimeRequest::Immediate {
                operation: operation.into(),
                target: target.into(),
                arguments,
            })
            .is_err()
        {
            return Some(Err(
                "controlled runtime request receiver disconnected".into()
            ));
        }
        Some(
            self.controlled_immediate_results
                .as_ref()
                .and_then(|results| results.try_recv().ok())
                .unwrap_or_else(|| Err("controlled runtime supplied no result".into())),
        )
    }

    /// Drain any background snapshot, preserving per-container detail
    /// (logs/env/mounts) for the selected container that `ps` doesn't carry.
    pub(crate) fn drain_bg(&mut self, doc: &AutoCommit) {
        let Some(rx) = &self.bg_rx else { return };
        let mut latest = None;
        let mut pull_msgs = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            match msg {
                Bg::Snapshot(e) => latest = Some(e),
                Bg::Pull(m) => pull_msgs.push(m),
            }
        }
        for msg in pull_msgs {
            self.apply_pull_msg(msg);
        }
        if let Some(mut fresh) = latest {
            // Carry over the selected container's hydrated detail so the
            // detail tabs don't flicker empty between probes.
            if let Some(sel) = ViewDoc::selected(doc)
                && let Some(old) = self.engine.find(&sel).cloned()
                && let Some(newc) = fresh.containers.iter_mut().find(|c| c.id == sel)
            {
                if newc.logs.is_empty() {
                    newc.logs = old.logs;
                }
                if newc.env.is_empty() {
                    newc.env = old.env;
                }
                if newc.mounts.is_empty() {
                    newc.mounts = old.mounts;
                }
            }
            self.engine = fresh;
            self.engine_changed();
        }
    }

    /// Hydrate the selected container's detail (logs + inspect) on demand.
    pub(crate) fn hydrate_selection(&mut self, doc: &AutoCommit) {
        let Some(sel) = ViewDoc::selected(doc) else {
            return;
        };
        let needs = self
            .engine
            .containers
            .iter()
            .find(|c| c.id == sel)
            .is_some_and(|c| c.logs.is_empty() && c.env.is_empty());
        if !needs || !self.live {
            return;
        }
        #[cfg(test)]
        if let Some(controlled) = &self.controlled_runtime {
            let _ = controlled.send(ControlledRuntimeRequest::SelectedDetails { target: sel });
            return;
        }
        let logs = podman::fetch_logs(&sel, 200);
        if let Some(c) = self.engine.containers.iter_mut().find(|c| c.id == sel) {
            podman::inspect_into(c);
            if !logs.is_empty() {
                c.logs = logs;
            }
            self.engine_changed();
        }
    }

    fn container_name(&self, id: &str) -> String {
        self.engine
            .find(id)
            .map_or_else(|| id.to_string(), |c| c.name.clone())
    }

    /// Apply a lifecycle action. Destructive ones come through the confirm
    /// dialog, so this only runs once the person has agreed.
    pub(crate) fn apply_action(&mut self, action: Action, id: &str) {
        if self.engine.live {
            let verb = match action {
                Action::Start => "start",
                Action::Stop => "stop",
                Action::Restart => "restart",
                Action::Pause => "pause",
                Action::Unpause => "unpause",
                Action::Remove => "remove",
            };
            let _ = self.pending_control_invocation.emit(ControlInvocation {
                address: ActionAddress::new(format!("container:{id}"), verb),
                arguments: serde_json::Map::new(),
            });
        } else if !self.live {
            // The sample answers the click so a demo responds.
            let name = self.container_name(id);
            self.apply_action_offline(action, id);
            self.show_toast(format!("{} {name}", past_tense(action)), false);
        } else {
            self.show_toast("Podman isn't running", true);
        }
    }

    /// Mirror a lifecycle verb in the in-memory snapshot when no engine is
    /// present. Keeps the demo interactive.
    fn apply_action_offline(&mut self, action: Action, id: &str) {
        use crate::model::ContainerStatus as S;
        match action {
            Action::Remove => self.engine.containers.retain(|c| c.id != id),
            other => {
                if let Some(c) = self.engine.containers.iter_mut().find(|c| c.id == id) {
                    c.status = match other {
                        Action::Start | Action::Restart | Action::Unpause => S::Running,
                        Action::Stop => S::Exited,
                        Action::Pause => S::Paused,
                        Action::Remove => c.status,
                    };
                    c.status_detail = match c.status {
                        S::Running => "Up less than a second".into(),
                        S::Exited => "Exited (0) less than a second ago".into(),
                        S::Paused => "Paused".into(),
                        _ => c.status_detail.clone(),
                    };
                    if !c.status.is_running() {
                        c.stats = crate::model::Stats::default();
                    }
                }
            }
        }
        self.engine_changed();
    }

    // ---- streaming pull -------------------------------------------------

    /// Start a streaming `podman pull` of `reference` on a worker thread and
    /// show its progress. Offline it simulates a quick success so the demo
    /// responds.
    pub(crate) fn start_pull(&mut self, reference: &str) {
        let reference = reference.trim().to_string();
        if reference.is_empty() {
            self.show_toast("Enter an image to pull", true);
            return;
        }
        self.pull = Some(PullProgress {
            reference: reference.clone(),
            lines: vec![format!("Trying to pull {reference}...")],
            done: false,
            failed: None,
        });
        self.pull_dirty = true;
        if self.live {
            if let Some(tx) = self.bg_tx.clone() {
                let args = podman::pull_args(&reference);
                #[cfg(test)]
                if let Some(controlled) = &self.controlled_runtime {
                    let _ =
                        controlled.send(ControlledRuntimeRequest::Pull(ControlledPullRequest {
                            reference,
                            arguments: args,
                            events: tx,
                        }));
                    return;
                }
                std::thread::spawn(move || {
                    let (ptx, prx) = std::sync::mpsc::channel();
                    let worker = std::thread::spawn(move || podman::stream_command(&args, &ptx));
                    while let Ok(msg) = prx.recv() {
                        let last = matches!(msg, StreamMsg::Done | StreamMsg::Failed(_));
                        if tx.send(Bg::Pull(msg)).is_err() || last {
                            break;
                        }
                    }
                    let _ = worker.join();
                });
            }
        } else if let Some(p) = &mut self.pull {
            p.lines.push("Getting image source signatures".into());
            p.lines.push("Copying blob sha256:9fa9226be034 done".into());
            p.lines.push("Writing manifest to image destination".into());
            p.done = true;
        }
    }

    /// Fold one streamed pull message into the progress.
    fn apply_pull_msg(&mut self, msg: StreamMsg) {
        let Some(p) = &mut self.pull else { return };
        self.pull_dirty = true;
        match msg {
            StreamMsg::Line(l) => {
                p.lines.push(l);
                let n = p.lines.len();
                if n > 200 {
                    p.lines.drain(0..n - 200);
                }
            }
            StreamMsg::Done => {
                p.done = true;
                let r = p.reference.clone();
                self.show_toast(format!("Pulled {r}"), false);
                self.spawn_refresh();
            }
            StreamMsg::Failed(e) => {
                p.failed = Some(e.clone());
                self.show_toast(format!("Couldn't pull the image: {e}"), true);
            }
        }
    }

    // ---- image / volume actions ----------------------------------------

    /// Remove an image (post-confirmation). Force-removes so an unused tag
    /// goes even if a stopped container references it.
    pub(crate) fn remove_image(&mut self, reference: &str) {
        if self.engine.live {
            let _ = self.pending_control_invocation.emit(ControlInvocation {
                address: ActionAddress::new(format!("image:{reference}"), "remove"),
                arguments: serde_json::Map::new(),
            });
        } else if !self.live {
            self.engine.images.retain(|i| i.reference() != reference);
            self.engine_changed();
            self.show_toast(format!("Removed {reference}"), false);
        } else {
            self.show_toast("Podman isn't running", true);
        }
    }

    /// Tag an image with a new reference.
    pub(crate) fn tag_image(&mut self, source: &str, new_tag: &str) {
        let new_tag = new_tag.trim();
        if new_tag.is_empty() {
            self.show_toast("Enter a tag", true);
            return;
        }
        if self.live {
            #[cfg(test)]
            if let Some(result) = self.controlled_immediate(
                "containers.image.tag",
                source,
                vec!["tag".into(), source.into(), new_tag.into()],
            ) {
                match result {
                    Ok(()) => self.show_toast(format!("Tagged {source} as {new_tag}"), false),
                    Err(error) => self.show_toast(format!("Couldn't add the tag: {error}"), true),
                }
                self.spawn_refresh();
                return;
            }
            match podman::tag_image(source, new_tag) {
                Ok(()) => self.show_toast(format!("Tagged {source} as {new_tag}"), false),
                Err(e) => self.show_toast(format!("Couldn't add the tag: {e}"), true),
            }
            self.spawn_refresh();
        } else {
            self.show_toast(format!("Tagged {source} as {new_tag}"), false);
        }
    }

    /// Create a named volume.
    pub(crate) fn create_volume(&mut self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            self.show_toast("Enter a volume name", true);
            return;
        }
        if self.live {
            #[cfg(test)]
            if let Some(result) = self.controlled_immediate(
                "containers.volume.create",
                name,
                vec!["volume".into(), "create".into(), name.into()],
            ) {
                match result {
                    Ok(()) => self.show_toast(format!("Created volume {name}"), false),
                    Err(error) => {
                        self.show_toast(format!("Couldn't create the volume: {error}"), true);
                    }
                }
                self.spawn_refresh();
                return;
            }
            match podman::create_volume(name) {
                Ok(()) => self.show_toast(format!("Created volume {name}"), false),
                Err(e) => self.show_toast(format!("Couldn't create the volume: {e}"), true),
            }
            self.spawn_refresh();
        } else {
            self.engine.volumes.push(crate::model::Volume {
                name: name.to_string(),
                driver: "local".into(),
                mountpoint: format!(
                    "/var/home/sam/.local/share/containers/storage/volumes/{name}/_data"
                ),
                created: "Just now".into(),
                in_use_by: 0,
            });
            self.engine.volumes.sort_by(|a, b| a.name.cmp(&b.name));
            self.engine_changed();
            self.show_toast(format!("Created volume {name}"), false);
        }
    }

    /// Remove a named volume (post-confirmation).
    pub(crate) fn remove_volume(&mut self, name: &str) {
        if self.engine.live {
            let _ = self.pending_control_invocation.emit(ControlInvocation {
                address: ActionAddress::new(format!("volume:{name}"), "remove"),
                arguments: serde_json::Map::new(),
            });
        } else if !self.live {
            self.engine.volumes.retain(|v| v.name != name);
            self.engine_changed();
            self.show_toast(format!("Removed volume {name}"), false);
        } else {
            self.show_toast("Podman isn't running", true);
        }
    }

    /// Open a real interactive shell in a running container: the Tilo
    /// terminal running `podman exec -it <id> /bin/sh`.
    pub(crate) fn attach_shell(&mut self, id: &str) {
        if self.engine.live {
            let _ = self.pending_control_invocation.emit(ControlInvocation {
                address: ActionAddress::new(format!("container:{id}"), "shell"),
                arguments: serde_json::Map::new(),
            });
        } else if !self.live {
            let name = self.container_name(id);
            self.show_toast(format!("Opened a shell in {name}"), false);
        } else {
            self.show_toast("Podman isn't running", true);
        }
    }

    /// Enter a distrobox in the Tilo terminal.
    fn enter_distrobox(&mut self, name: &str) {
        if self.engine.live {
            let _ = self
                .pending_control_invocation
                .emit(ui::tilo_actions::ControlInvocation {
                    address: ui::tilo_actions::ActionAddress::new(
                        format!("distrobox:{name}"),
                        "enter",
                    ),
                    arguments: serde_json::Map::new(),
                });
        } else if !self.live {
            self.show_toast(format!("Entered {name}"), false);
        } else {
            self.show_toast("Podman isn't running", true);
        }
    }

    pub(crate) fn submit_create(&mut self, doc: &mut AutoCommit) {
        let Some(args) = ViewDoc::create_run_args(doc) else {
            self.show_toast("Choose an image to create the container from", true);
            return;
        };
        let image = ViewDoc::create_field(doc, CreateField::Image);
        self.apply_view(doc, &crate::resource::ContainersAction::ClearCreate);
        self.set_create_open(false);
        if self.live {
            // Run `podman run` off the UI thread, then re-probe.
            self.show_toast(format!("Creating a container from {image}"), false);
            if let Some(tx) = self.bg_tx.clone() {
                #[cfg(test)]
                if let Some(controlled) = &self.controlled_runtime {
                    let _ = controlled.send(ControlledRuntimeRequest::Create(
                        ControlledCreateRequest {
                            arguments: args,
                            events: tx,
                        },
                    ));
                    return;
                }
                std::thread::spawn(move || {
                    let _ = podman::run_once(&args);
                    let _ = tx.send(Bg::Snapshot(podman::probe().engine));
                });
            }
        } else {
            self.show_toast(format!("Created a container from {image}"), false);
        }
    }

    // ---- dialogs --------------------------------------------------------

    pub(crate) fn set_create_open(&mut self, open: bool) {
        if self.create_open == open {
            return;
        }
        self.create_open = open;
        self.create.set_open(open);
        self.focus_draft(open.then_some(CreateField::Image));
        self.structure_dirty = true;
    }

    /// Give keyboard focus to one draft field (or none).
    pub(crate) fn focus_draft(&mut self, field: Option<CreateField>) {
        crate::create_form::for_each_input(self.create.content_mut(), &mut |input| {
            let on = field == Some(input.field());
            Widget::set_focus(input.input_mut(), on);
        });
    }

    fn rebuild_confirm(&mut self) {
        self.confirm_dialog = self.confirm.as_ref().map(|confirm| {
            let accept = self.intents.clone();
            let cancel = self.intents.clone();
            let dismiss = self.intents.clone();
            ModalDialog::new(
                confirm.title(),
                Text::new(confirm.body()).with_role(TextRole::Body),
            )
            .with_test_id("containers-confirm-dialog")
            .with_destructive(true)
            .primary_action(confirm.verb(), move || accept.push(Intent::ConfirmAccept))
            .secondary_action("Cancel", move || cancel.push(Intent::ConfirmCancel))
            .on_dismiss(move || dismiss.push(Intent::ConfirmCancel))
            .open(true)
        });
        self.structure_dirty = true;
    }

    fn rebuild_prompt(&mut self) {
        self.prompt_dialog = self.prompt.as_ref().map(|prompt| {
            let submit = self.intents.clone();
            let cancel = self.intents.clone();
            let dismiss = self.intents.clone();
            let body =
                PromptBody::new(prompt.label(), prompt.placeholder()).with_initial(&prompt.value);
            ModalDialog::new(prompt.title(), body)
                .with_test_id("containers-prompt-dialog")
                .primary_action(prompt.primary(), move || submit.push(Intent::PromptSubmit))
                .secondary_action("Cancel", move || cancel.push(Intent::PromptCancel))
                .on_dismiss(move || dismiss.push(Intent::PromptCancel))
                .open(true)
        });
        self.structure_dirty = true;
    }

    fn rebuild_pull(&mut self) {
        let had = self.pull_dialog.is_some();
        self.pull_dialog = self.pull.as_ref().map(|pull| {
            let close = self.intents.clone();
            let dismiss = self.intents.clone();
            let settled = pull.done || pull.failed.is_some();
            ModalDialog::new(format!("Pulling {}", pull.reference), view::pull_body(pull))
                .with_test_id("containers-pull-dialog")
                .primary_action(if settled { "Done" } else { "Hide" }, move || {
                    close.push(Intent::ClosePull);
                })
                .on_dismiss(move || dismiss.push(Intent::ClosePull))
                .open(true)
        });
        self.pull_dirty = false;
        if had != self.pull_dialog.is_some() || self.pull_dialog.is_some() {
            self.structure_dirty = true;
        }
    }

    pub(crate) fn ask_confirm(&mut self, confirm: Confirm) {
        self.confirm = Some(confirm);
        self.rebuild_confirm();
    }

    pub(crate) fn ask_prompt(&mut self, prompt: Prompt) {
        self.prompt = Some(prompt);
        self.rebuild_prompt();
    }

    /// Apply the pending destructive confirm.
    fn accept_confirm(&mut self, doc: &mut AutoCommit) {
        if let Some(confirm) = self.confirm.take() {
            match confirm.target {
                ConfirmTarget::Container { id, action } => {
                    self.apply_action(action, &id);
                    if ViewDoc::selected(doc).as_deref() == Some(id.as_str())
                        && !self.engine.containers.iter().any(|c| c.id == id)
                    {
                        self.commit_view(
                            doc,
                            &crate::resource::ContainersAction::SetSelected { id: None },
                        );
                    }
                }
                ConfirmTarget::Image { reference } => self.remove_image(&reference),
                ConfirmTarget::Volume { name } => self.remove_volume(&name),
            }
        }
        self.rebuild_confirm();
    }

    /// Commit the prompt's typed value.
    fn submit_prompt(&mut self) {
        let value = self
            .prompt_dialog
            .as_ref()
            .map(|dialog| dialog.content().value().trim().to_owned())
            .unwrap_or_default();
        let Some(prompt) = self.prompt.take() else {
            return;
        };
        self.rebuild_prompt();
        match prompt.kind {
            PromptKind::PullImage => self.start_pull(&value),
            PromptKind::NewVolume => self.create_volume(&value),
            PromptKind::TagImage => self.tag_image(&prompt.context, &value),
        }
    }

    /// Stage a dialog or scene for a deterministic `--shots` frame.
    pub fn seed_shot(&mut self, scenario: &str) {
        match scenario {
            "confirm" => {
                let (name, id) = view::listed_containers(&self.engine).first().map_or_else(
                    || ("web".to_string(), "web".to_string()),
                    |c| (c.name.clone(), c.id.clone()),
                );
                self.ask_confirm(Confirm {
                    name,
                    target: ConfirmTarget::Container {
                        id,
                        action: Action::Remove,
                    },
                });
            }
            "prompt" => self.ask_prompt(Prompt {
                kind: PromptKind::NewVolume,
                value: "backups".into(),
                context: String::new(),
            }),
            "pull" => {
                self.pull = Some(PullProgress {
                    reference: "docker.io/library/alpine:3.20".into(),
                    lines: vec![
                        "Trying to pull docker.io/library/alpine:3.20...".into(),
                        "Getting image source signatures".into(),
                        "Copying blob sha256:9fa9226be034 2.1 MiB / 3.4 MiB".into(),
                        "Copying config sha256:1a2b3c4d5e6f done".into(),
                    ],
                    done: false,
                    failed: None,
                });
                self.rebuild_pull();
            }
            "empty" => {
                self.engine = Engine {
                    connection: podman::UNREACHABLE.into(),
                    live: false,
                    ..Engine::default()
                };
                self.live = true;
                self.engine_changed();
            }
            _ => {}
        }
        self.scenario = Some(scenario.to_owned());
    }

    // ---- intents --------------------------------------------------------

    /// Apply what the window's controls asked for.
    pub(crate) fn drain_intents(&mut self, doc: &mut AutoCommit) {
        let intents = self.intents.take();
        if intents.is_empty() {
            return;
        }
        for intent in intents {
            self.apply_intent(doc, intent);
        }
        self.refresh_view(doc);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one flat match from every control's intent to its effect"
    )]
    fn apply_intent(&mut self, doc: &mut AutoCommit, intent: Intent) {
        use crate::resource::ContainersAction as A;
        match intent {
            Intent::ShowSection(section) => {
                self.commit_view(
                    doc,
                    &A::SetSection {
                        section: section.as_str().to_owned(),
                    },
                );
                self.parts.offsets.list.set((0.0, 0.0));
            }
            Intent::Select(id) => {
                let changed = ViewDoc::selected(doc) != id;
                let show = id.is_some();
                if changed {
                    self.commit_view(doc, &A::SetSelected { id });
                    self.parts.offsets.detail.set((0.0, 0.0));
                }
                self.parts.pane.set(if show {
                    NavPane::Content
                } else {
                    NavPane::Overview
                });
            }
            Intent::ShowTab(tab) => {
                self.commit_view(
                    doc,
                    &A::SetTab {
                        tab: tab.as_str().to_owned(),
                    },
                );
                self.parts.offsets.detail.set((0.0, 0.0));
            }
            Intent::Lifecycle { id, action } => {
                if action.is_destructive() {
                    let name = self.container_name(&id);
                    self.ask_confirm(Confirm {
                        name,
                        target: ConfirmTarget::Container { id, action },
                    });
                } else {
                    self.apply_action(action, &id);
                }
            }
            Intent::OpenShell(id) => self.attach_shell(&id),
            Intent::NewContainer => self.set_create_open(true),
            Intent::PullImage(Some(reference)) => self.start_pull(&reference),
            Intent::PullImage(None) => self.ask_prompt(Prompt {
                kind: PromptKind::PullImage,
                value: String::new(),
                context: String::new(),
            }),
            Intent::TagImage(reference) => self.ask_prompt(Prompt {
                kind: PromptKind::TagImage,
                value: String::new(),
                context: reference,
            }),
            Intent::RemoveImage(reference) => self.ask_confirm(Confirm {
                name: reference.clone(),
                target: ConfirmTarget::Image { reference },
            }),
            Intent::NewVolume => self.ask_prompt(Prompt {
                kind: PromptKind::NewVolume,
                value: String::new(),
                context: String::new(),
            }),
            Intent::RemoveVolume(name) => self.ask_confirm(Confirm {
                name: name.clone(),
                target: ConfirmTarget::Volume { name },
            }),
            Intent::EnterDistrobox(name) => self.enter_distrobox(&name),
            Intent::EditDraft(field, value) => {
                self.apply_view(
                    doc,
                    &A::SetCreateField {
                        field: field.key().to_owned(),
                        value,
                    },
                );
                Widget::sync(&mut self.create, doc, &automerge::ROOT);
            }
            Intent::SubmitCreate => self.submit_create(doc),
            Intent::CloseCreate => self.set_create_open(false),
            Intent::ConfirmAccept => self.accept_confirm(doc),
            Intent::ConfirmCancel => {
                self.confirm = None;
                self.rebuild_confirm();
            }
            Intent::PromptSubmit => self.submit_prompt(),
            Intent::PromptCancel => {
                self.prompt = None;
                self.rebuild_prompt();
            }
            Intent::ClosePull => {
                self.pull = None;
                self.rebuild_pull();
            }
        }
    }

    // ---- view state -----------------------------------------------------

    pub(crate) fn refresh_view(&mut self, doc: &AutoCommit) {
        self.view = ViewState {
            section: ViewDoc::section(doc),
            selected: ViewDoc::selected(doc).filter(|id| self.engine.find(id).is_some()),
            tab: ViewDoc::tab(doc),
        };
        self.publish_model();
    }

    /// Hand the window what it shows, when that changed.
    fn publish_model(&mut self) {
        let published = (self.engine_rev, self.view.clone());
        if self.published.as_ref() == Some(&published) {
            return;
        }
        self.published = Some(published);
        self.parts.model.set(view::Model {
            engine: Rc::new(self.engine.clone()),
            view: self.view.clone(),
            controls: self.engine_controls_available(),
            connected: self.connected(),
            missing: self.live && !self.engine.live,
            history_in_header: self.history_in_header,
        });
    }

    /// Stage the opening scene of a demo or screenshot session.
    fn seed_demo_scene(&mut self, doc: &mut AutoCommit) {
        use crate::resource::ContainersAction as A;
        self.seeded = true;
        let scene = self.scenario.clone().unwrap_or_default();
        let section = match scene.as_str() {
            "images" | "pull" => Some(Section::Images),
            "volumes" | "prompt" => Some(Section::Volumes),
            "distrobox" => Some(Section::Distrobox),
            _ => None,
        };
        if let Some(section) = section {
            self.apply_view(
                doc,
                &A::SetSection {
                    section: section.as_str().into(),
                },
            );
        }
        if scene == "create" {
            for (field, value) in [
                (CreateField::Name, "web-frontend"),
                (CreateField::Image, "docker.io/library/nginx:1.27-alpine"),
                (CreateField::Ports, "8080:80, 8443:443"),
                (CreateField::Env, "TZ=UTC"),
                (CreateField::Volumes, "site:/usr/share/nginx/html"),
            ] {
                self.apply_view(
                    doc,
                    &A::SetCreateField {
                        field: field.key().to_string(),
                        value: value.to_string(),
                    },
                );
            }
            self.set_create_open(true);
        }
        let tab = match scene.as_str() {
            "stats" => DetailTab::Stats,
            "inspect" => DetailTab::Inspect,
            "terminal" => DetailTab::Terminal,
            _ => DetailTab::Logs,
        };
        if section.is_none()
            && ViewDoc::selected(doc).is_none()
            && let Some(first) = view::listed_containers(&self.engine).first().copied()
        {
            let id = first.id.clone();
            self.apply_view(doc, &A::SetSelected { id: Some(id) });
            self.parts.pane.set(NavPane::Content);
            self.apply_view(
                doc,
                &A::SetTab {
                    tab: tab.as_str().into(),
                },
            );
        }
    }

    fn handle_keys(&self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let WindowEvent::KeyboardInput { event: key, .. } = event else {
            return;
        };
        if key.state != ElementState::Pressed
            || ctx.modifiers.state().control_key()
            || ctx.modifiers.state().super_key()
            || ctx.modifiers.state().alt_key()
        {
            return;
        }
        let intent = match &key.logical_key {
            Key::Named(NamedKey::Escape) if self.view.selected.is_some() => {
                Some(Intent::Select(None))
            }
            Key::Character(ch) => match ch.as_str() {
                "1" => Some(Intent::ShowSection(Section::Containers)),
                "2" => Some(Intent::ShowSection(Section::Images)),
                "3" => Some(Intent::ShowSection(Section::Volumes)),
                "4" => Some(Intent::ShowSection(Section::Distrobox)),
                "n" | "N" if self.engine_controls_available() => Some(Intent::NewContainer),
                _ => None,
            },
            _ => None,
        };
        if let Some(intent) = intent {
            self.intents.push(intent);
            *ctx.handled = true;
        }
    }

    /// The dialog on top, if one is open.
    fn top_dialog_mut(&mut self) -> Option<&mut dyn Widget> {
        if let Some(dialog) = self.confirm_dialog.as_mut() {
            return Some(dialog);
        }
        if let Some(dialog) = self.prompt_dialog.as_mut() {
            return Some(dialog);
        }
        if let Some(dialog) = self.pull_dialog.as_mut() {
            return Some(dialog);
        }
        if self.create_open {
            return Some(&mut self.create);
        }
        None
    }

    fn open_dialogs(&self) -> Vec<&dyn Widget> {
        let mut out: Vec<&dyn Widget> = Vec::new();
        if self.create_open {
            out.push(&self.create);
        }
        if let Some(dialog) = &self.pull_dialog {
            out.push(dialog);
        }
        if let Some(dialog) = &self.prompt_dialog {
            out.push(dialog);
        }
        if let Some(dialog) = &self.confirm_dialog {
            out.push(dialog);
        }
        out
    }

    fn open_dialogs_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut out: Vec<&mut (dyn Widget + '_)> = Vec::new();
        if self.create_open {
            out.push(&mut self.create);
        }
        if let Some(dialog) = &mut self.pull_dialog {
            out.push(dialog);
        }
        if let Some(dialog) = &mut self.prompt_dialog {
            out.push(dialog);
        }
        if let Some(dialog) = &mut self.confirm_dialog {
            out.push(dialog);
        }
        out
    }

    fn paint_toast(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        window: Rect,
    ) {
        let Some(toast) = &self.toast else { return };
        let kind = if toast.error {
            ToastKind::Error
        } else {
            ToastKind::Success
        };
        paint_transient(
            scene,
            transform,
            tr,
            theme,
            window,
            ToastAnchor::BottomCenter,
            &ToastCard::new(&toast.message, kind),
        );
    }

    #[cfg(test)]
    fn publish_history_retention(&self) {
        let Some(observer) = &self.history_retention_observer else {
            return;
        };
        let mut instances = Vec::new();
        let mut values = Vec::new();
        let mut cursors = Vec::new();
        let mut selections = Vec::new();
        crate::create_form::each_input(self.create.content(), &mut |input| {
            instances.push(std::ptr::from_ref(input.input()).addr());
            values.push(input.input().value().to_owned());
            cursors.push(input.input().cursor_pos());
            selections.push(input.input().selection_range_public());
        });
        let pending_control = self
            .pending_control_invocation
            .controlled_pending_for_test()
            .map(|invocation| {
                (
                    invocation.address.resource.clone(),
                    invocation.address.action,
                )
            });
        observer.replace(Some(HistoryRetentionSnapshot {
            input_instances: instances,
            input_values: values,
            input_cursors: cursors,
            input_selections: selections,
            list_offset: self.parts.offsets.list.get(),
            detail_offset: self.parts.offsets.detail.get(),
            engine_connection: self.engine.connection.clone(),
            engine_counts: (
                self.engine.containers.len(),
                self.engine.images.len(),
                self.engine.volumes.len(),
                self.engine.distroboxes.len(),
            ),
            last_refresh: self.last_refresh,
            pending_control,
            background_channels: (self.bg_tx.is_some(), self.bg_rx.is_some()),
        }));
    }
}

/// How a finished lifecycle verb reads in a toast.
pub(crate) fn past_tense(action: Action) -> &'static str {
    match action {
        Action::Start => "Started",
        Action::Stop => "Stopped",
        Action::Restart => "Restarted",
        Action::Pause => "Paused",
        Action::Unpause => "Resumed",
        Action::Remove => "Removed",
    }
}

// =====================================================================
// Widget impl
// =====================================================================

impl Widget for ContainersApp {
    ui::provides!(
        dyn ui::history_mode::capability::DocumentHistory,
        dyn ui::widget::controls::ActionControls,
        dyn ui::resources::PublishesResources,
        dyn ui::search::capability::SearchSource,
        dyn ui::undo::CommittedActions
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[
            "containers.engine",
            "containers.container",
            "containers.logs",
            "containers.image",
            "containers.volume",
            "containers.distrobox",
        ]
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            self.drain_intents(doc);
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
            #[cfg(test)]
            if self
                .history_runtime_replacement
                .as_ref()
                .is_some_and(|(replace, _, _)| replace.replace(false))
                && let Some((_, runtime, identity)) = self.history_runtime_replacement.take()
            {
                self.controlled_runtime = Some(runtime);
                self.continuation_runtime_identity = Some(identity);
            }
            #[cfg(test)]
            if self
                .history_arm_restart
                .as_ref()
                .is_some_and(|arm| arm.replace(false))
            {
                self.apply_action(Action::Restart, "armed-container");
            }
            if let Some(target) = self.pending_resource_selection.take() {
                self.commit_view(
                    doc,
                    &crate::resource::ContainersAction::SetSelected { id: Some(target) },
                );
                self.parts.pane.set(NavPane::Content);
            }
            if let Some(section) = self.pending_resource_section.take() {
                self.commit_view(
                    doc,
                    &crate::resource::ContainersAction::SetSection {
                        section: section.as_str().to_owned(),
                    },
                );
            }
            if let Some(tab) = self.pending_resource_tab.take() {
                self.commit_view(
                    doc,
                    &crate::resource::ContainersAction::SetTab {
                        tab: tab.as_str().to_owned(),
                    },
                );
            }
            self.drain_bg(doc);
            if self.seed_demo && !self.seeded {
                self.seed_demo_scene(doc);
            }
            // Periodically re-probe the engine. Controlled fixtures own that
            // cadence through their request channel.
            if self.live && self.last_refresh.elapsed() >= podman::REFRESH_INTERVAL && {
                #[cfg(test)]
                {
                    self.controlled_runtime.is_none()
                }
                #[cfg(not(test))]
                {
                    true
                }
            } {
                self.spawn_refresh();
            }
            self.hydrate_selection(doc);
            self.drain_intents(doc);
            if self.pull_dirty {
                self.rebuild_pull();
            }
            if let Some(t) = &self.toast
                && t.born.elapsed().as_millis() > TOAST_MS
            {
                self.toast = None;
            }
            self.refresh_view(doc);
        }
        self.window.sync(state, path);
        self.create.sync(state, path);
        for dialog in [
            self.confirm_dialog.as_mut().map(|d| d as &mut dyn Widget),
            self.prompt_dialog.as_mut().map(|d| d as &mut dyn Widget),
            self.pull_dialog.as_mut().map(|d| d as &mut dyn Widget),
        ]
        .into_iter()
        .flatten()
        {
            dialog.sync(state, path);
        }
        #[cfg(test)]
        self.publish_history_retention();
    }

    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
        {
            let (doc, _) = ui::doc::document(state, path);
            self.drain_intents(doc);
            if self.pull_dirty {
                self.rebuild_pull();
            }
        }
        let window = self.window.update(state, path);
        window || self.structure_dirty
    }

    fn needs_idle_sync(&self) -> bool {
        self.live
            || self.toast.is_some()
            || self
                .pull
                .as_ref()
                .is_some_and(|p| !p.done && p.failed.is_none())
    }

    fn needs_animation_frame(&self) -> bool {
        let mut needed = !self.motion_reduced && self.toast.is_some();
        self.for_each_child(&mut |child| {
            needed = needed || child.needs_animation_frame();
        });
        needed
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let shell = self.window.build_layout(taffy, theme, node_map);
        let mut children = vec![shell];
        for dialog in self.open_dialogs_mut() {
            let node = dialog.build_layout(taffy, theme, node_map);
            let mut style = taffy.style(node).cloned().unwrap_or_default();
            style.position = taffy::Position::Absolute;
            let _ = taffy.set_style(node, style);
            children.push(node);
        }
        let node = taffy
            .new_with_children(
                Style {
                    display: Display::Flex,
                    size: Size {
                        width: percent(1.0_f32),
                        height: percent(1.0_f32),
                    },
                    ..Default::default()
                },
                &children,
            )
            .unwrap_or(shell);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(layout) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        let children = taffy.children(node).unwrap_or_default();
        let at = Point::new(self.rect.x0, self.rect.y0);
        let mut nodes = children.iter();
        if let Some(&shell) = nodes.next() {
            self.window.apply_layout(taffy, shell, at);
        }
        for (dialog, &child) in self.open_dialogs_mut().into_iter().zip(nodes) {
            dialog.apply_layout(taffy, child, at);
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mine = std::mem::take(&mut self.structure_dirty);
        let mut children = false;
        for child in self.children_mut() {
            children |= child.take_structure_dirty();
        }
        mine || children
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.window.draw(scene, transform, tr, theme, presence);
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        for child in self.children_mut() {
            child.draw_overlay(scene, transform, tr, theme, presence);
        }
        let window = presence.viewport.get().unwrap_or(self.rect);
        self.paint_toast(scene, transform, tr, theme, window);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let children = ctx.taffy.children(ctx.node_id).unwrap_or_default();
        let dialogs = self.open_dialogs().len();
        if dialogs > 0 {
            // The dialog on top is modal: it receives every event.
            let node = children.get(dialogs).copied();
            if let (Some(dialog), Some(node)) = (self.top_dialog_mut(), node) {
                dialog.handle_event(event, &mut ctx.reborrow(ctx.path, node));
            }
            *ctx.handled = true;
        } else {
            if let Some(&node) = children.first() {
                self.window
                    .handle_event(event, &mut ctx.reborrow(ctx.path, node));
            }
            if !*ctx.handled {
                self.handle_keys(event, ctx);
            }
        }
        self.drain_intents(ctx.state);
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut out: Vec<&dyn Widget> = vec![&self.window];
        out.extend(self.open_dialogs());
        out
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        visitor(&self.window);
        for dialog in self.open_dialogs() {
            visitor(dialog);
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let create_open = self.create_open;
        let mut out: Vec<&mut (dyn Widget + '_)> = vec![&mut self.window];
        if create_open {
            out.push(&mut self.create);
        }
        if let Some(dialog) = &mut self.pull_dialog {
            out.push(dialog);
        }
        if let Some(dialog) = &mut self.prompt_dialog {
            out.push(dialog);
        }
        if let Some(dialog) = &mut self.confirm_dialog {
            out.push(dialog);
        }
        out
    }

    fn name(&self) -> &'static str {
        "ContainersApp"
    }

    fn test_id(&self) -> Option<&str> {
        Some("containers-root")
    }

    fn traps_focus(&self) -> bool {
        !self.open_dialogs().is_empty()
    }

    fn accessibility(&self) -> Option<AccessNode> {
        let mut root = AccessNode::new(Role::Window)
            .with_name("Containers")
            .with_description(&self.engine.connection)
            .with_bounds(self.rect);
        if let Some(toast) = &self.toast {
            root.children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(&toast.message)
                    .with_hashed_key("containers.toast")
                    .with_live(if toast.error {
                        Politeness::Assertive
                    } else {
                        Politeness::Polite
                    }),
            );
        }
        Some(root)
    }

    fn set_motion_reduced(&mut self, reduced: bool) {
        self.motion_reduced = reduced;
        for child in self.children_mut() {
            child.set_motion_reduced(reduced);
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

impl ui::search::capability::SearchSource for ContainersApp {
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(self.search_index.provider())]
    }
}

impl ui::undo::CommittedActions for ContainersApp {
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }
}

impl ui::widget::controls::ActionControls for ContainersApp {
    fn install_control_custody(
        &mut self,
        domain: &ui::ControlDomain,
        attachment: ui::control_custody::CustodyAttachment,
    ) {
        for child in self.children_mut() {
            ui::widget::controls::install_control_custody(child, domain, attachment);
        }
        let _ = self
            .pending_control_invocation
            .install_custody(domain, attachment);
    }

    fn take_control_invocation(&mut self) -> Option<ui::tilo_actions::ControlInvocation> {
        for child in self.children_mut() {
            if let Some(invocation) = ui::widget::controls::take_control_invocation(child) {
                return Some(invocation);
            }
        }
        self.pending_control_invocation.take_legacy_compatibility()
    }
}

impl ui::history_mode::capability::DocumentHistory for ContainersApp {
    fn document_history_definition(&self) -> Option<&'static ui::history_mode::HistoryDefinition> {
        Some(&crate::framework_history::HISTORY)
    }
}

// =====================================================================
// Test roots
// =====================================================================

impl ContainersApp {
    #[doc(hidden)]
    pub fn history_test_root() -> Self {
        Self::with_engine(
            Engine::default(),
            false,
            false,
            None,
            None,
            controlled_input,
        )
    }

    #[cfg(test)]
    pub fn history_test_open_create_root() -> Self {
        let mut app = Self::history_test_root();
        app.set_create_open(true);
        app.parts.offsets.list.set((0.0, 41.0));
        app.parts.offsets.detail.set((0.0, 17.0));
        app
    }

    #[cfg(test)]
    pub fn history_test_prompt_root() -> Self {
        let mut app = Self::history_test_root();
        app.seed_shot("prompt");
        app
    }

    #[cfg(test)]
    pub fn history_test_confirm_root() -> Self {
        let mut app = Self::history_test_root();
        app.seed_shot("confirm");
        app
    }

    #[cfg(test)]
    pub fn history_test_pull_root() -> Self {
        let mut app = Self::history_test_root();
        app.pull = Some(PullProgress {
            reference: "quay.io/example/pending:1".into(),
            lines: vec!["Copying blob 1/2".into()],
            done: false,
            failed: None,
        });
        app.rebuild_pull();
        app
    }

    #[cfg(test)]
    pub fn history_test_runtime_root() -> (
        Self,
        Receiver<ControlledRuntimeRequest>,
        Sender<Result<(), String>>,
    ) {
        let engine = Engine {
            live: true,
            connection: "controlled Podman".into(),
            ..Engine::default()
        };
        let (bg_tx, bg_rx) = std::sync::mpsc::channel();
        let (requests_tx, requests_rx) = std::sync::mpsc::channel();
        let (results_tx, results_rx) = std::sync::mpsc::channel();
        let mut app = Self::with_engine(
            engine,
            true,
            false,
            Some(bg_tx),
            Some(bg_rx),
            controlled_input,
        );
        app.controlled_runtime = Some(requests_tx);
        app.controlled_immediate_results = Some(results_rx);
        app.continuation_runtime_identity = next_continuation_epoch()
            .zip(next_continuation_epoch())
            .map(
                |(runtime_epoch, target_epoch)| ContinuationRuntimeIdentity {
                    runtime_epoch,
                    target_epoch,
                    target_id: "armed-container".into(),
                },
            );
        (app, requests_rx, results_tx)
    }

    #[cfg(test)]
    pub fn history_test_retention_root() -> (
        Self,
        Receiver<ControlledRuntimeRequest>,
        Rc<RefCell<Option<HistoryRetentionSnapshot>>>,
    ) {
        let (mut app, requests, _results) = Self::history_test_runtime_root();
        app.parts.offsets.list.set((0.0, 41.0));
        app.parts.offsets.detail.set((0.0, 17.0));
        let observer = Rc::new(RefCell::new(None));
        app.history_retention_observer = Some(observer.clone());
        (app, requests, observer)
    }

    #[cfg(test)]
    pub fn history_test_armed_retention_root() -> ArmedRetentionRoot {
        let (mut app, requests, results) = Self::history_test_runtime_root();
        app.parts.offsets.list.set((0.0, 41.0));
        app.parts.offsets.detail.set((0.0, 17.0));

        app.engine = Self::history_test_runtime_engine("controlled Podman", false);
        app.engine_changed();
        app.spawn_refresh();
        app.start_pull("quay.io/example/armed:1");

        let mut create_doc = AutoCommit::new();
        ViewDoc::set_create_field(&mut create_doc, CreateField::Name, "armed-name");
        ViewDoc::set_create_field(
            &mut create_doc,
            CreateField::Image,
            "quay.io/example/create:1",
        );
        ViewDoc::set_create_field(&mut create_doc, CreateField::Ports, "8081:81");
        app.submit_create(&mut create_doc);

        let mut selection_doc = AutoCommit::new();
        ViewDoc::set_selected(&mut selection_doc, Some("armed-container"));
        app.hydrate_selection(&selection_doc);
        app.engine = Self::history_test_runtime_engine("controlled Podman", true);
        app.engine_changed();
        let observer = Rc::new(RefCell::new(None));
        app.history_retention_observer = Some(observer.clone());
        let arm = Rc::new(Cell::new(false));
        app.history_arm_restart = Some(arm.clone());
        let (replacement_tx, replacement_rx) = std::sync::mpsc::channel();
        let replace = Rc::new(Cell::new(false));
        if let Some((runtime_epoch, target_epoch)) =
            next_continuation_epoch().zip(next_continuation_epoch())
        {
            app.history_runtime_replacement = Some((
                replace.clone(),
                replacement_tx,
                ContinuationRuntimeIdentity {
                    runtime_epoch,
                    target_epoch,
                    target_id: "armed-container".into(),
                },
            ));
        }
        (
            app,
            requests,
            results,
            observer,
            arm,
            replacement_rx,
            replace,
        )
    }

    #[cfg(test)]
    pub fn history_test_runtime_engine(connection: &str, hydrated: bool) -> Engine {
        Engine {
            live: true,
            connection: connection.into(),
            containers: vec![crate::model::Container {
                id: "armed-container".into(),
                name: "Armed container".into(),
                image: "quay.io/example/armed:1".into(),
                status: crate::model::ContainerStatus::Running,
                status_detail: "Up under controlled runtime".into(),
                command: "/usr/bin/example".into(),
                created: "controlled".into(),
                ports: Vec::new(),
                mounts: Vec::new(),
                env: if hydrated {
                    vec![("MODE".into(), "controlled".into())]
                } else {
                    Vec::new()
                },
                labels: Vec::new(),
                stats: crate::model::Stats::default(),
                logs: if hydrated {
                    vec!["controlled detail".into()]
                } else {
                    Vec::new()
                },
            }],
            ..Engine::default()
        }
    }

    /// Whether the new-container form is open.
    pub fn is_create_open(&self) -> bool {
        self.create_open
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ReadDoc;
    use ui::tilo_actions::ActionAddress;

    fn driver(app: ContainersApp) -> ui::headless::Driver {
        ui::headless::Driver::for_root(ui::State::new(), app, (1280, 800), ui::Theme::dark())
    }

    fn app_of(driver: &mut ui::headless::Driver) -> &mut ContainersApp {
        driver.root_mut::<ContainersApp>()
    }

    fn doc_of(driver: &mut ui::headless::Driver) -> &mut AutoCommit {
        &mut driver.state_mut().doc
    }

    #[test]
    fn baseline_fresh_sync_must_not_author_create_draft() {
        let mut app = ContainersApp::history_test_root();
        let mut doc = AutoCommit::new();
        let heads = doc.get_heads();
        Widget::sync(&mut app, &mut doc, &automerge::ROOT);
        assert_eq!(
            doc.get_heads(),
            heads,
            "ordinary sync authored the document"
        );
        assert!(doc.get(&automerge::ROOT, "view").unwrap().is_none());
    }

    #[test]
    fn keys_and_buttons_move_between_sections_and_open_dialogs() {
        let mut d = driver(ContainersApp::new(false));
        let first = view::listed_containers(&app_of(&mut d).engine)[0]
            .id
            .clone();
        assert_eq!(
            ViewDoc::selected(doc_of(&mut d)).as_deref(),
            Some(first.as_str()),
            "a demo opens on the first container"
        );

        d.key("2");
        assert_eq!(ViewDoc::section(doc_of(&mut d)), Section::Images);
        d.click_named("images-pull").unwrap();
        assert_eq!(
            app_of(&mut d).prompt.as_ref().map(|p| p.kind),
            Some(PromptKind::PullImage)
        );
        d.key("Escape");
        assert!(app_of(&mut d).prompt.is_none(), "Escape cancels the prompt");

        d.key("3");
        assert_eq!(ViewDoc::section(doc_of(&mut d)), Section::Volumes);
        d.click_named("volumes-new").unwrap();
        assert_eq!(
            app_of(&mut d).prompt.as_ref().map(|p| p.kind),
            Some(PromptKind::NewVolume)
        );
        d.type_text("cache");
        d.key("Enter");
        assert!(app_of(&mut d).prompt.is_none());
        assert!(
            app_of(&mut d)
                .engine
                .volumes
                .iter()
                .any(|volume| volume.name == "cache"),
            "the typed name became a volume"
        );

        d.key("1");
        assert_eq!(ViewDoc::section(doc_of(&mut d)), Section::Containers);
        d.key("Escape");
        assert!(
            ViewDoc::selected(doc_of(&mut d)).is_none(),
            "Escape returns from the detail to the list"
        );
    }

    #[test]
    fn removing_asks_first_and_then_runs_once() {
        let mut d = driver(ContainersApp::new(false));
        let first = view::listed_containers(&app_of(&mut d).engine)[0].clone();
        assert!(first.status.is_running());
        d.click_named(&format!("detail-remove-{}", first.id))
            .unwrap();
        assert_eq!(
            app_of(&mut d).confirm.as_ref().map(|c| c.name.clone()),
            Some(first.name.clone()),
            "Remove opens the confirmation"
        );
        assert!(app_of(&mut d).engine.find(&first.id).is_some());
        d.key("Enter");
        assert!(app_of(&mut d).confirm.is_none());
        assert!(
            app_of(&mut d).engine.find(&first.id).is_none(),
            "the confirmed removal ran"
        );
        assert!(
            ViewDoc::selected(doc_of(&mut d)).is_none(),
            "the removed container is no longer selected"
        );

        let stop = view::listed_containers(&app_of(&mut d).engine)[0].clone();
        app_of(&mut d)
            .intents
            .push(Intent::Select(Some(stop.id.clone())));
        d.relayout();
        d.click_named(&format!("detail-stop-{}", stop.id)).unwrap();
        assert!(
            app_of(&mut d).confirm.is_none(),
            "stopping is not confirmed"
        );
        assert_eq!(
            app_of(&mut d).engine.find(&stop.id).map(|c| c.status),
            Some(crate::model::ContainerStatus::Exited)
        );
    }

    #[test]
    fn the_new_container_form_writes_the_shared_draft_and_submits_it() {
        let mut d = driver(ContainersApp::new(false));
        d.key("n");
        assert!(app_of(&mut d).create_open);
        d.type_text("alpine");
        assert_eq!(
            ViewDoc::create_field(doc_of(&mut d), CreateField::Image),
            "alpine",
            "typing in the focused image field writes the draft"
        );
        assert_eq!(
            ViewDoc::create_run_args(doc_of(&mut d)),
            Some(vec!["run".into(), "-d".into(), "alpine".into()])
        );
        d.key("Enter");
        assert!(
            !app_of(&mut d).create_open,
            "Enter submits and closes the form"
        );
        assert_eq!(
            ViewDoc::stored_create_field(doc_of(&mut d), CreateField::Image),
            crate::doc::StoredCreateField::Absent,
            "submitting clears the draft"
        );
    }

    #[test]
    fn lifecycle_effects_and_resources_are_exported_and_bounded() {
        // --- phase: lifecycle_control_emits_the_exported_effect_instead_of_running_podman_inline ---
        {
            let mut app = ContainersApp::new(false);
            app.engine.live = true;
            let id = app.engine.containers[0].id.clone();
            app.apply_action(Action::Stop, &id);
            let intent = ui::widget::controls::take_control_invocation(&mut app)
                .expect("the host receives an inert resource intent");
            assert_eq!(
                intent.address,
                ActionAddress::new(format!("container:{id}"), "stop")
            );
            assert!(intent.arguments.is_empty());
            assert_eq!(
                app.engine.find(&id).map(|container| container.status),
                Some(crate::model::ContainerStatus::Running),
                "the control does not mutate the engine before host authorization"
            );
        }

        // --- phase: resource_snapshot_is_bounded_and_omitted_identity_resolves_lazily ---
        {
            let limit = crate::published::RESOURCE_SNAPSHOT_LIMIT;
            let mut app = ContainersApp::new(false);
            let template = app.engine.containers[0].clone();
            app.engine.containers.clear();
            for index in 0..(limit + 17) {
                let mut container = template.clone();
                container.id = format!("durable-{index:03}");
                container.name = format!("Container {index:03}");
                app.engine.containers.push(container);
            }
            app.engine_changed();

            let doc = AutoCommit::new();
            let root = ui::resources::resources(&app, &doc).remove(0);
            assert_eq!(root.children.len(), limit);
            let omitted = format!("container:durable-{:03}", limit + 16);
            assert!(
                root.children
                    .iter()
                    .all(|resource| resource.id.0 != omitted)
            );
            assert_eq!(
                ui::resources::resource(&app, &doc, &omitted).map(|resource| resource.id.0),
                Some(omitted.clone())
            );

            app.engine
                .containers
                .retain(|container| format!("container:{}", container.id) != omitted);
            app.engine_changed();
            assert!(
                ui::resources::resource(&app, &doc, &omitted).is_none(),
                "a stale search address cannot recreate removed authority"
            );
            assert!(
                ui::search::SearchProvider::search(&app.search_index.provider(), &omitted)
                    .is_empty()
            );
        }
    }

    #[test]
    fn history_controlled_pull_retains_channel_and_settles_once() {
        let (mut app, requests, _results) = ContainersApp::history_test_runtime_root();
        app.start_pull(" quay.io/example/image:1 ");
        let ControlledRuntimeRequest::Pull(request) = requests.recv().unwrap() else {
            panic!("expected one controlled pull request");
        };
        assert_eq!(request.reference, "quay.io/example/image:1");
        assert_eq!(
            request.arguments,
            crate::podman::pull_args("quay.io/example/image:1")
        );
        assert!(
            requests.try_recv().is_err(),
            "pull was dispatched only once"
        );

        request.line("Copying blob 1/1").unwrap();
        request.done().unwrap();
        let doc = AutoCommit::new();
        app.drain_bg(&doc);
        let pull = app.pull.as_ref().expect("retained pull progress");
        assert_eq!(
            pull.lines.last().map(String::as_str),
            Some("Copying blob 1/1")
        );
        assert!(pull.done);
        assert!(pull.failed.is_none());
        assert!(matches!(
            requests.recv().unwrap(),
            ControlledRuntimeRequest::Refresh(_)
        ));
        let terminal = app.toast.as_ref().map(|toast| toast.born);
        app.drain_bg(&doc);
        assert_eq!(app.toast.as_ref().map(|toast| toast.born), terminal);
        assert!(
            requests.try_recv().is_err(),
            "terminal event was reduced once"
        );
    }

    #[test]
    fn controlled_runtime_owns_refresh_cadence() {
        let (mut app, requests, _results) = ContainersApp::history_test_runtime_root();
        app.last_refresh = Instant::now()
            .checked_sub(crate::podman::REFRESH_INTERVAL + std::time::Duration::from_millis(1))
            .expect("refresh interval fits below Instant::now");
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        assert!(
            requests.try_recv().is_err(),
            "controlled fixtures own refresh; wall-clock must not enqueue Refresh"
        );
    }

    #[test]
    fn history_controlled_create_keeps_copied_args_and_drains_one_snapshot() {
        let (mut app, requests, _results) = ContainersApp::history_test_runtime_root();
        let mut doc = AutoCommit::new();
        ViewDoc::set_create_field(&mut doc, CreateField::Name, "web");
        ViewDoc::set_create_field(&mut doc, CreateField::Image, "nginx:1");
        ViewDoc::set_create_field(&mut doc, CreateField::Ports, "8080:80");
        let historical = doc.get_heads()[0];
        app.set_create_open(true);
        app.submit_create(&mut doc);
        let ControlledRuntimeRequest::Create(request) = requests.recv().unwrap() else {
            panic!("expected one controlled create request");
        };
        assert_eq!(
            request.arguments,
            vec!["run", "-d", "--name", "web", "-p", "8080:80", "nginx:1"]
        );
        assert!(!app.create_open);
        assert_eq!(
            ViewDoc::stored_create_field(&doc, CreateField::Image),
            crate::doc::StoredCreateField::Absent
        );

        let current = doc.get_heads();
        let restore = ui::history_mode::restore::prepare_content_restore(
            &doc,
            &crate::framework_history::SCHEMA,
            &current,
            &[historical],
        )
        .unwrap();
        ui::history_mode::restore::apply_prepared_content_restore(&mut doc, &restore).unwrap();
        assert_eq!(ViewDoc::create_field(&doc, CreateField::Image), "nginx:1");
        assert_eq!(
            request.arguments.last().map(String::as_str),
            Some("nginx:1")
        );

        let refreshed = Engine {
            live: true,
            connection: "controlled post-run snapshot".into(),
            ..Engine::default()
        };
        request.post_run_snapshot(refreshed).unwrap();
        app.drain_bg(&doc);
        assert_eq!(app.engine.connection, "controlled post-run snapshot");
        app.drain_bg(&doc);
        assert!(
            requests.try_recv().is_err(),
            "create was dispatched only once"
        );
    }

    #[test]
    fn history_controlled_immediate_edges_capture_exact_requests() {
        let (mut app, requests, results) = ContainersApp::history_test_runtime_root();

        results.send(Ok(())).unwrap();
        app.tag_image("quay.io/example/source:1", " registry.local/copy:2 ");
        match requests.recv().unwrap() {
            ControlledRuntimeRequest::Immediate {
                operation,
                target,
                arguments,
            } => {
                assert_eq!(operation, "containers.image.tag");
                assert_eq!(target, "quay.io/example/source:1");
                assert_eq!(
                    arguments,
                    ["tag", "quay.io/example/source:1", "registry.local/copy:2"]
                );
            }
            _ => panic!("expected captured tag request"),
        }
        assert!(matches!(
            requests.recv().unwrap(),
            ControlledRuntimeRequest::Refresh(_)
        ));

        results.send(Ok(())).unwrap();
        app.create_volume(" cache ");
        match requests.recv().unwrap() {
            ControlledRuntimeRequest::Immediate {
                operation,
                target,
                arguments,
            } => {
                assert_eq!(operation, "containers.volume.create");
                assert_eq!(target, "cache");
                assert_eq!(arguments, ["volume", "create", "cache"]);
            }
            _ => panic!("expected captured volume request"),
        }
        assert!(matches!(
            requests.recv().unwrap(),
            ControlledRuntimeRequest::Refresh(_)
        ));
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn history_pending_control_intent_keeps_exact_target_until_host_take() {
        let (mut app, requests, _results) = ContainersApp::history_test_runtime_root();
        app.apply_action(Action::Restart, "retained-target");
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        app.sync(&mut doc, &automerge::ROOT);
        let intent =
            ui::widget::controls::take_control_invocation(&mut app).expect("retained intent");
        assert_eq!(intent.address.resource, "container:retained-target");
        assert_eq!(intent.address.action, "restart");
        assert!(intent.arguments.is_empty());
        assert!(ui::widget::controls::take_control_invocation(&mut app).is_none());
        assert!(requests.try_recv().is_err());
    }

    fn image_input(app: &ContainersApp) -> (usize, String, bool) {
        let mut found = (0, String::new(), false);
        crate::create_form::each_input(app.create.content(), &mut |input| {
            if input.field() == CreateField::Image {
                found = (
                    std::ptr::from_ref(input.input()).addr(),
                    input.input().value().to_owned(),
                    Widget::is_focused(input.input()),
                );
            }
        });
        found
    }

    #[test]
    fn restored_binding_never_edits_stale_text_object() {
        let mut doc = AutoCommit::new();
        ViewDoc::set_create_field(&mut doc, CreateField::Image, "old");
        let stale = ViewDoc::create_field_obj(&doc, CreateField::Image).unwrap();
        let mut app = ContainersApp::history_test_root();
        app.set_create_open(true);
        let mut d = ui::headless::Driver::for_root(
            ui::State::from_doc(doc),
            app,
            (1000, 750),
            ui::Theme::dark(),
        );
        let (instance, value, focused) = image_input(app_of(&mut d));
        assert_eq!(value, "old");
        assert!(focused);

        ViewDoc::clear_create(doc_of(&mut d));
        ViewDoc::set_create_field(doc_of(&mut d), CreateField::Image, "new");
        let current = ViewDoc::create_field_obj(doc_of(&mut d), CreateField::Image).unwrap();
        assert_ne!(stale, current);
        d.relayout();
        let (same, value, focused) = image_input(app_of(&mut d));
        assert_eq!(same, instance, "the field is the same instance");
        assert_eq!(value, "new");
        assert!(focused);

        d.key("End");
        d.type_text("!");
        assert_eq!(doc_of(&mut d).text(&stale).unwrap(), "old");
        assert_eq!(doc_of(&mut d).text(&current).unwrap(), "new!");
        assert_eq!(
            ViewDoc::create_field(doc_of(&mut d), CreateField::Image),
            "new!"
        );
    }
}
