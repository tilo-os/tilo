//! Root widget for the container manager.
//!
//! `ContainersApp` is a single self-laying-out widget that owns the engine
//! snapshot (containers / images / distroboxes mirrored from rootless
//! podman) and renders the whole surface — a left navigation rail, a master
//! list with inline lifecycle actions, a tabbed detail view, a create-
//! container form, and confirm/create modals. The new-container draft is the
//! resource's document (see [`crate::doc`]), edited a splice at a time
//! through the framework's dispatcher; where the user is (section, selection,
//! detail tab) is this window's own, and the engine projection is transient
//! host data that stays in memory.
//!
//! The widget hit-tests in `handle_event` against rectangles it recorded
//! during `draw`, which keeps the dense, data-driven layout (variable row
//! counts, wrapped port chips, tab strips) under direct control without
//! fighting a flexbox tree.

use std::sync::mpsc::{Receiver, Sender};
use ui::time::Instant;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use std::cell::Cell;
use std::rc::Rc;
use ui::a11y::{AccessNode, AccessState, Action as AccessibilityAction, Politeness, Role};
use ui::control_custody::RawControlSlot;
use ui::framework::{Dispatcher, LiveRow, TextControl};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tilo_actions::{
    ActionAddress, ActionSpec, ControlInvocation, OperationKind, PermissionRequirement,
    RetryPolicy, flatten,
};
use ui::tilo_resource::TextEdit;
use ui::widget::command_palette::{CommandEntry, CommandPalette};
use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind, paint_transient};
use ui::widget::{
    FlapState, ModalDialog, PromptBody, SearchField, SplitLayout, Text, TextInput, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::Key;

use taffy::prelude::*;

use crate::combobox::ComboState;
use crate::doc::{CreateDoc, CreateDraft, CreateField};
use crate::draw;
use crate::model::{Container, DetailTab, Engine, Section};
use crate::podman::{self, Action, ImageAction, StreamMsg};
use crate::resource::Containers;

const RAIL_WIDTH: f64 = 200.0;
/// Width of the rail when collapsed — a thin strip that still shows the
/// section icons and the expand toggle.
const RAIL_COLLAPSED: f64 = 56.0;
pub(crate) const HEADER_HEIGHT: f64 = 64.0;
pub(crate) const ROW_HEIGHT: f64 = 64.0;
pub(crate) const DETAIL_WIDTH: f64 = 420.0;
/// Below this width there is not enough room for both the navigation rail and
/// a useful master/detail surface. Large text gets the same treatment even on
/// a tablet: preserving readable controls matters more than preserving the
/// desktop split.
const COMPACT_VIEWPORT_WIDTH: f64 = 600.0;
const LARGE_TEXT_BASE_SIZE: f32 = 20.0;
const TOAST_MS: u128 = 4000;

/// A clickable region recorded during draw and resolved in `handle_event`.
#[derive(Clone)]
pub(crate) enum Hit {
    Section(Section),
    NewContainer,
    SelectContainer(String),
    SelectImage(usize),
    Action {
        id: String,
        action: Action,
    },
    /// An image lifecycle action (remove / pull-again), keyed by reference.
    ImageAct {
        reference: String,
        action: ImageAction,
    },
    /// Pull the image reference under the cursor (re-pull / update).
    PullImage(String),
    /// Pull the reference currently typed into the create-form image field.
    PullTyped,
    /// Tag the selected image — opens the tag dialog.
    TagImage(String),
    /// Inspect the image (toast/inspect view).
    InspectImage(String),
    /// Volume lifecycle.
    CreateVolume,
    RemoveVolume(String),
    /// Attach an interactive shell to a running container in a real terminal.
    AttachShell(String),
    DistroEnter(String),
    DistroCreate,
    DetailTab(DetailTab),
    CloseDetail,
    CreateField(CreateField),
    CreateSubmit,
    CreateCancel,
    /// Toggle the image-combobox suggestion dropdown.
    ComboToggle,
    /// Pick option `idx` (into the filtered list) from the combobox.
    ComboPick(usize),
    /// Collapse / expand the navigation sidebar.
    ToggleSidebar,
    /// Dismiss hit reused by the pull overlay's Close button.
    PromptCancel,
}

pub(crate) struct HitRegion {
    pub(crate) rect: Rect,
    pub(crate) hit: Hit,
}

/// What a destructive confirm targets — a container, an image, or a volume.
#[derive(Clone)]
pub(crate) enum ConfirmTarget {
    Container { id: String, action: Action },
    Image { reference: String },
    Volume { name: String },
}

/// The live destructive-confirm dialog: the shared [`ModalDialog`] plus the
/// `confirm` / `cancel` cells its action callbacks flip (drained by the host).
struct ConfirmDlg {
    dialog: ModalDialog<Text>,
    confirm: Rc<Cell<bool>>,
    cancel: Rc<Cell<bool>>,
}

/// The live text-prompt dialog: the shared [`ModalDialog`] hosting a real
/// [`PromptBody`] (a genuine [`ui::widget::TextInput`], so caret / selection /
/// clipboard work), plus the submit / cancel cells its buttons flip.
struct PromptDlg {
    dialog: ModalDialog<PromptBody>,
    submit: Rc<Cell<bool>>,
    cancel: Rc<Cell<bool>>,
}

/// A transient confirm dialog for a destructive action.
pub(crate) struct Confirm {
    /// Display name shown in the dialog body.
    pub(crate) name: String,
    /// Verb label shown in the title + OK button.
    pub(crate) verb: String,
    pub(crate) target: ConfirmTarget,
}

/// Kind of text the prompt dialog collects.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptKind {
    /// Create a new named volume.
    NewVolume,
    /// Tag an existing image (`source` carried in `Prompt::context`).
    TagImage,
}

/// A transient single-line text prompt (new volume name, image tag).
pub(crate) struct Prompt {
    pub(crate) kind: PromptKind,
    pub(crate) title: String,
    pub(crate) placeholder: String,
    pub(crate) value: String,
    /// Extra context (e.g. the image being tagged).
    pub(crate) context: String,
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

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent, orthogonal flag (live engine vs. \
              sample data, a past version, and separate open/dirty \
              states for unrelated overlays); they aren't a state machine, \
              and grouping them would mean churning every field access across \
              this widget and panels.rs for no behavioural gain"
)]
pub struct ContainersApp {
    /// The live (or sample) engine projection.
    pub(crate) engine: Engine,
    /// Retained, revision-gated search projection. Queries page this index
    /// instead of rescanning the engine snapshot on every keystroke.
    search_index: crate::search_provider::ContainerSearchIndex,
    rect: Rect,
    /// Mouse position in logical coords (for hover + hit-testing).
    pub(crate) cursor: Point,
    /// Click regions recorded during the last draw.
    pub(crate) hits: Vec<HitRegion>,
    /// Per-section scroll offsets (list scroll).
    pub(crate) list_scroll: f64,
    pub(crate) detail_scroll: f64,
    /// Active confirm dialog metadata, if any (name / verb / target).
    pub(crate) confirm: Option<Confirm>,
    /// The live confirm dialog (shared [`ModalDialog`]), built lazily from
    /// `confirm` and torn down when it resolves.
    confirm_dialog: Option<ConfirmDlg>,
    /// Active text-prompt metadata, if any (new volume / image tag / pull).
    pub(crate) prompt: Option<Prompt>,
    /// The live prompt dialog (shared [`ModalDialog`] hosting a real
    /// [`PromptBody`]/[`TextInput`]), built lazily from `prompt`.
    prompt_dialog: Option<PromptDlg>,
    /// Whether the create-container modal is open.
    pub(crate) create_open: bool,
    /// Which create field has keyboard focus.
    pub(crate) create_focus: CreateField,
    /// The six create-form fields, each showing its draft text and turning
    /// every edit into a splice of it.
    pub(crate) create_inputs: [TextControl<TextInput>; CreateField::COUNT],
    /// The draft as last presented.
    draft: CreateDraft,
    /// State of the typeable image combobox inside the create modal.
    pub(crate) combo: ComboState,
    /// Live pull progress overlay, if a pull is running.
    pub(crate) pull: Option<PullProgress>,
    /// The header's search affordance. Instead of an inline filter it is a
    /// launcher that opens the shared Ctrl+K [`CommandPalette`] scoped to
    /// this app — search does not live in the app.
    pub(crate) search_field: SearchField,
    /// The shared command palette, seeded with this app's containers when
    /// opened. This is the *same* toolkit widget the shell uses for its
    /// global Ctrl+K overlay, hosted here in app scope.
    pub(crate) palette: CommandPalette,
    /// Whether the app-scoped palette overlay is currently open.
    pub(crate) palette_open: bool,
    /// In-app lifecycle controls emit the same inert resource intent as Ctrl+K
    /// and agents. The host re-resolves authority, records the Effect, then
    /// calls `Widget::start_operation`.
    pub(crate) pending_control_invocation: RawControlSlot,
    /// Collapsible sidebar reveal kernel (the shared `ui` flap animation).
    pub(crate) sidebar_flap: FlapState,
    /// Wall-clock anchor for advancing the sidebar slide from the draw path.
    pub(crate) sidebar_tick: Option<Instant>,
    /// Current toast.
    pub(crate) toast: Option<Toast>,
    /// Background refresh plumbing. `None` in headless/shots mode.
    bg_rx: Option<Receiver<Bg>>,
    bg_tx: Option<Sender<Bg>>,
    #[cfg(test)]
    controlled_runtime: Option<Sender<ControlledRuntimeRequest>>,
    #[cfg(test)]
    controlled_immediate_results: Option<Receiver<Result<(), String>>>,
    last_refresh: Instant,
    pub(crate) live: bool,
    /// Whether a structural change happened this frame (forces relayout).
    pub(crate) structure_dirty: bool,
    /// Where the user is: the section, the selected container, its tab.
    pub(crate) view: ViewState,
    /// Showing a past version in History: nothing here is live.
    past: bool,
    /// Layout mode used by the last drawn frame. Event routing deliberately
    /// follows this instead of guessing from width alone, because 200% text
    /// also switches the surface to a single detail pane.
    compact_layout_active: bool,
    /// Framework motion policy. Time-driven state still updates through idle
    /// sync, but decorative toast/progress repaint loops stop when reduced.
    motion_reduced: bool,
    /// Keyboard focus for the immediate-mode root; its focused synthetic
    /// create field becomes the exported accessibility active descendant.
    has_focus: bool,
    /// Where edits of the draft go, once the framework attached the surface.
    dispatcher: Option<Dispatcher<Containers>>,
}

