//! Root widget for the container manager.
//!
//! `ContainersApp` is a single self-laying-out widget that owns the engine
//! snapshot (containers / images / distroboxes mirrored from rootless
//! podman) and renders the whole surface — a left navigation rail, a master
//! list with inline lifecycle actions, a tabbed detail view, a create-
//! container form, and confirm/create modals. Navigation and selection state
//! live in the shared CRDT document (see [`crate::doc`]) so undo/redo and
//! live collaboration work without extra plumbing; the engine projection is
//! transient host data and stays in memory.
//!
//! The widget hit-tests in `handle_event` against rectangles it recorded
//! during `draw`, which keeps the dense, data-driven layout (variable row
//! counts, wrapped port chips, tab strips) under direct control without
//! fighting a flexbox tree.

use std::sync::mpsc::{Receiver, Sender};
use std::time::Instant;

use automerge::{AutoCommit, ObjId};
use std::cell::Cell;
use std::rc::Rc;
use ui::PathSegment;
use ui::a11y::{AccessNode, AccessState, Action as AccessibilityAction, Politeness, Role};
use ui::presence::PresenceCtx;
use ui::qos_actions::{
    ActionAddress, ActionSpec, ControlInvocation, OperationKind, PermissionRequirement, Resource,
    RetryPolicy, flatten,
};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::command_palette::{CommandEntry, CommandPalette};
use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind, paint_transient};
use ui::widget::{
    FlapState, ModalDialog, PromptBody, SearchField, SplitLayout, Text, TextInput, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::Key;

use taffy::prelude::*;

use crate::combobox::ComboState;
use crate::doc::{CreateField, DetailTab, Section, ViewDoc};
use crate::draw;
use crate::model::{Container, Engine};
use crate::podman::{self, Action, ImageAction, StreamMsg};

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

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent, orthogonal flag (live engine vs. \
              sample data, one-shot demo seeding, and separate open/dirty \
              states for unrelated overlays); they aren't a state machine, \
              and grouping them would mean churning every field access across \
              this widget and panels.rs for no behavioural gain"
)]
pub struct ContainersApp {
    /// The live (or sample) engine projection.
    pub(crate) engine: Engine,
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
    /// Shared CRDT-bound editors for the six dynamic create-form fields.
    pub(crate) create_inputs: [TextInput; CreateField::COUNT],
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
    /// Selection requested through the resource/action door. Applied during
    /// `sync`, where the framework deliberately provides mutable document
    /// access for collaborative view state.
    pending_resource_selection: Option<String>,
    /// Navigation requested by a canonical resource View operation. Applied
    /// on sync so it enters the same collaborative document path as clicks.
    pending_resource_section: Option<Section>,
    pending_resource_tab: Option<DetailTab>,
    /// In-app lifecycle controls emit the same inert resource intent as Ctrl+K
    /// and agents. The host re-resolves authority, records the Effect, then
    /// calls `invoke_resource_operation`.
    pub(crate) pending_control_invocation: Option<ControlInvocation>,
    /// Collapsible sidebar reveal kernel (the shared `ui` flap animation).
    pub(crate) sidebar_flap: FlapState,
    /// Wall-clock anchor for advancing the sidebar slide from the draw path.
    pub(crate) sidebar_tick: Option<Instant>,
    /// Current toast.
    pub(crate) toast: Option<Toast>,
    /// Background refresh plumbing. `None` in headless/shots mode.
    bg_rx: Option<Receiver<Bg>>,
    bg_tx: Option<Sender<Bg>>,
    last_refresh: Instant,
    pub(crate) live: bool,
    /// In offline/demo mode, seed an initial selection on the first sync so a
    /// fresh document renders a populated detail view (notably for `--shots`).
    seed_demo: bool,
    seeded: bool,
    /// Whether a structural change happened this frame (forces relayout).
    pub(crate) structure_dirty: bool,
    /// Cached snapshot of the CRDT-backed view state. Refreshed in `sync`
    /// and at the tail of every `handle_event`; `draw` reads it because the
    /// draw pass has no document access.
    view: ViewState,
    /// Layout mode used by the last drawn frame. Event routing deliberately
    /// follows this instead of guessing from width alone, because 200% text
    /// also switches the surface to a single detail pane.
    compact_layout_active: bool,
    /// Framework motion policy. Time-driven state still updates through idle
    /// sync, but decorative toast/progress repaint loops stop when reduced.
    motion_reduced: bool,
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
        let create_inputs = std::array::from_fn(|index| {
            let field = CreateField::ALL
                .get(index)
                .copied()
                .unwrap_or(CreateField::Name);
            TextInput::new(vec![
                PathSegment::Field("container_create".into()),
                PathSegment::Field(field.key().into()),
            ])
            .with_id(&format!("container_create_{}", field.key()))
            .with_placeholder(field.placeholder())
            .with_font_size(13.0)
            .with_plain_chrome()
        });
        Self {
            engine,
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
            combo: ComboState::default(),
            pull: None,
            search_field: SearchField::new()
                .opens_palette()
                .with_placeholder("Search containers…"),
            palette: CommandPalette::commands(),
            palette_open: false,
            pending_resource_selection: None,
            pending_resource_section: None,
            pending_resource_tab: None,
            pending_control_invocation: None,
            sidebar_flap: FlapState::open_when_docked(),
            sidebar_tick: None,
            toast: None,
            bg_rx,
            bg_tx,
            last_refresh: Instant::now(),
            live,
            seed_demo: !live,
            seeded: false,
            structure_dirty: false,
            view: ViewState::default(),
            compact_layout_active: false,
            motion_reduced: false,
        }
    }

    pub(crate) fn show_toast(&mut self, message: impl Into<String>, error: bool) {
        self.toast = Some(Toast {
            message: message.into(),
            error,
            born: Instant::now(),
        });
    }

    /// Kick a background podman re-probe so stats stay fresh without blocking
    /// the UI thread. No-op offline.
    fn spawn_refresh(&mut self) {
        if !self.live {
            return;
        }
        let Some(tx) = self.bg_tx.clone() else { return };
        self.last_refresh = Instant::now();
        std::thread::spawn(move || {
            let engine = podman::probe().engine;
            let _ = tx.send(Bg::Snapshot(engine));
        });
    }

    /// Drain any background snapshot, preserving per-container detail
    /// (logs/env/mounts) for the selected container that `ps` doesn't carry.
    fn drain_bg(&mut self, doc: &mut AutoCommit) {
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
        }
    }

    /// Hydrate the selected container's detail (logs + inspect) on demand.
    fn hydrate_selection(&mut self, doc: &mut AutoCommit) {
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
        let logs = podman::fetch_logs(&sel, 200);
        if let Some(c) = self.engine.containers.iter_mut().find(|c| c.id == sel) {
            podman::inspect_into(c);
            if !logs.is_empty() {
                c.logs = logs;
            }
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
            self.pending_control_invocation = Some(ControlInvocation {
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
            self.pending_control_invocation = Some(ControlInvocation {
                address: ActionAddress::new(format!("image:{reference}"), "remove"),
                arguments: serde_json::Map::new(),
            });
            self.show_toast(format!("Remove {reference} requested"), false);
        } else if !self.live {
            self.engine.images.retain(|i| i.reference() != reference);
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
            self.show_toast(format!("Created volume {name}"), false);
            self.structure_dirty = true;
        }
    }

    /// Remove a named volume (post-confirmation).
    pub(crate) fn remove_volume(&mut self, name: &str) {
        if self.engine.live {
            self.pending_control_invocation = Some(ControlInvocation {
                address: ActionAddress::new(format!("volume:{name}"), "remove"),
                arguments: serde_json::Map::new(),
            });
            self.show_toast(format!("Remove volume {name} requested"), false);
        } else if !self.live {
            self.engine.volumes.retain(|v| v.name != name);
            self.show_toast(format!("Removed volume {name}"), false);
            self.structure_dirty = true;
        } else {
            self.show_toast("Container engine is offline", true);
        }
    }

    /// Attach a real interactive shell to a running container by spawning the
    /// qOS terminal app running `podman exec -it <id> <shell>`.
    pub(crate) fn attach_shell(&mut self, id: &str) {
        let short = id.chars().take(12).collect::<String>();
        if self.engine.live {
            self.pending_control_invocation = Some(ControlInvocation {
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
        let args = ViewDoc::create_run_args(doc);
        let Some(args) = args else {
            self.show_toast("Image is required to create a container", true);
            return;
        };
        let name = ViewDoc::create_field(doc, CreateField::Image);
        if self.live {
            // Run `podman run` off the UI thread; report the result back via a
            // toast streamed through the bg channel, then re-probe.
            self.show_toast(format!("Creating container from {name}…"), false);
            ViewDoc::clear_create(doc);
            self.create_open = false;
            self.combo.close();
            if let Some(tx) = self.bg_tx.clone() {
                std::thread::spawn(move || {
                    let out = std::process::Command::new("podman").args(&args).output();
                    // Whatever the result, push a fresh snapshot so the new
                    // container appears (or the failure is implied by absence).
                    let _ = out;
                    let _ = tx.send(Bg::Snapshot(podman::probe().engine));
                });
            }
        } else {
            self.show_toast(format!("podman {}", args.join(" ")), false);
            ViewDoc::clear_create(doc);
            self.create_open = false;
            self.combo.close();
        }
    }

    /// Filtered containers for the current search query, sorted running-first.
    pub(crate) fn visible_containers(&self, query: &str) -> Vec<&Container> {
        let mut v: Vec<&Container> = self
            .engine
            .containers
            .iter()
            .filter(|c| c.matches(query))
            .collect();
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
        let resources = self.resources(&AutoCommit::new());
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
        let intent = self
            .take_control_invocation()
            .expect("palette emitted a resource action");
        let resources = self.resources(doc);
        let spec = ui::qos_actions::door::resolve(&resources, &intent.address)
            .expect("palette address resolves");
        let kind = spec.kind;
        let encoded = spec
            .build_json(&intent.arguments)
            .expect("valid palette arguments");
        assert!(
            self.invoke_resource_operation(
                kind,
                &encoded.type_id,
                &encoded.payload,
                doc,
                &automerge::ROOT,
            )
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
            scale_factor,
            &modifiers,
            doc,
            path,
            &mut ignored,
            focus_target,
            dummy_node,
            &dummy_taffy,
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
                    if ViewDoc::selected(doc).as_deref() == Some(id.as_str()) {
                        ViewDoc::set_selected(doc, None);
                    }
                }
                ConfirmTarget::Image { reference } => self.remove_image(&reference),
                ConfirmTarget::Volume { name } => self.remove_volume(&name),
            }
        }
        self.confirm_dialog = None;
        self.refresh_view(doc);
    }

    /// Seed a modal for a deterministic `--shots` frame (no event loop). The
    /// draw pass builds + lays out the shared dialog from this metadata.
    pub fn seed_shot(&mut self, scenario: &str) {
        match scenario {
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
                    image.map_or_else(|| "Image".into(), |i| i.reference()),
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

impl Widget for ContainersApp {
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

    fn resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
        let mut root = Resource::new("containers", "containers.engine", "Containers");
        if let Ok(spec) = ActionSpec::operation(
            "create",
            "Create container",
            OperationKind::View,
            "containers.engine",
            &["create"],
        ) {
            let spec = if self.engine.live {
                spec
            } else {
                spec.unavailable(
                    "engine_offline",
                    "Creating containers requires a connected Podman engine",
                )
            };
            root.actions.push(spec);
        }
        for container in &self.engine.containers {
            let mut resource = Resource::new(
                format!("container:{}", container.id),
                "containers.container",
                container.name.clone(),
            );
            let selection = vec!["select".to_string(), container.id.clone()];
            if let Ok(spec) = ActionSpec::operation(
                "select",
                format!("Open {}", container.name),
                OperationKind::View,
                "containers.container",
                &selection,
            ) {
                resource.actions.push(spec);
            }
            let verbs: &[(&str, &str)] = match container.status {
                crate::model::ContainerStatus::Running
                | crate::model::ContainerStatus::Restarting => &[
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
                let payload = vec![(*verb).to_string(), container.id.clone()];
                if let Ok(spec) = ActionSpec::operation(
                    *verb,
                    *label,
                    OperationKind::Effect,
                    "containers.container",
                    &payload,
                ) {
                    let mut spec = spec.requiring(PermissionRequirement::Admin).privileged();
                    if *verb == "remove" {
                        spec = spec.destructive();
                    } else if *verb != "restart" && *verb != "shell" {
                        spec = spec.retry_policy(RetryPolicy::Idempotent);
                    }
                    if !self.engine.live {
                        spec = spec.unavailable(
                            "engine_offline",
                            "Container engine actions are unavailable in demo mode",
                        );
                    }
                    resource.actions.push(spec);
                }
            }
            let mut logs = Resource::new(
                format!("container:{}:logs", container.id),
                "containers.logs",
                format!("{} logs", container.name),
            );
            if let Ok(spec) = ActionSpec::operation(
                "open",
                "Open logs",
                OperationKind::View,
                "containers.logs",
                &["logs", container.id.as_str()],
            ) {
                logs.actions.push(spec);
            }
            resource.children.push(logs);
            root.children.push(resource);
        }
        for image in &self.engine.images {
            let reference = image.reference();
            let payload = vec!["remove".to_string(), reference.clone()];
            let mut resource = Resource::new(
                format!("image:{reference}"),
                "containers.image",
                reference.clone(),
            );
            if let Ok(spec) = ActionSpec::operation(
                "open",
                "Show image",
                OperationKind::View,
                "containers.image",
                &["open", reference.as_str()],
            ) {
                resource.actions.push(spec);
            }
            if let Ok(spec) = ActionSpec::operation(
                "remove",
                "Remove image",
                OperationKind::Effect,
                "containers.image",
                &payload,
            ) {
                let mut spec = spec
                    .requiring(PermissionRequirement::Admin)
                    .privileged()
                    .destructive();
                if !self.engine.live {
                    spec = spec.unavailable(
                        "engine_offline",
                        "Container engine actions are unavailable in demo mode",
                    );
                }
                resource.actions.push(spec);
            }
            root.children.push(resource);
        }
        for volume in &self.engine.volumes {
            let payload = vec!["remove".to_string(), volume.name.clone()];
            let mut resource = Resource::new(
                format!("volume:{}", volume.name),
                "containers.volume",
                volume.name.clone(),
            );
            if let Ok(spec) = ActionSpec::operation(
                "open",
                "Show volume",
                OperationKind::View,
                "containers.volume",
                &["open", volume.name.as_str()],
            ) {
                resource.actions.push(spec);
            }
            if let Ok(spec) = ActionSpec::operation(
                "remove",
                "Remove volume",
                OperationKind::Effect,
                "containers.volume",
                &payload,
            ) {
                let mut spec = spec
                    .requiring(PermissionRequirement::Admin)
                    .privileged()
                    .destructive();
                if !self.engine.live {
                    spec = spec.unavailable(
                        "engine_offline",
                        "Container engine actions are unavailable in demo mode",
                    );
                }
                resource.actions.push(spec);
            }
            root.children.push(resource);
        }
        for distrobox in &self.engine.distroboxes {
            let mut resource = Resource::new(
                format!("distrobox:{}", distrobox.name),
                "containers.distrobox",
                distrobox.name.clone(),
            );
            if let Ok(spec) = ActionSpec::operation(
                "open",
                "Show distrobox",
                OperationKind::View,
                "containers.distrobox",
                &["open", distrobox.name.as_str()],
            ) {
                resource.actions.push(spec);
            }
            if let Ok(spec) = ActionSpec::operation(
                "enter",
                "Enter distrobox",
                OperationKind::Effect,
                "containers.distrobox",
                &["enter", distrobox.name.as_str()],
            ) {
                let mut spec = spec.requiring(PermissionRequirement::Admin).privileged();
                if !self.engine.live {
                    spec = spec.unavailable(
                        "engine_offline",
                        "Distrobox actions are unavailable in demo mode",
                    );
                }
                resource.actions.push(spec);
            }
            root.children.push(resource);
        }
        vec![root]
    }

    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(
            crate::search_provider::ContainerSearchProvider::new(&self.engine),
        )]
    }

    fn invoke_resource_operation(
        &mut self,
        kind: OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        let args: Vec<String> = serde_json::from_slice(payload).map_err(|e| e.to_string())?;
        let Some(verb) = args.first().map(String::as_str) else {
            return Err("missing operation".into());
        };
        let target = args.get(1).map(String::as_str).unwrap_or_default();
        if kind == OperationKind::View && type_id == "containers.engine" && verb == "create" {
            self.create_open = true;
            self.focus_create_field(CreateField::Image);
            return Ok(true);
        }
        if kind == OperationKind::View && type_id == "containers.container" && verb == "select" {
            self.pending_resource_selection = Some(target.to_string());
            return Ok(true);
        }
        if kind == OperationKind::View && type_id == "containers.logs" && verb == "logs" {
            self.pending_resource_selection = Some(target.to_string());
            self.pending_resource_section = Some(Section::Containers);
            self.pending_resource_tab = Some(DetailTab::Logs);
            return Ok(true);
        }
        if kind == OperationKind::View && verb == "open" {
            self.pending_resource_section = Some(match type_id {
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

    fn sync(&mut self, doc: &mut AutoCommit, _path: &ObjId) {
        if let Some(target) = self.pending_resource_selection.take() {
            ViewDoc::set_selected(doc, Some(&target));
        }
        if let Some(section) = self.pending_resource_section.take() {
            ViewDoc::set_section(doc, section);
        }
        if let Some(tab) = self.pending_resource_tab.take() {
            ViewDoc::set_tab(doc, tab);
        }
        self.drain_bg(doc);
        // Seed a representative selection on the very first sync of a demo /
        // shots session so the detail view is populated out of the box.
        if self.seed_demo && !self.seeded {
            self.seeded = true;
            // An optional env hook selects a demo scene for screenshots
            // (`containers` / `images` / `volumes` / `distrobox` / `stats` /
            // `inspect` / `create` / `combo` / `pull` / `collapsed`).
            let scene = std::env::var("QOS_CONTAINERS_DEMO").unwrap_or_default();
            match scene.as_str() {
                "images" => ViewDoc::set_section(doc, Section::Images),
                "volumes" => ViewDoc::set_section(doc, Section::Volumes),
                "distrobox" => ViewDoc::set_section(doc, Section::Distrobox),
                "create" | "combo" => {
                    self.create_open = true;
                    self.create_focus = CreateField::Image;
                    ViewDoc::set_create_field(doc, CreateField::Name, "web-frontend");
                    ViewDoc::set_create_field(doc, CreateField::Image, "nginx");
                    ViewDoc::set_create_field(doc, CreateField::Ports, "8080:80, 8443:443");
                    ViewDoc::set_create_field(doc, CreateField::Env, "TZ=UTC");
                    ViewDoc::set_create_field(
                        doc,
                        CreateField::Volumes,
                        "site:/usr/share/nginx/html",
                    );
                    if scene == "combo" {
                        // Open the image suggestion dropdown for the shot.
                        self.combo.open = true;
                    }
                }
                "pull" => {
                    ViewDoc::set_section(doc, Section::Images);
                    self.pull = Some(PullProgress {
                        reference: "docker.io/library/alpine:3.20".into(),
                        lines: vec![
                            "Trying to pull docker.io/library/alpine:3.20...".into(),
                            "Getting image source signatures".into(),
                            "Copying blob sha256:9fa9226be034 [===========>     ] 2.1MiB / 3.4MiB"
                                .into(),
                            "Copying config sha256:1a2b3c4d5e6f done".into(),
                        ],
                        done: false,
                        failed: None,
                    });
                }
                "collapsed" => self.sidebar_flap.snap(false),
                // Capture the honest "no engine" zero-state (what the live app
                // shows when rootless podman is not reachable).
                "empty" => {
                    self.engine = Engine {
                        connection: "No container engine — is podman installed?".into(),
                        live: false,
                        ..Engine::default()
                    };
                }
                // Open the shared Ctrl+K palette (seeded with the sample
                // containers) so `--shots` can capture the search surface.
                "palette" => self.open_container_palette(),
                _ => {}
            }
            let tab = match scene.as_str() {
                "stats" => DetailTab::Stats,
                "inspect" => DetailTab::Inspect,
                "terminal" => DetailTab::Terminal,
                _ => DetailTab::Logs,
            };
            let list_scene = matches!(scene.as_str(), "images" | "volumes" | "distrobox" | "pull");
            if !list_scene
                && ViewDoc::selected(doc).is_none()
                && let Some(first) = self
                    .engine
                    .containers
                    .iter()
                    .find(|c| c.status.is_running())
                    .or_else(|| self.engine.containers.first())
            {
                ViewDoc::set_selected(doc, Some(&first.id));
                ViewDoc::set_tab(doc, tab);
            }
        }
        // Periodically re-probe the engine for fresh stats / state.
        if self.live && self.last_refresh.elapsed() >= podman::REFRESH_INTERVAL {
            self.spawn_refresh();
        }
        self.hydrate_selection(doc);
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
            Section::Images => self.draw_images(scene, tr, theme, content, &view),
            Section::Volumes => self.draw_volumes(scene, tr, theme, content, &view),
            Section::Distrobox => self.draw_distrobox(scene, tr, theme, content, &view),
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
                Color::from_rgba8(0, 0, 0, 128),
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
    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        doc: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
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
                    scale_factor,
                    modifiers,
                    doc,
                    path,
                    handled,
                    focus_target,
                    dummy_node,
                    &dummy_taffy,
                );
                if !self.palette.is_open() {
                    self.palette_open = false;
                }
            }
            *handled = true;
            self.refresh_view(doc);
            return;
        }

        // Ctrl+K opens the app-scoped palette — the same chord as the shell's
        // global search, here seeded with this app's resources.
        if let WindowEvent::KeyboardInput {
            event: key_event, ..
        } = event
            && key_event.state == ElementState::Pressed
            && modifiers.state().control_key()
            && matches!(&key_event.logical_key, Key::Character(c) if c.eq_ignore_ascii_case("k"))
        {
            self.open_container_palette();
            *handled = true;
            self.refresh_view(doc);
            return;
        }

        // A destructive-confirm or text-prompt dialog is modal: while it's up
        // the shared ModalDialog captures every event (typing, clicks, Esc)
        // and nothing leaks to the app behind the scrim.
        if self.confirm.is_some() {
            self.route_confirm_event(event, scale_factor, *modifiers, doc, handled);
            self.refresh_view(doc);
            return;
        }
        if self.prompt.is_some() {
            self.route_prompt_event(event, scale_factor, *modifiers, doc, handled);
            self.refresh_view(doc);
            return;
        }

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
                if self.create_open {
                    for field in CreateField::ALL {
                        self.route_create_input(
                            field,
                            event,
                            scale_factor,
                            *modifiers,
                            doc,
                            path,
                            focus_target,
                        );
                    }
                }
                // Keep the launcher's pointer position current for its click test.
                self.route_to_launcher(event, scale_factor, *modifiers, doc, path, focus_target);
                // Hover changes button shading — request a redraw.
                *handled = true;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * 48.0,
                    MouseScrollDelta::PixelDelta(p) => p.y / scale_factor,
                };
                // Scroll the panel under the cursor. A compact selected
                // container *is* the detail panel, so it must not leak wheel
                // events into an invisible master list.
                let compact_detail = self.compact_layout_active
                    && self.view.section == Section::Containers
                    && self.view.selected.is_some();
                if compact_detail
                    || self.cursor.x > self.rect.x0 + RAIL_WIDTH + self.list_area_width(doc)
                {
                    self.detail_scroll = (self.detail_scroll - dy).max(0.0);
                } else {
                    self.list_scroll = (self.list_scroll - dy).max(0.0);
                }
                *handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                // A click on the search launcher opens the palette instead of
                // running a list hit.
                self.route_to_launcher(event, scale_factor, *modifiers, doc, path, focus_target);
                if self.search_field.take_palette_request() {
                    self.open_container_palette();
                    *handled = true;
                } else {
                    self.on_click(doc, handled);
                    if self.create_open {
                        self.route_create_input(
                            self.create_focus,
                            event,
                            scale_factor,
                            *modifiers,
                            doc,
                            path,
                            focus_target,
                        );
                    }
                }
            }
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } => {
                self.on_key(
                    doc,
                    event,
                    key_event,
                    scale_factor,
                    *modifiers,
                    path,
                    focus_target,
                    handled,
                );
            }
            WindowEvent::Ime(_) if self.create_open => {
                let field = self.create_focus;
                self.focus_create_field(field);
                self.route_create_input(
                    field,
                    event,
                    scale_factor,
                    *modifiers,
                    doc,
                    path,
                    focus_target,
                );
                self.after_create_input(field);
                *handled = true;
            }
            _ => {}
        }
        // Refresh the cached view state after any mutation so the next draw
        // reflects it (draw has no doc access).
        self.refresh_view(doc);
    }

    fn name(&self) -> &'static str {
        "ContainersApp"
    }

    fn tooltip(&self) -> Option<&str> {
        let hit = self
            .hits
            .iter()
            .rev()
            .find(|region| region.rect.contains(self.cursor))?;
        Some(match &hit.hit {
            Hit::Section(section) => section.label(),
            Hit::NewContainer | Hit::DistroCreate => "Create container",
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
            Hit::CreateSubmit => "Create container",
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
                node = node.with_value(self.create_input(*field).value());
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
                    .with_state(AccessState::new().modal(true))
                    .with_children(controls),
            ]
        } else {
            controls
        };
        if let Some(pull) = &self.pull {
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
            children.push(
                AccessNode::new(Role::ProgressBar)
                    .with_name("Image pull")
                    .with_value(status)
                    .with_live(Politeness::Polite)
                    .with_state(AccessState::new().busy(!pull.done && pull.failed.is_none())),
            );
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

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &AccessibilityAction,
        state: &mut AutoCommit,
        _path: &ObjId,
    ) -> bool {
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
                    ViewDoc::set_create_field(state, field, value);
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

    fn take_control_invocation(&mut self) -> Option<ui::qos_actions::ControlInvocation> {
        self.palette
            .take_control_invocation()
            .or_else(|| self.pending_control_invocation.take())
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

// =====================================================================
// Cached view state
//
// `draw` has no access to the CRDT document, so the app keeps a small
// snapshot of the doc-backed view state refreshed at the tail of every
// `handle_event` and in `sync`.
// =====================================================================

#[derive(Clone)]
pub(crate) struct ViewState {
    pub section: Section,
    pub selected: Option<String>,
    pub tab: DetailTab,
    pub search: String,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            section: Section::Containers,
            selected: None,
            tab: DetailTab::Logs,
            search: String::new(),
        }
    }
}

impl ContainersApp {
    #[allow(
        clippy::expect_used,
        reason = "CreateField::COUNT and CreateField::ALL define the fixed six-slot form together"
    )]
    pub(crate) fn create_input_mut(&mut self, field: CreateField) -> &mut TextInput {
        self.create_inputs
            .get_mut(field.index())
            .expect("every CreateField has one shared input")
    }

    #[allow(
        clippy::expect_used,
        reason = "CreateField::COUNT and CreateField::ALL define the fixed six-slot form together"
    )]
    pub(crate) fn create_input(&self, field: CreateField) -> &TextInput {
        self.create_inputs
            .get(field.index())
            .expect("every CreateField has one shared input")
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

    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors Widget::handle_event for one embedded hand-laid field"
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
        // `sync` normally binds these once per frame. Bind defensively here as
        // well so an event arriving immediately after the modal opens still
        // edits the native collaborative Text object.
        let obj = ViewDoc::create_field_obj(doc, field);
        let input = self.create_input_mut(field);
        input.bind_text(obj);
        Widget::sync(input, doc, path);
        let mut taffy = TaffyTree::new();
        let Ok(node) = taffy.new_leaf(Style::default()) else {
            return;
        };
        let mut ignored = false;
        Widget::handle_event(
            self.create_input_mut(field),
            event,
            scale_factor,
            &modifiers,
            doc,
            path,
            &mut ignored,
            focus_target,
            node,
            &taffy,
        );
    }

    pub(crate) fn after_create_input(&mut self, field: CreateField) {
        if self.create_input_mut(field).take_dirty().is_some() && field == CreateField::Image {
            self.combo.open = true;
            self.combo.highlight = 0;
        }
    }

    fn sync_create_inputs(&mut self, doc: &mut AutoCommit, root: &ObjId) {
        for field in CreateField::ALL {
            let obj = ViewDoc::create_field_obj(doc, field);
            let input = self.create_input_mut(field);
            input.bind_text(obj);
            Widget::sync(input, doc, root);
        }
        if self.create_open {
            self.focus_create_field(self.create_focus);
        } else {
            self.blur_create_fields();
        }
    }

    pub(crate) fn refresh_view(&mut self, doc: &mut AutoCommit) {
        self.sync_create_inputs(doc, &automerge::ROOT);
        self.view = ViewState {
            section: ViewDoc::section(doc),
            selected: ViewDoc::selected(doc),
            tab: ViewDoc::tab(doc),
            search: ViewDoc::search(doc),
        };
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

    pub(crate) fn row_height(&self, theme: &Theme) -> f64 {
        if theme.tokens.typography.scale.base >= LARGE_TEXT_BASE_SIZE {
            84.0
        } else {
            ROW_HEIGHT
        }
    }

    /// The current (animated) navigation-rail width. Eased between fully
    /// expanded ([`RAIL_WIDTH`]) and a thin collapsed strip that still shows
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
        let now = Instant::now();
        let dt = self
            .sidebar_tick
            .map_or(0.0, |t| now.duration_since(t).as_secs_f64().min(0.1));
        self.sidebar_tick = Some(now);
        self.sidebar_flap.tick(SplitLayout::Docked, dt);
    }
}

