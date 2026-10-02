//! Driving a `ui` app without a display.
//!
//! `--shots` renders a cold-start frame offscreen but never delivers input,
//! so it can't answer *does a change survive a restart?* [`Driver`] does:
//! it stands up the app's widget tree over a Taffy layout at a fixed
//! viewport and feeds it the same `WindowEvent`s winit would, through the
//! same [`Widget::handle_event`] the
//! live app uses — nothing stubbed, including CRDT mutations.
//!
//! Apps opt in with one line in `main`, next to `--shots`:
//!
//! ```ignore
//! if let Some(spec) = tilo_ui::headless::spec_from_args(&args) {
//!     std::process::exit(tilo_ui::headless::run(spec, my_app::app_root(), "so.tilo.my-app"));
//! }
//! ```
//!
//! which adds a `--headless-drive '<json>'` flag:
//!
//! ```text
//! ebook --headless-drive '{"actions":[{"click_named":"library_item[0]"}]}'
//! ebook --headless-drive '{"dump":"/tmp/after.json"}'
//! ```
//!
//! The first process changes state and exits (flushing its store); the
//! second loads the store and dumps the document as JSON. Diffing the two
//! dumps is the persistence proof for the app's document.

mod controls;
mod document_history;
pub mod host_media;

pub use controls::{CONTROL_RESOURCE_PREFIX, Control, ControlInput};

use std::collections::{HashMap, HashSet};

use taffy::prelude::*;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::a11y::{AccessNode, Role};
use crate::accesskit_bridge::{
    build_presentation_tree_bound, build_presentation_with_resources_bound,
    build_tree_update_bound, resolve_node_id_bound,
};
use crate::permissions::PermissionContext;
use crate::presence::PresenceCtx;
use crate::state::State;
use crate::synthetic_input::key_event;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{EventCtx, TaffyTreeExt, Widget};

/// Viewport the driver lays out at unless a spec overrides it. Matches the
/// default `--shots` viewport so a driven frame and a screenshot agree.
pub const DEFAULT_VIEWPORT: (u32, u32) = (1280, 800);

/// One thing to do to the app.
#[derive(Debug, Clone)]
pub enum Action {
    /// Click the centre of the widget addressed by `name` — either its
    /// [`Widget::test_id`] or the `Parent/Child[index]` path fallback.
    ClickNamed(String),
    /// Click at raw logical coordinates.
    ClickAt(f64, f64),
    /// Direct-touch tap on a named widget.
    TapNamed(String),
    /// Direct-touch tap at logical coordinates.
    TapAt(f64, f64),
    /// Drag one touch from start to end, exercising tap cancellation and
    /// pixel-precise scrolling.
    SwipeAt(f64, f64, f64, f64),
    /// Press and release one named key (`Enter`, `Tab`, `Escape`, `Space`,
    /// `Backspace`, arrows) or a single character.
    Key(String),
    /// Type a string, one press/release pair per character.
    Type(String),
    /// Hold the given modifiers for subsequent actions. Empty clears them.
    Modifiers(Vec<String>),
    /// Drive frames until a named retained widget appears, or fail after the
    /// bounded timeout. This lets saved scenarios synchronize with workers
    /// and asynchronous model delegates without timing sleeps.
    WaitForNamed {
        target: String,
        timeout: crate::time::Duration,
    },
    /// Require semantic output or state at this exact point in a scenario.
    Expect(SemanticExpectation),
    /// Ask the named widget to perform an accessibility action. This uses
    /// the same `Widget::perform_action` and generic fallback path as a live
    /// AccessKit request.
    Perform {
        target: String,
        semantic_target: Option<String>,
        action: crate::a11y::Action,
    },
    /// Invoke one published action by address, through the same resolution,
    /// availability, confirmation and strict binding the live host applies.
    Invoke(Invocation),
}

/// A scenario step that performs a published action. See
/// [`Driver::invoke_action`].
#[derive(Clone)]
pub struct Invocation {
    pub address: tilo_actions::ActionAddress,
    pub arguments: serde_json::Map<String, serde_json::Value>,
    /// Explicit confirmation, for a `Confirm` contract.
    pub confirm: bool,
    /// The exact phrase, for a `Phrase` contract.
    pub confirmation_phrase: Option<String>,
    /// Pass only if the door refuses with this code.
    pub expect_error: Option<String>,
}

impl std::fmt::Debug for Invocation {
    /// Arguments can carry secrets; debug output names only their keys.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Invocation")
            .field("address", &self.address)
            .field("arguments", &self.arguments.keys().collect::<Vec<_>>())
            .field("confirm", &self.confirm)
            .field("confirmation_phrase", &self.confirmation_phrase.is_some())
            .field("expect_error", &self.expect_error)
            .finish()
    }
}

fn action_description(action: &Action) -> String {
    match action {
        Action::ClickNamed(target) => format!("click {target:?}"),
        Action::ClickAt(_, _) => "click at coordinates".into(),
        Action::TapNamed(target) => format!("tap {target:?}"),
        Action::TapAt(_, _) => "tap at coordinates".into(),
        Action::SwipeAt(_, _, _, _) => "swipe at coordinates".into(),
        Action::Key(key) if key.chars().count() == 1 => "key character".into(),
        Action::Key(key) => format!("key {key:?}"),
        Action::Type(value) => format!("type {} character(s)", value.chars().count()),
        Action::Modifiers(modifiers) if modifiers.is_empty() => "clear modifiers".into(),
        Action::Modifiers(modifiers) => format!("set modifiers {}", modifiers.join("+")),
        Action::WaitForNamed { target, .. } => format!("wait for {target:?}"),
        Action::Expect(expected) => expected.semantic_target.as_ref().map_or_else(
            || format!("expect {:?}", expected.target),
            |semantic| format!("expect {:?} / {semantic:?}", expected.target),
        ),
        Action::Perform {
            target,
            semantic_target,
            action,
        } => {
            let target = semantic_target.as_ref().map_or_else(
                || format!("{target:?}"),
                |semantic| format!("{target:?} / {semantic:?}"),
            );
            format!("perform {} on {target}", semantic_action_name(action))
        }
        Action::Invoke(invocation) => format!(
            "invoke {} on {:?}",
            invocation.address.action, invocation.address.resource
        ),
    }
}

fn semantic_action_name(action: &crate::a11y::Action) -> &'static str {
    match action {
        crate::a11y::Action::Activate => "activate",
        crate::a11y::Action::Focus => "focus",
        crate::a11y::Action::ToggleCheck => "toggle_check",
        crate::a11y::Action::Increment => "increment",
        crate::a11y::Action::Decrement => "decrement",
        crate::a11y::Action::Expand => "expand",
        crate::a11y::Action::Collapse => "collapse",
        crate::a11y::Action::SetValue(_) => "set_value",
        crate::a11y::Action::SetSelection { .. } => "set_selection",
        crate::a11y::Action::ScrollIntoView => "scroll_into_view",
        crate::a11y::Action::ShowContextMenu => "show_context_menu",
    }
}

/// App-facing semantic assertion used by saved scenarios.
#[derive(Debug, Clone, Default)]
pub struct SemanticExpectation {
    pub target: String,
    pub semantic_target: Option<String>,
    pub timeout: Option<crate::time::Duration>,
    pub role: Option<String>,
    pub name: Option<String>,
    pub name_contains: Option<String>,
    pub value: Option<String>,
    pub value_contains: Option<String>,
    pub focused: Option<bool>,
    pub selected: Option<bool>,
    pub checked: Option<bool>,
    pub expanded: Option<bool>,
    pub enabled: Option<bool>,
    pub busy: Option<bool>,
    pub invalid: Option<bool>,
}

impl SemanticExpectation {
    /// Start an assertion for one retained widget id or inspection path.
    #[must_use]
    pub fn named(target: impl Into<String>) -> Self {
        Self {
            target: target.into(),
            ..Self::default()
        }
    }

    /// Address a synthetic collection descendant by its stable model identity.
    #[must_use]
    pub fn semantic_target(mut self, target: impl Into<String>) -> Self {
        self.semantic_target = Some(target.into());
        self
    }

    /// Drive live-equivalent frames until this assertion matches or the
    /// explicit bound expires. Without this, assertions are immediate.
    #[must_use]
    pub fn within(mut self, timeout: crate::time::Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    #[must_use]
    pub fn role(mut self, role: crate::a11y::Role) -> Self {
        self.role = Some(role_name(role).to_owned());
        self
    }

    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    #[must_use]
    pub fn name_contains(mut self, fragment: impl Into<String>) -> Self {
        self.name_contains = Some(fragment.into());
        self
    }

    #[must_use]
    pub fn value(mut self, value: impl Into<String>) -> Self {
        self.value = Some(value.into());
        self
    }

    #[must_use]
    pub fn value_contains(mut self, fragment: impl Into<String>) -> Self {
        self.value_contains = Some(fragment.into());
        self
    }

    #[must_use]
    pub fn focused(mut self, expected: bool) -> Self {
        self.focused = Some(expected);
        self
    }

    #[must_use]
    pub fn selected(mut self, expected: bool) -> Self {
        self.selected = Some(expected);
        self
    }

    #[must_use]
    pub fn checked(mut self, expected: bool) -> Self {
        self.checked = Some(expected);
        self
    }

    #[must_use]
    pub fn expanded(mut self, expected: bool) -> Self {
        self.expanded = Some(expected);
        self
    }

    #[must_use]
    pub fn enabled(mut self, expected: bool) -> Self {
        self.enabled = Some(expected);
        self
    }

    #[must_use]
    pub fn busy(mut self, expected: bool) -> Self {
        self.busy = Some(expected);
        self
    }

    #[must_use]
    pub fn invalid(mut self, expected: bool) -> Self {
        self.invalid = Some(expected);
        self
    }
}

/// What a `--headless-drive` invocation should do.
#[derive(Debug, Clone, Default)]
pub struct Spec {
    pub viewport: Option<(u32, u32)>,
    pub actions: Vec<Action>,
    /// Fail the headless run when the final semantic tree violates the
    /// toolkit's accessibility contracts.
    pub audit: bool,
    /// Write a structured accessibility report without requiring enforcement.
    pub audit_report: Option<std::path::PathBuf>,
    /// Write the app's CRDT document here as JSON when the actions are done.
    pub dump: Option<std::path::PathBuf>,
    /// Write the addressable widget names here as JSON.
    pub targets: Option<std::path::PathBuf>,
    /// Write one semantic widget/accessibility snapshot here as JSON.
    pub inspect: Option<std::path::PathBuf>,
}

/// One node in a developer inspection snapshot.
///
/// `semantics` is the exact toolkit [`AccessNode`] used to build the exported
/// AccessKit tree. Its nested `children` include virtualized semantic rows and
/// cells that do not have a retained widget of their own.
#[derive(Debug, serde::Serialize)]
pub struct InspectionNode {
    pub path: String,
    pub widget: String,
    pub test_id: Option<String>,
    /// Whether the retained widget participates in keyboard focus traversal.
    /// Kept in the inspection format so an omitted semantic node cannot hide
    /// an interactive control from the accessibility audit.
    pub focusable: bool,
    /// `[x, y, width, height]` in logical window coordinates.
    pub rect: [f64; 4],
    /// Absolute logical clip applied to children, if the widget clips them.
    pub child_clip: Option<[f64; 4]>,
    /// Effective policy and authoring origins, when supplied by the widget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<crate::widget::LayoutInfo>,
    pub semantics: Option<AccessNode>,
    pub children: Vec<InspectionNode>,
}

/// A coherent post-layout snapshot of the retained and semantic trees.
#[derive(Debug, serde::Serialize)]
pub struct InspectionSnapshot {
    pub viewport: [u32; 2],
    /// Exact retained owner of the runner's current keyboard focus. Widgets
    /// in other mounted apps may independently retain local editor focus.
    /// Always serialize absence as null: omission means legacy unknown ownership.
    pub keyboard_focus_owner: Option<String>,
    pub root: InspectionNode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presentation: Option<InspectionNode>,
}

/// One deterministic accessibility-contract violation found in a laid-out
/// retained or synthetic semantic node.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AccessibilityIssue {
    pub path: String,
    pub code: String,
    pub message: String,
}

/// Current machine-readable accessibility-audit report format.
pub const ACCESSIBILITY_AUDIT_VERSION: u32 = 1;

/// Result of auditing the same semantic tree exported to assistive technology.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AccessibilityAudit {
    pub version: u32,
    pub issues: Vec<AccessibilityIssue>,
}

/// Current machine-readable Tilo-native contract report format.
pub const NATIVE_APP_AUDIT_VERSION: u32 = 2;

/// One reason an app cannot yet be certified as fully action-native.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct NativeAppIssue {
    pub control: String,
    pub code: String,
    pub message: String,
}

/// Runtime proof over the app's actual laid-out widget and resource trees.
///
/// A clean report means every inventoried retained control enters through a
/// fresh exported resource action and the complete focusable tree satisfies
/// the accessibility contract. The live [`crate::App`] then supplies the
/// remaining properties by construction: the same actions are published to
/// search and agents, pass through the permission door, and document changes
/// use the shared collaboration/history executor.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct NativeAppAudit {
    pub version: u32,
    pub accessibility: AccessibilityAudit,
    pub issues: Vec<NativeAppIssue>,
}

impl NativeAppAudit {
    #[must_use]
    pub fn is_certified(&self) -> bool {
        self.accessibility.is_clean() && self.issues.is_empty()
    }
}

impl Default for AccessibilityAudit {
    fn default() -> Self {
        Self {
            version: ACCESSIBILITY_AUDIT_VERSION,
            issues: Vec::new(),
        }
    }
}

impl AccessibilityAudit {
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.issues.is_empty()
    }
}

struct ComponentRunnerWakeState {
    receiver: std::cell::RefCell<std::sync::mpsc::Receiver<(u64, crate::wake::WakeEvent)>>,
    generation: std::cell::Cell<Option<u64>>,
    binding: crate::bound_dispatch::CallbackBinding,
    dirty: std::cell::Cell<bool>,
}
impl ComponentRunnerWakeState {
    fn drain(&self) {
        for (generation, event) in self.receiver.borrow_mut().try_iter() {
            if self.generation.get() == Some(generation)
                && self.binding.lifetime.accepts(&event)
                && self.binding.enter().is_ok()
            {
                self.dirty.set(true);
            }
        }
    }
    fn close(&self) {
        self.generation.set(None);
        self.drain();
        self.dirty.set(false);
    }
}

use crate::history_mode::frame::FrameworkInputFrame;

/// The canonical display-independent frame pipeline.
///
/// Both screenshot rendering and [`Driver`] input scenarios use this type, so
/// they cannot drift into subtly different sync/layout/draw implementations.
/// Live [`crate::App`] additionally owns the window and event loop, but follows
/// the same pass ordering documented by [`Self::relayout`].
/// Borrowed renderer input plus semantic output from the same real widget frame.
/// Cheap wake demand; deliberately excludes accessibility and inspection trees.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameSchedule {
    pub needs_redraw: bool,
    /// The part of `needs_redraw` that is queued work: a component wake, a
    /// pending invocation or a presence change. Widget dirtiness is left
    /// out (leaves default to dirty), so a host that takes frames only for
    /// input and this work stays idle while nothing happens.
    pub queued_work: bool,
    pub needs_animation_frame: bool,
    pub next_wake_deadline: Option<crate::time::Instant>,
}

/// Why a widget asks a live host for a frame
/// ([`FrameHarness::frame_requesters`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameDemand {
    /// [`Widget::needs_animation_frame`]: a frame every vsync.
    Animation,
    /// [`Widget::next_wake_deadline`] is due.
    Deadline,
}

/// Borrowed paint input without constructing an accessibility snapshot.
pub struct FrameRenderInput<'a> {
    pub scene: &'a Scene,
    pub background: vello::peniko::Color,
}

pub struct FrameOutput<'a> {
    pub scene: &'a Scene,
    pub background: vello::peniko::Color,
    pub logical_size: (u32, u32),
    pub device_size: (u32, u32),
    pub semantics: InspectionSnapshot,
    pub needs_redraw: bool,
    /// Independent continuous-update demand, even while pixels are clean.
    pub needs_animation_frame: bool,
    /// Earliest future update across the active presentation roots.
    pub next_wake_deadline: Option<crate::time::Instant>,
    pub ime: Option<crate::ime::ImeWindowState>,
}

mod caller_palette;
pub mod system_search;
pub mod test_support;

pub struct FrameHarness {
    control_domain: crate::ControlDomain,
    caller_palette: caller_palette::CallerPalette,
    system_search: system_search::SystemSearch,
    component_wake: Option<ComponentRunnerWakeState>,
    state: State,
    root: Box<dyn Widget>,
    history: Option<Box<crate::history_mode::host::HistoryHost>>,
    action_palette: Option<Box<crate::action_palette::ActionPalette>>,
    root_lifetime: uuid::Uuid,
    framework_input: std::rc::Rc<std::cell::Cell<bool>>,
    taffy: TaffyTree<()>,
    node_map: HashMap<NodeId, *mut dyn Widget>,
    bindings: crate::bound_dispatch::BoundNodeMap,
    component_close_reports: Vec<crate::application::ComponentCloseReport>,
    root_node: NodeId,
    child_node: NodeId,
    document_history: Box<crate::standalone_history_host::StandaloneHistoryHost>,
    history_chrome_enabled: bool,
    history_layout: Option<crate::standalone_history_host::HostHistoryLayout>,
    history_focus_pending: bool,
    #[allow(dead_code, reason = "used by feature-gated history input paths")]
    history_pointer_owned: bool,
    text: TextRenderer,
    theme: Theme,
    /// Scratch scene for the draw pass. Never rasterised — see
    /// [`Driver::relayout`] for why it is drawn at all.
    scene: Scene,
    presence: PresenceCtx,
    /// Presence changed through [`Self::update_presence`]: the next frame
    /// redraws (the collaborator overlays read presence at draw time) without
    /// an extra synchronous relayout.
    presence_redraw: bool,
    permissions: PermissionContext,
    modifiers: winit::event::Modifiers,
    interaction: crate::interaction::InteractionEnvironment,
    platform: crate::platform::PlatformConventions,
    touch_pointer: crate::interaction::TouchPointerEmulator,
    dispatching_touch_pointer: bool,
    viewport: (u32, u32),
    last_layout_viewport: Option<(u32, u32)>,
    input_scale: f64,
    host_pointer_position: Option<winit::dpi::PhysicalPosition<f64>>,
    /// Records this driver has open, when the app under test uses them —
    /// without this a record-aware app would be driven against its room
    /// index instead of the record it checked out, testing something the
    /// user never runs.
    records: Option<crate::record::RecordSet>,
    record_persistence: Option<(crate::record::RecordSet, automerge::AutoCommit)>,
    room_index: Option<automerge::AutoCommit>,
    /// The taffy node holding keyboard focus, or `None`. Mirrors
    /// [`crate::app::App::focused_node`] — focus is App-level, not
    /// something the root widget's `handle_event` does. Remapped across
    /// every `relayout` by scanning for `is_focused()`: widget instances
    /// survive a relayout, their taffy ids do not.
    focused: Option<NodeId>,
    focus_trap_active: bool,
    active_input_instance: Option<crate::application::InstanceId>,
    saved_instance_focus:
        HashMap<crate::application::InstanceId, crate::widget::layout::CompositionFocus>,
    input_sequence: std::cell::Cell<u64>,
    observed_input_owner: std::cell::RefCell<Option<String>>,
    focus_before_trap: Option<crate::widget::layout::CompositionFocus>,
    /// Identity the driver's commits and undos are attributed to. Mirrors
    /// [`crate::app::App`]'s `local_author` so a scenario exercises the same
    /// author-scoped history a user does.
    author: crate::permissions::UserId,
    /// Same decoder and transport doors used by the live application host.
    /// Tests register document action types explicitly, just as `main` does.
    action_registry: tilo_actions::ActionRegistry,
    action_transport: Box<dyn tilo_actions::ActionTransport>,
    pending_control_invocations: HashSet<tilo_actions::InvocationId>,
    control_failures: Vec<String>,
    bound_invocations:
        HashMap<tilo_actions::InvocationId, crate::bound_dispatch::BoundActionTarget>,
    /// Per-pass frame profiler, installed by the perf harness via
    /// [`Driver::begin_profiling`]. `None` on a normal (persistence) run, so
    /// [`Driver::relayout`] and [`Driver::send`] pay nothing. When set, each
    /// pass is timed and every frame committed — see [`crate::profiling`].
    profiler: Option<crate::profiling::FrameProfiler>,
    /// Whether the latest `send_host_event` was inert pointer motion that skipped relayout.
    last_send_skipped_relayout: bool,
    /// Opt-in diagnostics: dirty widget paths observed when pointer motion still demanded a frame.
    pointer_redraw_diagnostics: bool,
    pointer_redraw_sources: Vec<String>,
    /// Whether `taffy`, `node_map`, `bindings`, `root_node` and `child_node`
    /// describe a complete admitted composition that a later frame may
    /// re-solve in place. False until the first successful build and after
    /// every path that discards the raw maps, so the next frame rebuilds.
    layout_built: bool,
    /// Test override for the `TILO_UI_FULL_RELAYOUT` safety valve: `Some`
    /// wins over the environment.
    full_relayout_override: Option<bool>,
    /// What the most recent completed relayout did; see [`RelayoutStats`].
    last_relayout: RelayoutStats,
    /// Completed relayout frames since construction.
    relayout_frames: u64,
    /// Retained (non-rebuilt) frames completed since construction, used by
    /// the opt-in geometry check.
    retained_frames: u64,
    /// `0` disables the check; otherwise every Nth retained frame is
    /// compared against a fresh rebuild (debug builds only).
    retained_geometry_check_every: u32,
    /// Re-entrancy guard for the geometry check, whose forced rebuild runs
    /// through `try_relayout` itself.
    verifying_geometry: bool,
    /// Concrete type of `root` when it was mounted through
    /// [`FrameHarness::for_root`]; gates [`FrameHarness::root`].
    root_type: Option<std::any::TypeId>,
    /// How the node the root is laid out in is sized and styled.
    host: HostLayout,
}

/// The node a [`FrameHarness`] lays its root out in.
#[derive(Clone, Default)]
struct HostLayout {
    /// Its width is always the viewport's.
    style: Style,
    /// Its height is the root's content height rather than the viewport's.
    content_height: bool,
}

/// What one completed [`FrameHarness::relayout`] did with its layout tree.
///
/// A rebuilt frame discarded and recomposed the taffy tree, node map and
/// bindings; a retained frame kept them and either re-solved sizes in place
/// (`measured`, after invalidating `marked_dirty` nodes of which
/// `marked_leaves` are childless) or only re-applied positions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RelayoutStats {
    pub rebuilt: bool,
    pub measured: bool,
    pub marked_dirty: usize,
    pub marked_leaves: usize,
}

/// `TILO_UI_FULL_RELAYOUT=1` forces the headless host to rebuild its layout
/// tree on every frame, as it did before retained relayout — a safety valve if
/// a widget ever changes size without reporting `is_layout_dirty`, mirroring
/// `App`'s `TILO_UI_NO_SCROLL_OPT`.
fn full_relayout_forced() -> bool {
    use std::sync::OnceLock;
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| crate::env::var("TILO_UI_FULL_RELAYOUT").is_ok_and(|s| !s.is_empty()))
}

/// Backwards-compatible name for the input-driving use of [`FrameHarness`].
pub type Driver = FrameHarness;

/// What the root reported for one [`FrameHarness::dispatch_to_root_outcome`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RootDispatch {
    pub handled: bool,
    pub focus_target: Option<NodeId>,
}

/// Construction options for [`FrameHarness::for_root_with`].
#[derive(Default)]
pub struct FrameOptions {
    /// Defaults to [`Theme::dark`].
    pub theme: Option<Theme>,
    /// Defaults to [`TextRenderer::new`]; its text scale follows the theme.
    pub text: Option<TextRenderer>,
    /// Style of the viewport-sized node the root is laid out in, for a root
    /// that must not be stretched by the default row. Its size is replaced
    /// by the viewport.
    pub host_style: Option<Style>,
    /// Lay the root out at its content height, measured against unbounded
    /// height, the way a scrolling host lays out the document inside it. The
    /// viewport still bounds culling and rasterisation.
    pub content_height: bool,
}

/// A logical `(width, height)` as a harness viewport.
///
/// Fractions truncate and negative or NaN sizes become zero, so a size
/// written as a float names the same viewport it would as an integer.
#[must_use]
pub fn viewport<T: Into<f64>>((w, h): (T, T)) -> (u32, u32) {
    let (w, h) = (w.into(), h.into());
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "`as` saturates and maps NaN to zero, which is the documented contract"
    )]
    (w as u32, h as u32)
}

/// A [`FrameHarness`] over one concrete root that reads as that root.
///
/// Dereferencing yields the widget; a mutable dereference makes the next
/// frame recompose from scratch (see [`FrameHarness::root_mut`]). Harness
/// operations are associated functions so they never shadow the root's own
/// methods.
pub struct Framed<W: Widget + 'static> {
    harness: FrameHarness,
    root: std::marker::PhantomData<fn() -> W>,
}

impl<W: Widget + 'static> Framed<W> {
    /// Mount `root` over `state` and run its first frame at `viewport`.
    pub fn new(state: State, root: W, viewport: (u32, u32), theme: Theme) -> Self {
        Self::from_harness(FrameHarness::for_root(state, root, viewport, theme))
    }

    /// [`Self::new`] with every construction option spelled out.
    pub fn with_options(
        state: State,
        root: W,
        viewport: (u32, u32),
        options: FrameOptions,
    ) -> Self {
        Self::from_harness(FrameHarness::for_root_with(state, root, viewport, options))
    }

    fn from_harness(harness: FrameHarness) -> Self {
        let _: &W = harness.root();
        Self {
            harness,
            root: std::marker::PhantomData,
        }
    }

    pub fn harness(this: &Self) -> &FrameHarness {
        &this.harness
    }

    pub fn harness_mut(this: &mut Self) -> &mut FrameHarness {
        &mut this.harness
    }

    /// The root and the live document at once; see
    /// [`FrameHarness::root_and_state_mut`].
    pub fn split(this: &mut Self) -> (&mut W, &mut State) {
        this.harness.root_and_state_mut()
    }
}

impl<W: Widget + 'static> std::ops::Deref for Framed<W> {
    type Target = W;

    fn deref(&self) -> &W {
        self.harness.root()
    }
}

impl<W: Widget + 'static> std::ops::DerefMut for Framed<W> {
    fn deref_mut(&mut self) -> &mut W {
        self.harness.root_mut()
    }
}

impl FrameHarness {
    /// Mount a prepared component without a native window, GPU, or service discovery.
    /// # Errors
    /// Refuses exhausted mount generations or remounts, retaining original recovery.
    pub fn from_component(
        component: crate::application::AppComponent,
        viewport: (u32, u32),
        inputs: crate::application::ComponentRunnerInputs,
    ) -> Result<Self, crate::launch::LaunchError> {
        Self::from_component_with_wake_notifier(component, viewport, inputs, || {})
    }

    /// Mount with a lightweight scheduling signal. The callback captures a host
    /// task identifier/proxy, never a GPU, widget tree, or owning host reference.
    /// Browser hosts can dispatch that identifier to their local animation-frame queue.
    pub fn from_component_with_wake_notifier(
        mut component: crate::application::AppComponent,
        viewport: (u32, u32),
        inputs: crate::application::ComponentRunnerInputs,
        notify: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, crate::launch::LaunchError> {
        static NEXT_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let Ok(generation) = NEXT_GENERATION.fetch_update(
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
            |value| value.checked_add(1),
        ) else {
            return Err(component.setup_error(crate::launch::LaunchError::Operation(
                "component mount generation exhausted".into(),
            )));
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        let binding = component.callback_binding();
        let wake = ComponentRunnerWakeState {
            receiver: std::cell::RefCell::new(receiver),
            generation: std::cell::Cell::new(Some(generation)),
            binding: binding.clone(),
            dirty: std::cell::Cell::new(false),
        };
        if let Err(error) = component.mount(move |event| {
            if sender.send((generation, event)).is_ok() {
                notify();
            }
        }) {
            return Err(component.setup_error(error));
        }
        let history = component.history.take();
        let mut root = Box::new(crate::widget::BoundAppWidget { component });
        if let Some(registration) = &history
            && let Err(error) = registration.validate_root(root.as_ref())
        {
            return Err(root
                .component
                .setup_error(crate::launch::LaunchError::Operation(error.to_string())));
        }
        let mut harness = Self::with_inputs(
            State::new(),
            root,
            viewport,
            inputs.theme,
            inputs.text_renderer,
            || binding.author,
            Some(wake),
        );
        if let Some(registration) = history
            && let Err(error) = harness.install_history(registration)
        {
            harness.close_components();
            return Err(crate::launch::LaunchError::ComponentSetup {
                source: Box::new(crate::launch::LaunchError::Operation(error.to_string())),
                record: Box::new(binding.checkout),
                recovery: crate::launch::LaunchRecovery::new(
                    Some(binding.records),
                    harness.take_component_close_reports(),
                ),
            });
        }
        Ok(harness)
    }

    #[cfg(feature = "native")]
    pub(crate) fn resize_component_capture(&mut self, viewport: (u32, u32)) {
        let _ = self.resize(viewport);
    }

    /// Shared final-output boundary for prepared CLI capture and audit.
    #[cfg(feature = "native")]
    pub(crate) fn prepare_component_output(&mut self) {
        self.pump_component_wakes();
    }

    /// Force the mounted component's real record writer to acknowledge all
    /// pending edits, then project the resulting durability state.
    ///
    /// Capture scenarios use this at an explicit save boundary instead of
    /// depending on a wall-clock debounce or mutating a status widget.
    /// # Errors
    /// Returns the record writer's durable storage failure.
    pub fn flush_component_records(&mut self) -> Result<(), String> {
        let result = self
            .component_wake
            .as_ref()
            .ok_or_else(|| "driver has no mounted component".to_owned())?
            .binding
            .records
            .try_flush();
        self.relayout();
        result
    }

    fn drain_component_wakes(&self) {
        if let Some(wake) = &self.component_wake {
            wake.drain();
        }
    }
    fn pump_component_wakes(&mut self) {
        self.drain_component_wakes();
        if self
            .component_wake
            .as_ref()
            .is_some_and(|wake| wake.dirty.replace(false))
        {
            self.relayout();
        }
    }

    /// Capture/close embedded components while the host still owns their trees.
    /// Recovery reports stay owned by this host until explicitly transferred.
    pub fn close_components(&mut self) {
        self.control_domain.close();
        if let Some(wake) = &self.component_wake {
            wake.close();
        }
        self.component_close_reports
            .extend(crate::component_lifecycle::close_components(
                self.root.as_mut(),
            ));
        self.component_close_reports.extend(
            crate::component_lifecycle::take_component_close_reports(self.root.as_mut()),
        );
        self.discard_layout_tree();
    }

    /// Drop the raw layout maps so no later frame re-solves a composition
    /// whose widgets may have been replaced; the next frame rebuilds.
    fn discard_layout_tree(&mut self) {
        self.bindings.clear();
        self.node_map.clear();
        self.layout_built = false;
    }

    /// The app root's structure flag, and framework History's: it is laid
    /// out as a separate root, so its replaced controls are not the app's.
    fn take_composition_dirty(&mut self) -> bool {
        let root = self.root.take_structure_dirty();
        let history = self
            .history
            .as_mut()
            .is_some_and(|history| history.take_structure_dirty());
        root || history
    }

    /// Replace the whole root control of this harness. `node_map` holds raw
    /// pointers into the current tree and `focused` names a node of the
    /// current Taffy tree, so both are discarded before the old tree is
    /// dropped; assigning `root` directly would leave the next frame reading
    /// freed widgets.
    #[cfg(test)]
    fn replace_root(&mut self, root: Box<dyn Widget>) {
        self.discard_layout_tree();
        self.focused = None;
        self.focus_before_trap = None;
        self.root_type = None;
        self.root = root;
    }

    /// Force the next frame to recompose from scratch. Inputs that shape
    /// every widget's style at build time (theme, platform conventions,
    /// interaction environment, an installed history host) go through here
    /// because retained widgets are not required to report them as layout
    /// dirtiness.
    fn invalidate_layout_tree(&mut self) {
        self.layout_built = false;
    }
    pub fn component_close_reports(&self) -> &[crate::application::ComponentCloseReport] {
        self.drain_component_wakes();
        &self.component_close_reports
    }
    /// Transfer pending recovery to another owner; ordinary tile destruction cannot lose it.
    pub fn take_component_close_reports(
        &mut self,
    ) -> Vec<crate::application::ComponentCloseReport> {
        self.component_close_reports.extend(
            crate::component_lifecycle::take_component_close_reports(self.root.as_mut()),
        );
        std::mem::take(&mut self.component_close_reports)
    }

    fn inspect_node<R>(
        &self,
        node: NodeId,
        callback: impl FnOnce(&dyn Widget) -> R,
    ) -> Result<R, crate::bound_dispatch::BoundDispatchError> {
        // SAFETY: Driver owns this stable tree; no target reference escapes.
        unsafe {
            crate::bound_dispatch::inspect_bound_node(
                &self.bindings,
                &self.node_map,
                node,
                callback,
            )
        }
    }
    fn mutate_node<R>(
        &mut self,
        node: NodeId,
        callback: impl FnOnce(&mut dyn Widget) -> R,
    ) -> Result<R, crate::bound_dispatch::BoundDispatchError> {
        // SAFETY: exclusive Driver access prevents overlapping target references.
        unsafe {
            crate::bound_dispatch::mutate_bound_node(&self.bindings, &self.node_map, node, callback)
        }
    }
    fn dispatch_semantic_node<R>(
        &mut self,
        node: NodeId,
        action: &crate::a11y::Action,
        callback: impl FnOnce(&mut dyn Widget, &mut automerge::AutoCommit, &automerge::ObjId) -> R,
    ) -> Result<R, String> {
        let author = self.author.clone();
        // SAFETY: the runner exclusively owns the unchanged admitted tree.
        unsafe {
            crate::bound_dispatch::invoke_bound_semantic_node(
                &self.bindings,
                &self.node_map,
                node,
                action,
                &mut self.state,
                crate::bound_dispatch::SemanticCaller {
                    authority: &self.permissions,
                    author: &author,
                },
                callback,
            )
        }
    }
    fn dispatch_node<R>(
        &mut self,
        node: NodeId,
        callback: impl FnOnce(&mut dyn Widget, &mut automerge::AutoCommit, &automerge::ObjId) -> R,
    ) -> Result<R, crate::bound_dispatch::BoundDispatchError> {
        // SAFETY: exclusive Driver owns both maps and the stable live tree.
        unsafe {
            crate::bound_dispatch::invoke_bound_node(
                &self.bindings,
                &self.node_map,
                node,
                &mut self.state.doc,
                &self.state.root,
                callback,
            )
        }
    }

    /// Mirror the live host's focused-control fallback for Enter and Space.
    /// Widgets that consume the press keep ownership; releases never activate.
    fn handle_unconsumed_activation_key(&mut self, event: &WindowEvent, handled: &mut bool) {
        if *handled
            || !matches!(
                event,
                WindowEvent::KeyboardInput {
                    event: winit::event::KeyEvent {
                        logical_key: Key::Named(NamedKey::Enter | NamedKey::Space),
                        state: ElementState::Pressed,
                        ..
                    },
                    ..
                }
            )
        {
            return;
        }
        let Some(focused) = self.focused else {
            return;
        };
        let activated = self
            .dispatch_node(focused, |widget, doc, root| {
                widget.perform_action(&crate::a11y::Action::Activate, doc, root)
            })
            .unwrap_or(false);
        if activated {
            *handled = true;
        }
    }

    /// Stand the app up and resolve its first observable layout.
    ///
    /// Responsive structure discovered from the viewport is settled
    /// before this returns, so tests can address compact/wide controls
    /// immediately without an app-specific warm-up frame.
    /// # Errors
    /// Refuses invalid scope metadata or unavailable mounted document custody.
    pub fn from_managed_root(
        state: State,
        root: crate::history_mode::ManagedRoot,
        viewport: (u32, u32),
    ) -> Result<Self, crate::history_mode::HistoryRegistrationError> {
        Self::from_managed_root_with_constructor(state, root, viewport, Self::new)
    }

    fn from_managed_root_with_constructor(
        state: State,
        root: crate::history_mode::ManagedRoot,
        viewport: (u32, u32),
        constructor: impl FnOnce(State, Box<dyn Widget>, (u32, u32)) -> Self,
    ) -> Result<Self, crate::history_mode::HistoryRegistrationError> {
        root.history.validate()?;
        root.history.validate_root(root.widget.as_ref())?;
        let mut harness = constructor(state, root.widget, viewport);
        harness.install_history(root.history)?;
        Ok(harness)
    }

    /// Construct a managed root with caller-owned fonts and identity.
    /// # Errors
    /// Refuses invalid scope metadata or unavailable mounted document custody.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn from_managed_root_with_controlled_inputs(
        state: State,
        root: crate::history_mode::ManagedRoot,
        viewport: (u32, u32),
        theme: Theme,
        text: TextRenderer,
        author: crate::permissions::UserId,
    ) -> Result<Self, crate::history_mode::HistoryRegistrationError> {
        Self::from_managed_root_with_constructor(
            state,
            root,
            viewport,
            move |state, widget, size| {
                Self::new_with_controlled_inputs(state, widget, size, theme, text, author)
            },
        )
    }

    pub(crate) fn install_history(
        &mut self,
        registration: crate::history_mode::HistoryRegistration,
    ) -> Result<(), crate::history_mode::HistoryRegistrationError> {
        registration.validate_root(self.root.as_ref())?;
        let mut history =
            crate::history_mode::host::HistoryHost::new(registration, &self.state.doc)?;
        history.set_control_domain(&self.control_domain);
        self.history = Some(Box::new(history));
        self.invalidate_layout_tree();
        self.relayout();
        self.validate_history_registration()
    }

    /// Install the runner's current authority, including revocations.
    pub fn control_coverage(&self) -> crate::ControlCoverage {
        self.control_domain.coverage()
    }

    /// Replace permissions for the legacy host document. Bound components keep
    /// their own independently scoped permission contexts.
    pub fn set_permission_ctx(&mut self, permissions: PermissionContext) {
        self.permissions = permissions;
    }

    #[cfg(test)]
    pub(crate) fn set_history_clock(&mut self, clock: fn() -> Option<u64>) {
        if let Some(history) = &mut self.history {
            history.set_clock(clock);
        }
    }

    /// # Errors
    /// Refuses missing, stale or mismatched managed owner registrations.
    pub fn validate_history_registration(
        &self,
    ) -> Result<(), crate::history_mode::HistoryRegistrationError> {
        self.history
            .as_ref()
            .ok_or(crate::history_mode::HistoryRegistrationError::Missing)?
            .validate()?;
        self.history
            .as_ref()
            .ok_or(crate::history_mode::HistoryRegistrationError::Missing)?
            .validate_root(self.root.as_ref())
    }

    /// Review pre-entry controls retained by the managed History surface.
    pub fn pending_history_controls(
        &self,
    ) -> Vec<crate::history_mode::pending_controls::PendingControlReview> {
        self.history
            .as_ref()
            .map_or_else(Vec::new, |history| history.pending_control_reviews())
    }

    /// Mint a one-use token for the current presentation of one retained item.
    pub fn pending_history_control_token(
        &mut self,
        id: crate::history_mode::pending_controls::PendingControlId,
    ) -> Option<crate::history_mode::pending_controls::ContinuationToken> {
        self.history.as_mut()?.pending_control_token(id)
    }

    /// Continue one exact retained control under current authority.
    pub fn continue_pending_history_control(
        &mut self,
        token: &crate::history_mode::pending_controls::ContinuationToken,
    ) -> Option<crate::history_mode::pending_controls::PendingControlStatus> {
        self.continue_pending_history_control_checked(token, false)
    }

    /// Continue after the caller explicitly confirmed this exact current item.
    pub fn confirm_pending_history_control(
        &mut self,
        token: &crate::history_mode::pending_controls::ContinuationToken,
    ) -> Option<crate::history_mode::pending_controls::PendingControlStatus> {
        self.continue_pending_history_control_checked(token, true)
    }

    /// Cancel one exact retained control without dispatching it.
    pub fn cancel_pending_history_control(
        &mut self,
        token: &crate::history_mode::pending_controls::ContinuationToken,
    ) -> bool {
        self.history
            .as_mut()
            .is_some_and(|history| history.cancel_pending_control(token))
    }

    /// Invalidate only the managed History surface. The mounted live owner
    /// remains in place and continues its independent lifecycle.
    fn invalidate_unsupported_history_owner(&mut self) -> bool {
        if self
            .history
            .as_ref()
            .is_some_and(|history| history.validate_root(self.root.as_ref()).is_err())
        {
            return self
                .history
                .as_mut()
                .is_some_and(|history| history.invalidate_owner());
        }
        false
    }

    pub fn new(state: State, root: Box<dyn Widget>, viewport: (u32, u32)) -> Self {
        Self::new_with_theme(state, root, viewport, Theme::dark())
    }

    /// Stand the app up under an explicit theme and resolve the first
    /// observable layout, including viewport-dependent structure.
    pub fn new_with_theme(
        state: State,
        root: Box<dyn Widget>,
        viewport: (u32, u32),
        theme: Theme,
    ) -> Self {
        // Keep offscreen review/a11y runs faithful to the live host.  A
        // text-scale theme without a matching renderer scale only changes
        // widgets which manually consult `Theme::text_scale`; custom drawing
        // and shared TextInput then render at the wrong size, hiding exactly
        // the clipping and collision regressions these shots are meant to
        // catch.
        Self::with_inputs(
            state,
            root,
            viewport,
            theme,
            TextRenderer::new(),
            crate::collaboration::local_user_id,
            None,
        )
    }

    /// Construct with caller-owned fonts and identity, without ambient discovery.
    #[doc(hidden)]
    pub fn new_with_controlled_inputs(
        state: State,
        root: Box<dyn Widget>,
        viewport: (u32, u32),
        theme: Theme,
        text: TextRenderer,
        author: crate::permissions::UserId,
    ) -> Self {
        Self::with_inputs(state, root, viewport, theme, text, || author, None)
    }

    fn with_inputs_styled(
        state: State,
        root: Box<dyn Widget>,
        viewport: (u32, u32),
        theme: Theme,
        text: TextRenderer,
        host: HostLayout,
    ) -> Self {
        Self::with_inputs_and_host(
            state,
            root,
            viewport,
            theme,
            text,
            crate::collaboration::local_user_id,
            None,
            host,
        )
    }

    fn with_inputs(
        state: State,
        root: Box<dyn Widget>,
        viewport: (u32, u32),
        theme: Theme,
        text: TextRenderer,
        author: impl FnOnce() -> crate::permissions::UserId,
        component_wake: Option<ComponentRunnerWakeState>,
    ) -> Self {
        Self::with_inputs_and_host(
            state,
            root,
            viewport,
            theme,
            text,
            author,
            component_wake,
            HostLayout::default(),
        )
    }

    fn with_inputs_and_host(
        state: State,
        root: Box<dyn Widget>,
        viewport: (u32, u32),
        mut theme: Theme,
        mut text: TextRenderer,
        author: impl FnOnce() -> crate::permissions::UserId,
        component_wake: Option<ComponentRunnerWakeState>,
        host: HostLayout,
    ) -> Self {
        if component_wake.is_some() {
            let ui_scale = theme.ui_scale;
            theme = theme.with_component_scales(text.text_scale(), ui_scale);
        } else {
            text.set_text_scale(theme.text_scale);
        }
        let control_domain = crate::control_custody::ControlDomain::outer();
        let mut root = root;
        crate::widget::controls::install_control_custody(
            root.as_mut(),
            &control_domain,
            crate::control_custody::CustodyAttachment::Initial,
        );
        control_domain.finish_initial_installation();
        let mut d = Self {
            control_domain,
            caller_palette: Default::default(),
            system_search: Default::default(),
            component_wake,
            state,
            root,
            taffy: TaffyTree::new(),
            node_map: HashMap::new(),
            bindings: crate::bound_dispatch::BoundNodeMap::new(),
            history: None,
            action_palette: None,
            root_lifetime: uuid::Uuid::new_v4(),
            framework_input: std::rc::Rc::new(std::cell::Cell::new(false)),
            component_close_reports: vec![],
            root_node: NodeId::from(0usize),
            child_node: NodeId::from(0usize),
            document_history: Box::new(crate::standalone_history_host::StandaloneHistoryHost::new()),
            history_chrome_enabled: false,
            history_layout: None,
            history_focus_pending: false,
            history_pointer_owned: false,
            text,
            theme,
            scene: Scene::new(),
            records: None,
            record_persistence: None,
            room_index: None,
            presence: PresenceCtx::empty(),
            presence_redraw: false,
            permissions: PermissionContext::permissive(),
            modifiers: winit::event::Modifiers::default(),
            interaction: crate::interaction::InteractionEnvironment::default(),
            platform: crate::platform::PlatformConventions::default(),
            touch_pointer: crate::interaction::TouchPointerEmulator::default(),
            dispatching_touch_pointer: false,
            viewport,
            last_layout_viewport: None,
            input_scale: 1.0,
            host_pointer_position: None,
            focused: None,
            focus_trap_active: false,
            active_input_instance: None,
            saved_instance_focus: HashMap::new(),
            input_sequence: std::cell::Cell::new(0),
            observed_input_owner: std::cell::RefCell::new(None),
            focus_before_trap: None,
            author: author(),
            action_registry: tilo_actions::ActionRegistry::new(),
            action_transport: Box::new(tilo_actions::LocalTransport::new()),
            pending_control_invocations: HashSet::new(),
            control_failures: Vec::new(),
            bound_invocations: HashMap::new(),
            profiler: None,
            last_send_skipped_relayout: false,
            pointer_redraw_diagnostics: false,
            pointer_redraw_sources: Vec::new(),
            layout_built: false,
            full_relayout_override: None,
            last_relayout: RelayoutStats::default(),
            relayout_frames: 0,
            retained_frames: 0,
            retained_geometry_check_every: 0,
            verifying_geometry: false,
            root_type: None,
            host,
        };
        // Match `App::rebuild_theme`: reduced motion is both a paint-time
        // theme property and a lifecycle policy retained widgets use to settle
        // their own transitions and stop requesting frames.  Headless shots
        // must exercise both halves of that contract.
        d.root.set_motion_reduced(d.theme.reduce_motion);
        d.relayout();
        d
    }

    /// Mount one concrete widget as the root so a test can read and adjust
    /// it through [`Self::root`] and [`Self::root_mut`] between frames.
    pub fn for_root<W: Widget + 'static>(
        state: State,
        root: W,
        viewport: (u32, u32),
        theme: Theme,
    ) -> Self {
        Self::for_root_with(
            state,
            root,
            viewport,
            FrameOptions {
                theme: Some(theme),
                ..FrameOptions::default()
            },
        )
    }

    /// [`Self::for_root`] with every construction option spelled out.
    pub fn for_root_with<W: Widget + 'static>(
        state: State,
        root: W,
        viewport: (u32, u32),
        options: FrameOptions,
    ) -> Self {
        let FrameOptions {
            theme,
            text,
            host_style,
            content_height,
        } = options;
        let mut harness = Self::with_inputs_styled(
            state,
            Box::new(root),
            viewport,
            theme.unwrap_or_else(Theme::dark),
            text.unwrap_or_default(),
            HostLayout {
                style: host_style.unwrap_or_default(),
                content_height,
            },
        );
        harness.root_type = Some(std::any::TypeId::of::<W>());
        harness
    }

    /// The root mounted by [`Self::for_root`].
    ///
    /// # Panics
    /// When the root was not mounted through [`Self::for_root`] as a `W`.
    #[must_use]
    pub fn root<W: Widget + 'static>(&self) -> &W {
        assert_eq!(
            self.root_type,
            Some(std::any::TypeId::of::<W>()),
            "FrameHarness::root: the root was not mounted as this type"
        );
        // SAFETY: `root_type` records the concrete type `root` was boxed from
        // and is cleared whenever `root` is replaced.
        unsafe { &*std::ptr::from_ref::<dyn Widget>(self.root.as_ref()).cast::<W>() }
    }

    /// Mutable access to the root mounted by [`Self::for_root`]. Anything
    /// may change through it, so the next frame recomposes from scratch.
    ///
    /// # Panics
    /// When the root was not mounted through [`Self::for_root`] as a `W`.
    pub fn root_mut<W: Widget + 'static>(&mut self) -> &mut W {
        assert_eq!(
            self.root_type,
            Some(std::any::TypeId::of::<W>()),
            "FrameHarness::root_mut: the root was not mounted as this type"
        );
        self.discard_layout_tree();
        // SAFETY: as in `root`; the raw layout maps that pointed into this
        // tree were discarded above.
        unsafe { &mut *std::ptr::from_mut::<dyn Widget>(self.root.as_mut()).cast::<W>() }
    }

    /// The root mounted by [`Self::for_root`] together with the live
    /// document, for calls that take both. Invalidates as [`Self::root_mut`].
    ///
    /// # Panics
    /// When the root was not mounted through [`Self::for_root`] as a `W`.
    pub fn root_and_state_mut<W: Widget + 'static>(&mut self) -> (&mut W, &mut State) {
        assert_eq!(
            self.root_type,
            Some(std::any::TypeId::of::<W>()),
            "FrameHarness::root_and_state_mut: the root was not mounted as this type"
        );
        self.discard_layout_tree();
        // SAFETY: as in `root_mut`.
        let root =
            unsafe { &mut *std::ptr::from_mut::<dyn Widget>(self.root.as_mut()).cast::<W>() };
        (root, &mut self.state)
    }

    /// Run one frame at `viewport`, even when neither the size nor the
    /// composition changed since the last one.
    ///
    /// # Errors
    /// As [`Self::resize`].
    pub fn relayout_at(
        &mut self,
        viewport: (u32, u32),
    ) -> Result<(), crate::widget::WidgetLayoutError> {
        self.viewport = viewport;
        self.try_relayout()
    }

    /// Run one frame that rebuilds the whole layout tree, as a structural
    /// change does, even when no widget reported one.
    ///
    /// # Errors
    /// As [`Self::resize`].
    pub fn recompose(&mut self) -> Result<(), crate::widget::WidgetLayoutError> {
        self.invalidate_layout_tree();
        self.try_relayout()
    }

    /// [`Self::relayout_at`] under `theme`, which later frames keep.
    ///
    /// # Errors
    /// As [`Self::resize`].
    pub fn relayout_themed(
        &mut self,
        viewport: (u32, u32),
        theme: Theme,
    ) -> Result<(), crate::widget::WidgetLayoutError> {
        self.theme = theme;
        self.text.set_text_scale(self.theme.text_scale);
        self.invalidate_layout_tree();
        self.relayout_at(viewport)
    }

    /// Hand `event` straight to the root's `handle_event` against the last
    /// frame's layout, bypassing host focus routing, and report whether the
    /// root marked it handled. Nothing is laid out until the next frame,
    /// which rebuilds only if the root reports a structural change.
    pub fn dispatch_to_root(&mut self, event: &WindowEvent) -> bool {
        let modifiers = self.modifiers;
        self.dispatch_to_root_with(event, &modifiers)
    }

    /// [`Self::dispatch_to_root`] with an explicit modifier state for this
    /// one event.
    pub fn dispatch_to_root_with(
        &mut self,
        event: &WindowEvent,
        modifiers: &winit::event::Modifiers,
    ) -> bool {
        self.dispatch_to_root_outcome(event, modifiers).handled
    }

    /// [`Self::dispatch_to_root_with`], also reporting the node the root
    /// asked to focus.
    pub fn dispatch_to_root_outcome(
        &mut self,
        event: &WindowEvent,
        modifiers: &winit::event::Modifiers,
    ) -> RootDispatch {
        let mut handled = false;
        let mut focus_target = None;
        self.root.handle_event(
            event,
            &mut EventCtx {
                scale_factor: self.input_scale,
                modifiers,
                state: &mut self.state.doc,
                path: &self.state.root,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: self.child_node,
                taffy: &self.taffy,
            },
        );
        RootDispatch {
            handled,
            focus_target,
        }
    }

    /// The root widget, whatever it was mounted as.
    #[must_use]
    pub fn root_widget(&self) -> &dyn Widget {
        self.root.as_ref()
    }

    /// The live document, read-only.
    #[must_use]
    pub fn state(&self) -> &State {
        &self.state
    }

    /// Theme the next frame is drawn with.
    #[must_use]
    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Resize in logical pixels and settle geometry-dependent composition
    /// before exposing targets, accessibility, or a drawable frame. Uses the
    /// same bounded prepaint lifecycle as the live App host. Equal sizes are a
    /// no-op; paint and scroll-only frames do not trigger viewport refreshes.
    ///
    /// # Errors
    /// Returns a layout error if measurement or bounded composition settlement
    /// fails. The failed frame is not painted; callers may retry the resize.
    pub fn resize(&mut self, viewport: (u32, u32)) -> Result<(), crate::widget::WidgetLayoutError> {
        if self.viewport == viewport && self.last_layout_viewport == Some(viewport) {
            return Ok(());
        }
        self.viewport = viewport;
        self.try_relayout()
    }

    /// Register replayable document action decoders for a driven app.
    /// Mirrors [`crate::App::action_registry_mut`].
    /// Install an authenticated peer's action transport before dispatching actions.
    /// Existing authority/owner/admission checks still run in the bound dispatcher.
    pub fn set_action_transport(
        &mut self,
        transport: Box<dyn tilo_actions::ActionTransport>,
    ) -> Result<(), String> {
        if !self.bound_invocations.is_empty() || !self.pending_control_invocations.is_empty() {
            return Err("cannot replace transport while actions are pending".into());
        }
        self.action_transport = transport;
        Ok(())
    }

    pub fn action_registry_mut(&mut self) -> &mut tilo_actions::ActionRegistry {
        self.pump_component_wakes();
        &mut self.action_registry
    }

    /// Undo through the component that owns the action decoders. The harness
    /// registry is the fallback for a widget that registered actions on the
    /// driver itself.
    fn history_through_component(
        root: &mut dyn crate::widget::Widget,
        doc: &mut automerge::AutoCommit,
        binding: &crate::bound_dispatch::CallbackBinding,
        request: crate::undo::UndoRequest,
        fallback: &tilo_actions::ActionRegistry,
    ) -> bool {
        let mut acted = None;
        let mut visit = |found: &crate::bound_dispatch::CallbackBinding,
                         _: &mut dyn crate::widget::AppWidget,
                         registry: &tilo_actions::ActionRegistry| {
            if found.same_component(binding) {
                acted = Some(
                    found
                        .history_request_with_registry(doc, request, Some(registry))
                        .ok()
                        .flatten()
                        .is_some(),
                );
            }
            None
        };
        let visited = root.visit_components(&mut crate::bound_dispatch::ComponentRequest {
            target: Some(binding),
            found: false,
            visit: &mut visit,
        });
        if let Some(acted) = acted {
            return acted;
        }
        if visited.is_err() {
            return false;
        }
        binding
            .history_request_with_registry(doc, request, Some(fallback))
            .ok()
            .flatten()
            .is_some()
    }

    /// Undo the last action, the way the live host does — through
    /// [`State::undo_as`](crate::state::State::undo_as) with this driver's
    /// author. Returns whether anything was undone.
    ///
    /// The driver's one undo door: `Ctrl+Z` and an
    /// [`UndoButton`](crate::widget::UndoButton) both arrive here, so a
    /// scenario cannot pass against a path the user never takes.
    pub fn perform_undo(&mut self) -> bool {
        self.pump_component_wakes();
        if crate::history_mode::embedded::keyboard_captured(
            self.root.as_ref(),
            self.focused,
            &self.taffy,
        ) || self
            .history
            .as_ref()
            .is_some_and(|history| history.captures_input())
        {
            return false;
        }
        let author = self.author.clone();
        let done = if let Some(binding) = self
            .focused
            .and_then(|node| self.bindings.binding_for(node))
            .or_else(|| self.root.callback_binding())
        {
            Self::history_through_component(
                &mut *self.root,
                &mut self.state.doc,
                &binding,
                crate::undo::UndoRequest::Undo,
                &self.action_registry,
            )
        } else if let Some(owner) = self.focused.and_then(|node| self.bindings.legacy_for(node)) {
            crate::bound_dispatch::legacy_history_request(
                &owner,
                crate::undo::UndoRequest::Undo,
                &self.permissions,
                &author,
            )
            .ok()
            .flatten()
            .is_some()
        } else {
            self.state.undo_as(Some(&author), None, None).is_some()
        };
        if done {
            self.relayout();
        }
        done
    }

    /// Redo the last undone action. The mirror of [`Self::perform_undo`].
    pub fn perform_redo(&mut self) -> bool {
        self.pump_component_wakes();
        if crate::history_mode::embedded::keyboard_captured(
            self.root.as_ref(),
            self.focused,
            &self.taffy,
        ) || self
            .history
            .as_ref()
            .is_some_and(|history| history.captures_input())
        {
            return false;
        }
        let author = self.author.clone();
        let done = if let Some(binding) = self
            .focused
            .and_then(|node| self.bindings.binding_for(node))
            .or_else(|| self.root.callback_binding())
        {
            Self::history_through_component(
                &mut *self.root,
                &mut self.state.doc,
                &binding,
                crate::undo::UndoRequest::Redo,
                &self.action_registry,
            )
        } else if let Some(owner) = self.focused.and_then(|node| self.bindings.legacy_for(node)) {
            crate::bound_dispatch::legacy_history_request(
                &owner,
                crate::undo::UndoRequest::Redo,
                &self.permissions,
                &author,
            )
            .ok()
            .flatten()
            .is_some()
        } else {
            self.state.redo_as(Some(&author), None, None).is_some()
        };
        if done {
            self.relayout();
        }
        done
    }

    /// History selected by the focused component, or the outer history when unbound.
    /// Widget publication remains specific to each widget's own component binding.
    pub fn undo_status(&mut self) -> crate::undo::UndoStatus {
        self.pump_component_wakes();
        if let Some(binding) = self
            .focused
            .and_then(|node| self.bindings.binding_for(node))
            .or_else(|| self.root.callback_binding())
        {
            binding.undo_status(&mut self.state.doc).unwrap_or_default()
        } else if let Some(status) = self
            .focused
            .and_then(|node| self.bindings.legacy_status(node, &self.author))
        {
            status
        } else {
            self.state.undo_status(Some(&self.author))
        }
    }

    /// Perform any undo/redo the widget tree asked for. Returns whether
    /// anything happened.
    fn drain_undo_requests(&mut self) -> bool {
        let mut requests = Vec::new();
        let mut visited = HashSet::new();
        for (&node, pointer) in &self.node_map {
            // SAFETY: node_map pointers are valid until the next `relayout`.
            if !visited.insert(unsafe { crate::widget::node_sweep_identity(*pointer) }) {
                continue;
            }
            // SAFETY: same invariant as every other `node_map` dereference in
            // this driver — pointers are valid until the next `relayout`.
            if let Ok(Some(req)) = unsafe {
                crate::bound_dispatch::mutate_bound_node(
                    &self.bindings,
                    &self.node_map,
                    node,
                    |widget| crate::undo::take_undo_request(widget),
                )
            } {
                requests.push((
                    self.bindings.binding_for(node),
                    self.bindings.legacy_for(node),
                    req,
                ));
            }
        }
        let mut acted = false;
        for (binding, legacy, req) in requests {
            acted |= if let Some(binding) = binding {
                Self::history_through_component(
                    &mut *self.root,
                    &mut self.state.doc,
                    &binding,
                    req,
                    &self.action_registry,
                )
            } else if let Some(owner) = legacy {
                crate::bound_dispatch::legacy_history_request(
                    &owner,
                    req,
                    &self.permissions,
                    &self.author,
                )
                .ok()
                .flatten()
                .is_some()
            } else {
                match req {
                    crate::undo::UndoRequest::Undo => self.perform_undo(),
                    crate::undo::UndoRequest::Redo => self.perform_redo(),
                }
            };
        }
        acted
    }

    /// Hand the live undo status to every laid-out widget, so a control
    /// renders and reports the same truth it would live.
    fn publish_undo_status(&mut self) {
        // The legacy status warmer observes live heads (and can therefore
        // close an unfinished transaction). Framework navigation must not.
        if self.framework_input.get()
            || self
                .history
                .as_ref()
                .is_some_and(|history| history.captures_input())
        {
            return;
        }
        let snapshots = crate::bound_dispatch::discover_bound_resources(
            self.root.as_ref(),
            &self.state.doc,
            None,
        )
        .unwrap_or_default();
        let _ = crate::bound_dispatch::refresh_bound_controls(self.root.as_mut(), &snapshots);
        if let Err(error) = crate::bound_dispatch::legacy_document::refresh_legacy_controls(
            self.root.as_mut(),
            &self.permissions,
            &self.author,
        ) {
            tracing::warn!(%error, "headless legacy availability refused");
        }
        let embedded = crate::history_mode::embedded::has_surfaces(self.root.as_ref());
        // Preserve the legacy status boundary only outside managed History composition.
        if !embedded
            && (self.state.can_undo_as(Some(&self.author))
                || self.state.can_redo_as(Some(&self.author)))
        {
            self.state.doc.get_heads();
        }
        let mut outer_status = None;
        let mut visited = HashSet::new();
        for (&node, pointer) in &self.node_map {
            // SAFETY: node_map pointers are valid until the next `relayout`.
            if !visited.insert(unsafe { crate::widget::node_sweep_identity(*pointer) }) {
                continue;
            }
            let binding = self.bindings.binding_for(node);
            // Preserve bound history label warming across pending edits.
            let bound_status = binding
                .as_ref()
                .map(|binding| binding.undo_status(&mut self.state.doc).unwrap_or_default())
                .or_else(|| self.bindings.legacy_status(node, &self.author));
            let mut status = || {
                bound_status.clone().unwrap_or_else(|| {
                    outer_status
                        .get_or_insert_with(|| {
                            if embedded {
                                self.state.undo_status_snapshot(Some(&self.author))
                            } else {
                                self.state.undo_status(Some(&self.author))
                            }
                        })
                        .clone()
                })
            };
            // SAFETY: as in `drain_undo_requests`.
            let _ = unsafe {
                crate::bound_dispatch::mutate_bound_node(
                    &self.bindings,
                    &self.node_map,
                    node,
                    |widget| crate::undo::set_undo_status(widget, &mut status),
                )
            };
        }
    }
    /// Use a different palette for subsequent frames.
    pub fn set_theme(&mut self, theme: Theme) {
        self.pump_component_wakes();
        self.theme = theme;
        self.text.set_text_scale(self.theme.text_scale);
        self.invalidate_layout_tree();
        self.relayout();
    }

    /// Set the input capabilities for the next frame without synthesising an
    /// OS event. Controls receive the same propagation as in the live host.
    pub fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        self.pump_component_wakes();
        self.interaction = environment;
        self.root.set_interaction_environment(environment);
        self.invalidate_layout_tree();
        self.relayout();
    }

    pub fn interaction_environment(&self) -> crate::interaction::InteractionEnvironment {
        self.drain_component_wakes();
        self.interaction
    }

    /// Switch platform HIG behavior without cross-compiling the test.
    pub fn set_platform_conventions(&mut self, conventions: crate::platform::PlatformConventions) {
        self.pump_component_wakes();
        self.platform = conventions;
        self.root.set_platform_conventions(conventions);
        self.invalidate_layout_tree();
        self.relayout();
    }

    pub fn platform_conventions(&self) -> crate::platform::PlatformConventions {
        self.drain_component_wakes();
        self.platform
    }

    /// Read wake demand without materializing document text or semantic nodes.
    pub fn frame_schedule(&self) -> FrameSchedule {
        self.drain_component_wakes();
        let queued_work = self.presence_redraw
            || !self.bound_invocations.is_empty()
            || !self.pending_control_invocations.is_empty()
            || self
                .component_wake
                .as_ref()
                .is_some_and(|wake| wake.dirty.get());
        self.with_schedule_roots(|roots| FrameSchedule {
            needs_animation_frame: roots.iter().any(|root| root.needs_animation_frame()),
            next_wake_deadline: roots
                .iter()
                .filter_map(|root| root.next_wake_deadline())
                .min(),
            needs_redraw: roots.iter().any(|root| root.needs_redraw()) || queued_work,
            queued_work,
        })
    }

    /// The roots [`Self::frame_schedule`] reads demand from.
    fn with_schedule_roots<R>(&self, visit: impl FnOnce(Vec<&dyn Widget>) -> R) -> R {
        if self.history_layout.is_some() && self.system_search_presentation().is_none() {
            return self.presentation(|history, _| {
                let mut roots: Vec<&dyn Widget> = vec![history];
                if let Some(palette) = self
                    .action_palette
                    .as_deref()
                    .filter(|palette| palette.is_presented())
                {
                    roots.push(palette);
                }
                visit(roots)
            });
        }
        visit(self.presentation_roots())
    }

    /// The widgets behind [`Self::frame_schedule`]'s animation demand and due
    /// wake deadline, each as the path of widget names from its root to the
    /// deepest widget whose demand none of its children explains. A
    /// diagnostic for idle redraws; it walks only the demanding branches.
    pub fn frame_requesters(&self) -> Vec<(String, FrameDemand)> {
        fn visit(
            widget: &dyn Widget,
            now: crate::time::Instant,
            path: &mut Vec<String>,
            out: &mut Vec<(String, FrameDemand)>,
        ) -> (bool, bool) {
            let animating = widget.needs_animation_frame();
            let due = widget.next_wake_deadline().is_some_and(|at| at <= now);
            if !animating && !due {
                return (false, false);
            }
            path.push(widget.name().to_owned());
            let (mut child_animating, mut child_due) = (false, false);
            widget.for_each_child(&mut |child| {
                let (animating, due) = visit(child, now, path, out);
                child_animating |= animating;
                child_due |= due;
            });
            if animating && !child_animating {
                out.push((path.join("/"), FrameDemand::Animation));
            }
            if due && !child_due {
                out.push((path.join("/"), FrameDemand::Deadline));
            }
            path.pop();
            (animating, due)
        }
        self.drain_component_wakes();
        let now = crate::time::now();
        self.with_schedule_roots(|roots| {
            let mut out = Vec::new();
            for root in roots {
                visit(root, now, &mut Vec::new(), &mut out);
            }
            out
        })
    }

    /// Drain optional canonical font assets demanded by the most recent
    /// authoritative shaping pass.
    pub fn take_browser_font_demands(&mut self) -> Vec<String> {
        self.text.take_browser_font_demands()
    }

    /// Install one verified optional font, invalidate text caches, then
    /// rebuild layout before the browser is allowed to request a redraw.
    /// # Errors
    /// Propagates canonical font integrity and face-validation failures.
    pub fn install_browser_font(
        &mut self,
        name: &str,
        bytes: Vec<u8>,
    ) -> Result<bool, crate::font_catalog::FontCatalogError> {
        let changed = self.text.install_browser_font(name, bytes)?;
        if changed {
            self.invalidate_layout_tree();
            self.relayout();
        }
        Ok(changed)
    }

    /// Rearm shaping demand after a host fetch failed, without producing a
    /// frame or mutating focus/document state on asynchronous completion.
    pub fn browser_font_failed(&mut self) {
        self.text.browser_font_failed();
        self.invalidate_layout_tree();
    }

    /// Diagnostic paths of dirty subtrees, without inspecting document content.
    /// Queried outside a measured frame; not part of normal scheduling.
    pub fn redraw_sources(&self) -> Vec<String> {
        fn visit(widget: &dyn Widget, parent: &str, out: &mut Vec<String>) {
            let Ok(_entry) = crate::bound_dispatch::enter_widget(widget) else {
                return;
            };
            if !widget.needs_redraw() {
                return;
            }
            let path = format!("{parent}/{}", widget.name());
            out.push(path.clone());
            for child in widget.children() {
                visit(child, &path, out);
            }
        }
        self.drain_component_wakes();
        let mut result = Vec::new();
        if self.history_layout.is_some() && self.system_search_presentation().is_none() {
            self.presentation(|history, _| {
                visit(history, "", &mut result);
                if let Some(palette) = self
                    .action_palette
                    .as_deref()
                    .filter(|palette| palette.is_presented())
                {
                    visit(palette, "", &mut result);
                }
            });
        } else {
            for root in self.presentation_roots() {
                visit(root, "", &mut result);
            }
        }
        result
    }

    /// Borrow the completed scene. Use `frame_output` when semantics are needed.
    pub fn frame_render_input(&self) -> FrameRenderInput<'_> {
        self.drain_component_wakes();
        FrameRenderInput {
            scene: &self.scene,
            background: self.theme.bg_app,
        }
    }

    /// Inspect a completed frame without creating a GPU, worker, or window.
    pub fn frame_output(&self) -> FrameOutput<'_> {
        let schedule = self.frame_schedule();
        FrameOutput {
            scene: &self.scene,
            background: self.theme.bg_app,
            logical_size: self.viewport,
            device_size: (
                (f64::from(self.viewport.0) * self.input_scale).ceil() as u32,
                (f64::from(self.viewport.1) * self.input_scale).ceil() as u32,
            ),
            semantics: self.inspection(),
            ime: self.ime_window(),
            needs_animation_frame: schedule.needs_animation_frame,
            next_wake_deadline: schedule.next_wake_deadline,
            needs_redraw: schedule.needs_redraw,
        }
    }

    /// The IME window of the current keyboard owner: the `ime` of
    /// [`Self::frame_output`] without building the whole inspection tree.
    pub fn ime_window(&self) -> Option<crate::ime::ImeWindowState> {
        self.system_search_ime()
            .or_else(|| {
                self.caller_palette.palette.as_ref().and_then(|p| {
                    crate::ime::ImeWindowState::from_widget(p.presentation_widgets().0)
                })
            })
            .or_else(|| {
                self.focused.and_then(|id| {
                    self.inspect_node(id, crate::ime::ImeWindowState::from_widget)
                        .ok()
                        .flatten()
                })
            })
    }

    /// Pump component wakes and animate using the production update/layout/draw passes.
    /// Returns whether a new scene was produced. The host schedules this on its frame clock.
    pub fn poll_frame(&mut self) -> Result<bool, crate::widget::WidgetLayoutError> {
        let frame = self.frame_schedule();
        if !frame.needs_redraw
            && !frame.needs_animation_frame
            && frame
                .next_wake_deadline
                .is_none_or(|deadline| deadline > crate::time::now())
        {
            return Ok(false);
        }
        self.host_tick();
        self.try_relayout()?;
        Ok(true)
    }

    /// Rasterise the most recently laid-out frame to a PNG.
    ///
    /// `new`, input delivery, and `set_theme` all finish with `relayout`, so
    /// this never runs a separate screenshot-only widget pipeline. A browser
    /// page cannot read a frame back synchronously: there nothing is written.
    pub fn render_png(&self, out: &std::path::Path) {
        self.render_png_on(out, vello::peniko::Color::TRANSPARENT);
    }

    /// Capture the composited native-window appearance, including its theme
    /// background. Use this for rendered contrast measurements. Nothing is
    /// written in a browser page, as for [`FrameHarness::render_png`].
    pub fn render_window_png(&self, out: &std::path::Path) {
        self.render_png_on(out, self.theme.bg_app);
    }

    /// Rasterise the most recently laid-out frame over a transparent
    /// background, as tightly packed RGBA8 rows `viewport.0` pixels wide.
    /// Empty in a browser page, which cannot read a frame back synchronously.
    pub fn render_rgba(&self) -> Vec<u8> {
        self.render_rgba_on(vello::peniko::Color::TRANSPARENT)
    }

    /// [`FrameHarness::render_rgba`] over the theme background, as the
    /// native window composites it.
    pub fn render_window_rgba(&self) -> Vec<u8> {
        self.render_rgba_on(self.theme.bg_app)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn render_rgba_on(&self, background: vello::peniko::Color) -> Vec<u8> {
        self.drain_component_wakes();
        let (w, h) = self.viewport;
        crate::offscreen::OffscreenRenderer::new(w, h).render_resilient(&self.scene, background)
    }

    #[cfg(target_arch = "wasm32")]
    fn render_rgba_on(&self, _background: vello::peniko::Color) -> Vec<u8> {
        tracing::warn!("a browser page has no offscreen renderer; the frame is empty");
        Vec::new()
    }

    #[cfg(target_arch = "wasm32")]
    fn render_png_on(&self, out: &std::path::Path, _background: vello::peniko::Color) {
        tracing::warn!(path = %out.display(), "a browser page has no offscreen renderer; no PNG written");
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn render_png_on(&self, out: &std::path::Path, background: vello::peniko::Color) {
        self.drain_component_wakes();
        if let Some(parent) = out.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let (w, h) = self.viewport;
        let mut renderer = crate::offscreen::OffscreenRenderer::new(w, h);
        renderer.render_png(&self.scene, background, out);
    }

    /// Drive a record-aware app: the live document is a record's, and the
    /// widget names which one. Mirrors [`crate::App::with_records`].
    #[must_use]
    pub fn with_records(
        mut self,
        records: crate::record::RecordSet,
        room_index: automerge::AutoCommit,
    ) -> Self {
        self.attach_records(records, room_index);
        self
    }

    fn attach_records(
        &mut self,
        mut records: crate::record::RecordSet,
        room_index: automerge::AutoCommit,
    ) {
        records.discover_room(&room_index);
        self.records = Some(records);
        self.room_index = Some(room_index);
        self.relayout();
    }

    /// Configure the same per-record store as the live host, including apps
    /// whose asynchronous first open supplies no record during construction.
    fn attach_prepared_record_persistence(
        &mut self,
        records: crate::record::RecordSet,
        room_index: automerge::AutoCommit,
    ) {
        self.record_persistence = Some((records, room_index));
        self.tick_records();
    }

    fn tick_records(&mut self) {
        if self.records.is_none()
            && crate::record::capability::record_intent(self.root.as_ref()).is_some()
            && let Some((mut records, room_index)) = self.record_persistence.take()
        {
            records.discover_room(&room_index);
            self.records = Some(records);
            self.room_index = Some(room_index);
        }
        if let Some(set) = self.records.as_mut() {
            crate::record::tick(set, &mut self.state, self.room_index.as_mut(), &*self.root);
        }
    }

    fn persistence_document(&mut self) -> &mut automerge::AutoCommit {
        self.room_index.as_mut().unwrap_or(&mut self.state.doc)
    }

    /// The records this driver has open, if the app under test uses them.
    pub fn records(&self) -> Option<&crate::record::RecordSet> {
        self.drain_component_wakes();
        self.records.as_ref()
    }

    /// The record the root widget names, if any. Read before installing a
    /// record set so an app that does not use records is left alone.
    pub fn root_record_intent(&self) -> Option<crate::record::RecordCheckout> {
        self.drain_component_wakes();
        crate::record::capability::record_intent(self.root.as_ref())
    }

    /// Offer the live document to its record's writer. Returns whether a
    /// record set handled it — `false` means the caller should persist the
    /// document itself, as an app without records does.
    pub fn persist_tick(&mut self) -> bool {
        self.pump_component_wakes();
        if let Some(wake) = &self.component_wake {
            wake.binding.records.tick();
            return true;
        }
        match self.records.as_mut() {
            Some(set) => {
                set.tick(&mut self.state.doc);
                true
            }
            None => false,
        }
    }

    /// Write out every open record and wait for it to land.
    pub fn persist_flush(&mut self) {
        if let Err(error) = self.try_persist_flush() {
            tracing::error!(%error, "headless record persistence failed");
        }
    }

    /// Flush records while retaining storage failures for the command's exit status.
    ///
    /// # Errors
    /// Reports unavailable or failed record writers.
    pub fn try_persist_flush(&mut self) -> Result<(), String> {
        self.pump_component_wakes();
        if let Some(wake) = &self.component_wake {
            return wake.binding.records.try_flush();
        }
        match self.records.as_mut() {
            Some(set) => set.try_flush(&mut self.state.doc),
            None => Ok(()),
        }
    }

    /// Re-run the app's frame: sync from the CRDT, rebuild Taffy, apply
    /// layout, and draw. Called after every action so the next one sees
    /// where things actually moved.
    ///
    /// The draw pass isn't decoration: many widgets record their hit-test
    /// rects while drawing, so skipping it would leave every click silently
    /// missing — exactly the false-pass this harness must avoid. Nothing is
    /// rasterised; the `Scene` is encoded on the CPU and discarded.
    #[allow(
        clippy::cast_precision_loss,
        reason = "viewport width/height are pixel dimensions, always far below \
                  f32's 2^24 exact-integer range"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "one exclusive sync/build/admission/measure/apply sequence keeps layout ownership explicit"
    )]
    pub fn relayout(&mut self) {
        self.poll_touch_hold(crate::time::now());
        if let Err(error) = self.try_relayout() {
            tracing::error!(
                ?error,
                "headless: widget layout did not settle; skipping frame"
            );
        }
    }

    #[allow(
        clippy::cast_precision_loss,
        clippy::too_many_lines,
        reason = "exclusive headless frame lifecycle with f32 logical pixel sizes"
    )]
    fn try_relayout(&mut self) -> Result<(), crate::widget::WidgetLayoutError> {
        use crate::widget::layout::{
            CompositionFocus, composition_geometry, settle_widget_composition,
        };
        let viewport_changed = self.last_layout_viewport != Some(self.viewport);
        self.presence_redraw = false;
        // A failed attempt mutates the tree as well as the requested viewport.
        // Only a successful complete frame may restore this validity marker.
        self.last_layout_viewport = None;
        // Event dispatch may replace controls before this frame starts. Old
        // node pointers are invalid in that case; retained controls carry their
        // own focus until the fresh map below recovers it.
        let structure_changed = self.take_composition_dirty();
        let focus = if self.node_map.is_empty() {
            // The first frame has no previous layout focus to transfer. A
            // component's restored application view owns its initial focus.
            None
        } else if structure_changed {
            CompositionFocus::capture_live(self.root.as_ref())
        } else {
            // SAFETY: no structural mutation since these maps were admitted.
            unsafe { CompositionFocus::capture(self.focused, &self.node_map, &self.bindings) }
        };
        self.drain_component_wakes();
        if let Some(wake) = &self.component_wake {
            wake.dirty.set(false);
        }
        // Seal controls that were already queued when the framework entry
        // callback fired. Hidden update/sync output is deliberately excluded.
        if let Some(history) = &mut self.history
            && history.captures_input()
        {
            history.capture_pending_controls(
                self.root.as_mut(),
                &self.state.doc,
                self.root_lifetime,
            );
        }
        self.capture_palette_history();
        crate::history_mode::embedded::capture_entries(self.root.as_mut(), &self.state.doc);
        // Select the current record before update, then observe a first
        // asynchronous open before sync can accidentally edit the room index.
        self.tick_records();
        self.root.update(&mut self.state.doc, &self.state.root);
        self.tick_records();
        let t_sync = self.profiler.as_ref().map(|_| crate::time::now());
        self.root.sync(&mut self.state.doc, &self.state.root);
        if let Some(p) = self.profiler.as_mut() {
            p.record_since(crate::profiling::Pass::Sync, t_sync);
        }

        let (standalone_history_changed, standalone_history_topology_changed) =
            self.prepare_history_frame();
        let mut history_invalidated = self.invalidate_unsupported_history_owner();
        let was_history = self
            .history
            .as_ref()
            .is_some_and(|history| history.captures_input());
        // A refresh may replace managed History controls. Discard every raw
        // layout map before refreshing so no stale callback target survives,
        // and refresh only then: the History source changes on other threads,
        // so a refresh after a negative check could still replace controls.
        let refresh_history = self.history.as_ref().is_some_and(|history| {
            history.validate_root(self.root.as_ref()).is_err()
                || history.needs_refresh(&self.state.doc, f64::from(self.viewport.0))
        });
        if refresh_history {
            self.discard_layout_tree();
            history_invalidated = true;
            if let Some(history) = &mut self.history {
                history.set_width(f64::from(self.viewport.0));
                history.refresh(&self.state.doc);
            }
        }
        let returning_live = self
            .history
            .as_ref()
            .is_some_and(|history| history.returning_live());
        if was_history
            || returning_live
            || self
                .history
                .as_ref()
                .is_some_and(|history| history.captures_input())
        {
            // These local intents have not crossed the execution door. Never
            // carry them into preview, or back out to run against the live doc.
            if let Some(history) = &mut self.history {
                history.discard_pending_output(self.root.as_mut());
            }
        }
        if returning_live {
            if let Some(history) = &mut self.history {
                history.finish_return_to_live();
                history.flush_pending_toolbar();
            }
            self.discard_layout_tree();
            self.focused = None;
            if let Some(palette) = self.action_palette.as_deref_mut() {
                palette.resume_after_history();
            }
        }
        // Retained relayout, as `App::run_layout_pass` does it: the taffy
        // tree, node map, bindings and focus survive from frame to frame.
        // Recompose only when the composition itself changed (a widget
        // reports structure dirty, a history owner was invalidated, the
        // viewport moved, or nothing has been built yet); re-solve sizes in
        // place only when some widget reports it needs measuring; otherwise
        // just re-apply positions. `TILO_UI_FULL_RELAYOUT` forces the
        // rebuild path on every frame.
        let history_width = f64::from(self.viewport.0);
        let structure = self.full_relayout_forced_now()
            || !self.layout_built
            || viewport_changed
            || structure_changed
            || history_invalidated
            || standalone_history_topology_changed
            || self.take_composition_dirty()
            || crate::history_mode::embedded::needs_refresh(
                self.root.as_ref(),
                &self.state.doc,
                history_width,
                None,
            );
        let must_compute =
            structure || standalone_history_changed || self.root.layout_needs_measure();
        let mut stats = RelayoutStats::default();
        // Captured lazily, immediately before a build replaces controls.
        let mut view = None;
        settle_widget_composition(viewport_changed, |rebuild| {
            let t_build = self.profiler.as_ref().map(|_| crate::time::now());
            let mut rebuilt_this_pass = false;
            if structure || rebuild {
                rebuilt_this_pass = true;
                stats.rebuilt = true;
                let view = view
                    .get_or_insert_with(|| crate::view::capability::view_state(self.root.as_ref()));
                self.layout_built = false;
                self.taffy = TaffyTree::new();
                self.component_close_reports.extend(
                    crate::component_lifecycle::take_component_close_reports(self.root.as_mut()),
                );
                self.bindings.clear();
                self.node_map.clear();
                if let Some(history) = &mut self.history {
                    history.flush_pending_toolbar();
                }
                crate::history_mode::embedded::refresh(
                    self.root.as_mut(),
                    &self.state.doc,
                    f64::from(self.viewport.0),
                    None,
                );
                self.child_node = if let Some(history) = &mut self.history {
                    history.build_composition(
                        self.root.as_mut(),
                        &mut self.taffy,
                        &self.theme,
                        &mut self.node_map,
                    )
                } else {
                    self.root
                        .build_layout(&mut self.taffy, &self.theme, &mut self.node_map)
                };
                // Newly composed controls need sync. If it replaces delegates,
                // discard these raw maps without admitting or measuring them.
                self.root.sync(&mut self.state.doc, &self.state.root);
                let history_invalidated = self.invalidate_unsupported_history_owner();
                if self.take_composition_dirty() || history_invalidated {
                    self.node_map.clear();
                    return Ok((None, true));
                }
                let palette_node = self
                    .action_palette
                    .as_deref_mut()
                    .filter(|palette| palette.is_presented())
                    .map(|palette| {
                        palette.build_layout(&mut self.taffy, &self.theme, &mut self.node_map)
                    });
                let presentation_node = if self.history_chrome_enabled {
                    self.build_history_frame(palette_node)
                } else {
                    if let Some(node) = palette_node {
                        self.taffy.add_child(self.child_node, node)?;
                    }
                    // SAFETY: exclusive runner owns this exact fresh topology.
                    unsafe {
                        self.bindings.rebuild_with_retained_root(
                            self.history.as_ref().map_or_else(
                                || self.root.as_ref(),
                                |history| history.as_ref() as &dyn Widget,
                            ),
                            self.history.as_ref().map(|_| self.root.as_ref()),
                            &self.taffy,
                            self.child_node,
                            &self.node_map,
                        );
                    }
                    self.child_node
                };
                if let Some(view) = &view {
                    crate::view::capability::restore_view_state(self.root.as_mut(), view);
                }
                // Retained app controls own their ordinary event focus (some
                // intentionally focus both sidebar and editor). Restore after
                // replacement only when the new tree has lost that focus, or
                // when viewport recomposition requires identity matching.
                // SAFETY: every pointer belongs to the freshly admitted tree.
                let retained_focus = self.node_map.keys().any(|&node| unsafe {
                    crate::bound_dispatch::inspect_bound_node(
                        &self.bindings,
                        &self.node_map,
                        node,
                        |widget| widget.is_focusable() && widget.is_focused(),
                    )
                    .unwrap_or(false)
                });
                if let Some(focus) = &focus
                    && (viewport_changed || !retained_focus)
                {
                    // SAFETY: freshly admitted stable tree, exclusively owned.
                    self.focused = unsafe { focus.restore(&self.node_map, &self.bindings, None) };
                }
                let (w, h) = self.viewport;
                self.root_node = self.taffy.parent_node(
                    Style {
                        size: Size {
                            width: Dimension::length(w as f32),
                            height: if self.host.content_height {
                                Dimension::auto()
                            } else {
                                Dimension::length(h as f32)
                            },
                        },
                        ..self.host.style.clone()
                    },
                    &[presentation_node],
                );
                self.layout_built = true;
            }
            if rebuilt_this_pass || must_compute {
                if !rebuilt_this_pass {
                    // Taffy caches every node's measurement; a retained tree
                    // re-measures exactly the widgets that report a possible
                    // size change (and their ancestors, which `mark_dirty`
                    // invalidates on the way up).
                    let (marked, leaves) = self.mark_layout_dirty_nodes();
                    stats.marked_dirty = stats.marked_dirty.saturating_add(marked);
                    stats.marked_leaves = stats.marked_leaves.saturating_add(leaves);
                }
                stats.measured = true;
                let (w, h) = self.viewport;
                // SAFETY: runner exclusively owns the exact stable admitted tree.
                unsafe {
                    crate::widget::compute_widget_layout(
                        &mut self.taffy,
                        self.root_node,
                        Size {
                            width: AvailableSpace::Definite(w as f32),
                            height: if self.host.content_height {
                                AvailableSpace::MaxContent
                            } else {
                                AvailableSpace::Definite(h as f32)
                            },
                        },
                        &self.node_map,
                        &self.bindings,
                        &mut self.text,
                    )?;
                }
            }
            if let Some(profiler) = self.profiler.as_mut() {
                profiler.record_since(crate::profiling::Pass::BuildLayout, t_build);
            }
            let t_apply = self.profiler.as_ref().map(|_| crate::time::now());
            if self.history_chrome_enabled {
                self.apply_history_frame();
            } else if let Some(history) = &mut self.history {
                history.apply_composition(
                    self.root.as_mut(),
                    &self.taffy,
                    self.child_node,
                    Point::ORIGIN,
                );
            } else {
                self.root
                    .apply_layout(&self.taffy, self.child_node, Point::ORIGIN);
            }
            if let Some(profiler) = self.profiler.as_mut() {
                profiler.record_since(crate::profiling::Pass::ApplyLayout, t_apply);
            }
            let history_invalidated = self.invalidate_unsupported_history_owner();
            let dirty = self.take_composition_dirty() || history_invalidated;
            let geometry = if viewport_changed {
                composition_geometry(&self.taffy, self.root_node)?
            } else {
                vec![]
            };
            Ok((Some(geometry), dirty))
        })?;
        self.last_layout_viewport = Some(self.viewport);
        self.last_relayout = stats;
        self.relayout_frames = self.relayout_frames.saturating_add(1);
        if !stats.rebuilt {
            self.retained_frames = self.retained_frames.saturating_add(1);
        }
        if self
            .history
            .as_ref()
            .is_some_and(|history| history.has_review_request())
            || crate::history_mode::embedded::has_review(self.root.as_ref())
        {
            // Admission follows complete composition settlement. Remove raw
            // maps before replacing modal chrome, then build its fresh map.
            self.discard_layout_tree();
            self.focused = None;
            if let Some(history) = &mut self.history {
                let _ = history.service_restore(
                    &mut self.state,
                    self.root.as_ref(),
                    &self.permissions,
                    &self.author,
                );
            }
            let _ = crate::history_mode::embedded::service_reviews(
                self.root.as_mut(),
                &mut self.state,
                &self.permissions,
                &self.author,
            );
            return self.try_relayout();
        }
        if let Some(focus) = unsafe {
            crate::history_mode::embedded::restore_focus(
                self.root.as_mut(),
                &self.node_map,
                &self.bindings,
            )
        } {
            self.focused = Some(focus);
        }

        // The taffy tree was rebuilt, so the previous `focused` NodeId is
        // stale; recover it by scanning the fresh node map for whichever
        // widget still reports `is_focused()`.
        let restoring_live = self
            .history
            .as_ref()
            .is_some_and(|history| history.returning_live());
        if restoring_live {
            self.focused = self.history.as_mut().and_then(|history| unsafe {
                history.restore_focus(&self.node_map, &self.bindings)
            });
        } else {
            let allowed: Vec<_> = self
                .node_map
                .keys()
                .copied()
                .filter(|&node| {
                    (self.focus_trap_active
                        || self
                            .history
                            .as_ref()
                            .is_some_and(|history| history.captures_input())
                        || self.active_input_instance.is_none_or(|instance| {
                            self.bindings
                                .binding_for(node)
                                .is_some_and(|binding| binding.instance_id == instance)
                                && self.app_focus_eligible(node)
                        }))
                        && crate::history_mode::embedded::allows_focus(
                            self.root.as_ref(),
                            node,
                            &self.taffy,
                        )
                        && self.history.as_ref().is_none_or(|history| {
                            !history.has_saved_focus() || history.owns_node(node, &self.taffy)
                        })
                })
                .collect();
            // An exact owner restored above outranks unrelated retained focus
            // flags (several mounted editors may initially report focus).
            self.focused = self.focused.filter(|node| {
                allowed.contains(node)
                    && self
                        .inspect_node(*node, |widget| {
                            widget.is_focusable() && widget.is_enabled() && widget.is_focused()
                        })
                        .unwrap_or(false)
            });
            if self.focused.is_none() {
                // SAFETY: these maps describe this exclusive runner's current tree.
                self.focused = unsafe {
                    CompositionFocus::current(&self.node_map, &self.bindings, Some(&allowed))
                };
            }
        }

        self.update_focus_trap(focus);

        // After layout (so a control built this pass is in `node_map`) and
        // before draw and any accessibility read — a shot must show the
        // enabled state the user would see.
        self.publish_undo_status();

        let t_draw = self.profiler.as_ref().map(|_| crate::time::now());
        self.scene.reset();
        // Cull against the window exactly as the live host does, so offscreen
        // frames paint (and cost) what a window would.
        let previous_viewport = self.presence.push_viewport(Rect::new(
            0.0,
            0.0,
            f64::from(self.viewport.0),
            f64::from(self.viewport.1),
        ));
        if let Some(history) = &mut self.history {
            history.draw(
                &mut self.scene,
                Affine::IDENTITY,
                &mut self.text,
                &self.theme,
                &self.presence,
            );
            history.mark_drawn();
        }
        if self
            .history
            .as_ref()
            .is_none_or(|history| history.live_visible())
            && self.history_layout.is_none_or(|layout| layout.app_visible)
        {
            crate::widget::native_view_frame_start(self.root.as_mut());
            self.root.draw_with_presence(
                &mut self.scene,
                Affine::IDENTITY,
                &mut self.text,
                &self.theme,
                &self.presence,
                &self.permissions,
            );
            self.root.draw_overlay(
                &mut self.scene,
                Affine::IDENTITY,
                &mut self.text,
                &self.theme,
                &self.presence,
            );
            self.root.mark_drawn();
        }
        if self.history_chrome_enabled {
            let context = crate::standalone_history::HistoryReadContext {
                document: &self.state.doc,
                active_record: self.records.as_ref().and_then(|records| records.active()),
                document_identity: self.state.document_identity(),
                permissions: &self.permissions,
            };
            let drawn = self
                .document_history
                .draw(
                    &context,
                    &mut self.scene,
                    Affine::IDENTITY,
                    &mut self.text,
                    &self.theme,
                    &self.presence,
                )
                .map_err(|_| crate::widget::WidgetLayoutError::CompositionDidNotSettle);
            if let Err(error) = drawn {
                self.presence.pop_viewport(previous_viewport);
                return Err(error);
            }
        }
        if let Some(palette) = self
            .action_palette
            .as_deref_mut()
            .filter(|p| p.is_presented())
        {
            let width = f64::from(self.viewport.0);
            let card = (width * 0.6).clamp(360.0, 640.0);
            palette.layout_with_text(
                &self.theme,
                Point::new(
                    ((width - card) / 2.0).max(0.0),
                    (f64::from(self.viewport.1) * 0.12).max(24.0),
                ),
                card,
                &mut self.text,
            );
            palette.draw(
                &mut self.scene,
                Affine::IDENTITY,
                &mut self.text,
                &self.theme,
                &self.presence,
            );
        }
        self.presence.pop_viewport(previous_viewport);
        if let Some(p) = self.profiler.as_mut() {
            p.record_since(crate::profiling::Pass::Draw, t_draw);
        }

        // Focus was already recovered for this tree before draw, with the
        // active-instance filter a second unfiltered pass would undo.
        self.draw_caller_palette();
        self.refresh_system_search();
        self.draw_system_search();

        if self
            .action_palette
            .as_deref()
            .is_some_and(|palette| palette.is_presented())
        {
            self.focused = self.focus_order().first().copied();
        }

        if let Some(history) = &mut self.history {
            if history.has_settled_control_return() {
                history.finish_control_return();
            }
        }
        crate::history_mode::embedded::finish_returns(self.root.as_mut());

        self.restore_history_focus();

        // One `relayout` is one rendered frame; commit it, folding in any
        // `handle_event` time banked by a preceding `send`.
        if let Some(p) = self.profiler.as_mut() {
            p.commit_frame_summed();
        }
        self.check_retained_geometry_if_due();
        Ok(())
    }

    /// Drive production-equivalent frames until `condition` becomes true or
    /// `timeout` expires.
    ///
    /// This is the deterministic test seam for worker completions and
    /// asynchronous model notifications. Each attempt runs update, sync,
    /// layout, and draw. Between attempts the thread parks on the current
    /// [`crate::background::Background`]: a pumped executor runs its queue and
    /// advances its virtual clock instead of waiting in real time.
    pub fn wait_until(
        &mut self,
        timeout: crate::time::Duration,
        mut condition: impl FnMut(&Self) -> bool,
    ) -> bool {
        self.relayout();
        self.idle_control_tick();
        self.relayout();
        if condition(self) {
            return true;
        }
        let background = crate::background::Background::current();
        let Some(deadline) = background.now().checked_add(timeout) else {
            return false;
        };
        loop {
            let now = background.now();
            if now >= deadline {
                return false;
            }
            // Park on the executor so background work completing (or a
            // pumped clock advancing) ends the wait early. Work that settles
            // outside the executor's view is still noticed within a slice.
            let slice = now
                .checked_add(crate::time::Duration::from_millis(1))
                .unwrap_or(deadline);
            background.park(slice.min(deadline));
            self.relayout();
            self.idle_control_tick();
            self.relayout();
            if condition(self) {
                return true;
            }
        }
    }

    /// Run the normal frame pipeline until requested visual work completes.
    ///
    /// # Errors
    /// Returns an error instead of capturing an unfinished placeholder when
    /// the work does not complete within `timeout`.
    pub fn wait_for_visuals(&mut self, timeout: crate::time::Duration) -> Result<(), String> {
        if self.wait_until(timeout, |driver| !driver.root.has_pending_visual_work()) {
            Ok(())
        } else {
            Err("Timed out waiting for visual work to finish".into())
        }
    }

    /// Wait until a retained widget with `name` is present in the live layout.
    ///
    /// # Errors
    ///
    /// Returns a timeout error naming the missing target.
    pub fn wait_for_named(
        &mut self,
        name: &str,
        timeout: crate::time::Duration,
    ) -> Result<(), String> {
        self.pump_component_wakes();
        if self.wait_until(timeout, |driver| driver.targets().contains_key(name)) {
            Ok(())
        } else {
            Err(format!(
                "timed out after {} ms waiting for widget {name:?}",
                timeout.as_millis()
            ))
        }
    }

    /// The keyboard-focusable widgets in tree order — the same DFS
    /// `crate::app::App::update_focusable_order` walks, so a Tab that
    /// reaches a widget here reaches it in the live app too. The deepest modal
    /// focus trap confines this order using the same policy as the native host.
    pub fn focus_order(&self) -> Vec<NodeId> {
        self.drain_component_wakes();
        let mut order = Vec::new();
        if self
            .action_palette
            .as_deref()
            .is_some_and(|palette| palette.is_presented())
        {
            return self
                .node_map
                .keys()
                .copied()
                .filter(|&node| {
                    self.inspect_node(node, |widget| widget.test_id() == Some("action-palette"))
                        .unwrap_or(false)
                })
                .collect();
        }
        if self.history_layout.is_some() {
            self.presentation(|root, node| {
                order = crate::focus_navigation::order(node, root, &self.taffy).0;
            });
            return order;
        }
        if self.history.is_some() || crate::history_mode::embedded::has_surfaces(self.root.as_ref())
        {
            let root = self
                .history
                .as_ref()
                .and_then(|history| history.review_layout_node())
                .unwrap_or(self.child_node);
            let mut pending = vec![root];
            while let Some(node) = pending.pop() {
                if crate::history_mode::embedded::allows_focus(
                    self.root.as_ref(),
                    node,
                    &self.taffy,
                ) && self.history.as_ref().is_none_or(|history| {
                    !history.captures_input() || history.owns_node(node, &self.taffy)
                }) && self
                    .inspect_node(node, |widget| widget.is_focusable() && widget.is_enabled())
                    .unwrap_or(false)
                {
                    order.push(node);
                }
                pending.extend(
                    self.taffy
                        .children(node)
                        .unwrap_or_default()
                        .into_iter()
                        .rev(),
                );
            }
        } else {
            order =
                crate::focus_navigation::order(self.child_node, self.root.as_ref(), &self.taffy).0;
        }
        order
    }

    fn update_focus_trap(
        &mut self,
        before_layout: Option<crate::widget::layout::CompositionFocus>,
    ) {
        if self.history.is_some() || crate::history_mode::embedded::has_surfaces(self.root.as_ref())
        {
            // A generic modal can hide retained editors with framework history.
            // Returning to those editors must restore the exact prior owner;
            // a resource/workspace switch cannot match an absent owner.
            if self.focus_trap_active
                && let Some(saved) = self.focus_before_trap.take()
            {
                let order = self.focus_order();
                if let Some(target) =
                    unsafe { saved.restore(&self.node_map, &self.bindings, Some(&order)) }
                {
                    self.set_focused(Some(target));
                }
            }
            self.focus_trap_active = self
                .history
                .as_ref()
                .is_some_and(|history| history.review_is_open());
            self.focus_before_trap = None;
            return;
        }
        let (mut order, trapped) =
            crate::focus_navigation::order(self.child_node, self.root.as_ref(), &self.taffy);
        if !self.focus_trap_active && trapped {
            // Capture a stable semantic/owner identity, never carry a stale Taffy id
            // through the portable host's complete layout rebuilds.
            self.focus_before_trap = before_layout.or_else(|| unsafe {
                crate::widget::layout::CompositionFocus::capture(
                    self.focused,
                    &self.node_map,
                    &self.bindings,
                )
            });
        }
        let mut before = if self.focus_trap_active && !trapped {
            // Bound component delegates can be absent from Widget::children's
            // generic order. Eligibility comes from this newly admitted layout,
            // with the same embedded-history restrictions as mapped traversal.
            let mut allowed = Vec::new();
            let mut pending = vec![self.child_node];
            while let Some(node) = pending.pop() {
                if crate::history_mode::embedded::allows_focus(
                    self.root.as_ref(),
                    node,
                    &self.taffy,
                ) && self
                    .inspect_node(node, |widget| widget.is_focusable() && widget.is_enabled())
                    .unwrap_or(false)
                {
                    allowed.push(node);
                }
                pending.extend(self.taffy.children(node).unwrap_or_default());
            }
            let restored = self.focus_before_trap.take().and_then(|saved| unsafe {
                saved.restore(&self.node_map, &self.bindings, Some(&allowed))
            });
            if let Some(target) = restored
                && !order.contains(&target)
            {
                order.push(target);
            }
            restored
        } else {
            None
        };
        if let Some(next) = crate::focus_navigation::reconcile_trap(
            &mut self.focus_trap_active,
            &mut before,
            self.focused,
            &order,
            trapped,
        ) {
            self.set_focused(next);
        }
    }

    /// Reject external activation while a modal owns input. Search acceptance
    /// calls this only after its overlay has closed and its intent was taken.
    pub fn check_app_activation(&self) -> Result<(), String> {
        if self.root.modal_capture().is_some()
            || self.focus_trap_active
            || self.caller_palette.palette.is_some()
            || self.caller_palette.pending.is_some()
            || self.global_search_query().is_some()
            || self
                .history
                .as_ref()
                .is_some_and(|history| history.captures_input())
            || crate::history_mode::embedded::captures_input(self.root.as_ref())
        {
            Err("Finish the current dialog or command first".into())
        } else {
            Ok(())
        }
    }

    /// Remember a stable component/control identity, never a layout NodeId.
    pub fn remember_app_focus(&mut self) {
        if let Some(binding) = self
            .focused
            .and_then(|node| self.bindings.binding_for(node))
        {
            // SAFETY: these maps belong to this exclusively borrowed runner.
            if let Some(focus) = unsafe {
                crate::widget::layout::CompositionFocus::capture(
                    self.focused,
                    &self.node_map,
                    &self.bindings,
                )
            } {
                self.saved_instance_focus.insert(binding.instance_id, focus);
            }
        }
    }

    fn app_focus_eligible(&self, node: NodeId) -> bool {
        self.taffy
            .style(node)
            .is_ok_and(|style| style.display != taffy::Display::None)
            && self
                .inspect_node(node, |widget| {
                    let rect = widget.get_rect();
                    widget.is_focusable()
                        && widget.is_enabled()
                        && rect.width() > 0.0
                        && rect.height() > 0.0
                        && widget
                            .accessibility()
                            .is_none_or(|access| !access.state.hidden && !access.state.disabled)
                })
                .unwrap_or(false)
    }

    fn app_default_focus(&self, order: &[NodeId]) -> Option<NodeId> {
        order
            .iter()
            .copied()
            .find(|&node| {
                self.inspect_node(node, |widget| widget.is_default_keyboard_focus())
                    .unwrap_or(false)
            })
            .or_else(|| order.first().copied())
    }

    fn blur_inactive_app_components(&mut self, active: crate::application::InstanceId) {
        let outgoing: Vec<_> = self
            .node_map
            .keys()
            .copied()
            .filter(|&node| {
                self.bindings
                    .binding_for(node)
                    .is_some_and(|binding| binding.instance_id != active)
            })
            .collect();
        for node in outgoing {
            let _ = self.mutate_node(node, |widget| widget.set_focus(false));
        }
    }

    /// Resolve focus only in the selected, live app instance. A missing control
    /// is an authoritative no-owner result; layout cannot revive another app.
    pub fn prepare_app_activation(&mut self, instance: crate::application::InstanceId) {
        self.remember_app_focus();
        // Clear an unfinished preedit in its original bound owner, without commit.
        self.send(&WindowEvent::Ime(winit::event::Ime::Preedit(
            String::new(),
            None,
        )));
        self.modifiers = winit::event::Modifiers::default();
        self.active_input_instance = Some(instance);
        self.set_focused(None);
    }

    pub fn activate_app_focus(&mut self, instance: crate::application::InstanceId) {
        let order: Vec<_> = self
            .focus_order()
            .into_iter()
            .filter(|&node| {
                self.bindings
                    .binding_for(node)
                    .is_some_and(|binding| binding.instance_id == instance)
                    && self.app_focus_eligible(node)
            })
            .collect();
        self.active_input_instance = Some(instance);
        let saved = self.saved_instance_focus.remove(&instance);
        // SAFETY: restore resolves the captured stable identity in this live map.
        let target = saved
            .as_ref()
            .and_then(|saved| unsafe {
                saved.restore(&self.node_map, &self.bindings, Some(&order))
            })
            .or_else(|| self.app_default_focus(&order));
        if target.is_none() {
            let focused: Vec<_> = self
                .node_map
                .keys()
                .copied()
                .filter(|&node| {
                    self.inspect_node(node, |widget| widget.is_focused())
                        .unwrap_or(false)
                })
                .collect();
            for node in focused {
                let _ = self.mutate_node(node, |widget| widget.set_focus(false));
            }
        }
        self.set_focused(target);
        self.input_sequence
            .set(self.input_sequence.get().wrapping_add(1));
        self.remember_app_focus();
        self.relayout();
        // Reconciliation can restore an opaque component's app-local focus
        // flags after the runner has moved its own focus. Enforce the selected
        // instance after that final layout so old controls cannot stay blue or
        // claim accessibility focus.
        self.blur_inactive_app_components(instance);
    }

    /// No-control shortcut routing stays inside the selected app and its
    /// owning chrome. Never dispatch a text event or fall back to the root.
    fn dispatch_scoped_shortcut(&mut self, event: &WindowEvent) {
        let Some(instance) = self.active_input_instance else {
            return;
        };
        let WindowEvent::KeyboardInput { event: key, .. } = event else {
            return;
        };
        let modifiers = self.modifiers.state();
        let command = modifiers.control_key() || modifiers.super_key() || modifiers.alt_key();
        let nontext = matches!(&key.logical_key, Key::Named(named) if !matches!(named, NamedKey::Space | NamedKey::Enter));
        if !command && !nontext {
            return;
        }
        let scopes: Vec<_> = self
            .node_map
            .keys()
            .copied()
            .filter(|&node| {
                self.bindings
                    .binding_for(node)
                    .is_none_or(|binding| binding.instance_id == instance)
                    && self
                        .inspect_node(node, |widget| {
                            widget.keyboard_input_scope() == Some(instance)
                                && widget.is_enabled()
                                && widget.get_rect().width() > 0.0
                                && widget.get_rect().height() > 0.0
                                && widget.accessibility().is_none_or(|access| {
                                    !access.state.hidden && !access.state.disabled
                                })
                        })
                        .unwrap_or(false)
            })
            .collect();
        // Prefer the owning chrome over its component child. Ambiguous sibling
        // declarations fail closed instead of selecting by map iteration order.
        let roots: Vec<_> = scopes
            .iter()
            .copied()
            .filter(|&node| {
                !scopes
                    .iter()
                    .any(|&other| other != node && self.bindings.is_descendant(node, other))
            })
            .collect();
        let [target] = roots.as_slice() else {
            return;
        };
        let target = *target;
        let mut handled = false;
        let mut focus = None;
        // SAFETY: the exclusive runner owns the admitted tree and maps. This
        // invokes only one live scoped subtree using its document provenance.
        let _ = unsafe {
            crate::bound_dispatch::invoke_bound_node(
                &self.bindings,
                &self.node_map,
                target,
                &mut self.state.doc,
                &self.state.root,
                |widget, doc, path| {
                    widget.handle_event(
                        event,
                        &mut EventCtx {
                            scale_factor: self.input_scale,
                            modifiers: &self.modifiers,
                            state: doc,
                            path,
                            handled: &mut handled,
                            focus_target: &mut focus,
                            node_id: target,
                            taffy: &self.taffy,
                        },
                    )
                },
            )
        };
        if let Some(focus) = focus.filter(|&node| {
            self.bindings.is_descendant(node, target) && self.app_focus_eligible(node)
        }) {
            self.set_focused(Some(focus));
        }
        self.host_tick();
        self.relayout();
    }

    /// Mount-local token for asynchronous input. Includes changes that do not
    /// pass through explicit activation (pointer, semantics, modal, removal).
    pub fn input_ownership_sequence(&self) -> u64 {
        let owner = if self.global_search_query().is_some() {
            Some("system-search".into())
        } else if self.caller_palette.palette.is_some() {
            Some("caller-palette".into())
        } else {
            self.focused.and_then(|node| {
                self.inspect_node(node, |widget| {
                    fn focused_semantics(node: &crate::a11y::AccessNode, keys: &mut Vec<String>) {
                        if node.state.focused {
                            if let Some(key) = node
                                .action_target
                                .as_ref()
                                .or(node.semantic_key.as_ref())
                                .or(node.name.as_ref())
                            {
                                keys.push(key.clone());
                            }
                        }
                        for child in &node.children {
                            focused_semantics(child, keys);
                        }
                    }
                    let semantics = widget.accessibility();
                    let control = widget
                        .test_id()
                        .map(str::to_owned)
                        .or_else(|| semantics.as_ref().and_then(|a| a.semantic_key.clone()))
                        .unwrap_or_else(|| widget.name().to_owned());
                    let mut focused = Vec::new();
                    if let Some(semantics) = semantics {
                        focused_semantics(&semantics, &mut focused);
                    }
                    let control = format!("{control}:{focused:?}");
                    self.bindings.binding_for(node).map_or_else(
                        || control.clone(),
                        |binding| format!("{}:{control}", binding.lifetime_id),
                    )
                })
                .ok()
            })
        };
        if *self.observed_input_owner.borrow() != owner {
            *self.observed_input_owner.borrow_mut() = owner;
            self.input_sequence
                .set(self.input_sequence.get().wrapping_add(1));
        }
        self.input_sequence.get()
    }

    /// The taffy node holding keyboard focus, if any.
    pub fn focused(&self) -> Option<NodeId> {
        self.drain_component_wakes();
        self.focused
    }

    /// Mirrors [`crate::app::App`]: an explicitly opted-in focused widget
    /// receives Tab before the host considers focus traversal.
    fn focused_widget_wants_tab_input(&self) -> bool {
        self.focused.is_some_and(|node| {
            self.inspect_node(node, |widget| widget.wants_tab_input())
                .unwrap_or(false)
        })
    }

    /// Move keyboard focus to the widget at `node`, toggling `set_focus` on
    /// the outgoing/incoming widgets exactly as
    /// [`crate::app::App::set_focused_node`] does.
    fn set_focused(&mut self, node: Option<NodeId>) {
        if self.focused != node {
            self.remember_app_focus();
            self.input_sequence
                .set(self.input_sequence.get().wrapping_add(1));
        }
        if self.active_input_instance.is_some()
            && let Some(binding) = node.and_then(|node| self.bindings.binding_for(node))
        {
            self.active_input_instance = Some(binding.instance_id);
        }
        if unsafe {
            crate::history_mode::embedded::move_focus(
                self.root.as_mut(),
                node,
                self.focused,
                &self.node_map,
                &self.bindings,
                &self.taffy,
            )
        } {
            self.focused = None;
        }
        let resumed_live = self.history.as_ref().is_some_and(|history| {
            !history.captures_input()
                && history.has_saved_focus()
                && node.is_some_and(|node| !history.owns_node(node, &self.taffy))
        });
        if resumed_live {
            // Tabbing through unopened chrome is temporary suspension. Keep
            // the chosen editor's current caret; do not restore old view state.
            if let Some(history) = &mut self.history {
                history.discard_saved_focus();
            }
            let outgoing: Vec<_> = self
                .node_map
                .keys()
                .copied()
                .filter(|candidate| {
                    Some(*candidate) != node
                        && self
                            .inspect_node(*candidate, |widget| widget.is_focused())
                            .unwrap_or(false)
                })
                .collect();
            for outgoing in outgoing {
                let _ = self.mutate_node(outgoing, |widget| widget.set_focus(false));
            }
        }
        if self.focused == node {
            return;
        }
        if let Some(history) = &mut self.history
            && node.is_some_and(|node| history.owns_node(node, &self.taffy))
            && self
                .focused
                .is_some_and(|node| !history.owns_node(node, &self.taffy))
        {
            // Suspend the runner's live focus without synthesizing blur. The
            // unfinished live editor and its IME/local state stay in place.
            unsafe {
                history.preserve_focus(self.focused, &self.node_map, &self.bindings, &self.taffy);
            }
            self.focused = None;
        }
        self.root.focus_requested(node, &self.taffy);
        if let Some(history) = &mut self.history
            && node.is_some_and(|node| history.owns_node(node, &self.taffy))
            && self
                .focused
                .is_some_and(|node| !history.owns_node(node, &self.taffy))
        {
            // Suspend the runner's live focus without synthesizing blur. The
            // unfinished live editor and its IME/local state stay in place.
            unsafe {
                history.preserve_focus(self.focused, &self.node_map, &self.bindings, &self.taffy);
            }
            self.focused = None;
        }
        if let Some(target) = node {
            // Root dispatch visits every live app. An exact owner must also
            // clear competing mount/restoration flags, or a preceding editor
            // can consume its first key. Keep the selected compound editor's
            // ancestors/children, and never blur a history-suspended caret.
            let competing: Vec<_> = self
                .node_map
                .keys()
                .copied()
                .filter(|&candidate| {
                    candidate != target
                        && !self.bindings.is_descendant(candidate, target)
                        && !self.bindings.is_descendant(target, candidate)
                        && crate::history_mode::embedded::allows_focus(
                            self.root.as_ref(),
                            candidate,
                            &self.taffy,
                        )
                        && self.history.as_ref().is_none_or(|history| {
                            !history.has_saved_focus() || history.owns_node(candidate, &self.taffy)
                        })
                        && self
                            .inspect_node(candidate, |widget| {
                                widget.is_focusable() && widget.is_focused()
                            })
                            .unwrap_or(false)
                })
                .collect();
            for competing in competing {
                let _ = self.mutate_node(competing, |widget| widget.set_focus(false));
            }
        }
        if self.focused == node {
            return;
        }
        if let Some(old) = self.focused {
            let _ = self.mutate_node(old, |widget| widget.set_focus(false));
        }
        if let Some(new) = node {
            let _ = self.mutate_node(new, |widget| widget.set_focus(true));
        }
        self.focused = node;
    }

    /// Advance keyboard focus like Tab (or Shift+Tab when `back`), cycling the
    /// focus order. Returns the newly-focused node.
    pub fn tab(&mut self, back: bool) -> Option<NodeId> {
        self.pump_component_wakes();
        let order = self.focus_order();
        if order.is_empty() {
            return None;
        }
        let next = crate::focus_navigation::tab_index(&order, self.focused, back)?;
        // `next` is constructed above from `0..order.len()` in every branch.
        let target = *order.get(next)?;
        let framework =
            crate::history_mode::embedded::owns_node(self.root.as_ref(), target, &self.taffy)
                || self
                    .history
                    .as_ref()
                    .is_some_and(|history| history.owns_node(target, &self.taffy));
        let _framework_frame = FrameworkInputFrame::new(&self.framework_input, framework);
        self.set_focused(Some(target));
        self.relayout();
        Some(target)
    }

    /// The accessibility node of every keyboard-focusable control, in focus
    /// order — one entry per Tab stop. Non-mutating (it does not move focus),
    /// so a test can assert the expected controls exist with the right roles,
    /// names, and states in a single pass.
    pub fn focus_access_nodes(&self) -> Vec<AccessNode> {
        self.drain_component_wakes();
        self.focus_order()
            .into_iter()
            .filter_map(|id| {
                self.inspect_node(id, |widget| widget.accessibility())
                    .ok()
                    .flatten()
            })
            .collect()
    }

    /// The local accessibility node of the currently-focused widget, if any —
    /// what a screen reader would announce when Tab lands here.
    pub fn focused_access_node(&self) -> Option<AccessNode> {
        self.drain_component_wakes();
        let id = self.focused?;
        self.inspect_node(id, |widget| widget.accessibility())
            .ok()
            .flatten()
    }

    /// The full accessibility tree the AccessKit bridge exports for the
    /// current frame — the exact `(NodeId, Node)` list that reaches AT-SPI2 /
    /// Orca and the built-in screen reader, with the driver's focus marked.
    pub fn a11y_nodes(&self) -> Vec<(accesskit::NodeId, accesskit::Node)> {
        if let Some(update) = self.system_search_a11y() {
            return update.nodes;
        }
        self.drain_component_wakes();
        if self.history_layout.is_some() {
            return self.presentation(|root, _| {
                let mut roots: Vec<&dyn Widget> = vec![root];
                if let Some(palette) = self
                    .action_palette
                    .as_deref()
                    .filter(|palette| palette.is_presented())
                {
                    roots.push(palette);
                }
                build_presentation_with_resources_bound(
                    &roots,
                    &self.node_map,
                    self.focused,
                    1.0,
                    &self.history_resources_snapshot().unwrap_or_default(),
                )
                .nodes
            });
        }
        let exported = self.bindings.exported_nodes(&self.node_map);
        if self.history.is_none()
            && !self
                .action_palette
                .as_deref()
                .is_some_and(|p| p.is_presented())
        {
            return build_tree_update_bound(
                self.root.as_ref(),
                &exported.nodes,
                exported.focus(self.focused),
                1.0,
            )
            .nodes;
        }
        build_presentation_tree_bound(
            &self.presentation_roots(),
            &exported.nodes,
            exported.focus(self.focused),
            1.0,
        )
        .nodes
    }

    /// The id of the exported accessibility node whose accessible name is
    /// `name` — the handle a headless test uses to address an element that has
    /// no widget of its own (a table row, a clip on a timeline track).
    ///
    /// Returns `None` when nothing carries that name this frame, which is a
    /// meaningful failure: an element a test cannot name is one a screen-reader
    /// user cannot find either.
    pub fn a11y_node_named(&self, name: &str) -> Option<accesskit::NodeId> {
        self.drain_component_wakes();
        self.a11y_nodes()
            .into_iter()
            .find(|(_, n)| n.label() == Some(name))
            .map(|(id, _)| id)
    }

    /// The accessibility node the exported tree currently marks focused — what
    /// a screen reader would be sitting on. For a canvas widget this is the
    /// element inside it, not the widget, so a test can assert where the user
    /// actually is.
    pub fn a11y_focus(&self) -> Option<(accesskit::NodeId, accesskit::Node)> {
        if let Some(update) = self.system_search_a11y() {
            return update.nodes.into_iter().find(|(id, _)| *id == update.focus);
        }
        self.drain_component_wakes();
        let update = if self.history_layout.is_some() {
            self.presentation(|root, _| {
                let mut roots: Vec<&dyn Widget> = vec![root];
                if let Some(palette) = self
                    .action_palette
                    .as_deref()
                    .filter(|palette| palette.is_presented())
                {
                    roots.push(palette);
                }
                build_presentation_with_resources_bound(
                    &roots,
                    &self.node_map,
                    self.focused,
                    1.0,
                    &self.history_resources_snapshot().unwrap_or_default(),
                )
            })
        } else {
            let exported = self.bindings.exported_nodes(&self.node_map);
            build_presentation_tree_bound(
                &self.presentation_roots(),
                &exported.nodes,
                exported.focus(self.focused),
                1.0,
            )
        };
        let focus = update.focus;
        update.nodes.into_iter().find(|(id, _)| *id == focus)
    }

    /// Perform an accessibility [`Action`](crate::a11y::Action) on an exported
    /// node, exactly as an assistive technology would.
    ///
    /// This is the headless counterpart of
    /// [`App::dispatch_a11y_action`](crate::app::App::dispatch_a11y_action) and
    /// deliberately takes the same route: resolve the id against the live tree,
    /// then hand the owning widget the action *plus the key path naming the
    /// element*, so a synthetic node is operated rather than the widget that
    /// painted it. Without this a canvas app could be described in a headless
    /// test but not driven by one, which caps how well it can be verified at
    /// all.
    ///
    /// # Errors
    ///
    /// Returns an error when the id names nothing in the current frame — a
    /// stale reference is reported, never quietly retargeted at whatever now
    /// occupies the slot — or when it addresses the inoperable window root.
    pub fn perform_a11y_action(
        &mut self,
        target: accesskit::NodeId,
        action: &crate::a11y::Action,
    ) -> Result<(), String> {
        use crate::accesskit_bridge::A11yTarget;
        self.pump_component_wakes();
        if self.global_search_query().is_some() {
            let handled = self.perform_system_search_a11y(target, action);
            self.finish_system_search();
            self.relayout();
            return if handled {
                Ok(())
            } else {
                Err("system search owns input".into())
            };
        }
        if let Some(result) = self.history_a11y_resource(target, action) {
            return result;
        }
        let resolved = if self.history_layout.is_some() {
            self.try_presentation(|root, _| {
                resolve_node_id_bound(root, &self.node_map, target).or_else(|| {
                    self.action_palette
                        .as_deref()
                        .filter(|palette| palette.is_presented())
                        .and_then(|palette| resolve_node_id_bound(palette, &self.node_map, target))
                })
            })?
        } else {
            let exported = self.bindings.exported_nodes(&self.node_map);
            self.presentation_roots()
                .into_iter()
                .find_map(|root| exported.resolve(root, target))
        }
        .ok_or_else(|| format!("no accessibility node {target:?} in the current frame"))?;
        let (taffy_id, key_path, ordinal_path, semantic_target) = match resolved {
            A11yTarget::Root => return Err("the window root cannot be operated".into()),
            A11yTarget::Widget(id) => (id, Vec::new(), Vec::new(), None),
            // Keys here: the driver dispatches through `perform_action_at`,
            // which identifies a synthetic child by identity.
            A11yTarget::Synthetic {
                owner,
                key_path,
                ordinal_path,
                semantic_target,
            } => (owner, key_path, ordinal_path, semantic_target),
        };
        if let Some(target) = semantic_target
            .as_deref()
            .filter(|t| t.starts_with("ordinary-confirmation:"))
        {
            return self.perform_palette_action(target, action);
        }
        if self.is_history_node(taffy_id) {
            return self.history_named_action(taffy_id, semantic_target.as_deref(), action);
        }
        let framework =
            crate::history_mode::embedded::owns_node(self.root.as_ref(), taffy_id, &self.taffy)
                || self
                    .history
                    .as_ref()
                    .is_some_and(|history| history.owns_node(taffy_id, &self.taffy));
        let _framework_frame = FrameworkInputFrame::new(
            &self.framework_input,
            framework || self.bindings.owns_document(taffy_id),
        );
        if framework {
            self.set_focused(Some(taffy_id));
        }
        // Operating an element inside a widget also moves the keyboard to that
        // widget, matching the App: the widget moves its own cursor, the host
        // moves the focus. Every verb, not just Focus — an activation must
        // leave the reader on what it operated.
        if !key_path.is_empty() {
            self.set_focused(Some(taffy_id));
        }
        let handled = self
            .dispatch_semantic_node(taffy_id, action, |widget, doc, root| {
                if let Some(target) = semantic_target.as_deref() {
                    widget.perform_semantic_action(target, action, doc, root)
                } else {
                    widget.perform_action_at(action, &key_path, doc, root)
                }
            })
            .map_err(|error| format!("component dispatch refused: {error:?}"))?;
        if handled
            && (matches!(action, crate::a11y::Action::Focus)
                || (matches!(action, crate::a11y::Action::SetSelection { .. })
                    && self
                        .inspect_node(taffy_id, |widget| widget.is_focused())
                        .unwrap_or(false)))
        {
            self.set_focused(Some(taffy_id));
        }
        if !handled {
            match action {
                crate::a11y::Action::Focus => {
                    if !ordinal_path.is_empty() {
                        let _ = self.mutate_node(taffy_id, |widget| {
                            widget.focus_synthetic_accessibility(&ordinal_path)
                        });
                    }
                    self.set_focused(Some(taffy_id));
                }
                // Fall back the way the App does: a click at the widget's
                // centre. Widget-granular by nature — it cannot reach inside a
                // canvas, which is why `perform_action_at` exists.
                crate::a11y::Action::Activate | crate::a11y::Action::ToggleCheck => {
                    let rect = (!ordinal_path.is_empty())
                        .then(|| {
                            self.inspect_node(taffy_id, |widget| {
                                widget.synthetic_accessibility_rect(&ordinal_path)
                            })
                            .ok()
                            .flatten()
                        })
                        .flatten()
                        .unwrap_or_else(|| {
                            self.inspect_node(taffy_id, |widget| widget.get_rect())
                                .unwrap_or(Rect::ZERO)
                        });
                    let c = rect.center();
                    self.click_at(c.x, c.y);
                }
                _ => {}
            }
        }
        // The widget may have mutated the document from `perform_action_at` —
        // dispatching an action, or asking for a commit — and this route does
        // not go through `send`. Close the tick here too, or an app driven the
        // way an assistive technology drives it leaves no history behind.
        if framework {
            self.poll_control_invocations();
        } else {
            self.host_tick();
        }
        self.relayout();
        drop(_framework_frame);
        if self.service_pending_history_commands() {
            let _framework_frame = FrameworkInputFrame::new(&self.framework_input, true);
            self.relayout();
        }
        Ok(())
    }

    /// Tab through the entire focus order from the top, returning the
    /// accessible `(role, name)` a screen reader would announce at each stop.
    /// One assertion proves both that every control is reachable by keyboard
    /// and that each announces a sensible role + label.
    pub fn tab_through(&mut self) -> Vec<(crate::a11y::Role, Option<String>)> {
        self.pump_component_wakes();
        self.set_focused(None);
        let count = self.focus_order().len();
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            self.tab(false);
            if let Some(node) = self.focused_access_node() {
                out.push((node.role, node.name));
            }
        }
        out
    }

    /// Every widget the driver can address, by `test_id` or path, with the
    /// rect it currently occupies.
    fn presentation_roots(&self) -> Vec<&dyn Widget> {
        let mut roots = Vec::new();
        if let Some(search) = self.system_search_presentation() {
            roots.push(search);
            return roots;
        }
        if let Some(history) = &self.history {
            roots.push(history.as_ref() as &dyn Widget);
        }
        if let Some(palette) = self.action_palette.as_deref().filter(|p| p.is_presented()) {
            roots.push(palette as &dyn Widget);
            return roots;
        }
        if self
            .history
            .as_ref()
            .is_none_or(|history| history.live_interactive())
        {
            roots.push(self.root.as_ref());
        }
        roots
    }

    pub fn targets(&self) -> HashMap<String, Rect> {
        self.drain_component_wakes();
        let mut out = HashMap::new();
        let mut counts = HashMap::new();
        if let Some(search) = self.system_search_presentation() {
            collect_targets(search, "", &mut counts, &mut out);
            return out;
        }
        if self.history_layout.is_some() {
            self.presentation(|root, _| {
                collect_targets(root, "", &mut counts, &mut out);
                if let Some(palette) = self
                    .action_palette
                    .as_deref()
                    .filter(|palette| palette.is_presented())
                {
                    collect_targets(palette, "", &mut counts, &mut out);
                }
            });
        } else {
            for root in self.presentation_roots() {
                collect_targets(root, "", &mut counts, &mut out);
            }
        }
        out
    }

    /// Perform one semantic action against a stable widget id or fallback
    /// path, through the same routing contract as live assistive technology.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing target or an action that neither the
    /// widget nor the app-level generic fallback supports.
    pub fn perform_named(
        &mut self,
        name: &str,
        action: &crate::a11y::Action,
    ) -> Result<(), String> {
        self.pump_component_wakes();
        self.perform_semantic_named(name, None, action)
    }

    /// Perform an action against a synthetic semantic child owned by a named
    /// retained widget. The child target is the stable opaque value exposed
    /// as `action_target` in inspection output.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing owner or unsupported child action.
    pub fn perform_semantic_named(
        &mut self,
        name: &str,
        semantic_target: Option<&str>,
        action: &crate::a11y::Action,
    ) -> Result<(), String> {
        use crate::a11y::Action as SemanticAction;
        use automerge::transaction::Transactable;
        self.pump_component_wakes();
        if name == "action-palette" {
            let target = semantic_target
                .ok_or_else(|| "palette action needs a published semantic target".to_owned())?;
            return self.perform_palette_action(target, action);
        }

        if self.global_search_query().is_some() {
            let handled = self.perform_system_search_semantic(name, semantic_target, action);
            self.finish_system_search();
            self.relayout();
            return if handled {
                Ok(())
            } else {
                Err("system search owns input".into())
            };
        }
        if self.caller_palette.palette.is_some() {
            let handled = self.perform_caller_palette_semantic(name, semantic_target, action);
            self.finish_caller_palette();
            self.relayout();
            return if handled {
                Ok(())
            } else {
                Err("palette owns input".into())
            };
        }
        self.try_presentation(|_, _| ())?;
        if let Some(target) = semantic_target {
            // The generic Focus fallback must never resurrect a removed or
            // forged synthetic child, nor bypass its published edit guards.
            let access = self.access_node_named(name, Some(target))?;
            if access.state.disabled
                || (access.state.read_only
                    && matches!(
                        action,
                        SemanticAction::SetValue(_) | SemanticAction::SetSelection { .. }
                    ))
            {
                return Err("semantic target is disabled or read-only".into());
            }
        }
        let targets = self.named_target_nodes();
        let node = targets
            .get(name)
            .copied()
            .ok_or_else(|| format!("no addressable widget named {name:?}"))?;
        if self.is_history_node(node) {
            return self.history_named_action(node, semantic_target, action);
        }
        if crate::history_mode::embedded::owns_node(self.root.as_ref(), node, &self.taffy)
            || self
                .history
                .as_ref()
                .is_some_and(|history| history.owns_node(node, &self.taffy))
        {
            let _framework_frame = FrameworkInputFrame::new(&self.framework_input, true);
            if matches!(action, SemanticAction::Focus | SemanticAction::Activate) {
                self.set_focused(Some(node));
            }
            let handled = self
                .dispatch_node(node, |widget, doc, root| {
                    if let Some(target) = semantic_target {
                        widget.perform_semantic_action(target, action, doc, root)
                    } else {
                        widget.perform_action(action, doc, root)
                    }
                })
                .map_err(|error| format!("History input refused: {error:?}"))?;
            if !handled && !matches!(action, SemanticAction::Focus) {
                return Err("History control does not support this action".into());
            }
            self.poll_control_invocations();
            self.relayout();
            return Ok(());
        }
        // A requested semantic action is one deterministic interaction, so a
        // document mutation it performs is one deterministic undo boundary.
        // Capture before dispatch: `get_heads` also flushes any earlier work,
        // keeping that work out of this action's bracket.
        let owns_document = self.bindings.owns_document(node);
        let _owned_frame = FrameworkInputFrame::new(&self.framework_input, owns_document);
        let before = if owns_document
            || matches!(
                action,
                SemanticAction::Focus | SemanticAction::SetSelection { .. }
            ) {
            self.state.doc.base_heads()
        } else {
            self.state.doc.get_heads()
        };
        let handled = self
            .dispatch_semantic_node(node, action, |widget, doc, root| {
                if let Some(target) = semantic_target {
                    widget.perform_semantic_action(target, action, doc, root)
                } else {
                    widget.perform_action(action, doc, root)
                }
            })
            .map_err(|error| format!("component dispatch refused: {error:?}"))?;
        if handled {
            if matches!(action, SemanticAction::Focus)
                || (matches!(action, SemanticAction::SetSelection { .. })
                    && self
                        .inspect_node(node, |widget| widget.is_focused())
                        .unwrap_or(false))
            {
                self.set_focused(Some(node));
            }
            if !owns_document && self.state.doc.get_heads() != before {
                self.state.record_committed_action(before, None, None);
                crate::undo::on_commit(self.root.as_mut());
            } else if !owns_document && crate::undo::needs_commit(self.root.as_ref()) {
                self.state.commit_action(None, None);
                crate::undo::on_commit(self.root.as_mut());
            }
            // Action-bound controls enqueue an intent instead of mutating the
            // document inside `perform_action`; drain it before presenting
            // the next frame, exactly as the live host does.
            self.host_tick();
            self.relayout();
            return Ok(());
        }

        match action {
            SemanticAction::Focus => {
                self.set_focused(Some(node));
                self.relayout();
                Ok(())
            }
            SemanticAction::Activate | SemanticAction::ToggleCheck if semantic_target.is_none() => {
                let rect = self
                    .inspect_node(node, |widget| widget.get_rect())
                    .map_err(|error| format!("component inspection refused: {error:?}"))?;
                if rect.width() <= 0.0 || rect.height() <= 0.0 {
                    return Err(format!("widget {name:?} has an empty rect: {rect:?}"));
                }
                let center = rect.center();
                self.click_at(center.x, center.y);
                Ok(())
            }
            _ => Err(format!(
                "widget {name:?} does not support semantic action {action:?}"
            )),
        }
    }

    /// Assert the live accessibility contract of a retained widget or one of
    /// its synthetic model-addressed descendants.
    ///
    /// # Errors
    ///
    /// Returns every mismatched field in one diagnostic, or a precise missing
    /// target/semantics error. An expectation built with
    /// [`SemanticExpectation::within`] drives live-equivalent frames until it
    /// matches or the explicit bound expires.
    pub fn expect(&mut self, expected: &SemanticExpectation) -> Result<(), String> {
        self.pump_component_wakes();
        let Some(timeout) = expected.timeout else {
            return self.expect_now(expected);
        };
        let mut last_error = None;
        if self.wait_until(timeout, |driver| match driver.expect_now(expected) {
            Ok(()) => true,
            Err(error) => {
                last_error = Some(error);
                false
            }
        }) {
            Ok(())
        } else {
            Err(format!(
                "timed out after {} ms: {}",
                timeout.as_millis(),
                last_error.unwrap_or_else(|| "expectation did not match".into())
            ))
        }
    }

    fn expect_now(&self, expected: &SemanticExpectation) -> Result<(), String> {
        let node = self.access_node_named(&expected.target, expected.semantic_target.as_deref())?;
        let mut mismatches = Vec::new();
        let actual_role = role_name(node.role);
        if let Some(role) = expected.role.as_deref()
            && role != actual_role
        {
            mismatches.push(format!("role expected {role:?}, got {actual_role:?}"));
        }
        compare_text(
            &mut mismatches,
            "name",
            expected.name.as_deref(),
            node.name.as_deref(),
        );
        compare_contains(
            &mut mismatches,
            "name",
            expected.name_contains.as_deref(),
            node.name.as_deref(),
        );
        compare_text(
            &mut mismatches,
            "value",
            expected.value.as_deref(),
            node.value.as_deref(),
        );
        compare_contains(
            &mut mismatches,
            "value",
            expected.value_contains.as_deref(),
            node.value.as_deref(),
        );
        compare_bool(
            &mut mismatches,
            "focused",
            expected.focused,
            node.state.focused,
        );
        compare_bool(
            &mut mismatches,
            "selected",
            expected.selected,
            node.state.selected,
        );
        compare_optional_bool(
            &mut mismatches,
            "checked",
            expected.checked,
            node.state.checked,
        );
        compare_optional_bool(
            &mut mismatches,
            "expanded",
            expected.expanded,
            node.state.expanded,
        );
        compare_bool(
            &mut mismatches,
            "enabled",
            expected.enabled,
            !node.state.disabled,
        );
        compare_bool(&mut mismatches, "busy", expected.busy, node.state.busy);
        compare_bool(
            &mut mismatches,
            "invalid",
            expected.invalid,
            node.state.invalid,
        );
        if mismatches.is_empty() {
            Ok(())
        } else {
            let semantic = expected
                .semantic_target
                .as_deref()
                .map_or_else(String::new, |target| format!(" semantic target {target:?}"));
            Err(format!(
                "expectation failed for widget {:?}{semantic}: {}",
                expected.target,
                mismatches.join("; ")
            ))
        }
    }

    // Address/name is bounded correlation, not universal Rust object identity:
    // a non-retained hidden child with its wrapper's exact name/address cannot
    // be distinguished. Refuse ambiguity observed in either registered or
    // retained sets; all resulting nodes still pass normal bound admission.
    fn named_target_nodes(&self) -> HashMap<String, NodeId> {
        let mut registered = HashMap::with_capacity(self.node_map.len());
        for &node in self.node_map.keys() {
            // Read correlation metadata only through the existing admitted
            // inspection. A data address alone also names inline first fields;
            // a dyn vtable is not a stable type identity across codegen sites.
            let Ok(key) = self.inspect_node(node, |widget| {
                (
                    std::ptr::from_ref(widget).cast::<()>() as usize,
                    widget.name().to_owned(),
                )
            }) else {
                continue;
            };
            registered
                .entry(key)
                .and_modify(|entry| *entry = None)
                .or_insert(Some(node));
        }
        let mut retained = Vec::new();
        if self.history_layout.is_some() {
            self.presentation(|root, _| {
                let mut counts = HashMap::new();
                collect_named_targets(root, "", &mut counts, &mut retained);
                if let Some(palette) = self
                    .action_palette
                    .as_deref()
                    .filter(|palette| palette.is_presented())
                {
                    collect_named_targets(palette, "", &mut counts, &mut retained);
                }
            });
        } else {
            for root in self.presentation_roots() {
                collect_named_targets(root, "", &mut HashMap::new(), &mut retained);
            }
        }
        let mut unique = HashMap::new();
        for target in &retained {
            unique
                .entry(target.key.clone())
                .and_modify(|entry| *entry = false)
                .or_insert(true);
        }
        let mut targets = HashMap::new();
        // Retained order keeps the established deterministic path/test-id aliases.
        for target in retained {
            if unique.get(&target.key) != Some(&true) {
                continue;
            }
            let Some(&Some(node)) = registered.get(&target.key) else {
                continue;
            };
            targets.insert(target.path, node);
            if let Some(bare) = target.bare {
                targets.entry(bare).or_insert(node);
            }
            if let Some(id) = target.id {
                targets.insert(id, node);
            }
        }
        targets
    }

    /// App instance bound to a live semantic owner path or stable widget id.
    pub fn semantic_owner_instance(&self, name: &str) -> Option<crate::application::InstanceId> {
        let node = *self.named_target_nodes().get(name)?;
        self.bindings
            .binding_for(node)
            .map(|binding| binding.instance_id)
    }

    fn access_node_named(
        &self,
        name: &str,
        semantic_target: Option<&str>,
    ) -> Result<AccessNode, String> {
        let targets = self.named_target_nodes();
        let node = targets
            .get(name)
            .copied()
            .ok_or_else(|| format!("no addressable widget named {name:?}"))?;
        let access = self
            .inspect_node(node, |widget| widget.accessibility())
            .ok()
            .flatten()
            .ok_or_else(|| format!("widget {name:?} exposes no accessibility semantics"))?;
        let Some(target) = semantic_target else {
            return Ok(access);
        };
        find_semantic_target(&access, target)
            .cloned()
            .ok_or_else(|| format!("widget {name:?} has no semantic target {target:?}"))
    }

    /// Capture widget identity, geometry, hierarchy, and accessibility from
    /// the same laid-out frame.
    pub fn inspection(&self) -> InspectionSnapshot {
        self.drain_component_wakes();
        let mut sibling_counts = HashMap::new();
        let root = if let Some(search) = self.system_search_inspection() {
            search
        } else if let Some(palette) = self.caller_palette_inspection() {
            palette
        } else if let Some(history) = &self.history {
            let mut presentation = collect_inspection(history.as_ref(), "", &mut sibling_counts);
            if let Some(palette) = self.action_palette.as_deref().filter(|p| p.is_presented()) {
                presentation
                    .children
                    .push(collect_inspection(palette, "", &mut sibling_counts));
            } else if history.live_visible() {
                presentation.children.push(collect_inspection(
                    self.root.as_ref(),
                    "",
                    &mut sibling_counts,
                ));
            }
            presentation
        } else if let Some(palette) = self.action_palette.as_deref().filter(|p| p.is_presented()) {
            collect_inspection(palette, "", &mut sibling_counts)
        } else {
            collect_inspection(self.root.as_ref(), "", &mut sibling_counts)
        };
        fn owner_path(
            node: &InspectionNode,
            targets: &HashMap<String, NodeId>,
            focused: NodeId,
        ) -> Option<String> {
            if targets.get(&node.path) == Some(&focused) {
                return Some(node.path.clone());
            }
            node.children
                .iter()
                .find_map(|child| owner_path(child, targets, focused))
        }
        // Use the same admitted retained-owner resolution as semantic dispatch,
        // never a DOM ordering heuristic or a pointer from another component.
        let keyboard_focus_owner = if self.global_search_query().is_some() {
            Some("system-search".into())
        } else if self.caller_palette.palette.is_some() {
            // The caller palette routes keyboard events to its private search
            // field, outside the document's retained focus tree. Use the field
            // from this exact palette inspection, not the background owner.
            root.children
                .first()
                .filter(|field| {
                    field
                        .semantics
                        .as_ref()
                        .is_some_and(|node| node.state.focused)
                })
                .map(|field| field.path.clone())
        } else {
            self.focused
                .and_then(|focused| owner_path(&root, &self.named_target_nodes(), focused))
        };
        let presentation = self
            .history_chrome_enabled
            .then(|| {
                let mut counts = HashMap::new();
                self.try_presentation(|visible, _| collect_inspection(visible, "", &mut counts))
                    .ok()
            })
            .flatten();
        InspectionSnapshot {
            viewport: [self.viewport.0, self.viewport.1],
            keyboard_focus_owner,
            root,
            presentation,
        }
    }

    /// Pretty JSON form used by the standard `--headless-drive` inspector.
    ///
    /// # Errors
    ///
    /// Returns an error if serializing the pure-data snapshot fails.
    pub fn inspection_json(&self) -> Result<String, serde_json::Error> {
        self.drain_component_wakes();
        serde_json::to_string_pretty(&self.inspection())
    }

    /// Audit the exact retained and synthetic semantic tree exported in the
    /// current laid-out frame.
    #[must_use]
    pub fn accessibility_audit(&self) -> AccessibilityAudit {
        self.drain_component_wakes();
        let inspection = self.inspection();
        let mut issues = Vec::new();
        audit_inspection_node(&inspection.root, &mut issues);
        AccessibilityAudit {
            version: ACCESSIBILITY_AUDIT_VERSION,
            issues,
        }
    }

    /// Certify the laid-out application against the action-native contract.
    ///
    /// Unlike a source-pattern check this resolves bindings against the live
    /// resource tree, so conditional resources and custom widgets are covered.
    /// A custom-painted control participates by exposing accessibility
    /// semantics and an [`crate::widget::ActionEmitter`] inventory entry.
    #[must_use]
    pub fn native_app_audit(&self) -> NativeAppAudit {
        use crate::widget::ControlBindingKind;
        self.drain_component_wakes();

        let resources = self.resources();
        let bindings = crate::widget::controls::control_action_bindings(self.root.as_ref());
        let inventoried_controls = bindings
            .iter()
            .map(|binding| binding.control_id.clone())
            .collect::<HashSet<_>>();
        // The contract is the application's: History mode's chrome, which the
        // host draws around a registered document owner, is toolkit UI and is
        // audited with the toolkit, not with every app that has a history.
        let application = if self.history.is_some() {
            collect_inspection(self.root.as_ref(), "", &mut HashMap::new())
        } else {
            self.inspection().root
        };
        let has_semantics = inspection_has_semantics(&application);
        let semantic_effect_controls = effect_control_ids(&application);
        let mut issues = bindings
            .into_iter()
            .filter_map(|binding| match binding.kind {
                ControlBindingKind::Action(address) => {
                    tilo_actions::door::resolve(&resources, &address)
                        .is_none()
                        .then(|| NativeAppIssue {
                            control: binding.control_id,
                            code: "stale-action-binding".into(),
                            message: format!(
                                "bound action {}/{} is absent from the live resource tree",
                                address.resource, address.action
                            ),
                        })
                }
                ControlBindingKind::CollaborativeDocument(_)
                | ControlBindingKind::SemanticAction(_) => None,
                ControlBindingKind::LegacyDocumentPath(path) => Some(NativeAppIssue {
                    control: binding.control_id,
                    code: "legacy-document-path".into(),
                    message: format!(
                        "direct document path {path:?} bypasses the resource/action door"
                    ),
                }),
                ControlBindingKind::LegacyCallback => Some(NativeAppIssue {
                    control: binding.control_id,
                    code: "legacy-callback".into(),
                    message: "imperative callback bypasses the resource/action door".into(),
                }),
            })
            .collect::<Vec<_>>();
        if !has_semantics && inventoried_controls.is_empty() {
            issues.push(NativeAppIssue {
                control: "application-root".into(),
                code: "empty-native-contract".into(),
                message: "application exposes neither accessibility semantics nor a control/action inventory".into(),
            });
        }
        for control in semantic_effect_controls.difference(&inventoried_controls) {
            issues.push(NativeAppIssue {
                control: control.clone(),
                code: "missing-action-binding".into(),
                message: "effect-capable semantic control has no resource/action binding".into(),
            });
        }
        for control in inventoried_controls.difference(&semantic_effect_controls) {
            issues.push(NativeAppIssue {
                control: control.clone(),
                code: "orphan-action-binding".into(),
                message: "action binding has no matching effect-capable semantic control".into(),
            });
        }
        NativeAppAudit {
            version: NATIVE_APP_AUDIT_VERSION,
            accessibility: self.accessibility_audit(),
            issues,
        }
    }

    /// Snapshot the exact resource tree the native host currently publishes.
    /// Tests can assert search/agent visibility without reaching into widget
    /// internals or rebuilding a parallel catalogue.
    pub fn resources(&self) -> Vec<tilo_actions::Resource> {
        self.drain_component_wakes();
        let mut resources = Vec::new();
        crate::widget::collect_widget_resources(
            self.root.as_ref(),
            &self.state.doc,
            &mut resources,
        );
        if let Ok(legacy) = crate::bound_dispatch::legacy_document::legacy_resources(
            self.root.as_ref(),
            &self.permissions,
            &self.author,
            false,
        ) {
            resources.extend(legacy);
        }
        let mut semantics = crate::a11y_tree::SemanticTree::new();
        semantics.refresh_bound(
            self.root.as_ref(),
            &self.node_map,
            &self.bindings.semantic_node_names(&self.node_map),
            self.focused,
            &resources,
        );
        resources.extend(semantics.action_resources());
        resources
    }

    /// The live document. Used to dump state and to hand to a saver.
    pub fn state_mut(&mut self) -> &mut State {
        self.pump_component_wakes();
        &mut self.state
    }

    /// Deliver platform input including host keyboard policy. Browser/native adapters
    /// forward real events here; synthetic helpers use the same path.
    pub fn send_host_event(&mut self, event: &WindowEvent) {
        self.last_send_skipped_relayout = false;
        if let WindowEvent::ModifiersChanged(modifiers) = event {
            // DOM key capture and Winit can publish the same snapshot repeatedly.
            // Only transitions are input; duplicate snapshots must not run the
            // document sync, action discovery and layout passes below.
            if self.modifiers == *modifiers {
                return;
            }
            self.modifiers = *modifiers;
        }
        // An open search is modal above both live History chrome and the
        // document. Update modifiers first, then capture before History can
        // observe or claim the event.
        if self.system_search_event(event) {
            return;
        }
        // Framework presentations own captured keyboard input before global
        // host navigation, matching native App's early History dispatch.
        let history_input = self
            .history
            .as_ref()
            .is_some_and(|history| history.controls_event(event, self.input_scale))
            || matches!(event, WindowEvent::KeyboardInput { .. })
                && crate::history_mode::embedded::keyboard_captured(
                    self.root.as_ref(),
                    self.focused,
                    &self.taffy,
                );
        if history_input {
            self.send(event);
            return;
        }
        if self.caller_palette_event(event) {
            return;
        }
        if let WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == ElementState::Pressed
        {
            let modal = self.root.modal_capture();
            let navigation =
                crate::focus_navigation::keyboard_nav_allowed(modal, self.focus_trap_active);
            if key.logical_key == Key::Named(winit::keyboard::NamedKey::Tab)
                && navigation
                && !self.focused_widget_wants_tab_input()
                && self.tab(self.modifiers.state().shift_key()).is_some()
            {
                self.relayout();
                return;
            }
            if modal.is_none()
                && (self.active_input_instance.is_none() || self.focused.is_some())
                && let Key::Character(character) = &key.logical_key
            {
                let mods = self.modifiers.state();
                if let Some(request) = crate::undo::shortcut(
                    self.platform,
                    character,
                    self.platform.command_modifier_down(mods),
                    mods.shift_key(),
                ) {
                    let framework = self.framework_input.get()
                        || crate::history_mode::embedded::keyboard_captured(
                            self.root.as_ref(),
                            self.focused,
                            &self.taffy,
                        )
                        || self.history.as_ref().is_some_and(|history| {
                            history.captures_input()
                                || self
                                    .focused
                                    .is_some_and(|node| history.owns_node(node, &self.taffy))
                        });
                    if framework {
                        return;
                    }
                    if crate::undo::handle_local_undo_request(self.root.as_mut(), request) {
                        self.relayout();
                    } else {
                        match request {
                            crate::undo::UndoRequest::Undo => {
                                self.perform_undo();
                            }
                            crate::undo::UndoRequest::Redo => {
                                self.perform_redo();
                            }
                        }
                    }
                    return;
                }
            }
        }
        let host_navigation_allowed = crate::focus_navigation::keyboard_nav_allowed(
            self.root.modal_capture(),
            self.focus_trap_active,
        );
        self.send_with_host_navigation(event, host_navigation_allowed);
    }

    /// Deliver one event through the production dispatch path.
    pub fn send(&mut self, event: &WindowEvent) {
        self.send_with_host_navigation(event, false);
    }

    fn send_with_host_navigation(&mut self, event: &WindowEvent, host_navigation_allowed: bool) {
        if let WindowEvent::ModifiersChanged(modifiers) = event {
            self.modifiers = *modifiers;
        }
        // Search owns the frame while presented; underlying History and app
        // surfaces must not observe its pointer or keyboard input.
        if self.system_search_event(event) {
            return;
        }
        if self
            .action_palette
            .as_deref()
            .is_some_and(|p| p.is_presented())
            && !self.history.as_ref().is_some_and(|h| h.captures_input())
        {
            self.discard_layout_tree();
            self.focused = None;
            if let Some(palette) = self.action_palette.as_deref_mut() {
                palette.handle_event(
                    event,
                    &mut EventCtx {
                        scale_factor: self.input_scale,
                        modifiers: &self.modifiers,
                        state: &mut self.state.doc,
                        path: &self.state.root,
                        handled: &mut false,
                        focus_target: &mut None,
                        node_id: NodeId::from(0usize),
                        taffy: &self.taffy,
                    },
                );
            }
            self.finish_action_palette_input();
            self.relayout();
            return;
        }
        // This guard includes modality-driven relayout before dispatch, and
        // remains set through the frame that closes History.
        let embedded = crate::history_mode::embedded::observe_event(
            self.root.as_mut(),
            event,
            self.input_scale,
            self.focused,
            &self.taffy,
        );
        let mut history_input = self.framework_input.get()
            || embedded
            || self
                .history
                .as_ref()
                .is_some_and(|history| history.controls_event(event, self.input_scale));
        let framework_frame = FrameworkInputFrame::new(&self.framework_input, history_input);
        self.pump_component_wakes();
        let detected = if self.dispatching_touch_pointer {
            None
        } else {
            match event {
                WindowEvent::Touch(_) => Some(self.interaction.with_touch_modality()),
                WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. } => Some(self.interaction.with_pointer_modality()),
                WindowEvent::KeyboardInput {
                    event:
                        winit::event::KeyEvent {
                            state: ElementState::Pressed,
                            ..
                        },
                    ..
                } => Some(self.interaction.with_keyboard_modality()),
                _ => None,
            }
        };
        if let Some(environment) = detected
            && environment != self.interaction
        {
            self.interaction = environment;
            self.root.set_interaction_environment(environment);
            self.invalidate_layout_tree();
            self.relayout();
        }
        if self.active_input_instance.is_some()
            && self.focused.is_none()
            && matches!(
                event,
                WindowEvent::Ime(_) | WindowEvent::KeyboardInput { .. }
            )
            && self.root.modal_capture().is_none()
            && !history_input
        {
            self.dispatch_scoped_shortcut(event);
            return;
        }
        if let WindowEvent::Touch(touch) = event {
            let scale = self.input_scale;
            let mut touch_pointer = std::mem::take(&mut self.touch_pointer);
            let pointer_events = touch_pointer.events(touch, |position| {
                self.claims_raw_touch_at(Point::new(position.x / scale, position.y / scale))
            });
            self.touch_pointer = touch_pointer;
            self.dispatch_touch_pointer(touch.device_id, pointer_events);
        }
        // Mouse side buttons navigate the tree's history, as in the live host.
        if let Some(direction) = crate::interaction::mouse_nav_direction(event)
            && self
                .root
                .on_nav_gesture(direction, &mut self.state.doc, &self.state.root)
        {
            return;
        }
        let mut handled = false;
        let mut focus_target: Option<NodeId> = None;
        let t_event = self.profiler.as_ref().map(|_| crate::time::now());
        let history_handled = self.history_event(event, &mut handled, &mut focus_target);
        history_input |= history_handled;
        let standalone_frame = FrameworkInputFrame::new(&self.framework_input, history_handled);
        if !history_handled && let Some(history) = &mut self.history {
            history.route_event(
                self.root.as_mut(),
                event,
                &mut EventCtx {
                    scale_factor: self.input_scale,
                    modifiers: &self.modifiers,
                    state: &mut self.state.doc,
                    path: &self.state.root,
                    handled: &mut handled,
                    focus_target: &mut focus_target,
                    node_id: self.child_node,
                    taffy: &self.taffy,
                },
            );
        } else if !history_handled && self.history_layout.is_none_or(|layout| layout.app_visible) {
            self.root.handle_event(
                event,
                &mut EventCtx {
                    scale_factor: self.input_scale,
                    modifiers: &self.modifiers,
                    state: &mut self.state.doc,
                    path: &self.state.root,
                    handled: &mut handled,
                    focus_target: &mut focus_target,
                    node_id: self.child_node,
                    taffy: &self.taffy,
                },
            );
        }
        if let Some(p) = self.profiler.as_mut() {
            // Banked; folded into the frame the following `relayout` commits.
            p.record_since(crate::profiling::Pass::HandleEvent, t_event);
        }
        // Mirror the App: a widget that claimed focus during dispatch
        // becomes the focused node, so subsequent keystrokes route to it.
        if let Some(t) = focus_target {
            self.set_focused(Some(t));
        }
        if host_navigation_allowed
            && !handled
            && !history_input
            && self.modifiers.state().alt_key()
            && let WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == ElementState::Pressed
            && let Key::Named(named) = key.logical_key
            && let Some(direction) = crate::focus_navigation::Direction::from_key(named)
        {
            let order = self.focus_order();
            let next = match self.focused {
                Some(current) => self
                    .inspect_node(current, |widget| widget.get_rect())
                    .ok()
                    .and_then(|rect| {
                        crate::focus_navigation::next(
                            (current, rect),
                            order.iter().filter_map(|&id| {
                                self.inspect_node(id, |widget| widget.get_rect())
                                    .ok()
                                    .map(|rect| (id, rect))
                            }),
                            direction,
                        )
                    }),
                None => order.first().copied(),
            };
            if let Some(next) = next {
                self.set_focused(Some(next));
            }
            // Host directional navigation owns recognized Alt+Arrow input even
            // when no focus target exists, matching the live App contract.
            handled = true;
        }
        if history_handled {
            if let Err(error) = self.history_tick() {
                tracing::debug!(%error, "headless History input refused");
            }
        } else {
            if !history_input
                || self.focused.is_some_and(|node| {
                    crate::history_mode::embedded::owns_node(self.root.as_ref(), node, &self.taffy)
                        || self
                            .history
                            .as_ref()
                            .is_some_and(|history| history.owns_node(node, &self.taffy))
                })
            {
                self.handle_unconsumed_activation_key(event, &mut handled);
            }
            if history_input {
                // Framework editors (and History chrome) capture the keyboard
                // before the ordinary host tick. Still record dispatched action
                // brackets: ActionHost records them through get_heads, which
                // closes the Automerge transaction so the next resource sync
                // can refresh incrementally instead of re-reading the whole
                // document on every keystroke.
                //
                // Do not dispatch new live-tree control invocations here.
                // History seals those queues on entry; dispatching first would
                // empty them before capture and drop retained work.
                self.poll_control_invocations();
                let author = self.author.clone();
                // SAFETY: this host exclusively owns the unchanged root and admitted layout.
                unsafe {
                    crate::undo::record_dispatched_action_bound(
                        &mut self.state,
                        std::ptr::from_mut(self.root.as_mut()),
                        &self.node_map,
                        &self.bindings,
                        &author,
                        &self.permissions,
                    );
                }
            } else {
                self.host_tick();
            }
        }
        // Inert pointer motion: no widget consumed it, focus and the interaction
        // environment are unchanged, and no presentation root reports new paint,
        // animation or a due wake. Such a move cannot change the frame, so it
        // skips the compose/layout/draw pass. Hover changes still count as paint
        // because retained widgets compare hover against their drawn state.
        let unconsumed_motion = matches!(event, WindowEvent::CursorMoved { .. })
            && !handled
            && !history_input
            && focus_target.is_none();
        let inert = unconsumed_motion && !self.frame_schedule_demands_frame();
        if unconsumed_motion && !inert && self.pointer_redraw_diagnostics {
            self.pointer_redraw_sources = self.redraw_sources();
        }
        self.last_send_skipped_relayout = inert;
        if !inert {
            self.relayout();
        }
        // Frames restore the thread-wide flag they captured, so they must
        // close in reverse order of opening.
        drop(standalone_frame);
        drop(framework_frame);
        if self.service_pending_history_commands() {
            let _framework_frame = FrameworkInputFrame::new(&self.framework_input, true);
            self.relayout();
        }
    }

    /// Diagnostics only: record which dirty widget paths made unconsumed
    /// pointer motion produce a frame. Off by default; no document content.
    pub fn set_pointer_redraw_diagnostics(&mut self, enabled: bool) {
        self.pointer_redraw_diagnostics = enabled;
        self.pointer_redraw_sources.clear();
    }
    pub fn take_pointer_redraw_sources(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pointer_redraw_sources)
    }

    fn frame_schedule_demands_frame(&self) -> bool {
        let schedule = self.frame_schedule();
        schedule.needs_redraw
            || schedule.needs_animation_frame
            || schedule
                .next_wake_deadline
                .is_some_and(|wake| wake <= crate::time::now())
    }

    /// Whether the most recent [`Self::send_host_event`] skipped its relayout
    /// because it was inert pointer motion.
    pub fn last_send_skipped_relayout(&self) -> bool {
        self.last_send_skipped_relayout
    }

    /// Everything the live host does between one interaction and the next
    /// frame, in the order [`App`](crate::app::App)'s idle tick does it.
    ///
    /// Runs after *every* route that can reach a widget — a window event
    /// ([`send`](Self::send)) and an accessibility verb
    /// ([`perform_a11y_action`](Self::perform_a11y_action)) alike. A route that
    /// skipped it would let a scenario mutate the document without the history
    /// ever learning about it, and a test written over that route could drive
    /// an app but never observe what the driving produced.
    ///
    /// The three steps are ordered, not incidental:
    ///
    /// 1. **Dispatched actions** become one undo boundary, through the shared
    ///    [`undo::record_dispatched_action`](crate::undo::record_dispatched_action)
    ///    the App calls.
    /// 2. **A requested commit** closes the transaction, for widgets that edit
    ///    the document directly instead of dispatching actions.
    /// 3. **Undo/redo the tree asked for** runs last, so an action performed in
    ///    the same tick is already on the stack and is what gets reverted.
    fn idle_control_tick(&mut self) {
        self.dispatch_control_invocations();
        self.poll_control_invocations();
        let author = self.author.clone();
        // SAFETY: this host exclusively owns the unchanged root and admitted layout.
        unsafe {
            crate::undo::record_dispatched_action_bound(
                &mut self.state,
                std::ptr::from_mut(self.root.as_mut()),
                &self.node_map,
                &self.bindings,
                &author,
                &self.permissions,
            );
        }
    }

    fn host_tick(&mut self) {
        self.idle_control_tick();
        let author = self.author.clone();
        if crate::undo::needs_commit(self.root.as_ref()) {
            // Authored *and* carrying the widget's commit context. An
            // anonymous entry is invisible to author-scoped undo, so a
            // scenario that pressed Ctrl+Z would find nothing where the user
            // finds their last edit; dropping the context loses the
            // provenance the history explorer reads.
            let context = crate::undo::take_commit_context(self.root.as_mut());
            self.state
                .commit_action_authored_with_context(None, None, Some(author), context);
            crate::undo::on_commit(self.root.as_mut());
        }
        // An Undo/Redo control the widget tree activated during that event.
        // Same route the live host takes: the widget asks, the host performs.
        self.drain_undo_requests();
    }

    /// Invoke one explicit component destination using its current captured owner.
    ///
    /// # Errors
    /// Refuses unavailable, denied, stale or ambiguous destinations and action failures.
    pub fn invoke_bound_action(
        &mut self,
        address: &crate::bound_dispatch::BoundActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        self.pump_component_wakes();
        self.invoke_bound_action_checked(address, arguments, false)
            .map_err(crate::bound_dispatch::BoundActionError::into_door)
    }

    /// Invoke an action after the host caller explicitly confirmed this exact
    /// request. Permission, live owner and action availability are still checked.
    ///
    /// # Errors
    /// Refuses unavailable, denied, stale or ambiguous destinations and action failures.
    pub fn invoke_bound_action_confirmed(
        &mut self,
        address: &crate::bound_dispatch::BoundActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        self.pump_component_wakes();
        self.invoke_bound_action_checked(address, arguments, true)
            .map_err(crate::bound_dispatch::BoundActionError::into_door)
    }

    /// The resources the hosted component publishes, as its permissions let
    /// its bound actions be invoked. With `id`, only that resource: an app
    /// may resolve it lazily (a search hit, an open document), so it can be
    /// absent from the root listing and still be addressable.
    ///
    /// # Errors
    /// Refuses a closed or unavailable component.
    pub fn bound_resources(
        &self,
        id: Option<&str>,
    ) -> Result<Vec<tilo_actions::Resource>, tilo_actions::DoorError> {
        self.bound_resources_of(None, id)
    }

    /// [`Self::bound_resources`] of one hosted component instance, such as
    /// one window of a desktop; `None` for every hosted component.
    ///
    /// # Errors
    /// Refuses a closed or unavailable component.
    pub fn bound_resources_of(
        &self,
        instance: Option<crate::application::InstanceId>,
        id: Option<&str>,
    ) -> Result<Vec<tilo_actions::Resource>, tilo_actions::DoorError> {
        self.drain_component_wakes();
        let lazy = id.map(|id| tilo_actions::ActionAddress::new(id, ""));
        let resources = crate::bound_dispatch::discover_bound_resources(
            self.root.as_ref(),
            &self.state.doc,
            lazy.as_ref(),
        )
        .map_err(|error| crate::bound_dispatch::BoundActionError::Dispatch(error).into_door())?
        .into_iter()
        .filter(|snapshot| instance.is_none_or(|instance| snapshot.binding.instance_id == instance))
        .flat_map(|snapshot| snapshot.resources)
        .collect::<Vec<_>>();
        // The app's own resolution of `id` is a top-level entry and wins over
        // the summary its eager tree may hold (a channel without its messages).
        Ok(match id {
            Some(id) => resources
                .iter()
                .find(|resource| resource.id.as_str() == id)
                .or_else(|| crate::bound_dispatch::find_action_resource(&resources, id))
                .cloned()
                .into_iter()
                .collect(),
            None => resources,
        })
    }

    fn invoke_bound_action_checked(
        &mut self,
        address: &crate::bound_dispatch::BoundActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirmed: bool,
    ) -> Result<crate::widget::ResourceOperationStart, crate::bound_dispatch::BoundActionError>
    {
        let snapshots = crate::bound_dispatch::discover_bound_resources(
            self.root.as_ref(),
            &self.state.doc,
            Some(&address.address),
        )?;
        let target = crate::bound_dispatch::select_bound_target(
            &snapshots,
            &address.address,
            Some(address.instance_id),
        )?
        .ok_or_else(|| tilo_actions::DoorError::Unknown(address.address.clone()))?;
        let invocation = tilo_actions::InvocationId::new();
        let started = crate::bound_dispatch::invoke_resource_action(
            self.root.as_mut(),
            &mut self.state.doc,
            &target,
            self.action_transport.as_mut(),
            crate::bound_dispatch::ResourceActionRequest {
                argument: None,
                arguments: Some(arguments),
                actor: &self.author.0,
                confirmed,
                broker_admitted: false,
                invocation,
                ambient: None,
                scope: None,
            },
        )?;
        if matches!(started, crate::widget::ResourceOperationStart::Accepted) {
            self.bound_invocations.insert(invocation, target);
        }
        Ok(started)
    }

    fn open_ordinary_confirmation(
        &mut self,
        target: crate::bound_dispatch::legacy_document::LocalResourceTarget,
        intent: &tilo_actions::ControlInvocation,
    ) {
        let request = crate::bound_dispatch::OrdinaryConfirmation::prepare(
            self.root.as_ref(),
            &self.state.doc,
            target,
            &self.control_domain,
            intent.address.clone(),
            crate::bound_dispatch::ControlContinuationArguments::Json(intent.arguments.clone()),
        );
        let Ok(request) = request else {
            return;
        };
        self.discard_layout_tree();
        self.focused = None;
        let mut palette = Box::new(crate::action_palette::ActionPalette::with_host_text(
            self.text.font_system.locale().to_owned(),
        ));
        palette.activate_ordinary(request);
        self.action_palette = Some(palette);
    }
    fn finish_action_palette_input(&mut self) {
        let pending = self
            .action_palette
            .as_deref_mut()
            .and_then(crate::action_palette::ActionPalette::take_operation);
        if let Some(pending) = pending {
            let result = (|| {
                let request = pending.ordinary.as_ref().ok_or_else(|| {
                    tilo_actions::DoorError::InvalidArguments(
                        "Missing original confirmation request".into(),
                    )
                })?;
                request.validate(self.root.as_ref(), &self.state.doc, &self.control_domain)?;
                if let Some(target) = &pending.history_target {
                    let captured = pending
                        .history_witness
                        .as_ref()
                        .and_then(|w| w.as_ref().ok())
                        .ok_or_else(|| tilo_actions::DoorError::Unavailable {
                            code: "unsupported_continuation_identity".into(),
                            reason: "Original History continuation evidence is unavailable".into(),
                        })?;
                    let invocation = pending.control_invocation().ok_or_else(|| {
                        tilo_actions::DoorError::InvalidArguments("Invalid retained request".into())
                    })?;
                    let current = crate::bound_dispatch::control_witness_with_arguments(
                        self.root.as_ref(),
                        &self.state.doc,
                        target,
                        &invocation,
                        &request.arguments,
                    )?;
                    if &current != captured {
                        return Err(tilo_actions::DoorError::Unavailable {
                            code: "stale_pending_palette_action".into(),
                            reason: "History continuation changed".into(),
                        });
                    }
                }
                self.dispatch_local_control(
                    request.target.clone(),
                    &request.address,
                    &request.arguments,
                    true,
                )
            })();
            if let Err(error) = result {
                tracing::warn!(%error,"headless confirmation refused");
                if pending.history_target.is_some() {
                    if let Some(palette) = self.action_palette.as_deref_mut() {
                        palette.retain_history_refusal(pending, error.to_string());
                    }
                }
            }
        }
        if self.action_palette.as_deref().is_some_and(|p| !p.is_open()) {
            self.action_palette = None;
        }
    }
    fn perform_palette_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
    ) -> Result<(), String> {
        self.discard_layout_tree();
        self.focused = None;
        let handled = self.action_palette.as_deref_mut().is_some_and(|palette| {
            palette.perform_semantic_action(target, action, &mut self.state.doc, &self.state.root)
        });
        self.finish_action_palette_input();
        self.relayout();
        if handled {
            Ok(())
        } else {
            Err("stale or unsupported palette target".into())
        }
    }
    fn capture_palette_history(&mut self) {
        if !self.history.as_ref().is_some_and(|h| h.captures_input()) {
            return;
        }
        let Some(request) = self
            .action_palette
            .as_deref()
            .filter(|p| !p.history_has_custody())
            .and_then(|p| p.ordinary_request())
            .cloned()
        else {
            return;
        };
        let invocation = tilo_actions::ControlInvocation {
            address: request.address.clone(),
            arguments: match &request.arguments {
                crate::bound_dispatch::ControlContinuationArguments::Json(json) => json.clone(),
                _ => serde_json::Map::new(),
            },
        };
        let witness = request
            .validate_owner(&self.control_domain)
            .and_then(|()| {
                crate::bound_dispatch::control_witness_with_arguments(
                    self.root.as_ref(),
                    &self.state.doc,
                    &request.target,
                    &invocation,
                    &request.arguments,
                )
            })
            .map_err(|e| e.to_string());
        let identity = Some(
            witness
                .as_ref()
                .map(|w| w.identity().clone())
                .map_err(Clone::clone),
        );
        self.discard_layout_tree();
        self.focused = None;
        if let Some(palette) = self.action_palette.as_deref_mut() {
            palette.suspend_for_history();
            palette.retain_history_custody(request.target, identity, Some(witness));
        }
    }

    fn dispatch_bound_control(
        &mut self,
        target: crate::bound_dispatch::BoundActionTarget,
        intent: &tilo_actions::ControlInvocation,
    ) {
        let invocation = tilo_actions::InvocationId::new();
        let result = crate::bound_dispatch::invoke_resource_action(
            self.root.as_mut(),
            &mut self.state.doc,
            &target,
            self.action_transport.as_mut(),
            crate::bound_dispatch::ResourceActionRequest {
                argument: None,
                arguments: Some(&intent.arguments),
                actor: &self.author.0,
                confirmed: false,
                broker_admitted: false,
                invocation,
                ambient: None,
                scope: None,
            },
        )
        .map_err(crate::bound_dispatch::BoundActionError::into_door);
        if let Err(error) = crate::bound_dispatch::notify_control_invocation_result(
            self.root.as_mut(),
            &target,
            &result,
        ) {
            tracing::warn!(?error, "headless: bound control feedback unavailable");
        }
        match result {
            Ok(crate::widget::ResourceOperationStart::Accepted) => {
                self.bound_invocations.insert(invocation, target);
            }
            Ok(crate::widget::ResourceOperationStart::Completed(_)) => {}
            Err(tilo_actions::DoorError::ConfirmationRequired) => {
                self.open_ordinary_confirmation(
                    crate::bound_dispatch::legacy_document::LocalResourceTarget::Component(target),
                    intent,
                );
            }
            Err(error) => {
                tracing::warn!(code = error.code(), %error, "headless: bound control refused");
                self.control_failures.push(error.to_string());
            }
        }
    }

    fn dispatch_legacy_control(
        &mut self,
        target: &crate::bound_dispatch::legacy_document::LegacyResourceTarget,
        intent: &tilo_actions::ControlInvocation,
    ) {
        use crate::bound_dispatch::legacy_document::{
            LegacyDocumentServices, LegacyInvocation, authorize_legacy_operation,
            invoke_legacy_document, invoke_legacy_effect, resolve_legacy_invocation,
        };
        let invocation = tilo_actions::InvocationId::new();
        let result = (|| {
            let resources = resolve_legacy_invocation(self.root.as_ref(), target)?;
            let (arguments, _, spec) = crate::bound_dispatch::resolve_resource_invocation(
                &resources,
                &target.address,
                None,
                Some(&intent.arguments),
                false,
            )?;
            if spec.kind == tilo_actions::OperationKind::Document {
                let _ = invoke_legacy_document(
                    self.root.as_mut(),
                    target,
                    LegacyDocumentServices {
                        registry: &self.action_registry,
                        transport: self.action_transport.as_mut(),
                        qualified_local_transport: true,
                        authority: &self.permissions,
                        author: &self.author,
                    },
                    LegacyInvocation {
                        argument: None,
                        arguments: Some(&intent.arguments),
                        actor: &self.author.0,
                        broker_admitted: false,
                        ambient: None,
                    },
                )?;
                return Ok(crate::widget::ResourceOperationStart::Completed(
                    tilo_actions::ActionOutput::default(),
                ));
            }
            authorize_legacy_operation(target, &self.permissions, spec)?;
            let encoded = crate::bound_dispatch::encode_resource_action(
                &resources,
                &target.address,
                arguments.as_ref(),
                None,
            )?;
            let result = invoke_legacy_effect(
                self.root.as_mut(),
                target,
                invocation,
                spec.kind,
                &encoded,
                &mut self.state.doc,
            )?;
            match &result {
                crate::widget::ResourceOperationStart::Accepted
                    if spec.contract.execution == tilo_actions::ExecutionMode::Immediate =>
                {
                    return Err(tilo_actions::DoorError::Failed(
                        "Immediate operation remained pending".into(),
                    ));
                }
                crate::widget::ResourceOperationStart::Completed(output) => spec
                    .contract
                    .result
                    .validate(output)
                    .map_err(tilo_actions::DoorError::Failed)?,
                crate::widget::ResourceOperationStart::Accepted => {}
            }
            Ok(result)
        })();
        if let Err(error) =
            crate::bound_dispatch::legacy_document::notify_legacy_control_invocation_result(
                self.root.as_mut(),
                target,
                &result,
            )
        {
            tracing::warn!(%error, "headless: legacy control feedback unavailable");
        }
        match result {
            Ok(crate::widget::ResourceOperationStart::Accepted) => {
                self.pending_control_invocations.insert(invocation);
            }
            Ok(_) => {}
            Err(tilo_actions::DoorError::ConfirmationRequired) => self.open_ordinary_confirmation(
                crate::bound_dispatch::legacy_document::LocalResourceTarget::Legacy(target.clone()),
                intent,
            ),
            Err(error) => {
                tracing::warn!(%error, "headless legacy action refused");
                self.control_failures.push(error.to_string());
            }
        }
    }

    fn continue_pending_history_control_checked(
        &mut self,
        token: &crate::history_mode::pending_controls::ContinuationToken,
        confirmed: bool,
    ) -> Option<crate::history_mode::pending_controls::PendingControlStatus> {
        if self.framework_input.get()
            || self
                .history
                .as_ref()
                .is_none_or(|history| history.captures_input())
        {
            return None;
        }
        let prepared = self
            .history
            .as_mut()?
            .prepare_pending_control(token, confirmed)?;
        let id = prepared.id;
        let result = self.dispatch_prepared_history_control(prepared);
        let history = self.history.as_mut()?;
        match result {
            Err(tilo_actions::DoorError::ConfirmationRequired) if !confirmed => {
                history.pending_control_needs_confirmation(id);
                Some(crate::history_mode::pending_controls::PendingControlStatus::NeedsConfirmation)
            }
            Ok(_) => {
                history.pending_control_consumed(id);
                Some(crate::history_mode::pending_controls::PendingControlStatus::Consumed)
            }
            Err(error) => {
                let reason = error.to_string();
                history.pending_control_refused(id, reason.clone());
                Some(crate::history_mode::pending_controls::PendingControlStatus::Refused(reason))
            }
        }
    }

    fn service_pending_history_commands(&mut self) -> bool {
        let commands = self
            .history
            .as_ref()
            .map_or_else(Vec::new, |history| history.drain_pending_control_commands());
        let mut serviced = !commands.is_empty();
        for command in commands {
            match command {
                crate::history_mode::pending_controls::ContinuationCommand::Review(token) => {
                    let _ = self.continue_pending_history_control(&token);
                }
                crate::history_mode::pending_controls::ContinuationCommand::Confirm(
                    token,
                    value,
                ) => {
                    let prepared = self
                        .history
                        .as_mut()
                        .and_then(|history| history.prepare_pending_control_typed(&token, &value));
                    if let Some(prepared) = prepared {
                        let id = prepared.id;
                        let outcome = self.dispatch_prepared_history_control(prepared);
                        if let Some(history) = self.history.as_mut() {
                            match outcome {
                                Ok(_) => history.pending_control_consumed(id),
                                Err(error) => {
                                    history.pending_control_refused(id, error.to_string())
                                }
                            }
                        }
                    }
                }
                crate::history_mode::pending_controls::ContinuationCommand::Cancel(token) => {
                    self.cancel_pending_history_control(&token);
                }
            }
        }
        let embedded = crate::history_mode::embedded::drain_pending_commands(self.root.as_mut());
        serviced |= !embedded.is_empty();
        for pending in embedded {
            let (token, confirmation) = match pending.command {
                crate::history_mode::pending_controls::ContinuationCommand::Review(token) => {
                    (token, None)
                }
                crate::history_mode::pending_controls::ContinuationCommand::Confirm(
                    token,
                    value,
                ) => (token, Some(value)),
                crate::history_mode::pending_controls::ContinuationCommand::Cancel(token) => {
                    crate::history_mode::embedded::cancel_pending_command(
                        self.root.as_mut(),
                        pending.surface,
                        &token,
                    );
                    continue;
                }
            };
            let prepared = match confirmation.as_deref() {
                Some(value) => crate::history_mode::embedded::prepare_pending_command_typed(
                    self.root.as_mut(),
                    pending.surface,
                    &token,
                    value,
                ),
                None => crate::history_mode::embedded::prepare_pending_command(
                    self.root.as_mut(),
                    pending.surface,
                    &token,
                    false,
                ),
            };
            let Some(prepared) = prepared else {
                continue;
            };
            let confirmed = confirmation.is_some();
            let id = prepared.id;
            let result = self.dispatch_prepared_history_control(prepared);
            let outcome = match result {
                Err(tilo_actions::DoorError::ConfirmationRequired) if !confirmed => {
                    crate::history_mode::embedded::PendingCommandOutcome::NeedsConfirmation
                }
                Ok(_) => crate::history_mode::embedded::PendingCommandOutcome::Consumed,
                Err(error) => {
                    crate::history_mode::embedded::PendingCommandOutcome::Refused(error.to_string())
                }
            };
            crate::history_mode::embedded::finish_pending_command(
                self.root.as_mut(),
                pending.surface,
                id,
                &outcome,
            );
        }
        serviced
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one exact continuation door keeps component, legacy and outer lifecycle bookkeeping together"
    )]
    fn dispatch_prepared_history_control(
        &mut self,
        prepared: crate::history_mode::pending_controls::PreparedContinuation,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        let captured =
            prepared
                .witness
                .as_ref()
                .map_err(|reason| tilo_actions::DoorError::Unavailable {
                    code: "stale_pending_control".into(),
                    reason: reason.clone(),
                })?;
        let current = crate::bound_dispatch::control_witness(
            self.root.as_ref(),
            &self.state.doc,
            &prepared.target,
            &prepared.invocation,
        )?;
        if &current != captured {
            return Err(tilo_actions::DoorError::Unavailable {
                code: "stale_pending_control".into(),
                reason: "The action meaning or confirmation policy changed while History was open"
                    .into(),
            });
        }
        self.dispatch_local_control(
            prepared.target,
            &prepared.invocation.address,
            &crate::bound_dispatch::ControlContinuationArguments::Json(
                prepared.invocation.arguments,
            ),
            prepared.confirmed,
        )
    }

    fn dispatch_local_control(
        &mut self,
        target: crate::bound_dispatch::legacy_document::LocalResourceTarget,
        address: &tilo_actions::ActionAddress,
        original: &crate::bound_dispatch::ControlContinuationArguments,
        confirmed: bool,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        use crate::bound_dispatch::legacy_document::LocalResourceTarget;
        let (argument, json) = original.as_door_arguments();
        let invocation_id = tilo_actions::InvocationId::new();
        let started = match target {
            LocalResourceTarget::Component(target) => {
                let result = crate::bound_dispatch::invoke_resource_action(
                    self.root.as_mut(),
                    &mut self.state.doc,
                    &target,
                    self.action_transport.as_mut(),
                    crate::bound_dispatch::ResourceActionRequest {
                        argument,
                        arguments: json,
                        actor: &self.author.0,
                        confirmed,
                        broker_admitted: false,
                        invocation: invocation_id,
                        ambient: None,
                        scope: None,
                    },
                )
                .map_err(crate::bound_dispatch::BoundActionError::into_door)?;
                if matches!(result, crate::widget::ResourceOperationStart::Accepted) {
                    self.bound_invocations.insert(invocation_id, target);
                }
                result
            }
            LocalResourceTarget::Legacy(target) => {
                use crate::bound_dispatch::legacy_document::{
                    LegacyDocumentServices, LegacyInvocation, authorize_legacy_operation,
                    invoke_legacy_document, invoke_legacy_effect, resolve_legacy_invocation,
                };
                let resources = resolve_legacy_invocation(self.root.as_ref(), &target)?;
                let (arguments, _, spec) = crate::bound_dispatch::resolve_resource_invocation(
                    &resources,
                    &target.address,
                    argument,
                    json,
                    confirmed,
                )?;
                if spec.kind == tilo_actions::OperationKind::Document {
                    invoke_legacy_document(
                        self.root.as_mut(),
                        &target,
                        LegacyDocumentServices {
                            registry: &self.action_registry,
                            transport: self.action_transport.as_mut(),
                            qualified_local_transport: true,
                            authority: &self.permissions,
                            author: &self.author,
                        },
                        LegacyInvocation {
                            argument,
                            arguments: json,
                            actor: &self.author.0,
                            broker_admitted: false,
                            ambient: None,
                        },
                    )?;
                    crate::widget::ResourceOperationStart::Completed(
                        tilo_actions::ActionOutput::default(),
                    )
                } else {
                    authorize_legacy_operation(&target, &self.permissions, spec)?;
                    let encoded = crate::bound_dispatch::encode_resource_action(
                        &resources,
                        &target.address,
                        arguments.as_ref(),
                        argument,
                    )?;
                    let result = invoke_legacy_effect(
                        self.root.as_mut(),
                        &target,
                        invocation_id,
                        spec.kind,
                        &encoded,
                        &mut self.state.doc,
                    )?;
                    match &result {
                        crate::widget::ResourceOperationStart::Accepted
                            if spec.contract.execution
                                == tilo_actions::ExecutionMode::Immediate =>
                        {
                            return Err(tilo_actions::DoorError::Failed(
                                "Immediate operation remained pending".into(),
                            ));
                        }
                        crate::widget::ResourceOperationStart::Completed(output) => spec
                            .contract
                            .result
                            .validate(output)
                            .map_err(tilo_actions::DoorError::Failed)?,
                        crate::widget::ResourceOperationStart::Accepted => {
                            self.pending_control_invocations.insert(invocation_id);
                        }
                    }
                    result
                }
            }
            LocalResourceTarget::Outer => {
                let mut resources = Vec::new();
                crate::bound_dispatch::collect_unbound_resources(
                    self.root.as_ref(),
                    &self.state.doc,
                    &mut resources,
                );
                if tilo_actions::door::resolve(&resources, address).is_none()
                    && let Some(resource) = crate::bound_dispatch::resolve_unbound_resource(
                        self.root.as_ref(),
                        &self.state.doc,
                        &address.resource,
                    )
                {
                    resources.push(resource);
                }
                resources = resources
                    .into_iter()
                    .filter_map(|resource| {
                        Self::filter_pending_resource_for_authority(&self.permissions, resource)
                    })
                    .collect();
                let (arguments, resource, spec) =
                    crate::bound_dispatch::resolve_resource_invocation(
                        &resources, address, argument, json, confirmed,
                    )?;
                let encoded = crate::bound_dispatch::encode_resource_action(
                    &resources,
                    address,
                    arguments.as_ref(),
                    argument,
                )?;
                let before = (spec.kind == tilo_actions::OperationKind::Document)
                    .then(|| self.state.doc.get_heads());
                let result = crate::bound_dispatch::execute_resource_operation(
                    self.root.as_mut(),
                    &mut self.state.doc,
                    &self.state.root,
                    &self.action_registry,
                    self.action_transport.as_mut(),
                    crate::bound_dispatch::ResourceOperation {
                        address,
                        resource,
                        spec,
                        encoded,
                        actor: &self.author.0,
                        local_author: &self.author.0,
                        broker_admitted: false,
                        invocation: invocation_id,
                        ambient: None,
                        scope: None,
                    },
                )?;
                if let Some(before) = before {
                    self.state.record_committed_action_authored(
                        before,
                        None,
                        None,
                        Some(self.author.clone()),
                    );
                }
                if matches!(result, crate::widget::ResourceOperationStart::Accepted) {
                    self.pending_control_invocations.insert(invocation_id);
                }
                result
            }
        };
        Ok(started)
    }

    fn filter_pending_resource_for_authority(
        authority: &PermissionContext,
        mut resource: tilo_actions::Resource,
    ) -> Option<tilo_actions::Resource> {
        use crate::permissions::Verb;
        resource.actions.retain(|action| {
            let verb = match action.contract.permission {
                tilo_actions::PermissionRequirement::None => return true,
                tilo_actions::PermissionRequirement::Read => Verb::Read,
                tilo_actions::PermissionRequirement::Comment => Verb::Comment,
                tilo_actions::PermissionRequirement::Write => Verb::Write,
                tilo_actions::PermissionRequirement::Invite => Verb::Invite,
                tilo_actions::PermissionRequirement::Admin => Verb::Admin,
            };
            authority.check(&crate::permissions::PermissionScope::app(
                crate::collaboration::local_app_id(),
                verb,
            ))
        });
        resource.children = resource
            .children
            .into_iter()
            .filter_map(|child| Self::filter_pending_resource_for_authority(authority, child))
            .collect();
        (!resource.actions.is_empty() || !resource.children.is_empty()).then_some(resource)
    }

    fn dispatch_control_invocations(&mut self) {
        crate::history_mode::embedded::discard_active_output(self.root.as_mut());
        let mut registered = self.control_domain.drain_partitioned();
        let bound = crate::bound_dispatch::drain_control_invocations(self.root.as_mut());
        if let Some(error) = bound.error {
            tracing::warn!(
                ?error,
                "headless: control traversal incomplete; delivering already drained intents"
            );
        }
        for (target, intent) in registered.components.into_iter().chain(bound.pending) {
            self.dispatch_bound_control(target, &intent);
        }

        let batch =
            crate::bound_dispatch::legacy_document::drain_legacy_controls(self.root.as_mut());
        for error in batch.failures {
            tracing::warn!(%error, "headless legacy drain refused");
        }
        for (target, intent) in registered.legacy.into_iter().chain(batch.controls) {
            self.dispatch_legacy_control(&target, &intent);
        }
        while let Some(intent) = registered
            .outer
            .pop_front()
            .or_else(|| crate::widget::controls::take_control_invocation(self.root.as_mut()))
        {
            // The remaining controls originated in the outer document. Never
            // reselect a matching component or legacy owner from their address.
            let mut resources = Vec::new();
            crate::bound_dispatch::collect_unbound_resources(
                self.root.as_ref(),
                &self.state.doc,
                &mut resources,
            );
            if tilo_actions::door::resolve(&resources, &intent.address).is_none()
                && let Some(resource) = crate::bound_dispatch::resolve_unbound_resource(
                    self.root.as_ref(),
                    &self.state.doc,
                    &intent.address.resource,
                )
            {
                resources.push(resource);
            }
            let invocation = tilo_actions::InvocationId::new();
            let result = (|| {
                let (arguments, resource, spec) =
                    crate::bound_dispatch::resolve_resource_invocation(
                        &resources,
                        &intent.address,
                        None,
                        Some(&intent.arguments),
                        false,
                    )?;
                if !crate::bound_dispatch::permits_local_action(
                    &self.permissions,
                    &crate::collaboration::local_app_id(),
                    spec,
                ) {
                    return Err(tilo_actions::DoorError::Unknown(intent.address.clone()));
                }
                let encoded = crate::bound_dispatch::encode_resource_action(
                    &resources,
                    &intent.address,
                    arguments.as_ref(),
                    None,
                )?;
                let before = (spec.kind == tilo_actions::OperationKind::Document)
                    .then(|| self.state.doc.get_heads());
                let started = crate::bound_dispatch::execute_resource_operation(
                    self.root.as_mut(),
                    &mut self.state.doc,
                    &self.state.root,
                    &self.action_registry,
                    self.action_transport.as_mut(),
                    crate::bound_dispatch::ResourceOperation {
                        address: &intent.address,
                        resource,
                        spec,
                        encoded,
                        actor: &self.author.0,
                        local_author: &self.author.0,
                        broker_admitted: false,
                        invocation,
                        ambient: None,
                        scope: None,
                    },
                )?;
                if let Some(before) = before {
                    self.state.record_committed_action_authored(
                        before,
                        None,
                        None,
                        Some(self.author.clone()),
                    );
                }
                Ok::<_, tilo_actions::DoorError>(started)
            })();
            crate::widget::controls::on_control_invocation_result(
                self.root.as_mut(),
                &intent.address,
                &result,
            );
            match result {
                Ok(crate::widget::ResourceOperationStart::Accepted) => {
                    self.pending_control_invocations.insert(invocation);
                }
                Ok(crate::widget::ResourceOperationStart::Completed(_)) => {}
                Err(tilo_actions::DoorError::ConfirmationRequired) => self
                    .open_ordinary_confirmation(
                        crate::bound_dispatch::legacy_document::LocalResourceTarget::Outer,
                        &intent,
                    ),
                Err(error) => {
                    tracing::warn!(code = error.code(), %error, "headless: control action refused");
                    self.control_failures.push(error.to_string());
                }
            }
        }
    }

    /// The legacy executor for declarations belonging to the outer document;
    /// bound targets are selected (or refused) before this is reached.
    /// `confirmed` is true only when the caller already completed the
    /// action's confirmation contract.
    fn invoke_unbound(
        &mut self,
        address: &tilo_actions::ActionAddress,
        supplied: &serde_json::Map<String, serde_json::Value>,
        confirmed: bool,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        let mut resources = Vec::new();
        crate::bound_dispatch::collect_unbound_resources(
            self.root.as_ref(),
            &self.state.doc,
            &mut resources,
        );
        if tilo_actions::door::resolve(&resources, address).is_none()
            && let Some(resource) = crate::bound_dispatch::resolve_unbound_resource(
                self.root.as_ref(),
                &self.state.doc,
                &address.resource,
            )
        {
            resources.push(resource);
        }
        let invocation = tilo_actions::InvocationId::new();
        let (arguments, resource, spec) = crate::bound_dispatch::resolve_resource_invocation(
            &resources,
            address,
            None,
            Some(supplied),
            confirmed,
        )?;
        if !crate::bound_dispatch::permits_local_action(
            &self.permissions,
            &crate::collaboration::local_app_id(),
            spec,
        ) {
            return Err(tilo_actions::DoorError::Unknown(address.clone()));
        }
        let encoded = crate::bound_dispatch::encode_resource_action(
            &resources,
            address,
            arguments.as_ref(),
            None,
        )?;
        let before = (spec.kind == tilo_actions::OperationKind::Document)
            .then(|| self.state.doc.get_heads());
        let started = crate::bound_dispatch::execute_resource_operation(
            self.root.as_mut(),
            &mut self.state.doc,
            &self.state.root,
            &self.action_registry,
            self.action_transport.as_mut(),
            crate::bound_dispatch::ResourceOperation {
                address,
                resource,
                spec,
                encoded,
                actor: &self.author.0,
                local_author: &self.author.0,
                broker_admitted: false,
                invocation,
                ambient: None,
                scope: None,
            },
        )?;
        if let Some(before) = before {
            self.state.record_committed_action_authored(
                before,
                None,
                None,
                Some(self.author.clone()),
            );
        }
        if matches!(started, crate::widget::ResourceOperationStart::Accepted) {
            self.pending_control_invocations.insert(invocation);
        }
        Ok(started)
    }

    fn poll_control_invocations(&mut self) {
        let bound = self
            .bound_invocations
            .iter()
            .map(|(id, target)| (*id, target.clone()))
            .collect::<Vec<_>>();
        for (invocation, target) in bound {
            let update = crate::bound_dispatch::poll_resource_operation(
                self.root.as_mut(),
                &self.state.doc,
                &target,
                invocation,
                false,
            );
            match &update {
                Ok(Some(update)) => self.record_control_failure(update),
                Err(error) => self.control_failures.push(format!("{error:?}")),
                Ok(None) => {}
            }
            if update.is_err()
                || update
                    .as_ref()
                    .ok()
                    .and_then(|update| update.as_ref())
                    .is_some_and(tilo_actions::InvocationUpdate::is_terminal)
            {
                self.bound_invocations.remove(&invocation);
            }
        }

        let invocations = self
            .pending_control_invocations
            .iter()
            .copied()
            .collect::<Vec<_>>();
        for invocation in invocations {
            if let Some(update) = self.root.poll_resource_operation(
                invocation,
                false,
                &self.state.doc,
                &self.state.root,
            ) && update.is_terminal()
            {
                self.record_control_failure(&update);
                self.pending_control_invocations.remove(&invocation);
            }
        }
    }

    fn record_control_failure(&mut self, update: &tilo_actions::InvocationUpdate) {
        match update {
            tilo_actions::InvocationUpdate::Failed { error, .. } => {
                self.control_failures.push(error.message.clone());
            }
            tilo_actions::InvocationUpdate::Cancelled { message, .. } => {
                self.control_failures.push(
                    message
                        .clone()
                        .unwrap_or_else(|| "Control operation cancelled".into()),
                );
            }
            _ => {}
        }
    }

    fn dispatch_touch_pointer(
        &mut self,
        device_id: winit::event::DeviceId,
        events: Vec<crate::interaction::TouchPointerEvent>,
    ) {
        let nested = std::mem::replace(&mut self.dispatching_touch_pointer, true);
        for pointer_event in events {
            self.send(&pointer_event.window_event(device_id));
        }
        self.dispatching_touch_pointer = nested;
    }

    /// Whether a laid-out widget claims raw per-finger touch at `point`
    /// (logical coords); see [`Widget::claims_raw_touch`].
    fn claims_raw_touch_at(&self, point: Point) -> bool {
        self.node_map.keys().any(|&node| {
            self.inspect_node(node, |widget| {
                widget.get_rect().contains(point) && widget.claims_raw_touch(point)
            })
            .unwrap_or(false)
        })
    }

    /// Fire a touch long-press whose hold deadline passed at `now`.
    fn poll_touch_hold(&mut self, now: crate::time::Instant) {
        if let Some((device_id, events)) = self.touch_pointer.poll(now) {
            self.dispatch_touch_pointer(device_id, events);
        }
    }

    /// Hold one finger still at logical `(x, y)` past the long-press delay,
    /// then lift it: the touchscreen's secondary click. The hold is resolved
    /// at its deadline instead of waiting for it in real time.
    pub fn touch_hold_at(&mut self, x: f64, y: f64) {
        self.pump_component_wakes();
        self.send_touch_phase(winit::event::TouchPhase::Started, x, y);
        if let Some(deadline) = self.touch_pointer.long_press_deadline() {
            self.poll_touch_hold(deadline);
        }
        self.relayout();
        self.send_touch_phase(winit::event::TouchPhase::Ended, x, y);
    }

    /// Deliver one finger's contact update at logical `(x, y)`; `id`
    /// distinguishes simultaneous fingers.
    pub fn touch(&mut self, id: u64, phase: winit::event::TouchPhase, x: f64, y: f64) {
        self.pump_component_wakes();
        self.send(&WindowEvent::Touch(winit::event::Touch {
            device_id: winit::event::DeviceId::dummy(),
            phase,
            location: winit::dpi::PhysicalPosition::new(x * self.input_scale, y * self.input_scale),
            force: None,
            id,
        }));
    }

    /// Move the pointer to `(x, y)` and press+release the left button — the
    /// three events a click produces, in order.
    pub fn click_at(&mut self, x: f64, y: f64) {
        self.pump_component_wakes();
        self.send(&WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(x * self.input_scale, y * self.input_scale),
        });
        self.send(&WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        });
        self.send(&WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Released,
            button: MouseButton::Left,
        });
    }

    /// Direct-touch activation at logical coordinates.
    pub fn tap_at(&mut self, x: f64, y: f64) {
        self.pump_component_wakes();
        for phase in [
            winit::event::TouchPhase::Started,
            winit::event::TouchPhase::Ended,
        ] {
            self.send_touch_phase(phase, x, y);
        }
    }

    /// Move one direct touch through enough intermediate points for widgets
    /// to receive smooth pixel scroll deltas rather than one synthetic jump.
    pub fn swipe_at(&mut self, start: (f64, f64), end: (f64, f64)) {
        self.pump_component_wakes();
        self.send_touch_phase(winit::event::TouchPhase::Started, start.0, start.1);
        for fraction in [0.25, 0.5, 0.75, 1.0] {
            let x = start.0 + (end.0 - start.0) * fraction;
            let y = start.1 + (end.1 - start.1) * fraction;
            self.send_touch_phase(winit::event::TouchPhase::Moved, x, y);
        }
        self.send_touch_phase(winit::event::TouchPhase::Ended, end.0, end.1);
    }

    fn send_touch_phase(&mut self, phase: winit::event::TouchPhase, x: f64, y: f64) {
        self.touch(0, phase, x, y);
    }

    /// Touch the centre of a named widget.
    ///
    /// # Errors
    ///
    /// Returns an error when `name` is absent or its widget has no tappable
    /// layout rectangle.
    pub fn tap_named(&mut self, name: &str) -> Result<(), String> {
        self.pump_component_wakes();
        let rect = self
            .targets()
            .get(name)
            .copied()
            .ok_or_else(|| format!("no addressable widget named {name:?}"))?;
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return Err(format!("widget {name:?} has an empty rect: {rect:?}"));
        }
        let center = rect.center();
        self.tap_at(center.x, center.y);
        Ok(())
    }

    /// Click the centre of the named widget. `Err` names the target when it
    /// is not in the tree, so a stale scenario fails loudly instead of
    /// quietly doing nothing.
    ///
    /// # Errors
    ///
    /// Returns an error if the widget is not found or has a zero-sized rect.
    pub fn click_named(&mut self, name: &str) -> Result<(), String> {
        self.pump_component_wakes();
        let rect = self
            .targets()
            .get(name)
            .copied()
            .ok_or_else(|| format!("no addressable widget named {name:?}"))?;
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return Err(format!("widget {name:?} has an empty rect: {rect:?}"));
        }
        let c = rect.center();
        self.click_at(c.x, c.y);
        Ok(())
    }

    /// Press and release one key.
    ///
    /// `Tab` drives host focus traversal unless the focused widget explicitly
    /// owns it (for example, a code editor's indentation). This mirrors
    /// [`crate::app::App`]'s live routing; every delivered key then follows
    /// the normal root-widget event path.
    pub fn key(&mut self, spec: &str) {
        // Press and release are one interaction even when the press returns
        // to live content and replaces the chrome before release arrives.
        let framework = crate::history_mode::embedded::keyboard_captured(
            self.root.as_ref(),
            self.focused,
            &self.taffy,
        ) || self.history.as_ref().is_some_and(|history| {
            history.captures_input()
                || self
                    .focused
                    .is_some_and(|node| history.owns_node(node, &self.taffy))
        });
        let _framework_frame = FrameworkInputFrame::new(&self.framework_input, framework);
        self.pump_component_wakes();
        let key = parse_key(spec);
        for state in [ElementState::Pressed, ElementState::Released] {
            self.send_host_event(&key_event(&key, state));
        }
    }

    /// Type a string one character at a time.
    pub fn type_text(&mut self, text: &str) {
        self.pump_component_wakes();
        for ch in text.chars() {
            self.key(&ch.to_string());
        }
    }

    /// Commit text through the production input-method path in one event.
    ///
    /// Use this for scenarios whose subject is the resulting document rather
    /// than physical-key handling. It models a completed input-method
    /// composition without manufacturing a rendered frame for every character.
    pub fn commit_text(&mut self, text: &str) {
        self.pump_component_wakes();
        self.send(&WindowEvent::Ime(winit::event::Ime::Commit(text.into())));
    }

    /// Set the physical-to-logical input scale; layout and capture remain logical pixels.
    /// # Errors
    /// Rejects a non-finite or non-positive scale.
    pub fn set_input_scale(&mut self, scale: f64) -> Result<(), String> {
        if !scale.is_finite() || scale <= 0.0 {
            return Err("input scale must be finite and positive".into());
        }
        self.input_scale = scale;
        Ok(())
    }

    /// What the focused widget publishes as presence (its caret or
    /// selection), for a live host to send to peers.
    pub fn focus_presence(
        &self,
    ) -> Option<(
        crate::presence::PresenceKey,
        crate::collaboration::PresencePayload,
    )> {
        self.focused.and_then(|id| {
            self.inspect_node(id, |widget| {
                crate::presence::capability::presence_payload(widget)
            })
            .ok()
            .flatten()
        })
    }

    /// The collaborator state the draw pass renders (remote carets,
    /// selections, names), for a live host applying peers' presence. Takes
    /// effect in the next relayout.
    pub fn presence_mut(&mut self) -> &mut PresenceCtx {
        &mut self.presence
    }

    /// Replace the host-projected collaborator state used by the real draw pass.
    /// Capture and integration hosts use this to exercise the same presence
    /// rendering that a live broker frame supplies, without a broker process.
    pub fn set_presence_context(&mut self, presence: PresenceCtx) {
        self.presence = presence;
        self.relayout();
    }

    /// Mutate the collaborator presence read by the draw pass. When `update`
    /// reports a change the next scheduled frame redraws; unlike
    /// [`Self::set_presence_context`] this never runs a synchronous relayout,
    /// so live remote carets cost one ordinary frame at display cadence.
    pub fn update_presence(&mut self, update: impl FnOnce(&mut PresenceCtx) -> bool) -> bool {
        let changed = update(&mut self.presence);
        self.presence_redraw |= changed;
        changed
    }

    /// Read-only view of the collaborator presence drawn by this harness.
    pub fn presence(&self) -> &PresenceCtx {
        &self.presence
    }

    /// Outbound caret/selection presence of the widget holding keyboard
    /// focus (the same poll [`crate::app::App`] broadcasts to peers).
    pub fn focus_presence_payload(
        &self,
    ) -> Option<(
        crate::presence::PresenceKey,
        crate::collaboration::PresencePayload,
    )> {
        let node = self.focused?;
        self.inspect_node(node, |widget| {
            crate::presence::capability::presence_payload(widget)
        })
        .ok()
        .flatten()
    }

    /// Send wheel line-deltas at the last pointer position. Move the pointer
    /// inside the intended content viewport first, as a live user would.
    pub fn scroll(&mut self, dx: f64, dy: f64) {
        self.pump_component_wakes();
        // Deltas are line counts chosen by the driver's own caller (test
        // code or an agent), always small integers; there is no fallible
        // f64->f32 conversion to fall back to, and winit's own delta type
        // is f32.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "line-delta counts are small integers; winit's `LineDelta` is f32 and there is no fallible f64->f32 conversion"
        )]
        let (dx, dy) = (dx as f32, dy as f32);
        self.send(&WindowEvent::MouseWheel {
            device_id: winit::event::DeviceId::dummy(),
            delta: winit::event::MouseScrollDelta::LineDelta(dx, dy),
            phase: winit::event::TouchPhase::Moved,
        });
    }

    // -- Perf harness plumbing (see [`crate::profiling`]) ------------------

    /// Install a capture profiler so [`Self::relayout`] / [`Self::send`] time
    /// each pass and commit a per-frame sample. Called by
    /// [`crate::profiling::run_scenario`].
    pub fn begin_profiling(&mut self) {
        self.pump_component_wakes();
        self.profiler = Some(crate::profiling::FrameProfiler::for_capture());
    }

    /// The root's native input diagnostic, as the native runner samples it
    /// (see [`crate::widget::Widget::native_input_snapshot`]).
    pub fn native_input_snapshot(&self) -> Option<serde_json::Value> {
        self.root
            .native_input_snapshot(&self.state.doc, &self.state.root)
    }

    /// Stop per-pass timing; subsequent frames pay nothing again.
    pub fn end_profiling(&mut self) {
        self.profiler = None;
    }

    // -- Retained relayout diagnostics -------------------------------------

    /// Whether this frame must rebuild the layout tree regardless of what the
    /// widgets report: the test override first, then `TILO_UI_FULL_RELAYOUT`.
    fn full_relayout_forced_now(&self) -> bool {
        self.full_relayout_override
            .unwrap_or_else(full_relayout_forced)
    }

    /// Force (`Some(true)`) or forbid (`Some(false)`) the full-rebuild path
    /// for every following frame, overriding `TILO_UI_FULL_RELAYOUT`; `None`
    /// returns to the environment's choice. Benchmarks use this to compare
    /// both paths inside one process.
    pub fn set_full_relayout(&mut self, forced: Option<bool>) {
        self.full_relayout_override = forced;
    }

    /// What the most recent completed relayout did with its layout tree.
    pub fn last_relayout(&self) -> RelayoutStats {
        self.last_relayout
    }

    /// Completed relayout frames since construction; unchanged across a
    /// skipped (inert pointer) `send`.
    pub fn relayout_frames(&self) -> u64 {
        self.relayout_frames
    }

    /// Completed frames that recomposed the layout tree. One input may
    /// produce several frames (press and release, a commit tick), so callers
    /// asserting on a structural change compare this across the input.
    pub fn rebuilt_frames(&self) -> u64 {
        self.relayout_frames.saturating_sub(self.retained_frames)
    }

    /// Every taffy node of the current composition, sorted. Equal across two
    /// frames exactly when the tree was retained rather than rebuilt.
    pub fn layout_node_ids(&self) -> Vec<u64> {
        let mut ids: Vec<u64> = self.node_map.keys().map(|node| u64::from(*node)).collect();
        ids.sort_unstable();
        ids
    }

    /// Invalidate taffy's cached measurement for every node whose widget
    /// reports it may have changed size (`is_layout_dirty`) or must be
    /// re-solved (`layout_needs_measure`), so the following
    /// `compute_widget_layout` re-measures exactly those and their ancestors.
    /// A node that cannot be inspected is invalidated too: stale is worse than
    /// a redundant measure. Returns `(marked nodes, marked childless nodes)`.
    fn mark_layout_dirty_nodes(&mut self) -> (usize, usize) {
        let dirty: Vec<NodeId> = self
            .node_map
            .keys()
            .copied()
            .filter(|&node| {
                // SAFETY: these maps describe this exclusive runner's current
                // admitted tree; the callback only reads widget flags.
                unsafe {
                    crate::bound_dispatch::inspect_bound_node(
                        &self.bindings,
                        &self.node_map,
                        node,
                        |widget| widget.is_layout_dirty() || widget.layout_needs_measure(),
                    )
                }
                .unwrap_or(true)
            })
            .collect();
        let mut leaves = 0usize;
        for node in &dirty {
            if self
                .taffy
                .children(*node)
                .is_ok_and(|children| children.is_empty())
            {
                leaves = leaves.saturating_add(1);
            }
            let _ = self.taffy.mark_dirty(*node);
        }
        (dirty.len(), leaves)
    }

    /// Compare the retained composition's geometry against a fresh rebuild of
    /// the same state. Runs one forced full relayout, so it is a test and
    /// diagnostics hook, not something a live host calls per frame.
    ///
    /// # Errors
    /// Describes the first node (in traversal order) whose child count,
    /// location or size differs, or the layout error of the rebuild.
    pub fn verify_retained_geometry(&mut self) -> Result<(), String> {
        use crate::widget::layout::composition_geometry;
        if !self.layout_built {
            return Ok(());
        }
        let retained = composition_geometry(&self.taffy, self.root_node)
            .map_err(|error| format!("retained geometry unavailable: {error}"))?;
        // The verification frame is not one the caller asked for: keep the
        // diagnostics describing the frame under test.
        let (stats, frames, retained_frames) = (
            self.last_relayout,
            self.relayout_frames,
            self.retained_frames,
        );
        let previous = self.full_relayout_override;
        self.full_relayout_override = Some(true);
        self.verifying_geometry = true;
        let rebuilt = self.try_relayout();
        self.verifying_geometry = false;
        self.full_relayout_override = previous;
        self.last_relayout = stats;
        self.relayout_frames = frames;
        self.retained_frames = retained_frames;
        rebuilt.map_err(|error| format!("fresh rebuild did not settle: {error}"))?;
        let fresh = composition_geometry(&self.taffy, self.root_node)
            .map_err(|error| format!("fresh geometry unavailable: {error}"))?;
        if retained.len() != fresh.len() {
            return Err(format!(
                "retained composition has {} nodes, a fresh rebuild {}",
                retained.len(),
                fresh.len()
            ));
        }
        for (index, (kept, built)) in retained.iter().zip(&fresh).enumerate() {
            if kept != built {
                return Err(format!(
                    "node {index} of {}: retained (children {}, at {:?}, size {:?}) but rebuilt (children {}, at {:?}, size {:?})",
                    fresh.len(),
                    kept.0,
                    kept.1,
                    kept.2,
                    built.0,
                    built.1,
                    built.2
                ));
            }
        }
        Ok(())
    }

    /// Debug builds only: after every `every`th retained frame, verify the
    /// retained geometry against a fresh rebuild and panic on a mismatch.
    /// `0` disables the check (the default).
    pub fn set_retained_geometry_check(&mut self, every: u32) {
        self.retained_geometry_check_every = if cfg!(debug_assertions) { every } else { 0 };
    }

    fn check_retained_geometry_if_due(&mut self) {
        if self.retained_geometry_check_every == 0
            || self.verifying_geometry
            || self.last_relayout.rebuilt
            || !self
                .retained_frames
                .is_multiple_of(u64::from(self.retained_geometry_check_every))
        {
            return;
        }
        if let Err(mismatch) = self.verify_retained_geometry() {
            tracing::error!(%mismatch, "headless: retained layout diverged from a fresh rebuild");
            debug_assert!(
                false,
                "retained layout diverged from a fresh rebuild: {mismatch}"
            );
        }
    }

    /// Render one frame with no input and record it. A frame here is one
    /// `relayout`, which runs the sync → build → apply → draw passes.
    pub fn profile_frame(&mut self) {
        self.pump_component_wakes();
        self.relayout();
    }

    /// Discard captured frames (e.g. warmup) without stopping capture.
    pub fn clear_profile_samples(&mut self) {
        self.pump_component_wakes();
        if let Some(p) = self.profiler.as_mut() {
            p.clear_capture();
        }
    }

    /// Take every captured frame so far, draining the buffer.
    pub fn take_profile_samples(&mut self) -> Vec<crate::profiling::FrameTiming> {
        self.pump_component_wakes();
        self.profiler
            .as_mut()
            .map(super::profiling::FrameProfiler::take_capture)
            .unwrap_or_default()
    }

    /// Set the held modifier set. Names: `ctrl`, `shift`, `alt`, `logo`.
    pub fn set_modifiers(&mut self, names: &[String]) {
        self.pump_component_wakes();
        let mut st = winit::keyboard::ModifiersState::empty();
        for n in names {
            match n.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => st |= winit::keyboard::ModifiersState::CONTROL,
                "shift" => st |= winit::keyboard::ModifiersState::SHIFT,
                "alt" => st |= winit::keyboard::ModifiersState::ALT,
                "logo" | "super" | "meta" => st |= winit::keyboard::ModifiersState::SUPER,
                _ => {}
            }
        }
        self.modifiers = winit::event::Modifiers::from(st);
        self.send(&WindowEvent::ModifiersChanged(self.modifiers));
    }

    /// Run one action.
    ///
    /// # Errors
    ///
    /// Returns an error if the action cannot be performed.
    pub fn perform(&mut self, action: &Action) -> Result<(), String> {
        self.pump_component_wakes();
        match action {
            Action::ClickNamed(name) => self.click_named(name),
            Action::ClickAt(x, y) => {
                self.click_at(*x, *y);
                Ok(())
            }
            Action::TapNamed(name) => self.tap_named(name),
            Action::TapAt(x, y) => {
                self.tap_at(*x, *y);
                Ok(())
            }
            Action::SwipeAt(x0, y0, x1, y1) => {
                self.swipe_at((*x0, *y0), (*x1, *y1));
                Ok(())
            }
            Action::Key(k) => {
                self.key(k);
                Ok(())
            }
            Action::Type(t) => {
                self.type_text(t);
                Ok(())
            }
            Action::Modifiers(m) => {
                self.set_modifiers(m);
                Ok(())
            }
            Action::WaitForNamed { target, timeout } => self.wait_for_named(target, *timeout),
            Action::Expect(expectation) => self.expect(expectation),
            Action::Perform {
                target,
                semantic_target,
                action,
            } => self.perform_semantic_named(target, semantic_target.as_deref(), action),
            Action::Invoke(invocation) => self.perform_invocation(invocation),
        }
    }

    fn perform_invocation(&mut self, invocation: &Invocation) -> Result<(), String> {
        let result = self.invoke_action(
            &invocation.address,
            &invocation.arguments,
            invocation.confirm,
            invocation.confirmation_phrase.as_deref(),
        );
        match (result, invocation.expect_error.as_deref()) {
            (Ok(_), None) => Ok(()),
            (Ok(_), Some(code)) => Err(format!(
                "expected the door to refuse with {code}, but the action ran"
            )),
            (Err(error), Some(code)) if error.code() == code => Ok(()),
            (Err(error), _) => Err(format!("{}: {error}", error.code())),
        }
    }

    /// Invoke one published action by address, the way an agent, a script
    /// or a screen reader does: resolve it against the tree the app
    /// publishes now, refuse it when unavailable, require the confirmation
    /// its contract names (`confirm` for `Confirm`, the exact phrase for
    /// `Phrase`), bind the arguments strictly, and run it through the host's
    /// own executor. Then run the same host tick a real input event gets.
    ///
    /// # Errors
    ///
    /// Returns the door's refusal: unknown or hidden address, unavailable
    /// action, missing confirmation, invalid arguments, or a failed run.
    pub fn invoke_action(
        &mut self,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirm: bool,
        confirmation_phrase: Option<&str>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        self.pump_component_wakes();
        let resources = self.resources();
        let spec = tilo_actions::door::resolve(&resources, address)
            .ok_or_else(|| tilo_actions::DoorError::Unknown(address.clone()))?;
        if let tilo_actions::Availability::Unavailable { code, reason } =
            &spec.contract.availability
        {
            return Err(tilo_actions::DoorError::Unavailable {
                code: code.clone(),
                reason: reason.clone(),
            });
        }
        let confirmed = match &spec.confirmation_for_json(arguments) {
            tilo_actions::ConfirmationPolicy::None => false,
            tilo_actions::ConfirmationPolicy::Confirm if confirm => true,
            tilo_actions::ConfirmationPolicy::Phrase { phrase }
                if confirmation_phrase == Some(phrase.as_str()) =>
            {
                true
            }
            tilo_actions::ConfirmationPolicy::Confirm
            | tilo_actions::ConfirmationPolicy::Phrase { .. } => {
                return Err(tilo_actions::DoorError::ConfirmationRequired);
            }
        };
        let result = if controls::is_control_operation(spec) {
            self.invoke_control_action(address, arguments)
        } else {
            self.invoke_address(address, arguments, confirmed)
        };
        self.host_tick();
        self.relayout();
        result
    }

    /// The instance id of the hosted application component, the
    /// `instance_id` its bound actions are addressed by. `None` when the root
    /// is not a component.
    pub fn component_instance_id(&self) -> Option<crate::application::InstanceId> {
        self.root
            .callback_binding()
            .map(|binding| binding.instance_id)
    }

    /// Admit an agent task participant into the hosted component under
    /// `label`, its display name, as a host worker does before the task's
    /// first invocation (`HostWorker::join_participant`). A host whose own author
    /// is that participant makes the component act as the agent. Idempotent.
    pub fn join_participant(&mut self, participant: &tilo_actions::ParticipantId, label: &str) {
        crate::collaboration::participants::join_participant(self.root.as_mut(), participant);
        crate::collaboration::participants::set_participant_label(
            self.root.as_mut(),
            participant,
            label,
        );
    }

    /// The scene the last laid-out frame encoded: what [`Self::render_rgba`]
    /// rasterises. Its encoding is exact where pixels are not, so a capture's
    /// determinism is asserted on it.
    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    /// Reach the first retained widget of type `T` — found through
    /// [`Widget::as_any_mut`], depth first along `children_mut`, into hosted
    /// components — run `visit` on it, then run one input-free frame (the
    /// host tick and a relayout) so whatever it queued is handled as after an
    /// event. `None` when no widget of that type opts in to `as_any_mut`.
    ///
    /// This is how a screenshot scenario stages view state (a selection, an
    /// open dialog) in the widget that owns it, instead of reaching around it.
    pub fn with_widget_mut<T: 'static, O>(&mut self, visit: impl FnOnce(&mut T) -> O) -> Option<O> {
        fn find<T: 'static>(widget: &mut dyn Widget) -> Option<&mut T> {
            if widget.as_any_mut().is_some_and(|any| any.is::<T>()) {
                return widget.as_any_mut()?.downcast_mut::<T>();
            }
            widget.children_mut().into_iter().find_map(find::<T>)
        }
        self.pump_component_wakes();
        let output = visit(find::<T>(self.root.as_mut())?);
        self.relayout();
        self.host_tick();
        self.relayout();
        Some(output)
    }

    /// Deliver an out-of-band payload the way the live host delivers a
    /// [`crate::app::UserEvent::External`] — a worker's progress line, or a
    /// wake-up for requests the app queued itself — then run the host tick.
    pub fn deliver_external(&mut self, payload: &str) {
        self.pump_component_wakes();
        self.root
            .on_external_event(payload, &mut self.state.doc, &self.state.root);
        self.host_tick();
        self.relayout();
    }

    fn invoke_address(
        &mut self,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirmed: bool,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        let selected = crate::bound_dispatch::discover_bound_resources(
            self.root.as_ref(),
            &self.state.doc,
            Some(address),
        )
        .map_err(crate::bound_dispatch::BoundActionError::Dispatch)
        .and_then(|snapshots| crate::bound_dispatch::select_bound_target(&snapshots, address, None))
        .map_err(crate::bound_dispatch::BoundActionError::into_door)?;
        let Some(target) = selected else {
            return self.invoke_unbound(address, arguments, confirmed);
        };
        let invocation = tilo_actions::InvocationId::new();
        let started = crate::bound_dispatch::invoke_resource_action(
            self.root.as_mut(),
            &mut self.state.doc,
            &target,
            self.action_transport.as_mut(),
            crate::bound_dispatch::ResourceActionRequest {
                argument: None,
                arguments: Some(arguments),
                actor: &self.author.0,
                confirmed,
                broker_admitted: false,
                invocation,
                ambient: None,
                scope: None,
            },
        )
        .map_err(crate::bound_dispatch::BoundActionError::into_door)?;
        if matches!(started, crate::widget::ResourceOperationStart::Accepted) {
            self.bound_invocations.insert(invocation, target);
        }
        Ok(started)
    }

    /// The document rendered as JSON, for a harness to diff across a
    /// restart.
    pub fn dump_json(&mut self) -> String {
        self.pump_component_wakes();
        if let Some(wake) = &self.component_wake {
            return wake
                .binding
                .records
                .read(&wake.binding.checkout.id, |doc| {
                    serde_json::to_string_pretty(&automerge::AutoSerde::from(doc))
                        .unwrap_or_else(|error| format!("{{\"error\":\"{error}\"}}"))
                })
                .unwrap_or_else(|| "{\"error\":\"canonical record unavailable\"}".into());
        }
        serde_json::to_string_pretty(&automerge::AutoSerde::from(self.state.doc.document()))
            .unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
    }
}

fn find_semantic_target<'a>(node: &'a AccessNode, target: &str) -> Option<&'a AccessNode> {
    if node.action_target.as_deref() == Some(target) {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| find_semantic_target(child, target))
}

fn compare_text(
    mismatches: &mut Vec<String>,
    field: &str,
    expected: Option<&str>,
    actual: Option<&str>,
) {
    if let Some(expected) = expected
        && actual != Some(expected)
    {
        mismatches.push(format!("{field} expected {expected:?}, got {actual:?}"));
    }
}

fn compare_contains(
    mismatches: &mut Vec<String>,
    field: &str,
    expected: Option<&str>,
    actual: Option<&str>,
) {
    if let Some(expected) = expected
        && !actual.is_some_and(|actual| actual.contains(expected))
    {
        mismatches.push(format!(
            "{field} expected to contain {expected:?}, got {actual:?}"
        ));
    }
}

fn compare_bool(mismatches: &mut Vec<String>, field: &str, expected: Option<bool>, actual: bool) {
    if let Some(expected) = expected
        && expected != actual
    {
        mismatches.push(format!("{field} expected {expected}, got {actual}"));
    }
}

fn compare_optional_bool(
    mismatches: &mut Vec<String>,
    field: &str,
    expected: Option<bool>,
    actual: Option<bool>,
) {
    if let Some(expected) = expected
        && actual != Some(expected)
    {
        mismatches.push(format!("{field} expected {expected}, got {actual:?}"));
    }
}

fn role_name(role: crate::a11y::Role) -> &'static str {
    use crate::a11y::Role;
    match role {
        Role::Window => "window",
        Role::Dialog => "dialog",
        Role::Group => "group",
        Role::Generic => "generic",
        Role::Toolbar => "toolbar",
        Role::TabBar => "tab_bar",
        Role::Tab => "tab",
        Role::TabPanel => "tab_panel",
        Role::List => "list",
        Role::ListItem => "list_item",
        Role::Table => "table",
        Role::Row => "row",
        Role::Cell => "cell",
        Role::Tree => "tree",
        Role::TreeItem => "tree_item",
        Role::Menu => "menu",
        Role::MenuItem => "menu_item",
        Role::Button => "button",
        Role::ToggleButton => "toggle_button",
        Role::Checkbox => "checkbox",
        Role::RadioButton => "radio_button",
        Role::Switch => "switch",
        Role::Slider => "slider",
        Role::SpinButton => "spin_button",
        Role::ComboBox => "combo_box",
        Role::TextInput => "text_input",
        Role::MultilineTextInput => "multiline_text_input",
        Role::Link => "link",
        Role::ProgressBar => "progress_bar",
        Role::Spinner => "spinner",
        Role::Tooltip => "tooltip",
        Role::Image => "image",
        Role::Label => "label",
        Role::StaticText => "static_text",
        Role::Heading { .. } => "heading",
        Role::Paragraph => "paragraph",
        Role::Document => "document",
        Role::Terminal => "terminal",
        Role::Canvas => "canvas",
        Role::Unknown => "unknown",
    }
}

fn collect_targets(
    w: &dyn Widget,
    parent_path: &str,
    sibling_counts: &mut HashMap<String, usize>,
    out: &mut HashMap<String, Rect>,
) {
    let Ok(_entry) = crate::bound_dispatch::enter_widget(w) else {
        return;
    };
    let (name, path, my_index) = next_widget_path(w, parent_path, sibling_counts);
    out.insert(path.clone(), w.get_rect());
    if my_index == 0 {
        let bare = if parent_path.is_empty() {
            name
        } else {
            format!("{parent_path}/{name}")
        };
        out.entry(bare).or_insert_with(|| w.get_rect());
    }
    if let Some(id) = w.test_id() {
        // A widget whose own node names narrower bounds (an empty state's
        // button inside its panel) is operated where it says it is, as
        // assistive technology would.
        let rect = w
            .accessibility()
            .and_then(|node| node.bounds)
            .map(crate::a11y::AccessBounds::to_rect)
            .filter(|bounds| bounds.width() > 0.0 && bounds.height() > 0.0)
            .unwrap_or_else(|| w.get_rect());
        out.insert(id.to_string(), rect);
    }
    let mut child_counts: HashMap<String, usize> = HashMap::new();
    for child in w.children() {
        collect_targets(child, &path, &mut child_counts, out);
    }
}

struct RetainedNamedTarget {
    key: (usize, String),
    path: String,
    bare: Option<String>,
    id: Option<String>,
}

fn collect_named_targets(
    widget: &dyn Widget,
    parent_path: &str,
    sibling_counts: &mut HashMap<String, usize>,
    out: &mut Vec<RetainedNamedTarget>,
) {
    let Ok(_entry) = crate::bound_dispatch::enter_widget(widget) else {
        return;
    };
    let (name, path, my_index) = next_widget_path(widget, parent_path, sibling_counts);
    // Include even unregistered wrappers: otherwise their same-name inline
    // child could be mistaken for them by a registration-only uniqueness check.
    let bare = (my_index == 0).then(|| {
        if parent_path.is_empty() {
            name.clone()
        } else {
            format!("{parent_path}/{name}")
        }
    });
    out.push(RetainedNamedTarget {
        key: (std::ptr::from_ref(widget).cast::<()>() as usize, name),
        path: path.clone(),
        bare,
        id: widget.test_id().map(str::to_owned),
    });
    let mut child_counts = HashMap::new();
    for child in widget.children() {
        collect_named_targets(child, &path, &mut child_counts, out);
    }
}

fn collect_inspection(
    widget: &dyn Widget,
    parent_path: &str,
    sibling_counts: &mut HashMap<String, usize>,
) -> InspectionNode {
    let Ok(_entry) = crate::bound_dispatch::enter_widget(widget) else {
        return InspectionNode {
            path: parent_path.into(),
            widget: "ClosedComponent".into(),
            test_id: None,
            focusable: false,
            rect: [0.0; 4],
            child_clip: None,
            layout: None,
            semantics: None,
            children: vec![],
        };
    };
    let (name, path, _) = next_widget_path(widget, parent_path, sibling_counts);
    let rect = widget.get_rect();
    let mut child_counts = HashMap::new();
    let children = widget
        .children()
        .into_iter()
        .map(|child| collect_inspection(child, &path, &mut child_counts))
        .collect();
    InspectionNode {
        path,
        widget: name,
        test_id: widget.test_id().map(str::to_owned),
        focusable: widget.is_focusable(),
        rect: [rect.x0, rect.y0, rect.width(), rect.height()],
        child_clip: widget
            .child_clip_rect()
            .map(|r| [r.x0, r.y0, r.width(), r.height()]),
        layout: widget.layout_info(),
        semantics: widget.accessibility(),
        children,
    }
}

fn audit_inspection_node(node: &InspectionNode, issues: &mut Vec<AccessibilityIssue>) {
    if node.focusable && node.semantics.is_none() {
        push_audit_issue(
            issues,
            &node.path,
            "missing-semantics",
            "a keyboard-focusable widget must expose accessibility semantics",
        );
    }
    if let Some(semantics) = &node.semantics {
        audit_access_node(semantics, &node.path, issues);
    }
    for child in &node.children {
        audit_inspection_node(child, issues);
    }
}

fn effect_control_ids(node: &InspectionNode) -> HashSet<String> {
    let mut controls = HashSet::new();
    if let Some(semantics) = &node.semantics {
        collect_effect_access_nodes(
            semantics,
            node.test_id.as_deref(),
            &node.path,
            &mut controls,
        );
    }
    for child in &node.children {
        controls.extend(effect_control_ids(child));
    }
    controls
}

fn inspection_has_semantics(node: &InspectionNode) -> bool {
    node.semantics.is_some() || node.children.iter().any(inspection_has_semantics)
}

fn collect_effect_access_nodes(
    node: &AccessNode,
    retained_id: Option<&str>,
    path: &str,
    controls: &mut HashSet<String>,
) {
    let effect = matches!(
        node.role,
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
    );
    if effect {
        let identity = node
            .action_target
            .clone()
            .or_else(|| retained_id.map(str::to_owned))
            .or_else(|| node.semantic_key.clone())
            .unwrap_or_else(|| format!("{path}/<unaddressable-{:?}>", node.role));
        controls.insert(identity);
    }
    for (index, child) in node.children.iter().enumerate() {
        collect_effect_access_nodes(child, None, &format!("{path}/semantic[{index}]"), controls);
    }
}

fn audit_access_node(node: &AccessNode, path: &str, issues: &mut Vec<AccessibilityIssue>) {
    audit_access_node_contract(node, path, issues);
    audit_access_children(node, path, issues);
}

fn audit_access_node_contract(node: &AccessNode, path: &str, issues: &mut Vec<AccessibilityIssue>) {
    let has_name = node
        .name
        .as_deref()
        .is_some_and(|name| !name.trim().is_empty());
    if semantic_role_requires_name(node.role) && !has_name {
        push_audit_issue(
            issues,
            path,
            "missing-name",
            format!(
                "{} requires a non-empty accessible name",
                role_name(node.role)
            ),
        );
    }
    if node.state.focusable && !has_accessible_content_name(node) {
        push_audit_issue(
            issues,
            path,
            "unnamed-focus-target",
            "a keyboard-focusable semantic node requires a non-empty accessible name",
        );
    }
    if node.role == Role::Unknown {
        push_audit_issue(
            issues,
            path,
            "unknown-role",
            "replace Role::Unknown with the semantic role exposed to assistive technology",
        );
    }
    if node.state.protected && node.value.is_some() {
        push_audit_issue(
            issues,
            path,
            "protected-value-exposed",
            "a protected control must not expose its value",
        );
    }
    match (node.pos_in_set, node.set_size) {
        (Some(position), Some(size)) if size == 0 || position == 0 || position > size => {
            push_audit_issue(
                issues,
                path,
                "invalid-set-position",
                format!("pos_in_set {position} must be within 1..={size}"),
            );
        }
        (Some(_), None) | (None, Some(_)) => push_audit_issue(
            issues,
            path,
            "incomplete-set-position",
            "pos_in_set and set_size must be supplied together",
        ),
        _ => {}
    }
    if node.level == Some(0) {
        push_audit_issue(
            issues,
            path,
            "invalid-level",
            "semantic hierarchy levels are one-based",
        );
    }
    if node.action_target.as_deref() == Some("") {
        push_audit_issue(
            issues,
            path,
            "empty-action-target",
            "synthetic action targets must be non-empty stable model identities",
        );
    }
}

fn audit_access_children(node: &AccessNode, path: &str, issues: &mut Vec<AccessibilityIssue>) {
    let keyed = node
        .children
        .iter()
        .filter(|child| child.key.is_some())
        .count();
    if keyed > 0 && keyed < node.children.len() {
        push_audit_issue(
            issues,
            path,
            "mixed-synthetic-keys",
            "synthetic siblings must be either all keyed or all position-addressed",
        );
    }
    let mut keys = HashSet::new();
    let mut targets = HashSet::new();
    for (index, child) in node.children.iter().enumerate() {
        let child_path = semantic_child_path(path, child, index);
        if let Some(key) = child.key
            && !keys.insert(key)
        {
            push_audit_issue(
                issues,
                &child_path,
                "duplicate-synthetic-key",
                format!("synthetic sibling key {key} is not unique"),
            );
        }
        if let Some(target) = child.action_target.as_deref()
            && !target.is_empty()
            && !targets.insert(target)
        {
            push_audit_issue(
                issues,
                &child_path,
                "duplicate-action-target",
                format!("synthetic sibling action target {target:?} is not unique"),
            );
        }
        audit_access_node(child, &child_path, issues);
    }
}

fn has_accessible_content_name(node: &AccessNode) -> bool {
    node.name
        .as_deref()
        .is_some_and(|name| !name.trim().is_empty())
        || node.children.iter().any(has_accessible_content_name)
}

fn semantic_role_requires_name(role: Role) -> bool {
    matches!(
        role,
        Role::Tab
            | Role::MenuItem
            | Role::Button
            | Role::ToggleButton
            | Role::Checkbox
            | Role::RadioButton
            | Role::Switch
            | Role::Slider
            | Role::SpinButton
            | Role::ComboBox
            | Role::TextInput
            | Role::MultilineTextInput
            | Role::Link
            | Role::Image
    )
}

fn semantic_child_path(parent: &str, child: &AccessNode, index: usize) -> String {
    if let Some(target) = child.action_target.as_deref() {
        format!("{parent}/semantic[{target:?}]")
    } else if let Some(key) = child.key {
        format!("{parent}/semantic[key={key}]")
    } else {
        format!("{parent}/semantic[{index}]")
    }
}

fn push_audit_issue(
    issues: &mut Vec<AccessibilityIssue>,
    path: &str,
    code: &str,
    message: impl Into<String>,
) {
    issues.push(AccessibilityIssue {
        path: path.to_owned(),
        code: code.to_owned(),
        message: message.into(),
    });
}

fn next_widget_path(
    widget: &dyn Widget,
    parent_path: &str,
    sibling_counts: &mut HashMap<String, usize>,
) -> (String, String, usize) {
    let name = widget.name().to_string();
    let sibling_key = format!("{parent_path}::{name}");
    let index = sibling_counts.entry(sibling_key).or_insert(0);
    let current = *index;
    // Bounded by siblings under one parent; saturating makes that explicit.
    *index = index.saturating_add(1);
    let path = if parent_path.is_empty() {
        format!("{name}[{current}]")
    } else {
        format!("{parent_path}/{name}[{current}]")
    };
    (name, path, current)
}

fn parse_key(spec: &str) -> Key {
    match spec {
        "Enter" | "Return" => Key::Named(NamedKey::Enter),
        "Tab" => Key::Named(NamedKey::Tab),
        "Escape" | "Esc" => Key::Named(NamedKey::Escape),
        "Space" => Key::Named(NamedKey::Space),
        "Backspace" => Key::Named(NamedKey::Backspace),
        "Delete" => Key::Named(NamedKey::Delete),
        "ArrowUp" | "Up" => Key::Named(NamedKey::ArrowUp),
        "ArrowDown" | "Down" => Key::Named(NamedKey::ArrowDown),
        "ArrowLeft" | "Left" => Key::Named(NamedKey::ArrowLeft),
        "ArrowRight" | "Right" => Key::Named(NamedKey::ArrowRight),
        "F1" => Key::Named(NamedKey::F1),
        "F2" => Key::Named(NamedKey::F2),
        "F3" => Key::Named(NamedKey::F3),
        "F4" => Key::Named(NamedKey::F4),
        "F5" => Key::Named(NamedKey::F5),
        "F6" => Key::Named(NamedKey::F6),
        "F7" => Key::Named(NamedKey::F7),
        "F8" => Key::Named(NamedKey::F8),
        "F9" => Key::Named(NamedKey::F9),
        "F10" => Key::Named(NamedKey::F10),
        "F11" => Key::Named(NamedKey::F11),
        "F12" => Key::Named(NamedKey::F12),
        "Home" => Key::Named(NamedKey::Home),
        "End" => Key::Named(NamedKey::End),
        other => Key::Character(other.into()),
    }
}

/// Parse `--headless-drive '<json>'` (or `--headless-drive=<json>`) out of
/// argv. `None` when the flag is absent, which is every normal launch.
///
/// The JSON is an object: `viewport` (`[w, h]`), `actions` (a list of
/// one-key objects — `{"click_named": "..."}`, `{"click_at": [x, y]}`,
/// `{"tap_named": "..."}`, `{"tap_at": [x, y]}`,
/// `{"swipe_at": [x0, y0, x1, y1]}`,
/// `{"key": "Enter"}`, `{"type": "hello"}`, `{"modifiers": ["ctrl"]}`,
/// `{"wait_for_named":{"target":"result","timeout_ms":1000}}`,
/// `{"expect":{"target":"result","name_contains":"Done"}}`,
/// `{"perform":{"target":"name","action":"activate"}}`),
/// `audit` (fail on semantic accessibility violations), `audit_report` (path
/// for its versioned JSON result), `dump` (path for document JSON), `targets`
/// (path for the addressable-widget list), and `inspect` (path for the combined
/// widget/semantic tree).
// Every app's `main` calls this before building any window state (see the
// module doc), so exiting on a malformed `--headless-drive` payload is the
// same "fail before anything is built" contract a CLI arg parser has —
// there is nothing yet to unwind. Threading a `Result` through instead
// would mean every one of the ~15 call sites across `apps/*` duplicates
// this same exit, for no behavioural change.
#[allow(
    clippy::exit,
    reason = "arg-parsing entry point mirrors a CLI flag parser: fails fast before any window/Drop state exists, exactly like the callers already do via `std::process::exit(tilo_ui::headless::run(..))`"
)]
pub fn spec_from_args(args: &[String]) -> Option<Spec> {
    let raw = value_arg(args, "--headless-drive")?;
    match parse_spec(&raw) {
        Ok(spec) => Some(spec),
        Err(e) => {
            eprintln!("--headless-drive: {e}");
            std::process::exit(2);
        }
    }
}

fn value_arg(args: &[String], flag: &str) -> Option<String> {
    let mut it = args.iter().skip(1);
    let eq = format!("{flag}=");
    while let Some(a) = it.next() {
        if a == flag {
            return it.next().cloned();
        }
        if let Some(rest) = a.strip_prefix(&eq) {
            return Some(rest.to_string());
        }
    }
    None
}

/// Parse the `--headless-drive` payload.
///
/// # Errors
///
/// Returns an error if the JSON is invalid or does not conform to the expected structure.
pub fn parse_spec(raw: &str) -> Result<Spec, String> {
    let wire = tilo_headless_schema::parse_spec(raw)?;
    Ok(Spec {
        viewport: wire.viewport,
        actions: wire.actions.into_iter().map(action_from_wire).collect(),
        audit: wire.audit,
        audit_report: wire.audit_report.map(Into::into),
        dump: wire.dump.map(Into::into),
        targets: wire.targets.map(Into::into),
        inspect: wire.inspect.map(Into::into),
    })
}

fn action_from_wire(action: tilo_headless_schema::Action) -> Action {
    use tilo_headless_schema::Action as Wire;
    match action {
        Wire::ClickNamed(target) => Action::ClickNamed(target),
        Wire::ClickAt(x, y) => Action::ClickAt(x, y),
        Wire::TapNamed(target) => Action::TapNamed(target),
        Wire::TapAt(x, y) => Action::TapAt(x, y),
        Wire::SwipeAt(x0, y0, x1, y1) => Action::SwipeAt(x0, y0, x1, y1),
        Wire::Key(key) => Action::Key(key),
        Wire::Type(text) => Action::Type(text),
        Wire::Modifiers(modifiers) => Action::Modifiers(modifiers),
        Wire::WaitForNamed { target, timeout_ms } => Action::WaitForNamed {
            target,
            timeout: crate::time::Duration::from_millis(timeout_ms),
        },
        Wire::Expect(expected) => Action::Expect(SemanticExpectation {
            target: expected.target,
            semantic_target: expected.semantic_target,
            timeout: expected.timeout_ms.map(crate::time::Duration::from_millis),
            role: expected.role,
            name: expected.name,
            name_contains: expected.name_contains,
            value: expected.value,
            value_contains: expected.value_contains,
            focused: expected.focused,
            selected: expected.selected,
            checked: expected.checked,
            expanded: expected.expanded,
            enabled: expected.enabled,
            busy: expected.busy,
            invalid: expected.invalid,
        }),
        Wire::Perform {
            target,
            semantic_target,
            action,
        } => Action::Perform {
            target,
            semantic_target,
            action: semantic_action_from_wire(action),
        },
        Wire::Invoke(invocation) => Action::Invoke(Invocation {
            address: tilo_actions::ActionAddress::new(invocation.resource, invocation.action),
            arguments: invocation.args,
            confirm: invocation.confirm,
            confirmation_phrase: invocation.confirmation_phrase,
            expect_error: invocation.expect_error,
        }),
    }
}

fn semantic_action_from_wire(action: tilo_headless_schema::SemanticAction) -> crate::a11y::Action {
    use tilo_headless_schema::SemanticAction as Wire;
    match action {
        Wire::Activate => crate::a11y::Action::Activate,
        Wire::Focus => crate::a11y::Action::Focus,
        Wire::ToggleCheck => crate::a11y::Action::ToggleCheck,
        Wire::Increment => crate::a11y::Action::Increment,
        Wire::Decrement => crate::a11y::Action::Decrement,
        Wire::Expand => crate::a11y::Action::Expand,
        Wire::Collapse => crate::a11y::Action::Collapse,
        Wire::ScrollIntoView => crate::a11y::Action::ScrollIntoView,
        Wire::ShowContextMenu => crate::a11y::Action::ShowContextMenu,
        Wire::SetValue(value) => crate::a11y::Action::SetValue(value),
        Wire::SetSelection { anchor, focus } => crate::a11y::Action::SetSelection { anchor, focus },
    }
}

fn drive_spec_actions(
    spec: &Spec,
    driver: &mut Driver,
    mut persist: impl FnMut(&mut Driver),
) -> Option<String> {
    tilo_startup_metrics::mark("headless-actions-start");
    // Native idle frames admit queued local controls even without input. Use
    // the same host tick here, including fresh permissions and undo recording.
    if !driver.wait_until(crate::time::Duration::from_secs(10), |driver| {
        !crate::widget::controls::has_pending_control_invocation(driver.root.as_ref())
            && driver.pending_control_invocations.is_empty()
            && driver.bound_invocations.is_empty()
    }) {
        return Some("startup control invocation did not become ready".into());
    }
    if let Some(error) = driver.control_failures.first() {
        return Some(format!("startup control invocation failed: {error}"));
    }
    persist(driver);
    for (index, action) in spec.actions.iter().enumerate() {
        if let Err(error) = driver.perform(action) {
            tilo_startup_metrics::mark("headless-action-failed");
            tilo_startup_metrics::mark("headless-actions-done");
            return Some(format!(
                "action {} of {} ({}): {error}",
                index.saturating_add(1),
                spec.actions.len(),
                action_description(action)
            ));
        }
        persist(driver);
    }
    tilo_startup_metrics::mark("headless-actions-done");
    None
}

fn audit_spec(spec: &Spec, driver: &Driver, failure: &mut Option<String>) -> Result<(), String> {
    if !spec.audit && spec.audit_report.is_none() {
        return Ok(());
    }
    let audit = driver.accessibility_audit();
    if spec.audit {
        for issue in &audit.issues {
            eprintln!(
                "headless accessibility [{}] {}: {}",
                issue.code, issue.path, issue.message
            );
        }
    }
    if let Some(path) = &spec.audit_report {
        let json = serde_json::to_string_pretty(&audit)
            .map_err(|error| format!("serializing accessibility audit: {error}"))?;
        write_headless_output(path, json).map_err(|error| {
            format!("writing accessibility audit to {}: {error}", path.display())
        })?;
    }
    if spec.audit && !audit.is_clean() && failure.is_none() {
        *failure = Some(format!(
            "accessibility audit found {} issue(s)",
            audit.issues.len()
        ));
    }
    Ok(())
}

fn write_spec_outputs(spec: &Spec, driver: &mut Driver) -> Result<(), String> {
    if let Some(path) = &spec.targets {
        let mut names: Vec<(String, [f64; 4])> = driver
            .targets()
            .into_iter()
            .map(|(name, rect)| (name, [rect.x0, rect.y0, rect.width(), rect.height()]))
            .collect();
        names.sort_by(|a, b| a.0.cmp(&b.0));
        let json = serde_json::to_string_pretty(&names).unwrap_or_default();
        write_headless_output(path, json)
            .map_err(|error| format!("writing targets to {}: {error}", path.display()))?;
    }
    if let Some(path) = &spec.dump {
        write_headless_output(path, driver.dump_json())
            .map_err(|error| format!("writing dump to {}: {error}", path.display()))?;
    }
    if let Some(path) = &spec.inspect {
        let json = driver
            .inspection_json()
            .map_err(|error| format!("serializing inspection: {error}"))?;
        write_headless_output(path, json)
            .map_err(|error| format!("writing inspection to {}: {error}", path.display()))?;
    }
    Ok(())
}

/// Drive the same prepared component with existing actions, audit, and output formats.
/// Returns 0 for success, 1 for an action/output failure, and 3 for durability failure.
pub fn run_component(
    spec: &Spec,
    component: crate::application::AppComponent,
    inputs: crate::application::ComponentRunnerInputs,
) -> i32 {
    tilo_startup_metrics::mark("headless-setup-start");
    let records = component.callback_binding().records;
    let mut driver = match Driver::from_component(
        component,
        spec.viewport.unwrap_or(DEFAULT_VIEWPORT),
        inputs,
    ) {
        Ok(driver) => driver,
        Err(error) => {
            tilo_startup_metrics::mark("headless-setup-failed");
            eprintln!("headless: component setup failed: {error}");
            return 3;
        }
    };
    tilo_startup_metrics::mark("headless-setup-ready");
    let mut failure = drive_spec_actions(spec, &mut driver, |driver| {
        driver.persist_tick();
    });
    driver.pump_component_wakes();
    if let Err(error) =
        audit_spec(spec, &driver, &mut failure).and_then(|()| write_spec_outputs(spec, &mut driver))
    {
        tilo_startup_metrics::mark("headless-output-failed");
        failure = Some(error);
    } else {
        tilo_startup_metrics::mark("headless-output-done");
    }
    tilo_startup_metrics::mark("headless-durability-start");
    driver.close_components();
    let reports = driver.component_close_reports();
    if reports.iter().any(|report| {
        report.view_error.is_some()
            || report.record_error.is_some()
            || report.teardown_error.is_some()
    }) {
        tilo_startup_metrics::mark("headless-durability-failed");
        eprintln!("headless: component close could not persist all state");
        return 3;
    }
    if let Err(error) = records.try_flush() {
        tilo_startup_metrics::mark("headless-durability-failed");
        eprintln!("headless: state write failed: {error}");
        return 3;
    }
    tilo_startup_metrics::mark("headless-durability-done");
    match failure {
        Some(error) => {
            eprintln!("headless: {error}");
            1
        }
        None => 0,
    }
}

/// Run `spec` against `root`, persisting through this package's store.
///
/// The document comes from, and goes back to, the same place the live app
/// uses — that is the point. Returns a process exit code: `0` when every
/// action succeeded, `1` when one did not (with the reason on stderr) and
/// `3` when persistence itself was unavailable, which a sweep must not
/// mistake for "nothing changed".
pub fn run(spec: &Spec, root: Box<dyn Widget>, manifest_id: &str) -> i32 {
    run_with_theme(spec, root, manifest_id, Theme::dark())
}

/// Drive the same prepared persistence path with an explicit capture theme.
#[cfg(not(target_arch = "wasm32"))]
pub fn run_with_theme(spec: &Spec, root: Box<dyn Widget>, manifest_id: &str, theme: Theme) -> i32 {
    use crate::launch::{LegacyLaunchInputs, LegacyRecordPolicy};
    tilo_startup_metrics::mark("headless-setup-start");
    let records = crate::record::RecordSet::open(manifest_id);
    let expected_index =
        match app_state::DocStore::legacy_package_document_path(records.package_id()) {
            Ok(path) => path,
            Err(error) => {
                tilo_startup_metrics::mark("headless-setup-failed");
                eprintln!("headless: invalid namespace: {error}");
                return 3;
            }
        };
    let (state, autosave) = crate::persist::open(manifest_id);
    let index = state.doc.clone();
    let mut inputs = LegacyLaunchInputs::new(state, root, autosave);
    inputs.records = LegacyRecordPolicy::AutomaticHeadless {
        records,
        index,
        expected_index,
    };
    run_initialized_with_history(spec, inputs, theme, None)
}

/// Browser builds retain the headless-drive API but cannot open or flush the
/// native package store that gives the runner its durability guarantee.
#[cfg(target_arch = "wasm32")]
pub fn run_with_theme(_: &Spec, _: Box<dyn Widget>, _: &str, _: Theme) -> i32 {
    tilo_startup_metrics::mark("headless-durability-failed");
    3
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn build_initialized_driver(
    inputs: crate::launch::LegacyLaunchInputs,
    viewport: (u32, u32),
) -> Result<(Box<Driver>, crate::persist::DocWriter), String> {
    use crate::launch::LegacyRecordPolicy;
    let autosave = &inputs.autosave;
    // An explicitly disabled writer is not successful durability, and never
    // causes this runner to reopen a different default store behind the app.
    if inputs.persistence == crate::launch::LegacyPersistencePolicy::Host && !autosave.is_enabled()
    {
        return Err(format!(
            "persistence unavailable: {}",
            autosave.last_error().unwrap_or_default()
        ));
    }
    inputs.validate_persistence_ownership()?;
    if let LegacyRecordPolicy::AutomaticHeadless {
        records,
        expected_index,
        ..
    } = &inputs.records
        && (autosave.package_id() != records.package_id()
            || autosave.document_path() != expected_index
            || app_state::DocStore::legacy_package_document_path(records.package_id())
                .ok()
                .as_ref()
                != Some(expected_index))
    {
        return Err("custom persistence namespace requires explicit prepared inputs".into());
    }
    Ok(build_legacy_driver(inputs, viewport, Theme::dark(), true))
}

/// Inspect the same explicit record binding without requiring or flushing a
/// writer. Legacy automatic headless persistence remains opt-in to headless
/// execution; an audit must not activate that compatibility behavior.
#[cfg(feature = "native")]
pub(crate) fn build_audit_driver(
    inputs: crate::launch::LegacyLaunchInputs,
    viewport: (u32, u32),
    theme: Theme,
) -> (Box<Driver>, crate::persist::DocWriter) {
    build_legacy_driver(inputs, viewport, theme, false)
}

#[cfg(not(target_arch = "wasm32"))]
fn build_legacy_driver(
    inputs: crate::launch::LegacyLaunchInputs,
    viewport: (u32, u32),
    theme: Theme,
    activate_automatic_records: bool,
) -> (Box<Driver>, crate::persist::DocWriter) {
    use crate::launch::{LegacyLaunchInputs, LegacyRecordPolicy};
    let LegacyLaunchInputs {
        mut state,
        root,
        autosave,
        mut records,
        registry,
        persistence: _,
    } = inputs;
    if activate_automatic_records
        && let LegacyRecordPolicy::AutomaticHeadless { index, .. } = &mut records
    {
        // A custom opener may have cloned an outstanding transaction into
        // both documents. Close that identical index transaction before any
        // index-only writes can become part of it. fork() below closes the
        // source transaction with the same default commit options.
        let _ = index.get_heads();
        // The package index and bootstrap state may share committed history,
        // but must not independently author changes with the index's actor.
        // fork() closes pending transactions before assigning a branch actor.
        // Actual record admission later installs that record store's actor;
        // authenticated action attribution remains the host's author identity.
        let bootstrap = state.doc.fork();
        state.adopt_doc(bootstrap);
    }
    let mut driver = Box::new(Driver::new_with_theme(state, root, viewport, theme));
    *driver.action_registry_mut() = registry;
    match records {
        LegacyRecordPolicy::Explicit { records, index } => driver.attach_records(records, index),
        LegacyRecordPolicy::AutomaticHeadless { records, index, .. }
            if activate_automatic_records =>
        {
            driver.attach_prepared_record_persistence(records, index);
        }
        LegacyRecordPolicy::None | LegacyRecordPolicy::AutomaticHeadless { .. } => {}
    }
    (driver, autosave)
}

#[cfg(test)]
pub(crate) fn run_initialized(spec: &Spec, inputs: crate::launch::LegacyLaunchInputs) -> i32 {
    run_initialized_with_history(spec, inputs, Theme::dark(), None)
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn run_initialized_with_history(
    spec: &Spec,
    inputs: crate::launch::LegacyLaunchInputs,
    theme: Theme,
    history: Option<crate::history_mode::HistoryRegistration>,
) -> i32 {
    let persistence = inputs.persistence;
    let (mut driver, mut autosave) =
        match build_initialized_driver(inputs, spec.viewport.unwrap_or(DEFAULT_VIEWPORT)) {
            Ok(prepared) => prepared,
            Err(error) => {
                tilo_startup_metrics::mark("headless-setup-failed");
                eprintln!("headless: {error}");
                return 3;
            }
        };

    driver.root.set_motion_reduced(theme.reduce_motion);
    driver.set_theme(theme);
    if let Some(history) = history
        && let Err(error) = driver.install_history(history)
    {
        tilo_startup_metrics::mark("headless-setup-failed");
        eprintln!("headless: History setup failed: {error}");
        return 3;
    }
    driver.relayout();
    tilo_startup_metrics::mark("headless-setup-ready");
    let mut failure = drive_spec_actions(spec, &mut driver, |driver| {
        driver.persist_tick();
        autosave.request(driver.persistence_document());
    });
    tilo_startup_metrics::mark("headless-record-flush-start");
    if let Err(error) = driver.try_persist_flush() {
        tilo_startup_metrics::mark("headless-durability-failed");
        eprintln!("headless: record write failed: {error}");
        return 3;
    }
    tilo_startup_metrics::mark("headless-record-flush-done");

    if let Err(error) =
        audit_spec(spec, &driver, &mut failure).and_then(|()| write_spec_outputs(spec, &mut driver))
    {
        tilo_startup_metrics::mark("headless-output-failed");
        eprintln!("headless: {error}");
        return 1;
    }

    tilo_startup_metrics::mark("headless-output-done");
    tilo_startup_metrics::mark("headless-durability-start");
    // The same two calls `App::exiting` makes, for the same reason.
    let durability = if persistence == crate::launch::LegacyPersistencePolicy::WidgetOwned {
        driver.close_components();
        crate::app::flush_widget_owned_persistence(
            driver.root.as_mut(),
            &mut driver.component_close_reports,
        )
    } else {
        autosave.request(driver.persistence_document());
        autosave.flush();
        autosave.last_error().map_or(Ok(()), |error| Err(error))
    };
    if let Err(e) = durability {
        tilo_startup_metrics::mark("headless-durability-failed");
        eprintln!("headless: state write failed: {e}");
        return 3;
    }
    tilo_startup_metrics::mark("headless-durability-done");
    match failure {
        Some(e) => {
            eprintln!("headless: {e}");
            1
        }
        None => 0,
    }
}

fn write_headless_output(
    path: &std::path::Path,
    contents: impl AsRef<[u8]>,
) -> std::io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents)
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;

    /// A widget that asks for frames as configured, over its children.
    struct DemandProbe {
        name: &'static str,
        animating: bool,
        deadline: Option<crate::time::Instant>,
        children: Vec<DemandProbe>,
    }

    impl DemandProbe {
        fn leaf(name: &'static str) -> Self {
            Self {
                name,
                animating: false,
                deadline: None,
                children: Vec::new(),
            }
        }
    }

    impl Widget for DemandProbe {
        fn sync(&mut self, _state: &mut crate::widget::AppModel, _path: &crate::widget::ModelPath) {
        }
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            _node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            taffy.leaf_node(taffy::Style::default())
        }
        fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, _origin: Point) {}
        fn draw(
            &mut self,
            _scene: &mut Scene,
            _transform: Affine,
            _text_renderer: &mut TextRenderer,
            _theme: &Theme,
            _presence: &PresenceCtx,
        ) {
        }
        fn children(&self) -> Vec<&dyn Widget> {
            self.children
                .iter()
                .map(|child| child as &dyn Widget)
                .collect()
        }
        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            self.children
                .iter_mut()
                .map(|child| child as &mut (dyn Widget + '_))
                .collect()
        }
        fn needs_animation_frame(&self) -> bool {
            self.animating || self.children.iter().any(Widget::needs_animation_frame)
        }
        fn next_wake_deadline(&self) -> Option<crate::time::Instant> {
            self.children
                .iter()
                .filter_map(Widget::next_wake_deadline)
                .chain(self.deadline)
                .min()
        }
        fn name(&self) -> &str {
            self.name
        }
        fn get_rect(&self) -> Rect {
            Rect::ZERO
        }
    }

    #[test]
    fn frame_requesters_name_the_widgets_that_ask_for_frames() {
        let now = crate::time::now();
        let mut spinner = DemandProbe::leaf("Spinner");
        spinner.animating = true;
        let mut clock = DemandProbe::leaf("Clock");
        clock.deadline = Some(now);
        let mut later = DemandProbe::leaf("Later");
        later.deadline = now.checked_add(std::time::Duration::from_secs(60));
        let mut panel = DemandProbe::leaf("Panel");
        panel.children = vec![spinner, DemandProbe::leaf("Idle")];
        let mut root = DemandProbe::leaf("Root");
        root.children = vec![panel, clock, later];
        let harness = FrameHarness::new(State::new(), Box::new(root), (200, 100));
        let mut requesters = harness.frame_requesters();
        requesters.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            requesters,
            vec![
                ("Root/Clock".to_owned(), FrameDemand::Deadline),
                ("Root/Panel/Spinner".to_owned(), FrameDemand::Animation),
            ]
        );
    }

    #[test]
    fn an_idle_tree_has_no_frame_requesters() {
        let mut root = DemandProbe::leaf("Root");
        root.children = vec![DemandProbe::leaf("Idle")];
        let harness = FrameHarness::new(State::new(), Box::new(root), (200, 100));
        assert!(harness.frame_requesters().is_empty());
        assert!(!harness.frame_schedule().needs_animation_frame);
    }

    #[test]
    fn late_browser_font_install_rebuilds_layout_without_mutating_document_or_focus() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts");
        let catalog = crate::font_catalog::FontCatalog::product("", "en-US");
        let fonts = catalog
            .load_browser_startup_bytes(|name| std::fs::read(root.join(name)).ok())
            .unwrap();
        let mut driver = Driver::new_with_controlled_inputs(
            State::new(),
            Box::new(crate::widget::Text::new("مَرْحَبًا")),
            (400, 200),
            Theme::dark(),
            TextRenderer::with_browser_font_system(fonts),
            crate::permissions::UserId("font-test".into()),
        );
        assert_eq!(
            driver.take_browser_font_demands(),
            ["NotoSansArabic[wght].ttf"]
        );
        let frames = driver.relayout_frames();
        let heads = driver.state.doc.get_heads();
        let focused = driver.focused;
        assert!(
            driver
                .install_browser_font(
                    "NotoSansArabic[wght].ttf",
                    std::fs::read(root.join("NotoSansArabic[wght].ttf")).unwrap(),
                )
                .unwrap()
        );
        assert_eq!(driver.relayout_frames(), frames + 1);
        assert!(driver.last_relayout().rebuilt);
        assert_eq!(driver.state.doc.get_heads(), heads);
        assert_eq!(driver.focused, focused);
    }

    static CONTROLLED_HISTORY: crate::history_mode::HistoryDefinition =
        crate::history_mode::HistoryDefinition {
            app_id: "dev.tilo.controlled-history-fixture",
            scopes: &[crate::history_mode::HistoryScopeDefinition {
                key: "document",
                title: "Controlled notebook",
                presentation: crate::history_mode::HistoryPresentation::Domain {
                    create: crate::history_mode::test_preview,
                },
                content_schema: None,
            }],
        };

    fn controlled_history_registration(record: u64) -> crate::history_mode::HistoryRegistration {
        crate::history_mode::HistoryRegistration::new(
            &CONTROLLED_HISTORY,
            vec![
                crate::history_mode::HistorySourceRegistration::HostDocument {
                    scope: "document",
                    resource: crate::history_mode::HistoryResource::new(
                        "controlled-profile",
                        "controlled-workspace",
                        CONTROLLED_HISTORY.app_id,
                        crate::RecordId::for_tab(record),
                    ),
                },
            ],
        )
        .expect("valid controlled History registration")
    }

    fn controlled_owned_root(
        record: u64,
    ) -> (Box<dyn Widget>, crate::history_mode::HistoryRegistration) {
        let store = crate::record::RecordStore::in_memory();
        let checkout = crate::record::RecordCheckout::new(
            crate::RecordId::for_tab(record),
            crate::record::RecordKind::new("controlled-history"),
        );
        let mut tile = crate::widget::TileHost::new(crate::widget::TileContent::Stub {
            kind: crate::widget::AppKind::Terminal,
            label: "Controlled owner".into(),
        });
        tile.bind_or_create_record(store, checkout);
        let owner = Widget::history_owner(&tile).expect("bound in-memory owner");
        let registration = crate::history_mode::HistoryRegistration::new(
            &CONTROLLED_HISTORY,
            vec![
                crate::history_mode::HistorySourceRegistration::owned(
                    "document",
                    CONTROLLED_HISTORY.app_id,
                    owner,
                )
                .expect("valid owned source"),
            ],
        )
        .expect("valid owned registration");
        (Box::new(tile), registration)
    }

    #[test]
    fn controlled_managed_root_installs_history_and_explicit_author() {
        let author = crate::permissions::UserId("controlled-history-author".into());
        let mut state = State::new();
        state
            .doc
            .put(automerge::ROOT, "body", "controlled entry")
            .unwrap();
        state.commit_action(None, None);
        let mut driver = Driver::from_managed_root_with_controlled_inputs(
            state,
            crate::history_mode::ManagedRoot::new(
                Box::new(crate::widget::Column::new()),
                controlled_history_registration(910),
            ),
            (900, 650),
            Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            author.clone(),
        )
        .expect("controlled managed root installs through the public lifecycle");

        assert_eq!(driver.author, author);
        assert_eq!(driver.validate_history_registration(), Ok(()));
        assert!(driver.targets().contains_key("history-open"));
        driver
            .perform_named("history-open", &crate::a11y::Action::Activate)
            .expect("public History target activates");
        assert!(driver.targets().contains_key("activity_timeline"));
    }

    #[test]
    fn controlled_managed_root_rejects_mismatched_owner() {
        let (ordinary_root, _) = controlled_owned_root(920);
        let (_ordinary_registration_owner, ordinary_registration) = controlled_owned_root(921);
        let Err(expected) = Driver::from_managed_root(
            State::new(),
            crate::history_mode::ManagedRoot::new(ordinary_root, ordinary_registration),
            (900, 650),
        ) else {
            panic!("ordinary managed construction accepted a mismatched owner")
        };

        let (controlled_root, _) = controlled_owned_root(922);
        let (_controlled_registration_owner, controlled_registration) = controlled_owned_root(923);
        let Err(actual) = Driver::from_managed_root_with_controlled_inputs(
            State::new(),
            crate::history_mode::ManagedRoot::new(controlled_root, controlled_registration),
            (900, 650),
            Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("controlled-mismatch-author".into()),
        ) else {
            panic!("controlled managed construction accepted a mismatched owner")
        };

        assert_eq!(
            expected,
            crate::history_mode::HistoryRegistrationError::UnsupportedOwner
        );
        assert_eq!(actual, expected);
    }

    #[test]
    fn portable_caller_palette_opens_over_the_real_frame() {
        let mut frame = FrameHarness::new_with_controlled_inputs(
            State::new(),
            Box::new(crate::widget::TextInput::local().with_id("document")),
            (640, 480),
            Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("fixture".into()),
        );
        frame.set_platform_conventions(crate::platform::PlatformConventions::new(
            crate::platform::Platform::Linux,
        ));
        frame.send_host_event(&WindowEvent::ModifiersChanged(
            winit::event::Modifiers::from(winit::keyboard::ModifiersState::CONTROL),
        ));
        frame.send_host_event(&key_event(
            &Key::Character("k".into()),
            ElementState::Pressed,
        ));
        assert!(
            frame
                .inspection_json()
                .unwrap()
                .contains("action-palette-results")
        );
    }

    #[test]
    fn portable_host_events_preserve_modifiers_tab_and_command_undo() {
        let input = crate::widget::TextInput::local()
            .with_id("name")
            .with_initial_value("hello");
        let mut frame = FrameHarness::new_with_controlled_inputs(
            State::new(),
            Box::new(
                crate::widget::Column::new()
                    .push(Box::new(input))
                    .push(Box::new(crate::widget::Button::new("done", "Done", None))),
            ),
            (320, 180),
            Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("fixture".into()),
        );
        frame.set_platform_conventions(crate::platform::PlatformConventions::new(
            crate::platform::Platform::MacOs,
        ));
        frame
            .perform_named("name", &crate::a11y::Action::Focus)
            .unwrap();
        frame.send_host_event(&WindowEvent::ModifiersChanged(
            winit::event::Modifiers::from(winit::keyboard::ModifiersState::SUPER),
        ));
        frame.send_host_event(&key_event(
            &Key::Character("a".into()),
            ElementState::Pressed,
        ));
        frame.send_host_event(&WindowEvent::ModifiersChanged(
            winit::event::Modifiers::default(),
        ));
        frame.send_host_event(&key_event(
            &Key::Character("x".into()),
            ElementState::Pressed,
        ));
        assert_eq!(
            frame
                .access_node_named("name", None)
                .unwrap()
                .value
                .as_deref(),
            Some("x")
        );
        frame.send_host_event(&WindowEvent::ModifiersChanged(
            winit::event::Modifiers::default(),
        ));
        frame.send_host_event(&key_event(
            &Key::Named(winit::keyboard::NamedKey::Tab),
            ElementState::Pressed,
        ));
        assert!(frame.access_node_named("done", None).unwrap().state.focused);
        frame.send_host_event(&WindowEvent::ModifiersChanged(
            winit::event::Modifiers::from(winit::keyboard::ModifiersState::ALT),
        ));
        frame.send_host_event(&key_event(
            &Key::Named(winit::keyboard::NamedKey::ArrowUp),
            ElementState::Pressed,
        ));
        assert!(frame.access_node_named("name", None).unwrap().state.focused);
        frame.send_host_event(&WindowEvent::ModifiersChanged(
            winit::event::Modifiers::from(
                winit::keyboard::ModifiersState::ALT | winit::keyboard::ModifiersState::SHIFT,
            ),
        ));
        frame.send_host_event(&key_event(
            &Key::Named(winit::keyboard::NamedKey::ArrowLeft),
            ElementState::Pressed,
        ));
        let name = frame.access_node_named("name", None).unwrap();
        assert!(name.state.focused);
        assert_eq!(
            name.text_selection,
            Some(crate::a11y::AccessibleTextSelection {
                anchor: 1,
                focus: 0,
            })
        );
        let mut document = super::dispatched_action_driver_tests::action_driver();
        document.set_platform_conventions(crate::platform::PlatformConventions::new(
            crate::platform::Platform::MacOs,
        ));
        document.send_host_event(&key_event(
            &Key::Character("a".into()),
            ElementState::Pressed,
        ));
        assert_eq!(
            super::dispatched_action_driver_tests::body(&mut document),
            "a"
        );
        document.send_host_event(&WindowEvent::ModifiersChanged(
            winit::event::Modifiers::from(winit::keyboard::ModifiersState::SUPER),
        ));
        document.send_host_event(&key_event(
            &Key::Character("z".into()),
            ElementState::Pressed,
        ));
        assert_eq!(
            super::dispatched_action_driver_tests::body(&mut document),
            ""
        );
    }

    #[test]
    fn portable_frame_exposes_focused_text_input_ime_and_hides_it_on_button_focus() {
        let input = crate::widget::TextInput::local()
            .with_id("name")
            .with_initial_value("hello");
        let mut frame = FrameHarness::new_with_controlled_inputs(
            State::new(),
            Box::new(
                crate::widget::Column::new()
                    .push(Box::new(input))
                    .push(Box::new(crate::widget::Button::new("done", "Done", None))),
            ),
            (320, 180),
            Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("fixture".into()),
        );
        frame
            .perform_named("name", &crate::a11y::Action::Focus)
            .unwrap();
        let ime = frame.frame_output().ime.unwrap();
        assert!(ime.width > 0 && ime.height > 0);
        frame.send(&WindowEvent::Ime(winit::event::Ime::Commit("é".into())));
        assert!(
            frame
                .access_node_named("name", None)
                .unwrap()
                .value
                .unwrap()
                .contains('é')
        );
        frame
            .perform_named("done", &crate::a11y::Action::Focus)
            .unwrap();
        assert!(frame.frame_output().ime.is_none());
    }

    #[test]
    fn portable_frame_exports_real_scene_size_and_input_result() {
        use automerge::ReadDoc;
        let mut frame = FrameHarness::new_with_controlled_inputs(
            State::new(),
            Box::new(crate::widget::Checkbox::new("remember", "remember").with_id("remember")),
            (320, 180),
            Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("fixture".into()),
        );
        frame.set_input_scale(2.0).unwrap();
        let output = frame.frame_output();
        assert_eq!(output.logical_size, (320, 180));
        assert_eq!(output.device_size, (640, 360));
        assert!(!output.scene.encoding().draw_tags.is_empty());
        frame.tap_named("remember").unwrap();
        assert_eq!(
            frame
                .state_mut()
                .doc
                .get(automerge::ROOT, "remember")
                .unwrap()
                .unwrap()
                .0
                .as_bool(),
            Some(true)
        );
        frame.resize((480, 240)).unwrap();
        assert_eq!(frame.frame_output().logical_size, (480, 240));
        assert!(frame.frame_output().semantics.root.semantics.is_some());
    }

    #[test]
    fn headless_focus_and_key_activation_route_to_the_right_target() {
        // Case `anonymous_headless_focus_survives_rebuild_without_state_transfer`.
        {
            let mut driver = Driver::new_with_controlled_inputs(
                State::new(),
                Box::new(crate::widget::layout::AnonymousFocusProbe::new(
                    "own selection",
                )),
                (600, 400),
                Theme::dark(),
                crate::font_catalog::fixture_renderer(),
                crate::permissions::UserId("anonymous-test".into()),
            );
            let old = driver.focused.unwrap();
            // A plain relayout retains the composition; force the rebuild this
            // scenario is about, so the anonymous control has to be found again
            // in a fresh tree rather than kept by node id.
            driver.set_full_relayout(Some(true));
            driver.relayout();
            assert!(driver.last_relayout().rebuilt);
            let current = driver.focused.unwrap();
            assert_eq!(
                driver
                    .inspect_node(current, |widget| crate::undo::get_state(widget))
                    .unwrap()
                    .as_deref(),
                Some("own selection")
            );
            assert!(
                driver
                    .inspect_node(current, |widget| widget.is_focused())
                    .unwrap(),
                "the same instance keeps focus across the rebuild"
            );
            let _ = old;
        }

        // Case `anonymous_headless_unrelated_focused_siblings_have_no_input_target`.
        {
            let root = Column::new()
                .push(Box::new(crate::widget::layout::AnonymousFocusProbe::new(
                    "a",
                )))
                .push(Box::new(crate::widget::layout::AnonymousFocusProbe::new(
                    "b",
                )));
            let mut driver = Driver::new_with_controlled_inputs(
                State::new(),
                Box::new(root),
                (600, 400),
                Theme::dark(),
                crate::font_catalog::fixture_renderer(),
                crate::permissions::UserId("fixture".into()),
            );
            assert!(driver.focused.is_none());
            driver.relayout();
            assert!(driver.focused.is_none());
        }

        // Case `function_keys_do_not_type_their_names_into_a_focused_field`.
        {
            let mut driver = Driver::new(
                State::new(),
                Box::new(TextInput::local().with_id("function-key-field")),
                (640, 160),
            );
            driver
                .perform_named("function-key-field", &crate::a11y::Action::Focus)
                .unwrap();
            driver.type_text("keep");
            for index in 1..=12 {
                driver.key(&format!("F{index}"));
            }
            let snapshot = driver.inspection();
            assert_eq!(
                snapshot.root.semantics.unwrap().value.as_deref(),
                Some("keep")
            );
        }

        // Case `app_wrapper_routes_tab_to_an_opted_in_code_editor_but_traverses_forms`:
        // The frame harness is the app wrapper used by scenarios. Keep Tab
        // coverage here rather than only in direct widget tests: the live host
        // decides whether focus traversal intercepts the key before a widget can
        // see it.
        {
            let mut editor = Driver::new(State::new(), Box::new(CodeEditor::local()), (320, 160));
            editor.tab(false);
            editor.key("Tab");
            let editor_node = editor.focused().expect("editor has focus");
            // The retained node is valid until relayout, and `key` has already
            // completed its relayout before this observation.
            assert_eq!(
                unsafe {
                    crate::undo::get_state(&(*(*editor.node_map.get(&editor_node).unwrap())))
                },
                Some("4".into()),
                "Tab reaches the code editor and inserts its configured indent"
            );
            editor.set_modifiers(&["shift".into()]);
            editor.key("Tab");
            let editor_node = editor.focused().expect("editor remains focused");
            assert_eq!(
                unsafe {
                    crate::undo::get_state(&(*(*editor.node_map.get(&editor_node).unwrap())))
                },
                Some("0".into()),
                "Shift+Tab reaches the same editor and outdents"
            );

            let form = Column::new()
                .push(Box::new(TextInput::local().with_id("field")))
                .push(Box::new(Button::new("next", "Next", None)));
            let mut fields = Driver::new(State::new(), Box::new(form), (320, 160));
            fields.tab(false);
            let field = fields.focused().expect("first field has focus");
            fields.key("Tab");
            assert_ne!(
                fields.focused(),
                Some(field),
                "ordinary fields yield Tab to focus traversal"
            );
        }

        // Case `enter_and_space_activate_a_focused_button_once_per_press`.
        {
            let activations: std::rc::Rc<std::cell::Cell<usize>> =
                std::rc::Rc::new(std::cell::Cell::new(0));
            let observed = std::rc::Rc::clone(&activations);
            let button = Button::new("save", "Save", None).on_click(move || {
                observed.set(observed.get().saturating_add(1));
            });
            let mut driver = Driver::new(State::new(), Box::new(button), (400, 200));
            driver
                .perform_named("save", &crate::a11y::Action::Focus)
                .expect("button accepts focus");

            driver.key("Enter");
            assert_eq!(activations.get(), 1, "Enter activates only on key press");
            driver.key("Space");
            assert_eq!(activations.get(), 2, "Space activates only on key press");
        }

        // Case `consumed_enter_does_not_run_the_focused_activation_fallback`.
        {
            let activations = std::rc::Rc::new(std::cell::Cell::new(0));
            let control = ConsumingActivationControl {
                activations: std::rc::Rc::clone(&activations),
                focused: false,
                rect: Rect::ZERO,
            };
            let mut driver = Driver::new(State::new(), Box::new(control), (400, 200));
            driver
                .perform_named("consuming-activation", &crate::a11y::Action::Focus)
                .expect("control accepts focus");

            driver.key("Enter");

            assert_eq!(
                activations.get(),
                1,
                "the consumed key press must not activate a second time"
            );
        }
    }

    #[test]
    fn headless_record_policy_stays_explicit_and_checked() {
        // Case `legacy_audit_preserves_explicit_record_index_theme_and_disabled_writer`.
        {
            use automerge::{ReadDoc, transaction::Transactable};
            let mut state = State::new();
            state
                .doc
                .put(automerge::ROOT, "body_marker", "body")
                .unwrap();
            let mut index = automerge::AutoCommit::new();
            index.put(automerge::ROOT, "index_marker", "index").unwrap();
            let inputs = crate::launch::LegacyLaunchInputs::new(
                state,
                Box::new(crate::widget::Text::new("audit")),
                crate::persist::DocWriter::disabled("audit-only fixture"),
            )
            .with_records(crate::record::RecordSet::in_memory(), index);
            let (mut driver, autosave) =
                build_audit_driver(inputs, (800, 600), Theme::light().with_text_scale(1.5));
            assert!(!autosave.is_enabled());
            assert!(
                driver.records().is_some(),
                "audit must retain the explicit record owner"
            );
            assert_eq!(driver.theme.text_scale, 1.5);
            assert_eq!(
                driver
                    .state
                    .doc
                    .get(automerge::ROOT, "body_marker")
                    .unwrap()
                    .unwrap()
                    .0
                    .as_str(),
                Some("body")
            );
            assert_eq!(
                driver
                    .persistence_document()
                    .get(automerge::ROOT, "index_marker")
                    .unwrap()
                    .unwrap()
                    .0
                    .as_str(),
                Some("index")
            );
        }

        // Case `legacy_audit_does_not_activate_automatic_headless_record_policy`.
        {
            let mut inputs = crate::launch::LegacyLaunchInputs::new(
                State::new(),
                Box::new(crate::widget::Text::new("audit")),
                crate::persist::DocWriter::disabled("audit-only fixture"),
            );
            inputs.records = crate::launch::LegacyRecordPolicy::AutomaticHeadless {
                records: crate::record::RecordSet::in_memory(),
                index: automerge::AutoCommit::new(),
                expected_index: std::path::PathBuf::from("unused-audit-namespace"),
            };
            let (driver, autosave) = build_audit_driver(inputs, (800, 600), Theme::light());
            assert!(!autosave.is_enabled());
            assert!(driver.records().is_none());
        }

        // Case `widget_owned_persistence_requires_checked_success`.
        {
            use crate::widget::control_result_probe::Probe;
            for outcome in [None, Some(Err("disk full".into())), Some(Ok(()))] {
                let succeeds = outcome == Some(Ok(()));
                let (mut probe, observed) = Probe::new(false);
                probe.owned_flush = outcome;
                let inputs = crate::launch::LegacyLaunchInputs::new(
                    State::new(),
                    Box::new(probe),
                    crate::persist::DocWriter::disabled("fixture owns storage"),
                )
                .with_widget_owned_persistence();
                assert_eq!(
                    run_initialized_with_history(&Spec::default(), inputs, Theme::dark(), None),
                    if succeeds { 0 } else { 3 }
                );
                assert_eq!(observed.flushes.get(), 1);
            }
        }

        // Case `automatic_record_bootstrap_commits_shared_pending_transaction_before_forking`.
        {
            use crate::launch::{LegacyLaunchInputs, LegacyRecordPolicy};
            use automerge::{AutoCommit, ROOT, ReadDoc};
            let mut state = State::new();
            state
                .doc
                .put(ROOT, "pending_from_opener", "retained")
                .unwrap();
            assert!(state.doc.pending_ops() > 0);
            let index = state.doc.clone();
            let original_actor = index.get_actor().clone();
            let mut inputs = LegacyLaunchInputs::new(
                state,
                Box::new(Button::new("plain-root", "Plain", None)),
                crate::persist::DocWriter::disabled("no writes in this fixture"),
            );
            inputs.records = LegacyRecordPolicy::AutomaticHeadless {
                records: crate::record::RecordSet::in_memory(),
                index,
                expected_index: std::path::PathBuf::from("not-used-by-this-in-memory-fixture"),
            };
            let (mut driver, _) = build_legacy_driver(inputs, (400, 200), Theme::dark(), true);
            let (_, index) = driver.record_persistence.as_mut().unwrap();
            assert_eq!(
                index.pending_ops(),
                0,
                "cloned opener transaction is closed before index mutation"
            );
            assert_eq!(
                index.get_actor(),
                &original_actor,
                "index keeps its actual writer identity"
            );
            assert_ne!(driver.state.doc.get_actor(), &original_actor);
            assert_eq!(
                index.get_heads(),
                driver.state.doc.get_heads(),
                "identical pending input must commit to exactly the same hash"
            );
            index.put(ROOT, "index_only", "reference").unwrap();
            driver
                .state
                .doc
                .put(ROOT, "bootstrap_only", "loaded body")
                .unwrap();
            let mut joined = driver.state.doc.clone();
            joined
                .merge(index)
                .expect("separate index/bootstrap authors cannot collide");
            let reopened =
                AutoCommit::load(&joined.save()).expect("persisted merged history must reload");
            for (key, value) in [
                ("pending_from_opener", "retained"),
                ("index_only", "reference"),
                ("bootstrap_only", "loaded body"),
            ] {
                assert_eq!(
                    reopened.get(ROOT, key).unwrap().unwrap().0.as_str(),
                    Some(value)
                );
            }
        }
    }

    use crate::widget::{Button, Checkbox, CodeEditor, Column, TextInput, TreeNode, TreeView};

    struct StructureReplacingEditor {
        current: Box<dyn Widget>,
        retired: Vec<Box<dyn Widget>>,
        structure_dirty: bool,
        replace_during_sync: bool,
    }

    impl StructureReplacingEditor {
        fn editor(value: &str) -> Box<dyn Widget> {
            Box::new(
                TextInput::local_with_language("en_US")
                    .with_spellcheck(false)
                    .with_autocorrect(false)
                    .with_id("structure-editor")
                    .with_accessible_name("Structure editor")
                    .with_initial_value(value),
            )
        }

        fn new(value: &str) -> Self {
            Self {
                current: Self::editor(value),
                retired: Vec::new(),
                structure_dirty: false,
                replace_during_sync: false,
            }
        }

        fn replace_before_relayout(&mut self) {
            let access = self.current.accessibility().unwrap();
            let value = access.value.unwrap();
            let state = crate::undo::get_state(self.current.as_ref()).unwrap();
            let focused = self.current.is_focused();
            let mut current = Self::editor(&value);
            crate::undo::set_state(current.as_mut(), &state);
            current.set_focus(focused);

            let mut stale = std::mem::replace(&mut self.current, current);
            // The admitted map still points at this allocation until the
            // Driver starts its next relayout. Keep it alive so the fixture
            // observes the logical stale-map distinction without a UAF.
            stale.set_focus(false);
            crate::undo::set_state(stale.as_mut(), "0");
            self.retired.push(stale);
            self.structure_dirty = true;
            self.replace_during_sync = true;
        }
    }

    impl Widget for StructureReplacingEditor {
        crate::testing::delegate_widget_methods!(
            current: build_layout,
            apply_layout,
            draw,
            get_rect
        );
        fn name(&self) -> &'static str {
            "StructureReplacingEditor"
        }

        fn sync(&mut self, state: &mut crate::widget::AppModel, path: &crate::widget::ModelPath) {
            if std::mem::take(&mut self.replace_during_sync) {
                let value = self.current.accessibility().unwrap().value.unwrap();
                let current = std::mem::replace(&mut self.current, Self::editor(&value));
                self.retired.push(current);
                self.structure_dirty = true;
            }
            self.current.sync(state, path);
        }

        fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
            let replace = matches!(
                event,
                WindowEvent::KeyboardInput {
                    event: winit::event::KeyEvent {
                        logical_key: Key::Named(NamedKey::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                    ..
                }
            );
            self.current.handle_event(event, ctx);
            if replace {
                self.replace_before_relayout();
                *ctx.handled = true;
            }
        }

        fn children(&self) -> Vec<&dyn Widget> {
            vec![self.current.as_ref()]
        }

        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            vec![self.current.as_mut()]
        }

        fn take_structure_dirty(&mut self) -> bool {
            std::mem::take(&mut self.structure_dirty)
        }
    }

    #[test]
    fn structure_dirty_relayout_captures_current_live_editor_focus_and_state_before_rebuild() {
        let mut driver = Driver::new_with_controlled_inputs(
            State::new(),
            Box::new(StructureReplacingEditor::new("draft")),
            (600, 400),
            Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("structure-focus-test".into()),
        );
        driver
            .perform_named(
                "structure-editor",
                &crate::a11y::Action::SetValue("current draft".into()),
            )
            .unwrap();
        driver
            .perform_named("structure-editor", &crate::a11y::Action::Focus)
            .unwrap();
        driver
            .perform_named(
                "structure-editor",
                &crate::a11y::Action::SetSelection {
                    anchor: 2,
                    focus: 7,
                },
            )
            .unwrap();
        let before = driver.access_node_named("structure-editor", None).unwrap();
        assert_eq!(before.value.as_deref(), Some("current draft"));
        assert_eq!(
            before.text_selection,
            Some(crate::a11y::AccessibleTextSelection {
                anchor: 2,
                focus: 7,
            })
        );
        assert!(before.state.focused);

        // A real widget interaction replaces the live application view before
        // Driver::try_relayout. Sync then mounts one fresh editor, requiring
        // the pre-frame live snapshot to restore local focus and selection.
        driver.key("Escape");

        let after = driver.access_node_named("structure-editor", None).unwrap();
        assert_eq!(after.value, before.value);
        assert_eq!(after.text_selection, before.text_selection);
        assert!(after.state.focused);
        let bounds = driver.targets()["structure-editor"];
        assert!(bounds.width() > 0.0 && bounds.height() > 0.0);
        let (_, exported_focus) = driver.a11y_focus().expect("public focus survives rebuild");
        assert_eq!(exported_focus.label(), Some("Structure editor"));
    }

    struct ConsumingActivationControl {
        activations: std::rc::Rc<std::cell::Cell<usize>>,
        focused: bool,
        rect: Rect,
    }

    impl Widget for ConsumingActivationControl {
        fn is_default_keyboard_focus(&self) -> bool {
            true
        }

        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = taffy
                .new_leaf(Style {
                    size: Size {
                        width: Dimension::length(120.0),
                        height: Dimension::length(40.0),
                    },
                    ..Default::default()
                })
                .unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }

        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
            let layout = taffy.layout(node).unwrap();
            self.rect = Rect::new(
                origin.x,
                origin.y,
                origin.x + f64::from(layout.size.width),
                origin.y + f64::from(layout.size.height),
            );
        }

        fn draw(
            &mut self,
            _scene: &mut Scene,
            _transform: Affine,
            _text_renderer: &mut TextRenderer,
            _theme: &Theme,
            _presence: &PresenceCtx,
        ) {
        }

        fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
            if self.focused
                && matches!(
                    event,
                    WindowEvent::KeyboardInput {
                        event: winit::event::KeyEvent {
                            logical_key: Key::Named(NamedKey::Enter | NamedKey::Space),
                            state: ElementState::Pressed,
                            ..
                        },
                        ..
                    }
                )
            {
                self.activations
                    .set(self.activations.get().saturating_add(1));
                *ctx.handled = true;
            }
        }

        fn perform_action(
            &mut self,
            action: &crate::a11y::Action,
            _state: &mut crate::widget::AppModel,
            _path: &crate::widget::ModelPath,
        ) -> bool {
            if matches!(action, crate::a11y::Action::Activate) {
                self.activations
                    .set(self.activations.get().saturating_add(1));
                return true;
            }
            false
        }

        fn name(&self) -> &'static str {
            "ConsumingActivationControl"
        }

        fn test_id(&self) -> Option<&str> {
            Some("consuming-activation")
        }

        fn get_rect(&self) -> Rect {
            self.rect
        }

        fn is_focusable(&self) -> bool {
            true
        }

        fn set_focus(&mut self, focused: bool) {
            self.focused = focused;
        }

        fn is_focused(&self) -> bool {
            self.focused
        }
    }

    /// A panel whose own node is a small button drawn inside it, the shape
    /// of an empty state: only a press on the button counts.
    struct PanelButton {
        rect: Rect,
        pointer: Point,
        presses: std::rc::Rc<std::cell::Cell<usize>>,
    }

    impl PanelButton {
        fn button(&self) -> Rect {
            Rect::new(
                self.rect.x0 + 10.0,
                self.rect.y1 - 40.0,
                self.rect.x0 + 90.0,
                self.rect.y1 - 10.0,
            )
        }
    }

    impl Widget for PanelButton {
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = taffy
                .new_leaf(Style {
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::percent(1.0),
                    },
                    ..Default::default()
                })
                .unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }

        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
            let layout = taffy.layout(node).unwrap();
            self.rect = Rect::new(
                origin.x,
                origin.y,
                origin.x + f64::from(layout.size.width),
                origin.y + f64::from(layout.size.height),
            );
        }

        fn draw(
            &mut self,
            _scene: &mut Scene,
            _transform: Affine,
            _text_renderer: &mut TextRenderer,
            _theme: &Theme,
            _presence: &PresenceCtx,
        ) {
        }

        fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
            match event {
                WindowEvent::CursorMoved { position, .. } => {
                    self.pointer =
                        Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                }
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    ..
                } if self.button().contains(self.pointer) => {
                    self.presses.set(self.presses.get().saturating_add(1));
                    *ctx.handled = true;
                }
                _ => {}
            }
        }

        fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
            Some(
                crate::a11y::AccessNode::new(crate::a11y::Role::Button)
                    .with_name("Create")
                    .with_bounds(self.button()),
            )
        }

        fn name(&self) -> &'static str {
            "PanelButton"
        }

        fn test_id(&self) -> Option<&str> {
            Some("panel-button")
        }

        fn get_rect(&self) -> Rect {
            self.rect
        }
    }

    #[test]
    fn a_named_click_lands_on_the_bounds_the_widget_names() {
        let presses = std::rc::Rc::new(std::cell::Cell::new(0));
        let mut driver = Driver::new(
            State::new(),
            Box::new(PanelButton {
                rect: Rect::ZERO,
                pointer: Point::ZERO,
                presses: std::rc::Rc::clone(&presses),
            }),
            (400, 300),
        );
        driver.click_named("panel-button").unwrap();
        assert_eq!(presses.get(), 1);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn synthetic_key_helper_preserves_keyboard_route_and_modifier_supplements() {
        use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;

        let key = Key::Character("Q".into());
        let pressed = key_event(&key, ElementState::Pressed);
        let released = key_event(&key, ElementState::Released);

        for (window_event, expected_state, expected_text) in [
            (pressed, ElementState::Pressed, Some("Q")),
            (released, ElementState::Released, None),
        ] {
            let WindowEvent::KeyboardInput {
                device_id,
                event,
                is_synthetic,
            } = window_event
            else {
                panic!("key_event must use WindowEvent::KeyboardInput");
            };
            assert_eq!(device_id, winit::event::DeviceId::dummy());
            assert!(!is_synthetic);
            assert_eq!(
                event.physical_key,
                winit::keyboard::PhysicalKey::Unidentified(
                    winit::keyboard::NativeKeyCode::Unidentified
                )
            );
            assert_eq!(event.logical_key, key);
            assert_eq!(event.text.as_deref(), expected_text);
            assert_eq!(event.location, winit::keyboard::KeyLocation::Standard);
            assert_eq!(event.state, expected_state);
            assert!(!event.repeat);
            assert_eq!(event.text_with_all_modifiers(), expected_text);
            assert_eq!(event.key_without_modifiers(), key);
        }
    }

    #[test]
    fn audits_certify_action_binding_search_projection_and_accessibility() {
        // Case `native_app_audit_proves_action_binding_and_a11y_together`.
        {
            let resource = crate::widget::resource_actions::resource(
                "example", "Example", "save", "Save", false,
            );
            let binding = tilo_actions::ActionBinding::unit_for(&resource, "save").unwrap();
            let calls = std::rc::Rc::new(std::cell::Cell::new(0));
            let operation_calls = calls.clone();
            let root = crate::widget::resource_actions::immediate(
                Box::new(Column::new().push(Box::new(
                    Button::new("save", "Save", None).on_action(binding),
                ))),
                resource,
                move |_| {
                    operation_calls.set(operation_calls.get() + 1);
                    Ok(tilo_actions::ActionOutput::default())
                },
            );
            let mut driver = Driver::new(State::new(), root, (320, 160));
            assert!(driver.native_app_audit().is_certified());
            driver
                .perform_named("save", &crate::a11y::Action::Activate)
                .expect("bound control dispatches through the headless host");
            assert_eq!(calls.get(), 1);

            let legacy = Driver::new(
                State::new(),
                Box::new(Button::new("legacy", "Legacy", None).on_click(|| {})),
                (320, 160),
            );
            let audit = legacy.native_app_audit();
            assert!(!audit.is_certified());
            assert_eq!(audit.issues[0].code, "legacy-callback");

            let omitted = Driver::new(
                State::new(),
                Box::new(Button::new("omitted", "Omitted", None)),
                (320, 160),
            );
            assert!(
                omitted
                    .native_app_audit()
                    .issues
                    .iter()
                    .any(|issue| issue.code == "missing-action-binding"),
                "an interactive custom/control implementation cannot certify itself by omitting inventory"
            );

            let empty = Driver::new(State::new(), Box::new(Column::new()), (320, 160));
            assert!(
                empty
                    .native_app_audit()
                    .issues
                    .iter()
                    .any(|issue| issue.code == "empty-native-contract"),
                "a custom-painted app cannot certify itself by reporting neither semantics nor controls"
            );
        }

        // Case `quick_button_handler_is_a_certified_searchable_action`.
        {
            let calls = std::rc::Rc::new(std::cell::Cell::new(0));
            let invoked = calls.clone();
            let button = Button::new("refresh", "Refresh", None).on_effect(move || {
                invoked.set(invoked.get() + 1);
            });
            let mut driver = Driver::new(State::new(), Box::new(button), (320, 160));

            assert!(driver.native_app_audit().is_certified());
            assert!(driver.resources().iter().any(|resource| {
                resource.id.as_str() == "tilo.control:refresh"
                    && resource.actions.iter().any(|action| action.id == "invoke")
            }));
            driver
                .perform_named("refresh", &crate::a11y::Action::Activate)
                .expect("the visible control enters through its action");
            assert_eq!(calls.get(), 1);
        }

        // Case `collaborative_button_is_projected_for_search_and_agents`.
        {
            use automerge::ReadDoc as _;

            let button = Button::new("complete", "Complete", Some("completed"));
            let mut driver = Driver::new(State::new(), Box::new(button), (320, 160));

            assert!(driver.native_app_audit().is_certified());
            assert!(driver.resources().iter().any(|resource| {
                resource.kind == "tilo.ui.control"
                    && resource.label == "Complete"
                    && resource
                        .actions
                        .iter()
                        .any(|action| action.id == "activate")
            }));
            driver
                .perform_named("complete", &crate::a11y::Action::Activate)
                .expect("semantic activation reaches the collaborative control");
            assert!(matches!(
                driver.state.doc.get(&automerge::ROOT, "completed"),
                Ok(Some((automerge::Value::Scalar(value), _))) if value.as_ref() == &automerge::ScalarValue::Boolean(true)
            ));
        }

        // Case `accessibility_audit_checks_controls_and_synthetic_identity_contracts`.
        {
            let mut issues = Vec::new();
            let root = AccessNode::new(Role::Group).with_children(vec![
                AccessNode::new(Role::Button)
                    .with_key(7)
                    .with_action_target("duplicate")
                    .with_position(0, 2),
                AccessNode::new(Role::TextInput)
                    .with_action_target("duplicate")
                    .with_value("secret")
                    .with_state(
                        crate::a11y::AccessState::new()
                            .focusable(true)
                            .protected(true),
                    )
                    .with_level(0),
            ]);
            audit_access_node(&root, "Root[0]", &mut issues);
            let codes = issues
                .iter()
                .map(|issue| issue.code.as_str())
                .collect::<HashSet<_>>();
            for code in [
                "missing-name",
                "unnamed-focus-target",
                "protected-value-exposed",
                "invalid-set-position",
                "invalid-level",
                "duplicate-action-target",
            ] {
                assert!(codes.contains(code), "missing {code}: {issues:#?}");
            }
            assert!(issues.iter().all(|issue| issue.path.starts_with("Root[0]")));
            let report = serde_json::to_value(AccessibilityAudit {
                version: ACCESSIBILITY_AUDIT_VERSION,
                issues: issues.clone(),
            })
            .expect("serialize audit report");
            assert_eq!(report["version"], ACCESSIBILITY_AUDIT_VERSION);
            assert!(report["issues"].is_array());
            assert!(
                report["issues"][0]["path"]
                    .as_str()
                    .is_some_and(|path| path.starts_with("Root[0]"))
            );

            let driver = Driver::new(
                State::new(),
                Box::new(Button::new("save", "Save", None)),
                (400, 200),
            );
            assert!(driver.accessibility_audit().is_clean());
        }
    }

    #[test]
    fn headless_spec_parses_every_shape_and_rejects_malformed_input() {
        // Case `spec_parses_every_action_shape`.
        {
            let spec = parse_spec(
            r#"{"viewport":[800,600],"audit":true,"audit_report":"/tmp/a.json","dump":"/tmp/d.json","targets":"/tmp/t.json",
                "inspect":"/tmp/i.json",
                "actions":[{"click_named":"save"},{"click_at":[10,20]},
                           {"tap_named":"save"},{"tap_at":[30,40]},
                           {"swipe_at":[100,200,100,20]},
                           {"key":"Enter"},{"type":"hi"},{"modifiers":["ctrl"]},
                           {"wait_for_named":{"target":"result","timeout_ms":750}},
                           {"expect":{"target":"result","role":"static_text","name_contains":"Done","busy":false,"timeout_ms":1000}},
                           {"perform":{"target":"name","action":"set_value","value":"Ada"}},
                           {"perform":{"target":"name","action":"set_selection","anchor":0,"focus":3}},
                           {"perform":{"target":"workspace-tree","semantic_target":"node:2","action":"expand"}}]}"#,
        )
        .expect("spec parses");
            assert_eq!(spec.viewport, Some((800, 600)));
            assert!(spec.audit);
            assert_eq!(
                spec.audit_report.as_deref(),
                Some(std::path::Path::new("/tmp/a.json"))
            );
            assert_eq!(
                spec.dump.as_deref(),
                Some(std::path::Path::new("/tmp/d.json"))
            );
            assert_eq!(
                spec.inspect.as_deref(),
                Some(std::path::Path::new("/tmp/i.json"))
            );
            assert_eq!(spec.actions.len(), 13);
            assert!(matches!(&spec.actions[0], Action::ClickNamed(n) if n == "save"));
            assert!(matches!(&spec.actions[2], Action::TapNamed(n) if n == "save"));
            assert!(matches!(
                &spec.actions[4],
                Action::SwipeAt(100.0, 200.0, 100.0, 20.0)
            ));
            assert!(matches!(&spec.actions[6], Action::Type(t) if t == "hi"));
            assert!(matches!(
                &spec.actions[8],
                Action::WaitForNamed { target, timeout }
                    if target == "result" && *timeout == crate::time::Duration::from_millis(750)
            ));
            assert!(matches!(
                &spec.actions[9],
                Action::Expect(SemanticExpectation { target, role: Some(role), busy: Some(false), timeout: Some(timeout), .. })
                    if target == "result" && role == "static_text"
                        && *timeout == crate::time::Duration::from_secs(1)
            ));
            assert!(matches!(
                &spec.actions[10],
                Action::Perform { target, action: crate::a11y::Action::SetValue(value), .. }
                    if target == "name" && value == "Ada"
            ));
            assert!(matches!(
                &spec.actions[11],
                Action::Perform {
                    action: crate::a11y::Action::SetSelection {
                        anchor: 0,
                        focus: 3
                    },
                    ..
                }
            ));
            assert!(matches!(
                &spec.actions[12],
                Action::Perform {
                    target,
                    semantic_target: Some(semantic_target),
                    action: crate::a11y::Action::Expand,
                } if target == "workspace-tree" && semantic_target == "node:2"
            ));
        }

        // Case `an_unknown_action_is_rejected_rather_than_ignored`.
        {
            let err = parse_spec(r#"{"actions":[{"teleport":"x"}]}"#).unwrap_err();
            assert!(err.contains("teleport"), "{err}");
        }

        // Case `action_descriptions_locate_failures_without_echoing_entered_values`.
        {
            let typed = action_description(&Action::Type("private value".into()));
            assert_eq!(typed, "type 13 character(s)");
            assert!(!typed.contains("private"));

            let set = action_description(&Action::Perform {
                target: "password".into(),
                semantic_target: None,
                action: crate::a11y::Action::SetValue("hunter2".into()),
            });
            assert_eq!(set, "perform set_value on \"password\"");
            assert!(!set.contains("hunter2"));

            let expected = action_description(&Action::Expect(
                SemanticExpectation::named("items").semantic_target("row:42"),
            ));
            assert_eq!(expected, "expect \"items\" / \"row:42\"");
        }

        // Case `malformed_headless_viewports_and_actions_are_rejected`.
        {
            for raw in [
                r#"{"viewport":[0,600]}"#,
                r#"{"viewport":[800]}"#,
                r#"{"viewport":"800x600"}"#,
                r#"{"actions":[{"key":"Enter","type":"oops"}]}"#,
                r#"{"actions":[{"perform":{"target":"name","action":"set_value"}}]}"#,
                r#"{"actions":[{"perform":{"target":"name","action":"focus","value":"extra"}}]}"#,
                r#"{"actions":[{"perform":{"target":"name","action":"set_selection","anchor":-1,"focus":0}}]}"#,
                r#"{"actions":[{"wait_for_named":{"target":"result","timeout_ms":0}}]}"#,
                "[]",
            ] {
                assert!(parse_spec(raw).is_err(), "accepted {raw}");
            }
        }

        // Case `unknown_headless_fields_fail_instead_of_doing_nothing`.
        {
            let error = parse_spec(r#"{"steps":[]}"#).unwrap_err();
            assert!(error.contains("unknown field \"steps\""), "{error}");
            assert!(error.contains("actions"), "{error}");
        }

        // Case `headless_outputs_create_explicit_nested_parent_directories`.
        {
            let temp = tempfile::tempdir().expect("temporary output root");
            let output = temp.path().join("artifacts/audit/inspect.json");
            write_headless_output(&output, b"{}\n").expect("write nested output");
            assert_eq!(std::fs::read_to_string(output).unwrap(), "{}\n");
        }
    }

    #[test]
    fn pointer_touch_and_wait_actions_reach_named_targets_or_fail() {
        // Case `clicking_a_named_target_reaches_the_widget`:
        // A button click must reach the widget through the same
        // `handle_event` the live app calls.
        {
            let button = Button::new("btn", "Save", None);
            let mut driver = Driver::new(State::new(), Box::new(button), (400, 200));
            let targets = driver.targets();
            assert!(
                targets.keys().any(|k| k.starts_with("Button")),
                "the button must be addressable, got {:?}",
                targets.keys().collect::<Vec<_>>()
            );
            let name = targets
                .keys()
                .find(|k| k.starts_with("Button"))
                .unwrap()
                .clone();
            driver.click_named(&name).expect("click lands");
        }

        // Case `touch_profile_enlarges_and_activates_the_same_button`.
        {
            use std::cell::Cell;
            use std::rc::Rc;

            let fired = Rc::new(Cell::new(0));
            let fired_by_button = Rc::clone(&fired);
            let button = Button::new("save", "Save", None)
                .fit_content(32.0)
                .on_click(move || fired_by_button.set(fired_by_button.get() + 1));
            let mut driver = Driver::new(State::new(), Box::new(button), (400, 200));

            let pointer_rect = driver.targets()["save"];
            assert_eq!(pointer_rect.height(), 32.0);

            driver.set_interaction_environment(crate::InteractionEnvironment::TOUCH);
            let touch_rect = driver.targets()["save"];
            assert_eq!(touch_rect.height(), 48.0);
            driver.tap_named("save").expect("touch lands");
            assert_eq!(fired.get(), 1);
            assert_eq!(
                driver.interaction_environment(),
                crate::InteractionEnvironment::TOUCH
            );

            driver.key("Enter");
            assert_eq!(
                driver.interaction_environment().pointer_precision,
                crate::PointerPrecision::Coarse,
                "keyboard use must not make a hybrid touchscreen's targets small"
            );
        }

        // Case `host_touch_routing_reaches_pointer_only_controls_and_cancels_swipes`.
        {
            use automerge::ReadDoc;

            let checkbox = Checkbox::new("remember", "remember").with_id("remember");
            let mut driver = Driver::new(State::new(), Box::new(checkbox), (400, 200));
            driver.set_interaction_environment(crate::InteractionEnvironment::TOUCH);
            let rect = driver.targets()["remember"];
            assert_eq!((rect.width(), rect.height()), (48.0, 48.0));

            let center = rect.center();
            self::send_touch(
                &mut driver,
                winit::event::TouchPhase::Started,
                center.x,
                center.y,
            );
            self::send_touch(
                &mut driver,
                winit::event::TouchPhase::Moved,
                center.x + 30.0,
                center.y,
            );
            self::send_touch(
                &mut driver,
                winit::event::TouchPhase::Ended,
                center.x + 30.0,
                center.y,
            );
            assert!(
                driver
                    .state_mut()
                    .doc
                    .get(&automerge::ROOT, "remember")
                    .unwrap()
                    .is_none(),
                "a drag beyond tap slop must not toggle on pointer-down"
            );

            driver.tap_named("remember").expect("touch lands");
            let (value, _) = driver
                .state_mut()
                .doc
                .get(&automerge::ROOT, "remember")
                .unwrap()
                .unwrap();
            assert_eq!(value.as_bool(), Some(true));
        }

        // Case `pointer_and_touch_preserve_keyboard_family_and_discovered_devices`.
        {
            let mut driver = Driver::new(
                State::new(),
                Box::new(Button::new("save", "Save", None)),
                (400, 200),
            );
            let initial = crate::InteractionEnvironment {
                keyboard_family: crate::interaction::KeyboardFamily::Apple,
                available: crate::interaction::InputAvailability::NONE
                    .with(crate::interaction::InputDevice::Keyboard)
                    .with(crate::interaction::InputDevice::Pointer)
                    .with(crate::interaction::InputDevice::Touch)
                    .with(crate::interaction::InputDevice::Controller),
                ..crate::InteractionEnvironment::default()
            };
            driver.set_interaction_environment(initial);
            driver.scroll(0.0, -1.0);
            let pointer = driver.interaction_environment();
            assert_eq!(pointer.keyboard_family, initial.keyboard_family);
            assert_eq!(pointer.available, initial.available);
            assert_eq!(pointer.pointer_precision, crate::PointerPrecision::Fine);
            send_touch(&mut driver, winit::event::TouchPhase::Started, 10.0, 10.0);
            let touch = driver.interaction_environment();
            assert_eq!(touch.keyboard_family, initial.keyboard_family);
            assert_eq!(touch.available, initial.available);
            assert_eq!(touch.pointer_precision, crate::PointerPrecision::Coarse);
        }

        // Case `clicking_a_missing_target_is_an_error`.
        {
            let mut driver = Driver::new(
                State::new(),
                Box::new(Button::new("b", "B", None)),
                (400, 200),
            );
            let err = driver.click_named("no_such_widget").unwrap_err();
            assert!(err.contains("no_such_widget"), "{err}");
        }

        // Case `wait_action_succeeds_for_a_named_widget_and_reports_bounded_timeout`.
        {
            let mut driver = Driver::new(
                State::new(),
                Box::new(Button::new("ready", "Ready", None)),
                (400, 200),
            );
            driver
                .perform(&Action::WaitForNamed {
                    target: "ready".into(),
                    timeout: crate::time::Duration::from_millis(10),
                })
                .expect("an already-present target succeeds immediately");

            let error = driver
                .perform(&Action::WaitForNamed {
                    target: "never-appears".into(),
                    timeout: crate::time::Duration::from_millis(1),
                })
                .unwrap_err();
            assert!(error.contains("1 ms"), "{error}");
            assert!(error.contains("never-appears"), "{error}");
        }
    }

    fn send_touch(driver: &mut Driver, phase: winit::event::TouchPhase, x: f64, y: f64) {
        driver.send(&WindowEvent::Touch(winit::event::Touch {
            device_id: winit::event::DeviceId::dummy(),
            phase,
            location: winit::dpi::PhysicalPosition::new(x, y),
            force: None,
            id: 99,
        }));
    }

    #[test]
    fn a_tap_seats_the_caret_and_a_double_tap_selects_the_word() {
        let tapped = |taps: usize| {
            let mut driver = Driver::new_with_controlled_inputs(
                State::new(),
                Box::new(
                    TextInput::local()
                        .with_id("field")
                        .with_initial_value("hello world"),
                ),
                (400, 200),
                Theme::dark(),
                crate::font_catalog::fixture_renderer(),
                crate::permissions::UserId("fixture-user".into()),
            );
            let rect = driver.targets()["field"];
            // Inside "hello", clear of the field's padding.
            let (x, y) = (rect.x0 + 24.0, rect.center().y);
            for _ in 0..taps {
                driver.tap_at(x, y);
            }
            driver
                .access_node_named("field", None)
                .unwrap()
                .text_selection
                .expect("a focused field reports its caret")
        };

        let caret = tapped(1);
        assert_eq!(caret.anchor, caret.focus, "one tap selected {caret:?}");
        assert!(caret.focus > 0 && caret.focus < 5, "{caret:?}");
        assert_eq!(
            tapped(2),
            crate::a11y::AccessibleTextSelection {
                anchor: 0,
                focus: 5
            }
        );
    }

    #[test]
    fn semantic_actions_use_the_widget_path_and_report_mismatches() {
        // Case `semantic_set_value_and_focus_update_the_inspection_tree`.
        {
            let input = TextInput::local()
                .with_id("name-input")
                .with_initial_value("world")
                .with_accessible_name("Name");
            let mut driver = Driver::new_with_controlled_inputs(
                State::new(),
                Box::new(input),
                (400, 200),
                Theme::dark(),
                crate::font_catalog::fixture_renderer(),
                crate::permissions::UserId("fixture-user".into()),
            );

            driver
                .perform_named(
                    "name-input",
                    &crate::a11y::Action::SetValue("Ada".to_owned()),
                )
                .expect("set value through semantic action");
            driver
                .perform_named("name-input", &crate::a11y::Action::Focus)
                .expect("focus through semantic action");

            let semantics = driver
                .inspection()
                .root
                .semantics
                .expect("text input semantics");
            assert_eq!(semantics.value.as_deref(), Some("Ada"));
            assert!(semantics.state.focused);
        }

        // Case `semantic_document_edit_is_immediately_undoable`.
        {
            let input = TextInput::local()
                .with_id("title-input")
                .with_on_change_path("title");
            let mut driver = Driver::new_with_controlled_inputs(
                State::new(),
                Box::new(input),
                (400, 200),
                Theme::dark(),
                crate::font_catalog::fixture_renderer(),
                crate::permissions::UserId("task-4-fixture-author".into()),
            );

            driver
                .perform_named(
                    "title-input",
                    &crate::a11y::Action::SetValue("Project Atlas".to_owned()),
                )
                .expect("set durable value through semantic action");
            driver
                .state_mut()
                .undo(None, None)
                .expect("semantic mutation records one undo boundary");
            driver.relayout();

            let semantics = driver
                .inspection()
                .root
                .semantics
                .expect("text input semantics");
            assert_eq!(semantics.value.as_deref(), Some(""));
        }

        // Case `semantic_activate_uses_the_widget_action_path`.
        {
            use std::cell::Cell;
            use std::rc::Rc;

            let fired = Rc::new(Cell::new(0));
            let observed = Rc::clone(&fired);
            let button = Button::new("save", "Save", None).on_click(move || {
                observed.set(observed.get() + 1);
            });
            let mut driver = Driver::new_with_controlled_inputs(
                State::new(),
                Box::new(button),
                (400, 200),
                Theme::dark(),
                crate::font_catalog::fixture_renderer(),
                crate::permissions::UserId("task-4-fixture-author".into()),
            );

            driver
                .perform_named("save", &crate::a11y::Action::Activate)
                .expect("activate button");
            assert_eq!(fired.get(), 1);
        }

        // Case `unsupported_semantic_action_fails_loudly`.
        {
            let mut driver = Driver::new(
                State::new(),
                Box::new(Button::new("save", "Save", None)),
                (400, 200),
            );
            let error = driver
                .perform_named("save", &crate::a11y::Action::Increment)
                .unwrap_err();
            assert!(error.contains("does not support"), "{error}");
            assert!(error.contains("save"), "{error}");
        }

        // Case `semantic_expectation_reports_all_mismatches_and_addresses_model_rows`.
        {
            let root = TreeNode::branch(1, "Root", (), vec![TreeNode::leaf(2, "Child", ())]);
            let tree = TreeView::new(root).with_expanded([1]).with_id("items-tree");
            let mut driver = Driver::new(State::new(), Box::new(tree), (500, 300));
            driver
                .perform_semantic_named(
                    "items-tree",
                    Some("node:2"),
                    &crate::a11y::Action::Activate,
                )
                .expect("select child row");
            driver
                .expect(
                    &SemanticExpectation::named("items-tree")
                        .semantic_target("node:2")
                        .role(crate::a11y::Role::TreeItem)
                        .name("Child")
                        .selected(true)
                        .enabled(true),
                )
                .expect("semantic row matches");

            let error = driver
                .expect(
                    &SemanticExpectation::named("items-tree")
                        .semantic_target("node:2")
                        .name("Wrong")
                        .selected(false),
                )
                .unwrap_err();
            assert!(error.contains("name expected"), "{error}");
            assert!(
                error.contains("selected expected false, got true"),
                "{error}"
            );

            let timeout = driver
                .expect(
                    &SemanticExpectation::named("items-tree")
                        .semantic_target("node:2")
                        .name("Still wrong")
                        .within(crate::time::Duration::from_millis(2)),
                )
                .unwrap_err();
            assert!(timeout.contains("timed out after 2 ms"), "{timeout}");
            assert!(timeout.contains("name expected"), "{timeout}");
            assert!(timeout.contains("Still wrong"), "{timeout}");
        }

        // Case `semantic_tree_row_actions_use_stable_model_ids`.
        {
            let root = TreeNode::branch(1, "Root", (), vec![TreeNode::leaf(2, "Child", ())]);
            let tree = TreeView::new(root)
                .with_id("items-tree")
                .with_accessible_name("Items");
            let mut driver = Driver::new(State::new(), Box::new(tree), (640, 360));

            driver
                .perform_semantic_named("items-tree", Some("node:1"), &crate::a11y::Action::Focus)
                .expect("focus root row");
            driver
                .perform_semantic_named("items-tree", Some("node:1"), &crate::a11y::Action::Expand)
                .expect("expand root row");
            driver
                .perform_semantic_named(
                    "items-tree",
                    Some("node:2"),
                    &crate::a11y::Action::Activate,
                )
                .expect("activate child row");

            let semantics = driver.inspection().root.semantics.expect("tree semantics");
            let root = semantics
                .children
                .iter()
                .find(|node| node.action_target.as_deref() == Some("node:1"))
                .expect("root semantic row");
            let child = semantics
                .children
                .iter()
                .find(|node| node.action_target.as_deref() == Some("node:2"))
                .expect("child semantic row");
            assert_eq!(root.state.expanded, Some(true));
            assert!(child.state.selected);
            assert_eq!(semantics.value.as_deref(), Some("Child"));

            driver
                .perform_semantic_named(
                    "items-tree",
                    Some("node:1"),
                    &crate::a11y::Action::Collapse,
                )
                .expect("collapse root row");
            let semantics = driver.inspection().root.semantics.expect("tree semantics");
            assert_eq!(semantics.children[0].state.expanded, Some(false));
            assert_eq!(semantics.value.as_deref(), Some("Child"));
        }
    }

    #[test]
    fn inspection_and_dump_reflect_geometry_semantics_and_document() {
        // Case `inspection_combines_widget_geometry_and_redacted_semantics`.
        {
            let root = Column::new()
                .push(Box::new(
                    TextInput::local()
                        .with_initial_value("swordfish")
                        .with_secret(true)
                        .with_accessible_name("Password"),
                ))
                .push(Box::new(Button::new("save", "Save", None)));
            let driver = Driver::new(State::new(), Box::new(root), (640, 360));
            let json = driver.inspection_json().expect("inspection JSON");
            let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");

            assert_eq!(value["viewport"], serde_json::json!([640, 360]));
            assert_eq!(value["root"]["widget"], "Column");
            assert_eq!(value["root"]["children"][0]["widget"], "TextInput");
            assert_eq!(
                value["root"]["children"][0]["semantics"]["name"],
                "Password"
            );
            assert_eq!(
                value["root"]["children"][0]["semantics"]["state"]["protected"],
                true
            );
            assert_eq!(value["root"]["children"][1]["test_id"], "save");
            assert!(
                value["root"]["children"][1]["rect"]
                    .as_array()
                    .is_some_and(|rect| rect.len() == 4)
            );
            assert!(!json.contains("swordfish"), "secret leaked: {json}");
        }

        // Case `inspection_retains_virtualized_semantic_children`.
        {
            let root = TreeNode::branch(1, "Root", (), vec![TreeNode::leaf(2, "Child", ())]);
            let tree = TreeView::new(root)
                .with_expanded([1])
                .with_accessible_name("Items");
            let driver = Driver::new(State::new(), Box::new(tree), (640, 360));
            let inspection = driver.inspection();
            let semantics = inspection.root.semantics.expect("tree semantics");

            assert_eq!(semantics.role, crate::a11y::Role::Tree);
            assert!(
                semantics.children.len() >= 2,
                "visible tree rows remain nested semantic children"
            );
            assert_eq!(semantics.children[0].role, crate::a11y::Role::TreeItem);
            assert_eq!(
                semantics.children[0].action_target.as_deref(),
                Some("node:1")
            );
            assert!(
                semantics.children[0].bounds.is_some(),
                "inspection includes the visible row's logical geometry"
            );
        }

        // Case `dump_json_reflects_the_document`.
        {
            use automerge::transaction::Transactable;
            let mut state = State::new();
            state
                .doc
                .put(&automerge::ROOT, "greeting", "hello")
                .unwrap();
            let mut driver = Driver::new(state, Box::new(Button::new("b", "B", None)), (400, 200));
            let json = driver.dump_json();
            assert!(json.contains("greeting"), "{json}");
            assert!(json.contains("hello"), "{json}");
        }
    }

    #[test]
    fn local_control_results_report_refusal_confirmation_and_success() {
        // Case `local_control_result_does_not_broadcast_into_component_siblings`.
        {
            use crate::application_test_support::ProbeHost;
            use crate::widget::{Column, TileHost};
            for closed in [false, true] {
                let owner = ProbeHost::new();
                let (component, sibling_seen) =
                    crate::widget::control_result_probe::component(&owner, 11, false);
                let (legacy, legacy_seen) = crate::widget::control_result_probe::Probe::new(false);
                let root = Column::new()
                    .push(Box::new(legacy))
                    .push(Box::new(TileHost::from_component(component)));
                let mut host = Driver::new_with_controlled_inputs(
                    State::new(),
                    Box::new(root),
                    (400, 200),
                    Theme::dark(),
                    crate::font_catalog::fixture_renderer(),
                    crate::permissions::UserId("reader".into()),
                );
                if closed {
                    host.close_components();
                }
                legacy_seen.enqueue("missing");
                host.dispatch_control_invocations();
                assert_eq!(legacy_seen.results.borrow().len(), 1);
                assert!(
                    sibling_seen.results.borrow().is_empty(),
                    "legacy feedback must not enter bound sibling"
                );
                assert_eq!(sibling_seen.calls.get(), 0);
            }
        }

        // Case `bound_local_control_result_reports_permission_refusal_and_success`.
        {
            use crate::application_test_support::{ProbeHost, runner_inputs};
            for accepted in [false, true] {
                let owner = ProbeHost::new();
                let (component, observed) =
                    crate::widget::control_result_probe::component(&owner, 11, accepted);
                let binding = component.callback_binding();
                let mut host =
                    Driver::from_component(component, (400, 200), runner_inputs()).unwrap();
                observed.enqueue("run");
                host.dispatch_control_invocations();
                assert_eq!(observed.calls.get(), 1);
                let expected = if accepted {
                    crate::widget::ResourceOperationStart::Accepted
                } else {
                    crate::widget::ResourceOperationStart::Completed(
                        tilo_actions::ActionOutput::default(),
                    )
                };
                assert_eq!(*observed.results.borrow(), vec![Ok(expected)]);
                observed.enqueue("run");
                *binding.permissions.borrow_mut() = PermissionContext::from_capabilities(
                    collab_types::caps::Actor::person(
                        Some(collab_types::role::Role::Viewer),
                        "reader",
                    ),
                    Vec::new(),
                );
                let before = owner
                    .records
                    .read(&binding.checkout.id, automerge::AutoCommit::base_heads)
                    .unwrap();
                host.dispatch_control_invocations();
                assert_eq!(observed.calls.get(), 1, "revoked permission blocks handler");
                assert_eq!(
                    owner
                        .records
                        .read(&binding.checkout.id, automerge::AutoCommit::base_heads)
                        .unwrap(),
                    before
                );
                assert_eq!(observed.results.borrow().len(), 2);
                assert!(matches!(observed.results.borrow().last(), Some(Err(_))));
            }
        }

        // Case `local_control_result_keeps_confirmation_as_refusal`.
        {
            let (mut probe, observed) = crate::widget::control_result_probe::Probe::new(false);
            probe.confirmation = true;
            let mut host = Driver::new_with_controlled_inputs(
                State::new(),
                Box::new(probe),
                (400, 200),
                Theme::dark(),
                crate::font_catalog::fixture_renderer(),
                crate::permissions::UserId("reader".into()),
            );
            observed.enqueue("run");
            host.dispatch_control_invocations();
            assert_eq!(observed.calls.get(), 0);
            assert_eq!(
                *observed.results.borrow(),
                vec![Err(tilo_actions::DoorError::ConfirmationRequired)]
            );
        }

        // Case `local_control_result_reports_completed_and_accepted`.
        {
            use crate::widget::control_result_probe::Probe;
            for accepted in [false, true] {
                let (probe, observed) = Probe::new(accepted);
                let mut host = Driver::new_with_controlled_inputs(
                    State::new(),
                    Box::new(probe),
                    (400, 200),
                    Theme::dark(),
                    crate::font_catalog::fixture_renderer(),
                    crate::permissions::UserId("reader".into()),
                );
                observed.enqueue("run");
                host.dispatch_control_invocations();
                assert_eq!(observed.calls.get(), 1);
                let expected = if accepted {
                    crate::widget::ResourceOperationStart::Accepted
                } else {
                    crate::widget::ResourceOperationStart::Completed(
                        tilo_actions::ActionOutput::default(),
                    )
                };
                assert_eq!(*observed.results.borrow(), vec![Ok(expected)]);
                observed.enqueue("missing");
                host.dispatch_control_invocations();
                assert_eq!(observed.calls.get(), 1);
                assert!(matches!(
                    observed.results.borrow().last(),
                    Some(Err(tilo_actions::DoorError::Unknown(_)))
                ));
            }
        }

        // Case `local_control_result_reports_fresh_permission_refusal_without_mutation`.
        {
            use crate::widget::control_result_probe::Probe;
            let (probe, observed) = Probe::new(false);
            let mut host = Driver::new_with_controlled_inputs(
                State::new(),
                Box::new(probe),
                (400, 200),
                Theme::dark(),
                crate::font_catalog::fixture_renderer(),
                crate::permissions::UserId("reader".into()),
            );
            observed.enqueue("run");
            let readonly = PermissionContext::from_capabilities(
                collab_types::caps::Actor::person(Some(collab_types::role::Role::Viewer), "reader"),
                Vec::new(),
            );
            host.set_permission_ctx(readonly);
            let before = host.state.doc.base_heads();
            host.dispatch_control_invocations();
            assert_eq!(observed.calls.get(), 0, "permission checked before handler");
            assert_eq!(host.state.doc.base_heads(), before);
            assert_eq!(
                automerge::transaction::Transactable::pending_ops(&host.state.doc),
                0
            );
            assert!(matches!(
                observed.results.borrow().as_slice(),
                [Err(tilo_actions::DoorError::Unknown(_))]
            ));
            assert!(observed.pending.borrow().is_empty());
        }

        // Case `startup_control_waits_for_terminal_without_committing_typing`.
        {
            use crate::widget::control_result_probe::Probe;
            for succeeds in [true, false] {
                let (mut probe, observed) = Probe::new(true);
                probe.terminal_after = Some((3, succeeds));
                probe.dirty_typing = true;
                probe.owned_flush = Some(Ok(()));
                observed.enqueue("run");
                let inputs = crate::launch::LegacyLaunchInputs::new(
                    State::new(),
                    Box::new(probe),
                    crate::persist::DocWriter::disabled("fixture owns storage"),
                )
                .with_widget_owned_persistence();
                assert_eq!(
                    run_initialized_with_history(&Spec::default(), inputs, Theme::dark(), None),
                    i32::from(!succeeds)
                );
                assert_eq!(observed.calls.get(), 1);
                assert_eq!(observed.polls.get(), 3);
                assert_eq!(
                    observed.commits.get(),
                    0,
                    "idle admission must not commit unrelated pending typing"
                );
                assert_eq!(observed.flushes.get(), 1);
            }
        }
    }

    struct LateRecordProbe {
        frames: usize,
        checkout: crate::record::RecordCheckout,
        ready: bool,
    }
    impl Widget for LateRecordProbe {
        crate::provides!(dyn crate::record::capability::RecordScoped);

        fn update(
            &mut self,
            doc: &mut crate::widget::AppModel,
            path: &crate::widget::ModelPath,
        ) -> bool {
            let (doc, _) = crate::doc::document(doc, path);
            self.frames = self.frames.saturating_add(1);
            if self.frames == 2 {
                doc.put(automerge::ROOT, "loaded", "initial content")
                    .unwrap();
            }
            self.frames == 2
        }
        fn sync(&mut self, doc: &mut crate::widget::AppModel, path: &crate::widget::ModelPath) {
            let (doc, _) = crate::doc::document(doc, path);
            self.ready =
                crate::record::identity(doc).is_some_and(|record| record.id == self.checkout.id);
        }
        fn test_id(&self) -> Option<&str> {
            self.ready.then_some("record-ready")
        }
        fn build_layout(
            &mut self,
            tree: &mut TaffyTree<()>,
            _: &Theme,
            map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = tree.new_leaf(taffy::Style::default()).unwrap();
            map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }
        fn apply_layout(&mut self, _: &TaffyTree<()>, _: NodeId, _: Point) {}
        fn draw(
            &mut self,
            _: &mut Scene,
            _: Affine,
            _: &mut TextRenderer,
            _: &Theme,
            _: &PresenceCtx,
        ) {
        }
        fn name(&self) -> &'static str {
            "LateRecordProbe"
        }
        fn get_rect(&self) -> vello::kurbo::Rect {
            vello::kurbo::Rect::ZERO
        }
        fn perform_action(
            &mut self,
            action: &crate::a11y::Action,
            doc: &mut crate::widget::AppModel,
            path: &crate::widget::ModelPath,
        ) -> bool {
            use automerge::ReadDoc;
            let (doc, _) = crate::doc::document(doc, path);

            if self.ready && matches!(action, crate::a11y::Action::Activate) {
                doc.put(automerge::ROOT, "payload", "late record content")
                    .unwrap();
                let edits = doc
                    .get(automerge::ROOT, "edit_count")
                    .unwrap()
                    .and_then(|(value, _)| value.as_u64())
                    .unwrap_or(0);
                doc.put(automerge::ROOT, "edit_count", edits + 1).unwrap();
                true
            } else {
                false
            }
        }
    }

    impl crate::record::capability::RecordScoped for LateRecordProbe {
        fn record_intent(&self) -> Option<crate::record::RecordCheckout> {
            (self.frames >= 2).then(|| self.checkout.clone())
        }
    }

    /// A standalone app whose root names a record: the runner checks that
    /// record out as its own State, so the root is not a separate owner.
    struct RecordRootProbe {
        checkout: crate::record::RecordCheckout,
    }
    impl Widget for RecordRootProbe {
        crate::provides!(
            dyn crate::resources::PublishesResources,
            dyn crate::record::capability::RecordScoped
        );

        crate::testing::inert_widget_methods!(
            leaf_layout,
            apply_layout,
            draw,
            get_rect,
            name = "RecordRootProbe"
        );
    }

    impl crate::record::capability::RecordScoped for RecordRootProbe {
        fn record_intent(&self) -> Option<crate::record::RecordCheckout> {
            Some(self.checkout.clone())
        }
    }

    impl crate::resources::PublishesResources for RecordRootProbe {
        fn resources(&self, _: &automerge::AutoCommit) -> Vec<tilo_actions::Resource> {
            vec![tilo_actions::Resource::new(
                "probe.document:1",
                "probe.document",
                "Probe document",
            )]
        }
    }

    #[test]
    fn standalone_record_backed_root_publishes_its_resources() {
        use crate::record::{RecordCheckout, RecordId, RecordKind, RecordSet};
        let checkout = RecordCheckout::new(
            RecordId::for_file("standalone-record-root"),
            RecordKind::new("document"),
        );
        let root = RecordRootProbe {
            checkout: checkout.clone(),
        };
        let mut harness = FrameHarness::new(State::new(), Box::new(root), (320, 240))
            .with_records(RecordSet::in_memory(), automerge::AutoCommit::new());
        harness.relayout();
        assert_eq!(
            crate::record::identity(&harness.state_mut().doc).map(|record| record.id),
            Some(checkout.id),
            "the runner checked the root's record out as its State"
        );
        let resources = harness.resources();
        assert!(
            resources
                .iter()
                .any(|resource| resource.id.as_str() == "probe.document:1"),
            "Ctrl+K sees the record-backed root's resources: {:?}",
            resources.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn late_record_intent_uses_record_store_and_preserves_room_index() {
        const CHILD: &str = "TILO_LATE_RECORD_TEST_CHILD";
        const PACKAGE: &str = "so.tilo.late-record-test";
        if let Ok(case) = std::env::var(CHILD) {
            use crate::record::{RecordCheckout, RecordId, RecordKind, RecordSet};
            use automerge::ReadDoc;
            let checkout = RecordCheckout::new(
                RecordId::for_file("late-test-document"),
                RecordKind::new("document"),
            );
            let blocked = case == "blocked";
            if blocked {
                // Block only the record namespace; the room index remains writable.
                let room_store = app_state::DocStore::open_room(
                    PACKAGE,
                    &std::env::var("TILO_WORKSPACE_ID").unwrap(),
                )
                .unwrap();
                let records = room_store.dir().join(app_state::RECORDS_DIR);
                std::fs::write(records, b"blocked record namespace").unwrap();
            }
            let root = LateRecordProbe {
                frames: 0,
                checkout: checkout.clone(),
                ready: false,
            };
            let spec = Spec {
                actions: vec![
                    Action::WaitForNamed {
                        target: "record-ready".into(),
                        timeout: crate::time::Duration::from_secs(2),
                    },
                    Action::Perform {
                        target: "record-ready".into(),
                        semantic_target: None,
                        action: crate::a11y::Action::Activate,
                    },
                ],
                ..Spec::default()
            };
            let exit = run(&spec, Box::new(root), PACKAGE);
            if blocked {
                assert_eq!(exit, 3, "unavailable record writer must not report success");
                return;
            }
            assert_eq!(exit, 0);
            let (mut room, _) = crate::persist::open(PACKAGE);
            assert!(
                room.doc.get(automerge::ROOT, "payload").unwrap().is_none(),
                "record body must not replace room index"
            );
            assert!(
                room.doc.get(automerge::ROOT, "loaded").unwrap().is_none(),
                "late loaded bytes belong to the record, not its index"
            );
            assert!(crate::record::room::holds(&room.doc, &checkout.id));
            let mut records = RecordSet::open(PACKAGE);
            let mut state = State::new();
            assert!(records.checkout(&mut state, Some(&mut room.doc), &checkout));
            assert_eq!(
                state
                    .doc
                    .get(automerge::ROOT, "payload")
                    .unwrap()
                    .unwrap()
                    .0
                    .as_str(),
                Some("late record content")
            );
            return;
        }
        for case in ["success", "blocked"] {
            let temp = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "headless::tests::late_record_intent_uses_record_store_and_preserves_room_index",
                    "--nocapture",
                ])
                .env(CHILD, case)
                .env("XDG_DATA_HOME", temp.path())
                .env("TILO_PACKAGE_ID", PACKAGE)
                .env("TILO_WORKSPACE_ID", "22222222-2222-4222-8222-222222222222")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            // libtest exits 0 when `--exact` selects nothing; require the child ran.
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed"),
                "{case}: the child ran no test: {}",
                String::from_utf8_lossy(&output.stdout)
            );
        }
    }

    #[test]
    fn automatic_record_restarts_keep_distinct_index_and_record_authors_and_fail_closed() {
        const CHILD: &str = "TILO_RECORD_RESTART_TEST_CHILD";
        const PACKAGE: &str = "so.tilo.record-restart-test";
        const ROOM: &str = "33333333-3333-4333-8333-333333333333";
        if std::env::var_os(CHILD).is_some() {
            use crate::record::{RecordCheckout, RecordId, RecordKind, RecordSet};
            use automerge::{ActorId, AutoCommit, ROOT, ReadDoc};
            let checkout = RecordCheckout::new(RecordId::minted(), RecordKind::new("document"));
            let mut original_record_actor = None;
            let mut original_index_actor = None;
            for expected in 1_u64..=3 {
                let root = LateRecordProbe {
                    frames: 0,
                    checkout: checkout.clone(),
                    ready: false,
                };
                let spec = Spec {
                    actions: vec![
                        Action::WaitForNamed {
                            target: "record-ready".into(),
                            timeout: crate::time::Duration::from_secs(2),
                        },
                        Action::Perform {
                            target: "record-ready".into(),
                            semantic_target: None,
                            action: crate::a11y::Action::Activate,
                        },
                    ],
                    ..Spec::default()
                };
                assert_eq!(
                    run(&spec, Box::new(root), PACKAGE),
                    0,
                    "actual headless restart {expected}"
                );
                let mut index_store = app_state::DocStore::open_with_manifest_id(PACKAGE).unwrap();
                let index = index_store.load().unwrap().unwrap();
                let mut record_store = app_state::DocStore::open_room_record(
                    PACKAGE,
                    ROOM,
                    &checkout.id.storage_key(),
                )
                .unwrap();
                let mut record = record_store.load().unwrap().unwrap();
                let stored_bytes = std::fs::read(record_store.document_path()).unwrap();
                AutoCommit::load(&stored_bytes)
                    .expect("all saved record bytes must be loadable, not a fallback");
                assert_eq!(
                    record.get(ROOT, "edit_count").unwrap().unwrap().0.as_u64(),
                    Some(expected)
                );
                assert!(index.get(ROOT, "edit_count").unwrap().is_none());
                assert!(crate::record::room::holds(&index, &checkout.id));
                assert_ne!(index_store.actor(), record_store.actor());
                if let Some(actor) = &original_record_actor {
                    assert_eq!(actor, record_store.actor());
                }
                if let Some(actor) = &original_index_actor {
                    assert_eq!(actor, index_store.actor());
                }
                original_record_actor = Some(record_store.actor().clone());
                original_index_actor = Some(index_store.actor().clone());
                // The successful edit, not just the loaded document cursor,
                // must carry the original record actor after admission.
                let head = record.get_heads();
                assert!(
                    head.iter()
                        .all(|hash| record.get_change_by_hash(hash).unwrap().actor_id()
                            == record_store.actor())
                );
            }

            for corrupt in [false, true] {
                let want = RecordCheckout::new(RecordId::minted(), RecordKind::new("document"));
                let mut shared =
                    AutoCommit::new().with_actor(ActorId::from(b"duplicate-writer".as_slice()));
                shared.put(ROOT, "seed", "base").unwrap();
                shared.commit();
                let mut incoming = shared.clone();
                let mut stored = shared;
                incoming.put(ROOT, "branch", "unsaved live edit").unwrap();
                incoming.commit();
                stored.put(ROOT, "branch", "original stored edit").unwrap();
                stored.commit();
                let mut store =
                    app_state::DocStore::open_room_record(PACKAGE, ROOM, &want.id.storage_key())
                        .unwrap();
                store.save(&mut stored).unwrap();
                let path = store.document_path().to_path_buf();
                drop(store);
                if corrupt {
                    std::fs::write(&path, b"invalid automerge original bytes").unwrap();
                }
                let bytes = std::fs::read(&path).unwrap();
                let mut records = RecordSet::open(PACKAGE);
                let mut state = State::from_doc(incoming);
                assert!(records.checkout(&mut state, None, &want));
                assert_eq!(
                    state.doc.get(ROOT, "branch").unwrap().unwrap().0.as_str(),
                    Some("unsaved live edit")
                );
                let error = records.try_flush(&mut state.doc).unwrap_err();
                assert!(
                    error.contains("stored record"),
                    "original record failure must reach checked flush: {error}"
                );
                drop(records);
                assert_eq!(
                    std::fs::read(&path).unwrap(),
                    bytes,
                    "failed read/merge must preserve exact original bytes"
                );
                if corrupt {
                    let spec = Spec {
                        actions: vec![Action::WaitForNamed {
                            target: "record-ready".into(),
                            timeout: crate::time::Duration::from_secs(2),
                        }],
                        ..Spec::default()
                    };
                    let root = LateRecordProbe {
                        frames: 0,
                        checkout: want,
                        ready: false,
                    };
                    assert_eq!(
                        run(&spec, Box::new(root), PACKAGE),
                        3,
                        "actual headless execution must report the refused record load"
                    );
                    assert_eq!(std::fs::read(&path).unwrap(), bytes);
                }
            }
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let stdout = temp.path().join("stdout.log");
        let stderr = temp.path().join("stderr.log");
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", "headless::tests::automatic_record_restarts_keep_distinct_index_and_record_authors_and_fail_closed", "--nocapture"])
            .env(CHILD, "1").env("XDG_DATA_HOME", temp.path())
            .env("TILO_PACKAGE_ID", PACKAGE).env("TILO_WORKSPACE_ID", ROOM)
            .stdout(std::fs::File::create(&stdout).unwrap()).stderr(std::fs::File::create(&stderr).unwrap());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let mut child = command.spawn().unwrap();
        let deadline = tilo_test_env::hang_deadline();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if crate::time::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("record restart regression timed out");
            }
            std::thread::sleep(crate::time::Duration::from_millis(20));
        };
        let stdout = std::fs::read_to_string(stdout).unwrap();
        assert!(
            status.success(),
            "{stdout}\n{}",
            std::fs::read_to_string(stderr).unwrap()
        );
        // libtest exits 0 when `--exact` selects nothing; require the child ran.
        assert!(
            stdout.contains("test result: ok. 1 passed"),
            "the child ran no test: {stdout}"
        );
    }

    #[test]
    fn app_focus_prefers_declared_default_without_invoking_its_action() {
        let activations = std::rc::Rc::new(std::cell::Cell::new(0));
        let mut driver = Driver::new_with_controlled_inputs(
            State::new(),
            Box::new(
                Column::new()
                    .push(Box::new(Button::new("toolbar", "Toolbar", None)))
                    .push(Box::new(ConsumingActivationControl {
                        activations: activations.clone(),
                        focused: false,
                        rect: Rect::ZERO,
                    })),
            ),
            (400, 200),
            Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("fixture-user".into()),
        );
        let order = driver.focus_order();
        assert_eq!(order.len(), 2);
        let target = driver.app_default_focus(&order);
        assert_eq!(target, order.get(1).copied());
        driver.set_focused(target);
        assert_eq!(activations.get(), 0);
    }

    #[test]
    fn app_focus_eligibility_excludes_disabled_and_hidden_controls() {
        let mut driver = Driver::new_with_controlled_inputs(
            State::new(),
            Box::new(
                Column::new()
                    .push(Box::new(Button::new("enabled", "Enabled", None)))
                    .push(Box::new(
                        Button::new("disabled", "Disabled", None).with_enabled(false),
                    )),
            ),
            (400, 200),
            Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("fixture-user".into()),
        );
        let enabled = driver.named_target_nodes()["enabled"];
        let disabled = driver.named_target_nodes()["disabled"];
        assert!(driver.app_focus_eligible(enabled));
        assert!(!driver.app_focus_eligible(disabled));
        let mut style = driver.taffy.style(enabled).unwrap().clone();
        style.display = taffy::Display::None;
        driver.taffy.set_style(enabled, style).unwrap();
        assert!(!driver.app_focus_eligible(enabled));
    }

    #[test]
    fn no_control_scope_preserves_group_shortcuts_without_background_input() {
        use crate::widget::{AppKind, TabDescriptor, TileContent, WindowGroup, WindowWidth};
        let group = |id| {
            Box::new(WindowGroup::new(
                id,
                vec![(
                    TabDescriptor::new(id, "No controls"),
                    TileContent::Stub {
                        kind: AppKind::Settings,
                        label: "Empty".into(),
                    },
                )],
                id,
                WindowWidth::Normal,
            ))
        };
        let mut previous = group(41);
        let mut destination = group(42);
        let previous_ptr = std::ptr::from_mut(previous.as_mut());
        let destination_ptr = std::ptr::from_mut(destination.as_mut());
        let mut driver = Driver::new_with_controlled_inputs(
            State::new(),
            Box::new(
                Column::new()
                    .push(Box::new(
                        TextInput::local()
                            .with_id("background")
                            .with_initial_value("retained"),
                    ))
                    .push(previous)
                    .push(destination),
            ),
            (800, 800),
            Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("fixture-user".into()),
        );
        driver
            .perform_named("background", &crate::a11y::Action::Focus)
            .unwrap();
        driver.type_text(" draft");
        let value = driver
            .inspect_node(driver.named_target_nodes()["background"], |widget| {
                widget.accessibility().unwrap().value
            })
            .unwrap();
        driver.prepare_app_activation(42);
        driver.activate_app_focus(42);
        assert!(driver.focused().is_none());
        // A background app may retain a flag; it must not receive shortcut fallback.
        let background = driver.named_target_nodes()["background"];
        driver
            .mutate_node(background, |widget| widget.set_focus(true))
            .unwrap();
        driver.set_modifiers(&["ctrl".into(), "shift".into()]);
        driver.key("p");
        // SAFETY: the boxed groups remain retained by driver; these reads end before its next call.
        unsafe {
            assert!(
                (&*destination_ptr).bar_for_tests().is_pinned(42),
                "selected no-control group still supports Ctrl+Shift+P"
            );
            assert!(
                !(&*previous_ptr).bar_for_tests().is_pinned(41),
                "shortcut must not visit a previous group"
            );
        }
        for modifier in ["ctrl", "super"] {
            driver.set_modifiers(&[modifier.into()]);
            driver.key("z");
        }
        driver.set_modifiers(&[]);
        driver.type_text("must not leak");
        driver.send_host_event(&WindowEvent::Ime(winit::event::Ime::Commit(
            "nor IME".into(),
        )));
        assert!(driver.focused().is_none());
        assert_eq!(
            driver
                .inspect_node(driver.named_target_nodes()["background"], |widget| widget
                    .accessibility()
                    .unwrap()
                    .value)
                .unwrap(),
            value
        );
    }

    #[test]
    fn app_activation_without_controls_keeps_an_authoritative_empty_scope() {
        let input = TextInput::local()
            .with_id("background")
            .with_initial_value("unchanged");
        let mut driver = Driver::new_with_controlled_inputs(
            State::new(),
            Box::new(input),
            (400, 200),
            Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("fixture-user".into()),
        );
        driver
            .perform_named("background", &crate::a11y::Action::Focus)
            .unwrap();
        driver.prepare_app_activation(42);
        driver.activate_app_focus(42);
        assert!(driver.focused().is_none());
        // A retained editor flag in another instance must never regain authority.
        let node = driver.named_target_nodes()["background"];
        driver
            .mutate_node(node, |widget| widget.set_focus(true))
            .unwrap();
        driver.relayout();
        assert!(driver.focused().is_none());
        driver.send_host_event(&WindowEvent::Ime(winit::event::Ime::Commit("leak".into())));
        assert_eq!(
            driver.inspection().root.semantics.unwrap().value.as_deref(),
            Some("unchanged")
        );
    }

    #[test]
    fn input_ownership_token_changes_for_control_focus_but_not_passive_layout() {
        let mut driver = Driver::new_with_controlled_inputs(
            State::new(),
            Box::new(
                Column::new()
                    .push(Box::new(TextInput::local().with_id("first")))
                    .push(Box::new(TextInput::local().with_id("second"))),
            ),
            (400, 200),
            Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("fixture-user".into()),
        );
        driver
            .perform_named("first", &crate::a11y::Action::Focus)
            .unwrap();
        let first = driver.input_ownership_sequence();
        driver.relayout();
        assert_eq!(driver.input_ownership_sequence(), first);
        driver
            .perform_named("second", &crate::a11y::Action::Focus)
            .unwrap();
        assert_ne!(driver.input_ownership_sequence(), first);
    }

    #[test]
    fn inspection_identifies_keyboard_owner_among_locally_focused_editors() {
        let mut first = TextInput::local()
            .with_id("first")
            .with_accessible_name("First");
        let mut second = TextInput::local()
            .with_id("second")
            .with_accessible_name("Second");
        first.set_focus(true);
        second.set_focus(true);
        let mut driver = Driver::new_with_controlled_inputs(
            State::new(),
            Box::new(Column::new().push(Box::new(first)).push(Box::new(second))),
            (400, 200),
            Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("fixture-user".into()),
        );
        driver
            .perform_named("second", &crate::a11y::Action::Focus)
            .unwrap();
        // A mounted app's local restoration can keep its editor focused even
        // though the shared runner routes the keyboard to another app.
        let first_node = driver.named_target_nodes()["first"];
        driver
            .mutate_node(first_node, |widget| widget.set_focus(true))
            .unwrap();
        let snapshot = driver.inspection();
        assert!(
            snapshot.root.children[0]
                .semantics
                .as_ref()
                .unwrap()
                .state
                .focused,
            "apps can preserve their own local editor focus"
        );
        let wire = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(
            wire["keyboard_focus_owner"].as_str(),
            Some(snapshot.root.children[1].path.as_str())
        );
        driver
            .perform_named("first", &crate::a11y::Action::Focus)
            .unwrap();
        let snapshot = driver.inspection();
        assert_eq!(
            serde_json::to_value(&snapshot).unwrap()["keyboard_focus_owner"].as_str(),
            Some(snapshot.root.children[0].path.as_str())
        );

        let second_node = driver.named_target_nodes()["second"];
        driver
            .mutate_node(second_node, |widget| widget.set_focus(true))
            .unwrap();
        driver.set_focused(None);
        let snapshot = driver.inspection();
        assert!(
            snapshot.root.children[1]
                .semantics
                .as_ref()
                .unwrap()
                .state
                .focused,
            "a neighboring app can retain local focus after the runner clears its owner"
        );
        let wire = serde_json::to_value(&snapshot).unwrap();
        assert!(
            wire.as_object()
                .unwrap()
                .contains_key("keyboard_focus_owner"),
            "authoritative no-owner must not become a legacy snapshot with unknown ownership"
        );
        assert!(wire["keyboard_focus_owner"].is_null());
    }

    #[test]
    fn semantic_token_focus_rejects_unpublished_targets() {
        let mut driver = Driver::new_with_controlled_inputs(
            State::new(),
            Box::new(Button::new("save", "Save", None)),
            (400, 200),
            Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("fixture-user".into()),
        );
        assert!(
            driver
                .perform_semantic_named("save", Some("forged"), &crate::a11y::Action::Focus)
                .is_err()
        );
        assert!(driver.focused_access_node().is_none());
    }

    #[test]
    fn semantic_empty_replacement_clears_bound_text_and_has_one_undo_boundary() {
        let input = TextInput::local()
            .with_id("draft")
            .with_on_change_path("draft");
        let mut driver = Driver::new_with_controlled_inputs(
            State::new(),
            Box::new(input),
            (400, 200),
            Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("fixture-user".into()),
        );
        driver
            .perform_named("draft", &crate::a11y::Action::SetValue("Draft 🦀".into()))
            .unwrap();
        driver
            .perform_named("draft", &crate::a11y::Action::SetValue(String::new()))
            .unwrap();
        assert_eq!(
            driver
                .access_node_named("draft", None)
                .unwrap()
                .value
                .as_deref(),
            Some("")
        );
        driver.state_mut().undo(None, None).unwrap();
        driver.relayout();
        assert_eq!(
            driver
                .access_node_named("draft", None)
                .unwrap()
                .value
                .as_deref(),
            Some("Draft 🦀")
        );
    }
}

#[cfg(test)]
mod a11y_driver_tests {
    use super::*;
    use crate::a11y::{AccessNode, AccessState, Action, Role};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A canvas widget: paints its own contents, exposes them only as synthetic
    /// accessibility nodes, and implements only the routed action hook. The
    /// shape a timeline or a spreadsheet grid takes.
    struct Strip {
        items: Rc<RefCell<Vec<&'static str>>>,
        cursor: usize,
        focused: bool,
        activated: Rc<RefCell<Vec<String>>>,
    }

    impl Widget for Strip {
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _t: &crate::theme::Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let n = taffy.new_leaf(taffy::Style::default()).unwrap();
            node_map.insert(n, std::ptr::from_mut::<dyn Widget>(self));
            n
        }
        fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: NodeId, _o: vello::kurbo::Point) {}
        fn draw(
            &mut self,
            _sc: &mut Scene,
            _tr: vello::kurbo::Affine,
            _tx: &mut crate::text_render::TextRenderer,
            _th: &crate::theme::Theme,
            _pr: &PresenceCtx,
        ) {
        }
        fn name(&self) -> &'static str {
            "Strip"
        }
        fn get_rect(&self) -> Rect {
            Rect::new(0.0, 0.0, 300.0, 80.0)
        }
        fn is_focusable(&self) -> bool {
            true
        }
        fn set_focus(&mut self, f: bool) {
            self.focused = f;
        }
        fn is_focused(&self) -> bool {
            self.focused
        }
        fn accessibility(&self) -> Option<AccessNode> {
            Some(
                AccessNode::new(Role::Canvas)
                    .with_name("Strip")
                    .with_semantic_key("strip")
                    .with_state(AccessState::new().focusable(true).focused(self.focused))
                    .with_children(
                        self.items
                            .borrow()
                            .iter()
                            .enumerate()
                            .map(|(i, id)| {
                                AccessNode::new(Role::Button)
                                    .with_name(*id)
                                    .with_hashed_key(*id)
                                    .with_state(
                                        AccessState::new()
                                            .focusable(true)
                                            .focused(i == self.cursor),
                                    )
                            })
                            .collect(),
                    )
                    // This is the high-complexity custom-canvas path: one
                    // keyed tree becomes searchable/agent-driveable while AT
                    // still reaches the existing `perform_action_at` switch.
                    .with_keyed_action_routes("test-strip"),
            )
        }
        fn perform_action_at(
            &mut self,
            action: &Action,
            key_path: &[u64],
            _s: &mut crate::widget::AppModel,
            _p: &crate::widget::ModelPath,
        ) -> bool {
            let Some(&key) = key_path.first() else {
                return false;
            };
            let Some(i) = self
                .items
                .borrow()
                .iter()
                .position(|id| crate::a11y::hash_key(id) == key)
            else {
                return false;
            };
            let name = self.items.borrow()[i];
            match action {
                Action::Focus => {
                    self.cursor = i;
                    true
                }
                Action::Activate => {
                    self.cursor = i;
                    self.activated.borrow_mut().push(name.into());
                    true
                }
                _ => false,
            }
        }
    }

    fn driver() -> (Driver, Rc<RefCell<Vec<String>>>) {
        let (d, activated, _items) = driver_with_items();
        (d, activated)
    }

    #[allow(
        clippy::type_complexity,
        reason = "the fixture exposes separate activation and mutable item handles for production driver assertions"
    )]
    fn driver_with_items() -> (
        Driver,
        Rc<RefCell<Vec<String>>>,
        Rc<RefCell<Vec<&'static str>>>,
    ) {
        let activated: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let items: Rc<RefCell<Vec<&'static str>>> =
            Rc::new(RefCell::new(vec!["alpha", "beta", "gamma"]));
        let strip = Strip {
            items: items.clone(),
            cursor: 0,
            focused: false,
            activated: activated.clone(),
        };
        (
            Driver::new_with_controlled_inputs(
                State::new(),
                Box::new(strip),
                (800, 600),
                Theme::dark(),
                crate::font_catalog::fixture_renderer(),
                crate::permissions::UserId("fixture-user".into()),
            ),
            activated,
            items,
        )
    }

    #[test]
    fn a11y_driver_names_focuses_and_activates_widgetless_elements() {
        // Case `a_headless_test_can_name_an_element_that_has_no_widget`.
        {
            let (d, _a) = driver();
            for name in ["alpha", "beta", "gamma"] {
                assert!(
                    d.a11y_node_named(name).is_some(),
                    "an element a test cannot name is one a screen-reader user cannot find"
                );
            }
            assert_eq!(d.a11y_node_named("delta"), None);
        }

        // Case `a_headless_test_can_activate_one_element_inside_a_canvas`.
        {
            // The whole point: before, a canvas could be described in a headless
            // test but only clicked at coordinates, which activates whatever
            // happens to sit there.
            let (mut d, activated) = driver();
            let beta = d.a11y_node_named("beta").unwrap();
            d.perform_a11y_action(beta, &Action::Activate).unwrap();
            assert_eq!(*activated.borrow(), vec!["beta".to_string()]);

            let gamma = d.a11y_node_named("gamma").unwrap();
            d.perform_a11y_action(gamma, &Action::Activate).unwrap();
            assert_eq!(
                *activated.borrow(),
                vec!["beta".to_string(), "gamma".to_string()]
            );
        }

        // Case `focusing_an_element_moves_the_exported_focus_onto_it`.
        {
            let (mut d, _a) = driver();
            let gamma = d.a11y_node_named("gamma").unwrap();
            d.perform_a11y_action(gamma, &Action::Focus).unwrap();

            let (id, node) = d.a11y_focus().expect("something is focused");
            assert_eq!(id, gamma, "the reader sits on the element, not the canvas");
            assert_eq!(node.label(), Some("gamma"));
        }
    }

    #[test]
    fn a11y_node_references_survive_relayout_or_report_gone() {
        // Case `a_node_reference_survives_a_relayout`.
        {
            // The gap this closes: the host throws its TaffyTree away and rebuilds
            // it on every relayout, and ids used to be derived from taffy's
            // per-tree node ids — so every id in the exported tree changed on each
            // layout pass. An assistive technology holding a reference across a
            // repaint got an id that no longer resolved, or one that resolved to a
            // different element, which is worse than having no id at all.
            let (mut d, _a) = driver();
            let before: Vec<(String, accesskit::NodeId)> = d
                .a11y_nodes()
                .into_iter()
                .map(|(id, n)| (n.label().unwrap_or_default().to_string(), id))
                .collect();
            assert!(before.len() >= 4, "the canvas and its items are exported");

            // A layout pass, exactly as any frame does.
            d.relayout();

            let after: Vec<(String, accesskit::NodeId)> = d
                .a11y_nodes()
                .into_iter()
                .map(|(id, n)| (n.label().unwrap_or_default().to_string(), id))
                .collect();
            assert_eq!(
                before, after,
                "every exported id must be the same after a relayout"
            );
        }

        // Case `an_id_taken_before_a_relayout_still_operates_the_same_element`.
        {
            // Stability is only worth anything if the id still *works*: an AT holds
            // its reference across frames and then acts on it.
            let (mut d, activated) = driver();
            let beta = d.a11y_node_named("beta").unwrap();

            d.relayout();
            d.relayout();

            d.perform_a11y_action(beta, &Action::Activate).unwrap();
            assert_eq!(
                *activated.borrow(),
                vec!["beta".to_string()],
                "the reference must still operate the element it named"
            );
        }

        // Case `a_held_reference_follows_its_element_when_an_earlier_one_is_removed`.
        {
            // Identity keys, observed. Removing an earlier element renumbers every
            // element after it. Keyed by ordinal, a reference an assistive
            // technology took before the removal would afterwards name a DIFFERENT
            // element — it would operate the wrong one, silently. Keyed by
            // identity, it keeps naming the element it was taken from.
            let (mut d, activated, items) = driver_with_items();
            let gamma = d.a11y_node_named("gamma").expect("gamma is addressable");

            // Remove the first element, so gamma slides from third place to second.
            items.borrow_mut().remove(0);
            d.relayout();
            assert!(d.a11y_node_named("alpha").is_none(), "alpha is gone");
            assert!(
                d.a11y_node_named("gamma").is_some(),
                "gamma is still listed"
            );

            d.perform_a11y_action(gamma, &Action::Activate)
                .expect("the reference must still resolve");
            assert_eq!(
                *activated.borrow(),
                vec!["gamma".to_string()],
                "a held reference must operate the element it named, not the one \
             that took its place"
            );
        }

        // Case `an_id_that_names_nothing_is_reported_not_silently_retargeted`.
        {
            let (mut d, activated) = driver();
            let real = d.a11y_node_named("alpha").unwrap();
            let stale = accesskit::NodeId(real.0 ^ 0xdead_beef);
            let err = d
                .perform_a11y_action(stale, &Action::Activate)
                .expect_err("a stale id must be an error, not a guess");
            assert!(err.contains("no accessibility node"), "got {err}");
            assert!(activated.borrow().is_empty());
        }
    }

    fn control_named(d: &Driver, name: &str) -> Control {
        d.controls()
            .into_iter()
            .find(|control| control.name.as_deref() == Some(name))
            .unwrap_or_else(|| panic!("no control named {name}"))
    }

    #[test]
    fn every_on_screen_control_is_listed_with_what_it_answers() {
        let (d, _a) = driver();
        let beta = control_named(&d, "beta");
        assert_eq!(beta.role, accesskit::Role::Button);
        assert!(beta.verbs.iter().any(|verb| verb == "activate"), "{beta:?}");
        assert!(beta.verbs.iter().any(|verb| verb == "focus"), "{beta:?}");
        assert_eq!(
            beta.id,
            d.a11y_node_named("beta").unwrap().0.to_string(),
            "a control is named by its exported node"
        );
    }

    #[test]
    fn pressing_and_focusing_a_control_reaches_the_element_inside_a_canvas() {
        let (mut d, activated) = driver();
        let gamma = control_named(&d, "gamma");
        d.operate_control(&gamma.id, &ControlInput::Press).unwrap();
        assert_eq!(*activated.borrow(), vec!["gamma".to_string()]);
        let alpha = control_named(&d, "alpha");
        d.operate_control(&alpha.id, &ControlInput::Focus).unwrap();
        assert_eq!(*activated.borrow(), vec!["gamma".to_string()]);
        assert!(
            d.operate_control(&alpha.id, &ControlInput::SetValue("x".into()))
                .is_err(),
            "a button takes no value"
        );
        assert!(d.operate_control("nope", &ControlInput::Press).is_err());
    }

    #[test]
    fn a_projected_control_action_runs_through_the_door() {
        let (mut d, activated) = driver();
        let resource = d
            .resources()
            .into_iter()
            .find(|resource| {
                resource.id.as_str().starts_with(CONTROL_RESOURCE_PREFIX)
                    && resource.label == "beta"
            })
            .expect("the keyed element inside the canvas is published");
        let address = tilo_actions::ActionAddress::new(resource.id.as_str(), "activate");
        d.invoke_action(&address, &serde_json::Map::new(), false, None)
            .unwrap();
        assert_eq!(*activated.borrow(), vec!["beta".to_string()]);
    }
}

/// Undo, driven through the headless harness — the path every scenario and
/// every screenshot takes. A control that worked only under the live event
/// loop would be dead in all of them.
#[cfg(test)]
mod undo_driver_tests {
    use super::*;
    use crate::state::State;

    /// A minimal editor: appends every typed character to `body` and asks for
    /// a commit boundary straight away.
    ///
    /// Deliberately a widget rather than a direct write to the document —
    /// the edit has to travel the driver's own `send` → `needs_commit` →
    /// commit path, or a test cannot see how that path attributes the action.
    struct Typer {
        rect: vello::kurbo::Rect,
        pending: bool,
    }

    impl crate::widget::Widget for Typer {
        crate::provides!(dyn crate::undo::UndoPoints);

        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn crate::widget::Widget>,
        ) -> NodeId {
            let node = taffy
                .new_leaf(Style {
                    size: Size {
                        width: Dimension::length(40.0),
                        height: Dimension::length(20.0),
                    },
                    ..Default::default()
                })
                .unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn crate::widget::Widget>(self));
            node
        }

        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
            let l = taffy.layout(node).unwrap();
            let x = origin.x + f64::from(l.location.x);
            let y = origin.y + f64::from(l.location.y);
            self.rect = vello::kurbo::Rect::new(
                x,
                y,
                x + f64::from(l.size.width),
                y + f64::from(l.size.height),
            );
        }

        fn draw(
            &mut self,
            _s: &mut Scene,
            _t: Affine,
            _tr: &mut TextRenderer,
            _th: &Theme,
            _p: &PresenceCtx,
        ) {
        }

        fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
            use automerge::{ReadDoc, transaction::Transactable};

            let doc = &mut *ctx.state;
            let WindowEvent::KeyboardInput { event: ke, .. } = event else {
                return;
            };
            if ke.state != ElementState::Pressed {
                return;
            }
            let winit::keyboard::Key::Character(c) = &ke.logical_key else {
                return;
            };
            let obj = match doc.get(&automerge::ROOT, "body") {
                Ok(Some((_, o))) => o,
                _ => doc
                    .put_object(&automerge::ROOT, "body", automerge::ObjType::Text)
                    .unwrap(),
            };
            let len = doc.length(&obj);
            doc.splice_text(&obj, len, 0, c.as_str()).unwrap();
            self.pending = true;
            *ctx.handled = true;
        }

        fn name(&self) -> &'static str {
            "Typer"
        }

        fn get_rect(&self) -> vello::kurbo::Rect {
            self.rect
        }
    }

    impl crate::undo::UndoPoints for Typer {
        fn needs_commit(&self) -> bool {
            self.pending
        }
        fn on_commit(&mut self) {
            self.pending = false;
        }
    }

    /// A typing surface plus history controls, and a driver over them.
    fn undo_driver() -> Driver {
        let row = crate::widget::Row::new()
            .push(Box::new(Typer {
                rect: vello::kurbo::Rect::ZERO,
                pending: false,
            }))
            .push(Box::new(crate::widget::UndoButton::undo()))
            .push(Box::new(crate::widget::UndoButton::redo()));
        Driver::new_with_controlled_inputs(
            State::new(),
            Box::new(row),
            (400, 120),
            Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("task-4-fixture-author".into()),
        )
    }

    /// One edit, typed through the driver and committed by the driver —
    /// so how the driver attributes an action is part of what is under test.
    fn type_action(d: &mut Driver, text: &str) {
        d.type_text(text);
        assert!(
            d.undo_status().can_undo,
            "typing through the driver must produce an undoable action"
        );
    }

    fn body(d: &mut Driver) -> String {
        use automerge::ReadDoc;
        let state = d.state_mut();
        match state.doc.get(&automerge::ROOT, "body") {
            Ok(Some((_, obj))) => state.doc.text(&obj).unwrap_or_default(),
            _ => String::new(),
        }
    }

    #[test]
    fn undo_driver_reaches_one_undo_and_publishes_status() {
        // Case `clicking_undo_and_pressing_ctrl_z_reach_the_same_undo`:
        // Clicking the control in a scenario undoes, and reaches the same place
        // the harness's Ctrl+Z does. A control that only worked live would be
        // dead in every screenshot and every driven test.
        {
            let mut via_click = undo_driver();
            type_action(&mut via_click, "hello");
            via_click
                .click_named("undo_button")
                .expect("the control is addressable once it can act");
            assert_eq!(
                body(&mut via_click),
                "hell",
                "the click undid the last action — one keystroke, one boundary"
            );

            let mut via_key = undo_driver();
            type_action(&mut via_key, "hello");
            via_key
                .set_platform_conventions(crate::PlatformConventions::new(crate::Platform::Linux));
            via_key.set_modifiers(&["ctrl".to_string()]);
            via_key.key("z");
            assert_eq!(
                body(&mut via_key),
                body(&mut via_click),
                "the shortcut and the control land in the same place"
            );

            // And redo, through the control.
            via_click
                .click_named("redo_button")
                .expect("redo became available");
            assert_eq!(body(&mut via_click), "hello");
        }

        // Case `the_driver_publishes_the_status_to_the_widget_tree`:
        // The harness must publish the status, or a control renders dead in every
        // shot regardless of the history behind it.
        {
            let mut d = undo_driver();

            // Nothing to undo: the control is disabled, so it is not in the focus
            // order the harness tabs through.
            assert!(
                d.focus_order().is_empty(),
                "an empty history leaves no focusable history control"
            );

            type_action(&mut d, "hello");
            assert_eq!(
                d.focus_order().len(),
                1,
                "the Undo control becomes focusable once there is an action"
            );
            let status = d.undo_status();
            assert!(status.can_undo && !status.can_redo);
            // Each keystroke is its own commit boundary here, so the newest
            // action inserted one character.
            assert_eq!(status.undo_summary.as_deref(), Some("+1 -0 chars"));
        }

        // Case `clicking_a_disabled_control_in_a_scenario_does_nothing`:
        // A scenario that clicks a control with nothing to undo must not silently
        // change the document.
        {
            let mut d = undo_driver();
            type_action(&mut d, "hello");
            assert!(!d.undo_status().can_redo, "nothing has been undone");
            // Redo is unavailable: nothing has been undone yet.
            let _ = d.click_named("redo_button");
            assert_eq!(body(&mut d), "hello", "the document is untouched");
        }
    }
}

/// Undo for an app whose edits are dispatched [`tilo_actions`] actions — the
/// shape every migrated Tilo app has — driven headlessly.
///
/// The module above drives the *other* editing path, `needs_commit` /
/// `on_commit`, where the widget writes the document itself. An app built on
/// [`ActionHost`](crate::action_host::ActionHost) never sets `needs_commit`:
/// its boundary arrives through
/// [`take_committed_action`](crate::undo::CommittedActions::take_committed_action)
/// instead. Until the driver drained that, a headless test of such an app could
/// deliver the input but could not observe the undo history the input produced,
/// so undo behaviour was untestable for the majority of the fleet.
#[cfg(test)]
mod dispatched_action_driver_tests {
    use super::*;
    use crate::a11y::{AccessNode, AccessState, Role};
    use crate::state::State;
    use serde::{Deserialize, Serialize};
    use tilo_actions::{ActionError, Doc, ExecutionLocus};

    /// An action that appends `text` to the document's `body`. Deterministic, as
    /// the executor requires.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct Append {
        text: String,
    }

    const APPEND: &str = "test.append";

    impl tilo_actions::Action for Append {
        fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
            use automerge::{ReadDoc, transaction::Transactable};
            let obj = match doc.get(&automerge::ROOT, "body") {
                Ok(Some((_, o))) => o,
                _ => doc.put_object(&automerge::ROOT, "body", automerge::ObjType::Text)?,
            };
            let len = doc.length(&obj);
            doc.splice_text(&obj, len, 0, &self.text)?;
            Ok(())
        }
        fn locus(&self) -> ExecutionLocus {
            ExecutionLocus::ClientLocal
        }
    }

    /// An editor in the migrated shape: every edit is dispatched through
    /// [`ActionHost`](crate::action_host::ActionHost), and the only thing it
    /// tells the host is the bracket. It deliberately does **not** implement
    /// `needs_commit` — that is the whole distinction under test.
    ///
    /// Two ways in, because a canvas app has two: a keystroke, and an
    /// accessibility verb routed to `perform_action_at` (how an assistive
    /// technology, and every headless scenario over a canvas, drives it).
    struct ActionEditor {
        rect: vello::kurbo::Rect,
        actions: crate::action_host::ActionHost,
        focused: bool,
        /// Whether this editor claims the event. A `Row` stops dispatching to
        /// later siblings once a child claims one, so a second editor that must
        /// also see the keystroke is built without the claim — the shape of a
        /// pane that reacts to an edit alongside the widget that owns it.
        claims: bool,
    }

    impl ActionEditor {
        fn new() -> Self {
            Self {
                rect: vello::kurbo::Rect::ZERO,
                actions: crate::action_host::ActionHost::new(),
                focused: false,
                claims: true,
            }
        }

        /// An editor that dispatches but lets the event travel on.
        fn passive() -> Self {
            Self {
                claims: false,
                ..Self::new()
            }
        }
    }

    impl crate::widget::Widget for ActionEditor {
        crate::provides!(dyn crate::undo::CommittedActions);

        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn crate::widget::Widget>,
        ) -> NodeId {
            let node = taffy
                .new_leaf(Style {
                    size: Size {
                        width: Dimension::length(60.0),
                        height: Dimension::length(20.0),
                    },
                    ..Default::default()
                })
                .unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn crate::widget::Widget>(self));
            node
        }

        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
            let l = taffy.layout(node).unwrap();
            let x = origin.x + f64::from(l.location.x);
            let y = origin.y + f64::from(l.location.y);
            self.rect = vello::kurbo::Rect::new(
                x,
                y,
                x + f64::from(l.size.width),
                y + f64::from(l.size.height),
            );
        }

        fn draw(
            &mut self,
            _s: &mut Scene,
            _t: Affine,
            _tr: &mut TextRenderer,
            _th: &Theme,
            _p: &PresenceCtx,
        ) {
        }

        fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
            let doc = &mut *ctx.state;
            let handled = &mut *ctx.handled;
            let text = match event {
                WindowEvent::KeyboardInput { event: ke, .. }
                    if ke.state == ElementState::Pressed =>
                {
                    let winit::keyboard::Key::Character(text) = &ke.logical_key else {
                        return;
                    };
                    text.as_str()
                }
                WindowEvent::Ime(winit::event::Ime::Commit(text)) => text.as_str(),
                _ => return,
            };
            // "2" dispatches two actions within the one event, so a test can
            // prove the host brackets the *event*, not each action.
            if text == "2" {
                for text in ["x", "y"] {
                    self.actions.dispatch(
                        doc,
                        APPEND,
                        &Append {
                            text: text.to_string(),
                        },
                    );
                }
            } else {
                self.actions.dispatch(
                    doc,
                    APPEND,
                    &Append {
                        text: text.to_string(),
                    },
                );
            }
            *handled = self.claims;
        }

        fn perform_action_at(
            &mut self,
            action: &crate::a11y::Action,
            _key_path: &[u64],
            doc: &mut crate::widget::AppModel,
            path: &crate::widget::ModelPath,
        ) -> bool {
            let (doc, _) = crate::doc::document(doc, path);
            if !matches!(action, crate::a11y::Action::Activate) {
                return false;
            }
            self.actions.dispatch(
                doc,
                APPEND,
                &Append {
                    text: "!".to_string(),
                },
            );
            true
        }

        fn name(&self) -> &'static str {
            "ActionEditor"
        }

        fn get_rect(&self) -> vello::kurbo::Rect {
            self.rect
        }

        fn is_focusable(&self) -> bool {
            true
        }

        fn set_focus(&mut self, f: bool) {
            self.focused = f;
        }

        fn is_focused(&self) -> bool {
            self.focused
        }

        fn accessibility(&self) -> Option<AccessNode> {
            Some(
                AccessNode::new(Role::Canvas)
                    .with_name("Editor")
                    .with_state(AccessState::new().focusable(true).focused(self.focused)),
            )
        }
    }

    impl crate::undo::CommittedActions for ActionEditor {
        fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
            self.actions.take_committed()
        }
    }

    /// The editor plus the history controls an app would put in its toolbar.
    pub(super) fn action_driver() -> Driver {
        let row = crate::widget::Row::new()
            .push(Box::new(ActionEditor::new()))
            .push(Box::new(crate::widget::UndoButton::undo()))
            .push(Box::new(crate::widget::UndoButton::redo()));
        Driver::new_with_controlled_inputs(
            State::new(),
            Box::new(row),
            (400, 120),
            Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("task-4-fixture-author".into()),
        )
    }

    pub(super) fn body(d: &mut Driver) -> String {
        use automerge::ReadDoc;
        let state = d.state_mut();
        match state.doc.get(&automerge::ROOT, "body") {
            Ok(Some((_, obj))) => state.doc.text(&obj).unwrap_or_default(),
            _ => String::new(),
        }
    }

    #[test]
    fn dispatched_actions_are_undoable_through_every_input_path() {
        // Case `lazy_status_preserves_transaction_boundary_without_a_consumer`.
        {
            use automerge::transaction::Transactable;
            let author = crate::permissions::UserId("lazy-status-author".into());
            let mut state = State::new();
            state.doc.put(automerge::ROOT, "value", "first").unwrap();
            assert!(state.commit_action_authored(None, None, Some(author.clone())));
            let mut driver = Driver::new_with_controlled_inputs(
                state,
                Box::new(crate::widget::Spacer::new()),
                (100, 100),
                Theme::dark(),
                crate::font_catalog::fixture_renderer(),
                author,
            );
            driver
                .state
                .doc
                .put(automerge::ROOT, "value", "pending")
                .unwrap();
            assert!(driver.state.doc.pending_ops() > 0);
            driver.publish_undo_status();
            assert_eq!(
                driver.state.doc.pending_ops(),
                0,
                "lazy labels must preserve the eager status transaction boundary"
            );
        }

        // Case `an_edit_dispatched_as_an_action_lights_undo_and_reverts_when_pressed`:
        // The whole covenant on one interaction: a real edit dispatched as an
        // action, the Undo control lighting because of it, pressing that control,
        // and the edit going away.
        //
        // Every step is the driver's own output — `undo_status` is what the
        // harness publishes to the tree, `click_named` is the production click
        // path, and the document is read back from the live state.
        {
            let mut d = action_driver();

            assert!(
                !d.undo_status().can_undo,
                "nothing has happened yet, so the control must be dark"
            );

            d.type_text("a");
            assert_eq!(
                body(&mut d),
                "a",
                "the dispatched action reached the document"
            );
            assert!(
                d.undo_status().can_undo,
                "an action-dispatched edit must be an undo boundary the local user owns"
            );

            d.click_named("undo_button")
                .expect("the control is addressable once it can act");
            assert_eq!(body(&mut d), "", "pressing Undo reverted the edit");
            assert!(
                d.undo_status().can_redo,
                "and the reverted action is available to redo"
            );

            d.click_named("redo_button").expect("redo became available");
            assert_eq!(body(&mut d), "a", "redo put the action back");
        }

        // Case `committed_text_uses_the_real_ime_path_as_one_edit`.
        {
            let mut d = action_driver();

            d.commit_text("東京 notes");
            assert_eq!(body(&mut d), "東京 notes");
            assert!(d.perform_undo());
            assert_eq!(body(&mut d), "");
        }

        // Case `ctrl_z_and_the_control_undo_the_same_dispatched_action`:
        // The keyboard and the control must reach the same undo — for the action
        // path too, not just the `needs_commit` one.
        {
            let mut via_key = action_driver();
            via_key.type_text("a");
            via_key
                .set_platform_conventions(crate::PlatformConventions::new(crate::Platform::Linux));
            via_key.set_modifiers(&["ctrl".to_string()]);
            via_key.key("z");

            let mut via_click = action_driver();
            via_click.type_text("a");
            via_click.click_named("undo_button").expect("addressable");

            assert_eq!(body(&mut via_key), "");
            assert_eq!(
                body(&mut via_key),
                body(&mut via_click),
                "the shortcut and the control land in the same place"
            );
        }

        // Case `an_action_performed_through_accessibility_is_undoable`:
        // A canvas app is driven through the accessibility tree, not through
        // clicks — `perform_a11y_action` is the route
        // [`docs/headless-testing.md`] tells scenarios to take. That route mutates
        // the document without going through `send`, so it needs the boundary
        // recorded too.
        {
            let mut d = action_driver();
            let node = d
                .a11y_node_named("Editor")
                .expect("the canvas is exported to the accessibility tree");

            d.perform_a11y_action(node, &crate::a11y::Action::Activate)
                .expect("the widget consumes Activate");

            assert_eq!(body(&mut d), "!", "the a11y verb dispatched the action");
            assert!(
                d.undo_status().can_undo,
                "an action performed through accessibility must be undoable too"
            );
            assert!(d.perform_undo(), "and the history actually pops it");
            assert_eq!(body(&mut d), "");
        }
    }

    #[test]
    fn dispatched_action_boundaries_follow_interactions() {
        // Case `several_actions_in_one_event_are_one_boundary`:
        // One event is one undo boundary, however many actions it dispatched —
        // [`ActionHost`](crate::action_host::ActionHost)'s contract, observed
        // through the driver rather than asserted against the host in isolation.
        {
            let mut d = action_driver();
            d.type_text("2"); // dispatches two Appends within the single keystroke
            assert_eq!(body(&mut d), "xy", "both actions applied");

            assert!(d.perform_undo(), "the batch is undoable");
            assert_eq!(
                body(&mut d),
                "",
                "one undo reverted the whole event, not half of it"
            );
            assert!(
                !d.undo_status().can_undo,
                "and there is no second boundary left behind"
            );
        }

        // Case `separate_interactions_are_separate_boundaries`:
        // Two separate interactions are two separate boundaries, undone newest
        // first. Guards the rising-edge drain: a host that failed to clear the
        // bracket would bracket the second edit from the first edit's heads and
        // revert both at once.
        {
            let mut d = action_driver();
            d.type_text("a");
            d.type_text("b");
            assert_eq!(body(&mut d), "ab");

            assert!(d.perform_undo());
            assert_eq!(body(&mut d), "a", "undo reverted only the newer edit");
            assert!(d.perform_undo());
            assert_eq!(body(&mut d), "");
        }

        // Case `two_widgets_dispatching_in_one_tick_are_one_boundary_from_the_earlier`:
        // Two widgets dispatching in the same tick is still one boundary, and it
        // spans from the earlier of their brackets.
        //
        // The second widget captures its bracket *after* the first has already
        // changed the document, so recording from that later bracket would strand
        // the first widget's edit outside the entry: one Ctrl+Z would revert half
        // the tick and leave the rest with nothing to undo it. Which bracket the
        // sweep happens to see first is a hash-map order, so this cannot be left
        // to luck.
        {
            // Both editors handle the same keystroke, so a single key press
            // dispatches from each in turn.
            let row = crate::widget::Row::new()
                .push(Box::new(ActionEditor::passive()))
                .push(Box::new(ActionEditor::new()))
                .push(Box::new(crate::widget::UndoButton::undo()));
            let mut d = Driver::new(State::new(), Box::new(row), (400, 120));

            d.type_text("a");
            assert_eq!(body(&mut d), "aa", "both widgets dispatched");

            assert!(d.perform_undo(), "the tick is undoable");
            assert_eq!(
                body(&mut d),
                "",
                "one undo reverted the whole tick — the entry started before the \
             first widget's action, not between the two"
            );
            assert!(
                !d.undo_status().can_undo,
                "and left no orphaned second boundary"
            );
        }

        // Case `an_event_that_dispatches_nothing_records_no_boundary`:
        // An event that dispatches nothing must not push an empty entry, or the
        // next Ctrl+Z is spent on a no-op and the user's edit survives a
        // press they expected to undo it.
        {
            let mut d = action_driver();
            d.type_text("a");

            // A pointer move: reaches the tree, dispatches no action.
            d.send(&WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(5.0, 5.0),
            });

            assert!(
                d.perform_undo(),
                "the real edit is still the top of the stack"
            );
            assert_eq!(body(&mut d), "", "and one press reverted it");
            assert!(
                !d.undo_status().can_undo,
                "the pointer move left nothing behind"
            );
        }
    }
}

impl Drop for FrameHarness {
    fn drop(&mut self) {
        self.control_domain.close();
        self.bindings.clear();
        self.node_map.clear();
    }
}

// Draft to append under cfg(test) in headless.rs after initial source release.
#[cfg(test)]
mod prepared_driver_tests {
    use super::*;
    use crate::application::{AppComponent, HostContext, LaunchTarget, RestoreInput};
    use crate::application_test_support::*;
    use automerge::{ROOT, ReadDoc, transaction::Transactable};
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    #[derive(Clone, Default)]
    struct Observations {
        published: Rc<RefCell<crate::undo::UndoStatus>>,
        syncs: Rc<Cell<usize>>,
        local: Rc<RefCell<Option<crate::LocalState<String>>>>,
    }
    thread_local! { static OBSERVED: RefCell<Observations> = RefCell::new(Observations::default()); }
    struct Observer {
        observed: Observations,
        snapshot: String,
        rect: vello::kurbo::Rect,
        focused: bool,
        motion_delivered: bool,
    }
    impl Observer {
        fn new(observed: Observations) -> Self {
            Self {
                observed,
                snapshot: String::new(),
                rect: vello::kurbo::Rect::ZERO,
                focused: false,
                motion_delivered: false,
            }
        }
    }
    impl Widget for Observer {
        crate::provides!(dyn crate::undo::UndoControl, dyn crate::undo::UndoPoints);

        fn set_motion_reduced(&mut self, _: bool) {
            if !std::mem::replace(&mut self.motion_delivered, true)
                && let Some(local) = self.observed.local.borrow().as_ref()
            {
                local.writer().set("initial callback".into());
            }
        }
        fn sync(&mut self, _: &mut crate::widget::AppModel, _: &crate::widget::ModelPath) {
            self.observed
                .syncs
                .set(self.observed.syncs.get().saturating_add(1));
            if let Some(local) = self.observed.local.borrow().as_ref() {
                self.snapshot = local.with(Clone::clone);
            }
        }
        fn build_layout(
            &mut self,
            tree: &mut TaffyTree<()>,
            _: &Theme,
            nodes: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = tree
                .new_leaf(Style {
                    size: Size {
                        width: Dimension::length(80.0),
                        height: Dimension::length(30.0),
                    },
                    ..Default::default()
                })
                .unwrap();
            nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }
        fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
            let layout = tree.layout(node).unwrap();
            self.rect = vello::kurbo::Rect::new(
                origin.x,
                origin.y,
                origin.x + f64::from(layout.size.width),
                origin.y + f64::from(layout.size.height),
            );
        }
        fn draw(
            &mut self,
            _: &mut Scene,
            _: Affine,
            _: &mut TextRenderer,
            _: &Theme,
            _: &PresenceCtx,
        ) {
        }
        fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
            if let WindowEvent::KeyboardInput { event, .. } = event
                && self.focused
                && event.state == ElementState::Pressed
                && let Some(text) = event.text.as_deref()
            {
                let before = ctx
                    .state
                    .get(ROOT, "body")
                    .unwrap()
                    .unwrap()
                    .0
                    .as_str()
                    .unwrap()
                    .to_owned();
                ctx.state
                    .put(ROOT, "body", format!("{before}{text}"))
                    .unwrap();
                *ctx.handled = true;
            }
        }
        fn is_focusable(&self) -> bool {
            true
        }
        fn is_focused(&self) -> bool {
            self.focused
        }
        fn set_focus(&mut self, value: bool) {
            self.focused = value;
        }
        fn name(&self) -> &'static str {
            "runner-observer"
        }
        fn get_rect(&self) -> vello::kurbo::Rect {
            self.rect
        }
    }

    impl crate::undo::UndoPoints for Observer {
        fn get_state(&self) -> Option<String> {
            Some(self.snapshot.clone())
        }
    }

    impl crate::undo::UndoControl for Observer {
        fn set_undo_status(&mut self, status: &mut dyn FnMut() -> crate::undo::UndoStatus) {
            *self.observed.published.borrow_mut() = status();
        }
    }
    impl crate::widget::AppWidget for Observer {
        fn app_kind(&self) -> crate::widget::AppKind {
            crate::widget::AppKind::Editor
        }
        fn app_title(&self) -> &'static str {
            "Observer"
        }
        fn app_icon(&self) -> crate::widget::IconDescriptor {
            crate::widget::IconDescriptor::Glyph("O")
        }
        fn permission_hint(&self) -> Option<crate::widget::AppKind> {
            None
        }
    }
    fn factory(
        mut context: HostContext,
        _: LaunchTarget,
        _: RestoreInput,
    ) -> Result<AppComponent, crate::launch::LaunchError> {
        context
            .initialize_new_record(|doc| {
                doc.put(ROOT, "body", "").map_err(|error| error.to_string())
            })
            .map_err(crate::launch::LaunchError::Record)?;
        let observed = OBSERVED.with_borrow(Clone::clone);
        *observed.local.borrow_mut() = Some(crate::LocalState::new("construction".into()));
        observed
            .local
            .borrow()
            .as_ref()
            .unwrap()
            .writer()
            .set("before mount".into());
        Ok(AppComponent::new(
            context,
            Box::new(Observer::new(observed)),
            tilo_actions::ActionRegistry::new(),
        ))
    }
    fn prepared() -> (
        ProbeHost,
        Driver,
        Observations,
        crate::record::RecordCheckout,
    ) {
        let host = ProbeHost::new();
        let definition = definition(factory);
        let observed = Observations::default();
        OBSERVED.with_borrow_mut(|slot| *slot = observed.clone());
        let context = host
            .prepare_with_permissions(&definition, 11, &LaunchTarget::New, runner_permissions())
            .unwrap();
        let checkout = context.record().clone();
        let component = definition
            .instantiate(context, LaunchTarget::New, RestoreInput::None)
            .unwrap();
        let driver = Driver::from_component(component, (320, 180), runner_inputs()).unwrap();
        (host, driver, observed, checkout)
    }
    #[test]
    fn prepared_driver_publishes_undo_status_and_guards_wakes() {
        // Case `prepared_driver_selected_undo_status_preserves_outer_publication`.
        {
            let (host, mut driver, bound, checkout) = prepared();
            let outer = Observations::default();
            // Preserve the owned bound adapter as a child; this test adds ordinary host chrome.
            let component_root =
                std::mem::replace(&mut driver.root, Box::new(crate::widget::Column::new()));
            driver.root = Box::new(
                crate::widget::Column::new()
                    .push(Box::new(Observer::new(outer.clone())))
                    .push(component_root),
            );
            driver.state.doc.put(ROOT, "outer", true).unwrap();
            driver
                .state
                .commit_action_authored_with_context(None, None, Some(author()), None);
            driver.state.undo_as(Some(&author()), None, None).unwrap();
            driver.relayout();
            let outer_baseline = driver.state.undo_status(Some(&author()));
            assert!(!outer_baseline.can_undo && outer_baseline.can_redo);
            let node = *driver
                .node_map
                .keys()
                .find(|node| driver.bindings.binding_for(**node).is_some())
                .unwrap();
            driver.set_focused(Some(node));
            driver.type_text("first");
            let selected = driver.undo_status();
            assert!(selected.can_undo && !selected.can_redo);
            assert_eq!(*bound.published.borrow(), selected);
            assert_eq!(*outer.published.borrow(), outer_baseline);
            driver.type_text(" second");
            for _ in 0..3 {
                assert!(driver.undo_status().can_undo);
            }
            assert_eq!(body(&host.records, &checkout), "first second");
            assert!(driver.perform_undo());
            assert_eq!(body(&host.records, &checkout), "");
            assert!(!driver.undo_status().can_undo && driver.undo_status().can_redo);
            assert!(driver.perform_redo());
            assert_eq!(body(&host.records, &checkout), "first second");
            assert_eq!(*outer.published.borrow(), outer_baseline);
            assert_eq!(driver.state.undo_status(Some(&author())), outer_baseline);
            driver.set_focused(None);
            assert_eq!(driver.undo_status(), outer_baseline);
        }

        // Case `prepared_driver_wakes_are_installed_before_callbacks_and_stale_wakes_are_rejected`.
        {
            let mut outer_scope = crate::WakeScope::new();
            let outer_wakes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let count = outer_wakes.clone();
            outer_scope
                .bind(move |_| {
                    count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                })
                .unwrap();
            let _outer = outer_scope.enter();
            let (_host, mut driver, observed, _) = prepared();
            assert_eq!(
                crate::undo::get_state(driver.root.as_ref()),
                Some("initial callback".into())
            );
            assert_eq!(outer_wakes.load(std::sync::atomic::Ordering::Relaxed), 0);
            let writer = observed.local.borrow().as_ref().unwrap().writer();
            let before = observed.syncs.get();
            writer.set("pending".into());
            let _ = driver.inspection();
            assert_eq!(
                observed.syncs.get(),
                before,
                "read boundary must not perform a frame"
            );
            let _ = driver.action_registry_mut();
            assert!(observed.syncs.get() > before);
            assert_eq!(
                crate::undo::get_state(driver.root.as_ref()),
                Some("pending".into())
            );
            writer.set("queued before close".into());
            driver.close_components();
            assert!(!writer.try_set("stale".into()));
            assert!(!driver.component_wake.as_ref().unwrap().dirty.get());
            let (_next, successor, next_observed, _) = prepared();
            let successor_syncs = next_observed.syncs.get();
            assert!(successor_syncs > 0, "initial composition was synchronized");
            assert!(!writer.try_set("late successor result".into()));
            assert_eq!(
                crate::undo::get_state(successor.root.as_ref()),
                Some("initial callback".into())
            );
            assert_eq!(
                next_observed.syncs.get(),
                successor_syncs,
                "stale wake must not synchronize the successor"
            );
            outer_scope.close();
        }
    }

    #[test]
    fn duplicate_host_modifiers_do_not_sync_the_document_tree() {
        let (_host, mut driver, observed, _) = prepared();
        let before = observed.syncs.get();
        for _ in 0..100 {
            driver.send_host_event(&WindowEvent::ModifiersChanged(
                winit::event::Modifiers::default(),
            ));
        }
        assert_eq!(
            observed.syncs.get(),
            before,
            "unchanged modifiers must not relayout"
        );
        let shifted = winit::event::Modifiers::from(winit::keyboard::ModifiersState::SHIFT);
        driver.send_host_event(&WindowEvent::ModifiersChanged(shifted));
        let changed = observed.syncs.get();
        assert!(
            changed > before,
            "real modifier transitions still reach widgets"
        );
        driver.send_host_event(&WindowEvent::ModifiersChanged(shifted));
        assert_eq!(observed.syncs.get(), changed);
        driver.send_host_event(&WindowEvent::ModifiersChanged(
            winit::event::Modifiers::default(),
        ));
        assert!(
            observed.syncs.get() > changed,
            "modifier release still reaches widgets"
        );
    }
}

#[cfg(test)]
mod history_input_frame_tests {
    use super::FrameworkInputFrame;
    #[test]
    fn nested_input_guard_restores_previous_state_on_return_and_unwind() {
        let flag = std::rc::Rc::new(std::cell::Cell::new(false));
        {
            let _outer = FrameworkInputFrame::new(&flag, true);
            {
                let _inner = FrameworkInputFrame::new(&flag, false);
                assert!(flag.get());
            }
            assert!(flag.get());
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _unwinding = FrameworkInputFrame::new(&flag, true);
                panic!("fixture input callback unwind");
            }));
            assert!(result.is_err());
            assert!(flag.get());
        }
        assert!(!flag.get());
    }
}

#[cfg(test)]
#[path = "bound_dispatch/legacy_control_batch_tests.rs"]
mod legacy_control_batch_tests;

#[cfg(test)]
mod ordinary_confirmation_owner_tests {
    #[test]
    fn ordinary_confirmation_driver_closed_or_replaced_outer_domain_refuses() {
        for replacement in [false, true] {
            let (root, ledger, _) = crate::app::ordinary_confirmation_tests::fixture(
                tilo_actions::ConfirmationPolicy::Confirm,
            );
            let mut driver = super::Driver::from_managed_root_with_controlled_inputs(
                crate::State::new(),
                root,
                (900, 650),
                crate::Theme::dark(),
                crate::font_catalog::fixture_renderer(),
                crate::permissions::UserId("fixture".into()),
            )
            .unwrap();
            driver
                .perform_semantic_named("ordinary-emitter", None, &crate::a11y::Action::Activate)
                .unwrap();
            let field = driver.a11y_node_named("Confirmation").unwrap();
            driver
                .perform_a11y_action(field, &crate::a11y::Action::SetValue("CONFIRM".into()))
                .unwrap();
            let old = driver.control_domain.clone();
            if replacement {
                driver.control_domain = crate::ControlDomain::new();
            } else {
                old.close();
            }
            let submit = driver.a11y_node_named("Confirm").unwrap();
            driver
                .perform_a11y_action(submit, &crate::a11y::Action::Activate)
                .unwrap();
            assert!(ledger.borrow().is_empty());
        }
    }
}

#[cfg(test)]
pub(crate) mod legacy_control_feedback_tests {
    use super::*;
    use crate::widget::{Column, IconDescriptor, TileContent, TileHost};
    use std::{
        cell::{Cell, RefCell},
        collections::VecDeque,
        rc::Rc,
    };

    #[derive(Clone, Default)]
    pub(crate) struct Observed {
        pub(crate) results: Rc<RefCell<Vec<(String, bool)>>>,
        pub(crate) pending: Rc<RefCell<VecDeque<tilo_actions::ControlInvocation>>>,
        original: Rc<Cell<Option<tilo_actions::InvocationId>>>,
        terminal: Rc<Cell<Option<bool>>>,
        pub(crate) receipts: Rc<Cell<usize>>,
    }
    impl Observed {
        pub(crate) fn begin(&self) {
            self.pending
                .borrow_mut()
                .push_back(tilo_actions::ControlInvocation {
                    address: tilo_actions::ActionAddress::new("feedback:import", "begin"),
                    arguments: serde_json::Map::new(),
                });
        }
    }
    struct ImportProbe {
        observed: Observed,
        fail: bool,
        descendants: Column,
    }
    impl Widget for ImportProbe {
        crate::provides!(
            dyn crate::widget::controls::ActionControls,
            dyn crate::resources::PublishesResources
        );

        crate::testing::delegate_widget_methods!(
            descendants: sync,
            build_layout,
            apply_layout,
            draw,
            get_rect
        );
        fn name(&self) -> &str {
            "Queued import fixture"
        }
        fn children(&self) -> Vec<&dyn Widget> {
            vec![&self.descendants]
        }
        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            vec![&mut self.descendants]
        }
        fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
            &["feedback.begin"]
        }
        fn start_operation(
            &mut self,
            op: &crate::widget::ResourceOperation<'_>,
            _: &mut automerge::AutoCommit,
            _: &automerge::ObjId,
        ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
            let Some(request) = op
                .unscoped()
                .filter(|request| request.type_id == "feedback.begin")
            else {
                return Ok(None);
            };
            self.observed.original.set(Some(request.invocation));
            self.observed
                .pending
                .borrow_mut()
                .push_back(tilo_actions::ControlInvocation {
                    address: tilo_actions::ActionAddress::new("feedback:import", "finalize"),
                    arguments: serde_json::Map::new(),
                });
            Ok(Some(crate::widget::ResourceOperationStart::Accepted))
        }
        fn poll_resource_operation(
            &mut self,
            invocation: tilo_actions::InvocationId,
            _: bool,
            _: &automerge::AutoCommit,
            _: &automerge::ObjId,
        ) -> Option<tilo_actions::InvocationUpdate> {
            if self.observed.original.get() != Some(invocation) {
                return None;
            }
            let succeeded = self.observed.terminal.take()?;
            self.observed.original.set(None);
            self.observed.receipts.set(self.observed.receipts.get() + 1);
            Some(if succeeded {
                tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput::default(),
                }
            } else {
                tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "finalize_failed".into(),
                        message: "Canonical import finalizer failed".into(),
                        details: None,
                        retryable: false,
                    },
                }
            })
        }
    }

    impl crate::resources::PublishesResources for ImportProbe {
        fn resources(&self, _: &automerge::AutoCommit) -> Vec<tilo_actions::Resource> {
            let contract = tilo_actions::ActionContract {
                execution: tilo_actions::ExecutionMode::Background { cancellable: false },
                result: tilo_actions::ResultSpec::Job,
                ..tilo_actions::ActionContract::default()
            };
            let begin = tilo_actions::ActionSpec::operation(
                "begin",
                "Begin",
                tilo_actions::OperationKind::Effect,
                "feedback.begin",
                &(),
            )
            .unwrap()
            .with_contract(contract);
            let finalize = tilo_actions::ActionSpec::immediate_encoded(
                "finalize",
                "Finalize",
                tilo_actions::ExecutionLocus::ClientLocal,
                tilo_actions::EncodedAction::encode(
                    "controlled.legacy",
                    &crate::bound_dispatch::legacy_document_tests::SetValue {
                        value: 2,
                        fail: self.fail,
                        remote: false,
                    },
                )
                .unwrap(),
            );
            vec![
                tilo_actions::Resource::new("feedback:import", "import", "Import")
                    .with_action(begin)
                    .with_action(finalize),
            ]
        }
    }

    impl crate::widget::controls::ActionControls for ImportProbe {
        fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
            self.observed.pending.borrow_mut().pop_front()
        }
        fn on_control_invocation_result(
            &mut self,
            address: &tilo_actions::ActionAddress,
            result: &Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError>,
        ) {
            self.observed
                .results
                .borrow_mut()
                .push((address.action.clone(), result.is_ok()));
            if address.action == "finalize" {
                self.observed.terminal.set(Some(result.is_ok()));
            }
            // Exercise ordinary container forwarding after the owning callback.
            self.descendants
                .on_control_invocation_result(address, result);
        }
        fn has_pending_control_invocation(&self) -> bool {
            !self.observed.pending.borrow().is_empty()
        }
    }
    crate::testing::test_app_widget!(
        ImportProbe,
        "Import fixture",
        icon = IconDescriptor::Glyph("I")
    );
    pub(crate) fn owner(
        id: u64,
        fail: bool,
        nested: Option<TileHost>,
    ) -> (
        crate::bound_dispatch::legacy_document_tests::Fixture,
        TileHost,
        Observed,
    ) {
        let fixture = crate::bound_dispatch::legacy_document_tests::Fixture::new(id);
        let observed = Observed::default();
        let mut descendants = Column::new();
        if let Some(nested) = nested {
            descendants = descendants.push(Box::new(nested));
        }
        let mut tile = TileHost::new(TileContent::App(Box::new(ImportProbe {
            observed: observed.clone(),
            fail,
            descendants,
        })));
        tile.bind_record(fixture.store.clone(), fixture.checkout.clone());
        (fixture, tile, observed)
    }
    pub(crate) fn register(registry: &mut tilo_actions::ActionRegistry) {
        registry.register::<crate::bound_dispatch::legacy_document_tests::SetValue>(
            "controlled.legacy",
        );
    }
    #[test]
    fn queued_legacy_finalizer_settles_original_receipt_and_runner_status() {
        for fail in [false, true] {
            let (_nested_owner, nested, nested_seen) = owner(99031, false, None);
            let (owner, tile, seen) = owner(99030, fail, Some(nested));
            seen.begin();
            let mut inputs = crate::launch::LegacyLaunchInputs::new(
                State::new(),
                Box::new(tile),
                crate::persist::DocWriter::disabled("canonical owner writes"),
            )
            .with_widget_owned_persistence();
            register(&mut inputs.registry);
            assert_eq!(
                run_initialized_with_history(&Spec::default(), inputs, Theme::dark(), None),
                i32::from(fail)
            );
            assert_eq!(
                *seen.results.borrow(),
                vec![("begin".into(), true), ("finalize".into(), !fail)]
            );
            assert_eq!(
                seen.receipts.get(),
                1,
                "finalizer feedback must complete the original background import"
            );
            assert!(
                nested_seen.results.borrow().is_empty(),
                "same-address nested owner cannot receive feedback"
            );
            assert!(
                owner.store.try_flush_record(&owner.checkout.id).is_ok(),
                "operation failure is separate from actual durability"
            );
        }
    }
    #[test]
    fn refused_legacy_queue_is_a_startup_failure_and_does_not_notify_sibling() {
        let (_owner, tile, seen) = owner(99040, false, None);
        let (_sibling_owner, sibling, sibling_seen) = owner(99041, false, None);
        let root = Column::new().push(Box::new(tile)).push(Box::new(sibling));
        let mut driver = Driver::new_with_controlled_inputs(
            State::new(),
            Box::new(root),
            (400, 200),
            Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("reader".into()),
        );
        seen.pending
            .borrow_mut()
            .push_back(tilo_actions::ControlInvocation {
                address: tilo_actions::ActionAddress::new("feedback:import", "missing"),
                arguments: serde_json::Map::new(),
            });
        let failure = drive_spec_actions(&Spec::default(), &mut driver, |_| {});
        assert!(failure.is_some_and(|error| error.contains("startup control invocation failed")));
        assert_eq!(*seen.results.borrow(), vec![("missing".into(), false)]);
        assert!(sibling_seen.results.borrow().is_empty());
    }
    #[test]
    fn stale_legacy_control_feedback_cannot_reach_replacement_owner() {
        let (_old_owner, tile, old_seen) = owner(99060, false, None);
        let target = crate::bound_dispatch::legacy_document::LegacyResourceTarget {
            owner: tile.history_owner().unwrap(),
            address: tilo_actions::ActionAddress::new("feedback:import", "begin"),
        };
        drop(tile);
        let (_replacement_owner, replacement, replacement_seen) = owner(99060, false, None);
        let mut driver = Driver::new_with_controlled_inputs(
            State::new(),
            Box::new(replacement),
            (400, 200),
            Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("reader".into()),
        );
        driver.dispatch_legacy_control(
            &target,
            &tilo_actions::ControlInvocation {
                address: target.address.clone(),
                arguments: serde_json::Map::new(),
            },
        );
        assert_eq!(driver.control_failures.len(), 1);
        assert!(old_seen.results.borrow().is_empty());
        assert!(
            replacement_seen.results.borrow().is_empty(),
            "same record ID cannot inherit retired feedback"
        );
    }
}

#[cfg(test)]
mod portable_host_tests;

#[cfg(test)]
mod framed_tests {
    use super::*;

    struct Probe {
        width: f32,
        rect: Rect,
        syncs: usize,
        presses: usize,
    }

    impl Probe {
        fn new(width: f32) -> Self {
            Self {
                width,
                rect: Rect::ZERO,
                syncs: 0,
                presses: 0,
            }
        }
    }

    impl Widget for Probe {
        fn sync(&mut self, _state: &mut crate::widget::AppModel, _path: &crate::widget::ModelPath) {
            self.syncs = self.syncs.saturating_add(1);
        }

        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = taffy
                .new_leaf(Style {
                    size: Size {
                        width: Dimension::length(self.width),
                        height: Dimension::length(40.0),
                    },
                    ..Default::default()
                })
                .unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }

        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
            let layout = taffy.layout(node).unwrap();
            self.rect = Rect::new(
                origin.x + f64::from(layout.location.x),
                origin.y + f64::from(layout.location.y),
                origin.x + f64::from(layout.location.x + layout.size.width),
                origin.y + f64::from(layout.location.y + layout.size.height),
            );
        }

        fn draw(
            &mut self,
            _scene: &mut Scene,
            _transform: Affine,
            _text_renderer: &mut TextRenderer,
            _theme: &Theme,
            _presence: &PresenceCtx,
        ) {
        }

        fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
            if matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    ..
                }
            ) {
                self.presses = self.presses.saturating_add(1);
                *ctx.handled = true;
            }
        }

        fn get_rect(&self) -> Rect {
            self.rect
        }

        fn name(&self) -> &'static str {
            "Probe"
        }
    }

    fn press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }

    #[test]
    fn framed_root_runs_a_frame_on_mount_and_recomposes_after_mutable_access() {
        let mut probe = Framed::new(State::new(), Probe::new(120.0), (400, 200), Theme::dark());
        assert!(probe.syncs > 0, "mount runs the production sync pass");
        assert!((probe.rect.width() - 120.0).abs() < f64::EPSILON);

        Framed::harness_mut(&mut probe).relayout();
        assert!(
            !Framed::harness(&probe).last_relayout().rebuilt,
            "an untouched root keeps its retained tree"
        );

        probe.width = 200.0;
        Framed::harness_mut(&mut probe).relayout();
        assert!(Framed::harness(&probe).last_relayout().rebuilt);
        assert!((probe.rect.width() - 200.0).abs() < f64::EPSILON);
    }

    #[test]
    fn recompose_rebuilds_an_unchanged_tree() {
        let mut probe = Framed::new(State::new(), Probe::new(120.0), (400, 200), Theme::dark());
        Framed::harness_mut(&mut probe).recompose().unwrap();
        assert!(Framed::harness(&probe).last_relayout().rebuilt);
        assert!((probe.rect.width() - 120.0).abs() < f64::EPSILON);
    }

    #[test]
    fn dispatch_to_root_reaches_the_root_and_keeps_the_retained_tree() {
        let mut probe = Framed::new(State::new(), Probe::new(120.0), (400, 200), Theme::dark());
        let outcome = Framed::harness_mut(&mut probe)
            .dispatch_to_root_outcome(&press(), &winit::event::Modifiers::default());
        assert!(outcome.handled);
        assert_eq!(outcome.focus_target, None);
        assert_eq!(probe.presses, 1);

        Framed::harness_mut(&mut probe).relayout();
        assert!(!Framed::harness(&probe).last_relayout().rebuilt);
    }

    #[test]
    fn host_style_positions_the_root_inside_the_viewport() {
        let probe = Framed::with_options(
            State::new(),
            Probe::new(120.0),
            (400, 200),
            FrameOptions {
                host_style: Some(Style {
                    justify_content: Some(JustifyContent::Center),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        assert!((probe.rect.x0 - 140.0).abs() < f64::EPSILON);
    }

    /// A leaf 300 px tall that records the heights it was offered.
    struct Tall {
        rect: Rect,
        offered: Vec<AvailableSpace>,
    }

    impl Widget for Tall {
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = taffy.new_leaf(Style::default()).unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }

        fn measure(
            &mut self,
            _known: Size<Option<f32>>,
            available: Size<AvailableSpace>,
            _text_renderer: &mut TextRenderer,
        ) -> Size<f32> {
            self.offered.push(available.height);
            Size {
                width: 100.0,
                height: 300.0,
            }
        }

        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
            let layout = taffy.layout(node).unwrap();
            self.rect = Rect::from_origin_size(
                (
                    origin.x + f64::from(layout.location.x),
                    origin.y + f64::from(layout.location.y),
                ),
                (f64::from(layout.size.width), f64::from(layout.size.height)),
            );
        }

        fn draw(
            &mut self,
            _scene: &mut Scene,
            _transform: Affine,
            _text_renderer: &mut TextRenderer,
            _theme: &Theme,
            _presence: &PresenceCtx,
        ) {
        }

        fn get_rect(&self) -> Rect {
            self.rect
        }

        fn name(&self) -> &'static str {
            "Tall"
        }
    }

    #[test]
    fn content_height_lays_the_root_out_past_the_viewport() {
        let tall = || Tall {
            rect: Rect::ZERO,
            offered: Vec::new(),
        };
        let bounded = Framed::new(State::new(), tall(), (400, 200), Theme::dark());
        assert!((bounded.rect.height() - 200.0).abs() < f64::EPSILON);
        assert!(!bounded.offered.contains(&AvailableSpace::MaxContent));

        let unbounded = Framed::with_options(
            State::new(),
            tall(),
            (400, 200),
            FrameOptions {
                content_height: true,
                ..Default::default()
            },
        );
        assert!((unbounded.rect.height() - 300.0).abs() < f64::EPSILON);
        assert!(unbounded.offered.contains(&AvailableSpace::MaxContent));
    }

    #[test]
    #[should_panic(expected = "the root was not mounted as this type")]
    fn typed_root_access_rejects_another_type() {
        let harness =
            FrameHarness::for_root(State::new(), Probe::new(120.0), (400, 200), Theme::dark());
        let _ = harness.root::<crate::widget::Text>();
    }

    #[test]
    fn viewport_truncates_and_clamps_float_sizes() {
        assert_eq!(viewport((1200.9_f32, 760.0)), (1200, 760));
        assert_eq!(viewport((-3.0_f64, f64::NAN)), (0, 0));
    }
}
