//! Root widget for the container manager.
//!
//! [`ContainersApp`] owns the engine snapshot (containers, images, volumes
//! and distroboxes mirrored from rootless podman) and presents it in a
//! [`NavShell`](ui::widget::NavShell): the sections are the sidebar, the container list is the
//! overview and the chosen container is the content (see `crate::view`).
//! Dialogs (confirming a removal, asking for a name, the new-container form,
//! pull progress) are [`ModalDialog`]s mounted beside the shell.
//!
//! The new-container draft is the resource's record (see [`crate::draft`]),
//! edited a splice at a time through the framework's dispatcher; where the
//! user is (section, selection, detail tab) is this window's own, and the
//! engine projection is host data that stays in memory.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender};
use ui::widget::TaffyTreeExt;

use crate::resource::ContainersEffect;
#[cfg(test)]
use automerge::AutoCommit;
use automerge::ObjId;
use taffy::prelude::*;
use ui::a11y::{AccessNode, Role};
use ui::framework::{Dispatcher, LiveRow};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tilo_actions::ActionSpec;
use ui::tilo_resource::Effect as _;
use ui::time::Instant;
use ui::tokens::TextRole;
use ui::widget::toast::{Notice, ToastKind};
use ui::widget::{
    AppModel, Column, ModalDialog, ModelPath, ModelView, NavPane, PromptBody, Text, TextInput,
    Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::draft::CreateField;
use crate::model::{DetailTab, Engine, Section};
use crate::podman::{self, Action, StreamMsg};
use crate::resource::Containers;
use crate::view::{self, Intent, Intents};

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
    NewVolume,
    /// Tag an existing image (the source is in `Prompt::context`).
    TagImage,
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
    pub fn line(&self, line: impl Into<String>) -> Result<(), String> {
        self.events
            .send(Bg::Pull(StreamMsg::Line(line.into())))
            .map_err(|_| "controlled pull receiver disconnected".into())
    }

    pub fn done(&self) -> Result<(), String> {
        self.events
            .send(Bg::Pull(StreamMsg::Done))
            .map_err(|_| "controlled pull receiver disconnected".into())
    }

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
    pub fn post_run_snapshot(&self, engine: Engine) -> Result<(), String> {
        self.events
            .send(Bg::Snapshot(engine))
            .map_err(|_| "controlled create receiver disconnected".into())
    }
}

/// Where the user is: the section, the selected container and its detail
/// tab. This window's own: never in the document, never shared, never undone.
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
    reason = "independent flags: live engine vs. sample data, a past version, \
              an open form and a dirty structure"
)]
pub struct ContainersApp {
    /// The live (or sample) engine projection.
    pub(crate) engine: Engine,
    /// Bumped whenever `engine` changes.
    engine_rev: u64,
    /// Retained, revision-gated search projection.
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
    /// What the form's image field suggests: the engine's images.
    installed_images: crate::create_form::InstalledImages,
    pub(crate) create_open: bool,
    pub(crate) confirm: Option<Confirm>,
    confirm_dialog: Option<ModalDialog<Text>>,
    pub(crate) prompt: Option<Prompt>,
    prompt_dialog: Option<ModalDialog<PromptBody>>,
    /// Live pull progress, if a pull is running or just finished.
    pub(crate) pull: Option<PullProgress>,
    pull_dialog: Option<ModalDialog<Column>>,
    pull_dirty: bool,
    /// The engine effects the window's controls asked the host for, for
    /// tests of a bare widget.
    #[cfg(test)]
    pub(crate) requested: Vec<(String, ContainersEffect)>,
    /// What a search hit or an agent's `open` named, shown with the next
    /// frame.
    opened: Rc<RefCell<Vec<String>>>,
    pub(crate) toast: Notice,
    /// Background refresh plumbing. `None` in headless/shots mode.
    bg_rx: Option<Receiver<Bg>>,
    bg_tx: Option<Sender<Bg>>,
    #[cfg(test)]
    controlled_runtime: Option<Sender<ControlledRuntimeRequest>>,
    #[cfg(test)]
    controlled_immediate_results: Option<Receiver<Result<(), String>>>,
    pub(crate) last_refresh: Instant,
    pub(crate) live: bool,
    /// Showing a past version in History: nothing here is live.
    past: bool,
    /// The structure changed (a dialog opened or closed, the shell was
    /// rebuilt): the host lays the tree out again.
    structure_dirty: bool,
    /// Where the user is: the section, the selected container, its tab.
    pub(crate) view: ViewState,
    /// Where edits of the draft go, once the framework attached the surface.
    dispatcher: Option<Dispatcher<Containers>>,
}

impl Default for ContainersApp {
    fn default() -> Self {
        Self::new()
    }
}

/// A press of Escape, whatever modifiers are held.
fn is_escape(event: &WindowEvent) -> bool {
    matches!(
        event,
        WindowEvent::KeyboardInput { event: key, .. }
            if key.state == ElementState::Pressed
                && key.logical_key == Key::Named(NamedKey::Escape)
    )
}

#[cfg(test)]
fn controlled_input() -> TextInput {
    TextInput::local_with_language("en_US")
        .with_spellcheck(false)
        .with_autocorrect(false)
}