impl Default for ContainersApp {
    fn default() -> Self {
        Self::new()
    }
}

/// Input dependencies are independent of whether the runtime is live.
#[derive(Clone, Copy)]
struct InputFactories {
    text: fn() -> TextInput,
    search: fn() -> SearchField,
}

impl InputFactories {
    fn production() -> Self {
        Self {
            text: TextInput::local,
            search: SearchField::new,
        }
    }

    #[cfg(test)]
    fn controlled() -> Self {
        Self {
            text: || {
                TextInput::local_with_language("en_US")
                    .with_spellcheck(false)
                    .with_autocorrect(false)
            },
            search: || SearchField::with_language("en_US"),
        }
    }
}

impl ContainersApp {
    /// The manager with no engine yet: the framework's
    /// [`attach`](ui::framework::ResourceSurface::attach) connects it to
    /// podman in a live window, shows a capture's engine, or leaves it empty
    /// for a past version.
    pub fn new() -> Self {
        Self::with_engine(Engine::default(), InputFactories::production())
    }

    /// The manager over the bundled sample engine, offline: lifecycle
    /// controls change the sample instead of reaching podman.
    pub fn sample() -> Self {
        Self::with_engine(crate::model::sample_engine(), InputFactories::production())
    }

    fn with_engine(engine: Engine, inputs: InputFactories) -> Self {
        let search_index = crate::search_provider::ContainerSearchIndex::new(&engine);
        let create_inputs = CreateField::ALL.map(|field| {
            let input = (inputs.text)()
                .with_id(&format!("container_create_{}", field.key()))
                .with_placeholder(field.placeholder())
                .with_font_size(13.0)
                .with_plain_chrome();
            TextControl::view_only(input, move |doc| CreateDoc::field_obj(doc, field))
        });
        Self {
            engine,
            search_index,
            rect: Rect::ZERO,
            cursor: Point::ZERO,
            hits: Vec::new(),
            list_scroll: 0.0,
            detail_scroll: 0.0,
            confirm: None,
            confirm_dialog: None,
            prompt: None,
            prompt_dialog: None,
            create_open: false,
            create_focus: CreateField::Name,
            create_inputs,
            draft: CreateDraft::default(),
            combo: ComboState::default(),
            pull: None,
            search_field: (inputs.search)()
                .opens_palette()
                .with_placeholder(crate::panels::LAUNCHER_PLACEHOLDER),
            palette: CommandPalette::commands(),
            palette_open: false,
            pending_control_invocation: RawControlSlot::replace_one(),
            sidebar_flap: FlapState::open_when_docked(),
            sidebar_tick: None,
            toast: None,
            bg_rx: None,
            bg_tx: None,
            #[cfg(test)]
            controlled_runtime: None,
            #[cfg(test)]
            controlled_immediate_results: None,
            last_refresh: ui::time::now(),
            live: false,
            structure_dirty: false,
            view: ViewState::default(),
            past: false,
            compact_layout_active: false,
            motion_reduced: false,
            has_focus: false,
            dispatcher: None,
        }
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
        self.search_index.sync(&self.engine);
        self.structure_dirty = true;
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
        for (field, input) in CreateField::ALL.into_iter().zip(&mut self.create_inputs) {
            input.attach(crate::resource::create_field_dispatcher(&dispatcher, field));
        }
        self.dispatcher = Some(dispatcher);
    }

    /// The draft as the document holds it now. A past version with a draft
    /// shows the form, which is all it holds.
    pub(crate) fn present_draft(&mut self, draft: &CreateDraft) {
        draft.clone_into(&mut self.draft);
        if self.past {
            self.create_open = !draft.is_empty();
        }
    }

    /// Show `section`, with nothing selected in it.
    pub(crate) fn show_section(&mut self, section: Section) {
        self.view.section = section;
        self.view.selected = None;
        self.list_scroll = 0.0;
        self.structure_dirty = true;
    }

    /// Select the container `id` (`None` clears the selection).
    pub(crate) fn select_container(&mut self, id: Option<String>) {
        if id.is_some() {
            self.view.section = Section::Containers;
        }
        self.view.selected = id;
        self.detail_scroll = 0.0;
        self.structure_dirty = true;
    }

    /// Show the selected container's `tab`.
    pub(crate) fn show_tab(&mut self, tab: DetailTab) {
        self.view.tab = tab;
        self.detail_scroll = 0.0;
        self.structure_dirty = true;
    }

    /// Open the create-container form on its image field.
    pub(crate) fn open_create(&mut self) {
        self.create_open = true;
        self.focus_create_field(CreateField::Image);
    }

    /// Replace the text of `field` in the draft, as one edit.
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

    /// Empty the draft, once a container was created from it.
    fn clear_create(&self, doc: &mut AutoCommit) {
        if let Some(dispatcher) = &self.dispatcher {
            let _ = dispatcher.apply_now(doc, &crate::resource::ContainersAction::ClearCreate);
        }
    }

    /// The live rows of the engine, a bounded page of them, and an
    /// in-flight pull.
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

    /// The live row `id`, wherever it is in the engine.
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

    /// A live engine with one running container, its detail fetched when
    /// `hydrated`.
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
        let mut app = Self::with_engine(engine, InputFactories::controlled());
        app.live = true;
        app.bg_tx = Some(bg_tx);
        app.bg_rx = Some(bg_rx);
        app.controlled_runtime = Some(requests_tx);
        app.controlled_immediate_results = Some(results_rx);
        (app, requests_rx, results_tx)
    }

    pub(crate) fn show_toast(&mut self, message: impl Into<String>, error: bool) {
        self.toast = Some(Toast {
            message: message.into(),
            error,
            born: ui::time::now(),
        });
    }

    /// Kick a background podman re-probe so stats stay fresh without blocking
    /// the UI thread. No-op offline.
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

    /// Drain any background snapshot, preserving per-container detail
    /// (logs/env/mounts) for the selected container that `ps` doesn't carry.
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
            self.search_index.sync(&self.engine);
        }
    }

    /// Hydrate the selected container's detail (logs + inspect) on demand.
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
            self.search_index.sync(&self.engine);
        }
    }

    /// Apply a lifecycle action, then re-probe. Destructive ones come through
    /// the confirm dialog so this is only ever invoked post-confirmation.
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

    /// Mutate the in-memory snapshot to mirror a lifecycle verb when no
    /// engine is present. Keeps the demo interactive.
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
        self.search_index.sync(&self.engine);
        self.structure_dirty = true;
    }

    // --- streaming pull -------------------------------------------------

    /// Start a streaming `podman pull` of `reference` on a worker thread,
    /// opening the progress overlay. Offline it simulates a quick success so
    /// the demo surface responds.
    pub(crate) fn start_pull(&mut self, reference: &str) {
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

    /// Fold one streamed pull message into the progress overlay.
    fn apply_pull_msg(&mut self, msg: StreamMsg) {
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

    // --- image / volume actions ----------------------------------------

    /// Remove an image (post-confirmation). Force-removes so an unused tag
    /// goes even if a stopped container references it.
    pub(crate) fn remove_image(&mut self, reference: &str) {
        if self.engine.live {
            let _ = self.pending_control_invocation.emit(ControlInvocation {
                address: ActionAddress::new(format!("image:{reference}"), "remove"),
                arguments: serde_json::Map::new(),
            });
            self.show_toast(format!("Remove {reference} requested"), false);
        } else if !self.live {
            self.engine.images.retain(|i| i.reference() != reference);
            self.search_index.sync(&self.engine);
            self.show_toast(format!("Removed {reference}"), false);
            self.structure_dirty = true;
        } else {
            self.show_toast("Container engine is offline", true);
        }
    }

    /// Tag an image with a new reference.
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
            self.search_index.sync(&self.engine);
            self.show_toast(format!("Created volume {name}"), false);
            self.structure_dirty = true;
        }
    }

    /// Remove a named volume (post-confirmation).
    pub(crate) fn remove_volume(&mut self, name: &str) {
        if self.engine.live {
            let _ = self.pending_control_invocation.emit(ControlInvocation {
                address: ActionAddress::new(format!("volume:{name}"), "remove"),
                arguments: serde_json::Map::new(),
            });
            self.show_toast(format!("Remove volume {name} requested"), false);
        } else if !self.live {
            self.engine.volumes.retain(|v| v.name != name);
            self.search_index.sync(&self.engine);
            self.show_toast(format!("Removed volume {name}"), false);
            self.structure_dirty = true;
        } else {
            self.show_toast("Container engine is offline", true);
        }
    }

    /// Attach a real interactive shell to a running container by spawning the
    /// Tilo terminal app running `podman exec -it <id> <shell>`.
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
            self.create_open = false;
            self.combo.close();
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
            self.create_open = false;
            self.combo.close();
        }
    }

    /// The containers in list order: running first, then by name.
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

    /// Seed the shared palette with this app's containers and open it.
    ///
    /// The palette is the OS-shared Ctrl+K widget; the app only supplies
    /// the rows (its containers) and consumes the outcome. Enter on a row
    /// selects that container — the same "find and jump to" the old inline
    /// search bar did, but through the shared search surface.
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

    /// Forward a pointer event to the header search launcher. The launcher
    /// is a non-editable pill (see [`ui::widget::SearchField::opens_palette`])
    /// that hit-tests off its own rect, so a throwaway taffy node satisfies
    /// the `Widget` signature without a real layout tree.
    fn route_to_launcher(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        doc: &mut AutoCommit,
        path: &ObjId,
        focus_target: &mut Option<NodeId>,
    ) {
        let mut dummy_taffy: TaffyTree<()> = TaffyTree::new();
        // A single-leaf throwaway tree; this can't realistically fail, but
        // skip the forward rather than panic if it ever does.
        let Ok(dummy_node) = dummy_taffy.new_leaf(Style::default()) else {
            return;
        };
        let mut ignored = false;
        self.search_field.handle_event(
            event,
            &mut ui::widget::EventCtx {
                scale_factor,
                modifiers: &modifiers,
                state: doc,
                path,
                handled: &mut ignored,
                focus_target,
                node_id: dummy_node,
                taffy: &dummy_taffy,
            },
        );
    }
}

