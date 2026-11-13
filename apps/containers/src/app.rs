//! The container manager's window: [`ContainersState`] holds the engine snapshot, where
//! the window is and what its dialogs ask, and [`ContainersApp`] presents it in a
//! [`NavShell`](ui::widget::NavShell) with the dialogs mounted beside it. The draft is
//! the record; the engine stays in memory.

use std::cell::{RefCell, RefMut};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender};
use ui::widget::TaffyTreeExt;

use crate::resource::ContainersEffect;
#[cfg(test)]
use automerge::AutoCommit;
use automerge::ObjId;
use taffy::prelude::*;
use ui::a11y::{AccessNode, Role};
use ui::framework::{Dispatcher, Effected, Live, LiveRow, Publishes, RunMode, ViewCx};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tilo_actions::ActionSpec;
use ui::tilo_resource::Effect as _;
use ui::time::{Duration, Instant};
use ui::tokens::TextRole;
use ui::widget::DrawChild;
use ui::widget::toast::{Notice, ToastKind};
use ui::widget::{
    AppModel, Column, Element, ModalDialog, ModelPath, ModelView, NavPane, PromptBody, Text,
    TextInput, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::draft::CreateField;
use crate::model::{Container, DetailTab, Engine, Section};
use crate::podman::{self, Action, StreamMsg};
use crate::resource::Containers;
use crate::view::{self, Intent, Intents};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ConfirmTarget {
    Container { id: String, action: Action },
    Image { reference: String },
    Volume { name: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Confirm {
    pub(crate) name: String,
    pub(crate) target: ConfirmTarget,
}

impl Confirm {
    fn title(&self) -> String {
        format!("Remove {}?", self.name)
    }

    fn body(&self) -> String {
        let message = match self.target {
            ConfirmTarget::Container { .. } => "containers-remove-container-body",
            ConfirmTarget::Image { .. } => "containers-remove-image-body",
            ConfirmTarget::Volume { .. } => "containers-remove-volume-body",
        };
        crate::resource::CATALOG.t(message)
    }

    fn verb(&self) -> &'static str {
        match self.target {
            ConfirmTarget::Container { .. } => "Remove container",
            ConfirmTarget::Image { .. } => "Remove image",
            ConfirmTarget::Volume { .. } => "Remove volume",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PromptKind {
    NewVolume,
    TagImage,
    PullImage,
}

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

    fn label(&self) -> String {
        crate::resource::CATALOG.t(match self.kind {
            PromptKind::NewVolume => "containers-prompt-name",
            PromptKind::TagImage => "containers-prompt-tag",
            PromptKind::PullImage => "containers-prompt-image",
        })
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

pub(crate) struct PullProgress {
    pub(crate) reference: String,
    pub(crate) lines: Vec<String>,
    pub(crate) done: bool,
    pub(crate) failed: Option<String>,
    /// The person stopped it and the engine was told to end it.
    pub(crate) stopped: bool,
    pub(crate) fetched: Option<(u64, Option<u64>)>,
    /// Pressing Stop sets it, and the worker kills the engine's process rather than
    /// only hiding the sheet.
    pub(crate) stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// How long a verb may take to show in the engine before the app stops claiming it is
/// in flight.
const VERB_IN_FLIGHT_FOR: Duration = Duration::from_secs(30);

/// A verb asked on a container, and the state it was in when asked: it has landed once
/// the engine reports another state.
#[derive(Clone, Debug)]
struct InFlight {
    id: String,
    was: crate::model::ContainerStatus,
    asked: Instant,
}

/// How many output lines a pull keeps: enough to read why it stalled without holding a
/// whole pull log.
const PULL_LINES_KEPT: usize = 500;

/// What an engine output line says about bytes fetched: `Copying blob … 2.1 MiB / 3.4
/// MiB`, or the size alone where the engine names no total.
fn pulled_bytes(line: &str) -> Option<(u64, Option<u64>)> {
    fn scale(unit: &str) -> Option<f64> {
        Some(match unit.to_ascii_lowercase().as_str() {
            "b" => 1.0,
            "kb" | "kib" => 1_024.0,
            "mb" | "mib" => 1_048_576.0,
            "gb" | "gib" => 1_073_741_824.0,
            _ => return None,
        })
    }
    fn size(segment: &str) -> Option<u64> {
        let tokens: Vec<&str> = segment.split_whitespace().collect();
        tokens.iter().enumerate().rev().find_map(|(index, token)| {
            let (number, unit) = match token.find(|c: char| c.is_ascii_alphabetic()) {
                Some(at) => token.split_at(at),
                // A bare number is a size only when the word after it is the unit.
                None => (*token, *tokens.get(index + 1)?),
            };
            let value: f64 = number.parse().ok()?;
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a parsed byte count is a whole, positive number well inside u64"
            )]
            let bytes = (value * scale(unit)?) as u64;
            Some(bytes)
        })
    }
    let (done, total) = line.split_once('/').unwrap_or((line, ""));
    Some((size(done)?, size(total)))
}

/// A background message: a fresh probe, a pull line or its end, or the chosen
/// container's detail.
pub(crate) enum Bg {
    Snapshot(Engine),
    Pull(StreamMsg),
    /// The chosen container with its logs and inspect data, read beside the probe so the
    /// Logs tab follows a container that keeps logging.
    Detail(Box<Container>),
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
    pub fn post_run_snapshot(&self, engine: Engine) -> Result<(), String> {
        self.events
            .send(Bg::Snapshot(engine))
            .map_err(|_| "controlled create receiver disconnected".into())
    }
}

/// Where the person is: the section, the selected container and its tab. This window's
/// own, never shared and never undone.
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

/// What the window shows and the view's effect handler answers with.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent flags: live engine vs. sample data, a past version, \
              an open form and what the window has yet to follow"
)]
pub(crate) struct ContainersState {
    pub(crate) engine: Engine,
    /// Bumped whenever `engine` changes.
    engine_rev: u64,
    parts: view::Parts,
    /// The engine revision and view last published to the model.
    published: Option<(u64, ViewState)>,
    intents: Intents,
    installed_images: crate::create_form::InstalledImages,
    pub(crate) create_open: bool,
    create_toggled: bool,
    pub(crate) confirm: Option<Confirm>,
    confirm_changed: bool,
    pub(crate) prompt: Option<Prompt>,
    prompt_changed: bool,
    pub(crate) pull: Option<PullProgress>,
    pull_dirty: bool,
    /// The engine effects the window's controls asked for, for tests of a bare widget.
    #[cfg(test)]
    pub(crate) requested: Vec<(String, ContainersEffect)>,
    /// What a search hit or an agent's `open` named, shown with the next frame.
    opened: Rc<RefCell<Vec<String>>>,
    pub(crate) toast: Notice,
    bg_rx: Option<Receiver<Bg>>,
    bg_tx: Option<Sender<Bg>>,
    #[cfg(test)]
    controlled_runtime: Option<Sender<ControlledRuntimeRequest>>,
    #[cfg(test)]
    controlled_immediate_results: Option<Receiver<Result<(), String>>>,
    pub(crate) last_refresh: Instant,
    /// A verb asked of the engine and not yet visible in it: until the engine answers,
    /// that container's controls wait, as clicking again after nothing changed was
    /// what people did.
    in_flight: Option<InFlight>,
    pub(crate) live: bool,
    past: bool,
    pub(crate) view: ViewState,
    dispatcher: Option<Dispatcher<Containers>>,
}

/// The window: the shared [`ContainersState`] in its shell, with the dialogs mounted
/// beside it.
pub struct ContainersApp {
    state: Rc<RefCell<ContainersState>>,
    /// The draft as the record holds it, shown as it changes; `None` outside the
    /// framework.
    document: Option<Live<Containers>>,
    presented: Option<(u64, u64)>,
    rect: Rect,
    /// The window, rebuilt part by part from the published model.
    window: ModelView<(Section, bool, bool)>,
    intents: Intents,
    /// The new-container form, retained while closed so its fields keep their caret and
    /// binding.
    create: ModalDialog<Column>,
    confirm_dialog: Option<ModalDialog<Text>>,
    prompt_dialog: Option<ModalDialog<PromptBody>>,
    pull_dialog: Option<ModalDialog<Column>>,
    /// The structure changed (a dialog opened or closed, the shell was rebuilt).
    structure_dirty: bool,

    dirty: bool,
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

#[cfg(test)]
fn controlled_input() -> TextInput {
    TextInput::local_with_language("en_US")
        .with_spellcheck(false)
        .with_autocorrect(false)
}

impl ContainersApp {
    pub fn new() -> Self {
        Self::with_engine(Engine::default(), TextInput::local)
    }

    /// The manager over the sample engine, offline: lifecycle controls change the sample
    /// instead of reaching podman.
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
            actions: None,
        };
        let window = view::window(&parts);
        let state = ContainersState {
            engine,
            engine_rev: 0,
            parts,
            published: None,
            intents: intents.clone(),
            installed_images,
            create_open: false,
            create_toggled: false,
            confirm: None,
            confirm_changed: false,
            prompt: None,
            prompt_changed: false,
            pull: None,
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
            in_flight: None,
            live: false,
            past: false,
            view: ViewState::default(),
            dispatcher: None,
        };
        Self {
            dirty: true,
            state: Rc::new(RefCell::new(state)),
            document: None,
            presented: None,
            rect: Rect::ZERO,
            window,
            intents,
            create,
            confirm_dialog: None,
            prompt_dialog: None,
            pull_dialog: None,
            structure_dirty: true,
        }
    }

    pub(crate) fn state(&self) -> RefMut<'_, ContainersState> {
        self.state.borrow_mut()
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

    fn follow_document(&mut self) {
        let Some(document) = &self.document else {
            return;
        };
        let Some(shown) = document.changed_since(&mut self.presented) else {
            return;
        };
        self.state().present_draft(&shown.draft);
    }

    /// Mount, rebuild or close the dialogs the state asked for since.
    fn follow_state(&mut self) {
        let mut state = self.state.borrow_mut();
        if std::mem::take(&mut state.create_toggled) {
            self.create.set_open(state.create_open);
            focus_draft(
                &mut self.create,
                state.create_open.then_some(CreateField::Image),
            );
            self.structure_dirty = true;
        }
        if std::mem::take(&mut state.confirm_changed) {
            self.confirm_dialog = state.confirm.as_ref().map(|confirm| {
                let accept = self.intents.clone();
                let cancel = self.intents.clone();
                let dismiss = self.intents.clone();
                ModalDialog::new(
                    confirm.title(),
                    Text::new(&confirm.body()).with_role(TextRole::Body),
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
        if std::mem::take(&mut state.prompt_changed) {
            self.prompt_dialog = state.prompt.as_ref().map(|prompt| {
                let submit = self.intents.clone();
                let cancel = self.intents.clone();
                let dismiss = self.intents.clone();
                let body = PromptBody::new(&prompt.label(), prompt.placeholder())
                    .with_initial(&prompt.value);
                ModalDialog::new(prompt.title(), body)
                    .with_test_id("containers-prompt-dialog")
                    .primary_action(prompt.primary(), move || submit.push(Intent::PromptSubmit))
                    .secondary_action("Cancel", move || cancel.push(Intent::PromptCancel))
                    .on_dismiss(move || dismiss.push(Intent::PromptCancel))
                    .open(true)
            });
            self.structure_dirty = true;
        }
        if std::mem::take(&mut state.pull_dirty) {
            let had = self.pull_dialog.is_some();
            let intents = self.intents.clone();
            self.pull_dialog = state.pull.as_ref().map(|pull| {
                let dismiss = self.intents.clone();
                let title = if pull.failed.is_some() {
                    format!("Couldn't pull {}", pull.reference)
                } else if pull.done {
                    format!("Pulled {}", pull.reference)
                } else {
                    format!("Pulling {}", pull.reference)
                };
                // Closing while the pull runs stops it.
                ModalDialog::new(title, view::pull_body(pull, &intents))
                    .with_test_id("containers-pull-dialog")
                    .on_dismiss(move || dismiss.push(Intent::ClosePull))
                    .open(true)
            });
            if had != self.pull_dialog.is_some() || self.pull_dialog.is_some() {
                self.structure_dirty = true;
            }
        }
    }

    /// Apply what the window's controls asked for, then follow the dialogs.
    pub(crate) fn drain_intents(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let typed = self
            .prompt_dialog
            .as_ref()
            .map(|dialog| dialog.content().value().trim().to_owned())
            .unwrap_or_default();
        self.state.borrow_mut().drain_intents(doc, &typed);
        self.follow_state();
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
                "n" | "N" if self.state.borrow().engine_controls_available() => {
                    Some(Intent::NewContainer)
                }
                _ => None,
            },
            _ => None,
        };
        if let Some(intent) = intent {
            self.intents.push(intent);
            *ctx.handled = true;
        }
    }

    fn create_open(&self) -> bool {
        self.state.borrow().create_open
    }

    fn top_dialog_mut(&mut self) -> Option<&mut dyn Widget> {
        let create_open = self.create_open();
        if let Some(dialog) = self.confirm_dialog.as_mut() {
            return Some(dialog);
        }
        if let Some(dialog) = self.prompt_dialog.as_mut() {
            return Some(dialog);
        }
        if let Some(dialog) = self.pull_dialog.as_mut() {
            return Some(dialog);
        }
        if create_open {
            return Some(&mut self.create);
        }
        None
    }

    fn open_dialogs(&self) -> Vec<&dyn Widget> {
        let mut out: Vec<&dyn Widget> = Vec::new();
        if self.create_open() {
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
        let create_open = self.create_open();
        let mut out: Vec<&mut (dyn Widget + '_)> = Vec::new();
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

    pub fn stage_scene(&mut self, scenario: Option<&str>) {
        self.state().stage_scene(scenario);
    }

    pub fn stage_sample_engine(&mut self) {
        self.state().stage_sample_engine();
    }

    pub fn perform_effect(&mut self, effect: ContainersEffect) -> Result<(), String> {
        self.state().perform_effect(effect)
    }

    pub fn is_create_open(&self) -> bool {
        self.create_open()
    }

    pub fn section(&self) -> Section {
        self.state.borrow().view.section
    }

    pub fn selected(&self) -> Option<String> {
        self.state.borrow().view.selected.clone()
    }

    pub fn tab(&self) -> DetailTab {
        self.state.borrow().view.tab
    }
}

fn focus_draft(create: &mut ModalDialog<Column>, field: Option<CreateField>) {
    crate::create_form::for_each_input(create.content_mut(), &mut |input| {
        let on = field == Some(input.field());
        Widget::set_focus(input.input_mut(), on);
    });
}

/// The view over `window`: it shares the window's state with the effect handler, attaches
/// it to the engine this run shows and publishes the engine's rows and search entries.
pub(crate) fn window(cx: &ViewCx<Containers>, mut window: ContainersApp) -> Element {
    window.document = Some(cx.live().clone());
    // The pages' rows run the app's published actions, and the window is built before it
    // joins the view: give it its actions now and lay it out again.
    {
        let mut state = window.state.borrow_mut();
        state.parts.actions = Some(cx.actions());
        window.window = view::window(&state.parts);
    }
    let dispatcher = cx.dispatcher();
    crate::create_form::for_each_input(window.create.content_mut(), &mut |input| {
        input.attach(dispatcher.clone());
    });
    let state = Rc::clone(&window.state);
    {
        let mut state = state.borrow_mut();
        match cx.mode() {
            RunMode::Live => state.connect(),
            RunMode::Capture => {
                if let Some(engine) = cx.fixture::<Engine>() {
                    state.show_engine((*engine).clone());
                }
            }
            RunMode::Past => state.show_past(),
        }
        state.dispatcher = Some(dispatcher);
    }
    // A search hit, or an agent's `open`, shows the row it names.
    cx.on_open(state.borrow().opener());
    {
        let (answer, status) = (Rc::clone(&state), Rc::clone(&state));
        cx.effects(move |effect: ContainersEffect, _| match effect {
            ContainersEffect::ReadContainer { container } => answer
                .borrow()
                .read_container(&container)
                .map(Effected::value),
            effect => answer
                .borrow_mut()
                .perform_effect(effect)
                .map(|()| Effected::done()),
        })
        .status(move || status.borrow().effect_statuses());
    }
    {
        let state = Rc::clone(&state);
        cx.on_stage(move |scenario| state.borrow_mut().stage_scene(scenario));
    }
    let (rows, row, entries) = (Rc::clone(&state), Rc::clone(&state), state);
    cx.publishing(
        Publishes::new()
            .live_rows(move || rows.borrow().engine_rows())
            .live_row(move |id| row.borrow().engine_row(id))
            // Containers with their recent logs, images, volumes and distroboxes: what the
            // engine holds, not the document.
            .search_entries(move || crate::search::engine_entries(&entries.borrow().engine)),
        Box::new(window),
    )
}

impl ContainersState {
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
    }

    /// Show a past version: its draft, and no engine, which this device's live state has
    /// no history for.
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

    /// The draft as the record holds it now; a past version with a draft shows the form.
    pub(crate) fn present_draft(&mut self, draft: &crate::draft::CreateDraft) {
        if self.past {
            self.set_create_open(!draft.is_empty());
        }
    }

    pub(crate) fn show_section(&mut self, section: Section) {
        self.view.section = section;
        self.view.selected = None;
        self.parts.pane.set(NavPane::Overview);
        self.parts.offsets.list.set((0.0, 0.0));
        self.publish_model();
    }

    /// Select `id` (`None` clears it), bringing its detail to the front of a narrow
    /// window.
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

    /// Ask the host for `effect` as the live row `row` offers it: it checks permission,
    /// availability and confirmation, records the effect in history, then the view runs
    /// it.
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

    /// What a search hit or an agent's `open` runs: show the row its anchor names.
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

    /// Whether lifecycle controls are live: the sample stays interactive, and a
    /// disconnected engine does not advertise controls that can only fail.
    pub(crate) fn engine_controls_available(&self) -> bool {
        !self.past && (self.engine.live || !self.live)
    }

    /// Whether the sidebar reports the engine as reachable; the sample stands in for a
    /// connected engine.
    fn connected(&self) -> bool {
        !self.past && (self.engine.live || !self.live)
    }

    /// Kick a background re-probe so the stats stay fresh without blocking the UI.
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
        let selected = self.view.selected.clone();
        std::thread::spawn(move || {
            let engine = podman::probe().engine;
            // The chosen container's logs and inspect data are read beside the probe, so a
            // container that logs an error after it was opened says so on the next pass.
            let detail = selected
                .and_then(|id| {
                    let mut container = engine.containers.iter().find(|c| c.id == id)?.clone();
                    podman::inspect_into(&mut container);
                    match podman::fetch_logs(&id, 200) {
                        Ok(logs) => {
                            container.logs = logs;
                            container.logs_error = None;
                            container.logs_fetched_unix_ms = ui::time::try_unix_now_ms();
                        }
                        Err(error) => container.logs_error = Some(error),
                    }
                    Some(container)
                })
                .map(|container| Bg::Detail(Box::new(container)));
            let _ = tx.send(Bg::Snapshot(engine));
            if let Some(detail) = detail {
                let _ = tx.send(detail);
            }
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

    /// Drain any background snapshot, keeping the selected container's detail that `ps`
    /// does not carry.
    pub(crate) fn drain_bg(&mut self) {
        let Some(rx) = &self.bg_rx else { return };
        let mut latest = None;
        let mut pull_msgs = Vec::new();
        let mut details = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            match msg {
                Bg::Snapshot(e) => latest = Some(e),
                Bg::Pull(m) => pull_msgs.push(m),
                Bg::Detail(container) => details.push(container),
            }
        }
        for msg in pull_msgs {
            self.apply_pull_msg(msg);
        }
        if let Some(mut fresh) = latest {
            // Carry the selected container's detail over so the tabs do not flicker
            // empty between probes.
            if let Some(sel) = self.view.selected.clone()
                && let Some(old) = self.engine.find(&sel).cloned()
                && let Some(newc) = fresh.containers.iter_mut().find(|c| c.id == sel)
            {
                // Logs are not carried over: the detail read beside this probe replaces
                // them with what the container has written now.
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
        for detail in details {
            let Some(container) = self
                .engine
                .containers
                .iter_mut()
                .find(|c| c.id == detail.id)
            else {
                continue;
            };
            *container = *detail;
            self.engine_changed();
        }
    }

    /// Read the chosen container's detail (logs and inspect) on the probe thread, never
    /// in the frame.
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
        if self.last_refresh.elapsed() >= std::time::Duration::from_millis(250) {
            self.spawn_refresh();
        }
    }

    fn container_name(&self, id: &str) -> String {
        self.engine
            .find(id)
            .map_or_else(|| id.to_string(), |c| c.name.clone())
    }

    /// Apply a lifecycle action; a destructive one arrives only after the person
    /// agreed.
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

    /// Mirror a verb in the in-memory snapshot when no engine is present, so the demo
    /// stays interactive.
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

    /// Start a streaming `podman pull` on a worker thread and show its progress; offline
    /// it simulates a quick success.
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
            stopped: false,
            fetched: None,
            stop: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
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
                let stop = self
                    .pull
                    .as_ref()
                    .map_or_else(Default::default, |p| std::sync::Arc::clone(&p.stop));
                std::thread::spawn(move || {
                    let (ptx, prx) = std::sync::mpsc::channel();
                    let worker = std::thread::spawn(move || {
                        podman::stream_command_stoppable(&args, &ptx, &stop);
                    });
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

    /// Ask the engine to end the download; the worker kills the process.
    pub(crate) fn stop_pull(&mut self) {
        let Some(pull) = &mut self.pull else { return };
        if pull.done || pull.stopped {
            return;
        }
        pull.stop.store(true, std::sync::atomic::Ordering::Release);
        self.pull_dirty = true;
    }

    fn apply_pull_msg(&mut self, msg: StreamMsg) {
        let Some(p) = &mut self.pull else { return };
        self.pull_dirty = true;
        match msg {
            StreamMsg::Line(l) => {
                if let Some(fetched) = pulled_bytes(&l) {
                    p.fetched = Some(fetched);
                }
                p.lines.push(l);
                let n = p.lines.len();
                if n > PULL_LINES_KEPT {
                    p.lines.drain(0..n - PULL_LINES_KEPT);
                }
            }
            StreamMsg::Stopped => {
                p.stopped = true;
                let r = p.reference.clone();
                self.show_toast(format!("Stopped pulling {r}"), false);
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

    /// Remove an image after confirmation, forcing it so an unused tag goes even when a
    /// stopped container references it.
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

    /// Open a real shell in a running container: the Tilo terminal running `podman exec
    /// -it <id> /bin/sh`.
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
        self.create_toggled = true;
    }

    pub(crate) fn ask_confirm(&mut self, confirm: Confirm) {
        self.confirm = Some(confirm);
        self.confirm_changed = true;
    }

    pub(crate) fn ask_prompt(&mut self, prompt: Prompt) {
        self.prompt = Some(prompt);
        self.prompt_changed = true;
    }

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
        self.confirm_changed = true;
    }

    fn submit_prompt(&mut self, value: &str) {
        let Some(prompt) = self.prompt.take() else {
            return;
        };
        self.prompt_changed = true;
        match prompt.kind {
            PromptKind::PullImage => self.start_pull(value),
            PromptKind::NewVolume => self.create_volume(value),
            PromptKind::TagImage => self.tag_image(&prompt.context, value),
        }
    }

    /// Show the sample engine on an existing host, with no device polling. The live
    /// action executor stays installed; the offline engine refuses real lifecycle
    /// effects.
    pub(crate) fn stage_sample_engine(&mut self) {
        self.live = false;
        self.bg_rx = None;
        self.bg_tx = None;
        self.show_engine(crate::model::sample_engine());
    }

    /// Stage a capture's section, selection, tab and open dialog; the engine and the
    /// draft are the scenario's already.
    pub(crate) fn stage_scene(&mut self, scenario: Option<&str>) {
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
                    stopped: false,
                    fetched: Some((2_202_009, Some(3_565_158))),
                    stop: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                });
                self.pull_dirty = true;
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

    /// Apply what the window's controls asked for; `typed` is what the prompt holds.
    fn drain_intents(&mut self, doc: &mut ui::widget::Edits<'_>, typed: &str) {
        self.show_opened();
        let intents = self.intents.take();
        if intents.is_empty() {
            return;
        }
        for intent in intents {
            self.apply_intent(doc, intent, typed);
        }
        self.publish_model();
    }

    fn apply_intent(&mut self, doc: &mut ui::widget::Edits<'_>, intent: Intent, typed: &str) {
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
            Intent::PullImage => self.ask_prompt(Prompt {
                kind: PromptKind::PullImage,
                value: String::new(),
                context: String::new(),
            }),
            Intent::NewVolume => self.ask_prompt(Prompt {
                kind: PromptKind::NewVolume,
                value: String::new(),
                context: String::new(),
            }),
            Intent::SubmitCreate => self.submit_create(doc),
            Intent::CloseCreate => self.set_create_open(false),
            Intent::ConfirmAccept => self.accept_confirm(),
            Intent::ConfirmCancel => {
                self.confirm = None;
                self.confirm_changed = true;
            }
            Intent::PromptSubmit => self.submit_prompt(typed),
            Intent::PromptCancel => {
                self.prompt = None;
                self.prompt_changed = true;
            }
            Intent::StopPull => self.stop_pull(),
            Intent::ClosePull => {
                // Closing a pull that is still running ends it.
                if self
                    .pull
                    .as_ref()
                    .is_some_and(|p| !p.done && p.failed.is_none())
                {
                    self.stop_pull();
                }
                self.pull = None;
                self.pull_dirty = true;
            }
            Intent::Reconnect => self.spawn_refresh(),
        }
    }

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
        let busy = self.saw_in_flight();
        self.parts.model.set(
            view::Model {
                engine: Rc::new(self.engine.clone()),
                view,
                controls: self.engine_controls_available(),
                connected: self.connected(),
                missing: self.live && !self.engine.live,
                busy,
                ..view::Model::default()
            }
            .digested(),
        );
    }

    /// What differs from the effects' declarations: creating needs a connected engine.
    pub(crate) fn effect_statuses(&self) -> Vec<ui::framework::EffectStatus> {
        if self.engine.live {
            Vec::new()
        } else {
            vec![ui::framework::EffectStatus::unavailable(
                ContainersEffect::CREATE,
                "engine_offline",
                "Creating containers requires a connected Podman engine",
            )]
        }
    }

    fn tick(&mut self) {
        self.drain_bg();
        // Periodically re-probe the engine; a controlled fixture owns that cadence
        // through its request channel.
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
        self.toast.expire();
        self.toast.publish();
        self.publish_model();
    }

    /// Whether the window keeps syncing while idle: a live engine, or a pull still
    /// running.
    fn needs_idle_sync(&self) -> bool {
        self.live
            || self
                .pull
                .as_ref()
                .is_some_and(|p| !p.done && p.failed.is_none())
    }
}

/// How a finished verb reads in the wall's notice.
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

impl Widget for ContainersApp {
    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, path: &ObjId) {
        for child in self.children_mut() {
            child.settle(edits, path);
        }
        self.drain_intents(edits);
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.dirty = true;
        self.follow_document();
        self.state.borrow_mut().tick();
        self.follow_state();
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
        self.follow_state();
        let window = self.window.update(state, path);
        window || self.structure_dirty
    }

    fn needs_idle_sync(&self) -> bool {
        self.state.borrow().needs_idle_sync()
    }

    fn needs_animation_frame(&self) -> bool {
        let mut needed = false;
        self.for_each_child(&mut |child| {
            needed = needed || child.needs_animation_frame();
        });
        needed
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        let mut earliest = self.state.borrow().toast.deadline();
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
        self.window
            .draw_child(scene, transform, tr, theme, presence);
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
        self.dirty = true;
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
            let pane = self.state.borrow().parts.pane.clone();
            let before = pane.get();
            if let Some(&node) = children.first() {
                self.window
                    .handle_event(event, &mut ctx.reborrow(ctx.path, node));
            }
            // Escape closes the detail the same way at every width: in a narrow window the
            // shell takes it as Back, and the selection goes with the screen it was shown
            // on.
            let went_back = before == NavPane::Content && pane.get() == NavPane::Overview;
            let selected = self.state.borrow().view.selected.is_some();
            if is_escape(event) && selected && (went_back || !*ctx.handled) {
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
        let create_open = self.create_open();
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

    fn needs_redraw(&self) -> bool {
        ui::widget::redraw_or_children(self.dirty, self)
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
        ui::widget::Widget::mark_children_drawn(self);
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
                .with_name(crate::resource::CATALOG.t("containers-a11y-window"))
                .with_description(&self.state.borrow().engine.connection)
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
        let app = Self::with_engine(engine, controlled_input);
        {
            let mut state = app.state();
            state.live = true;
            state.bg_tx = Some(bg_tx);
            state.bg_rx = Some(bg_rx);
            state.controlled_runtime = Some(requests_tx);
            state.controlled_immediate_results = Some(results_rx);
        }
        (app, requests_rx, results_tx)
    }

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
                logs_fetched_unix_ms: None,
            }],
            ..Engine::default()
        }
    }
}

const RESOURCE_SNAPSHOT_LIMIT: usize = 200;

const ENGINE_OFFLINE: &str = "containers-engine-offline-message";

/// `effect` as a row offers it: an engine effect only while the engine answers, and
/// otherwise what it says instead.
fn offered(effect: &ContainersEffect, engine_live: bool, message: &str) -> ActionSpec {
    let spec = effect.bound();
    if engine_live || effect.engine_operation().is_none() {
        spec
    } else {
        spec.unavailable("engine_offline", crate::resource::CATALOG.t(message))
    }
}

pub(crate) fn container_row(container: &crate::model::Container, engine_live: bool) -> LiveRow {
    let id = || container.id.clone();
    let mut row = LiveRow::new(
        format!("container:{}", container.id),
        "containers.container",
        container.name.clone(),
    )
    .operation(
        ContainersEffect::ReadContainer { container: id() }
            .bound()
            .reads_document(),
    )
    .operation({
        let mut open = ContainersEffect::SelectContainer { id: Some(id()) }.bound();
        open.type_mut().label = crate::resource::CATALOG.t_args(
            "containers-action-open",
            &[("name", ui::i18n::ArgValue::Text(container.name.clone()))],
        );
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
    .operation(offered(
        &ContainersEffect::EnterDistrobox {
            distrobox: distrobox.name.clone(),
        },
        engine_live,
        "containers-demo-unavailable",
    ))
}

impl ContainersState {
    /// Note that `verb` was asked on `target` and has not shown in the engine yet.
    fn begin_in_flight(&mut self, kind: &str, target: &str) {
        if kind != "containers.container" {
            return;
        }
        let was = self.engine.find(target).map(|c| c.status);
        self.in_flight = was.map(|was| InFlight {
            id: target.to_owned(),
            was,
            asked: ui::time::now(),
        });
        self.engine_changed();
    }

    /// The container a verb is in flight on, dropping a record the engine answered or one
    /// that waited too long.
    fn saw_in_flight(&mut self) -> Option<String> {
        let asked = self.in_flight.as_ref()?;
        let answered = self
            .engine
            .find(&asked.id)
            .is_none_or(|c| c.status != asked.was);
        if answered || asked.asked.elapsed() >= VERB_IN_FLIGHT_FOR {
            self.in_flight = None;
            return None;
        }
        Some(asked.id.clone())
    }

    /// What a verb is called while the engine is doing it.
    fn in_flight_word(verb: &str) -> &'static str {
        match verb {
            "start" => "Starting",
            "stop" => "Stopping",
            "restart" => "Restarting",
            "pause" => "Pausing",
            "unpause" => "Resuming",
            "remove" => "Removing",
            _ => "Asking the engine for",
        }
    }

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
            self.begin_in_flight(kind, target);
            self.spawn_refresh();
            let name = self.container_name(target);
            self.show_toast(format!("{} {name}…", Self::in_flight_word(verb)), false);
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
        self.begin_in_flight(kind, target);
        self.spawn_refresh();
        let name = if kind == "containers.container" {
            self.container_name(target)
        } else {
            target.to_owned()
        };
        self.show_toast(format!("{} {name}…", Self::in_flight_word(verb)), false);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