impl ContainersApp {
    /// The manager with no engine yet: the framework's
    /// [`attach`](ui::framework::ResourceSurface::attach) connects it to
    /// podman in a live window, shows a capture's engine, or leaves it empty
    /// for a past version.
    pub fn new() -> Self {
        Self::with_engine(Engine::default(), TextInput::local)
    }

    /// The manager over the bundled sample engine, offline: lifecycle
    /// controls change the sample instead of reaching podman.
    pub fn sample() -> Self {
        Self::with_engine(crate::model::sample_engine(), TextInput::local)
    }

    fn with_engine(engine: Engine, make_input: fn() -> TextInput) -> Self {
        let intents = Intents::default();
        let installed_images = crate::create_form::InstalledImages::new(
            crate::create_form::image_references(&engine).into(),
        );
        let create = Self::create_dialog(&intents, make_input, &installed_images);
        let parts = view::Parts {
            model: ui::LocalState::new(view::Model::default()),
            pane: ui::LocalState::new(NavPane::Overview),
            intents: intents.clone(),
            offsets: view::Offsets::default(),
            sort: ui::LocalState::new(None),
            detail_title: ui::LocalState::new(String::new()),
        };
        let window = view::window(&parts);
        Self {
            engine,
            engine_rev: 0,
            rect: Rect::ZERO,
            window,
            parts,
            published: None,
            intents,
            create,
            installed_images,
            create_open: false,
            confirm: None,
            confirm_dialog: None,
            prompt: None,
            prompt_dialog: None,
            pull: None,
            pull_dialog: None,
            pull_dirty: false,
            #[cfg(test)]
            requested: Vec::new(),
            opened: Rc::default(),
            toast: Notice::default(),
            bg_rx: None,
            bg_tx: None,
            #[cfg(test)]
            controlled_runtime: None,
            #[cfg(test)]
            controlled_immediate_results: None,
            last_refresh: ui::time::now(),
            live: false,
            past: false,
            structure_dirty: true,
            view: ViewState::default(),
            dispatcher: None,
        }
    }

    fn create_dialog(
        intents: &Intents,
        make_input: fn() -> TextInput,
        images: &crate::create_form::InstalledImages,
    ) -> ModalDialog<Column> {
        let submit = intents.clone();
        let cancel = intents.clone();
        let dismiss = intents.clone();
        ModalDialog::new(
            "New container",
            crate::create_form::form(make_input, images),
        )
        .with_test_id("containers-create-dialog")
        .primary_action("Create container", move || {
            submit.push(Intent::SubmitCreate);
        })
        .secondary_action("Cancel", move || cancel.push(Intent::CloseCreate))
        .on_dismiss(move || dismiss.push(Intent::CloseCreate))
    }

    /// Talk to rootless podman: probe it now, then keep the snapshot fresh
    /// in the background and run lifecycle actions against it.
    pub(crate) fn connect(&mut self) {
        let (tx, rx) = std::sync::mpsc::channel();
        self.bg_tx = Some(tx);
        self.bg_rx = Some(rx);
        self.live = true;
        self.show_engine(podman::probe().engine);
    }

    /// Show `engine` in place of the current snapshot.
    pub(crate) fn show_engine(&mut self, engine: Engine) {
        self.engine = engine;
        self.engine_changed();
    }

    /// Show a past version: its draft, and no engine, which is this device's
    /// live state and has no history.
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

    /// Edit the draft through `dispatcher` from now on.
    pub(crate) fn attach_dispatcher(&mut self, dispatcher: Dispatcher<Containers>) {
        crate::create_form::for_each_input(self.create.content_mut(), &mut |input| {
            input.attach(dispatcher.clone());
        });
        self.dispatcher = Some(dispatcher);
    }

    /// The draft as the document holds it now. A past version with a draft
    /// shows the form, which is all it holds.
    pub(crate) fn present_draft(&mut self, draft: &crate::draft::CreateDraft) {
        if self.past {
            self.set_create_open(!draft.is_empty());
        }
    }

    /// Show `section`, with nothing selected in it.
    pub(crate) fn show_section(&mut self, section: Section) {
        self.view.section = section;
        self.view.selected = None;
        self.parts.pane.set(NavPane::Overview);
        self.parts.offsets.list.set((0.0, 0.0));
        self.publish_model();
    }

    /// Select the container `id` (`None` clears the selection), bringing
    /// its detail to the front of a narrow window.
    pub(crate) fn select_container(&mut self, id: Option<String>) {
        if id.is_some() {
            self.view.section = Section::Containers;
        }
        self.parts.pane.set(if id.is_some() {
            NavPane::Content
        } else {
            NavPane::Overview
        });
        if self.view.selected != id {
            self.view.selected = id;
            self.parts.offsets.detail.set((0.0, 0.0));
        }
        self.publish_model();
    }

    pub(crate) fn show_tab(&mut self, tab: DetailTab) {
        self.view.tab = tab;
        self.parts.offsets.detail.set((0.0, 0.0));
        self.publish_model();
    }

    /// Open the new-container form on its image field.
    pub(crate) fn open_create(&mut self) {
        self.set_create_open(true);
    }

    /// Empty the draft, once a container was created from it.
    fn clear_create(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let Some(dispatcher) = &self.dispatcher else {
            return;
        };
        if let Err(error) = dispatcher.submit_now(doc, Containers::CLEAR_CREATE.with(())) {
            self.show_toast(format!("The draft was not cleared: {error}"), true);
        }
    }