// =====================================================================
// Shared-modal dialog wiring (destructive confirm + text prompt)
// =====================================================================

impl ContainersApp {
    /// Build the destructive-confirm dialog from the current `confirm`
    /// metadata. The affirmative button is destructive (danger-coloured).
    fn build_confirm_dialog(&self) -> Option<ConfirmDlg> {
        let confirm = self.confirm.as_ref()?;
        let title = format!("{}?", confirm.verb);
        let body = format!(
            "This permanently removes \u{201c}{}\u{201d}. This cannot be undone.",
            confirm.name
        );
        let ok = Rc::new(Cell::new(false));
        let cancel = Rc::new(Cell::new(false));
        let dialog = ModalDialog::new(title, Text::new(&body).with_size(13.5))
            .open(true)
            .with_max_size(420.0, 190.0)
            .with_destructive(true)
            .primary_action("Remove", {
                let c = ok.clone();
                move || c.set(true)
            })
            .secondary_action("Cancel", {
                let c = cancel.clone();
                move || c.set(true)
            })
            .on_dismiss({
                let c = cancel.clone();
                move || c.set(true)
            });
        Some(ConfirmDlg {
            dialog,
            confirm: ok,
            cancel,
        })
    }

    /// Build the text-prompt dialog from the current `prompt` metadata. The
    /// field is a real [`PromptBody`]/[`TextInput`], seeded with any staged
    /// value.
    fn build_prompt_dialog(&self) -> Option<PromptDlg> {
        let prompt = self.prompt.as_ref()?;
        let primary = match prompt.kind {
            PromptKind::NewVolume if prompt.context == "__pull__" => "Pull",
            PromptKind::NewVolume => "Create",
            PromptKind::TagImage => "Tag",
        };
        let body = PromptBody::new("", &prompt.placeholder).with_initial(&prompt.value);
        let submit = Rc::new(Cell::new(false));
        let cancel = Rc::new(Cell::new(false));
        let dialog = ModalDialog::new(prompt.title.clone(), body)
            .open(true)
            .with_max_size(440.0, 200.0)
            .primary_action(primary, {
                let s = submit.clone();
                move || s.set(true)
            })
            .secondary_action("Cancel", {
                let c = cancel.clone();
                move || c.set(true)
            })
            .on_dismiss({
                let c = cancel.clone();
                move || c.set(true)
            });
        Some(PromptDlg {
            dialog,
            submit,
            cancel,
        })
    }

    fn ensure_confirm_dialog(&mut self) {
        if self.confirm_dialog.is_none() {
            self.confirm_dialog = self.build_confirm_dialog();
        }
    }

    fn ensure_prompt_dialog(&mut self) {
        if self.prompt_dialog.is_none() {
            self.prompt_dialog = self.build_prompt_dialog();
        }
    }

    /// Lay a shared dialog out against the app rect so its box + footer
    /// buttons resolve. Generic over which dialog slot is active.
    fn layout_confirm_dialog(&mut self, theme: &Theme, tr: &mut TextRenderer) {
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return;
        }
        let rect = self.rect;
        if let Some(cd) = self.confirm_dialog.as_mut() {
            cd.dialog.layout_in(rect, theme, tr);
        }
    }

    fn layout_prompt_dialog(&mut self, theme: &Theme, tr: &mut TextRenderer) {
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return;
        }
        let rect = self.rect;
        if let Some(pd) = self.prompt_dialog.as_mut() {
            pd.dialog.layout_in(rect, theme, tr);
        }
    }

    /// Route an event into the open confirm dialog, then apply the flags its
    /// callbacks set (Enter is a submit convenience).
    fn route_confirm_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        doc: &mut AutoCommit,
        handled: &mut bool,
    ) {
        if enter_pressed(event) {
            self.apply_confirm(doc);
            *handled = true;
            return;
        }
        self.ensure_confirm_dialog();
        let viewport = self.rect;
        let (ok, cancel) = {
            let Some(cd) = self.confirm_dialog.as_mut() else {
                return;
            };
            if cd.dialog.dialog_rect() == Rect::ZERO {
                cd.dialog
                    .layout_in(viewport, &Theme::dark(), &mut TextRenderer::new());
            }
            cd.dialog
                .handle_detached(event, scale_factor, modifiers, doc, &automerge::ROOT);
            (cd.confirm.replace(false), cd.cancel.replace(false))
        };
        if ok {
            self.apply_confirm(doc);
        } else if cancel {
            self.confirm = None;
            self.confirm_dialog = None;
        }
        *handled = true;
    }

    /// Route an event into the open prompt dialog, then apply the flags its
    /// callbacks set (Enter is a submit convenience). Reads the committed text
    /// from the embedded real field.
    fn route_prompt_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        doc: &mut AutoCommit,
        handled: &mut bool,
    ) {
        if enter_pressed(event) {
            self.submit_prompt(doc);
            *handled = true;
            return;
        }
        self.ensure_prompt_dialog();
        let viewport = self.rect;
        let (submit, cancel) = {
            let Some(pd) = self.prompt_dialog.as_mut() else {
                return;
            };
            if pd.dialog.dialog_rect() == Rect::ZERO {
                pd.dialog
                    .layout_in(viewport, &Theme::dark(), &mut TextRenderer::new());
            }
            pd.dialog
                .handle_detached(event, scale_factor, modifiers, doc, &automerge::ROOT);
            (pd.submit.replace(false), pd.cancel.replace(false))
        };
        if submit {
            self.submit_prompt(doc);
        } else if cancel {
            self.prompt = None;
            self.prompt_dialog = None;
        }
        *handled = true;
    }

    /// Apply the pending destructive confirm against the real engine/document.
    fn apply_confirm(&mut self, doc: &mut AutoCommit) {
        if let Some(confirm) = self.confirm.take() {
            match confirm.target {
                ConfirmTarget::Container { id, action } => {
                    self.apply_action(action, &id);
                    if self.view.selected.as_deref() == Some(id.as_str()) {
                        self.select_container(None);
                    }
                }
                ConfirmTarget::Image { reference } => self.remove_image(&reference),
                ConfirmTarget::Volume { name } => self.remove_volume(&name),
            }
        }
        self.confirm_dialog = None;
        self.refresh_view(doc);
    }

    /// Read the prompt's typed value out of the embedded field and commit it.
    fn submit_prompt(&mut self, doc: &mut AutoCommit) {
        let value = self
            .prompt_dialog
            .as_ref()
            .map(|pd| pd.dialog.content().value().to_string())
            .unwrap_or_default();
        self.commit_prompt(value.trim());
        self.prompt_dialog = None;
        self.refresh_view(doc);
    }
}

/// Whether `event` is a key-press of Enter (the modal submit convenience).
fn enter_pressed(event: &WindowEvent) -> bool {
    matches!(
        event,
        WindowEvent::KeyboardInput { event: ke, .. }
            if ke.state == ElementState::Pressed
                && matches!(ke.logical_key, Key::Named(winit::keyboard::NamedKey::Enter))
    )
}

// =====================================================================
// Accessibility mirror for the immediate-mode surface
// =====================================================================

