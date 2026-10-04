//! Root widget for the container manager.
//!
//! [`ContainersApp`] owns the engine snapshot (containers, images, volumes
//! and distroboxes mirrored from rootless podman) and presents it in a
//! [`NavShell`]: the sections are the sidebar, the container list is the
//! overview and the chosen container is the content (see [`crate::view`]).
//! Dialogs (confirming a removal, asking for a name, the new-container form,
//! pull progress) are [`ModalDialog`]s mounted beside the shell.
//!
//! The create draft lives in the shared document; navigation is local and the engine projection is host data and stays in memory.

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

use crate::doc::{CreateDoc, CreateDraft, CreateField};
use crate::model::Engine;
use crate::model::{Container, DetailTab, Section};
use crate::podman::{self, Action, StreamMsg};
use crate::resource::Containers;
use crate::view::{self, Intent, Intents};
use ui::framework::{Dispatcher, LiveRow};
use ui::tilo_actions::{ActionSpec, OperationKind, PermissionRequirement, RetryPolicy, flatten};
use ui::tilo_resource::TextEdit;
use ui::widget::command_palette::{CommandEntry, CommandPalette};

/// How long a result toast stays on screen.
const TOAST_MS: u128 = 4000;

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
            PromptKind::TagImage => "ghcr.io/northwind/api:edge",
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
enum Bg {
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

pub struct ContainersApp {
    pub(crate) engine: Engine,
    engine_rev: u64,
    pub(crate) search_index: crate::search_provider::ContainerSearchIndex,
    rect: Rect,
    window: ModelView<(Section, bool, bool)>,
    parts: view::Parts,
    published: Option<(u64, ViewState)>,
    intents: Intents,
    create: ModalDialog<Column>,
    pub(crate) create_open: bool,
    pub(crate) confirm: Option<Confirm>,
    confirm_dialog: Option<ModalDialog<Text>>,
    pub(crate) prompt: Option<Prompt>,
    prompt_dialog: Option<ModalDialog<PromptBody>>,
    pub(crate) pull: Option<PullProgress>,
    pull_dialog: Option<ModalDialog<Column>>,
    pull_dirty: bool,
    pub(crate) pending_control_invocation: RawControlSlot,
    pub(crate) toast: Option<Toast>,
    bg_rx: Option<Receiver<Bg>>,
    bg_tx: Option<Sender<Bg>>,
    #[cfg(test)]
    controlled_runtime: Option<Sender<ControlledRuntimeRequest>>,
    #[cfg(test)]
    controlled_immediate_results: Option<Receiver<Result<(), String>>>,
    pub(crate) last_refresh: Instant,
    pub(crate) live: bool,
    structure_dirty: bool,
    pub(crate) view: ViewState,
    motion_reduced: bool,
    past: bool,
    draft: CreateDraft,
    dispatcher: Option<Dispatcher<Containers>>,
    palette: CommandPalette,
    palette_open: bool,
}
impl Default for ContainersApp {
    fn default() -> Self {
        Self::new()
    }
}
fn is_escape(event: &WindowEvent) -> bool {
    matches!(
        event,
        WindowEvent::KeyboardInput { event: key, .. }
            if key.state == ElementState::Pressed
                && key.logical_key == Key::Named(NamedKey::Escape)
    )
}
fn controlled_input() -> TextInput {
    TextInput::local_with_language("en_US")
        .with_spellcheck(false)
        .with_autocorrect(false)
}
impl ContainersApp {
    pub fn new() -> Self {
        Self::with_engine(Engine::default(), TextInput::local)
    }
    pub fn sample() -> Self {
        Self::with_engine(crate::model::sample_engine(), TextInput::local)
    }
    fn with_engine(engine: Engine, make_input: fn() -> TextInput) -> Self {
        let search_index = crate::search_provider::ContainerSearchIndex::new(&engine);
        let intents = Intents::default();
        let create = Self::create_dialog(&intents, make_input);
        let parts = view::Parts {
            model: ui::LocalState::new(view::Model::default()),
            pane: ui::LocalState::new(NavPane::Overview),
            intents: intents.clone(),
            offsets: view::Offsets::default(),
            sidebar_hidden: ui::LocalState::new(false),
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
            pending_control_invocation: RawControlSlot::replace_one(),
            toast: None,
            bg_rx: None,
            bg_tx: None,
            #[cfg(test)]
            controlled_runtime: None,
            #[cfg(test)]
            controlled_immediate_results: None,
            last_refresh: ui::time::now(),
            live: false,
            structure_dirty: true,
            view: ViewState::default(),
            motion_reduced: false,
            dispatcher: None,
            draft: CreateDraft::default(),
            past: false,
            palette: CommandPalette::commands(),
            palette_open: false,
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
    pub(crate) fn connect(&mut self) {
        let (tx, rx) = std::sync::mpsc::channel();
        self.bg_tx = Some(tx);
        self.bg_rx = Some(rx);
        self.live = true;
        self.show_engine(podman::probe().engine);
    }
    pub(crate) fn show_engine(&mut self, engine: Engine) {
        self.engine = engine;
        self.engine_changed();
        self.structure_dirty = true;
    }
    pub(crate) fn show_past(&mut self) {
        self.past = true;
        self.show_engine(Engine {
            connection: "Containers, images, volumes and distroboxes are this device's \
                         live state and are not part of History"
                .into(),
            live: false,
            ..Engine::default()
        });
    }
    pub(crate) fn present_draft(&mut self, draft: &CreateDraft) {
        draft.clone_into(&mut self.draft);
        if self.past {
            self.set_create_open(!draft.is_empty());
        }
    }
    pub(crate) fn show_section(&mut self, section: Section) {
        self.view.section = section;
        self.view.selected = None;
        self.parts.offsets.list.set((0.0, 0.0));
        self.structure_dirty = true;
    }
    pub(crate) fn select_container(&mut self, id: Option<String>) {
        if id.is_some() {
            self.view.section = Section::Containers;
        }
        self.parts.pane.set(if id.is_some() {
            NavPane::Content
        } else {
            NavPane::Overview
        });
        self.view.selected = id;
        self.parts.offsets.detail.set((0.0, 0.0));
        self.structure_dirty = true;
    }
    pub(crate) fn show_tab(&mut self, tab: DetailTab) {
        self.view.tab = tab;
        self.parts.offsets.detail.set((0.0, 0.0));
        self.structure_dirty = true;
    }
    pub(crate) fn set_create_field(
        &mut self,
        doc: &mut AutoCommit,
        field: CreateField,
        text: &str,
    ) {
        let Some(dispatcher) = &self.dispatcher else {
            return;
        };
        let edits = crate::resource::create_field_dispatcher(dispatcher, field);
        if let Err(error) = edits.apply_now(
            doc,
            &TextEdit::ReplaceText {
                text: text.to_owned(),
            },
        ) {
            self.show_toast(format!("The draft was not changed: {error}"), true);
        }
        self.refresh_view(doc);
    }
    fn clear_create(&self, doc: &mut AutoCommit) {
        if let Some(dispatcher) = &self.dispatcher {
            let _ = dispatcher.apply_now(doc, &crate::resource::ContainersAction::ClearCreate);
        }
    }
    pub(crate) fn engine_rows(&self) -> Vec<LiveRow> {
        let live = self.engine.live;
        let mut rows: Vec<LiveRow> = self
            .engine
            .containers
            .iter()
            .map(|container| container_row(container, live))
            .chain(
                self.engine
                    .images
                    .iter()
                    .map(|image| image_row(image, live)),
            )
            .chain(
                self.engine
                    .volumes
                    .iter()
                    .map(|volume| volume_row(volume, live)),
            )
            .chain(
                self.engine
                    .distroboxes
                    .iter()
                    .map(|distrobox| distrobox_row(distrobox, live)),
            )
            .take(RESOURCE_SNAPSHOT_LIMIT)
            .collect();
        if let Some(pull) = &self.pull {
            rows.push(LiveRow::new(
                "containers.pull-job",
                "containers.job",
                format!("Pulling {}", pull.reference),
            ));
        }
        rows
    }
    pub(crate) fn engine_row(&self, id: &str) -> Option<LiveRow> {
        let live = self.engine.live;
        if let Some(container_id) = id
            .strip_prefix("container:")
            .and_then(|rest| rest.strip_suffix(":logs"))
        {
            return self.engine.find(container_id).map(logs_row);
        }
        if let Some(container_id) = id.strip_prefix("container:") {
            return self
                .engine
                .find(container_id)
                .map(|container| container_row(container, live));
        }
        if let Some(reference) = id.strip_prefix("image:") {
            return self
                .engine
                .images
                .iter()
                .find(|item| item.reference() == reference)
                .map(|item| image_row(item, live));
        }
        if let Some(name) = id.strip_prefix("volume:") {
            return self
                .engine
                .volumes
                .iter()
                .find(|item| item.name == name)
                .map(|item| volume_row(item, live));
        }
        if let Some(name) = id.strip_prefix("distrobox:") {
            return self
                .engine
                .distroboxes
                .iter()
                .find(|item| item.name == name)
                .map(|item| distrobox_row(item, live));
        }
        None
    }
    pub(crate) fn show_toast(&mut self, message: impl Into<String>, error: bool) {
        self.toast = Some(Toast {
            message: message.into(),
            error,
            born: ui::time::now(),
        });
    }
    fn spawn_refresh(&mut self) {
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
    fn drain_bg(&mut self) {
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
            if let Some(sel) = self.view.selected.clone()
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
    fn hydrate_selection(&mut self) {
        let Some(sel) = self.view.selected.clone() else {
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
    pub(crate) fn apply_action(&mut self, action: Action, id: &str) {
        let name = self
            .engine
            .find(id)
            .map_or_else(|| id.to_string(), |c| c.name.clone());
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
            self.show_toast(format!("{} {} requested", action.label(), name), false);
        } else if !self.live {
            // Offline: reflect the action in the local snapshot so the
            // sample surface still responds to clicks.
            self.apply_action_offline(action, id);
            self.show_toast(format!("{} {}", action.label(), name), false);
        } else {
            self.show_toast("Container engine is offline", true);
        }
    }
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
                        S::Running => "Up just now".into(),
                        S::Exited => "Exited (0) just now".into(),
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
        self.structure_dirty = true;
    }
    pub(crate) fn start_pull(&mut self, reference: &str) {
        self.pull_dirty = true;
        let reference = reference.trim().to_string();
        if reference.is_empty() {
            self.show_toast("Enter an image reference to pull", true);
            return;
        }
        self.pull = Some(PullProgress {
            reference: reference.clone(),
            lines: vec![format!("Trying to pull {reference}...")],
            done: false,
            failed: None,
        });
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
        } else {
            // Offline: deterministic simulated progress so --shots/demo works.
            if let Some(p) = &mut self.pull {
                p.lines.push("Getting image source signatures".into());
                p.lines.push("Copying blob sha256:1a2b3c… done".into());
                p.lines.push("Writing manifest to image destination".into());
                p.done = true;
            }
        }
    }
    fn apply_pull_msg(&mut self, msg: StreamMsg) {
        self.pull_dirty = true;
        let Some(p) = &mut self.pull else { return };
        match msg {
            StreamMsg::Line(l) => {
                p.lines.push(l);
                // Bound the overlay to a readable tail.
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
                self.show_toast(format!("Pull failed: {e}"), true);
            }
        }
    }
    pub(crate) fn remove_image(&mut self, reference: &str) {
        if self.engine.live {
            let _ = self.pending_control_invocation.emit(ControlInvocation {
                address: ActionAddress::new(format!("image:{reference}"), "remove"),
                arguments: serde_json::Map::new(),
            });
            self.show_toast(format!("Remove {reference} requested"), false);
        } else if !self.live {
            self.engine.images.retain(|i| i.reference() != reference);
            self.engine_changed();
            self.show_toast(format!("Removed {reference}"), false);
            self.structure_dirty = true;
        } else {
            self.show_toast("Container engine is offline", true);
        }
    }
    pub(crate) fn tag_image(&mut self, source: &str, new_tag: &str) {
        let new_tag = new_tag.trim();
        if new_tag.is_empty() {
            self.show_toast("Enter a new tag", true);
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
                    Err(error) => self.show_toast(format!("Tag failed: {error}"), true),
                }
                self.spawn_refresh();
                return;
            }
            match podman::tag_image(source, new_tag) {
                Ok(()) => self.show_toast(format!("Tagged {source} as {new_tag}"), false),
                Err(e) => self.show_toast(format!("Tag failed: {e}"), true),
            }
            self.spawn_refresh();
        } else {
            self.show_toast(format!("podman tag {source} {new_tag}"), false);
        }
    }
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
                    Err(error) => self.show_toast(format!("Create failed: {error}"), true),
                }
                self.spawn_refresh();
                return;
            }
            match podman::create_volume(name) {
                Ok(()) => self.show_toast(format!("Created volume {name}"), false),
                Err(e) => self.show_toast(format!("Create failed: {e}"), true),
            }
            self.spawn_refresh();
        } else {
            self.engine.volumes.push(crate::model::Volume {
                name: name.to_string(),
                driver: "local".into(),
                mountpoint: format!(".../volumes/{name}/_data"),
                created: "just now".into(),
                in_use_by: 0,
            });
            self.engine.volumes.sort_by(|a, b| a.name.cmp(&b.name));
            self.engine_changed();
            self.show_toast(format!("Created volume {name}"), false);
            self.structure_dirty = true;
        }
    }
    pub(crate) fn remove_volume(&mut self, name: &str) {
        if self.engine.live {
            let _ = self.pending_control_invocation.emit(ControlInvocation {
                address: ActionAddress::new(format!("volume:{name}"), "remove"),
                arguments: serde_json::Map::new(),
            });
            self.show_toast(format!("Remove volume {name} requested"), false);
        } else if !self.live {
            self.engine.volumes.retain(|v| v.name != name);
            self.engine_changed();
            self.show_toast(format!("Removed volume {name}"), false);
            self.structure_dirty = true;
        } else {
            self.show_toast("Container engine is offline", true);
        }
    }
    pub(crate) fn attach_shell(&mut self, id: &str) {
        let short = id.chars().take(12).collect::<String>();
        if self.engine.live {
            let _ = self.pending_control_invocation.emit(ControlInvocation {
                address: ActionAddress::new(format!("container:{id}"), "shell"),
                arguments: serde_json::Map::new(),
            });
            self.show_toast(format!("Shell for {short} requested"), false);
        } else if !self.live {
            self.show_toast(format!("podman exec -it {short} /bin/sh"), false);
        } else {
            self.show_toast("Container engine is offline", true);
        }
    }
    pub(crate) fn submit_create(&mut self, doc: &mut AutoCommit) {
        let draft = CreateDoc::read(doc);
        let Some(args) = draft.run_args() else {
            self.show_toast("Image is required to create a container", true);
            return;
        };
        let name = draft.get(CreateField::Image).to_owned();
        if self.live {
            // Run `podman run` off the UI thread; report the result back via a
            // toast streamed through the bg channel, then re-probe.
            self.show_toast(format!("Creating container from {name}…"), false);
            self.clear_create(doc);
            self.set_create_open(false);
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
                    let out = podman::run_once(&args);
                    // Whatever the result, push a fresh snapshot so the new
                    // container appears (or the failure is implied by absence).
                    let _ = out;
                    let _ = tx.send(Bg::Snapshot(podman::probe().engine));
                });
            }
        } else {
            self.show_toast(format!("podman {}", args.join(" ")), false);
            self.clear_create(doc);
            self.set_create_open(false);
        }
    }
    pub(crate) fn visible_containers(&self) -> Vec<&Container> {
        let mut v: Vec<&Container> = self.engine.containers.iter().collect();
        v.sort_by(|a, b| {
            b.status
                .is_running()
                .cmp(&a.status.is_running())
                .then_with(|| a.name.cmp(&b.name))
        });
        v
    }
    pub(crate) fn open_container_palette(&mut self) {
        let resources: Vec<_> = self
            .engine_rows()
            .into_iter()
            .map(LiveRow::into_resource)
            .collect();
        let entries: Vec<CommandEntry> = flatten(&resources)
            .into_iter()
            .filter(|action| action.action.id == "select")
            .map(|action| CommandEntry::from_flat_action(&action))
            .collect();
        self.palette.set_entries(entries);
        self.palette.open();
        self.palette_open = true;
        self.structure_dirty = true;
    }
    fn answer_resource_operation(
        &mut self,
        kind: OperationKind,
        type_id: &str,
        payload: &[u8],
    ) -> Result<bool, String> {
        let args: Vec<String> = serde_json::from_slice(payload).map_err(|e| e.to_string())?;
        let Some(verb) = args.first().map(String::as_str) else {
            return Err("missing operation".into());
        };
        let target = args.get(1).map(String::as_str).unwrap_or_default();
        if kind == OperationKind::View && type_id == "containers.container" && verb == "select" {
            self.select_container(Some(target.to_string()));
            return Ok(true);
        }
        if kind == OperationKind::View && type_id == "containers.logs" && verb == "logs" {
            self.select_container(Some(target.to_string()));
            self.show_tab(DetailTab::Logs);
            return Ok(true);
        }
        if kind == OperationKind::View && verb == "open" {
            self.show_section(match type_id {
                "containers.image" => Section::Images,
                "containers.volume" => Section::Volumes,
                "containers.distrobox" => Section::Distrobox,
                _ => return Ok(false),
            });
            return Ok(true);
        }
        if kind != OperationKind::Effect {
            return Ok(false);
        }
        if !self.engine.live {
            return Err("container engine actions are unavailable while Podman is offline".into());
        }
        #[cfg(test)]
        if let Some(result) = self.controlled_immediate(
            format!("{type_id}.{verb}"),
            target.to_string(),
            args.clone(),
        ) {
            result?;
            self.spawn_refresh();
            self.show_toast(format!("{verb} accepted for {target}"), false);
            return Ok(true);
        }
        match type_id {
            "containers.container" => match verb {
                "start" => {
                    podman::run_action(Action::Start, target)?;
                }
                "stop" => {
                    podman::run_action(Action::Stop, target)?;
                }
                "restart" => {
                    podman::run_action(Action::Restart, target)?;
                }
                "pause" => {
                    podman::run_action(Action::Pause, target)?;
                }
                "unpause" => {
                    podman::run_action(Action::Unpause, target)?;
                }
                "remove" => {
                    podman::run_action(Action::Remove, target)?;
                }
                "shell" => {
                    crate::terminal::spawn_exec_attach(target, "/bin/sh")
                        .map_err(|e| e.to_string())?;
                }
                _ => return Ok(false),
            },
            "containers.image" if verb == "remove" => {
                podman::remove_image(target, true)?;
            }
            "containers.volume" if verb == "remove" => {
                podman::remove_volume(target, false)?;
            }
            "containers.distrobox" if verb == "enter" => {
                crate::terminal::spawn_distrobox_enter(target).map_err(|e| e.to_string())?;
            }
            _ => return Ok(false),
        }
        self.spawn_refresh();
        self.show_toast(format!("{verb} accepted for {target}"), false);
        Ok(true)
    }
    pub(crate) fn engine_controls_available(&self) -> bool {
        self.engine.live
            || (!self.live
                && (!self.engine.containers.is_empty()
                    || !self.engine.images.is_empty()
                    || !self.engine.volumes.is_empty()
                    || !self.engine.distroboxes.is_empty()))
    }
    pub fn section(&self) -> Section {
        self.view.section
    }
    pub fn selected(&self) -> Option<&str> {
        self.view.selected.as_deref()
    }
    pub fn tab(&self) -> DetailTab {
        self.view.tab
    }
    pub fn is_create_open(&self) -> bool {
        self.create_open
    }
    #[cfg(test)]
    pub(crate) fn controlled_engine(connection: &str, hydrated: bool) -> Engine {
        Engine {
            live: true,
            connection: connection.into(),
            containers: vec![Container {
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
    #[cfg(test)]
    pub(crate) fn controlled_runtime_root() -> (
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
        let mut app = Self::with_engine(engine, controlled_input);
        app.live = true;
        app.bg_tx = Some(bg_tx);
        app.bg_rx = Some(bg_rx);
        app.controlled_runtime = Some(requests_tx);
        app.controlled_immediate_results = Some(results_rx);
        (app, requests_rx, results_tx)
    }
    #[cfg(test)]
    fn controlled_immediate(
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
    #[cfg(test)]
    pub(crate) fn dispatch_palette_invocation_for_test(&mut self, doc: &mut AutoCommit) {
        let intent = ui::widget::controls::take_control_invocation(self)
            .expect("palette emitted a resource action");
        let resources: Vec<_> = self
            .engine_rows()
            .into_iter()
            .map(LiveRow::into_resource)
            .collect();
        let spec = ui::tilo_actions::door::resolve(&resources, &intent.address)
            .expect("palette address resolves");
        let kind = spec.kind;
        let encoded = spec
            .build_json(&intent.arguments)
            .expect("valid palette arguments");
        assert!(
            self.answer_resource_operation(kind, &encoded.type_id, &encoded.payload,)
                .expect("resource operation")
        );
        self.sync(doc, &automerge::ROOT);
    }

    pub(crate) fn attach_dispatcher(&mut self, dispatcher: Dispatcher<Containers>) {
        crate::create_form::for_each_input(self.create.content_mut(), &mut |input| {
            input.attach(crate::resource::create_field_dispatcher(
                &dispatcher,
                input.field(),
            ));
        });
        self.dispatcher = Some(dispatcher);
    }
    pub(crate) fn open_create(&mut self) {
        self.set_create_open(true);
    }
    pub(crate) fn refresh_view(&mut self, _doc: &AutoCommit) {
        self.publish_model();
    }
    fn connected(&self) -> bool {
        self.engine_controls_available()
    }
    pub(crate) fn engine_changed(&mut self) {
        self.engine_rev = self.engine_rev.wrapping_add(1);
        self.search_index.sync(&self.engine);
        let options = self.engine.image_references();
        crate::create_form::for_each_input(self.create.content_mut(), &mut |input| {
            input.set_options(&options)
        });
    }
    fn container_name(&self, id: &str) -> String {
        self.engine
            .find(id)
            .map_or_else(|| id.to_string(), |c| c.name.clone())
    }
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
    pub(crate) fn set_create_open(&mut self, open: bool) {
        if self.create_open == open {
            return;
        }
        self.create_open = open;
        self.create.set_open(open);
        self.focus_draft(open.then_some(CreateField::Image));
        self.structure_dirty = true;
    }
    pub(crate) fn focus_draft(&mut self, field: Option<CreateField>) {
        crate::create_form::for_each_input(self.create.content_mut(), &mut |input| {
            let on = field == Some(input.field());
            Widget::set_focus(input, on);
            if !on {
                input.close_suggestions();
            }
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
            let dismiss = self.intents.clone();
            let title = if pull.failed.is_some() {
                format!("Couldn't pull {}", pull.reference)
            } else if pull.done {
                format!("Pulled {}", pull.reference)
            } else {
                format!("Pulling {}", pull.reference)
            };
            // One way out: the close control (and Escape). Closing while the
            // pull runs only hides it; a toast says when it finishes.
            ModalDialog::new(title, view::pull_body(pull))
                .with_test_id("containers-pull-dialog")
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
    fn publish_model(&mut self) {
        let published = (self.engine_rev, self.view.clone());
        if self.published.as_ref() == Some(&published) {
            return;
        }
        self.published = Some(published);
        self.parts.model.set(
            view::Model {
                engine: Rc::new(self.engine.clone()),
                view: self.view.clone(),
                controls: self.engine_controls_available(),
                connected: self.connected(),
                missing: self.live && !self.engine.live,
                ..view::Model::default()
            }
            .digested(),
        );
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
    fn accept_confirm(&mut self, doc: &mut AutoCommit) {
        if let Some(confirm) = self.confirm.take() {
            match confirm.target {
                ConfirmTarget::Container { id, action } => {
                    self.apply_action(action, &id);
                    if self.view.selected.clone().as_deref() == Some(id.as_str())
                        && !self.engine.containers.iter().any(|c| c.id == id)
                    {
                        self.select_container(None);
                    }
                }
                ConfirmTarget::Image { reference } => self.remove_image(&reference),
                ConfirmTarget::Volume { name } => self.remove_volume(&name),
            }
        }
        self.rebuild_confirm();
    }
    fn apply_intent(&mut self, doc: &mut AutoCommit, intent: Intent) {
        match intent {
            Intent::ShowSection(section) => self.show_section(section),
            Intent::Select(id) => self.select_container(id),
            Intent::ShowTab(tab) => self.show_tab(tab),
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
            Intent::EditDraft(field, value) => self.set_create_field(doc, field, &value),
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
            Intent::Reconnect => self.spawn_refresh(),
        }
    }
    pub fn stage_scene(&mut self, scenario: Option<&str>) {
        let scene = scenario.unwrap_or_default();
        match scene {
            "images" | "pull" => self.show_section(Section::Images),
            "volumes" => self.show_section(Section::Volumes),
            "distrobox" => self.show_section(Section::Distrobox),
            "create" | "combo" => {
                self.open_create();
                if scene == "combo" {
                    crate::create_form::for_each_input(self.create.content_mut(), &mut |input| {
                        input.open_suggestions()
                    });
                }
                // The image suggestion dropdown, open over the form.
            }
            "collapsed" => self.parts.sidebar_hidden.set(true),
            // The shared Ctrl+K palette, seeded with the sample containers.
            "palette" => self.open_container_palette(),
            "confirm" => {
                let (name, id) = self.engine.containers.first().map_or_else(
                    || ("web".to_string(), "web".to_string()),
                    |c| (c.name.clone(), c.id.clone()),
                );
                self.confirm = Some(Confirm {
                    name,
                    target: ConfirmTarget::Container {
                        id,
                        action: Action::Remove,
                    },
                });
            }
            "prompt" => {
                self.prompt = Some(Prompt {
                    kind: PromptKind::NewVolume,
                    value: "backups".into(),
                    context: String::new(),
                });
            }
            _ => {}
        }
        if scene == "pull" {
            self.pull = Some(PullProgress {
                reference: "docker.io/library/alpine:3.20".into(),
                lines: vec![
                    "Trying to pull docker.io/library/alpine:3.20...".into(),
                    "Getting image source signatures".into(),
                    "Copying blob sha256:9fa9226be034 [===========>     ] 2.1MiB / 3.4MiB".into(),
                    "Copying config sha256:1a2b3c4d5e6f done".into(),
                ],
                done: false,
                failed: None,
            });
        }
        let list_scene = matches!(scene, "images" | "volumes" | "distrobox" | "pull");
        if !list_scene
            && let Some(first) = self
                .engine
                .containers
                .iter()
                .find(|c| c.status.is_running())
                .or_else(|| self.engine.containers.first())
        {
            let id = first.id.clone();
            self.select_container(Some(id));
            self.show_tab(match scene {
                "stats" => DetailTab::Stats,
                "inspect" => DetailTab::Inspect,
                "terminal" => DetailTab::Terminal,
                _ => DetailTab::Logs,
            });
        }
        self.rebuild_confirm();
        self.rebuild_prompt();
        self.rebuild_pull();
        self.publish_model();
    }
}
const RESOURCE_SNAPSHOT_LIMIT: usize = 200;

/// A view or effect operation on a row, its payload the verb and target.
fn row_operation(
    id: &str,
    label: impl Into<String>,
    kind: OperationKind,
    type_id: &str,
    payload: &[&str],
) -> Option<ActionSpec> {
    ActionSpec::operation(id, label, kind, type_id, &payload).ok()
}

/// An engine effect: needs an administrator, and the engine.
fn engine_effect(spec: ActionSpec, engine_live: bool, offline: &str) -> ActionSpec {
    let spec = spec.requiring(PermissionRequirement::Admin).privileged();
    if engine_live {
        spec
    } else {
        spec.unavailable("engine_offline", offline)
    }
}

const ENGINE_OFFLINE: &str = "Container engine actions are unavailable in demo mode";

pub(crate) fn container_row(container: &crate::model::Container, engine_live: bool) -> LiveRow {
    let mut row = LiveRow::new(
        format!("container:{}", container.id),
        "containers.container",
        container.name.clone(),
    );
    if let Some(spec) = row_operation(
        "select",
        format!("Open {}", container.name),
        OperationKind::View,
        "containers.container",
        &["select", &container.id],
    ) {
        row = row.operation(spec);
    }
    let verbs: &[(&str, &str)] = match container.status {
        crate::model::ContainerStatus::Running | crate::model::ContainerStatus::Restarting => &[
            ("stop", "Stop"),
            ("restart", "Restart"),
            ("pause", "Pause"),
            ("shell", "Open shell"),
        ],
        crate::model::ContainerStatus::Paused => &[
            ("unpause", "Resume"),
            ("stop", "Stop"),
            ("remove", "Remove"),
            ("shell", "Open shell"),
        ],
        _ => &[("start", "Start"), ("remove", "Remove")],
    };
    for (verb, label) in verbs {
        if let Some(spec) = row_operation(
            verb,
            *label,
            OperationKind::Effect,
            "containers.container",
            &[verb, &container.id],
        ) {
            let mut spec = engine_effect(spec, engine_live, ENGINE_OFFLINE);
            if *verb == "remove" {
                spec = spec.destructive();
            } else if *verb != "restart" && *verb != "shell" {
                spec = spec.retry_policy(RetryPolicy::Idempotent);
            }
            row = row.operation(spec);
        }
    }
    row.row(logs_row(container))
}

fn logs_row(container: &crate::model::Container) -> LiveRow {
    let mut logs = LiveRow::new(
        format!("container:{}:logs", container.id),
        "containers.logs",
        format!("{} logs", container.name),
    );
    if let Some(spec) = row_operation(
        "open",
        "Open logs",
        OperationKind::View,
        "containers.logs",
        &["logs", &container.id],
    ) {
        logs = logs.operation(spec);
    }
    logs
}

fn image_row(image: &crate::model::Image, engine_live: bool) -> LiveRow {
    let reference = image.reference();
    let mut row = LiveRow::new(
        format!("image:{reference}"),
        "containers.image",
        reference.clone(),
    );
    if let Some(spec) = row_operation(
        "open",
        "Show image",
        OperationKind::View,
        "containers.image",
        &["open", &reference],
    ) {
        row = row.operation(spec);
    }
    if let Some(spec) = row_operation(
        "remove",
        "Remove image",
        OperationKind::Effect,
        "containers.image",
        &["remove", &reference],
    ) {
        row = row.operation(engine_effect(spec, engine_live, ENGINE_OFFLINE).destructive());
    }
    row
}

fn volume_row(volume: &crate::model::Volume, engine_live: bool) -> LiveRow {
    let mut row = LiveRow::new(
        format!("volume:{}", volume.name),
        "containers.volume",
        volume.name.clone(),
    );
    if let Some(spec) = row_operation(
        "open",
        "Show volume",
        OperationKind::View,
        "containers.volume",
        &["open", &volume.name],
    ) {
        row = row.operation(spec);
    }
    if let Some(spec) = row_operation(
        "remove",
        "Remove volume",
        OperationKind::Effect,
        "containers.volume",
        &["remove", &volume.name],
    ) {
        row = row.operation(engine_effect(spec, engine_live, ENGINE_OFFLINE).destructive());
    }
    row
}

fn distrobox_row(distrobox: &crate::model::DistroBox, engine_live: bool) -> LiveRow {
    let mut row = LiveRow::new(
        format!("distrobox:{}", distrobox.name),
        "containers.distrobox",
        distrobox.name.clone(),
    );
    if let Some(spec) = row_operation(
        "open",
        "Show distrobox",
        OperationKind::View,
        "containers.distrobox",
        &["open", &distrobox.name],
    ) {
        row = row.operation(spec);
    }
    if let Some(spec) = row_operation(
        "enter",
        "Enter distrobox",
        OperationKind::Effect,
        "containers.distrobox",
        &["enter", &distrobox.name],
    ) {
        row = row.operation(engine_effect(
            spec,
            engine_live,
            "Distrobox actions are unavailable in demo mode",
        ));
    }
    row
}

impl Widget for ContainersApp {
    ui::provides!(
        dyn ui::widget::controls::ActionControls,
        dyn ui::search::capability::SearchSource
    );
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[
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
            return Ok(Some(started));
        }
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        Ok(self
            .answer_resource_operation(request.kind, request.type_id, request.payload)?
            .then(|| {
                ui::widget::ResourceOperationStart::Completed(
                    ui::tilo_actions::ActionOutput::default(),
                )
            }))
    }
    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        {
            let (doc, _) = ui::doc::document(state, path);
            self.drain_bg();
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
            self.hydrate_selection();
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
        self.palette.sync(state, path);
        let options = self.engine.image_references();
        crate::create_form::for_each_input(self.create.content_mut(), &mut |input| {
            input.set_options(&options)
        });
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
        if self.palette_open {
            let (width, height) = self.palette.desired_size();
            let center = self.rect.center().x;
            self.palette.set_rect(Rect::new(
                center - width / 2.0,
                self.rect.y0 + 88.0,
                center + width / 2.0,
                self.rect.y0 + 88.0 + height,
            ));
            self.palette.draw(scene, transform, tr, theme, presence);
        }
        let window = presence.viewport.get().unwrap_or(self.rect);
        self.paint_toast(scene, transform, tr, theme, window);
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if self.palette_open {
            self.palette.handle_event(event, ctx);
            if !self.palette.is_open() {
                self.palette_open = false;
            }
            *ctx.handled = true;
            return;
        }
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
            let pane = self.parts.pane.get();
            if let Some(&node) = children.first() {
                self.window
                    .handle_event(event, &mut ctx.reborrow(ctx.path, node));
            }
            // Escape closes the detail the same way at every width: in a
            // narrow window the shell takes it as Back, and the selection
            // goes with the screen it was shown on.
            let went_back = pane == NavPane::Content && self.parts.pane.get() == NavPane::Overview;
            if is_escape(event) && self.view.selected.is_some() && (went_back || !*ctx.handled) {
                self.intents.push(Intent::Select(None));
                *ctx.handled = true;
            } else if !*ctx.handled {
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
        self.palette_open || !self.open_dialogs().is_empty()
    }
    fn accessibility(&self) -> Option<AccessNode> {
        if self.palette_open {
            return self.palette.accessibility();
        }
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
    fn modal_capture(&self) -> Option<Rect> {
        self.traps_focus().then_some(self.rect)
    }
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        if self.palette_open {
            return self
                .palette
                .perform_semantic_action(target, action, state, path);
        }
        false
    }
    fn set_motion_reduced(&mut self, reduced: bool) {
        self.motion_reduced = reduced;
        self.palette.set_motion_reduced(reduced);
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
impl ui::widget::controls::ActionControls for ContainersApp {
    fn install_control_custody(
        &mut self,
        domain: &ui::ControlDomain,
        attachment: ui::control_custody::CustodyAttachment,
    ) {
        ui::widget::controls::install_control_custody(&mut self.palette, domain, attachment);
        for child in self.children_mut() {
            ui::widget::controls::install_control_custody(child, domain, attachment);
        }
        let _ = self
            .pending_control_invocation
            .install_custody(domain, attachment);
    }

    fn take_control_invocation(&mut self) -> Option<ui::tilo_actions::ControlInvocation> {
        if let Some(intent) = ui::widget::controls::take_control_invocation(&mut self.palette) {
            return Some(intent);
        }
        for child in self.children_mut() {
            if let Some(invocation) = ui::widget::controls::take_control_invocation(child) {
                return Some(invocation);
            }
        }
        self.pending_control_invocation.take_legacy_compatibility()
    }
}

#[cfg(test)]
mod runtime_tests {
    use super::*;
    #[test]
    fn controlled_pull_retains_channel_and_settles_once() {
        let (mut app, requests, _results) = ContainersApp::controlled_runtime_root();
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
        app.drain_bg();
        let pull = app.pull.as_ref().expect("retained pull overlay");
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
        app.drain_bg();
        assert_eq!(app.toast.as_ref().map(|toast| toast.born), terminal);
        assert!(
            requests.try_recv().is_err(),
            "terminal event was reduced once"
        );
    }
    #[test]
    fn controlled_runtime_owns_refresh_cadence() {
        let (mut app, requests, _results) = ContainersApp::controlled_runtime_root();
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
    fn controlled_create_runs_the_draft_and_drains_one_snapshot() {
        let (mut app, requests, _results) = ContainersApp::controlled_runtime_root();
        let mut doc = AutoCommit::new();
        for (field, text) in [
            (CreateField::Name, "web"),
            (CreateField::Image, "nginx:1"),
            (CreateField::Ports, "8080:80"),
        ] {
            ui::tilo_actions::Action::apply(
                &crate::resource::ContainersAction::EditCreate {
                    field,
                    edit: TextEdit::ReplaceText { text: text.into() },
                },
                &mut doc,
            )
            .unwrap();
        }
        app.create_open = true;
        app.submit_create(&mut doc);
        let ControlledRuntimeRequest::Create(request) = requests.recv().unwrap() else {
            panic!("expected one controlled create request");
        };
        assert_eq!(
            request.arguments,
            vec!["run", "-d", "--name", "web", "-p", "8080:80", "nginx:1"]
        );
        assert!(!app.create_open);

        let refreshed = Engine {
            live: true,
            connection: "controlled post-run snapshot".into(),
            ..Engine::default()
        };
        request.post_run_snapshot(refreshed).unwrap();
        app.drain_bg();
        assert_eq!(app.engine.connection, "controlled post-run snapshot");
        app.drain_bg();
        assert!(
            requests.try_recv().is_err(),
            "create was dispatched only once"
        );
    }
    #[test]
    fn controlled_immediate_edges_capture_exact_requests() {
        let (mut app, requests, results) = ContainersApp::controlled_runtime_root();

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
    fn pending_control_intent_keeps_exact_target_until_host_take() {
        let (mut app, requests, _results) = ContainersApp::controlled_runtime_root();
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
    #[test]
    fn control_custody_containers_all_five_boundaries_copy_empty_descriptors_and_offline_preserves_last()
     {
        let (mut app, _requests, _results) = ContainersApp::controlled_runtime_root();
        app.engine = ContainersApp::controlled_engine("controlled Podman", true);

        let pending = |app: &ContainersApp| {
            let invocation = app
                .pending_control_invocation
                .controlled_pending_for_test()
                .expect("retained private descriptor");
            assert!(invocation.arguments.is_empty());
            invocation.address
        };

        app.apply_action(Action::Restart, "armed-container");
        assert_eq!(
            pending(&app),
            ui::tilo_actions::ActionAddress::new("container:armed-container", "restart")
        );
        app.remove_image("quay.io/example/image:1");
        assert_eq!(
            pending(&app),
            ui::tilo_actions::ActionAddress::new("image:quay.io/example/image:1", "remove")
        );
        app.remove_volume("example-volume");
        assert_eq!(
            pending(&app),
            ui::tilo_actions::ActionAddress::new("volume:example-volume", "remove")
        );
        app.attach_shell("armed-container");
        assert_eq!(
            pending(&app),
            ui::tilo_actions::ActionAddress::new("container:armed-container", "shell")
        );
        app.enter_distrobox("example-box");
        let last = pending(&app);
        assert_eq!(
            last,
            ui::tilo_actions::ActionAddress::new("distrobox:example-box", "enter")
        );

        app.engine.live = false;
        app.remove_volume("offline-volume");
        assert_eq!(
            pending(&app),
            last,
            "an offline attempt does not clear the older descriptor"
        );
    }
    #[test]
    fn lifecycle_effects_and_resources_are_exported_and_bounded() {
        // --- phase: lifecycle_control_emits_the_exported_effect_instead_of_running_podman_inline ---
        {
            let mut app = ContainersApp::sample();
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
            let mut app = ContainersApp::sample();
            let template = app.engine.containers[0].clone();
            app.engine.containers.clear();
            for index in 0..(RESOURCE_SNAPSHOT_LIMIT + 17) {
                let mut container = template.clone();
                container.id = format!("durable-{index:03}");
                container.name = format!("Container {index:03}");
                app.engine.containers.push(container);
            }
            app.search_index.sync(&app.engine);

            let rows = app.engine_rows();
            assert_eq!(rows.len(), RESOURCE_SNAPSHOT_LIMIT);
            let omitted = format!("container:durable-{:03}", RESOURCE_SNAPSHOT_LIMIT + 16);
            assert!(rows.iter().all(|row| row.id() != omitted));
            assert_eq!(
                app.engine_row(&omitted).map(|row| row.id().to_owned()),
                Some(omitted.clone())
            );

            app.engine
                .containers
                .retain(|container| format!("container:{}", container.id) != omitted);
            app.search_index.sync(&app.engine);
            assert!(
                app.engine_row(&omitted).is_none(),
                "a stale palette address cannot recreate removed authority"
            );
            assert!(
                ui::search::SearchProvider::search(&app.search_index.provider(), &omitted,)
                    .is_empty()
            );
        }
    }

    #[test]
    fn palette_and_retained_dialogs_drive_container_operations() {
        use ui::headless::Driver;
        let mut hosted =
            ui::framework::testing::TestApp::<crate::resource::ContainersShell>::capture(Some(
                "palette",
            ))
            .unwrap();
        let expected = hosted
            .harness()
            .with_widget_mut::<ContainersApp, _>(|app| {
                let rows = app.palette.visible_entries();
                assert_eq!(rows.len(), app.engine.containers.len());
                assert!(app.engine.containers.iter().all(|container| {
                    rows.iter().any(|row| row.label.contains(&container.name))
                }));
                app.engine.containers[0].id.clone()
            })
            .unwrap();
        hosted.harness().key("Enter");
        hosted.settle();
        hosted
            .harness()
            .with_widget_mut::<ContainersApp, _>(|app| {
                assert!(!app.palette_open);
                assert_eq!(app.selected(), Some(expected.as_str()));
            })
            .unwrap();
        fn activate(driver: &mut Driver, name: &str) {
            let id = driver
                .a11y_nodes()
                .into_iter()
                .find(|(_, node)| node.label() == Some(name))
                .map(|(id, _)| id)
                .unwrap();
            driver
                .perform_a11y_action(id, &ui::a11y::Action::Activate)
                .unwrap();
        }
        let mut app = ContainersApp::sample();
        let reference = app.engine.images[0].reference();
        let images = app.engine.images.len();
        app.ask_confirm(Confirm {
            name: reference.clone(),
            target: ConfirmTarget::Image { reference },
        });
        let mut driver = Driver::for_root(ui::State::new(), app, (1000, 800), Theme::dark());
        activate(&mut driver, "Remove image");
        assert!(driver.root::<ContainersApp>().confirm.is_none());
        assert_eq!(
            driver.root::<ContainersApp>().engine.images.len(),
            images - 1
        );
        let app = driver.root_mut::<ContainersApp>();
        let reference = app.engine.images[0].reference();
        app.ask_confirm(Confirm {
            name: reference.clone(),
            target: ConfirmTarget::Image { reference },
        });
        driver.relayout();
        driver.key("Escape");
        assert!(driver.root::<ContainersApp>().confirm.is_none());
        assert_eq!(
            driver.root::<ContainersApp>().engine.images.len(),
            images - 1
        );
        let volumes = driver.root::<ContainersApp>().engine.volumes.len();
        driver.root_mut::<ContainersApp>().ask_prompt(Prompt {
            kind: PromptKind::NewVolume,
            value: String::new(),
            context: String::new(),
        });
        driver.relayout();
        driver.type_text("data");
        assert_eq!(
            driver
                .root::<ContainersApp>()
                .prompt_dialog
                .as_ref()
                .unwrap()
                .content()
                .value(),
            "data"
        );
        activate(&mut driver, "Create volume");
        assert!(driver.root::<ContainersApp>().prompt.is_none());
        assert_eq!(
            driver.root::<ContainersApp>().engine.volumes.len(),
            volumes + 1
        );
        assert!(
            driver
                .root::<ContainersApp>()
                .engine
                .volumes
                .iter()
                .any(|volume| volume.name == "data")
        );
    }
}