#[cfg(test)]
mod palette_launcher_tests {
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

    #[test]
    fn compact_breakpoint_preserves_tablet_desktop_but_protects_large_text() {
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

    #[test]
    fn compact_selection_is_a_full_page_drill_in() {
        let mut app = ContainersApp::new(false);
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
        assert_eq!(app.row_height(&theme), ROW_HEIGHT);

        let large = ui::theme::Theme::dark().with_text_scale(2.0);
        app.rect = Rect::new(0.0, 0.0, 768.0, 900.0);
        assert!(app.uses_single_pane_detail(&large));
        assert_eq!(app.row_height(&large), 84.0);
    }

    /// Opening the app-scoped palette seeds it from the app's *real*
    /// containers — the search rows are the app's resources, not a
    /// bespoke in-app search UI. Empty stays empty otherwise.
    #[test]
    fn open_palette_seeds_one_row_per_container() {
        let mut app = ContainersApp::new(false);
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

    /// Enter on a palette row routes through the real `handle_event`
    /// path and selects that container — proving the palette (not the
    /// app's own search) drives selection.
    #[test]
    fn enter_on_a_palette_row_selects_that_container() {
        let mut app = ContainersApp::new(false);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;

        app.open_container_palette();
        let expected = app.engine.containers[0].id.clone();

        let (taffy, node) = dummy_layout();
        let mut handled = false;
        let mut focus = None;
        app.handle_event(
            &key_press(Key::Named(NamedKey::Enter)),
            1.0,
            &winit::event::Modifiers::default(),
            &mut doc,
            &root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
        app.dispatch_palette_invocation_for_test(&mut doc);

        assert!(!app.palette_open, "Enter closes the palette");
        assert_eq!(
            ViewDoc::selected(&mut doc).as_deref(),
            Some(expected.as_str()),
            "activating a palette row selects that container"
        );
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
            1.0,
            &winit::event::Modifiers::default(),
            doc,
            root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
    }

    /// The destructive confirm routes through the shared `ModalDialog`: a click
    /// on its danger primary performs the real removal and closes; Escape on a
    /// fresh one cancels without touching the engine.
    #[test]
    fn confirm_dialog_primary_removes_image_and_escape_cancels() {
        let mut app = ContainersApp::new(false);
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

        let mut b = ContainersApp::new(false);
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

    /// The text prompt routes through the shared `ModalDialog` hosting a real
    /// `TextInput`: characters typed land in the field, and the primary button
    /// commits that value (creating a volume named from the typed text).
    #[test]
    fn prompt_dialog_creates_volume_from_typed_field() {
        let mut app = ContainersApp::new(false);
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

    #[test]
    fn lifecycle_control_emits_the_exported_effect_instead_of_running_podman_inline() {
        let mut app = ContainersApp::new(false);
        app.engine.live = true;
        let id = app.engine.containers[0].id.clone();
        app.apply_action(Action::Stop, &id);
        let intent = app
            .take_control_invocation()
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
}