impl ContainersApp {
    /// Stable semantic identity for a painted hit. Dynamic targets use the
    /// engine object's stable id/reference rather than its visible row index,
    /// so an assistive technology can retain focus across refreshes/re-sorts.
    fn semantic_target(&self, hit: &Hit) -> String {
        match hit {
            Hit::Section(section) => format!("containers.section:{}", section.as_str()),
            Hit::NewContainer => "containers.create.open".into(),
            Hit::SelectContainer(id) => format!("containers.container:{id}:select"),
            Hit::SelectImage(index) => self.engine.images.get(*index).map_or_else(
                || format!("containers.image-index:{index}:select"),
                |image| format!("containers.image:{}:select", image.reference()),
            ),
            Hit::Action { id, action } => {
                format!(
                    "containers.container:{id}:{}",
                    action.label().to_ascii_lowercase()
                )
            }
            Hit::ImageAct { reference, action } => {
                format!(
                    "containers.image:{reference}:{}",
                    action.label().to_ascii_lowercase()
                )
            }
            Hit::PullImage(reference) => format!("containers.image:{reference}:pull"),
            Hit::PullTyped => "containers.image.pull".into(),
            Hit::TagImage(reference) => format!("containers.image:{reference}:tag"),
            Hit::InspectImage(reference) => format!("containers.image:{reference}:inspect"),
            Hit::CreateVolume => "containers.volume.create".into(),
            Hit::RemoveVolume(name) => format!("containers.volume:{name}:remove"),
            Hit::AttachShell(id) => format!("containers.container:{id}:shell"),
            Hit::DistroEnter(name) => format!("containers.distrobox:{name}:enter"),
            Hit::DistroCreate => "containers.distrobox.create".into(),
            Hit::DetailTab(tab) => format!("containers.detail.tab:{}", tab.as_str()),
            Hit::CloseDetail => "containers.detail.close".into(),
            Hit::CreateField(field) => format!("containers.create.field:{}", field.key()),
            Hit::CreateSubmit => "containers.create.submit".into(),
            Hit::CreateCancel => "containers.create.cancel".into(),
            Hit::ComboToggle => "containers.create.image-options".into(),
            Hit::ComboPick(index) => format!("containers.create.image-option:{index}"),
            Hit::ToggleSidebar => "containers.sidebar.toggle".into(),
            Hit::PromptCancel => "containers.progress.close".into(),
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one flat match giving every hit region its accessible role, name and description"
    )]
    fn semantic_copy(&self, hit: &Hit) -> (Role, String, Option<String>, AccessState) {
        let focusable = AccessState::new().focusable(true);
        match hit {
            Hit::Section(section) => (
                Role::Tab,
                section.label().into(),
                Some(format!(
                    "Shortcut: {}",
                    match section {
                        Section::Containers => 1,
                        Section::Images => 2,
                        Section::Volumes => 3,
                        Section::Distrobox => 4,
                    }
                )),
                focusable.selected(self.view.section == *section),
            ),
            Hit::NewContainer => (
                Role::Button,
                "New container".into(),
                Some("Shortcut: N".into()),
                focusable,
            ),
            Hit::SelectContainer(id) => {
                let container = self.engine.find(id);
                let name = container.map_or_else(|| id.clone(), |c| c.name.clone());
                let detail = container
                    .map(|c| format!("{}; {}; {}", c.image, c.status.label(), c.status_detail));
                (
                    Role::ListItem,
                    name,
                    detail,
                    focusable.selected(self.view.selected.as_deref() == Some(id)),
                )
            }
            Hit::SelectImage(index) => {
                let image = self.engine.images.get(*index);
                (
                    Role::ListItem,
                    image.map_or_else(|| "Image".into(), crate::model::Image::reference),
                    image.map(|i| format!("{}; {}", ui::format::bytes(i.size_bytes), i.created)),
                    focusable,
                )
            }
            Hit::Action { id, action } => (
                Role::Button,
                format!(
                    "{} {}",
                    action.label(),
                    self.engine
                        .find(id)
                        .map_or(id.as_str(), |c| c.name.as_str())
                ),
                None,
                focusable,
            ),
            Hit::ImageAct { reference, action } => (
                Role::Button,
                format!("{} {reference}", action.label()),
                None,
                focusable,
            ),
            Hit::PullImage(reference) => (
                Role::Button,
                format!("Pull image {reference}"),
                Some("Download or update this image".into()),
                focusable,
            ),
            Hit::PullTyped => (
                Role::Button,
                "Pull image".into(),
                Some("Download the image named in the form".into()),
                focusable,
            ),
            Hit::TagImage(reference) => (
                Role::Button,
                format!("Tag image {reference}"),
                None,
                focusable,
            ),
            Hit::InspectImage(reference) => (
                Role::Button,
                format!("Inspect image {reference}"),
                None,
                focusable,
            ),
            Hit::CreateVolume => (Role::Button, "New volume".into(), None, focusable),
            Hit::RemoveVolume(name) => (
                Role::Button,
                format!("Remove volume {name}"),
                None,
                focusable,
            ),
            Hit::AttachShell(id) => (
                Role::Button,
                format!(
                    "Open shell in {}",
                    self.engine
                        .find(id)
                        .map_or(id.as_str(), |c| c.name.as_str())
                ),
                None,
                focusable,
            ),
            Hit::DistroEnter(name) => (
                Role::Button,
                format!("Enter distrobox {name}"),
                None,
                focusable,
            ),
            Hit::DistroCreate => (Role::Button, "Create distrobox".into(), None, focusable),
            Hit::DetailTab(tab) => (
                Role::Tab,
                tab.label().into(),
                None,
                focusable.selected(self.view.tab == *tab),
            ),
            Hit::CloseDetail => (
                Role::Button,
                "Back to containers".into(),
                Some("Shortcut: Escape".into()),
                focusable,
            ),
            Hit::CreateField(field) => (
                Role::TextInput,
                field.label().into(),
                Some(field.placeholder().into()),
                focusable.focused(self.create_focus == *field),
            ),
            Hit::CreateSubmit => (
                Role::Button,
                "Create container".into(),
                None,
                focusable.disabled(
                    self.create_input(CreateField::Image)
                        .value()
                        .trim()
                        .is_empty(),
                ),
            ),
            Hit::CreateCancel => (
                Role::Button,
                "Cancel".into(),
                Some("Shortcut: Escape".into()),
                focusable,
            ),
            Hit::ComboToggle => (
                Role::ComboBox,
                "Installed image suggestions".into(),
                None,
                focusable.expanded(self.combo.open),
            ),
            Hit::ComboPick(index) => {
                let options = self.engine.image_references();
                let name = options
                    .get(*index)
                    .cloned()
                    .unwrap_or_else(|| format!("Image option {}", index + 1));
                (
                    Role::ListItem,
                    name,
                    None,
                    focusable.selected(self.combo.highlight == *index),
                )
            }
            Hit::ToggleSidebar => (
                Role::Button,
                if self.sidebar_flap.is_open(SplitLayout::Docked) {
                    "Collapse navigation"
                } else {
                    "Expand navigation"
                }
                .into(),
                Some("Shortcut: B".into()),
                focusable,
            ),
            Hit::PromptCancel => (Role::Button, "Close progress".into(), None, focusable),
        }
    }
}

// =====================================================================
// Widget impl
// =====================================================================

/// Keep palette authority refreshes proportional to the visible context.
/// Search retains the complete engine catalogue and `Widget::resource`
/// materialises an omitted stable identity only when a result is activated.
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