    /// Ask the host for `effect` as the live row `row` offers it: it
    /// checks permission, availability and confirmation, records the effect
    /// in activity history, and only then does the surface run it.
    #[cfg_attr(
        not(test),
        allow(
            clippy::needless_pass_by_ref_mut,
            clippy::needless_pass_by_value,
            reason = "a test also keeps what was asked for"
        )
    )]
    fn request(&mut self, row: String, effect: ContainersEffect) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.effect_on(&row, &effect);
        }
        #[cfg(test)]
        self.requested.push((row, effect));
    }

    /// What a search hit or an agent's `open` runs: show the row its
    /// anchor names, as choosing it does.
    pub(crate) fn opener(&self) -> impl FnMut(&str) -> Result<(), &'static str> + 'static {
        let opened = Rc::clone(&self.opened);
        move |anchor| {
            if !["container:", "image:", "volume:", "distrobox:"]
                .iter()
                .any(|prefix| anchor.starts_with(prefix))
            {
                return Err("not a container, image, volume or distrobox");
            }
            opened.borrow_mut().push(anchor.to_owned());
            Ok(())
        }
    }

    /// Show what the openers named since the last frame.
    fn show_opened(&mut self) {
        let opened = std::mem::take(&mut *self.opened.borrow_mut());
        for anchor in opened {
            if let Some(id) = anchor
                .strip_prefix("container:")
                .and_then(|rest| rest.strip_suffix(":logs"))
            {
                self.select_container(Some(id.to_owned()));
                self.show_tab(DetailTab::Logs);
            } else if let Some(id) = anchor.strip_prefix("container:") {
                self.select_container(Some(id.to_owned()));
            } else if anchor.starts_with("image:") {
                self.show_section(Section::Images);
            } else if anchor.starts_with("volume:") {
                self.show_section(Section::Volumes);
            } else if anchor.starts_with("distrobox:") {
                self.show_section(Section::Distrobox);
            }
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

    /// The engine changed: the search index and the shell follow it.
    pub(crate) fn engine_changed(&mut self) {
        self.engine_rev = self.engine_rev.wrapping_add(1);
        *self.installed_images.borrow_mut() = crate::create_form::image_references(&self.engine);
    }

    pub(crate) fn show_toast(&mut self, message: impl Into<String>, error: bool) {
        let kind = if error {
            ToastKind::Error
        } else {
            ToastKind::Success
        };
        self.toast.show(message, kind);
    }

    /// Whether lifecycle affordances are live. The deterministic sample
    /// stays interactive for demos and tests; a disconnected engine does not
    /// advertise controls that can only fail.
    pub(crate) fn engine_controls_available(&self) -> bool {
        !self.past && (self.engine.live || !self.live)
    }

    /// Whether the sidebar reports the engine as reachable. The sample shown
    /// by demos and screenshots stands in for a connected engine.
    fn connected(&self) -> bool {
        !self.past && (self.engine.live || !self.live)
    }

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
    pub(crate) fn drain_bg(&mut self) {
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
                    newc.logs_error = old.logs_error;
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
    pub(crate) fn hydrate_selection(&mut self) {
        let Some(sel) = self.view.selected.clone() else {
            return;
        };
        let needs = self
            .engine
            .containers
            .iter()
            .find(|c| c.id == sel)
            .is_some_and(|c| c.logs.is_empty() && c.logs_error.is_none() && c.env.is_empty());
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
            match logs {
                Ok(logs) => {
                    c.logs = logs;
                    c.logs_error = None;
                }
                Err(error) => {
                    c.logs_error = Some(error);
                }
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
            let container = id.to_owned();
            let effect = match action {
                Action::Start => ContainersEffect::Start { container },
                Action::Stop => ContainersEffect::Stop { container },
                Action::Restart => ContainersEffect::Restart { container },
                Action::Pause => ContainersEffect::Pause { container },
                Action::Unpause => ContainersEffect::Unpause { container },
                Action::Remove => ContainersEffect::RemoveContainer { container },
            };
            self.request(format!("container:{id}"), effect);
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

    /// Remove an image (post-confirmation). Force-removes so an unused tag
    /// goes even if a stopped container references it.
    pub(crate) fn remove_image(&mut self, reference: &str) {
        if self.engine.live {
            self.request(
                format!("image:{reference}"),
                ContainersEffect::RemoveImage {
                    image: reference.to_owned(),
                },
            );
        } else if !self.live {
            self.engine.images.retain(|i| i.reference() != reference);
            self.engine_changed();
        } else {
            self.show_toast("Podman isn't running", true);
        }
    }

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
                    Ok(()) => {}
                    Err(error) => {
                        self.show_toast(format!("Couldn't create the volume: {error}"), true);
                    }
                }
                self.spawn_refresh();
                return;
            }
            match podman::create_volume(name) {
                Ok(()) => {}
                Err(e) => self.show_toast(format!("Couldn't create the volume: {e}"), true),
            }
            self.spawn_refresh();
        } else {
            self.engine.volumes.push(crate::model::Volume {
                name: name.to_string(),
                driver: "local".into(),
                mountpoint: format!(
                    "/var/home/dev/.local/share/containers/storage/volumes/{name}/_data"
                ),
                created: "Just now".into(),
                in_use_by: 0,
            });
            self.engine.volumes.sort_by(|a, b| a.name.cmp(&b.name));
            self.engine_changed();
        }
    }

    /// Remove a named volume (post-confirmation).
    pub(crate) fn remove_volume(&mut self, name: &str) {
        if self.engine.live {
            self.request(
                format!("volume:{name}"),
                ContainersEffect::RemoveVolume {
                    volume: name.to_owned(),
                },
            );
        } else if !self.live {
            self.engine.volumes.retain(|v| v.name != name);
            self.engine_changed();
        } else {
            self.show_toast("Podman isn't running", true);
        }
    }

    /// Open a real interactive shell in a running container: the Tilo
    /// terminal running `podman exec -it <id> /bin/sh`.
    pub(crate) fn attach_shell(&mut self, id: &str) {
        if self.engine.live {
            self.request(
                format!("container:{id}"),
                ContainersEffect::Shell {
                    container: id.to_owned(),
                },
            );
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
            self.request(
                format!("distrobox:{name}"),
                ContainersEffect::EnterDistrobox {
                    distrobox: name.to_owned(),
                },
            );
        } else if !self.live {
            self.show_toast(format!("Entered {name}"), false);
        } else {
            self.show_toast("Podman isn't running", true);
        }
    }

    pub(crate) fn submit_create(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let draft = <Containers as ui::tilo_resource::Resource>::read(doc.doc())
            .map(|record| record.draft)
            .unwrap_or_default();
        let Some(args) = draft.run_args() else {
            self.show_toast("Choose an image to create the container from", true);
            return;
        };
        let image = draft.get(CreateField::Image).trim().to_owned();
        self.clear_create(doc);
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

    /// Apply the pending destructive confirm.
    fn accept_confirm(&mut self) {
        if let Some(confirm) = self.confirm.take() {
            match confirm.target {
                ConfirmTarget::Container { id, action } => {
                    self.apply_action(action, &id);
                    if self.view.selected.as_deref() == Some(id.as_str())
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

    /// Present the authored screenshot engine on an existing host, with no
    /// device polling. The framework's live action executor stays installed;
    /// the offline engine refuses actual Podman lifecycle effects.
    pub fn stage_sample_engine(&mut self) {
        self.live = false;
        self.bg_rx = None;
        self.bg_tx = None;
        self.show_engine(crate::model::sample_engine());
    }

    /// Stage a screenshot scenario (`None` for a plain `--shots`): the
    /// section, selection and tab it shows, and the dialog it has open. The
    /// engine and the draft are the scenario's already.
    pub fn stage_scene(&mut self, scenario: Option<&str>) {
        let scene = scenario.unwrap_or_default();
        match scene {
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
            // The engine did not answer a live window: the pages say so.
            "empty" => self.live = true,
            "create" => self.set_create_open(true),
            _ => {}
        }
        let section = match scene {
            "images" | "pull" => Some(Section::Images),
            "volumes" | "prompt" => Some(Section::Volumes),
            "distrobox" => Some(Section::Distrobox),
            _ => None,
        };
        if let Some(section) = section {
            self.show_section(section);
        } else if self.view.selected.is_none()
            && let Some(first) = view::listed_containers(&self.engine).first().copied()
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
    }

    /// Apply what the window's controls asked for.
    pub(crate) fn drain_intents(&mut self, doc: &mut ui::widget::Edits<'_>) {
        self.show_opened();
        let intents = self.intents.take();
        if intents.is_empty() {
            return;
        }
        for intent in intents {
            self.apply_intent(doc, intent);
        }
        self.publish_model();
    }

    fn apply_intent(&mut self, doc: &mut ui::widget::Edits<'_>, intent: Intent) {
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
            Intent::SubmitCreate => self.submit_create(doc),
            Intent::CloseCreate => self.set_create_open(false),
            Intent::ConfirmAccept => self.accept_confirm(),
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

    /// Hand the window what it shows, when that changed.
    fn publish_model(&mut self) {
        // A container that left the engine is no longer shown as chosen.
        let mut view = self.view.clone();
        view.selected = view.selected.filter(|id| self.engine.find(id).is_some());
        let published = (self.engine_rev, view.clone());
        if self.published.as_ref() == Some(&published) {
            return;
        }
        self.published = Some(published);
        let title = view
            .selected
            .as_deref()
            .and_then(|id| self.engine.find(id))
            .map(|container| container.name.clone())
            .unwrap_or_default();
        if self.parts.detail_title.with(|shown| *shown != title) {
            self.parts.detail_title.set(title);
        }
        self.parts.model.set(
            view::Model {
                engine: Rc::new(self.engine.clone()),
                view,
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
}

/// How a finished lifecycle verb reads in the window bar's notice.
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
    ui::provides!(dyn ui::widget::controls::ActionControls);

    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, path: &ObjId) {
        for child in self.children_mut() {
            child.settle(edits, path);
        }
        self.drain_intents(edits);
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        {
            let (_doc, _) = ui::doc::document_ref(state, path);
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
            if self.pull_dirty {
                self.rebuild_pull();
            }
            self.toast.expire();
            // The result says so in the window's bar while it lasts.
            self.toast.publish();
            self.publish_model();
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
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        {
            let (_doc, _) = ui::doc::document_ref(state, path);
            if self.pull_dirty {
                self.rebuild_pull();
            }
        }
        let window = self.window.update(state, path);
        window || self.structure_dirty
    }

    fn needs_idle_sync(&self) -> bool {
        self.live
            || self
                .pull
                .as_ref()
                .is_some_and(|p| !p.done && p.failed.is_none())
    }

    fn needs_animation_frame(&self) -> bool {
        let mut needed = false;
        self.for_each_child(&mut |child| {
            needed = needed || child.needs_animation_frame();
        });
        needed
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        let mut earliest = self.toast.deadline();
        self.for_each_child(&mut |child| {
            if let Some(deadline) = child.next_wake_deadline() {
                earliest = Some(earliest.map_or(deadline, |old: Instant| old.min(deadline)));
            }
        });
        earliest
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
        let style = Style {
            display: Display::Flex,
            size: Size::from_percent(1.0_f32, 1.0_f32),
            ..Default::default()
        };
        taffy.widget_node(style, &children, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
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
        self.drain_intents(&mut ctx.edits());
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
        Some(
            AccessNode::new(Role::Window)
                .with_name("Containers")
                .with_description(&self.engine.connection)
                .with_bounds(self.rect),
        )
    }

    fn set_motion_reduced(&mut self, reduced: bool) {
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

impl ui::widget::controls::ActionControls for ContainersApp {
    fn install_control_custody(
        &mut self,
        domain: &ui::ControlDomain,
        attachment: ui::control_custody::CustodyAttachment,
    ) {
        for child in self.children_mut() {
            ui::widget::controls::install_control_custody(child, domain, attachment);
        }
    }

    fn take_control_invocation(&mut self) -> Option<ui::tilo_actions::ControlInvocation> {
        for child in self.children_mut() {
            if let Some(invocation) = ui::widget::controls::take_control_invocation(child) {
                return Some(invocation);
            }
        }
        None
    }
}

// =====================================================================
// Test roots
// =====================================================================

impl ContainersApp {
    /// A manager whose podman calls are requests a test answers.
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

    /// A live engine with one running container, its detail fetched when
    /// `hydrated`.
    #[cfg(test)]
    pub(crate) fn controlled_engine(connection: &str, hydrated: bool) -> Engine {
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
                logs_error: None,
                logs: if hydrated {
                    vec!["controlled detail".into()]
                } else {
                    Vec::new()
                },
            }],
            ..Engine::default()
        }
    }

    pub fn is_create_open(&self) -> bool {
        self.create_open
    }

    /// The section shown.
    pub fn section(&self) -> Section {
        self.view.section
    }

    /// The selected container's id.
    pub fn selected(&self) -> Option<&str> {
        self.view.selected.as_deref()
    }

    pub fn tab(&self) -> DetailTab {
        self.view.tab
    }
}

const RESOURCE_SNAPSHOT_LIMIT: usize = 200;

/// `effect`, as a row offers it: an engine effect only while the engine
/// answers.
fn offered(effect: &ContainersEffect, engine_live: bool, offline: &str) -> ActionSpec {
    let spec = effect.bound();
    if engine_live || effect.engine_operation().is_none() {
        spec
    } else {
        spec.unavailable("engine_offline", offline)
    }
}

const ENGINE_OFFLINE: &str = "Container engine actions are unavailable in demo mode";

pub(crate) fn container_row(container: &crate::model::Container, engine_live: bool) -> LiveRow {
    let id = || container.id.clone();
    let mut row = LiveRow::new(
        format!("container:{}", container.id),
        "containers.container",
        container.name.clone(),
    )
    .operation({
        let mut open = ContainersEffect::SelectContainer { id: Some(id()) }.bound();
        open.label = format!("Open {}", container.name);
        open
    });
    let effects = match container.status {
        crate::model::ContainerStatus::Running | crate::model::ContainerStatus::Restarting => vec![
            ContainersEffect::Stop { container: id() },
            ContainersEffect::Restart { container: id() },
            ContainersEffect::Pause { container: id() },
            ContainersEffect::Shell { container: id() },
        ],
        crate::model::ContainerStatus::Paused => vec![
            ContainersEffect::Unpause { container: id() },
            ContainersEffect::Stop { container: id() },
            ContainersEffect::RemoveContainer { container: id() },
            ContainersEffect::Shell { container: id() },
        ],
        _ => vec![
            ContainersEffect::Start { container: id() },
            ContainersEffect::RemoveContainer { container: id() },
        ],
    };
    for effect in &effects {
        row = row.operation(offered(effect, engine_live, ENGINE_OFFLINE));
    }
    row.row(logs_row(container))
}

fn logs_row(container: &crate::model::Container) -> LiveRow {
    LiveRow::new(
        format!("container:{}:logs", container.id),
        "containers.logs",
        format!("{} logs", container.name),
    )
    .effect(&ContainersEffect::OpenLogs {
        container: container.id.clone(),
    })
}

fn image_row(image: &crate::model::Image, engine_live: bool) -> LiveRow {
    let reference = image.reference();
    LiveRow::new(
        format!("image:{reference}"),
        "containers.image",
        reference.clone(),
    )
    .effect(&ContainersEffect::ShowImage {
        image: reference.clone(),
    })
    .operation(offered(
        &ContainersEffect::RemoveImage { image: reference },
        engine_live,
        ENGINE_OFFLINE,
    ))
}

fn volume_row(volume: &crate::model::Volume, engine_live: bool) -> LiveRow {
    LiveRow::new(
        format!("volume:{}", volume.name),
        "containers.volume",
        volume.name.clone(),
    )
    .effect(&ContainersEffect::ShowVolume {
        volume: volume.name.clone(),
    })
    .operation(offered(
        &ContainersEffect::RemoveVolume {
            volume: volume.name.clone(),
        },
        engine_live,
        ENGINE_OFFLINE,
    ))
}

fn distrobox_row(distrobox: &crate::model::DistroBox, engine_live: bool) -> LiveRow {
    LiveRow::new(
        format!("distrobox:{}", distrobox.name),
        "containers.distrobox",
        distrobox.name.clone(),
    )
    .effect(&ContainersEffect::ShowDistrobox {
        distrobox: distrobox.name.clone(),
    })
    .operation(offered(
        &ContainersEffect::EnterDistrobox {
            distrobox: distrobox.name.clone(),
        },
        engine_live,
        "Distrobox actions are unavailable in demo mode",
    ))
}

impl ContainersApp {
    /// Ask the engine to `verb` the `kind` of thing named `target`.
    pub(crate) fn run_engine_operation(
        &mut self,
        kind: &str,
        verb: &str,
        target: &str,
    ) -> Result<(), String> {
        if !self.engine.live {
            return Err("container engine actions are unavailable while Podman is offline".into());
        }
        #[cfg(test)]
        if let Some(result) = self.controlled_immediate(
            format!("{kind}.{verb}"),
            target.to_string(),
            vec![verb.to_string(), target.to_string()],
        ) {
            result?;
            self.spawn_refresh();
            self.show_toast(format!("{verb} accepted for {target}"), false);
            return Ok(());
        }
        match (kind, verb) {
            ("containers.container", "start") => podman::run_action(Action::Start, target)?,
            ("containers.container", "stop") => podman::run_action(Action::Stop, target)?,
            ("containers.container", "restart") => podman::run_action(Action::Restart, target)?,
            ("containers.container", "pause") => podman::run_action(Action::Pause, target)?,
            ("containers.container", "unpause") => podman::run_action(Action::Unpause, target)?,
            ("containers.container", "remove") => podman::run_action(Action::Remove, target)?,
            ("containers.container", "shell") => {
                crate::terminal::open(crate::terminal::exec_attach_intent(target))?;
            }
            ("containers.image", "remove") => podman::remove_image(target, true)?,
            ("containers.volume", "remove") => podman::remove_volume(target, false)?,
            ("containers.distrobox", "enter") => {
                crate::terminal::open(crate::terminal::distrobox_enter_intent(target))?;
            }
            _ => return Err(format!("`{verb}` is not a container engine operation")),
        }
        self.spawn_refresh();
        self.show_toast(format!("{verb} accepted for {target}"), false);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sample engine, staged as a plain `--shots` opens it.
    fn staged() -> ContainersApp {
        let mut app = ContainersApp::sample();
        app.stage_scene(None);
        app
    }

    fn driver(app: ContainersApp) -> ui::headless::FrameHarness {
        ui::headless::FrameHarness::for_root(ui::State::new(), app, (1280, 800), ui::Theme::dark())
    }

    fn app_of(driver: &mut ui::headless::FrameHarness) -> &mut ContainersApp {
        driver.root_mut::<ContainersApp>()
    }

    /// A finished verb says so in the window's bar, not over the list.
    #[test]
    fn a_result_is_the_window_bars_notice() {
        let mut d = driver(staged());
        app_of(&mut d).show_toast("Stopped web", false);
        d.relayout();
        let notice = ui::window_toolbar::notice().expect("the bar shows the result");
        assert_eq!(notice.text, "Stopped web");
        assert_eq!(notice.kind, ToastKind::Success);
        app_of(&mut d).show_toast("Podman isn't running", true);
        d.relayout();
        assert_eq!(
            ui::window_toolbar::notice().map(|notice| (notice.text, notice.kind)),
            Some(("Podman isn't running".to_owned(), ToastKind::Error)),
            "a newer result replaces the older one"
        );
    }

    #[test]
    fn keys_and_buttons_move_between_sections_and_open_dialogs() {
        let mut d = driver(staged());
        let first = view::listed_containers(&app_of(&mut d).engine)[0]
            .id
            .clone();
        assert_eq!(
            app_of(&mut d).selected(),
            Some(first.as_str()),
            "a demo opens on the first container"
        );

        d.key("2");
        assert_eq!(app_of(&mut d).section(), Section::Images);
        d.click_named("images-pull").unwrap();
        assert_eq!(
            app_of(&mut d).prompt.as_ref().map(|p| p.kind),
            Some(PromptKind::PullImage)
        );
        d.key("Escape");
        assert!(app_of(&mut d).prompt.is_none(), "Escape cancels the prompt");

        d.key("3");
        assert_eq!(app_of(&mut d).section(), Section::Volumes);
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
        assert_eq!(app_of(&mut d).section(), Section::Containers);
        app_of(&mut d).select_container(Some(first));
        d.relayout();
        d.key("Escape");
        assert!(
            app_of(&mut d).selected().is_none(),
            "Escape returns from the detail to the list"
        );
    }

    #[test]
    fn the_detail_closes_from_its_header_control() {
        let mut d = driver(staged());
        assert!(app_of(&mut d).selected().is_some());
        d.click_named("detail-close").unwrap();
        assert!(
            app_of(&mut d).selected().is_none(),
            "the close control clears the chosen container"
        );
        d.relayout();
        assert!(
            !d.targets().contains_key("detail-close"),
            "with nothing chosen there is nothing to close"
        );
    }

    #[test]
    fn the_section_and_its_commands_are_in_the_window_bar() {
        let mut d = driver(staged());
        d.present_in_window_bar();
        d.relayout();
        let bar = ui::window_toolbar::declared().expect("the shell declares the window's slots");
        let app = app_of(&mut d);
        let chosen = app
            .selected()
            .and_then(|id| app.engine.find(id))
            .map(|container| container.name.clone())
            .expect("the staged window shows a container");
        assert_eq!(
            bar.location
                .as_ref()
                .map(|location| location.label.as_str()),
            Some(chosen.as_str()),
            "the chosen container names the window"
        );
        let names: Vec<&str> = bar
            .actions
            .iter()
            .map(|action| action.name.as_str())
            .collect();
        assert!(
            names.contains(&"Remove"),
            "the container's verbs are bar actions: {names:?}"
        );
        assert!(
            !d.targets()
                .keys()
                .any(|id| id.starts_with("detail-remove-")),
            "the detail draws no verbs of its own"
        );
        assert!(
            !bar.actions
                .iter()
                .any(|action| action.name.contains("Search")),
            "the bar's own search is the only one: {:?}",
            bar.actions
        );
        let close = bar
            .actions
            .iter()
            .find(|action| action.name == "Close details")
            .expect("closing the detail is a bar action");
        assert_eq!(
            close.icon, "panel-right",
            "closing the detail shows the pane it closes, not a cross beside the search"
        );
        let close = close.id.clone();
        assert!(
            !d.targets().contains_key("detail-close"),
            "the window draws no header row of its own"
        );
        d.press_window_slot(ui::window_toolbar::ToolbarEvent::Action { id: close });
        // The shell presses the header's button in the frame's settle pass;
        // the window then dispatches what the button invoked.
        d.idle();
        d.relayout();
        assert!(
            app_of(&mut d).selected().is_none(),
            "the bar closed the detail"
        );
        let bar = ui::window_toolbar::declared().expect("the window's slots");
        assert_eq!(
            bar.location.map(|location| location.label).as_deref(),
            Some("Containers"),
            "with nothing chosen the section names the window"
        );

        d.key("2");
        d.relayout();
        let bar = ui::window_toolbar::declared().expect("the window's slots");
        assert_eq!(
            bar.location.map(|location| location.label).as_deref(),
            Some("Images")
        );
    }

    #[test]
    fn escape_closes_the_detail_in_a_narrow_window_too() {
        let mut d = ui::headless::FrameHarness::for_root(
            ui::State::new(),
            staged(),
            (390, 844),
            ui::Theme::dark(),
        );
        assert!(app_of(&mut d).selected().is_some());
        assert_eq!(app_of(&mut d).parts.pane.get(), NavPane::Content);
        d.key("Escape");
        assert!(
            app_of(&mut d).selected().is_none(),
            "Escape clears the selection as it does in a wide window"
        );
        assert_eq!(
            app_of(&mut d).parts.pane.get(),
            NavPane::Overview,
            "and the list is in front again"
        );
    }

    #[test]
    fn log_lines_read_their_severity() {
        use ui::widget::Tone;
        assert_eq!(
            view::line_severity("2026-06-30T13:05:01Z WARN  api::auth: 1 failed login"),
            Tone::Warning
        );
        assert_eq!(
            view::line_severity("WARNING: Console endpoint is listening on a dynamic port"),
            Tone::Warning
        );
        assert_eq!(
            view::line_severity("logger=sqlstore level=error msg=\"database locked\""),
            Tone::Danger
        );
        assert_eq!(
            view::line_severity("2026/06/30 09:14:01 [error] 1#1: open() failed"),
            Tone::Danger
        );
        assert_eq!(
            view::line_severity("api::http: GET /api/errors 200 12ms"),
            Tone::Neutral,
            "a word that only contains a level is not one"
        );
    }

    #[test]
    fn removing_asks_first_and_then_runs_once() {
        let mut d = driver(staged());
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
        // The dialog opens on its safe answer: Enter keeps the container.
        d.key("Enter");
        assert!(app_of(&mut d).confirm.is_none());
        assert!(
            app_of(&mut d).engine.find(&first.id).is_some(),
            "Enter on the opened confirmation cancels"
        );

        d.click_named(&format!("detail-remove-{}", first.id))
            .unwrap();
        let remove = d
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| {
                node.label() == Some("Remove container")
                    && node.role() == ui::accesskit::Role::Button
            })
            .map(|(id, _)| id)
            .expect("the confirmation's Remove container action");
        d.perform_a11y_action(remove, &ui::a11y::Action::Activate)
            .unwrap();
        assert!(app_of(&mut d).confirm.is_none());
        assert!(
            app_of(&mut d).engine.find(&first.id).is_none(),
            "the confirmed removal ran"
        );
        assert!(
            app_of(&mut d).selected().is_none(),
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

    /// Moving around is the window's own: switching sections leaves the
    /// selection behind, and a selection opens the containers section.
    #[test]
    fn where_the_window_is_stays_local() {
        let mut app = staged();
        let id = app.selected().map(ToOwned::to_owned).unwrap();
        app.show_section(Section::Images);
        assert_eq!(app.section(), Section::Images);
        assert!(app.selected().is_none());
        app.select_container(Some(id.clone()));
        assert_eq!(app.section(), Section::Containers);
        assert_eq!(app.selected(), Some(id.as_str()));
        app.show_tab(DetailTab::Inspect);
        assert_eq!(app.tab(), DetailTab::Inspect);
    }

    #[test]
    fn lifecycle_effects_and_resources_are_exported_and_bounded() {
        // --- phase: lifecycle_control_emits_the_exported_effect_instead_of_running_podman_inline ---
        {
            let mut app = ContainersApp::sample();
            app.engine.live = true;
            let id = app.engine.containers[0].id.clone();
            app.apply_action(Action::Stop, &id);
            assert_eq!(
                app.requested.last(),
                Some(&(
                    format!("container:{id}"),
                    ContainersEffect::Stop {
                        container: id.clone()
                    }
                )),
                "the host is asked for the exported effect on the container's row"
            );
            assert_eq!(
                app.engine.find(&id).map(|container| container.status),
                Some(crate::model::ContainerStatus::Running),
                "the control does not mutate the engine before host authorization"
            );
        }

        // --- phase: resource_snapshot_is_bounded_and_omitted_identity_resolves_lazily ---
        {
            let limit = RESOURCE_SNAPSHOT_LIMIT;
            let mut app = ContainersApp::sample();
            let template = app.engine.containers[0].clone();
            app.engine.containers.clear();
            for index in 0..(limit + 17) {
                let mut container = template.clone();
                container.id = format!("durable-{index:03}");
                container.name = format!("Container {index:03}");
                app.engine.containers.push(container);
            }
            app.engine_changed();

            let rows = app.engine_rows();
            assert_eq!(rows.len(), limit);
            let omitted = format!("container:durable-{:03}", limit + 16);
            assert!(rows.iter().all(|row| row.id() != omitted));
            assert_eq!(
                app.engine_row(&omitted).map(|row| row.id().to_owned()),
                Some(omitted.clone())
            );

            app.engine
                .containers
                .retain(|container| format!("container:{}", container.id) != omitted);
            app.engine_changed();
            assert!(
                app.engine_row(&omitted).is_none(),
                "a stale search address cannot recreate removed authority"
            );
            assert!(
                crate::search::engine_entries(&app.engine)
                    .iter()
                    .all(|entry| {
                        entry
                            .action
                            .as_ref()
                            .is_none_or(|action| action.resource != omitted)
                    }),
                "removed containers are absent from canonical search entries"
            );
        }
    }

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
        let terminal = app.toast.deadline();
        app.drain_bg();
        assert_eq!(app.toast.deadline(), terminal);
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
        let doc = AutoCommit::new();
        app.sync(&doc, &automerge::ROOT);
        assert!(
            requests.try_recv().is_err(),
            "controlled fixtures own refresh; wall-clock must not enqueue Refresh"
        );
    }

    #[test]
    fn controlled_create_runs_the_draft_and_drains_one_snapshot() {
        let (mut app, requests, _results) = ContainersApp::controlled_runtime_root();
        let mut record = ui::tilo_resource::Replica::<Containers>::new();
        for (field, text) in [
            (CreateField::Name, "web"),
            (CreateField::Image, "nginx:1"),
            (CreateField::Ports, "8080:80"),
        ] {
            record
                .apply(ui::tilo_resource::Invoke::edit(&field.path().replace(text)).unwrap())
                .unwrap();
        }
        let mut doc = record.doc().clone();
        app.set_create_open(true);
        app.submit_create(&mut ui::widget::Edits::lend(&mut doc));
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
    fn a_lifecycle_control_asks_the_host_for_its_exact_target() {
        let (mut app, requests, _results) = ContainersApp::controlled_runtime_root();
        app.apply_action(Action::Restart, "retained-target");
        let doc = AutoCommit::new();
        app.sync(&doc, &automerge::ROOT);
        app.sync(&doc, &automerge::ROOT);
        assert_eq!(
            app.requested,
            [(
                "container:retained-target".to_owned(),
                ContainersEffect::Restart {
                    container: "retained-target".into()
                }
            )],
            "one effect on exactly that container's row"
        );
        assert!(
            requests.try_recv().is_err(),
            "nothing reaches the engine before the host answers"
        );
    }

    /// A controlled live engine selects and hydrates its one container.
    #[test]
    fn controlled_selection_asks_for_its_detail() {
        let (mut app, requests, _results) = ContainersApp::controlled_runtime_root();
        app.show_engine(ContainersApp::controlled_engine("controlled Podman", false));
        app.select_container(Some("armed-container".into()));
        app.hydrate_selection();
        assert!(matches!(
            requests.recv().unwrap(),
            ControlledRuntimeRequest::SelectedDetails { target } if target == "armed-container"
        ));
        app.show_engine(ContainersApp::controlled_engine("controlled Podman", true));
        app.hydrate_selection();
        assert!(
            requests.try_recv().is_err(),
            "a hydrated container is not asked again"
        );
    }
}