fn container_row(container: &crate::model::Container, engine_live: bool) -> LiveRow {
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

impl ContainersApp {
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

    fn sync(&mut self, doc: &mut AppModel, path: &ModelPath) {
        let (doc, _) = ui::doc::document(doc, path);
        self.drain_bg();
        // Periodically re-probe the engine for fresh stats / state. Controlled
        // fixtures own that cadence through the request channel; a wall-clock
        // tick would race their emptiness asserts under a slow runner.
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
        self.refresh_view(doc);
    }

    fn needs_idle_sync(&self) -> bool {
        // Keep syncing while live so the background poller's snapshots and
        // the toast timeout are picked up without needing input. A running
        // pull also streams in via the bg channel.
        self.live
            || self.toast.is_some()
            || self
                .pull
                .as_ref()
                .is_some_and(|p| !p.done && p.failed.is_none())
    }

    fn needs_animation_frame(&self) -> bool {
        // Toast fade, the sidebar collapse slide, and live pull progress all
        // keep the host pumping frames.
        (!self.motion_reduced && self.toast.is_some())
            || self.sidebar_flap.is_animating(SplitLayout::Docked)
            || (!self.motion_reduced
                && self
                    .pull
                    .as_ref()
                    .is_some_and(|p| !p.done && p.failed.is_none()))
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The widget self-lays-out in apply_layout/draw, so it only needs a
        // single full-bleed leaf node.
        let node = match taffy.new_leaf(Style {
            size: Size {
                width: percent(1.0_f32),
                height: percent(1.0_f32),
            },
            ..Default::default()
        }) {
            Ok(node) => node,
            // `new_leaf` only errs when the tree is at capacity; degrade to
            // an inert node rather than aborting the whole session.
            Err(_) => NodeId::from(0u64),
        };
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
    }

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.structure_dirty)
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // Install the host's DPI transform so every draw helper this frame
        // composes onto it (HiDPI-correct without threading it everywhere).
        draw::set_base(transform);
        // Advance the sidebar collapse slide off a real per-frame delta.
        self.tick_sidebar();
        // Expire the toast.
        if let Some(t) = &self.toast
            && t.born.elapsed().as_millis() > TOAST_MS
        {
            self.toast = None;
        }
        self.hits.clear();
        let r = self.rect;
        let c = &theme.tokens.colors;

        // App background.
        scene.fill(Fill::NonZero, transform, c.surfaces.base, None, &r);

        // The draw pass has no document access; it renders from the view
        // snapshot refreshed in `sync` / `handle_event`.
        let view = self.view.clone();

        // --- Navigation rail ---
        // Compact is deliberately a navigation rail, not a squeezed desktop
        // sidebar. It keeps every section one tap away while leaving a full
        // pane for the current list or detail.
        let compact = self.uses_compact_layout(theme);
        self.compact_layout_active = compact;
        let rail = Rect::new(r.x0, r.y0, r.x0 + self.rail_width_for(compact), r.y1);
        self.draw_rail(scene, tr, theme, rail, &view, compact);

        // --- Content area ---
        let content = Rect::new(rail.x1, r.y0, r.x1, r.y1);
        match view.section {
            Section::Containers => self.draw_containers(scene, tr, theme, content, &view),
            Section::Images => self.draw_images(scene, tr, theme, content),
            Section::Volumes => self.draw_volumes(scene, tr, theme, content),
            Section::Distrobox => self.draw_distrobox(scene, tr, theme, content),
        }

        // --- Overlays ---
        if self.create_open {
            self.draw_create_modal(scene, tr, theme, presence, r);
        }
        if self.pull.is_some() {
            self.draw_pull_overlay(scene, tr, theme, r);
        }
        if self.prompt.is_some() {
            self.ensure_prompt_dialog();
            self.layout_prompt_dialog(theme, tr);
            if let Some(pd) = self.prompt_dialog.as_mut() {
                pd.dialog.draw(scene, transform, tr, theme, presence);
            }
        } else {
            self.prompt_dialog = None;
        }
        if self.confirm.is_some() {
            self.ensure_confirm_dialog();
            self.layout_confirm_dialog(theme, tr);
            if let Some(cd) = self.confirm_dialog.as_mut() {
                cd.dialog.draw(scene, transform, tr, theme, presence);
            }
        } else {
            self.confirm_dialog = None;
        }
        if let Some(toast) = &self.toast {
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
                r,
                ToastAnchor::BottomCenter,
                &ToastCard::new(&toast.message, kind),
            );
        }

        // --- Shared command palette (topmost) ---
        if self.palette_open {
            // Dim the app behind the palette for legibility.
            scene.fill(
                Fill::NonZero,
                transform,
                ui::paint::with_alpha(c.surfaces.scrim, 128),
                None,
                &r,
            );
            let (pw, ph) = self.palette.desired_size();
            let cx = r.center().x;
            let top = r.y0 + 88.0;
            let card = Rect::new(cx - pw / 2.0, top, cx + pw / 2.0, top + ph);
            self.palette.set_rect(card);
            self.palette.draw(scene, transform, tr, theme, presence);
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the top-level dispatcher coordinates overlays, shared inputs, and app navigation"
    )]
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // While open, the shared palette owns all input. It runs against the
        // live document; an Enter outcome selects the chosen container, and
        // Escape (or clicking away) closes it.
        if self.palette_open {
            let mut dummy_taffy: TaffyTree<()> = TaffyTree::new();
            // A single-leaf throwaway tree; this can't realistically fail,
            // but skip the forward rather than panic if it ever does.
            if let Ok(dummy_node) = dummy_taffy.new_leaf(Style::default()) {
                self.palette.handle_event(
                    event,
                    &mut ui::widget::EventCtx {
                        taffy: &dummy_taffy,
                        ..ctx.reborrow(ctx.path, dummy_node)
                    },
                );
                if !self.palette.is_open() {
                    self.palette_open = false;
                }
            }
            *ctx.handled = true;
            self.refresh_view(ctx.state);
            return;
        }

        // Ctrl+K opens the app-scoped palette — the same chord as the shell's
        // global search, here seeded with this app's resources.
        if let WindowEvent::KeyboardInput {
            event: key_event, ..
        } = event
            && key_event.state == ElementState::Pressed
            && ctx.modifiers.state().control_key()
            && matches!(&key_event.logical_key, Key::Character(c) if c.eq_ignore_ascii_case("k"))
        {
            self.open_container_palette();
            *ctx.handled = true;
            self.refresh_view(ctx.state);
            return;
        }

        // A destructive-confirm or text-prompt dialog is modal: while it's up
        // the shared ModalDialog captures every event (typing, clicks, Esc)
        // and nothing leaks to the app behind the scrim.
        if self.confirm.is_some() {
            self.route_confirm_event(
                event,
                ctx.scale_factor,
                *ctx.modifiers,
                ctx.state,
                ctx.handled,
            );
            self.refresh_view(ctx.state);
            return;
        }
        if self.prompt.is_some() {
            self.route_prompt_event(
                event,
                ctx.scale_factor,
                *ctx.modifiers,
                ctx.state,
                ctx.handled,
            );
            self.refresh_view(ctx.state);
            return;
        }

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                if self.create_open {
                    for field in CreateField::ALL {
                        self.route_create_input(
                            field,
                            event,
                            ctx.scale_factor,
                            *ctx.modifiers,
                            ctx.state,
                            ctx.path,
                            ctx.focus_target,
                        );
                    }
                }
                // Keep the launcher's pointer position current for its click test.
                self.route_to_launcher(
                    event,
                    ctx.scale_factor,
                    *ctx.modifiers,
                    ctx.state,
                    ctx.path,
                    ctx.focus_target,
                );
                // Hover changes button shading — request a redraw.
                *ctx.handled = true;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * 48.0,
                    MouseScrollDelta::PixelDelta(p) => p.y / ctx.scale_factor,
                };
                // Scroll the panel under the cursor. A compact selected
                // container *is* the detail panel, so it must not leak wheel
                // events into an invisible master list.
                let compact_detail = self.compact_layout_active
                    && self.view.section == Section::Containers
                    && self.view.selected.is_some();
                if compact_detail
                    || self.cursor.x > self.rect.x0 + RAIL_WIDTH + self.list_area_width(ctx.state)
                {
                    self.detail_scroll = (self.detail_scroll - dy).max(0.0);
                } else {
                    self.list_scroll = (self.list_scroll - dy).max(0.0);
                }
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                // A click on the search launcher opens the palette instead of
                // running a list hit.
                self.route_to_launcher(
                    event,
                    ctx.scale_factor,
                    *ctx.modifiers,
                    ctx.state,
                    ctx.path,
                    ctx.focus_target,
                );
                if self.search_field.take_palette_request() {
                    self.open_container_palette();
                    *ctx.handled = true;
                } else {
                    self.on_click(ctx.state, ctx.handled);
                    if self.create_open {
                        self.route_create_input(
                            self.create_focus,
                            event,
                            ctx.scale_factor,
                            *ctx.modifiers,
                            ctx.state,
                            ctx.path,
                            ctx.focus_target,
                        );
                    }
                }
            }
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } => {
                self.on_key(event, ctx, key_event);
            }
            WindowEvent::Ime(_) if self.create_open => {
                let field = self.create_focus;
                self.focus_create_field(field);
                self.route_create_input(
                    field,
                    event,
                    ctx.scale_factor,
                    *ctx.modifiers,
                    ctx.state,
                    ctx.path,
                    ctx.focus_target,
                );
                *ctx.handled = true;
            }
            _ => {}
        }
        // Refresh the cached view state after any mutation so the next draw
        // reflects it (draw has no doc access).
        self.refresh_view(ctx.state);
    }

    fn name(&self) -> &'static str {
        "ContainersApp"
    }

    fn test_id(&self) -> Option<&str> {
        Some("containers-root")
    }

    fn tooltip(&self) -> Option<&str> {
        let hit = self
            .hits
            .iter()
            .rev()
            .find(|region| region.rect.contains(self.cursor))?;
        Some(match &hit.hit {
            Hit::Section(section) => section.label(),
            Hit::NewContainer | Hit::DistroCreate | Hit::CreateSubmit => "Create container",
            Hit::SelectContainer(_) => "Open container details",
            Hit::SelectImage(_) => "Show image details",
            Hit::Action { action, .. } => action.label(),
            Hit::ImageAct { action, .. } => action.label(),
            Hit::PullImage(_) | Hit::PullTyped => "Pull image",
            Hit::TagImage(_) => "Tag image",
            Hit::InspectImage(_) => "Inspect image",
            Hit::CreateVolume => "Create volume",
            Hit::RemoveVolume(_) => "Remove volume",
            Hit::AttachShell(_) => "Open interactive shell",
            Hit::DistroEnter(_) => "Enter distrobox",
            Hit::DetailTab(tab) => tab.label(),
            Hit::CloseDetail => "Back to containers (Esc)",
            Hit::CreateField(field) => field.label(),
            Hit::CreateCancel => "Cancel (Esc)",
            Hit::ComboToggle => "Show installed images",
            Hit::ComboPick(_) => "Use this image",
            Hit::ToggleSidebar => "Toggle navigation (B)",
            Hit::PromptCancel => "Close",
        })
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.hits
            .iter()
            .rev()
            .any(|region| region.rect.contains(point))
            .then_some(winit::window::CursorIcon::Pointer)
    }

    fn traps_focus(&self) -> bool {
        self.palette_open
            || self.create_open
            || self.pull.is_some()
            || self.prompt.is_some()
            || self.confirm.is_some()
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.traps_focus().then_some(self.rect)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the immediate-mode accessibility mirror exports the complete visible surface"
    )]
    fn accessibility(&self) -> Option<AccessNode> {
        if self.palette_open {
            return self.palette.accessibility();
        }
        if let Some(dialog) = self
            .confirm_dialog
            .as_ref()
            .and_then(|dialog| dialog.dialog.accessibility())
        {
            return Some(dialog);
        }
        if let Some(dialog) = self
            .prompt_dialog
            .as_ref()
            .and_then(|dialog| dialog.dialog.accessibility())
        {
            return Some(dialog);
        }

        let modal_prefix = if self.create_open {
            Some("containers.create.")
        } else if self.pull.is_some() {
            Some("containers.progress.")
        } else {
            None
        };
        let mut targets = std::collections::HashSet::new();
        let mut controls = Vec::new();
        if modal_prefix.is_none() && self.search_field.get_rect() != Rect::ZERO {
            controls.push(
                AccessNode::new(Role::Button)
                    .with_name("Open search")
                    .with_description("Shortcut: Control K or slash")
                    .with_action_target("containers.search.open")
                    .with_bounds(self.search_field.get_rect())
                    .with_state(AccessState::new().focusable(true)),
            );
            targets.insert("containers.search.open".to_string());
        }
        for region in &self.hits {
            let target = self.semantic_target(&region.hit);
            if modal_prefix.is_some_and(|prefix| !target.starts_with(prefix))
                || !targets.insert(target.clone())
            {
                continue;
            }
            let (role, name, description, state) = self.semantic_copy(&region.hit);
            let mut node = AccessNode::new(role)
                .with_name(name)
                .with_action_target(target)
                .with_bounds(region.rect)
                .with_state(state);
            if let Some(description) = description {
                node = node.with_description(description);
            }
            if let Hit::CreateField(field) = &region.hit {
                let input = self.create_input(*field);
                node = node.with_value(input.value());
                let (anchor, focus) = input
                    .selection_anchor_focus_public()
                    .unwrap_or_else(|| (input.cursor_pos(), input.cursor_pos()));
                node = node.with_text_selection(anchor, focus);
            }
            controls.push(node);
        }

        let mut children = if let Some(prefix) = modal_prefix {
            let name = if prefix == "containers.create." {
                "Create container"
            } else {
                "Image pull progress"
            };
            vec![
                AccessNode::new(Role::Dialog)
                    .with_name(name)
                    .with_bounds(if prefix == "containers.create." {
                        crate::presentation::create_form_geometry(self.rect).modal
                    } else {
                        self.rect
                    })
                    .with_state(AccessState::new().modal(true))
                    .with_children(controls),
            ]
        } else {
            controls
        };
        if let Some(pull) = &self.pull {
            children.push(pull_progress_node(pull));
        }
        if let Some(toast) = &self.toast {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(&toast.message)
                    .with_live(if toast.error {
                        Politeness::Assertive
                    } else {
                        Politeness::Polite
                    }),
            );
        }
        Some(
            AccessNode::new(Role::Window)
                .with_name("Containers")
                .with_description(&self.engine.connection)
                .with_bounds(self.rect)
                .with_children(children),
        )
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one semantic target table for every painted a11y control"
    )]
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &AccessibilityAction,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        let (state, _) = ui::doc::document(state, path);
        if self.palette_open {
            return self
                .palette
                .perform_semantic_action(target, action, state, &automerge::ROOT);
        }
        if self.confirm.is_some() {
            self.ensure_confirm_dialog();
            let handled = self.confirm_dialog.as_mut().is_some_and(|dialog| {
                dialog
                    .dialog
                    .perform_semantic_action(target, action, state, &automerge::ROOT)
            });
            let (confirm, cancel) = self
                .confirm_dialog
                .as_ref()
                .map_or((false, false), |dialog| {
                    (dialog.confirm.replace(false), dialog.cancel.replace(false))
                });
            if confirm {
                self.apply_confirm(state);
            } else if cancel {
                self.confirm = None;
                self.confirm_dialog = None;
            }
            return handled;
        }
        if target == "containers.search.open"
            && matches!(
                action,
                AccessibilityAction::Activate | AccessibilityAction::Focus
            )
        {
            self.open_container_palette();
            return true;
        }
        if self.prompt.is_some() {
            self.ensure_prompt_dialog();
            let handled = self.prompt_dialog.as_mut().is_some_and(|dialog| {
                dialog
                    .dialog
                    .perform_semantic_action(target, action, state, &automerge::ROOT)
            });
            let (submit, cancel) = self
                .prompt_dialog
                .as_ref()
                .map_or((false, false), |dialog| {
                    (dialog.submit.replace(false), dialog.cancel.replace(false))
                });
            if submit {
                self.submit_prompt(state);
            } else if cancel {
                self.prompt = None;
                self.prompt_dialog = None;
            }
            return handled;
        }
        if let Some(field_key) = target.strip_prefix("containers.create.field:") {
            let Some(field) = CreateField::ALL
                .into_iter()
                .find(|field| field.key() == field_key)
            else {
                return false;
            };
            match action {
                AccessibilityAction::Focus | AccessibilityAction::Activate => {
                    self.apply_hit(state, Hit::CreateField(field));
                    self.refresh_view(state);
                    return true;
                }
                AccessibilityAction::SetValue(value) => {
                    self.set_create_field(state, field, value);
                    self.focus_create_field(field);
                    self.refresh_view(state);
                    return true;
                }
                _ => return false,
            }
        }
        if !matches!(
            action,
            AccessibilityAction::Activate | AccessibilityAction::Focus
        ) {
            return false;
        }
        let hit = self
            .hits
            .iter()
            .find(|region| self.semantic_target(&region.hit) == target)
            .map(|region| region.hit.clone());
        if let Some(hit) = hit {
            self.apply_hit(state, hit);
            self.refresh_view(state);
            true
        } else {
            false
        }
    }

    fn set_motion_reduced(&mut self, reduced: bool) {
        self.motion_reduced = reduced;
        if reduced {
            self.sidebar_flap.settle(SplitLayout::Docked);
            self.sidebar_tick = None;
        }
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.has_focus = focused;
    }

    fn is_focused(&self) -> bool {
        self.has_focus
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        true
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
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
        self.palette.install_control_custody(domain, attachment);
        let _ = self
            .pending_control_invocation
            .install_custody(domain, attachment);
    }
    fn take_control_invocation(&mut self) -> Option<ui::tilo_actions::ControlInvocation> {
        if let Some(invocation) = self.palette.take_control_invocation() {
            return Some(invocation);
        }
        self.pending_control_invocation.take_legacy_compatibility()
    }
    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        if self.palette_open {
            self.palette.control_action_bindings()
        } else {
            let mut seen = std::collections::HashSet::new();
            let mut bindings = vec![ui::widget::ControlBindingMetadata::semantic(
                "containers.search.open",
            )];
            bindings.extend(
                self.hits
                    .iter()
                    .filter_map(|region| {
                        let role = self.semantic_copy(&region.hit).0;
                        if !matches!(
                            role,
                            Role::Button
                                | Role::ToggleButton
                                | Role::Checkbox
                                | Role::RadioButton
                                | Role::Switch
                                | Role::ComboBox
                                | Role::Slider
                                | Role::SpinButton
                                | Role::TextInput
                                | Role::MultilineTextInput
                        ) {
                            return None;
                        }
                        let control_id = self.semantic_target(&region.hit);
                        if !seen.insert(control_id.clone()) {
                            return None;
                        }
                        let action = match &region.hit {
                            Hit::Action { id, action } => Some(ActionAddress::new(
                                format!("container:{id}"),
                                match action {
                                    Action::Start => "start",
                                    Action::Stop => "stop",
                                    Action::Restart => "restart",
                                    Action::Pause => "pause",
                                    Action::Unpause => "unpause",
                                    Action::Remove => "remove",
                                },
                            )),
                            Hit::AttachShell(id) => {
                                Some(ActionAddress::new(format!("container:{id}"), "shell"))
                            }
                            Hit::ImageAct { reference, .. } => {
                                Some(ActionAddress::new(format!("image:{reference}"), "remove"))
                            }
                            Hit::RemoveVolume(name) => {
                                Some(ActionAddress::new(format!("volume:{name}"), "remove"))
                            }
                            Hit::DistroEnter(name) => {
                                Some(ActionAddress::new(format!("distrobox:{name}"), "enter"))
                            }
                            _ => None,
                        };
                        Some(ui::widget::ControlBindingMetadata {
                            control_id: control_id.clone(),
                            kind: action.map_or_else(
                                || ui::widget::ControlBindingKind::SemanticAction(control_id),
                                ui::widget::ControlBindingKind::Action,
                            ),
                        })
                    })
                    .collect::<Vec<_>>(),
            );
            bindings
        }
    }
}

// =====================================================================
// Where the user is
//
// The section, the selected container and its detail tab are this window's
// own: never in the document, never shared, never undone.
// =====================================================================

#[derive(Clone)]
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

/// The live progress bar announcing an image pull's current status.
fn pull_progress_node(pull: &PullProgress) -> AccessNode {
    let status = pull.failed.as_ref().map_or_else(
        || {
            if pull.done {
                format!("Finished pulling {}", pull.reference)
            } else {
                pull.lines
                    .last()
                    .cloned()
                    .unwrap_or_else(|| format!("Pulling {}", pull.reference))
            }
        },
        |error| format!("Pull failed: {error}"),
    );
    AccessNode::new(Role::ProgressBar)
        .with_name("Image pull")
        .with_value(status)
        .with_live(Politeness::Polite)
        .with_state(AccessState::new().busy(!pull.done && pull.failed.is_none()))
}

impl ContainersApp {
    /// Stage a screenshot scenario (`None` for a plain `--shots`): the
    /// section, selection and tab it shows, and the overlay it has open. The
    /// engine and the draft are the scenario's already.
    pub fn stage_scene(&mut self, scenario: Option<&str>) {
        let scene = scenario.unwrap_or_default();
        match scene {
            "images" | "pull" => self.show_section(Section::Images),
            "volumes" => self.show_section(Section::Volumes),
            "distrobox" => self.show_section(Section::Distrobox),
            "create" | "combo" => {
                self.open_create();
                // The image suggestion dropdown, open over the form.
                self.combo.open = scene == "combo";
            }
            "collapsed" => self.sidebar_flap.snap(false),
            // The shared Ctrl+K palette, seeded with the sample containers.
            "palette" => self.open_container_palette(),
            "confirm" => {
                let (name, id) = self.engine.containers.first().map_or_else(
                    || ("web".to_string(), "web".to_string()),
                    |c| (c.name.clone(), c.id.clone()),
                );
                self.confirm = Some(Confirm {
                    name,
                    verb: "Remove container".into(),
                    target: ConfirmTarget::Container {
                        id,
                        action: Action::Remove,
                    },
                });
            }
            "prompt" => {
                self.prompt = Some(Prompt {
                    kind: PromptKind::NewVolume,
                    title: "New volume".into(),
                    placeholder: "my-volume".into(),
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
    }

    #[allow(
        clippy::expect_used,
        reason = "CreateField::COUNT and CreateField::ALL define the fixed six-slot form together"
    )]
    pub(crate) fn create_input_mut(&mut self, field: CreateField) -> &mut TextInput {
        self.create_inputs
            .get_mut(field.index())
            .expect("every CreateField has one shared input")
            .control_mut()
    }

    #[allow(
        clippy::expect_used,
        reason = "CreateField::COUNT and CreateField::ALL define the fixed six-slot form together"
    )]
    pub(crate) fn create_input(&self, field: CreateField) -> &TextInput {
        self.create_inputs
            .get(field.index())
            .expect("every CreateField has one shared input")
            .control()
    }

    pub(crate) fn focus_create_field(&mut self, field: CreateField) {
        self.create_focus = field;
        let open = self.create_open;
        for candidate in CreateField::ALL {
            Widget::set_focus(self.create_input_mut(candidate), candidate == field && open);
        }
    }

    pub(crate) fn blur_create_fields(&mut self) {
        for field in CreateField::ALL {
            Widget::set_focus(self.create_input_mut(field), false);
        }
    }

    /// Hand `event` to the field's control, which submits what it typed
    /// as a splice of the draft. Typing in the image field opens its
    /// suggestions.
    #[allow(
        clippy::expect_used,
        reason = "CreateField::COUNT and CreateField::ALL define the fixed six-slot form together"
    )]
    pub(crate) fn route_create_input(
        &mut self,
        field: CreateField,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        doc: &mut AutoCommit,
        path: &ObjId,
        focus_target: &mut Option<NodeId>,
    ) {
        let before = self.create_input(field).value().to_string();
        let mut taffy = TaffyTree::new();
        let Ok(node) = taffy.new_leaf(Style::default()) else {
            return;
        };
        let mut ignored = false;
        Widget::handle_event(
            self.create_inputs
                .get_mut(field.index())
                .expect("every CreateField has one shared input"),
            event,
            &mut ui::widget::EventCtx {
                scale_factor,
                modifiers: &modifiers,
                state: doc,
                path,
                handled: &mut ignored,
                focus_target,
                node_id: node,
                taffy: &taffy,
            },
        );
        if field == CreateField::Image && self.create_input(field).value() != before {
            self.combo.open = true;
            self.combo.highlight = 0;
        }
    }

    /// Show the draft's texts in the form's fields, and focus the form's
    /// field while it is open.
    pub(crate) fn refresh_view(&mut self, doc: &mut AutoCommit) {
        for input in &mut self.create_inputs {
            input.pull(doc);
        }
        if self.create_open {
            self.focus_create_field(self.create_focus);
        } else {
            self.blur_create_fields();
        }
    }

    /// Width of the list panel (full content width when no detail is open,
    /// otherwise content minus the detail column).
    fn list_area_width(&self, _doc: &AutoCommit) -> f64 {
        // Event routing has no Theme reference. This conservative viewport
        // check still prevents wheel events from being routed into an invisible
        // detail column on phone-sized surfaces; large-text layouts are also
        // single-pane when drawn.
        let compact = self.compact_layout_active || self.rect.width() < COMPACT_VIEWPORT_WIDTH;
        let content_w = (self.rect.width() - self.rail_width_for(compact)).max(0.0);
        if self.view.selected.is_some() && self.view.section == Section::Containers && !compact {
            (content_w - DETAIL_WIDTH).max(240.0)
        } else {
            content_w
        }
    }

    /// The compact breakpoint is intentionally based on both available width
    /// and the rendered text scale. This makes 200% text a first-class layout
    /// mode instead of letting it silently overlap a desktop geometry.
    pub(crate) fn compact_layout_for(viewport_width: f64, base_text_size: f32) -> bool {
        viewport_width < COMPACT_VIEWPORT_WIDTH || base_text_size >= LARGE_TEXT_BASE_SIZE
    }

    pub(crate) fn uses_compact_layout(&self, theme: &Theme) -> bool {
        Self::compact_layout_for(self.rect.width(), theme.tokens.typography.scale.base)
    }

    /// Whether lifecycle affordances should be painted. The deterministic
    /// populated sample remains interactive for demos/tests; a genuinely empty
    /// disconnected engine does not advertise controls that can only fail.
    pub(crate) fn engine_controls_available(&self) -> bool {
        self.engine.live
            || (!self.live
                && (!self.engine.containers.is_empty()
                    || !self.engine.images.is_empty()
                    || !self.engine.volumes.is_empty()
                    || !self.engine.distroboxes.is_empty()))
    }

    /// A selected container is a full-page drill-in on compact surfaces. The
    /// close/back control and Escape return to the list; no content is hidden
    /// in a sliver beside it.
    pub(crate) fn uses_single_pane_detail(&self, theme: &Theme) -> bool {
        self.uses_compact_layout(theme)
            && self.view.section == Section::Containers
            && self.view.selected.is_some()
    }

    pub(crate) fn header_height(&self, theme: &Theme) -> f64 {
        if self.uses_compact_layout(theme) {
            120.0
        } else {
            HEADER_HEIGHT
        }
    }

    pub(crate) fn row_height(theme: &Theme) -> f64 {
        if theme.tokens.typography.scale.base >= LARGE_TEXT_BASE_SIZE {
            84.0
        } else {
            ROW_HEIGHT
        }
    }

    /// The current (animated) navigation-rail width. Eased between fully
    /// expanded (`RAIL_WIDTH`) and a thin collapsed strip that still shows
    /// the section icons, using the shared `ui` flap kernel.
    pub fn rail_width(&self) -> f64 {
        let reveal = self.sidebar_flap.reveal();
        RAIL_COLLAPSED + (RAIL_WIDTH - RAIL_COLLAPSED) * reveal
    }

    fn rail_width_for(&self, compact: bool) -> f64 {
        if compact {
            RAIL_COLLAPSED
        } else {
            self.rail_width()
        }
    }

    /// `true` once the rail has revealed enough to show its text labels.
    pub fn rail_expanded(&self) -> bool {
        self.sidebar_flap.reveal() > 0.5
    }

    /// Toggle the sidebar collapsed/expanded.
    pub(crate) fn toggle_sidebar(&mut self) {
        self.sidebar_flap.toggle(SplitLayout::Docked);
    }

    // --- public test / embedding hooks ---------------------------------
    //
    // The app renders with manual hit-testing rather than the retained widget
    // tree, so its transient UI state isn't reachable through child widgets.
    // These thin accessors let the e2e suite drive the create flow + combobox
    // through the same fields the production event path mutates.

    /// Open the create-container modal with the image combobox focused +
    /// dropdown shown (the create-with-combobox flow).
    pub fn open_create_with_combo(&mut self) {
        self.create_open = true;
        self.focus_create_field(CreateField::Image);
        self.combo.open = true;
        self.combo.highlight = 0;
    }

    /// The section shown.
    pub fn section(&self) -> Section {
        self.view.section
    }

    /// The selected container's id.
    pub fn selected(&self) -> Option<&str> {
        self.view.selected.as_deref()
    }

    /// The selected container's detail tab.
    pub fn tab(&self) -> DetailTab {
        self.view.tab
    }

    /// Whether the create modal is open.
    pub fn is_create_open(&self) -> bool {
        self.create_open
    }

    /// Whether the image combobox suggestion dropdown is showing.
    pub fn is_combo_open(&self) -> bool {
        self.combo.open
    }

    /// Advance the sidebar slide off a real per-frame delta (driven from the
    /// draw path, mirroring `NavSplit::self_tick`). The first draw sees
    /// `dt == 0` so the rail never teleports.
    pub(crate) fn tick_sidebar(&mut self) {
        let now = ui::time::now();
        let dt = self
            .sidebar_tick
            .map_or(0.0, |t| now.duration_since(t).as_secs_f64().min(0.1));
        self.sidebar_tick = Some(now);
        self.sidebar_flap.tick(SplitLayout::Docked, dt);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::event::KeyEvent;
    use winit::keyboard::{Key, KeyLocation, NamedKey, PhysicalKey};

    /// Build a synthetic key-press. `handle_event` only inspects
    /// `logical_key` + `state`; the rest tolerate the zero pattern.
    fn key_press(logical_key: Key) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = logical_key;
        ev.physical_key = PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }

    fn dummy_layout() -> (TaffyTree<()>, NodeId) {
        let mut t = TaffyTree::<()>::new();
        let n = t.new_leaf(Style::default()).unwrap();
        (t, n)
    }

    /// The compact breakpoint preserves tablet and desktop while protecting large text, and compact selection is a full-page drill-in.
    #[expect(
        clippy::float_cmp,
        reason = "layout reproduces authored whole-pixel sizes and their sums exactly in binary floating point; exact equality is what the test checks"
    )]
    #[test]
    fn compact_layout_drills_in_and_protects_large_text() {
        // --- phase: compact_breakpoint_preserves_tablet_desktop_but_protects_large_text ---
        {
            let normal = ui::theme::Theme::dark();
            let large = ui::theme::Theme::dark().with_text_scale(2.0);
            assert!(
                !ContainersApp::compact_layout_for(768.0, normal.tokens.typography.scale.base),
                "a normal-size tablet keeps the familiar split layout"
            );
            assert!(
                ContainersApp::compact_layout_for(390.0, normal.tokens.typography.scale.base),
                "a phone never receives a squeezed sidebar"
            );
            assert!(
                ContainersApp::compact_layout_for(768.0, large.tokens.typography.scale.base),
                "200% text gets a readable single-pane layout even on tablet"
            );
        }

        // --- phase: compact_selection_is_a_full_page_drill_in ---
        {
            let mut app = ContainersApp::sample();
            app.rect = Rect::new(0.0, 0.0, 390.0, 844.0);
            app.view.selected = app
                .engine
                .containers
                .first()
                .map(|container| container.id.clone());
            let theme = ui::theme::Theme::dark();

            assert!(app.uses_compact_layout(&theme));
            assert!(app.uses_single_pane_detail(&theme));
            assert_eq!(app.header_height(&theme), 120.0);
            assert_eq!(ContainersApp::row_height(&theme), ROW_HEIGHT);

            let large = ui::theme::Theme::dark().with_text_scale(2.0);
            app.rect = Rect::new(0.0, 0.0, 768.0, 900.0);
            assert!(app.uses_single_pane_detail(&large));
            assert_eq!(ContainersApp::row_height(&large), 84.0);
        }
    }

    /// The palette seeds one row per container and Enter selects it; confirm and prompt dialogs remove images and create volumes.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn palette_and_dialogs_drive_container_operations() {
        // --- phase: open_palette_seeds_one_row_per_container ---
        {
            let mut app = ContainersApp::sample();
            assert!(!app.palette_open);
            let count = app.engine.containers.len();
            assert!(count > 0, "sample engine has containers to search");

            app.open_container_palette();
            assert!(app.palette_open, "the launcher opens the shared palette");

            let rows: Vec<String> = app
                .palette
                .visible_entries()
                .iter()
                .map(|e| e.label.clone())
                .collect();
            assert_eq!(rows.len(), count, "one palette row per container");
            assert!(
                app.engine
                    .containers
                    .iter()
                    .all(|c| rows.iter().any(|r| r.contains(&c.name))),
                "every container is reachable through the shared palette"
            );
        }

        // --- phase: enter_on_a_palette_row_selects_that_container ---
        {
            let mut app = ContainersApp::sample();
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;

            app.open_container_palette();
            let expected = app.engine.containers[0].id.clone();

            let (taffy, node) = dummy_layout();
            let mut handled = false;
            let mut focus = None;
            app.handle_event(
                &key_press(Key::Named(NamedKey::Enter)),
                &mut ui::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &winit::event::Modifiers::default(),
                    state: &mut doc,
                    path: &root,
                    handled: &mut handled,
                    focus_target: &mut focus,
                    node_id: node,
                    taffy: &taffy,
                },
            );
            app.dispatch_palette_invocation_for_test(&mut doc);

            assert!(!app.palette_open, "Enter closes the palette");
            assert_eq!(
                app.view.selected.as_deref(),
                Some(expected.as_str()),
                "activating a palette row selects that container"
            );
        }

        // --- phase: confirm_dialog_primary_removes_image_and_escape_cancels ---
        {
            let mut app = ContainersApp::sample();
            app.rect = Rect::new(0.0, 0.0, 900.0, 700.0);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let reference = app.engine.images[0].reference();
            let before = app.engine.images.len();
            app.confirm = Some(Confirm {
                name: reference.clone(),
                verb: "Remove image".into(),
                target: ConfirmTarget::Image { reference },
            });
            // A cursor move builds + lays out the dialog so its primary rect resolves.
            dispatch(
                &mut app,
                &mut doc,
                &root,
                &cursor_at(Point::new(10.0, 10.0)),
            );
            let primary = app.confirm_dialog.as_ref().unwrap().dialog.primary_rect();
            dispatch(&mut app, &mut doc, &root, &cursor_at(primary.center()));
            dispatch(&mut app, &mut doc, &root, &left_press());
            assert!(
                app.confirm.is_none(),
                "confirm closed after the primary click"
            );
            assert!(app.confirm_dialog.is_none());
            assert_eq!(
                app.engine.images.len(),
                before - 1,
                "the image was removed on confirm"
            );

            let mut b = ContainersApp::sample();
            b.rect = Rect::new(0.0, 0.0, 900.0, 700.0);
            let refb = b.engine.images[0].reference();
            let beforeb = b.engine.images.len();
            b.confirm = Some(Confirm {
                name: refb.clone(),
                verb: "Remove image".into(),
                target: ConfirmTarget::Image { reference: refb },
            });
            dispatch(
                &mut b,
                &mut doc,
                &root,
                &key_press(Key::Named(NamedKey::Escape)),
            );
            assert!(b.confirm.is_none(), "Escape cancelled the confirm");
            assert_eq!(b.engine.images.len(), beforeb, "Escape removed nothing");
        }

        // --- phase: prompt_dialog_creates_volume_from_typed_field ---
        {
            let mut app = ContainersApp::sample();
            app.rect = Rect::new(0.0, 0.0, 900.0, 700.0);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let before = app.engine.volumes.len();
            app.prompt = Some(Prompt {
                kind: PromptKind::NewVolume,
                title: "New volume".into(),
                placeholder: "my-volume".into(),
                value: String::new(),
                context: String::new(),
            });
            dispatch(
                &mut app,
                &mut doc,
                &root,
                &cursor_at(Point::new(10.0, 10.0)),
            );
            for ch in ["d", "a", "t", "a"] {
                dispatch(&mut app, &mut doc, &root, &char_press(ch));
            }
            assert_eq!(
                app.prompt_dialog.as_ref().unwrap().dialog.content().value(),
                "data",
                "keystrokes land in the embedded real TextInput",
            );
            let primary = app.prompt_dialog.as_ref().unwrap().dialog.primary_rect();
            dispatch(&mut app, &mut doc, &root, &cursor_at(primary.center()));
            dispatch(&mut app, &mut doc, &root, &left_press());
            assert!(app.prompt.is_none(), "prompt closed after submit");
            assert_eq!(app.engine.volumes.len(), before + 1, "a volume was created");
            assert!(
                app.engine.volumes.iter().any(|v| v.name == "data"),
                "the volume is named from the typed field value",
            );
        }
    }

    // --- shared-ModalDialog confirm / prompt routing --------------------

    fn cursor_at(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        }
    }
    fn left_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }
    fn char_press(ch: &str) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Character(ch.into());
        ev.physical_key = PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = Some(ch.into());
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }
    fn dispatch(app: &mut ContainersApp, doc: &mut AutoCommit, root: &ObjId, ev: &WindowEvent) {
        let (taffy, node) = dummy_layout();
        let mut handled = false;
        let mut focus = None;
        app.handle_event(
            ev,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: doc,
                path: root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
    }

    /// Lifecycle controls emit the exported effect instead of running podman inline, and the resource snapshot is bounded with lazy identity resolution.
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
}
