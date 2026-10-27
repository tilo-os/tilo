//! Native compositor mode: `--mode native` wires a real smithay event loop
//! over `state::State`, with an auto-named listening socket (`wayland-1`,
//! `wayland-2`, …) handed to `DisplayHandle::insert_client`. Doesn't open a
//! TTY, talk to DRM/KMS, render, or forward input — those live elsewhere.

use crate::shell::input_policy::{
    is_lock_chord, is_mouse_keys_toggle, is_remote_input_kill_switch,
};
use crate::shell::pointer_readiness::PressFacts;
pub use crate::shell::session_policy::*;
use crate::shell::state::WorkspaceAuthority as _;
pub use crate::shell::test_control::{
    AGENT_CONTROL_NOT_PERMITTED, ReadLineError, TEST_CONTROL_MAX_LINE_BYTES, keyboard_input_event,
    read_bounded_line,
};
use std::collections::{HashMap, HashSet};
use std::io::{BufReader, Write};
use std::os::fd::{AsFd, AsRawFd, FromRawFd};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ObjId};

use crate::shell::collab_broker_capnp::LocalSessionLifecycle;
use crate::shell::search::action_api::{ActionArgs, ActionVisibility, ResourceRef};
use crate::shell::search::visibility::PolicyVisibility;

/// The gate makes the action API answer *for a caller*: a resource the
/// capability model denies is absent from enumeration and selection alike.
/// An unresolved workspace is an error, never a fallback to "show
/// everything". Parameters are typed + required-ness on the wire, so a
/// caller with no screen can build a valid invocation from them alone.
fn params_json(params: &[crate::shell::search::action_api::ActionParam]) -> Vec<serde_json::Value> {
    params
        .iter()
        .map(|p| {
            serde_json::json!({
                "name": p.name,
                "type": p.ty.label(),
                "required": p.required,
                "description": p.description,
                "choices": p.choices,
                "schema": p.schema,
                "nullable": p.nullable,
            })
        })
        .collect()
}

fn with_action_gate<R>(
    chrome_handle: &crate::shell::chrome_state::Chrome,
    f: impl FnOnce(&crate::shell::chrome_state::ChromeState, &dyn ActionVisibility) -> R,
) -> Result<R, String> {
    let chrome = chrome_handle.borrow();
    // Visibility is an inert snapshot, not mutation authority. Do not retain
    // the membership read lock while callbacks recheck certified admission:
    // a queued writer can make a nested read wait on our own outer guard.
    // The copy is the policy only: the gate reads ownership and capabilities,
    // never strips, tasks or the receipts every effect appends.
    let workspace = {
        let tree = chrome.tree();
        tree.desktop
            .active_workspace()
            .map(crate::shell::state::Workspace::policy_view)
            .ok_or_else(|| "no active workspace to resolve capabilities against".to_string())?
    };
    let actor_id = chrome.local_user_for_room(&workspace.room());
    let actor = workspace.actor_for_person(&actor_id);
    let gate = PolicyVisibility::new(&workspace, actor);
    Ok(f(&chrome, &gate))
}
use smithay::reexports::calloop::{self, EventLoop, Interest, Mode, PostAction, generic::Generic};
use smithay::reexports::wayland_server::{Display, Resource};
use smithay::wayland::socket::ListeningSocketSource;
use taffy::NodeId;
use vello::kurbo::Rect;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey, PhysicalKey, SmolStr};

use crate::compositor::accessibility::{NativeAccessibility, Request as AccessibilityRequest};
use crate::compositor::backend::drm::runtime::{DirectTouchDown, DirectTouchOwnership};
use crate::compositor::backend::{
    Backend, BackendEvent, DrmBackend, HeadlessBackend, Output, OutputId, TabletPhase,
    WinitBackend, combined_geometry, headless, logical_geometry, output_geometry,
};
use crate::compositor::capture::{CaptureExecutor, CaptureOutcome};
use crate::compositor::client_input;
use crate::compositor::drag_feedback;
use crate::compositor::floating_service::{FloatingServiceEvent, FloatingSurfaceService};
use crate::compositor::foreign;
use crate::compositor::frame::{FrameBuilder, FramePaint};
use crate::compositor::idle::{IdleConfig, SessionManager, SessionTransition};
use crate::compositor::input_environment::InputEnvironment;
use crate::compositor::lock_auth;
use crate::compositor::lock_controller::{LockController, LockKey, UnlockOutcome};
use crate::compositor::lock_screen::{self, LockView, lock_outputs};
use crate::compositor::metrics;
use crate::compositor::output_globals;
use crate::compositor::pacing;
use crate::compositor::present;
use crate::compositor::screen_dim::{Backlight, ScreenDimmer};
use crate::compositor::surface_outputs::{
    Change as SurfaceOutputChange, Memberships as SurfaceOutputMemberships,
};

/// The person's other devices showing each tab, by tab.
type PersonalViewers =
    std::collections::BTreeMap<u64, Vec<crate::shell::device_fabric_runtime::WindowViewer>>;

/// Members of a room viewing each of this device's windows, by tab
/// ([`crate::shell::network::ShellNetwork::room_window_viewers`]).
type MemberViewers = std::collections::BTreeMap<u64, Vec<crate::shell::network::RoomWindowViewer>>;

/// Each legacy window to stream, by surface, with the room its tab is in.
type DesiredShares = HashMap<
    u64,
    (
        crate::compositor::state::LegacyShare,
        crate::shell::state::RoomId,
    ),
>;

fn observed_frame_sequence(
    store: &crate::compositor::foreign::ForeignFrameStore,
    surface: u64,
    pixels: Option<(u64, u64)>,
) -> u64 {
    pixels.map_or_else(|| store.frame_seq(surface), |(seq, _)| seq)
}

fn insert_preedit_route(row: &mut serde_json::Value, route: &str, source: u64) {
    if let Some(object) = row.as_object_mut() {
        object.insert("input_route".into(), route.into());
        object.insert("input_source_surface_id".into(), source.into());
    }
}

/// The workspace hosts of each room this session holds, by room, when the
/// backend is headless: their viewers (web members) watch that room's
/// windows through them.
type RoomHosts =
    std::collections::BTreeMap<tilo_desktop_model::RoomId, std::collections::HashSet<u64>>;

/// Every host in `hosts`, whichever room it hosts.
fn all_hosts(hosts: &RoomHosts) -> std::collections::HashSet<u64> {
    hosts.values().flatten().copied().collect()
}

/// What [`LegacySharePlan`] last derived the shares from.
struct LegacyShareInputs {
    /// [`crate::shell::state::WorkspaceTreeHandle::generation`].
    tree: u64,
    /// Whether the backend was watched once the plan had asked for it.
    watched: bool,
    viewer_hosts: RoomHosts,
    personal: PersonalViewers,
    members: MemberViewers,
}

/// The legacy windows to stream and to whom, derived from the tree and the
/// viewers only when one of them changed: a wakeup that wrote nothing and
/// heard of no new viewer walks no tab.
#[derive(Default)]
struct LegacySharePlan {
    inputs: Option<LegacyShareInputs>,
    /// Each window to stream, with the room its tab is in.
    desired: DesiredShares,
    /// The same shares without their rooms, as reconciling takes them.
    shares: HashMap<u64, crate::compositor::state::LegacyShare>,
    /// Derivations so far, each a walk of every tab.
    #[cfg(test)]
    derivations: u64,
}

impl LegacySharePlan {
    /// Derive the shares again if `tree` was written, or the viewers or the
    /// backend's `watched` state changed, since they were last derived.
    /// `watch` is told whether a viewer needs the backend watched for it and
    /// answers whether the backend is watched then. Returns whether the
    /// shares were derived again, which is when what they feed is told.
    fn refresh(
        &mut self,
        tree: &crate::shell::state::WorkspaceTreeHandle,
        viewers: (RoomHosts, PersonalViewers, MemberViewers),
        watched: bool,
        watch: impl FnOnce(bool) -> bool,
    ) -> bool {
        let (viewer_hosts, personal, members) = viewers;
        // Loaded before the tree is read: a write racing the read below is
        // read again on the next call.
        let generation = tree.generation();
        if self.inputs.as_ref().is_some_and(|seen| {
            seen.tree == generation
                && seen.watched == watched
                && seen.viewer_hosts == viewer_hosts
                && seen.personal == personal
                && seen.members == members
        }) {
            return false;
        }
        let (desired, watched) = {
            let tree = tree.read();
            // A headless session whose rooms have hosts shows their legacy
            // windows to the hosts' viewers (browsers that opened them), and
            // the person's other devices the windows they show: it paints and
            // encodes while it has a viewer, and stops with the last.
            let for_host = has_hosted_legacy_windows(&tree, &viewer_hosts)
                || hosts_viewed_window(&tree, &personal, &members);
            let watched = watch(for_host);
            let desired = desired_legacy_shares(&tree, watched, &viewer_hosts, &personal, &members);
            drop(tree);
            (desired, watched)
        };
        self.shares = desired
            .iter()
            .map(|(surface, (share, _))| (*surface, share.clone()))
            .collect();
        self.desired = desired;
        self.inputs = Some(LegacyShareInputs {
            tree: generation,
            watched,
            viewer_hosts,
            personal,
            members,
        });
        #[cfg(test)]
        {
            self.derivations = self.derivations.saturating_add(1);
        }
        true
    }

    /// The headless host viewers and the person's devices the shares were
    /// derived for.
    fn viewers(&self) -> Option<(&RoomHosts, &PersonalViewers, &MemberViewers)> {
        self.inputs
            .as_ref()
            .map(|inputs| (&inputs.viewer_hosts, &inputs.personal, &inputs.members))
    }
}

/// Whether the active workspace shows a legacy window.
/// Whether a workspace whose room has a host holds a legacy window.
fn has_hosted_legacy_windows(tree: &crate::shell::state::WorkspaceTree, hosts: &RoomHosts) -> bool {
    tree.desktop.workspaces.iter().any(|workspace| {
        hosts
            .get(&workspace.room())
            .is_some_and(|room| !room.is_empty())
            && workspace
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .any(|tab| matches!(tab.tile, crate::shell::state::TileContent::Foreign { .. }))
    })
}
use crate::compositor::switch_control::{SwitchAction, SwitchControlRuntime};

use crate::compositor::control_invocation::{
    ControlInvocation, ControlInvocationPoll, InvocationProgress,
};
use crate::compositor::control_listing::{HeldListing, ListingStamp, ListingWait};
#[cfg(test)]
use crate::compositor::session_bootstrap::spawn_session_collab_broker;
use crate::compositor::session_bootstrap::{
    LoadedSession, finish_session_collab_broker, load_session_tree, service_socket_is_live,
    start_session_collab_broker,
};
use crate::compositor::state::{ClientState, InputOrigin, State};
use crate::compositor::test_control::{
    self, ControlState, KeyTag, NamedKeyTag, PlannedAction, PointerButtonKind, TouchPhaseKind,
    parse_request, serialise_response,
};
use crate::compositor::{BackendKind, parse_backend_from_args};

/// The lock surface hides the complete desktop tree.  Its background widgets
/// may legitimately keep animating (an opening app, a video, a closing
/// notification), but none of that motion is visible and therefore must not
/// keep repainting the full-screen lock scene.  State which can change the
/// lock surface itself remains an ordinary redraw edge.
#[derive(Clone, Copy, Debug, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "named redraw edges keep the native loop's existing independent causes explicit and make lock filtering directly testable"
)]
struct NativeRedrawReasons {
    force: bool,
    backend_event: bool,
    /// Input which changes compositor-owned pixels, most notably the cursor.
    /// This is distinct from `session`: a focused client may consume pointer
    /// motion while the shell must still repaint its own cursor.
    visible_input: bool,
    session: bool,
    revision: bool,
    hidden_desktop_animation: bool,
    apps: bool,
    peers: bool,
    presentation: bool,
    capture: bool,
    keepalive: bool,
    lock_clock: bool,
    /// A media-key readout drawn over the lock surface is fading or due to clear.
    lock_osd: bool,
    /// An earlier tick counted a change but painted no frame for it: the
    /// frame clock was not due yet, or the presenter was full. Most reasons
    /// are edges seen once (a client commit, input, a session change), so
    /// the change is owed until a frame is painted.
    owed: bool,
}

impl NativeRedrawReasons {
    fn content_changed(self, session_locked: bool) -> bool {
        self.owed
            || self.force
            || self.backend_event
            || self.visible_input
            || self.session
            || self.revision
            || (!session_locked
                && (self.hidden_desktop_animation
                    || self.apps
                    || self.peers
                    || self.presentation
                    || self.capture))
            || (!session_locked && self.keepalive)
            || (session_locked && (self.lock_clock || self.lock_osd))
    }

    /// Nothing but an animation of the desktop asks for this frame: no
    /// input, client or app content, chrome, session or owed change.
    fn only_motion(self, session_locked: bool) -> bool {
        let Self {
            force,
            backend_event,
            visible_input,
            session,
            revision,
            hidden_desktop_animation,
            apps,
            peers,
            presentation,
            capture,
            keepalive,
            lock_clock,
            lock_osd,
            owed,
        } = self;
        !session_locked
            && hidden_desktop_animation
            && ![
                force,
                backend_event,
                visible_input,
                session,
                revision,
                apps,
                peers,
                presentation,
                capture,
                keepalive,
                lock_clock,
                lock_osd,
                owed,
            ]
            .contains(&true)
    }

    /// The reasons [`Self::content_changed`] counts, as frame-trace
    /// [`metrics::paint_reason`] bits, gated exactly as it gates them.
    /// `transition` separates a compositor transition's frame run from the
    /// other animations folded into `hidden_desktop_animation`.
    fn paint_reason_bits(self, session_locked: bool, transition: bool) -> u16 {
        use metrics::paint_reason as r;
        let desktop = !session_locked;
        [
            (self.force, r::FORCE),
            (self.backend_event, r::INPUT),
            (self.visible_input, r::CURSOR),
            (self.session, r::SESSION),
            (self.revision, r::REVISION),
            (desktop && self.hidden_desktop_animation, r::ANIMATION),
            (
                desktop && self.hidden_desktop_animation && transition,
                r::TRANSITION,
            ),
            (desktop && self.apps, r::APPS),
            (desktop && self.peers, r::PEERS),
            (desktop && self.presentation, r::PRESENTATION),
            (desktop && self.capture, r::CAPTURE),
            (desktop && self.keepalive, r::KEEPALIVE),
            (session_locked && self.lock_clock, r::LOCK_CLOCK),
            (session_locked && self.lock_osd, r::LOCK_OSD),
            (self.owed, r::OWED),
        ]
        .into_iter()
        .filter(|(held, _)| *held)
        .fold(0, |bits, (_, bit)| bits | bit)
    }
}

/// The backend events one loop iteration dispatched, for whether they ask for
/// a frame by themselves ([`NativeRedrawReasons::backend_event`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct DispatchedInput {
    /// Keyboard presses and releases.
    keys: u64,
    /// Every other backend event.
    other: u64,
}

impl DispatchedInput {
    fn note(&mut self, event: &BackendEvent) {
        let key = matches!(
            event,
            BackendEvent::Input(WindowEvent::KeyboardInput { .. })
                | BackendEvent::PhysicalKey {
                    event: WindowEvent::KeyboardInput { .. },
                    ..
                }
        );
        if key {
            self.keys = self.keys.saturating_add(1);
        } else {
            self.other = self.other.saturating_add(1);
        }
    }

    /// Whether this input needs a frame of its own, with `keys_to_clients` of
    /// its keys forwarded to a client. A key only a client took changes no
    /// shell pixels: the client's commit asks for the frame that shows it.
    /// Painting for the key as well spent the frame clock's next deadline on
    /// a frame identical to the last one, so the commit that followed a
    /// millisecond later waited a refresh interval for the next deadline. A
    /// key the shell kept (a shortcut, typing into the chrome), and any other
    /// event, asks for a frame as before; whatever the shell changed in
    /// response asks again through its own reason.
    fn asks_for_frame(self, keys_to_clients: u64) -> bool {
        self.other > 0 || self.keys > keys_to_clients
    }
}

/// `+`-joined app ids of the tiles whose surfaces committed since the last
/// painted tick, for the frame trace. Characters that would break the CSV
/// row or the join are replaced; a surface no tab has adopted yet is named
/// by its id.
fn paint_client_label(
    tree: &crate::shell::state::WorkspaceTree,
    surfaces: &[crate::shell::foreign::SurfaceId],
) -> &'static str {
    let mut names: Vec<String> = surfaces
        .iter()
        .map(|surface| {
            crate::shell::tab_binding::app_id_for_surface(tree, *surface).map_or_else(
                || format!("surface-{surface}"),
                |app| {
                    app.chars()
                        .map(|c| {
                            if c == ',' || c == '+' || c.is_control() {
                                '_'
                            } else {
                                c
                            }
                        })
                        .collect()
                },
            )
        })
        .collect();
    names.sort_unstable();
    names.dedup();
    metrics::intern_clients(&names.join("+"))
}

/// An animation's resting frame is owed after its demand clears. The last
/// tick that saw it moving may not have painted (the presenter was full, or
/// the tick fell between display intervals), and the tick after it sees
/// nothing moving: without this latch a popover stays frozen part-way
/// through its fade, translucent over whatever is behind it.
#[derive(Clone, Copy, Debug, Default)]
struct AnimationSettle {
    owed: bool,
}

impl AnimationSettle {
    /// Whether this tick must still produce an animated frame.
    const fn due(self, animating: bool) -> bool {
        animating || self.owed
    }

    /// Record the tick: motion owes a frame painted after it ends.
    /// `animating` is the demand read before the paint; a paint may set a
    /// motion going itself (a tooltip's or popover's arrival starts as the
    /// frame draws its first pose), which no read before it could see, so
    /// a tick that painted at rest reads the demand again (`moving_now`).
    /// Returns whether the paint set a motion going: the loop must then
    /// come back on the next cadence, or the motion stays at its first,
    /// invisible pose until something unrelated paints.
    fn after_tick(
        &mut self,
        animating: bool,
        painted: bool,
        moving_now: impl FnOnce() -> bool,
    ) -> bool {
        let set_going = painted && !animating && moving_now();
        if animating || set_going {
            self.owed = true;
        } else if painted {
            self.owed = false;
        }
        set_going
    }
}

/// Who a painted tick's motion belongs to, for the frame trace
/// ([`metrics::FrameSample::paint_animations`]). Naming a content widget
/// walks the widget tree, so the label is worked out when the set of
/// animating sources (or the chrome) changes, and otherwise at most once per
/// [`ANIMATION_LABEL_REFRESH`]. The set of sources does not see which content
/// widget animates: one window's opening can hand the motion on to another
/// widget with the same `content` source, and a label kept for the whole run
/// named the window that had long since settled.
#[derive(Default)]
struct AnimationTraceLabel {
    key: Option<(crate::shell::frame::AnimationDemand, bool, u64)>,
    walked: Option<Instant>,
    label: &'static str,
}

/// How long a frame-trace motion label is kept while its sources stay the same.
const ANIMATION_LABEL_REFRESH: Duration = Duration::from_secs(1);

impl AnimationTraceLabel {
    fn label(
        &mut self,
        key: (crate::shell::frame::AnimationDemand, bool, u64),
        now: Instant,
        owners: impl FnOnce() -> Vec<String>,
    ) -> &'static str {
        let stale = self
            .walked
            .is_none_or(|at| now.saturating_duration_since(at) >= ANIMATION_LABEL_REFRESH);
        if self.key != Some(key) || stale {
            self.key = Some(key);
            self.walked = Some(now);
            self.label = metrics::intern_animations(&animation_trace_label(&owners(), key.1));
        }
        self.label
    }
}

/// [`AnimationTraceLabel`]'s text: each owner once, a widget path without
/// its geometry (which moves with the motion and would make every frame a
/// new label), `transition` for a compositor transition's frame run, and
/// `resting-frame` when nothing animates any more and the tick is the one
/// frame [`AnimationSettle`] owes after motion. Commas would split the CSV
/// cell, so none survive.
fn animation_trace_label(owners: &[String], transition: bool) -> String {
    let mut names: Vec<String> = Vec::new();
    for owner in owners {
        let path = owner
            .split('@')
            .next()
            .unwrap_or_default()
            .replace(',', ";");
        if !path.is_empty() && !names.contains(&path) {
            names.push(path);
        }
    }
    if transition {
        names.push("transition".to_string());
    }
    if names.is_empty() {
        names.push("resting-frame".to_string());
    }
    names.join("+")
}

const LOCK_CLOCK_REFRESH: Duration = Duration::from_secs(60);
const IDLE_BACKEND_POLL: Duration = Duration::from_millis(50);

/// Loop iterations of work after which a quiet park asks for a heap trim: startup
/// or a burst of input, not the one tick of a minute's clock repaint.
const QUIET_TRIM_AFTER_TICKS: u32 = 30;

/// A park at least this long (or one only an fd ends) is the loop going quiet.
const QUIET_PARK: Duration = Duration::from_secs(1);

/// How long the desktop rests after a frame that changed it before the
/// layers its strip pans are composited from are drawn again
/// (`FrameBuilder::layers_at_rest`): a pause in typing, not every key.
const LAYER_WARM_SETTLE: Duration = Duration::from_secs(1);
const WAYLAND_CLIENT_SERVICE_FALLBACK: Duration = Duration::from_millis(4);
const WAYLAND_REQUESTS_PER_CLIENT_PER_TICK: usize = 64;
const WAYLAND_PENDING_RETRY_FLOOR: Duration = Duration::from_millis(1);
const SLOW_WAYLAND_CLIENT_DISPATCH: Duration = Duration::from_millis(10);
const SLOW_PREPAINT_PHASE: Duration = Duration::from_millis(50);

/// What the input phase's trace calls the time spent on `event`.
fn input_event_step(event: &BackendEvent) -> &'static str {
    match event {
        BackendEvent::Input(WindowEvent::KeyboardInput { .. })
        | BackendEvent::PhysicalKey { .. } => "key-event",
        BackendEvent::Input(_) => "pointer-event",
        _ => "backend-event",
    }
}

fn slow_prepaint_phase(elapsed: Duration) -> bool {
    elapsed >= SLOW_PREPAINT_PHASE
}

fn wayland_client_service_deadline(wake_registered: bool, now: Instant) -> Option<Instant> {
    if wake_registered {
        None
    } else {
        now.checked_add(WAYLAND_CLIENT_SERVICE_FALLBACK)
    }
}

/// Client requests are dispatched after the tick's pump and paint decision,
/// and what they change (a new toplevel awaiting its first configure, a
/// title, a selection) is only picked up by the next tick's pump. Nothing
/// else need wake that tick on a backend that is never polled, so run it at
/// once; a tick that dispatches nothing parks normally.
///
/// A commit that only carries `wl_surface.frame` changes neither pixels nor
/// protocol the pump reads. Answering that callback is not a reason to run
/// the pump again: doing so woke an idle desktop about twice a second and
/// painted the client's echo.
fn client_followup_needed(clients_dispatched: bool, pump_visible: bool) -> bool {
    clients_dispatched && pump_visible
}

fn client_followup_deadline(pump_needed: bool, now: Instant) -> Option<Instant> {
    pump_needed.then_some(now)
}

/// Where the client phase of one loop iteration spent its time, for the
/// `SLOW-ITER` line: a stall there names its step instead of the whole phase.
struct ClientSteps {
    last: Instant,
    steps: [(&'static str, Duration); 8],
    len: usize,
}

impl ClientSteps {
    fn start() -> Self {
        Self {
            last: Instant::now(),
            steps: [("", Duration::ZERO); 8],
            len: 0,
        }
    }

    fn mark(&mut self, step: &'static str) {
        let now = Instant::now();
        if let Some(slot) = self.steps.get_mut(self.len) {
            *slot = (step, now.saturating_duration_since(self.last));
            self.len = self.len.saturating_add(1);
        }
        self.last = now;
    }
}

impl std::fmt::Display for ClientSteps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, (step, elapsed)) in self.steps.iter().take(self.len).enumerate() {
            if index > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{step}={elapsed:?}")?;
        }
        Ok(())
    }
}

/// A key event's handling, step by step; reported as `SLOW-KEY` when the
/// whole of it took long enough to delay the frame that answers it.
struct KeySteps {
    started: Instant,
    steps: ClientSteps,
}

const SLOW_KEY: Duration = Duration::from_millis(8);

impl KeySteps {
    fn start() -> Self {
        // A handler noted for an earlier key is not this key's.
        let _ = crate::compositor::key_latency::take_key_handler();
        Self {
            started: Instant::now(),
            steps: ClientSteps::start(),
        }
    }
}

impl Drop for KeySteps {
    fn drop(&mut self) {
        let total = self.started.elapsed();
        let handler = crate::compositor::key_latency::take_key_handler();
        if total >= SLOW_KEY {
            self.steps.mark("rest");
            match handler {
                Some(handler) => {
                    eprintln!(
                        "SLOW-KEY total={total:?} handler={handler:?} ({})",
                        self.steps
                    );
                }
                None => eprintln!("SLOW-KEY total={total:?} ({})", self.steps),
            }
        }
    }
}

fn dispatch_wayland_clients_fairly(
    display: &mut Display<State>,
    state: &mut State,
) -> std::io::Result<usize> {
    let started = Instant::now();
    let dispatched = display
        .backend()
        .dispatch_ready_clients_bounded(state, WAYLAND_REQUESTS_PER_CLIENT_PER_TICK)?;
    let elapsed = started.elapsed();
    if elapsed >= SLOW_WAYLAND_CLIENT_DISPATCH {
        tracing::warn!(
            target: "tilo::wayland_dispatch",
            dispatched,
            elapsed_ms = elapsed.as_secs_f64() * 1000.0,
            per_client_limit = WAYLAND_REQUESTS_PER_CLIENT_PER_TICK,
            "slow bounded Wayland client batch"
        );
    }
    Ok(dispatched)
}

/// [`native_loop_park_named`]'s timeout alone, for tests about how long the
/// loop sleeps rather than why.
#[cfg(test)]
fn native_loop_park(
    now: Instant,
    backend_has_event_wake: bool,
    wayland_dispatch_pending: bool,
    cadence_needed: bool,
    frame_deadline: Instant,
    deadlines: impl IntoIterator<Item = Option<Instant>>,
) -> Option<Duration> {
    native_loop_park_named(
        now,
        backend_has_event_wake,
        wayland_dispatch_pending,
        cadence_needed,
        frame_deadline,
        deadlines.into_iter().map(|deadline| ("deadline", deadline)),
    )
    .0
}

/// Select how long calloop may sleep. Presentation cadence is a deadline only
/// while visual work is pending; a static desktop instead sleeps until the
/// earliest real service/widget deadline and wakes early on registered fds.
///
/// Also names the deadline the park ends on — the `wake` column of the frame
/// trace, and what paint-stats blames when an idle desktop keeps waking.
/// `None` names a park only an fd can end.
fn native_loop_park_named(
    now: Instant,
    backend_has_event_wake: bool,
    wayland_dispatch_pending: bool,
    cadence_needed: bool,
    frame_deadline: Instant,
    deadlines: impl IntoIterator<Item = (&'static str, Option<Instant>)>,
) -> (Option<Duration>, Option<&'static str>) {
    let wayland_retry = wayland_dispatch_pending
        .then(|| now.checked_add(WAYLAND_PENDING_RETRY_FLOOR).unwrap_or(now));
    let semantic_deadline = deadlines
        .into_iter()
        .filter_map(|(name, deadline)| deadline.map(|at| (at, name)))
        .chain(wayland_retry.map(|at| (at, "wayland-retry")))
        .chain(cadence_needed.then_some((frame_deadline, "cadence")))
        .min_by_key(|(at, _)| *at);

    match semantic_deadline {
        Some((deadline, name)) => (Some(deadline.saturating_duration_since(now)), Some(name)),
        None if !backend_has_event_wake => (Some(IDLE_BACKEND_POLL), Some("idle-backend-poll")),
        None => (None, None),
    }
}

/// Name what ended a park of `park` that lasted `slept`: the deadline it was
/// set for when the timeout ran out, [`metrics::WAKE_IO`] when an fd fired
/// first. A timeout returns a little late, never early, so a sleep within
/// [`WAKE_TIMER_SLACK`] of the timeout still counts as the timer.
fn park_wake_reason(
    park: Option<Duration>,
    deadline: Option<&'static str>,
    slept: Duration,
) -> &'static str {
    match (park, deadline) {
        (Some(timeout), Some(name)) if slept.saturating_add(WAKE_TIMER_SLACK) >= timeout => name,
        _ => super::metrics::WAKE_IO,
    }
}

const WAKE_TIMER_SLACK: Duration = Duration::from_micros(200);

/// Paint-stats' count of what ended each park, by wake name.
#[derive(Default)]
struct WakeTally(Vec<(&'static str, u32)>);

impl WakeTally {
    fn note(&mut self, reason: &'static str) {
        match self.0.iter_mut().find(|(name, _)| *name == reason) {
            Some((_, count)) => *count = count.saturating_add(1),
            None => self.0.push((reason, 1)),
        }
    }

    fn take_summary(&mut self) -> String {
        let summary = self
            .0
            .iter()
            .map(|(name, count)| format!("{name}={count}"))
            .collect::<Vec<_>>()
            .join(",");
        self.0.clear();
        summary
    }
}

mod lock_view;
use lock_view::{LockClock, publish_accessibility_tree, retain_session_redraw};

/// A widget poll already ran in this tick's pump. Its deadline waking the
/// loop is not a present: the tree, the layout, and any animation are the
/// pump's result and the animation bit. Painting because the deadline
/// merely came due kept idle apps submitting frames.
fn present_for_app_poll(pump_changed: bool, _deadline_due: bool) -> bool {
    pump_changed
}

/// App-side scheduling for one native tick. Keep the pump's redraw decision
/// and the post-paint park together: a poll is work, not necessarily pixels,
/// and its answered deadline must not schedule that same work again.
struct NativeAppTick {
    tick_started: Instant,
    redraw: bool,
}

impl NativeAppTick {
    fn after_pump(frame: &FrameBuilder, changed: bool, tick_started: Instant) -> Self {
        let deadline_due = frame
            .next_wake_deadline()
            .is_some_and(|at| at <= tick_started);
        Self {
            tick_started,
            redraw: present_for_app_poll(changed, deadline_due),
        }
    }

    fn park_deadline(&self, frame: &FrameBuilder, pipe_ready: bool) -> Option<Instant> {
        widget_deadline_for_presenter(
            pipe_ready,
            widget_park_deadline(self.tick_started, frame.next_wake_deadline()),
        )
    }
}

/// A `wl_surface.frame` on a commit that did not change the tree, the
/// layout, or an animation is not a present. Release it on its own.
/// A callback asked for after this tick presented is released too: the
/// present already went out without it, so waiting for the next one stalls
/// the client until something unrelated changes the screen, and a client
/// waiting on that callback draws nothing that would. Only a frame this tick
/// still owes (one held back by the cadence or a full presenter) keeps its
/// callbacks for that present.
/// While locked the desktop is not shown, and the callback stays until a
/// real lock or unlock frame, as it did before idle apps stopped driving
/// cadence.
/// A blanked display hides the desktop as the lock does: callbacks wait for
/// the frame that wakes it, so clients rest too.
fn release_idle_client_callback(hidden: bool, content_changed: bool, presented: bool) -> bool {
    !hidden && (!content_changed || presented)
}

/// What one tick draws for what changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TickPaint {
    /// Something new is to be shown on this tick.
    content_changed: bool,
    /// A frame is built and handed to the presenter.
    should_paint: bool,
    /// A change seen but not painted, carried to the next tick.
    owed: bool,
}

/// Decide what a tick draws. A blanked display shows nothing, so nothing is
/// built or handed to the GPU while it is dark; what changed is owed to the
/// frame that wakes it.
fn tick_paint(
    reasons: NativeRedrawReasons,
    session: &SessionManager,
    cadence_due: bool,
    pipe_ready: bool,
) -> TickPaint {
    let wanted = reasons.content_changed(session.is_locked());
    let content_changed = wanted && !session.is_blanked();
    let should_paint = content_changed && cadence_due && pipe_ready;
    TickPaint {
        content_changed,
        should_paint,
        owed: wanted && !should_paint,
    }
}

/// Whether the next park must keep the frame deadline for a pending frame.
/// While the bounded presenter is full, an already-due frame deadline would
/// make every park a zero timeout and spin the loop until a frame retires;
/// the presenter's retirement ping wakes calloop as soon as capacity returns.
fn cadence_wait_needed(pipe_ready: bool, frame_pending: bool) -> bool {
    pipe_ready && frame_pending
}

/// Whether a tick that is about to park leaves its inputs answered without a
/// frame: it painted nothing, nothing is dirty (a frame held back by a full
/// presenter is dirty), and no frame is awaited (no cadence wait is armed and
/// no redraw was asked for after the paint decision).
fn unpainted_input_settled(painted: bool, content_changed: bool, frame_awaited: bool) -> bool {
    !(painted || content_changed || frame_awaited)
}

/// A transition the presenter could not take stays pending. The park while
/// the pipe is full with a frame due is not animation time; the next submit
/// still has the frame run ahead of it. A full pipe before the frame is due is
/// the normal wait for a vblank-paced presenter's flip, and is animation time.
fn present_hold_credit(presenter_full: bool, frame_due: bool, transition_pending: bool) -> bool {
    presenter_full && frame_due && transition_pending
}

/// A widget deadline only matters while the presenter can accept the frame
/// it would cause. Under back-pressure an overdue animation deadline would
/// turn every park into an immediate return while the paint gate keeps
/// declining the frame; retirement pings the loop once capacity is restored.
fn widget_deadline_for_presenter(
    pipe_ready: bool,
    widget_deadline: Option<Instant>,
) -> Option<Instant> {
    widget_deadline.filter(|_| pipe_ready)
}

#[cfg(test)]
#[path = "native_idle_work_tests.rs"]
mod idle_work_tests;

/// A widget deadline wakes the loop once. A widget may keep an overdue
/// deadline until a paint consumes it, while a due deadline alone does not
/// paint (`present_for_app_poll`): a deadline that was already due when
/// this tick started was answered by it, and waking for it again would run
/// the loop flat out without painting until something unrelated paints.
fn widget_park_deadline(tick_started: Instant, deadline: Option<Instant>) -> Option<Instant> {
    deadline.filter(|deadline| *deadline > tick_started)
}

/// How often a locked session re-reads its players, so the lock card follows
/// a track change or a pause made elsewhere.
const LOCK_MEDIA_REFRESH: Duration = Duration::from_secs(1);

/// The output id of the display the person works on, as the desktop's
/// partition records it.
fn focused_display_id(frame_builder: &FrameBuilder) -> Option<u32> {
    frame_builder
        .chrome()
        .tree()
        .desktop
        .outputs
        .focused_display()
}

/// The player the lock screen shows: the one the now-playing card would,
/// among players that are playing or paused. A stopped player has nothing
/// to control from a locked screen.
fn lock_now_playing() -> Option<crate::shell::now_playing::PlayerSnapshot> {
    use crate::shell::now_playing::{MprisSource, NowPlayingSource, PlaybackStatus, active_index};
    let source = MprisSource;
    source.refresh();
    let players: Vec<_> = source
        .players()
        .into_iter()
        .filter(|player| player.status != PlaybackStatus::Stopped)
        .collect();
    let index = active_index(&players, None)?;
    players.into_iter().nth(index)
}

/// Whether the lock card must repaint: anything it shows changed. The
/// playhead is not shown on the lock screen, so its movement is not a change.
fn lock_media_changed(
    before: Option<&crate::shell::now_playing::PlayerSnapshot>,
    after: Option<&crate::shell::now_playing::PlayerSnapshot>,
) -> bool {
    match (before, after) {
        (None, None) => false,
        (Some(before), Some(after)) => {
            before.bus_name != after.bus_name
                || before.status != after.status
                || before.title != after.title
                || before.artist != after.artist
                || before.artwork != after.artwork
                || before.can_play != after.can_play
                || before.can_pause != after.can_pause
                || before.can_go_next != after.can_go_next
                || before.can_go_previous != after.can_go_previous
        }
        _ => true,
    }
}

/// Send one lock-screen transport command to exactly the player the lock
/// card shows. Transport is all a locked session may do to media.
fn send_lock_transport(
    player: Option<&crate::shell::now_playing::PlayerSnapshot>,
    transport: crate::shell::quick_settings::now_playing::Transport,
) -> bool {
    use crate::shell::now_playing::{MprisSource, NowPlayingSource};
    let Some(player) = player.filter(|player| transport.enabled(player)) else {
        return false;
    };
    MprisSource.send(&player.bus_name, transport.command())
}

fn lock_clock_refresh_due(session_locked: bool, now: Instant, last_refresh: &mut Instant) -> bool {
    if !session_locked {
        *last_refresh = now;
        return false;
    }
    if now.saturating_duration_since(*last_refresh) >= LOCK_CLOCK_REFRESH {
        *last_refresh = now;
        true
    } else {
        false
    }
}

pub(crate) fn wayland_peer_credentials(stream: &UnixStream) -> Option<libc::ucred> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = libc::socklen_t::try_from(std::mem::size_of::<libc::ucred>()).ok()?;
    // SAFETY: both pointers name initialized storage of `length` bytes and the
    // socket remains open for the duration of `getsockopt`.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::from_mut(&mut credentials).cast(),
            std::ptr::from_mut(&mut length),
        )
    };
    if result != 0 || usize::try_from(length).ok()? < std::mem::size_of::<libc::ucred>() {
        return None;
    }
    (credentials.pid > 0).then_some(credentials)
}

/// The executable of the process on the other end of `stream`, as the
/// kernel reports it — the identity a trusted display tool is keyed by.
fn peer_executable(stream: &UnixStream) -> Option<String> {
    let credentials = wayland_peer_credentials(stream)?;
    std::fs::read_link(format!("/proc/{}/exe", credentials.pid))
        .ok()?
        .into_os_string()
        .into_string()
        .ok()
}

pub(crate) fn peer_shares_host_security_namespaces(pid: u32) -> bool {
    ["user", "mnt"].into_iter().all(|namespace| {
        let host = std::fs::metadata(format!("/proc/self/ns/{namespace}"));
        let peer = std::fs::metadata(format!("/proc/{pid}/ns/{namespace}"));
        matches!((host, peer), (Ok(host), Ok(peer)) if host.dev() == peer.dev() && host.ino() == peer.ino())
    })
}

fn unattributed_host_client_allowed(peer_uid: libc::uid_t, shares_namespaces: bool) -> bool {
    peer_uid == rustix::process::geteuid().as_raw() && shares_namespaces
}

fn public_client_state(stream: &UnixStream) -> Option<Arc<ClientState>> {
    let credentials = wayland_peer_credentials(stream)?;
    let pid = u32::try_from(credentials.pid).ok()?;
    if credentials.uid != rustix::process::geteuid().as_raw() {
        return None;
    }
    if let Some((identity, custody)) =
        crate::shell::launcher::verified_launch_custody_for_descendant(pid)
    {
        return Some(ClientState::attributed_with_custody_arc(
            identity,
            Some(custody),
            pid,
        ));
    }
    unattributed_host_client_allowed(credentials.uid, peer_shares_host_security_namespaces(pid))
        .then(|| {
            Arc::new(ClientState {
                legacy_guest: std::fs::read(format!("/proc/{pid}/environ")).ok().and_then(
                    |environ| legacy_agent_protocol::window::guest_from_environ(&environ),
                ),
                ..ClientState::admitted(pid)
            })
        })
}

/// Start the worker that attributes each new public Wayland connection
/// ([`public_client_state`]) off the loop, and the loop source that inserts
/// the clients it admits. Kernel credentials bind a sandboxed connection to
/// the exact compositor-supervised package/workspace launch; a direct host
/// client has no record and remains explicitly unconfined. Attributing reads
/// `/proc` (the process's ancestry, start times, namespaces and
/// environment), which the loop never waits for. Answers where to send each
/// accepted connection.
fn admit_public_clients(
    handle: &calloop::LoopHandle<'static, State>,
) -> std::io::Result<std::sync::mpsc::Sender<UnixStream>> {
    let (accepted, connections) = std::sync::mpsc::channel::<UnixStream>();
    let (admit, admissions) = calloop::channel::channel::<(UnixStream, Arc<ClientState>)>();
    handle
        .insert_source(admissions, |event, (), state| {
            if let calloop::channel::Event::Msg((stream, client_state)) = event
                && let Err(err) = state.display_handle.insert_client(stream, client_state)
            {
                eprintln!("tilo compositor: failed to insert client: {err}");
            }
        })
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    std::thread::Builder::new()
        .name("client-admission".into())
        .spawn(move || {
            while let Ok(stream) = connections.recv() {
                let Some(client_state) = public_client_state(&stream) else {
                    tracing::warn!("rejected unattributed Wayland client outside host namespaces");
                    continue;
                };
                if admit.send((stream, client_state)).is_err() {
                    return;
                }
            }
        })?;
    Ok(accepted)
}

fn external_floating_id(connection: u64, id: &str) -> String {
    format!("external:{connection}:{id}")
}

/// Resolve only the exact live launch that authenticated this shell-ui channel.
/// App ids and requested regions are descriptive; they never select another
/// instance's primary surface or substitute for its kernel process custody.
fn authenticated_app_owned_source(
    state: &State,
    frame_builder: &mut FrameBuilder,
    app_id: &str,
    launch: Option<&crate::compositor::floating_service::AuthenticatedLaunch>,
) -> Result<(u64, Rect), String> {
    let launch = launch.ok_or("floating source has no authenticated launch custody")?;
    if !crate::shell::launcher::launch_custody_is_current(
        launch.custody,
        app_id,
        &launch.workspace_id,
    ) {
        return Err("floating source launch custody retired".into());
    }
    let candidates = frame_builder
        .foreign_surfaces_for_app_room(app_id, &launch.workspace_id)
        .into_iter()
        .filter(|(surface, _)| state.surface_launch_custody(*surface) == Some(launch.custody))
        .collect::<Vec<_>>();
    match candidates.as_slice() {
        [source] => Ok(*source),
        _ => Err("floating source is unavailable or ambiguous for its exact launch".into()),
    }
}

#[derive(Clone)]
pub(crate) struct NativeAgentMarkerSource {
    launch: crate::compositor::floating_service::AuthenticatedLaunch,
    client: smithay::reexports::wayland_server::backend::ClientId,
    surface: u64,
    app_id: String,
    rect: Rect,
    painted_rect: Rect,
    agents: HashMap<String, String>,
}

impl NativeAgentMarkerSource {
    fn is_current(&self, state: &State, frame: &mut FrameBuilder) -> bool {
        let active_room = frame
            .chrome_handle()
            .borrow()
            .tree()
            .desktop
            .active_workspace()
            .map(|workspace| workspace.room().to_string());
        active_room.as_deref() == Some(self.launch.workspace_id.as_str())
            && authenticated_app_owned_source(state, frame, &self.app_id, Some(&self.launch))
                == Ok((self.surface, self.rect))
            && state
                .client_input
                .surface(self.surface)
                .and_then(smithay::reexports::wayland_server::Resource::client)
                .is_some_and(|client| client.id() == self.client)
            && foreign::store().is_presented(self.surface)
            && foreign::store().painted_frame_rect(self.surface, self.rect)
                == Some(self.painted_rect)
    }
}

fn publish_native_agent_markers(
    state: &mut State,
    frame: &mut FrameBuilder,
    connection: u64,
    app_id: &str,
    launch: Option<&crate::compositor::floating_service::AuthenticatedLaunch>,
    markers: Vec<ui::app::PaintedActionMarker>,
) {
    let previous = state.native_agent_markers.remove(&connection);
    // Apps acknowledge every presented frame, including frames with no marker.
    // No previous source means there is no composer continuation to validate.
    if markers.is_empty() && previous.is_none() {
        frame.host_mut().retire_accessible_agent_markers(connection);
        return;
    }
    let source = (|| {
        let (surface, rect) = authenticated_app_owned_source(state, frame, app_id, launch).ok()?;
        let client = state.client_input.surface(surface)?.client()?.id();
        let source = NativeAgentMarkerSource {
            launch: launch?.clone(),
            client,
            surface,
            app_id: app_id.to_owned(),
            rect,
            painted_rect: foreign::store().painted_frame_rect(surface, rect)?,
            agents: HashMap::new(),
        };
        source.is_current(state, frame).then_some(source)
    })();
    let Some(mut source) = source else {
        frame.host_mut().retire_accessible_agent_markers(connection);
        return;
    };
    let room = source.launch.workspace_id.clone();
    let published: Vec<crate::shell::overlay_host::AccessibleAgentMarker> = markers
        .into_iter()
        .take(16)
        .filter_map(|painted| {
            let identity = frame
                .host()
                .certified_marker_agent(&room, &painted.interaction.actor.id)?;
            if painted.interaction.actor.kind != tilo_actions::ActorKind::Agent {
                return None;
            }
            let viewport = Rect::new(
                painted.viewport[0],
                painted.viewport[1],
                painted.viewport[2],
                painted.viewport[3],
            );
            if !viewport.is_finite() || viewport.width() <= 0.0 || viewport.height() <= 0.0 {
                return None;
            }
            let placement = vello::kurbo::Affine::translate((-viewport.x0, -viewport.y0))
                .then_scale_non_uniform(
                    source.painted_rect.width() / viewport.width(),
                    source.painted_rect.height() / viewport.height(),
                )
                .then_translate((source.painted_rect.x0, source.painted_rect.y0).into());
            let translate = |values: [f64; 4]| {
                let rect = Rect::new(values[0], values[1], values[2], values[3]);
                if !rect.is_finite() || rect.width() <= 0.0 || rect.height() <= 0.0 {
                    return None;
                }
                let placed = placement
                    .transform_rect_bbox(rect)
                    .intersect(source.painted_rect);
                (placed.width() > 0.0 && placed.height() > 0.0).then_some(placed)
            };
            let user = ui::permissions::UserId(identity.participant_id);
            Some(crate::shell::overlay_host::AccessibleAgentMarker {
                connection,
                surface: source.surface,
                app_id: source.app_id.clone(),
                room: room.clone(),
                agent: identity.agent_key,
                marker: translate(painted.marker)?,
                locus: translate(painted.locus)?,
                protected: translate(painted.protected)?,
                label: frame.presence().display_name(&user),
                description: frame
                    .presence()
                    .interaction_status_label(&painted.interaction),
                interaction: painted.interaction,
                published_at: Instant::now(),
            })
        })
        .collect();
    if let Some(previous) = previous
        && previous.client == source.client
        && previous.surface == source.surface
        && previous.launch == source.launch
    {
        source.agents = previous.agents;
    }
    source.agents.extend(published.iter().map(
        |marker: &crate::shell::overlay_host::AccessibleAgentMarker| {
            (marker.agent.clone(), marker.interaction.actor.id.clone())
        },
    ));
    frame
        .host_mut()
        .publish_accessible_agent_markers(connection, published);
    state.native_agent_markers.insert(connection, source);
}

fn revalidate_native_agent_markers(
    state: &mut State,
    frame: &mut FrameBuilder,
    suppressed: bool,
) -> bool {
    let current: HashMap<_, _> = state
        .native_agent_markers
        .iter()
        .filter(|(_, source)| !suppressed && source.is_current(state, frame))
        .map(|(connection, source)| (*connection, source.clone()))
        .collect();
    state.native_agent_markers.clone_from(&current);
    let identities: HashMap<_, _> = current
        .iter()
        .map(|(connection, source)| {
            (
                *connection,
                (
                    source.surface,
                    source.app_id.clone(),
                    source.launch.workspace_id.clone(),
                ),
            )
        })
        .collect();
    // Resolve owner-backed membership before borrowing the overlay mutably.
    let broker = frame.host().marker_action_session_source();
    let changed = frame.host_mut().retain_accessible_agent_markers(|marker| {
        marker.is_fresh(Instant::now())
            && identities
                .get(&marker.connection)
                .is_some_and(|(surface, app, room)| {
                    *surface == marker.surface && *app == marker.app_id && *room == marker.room
                })
            && broker
                .as_ref()
                .and_then(|source| {
                    source.certified_marker_agent(&marker.room, &marker.interaction.actor.id)
                })
                .is_some_and(|identity| identity.agent_key == marker.agent)
    });
    // A composer opened on a selection stands beside no marker, so only the
    // agent's admission (the identity revision) and suppression close it.
    let about_selection = frame
        .host_mut()
        .agent_quick_composer_mut()
        .about_selection();
    if let Some(owner) = frame.host_mut().agent_quick_composer_mut().owner().cloned()
        && (suppressed
            || !about_selection
                && !current.values().any(|source| {
                    source.launch.workspace_id == owner.room
                        && source
                            .agents
                            .get(&owner.agent)
                            .and_then(|actor| {
                                broker.as_ref()?.certified_marker_agent(&owner.room, actor)
                            })
                            .is_some_and(|identity| identity.agent_key == owner.agent)
                })
            || broker
                .as_ref()
                .and_then(|source| source.identity_revision(&owner.room))
                != Some(owner.identity_revision))
    {
        frame.host_mut().agent_quick_composer_mut().dismiss();
    }
    let Some(marker) = frame.host().take_accessible_agent_marker_activation() else {
        return changed;
    };
    let Some(source) = current.get(&marker.connection) else {
        return changed;
    };
    if !source.is_current(state, frame) {
        return changed;
    }
    let Some(session) = broker.as_ref() else {
        return changed;
    };
    let Some(identity) = session.certified_marker_agent(&marker.room, &marker.interaction.actor.id)
    else {
        return changed;
    };
    if identity.agent_key != marker.agent {
        return changed;
    }
    let Some(identity_revision) = session.identity_revision(&marker.room) else {
        return changed;
    };
    drop(broker);
    let person = frame.chrome_handle().borrow().active_local_user();
    frame.host_mut().open_agent_quick_composer(
        crate::shell::agent_quick_composer::ComposerOwner {
            room: marker.room,
            agent: identity.agent_key,
            person,
            identity_revision,
        },
        &marker.label,
        crate::shell::agent_quick_composer::AgentQuickComposerOrigin {
            marker: marker.marker,
            protected: marker.protected,
            context: marker.description,
        },
    ) || changed
}

fn external_floating_parts(id: &str) -> Option<(u64, &str)> {
    let (connection, public_id) = id.strip_prefix("external:")?.split_once(':')?;
    Some((connection.parse().ok()?, public_id))
}

fn translate_external_region(region: Rect, app_rect: Rect) -> Result<Rect, String> {
    let translated = Rect::new(
        app_rect.x0 + region.x0,
        app_rect.y0 + region.y0,
        app_rect.x0 + region.x1,
        app_rect.y0 + region.y1,
    )
    .intersect(app_rect);
    (translated.area() > 0.0)
        .then_some(translated)
        .ok_or_else(|| "floating source lies outside its authenticated app surface".into())
}

fn translate_external_source(
    source: ui::floating_surface::FloatingSource,
    authenticated_app_id: &str,
    app_rect: Rect,
) -> Result<ui::floating_surface::FloatingSource, String> {
    use ui::floating_surface::FloatingSource;
    Ok(match source {
        FloatingSource::AppRegion { app_id, region } => {
            if app_id != authenticated_app_id {
                return Err(format!(
                    "floating source app id {app_id:?} does not match authenticated app {authenticated_app_id:?}"
                ));
            }
            FloatingSource::AppRegion {
                app_id: authenticated_app_id.to_string(),
                region: translate_external_region(region, app_rect)?,
            }
        }
        FloatingSource::AppElement {
            app_id,
            element_id,
            region,
        } => {
            if app_id != authenticated_app_id {
                return Err(format!(
                    "floating source app id {app_id:?} does not match authenticated app {authenticated_app_id:?}"
                ));
            }
            if element_id.trim().is_empty() {
                return Err("floating media element id is empty".into());
            }
            FloatingSource::AppElement {
                app_id: authenticated_app_id.to_string(),
                element_id,
                region: translate_external_region(region, app_rect)?,
            }
        }
        FloatingSource::Composite(sources) => FloatingSource::Composite(
            sources
                .into_iter()
                .map(|source| translate_external_source(source, authenticated_app_id, app_rect))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        FloatingSource::Surface { .. } => {
            return Err("external clients cannot name compositor surface ids".into());
        }
    })
}

fn validate_app_owned_source(
    source: &ui::floating_surface::FloatingSource,
    authenticated_app_id: &str,
) -> Result<(), String> {
    use ui::floating_surface::FloatingSource;
    match source {
        FloatingSource::AppRegion { app_id, .. } | FloatingSource::AppElement { app_id, .. }
            if app_id == authenticated_app_id =>
        {
            Ok(())
        }
        FloatingSource::Composite(sources) => sources
            .iter()
            .try_for_each(|source| validate_app_owned_source(source, authenticated_app_id)),
        FloatingSource::AppRegion { app_id, .. } | FloatingSource::AppElement { app_id, .. } => {
            Err(format!(
                "floating source app id {app_id:?} does not match authenticated app {authenticated_app_id:?}"
            ))
        }
        FloatingSource::Surface { .. } => {
            Err("external clients cannot name compositor surface ids".into())
        }
    }
}

fn app_owned_source_region(
    source: &ui::floating_surface::FloatingSource,
    authenticated_app_id: &str,
    app_size: vello::kurbo::Size,
) -> Option<Rect> {
    let (ui::floating_surface::FloatingSource::AppRegion { app_id, region }
    | ui::floating_surface::FloatingSource::AppElement { app_id, region, .. }) = source
    else {
        return None;
    };
    let bounds = Rect::from_origin_size(vello::kurbo::Point::ZERO, app_size);
    let visible = region.intersect(bounds);
    (app_id == authenticated_app_id && visible.area() > 0.0).then_some(visible)
}

/// Region of the primary app buffer that must survive while its native
/// floating surface commits its first frame.
///
/// `source` identifies the live content, but an app-owned call/navigation
/// float also carries title chrome, controls, shadow and its soft silhouette
/// in `presentation_rect`. Capturing only `source` makes those decorations
/// disappear as soon as the app receives `PrepareSurface`. The old primary
/// window can only contain the portion still inside its own bounds, so use
/// that exact visible intersection; moving the frozen proxy to the same
/// desktop rect then reproduces the last primary pixels without inventing an
/// opaque halo beyond the app edge.
fn app_owned_transfer_region(
    source: &ui::floating_surface::FloatingSource,
    presentation_rect: Option<Rect>,
    authenticated_app_id: &str,
    app_size: vello::kurbo::Size,
) -> Option<Rect> {
    let source = app_owned_source_region(source, authenticated_app_id, app_size)?;
    let bounds = Rect::from_origin_size(vello::kurbo::Point::ZERO, app_size);
    let decorated = presentation_rect.unwrap_or(source).intersect(bounds);
    (decorated.area() > 0.0).then_some(decorated)
}

// Coordinates have already been clamped to the u32-sized pixel buffer. The
// float-to-integer conversion intentionally rounds outward to retain edges.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "Crop endpoints are floor/ceil rounded and clamped to nonnegative u32 frame dimensions before conversion."
)]
fn frozen_transfer_frame(
    surface_id: u64,
    source: Rect,
    app_size: vello::kurbo::Size,
) -> Option<crate::compositor::state::FloatingTransferFrame> {
    let frame = foreign::store().snapshot(surface_id)?;
    if app_size.width <= 0.0 || app_size.height <= 0.0 || source.area() <= 0.0 {
        return None;
    }
    let scale_x = f64::from(frame.width) / app_size.width;
    let scale_y = f64::from(frame.height) / app_size.height;
    let x0 = (source.x0 * scale_x)
        .floor()
        .clamp(0.0, f64::from(frame.width)) as u32;
    let y0 = (source.y0 * scale_y)
        .floor()
        .clamp(0.0, f64::from(frame.height)) as u32;
    let x1 = (source.x1 * scale_x)
        .ceil()
        .clamp(0.0, f64::from(frame.width)) as u32;
    let y1 = (source.y1 * scale_y)
        .ceil()
        .clamp(0.0, f64::from(frame.height)) as u32;
    let width = x1.saturating_sub(x0);
    let height = y1.saturating_sub(y0);
    if width == 0 || height == 0 {
        return None;
    }
    let mut rgba = Vec::with_capacity(
        usize::try_from(width)
            .unwrap_or(0)
            .saturating_mul(usize::try_from(height).unwrap_or(0))
            .saturating_mul(4),
    );
    let stride = usize::try_from(frame.width).ok()?.saturating_mul(4);
    let row_start = usize::try_from(x0).ok()?.saturating_mul(4);
    let row_len = usize::try_from(width).ok()?.saturating_mul(4);
    for y in y0..y1 {
        let start = usize::try_from(y)
            .ok()?
            .saturating_mul(stride)
            .saturating_add(row_start);
        rgba.extend_from_slice(frame.rgba.get(start..start.saturating_add(row_len))?);
    }
    Some(crate::compositor::state::FloatingTransferFrame {
        rgba: Arc::new(rgba),
        width,
        height,
    })
}

/// Freeze the complete app-owned secondary surface for the reverse handoff.
/// Unlike the detach snapshot, this buffer already contains the complete
/// transparent window (soft outline, shadow, controls and content), so no
/// app-local crop or coordinate conversion is needed.
fn frozen_floating_return_frame(
    surface_id: u64,
) -> Option<crate::compositor::state::FloatingTransferFrame> {
    let frame = foreign::store().snapshot(surface_id)?;
    Some(crate::compositor::state::FloatingTransferFrame {
        rgba: Arc::new(frame.rgba),
        width: frame.width,
        height: frame.height,
    })
}

#[allow(
    clippy::arithmetic_side_effects,
    reason = "Kurbo rect/vector addition translates app-local f64 geometry into desktop coordinates without integer arithmetic."
)]
fn app_local_to_desktop(rect: Rect, app_rect: Rect) -> Rect {
    rect + app_rect.origin().to_vec2()
}

/// Serve the authenticated, workspace-scoped chooser connection alongside
/// any desktop portal. Legacy runtimes may use this connection for documents
/// while using the filtered session bus for notifications. `attended` says whether an
/// agent is driving the screen; an unattended session (one that saves into
/// `TILO_SAVE_DIRECTORY`) otherwise leaves its choosers to the apps.
fn start_chooser_service(
    portal: Option<&PortalBackend>,
    attended: Arc<AtomicBool>,
    wake: crate::session_runtime::Wake,
    documents: Option<tilo_documents::Documents>,
) -> Option<crate::compositor::chooser_service::ChooserService> {
    let portal = portal?;
    let save_directory = std::env::var_os(tilo_dialogs::portal::SAVE_DIRECTORY_ENV)
        .filter(|directory| !directory.is_empty())
        .map(std::path::PathBuf::from);
    let unattended = save_directory.is_some();
    let context = crate::compositor::chooser_service::ChooserContext {
        grants: portal.media_grants.clone(),
        pickers: Arc::new(portal.pickers.clone()),
        presence: Arc::new(move || !unattended || attended.load(Ordering::Acquire)),
        save_directory,
        wake,
        identity_selected: Arc::new(|room: &str| {
            crate::identity_registry::selected_identity_profile(room)
                .map(|profile| profile.is_some())
                .map_err(|error| error.to_string())
        }),
        documents,
    };
    match crate::compositor::chooser_service::ChooserService::start(context) {
        Ok(service) => {
            tracing::info!("serving the shell's file choosers to apps without a portal");
            Some(service)
        }
        Err(error) => {
            tracing::warn!(%error, "file chooser service unavailable");
            None
        }
    }
}

/// Mount the documents filesystem: the files people choose for apps in the
/// shell's chooser, which the launcher binds into each app's sandbox, one
/// app's view each (`tilo_documents`). Mounted before any app starts, for the
/// session; without FUSE a chooser reaches only what workspaces grant.
fn mount_documents() -> Option<(tilo_documents::Documents, tilo_documents::mount::Mounted)> {
    let at = tilo_documents::mountpoint()?;
    // Kept beside the person's other private portal answers, so a file
    // chosen in an earlier session reopens where it was.
    let documents = match crate::portal::private_state_root() {
        Some(root) => tilo_documents::Documents::kept_in(root.join("tilo/documents.json")),
        None => tilo_documents::Documents::new(),
    };
    match tilo_documents::mount::mount(documents.clone(), &at) {
        Ok(mounted) => {
            tracing::info!(at = %at.display(), "serving the files people choose for apps");
            Some((documents, mounted))
        }
        Err(error) => {
            tracing::warn!(
                %error,
                at = %at.display(),
                "no documents filesystem: choosers reach only what workspaces grant"
            );
            None
        }
    }
}

/// Serve system information to the apps the launcher connects, asking the
/// person through the portal's permission prompt for what an app was not
/// granted at launch. Without a portal nobody can be asked: the launcher's
/// grants are all an app gets.
fn start_system_stats_service(
    portal: Option<&PortalBackend>,
    wake: crate::session_runtime::Wake,
) -> Option<tilo_system_stats::server::Server> {
    let prompter: Arc<dyn tilo_system_stats::server::Prompter> = match portal {
        Some(portal) => Arc::new(crate::compositor::system_stats_service::PortalPrompter {
            grants: portal.media_grants.clone(),
            prompts: portal.media_prompts.clone(),
            wake,
        }),
        None => Arc::new(tilo_system_stats::server::NoPrompter),
    };
    crate::compositor::system_stats_service::start(prompter)
}

/// Open the files and links apps asked the chooser service to open, each in
/// the app that owns its type or claims its scheme, in the asking app's
/// workspace.
fn pump_chooser_opens(
    service: &crate::compositor::chooser_service::ChooserService,
    chrome: &crate::shell::chrome_state::Chrome,
) {
    for open in service.drain_opens() {
        let active = chrome.borrow().tree().desktop.active;
        let outcome = if open.caller.workspace_id == active {
            // Taken to the file when the person's own press in the asking
            // app is behind the request; else it opens beside them.
            let person = crate::compositor::activation::person_initiated(
                &chrome.borrow().tree(),
                open.caller.app_id.as_str(),
            );
            crate::shell::doc_open::open_for_app(&open.path, chrome, person)
                .map(|_| ())
                .map_err(|error| error.user_message())
        } else {
            Err(crate::shell::i18n::t("portal-chooser-other-workspace"))
        };
        if let Err(reason) = &outcome {
            tracing::info!(app = %open.caller.app_id, path = %open.path.display(), %reason, "chooser open refused");
        }
        let _ = open.reply.send(outcome);
    }
    service.open_requested_links(chrome);
}

/// Open the identity chooser apps asked for (their workspace has no identity
/// selected yet), when the asking app's workspace is the one on screen: the
/// identity is chosen for the workspace in front. Whether one opened.
fn pump_identity_choosers(
    service: &crate::compositor::chooser_service::ChooserService,
    frame_builder: &mut FrameBuilder,
) -> bool {
    let mut opened = false;
    for request in service.drain_identity_choosers() {
        let active = frame_builder.chrome_handle().borrow().tree().desktop.active;
        let outcome = if request.caller.workspace_id == active {
            opened |= frame_builder.host_mut().open_identity_chooser();
            Ok(())
        } else {
            Err(crate::shell::i18n::t("portal-chooser-other-workspace"))
        };
        if let Err(reason) = &outcome {
            tracing::info!(app = %request.caller.app_id, %reason, "identity chooser refused");
        }
        let _ = request.reply.send(outcome);
    }
    opened
}

/// Open the app choosers apps asked for (`ChooserService::serve_app_choosers`)
/// and answer one whose palette closed without a choice.
fn pump_app_choosers(
    service: &crate::compositor::chooser_service::ChooserService,
    frame_builder: &mut FrameBuilder,
) -> bool {
    let active = frame_builder.chrome_handle().borrow().tree().desktop.active;
    let host = frame_builder.host_mut();
    let opened = service.serve_app_choosers(active, |purpose, reply| {
        host.open_app_chooser(purpose, reply);
    });
    host.settle_app_chooser();
    opened
}

/// Answer an app's request about its own tab. The app id is the one the
/// launcher vouched for on this connection.
fn apply_app_tab_request(
    frame_builder: &mut FrameBuilder,
    app_id: &str,
    instance: Option<u64>,
    request: crate::shell::app_tabs::AppTabRequest,
) -> bool {
    let mut outcome = Ok(());
    frame_builder.with_chrome(|st| {
        outcome = crate::shell::app_tabs::apply_request(st, app_id, instance, request);
        st.bump();
    });
    if let Err(error) = outcome {
        tracing::info!(%app_id, %error, "app tab request refused");
    }
    true
}

/// Step through the tabs of the group `app_id`'s focused tab is in: the app
/// had the Ctrl+Tab first and left it alone. The connection vouches for the
/// app id, so an app steps only through its own focused tab's group; a
/// request that arrives after focus moved elsewhere does nothing.
fn apply_app_cycle_tab(
    frame_builder: &mut FrameBuilder,
    app_id: &str,
    instance: Option<u64>,
    forward: bool,
) -> bool {
    let focused = crate::shell::app_tabs::is_focused_app_tab(
        &frame_builder.chrome().tree(),
        app_id,
        instance,
    );
    focused
        && frame_builder
            .host_mut()
            .cycle_focused_tab(if forward { 1 } else { -1 })
}

#[path = "native_hosted_windows.rs"]
mod hosted_windows;
use hosted_windows::{
    desired_legacy_shares, hosts_viewed_window, remote_extents, rooms_hosting_windows,
    shown_hosted_windows, shown_room_hosted_windows,
};

#[cfg(test)]
#[path = "native_app_tab_tests.rs"]
mod app_tab_tests;

#[cfg(test)]
#[path = "native_window_bar_tests.rs"]
mod window_bar_tests;

// This is the ordered protocol event pump; its branches share authentication
// and handoff state that is clearer when kept in a single state-machine tick.
#[allow(
    clippy::too_many_lines,
    reason = "The protocol pump keeps authenticated connection events, surface handoffs, and disconnect cleanup in their processing order."
)]
fn pump_floating_surface_service(
    service: &FloatingSurfaceService,
    state: &mut State,
    frame_builder: &mut FrameBuilder,
    capture: &mut CaptureExecutor,
    connection_ids: &mut HashMap<u64, Vec<String>>,
    appearance: &mut crate::compositor::floating_service::AppearanceDelivery,
    outputs: &[Output],
    fallback_size: (u32, u32),
) -> bool {
    let desktop = floating_desktop_bounds(outputs, fallback_size);
    let mut changed = false;
    for event in service.drain() {
        match event {
            FloatingServiceEvent::Connected {
                connection,
                app_id,
                network_status_allowed,
            } => {
                appearance.connected(connection, &app_id, network_status_allowed);
                frame_builder
                    .host_mut()
                    .connect_window_share(connection, &app_id);
                if app_id == crate::shell::host_shell_allow::TERMINAL_APP_ID
                    && let Some(writer) = service.reply_writer(connection)
                {
                    frame_builder
                        .host_mut()
                        .connect_native_terminal(connection, writer);
                }
                service.send(
                    connection,
                    &ui::floating_surface::FloatingSurfaceHostMessage::DesktopBounds {
                        bounds: ui::floating_surface::protocol_rect_array(desktop),
                    },
                );
                tracing::debug!(connection, %app_id, "authenticated shell-ui client connected");
            }
            FloatingServiceEvent::Message {
                connection,
                app_id,
                launch,
                message,
            } => match message {
                ui::floating_surface::FloatingSurfaceClientMessage::DeviceShellOutcome {
                    request,
                    result,
                } => {
                    if app_id == crate::shell::host_shell_allow::TERMINAL_APP_ID {
                        frame_builder
                            .host_mut()
                            .complete_native_terminal(connection, request, result);
                    }
                    changed = true;
                }
                ui::floating_surface::FloatingSurfaceClientMessage::MoveDeviceDisplay {
                    request,
                    placement,
                } => {
                    let result = frame_builder
                        .host_mut()
                        .request_device_display_move(&app_id, placement);
                    service.send(connection, &ui::floating_surface::FloatingSurfaceHostMessage::DeviceDisplayMoveResult { request, result });
                    changed = true;
                }
                ui::floating_surface::FloatingSurfaceClientMessage::AllowHostShell => {
                    if app_id == crate::shell::host_shell_allow::TERMINAL_APP_ID {
                        if let Some(writer) = service.reply_writer(connection) {
                            frame_builder
                                .host_mut()
                                .request_native_host_shell_allow(writer);
                        }
                    } else {
                        service.send(
                            connection,
                            &ui::floating_surface::FloatingSurfaceHostMessage::HostShellAllowed {
                                allowed: false,
                            },
                        );
                    }
                    changed = true;
                }
                ui::floating_surface::FloatingSurfaceClientMessage::OpenCaptureStudio { mode } => {
                    let mode = match mode {
                        ui::floating_surface::CaptureStudioProtocolMode::Screenshot => {
                            ui::widget::CaptureMode::Screenshot
                        }
                        ui::floating_surface::CaptureStudioProtocolMode::Recording => {
                            ui::widget::CaptureMode::Recording
                        }
                        ui::floating_surface::CaptureStudioProtocolMode::PictureInPicture => {
                            ui::widget::CaptureMode::Pip
                        }
                    };
                    changed |= frame_builder.host_mut().open_capture_studio(mode);
                }
                ui::floating_surface::FloatingSurfaceClientMessage::Present { request } => {
                    let public_id = request.id.clone();
                    let result = ui::floating_surface::FloatingSurfaceRequest::try_from(request)
                        .and_then(|mut request| {
                            let (source_surface_id, app_rect) =
                                if request.role.uses_app_owned_host() {
                                    authenticated_app_owned_source(
                                        state,
                                        frame_builder,
                                        &app_id,
                                        launch.as_ref(),
                                    )?
                                } else {
                                    frame_builder.foreign_surface_for_app(&app_id).ok_or_else(
                                        || format!("{app_id} has no visible foreign surface"),
                                    )?
                                };
                            let owner = request
                                .role
                                .uses_app_owned_host()
                                .then(|| state.floating_owner_for_surface(source_surface_id))
                                .transpose()
                                .map_err(str::to_string)?;
                            if request.role.uses_app_owned_host() {
                                state
                                    .validate_floating_resume_owner(
                                        connection,
                                        &public_id,
                                        owner.as_deref(),
                                    )
                                    .map_err(str::to_string)?;
                            }
                            let (transfer_frame, transfer_source) = if request
                                .role
                                .uses_app_owned_host()
                            {
                                validate_app_owned_source(&request.source, &app_id)?;
                                let source = app_owned_transfer_region(
                                    &request.source,
                                    request.presentation_rect,
                                    &app_id,
                                    app_rect.size(),
                                );
                                let transfer_frame = source.and_then(|region| {
                                    frozen_transfer_frame(
                                        source_surface_id,
                                        region,
                                        app_rect.size(),
                                    )
                                });
                                let transfer_source = transfer_frame.as_ref().and_then(|_| {
                                    source.map(|region| {
                                        crate::compositor::state::FloatingTransferSource {
                                            rect: app_local_to_desktop(region, app_rect),
                                        }
                                    })
                                });
                                request.initial_rect = request
                                    .initial_rect
                                    .map(|rect| app_local_to_desktop(rect, app_rect));
                                request.presentation_rect = request
                                    .presentation_rect
                                    .map(|rect| app_local_to_desktop(rect, app_rect));
                                (transfer_frame, transfer_source)
                            } else {
                                let tracking_source = request.source.clone();
                                request.source =
                                    translate_external_source(request.source, &app_id, app_rect)?;
                                request.tracking_source = Some(tracking_source);
                                (None, None)
                            };
                            request.id = external_floating_id(connection, &public_id);
                            Ok((
                                request,
                                transfer_frame,
                                transfer_source,
                                [app_rect.x0, app_rect.y0],
                                owner,
                                source_surface_id,
                            ))
                        });
                    match result {
                        Ok((
                            request,
                            transfer_frame,
                            transfer_source,
                            coordinate_offset,
                            owner,
                            source_surface_id,
                        )) => {
                            if request.role.uses_app_owned_host() {
                                let content_rect = request.initial_rect.unwrap_or_else(|| {
                                    Rect::from_center_size(desktop.center(), request.preferred_size)
                                });
                                let window_rect = request
                                    .presentation_rect
                                    .unwrap_or(content_rect)
                                    .union(content_rect);
                                let (rect, content_rect) =
                                    clamp_external_floating_geometry_to_outputs(
                                        window_rect,
                                        content_rect,
                                        outputs,
                                        fallback_size,
                                    );
                                let transfer_covered = transfer_frame.is_some();
                                let Ok(resumed) = state.resume_floating_request_with_focus(
                                    connection,
                                    &public_id,
                                    (rect, content_rect),
                                    (transfer_frame.clone(), transfer_source),
                                    owner
                                        .as_ref()
                                        .map(|owner| (source_surface_id, owner.clone())),
                                ) else {
                                    tracing::warn!(
                                        "floating resume rejected incompatible input geometry"
                                    );
                                    continue;
                                };
                                let token = resumed.unwrap_or_else(|| {
                                    let token = uuid::Uuid::new_v4().simple().to_string();
                                    state.authorize_floating_toplevel(
                                        crate::compositor::state::PendingFloatingToplevel {
                                            owner: owner.clone(),
                                            token: token.clone(),
                                            connection,
                                            public_id: public_id.clone(),
                                            app_id: app_id.clone(),
                                            content_rect,
                                            input_rect: None,
                                            rect,
                                            transfer_frame,
                                            transfer_source,
                                            suspended: false,
                                        },
                                    );
                                    token
                                });
                                service.send(
                                    connection,
                                    &ui::floating_surface::FloatingSurfaceHostMessage::PrepareSurface {
                                        id: public_id,
                                        token,
                                        rect: ui::floating_surface::protocol_rect_array(rect),
                                        content_rect: ui::floating_surface::protocol_rect_array(
                                            content_rect,
                                        ),
                                        transfer_covered,
                                        coordinate_offset,
                                    },
                                );
                                changed = true;
                                continue;
                            }
                            let internal_id = request.id.clone();
                            let adopted_rect = request.initial_rect.map_or(
                                [0.0, 0.0, 1.0, 1.0],
                                ui::floating_surface::protocol_rect_array,
                            );
                            capture.queue_external_floating(request);
                            connection_ids
                                .entry(connection)
                                .or_default()
                                .push(internal_id);
                            service.send(
                                connection,
                                &ui::floating_surface::FloatingSurfaceHostMessage::Adopted {
                                    id: public_id,
                                    rect: adopted_rect,
                                },
                            );
                            changed = true;
                        }
                        Err(error) => {
                            tracing::warn!(%error, %app_id, "floating request rejected");
                            service.send(
                                connection,
                                &ui::floating_surface::FloatingSurfaceHostMessage::Closed {
                                    id: public_id,
                                },
                            );
                        }
                    }
                }
                ui::floating_surface::FloatingSurfaceClientMessage::UpdateRegion { id, region } => {
                    let result = ui::floating_surface::protocol_rect(region).and_then(|local| {
                        let (_, app_rect) = frame_builder
                            .foreign_surface_for_app(&app_id)
                            .ok_or_else(|| format!("{app_id} has no visible foreign surface"))?;
                        let translated = Rect::new(
                            app_rect.x0 + local.x0,
                            app_rect.y0 + local.y0,
                            app_rect.x0 + local.x1,
                            app_rect.y0 + local.y1,
                        )
                        .intersect(app_rect);
                        (translated.area() > 0.0)
                            .then_some((translated, local))
                            .ok_or_else(|| "floating update lies outside its app surface".into())
                    });
                    match result {
                        Ok((region, local)) => {
                            capture.update_external_floating_region(
                                external_floating_id(connection, &id),
                                region,
                                app_id.clone(),
                                local,
                            );
                            changed = true;
                        }
                        Err(error) => tracing::warn!(%error, %app_id, "floating update rejected"),
                    }
                }
                ui::floating_surface::FloatingSurfaceClientMessage::SurfaceGeometry {
                    id,
                    rect,
                    content_rect,
                    input_rect,
                } => match (
                    ui::floating_surface::protocol_rect(rect),
                    ui::floating_surface::protocol_rect(content_rect),
                ) {
                    (Ok(window_rect), Ok(content_rect)) => {
                        let Ok(input_rect) =
                            ui::floating_surface::protocol_input_rect(window_rect, input_rect)
                        else {
                            tracing::warn!("floating geometry has invalid input bounds");
                            continue;
                        };
                        let previous_origin = window_rect.union(content_rect).origin();
                        let (rect, content_rect) = clamp_external_floating_geometry_to_outputs(
                            window_rect,
                            content_rect,
                            outputs,
                            fallback_size,
                        );
                        let input_rect = ui::floating_surface::translate_input_rect(
                            input_rect,
                            previous_origin,
                            rect.origin(),
                        );
                        changed |= state.update_floating_toplevel_rect(
                            connection,
                            &id,
                            rect,
                            content_rect,
                            input_rect,
                        );
                    }
                    (Err(error), _) | (_, Err(error)) => {
                        tracing::warn!(%error, %app_id, "floating geometry rejected");
                    }
                },
                ui::floating_surface::FloatingSurfaceClientMessage::SourceCleared { id } => {
                    changed |= state.mark_floating_source_cleared(connection, &id);
                }
                ui::floating_surface::FloatingSurfaceClientMessage::DestinationPresented { id } => {
                    changed |= state.mark_floating_destination_presented(connection, &id);
                }
                ui::floating_surface::FloatingSurfaceClientMessage::Suspend {
                    id,
                    rect,
                    content_rect,
                } => {
                    if let (Ok(window_rect), Ok(content_rect)) = (
                        ui::floating_surface::protocol_rect(rect),
                        ui::floating_surface::protocol_rect(content_rect),
                    ) {
                        let (rect, content_rect) = clamp_external_floating_geometry_to_outputs(
                            window_rect,
                            content_rect,
                            outputs,
                            fallback_size,
                        );
                        changed |= state.update_retained_floating_geometry(
                            connection,
                            &id,
                            rect,
                            content_rect,
                        );
                    }
                    let return_frame = state
                        .floating_toplevels
                        .iter()
                        .find(|surface| surface.connection == connection && surface.public_id == id)
                        .and_then(|surface| frozen_floating_return_frame(surface.surface_id));
                    changed |= state.suspend_floating_toplevel(connection, &id, return_frame);
                    service.send(
                        connection,
                        &ui::floating_surface::FloatingSurfaceHostMessage::Suspended { id },
                    );
                }
                ui::floating_surface::FloatingSurfaceClientMessage::ActionMarkers { markers } => {
                    publish_native_agent_markers(
                        state,
                        frame_builder,
                        connection,
                        &app_id,
                        launch.as_ref(),
                        markers,
                    );
                    changed = true;
                }
                ui::floating_surface::FloatingSurfaceClientMessage::SetCaptureProtection {
                    protected,
                    generation,
                } => {
                    // The app id is the one the launcher vouched for, so a
                    // request can only ever withhold the asking app's own
                    // windows. It narrows what leaves the display, never
                    // widens it, so any app may ask.
                    foreign::store().request_app_capture_protection(connection, &app_id, protected);
                    if let Some(generation) = generation {
                        service.send(connection, &ui::floating_surface::FloatingSurfaceHostMessage::CaptureProtectionApplied { generation, protected });
                    }
                    tracing::debug!(connection, %app_id, protected, "capture protection requested");
                    changed = true;
                }
                ui::floating_surface::FloatingSurfaceClientMessage::OpenTabBeside {
                    target,
                    background,
                    instance,
                } => {
                    changed |= apply_app_tab_request(
                        frame_builder,
                        &app_id,
                        instance,
                        crate::shell::app_tabs::AppTabRequest::OpenBeside { target, background },
                    );
                }
                ui::floating_surface::FloatingSurfaceClientMessage::CloseTab { instance } => {
                    changed |= apply_app_tab_request(
                        frame_builder,
                        &app_id,
                        instance,
                        crate::shell::app_tabs::AppTabRequest::Close,
                    );
                }
                ui::floating_surface::FloatingSurfaceClientMessage::ReopenClosedTab => {
                    changed |= apply_app_tab_request(
                        frame_builder,
                        &app_id,
                        None,
                        crate::shell::app_tabs::AppTabRequest::ReopenClosed,
                    );
                }
                ui::floating_surface::FloatingSurfaceClientMessage::CycleTab {
                    forward,
                    instance,
                } => {
                    changed |= apply_app_cycle_tab(frame_builder, &app_id, instance, forward);
                }
                ui::floating_surface::FloatingSurfaceClientMessage::PlaceNextWindowBeside {
                    background,
                    instance,
                } => {
                    changed |= apply_app_tab_request(
                        frame_builder,
                        &app_id,
                        instance,
                        crate::shell::app_tabs::AppTabRequest::PlaceNextWindowBeside { background },
                    );
                }
                ui::floating_surface::FloatingSurfaceClientMessage::Dismiss { id } => {
                    let internal = external_floating_id(connection, &id);
                    capture.dismiss_external_floating(internal.clone());
                    if let Some(ids) = connection_ids.get_mut(&connection) {
                        ids.retain(|candidate| candidate != &internal);
                    }
                    changed |= state.dismiss_floating_toplevel(connection, &id);
                    service.send(
                        connection,
                        &ui::floating_surface::FloatingSurfaceHostMessage::Closed { id },
                    );
                }
                // Read by the service before the connection is admitted; a
                // second one says nothing new.
                ui::floating_surface::FloatingSurfaceClientMessage::Hello { .. } => {}
                ui::floating_surface::FloatingSurfaceClientMessage::SelectionCapable => {
                    frame_builder
                        .host_mut()
                        .note_selection_capable(connection, &app_id);
                }
                ui::floating_surface::FloatingSurfaceClientMessage::WindowResource { resource } => {
                    frame_builder
                        .host_mut()
                        .note_window_resource(connection, &app_id, resource);
                }
                ui::floating_surface::FloatingSurfaceClientMessage::AskAgent {
                    selection,
                    bounds,
                } => {
                    // The app id is the launcher's: a selection is only ever
                    // credited to the app that holds it, and only while it
                    // is focused — so the focused window is the one its
                    // window-local bounds are in.
                    let area = crate::shell::selection_agent::desktop_area(
                        bounds,
                        state.keyboard_focus_desktop_rect(),
                    );
                    changed |= frame_builder
                        .host_mut()
                        .receive_ask_agent(&app_id, selection, area);
                }
                ui::floating_surface::FloatingSurfaceClientMessage::Pointed {
                    request,
                    control,
                } => {
                    // Credited to the launcher's app id, and taken only as
                    // the answer to the point the shell asked that app about.
                    changed |= frame_builder
                        .host_mut()
                        .receive_pointed(&app_id, request, control);
                }
                ui::floating_surface::FloatingSurfaceClientMessage::PickAnswered {
                    request,
                    answer,
                } => {
                    // Credited to the launcher's app id, and taken only as
                    // the answer to what the picker asked that app.
                    changed |= frame_builder
                        .host_mut()
                        .receive_pick_answer(&app_id, request, answer);
                }
                message @ (ui::floating_surface::FloatingSurfaceClientMessage::WindowRecord {
                    ..
                }
                | ui::floating_surface::FloatingSurfaceClientMessage::Embeds {
                    ..
                }
                | ui::floating_surface::FloatingSurfaceClientMessage::SharedWindows {
                    ..
                }) => {
                    // The app id is the launcher's: an app only names its
                    // own window's record, and only the sharing app embeds
                    // views or says what it shares.
                    changed |= frame_builder
                        .host_mut()
                        .receive_window_share_message(connection, &app_id, &message)
                        .unwrap_or(false);
                }
                message @ (ui::floating_surface::FloatingSurfaceClientMessage::Toolbar {
                    ..
                }
                | ui::floating_surface::FloatingSurfaceClientMessage::EditLocation {
                    ..
                }
                | ui::floating_surface::FloatingSurfaceClientMessage::WindowOffer {
                    ..
                }
                | ui::floating_surface::FloatingSurfaceClientMessage::OpenSearch {
                    ..
                }) => {
                    // The app id is the launcher's: an app only ever reaches
                    // the bar of a tab that shows it.
                    changed |= frame_builder
                        .host_mut()
                        .receive_window_bar_message(connection, &app_id, &message)
                        .unwrap_or(false);
                }
                ui::floating_surface::FloatingSurfaceClientMessage::DescribeHistory { changes } => {
                    frame_builder
                        .host_mut()
                        .request_history_descriptions(connection, changes);
                }
                ui::floating_surface::FloatingSurfaceClientMessage::PaintCapable => {
                    // The app id is the launcher's, so an app can only ever
                    // offer its own windows' pages.
                    state.note_paint_app(&app_id, connection);
                    changed = true;
                }
                ui::floating_surface::FloatingSurfaceClientMessage::PaintStream {
                    payload,
                    page,
                    scale,
                } => {
                    use base64::Engine as _;
                    match base64::engine::general_purpose::STANDARD.decode(payload.as_bytes()) {
                        Ok(payload) => state.receive_paint_frame(
                            &app_id,
                            crate::compositor::state::PaintFrame {
                                payload,
                                page,
                                scale,
                            },
                        ),
                        Err(error) => {
                            tracing::debug!(connection, %app_id, %error, "malformed paint stream frame");
                        }
                    }
                }
            },
            FloatingServiceEvent::VersionMismatch {
                app_id,
                app_version,
            } => {
                crate::shell::app_versions::spoke_another_shell_version(&app_id, app_version);
            }
            FloatingServiceEvent::Disconnected { connection, app_id } => {
                frame_builder
                    .host_mut()
                    .retire_accessible_agent_markers(connection);
                frame_builder
                    .host_mut()
                    .disconnect_native_terminal(connection);
                frame_builder
                    .host_mut()
                    .forget_selection_capable(connection);
                frame_builder.host_mut().forget_app_toolbars(connection);
                frame_builder.host_mut().forget_window_share(connection);
                frame_builder
                    .host_mut()
                    .forget_history_descriptions(connection);
                appearance.disconnected(connection);
                // Closing the control socket does not destroy the last Wayland
                // buffer. Preserve its protection until the toplevel dies.
                let tabs = foreign::foreign_tabs_anywhere(&state.workspace_tree.read());
                for tab in tabs {
                    if tab.app_id == app_id {
                        foreign::store().retain_surface_capture_protection(tab.surface_id, &app_id);
                    }
                }
                for surface in &state.floating_toplevels {
                    if surface.connection == connection {
                        foreign::store()
                            .retain_surface_capture_protection(surface.surface_id, &app_id);
                    }
                }
                for id in connection_ids.remove(&connection).unwrap_or_default() {
                    capture.dismiss_external_floating(id);
                }
                state.dismiss_floating_connection(connection);
                state.drop_paint_connection(connection);
                state.native_agent_markers.remove(&connection);
                foreign::store().release_capture_protection(connection);
                tracing::debug!(connection, %app_id, "shell-ui client disconnected");
                changed = true;
            }
        }
    }
    // A click that picked what a feedback report is about asks the app
    // under it what control is there.
    for (connection, message) in frame_builder
        .host_mut()
        .route_feedback_point(Instant::now())
    {
        service.send(connection, &message);
    }
    // Picking what to report asks the windows under the pointer what is
    // there, and reads the picks again once the person finished.
    for (connection, message) in frame_builder.host_mut().route_feedback_pick(Instant::now()) {
        service.send(connection, &message);
    }
    // The ask-agent chord asks the focused app for its selection, or opens
    // the composer itself once that app had its moment to answer.
    for connection in frame_builder
        .host_mut()
        .route_ask_agent_chord(Instant::now())
    {
        service.send(
            connection,
            &ui::floating_surface::FloatingSurfaceHostMessage::ShareSelection,
        );
    }
    // What the person did with apps' slots in their windows' bars.
    for (connection, event) in frame_builder.host_mut().take_toolbar_outbox() {
        service.send(
            connection,
            &ui::floating_surface::FloatingSurfaceHostMessage::ToolbarEvent { event },
        );
    }
    // What the desktop says of windows shared live, to the apps it concerns.
    for (connection, message) in frame_builder.host_mut().take_window_share_outbox() {
        service.send(connection, &message);
    }
    // The windows of live views nothing embeds any more.
    for surface in crate::shell::live_embeds::take_closing() {
        state.close_surface_on_request(surface);
    }
    // What apps' changes did, for their History.
    for (connection, described) in frame_builder.host_mut().take_history_descriptions() {
        service.send(
            connection,
            &ui::floating_surface::FloatingSurfaceHostMessage::HistoryDescriptions { described },
        );
    }
    // Apps whose pages someone started or stopped watching.
    for (connection, watched) in state.paint_watch_changes() {
        let message = if watched {
            ui::floating_surface::FloatingSurfaceHostMessage::PaintKeyframe
        } else {
            ui::floating_surface::FloatingSurfaceHostMessage::PaintStop
        };
        service.send(connection, &message);
    }
    changed
}

#[allow(
    clippy::arithmetic_side_effects,
    reason = "Kurbo rect/vector arithmetic applies an f64 translation to keep a reachable surface patch within desktop bounds."
)]
fn clamp_external_floating_rect(rect: Rect, bounds: Rect) -> Rect {
    let mut offset = vello::kurbo::Vec2::ZERO;
    let reachable = 64.0;
    if rect.x1 < bounds.x0 + reachable {
        offset.x = bounds.x0 + reachable - rect.x1;
    } else if rect.x0 > bounds.x1 - reachable {
        offset.x = bounds.x1 - reachable - rect.x0;
    }
    if rect.y1 < bounds.y0 + reachable {
        offset.y = bounds.y0 + reachable - rect.y1;
    } else if rect.y0 > bounds.y1 - reachable {
        offset.y = bounds.y1 - reachable - rect.y0;
    }
    rect + offset
}

#[allow(
    clippy::arithmetic_side_effects,
    reason = "Kurbo point/rect arithmetic applies the same f64 clamp offset to window and content geometry."
)]
fn clamp_external_floating_geometry(
    window_rect: Rect,
    content_rect: Rect,
    bounds: Rect,
) -> (Rect, Rect) {
    let union = window_rect.union(content_rect);
    let rect = clamp_external_floating_rect(union, bounds);
    let offset = rect.origin() - union.origin();
    (rect, content_rect + offset)
}

fn floating_output_rects(outputs: &[Output], fallback_size: (u32, u32)) -> Vec<Rect> {
    let mut rects = outputs
        .iter()
        .filter(|output| output.size.0 > 0 && output.size.1 > 0)
        .map(Output::logical_rect)
        .collect::<Vec<_>>();
    if rects.is_empty() {
        rects.push(Rect::new(
            0.0,
            0.0,
            f64::from(fallback_size.0.max(1)),
            f64::from(fallback_size.1.max(1)),
        ));
    }
    rects
}

fn floating_desktop_bounds(outputs: &[Output], fallback_size: (u32, u32)) -> Rect {
    let rects = floating_output_rects(outputs, fallback_size);
    let Some((first, rest)) = rects.split_first() else {
        return Rect::new(0.0, 0.0, 1.0, 1.0);
    };
    rest.iter().fold(*first, |bounds, rect| bounds.union(*rect))
}

/// Preserve free movement across touching displays while preventing a float
/// from becoming stranded in a hole inside the desktop's bounding box. A
/// reachable 64×64 patch on any real output is enough; otherwise the nearest
/// output receives the same clamp used at the outside desktop edge.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Kurbo point subtraction computes f64 distances to choose the nearest reachable output before clamping a surface."
)]
fn clamp_external_floating_geometry_to_outputs(
    window_rect: Rect,
    content_rect: Rect,
    outputs: &[Output],
    fallback_size: (u32, u32),
) -> (Rect, Rect) {
    let union = window_rect.union(content_rect);
    let output_rects = floating_output_rects(outputs, fallback_size);
    let reachable = output_rects.iter().any(|output| {
        let visible = union.intersect(*output);
        visible.width() >= 64.0 && visible.height() >= 64.0
    });
    if reachable {
        return (union, content_rect);
    }

    let center = union.center();
    let target = output_rects
        .iter()
        .min_by(|a, b| {
            let distance = |output: &&Rect| {
                let nearest = vello::kurbo::Point::new(
                    center.x.clamp(output.x0, output.x1),
                    center.y.clamp(output.y0, output.y1),
                );
                (nearest - center).hypot2()
            };
            distance(a).total_cmp(&distance(b))
        })
        .copied()
        .unwrap_or_else(|| floating_desktop_bounds(outputs, fallback_size));
    clamp_external_floating_geometry(window_rect, content_rect, target)
}

#[allow(
    clippy::arithmetic_side_effects,
    reason = "Kurbo point/rect arithmetic moves each transfer source by the same f64 offset as its rehomed surface."
)]
fn rehome_native_floating_surfaces(
    state: &mut State,
    outputs: &[Output],
    fallback_size: (u32, u32),
) -> bool {
    let mut changed = false;
    for surface in &mut state.pending_floating_toplevels {
        let previous = surface.rect;
        let (rect, content_rect) = clamp_external_floating_geometry_to_outputs(
            surface.rect,
            surface.content_rect,
            outputs,
            fallback_size,
        );
        let offset = rect.origin() - previous.origin();
        changed |= rect != surface.rect || content_rect != surface.content_rect;
        surface.rect = rect;
        surface.content_rect = content_rect;
        surface.input_rect = ui::floating_surface::translate_input_rect(
            surface.input_rect,
            previous.origin(),
            rect.origin(),
        );
        if let Some(source) = surface.transfer_source.as_mut() {
            source.rect = source.rect + offset;
        }
    }
    for surface in &mut state.floating_toplevels {
        let previous = surface.rect;
        let (rect, content_rect) = clamp_external_floating_geometry_to_outputs(
            surface.rect,
            surface.content_rect,
            outputs,
            fallback_size,
        );
        let offset = rect.origin() - previous.origin();
        changed |= rect != surface.rect || content_rect != surface.content_rect;
        surface.rect = rect;
        surface.content_rect = content_rect;
        surface.input_rect = ui::floating_surface::translate_input_rect(
            surface.input_rect,
            previous.origin(),
            rect.origin(),
        );
        if let Some(source) = surface.transfer_source.as_mut() {
            source.rect = source.rect + offset;
        }
    }
    changed
}

fn pump_floating_toplevel_events(service: &FloatingSurfaceService, state: &mut State) -> bool {
    let events = state.take_floating_toplevel_events();
    let changed = !events.is_empty();
    for event in events {
        match event {
            crate::compositor::state::FloatingToplevelEvent::Mapped(surface) => {
                service.send(
                    surface.connection,
                    &ui::floating_surface::FloatingSurfaceHostMessage::Adopted {
                        id: surface.public_id,
                        rect: ui::floating_surface::protocol_rect_array(surface.content_rect),
                    },
                );
            }
            crate::compositor::state::FloatingToplevelEvent::Destroyed {
                connection,
                public_id,
            } => {
                service.send(
                    connection,
                    &ui::floating_surface::FloatingSurfaceHostMessage::Reattach { id: public_id },
                );
            }
        }
    }
    changed
}

/// Env var for the CI test-introspection hook: when set, [`run`] serializes
/// the shell `WorkspaceTree` to that path each frame tick as compact JSON;
/// `tests/compositor_test.rs` polls it to assert foreign-tab adoption +
/// teardown without a public shell-state API. Unset means the compositor
/// never touches the filesystem here.
const TEST_STATE_DUMP_ENV: &str = "TILO_TEST_STATE_DUMP";

/// Env var for the real-input e2e control protocol: when set to a writable
/// path, [`run`] binds a unix socket there accepting newline-delimited JSON
/// commands (`compositor::test_control::Command`). It types, clicks and
/// reads back state as if it were the person at the keyboard — even while
/// locked — so it is a **developer-build** surface, gated on
/// [`crate::compositor::dev_env::DEV_ENV_OVERRIDES`].
const TEST_CONTROL_ENV: &str = crate::shell::test_control_socket::TEST_CONTROL_ENV;
/// Set to `keep` by a dev image's login wrapper, which gives a person's own
/// login the control socket: that session is not a harness's and keeps first
/// use.
const TEST_CONTROL_FIRST_USE_ENV: &str = "TILO_TEST_CONTROL_FIRST_USE";

/// RAII guard that unlinks a unix-socket file when dropped. [`run`] also
/// drops these explicitly in shutdown order so endpoints disappear before
/// control returns to the display-manager wrapper.
pub(crate) struct UnlinkOnDrop(pub(crate) PathBuf);

impl Drop for UnlinkOnDrop {
    fn drop(&mut self) {
        // Errors are not actionable here — already gone, or no permission — so ignore.
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Remove a Wayland endpoint and Smithay's companion lock file before the
/// display manager starts the next graphical session.
fn unlink_wayland_endpoint(path: &Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(path.with_extension("lock"));
}

/// Remove private Wayland endpoints left by a compositor that died without
/// unwinding. These names are compositor-owned and include their creator PID;
/// a live PID is never touched, so a second same-user session cannot steal a
/// running compositor's privileged or OSK socket.
fn remove_stale_private_wayland_endpoints(runtime_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(runtime_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let endpoint_name = name.strip_suffix(".lock").unwrap_or(name);
        let pid = endpoint_name
            .strip_prefix("tilo-privileged-")
            .or_else(|| endpoint_name.strip_prefix("tilo-osk-"))
            .and_then(|suffix| suffix.split('-').next())
            .and_then(|pid| pid.parse::<u32>().ok());
        let Some(pid) = pid else {
            continue;
        };
        let pid_is_live = PathBuf::from(format!("/proc/{pid}")).exists();
        if !pid_is_live {
            unlink_wayland_endpoint(&runtime_dir.join(endpoint_name));
        }
    }
}

/// Bind the test-control unix socket, returning the listener plus an
/// `UnlinkOnDrop` guard on success, or `None` if unset/the bind fails
/// (logged via `eprintln!`); the production path never depends on this.
/// Resolve the path the test-control socket should be bound at, as a pure
/// function of the gate and raw variable — taking `dev_overrides`
/// ([`crate::compositor::dev_env::DEV_ENV_OVERRIDES`]) as an argument lets the shipped answer (always `None`) be asserted from a debug test run.
pub(crate) fn test_control_path(
    dev_overrides: bool,
    raw: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    if !dev_overrides {
        return None;
    }
    raw.map(PathBuf::from)
}

/// Same, for the per-frame `WorkspaceTree` dump. A shipped session has no
/// business serialising its window tree to a path the environment names.
pub(crate) fn test_state_dump_path(
    dev_overrides: bool,
    raw: Option<std::ffi::OsString>,
) -> Option<std::ffi::OsString> {
    if !dev_overrides {
        return None;
    }
    raw
}

fn bind_test_control_socket() -> Option<(UnixListener, UnlinkOnDrop)> {
    let path = test_control_path(
        crate::compositor::dev_env::DEV_ENV_OVERRIDES,
        std::env::var_os(TEST_CONTROL_ENV),
    )?;
    bind_control_listener(path, "test_control")
}

/// Environment variable naming the agent-control socket.
pub const AGENT_CONTROL_ENV: &str = "TILO_AGENT_CONTROL";

/// Resolve the agent-control socket path. Only a build with the
/// non-default `agent-control` feature, running the headless backend (a
/// hosted machine an agent drives, with no local person or lock screen),
/// binds one; `enabled` and `headless` are parameters so every branch is
/// testable from any build.
pub(crate) fn agent_control_path(
    enabled: bool,
    headless: bool,
    raw: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    if !enabled || !headless {
        return None;
    }
    raw.map(PathBuf::from).filter(|path| path.is_absolute())
}

fn bind_agent_control_socket(headless: bool) -> Option<(UnixListener, UnlinkOnDrop)> {
    let path = agent_control_path(
        cfg!(feature = "agent-control"),
        headless,
        std::env::var_os(AGENT_CONTROL_ENV),
    )?;
    bind_control_listener(path, "agent_control")
}

/// Serve the runtime's MCP projection of this session's broker over HTTP
/// (`TILO_RUNTIME_MCP_HTTP`), with the canonical pixel tools driving this
/// compositor through its agent-control socket. Apps an agent opens are
/// launched by this session, so they are its own attested clients.
#[cfg(feature = "agent-control")]
fn spawn_agent_mcp(
    broker: &crate::shell::collab_broker::BrokerHandle,
    tree_handle: &crate::shell::state::WorkspaceTreeHandle,
    headless: bool,
) {
    let Some(agent_socket) =
        agent_control_path(true, headless, std::env::var_os(AGENT_CONTROL_ENV))
    else {
        return;
    };
    let config = match tilo_runtime::mcp_http::Config::from_env() {
        Ok(Some(config)) => config,
        Ok(None) => return,
        Err(error) => {
            eprintln!("tilo compositor: agent MCP endpoint: {error}");
            return;
        }
    };
    let Some(action_broker) = broker.app_action_broker() else {
        eprintln!("tilo compositor: agent MCP endpoint: the action broker is unavailable");
        return;
    };
    let principal = {
        let tree = tree_handle.read();
        tree.desktop.active_workspace().map(|workspace| {
            let room = workspace.room();
            (
                room.to_string(),
                crate::shell::state::local_user_id_for_room(&room),
            )
        })
    };
    let Some((room, actor)) = principal else {
        eprintln!("tilo compositor: agent MCP endpoint: no active workspace");
        return;
    };
    let scratch = tilo_paths::xdg::cache_home().map(|cache| cache.join("tilo").join("computer"));
    let Some(scratch) = scratch.filter(|path| {
        std::fs::create_dir_all(path).is_ok()
            && std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o700))
                .is_ok()
    }) else {
        eprintln!("tilo compositor: agent MCP endpoint: no private screenshot directory");
        return;
    };
    let computer =
        std::sync::Arc::new(tilo_runtime::computer::Computer::new(agent_socket, scratch));
    // Viewer invitations keep their ledger with the session's durable state.
    let viewer_state = tilo_paths::xdg::state_home()
        .map(|state| state.join("tilo"))
        .filter(|path| std::fs::create_dir_all(path).is_ok());
    let viewer = match viewer_state.map(|state| tilo_runtime::viewer::start_from_env(&state)) {
        Some(Ok(viewer)) => viewer,
        Some(Err(error)) => {
            eprintln!("tilo compositor: viewer invitations unavailable: {error}");
            None
        }
        None => None,
    };
    let endpoint = tilo_runtime::mcp_http::Endpoint::with_viewer(
        crate::shell::remote_actions::BrokerActionHost::new(action_broker),
        room,
        actor,
        config.token.clone(),
        Some(computer),
        viewer,
    );
    match tilo_runtime::mcp_http::spawn(&config, endpoint) {
        Ok(bound) => eprintln!("tilo compositor agent MCP endpoint listening on {bound}"),
        Err(error) => eprintln!("tilo compositor: agent MCP endpoint: {error}"),
    }
}

fn bind_control_listener(path: PathBuf, label: &str) -> Option<(UnixListener, UnlinkOnDrop)> {
    // Replaces a stale socket; only this user can ever connect.
    match crate::shell::test_control_socket::bind_private(&path) {
        Ok(listener) => {
            if let Err(e) = listener.set_nonblocking(true) {
                eprintln!("tilo compositor: {label} set_nonblocking: {e}");
                return None;
            }
            eprintln!("tilo compositor {label} listening on {}", path.display());
            let guard = UnlinkOnDrop(path);
            Some((listener, guard))
        }
        Err(e) => {
            eprintln!("tilo compositor: {label} bind {}: {e}", path.display());
            None
        }
    }
}

/// Register a duplicate of a control listener as a wake token so a driver
/// which connects to an idle compositor is serviced immediately. Accepting
/// and executing requests stays on the compositor thread; this source only
/// prevents the outer calloop from sleeping through the listener's
/// readability.
fn control_listener_wake(
    listener: &UnixListener,
    handle: &calloop::LoopHandle<'static, State>,
) -> Option<calloop::RegistrationToken> {
    let duplicate = unsafe { libc::fcntl(listener.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
    if duplicate < 0 {
        eprintln!(
            "tilo compositor: control listener duplicate: {}",
            std::io::Error::last_os_error()
        );
        return None;
    }
    // SAFETY: F_DUPFD_CLOEXEC returned a new owned descriptor.
    let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(duplicate) };
    match handle.insert_source(Generic::new(fd, Interest::READ, Mode::Level), |_, _, _| {
        Ok(PostAction::Continue)
    }) {
        Ok(token) => Some(token),
        Err(error) => {
            eprintln!("tilo compositor: control listener wake source: {error}");
            None
        }
    }
}

/// File inside the session directory ([`tilo_paths::login::session_dir`])
/// holding the bare socket name (no trailing newline), so `WAYLAND_DISPLAY=$(cat …)` is exactly right.
pub const WAYLAND_DISPLAY_FILE: &str = "wayland-display";

/// Where [`publish_wayland_display`] writes, given `$XDG_RUNTIME_DIR`. Split
/// out so the path policy is testable without a live session.
///
/// `None` when the variable is unset or not absolute — guessing `/tmp`
/// would put a session-scoped file somewhere world-writable.
pub(crate) fn session_display_path(runtime_dir: Option<std::ffi::OsString>) -> Option<PathBuf> {
    Some(tilo_paths::login::session_dir(runtime_dir)?.join(WAYLAND_DISPLAY_FILE))
}

/// Publish the compositor's Wayland socket name to
/// `$XDG_RUNTIME_DIR/tilo-session/wayland-display`. The file belongs to the
/// session that published it while that session runs: a second compositor
/// in the same runtime directory (a headless one beside the desktop) leaves
/// it alone, and its printed stderr line is how it is reached. Never fatal
/// on failure. Returns the unlink guard so the file disappears with the
/// process.
fn publish_wayland_display(socket_name: &std::ffi::OsStr) -> Option<UnlinkOnDrop> {
    let path = session_display_path(std::env::var_os("XDG_RUNTIME_DIR"))?;
    if another_live_session_published(&path, socket_name) {
        eprintln!(
            "tilo compositor: {} names another running session; leaving it",
            path.display()
        );
        return None;
    }
    // 0700 when created: the test-control socket may share this directory.
    if let Some(dir) = path.parent()
        && let Err(e) = std::os::unix::fs::DirBuilderExt::mode(
            std::fs::DirBuilder::new().recursive(true),
            0o700,
        )
        .create(dir)
    {
        eprintln!("tilo compositor: publishing {}: {e}", path.display());
        return None;
    }
    // 0600: not a secret, but nothing else should write it; applied before
    // the rename so it's never briefly visible wider than intended.
    match tilo_files::native::atomic::write_with_mode(&path, socket_name.as_encoded_bytes(), 0o600)
    {
        Ok(()) => Some(UnlinkOnDrop(path)),
        Err(e) => {
            eprintln!("tilo compositor: publishing {}: {e}", path.display());
            None
        }
    }
}

/// Whether the display file at `path` names a Wayland socket other than
/// `ours` that a running compositor still answers on, beside it in the
/// runtime directory.
pub(crate) fn another_live_session_published(path: &Path, ours: &std::ffi::OsStr) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    let Ok(named) = std::fs::read(path) else {
        return false;
    };
    let named = std::ffi::OsStr::from_bytes(&named);
    let bare = Path::new(named).file_name() == Some(named);
    if named == ours || !bare {
        return false;
    }
    path.parent()
        .and_then(Path::parent)
        .is_some_and(|runtime| UnixStream::connect(runtime.join(named)).is_ok())
}

/// [`tilo_paths::login::WORKSPACE_ID_FILE`] beside the Wayland display this
/// compositor published, written off this thread; `None` where it published
/// none, so the session that did keeps its own.
fn published_room(
    display: Option<&UnlinkOnDrop>,
) -> Option<crate::compositor::published_room::PublishedRoom> {
    crate::compositor::published_room::PublishedRoom::at(
        display?
            .0
            .with_file_name(tilo_paths::login::WORKSPACE_ID_FILE),
    )
}

/// Env var naming the compositor's privileged Wayland socket, exported to
/// trusted session services outside app sandboxes. Clipboard managers and
/// the session-lock integration connect here instead of `WAYLAND_DISPLAY`.
pub const PRIVILEGED_SOCKET_ENV: &str = "TILO_WAYLAND_PRIVILEGED";

/// Bind the privileged Wayland socket and publish its name, carrying the
/// compositor's pid so concurrent sessions can't collide. The pid is only a
/// collision-free socket name; authorization comes from which acceptor made
/// the immutable per-connection `ClientState`.
#[allow(
    clippy::disallowed_methods,
    reason = "publishes the socket to privileged children, which inherit the compositor's \
              environment; no thread started earlier reads this variable"
)]
fn bind_privileged_socket() -> Result<(ListeningSocketSource, std::ffi::OsString), String> {
    let name = format!("tilo-privileged-{}", std::process::id());
    let socket = ListeningSocketSource::with_name(&name).map_err(|e| e.to_string())?;
    let socket_name = socket.socket_name().to_os_string();
    // Published for privileged children only; sandboxed apps get an explicit
    // environment from `tilo-launcher` and never inherit this.
    // SAFETY: helper threads already run at this point (settings, portal,
    // sync), but none of them reads this variable, and the value is written
    // once before any privileged child is spawned.
    unsafe { std::env::set_var(PRIVILEGED_SOCKET_ENV, &socket_name) };
    Ok((socket, socket_name))
}

/// Per-connection state for the test-control protocol: a buffered reader
/// over a non-blocking `UnixStream`, so partial lines accumulate across ticks.
struct TestControlConn {
    reader: BufReader<UnixStream>,
    input: test_control::BoundedLineBuffer,
    /// Accepted on the agent-control socket: only
    /// [`test_control::Command::agent_permitted`] commands are served.
    agent: bool,
    /// Accepted on the device admin's control socket: only
    /// [`test_control::Command::admin_permitted`] commands are served, for
    /// the session's own person and never on behalf of a seated agent.
    admin: bool,
    admin_identity_dir: Option<PathBuf>,
    /// Handed over by the agent door for a program running in an app: only
    /// [`super::workspace_door::permitted`] requests are served, with that
    /// app's workspace scope.
    scope: Option<super::workspace_door::AppScope>,
    /// A request already read off the stream before this connection was
    /// made (the agent door reads the first line to tell how it speaks),
    /// served before anything else.
    replay: Option<zeroize::Zeroizing<String>>,
    /// A scoped request waiting on the person's answer; nothing after it
    /// on this connection is read until then.
    held_for_consent: Option<HeldForConsent>,
    /// The sensitive request the person just allowed, run once.
    confirmed: Option<zeroize::Zeroizing<String>>,
    /// Wakes the compositor loop when request bytes arrive on this stream;
    /// removed from the loop together with the connection.
    wake: Option<calloop::RegistrationToken>,
    /// Wakes the loop when the stream can take more of a response the
    /// socket did not take whole; registered only while one is unfinished.
    output_wake: Option<calloop::RegistrationToken>,
    closed: bool,
    /// An `actions_invoke` answered once it is admitted, or once it settles.
    pending_invocation: Option<ControlInvocation>,
    /// An `actions_enumerate` with `wait_ms`, answered once its listing
    /// settles; nothing after it on this connection is read until then.
    pending_listing: Option<HeldListing>,
    /// A `snapshot` answered once its file is complete.
    pending_snapshot: Option<(SnapshotOutcome, test_control::Response)>,
    output: Vec<u8>,
    output_offset: usize,
}

/// [`InvocationProgress`] of the live frame: a job's status is read
/// through the action catalogue and its capability gate, exactly as
/// `actions_status` reads it.
struct FrameInvocationProgress<'a> {
    fb: &'a FrameBuilder,
    ctx: &'a mut TestControlCtx,
}

impl InvocationProgress for FrameInvocationProgress<'_> {
    fn status(&mut self, job: u64) -> Option<crate::shell::search::ActionReceipt> {
        use crate::shell::search::providers::live_app_actions;
        match execute_invoke_action(
            live_app_actions::PROVIDER_ID,
            &live_app_actions::job_status_id(job),
            &serde_json::Map::new(),
            self.fb,
            self.ctx,
        ) {
            Ok(crate::shell::search::InvocationDisposition::Accepted(receipt)) => Some(receipt),
            Ok(crate::shell::search::InvocationDisposition::Pending(_)) | Err(_) => None,
        }
    }

    fn next_change(&self) -> Option<tilo_actions::remote::Pending<()>> {
        self.fb.host().next_invocation_change()
    }
}

/// A control request that held the loop this long is named in the session
/// log, so a stall an agent or a test caused says which request it was.
const SLOW_CONTROL_REQUEST: Duration = Duration::from_millis(20);

/// Log `line`'s operation when handling it on the loop took `elapsed`. Only
/// the `op` field is read: a request can carry a secret (a lock passcode).
fn note_slow_control_request(line: &str, elapsed: Duration) {
    // Every other field is skipped, never copied out of the zeroized line.
    #[derive(serde::Deserialize)]
    struct Op<'a> {
        #[serde(borrow)]
        op: Option<&'a str>,
    }
    if elapsed < SLOW_CONTROL_REQUEST {
        return;
    }
    let op = serde_json::from_str::<Op<'_>>(line)
        .ok()
        .and_then(|request| request.op)
        .unwrap_or_default();
    // A measurement line for the session log, like SLOW-ITER.
    eprintln!(
        "SLOW-CONTROL op={op} loop_ms={:.1}",
        elapsed.as_secs_f64() * 1000.0
    );
}

/// The earliest a held-back invocation needs the loop again although
/// nothing woke it (see [`ControlInvocation::recheck_at`]).
fn control_invocation_deadline(conns: &[TestControlConn], now: Instant) -> Option<Instant> {
    conns
        .iter()
        .filter_map(|conn| {
            let invocation = conn
                .pending_invocation
                .as_ref()
                .and_then(|pending| pending.recheck_at(now));
            let listing = conn
                .pending_listing
                .as_ref()
                .and_then(|held| held.recheck_at(now));
            invocation.into_iter().chain(listing).min()
        })
        .min()
}

/// The action surface as a held listing compares it: what woke the loop,
/// and the broker's invocation and publication generations.
fn listing_stamp(frame_builder: &FrameBuilder) -> ListingStamp {
    ListingStamp {
        progress: crate::shell::search::invocation_wake::progress(),
        broker: frame_builder.host().app_action_generation(),
    }
}

/// Connections from programs running in apps served at once; more are
/// dropped as they arrive.
const MAX_SCOPED_CONTROL_CONNS: usize = 16;

#[path = "native_scoped_control.rs"]
mod scoped_control;
use scoped_control::HeldForConsent;

#[cfg(test)]
thread_local! {
    /// How many times a test-control connection's wake fired on this thread.
    static TEST_CONTROL_WAKES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Count one firing of a test-control connection's read or output wake, so a
/// test can tell a loop the connection woke from one that ran out its timeout.
fn note_test_control_wake() {
    #[cfg(test)]
    TEST_CONTROL_WAKES.with(|wakes| wakes.set(wakes.get().saturating_add(1)));
}

impl TestControlConn {
    /// A connection on the device admin's control socket, already authorised
    /// by [`crate::compositor::admin_control::authorize`].
    fn admin(stream: UnixStream, identity_dir: &Path) -> Option<Self> {
        let mut conn = Self::new(stream, false)?;
        conn.admin = true;
        conn.admin_identity_dir = Some(identity_dir.to_owned());
        Some(conn)
    }

    fn new(stream: UnixStream, agent: bool) -> Option<Self> {
        if let Err(e) = stream.set_nonblocking(true) {
            eprintln!("tilo compositor: test_control conn set_nonblocking: {e}");
            return None;
        }
        Some(Self {
            reader: BufReader::new(stream),
            input: test_control::BoundedLineBuffer::default(),
            agent,
            admin: false,
            admin_identity_dir: None,
            scope: None,
            replay: None,
            held_for_consent: None,
            confirmed: None,
            wake: None,
            output_wake: None,
            closed: false,
            pending_invocation: None,
            pending_listing: None,
            pending_snapshot: None,
            output: Vec::new(),
            output_offset: 0,
        })
    }

    /// Register this stream with `handle` as an edge-triggered wake. A
    /// driver's `connect` makes the listener readable and is accepted at
    /// once; its request line arrives separately, possibly only after this
    /// iteration has already found the stream empty. Without its own wake
    /// an idle compositor would sleep on the unread request until some
    /// unrelated event. Edge-triggered, because every pump drains the stream
    /// until it would block, and a level registration would spin on a peer
    /// that half-closed while it waits for its response.
    fn watch(&mut self, handle: &calloop::LoopHandle<'static, State>) {
        let clone = match self.reader.get_ref().try_clone() {
            Ok(clone) => clone,
            Err(error) => {
                eprintln!("tilo compositor: test_control conn wake clone: {error}");
                return;
            }
        };
        match handle.insert_source(
            Generic::new(clone, Interest::READ, Mode::Edge),
            |_, _, _| {
                note_test_control_wake();
                Ok(PostAction::Continue)
            },
        ) {
            Ok(token) => self.wake = Some(token),
            Err(error) => eprintln!("tilo compositor: test_control conn wake source: {error}"),
        }
    }

    /// Keep a wake on the stream's room for output exactly while a response
    /// is partly written. A response larger than the socket's send buffer
    /// (a full action listing is megabytes) is written as far as the socket
    /// takes it and the rest retained; the read wake never fires for the
    /// driver draining it, so without this an idle compositor sent the next
    /// piece only when something unrelated woke it, seconds apart, and a
    /// listing took a minute to arrive. Level-triggered: every pump writes
    /// until the socket would block, so it fires again only once the driver
    /// has read, and a driver that stops reading leaves it quiet.
    fn watch_output(&mut self, handle: &calloop::LoopHandle<'static, State>) {
        let unfinished = !self.closed && self.output_offset < self.output.len();
        match (unfinished, self.output_wake.is_some()) {
            (true, false) => {
                let clone = match self.reader.get_ref().try_clone() {
                    Ok(clone) => clone,
                    Err(error) => {
                        eprintln!("tilo compositor: test_control output wake clone: {error}");
                        return;
                    }
                };
                match handle.insert_source(
                    Generic::new(clone, Interest::WRITE, Mode::Level),
                    |_, _, _| {
                        note_test_control_wake();
                        Ok(PostAction::Continue)
                    },
                ) {
                    Ok(token) => self.output_wake = Some(token),
                    Err(error) => {
                        eprintln!("tilo compositor: test_control output wake source: {error}");
                    }
                }
            }
            (false, true) => {
                if let Some(token) = self.output_wake.take() {
                    handle.remove(token);
                }
            }
            _ => {}
        }
    }

    /// Remove every wake this connection registered with the loop.
    fn unwatch(&mut self, handle: &calloop::LoopHandle<'static, State>) {
        for token in [self.wake.take(), self.output_wake.take()]
            .into_iter()
            .flatten()
        {
            handle.remove(token);
        }
    }

    fn peer_disconnected(&self) -> bool {
        let mut descriptor = libc::pollfd {
            fd: self.reader.get_ref().as_raw_fd(),
            events: libc::POLLOUT,
            revents: 0,
        };
        // SAFETY: `descriptor` is one initialized `pollfd` and remains live
        // for this zero-timeout observation. `POLLHUP` distinguishes a peer
        // that closed the connection from read-side EOF caused only by its
        // `shutdown(Write)`; the latter must still receive our response.
        let ready = unsafe { libc::poll(&raw mut descriptor, 1, 0) };
        if ready == 0 {
            return false;
        }
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            eprintln!("tilo compositor: test_control connection check: {error}");
            return true;
        }
        descriptor.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
    }
}

/// Every window tab with artwork, across all workspaces, as the pill paints it.
fn tab_icons_json(tree: &crate::shell::state::WorkspaceTree) -> serde_json::Value {
    let rows: Vec<serde_json::Value> = tree
        .desktop
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .filter_map(|tab| {
            let icon = crate::app_icons::for_tile(&tab.tile)?;
            Some(serde_json::json!({ "tab": tab.id, "icon": icon.key() }))
        })
        .collect();
    serde_json::Value::Array(rows)
}

/// `ControlState` impl walking the frame builder. Captures a fresh snapshot
/// per `widget_registry()` call, so dispatch sees the live tree, uncached.
struct FrameBuilderControlState<'a> {
    fb: &'a FrameBuilder,
    /// The live CRDT the search providers read, plus its root object. `None`
    /// for layout/chrome-only test fixtures, so action methods refuse rather than report an empty surface.
    doc: Option<(&'a AutoCommit, &'a ObjId)>,
    /// The compositor's actual stream/input authority. Layout-only tests have
    /// no compositor and therefore leave this absent.
    shares: Option<&'a crate::network::streaming::ShareRegistry>,
    /// Read-only lock authentication status. Layout-only fixtures omit this;
    /// the native transport supplies it so an acceptance client can wait for
    /// an asynchronous verifier before entering a retry.
    lock: Option<&'a LockController>,
    /// The actual Wayland seat; layout-only fixtures cannot observe it.
    seat: Option<&'a State>,
    /// Where an `actions_invoke` leaves the address it resolved, for its
    /// execution in the same request; fixtures that run nothing omit it.
    resolved: Option<&'a std::cell::RefCell<Option<ResolvedInvocation>>>,
}

impl FrameBuilderControlState<'_> {
    fn insert_primary_preedit_route(&self, row: &mut serde_json::Value, surface: u64) {
        if let Some(child) = self
            .seat
            .and_then(|seat| seat.floating_preedit_child_for(surface))
        {
            insert_preedit_route(row, "floating-preedit-child", child);
        }
    }

    /// Which windows each display holds, in the order they are laid out,
    /// and which display has focus: the per-device partition as the person
    /// sees it. Lists every connected display, a single one included.
    fn displays_json(&self) -> Vec<serde_json::Value> {
        let summary = self.fb.host().display_partition_summary();
        let chrome = self.fb.chrome();
        let tree = chrome.tree();
        let partition = &tree.desktop.outputs;
        // An unpartitioned desktop (one display, ever) lists just that one.
        let primary = partition
            .primary()
            .or_else(|| summary.first().map(|(id, _)| *id));
        let focused = partition.focused_display().or(primary);
        let displays = summary
            .into_iter()
            .map(|(id, groups)| {
                serde_json::json!({
                    "id": id,
                    "primary": primary == Some(id),
                    "focused": focused == Some(id),
                    "groups": groups,
                })
            })
            .collect();
        drop(tree);
        drop(chrome);
        displays
    }

    /// The action listing for `query` through the *same* provider registry
    /// that feeds the Ctrl+K palette — but rendering nothing. With `page`,
    /// that page of it ([`test_control::ListingRequest`]); without, all of it.
    /// Only the answered rows become JSON: describing every row of a large
    /// catalogue cost more than building it.
    fn listing(
        &self,
        query: &str,
        include_programmatic: bool,
        page: Option<&test_control::ListingRequest>,
    ) -> Result<serde_json::Value, test_control::ActionRefusal> {
        let (doc, root) = self.doc.ok_or_else(|| {
            test_control::ActionRefusal::listing_failed("control state has no live document")
        })?;
        let chrome_handle = self.fb.chrome_handle();
        // The borrow lives only for the closure; `ActionCatalog` owns its results
        // outright, so the catalogue outlives the guards it was built through.
        let (catalog, launcher, complete) = with_action_gate(&chrome_handle, |chrome, gate| {
            let args = crate::shell::search::QueryArgs {
                needle: query,
                scope: crate::shell::chrome_state::SearchScope::All,
                active_provider_id: None,
                wanted_kinds: &[],
                chrome,
                doc,
                doc_root: root,
            };
            let first_use_gate = super::admin_control::FirstUseVisibility {
                base: gate,
                first_use: self.fb.host().onboarding_active(),
            };
            let catalog =
                self.fb
                    .host()
                    .action_catalog_query(&args, &first_use_gate, include_programmatic);
            let launcher = self.fb.host().launcher_answer(&args, page, &first_use_gate);
            // Read after the query, which may itself have started the work
            // that leaves the listing incomplete.
            let complete = self.fb.host().action_listing_complete(&args);
            (catalog, launcher, complete)
        })
        .map_err(test_control::ActionRefusal::listing_failed)?;
        // A paged listing is an agent's: apps' own resources and actions
        // first, files only when asked.
        let mut catalog = catalog;
        if let Some(page) = page {
            catalog.arrange(&page.kinds, page.files);
        }
        let fingerprint = test_control::listing_fingerprint(catalog.addresses());
        let window = page
            .map(|page| page.window(catalog.len(), fingerprint))
            .transpose()?;
        let compact = page.is_some_and(|page| !page.detail.is_full());
        let rows = window
            .as_ref()
            .map_or(0..catalog.len(), |window| window.rows.clone());
        let actions: Vec<serde_json::Value> = catalog
            .enumerate_rows(rows)
            .into_iter()
            .map(|a| {
                let hierarchy = a.hierarchy.as_ref();
                let mut row = serde_json::json!({
                    "provider": a.resource.provider,
                    "id": a.resource.id,
                    "kind": a.kind.id(),
                    "operation_kind": a.operation_kind,
                    "title": a.title,
                    "subtitle": a.subtitle,
                    "score": a.score,
                    // What to pass; empty for the nullary majority. A parameter list describes
                    // an invocation, it is not one, so enumeration stays side-effect-free.
                    "params": params_json(&a.params),
                    "contract": a.contract,
                    // The provider resolves identity from its authenticated source;
                    // opaque invocation IDs and presentation labels stay independent.
                    "application": hierarchy.map(|value| &value.application),
                    "room": hierarchy.map(|value| &value.room),
                    "record": hierarchy.map(|value| &value.record),
                    "resource": hierarchy.map(|value| &value.resource),
                    "semantic_identity": hierarchy.and_then(|value| value.semantic_identity.as_ref()),
                    "operation": hierarchy.map(|value| &value.operation),
                });
                if compact && let Some(row) = row.as_object_mut() {
                    row.remove("contract");
                }
                row
            })
            .collect();
        let mut answer = serde_json::Map::new();
        answer.insert("query".into(), query.into());
        answer.insert("count".into(), actions.len().into());
        answer.insert("actions".into(), actions.into());
        if let Some(launcher) = launcher {
            answer.insert("launcher".into(), launcher);
        }
        if let Some(window) = window {
            window.describe(&mut answer);
        }
        // Which listing this is — the identity its cursors are cut from — and
        // whether it is final. `truncated` says this page is not all of the
        // listing; `complete: false` says the listing itself is not all there
        // yet: an index build or an app's directory page that would add to it
        // is still on its way. `wait_ms` waits for that.
        answer.insert("revision".into(), format!("{fingerprint:016x}").into());
        answer.insert(
            "complete".into(),
            (complete && catalog.completion().complete).into(),
        );
        answer.insert("completion".into(), serde_json::json!(catalog.completion()));
        Ok(serde_json::Value::Object(answer))
    }
}

impl ControlState for FrameBuilderControlState<'_> {
    fn widget_registry(&self) -> HashMap<String, Rect> {
        self.fb.widget_registry()
    }

    /// What a press is proven against: the overlay host the registry was just
    /// walked from, the identity of the frame on screen, and whether the
    /// desktop is between two arrangements — opening setup, or switching
    /// workspace, strip or window group — where a rectangle read now is not
    /// the one a caller was shown.
    ///
    /// The side is asked of the host rather than of the whole desktop: a
    /// widget asking for frames for its own reasons is not a reason to hold
    /// every unrelated target back.
    fn press_witness(&self) -> Option<(&dyn ui::widget::Widget, PressFacts)> {
        let drawn = self.fb.drawn_revisions().last().copied();
        let host = self.fb.host();
        let in_transition = drawn.is_none() || host.desktop_is_moving();
        Some((host, PressFacts::at(drawn, in_transition)))
    }

    /// The peers the overlay would paint this frame, read from the same
    /// presence context `draw_real_peers` uses, keyed by room.
    fn peers_json(&self) -> serde_json::Value {
        let presence = self.fb.presence();
        let now = std::time::Instant::now();
        let rows: Vec<serde_json::Value> = presence
            .pointers
            .iter()
            .flat_map(|(key, peers)| {
                let scope = match key {
                    ui::presence::PresenceKey::Workspace { id } => id.clone(),
                    other => format!("{other:?}"),
                };
                peers.iter().map(move |(user, (x, y))| {
                    serde_json::json!({
                        "room": scope,
                        "user": user.0,
                        // The id is the identity (never blank); the name is what the room calls
                        // it, so a harness can tell a named peer from an unnamed one.
                        "name": presence.display_name(user),
                        "x": x,
                        "y": y,
                        // What they are saying beside the pointer (cursor
                        // chat), while its bubble shows.
                        "chat": presence
                            .chats
                            .bubble(user, now, ui::tokens::EffectiveMotion::default())
                            .map(|bubble| bubble.text),
                    })
                })
            })
            .collect();
        // A pointer is not a roster — different questions. Derived from
        // `collaborator_roster_rows` rather than re-walked, so reads can't drift from what a person sees.
        let chrome = self.fb.chrome();
        let roster: Vec<serde_json::Value> = tilo_desktop::collaborators::collaborator_roster_rows(
            &chrome.tree().desktop,
            &crate::shell::state::local_user_id(),
        )
        .into_iter()
        .map(|row| {
            serde_json::json!({
                // The participant id `presence.follow` takes.
                "id": row.id,
                "label": row.label,
                "role": row.role,
                // Kept separate from the label, same as the panel: inferring which of
                // these is a person from the label alone is the one mistake to avoid.
                "is_agent": row.is_agent,
                // Named but not arrived — the one thing a harness watching a join needs,
                // and what a screenshot of a dimmed row can't be asserted on.
                "invited": row.invited,
                // Connected and in this workspace: what Follow offers.
                "followable": row.followable,
            })
        })
        .collect();
        serde_json::json!({ "pointers": rows, "roster": roster })
    }

    fn sharing_json(&self) -> serde_json::Value {
        let Some(shares) = self.shares else {
            return serde_json::Value::Null;
        };
        let chrome = self.fb.chrome();
        let tree = chrome.tree();
        serde_json::json!({
            "exposures": shares.disclosure().into_iter().map(|(peer, surface, can_control)| {
                let participant = tree.desktop.workspaces.iter().find_map(|workspace| {
                    workspace.collaborators.iter()
                        .find(|person| person.session == Some(peer))
                        .map(|person| (workspace, person))
                });
                let name = participant.map_or_else(
                    || crate::shell::state::session_handle(peer),
                    |(_, person)| person.name.clone(),
                );
                let kind = participant.map_or("unknown", |(workspace, person)| {
                    if workspace.agent_members().iter()
                        .any(|(agent, _)| {
                            agent.eq_ignore_ascii_case(&person.id)
                                || agent.eq_ignore_ascii_case(&person.name)
                        }) {
                        "agent"
                    } else {
                        "person"
                    }
                });
                let target = tree.desktop.workspaces.iter().find_map(|workspace| {
                    workspace.strips.iter()
                        .flat_map(|strip| &strip.window_groups)
                        .flat_map(|group| &group.tabs)
                        .find(|tab| matches!(
                            &tab.tile,
                            crate::shell::state::TileContent::Foreign { surface_id, .. }
                                if *surface_id == surface
                        ))
                        .map(|tab| (workspace.name.clone(), tab.title.clone()))
                });
                serde_json::json!({
                    "peer": peer,
                    "name": name,
                    "kind": kind,
                    "surface": surface,
                    "workspace": target.as_ref().map(|(workspace, _)| workspace),
                    "target": target.as_ref().map(|(_, title)| title),
                    "can_see": true,
                    "can_control": can_control,
                })
            }).collect::<Vec<_>>()
        })
    }

    /// Serialised straight to JSON text: building a `serde_json::Value` of
    /// the whole tree first cost several times the serialisation. Without
    /// `include_history` the workspaces' append-only history stays out, so
    /// the dump costs the desktop as it is, not every action ever taken.
    fn workspace_tree_json(&self, include_history: bool) -> test_control::RawJson {
        let chrome = self.fb.chrome();
        let tree = chrome.tree();
        if include_history {
            test_control::RawJson::of(&*tree)
        } else {
            tilo_desktop_model::history::without_history(|| test_control::RawJson::of(&*tree))
        }
    }

    fn windows_json(&self) -> Result<serde_json::Value, String> {
        let chrome = self.fb.chrome();
        let tree = chrome.tree();
        Ok(serde_json::Value::Array(test_control::window_rows(
            &tree.desktop,
        )))
    }

    /// Report the compositor's own frame bookkeeping for every foreign toplevel.
    /// `frame_seq` counts frames actually taken from that client, so `0` means
    /// the tile is still the placeholder; `last_error` carries why a capture
    /// failed, which a blank tile would otherwise hide.
    fn foreign_frames_json(&self) -> serde_json::Value {
        let chrome = self.fb.chrome();
        let store = crate::compositor::foreign::store();
        let keyboard_focus = self
            .seat
            .and_then(|seat| seat.client_input.keyboard_focus());
        let rect_json = |rect: Rect| {
            serde_json::json!({
                "x": rect.x0, "y": rect.y0,
                "w": rect.width(), "h": rect.height(),
            })
        };
        let mut frames: Vec<serde_json::Value> =
            crate::compositor::foreign::foreign_tabs(&chrome.tree())
                .into_iter()
                .map(|tab| {
                    let (pixels, capture_pending) =
                        store.presentation_capture_state(tab.surface_id);
                    // The rect the widget tree laid this tab into, so a harness can aim at it.
                    let placement = store.placement(tab.surface_id);
                    let content_rect = placement.map(|rect| store.target_box(tab.surface_id, rect));
                    let content_origin = placement.map(|rect| {
                        let (x, y) = store.corner_origin(tab.surface_id, rect);
                        serde_json::json!({ "x": x, "y": y })
                    });
                    // What the client actually drew, beside the tab it was drawn into — an
                    // opening-then-resizing window is those two disagreeing for a moment.
                    let buffer = store
                        .frame_size(tab.surface_id)
                        .map(|(w, h)| serde_json::json!({ "w": w, "h": h }));
                    let mut row = serde_json::json!({
                        "surface_id": tab.surface_id,
                        "app_id": tab.app_id,
                        // Dialogs shown on this tile, bottom to top, where they are drawn.
                        "sheets": store.sheet_rects(tab.surface_id).into_iter().map(|(link, r)| {
                            serde_json::json!({
                                "surface_id": link.surface,
                                "parent": link.parent,
                                "modal": link.modal,
                                "rect": { "x": r.x0, "y": r.y0, "w": r.width(), "h": r.height() },
                            })
                        }).collect::<Vec<_>>(),
                        "frame_seq": observed_frame_sequence(store, tab.surface_id, pixels),
                        "last_error": store.last_error(tab.surface_id),
                        "rect": placement.map(&rect_json),
                        // The exact content box and surface-local origin used by
                        // `client_input::route_tile_point`. These are observation
                        // only: they carry no connection or input authority.
                        "content_rect": content_rect.map(&rect_json),
                        "content_origin": content_origin,
                        "buffer": buffer,
                        "keyboard_focused": keyboard_focus == Some(tab.surface_id),
                        // Whether the shell is painting this window yet.
                        "presented": store.is_presented(tab.surface_id),
                        "pixels_ready": pixels.is_some(),
                        "capture_pending": capture_pending,
                        // And, while it is not, whether its place is reserved past its
                        // first frame's budget: without this a harness can't tell an empty
                        // desktop from one whose window is on its way.
                        "opening": store.stands(tab.surface_id)
                            && !store.is_presented(tab.surface_id),
                    });
                    self.insert_primary_preedit_route(&mut row, tab.surface_id);
                    row
                })
                .collect();
        if let Some(seat) = self.seat {
            frames.extend(seat.floating_toplevels.iter().map(|surface| {
                let (pixels, capture_pending) =
                    store.presentation_capture_state(surface.surface_id);
                let input_source = seat.floating_preedit_source_for(surface.surface_id);
                // A mapped client or a retained transfer image is not proof
                // that its secondary surface is currently being presented.
                let visible = !surface.suspended
                    && surface.presented
                    && surface.transfer_source.is_none()
                    && store.is_presented(surface.surface_id);
                let (origin_x, origin_y) = store.corner_origin(surface.surface_id, surface.rect);
                let mut row = serde_json::json!({
                    "surface_id": surface.surface_id,
                    "app_id": surface.app_id,
                    "floating_id": surface.public_id,
                    "frame_seq": observed_frame_sequence(store, surface.surface_id, pixels),
                    "last_error": store.last_error(surface.surface_id),
                    "rect": {
                        "x": surface.rect.x0, "y": surface.rect.y0,
                        "w": surface.rect.width(), "h": surface.rect.height(),
                    },
                    "content_rect": rect_json(surface.content_rect),
                    "target_rect": rect_json(store.target_box(surface.surface_id, surface.rect)),
                    "frame_geometry": store.frame_geometry(surface.surface_id),
                    "device_scale": seat.surface_scale(surface.surface_id),
                    "content_origin": {
                        "x": origin_x,
                        "y": origin_y,
                    },
                    "buffer": store.frame_size(surface.surface_id)
                        .map(|(w, h)| serde_json::json!({ "w": w, "h": h })),
                    "presented": visible,
                    "pixels_ready": pixels.is_some(),
                    "capture_pending": capture_pending,
                    "keyboard_focused": keyboard_focus == Some(surface.surface_id),
                    "suspended": surface.suspended,
                    "transferring": surface.transfer_source.is_some(),
                });
                if let Some(source) = input_source {
                    insert_preedit_route(&mut row, "floating-preedit-source", source);
                }
                row
            }));
        }
        serde_json::Value::Array(frames)
    }

    /// Grant on the active workspace, on behalf of the local user, through
    /// `Workspace::grant_as` — which refuses a grantor who neither owns the
    /// room nor holds `Admin`, surfaced as an error, so this op cannot widen
    /// access past what the driver already holds.
    fn grant_capability(
        &self,
        subject: &str,
        scope: &str,
        access: &str,
        id: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        use crate::shell::state::CapOverride;
        let chrome = self.fb.chrome();
        let user = chrome.local_user_id.clone();
        // Clone the handle and drop the `ChromeState` borrow before locking the
        // tree — the RefCell guard has nothing left to do once the handle is in hand.
        let tree_handle = chrome.tree_handle();
        drop(chrome);
        let mut tree = tree_handle.write();
        let ws_id = tree
            .desktop
            .active_workspace()
            .map(|w| w.id)
            .ok_or_else(|| "no active workspace".to_string())?;
        let subject = test_control::parse_subject(subject)?;
        let scope = test_control::parse_scope(scope)?;
        let access = test_control::parse_access(access)?;
        let mut cap = CapOverride::grant(subject, scope, access);
        if let Some(id) = id {
            cap = cap.with_id(id);
        }
        // Through the tree's entry point, not `grant_as` directly, so this answers
        // with the same authority the rest of the compositor grants under.
        let granted = tree
            .grant_sandbox_in_workspace(ws_id, &user, cap)
            .ok_or_else(|| {
                format!("{user} may not administer this workspace, so nothing was granted")
            })?;
        // Nothing past this point touches the tree.
        drop(tree);
        Ok(serde_json::json!({ "granted": granted, "grantor": user }))
    }

    /// Give a grant on the active workspace through the room's governance,
    /// as the user at this session.
    fn allow(
        &self,
        subject: &str,
        node: &str,
        kinds: &[String],
        answer: &str,
        id: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        let subject = test_control::parse_policy_subject(subject)?;
        let node: tilo_action_policy::TreePath = node
            .parse()
            .map_err(|error: tilo_action_policy::PathError| error.to_string())?;
        let kinds = kinds
            .iter()
            .map(|kind| test_control::parse_kind(kind))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .fold(
                tilo_action_policy::KindSet::NONE,
                tilo_action_policy::KindSet::with,
            );
        let answer = test_control::parse_answer(answer)?;
        let chrome = self.fb.chrome();
        let user = chrome.local_user_id.clone();
        let tree_handle = chrome.tree_handle();
        drop(chrome);
        let mut tree = tree_handle.write();
        let (ws_id, room) = tree
            .desktop
            .active_workspace()
            .map(|w| (w.id, w.node()))
            .ok_or_else(|| "no active workspace".to_string())?;
        let grant = tilo_action_policy::Grant::new(
            id.map_or_else(|| format!("grant:{}", uuid::Uuid::new_v4()), str::to_owned),
            subject,
            room.join(&node),
            kinds,
            answer,
            tilo_action_policy::Subject::person(user.clone()),
        );
        let granted = tree
            .grant_in_workspace(ws_id, &user, grant)
            .ok_or_else(|| format!("{user} may not grant that in this workspace"))?;
        drop(tree);
        Ok(serde_json::json!({ "granted": granted, "grantor": user }))
    }

    /// Take back a grant on the active workspace through its governance.
    fn disallow(&self, id: &str) -> Result<serde_json::Value, String> {
        let chrome = self.fb.chrome();
        let user = chrome.local_user_id.clone();
        let tree_handle = chrome.tree_handle();
        drop(chrome);
        let mut tree = tree_handle.write();
        let ws_id = tree
            .desktop
            .active_workspace()
            .map(|w| w.id)
            .ok_or_else(|| "no active workspace".to_string())?;
        let revoked = tree.revoke_in_workspace(ws_id, &user, &tilo_action_policy::GrantId::new(id));
        drop(tree);
        if revoked {
            Ok(serde_json::json!({ "revoked": id, "revoker": user }))
        } else {
            Err(format!("{user} took back nothing named {id}"))
        }
    }

    /// Admit a person with a role on the active workspace, as the user at
    /// this session.
    fn admit(&self, person: &str, role: &str) -> Result<serde_json::Value, String> {
        let role = match role {
            "viewer" => tilo_action_policy::Role::Viewer,
            "commenter" => tilo_action_policy::Role::Commenter,
            "editor" => tilo_action_policy::Role::Editor,
            _ => {
                return Err(format!(
                    "role must be viewer / commenter / editor, got `{role}`"
                ));
            }
        };
        let chrome = self.fb.chrome();
        let user = chrome.local_user_id.clone();
        let tree_handle = chrome.tree_handle();
        drop(chrome);
        let mut tree = tree_handle.write();
        let ws_id = tree
            .desktop
            .active_workspace()
            .map(|w| w.id)
            .ok_or_else(|| "no active workspace".to_string())?;
        let admitted = tree.admit_in_workspace(ws_id, &user, person, role);
        drop(tree);
        admitted
            .map(|_| serde_json::json!({ "admitted": person, "admitter": user }))
            .ok_or_else(|| format!("{user} may not admit {person} to this workspace"))
    }

    /// Revoke on the active workspace through `Workspace::revoke_as` — same authority check as a grant.
    fn revoke_capability(&self, id: &str) -> Result<serde_json::Value, String> {
        let chrome = self.fb.chrome();
        let user = chrome.local_user_id.clone();
        // Same early-drop as `grant_capability`: nothing further needs the
        // `ChromeState` borrow once the tree handle is cloned out of it.
        let tree_handle = chrome.tree_handle();
        drop(chrome);
        let mut tree = tree_handle.write();
        let ws_id = tree
            .desktop
            .active_workspace()
            .map(|w| w.id)
            .ok_or_else(|| "no active workspace".to_string())?;
        let ws = tree
            .desktop
            .workspaces
            .iter()
            .find(|w| w.id == ws_id)
            .ok_or_else(|| "no active workspace".to_string())?;
        // Distinguish the two failures: a revoke returns false both for "you
        // may not" and "no such capability" — conflating them hides an unauthorised revoke as a no-op.
        if !ws.may_administer(&user) {
            return Err(format!(
                "{user} may not administer this workspace, so nothing was revoked"
            ));
        }
        let removed = tree.revoke_sandbox_in_workspace(ws_id, &user, id);
        // Nothing past this point touches the tree.
        drop(tree);
        Ok(serde_json::json!({ "revoked": removed, "revoker": user, "id": id }))
    }

    /// What the room's policy answers the live workspace's actor for one action.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "ws (borrowed from tree) is read all the way through building the final \
                  Ok(...) response below; there's no earlier point to drop it without cloning \
                  the workspace out for no real gain"
    )]
    fn check_action(
        &self,
        actor: &str,
        app: &str,
        resource: &str,
        action: &str,
        kind: &str,
        destructive: bool,
    ) -> Result<serde_json::Value, String> {
        let chrome = self.fb.chrome();
        // Same early-drop as `grant_capability`: only the tree is read below.
        let tree_handle = chrome.tree_handle();
        drop(chrome);
        let tree = tree_handle.read();
        let ws = tree
            .desktop
            .active_workspace()
            .ok_or_else(|| "no active workspace".to_string())?;
        let kind = test_control::parse_kind(kind)?;
        let actor = match actor.split_once(':') {
            Some(("person", id)) => ws.actor_for_person(&id.to_string()),
            Some(("app", id)) => ws.actor_for_app(id),
            Some(("agent", id)) => ws.actor_for_agent(id),
            _ => {
                return Err(format!(
                    "actor must be person:<id>, app:<id> or agent:<id>, got `{actor}`"
                ));
            }
        };
        let node = tilo_action_policy::TreePath::root()
            .app(app)
            .resource(resource)
            .action(action);
        let answer = crate::shell::search::visibility::answer_for(ws, &actor, &node, kind);
        let permitted = crate::shell::search::visibility::permits(ws, &actor, &node, kind);
        Ok(serde_json::json!({
            "answer": format!("{answer:?}"),
            "permitted": permitted,
            "needs_confirmation": permitted && destructive,
            "workspace": ws.id.raw(),
            "room": ws.room().to_string(),
        }))
    }

    /// Enumerate the exported action surface through the *same* provider
    /// registry that feeds the Ctrl+K palette — but renders nothing.
    /// The whole listing, unpaged.
    fn enumerate_actions(&self, query: &str) -> Result<serde_json::Value, String> {
        self.listing(query, false, None)
            .map_err(|refusal| refusal.message)
    }

    fn enumerate_actions_with_programmatic(
        &self,
        query: &str,
        include_programmatic: bool,
        page: &test_control::ListingRequest,
    ) -> Result<serde_json::Value, test_control::ActionRefusal> {
        self.listing(query, include_programmatic, Some(page))
    }

    /// Resolve one `(provider, id)` address against a fresh catalogue,
    /// reporting what it names without running it — selection and invocation
    /// are deliberately separate, so an unknown or gate-hidden address fails
    /// loudly here instead of becoming a silent no-op invocation.
    fn resolve_action(
        &self,
        provider: &str,
        id: &str,
        args: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value, test_control::ActionRefusal> {
        let (doc, root) = self.doc.ok_or_else(|| {
            test_control::ActionRefusal::unknown("control state has no live document")
        })?;
        let chrome_handle = self.fb.chrome_handle();
        let resource = ResourceRef::new(provider, id);
        let catalog = with_action_gate(&chrome_handle, |chrome, gate| {
            let first_use_gate = super::admin_control::FirstUseVisibility {
                base: gate,
                first_use: self.fb.host().onboarding_active(),
            };
            self.fb.host().action_catalog_for_resource(
                &resource,
                chrome,
                doc,
                root,
                &first_use_gate,
            )
        })
        .map_err(test_control::ActionRefusal::unknown)?;
        let selected = catalog.select(&resource).ok_or_else(|| {
            test_control::ActionRefusal::unknown(format!(
                "no visible action addressed by ({provider}, {id}) in the current catalogue"
            ))
        })?;
        // Bind before anything is planned: a bad argument set is refused with the
        // document untouched, and the refusal names the parameter to correct.
        ActionArgs::bind_json(selected.params(), args)
            .map_err(|e| test_control::ActionRefusal::invalid_arguments(e.to_string()))?;
        let resolved = serde_json::json!({
            "provider": provider,
            "id": id,
            "kind": selected.kind().id(),
            "title": selected.title(),
            "params": params_json(selected.params()),
            "contract": selected.contract(),
        });
        // The planned invocation runs from this resolution: resolving again
        // repeated the provider's whole lookup for the same answer.
        if let Some(slot) = self.resolved {
            *slot.borrow_mut() = Some(ResolvedInvocation { resource, catalog });
        }
        Ok(resolved)
    }

    /// Build a structured view of [`ChromeState`] for `state_dump`, constructed
    /// manually rather than `derive(Serialize)` because the chrome owns
    /// `Rc<RefCell<…>>` widget handles that can't sensibly serialise; this also
    /// lets fields like `search_scope` pick a wire-friendly enum-as-string form.
    #[allow(
        clippy::too_many_lines,
        reason = "one flat JSON literal with a line per dumped chrome field"
    )]
    fn chrome_state_json(&self) -> serde_json::Value {
        // Read before the chrome is borrowed below: asking the widgets whether
        // they animate reaches the top bar, which borrows the chrome itself.
        let animating = self.fb.animation_owners();
        let chrome = self.fb.chrome();
        let overlays = &chrome.overlays;
        let native_broker = chrome
            .native_app_opening
            .as_ref()
            .and_then(|opening| opening.0.upgrade().ok());
        let peer_sync = self.fb.host().peer_sync_snapshot();
        let (status_sharing, own_status, following) = self.fb.host().presence_snapshot();
        let room_sync = room_sync_json(&chrome.tree());
        let mut dump = serde_json::json!({
            // Where each workspace's room stands with its host, what its
            // apps are told (`ui::room_sync`), and which apps it lets write.
            "room_sync": room_sync,
            "seat_focus": self.seat.map(State::seat_focus_json),
            "native_window_provenance": self.seat.map(|seat| seat.native_window_provenance_with_broker(native_broker.as_ref())),
            "overlays": {
                "overview": overlays.overview,
                "quick_settings": overlays.quick_settings,
                "ws_settings": overlays.ws_settings,
                "collaborators": overlays.collaborators,
                "room_activity": overlays.room_activity,
                "power_menu": self.fb.host().power_menu_open(),
            },
            // What the pointer shows: a drawn shape, the focused client's
            // own surface, or nothing because the client hid it.
            "pointer": self.fb.pointer_image().describe(),
            // The input hardware and last-used modality the chrome adapts
            // to, and the language the desktop help overlay is written in.
            "input": input_environment_json(self.fb.host().interaction_environment(), self.fb.host().shortcut_help_modality()),
            "input_capture": {
                "onboarding": self.fb.host().onboarding_active(),
                "chrome": client_input::chrome_captures_input(self.fb),
                "inner_surface": self.fb.host().inner_surface_captures_input(),
                "maximized_controls": self.fb.host().maximized_chrome_captures_pointer(),
                "shortcut_help": overlays.shortcut_help,
                "projection": overlays.projection,
            },
            // The on-screen keyboard: whether its service has a surface,
            // and where it is painted while it serves the focused field.
            "on_screen_keyboard": {
                "connected": self.fb.osk_connected(),
                "requested": self.fb.osk_requested(),
                "mapped": self.fb.osk_mapped(),
                "commits": self.fb.osk_commits(),
                "serves_shell_field": self.fb.osk_serves_shell(),
                "shown": self.fb.osk_surface_rect().map(|(_, rect)| serde_json::json!({
                    "x": rect.x0, "y": rect.y0, "w": rect.width(), "h": rect.height(),
                })),
            },
            "search_scope": format!("{:?}", chrome.search_scope),
            "displays": self.displays_json(),
            // Applications' status-notifier items and their menus, as the top bar shows them.
            "tray": crate::tray::global().state_json(),
            // Each tab pill's artwork: `toplevel:` when the window set its
            // own, `theme:` for its application's icon.
            "tab_icons": tab_icons_json(&chrome.tree()),
            // The Alt+Tab switcher while it is open: its windows in order,
            // the selected one, and whether each card shows a live preview
            // (`live`), its app mark because the app holds secrets
            // (`withheld`), or its app mark for now (`none`).
            "window_switcher": self.fb.host().window_switcher_state_json(),
            // Who asks for animation frames now: chrome sources by name, the
            // deepest animating widgets by path with their geometry (and a
            // window's frame state). Empty when the desktop may sleep.
            "animating": animating,
            // Nested resource search is a visible place in the palette, not
            // merely provider-internal state. Expose the authenticated route
            // and label so agents and accessibility tooling can tell exactly
            // where subsequent search and invocation are scoped.
            "search_resource_scope": chrome.search_resource_scope.as_ref().map(|scope| serde_json::json!({
                "peer_id": scope.peer_id,
                "cluster_id": scope.cluster_id,
                "app_id": scope.app_id,
                "resource": scope.resource,
                "label": scope.label,
            })),
            "presence": {
                "status_sharing": match status_sharing {
                    ui::StatusSharing::Off => "off",
                    ui::StatusSharing::ManualOnly => "manual",
                    ui::StatusSharing::Automatic => "automatic",
                },
                "own_status": own_status,
                "following": following,
            },
            "ws_settings_tab": format!("{:?}", chrome.ws_settings_tab),
            "revision": chrome.revision,
            // What is actually on screen — without it a harness can only see that *a*
            // frame painted, not that a specific notification reached it.
            "notifications": chrome.notifications().iter().map(|n| serde_json::json!({
                "title": n.title,
                "body": n.body,
                "severity": format!("{:?}", n.severity),
                "persistent": n.dismiss_after >= tilo_desktop::notifications::PERSISTENT_DISMISS,
            })).collect::<Vec<_>>(),
        // The hosted file manager's drag, if in flight, and whether the compositor took it over to a client.
            "files_drag": file_explorer::external_drag::unclaimed().map(|d| d.paths),
            "files_drag_handed_off": file_explorer::external_drag::is_handed_off(),
            // The compositor-owned sheet on screen (portal review, polkit,
            // network secrets); masked values appear only as their length.
            "portal_dialog": self.fb.host().portal_dialog_state_json(),
            "editing_workspace_name": chrome.editing_workspace_name,
            "pending_grants_count": chrome.pending_grants.len(),
        // Mirrored from the SessionManager each tick; lets a headless test observe lock/unlock transitions.
            "session_locked": chrome.session_locked(),
            // Exact compositor-owned lock UI state. A harness must wait for
            // an asynchronous PAM result before typing a retry; without this,
            // the controller correctly swallows the second password while the
            // first worker is still in flight and the failure looks like lost
            // keyboard input.
            "lock_auth": {
                "verifying": self.lock.is_some_and(LockController::is_verifying),
                "password_len": self.lock.map_or(0, LockController::buffer_len),
                "error": self.lock.and_then(LockController::error),
                // The keyboard drawn on the lock screen, while it shows:
                // where each key sits, never what was typed.
                "keyboard": self.fb.lock_keyboard_keys().iter().map(|(name, rect)| {
                    serde_json::json!({
                        "key": name,
                        "x": rect.x0, "y": rect.y0, "w": rect.width(), "h": rect.height(),
                    })
                }).collect::<Vec<_>>(),
                // One lock surface per display, in scene pixels: which one
                // carries the password field, and at what scale it is drawn.
                "surfaces": self.fb.lock_surfaces().iter().map(|surface| {
                    let rect = |rect: Rect| serde_json::json!({
                        "x": rect.x0, "y": rect.y0, "w": rect.width(), "h": rect.height(),
                    });
                    serde_json::json!({
                        "display": rect(surface.output),
                        "scale": surface.scale,
                        "password": surface.password.map(rect),
                    })
                }).collect::<Vec<_>>(),
            },
            // Who this session is, and whether they may administer the room they're
            // in — read straight off the production helpers, not guessed from `owner`.
            "local_user_id": chrome.active_local_user(),
            "may_administer_active": chrome
                .tree()
                .desktop.active_workspace()
                .is_some_and(|ws| ws.may_administer(&chrome.local_user_for_room(&ws.room()))),
            // The peer-sync card's contents — the invite token, exposed for scripts as it is on screen.
            "peer_sync": {
                "invite_token": peer_sync.invite_token,
                "connect_input": peer_sync.connect_input,
                "status": peer_sync.status,
                // The invite composer: what's typed into "who are you inviting?", and who
                // the on-screen token was minted for — the same two facts the card shows.
                "invite_name": peer_sync.invite_name,
                "invite_for": peer_sync.invite_for,
            },
            // Whether apps can share edits at all, and why not when they can't — else
            // a harness can't tell "off" from "on and idle".
            "collab": {
                "available": chrome.collab_available(),
                "unavailable_reason": chrome.collab_unavailable,
                "socket": launcher::services::collab_socket().display().to_string(),
            },
            // What the Permissions tab's grant composer is composing — a click-driving
            // harness has no other way to see which subject/capability it landed on.
            "ws_grant_draft": {
                "subject": chrome
                    .tree()
                    .desktop.active_workspace()
                    .and_then(|ws| {
                        tilo_desktop::workspace_settings::draft_subject(
                            ws,
                            &chrome.ws_grant_draft,
                        )
                    })
                    .map(|s| format!("{:?}:{}", s.kind, s.id)),
                "capability": chrome.ws_grant_draft.choice().label(),
                "expiry": format!("{:?}", chrome.ws_grant_draft.expiry),
            },
            "palette": palette_json(self.fb),
            // Where along each strip the user is looking — two frames with an
            // identical tree can show different windows.
            "strip_scroll": self
                .fb
                .host()
                .strip_scroll_offsets()
                .into_iter()
                .map(|(strip, scroll_x, max_scroll_x)| serde_json::json!({
                    "strip": strip,
                    "scroll_x": scroll_x,
                    // Zero means the strip fits the display and can't pan — "unmoved" is vacuous then.
                    "max_scroll_x": max_scroll_x,
                }))
                .collect::<Vec<_>>(),
            // Debug-session acceptance drives the same live pointer targets
            // a person sees. Zero-area means the hover-revealed maximize or
            // maximized restore control is not currently painted.
            "active_window_controls": self.fb.host()
                .active_window_control_rects()
                .map(|(maximize, restore)| serde_json::json!({
                    "maximize": { "x": maximize.x0, "y": maximize.y0, "w": maximize.width(), "h": maximize.height() },
                    "restore": { "x": restore.x0, "y": restore.y0, "w": restore.width(), "h": restore.height() },
                })),
            // Super+S (or hovering the right-edge strip ribbon) opens the
            // strip switcher card; `true` while it is up.
            "strip_switcher_open": chrome.strip_switcher_open(),
            // Super+F on a Tilo window sets this; a client xdg fullscreen does too.
            // Maximize is a different field (`workspace_tree` group.maximized).
            "fullscreen_group": chrome.fullscreen_group(),
            "fullscreen": chrome.is_fullscreen(),
        });
        // Construction work counted at the workspace's factories, and the
        // workspace whose content those factories last reconciled.
        if let Some(fields) = dump.as_object_mut() {
            fields.insert(
                "workspace_work".into(),
                serde_json::json!(crate::shell::work_accounting::snapshot()),
            );
            fields.insert(
                "workspace_content".into(),
                serde_json::json!(self.fb.host().workspace_content_id()),
            );
            // The Quick Settings volume level, as its slider shows it, and
            // how many marks the open screenshot editor holds (null when it
            // is closed).
            fields.insert(
                "quick_settings_volume".into(),
                serde_json::json!(chrome.quick_settings.volume_level()),
            );
            fields.insert(
                "screenshot_markup_strokes".into(),
                serde_json::json!(self.fb.host().screenshot_markup_strokes()),
            );
            // Whether this device's identity waits for the clock: intact,
            // with a certificate the clock's reading refuses.
            fields.insert(
                "identity".into(),
                serde_json::json!({
                    "waits_for_clock": chrome.identity_waits_for_clock(),
                    "recovery": std::env::var_os(crate::shell::identity_provisioning::IDENTITY_RECOVERY_ENV)
                        .is_some_and(|value| value == "1"),
                }),
            );
        }
        dump
    }
}

/// Each workspace's room for `state_dump`: where it stands with its host
/// ([`crate::shell::room_sync`], `null` for a room with no host), the apps
/// its grants make authors, and how many grants it holds, as this device
/// last received them.
fn room_sync_json(tree: &crate::shell::state::WorkspaceTree) -> serde_json::Value {
    let now = tilo_clock::unix_now();
    tree.desktop
        .workspaces
        .iter()
        .map(|workspace| {
            serde_json::json!({
                "workspace": workspace.name,
                "room": workspace.room().to_string(),
                "state": crate::shell::room_sync::state(
                    crate::shell::network::room_topic_id(workspace.room()),
                ),
                "app_authors": workspace.launch_grants(now).authors,
                "grants": workspace.policy.grants.len(),
            })
        })
        .collect()
}

/// The command palette's live state, for `state_dump` — which row Enter
/// would activate, otherwise unobservable to a socket-driven harness.
fn palette_json(fb: &FrameBuilder) -> serde_json::Value {
    let (visible, query, selected, _) = fb.host().palette_snapshot();
    serde_json::json!({
        "visible": visible,
        "query": query,
        "selected_id": selected,
        // Which kind chips are lit — "filtered to apps" and "query that only apps
        // matched" give the same row list, but only the chip filter survives typing.
        "kind_filter": fb.host().palette_kind_filter(),
        // Label *and* subtitle — the subtitle says what the row will do, so a
        // badge or "not installed" marker wouldn't be visible from the label alone.
        "rows": fb
            .host()
            .palette_rows_detailed()
            .into_iter()
            .map(|(id, label, description)| serde_json::json!({
                "id": id,
                "label": label,
                "description": description,
            }))
            .collect::<Vec<_>>(),
    })
}

/// A screenshot taken after input: written once the clients in it answered
/// that input ([`crate::compositor::client_settle`]).
struct PendingClientSnapshot {
    path: std::path::PathBuf,
    crop: Option<vello::kurbo::Rect>,
    settle: crate::compositor::client_settle::Settle,
    /// Filled once the file is written, or failed to be.
    outcome: SnapshotOutcome,
}

/// How a screenshot's write went, shared between the deferred write and the
/// request that asked for it: the request is answered only once the file at
/// its path is complete, so a caller never reads a PNG still being written.
/// Where a screenshot written off the loop reports how it went; the
/// request's answer waits for it ([`pump_pending_test_control_conn`]).
type SnapshotOutcome = std::sync::Arc<std::sync::Mutex<Option<Result<(), String>>>>;

/// Per-process state the test-control wire needs to execute
/// [`PlannedAction`]s, mirroring `ShellDriver`'s fields: modifier snapshot, auxiliary `AutoCommit` doc, root `ObjId`, focus cursor.
/// A control client's request of the display ([`test_control::Command::DisplayBlank`],
/// [`test_control::Command::DisplayWakeFault`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DisplayRequest {
    Blank,
    WakeFault(crate::compositor::backend::WakeStage),
}

struct TestControlCtx {
    modifiers: Modifiers,
    doc: AutoCommit,
    root_obj: ObjId,
    focus: Option<NodeId>,
    cursor: PhysicalPosition<f64>,
    /// The headless E2E fixture has no libinput device, but it must exercise
    /// precisely the ownership handoff DRM uses before emitting real
    /// `wl_touch` traffic to its Wayland clients.
    direct_touch: DirectTouchOwnership,
    /// Control-socket actions the focused Wayland client took, counted so a
    /// batch the client alone answered is told from one that changed the
    /// chrome ([`Self::client_input_only`]).
    client_routed_actions: usize,
    /// The last batch of actions went to the client alone: the client's own
    /// commit is the frame it asks for, so the loop does not repaint for it,
    /// as it does not for a physical key.
    client_input_only: bool,
    /// Keys a control client asked to arrive as a physical keyboard's do
    /// ([`PlannedAction::SeatKey`]); the loop takes them with the backend's
    /// own events on its next iteration.
    seat_events: Vec<BackendEvent>,
    /// Touch the chrome or a touch-less client receives as pointer input;
    /// shared by physical and injected touch.
    touch_pointer: crate::compositor::touch_pointer::TouchPointer,
    /// Synthetic touchpad gestures go through the tracker the DRM runtime
    /// feeds from libinput, so they mean exactly what the fingers would.
    gestures: crate::compositor::backend::drm::gestures::GestureTracker,
    /// Attached input devices and the last kind of input used, from the
    /// backend or from injected test-control input alike. The shell chrome's
    /// shortcut help follows it.
    input: InputEnvironment,
    /// The latest `watch` answer, for the loop to hand to the backend.
    watch_request: Option<bool>,
    /// What control clients asked of the display, for the loop to apply to
    /// the session and the backend.
    display_requests: Vec<DisplayRequest>,
    /// One seat and strip per agent driving this compositor.
    agents: crate::compositor::agent_seats::AgentSeats,
    /// While an agent's command runs, the screen region its snapshots show:
    /// its strip's viewport.
    snapshot_crop: Option<vello::kurbo::Rect>,
    /// The window an agent's command aimed at, while it runs: its
    /// screenshots wait for that window's client alone.
    snapshot_window: Option<foreign::SurfaceId>,
    /// Input was injected since the last screenshot, which waits for the
    /// clients to answer it.
    input_since_snapshot: bool,
    /// Screenshot not written yet: the clients in it are still answering
    /// the preceding input.
    pending_snapshot: Option<PendingClientSnapshot>,
    /// Set by a `snapshot` command whose write was deferred; the connection
    /// that sent it holds its answer until the outcome is in.
    deferred_snapshot_reply: Option<SnapshotOutcome>,
    /// The address an `actions_invoke` resolved while it was dispatched,
    /// which the same request then runs without resolving it again.
    resolved_invocation: std::cell::RefCell<Option<ResolvedInvocation>>,
}

/// One `actions_invoke` address as its request's dispatch resolved it:
/// through the capability gate, against the live document.
struct ResolvedInvocation {
    resource: ResourceRef,
    catalog: crate::shell::search::action_api::ActionCatalog,
}

impl TestControlCtx {
    fn new() -> Self {
        Self {
            modifiers: Modifiers::default(),
            doc: AutoCommit::new(),
            root_obj: automerge::ROOT,
            focus: None,
            cursor: PhysicalPosition::new(0.0, 0.0),
            direct_touch: DirectTouchOwnership::default(),
            client_routed_actions: 0,
            client_input_only: false,
            seat_events: Vec::new(),
            touch_pointer: crate::compositor::touch_pointer::TouchPointer::default(),
            gestures: crate::compositor::backend::drm::gestures::GestureTracker::default(),
            input: InputEnvironment::default(),
            watch_request: None,
            display_requests: Vec::new(),
            agents: crate::compositor::agent_seats::AgentSeats::new(),
            snapshot_crop: None,
            snapshot_window: None,
            input_since_snapshot: false,
            pending_snapshot: None,
            deferred_snapshot_reply: None,
            resolved_invocation: std::cell::RefCell::new(None),
        }
    }

    /// Hand the current input environment to the shell chrome.
    fn publish_input_environment(&self, fb: &mut FrameBuilder) {
        fb.host_mut()
            .apply_input_environment(self.input.environment());
    }

    /// Fold injected test-control input into the input environment exactly
    /// as a backend reports physical devices: a named touch device is
    /// attached by its first contact, and every event class counts as used.
    fn note_injected_input(&mut self, action: &PlannedAction, fb: &mut FrameBuilder) {
        if !matches!(action, PlannedAction::Snapshot { .. }) {
            self.input_since_snapshot = true;
        }
        let changed = match action {
            PlannedAction::Touch { device, phase, .. } => {
                let attached = self.input.device_added(
                    format!("test-control:{device}"),
                    ui::InputAvailability::NONE.with(ui::InputDevice::Touch),
                );
                let used = matches!(phase, TouchPhaseKind::Down)
                    && self.input.note_used(ui::InputModality::Touch);
                attached || used
            }
            PlannedAction::KeyboardKey { pressed: true, .. } => {
                self.input.note_used(ui::InputModality::Keyboard)
            }
            PlannedAction::CursorMoved { .. } => self.input.note_pointer_motion(),
            PlannedAction::MouseLeftButton { .. }
            | PlannedAction::MousePointerButton { .. }
            | PlannedAction::MouseWheel { .. }
            | PlannedAction::Pen { .. }
            | PlannedAction::Touchpad(_) => self.input.note_used(ui::InputModality::Pointer),
            _ => false,
        };
        if changed {
            self.publish_input_environment(fb);
        }
    }
}

/// Translate a [`KeyTag`] from the wire to the `winit::keyboard::Key` the
/// host matches on: named keys map 1:1 to `NamedKey`; chars wrap in `Key::Character`.
fn keytag_into_winit_key(tag: KeyTag) -> Key {
    match tag {
        KeyTag::Named(n) => Key::Named(match n {
            NamedKeyTag::Enter => NamedKey::Enter,
            NamedKeyTag::Escape => NamedKey::Escape,
            NamedKeyTag::Tab => NamedKey::Tab,
            NamedKeyTag::ArrowLeft => NamedKey::ArrowLeft,
            NamedKeyTag::ArrowRight => NamedKey::ArrowRight,
            NamedKeyTag::ArrowUp => NamedKey::ArrowUp,
            NamedKeyTag::ArrowDown => NamedKey::ArrowDown,
            NamedKeyTag::Backspace => NamedKey::Backspace,
            NamedKeyTag::Delete => NamedKey::Delete,
            NamedKeyTag::Home => NamedKey::Home,
            NamedKeyTag::End => NamedKey::End,
            NamedKeyTag::PageUp => NamedKey::PageUp,
            NamedKeyTag::PageDown => NamedKey::PageDown,
            NamedKeyTag::F1 => NamedKey::F1,
            NamedKeyTag::F4 => NamedKey::F4,
            NamedKeyTag::PrintScreen => NamedKey::PrintScreen,
            NamedKeyTag::AudioVolumeUp => NamedKey::AudioVolumeUp,
            NamedKeyTag::AudioVolumeDown => NamedKey::AudioVolumeDown,
            NamedKeyTag::AudioVolumeMute => NamedKey::AudioVolumeMute,
            NamedKeyTag::MicrophoneVolumeMute => NamedKey::MicrophoneVolumeMute,
            NamedKeyTag::BrightnessUp => NamedKey::BrightnessUp,
            NamedKeyTag::BrightnessDown => NamedKey::BrightnessDown,
            NamedKeyTag::MediaPlayPause => NamedKey::MediaPlayPause,
            NamedKeyTag::MediaTrackNext => NamedKey::MediaTrackNext,
            NamedKeyTag::MediaTrackPrevious => NamedKey::MediaTrackPrevious,
            NamedKeyTag::Space => NamedKey::Space,
            NamedKeyTag::Shift => NamedKey::Shift,
            NamedKeyTag::Control => NamedKey::Control,
            NamedKeyTag::Alt => NamedKey::Alt,
            NamedKeyTag::Super => NamedKey::Super,
        }),
        KeyTag::Char(c) => Key::Character(SmolStr::new(c.to_string())),
    }
}

#[cfg(test)]
mod injected_key_mapping_tests {
    use super::keytag_into_winit_key;
    use crate::compositor::test_control::{KeyTag, NamedKeyTag};
    use winit::keyboard::{Key, NamedKey};

    #[test]
    fn injected_media_keys_map_to_the_production_logical_keys() {
        for (tag, expected) in [
            (NamedKeyTag::AudioVolumeUp, NamedKey::AudioVolumeUp),
            (NamedKeyTag::AudioVolumeDown, NamedKey::AudioVolumeDown),
            (NamedKeyTag::AudioVolumeMute, NamedKey::AudioVolumeMute),
            (
                NamedKeyTag::MicrophoneVolumeMute,
                NamedKey::MicrophoneVolumeMute,
            ),
            (NamedKeyTag::BrightnessUp, NamedKey::BrightnessUp),
            (NamedKeyTag::BrightnessDown, NamedKey::BrightnessDown),
            (NamedKeyTag::MediaPlayPause, NamedKey::MediaPlayPause),
            (NamedKeyTag::MediaTrackNext, NamedKey::MediaTrackNext),
            (
                NamedKeyTag::MediaTrackPrevious,
                NamedKey::MediaTrackPrevious,
            ),
        ] {
            assert_eq!(
                keytag_into_winit_key(KeyTag::Named(tag)),
                Key::Named(expected)
            );
        }
    }
}

fn switch_action_event(action: SwitchAction, pressed: bool) -> WindowEvent {
    keyboard_input_event(
        &Key::Named(match action {
            SwitchAction::Next => NamedKey::Tab,
            SwitchAction::Activate => NamedKey::Enter,
        }),
        pressed,
    )
}

/// The keystrokes a controller intent stands for. Directional focus is the
/// toolkit's Alt+arrow in an app, and a bare arrow once a shell surface owns
/// the keyboard (palette, overview, guide), where arrows already navigate.
fn controller_key_events(
    key: crate::shell::overlay_host::ControllerKey,
    chrome_captures: bool,
) -> Vec<WindowEvent> {
    use crate::compositor::controller_nav::NavDirection;
    use crate::shell::overlay_host::ControllerKey;
    let (named, alt) = match key {
        ControllerKey::Move(direction) => (
            match direction {
                NavDirection::Up => NamedKey::ArrowUp,
                NavDirection::Down => NamedKey::ArrowDown,
                NavDirection::Left => NamedKey::ArrowLeft,
                NavDirection::Right => NamedKey::ArrowRight,
            },
            !chrome_captures,
        ),
        ControllerKey::Activate => (NamedKey::Enter, false),
        ControllerKey::Back => (NamedKey::Escape, false),
    };
    let alt_key = Key::Named(NamedKey::Alt);
    let key = Key::Named(named);
    let mut events = Vec::with_capacity(4);
    if alt {
        events.push(keyboard_input_event(&alt_key, true));
    }
    events.push(keyboard_input_event(&key, true));
    events.push(keyboard_input_event(&key, false));
    if alt {
        events.push(keyboard_input_event(&alt_key, false));
    }
    events
}

/// Read local controllers and act on what they asked for: desktop-layout
/// intents run in the shell, focus intents become keystrokes through the
/// same client-first route physical keys take. Returns whether anything
/// changed. Input is still drained on the lock screen so presses made there
/// never replay after unlock.
fn pump_controller_navigation(
    runtime: &mut crate::compositor::controller_nav::ControllerNavRuntime,
    state: &mut State,
    frame_builder: &mut FrameBuilder,
    ctx: &mut TestControlCtx,
    locked: bool,
) -> bool {
    use crate::shell::overlay_host::ControllerDelivery;
    let forwarded = crate::shell::physical_gamepad::forwarded_physical_gamepads();
    let passthrough = state.fullscreen_target.is_some();
    let intents = runtime.poll(Instant::now(), &forwarded, passthrough);
    if locked || intents.is_empty() {
        return false;
    }
    for (intent, family) in intents {
        if let ControllerDelivery::Key(key) =
            frame_builder.host_mut().apply_controller_intent(intent)
        {
            let captured = crate::shell::input_policy::chrome_captures_input(frame_builder);
            for event in controller_key_events(key, captured) {
                if let WindowEvent::KeyboardInput { event: key_ev, .. } = &event {
                    client_input::apply_modifier_key(&mut ctx.modifiers, key_ev);
                }
                let to_client = client_input_allowed(frame_builder)
                    && client_input::route_input(state, frame_builder, &event, &ctx.modifiers);
                if !to_client {
                    let _ = frame_builder.dispatch_event(
                        &event,
                        &ctx.modifiers,
                        &mut ui::widget::Edits::lend(&mut ctx.doc),
                        &ctx.root_obj,
                        &mut ctx.focus,
                    );
                }
            }
        }
        // Chrome moves change the focused tab; move the Wayland seat before
        // the next intent so it lands in the window now in front.
        frame_builder.host_mut().flush_active_tab_writeback();
        client_input::sync_keyboard_focus(state, frame_builder);
        // Controller use is classified here, at its source. The keystrokes
        // it synthesised above never reach the input environment, so they
        // do not count as typing on a keyboard.
        if ctx.input.note_controller(family) {
            ctx.publish_input_environment(frame_builder);
        }
    }
    true
}

/// Apply a `ModifiersState` mask (already assembled by the caller from a
/// [`SetModifiers`] action) by overwriting `ctx.modifiers` — the host reads
/// the snapshot inline on the next chord. Mirrors `ShellDriver::set_modifiers`.
///
/// Takes the assembled mask rather than the four bare `ctrl`/`shift`/`alt`/
/// `logo` bools directly — as either bool parameters or a bool-only struct,
/// four independent flags read ambiguous at the call site
/// (`fn_params_excessive_bools`/`struct_excessive_bools`); a `ModifiersState`
/// bitmask is the type this data already is one call site away.
fn apply_modifier_mask(ctx: &mut TestControlCtx, mask: ModifiersState) {
    ctx.modifiers = Modifiers::from(mask);
}

/// Offer `event` to the focused Wayland client; returns `true` when it was
/// consumed and must not also reach the chrome.
fn client_input_allowed(fb: &FrameBuilder) -> bool {
    !fb.host().onboarding_active()
}

/// Results of the background media codecs install (it outlives setup and can
/// finish once the device comes online) become ordinary notifications.
fn post_codec_pack_notices(frame_builder: &mut FrameBuilder) -> bool {
    let notices = crate::shell::codec_pack_notice::drain();
    if notices.is_empty() {
        return false;
    }
    frame_builder.with_chrome(|chrome| {
        for notice in notices {
            chrome.push_notification_full(notice);
        }
        chrome.bump();
    });
    true
}

/// An NTFS drive the person opened opens in Files; what went wrong, or why
/// it opened read-only, becomes a notification.
fn post_drive_events(frame_builder: &mut FrameBuilder) -> bool {
    let notices = crate::shell::drives::drain_notices();
    let revealed = crate::shell::drives::pump_reveals(&frame_builder.chrome_handle());
    if notices.is_empty() {
        return revealed;
    }
    frame_builder.with_chrome(|chrome| {
        for notice in notices {
            chrome.push_notification_full(notice);
        }
        chrome.bump();
    });
    true
}

/// A staged system update applies live; the desktop finishes it while the
/// person is away, and asks for a restart of the machine only when part of
/// it needs one, at most every two days ([`crate::shell::system_update`]).
fn post_system_update_asks(frame_builder: &mut FrameBuilder, session: &SessionManager) -> bool {
    crate::shell::system_update::start();
    crate::shell::system_update::note_presence(
        session.idle_for(Instant::now()),
        session.is_locked() || session.is_blanked(),
    );
    // Deployed layers held on this image (a development machine only).
    crate::shell::held_layers::start();
    let mut asks = crate::shell::system_update::drain();
    asks.extend(crate::shell::held_layers::drain());
    // Networks NetworkManager could not join on its own for want of a
    // password the person can give.
    asks.extend(crate::network_secrets::drain_cannot_join());
    if asks.is_empty() {
        return false;
    }
    frame_builder.with_chrome(|chrome| {
        for ask in asks {
            chrome.push_notification_full(ask);
        }
        chrome.bump();
    });
    true
}

/// A collaborator's device at another peer protocol version: who has to
/// update ([`crate::shell::collaborator_versions`]).
fn post_collaborator_version_notices(frame_builder: &mut FrameBuilder) -> bool {
    let mut posted = false;
    frame_builder.with_chrome(|chrome| {
        let notices = crate::shell::collaborator_versions::drain(&chrome.tree());
        for notice in notices {
            chrome.push_notification_full(notice);
            posted = true;
        }
        if posted {
            chrome.bump();
        }
    });
    posted
}

/// Start XDG autostart entries once per login, as soon as there is a
/// workspace to attribute them to (and whose X11 display they may use).
fn run_autostart_once(frame_builder: &FrameBuilder) {
    static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if STARTED.load(std::sync::atomic::Ordering::Relaxed)
        || !crate::compositor::session_env::is_tilo_login()
    {
        return;
    }
    let Some(room) = frame_builder
        .chrome()
        .tree()
        .desktop
        .active_workspace()
        .map(|workspace| workspace.room().to_string())
    else {
        return;
    };
    STARTED.store(true, std::sync::atomic::Ordering::Relaxed);
    match crate::shell::host_apps::launch_autostart(Some(&room)) {
        Ok(started) => tracing::info!(started, "started XDG autostart entries"),
        Err(error) => tracing::warn!(%error, "XDG autostart entries were not started"),
    }
}

/// Host palette actions are queued by chrome, then launched here so the
/// compositor can mint an external xdg-activation token before the child is
/// spawned. This keeps the token single-use and binds it to the exact user
/// action rather than accepting an app-id guess later.
fn launch_queued_host_apps(
    state: &mut State,
    frame_builder: &mut FrameBuilder,
    wake: &crate::session_runtime::Wake,
) -> bool {
    let requests = crate::shell::host_apps::drain_queued_launches();
    let mut changed = false;
    for request in requests {
        let reopened = {
            let chrome = frame_builder.chrome();
            let tree = chrome.tree();
            crate::shell::host_apps::reopen_from_tray(
                &request,
                crate::tray::global(),
                &tree,
                Arc::clone(wake),
            )
        };
        if reopened {
            continue;
        }
        tracing::info!(
            target: "shell::compositor::host_launch",
            desktop_id = %request.app.desktop_id,
            display_name = %request.app.name,
            placement = ?request.placement,
            request_to_dispatch_us = request.requested_at.elapsed().as_micros(),
            "dispatching user-requested host application launch"
        );
        let token = state.issue_host_launch_activation(
            request.app.desktop_id.clone(),
            request.app.name.clone(),
            request.placement,
        );
        if let Err(error) = crate::shell::host_apps::launch_resolved_with_activation(
            request.app.clone(),
            None,
            request.portal_workspace_id,
            Some(&token),
        ) {
            tracing::error!(
                target: "shell::compositor::host_launch",
                desktop_id = %request.app.desktop_id,
                %error,
                "host application launch was refused before spawn"
            );
            // Nothing can consume this external token after a preflight
            // refusal. Remove it immediately, alongside the staged placement.
            state.cancel_host_launch_activation(&token);
            frame_builder.with_chrome(|chrome| {
                chrome.push_notification_full(
                    tilo_desktop::notifications::Notification::new(
                        crate::shell::launcher::launch_failure_title(&request.app.name),
                        error.notification_message(),
                    )
                    .with_severity(tilo_desktop::notifications::NotificationSeverity::Error)
                    .with_app_name(crate::shell::i18n::t("notification-app-host-apps"))
                    .with_dismiss_after(crate::shell::launcher::LAUNCH_FAILURE_NOTICE_LIFETIME),
                );
                chrome.bump();
            });
            changed = true;
        }
    }
    changed
}

fn to_client(
    state: &mut Option<&mut State>,
    fb: &mut FrameBuilder,
    event: &WindowEvent,
    modifiers: Modifiers,
) -> bool {
    if !client_input_allowed(fb) {
        return false;
    }
    match state {
        Some(st) => client_input::route_input(st, fb, event, &modifiers),
        None => false,
    }
}

/// Offer `event` to the client first, then the chrome — the shared shape
/// behind every [`PlannedAction`] arm that just synthesises one `WindowEvent`.
fn dispatch_synth_event(
    event: &WindowEvent,
    fb: &mut FrameBuilder,
    ctx: &mut TestControlCtx,
    state: &mut Option<&mut State>,
) {
    if to_client(state, fb, event, ctx.modifiers) {
        ctx.client_routed_actions = ctx.client_routed_actions.saturating_add(1);
        return;
    }
    if let Some(st) = state.as_deref_mut() {
        client_input::finish_chrome_drag_into_shell(st, fb, event);
    }
    fb.dispatch_event(
        event,
        &ctx.modifiers,
        &mut ui::widget::Edits::lend(&mut ctx.doc),
        &ctx.root_obj,
        &mut ctx.focus,
    );
}

/// Wire name of an input modality in `state-dump`.
fn input_modality_name(modality: ui::InputModality) -> &'static str {
    match modality {
        ui::InputModality::Pointer => "pointer",
        ui::InputModality::Touch => "touch",
        ui::InputModality::Keyboard => "keyboard",
        ui::InputModality::Controller => "controller",
    }
}

/// `state-dump`'s view of the chrome's input environment.
fn input_environment_json(
    environment: ui::InteractionEnvironment,
    help_modality: ui::InputModality,
) -> serde_json::Value {
    let available = [
        (ui::InputDevice::Keyboard, "keyboard"),
        (ui::InputDevice::Pointer, "pointer"),
        (ui::InputDevice::Touch, "touch"),
        (ui::InputDevice::Controller, "controller"),
    ]
    .into_iter()
    .filter(|(device, _)| environment.available.contains(*device))
    .map(|(_, name)| name)
    .collect::<Vec<_>>();
    serde_json::json!({
        "modality": input_modality_name(environment.modality),
        "available": available,
        "help_modality": input_modality_name(help_modality),
    })
}

/// The state dump's name for a key of the lock screen's keyboard.
fn passcode_key_name(key: ui::passcode_keyboard::PasscodeKey) -> String {
    use ui::passcode_keyboard::PasscodeKey;
    match key {
        PasscodeKey::Char(c) => c.to_string(),
        PasscodeKey::Shift => "shift".into(),
        PasscodeKey::Symbols => "symbols".into(),
        PasscodeKey::Space => "space".into(),
        PasscodeKey::Backspace => "backspace".into(),
        PasscodeKey::Enter => "enter".into(),
    }
}

/// Build the shell-side raw touch event for the test-control adapter. The
/// adapter delegates ownership to [`DirectTouchOwnership`]; this merely puts
/// that decision on the same chrome event wire as DRM's `touch_input_event`.
fn test_touch_input_event(
    slot: u32,
    x: f64,
    y: f64,
    phase: winit::event::TouchPhase,
) -> WindowEvent {
    WindowEvent::Touch(winit::event::Touch {
        device_id: winit::event::DeviceId::dummy(),
        phase,
        location: PhysicalPosition::new(x, y),
        force: None,
        id: u64::from(slot),
    })
}

/// Send an already arbitrated direct-touch event to clients through the
/// same routing the DRM backend uses ([`client_input::route_client_touch`]).
fn forward_test_control_touch(
    state: &mut Option<&mut State>,
    fb: &mut FrameBuilder,
    touch: crate::compositor::backend::TouchEvent,
) {
    let Some(state) = state.as_deref_mut() else {
        return;
    };
    let locked = fb.chrome().session_locked();
    client_input::route_client_touch(state, fb, touch, locked, false);
}

/// Run one explicit test-control touchscreen edge. This intentionally uses
/// the DRM ownership arena rather than reimplementing a test-only notion of
/// "third finger": E2E assertions therefore cover the exact cancel/frame
/// decision production hardware uses before the native loop forwards it.
fn dispatch_test_control_touch(
    device: &str,
    slot: u32,
    phase: TouchPhaseKind,
    x: f64,
    y: f64,
    fb: &mut FrameBuilder,
    ctx: &mut TestControlCtx,
    state: &mut Option<&mut State>,
) {
    use crate::compositor::backend::TouchEvent as BackendTouchEvent;
    use winit::event::TouchPhase;

    let key = DirectTouchOwnership::key(device, slot);
    let raw = |id,
               raw_x,
               raw_y,
               raw_phase,
               fb: &mut FrameBuilder,
               ctx: &mut TestControlCtx,
               state: &mut Option<&mut State>| {
        let event = test_touch_input_event(id, raw_x, raw_y, raw_phase);
        // Direct touch belongs to the shell gesture arena. Unlike pointer
        // input it must not first be routed as a client pointer event.
        let handled = fb.dispatch_event(
            &event,
            &ctx.modifiers,
            &mut ui::widget::Edits::lend(&mut ctx.doc),
            &ctx.root_obj,
            &mut ctx.focus,
        );
        let WindowEvent::Touch(touch) = &event else {
            return;
        };
        let taken_by_client = state.as_deref().is_some_and(State::touch_down_taken);
        let pointer_events = crate::compositor::touch_pointer::pointer_events_for_touch(
            &mut ctx.touch_pointer,
            touch,
            handled,
            taken_by_client,
            fb,
        );
        for pointer_event in pointer_events {
            dispatch_synth_event(&pointer_event, fb, ctx, state);
        }
    };
    let frame = |ownership: &mut DirectTouchOwnership,
                 state: &mut Option<&mut State>,
                 fb: &mut FrameBuilder| {
        if ownership.take_client_frame() {
            forward_test_control_touch(state, fb, BackendTouchEvent::Frame);
        }
    };

    match phase {
        TouchPhaseKind::Down => match ctx.direct_touch.down(key, (x, y)) {
            DirectTouchDown::ForwardToClient(id) => {
                // The client half first, as on DRM: the raw half then knows
                // whether a touch-capable client took the contact.
                forward_test_control_touch(state, fb, BackendTouchEvent::Down { slot: id, x, y });
                raw(id, x, y, TouchPhase::Started, fb, ctx, state);
            }
            DirectTouchDown::CancelClientForShell(id) => {
                if let Some(state) = state.as_deref_mut() {
                    state.note_touch_down_withheld_from_clients();
                }
                raw(id, x, y, TouchPhase::Started, fb, ctx, state);
                forward_test_control_touch(state, fb, BackendTouchEvent::Cancel);
                frame(&mut ctx.direct_touch, state, fb);
            }
            DirectTouchDown::ShellOwned(id) => {
                if let Some(state) = state.as_deref_mut() {
                    state.note_touch_down_withheld_from_clients();
                }
                raw(id, x, y, TouchPhase::Started, fb, ctx, state);
            }
        },
        TouchPhaseKind::Motion => {
            if let Some((id, to_client)) = ctx.direct_touch.motion(&key, (x, y)) {
                raw(id, x, y, TouchPhase::Moved, fb, ctx, state);
                if to_client {
                    forward_test_control_touch(
                        state,
                        fb,
                        BackendTouchEvent::Motion { slot: id, x, y },
                    );
                }
            }
        }
        TouchPhaseKind::Up => {
            if let Some(((last_x, last_y), id, to_client)) = ctx.direct_touch.up(&key) {
                raw(id, last_x, last_y, TouchPhase::Ended, fb, ctx, state);
                if to_client {
                    forward_test_control_touch(state, fb, BackendTouchEvent::Up { slot: id });
                }
            }
        }
        TouchPhaseKind::Cancel => {
            let forward_cancel = ctx.direct_touch.client_stream_active();
            for (id, (last_x, last_y)) in ctx.direct_touch.cancel() {
                raw(id, last_x, last_y, TouchPhase::Cancelled, fb, ctx, state);
            }
            if forward_cancel {
                forward_test_control_touch(state, fb, BackendTouchEvent::Cancel);
            }
            frame(&mut ctx.direct_touch, state, fb);
        }
        TouchPhaseKind::Frame => frame(&mut ctx.direct_touch, state, fb),
    }
}

/// Re-resolve and run one `InvokeAction` — split out of
/// [`execute_planned_action`] because catalog resolution, arg binding and
/// invoke-failure handling alone were half that function's line budget.
fn execute_invoke_action(
    provider: &str,
    id: &str,
    args: &serde_json::Map<String, serde_json::Value>,
    fb: &FrameBuilder,
    ctx: &mut TestControlCtx,
) -> Result<crate::shell::search::InvocationDisposition, InvokeFailure> {
    let chrome_handle = fb.chrome_handle();
    let resource = ResourceRef::new(provider, id);
    // This request's own dispatch resolved the address just now — through the
    // same gate, against the same document — and nothing has run since, so
    // that resolution is the current action. Anything else resolves afresh:
    // the document is mutable between requests, so we run the current action
    // or nothing.
    let resolved = ctx
        .resolved_invocation
        .get_mut()
        .take()
        .filter(|resolved| resolved.resource == resource);
    let catalog = match resolved {
        Some(resolved) => resolved.catalog,
        // Built through the same gate the dispatcher resolved against — without
        // it, a revoked capability wouldn't stop a caller who got past it once.
        None => match with_action_gate(&chrome_handle, |chrome, gate| {
            let first_use_gate = super::admin_control::FirstUseVisibility {
                base: gate,
                first_use: fb.host().onboarding_active(),
            };
            fb.host().action_catalog_for_resource(
                &resource,
                chrome,
                &ctx.doc,
                &ctx.root_obj,
                &first_use_gate,
            )
        }) {
            Ok(catalog) => catalog,
            Err(why) => {
                eprintln!("tilo compositor: cannot invoke ({provider}, {id}): {why}");
                return Err(InvokeFailure {
                    code: "action_failed".into(),
                    message: why,
                });
            }
        },
    };
    // The `Ref` above is dropped with the block, so the provider's callback
    // is free to borrow the chrome cell mutably — which most of them do.
    if let Some(selected) = catalog.select(&resource) {
        // Re-bound against *this* resolution's parameters — the catalogue was just
        // rebuilt and may have landed on a different action under the same address.
        let bound = selected
            .invoke_json_disposition(
                &mut ui::widget::Edits::lend(&mut ctx.doc),
                &ctx.root_obj,
                &chrome_handle,
                args,
            )
            .map_err(|e| InvokeFailure {
                code: e.code().to_string(),
                message: e.to_string(),
            });
        match bound {
            Ok(receipt) => Ok(receipt),
            Err(failure) => {
                eprintln!(
                    "tilo compositor: ({provider}, {id}) did not run: {}",
                    failure.message
                );
                Err(failure)
            }
        }
    } else {
        use crate::shell::search::providers::live_app_actions;
        let message = if provider == live_app_actions::PROVIDER_ID
            && live_app_actions::job_control_target(id).is_some()
        {
            live_app_actions::JOB_UNAVAILABLE_MESSAGE.to_string()
        } else {
            format!(
                "action ({provider}, {id}) vanished between resolve and invoke; nothing was run"
            )
        };
        eprintln!("tilo compositor: {message}");
        Err(InvokeFailure {
            code: "action_failed".to_string(),
            message,
        })
    }
}

/// Synthesise the [`WindowEvent`] for one [`PlannedAction`] and dispatch it
/// through the `FrameBuilder`'s shared `dispatch_event` wire — the same wire
/// `ShellDriver::dispatch` uses in-process. `device_id` is zeroed since
/// winit's `DeviceId` isn't inspected by the host.
///
/// `state` is the live compositor state when one exists (`None` in unit
/// tests below); when present, the event first goes through [`client_input::route_input`], like the live `BackendEvent::Input` arm.
#[allow(
    clippy::too_many_lines,
    reason = "Test-control actions intentionally share the live frame builder and state transaction; splitting their dispatcher would obscure which actions are valid in one control request."
)]
fn execute_planned_action(
    action: &PlannedAction,
    fb: &mut FrameBuilder,
    ctx: &mut TestControlCtx,
    state: Option<&mut State>,
) -> Option<Result<crate::shell::search::InvocationDisposition, InvokeFailure>> {
    let mut state = state;
    ctx.note_injected_input(action, fb);
    match action {
        PlannedAction::CursorMoved { x, y } => {
            ctx.cursor = PhysicalPosition::new(*x, *y);
            let event = WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: ctx.cursor,
            };
            dispatch_synth_event(&event, fb, ctx, &mut state);
        }
        PlannedAction::SetGroupWidth { permille } => {
            let touched = fb.host_mut().set_group_widths(*permille);
            tracing::info!(
                "test_control: set {} group(s) to {permille}permille",
                touched.len()
            );
            fb.with_chrome(super::super::shell::chrome_state::ChromeState::bump);
        }
        PlannedAction::SeedFilesDrag { paths } => {
            file_explorer::external_drag::begin(paths.clone());
        }
        PlannedAction::MouseLeftButton { pressed } => {
            let event = WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: if *pressed {
                    ElementState::Pressed
                } else {
                    ElementState::Released
                },
                button: MouseButton::Left,
            };
            dispatch_synth_event(&event, fb, ctx, &mut state);
        }
        PlannedAction::MousePointerButton { pressed, button } => {
            let event = WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: if *pressed {
                    ElementState::Pressed
                } else {
                    ElementState::Released
                },
                button: match button {
                    PointerButtonKind::Left => MouseButton::Left,
                    PointerButtonKind::Right => MouseButton::Right,
                    PointerButtonKind::Middle => MouseButton::Middle,
                },
            };
            dispatch_synth_event(&event, fb, ctx, &mut state);
        }
        PlannedAction::Touch {
            device,
            slot,
            phase,
            x,
            y,
        } => dispatch_test_control_touch(device, *slot, *phase, *x, *y, fb, ctx, &mut state),
        PlannedAction::Pen {
            phase,
            x,
            y,
            pressure,
        } => {
            let input = virtual_pen(*phase, (*x, *y), *pressure);
            ctx.cursor = PhysicalPosition::new(*x, *y);
            let fallback = match state.as_deref_mut() {
                Some(st) => client_input::route_tablet_event(st, fb, &input, false),
                None => crate::compositor::tablet::tablet_pointer_events(&input)
                    .into_iter()
                    .map(BackendEvent::Input)
                    .collect(),
            };
            for event in fallback {
                if let BackendEvent::Input(event) = event {
                    dispatch_synth_event(&event, fb, ctx, &mut state);
                }
            }
        }
        PlannedAction::Touchpad(edge) => {
            if let Some(gesture) = feed_touchpad_edge(&mut ctx.gestures, *edge) {
                apply_shell_gesture(fb, gesture, Instant::now());
            }
        }
        PlannedAction::KeyboardKey { key, pressed } => {
            let winit_key = keytag_into_winit_key(*key);
            let event = keyboard_input_event(&winit_key, *pressed);
            // Keep the running mask in step with the injected key stream — this wire
            // carries no `ModifiersChanged`, so a held modifier would otherwise be invisible.
            if let WindowEvent::KeyboardInput { event: key_ev, .. } = &event {
                client_input::apply_modifier_key(&mut ctx.modifiers, key_ev);
            }
            dispatch_synth_event(&event, fb, ctx, &mut state);
        }
        PlannedAction::SeatKey { key, pressed } => {
            ctx.seat_events
                .push(BackendEvent::Input(keyboard_input_event(
                    &keytag_into_winit_key(*key),
                    *pressed,
                )));
        }
        PlannedAction::MouseWheel { dx, dy } => {
            let event = WindowEvent::MouseWheel {
                device_id: winit::event::DeviceId::dummy(),
                delta: winit::event::MouseScrollDelta::LineDelta(*dx, *dy),
                phase: winit::event::TouchPhase::Moved,
            };
            dispatch_synth_event(&event, fb, ctx, &mut state);
        }
        PlannedAction::SetModifiers {
            ctrl,
            shift,
            alt,
            logo,
        } => {
            let mut mask = ModifiersState::empty();
            if *ctrl {
                mask |= ModifiersState::CONTROL;
            }
            if *shift {
                mask |= ModifiersState::SHIFT;
            }
            if *alt {
                mask |= ModifiersState::ALT;
            }
            if *logo {
                mask |= ModifiersState::SUPER;
            }
            // Mutate the modifier snapshot directly — the host reads it on the next chord dispatch, no event needed.
            apply_modifier_mask(ctx, mask);
            // No key event carries this change, so a gesture held open by a
            // modifier (Alt+Tab) hears the release here.
            if fb.host_mut().note_modifiers(mask) {
                fb.host_mut().flush_active_tab_writeback();
            }
        }
        PlannedAction::Snapshot { path } => {
            // Re-render into a fresh OffscreenRenderer rather than tap a backend-cached
            // `last_frame`, so this works for every backend, not just headless.
            // Errors are logged via `eprintln!` and swallowed — the dispatcher already
            // returned `Ok`, and crashing on a stray bad path defeats the point of a
            // test surface. A snapshot must show a drag's carried item too, drawn by
            // the live present loop on top of the scene.
            if let Some(outcome) = defer_client_snapshot(ctx, fb, path, state.as_deref_mut()) {
                ctx.deferred_snapshot_reply = Some(outcome);
            } else {
                let drag = snapshot_drag(state.as_deref());
                let outcome = SnapshotOutcome::default();
                start_snapshot(fb, path, drag, ctx.snapshot_crop, &outcome);
                ctx.deferred_snapshot_reply = Some(outcome);
            }
        }
        PlannedAction::InvokeAction {
            provider,
            id,
            args,
            device,
        } => {
            if let Some(device) = device {
                return match fb.invoke_enrolled_device(device, provider, id, args) {
                    Ok(()) => None,
                    Err(message) => Some(Err(InvokeFailure {
                        code: "action_failed".into(),
                        message,
                    })),
                };
            }
            return Some(execute_invoke_action(provider, id, args, fb, ctx));
        }
    }
    None
}

/// One event of the pen an agent or a test draws with: a tablet of its own
/// whose pen reports pressure, and nothing else.
fn virtual_pen(
    phase: TabletPhase,
    position: (f64, f64),
    pressure: f64,
) -> crate::compositor::backend::TabletInput {
    use crate::compositor::backend::{TabletDevice, TabletInput, TabletTool, TabletToolKind};
    TabletInput {
        device: TabletDevice {
            name: "Tilo virtual pen".into(),
            usb_id: None,
            syspath: None,
        },
        tool: TabletTool {
            kind: TabletToolKind::Pen,
            serial: 0,
            wacom_id: 0,
            pressure: true,
            distance: false,
            tilt: false,
            rotation: false,
            slider: false,
            wheel: false,
        },
        phase,
        position,
        pressure: pressure.clamp(0.0, 1.0),
        distance: 0.0,
        tilt: (0.0, 0.0),
        rotation: 0.0,
        slider: 0.0,
        wheel: (0.0, 0),
        // Milliseconds on a clock of the pen's own, as a device's is.
        time_msec: {
            static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
            u32::try_from(START.get_or_init(Instant::now).elapsed().as_millis()).unwrap_or(u32::MAX)
        },
    }
}

/// Why a planned invocation did not happen. Carries the action layer's own
/// code: `invalid_arguments` for a bad fit, `action_failed` when it ran and refused.
struct InvokeFailure {
    code: String,
    message: String,
}

/// Route a test-control [`PlannedAction`] to the lock screen while locked,
/// instead of app/chrome — the socket is a real input source and must be
/// captured exactly like live winit input. Keyboard feeds the password field
/// via the same winit→`LockKey` routing; pointer/modifier/snapshot actions
/// are swallowed (a snapshot must not leak the hidden chrome). A submit
/// starts an async verify; `lock.poll_verification()` delivers the actual unlock.
fn execute_locked_action(
    action: &PlannedAction,
    lock: &mut LockController,
    state: &mut State,
    ctx: &mut TestControlCtx,
) -> Option<InvokeFailure> {
    match action {
        PlannedAction::KeyboardKey { key, pressed } => {
            let winit_key = keytag_into_winit_key(*key);
            let event = keyboard_input_event(&winit_key, *pressed);
            if let WindowEvent::KeyboardInput { event: key_ev, .. } = &event {
                client_input::apply_modifier_key(&mut ctx.modifiers, key_ev);
            }
            client_input::consume_keyboard_while_captured(state, &event);
            lock.handle_event(&event, &ctx.modifiers);
            None
        }
        // The loop routes it to the lock as it does a physical key.
        PlannedAction::SeatKey { key, pressed } => {
            ctx.seat_events
                .push(BackendEvent::Input(keyboard_input_event(
                    &keytag_into_winit_key(*key),
                    *pressed,
                )));
            None
        }
        // Action invocations and snapshots are refused while locked, and say
        // so. `InvokeAction` matters most here: the action API reaches chrome
        // side-effects with no visible surface, so letting it through would
        // make the lock screen bypassable by a socket client that renders
        // nothing; answering `ok` for an action that never ran would leave a
        // caller who cannot see the screen believing it did.
        PlannedAction::Snapshot { .. } | PlannedAction::InvokeAction { .. } => {
            Some(InvokeFailure {
                code: "session_locked".into(),
                message: test_control::SESSION_LOCKED.into(),
            })
        }
        // Pointer and modifier input is swallowed while locked.
        PlannedAction::CursorMoved { .. }
        | PlannedAction::MouseLeftButton { .. }
        | PlannedAction::MousePointerButton { .. }
        | PlannedAction::Touch { .. }
        | PlannedAction::Pen { .. }
        | PlannedAction::Touchpad(_)
        | PlannedAction::MouseWheel { .. }
        | PlannedAction::SetModifiers { .. }
        | PlannedAction::SetGroupWidth { .. }
        | PlannedAction::SeedFilesDrag { .. } => None,
    }
}

/// The snapshot renderer, cached for the lifetime of the process.
///
/// **Deliberately never dropped, not `thread_local!`.** Dropping wgpu
/// resources touches wgpu's own debug-assertions TLS, and TLS destructors
/// run in reverse registration order — a thread-local cache here could tear
/// down after wgpu's and drop a `Renderer` against the corpse, and a panic
/// in a TLS destructor is SIGABRT, taking all ~1800 `shell --lib` tests
/// down, not just the two that snapshot.
///
/// A `static` has no destructor, so it outlives every thread; the leak is
/// the point (GPU allocations the kernel reclaims at exit). A resize still
/// drops it normally mid-execution, while wgpu's TLS is alive. Nothing
/// reaches here in a shipped build (socket compiled out).
static SNAPSHOT_RENDERER: std::sync::OnceLock<
    std::sync::Mutex<Option<ui::offscreen::OffscreenRenderer>>,
> = std::sync::OnceLock::new();

/// How many `OffscreenRenderer`s [`SnapshotJob::write`] has built: one per
/// distinct viewport size, *not* per thread — what
/// `snapshot_renderer_outlives_the_thread_that_built_it` asserts, since the
/// abort a per-thread cache causes can't be observed from inside the binary it kills.
static SNAPSHOT_RENDERERS_BUILT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// The drag ornament a snapshot has to draw on top of the scene, if a
/// drag is in flight. Absent, the PNG is the scene alone.
fn snapshot_drag(
    state: Option<&State>,
) -> Option<(
    vello::kurbo::Point,
    crate::compositor::drag_feedback::DragVisual,
)> {
    state.and_then(|s| {
        (s.selection.dnd_active
            || s.selection.chrome_drag.is_some()
            || file_explorer::external_drag::unclaimed().is_some())
        .then(|| {
            let (cx, cy) = s.client_input.cursor();
            (
                vello::kurbo::Point::new(cx, cy),
                s.selection.dnd_label.snapshot(),
            )
        })
    })
}

/// Hold `path` until the clients in it answered the input injected since
/// the last screenshot. Returns where the deferred write reports its
/// outcome; `None` when there was no input since, or no client to wait for,
/// and the call site writes at once.
fn defer_client_snapshot(
    ctx: &mut TestControlCtx,
    fb: &mut FrameBuilder,
    path: &std::path::Path,
    state: Option<&mut State>,
) -> Option<SnapshotOutcome> {
    let state = state?;
    if !std::mem::take(&mut ctx.input_since_snapshot) {
        return None;
    }
    // A shot still waiting is the older one: it goes out now, as it is.
    if let Some(pending) = ctx.pending_snapshot.take() {
        let drag = snapshot_drag(Some(state));
        start_snapshot(fb, &pending.path, drag, pending.crop, &pending.outcome);
    }
    let surfaces = match ctx.snapshot_window {
        Some(window) => foreign::toplevel_surface(state, window)
            .into_iter()
            .collect(),
        None => foreign::shown_toplevel_surfaces(state),
    };
    if surfaces.is_empty() {
        return None;
    }
    let outcome = SnapshotOutcome::default();
    ctx.pending_snapshot = Some(PendingClientSnapshot {
        path: path.to_path_buf(),
        crop: ctx.snapshot_crop,
        settle: crate::compositor::client_settle::Settle::begin(state, &surfaces, Instant::now()),
        outcome: std::sync::Arc::clone(&outcome),
    });
    Some(outcome)
}

/// Write a deferred screenshot once its clients have answered, or the wait
/// has run out. A shot still waiting stays pending; pings it sends go out
/// with the loop's next client flush.
fn publish_settled_snapshot(ctx: &mut TestControlCtx, fb: &mut FrameBuilder, state: &mut State) {
    use crate::compositor::client_settle::Progress;
    let Some(pending) = ctx.pending_snapshot.as_mut() else {
        return;
    };
    match pending.settle.advance(state, Instant::now()) {
        Progress::Waiting => return,
        Progress::Settled => {}
        Progress::TimedOut => tracing::warn!(
            target: "tilo::test_control",
            limit_ms = crate::compositor::client_settle::SETTLE_LIMIT.as_millis(),
            "screenshot after input taken while a client was still answering it"
        ),
    }
    let Some(pending) = ctx.pending_snapshot.take() else {
        return;
    };
    let drag = snapshot_drag(Some(state));
    start_snapshot(fb, &pending.path, drag, pending.crop, &pending.outcome);
}

/// Take a screenshot to `path`: the scene is drawn here, on the loop (it
/// is the loop's state), and rendered, encoded and written on a worker
/// that reports to `outcome` and wakes the loop. Rendering and encoding a
/// 2944x1840 PNG took the live desktop's loop hundreds of milliseconds,
/// during which no key and no frame got through.
fn start_snapshot(
    fb: &mut FrameBuilder,
    path: &std::path::Path,
    drag: Option<(
        vello::kurbo::Point,
        crate::compositor::drag_feedback::DragVisual,
    )>,
    crop: Option<vello::kurbo::Rect>,
    outcome: &SnapshotOutcome,
) {
    let job = match prepare_snapshot(fb, path, drag, crop) {
        Ok(job) => job,
        Err(message) => {
            *outcome.lock_recover() = Some(Err(message));
            return;
        }
    };
    let report = std::sync::Arc::clone(outcome);
    let spawned = std::thread::Builder::new()
        .name("tilo-snapshot".into())
        .spawn(move || {
            let written = job.write();
            *report.lock_recover() = Some(written);
            crate::shell::wake_compositor();
        });
    if let Err(error) = spawned {
        *outcome.lock_recover() =
            Some(Err(format!("could not start the snapshot writer: {error}")));
    }
}

/// A screenshot drawn on the loop, to be rendered and written off it.
struct SnapshotJob {
    scene: vello::Scene,
    size: (u32, u32),
    crop: Option<vello::kurbo::Rect>,
    path: std::path::PathBuf,
}

/// The loop's part of a screenshot: the settled scene, as a capture sees it.
fn prepare_snapshot(
    fb: &mut FrameBuilder,
    path: &std::path::Path,
    drag: Option<(
        vello::kurbo::Point,
        crate::compositor::drag_feedback::DragVisual,
    )>,
    crop: Option<vello::kurbo::Rect>,
) -> Result<SnapshotJob, String> {
    let (w, h) = fb.scene_size();
    // Scene dimensions are always non-negative pixel counts; clamped rather
    // than a bare `as u32` so a pathological value saturates instead of wrapping.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to [0.0, u32::MAX] just above, and .round()-ed first, so the cast to \
                  u32 neither truncates a fraction nor loses a sign"
    )]
    let (w, h) = (
        w.round().clamp(0.0, f64::from(u32::MAX)) as u32,
        h.round().clamp(0.0, f64::from(u32::MAX)) as u32,
    );
    if w == 0 || h == 0 {
        return Err(format!("the viewport is {w}x{h}, nothing to write"));
    }
    // A test-control snapshot may be the first paint after an input event.
    // Retained overlays retarget their interruptible transitions during
    // drawing, so a wall-clock sleep alone cannot settle them: without this
    // hook the capture itself starts a close transition and records the stale
    // open panel. This is capture-only; live rendering retains its animation.
    fb.host_mut().force_palette_settled();
    let mut scene = fb.paint_settled();
    if let Some((cursor, visual)) = drag {
        let theme = fb.theme().clone();
        let mut feedback = vello::Scene::new();
        drag_feedback::draw(
            &mut feedback,
            cursor,
            &visual,
            &theme,
            fb.text_renderer_mut(),
        );
        fb.compose_layout_scene(&feedback, &mut scene);
    }
    // The snapshot leaves the display — to a script, a test or an agent — so
    // it sees protected windows and secret prompts as a capture does.
    let scene = fb.capture_scene(&scene).into_owned();
    Ok(SnapshotJob {
        scene,
        size: (w, h),
        crop,
        path: path.to_path_buf(),
    })
}

impl SnapshotJob {
    /// Render, crop, encode and write the shot. Runs on any thread: the
    /// renderer is the process-wide [`SNAPSHOT_RENDERER`].
    fn write(self) -> Result<(), String> {
        let Self {
            scene,
            size: (w, h),
            crop,
            path,
        } = self;
        // The renderer is kept between snapshots, rebuilt only on viewport resize.
        //
        // `OffscreenRenderer::try_new` initialises every vello shader pipeline, so
        // building one per call put a full pipeline build inside every snapshot:
        // measured at 1920x1080, ~450ms/frame vs ~70ms actual render+encode.
        // See [`SNAPSHOT_RENDERER`] for why the cache is a `static`, not `thread_local!`.

        // `try_new`, not `new` inside `catch_unwind`: in a release build a panic
        // outside a `tilo_rt::panic_boundary` aborts, so a caught panic can't
        // happen — the `Result` is what
        // keeps the compositor alive with no wgpu adapter (CI without llvmpipe).
        // Failure isn't cached: the slot stays empty so a later snapshot retries.
        let cache = SNAPSHOT_RENDERER.get_or_init(|| std::sync::Mutex::new(None));
        // The mutex only needs to guard the renderer slot; encoding the PNG and
        // writing it to disk below are unrelated work and don't need it held, so
        // the guard's scope ends here once `rgba` is copied out.
        let rgba = {
            // A panic elsewhere poisons the mutex without damaging what it guards
            // (an intact renderer or empty slot) — recover rather than compound the failure.
            let mut slot = cache.lock_recover();
            let stale = match slot.as_ref() {
                Some(renderer) => renderer.width != w || renderer.height != h,
                None => true,
            };
            if stale {
                match ui::offscreen::OffscreenRenderer::try_new(w, h) {
                    Ok(renderer) => {
                        SNAPSHOT_RENDERERS_BUILT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        // Assigning drops any previous renderer here, mid-execution — the one place it's safe to.
                        *slot = Some(renderer);
                    }
                    Err(e) => {
                        *slot = None;
                        return Err(format!("no offscreen renderer: {e}"));
                    }
                }
            }
            // A shot is one render with no next frame to repair it, so it waits
            // out Vello growing its buffers: one that did not fit drops the paths
            // encoded last, and the scene draws every display in turn, so the
            // second display came back empty while the first looked whole.
            let Some(rendered) = slot.as_mut().map(|renderer| {
                renderer.try_render_resilient(&scene, vello::peniko::Color::TRANSPARENT)
            }) else {
                return Err("no offscreen renderer".to_string());
            };
            let rgba = rendered?;
            // `rgba` is now an owned buffer with no tie to the renderer, so the
            // mutex guard can go before the encode/write work below.
            drop(slot);
            rgba
        };
        let (w, h, rgba) = match crop {
            Some(crop) => crop_rgba(&rgba, (w, h), crop),
            None => (w, h, rgba),
        };
        if w == 0 || h == 0 {
            return Err("the crop is empty, nothing to write".to_string());
        }
        let png = ui::png::encode_rgba8(w, h, &rgba).map_err(|e| format!("encode: {e}"))?;
        write_whole(&path, &png).map_err(|e| format!("write {}: {e}", path.display()))
    }
}

/// Write `bytes` to a sibling file and rename it over `path`, so a reader
/// that opens `path` sees the previous file or the whole new one, never a
/// PNG cut off mid-write.
fn write_whole(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let name = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} names no file", path.display()),
        )
    })?;
    let mut staging_name = std::ffi::OsString::from(".");
    staging_name.push(name);
    staging_name.push(format!(".{}.partial", std::process::id()));
    let staging = path.with_file_name(staging_name);
    let written = std::fs::write(&staging, bytes).and_then(|()| std::fs::rename(&staging, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&staging);
    }
    written
}

/// The part of a `size` RGBA8 frame inside `crop`, clamped to the frame.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "every edge is clamped to [0, frame size] and rounded before the cast"
)]
fn crop_rgba(rgba: &[u8], (w, h): (u32, u32), crop: vello::kurbo::Rect) -> (u32, u32, Vec<u8>) {
    let edge = |value: f64, max: u32| value.round().clamp(0.0, f64::from(max)) as u32;
    let (x0, x1) = (edge(crop.x0, w), edge(crop.x1, w));
    let (y0, y1) = (edge(crop.y0, h), edge(crop.y1, h));
    let (cw, ch) = (x1.saturating_sub(x0), y1.saturating_sub(y0));
    let stride = (w as usize).saturating_mul(4).max(1);
    let start = (x0 as usize).saturating_mul(4);
    let end = start.saturating_add((cw as usize).saturating_mul(4));
    let out = rgba
        .chunks_exact(stride)
        .skip(y0 as usize)
        .take(ch as usize)
        .filter_map(|row| row.get(start..end))
        .flatten()
        .copied()
        .collect();
    (cw, ch, out)
}

/// Make `response` the connection's next frame and write as much of it as
/// the socket takes now.
fn queue_test_control_response(
    conn: &mut TestControlConn,
    response: &test_control::Response,
) -> PendingConnectionProgress {
    debug_assert!(
        conn.output.is_empty(),
        "a connection must finish its prior response before queueing another"
    );
    let mut out = serialise_response(response);
    out.push('\n');
    conn.output = out.into_bytes();
    conn.output_offset = 0;
    flush_test_control_output(conn)
}

fn flush_test_control_output(conn: &mut TestControlConn) -> PendingConnectionProgress {
    while conn.output_offset < conn.output.len() {
        #[expect(
            clippy::indexing_slicing,
            reason = "the loop condition keeps output_offset below output.len()"
        )]
        let pending = &conn.output[conn.output_offset..];
        match conn.reader.get_mut().write(pending) {
            Ok(0) => {
                eprintln!("tilo compositor: test_control write: disconnected");
                conn.closed = true;
                return PendingConnectionProgress::Closed;
            }
            Ok(written) => conn.output_offset = conn.output_offset.saturating_add(written),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                return PendingConnectionProgress::OutputPending;
            }
            Err(error) => {
                eprintln!("tilo compositor: test_control write: {error}");
                conn.closed = true;
                return PendingConnectionProgress::Closed;
            }
        }
    }
    if conn.output.is_empty() {
        PendingConnectionProgress::Idle
    } else {
        conn.output.clear();
        conn.output_offset = 0;
        PendingConnectionProgress::WroteResponse
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingConnectionProgress {
    Idle,
    Waiting,
    OutputPending,
    WroteResponse,
    Closed,
}

/// Finish what `conn` holds back before it reads another request: an
/// unflushed response, a snapshot being written, an invocation not answered
/// yet. A waiting invocation reads its job's progress through `jobs`.
fn pump_pending_test_control_conn(
    conn: &mut TestControlConn,
    jobs: &mut dyn InvocationProgress,
) -> PendingConnectionProgress {
    match flush_test_control_output(conn) {
        PendingConnectionProgress::Idle => {}
        progress => return progress,
    }
    if let Some((outcome, response)) = conn.pending_snapshot.take() {
        let written = outcome.lock_recover().take();
        return match written {
            None => {
                conn.pending_snapshot = Some((outcome, response));
                PendingConnectionProgress::Waiting
            }
            Some(Ok(())) => queue_test_control_response(conn, &response),
            Some(Err(message)) => queue_test_control_response(
                conn,
                &test_control::Response::Err {
                    code: "snapshot_failed".into(),
                    message,
                },
            ),
        };
    }
    if conn.pending_listing.is_some() {
        // Held until the action surface moves or its wait runs out; only
        // `pump_test_control_conn`, which reads the listing, runs it again.
        if conn.peer_disconnected() {
            conn.closed = true;
            return PendingConnectionProgress::Closed;
        }
        return PendingConnectionProgress::Waiting;
    }
    let Some(pending) = conn.pending_invocation.take() else {
        return PendingConnectionProgress::Idle;
    };
    if conn.peer_disconnected() {
        conn.closed = true;
        return PendingConnectionProgress::Closed;
    }
    match pending.poll(jobs) {
        ControlInvocationPoll::Waiting(pending) => {
            conn.pending_invocation = Some(pending);
            PendingConnectionProgress::Waiting
        }
        ControlInvocationPoll::Ready(response) => queue_test_control_response(conn, &response),
    }
}

/// The agent a command on the agent-control socket acts for when it names
/// none: a single-agent client keeps working without knowing about seats.
const DEFAULT_AGENT: &str = "agent";

/// The agent one control command runs for, once it holds the shared seat.
struct AgentScope {
    agent: String,
    peer: crate::network::protocol::PeerId,
    strip: crate::shell::state::StripId,
    /// What the agent sees ([`agent_viewport`], or the window it aimed at):
    /// its coordinates' origin and its screenshots' extent.
    viewport: Rect,
    /// The app id of the window the command aimed at, when it named one.
    window: Option<String>,
    /// That window's surface.
    surface: Option<foreign::SurfaceId>,
}

/// Where one app's window lives in the workspace tree.
struct WindowTarget {
    workspace: crate::shell::state::WorkspaceId,
    strip: crate::shell::state::StripId,
    group: crate::shell::state::GroupId,
    tab: u64,
    surface: foreign::SurfaceId,
}

/// The window of `app_id` that `agent` may act in: one in its own strip
/// first, else one in a strip no other agent works in. Refusals name what
/// the agent can aim at instead.
#[allow(
    clippy::result_large_err,
    reason = "the refusal is the wire Response the transport writes back as is, like parse_request's"
)]
fn agent_window(
    fb: &FrameBuilder,
    agents: &crate::compositor::agent_seats::AgentSeats,
    agent: &str,
    app_id: &str,
) -> Result<WindowTarget, test_control::Response> {
    let own = agents.seat(agent).and_then(|seat| seat.strip);
    let mut open = Vec::new();
    let mut busy = false;
    let mut found: Option<WindowTarget> = None;
    {
        let chrome = fb.chrome();
        let tree = chrome.tree();
        let Some(ws) = tree.desktop.active_workspace() else {
            return Err(agent_refusal("no_workspace", "no active workspace"));
        };
        for strip in &ws.strips {
            let owner = agents.strip_owner(strip.id).filter(|owner| *owner != agent);
            for group in &strip.window_groups {
                for tab in &group.tabs {
                    let crate::shell::state::TileContent::Foreign {
                        app_id: tab_app,
                        surface_id,
                    } = &tab.tile
                    else {
                        continue;
                    };
                    if tab_app != app_id {
                        open.push(tab_app.clone());
                        continue;
                    }
                    if owner.is_some() {
                        busy = true;
                        continue;
                    }
                    let better = found
                        .as_ref()
                        .is_none_or(|current| own == Some(strip.id) && own != Some(current.strip));
                    if better {
                        found = Some(WindowTarget {
                            workspace: ws.id,
                            strip: strip.id,
                            group: group.id,
                            tab: tab.id,
                            surface: *surface_id,
                        });
                    }
                }
            }
        }
        drop(tree);
        drop(chrome);
    }
    found.ok_or_else(|| {
        if busy {
            agent_refusal(
                "window_busy",
                &format!("another agent works in the strip holding {app_id}'s window"),
            )
        } else {
            open.sort();
            open.dedup();
            agent_refusal(
                "unknown_window",
                &format!(
                    "no window of {app_id} is open; open windows: [{}] (or none, for the whole screen)",
                    open.join(", ")
                ),
            )
        }
    })
}

/// The rect `target`'s window is drawn into, on screen.
fn window_viewport(fb: &FrameBuilder, target: &WindowTarget) -> Option<Rect> {
    let screen = agent_viewport(fb)?;
    let rect = foreign::store()
        .placement(target.surface)?
        .intersect(screen);
    (rect.width() >= 1.0 && rect.height() >= 1.0).then_some(rect)
}

impl AgentScope {
    fn origin(&self) -> (f64, f64) {
        (self.viewport.x0, self.viewport.y0)
    }
}

fn agent_refusal(code: &str, message: &str) -> test_control::Response {
    test_control::Response::Err {
        code: code.to_string(),
        message: message.to_string(),
    }
}

/// Run `actions` on the shared seat as `peer`'s input.
fn run_as_participant(
    peer: crate::network::protocol::PeerId,
    actions: &[PlannedAction],
    fb: &mut FrameBuilder,
    ctx: &mut TestControlCtx,
    state: &mut State,
) {
    if actions.is_empty() {
        return;
    }
    state.client_input.set_participant(Some(peer));
    for action in actions {
        let _ = execute_planned_action(action, fb, ctx, Some(state));
    }
    state.client_input.set_participant(None);
}

/// Give `agent` the shared seat before its command runs: find or make its
/// strip, release what the previous agent holds to that agent's window, focus
/// the strip (keyboard focus follows its active tab, and a launch lands in it)
/// and press again what `agent` itself holds. `Ok(None)` when the command was
/// `agent_leave`, which this completes.
#[allow(
    clippy::result_large_err,
    reason = "the refusal is the wire Response the transport writes back as is, like parse_request's"
)]
fn seat_agent(
    agent: &test_control::Agent,
    window: Option<&str>,
    command: &test_control::Command,
    fb: &mut FrameBuilder,
    ctx: &mut TestControlCtx,
    state: &mut State,
) -> Result<Option<AgentScope>, test_control::Response> {
    if matches!(command, test_control::Command::AgentLeave) {
        leave_agent(&agent.id, fb, ctx, state);
        return Ok(None);
    }
    let label = agent.label.clone().unwrap_or_else(|| agent.id.clone());
    let target = window
        .map(|app_id| agent_window(fb, &ctx.agents, &agent.id, app_id))
        .transpose()?;
    let mut strip = None;
    {
        let agents = &ctx.agents;
        let claimed = |id| {
            agents
                .strip_owner(id)
                .is_some_and(|owner| owner != agent.id)
        };
        let mut current = agents.seat(&agent.id).and_then(|seat| seat.strip);
        // The seated agent moved to another strip nobody else works in: that
        // strip is its own now, so its next command does not pull it back.
        if agents.seated() == Some(agent.id.as_str())
            && let Some(active) = fb.chrome().active_strip_id()
            && !claimed(active)
        {
            current = Some(active);
        }
        // Aiming at a window makes the strip holding it the agent's own.
        if let Some(target) = &target {
            current = Some(target.strip);
        }
        fb.with_chrome(|chrome| {
            // An agent seeing the whole screen sees its strip alone. One aimed
            // at a window sees just that window, which keeps the size it has,
            // so its app is not resized under it by the agent's arrival.
            if target.is_none() {
                chrome.set_single_strip(true);
            }
            strip = chrome.agent_strip(current, claimed, &label);
        });
    }
    let strip = strip.ok_or_else(|| agent_refusal("no_workspace", "no active workspace"))?;
    let peer = {
        let seat = ctx.agents.seat_mut(&agent.id, &label);
        seat.strip = Some(strip);
        seat.peer
    };
    fb.relayout();
    let screen = agent_viewport(fb)
        .ok_or_else(|| agent_refusal("no_screen", "the compositor has no screen yet"))?;
    let mut viewport = target
        .as_ref()
        .and_then(|target| window_viewport(fb, target))
        .unwrap_or(screen);
    let handover = ctx.agents.hand_over(
        &agent.id,
        &label,
        ctx.modifiers.state(),
        (viewport.x0, viewport.y0),
    );
    if let Some((previous, releases)) = &handover.release {
        run_as_participant(*previous, releases, fb, ctx, state);
    }
    let mut focused = false;
    fb.with_chrome(|chrome| {
        focused = match &target {
            Some(target) if !window_focused(chrome, target) => {
                chrome.focus_tab(target.workspace, target.strip, target.group, target.tab);
                true
            }
            Some(_) => false,
            None => chrome.focus_strip(strip),
        };
    });
    if focused {
        fb.relayout();
        fb.reconcile_native_focus(&mut ctx.focus);
    }
    if let Some(target) = &target
        && let Some(screen) = agent_viewport(fb)
        && foreign::store()
            .placement(target.surface)
            .is_some_and(|rect| rect.intersect(screen) != rect)
        && fb.host_mut().reveal_group_now(target.strip, target.group)
    {
        // The window runs off the screen: bring all of it into view.
        fb.relayout();
    }
    if let Some(target) = &target {
        viewport = window_viewport(fb, target).ok_or_else(|| {
            agent_refusal(
                "window_not_shown",
                "the window is not on screen yet; it may still be opening, try again",
            )
        })?;
    }
    client_input::sync_keyboard_focus(state, fb);
    run_as_participant(peer, &handover.restore, fb, ctx, state);
    if let Some(modifiers) = handover.modifiers {
        apply_modifier_mask(ctx, modifiers);
    }
    Ok(Some(AgentScope {
        agent: agent.id.clone(),
        peer,
        strip,
        viewport,
        window: window.map(str::to_owned),
        surface: target.as_ref().map(|target| target.surface),
    }))
}

/// Whether `target`'s tab is the one shown and focused already.
fn window_focused(chrome: &crate::shell::chrome_state::ChromeState, target: &WindowTarget) -> bool {
    let tree = chrome.tree();
    let desktop = &tree.desktop;
    let focused = desktop.active == target.workspace
        && desktop.active_strip.get(&target.workspace) == Some(&target.strip)
        && desktop.active_group.get(&target.strip) == Some(&target.group)
        && desktop
            .active_workspace()
            .and_then(|ws| ws.strips.iter().find(|s| s.id == target.strip))
            .and_then(|s| s.window_groups.iter().find(|g| g.id == target.group))
            .is_some_and(|g| g.active_tab == target.tab);
    drop(tree);
    focused
}

/// Where an agent sees and acts: the whole screen. While agents drive the
/// compositor only the focused strip is presented, filling the work area, and
/// the seat focuses the agent's own strip on its turn; the launcher, palette,
/// menus and dialogs its actions open are drawn over the whole screen, so
/// anything narrower would hide them from it.
fn agent_viewport(fb: &FrameBuilder) -> Option<Rect> {
    let (w, h) = fb.scene_size();
    (w > 0.0 && h > 0.0).then(|| Rect::new(0.0, 0.0, w, h))
}

/// Retire `agent`: its window sees the release of everything it still holds,
/// the shared-seat ledger forgets it, and its strip goes if it holds no window.
fn leave_agent(agent: &str, fb: &mut FrameBuilder, ctx: &mut TestControlCtx, state: &mut State) {
    let Some((seat, releases)) = ctx.agents.leave(agent) else {
        return;
    };
    run_as_participant(seat.peer, &releases, fb, ctx, state);
    state.remote_seats.drop_peer(seat.peer);
    let last = ctx.agents.is_empty();
    let mut changed = false;
    fb.with_chrome(|chrome| {
        if let Some(strip) = seat.strip {
            changed = chrome.remove_strip_if_empty(strip);
        }
        if last && chrome.single_strip() {
            chrome.set_single_strip(false);
            changed = true;
        }
    });
    if changed {
        fb.relayout();
        client_input::sync_keyboard_focus(state, fb);
    }
}

/// Dispatch `command` for the seated agent in `scope`: its coordinates are
/// relative to its viewport, what it lists is what its viewport shows, and a
/// pointer it would move outside the viewport is refused.
fn dispatch_for_agent(
    command: test_control::Command,
    scope: &AgentScope,
    fb: &FrameBuilder,
    ctx: &mut TestControlCtx,
    state: &mut State,
    lock: &LockController,
) -> (test_control::Response, Vec<PlannedAction>) {
    use test_control::{Command, Response};
    let (ox, oy) = scope.origin();
    let command = match command {
        Command::AgentSeat => {
            let v = scope.viewport;
            return (
                Response::Answer(serde_json::json!({
                    "agent": scope.agent,
                    "strip": scope.strip.0,
                    "viewport": [v.x0, v.y0, v.width(), v.height()],
                    "window": scope.window,
                })),
                Vec::new(),
            );
        }
        Command::ClickAt { x, y } => Command::ClickAt {
            x: x + ox,
            y: y + oy,
        },
        Command::PointerMove { x, y } => Command::PointerMove {
            x: x + ox,
            y: y + oy,
        },
        Command::Drag {
            x_from,
            y_from,
            x_to,
            y_to,
            steps,
            pressure,
        } => Command::Drag {
            x_from: x_from + ox,
            y_from: y_from + oy,
            x_to: x_to + ox,
            y_to: y_to + oy,
            steps,
            pressure,
        },
        other => other,
    };
    let (response, actions) = dispatch_control_command(command, fb, ctx, state, lock);
    let viewport = scope.viewport;
    let inside =
        |x: f64, y: f64| x >= viewport.x0 && x < viewport.x1 && y >= viewport.y0 && y < viewport.y1;
    if actions.iter().any(|action| {
        matches!(
            action,
            PlannedAction::CursorMoved { x, y } | PlannedAction::Pen { x, y, .. }
                if !inside(*x, *y)
        )
    }) {
        let message = match &scope.window {
            Some(app_id) => format!(
                "the pointer would leave {app_id}'s window, which is {}x{} pixels from its top-left",
                viewport.width().round(),
                viewport.height().round()
            ),
            None => "the pointer would leave the screen".to_owned(),
        };
        return (agent_refusal("outside_viewport", &message), Vec::new());
    }
    let response = match response {
        Response::Targets(targets) => Response::Targets(
            targets
                .into_iter()
                .filter(|target| {
                    let [x, y, w, h] = target.rect;
                    inside(x + w / 2.0, y + h / 2.0)
                })
                .map(|mut target| {
                    target.rect[0] -= ox;
                    target.rect[1] -= oy;
                    target
                })
                .collect(),
        ),
        other => other,
    };
    (response, actions)
}

/// Record where the agent `peer`'s keyboard and pointer landed on its
/// shared-seat ledger entry, for attribution.
fn note_agent_focus(state: &mut State, peer: crate::network::protocol::PeerId) {
    let keyboard = state.client_input.keyboard_focus();
    let pointer = state.client_input.pointer_focus();
    let seat = state.remote_seats.seat_mut(peer);
    match keyboard {
        Some(surface) => {
            let _ = seat.enter(surface);
        }
        None => {
            let _ = seat.leave();
        }
    }
    match pointer {
        Some(surface) => {
            let _ = seat.pointer_enter(surface);
        }
        None => {
            let _ = seat.pointer_leave();
        }
    }
}

/// Plan one control command against the live frame, as the transport's
/// person or the agent already seated for it.
fn dispatch_control_command(
    command: test_control::Command,
    frame_builder: &FrameBuilder,
    ctx: &mut TestControlCtx,
    state: &mut State,
    lock: &LockController,
) -> (test_control::Response, Vec<PlannedAction>) {
    match command {
        test_control::Command::Watch { watching } => {
            ctx.watch_request = Some(watching);
            (test_control::Response::Ok { frame: 0 }, Vec::new())
        }
        test_control::Command::MintHostLaunchActivation {
            desktop_id,
            display_name,
        } => {
            // Mirror the production host-palette transaction: it records the
            // destination against every desktop-entry window identity before
            // minting the optional activation token. Without this, a real X11
            // client which maps through StartupWMClass but does not bind
            // xdg-activation can never exercise the production placement path.
            let placement = crate::compositor::tab_binding::Placement::NewGroup;
            let identities = crate::shell::host_apps::find(&desktop_id)
                .map_or_else(|| vec![desktop_id.clone()], |app| app.window_identities);
            crate::shell::launcher::set_pending_placement_for_identities(
                &desktop_id,
                &identities,
                placement,
            );
            let token = state.issue_host_launch_activation(desktop_id, display_name, placement);
            (
                test_control::Response::Answer(serde_json::json!({
                    "activation_token": token,
                })),
                Vec::new(),
            )
        }
        test_control::Command::ProvisionTestIdentity => {
            (crate::compositor::dev_test_identity::respond(), Vec::new())
        }
        test_control::Command::DeadlockMainLoop => {
            if crate::compositor::dev_env::DEV_ENV_OVERRIDES {
                crate::compositor::watchdog::deadlock_this_thread_for_test();
            }
            (test_control::deadlock_main_loop_refused(), Vec::new())
        }
        test_control::Command::GpuFault { fault } => {
            (test_control::inject_gpu_fault(fault), Vec::new())
        }
        test_control::Command::DisplayBlank => {
            ctx.display_requests.push(DisplayRequest::Blank);
            (test_control::Response::Ok { frame: 0 }, Vec::new())
        }
        test_control::Command::DisplayWakeFault { stage } => {
            ctx.display_requests.push(DisplayRequest::WakeFault(stage));
            (test_control::Response::Ok { frame: 0 }, Vec::new())
        }
        cmd => {
            let state = FrameBuilderControlState {
                fb: frame_builder,
                doc: Some((&ctx.doc, &ctx.root_obj)),
                shares: Some(&state.share_registry),
                lock: Some(lock),
                seat: Some(state),
                resolved: Some(&ctx.resolved_invocation),
            };
            test_control::dispatch_command(&cmd, &state)
        }
    }
}

/// Drain pending bytes from `conn`'s buffer: for each complete `\n`-line,
/// parse a [`test_control::Command`], dispatch against `frame_builder`,
/// execute any resulting `PlannedAction`s, and write the JSON response back.
/// Partial lines stay buffered; EOF/error/write-failure marks the connection
/// closed for the outer loop to drop.
///
/// Cap on the per-line size: `read_line` buffers the whole line regardless
/// of size, so an unbounded no-newline stream could OOM the compositor. 64
/// KiB is well above any legitimate command and below meaningful memory pressure.
#[allow(
    clippy::too_many_lines,
    reason = "Connection pumping preserves request order, response framing, and frame-state mutation in one transaction to make test-control behavior deterministic."
)]
fn pump_test_control_conn(
    conn: &mut TestControlConn,
    frame_builder: &mut FrameBuilder,
    ctx: &mut TestControlCtx,
    session: &SessionManager,
    lock: &mut LockController,
    state: &mut State,
) -> bool {
    if conn.admin
        && let Some(identity_dir) = &conn.admin_identity_dir
    {
        let first_use = frame_builder.host().onboarding_active();
        if (first_use && session.is_locked())
            || super::admin_control::control_session_decision(identity_dir, first_use).is_err()
        {
            conn.closed = true;
            return false;
        }
    }
    // Whether any command produced a `PlannedAction` — synthesised input the
    // next frame must reflect; starts the input-latency measurement instrumentation uses.
    let mut injected_input = false;
    loop {
        // A request held for the person runs, or is refused, once they
        // answered; until then nothing after it is read.
        match scoped_control::settle_held(conn, frame_builder) {
            scoped_control::Held::Nothing => {}
            scoped_control::Held::Waiting => return injected_input,
            scoped_control::Held::Refused(refusal) => {
                match queue_test_control_response(conn, &refusal) {
                    PendingConnectionProgress::Closed
                    | PendingConnectionProgress::OutputPending => return injected_input,
                    _ => continue,
                }
            }
        }
        // A held listing runs again, before anything after it on this
        // connection is read, once the action surface moved or its wait ran
        // out.
        let stamp = listing_stamp(frame_builder);
        let mut rerun = conn
            .pending_listing
            .take_if(|held| held.due(Instant::now(), stamp));
        let mut jobs = FrameInvocationProgress {
            fb: &*frame_builder,
            ctx: &mut *ctx,
        };
        match pump_pending_test_control_conn(conn, &mut jobs) {
            PendingConnectionProgress::Waiting
            | PendingConnectionProgress::OutputPending
            | PendingConnectionProgress::Closed => {
                return injected_input;
            }
            PendingConnectionProgress::Idle | PendingConnectionProgress::WroteResponse => {}
        }
        let mut line = zeroize::Zeroizing::new(String::new());
        let read = match rerun.as_ref() {
            Some(held) => {
                line.push_str(&held.line);
                Ok(Some(()))
            }
            None => match conn.replay.take() {
                Some(first) => {
                    line.push_str(&first);
                    Ok(Some(()))
                }
                None => conn
                    .input
                    .read(&mut conn.reader, &mut line, TEST_CONTROL_MAX_LINE_BYTES),
            },
        };
        match read {
            Ok(None) => {
                conn.closed = true;
                return injected_input;
            }
            Ok(Some(())) => {
                let handling_started = Instant::now();
                // A listing that waits: armed, and the action surface read,
                // before the listing is — a change landing while it is read
                // still counts.
                let held_by = rerun.take().map(|held| held.settle_by);
                let mut listing_wait: Option<ListingWait> = None;
                // Only this request's own dispatch may hand its execution a resolution.
                *ctx.resolved_invocation.get_mut() = None;
                let mut scope: Option<AgentScope> = None;
                let mut diagnostic_query = None;
                // How long an `actions_invoke` asked to wait for its invocation to settle.
                let mut settle_for: Option<Duration> = None;
                let locked = session.is_locked();
                let scoped_refusal =
                    match scoped_control::gate(conn, &line, frame_builder, ctx, locked) {
                        scoped_control::Gate::Held => continue,
                        scoped_control::Gate::Refuse(refusal) => Some(refusal),
                        scoped_control::Gate::Serve => None,
                    };
                let parsed = match (parse_request(&line), scoped_refusal) {
                    (Ok(_), Some(refusal)) => Err(*refusal),
                    (parsed, _) => parsed,
                };
                let (mut resp, actions) = match parsed {
                    Ok(request)
                        if conn.admin
                            && (request.agent.is_some()
                                || request.window.is_some()
                                || !request.command.admin_permitted()) =>
                    {
                        (
                            test_control::Response::Err {
                                code: "not_permitted".to_string(),
                                message: test_control::ADMIN_CONTROL_NOT_PERMITTED.to_string(),
                            },
                            Vec::new(),
                        )
                    }
                    Ok(request)
                        if conn.admin
                            && frame_builder.host().onboarding_active()
                            && !super::admin_control::first_use_permitted(&request.command) =>
                    {
                        (
                            test_control::Response::Err {
                                code: "not_permitted".into(),
                                message: test_control::ADMIN_CONTROL_FIRST_USE_ONLY.into(),
                            },
                            Vec::new(),
                        )
                    }
                    Ok(request) if conn.agent && !request.command.agent_permitted() => (
                        test_control::Response::Err {
                            code: "not_permitted".to_string(),
                            message: AGENT_CONTROL_NOT_PERMITTED.to_string(),
                        },
                        Vec::new(),
                    ),
                    Ok(mut request) => {
                        // Settle layout before anything reads a rect. The relayout after an action
                        // (below) covers a tree the *socket* changed, not one a **client** changed
                        // — a window map/close/resize rearranges the strip with no command sent,
                        // so a caller asking mid-frame would get a stale rect. So layout is
                        // settled per command, not per action: idempotent, and cheap relative to
                        // the round-trip that asked for it.
                        frame_builder.relayout();
                        frame_builder.reconcile_native_focus(&mut ctx.focus);
                        // Everything the agent socket carries acts for some agent.
                        let agent = request.agent.or_else(|| {
                            conn.agent.then(|| test_control::Agent {
                                id: DEFAULT_AGENT.to_string(),
                                label: None,
                            })
                        });
                        if conn.admin
                            && agent.is_none()
                            && let Some(response) = super::lock_actions::dispatch(
                                &mut request.command,
                                session.is_locked(),
                                lock,
                            )
                        {
                            if matches!(&response, test_control::Response::Answer(answer) if answer["authentication"] == "verifying")
                            {
                                injected_input = true;
                            }
                            let progress = queue_test_control_response(conn, &response);
                            if matches!(
                                progress,
                                PendingConnectionProgress::Closed
                                    | PendingConnectionProgress::OutputPending
                            ) {
                                return injected_input;
                            }
                            continue;
                        }
                        if conn.admin
                            && agent.is_none()
                            && !session.is_locked()
                            && !frame_builder.host().onboarding_active()
                        {
                            if let test_control::Command::ActionsEnumerate { query, .. } =
                                &request.command
                            {
                                diagnostic_query = Some(query.clone());
                            }
                            if let Some(response) =
                                super::diagnostic_actions::dispatch(&request.command)
                            {
                                let progress = queue_test_control_response(conn, &response);
                                if matches!(
                                    progress,
                                    PendingConnectionProgress::Closed
                                        | PendingConnectionProgress::OutputPending
                                ) {
                                    return injected_input;
                                }
                                continue;
                            }
                        }
                        if let Some(response) =
                            super::diagnostic_actions::owner_required(&request.command)
                        {
                            let progress = queue_test_control_response(conn, &response);
                            if matches!(
                                progress,
                                PendingConnectionProgress::Closed
                                    | PendingConnectionProgress::OutputPending
                            ) {
                                return injected_input;
                            }
                            continue;
                        }
                        settle_for = super::control_invocation::settle_window(&request.command);
                        listing_wait = ListingWait::begin(
                            &request.command,
                            held_by,
                            listing_stamp(frame_builder),
                            || frame_builder.host().next_listing_change(),
                        );
                        match agent {
                            Some(_)
                                if session.is_locked()
                                    && matches!(
                                        request.command,
                                        test_control::Command::AgentSeat
                                            | test_control::Command::AgentLeave
                                    ) =>
                            {
                                (
                                    agent_refusal("session_locked", "the session is locked"),
                                    Vec::new(),
                                )
                            }
                            // Locked, input belongs to the lock screen, whoever sends it.
                            Some(agent) if !session.is_locked() => {
                                match seat_agent(
                                    &agent,
                                    request.window.as_deref(),
                                    &request.command,
                                    frame_builder,
                                    ctx,
                                    state,
                                ) {
                                    Ok(Some(seated)) => {
                                        let reply = dispatch_for_agent(
                                            request.command,
                                            &seated,
                                            frame_builder,
                                            ctx,
                                            state,
                                            lock,
                                        );
                                        scope = Some(seated);
                                        reply
                                    }
                                    Ok(None) => {
                                        (test_control::Response::Ok { frame: 0 }, Vec::new())
                                    }
                                    Err(refusal) => (refusal, Vec::new()),
                                }
                            }
                            _ => dispatch_control_command(
                                request.command,
                                frame_builder,
                                ctx,
                                state,
                                lock,
                            ),
                        }
                    }
                    Err(err_resp) => (err_resp, Vec::new()),
                };
                if let Some(query) = diagnostic_query.as_deref() {
                    super::diagnostic_actions::append_catalogue(&mut resp, query);
                }
                scoped_control::filter_answer(conn, &mut resp, frame_builder);
                if let Some(scope) = &scope {
                    state.client_input.set_participant(Some(scope.peer));
                    ctx.snapshot_crop = Some(scope.viewport);
                    ctx.snapshot_window = scope.surface;
                }
                // A planned action that could not run replaces this command's answer.
                // Actions execute before the reply is written; first failure wins, the rest still run.
                let routed_before = ctx.client_routed_actions;
                let mut failed: Option<InvokeFailure> = None;
                let mut receipt: Option<crate::shell::search::ActionReceipt> = None;
                let mut pending: Option<crate::shell::search::PendingInvocation> = None;
                for action in &actions {
                    if session.is_locked() {
                        // While locked the socket is captured by the lock surface like live input
                        // — actions go to the password field; `poll_verification` delivers unlock.
                        if let Some(failure) = execute_locked_action(action, lock, state, ctx) {
                            failed.get_or_insert(failure);
                        }
                    } else if let Some(outcome) =
                        execute_planned_action(action, frame_builder, ctx, Some(state))
                    {
                        match outcome {
                            Ok(crate::shell::search::InvocationDisposition::Accepted(admitted)) => {
                                receipt = Some(admitted);
                            }
                            Ok(crate::shell::search::InvocationDisposition::Pending(ticket)) => {
                                pending = Some(ticket);
                            }
                            Err(failure) => {
                                failed.get_or_insert(failure);
                            }
                        }
                    }
                    if let Some(scope) = &scope {
                        ctx.agents.record(&scope.agent, action, scope.origin());
                    }
                }
                // A resolution nothing ran from is stale by the next request.
                *ctx.resolved_invocation.get_mut() = None;
                if let Some(scope) = &scope {
                    state.client_input.set_participant(None);
                    ctx.snapshot_crop = None;
                    ctx.snapshot_window = None;
                    note_agent_focus(state, scope.peer);
                }
                // Re-run layout so the next command/paint sees rects post-mutation — a
                // chrome state change can alter structural layout, leaving stale rects otherwise.
                if !actions.is_empty() {
                    // Input only the client took changes no shell pixels: the
                    // client's commit will ask for the frame that shows it.
                    // Repainting here as well put a full-screen frame ahead of
                    // it that a physical key never causes.
                    // Keys queued for the input funnel ask for a frame
                    // there, as a physical key would, or not.
                    let seat_keys = actions
                        .iter()
                        .filter(|action| matches!(action, PlannedAction::SeatKey { .. }))
                        .count();
                    let handled_here = actions.len().saturating_sub(seat_keys);
                    if ctx.client_routed_actions.saturating_sub(routed_before) < handled_here {
                        injected_input = true;
                    } else if handled_here > 0 {
                        ctx.client_input_only = true;
                    }
                    frame_builder.relayout();
                    frame_builder.reconcile_native_focus(&mut ctx.focus);
                }
                let action_response = match &resp {
                    test_control::Response::Answer(action) => action.clone(),
                    _ => serde_json::Value::Null,
                };
                let deferred_snapshot = ctx.deferred_snapshot_reply.take();
                if let Some(outcome) = deferred_snapshot
                    && failed.is_none()
                {
                    conn.pending_snapshot = Some((outcome, resp));
                    return injected_input;
                }
                let settle_by = settle_for.and_then(|wait| tilo_clock::now().checked_add(wait));
                let resp = match failed {
                    Some(failure) => test_control::Response::Err {
                        code: failure.code,
                        message: failure.message,
                    },
                    None => match (pending, receipt) {
                        // Held back until admitted, or settled: the top of the
                        // loop answers it now if it can, else its wake does.
                        (Some(ticket), _) => {
                            conn.pending_invocation = Some(ControlInvocation::admitting(
                                ticket,
                                action_response,
                                settle_by,
                            ));
                            continue;
                        }
                        (None, Some(receipt)) => {
                            match ControlInvocation::admitted(action_response, receipt, settle_by) {
                                Ok(settling) => {
                                    conn.pending_invocation = Some(settling);
                                    continue;
                                }
                                Err(answer) => answer,
                            }
                        }
                        (None, None) => match listing_wait {
                            Some(wait) => match wait.settle(resp, &line) {
                                Ok(answer) => answer,
                                // Held until the listing settles: the top of
                                // the loop runs it again once something moved.
                                Err(held) => {
                                    conn.pending_listing = Some(held);
                                    continue;
                                }
                            },
                            None => resp,
                        },
                    },
                };
                note_slow_control_request(&line, handling_started.elapsed());
                match queue_test_control_response(conn, &resp) {
                    PendingConnectionProgress::Closed
                    | PendingConnectionProgress::OutputPending => return injected_input,
                    PendingConnectionProgress::WroteResponse => {}
                    #[expect(
                        clippy::unreachable,
                        reason = "the queued response always ends in a newline, so flushing it \
                                  writes it, leaves it pending, or closes the connection"
                    )]
                    PendingConnectionProgress::Idle | PendingConnectionProgress::Waiting => {
                        unreachable!("a queued response must write, wait, or close")
                    }
                }
            }
            Err(ReadLineError::TooLarge) => {
                tracing::warn!("test_control: client sent oversized line; dropping connection");
                conn.closed = true;
                return injected_input;
            }
            Err(ReadLineError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {
                return injected_input;
            }
            Err(ReadLineError::Io(e)) => {
                eprintln!("tilo compositor: test_control read: {e}");
                conn.closed = true;
                return injected_input;
            }
        }
    }
}

/// Follow the person's session on their other devices
/// (`crate::shell::owner_session`): idle time here counts from their use of a
/// nearby one, and a lock or unlock there may be followed. Publishes this
/// device's own session to them.
fn follow_owner_session(
    net: &crate::shell::network::ShellNetwork,
    session: &mut SessionManager,
    follow: &mut crate::shell::owner_session::SessionFollow,
) -> Option<crate::shell::owner_session::Follow> {
    let (local, _) = net.local_device_fabric_identity()?;
    let now = Instant::now();
    let now_ms = tilo_clock::unix_now_ms();
    let since_input = u64::try_from(session.input_idle_for(now).as_millis()).unwrap_or(u64::MAX);
    let input_ms = now_ms.saturating_sub(since_input);
    let followed = net
        .with_device_fabric(|fabric| {
            let snapshots = fabric.snapshots();
            let others = || {
                snapshots
                    .iter()
                    .filter(|(device, _)| **device != local)
                    .map(|(_, snapshot)| &snapshot.presence)
            };
            if let Some(used) = snapshots.get(&local).and_then(|own| {
                crate::shell::owner_session::latest_use_nearby(&own.presence, others())
            }) && let Some(at) =
                now.checked_sub(Duration::from_millis(now_ms.saturating_sub(used)))
            {
                session.note_use_elsewhere(at);
            }
            let own = snapshots.get(&local).map(|own| &own.presence);
            follow.decide(
                session.is_locked(),
                input_ms,
                session.in_use(now),
                others().map(|presence| {
                    let nearby = own.is_some_and(|own| {
                        crate::shell::owner_session::shares_network(own, presence)
                    });
                    (presence.session, nearby)
                }),
            )
        })
        .flatten();
    crate::shell::device_fabric_runtime::set_owner_session(
        follow.presence(session.is_locked(), input_ms),
    );
    followed
}

/// Turn a [`SessionTransition`] into a real backend effect.
///
/// `Dimmed`/`Undimmed` lower and restore the backlight, or switch the paint
/// path's dimming overlay where there is none ([`ScreenDimmer`]);
/// `Blanked`/`Woke` drive the backend's DPMS (no-op on winit/headless);
/// `Locked`/`Unlocked` are pure state the paint path reads via
/// `SessionManager::is_locked`, needing no backend call. On a render thread,
/// `pipe` carries the DPMS change on the same channel as frames so a blank can't overtake a queued frame.
fn apply_session_transition(
    transition: SessionTransition,
    backend: &mut dyn Backend,
    pipe: Option<&present::PresentPipe>,
    local_session: Option<&LocalSessionLifecycle>,
    dimmer: &mut ScreenDimmer<impl Backlight>,
    output_management: Option<&mut super::output_management::OutputManagementState>,
) {
    if let Some(local_session) = local_session {
        match transition {
            SessionTransition::Locked => local_session.set_locked(true),
            SessionTransition::Unlocked => local_session.set_locked(false),
            SessionTransition::Dimmed
            | SessionTransition::Undimmed
            | SessionTransition::Blanked
            | SessionTransition::Woke
            | SessionTransition::Suspend => {}
        }
    }
    // The flight record of every power transition, with the backlight as
    // the kernel holds it: a screen that stays dark after a wake must say
    // whether its backlight, its CRTC (the presenter's own line) or its
    // input is what did not come back.
    if matches!(
        transition,
        SessionTransition::Dimmed
            | SessionTransition::Undimmed
            | SessionTransition::Blanked
            | SessionTransition::Woke
    ) {
        eprintln!(
            "tilo compositor: session power {transition:?} backlight={}",
            os_actions::display::backlight_state().unwrap_or_else(|| "none".into())
        );
    }
    match transition {
        SessionTransition::Dimmed => dimmer.dim(),
        SessionTransition::Undimmed => dimmer.restore(),
        SessionTransition::Blanked => {
            if let Some(management) = output_management {
                management.set_session_blanked(true);
            }
            backend.set_dpms(false);
            if let Some(p) = pipe {
                p.set_dpms(false);
            }
        }
        SessionTransition::Woke => {
            if let Some(management) = output_management {
                management.set_session_blanked(false);
            }
            backend.set_dpms(true);
            if let Some(p) = pipe {
                p.set_dpms(true);
            }
            backend.note_wake("woke");
        }
        // Lock/Unlock are reflected via `SessionManager::is_locked`; Suspend is
        // gated by the inhibitor registry + issued through the power service, never here.
        SessionTransition::Locked | SessionTransition::Unlocked | SessionTransition::Suspend => {}
    }
    // And what it left behind. A panel lights asynchronously: each output's
    // own record (`DRM wake check`) says when, and whether, it came back.
    if matches!(
        transition,
        SessionTransition::Dimmed
            | SessionTransition::Undimmed
            | SessionTransition::Blanked
            | SessionTransition::Woke
    ) {
        eprintln!(
            "tilo compositor: session power {transition:?} applied backlight={}",
            os_actions::display::backlight_state().unwrap_or_else(|| "none".into())
        );
    }
}

/// Apply owner-session output power without turning it into input or unlock.
/// A global idle blank also belongs to the presenter, so restoring only the
/// connector cannot make a frame visible. Returns whether a paint is owed.
fn apply_output_power_request(
    request: (OutputId, bool),
    session: &mut SessionManager,
    backend: &mut dyn Backend,
    pipe: Option<&present::PresentPipe>,
    local_session: Option<&LocalSessionLifecycle>,
    dimmer: &mut ScreenDimmer<impl Backlight>,
    management: &mut super::output_management::OutputManagementState,
    now: Instant,
) -> bool {
    let (output, on) = request;
    if on {
        let transitions = session.request_display_wake(now);
        let woke = transitions.contains(&SessionTransition::Woke);
        for transition in transitions {
            apply_session_transition(
                transition,
                backend,
                pipe,
                local_session,
                dimmer,
                Some(&mut *management),
            );
        }
        if !woke {
            apply_session_transition(
                SessionTransition::Woke,
                backend,
                pipe,
                local_session,
                dimmer,
                Some(management),
            );
        }
    }
    backend.set_output_power(output, on);
    on
}

fn publish_native_session_state(
    local_session: Option<&LocalSessionLifecycle>,
    session: &SessionManager,
    onboarding_active: bool,
) {
    if let Some(local_session) = local_session {
        local_session.publish_native_state(session.is_locked(), onboarding_active);
    }
}

fn install_native_session_lifecycle(
    broker: &crate::shell::collab_broker::BrokerHandle,
    session: &SessionManager,
    onboarding_active: bool,
) -> LocalSessionLifecycle {
    let local_session = broker.local_session_lifecycle();
    publish_native_session_state(Some(&local_session), session, onboarding_active);
    local_session
}

fn close_native_session_authority(local_session: Option<&LocalSessionLifecycle>) {
    if let Some(local_session) = local_session {
        local_session.close();
    }
}

/// What one press of the hardware power key does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PowerKeyEffect {
    /// A display that could not be lit is modeset again.
    RelitOutputs,
    /// The press woke the blanked display; that is all it does.
    WokeDisplay,
    /// The machine suspends (the power settings' action).
    Suspend,
    /// The session locks (the power settings' action).
    Lock,
    /// The session is already locked: the display turns off.
    Blank,
    /// The power settings say to do nothing.
    Nothing,
}

/// What a power key press does: first what the person can see is wrong (a
/// display that could not be lit, a blanked one), then the power settings'
/// action. Locking a locked session would change nothing on screen, so the
/// display turns off instead.
fn power_key_effect(
    relit_outputs: bool,
    was_blanked: bool,
    locked: bool,
    action: Option<tilo_power_mgmt::PowerAction>,
) -> PowerKeyEffect {
    use tilo_power_mgmt::PowerAction;
    if relit_outputs {
        return PowerKeyEffect::RelitOutputs;
    }
    if was_blanked {
        return PowerKeyEffect::WokeDisplay;
    }
    match action {
        Some(PowerAction::Suspend) => PowerKeyEffect::Suspend,
        Some(PowerAction::Lock) if locked => PowerKeyEffect::Blank,
        Some(PowerAction::Lock) => PowerKeyEffect::Lock,
        Some(PowerAction::Ignore) | None => PowerKeyEffect::Nothing,
    }
}

/// Whether an event is the hardware power key being pressed — detected by
/// both the logical [`NamedKey::Power`] and physical key code, keymap-independent.
/// Translate the backend-neutral winit events into the evdev-shaped EIS
/// events.  Unknown physical keys intentionally remain local rather than
/// guessing a keycode for a layout-specific key.
#[allow(
    clippy::cast_possible_truncation,
    reason = "winit exposes pixel-wheel coordinates as f64 while the EIS wire event is f32; bounded desktop coordinates intentionally narrow here"
)]
fn portal_capture_event(
    event: &WindowEvent,
    state: &State,
) -> Option<crate::portal::CapturedInput> {
    use crate::portal::CapturedInput;
    match event {
        WindowEvent::CursorMoved { position, .. } => {
            let (old_x, old_y) = state.client_input.cursor();
            Some(CapturedInput::PointerMotion {
                x: position.x,
                y: position.y,
                dx: position.x - old_x,
                dy: position.y - old_y,
            })
        }
        WindowEvent::MouseInput { state, button, .. } => {
            let button = match button {
                MouseButton::Left => 0x110,
                MouseButton::Right => 0x111,
                MouseButton::Middle => 0x112,
                MouseButton::Back => 0x116,
                MouseButton::Forward => 0x115,
                MouseButton::Other(button) => u32::from(*button),
            };
            Some(CapturedInput::PointerButton {
                button,
                pressed: *state == ElementState::Pressed,
            })
        }
        WindowEvent::MouseWheel { delta, .. } => {
            let (dx, dy) = match delta {
                winit::event::MouseScrollDelta::LineDelta(x, y) => (*x, *y),
                winit::event::MouseScrollDelta::PixelDelta(position) => {
                    (position.x as f32, position.y as f32)
                }
            };
            Some(CapturedInput::Scroll { dx, dy })
        }
        WindowEvent::KeyboardInput { event, .. } => ui::evdev::from_physical(event.physical_key)
            .map(|keycode| CapturedInput::Keyboard {
                keycode,
                pressed: event.state == ElementState::Pressed,
            }),
        _ => None,
    }
}

#[derive(Clone, Debug)]
struct SpatialControlSession {
    target: crate::shell::personal_continuity::DeviceId,
    lease_nonce: [u8; 16],
    output: Option<String>,
    return_edge: crate::shell::device_fabric::DisplayEdge,
    x: f64,
    y: f64,
    pressed_keys: std::collections::BTreeSet<u32>,
    pressed_buttons: std::collections::BTreeSet<u32>,
    nearby_drop: Option<std::path::PathBuf>,
    /// The window whose tab was being dragged here when the pointer crossed:
    /// let go over the other device, it shows there.
    dragged_tab: Option<u64>,
}

/// Apply a production native-seat batch; the runtime drains revocation barriers
/// before surviving input. Both phases keep their existing timing/count names.
pub(crate) fn dispatch_spatial_input_batch(
    state: &mut State,
    batch: crate::shell::device_fabric_runtime::SpatialInputBatch,
) {
    use crate::shell::device_fabric_runtime::SpatialInputBatch;
    match batch {
        SpatialInputBatch::Releases(controllers) => {
            for controller in controllers {
                let _ = state.release_personal_fabric_input(&controller);
            }
        }
        SpatialInputBatch::Inputs(events) => {
            for (controller, event) in events {
                let _ = state.apply_personal_fabric_input(&controller, event);
            }
        }
    }
}

/// Deliver the input the person's other devices sent as this device's own:
/// pointer and buttons through the same queue as its own pointer, keys
/// through its keyboard layout, so the desktop, its apps and its clients all
/// take them, and the drawn pointer moves.
fn queue_personal_fabric_input(
    state: &mut State,
    backend: &mut dyn Backend,
    pending: &mut std::collections::VecDeque<BackendEvent>,
) {
    use crate::shell::device_fabric::{FabricInputEvent, ScrollUnit};
    let device_id = winit::event::DeviceId::dummy();
    for event in state.take_personal_fabric_input() {
        let input = match event {
            FabricInputEvent::PointerMotion { x, y } => {
                let point = (f64::from(x), f64::from(y));
                let placements = crate::compositor::backend::layout::placements(backend.outputs());
                let (scene_x, scene_y) =
                    crate::compositor::backend::layout::logical_to_scene(&placements, point);
                backend.place_pointer(scene_x, scene_y);
                WindowEvent::CursorMoved {
                    device_id,
                    position: winit::dpi::PhysicalPosition::new(point.0, point.1),
                }
            }
            FabricInputEvent::PointerButton { button, pressed } => {
                let Some(button) =
                    crate::compositor::backend::drm::runtime::map_mouse_button(button)
                else {
                    continue;
                };
                WindowEvent::MouseInput {
                    device_id,
                    state: if pressed {
                        ElementState::Pressed
                    } else {
                        ElementState::Released
                    },
                    button,
                }
            }
            FabricInputEvent::Scroll {
                horizontal,
                vertical,
                unit,
            } => {
                let (x, y) = (
                    f64::from(horizontal) / ScrollUnit::STEPS,
                    f64::from(vertical) / ScrollUnit::STEPS,
                );
                WindowEvent::MouseWheel {
                    device_id,
                    delta: match unit {
                        #[expect(
                            clippy::cast_possible_truncation,
                            reason = "wheel lines, which winit carries as f32"
                        )]
                        ScrollUnit::Lines => MouseScrollDelta::LineDelta(x as f32, y as f32),
                        ScrollUnit::Pixels => {
                            MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(x, y))
                        }
                    },
                    phase: winit::event::TouchPhase::Moved,
                }
            }
            FabricInputEvent::Key {
                keycode, pressed, ..
            } => {
                pending.extend(
                    backend
                        .translate_key(keycode, pressed)
                        .into_iter()
                        .map(|event| BackendEvent::Sibling(Box::new(event))),
                );
                continue;
            }
        };
        pending.push_back(BackendEvent::Sibling(Box::new(BackendEvent::Input(input))));
    }
}

#[cfg(test)]
pub(crate) fn teardown_spatial_control_for_test(
    network: &crate::shell::network::ShellNetwork,
    target: crate::shell::personal_continuity::DeviceId,
    nonce: [u8; 16],
    held: (
        std::collections::BTreeSet<u32>,
        std::collections::BTreeSet<u32>,
    ),
) -> bool {
    let mut active = Some(SpatialControlSession {
        target,
        lease_nonce: nonce,
        output: None,
        return_edge: crate::shell::device_fabric::DisplayEdge::Right,
        x: 0.0,
        y: 0.0,
        pressed_keys: held.0,
        pressed_buttons: held.1,
        nearby_drop: None,
        dragged_tab: None,
    });
    end_spatial_control(network, &mut active);
    active.is_none()
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "coordinates are clamped to the fabric wire bounds before narrowing"
)]
fn spatial_coord(value: f64) -> i32 {
    value.clamp(-1_000_000.0, 1_000_000.0).round() as i32
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "wheel deltas are clamped to the bounded fabric wire representation"
)]
fn spatial_scroll(value: f64) -> i32 {
    (value * crate::shell::device_fabric::ScrollUnit::STEPS)
        .clamp(-1_000_000.0, 1_000_000.0)
        .round() as i32
}

fn nearby_drop_candidate(
    paths: Option<&[std::path::PathBuf]>,
) -> Result<Option<std::path::PathBuf>, ()> {
    let Some(paths) = paths else {
        return Ok(None);
    };
    let [path] = paths else {
        return Err(());
    };
    let metadata = std::fs::symlink_metadata(path).map_err(|_| ())?;
    if metadata.file_type().is_symlink()
        || (!metadata.is_file() && !metadata.is_dir())
        || (metadata.is_file() && metadata.len() == 0)
    {
        return Err(());
    }
    Ok(Some(path.clone()))
}

fn queue_spatial_return_event(
    pending: &mut std::collections::VecDeque<BackendEvent>,
    point: (f64, f64),
    parked_point: Option<(f64, f64)>,
) {
    // An unlocked relative motion has a paired absolute event; a parked
    // physical mouse has none, while mouse keys report the parked point.
    // Preserve independently positioned events behind the return.
    if pending.front().is_some_and(|event| {
        matches!(event,
        BackendEvent::Input(WindowEvent::CursorMoved { position, .. })
            if parked_point.is_none_or(|old| old == (position.x, position.y)))
    }) {
        pending.pop_front();
    }
    // The pending queue has already crossed event_to_logical. Only the
    // backend warp uses scene coordinates; clients receive this logical point.
    pending.push_front(BackendEvent::Input(WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(point.0, point.1),
    }));
}

fn move_spatial_pointer(
    state: &State,
    network: &crate::shell::network::ShellNetwork,
    active: &mut Option<SpatialControlSession>,
    delta: (f64, f64),
) -> crate::compositor::spatial_pointer::SpatialMotion {
    use crate::compositor::spatial_pointer::SpatialMotion;
    if let Some(motion) = move_observed_spatial_pointer(
        state.client_input.pointer_position(),
        network,
        active,
        delta,
    ) {
        return motion;
    }
    if move_legacy_spatial_pointer(state, network, active, delta.0, delta.1) {
        if active.is_some() {
            SpatialMotion::Remote
        } else {
            SpatialMotion::Returned(None)
        }
    } else {
        SpatialMotion::Local
    }
}

fn move_observed_spatial_pointer(
    cursor: Option<(f64, f64)>,
    network: &crate::shell::network::ShellNetwork,
    active: &mut Option<SpatialControlSession>,
    delta: (f64, f64),
) -> Option<crate::compositor::spatial_pointer::SpatialMotion> {
    use crate::compositor::spatial_pointer::{MotionPlan, PointerAt, SpatialMotion, plan_motion};
    use crate::shell::device_fabric_runtime::person_layout;
    // A gesture begun on a legacy peer keeps its existing edge protocol.
    if active
        .as_ref()
        .is_some_and(|session| session.output.is_none())
    {
        return None;
    }
    let Some(view) = person_layout::current(network) else {
        if active.is_some() {
            end_spatial_control(network, active);
            return Some(SpatialMotion::Returned(None));
        }
        return None;
    };
    let at = if let Some(session) = active.as_ref() {
        let output = session.output.as_deref()?;
        let Some(display) = view
            .layout
            .find(session.target.as_str(), output)
            .filter(|display| person_layout::reachable(network, display))
        else {
            end_spatial_control(network, active);
            return Some(SpatialMotion::Returned(None));
        };
        PointerAt::Remote {
            device: session.target.as_str(),
            output,
            shared: display.to_shared((session.x, session.y)),
        }
    } else {
        // Older leased peers remain reachable through their existing edge
        // protocol until all participating targets supply output metadata.
        if view.legacy_targets.iter().any(|target| {
            crate::shell::device_fabric_runtime::spatial_pointer_lease(network, target).is_some()
        }) {
            return None;
        }
        let cursor = cursor?;
        let index = view.layout.locate(view.local.as_str(), cursor)?;
        if !view.layout.displays.get(index)?.enabled {
            return Some(SpatialMotion::Local);
        }
        PointerAt::Local(cursor)
    };
    let plan = plan_motion(&view.layout, view.local.as_str(), at, delta, |display| {
        person_layout::reachable(network, display)
    });
    Some(match plan {
        MotionPlan::Local => SpatialMotion::Local,
        MotionPlan::Return { local_point } => {
            end_spatial_control(network, active);
            SpatialMotion::Returned(local_point)
        }
        MotionPlan::Remote {
            device,
            output,
            device_point,
            ..
        } => move_to_observed_display(network, active, (device, output), device_point),
    })
}

fn move_to_observed_display(
    network: &crate::shell::network::ShellNetwork,
    active: &mut Option<SpatialControlSession>,
    destination: (String, String),
    point: (f64, f64),
) -> crate::compositor::spatial_pointer::SpatialMotion {
    use crate::compositor::spatial_pointer::SpatialMotion;
    let Ok(target) = crate::shell::personal_continuity::DeviceId::new(destination.0) else {
        return SpatialMotion::Local;
    };
    let Some(lease) = crate::shell::device_fabric_runtime::spatial_pointer_lease(network, &target)
    else {
        end_spatial_control(network, active);
        return SpatialMotion::Returned(None);
    };
    if active
        .as_ref()
        .is_some_and(|session| session.target == target && session.lease_nonce != lease.nonce)
    {
        end_spatial_control(network, active);
        return SpatialMotion::Returned(None);
    }
    let entering = active.is_none();
    let nearby_drop = if entering {
        let pending = file_explorer::external_drag::unclaimed();
        let Ok(drop) = nearby_drop_candidate(pending.as_ref().map(|drag| drag.paths.as_slice()))
        else {
            return SpatialMotion::Local;
        };
        drop
    } else {
        None
    };
    if active
        .as_ref()
        .is_some_and(|session| session.target != target)
    {
        // End the previous target's held input under its original authority.
        // The source file, if any, follows the pointer until its explicit drop.
        let drop = active
            .as_mut()
            .and_then(|session| session.nearby_drop.take());
        end_spatial_control(network, active);
        *active = Some(SpatialControlSession {
            target: target.clone(),
            lease_nonce: lease.nonce,
            output: Some(destination.1.clone()),
            return_edge: crate::shell::device_fabric::DisplayEdge::Left,
            x: point.0,
            y: point.1,
            pressed_keys: std::collections::BTreeSet::new(),
            pressed_buttons: std::collections::BTreeSet::new(),
            nearby_drop: drop,
            dragged_tab: None,
        });
    }
    if crate::shell::device_fabric_runtime::send_spatial_input_bound(
        network,
        &target,
        lease.nonce,
        crate::shell::device_fabric::FabricInputEvent::PointerMotion {
            x: spatial_coord(point.0),
            y: spatial_coord(point.1),
        },
    )
    .is_err()
    {
        end_spatial_control(network, active);
        return if entering {
            SpatialMotion::Local
        } else {
            SpatialMotion::Returned(None)
        };
    }
    if let Some(session) = active.as_mut() {
        session.output = Some(destination.1);
        session.x = point.0;
        session.y = point.1;
    } else {
        *active = Some(SpatialControlSession {
            target,
            lease_nonce: lease.nonce,
            output: Some(destination.1),
            return_edge: crate::shell::device_fabric::DisplayEdge::Left,
            x: point.0,
            y: point.1,
            pressed_keys: std::collections::BTreeSet::new(),
            pressed_buttons: std::collections::BTreeSet::new(),
            nearby_drop,
            dragged_tab: None,
        });
    }
    SpatialMotion::Remote
}

fn move_legacy_spatial_pointer(
    state: &State,
    network: &crate::shell::network::ShellNetwork,
    active: &mut Option<SpatialControlSession>,
    dx: f64,
    dy: f64,
) -> bool {
    if let Some(session) = active.as_mut() {
        session.x += dx;
        session.y += dy;
        let fabric = network.device_fabric();
        let Some(rect) = fabric
            .snapshots()
            .get(&session.target)
            .and_then(|snapshot| snapshot.display.as_ref())
            .map(|display| display.rect)
        else {
            end_spatial_control(network, active);
            return true;
        };
        let max_x = f64::from(rect.width.saturating_sub(1));
        let max_y = f64::from(rect.height.saturating_sub(1));
        let crossed_back = match session.return_edge {
            crate::shell::device_fabric::DisplayEdge::Left => session.x < 0.0,
            crate::shell::device_fabric::DisplayEdge::Right => session.x > max_x,
            crate::shell::device_fabric::DisplayEdge::Top => session.y < 0.0,
            crate::shell::device_fabric::DisplayEdge::Bottom => session.y > max_y,
        };
        if crossed_back {
            end_spatial_control(network, active);
            return true;
        }
        session.x = session.x.clamp(0.0, max_x);
        session.y = session.y.clamp(0.0, max_y);
        if crate::shell::device_fabric_runtime::send_spatial_input_bound(
            network,
            &session.target,
            session.lease_nonce,
            crate::shell::device_fabric::FabricInputEvent::PointerMotion {
                x: spatial_coord(session.x),
                y: spatial_coord(session.y),
            },
        )
        .is_err()
        {
            end_spatial_control(network, active);
        }
        return true;
    }
    let Some((cursor_x, cursor_y)) = state.client_input.pointer_position() else {
        return false;
    };
    let Some((local, _)) = network.local_device_fabric_identity() else {
        return false;
    };
    let fabric = network.device_fabric();
    let Some(local_rect) = fabric
        .snapshots()
        .get(&local)
        .and_then(|snapshot| snapshot.display.as_ref())
        .map(|display| display.rect)
    else {
        return false;
    };
    let Some(edge) = spatial_departure_edge(
        cursor_x,
        cursor_y,
        local_rect.width,
        local_rect.height,
        dx,
        dy,
    ) else {
        return false;
    };
    let local_x = spatial_coord(cursor_x);
    let local_y = spatial_coord(cursor_y);
    let Some(transition) = fabric.cross_edge(&local, edge, local_x, local_y) else {
        return false;
    };
    let Some(session) = begin_spatial_control(network, transition, edge, (dx, dy)) else {
        return false;
    };
    if crate::shell::device_fabric_runtime::send_spatial_input_bound(
        network,
        &session.target,
        session.lease_nonce,
        crate::shell::device_fabric::FabricInputEvent::PointerMotion {
            x: spatial_coord(session.x),
            y: spatial_coord(session.y),
        },
    )
    .is_err()
    {
        return false;
    }
    *active = Some(session);
    true
}

fn begin_spatial_control(
    network: &crate::shell::network::ShellNetwork,
    transition: crate::shell::device_fabric::EdgeTransition,
    edge: crate::shell::device_fabric::DisplayEdge,
    delta: (f64, f64),
) -> Option<SpatialControlSession> {
    let pending_drag = file_explorer::external_drag::unclaimed();
    // Unsupported Files drags stay local: losing their up edge would strand
    // the gesture on the source device.
    let nearby_drop =
        nearby_drop_candidate(pending_drag.as_ref().map(|drag| drag.paths.as_slice())).ok()?;
    let lease =
        crate::shell::device_fabric_runtime::spatial_pointer_lease(network, &transition.to)?;
    Some(SpatialControlSession {
        target: transition.to,
        lease_nonce: lease.nonce,
        output: None,
        return_edge: edge.opposite(),
        x: f64::from(transition.target_x) + delta.0,
        y: f64::from(transition.target_y) + delta.1,
        pressed_keys: std::collections::BTreeSet::new(),
        pressed_buttons: std::collections::BTreeSet::new(),
        nearby_drop,
        dragged_tab: None,
    })
}

fn spatial_departure_edge(
    cursor_x: f64,
    cursor_y: f64,
    width: u32,
    height: u32,
    dx: f64,
    dy: f64,
) -> Option<crate::shell::device_fabric::DisplayEdge> {
    use crate::shell::device_fabric::DisplayEdge;
    let max_x = f64::from(width.saturating_sub(1));
    let max_y = f64::from(height.saturating_sub(1));
    let horizontal = if cursor_x <= 0.0 && dx < 0.0 {
        Some(DisplayEdge::Left)
    } else if cursor_x >= max_x && dx > 0.0 {
        Some(DisplayEdge::Right)
    } else {
        None
    };
    let vertical = if cursor_y <= 0.0 && dy < 0.0 {
        Some(DisplayEdge::Top)
    } else if cursor_y >= max_y && dy > 0.0 {
        Some(DisplayEdge::Bottom)
    } else {
        None
    };
    match (horizontal, vertical) {
        (Some(edge), Some(_)) if dx.abs() >= dy.abs() => Some(edge),
        (Some(_) | None, Some(edge)) | (Some(edge), None) => Some(edge),
        (None, None) => None,
    }
}

#[cfg(test)]
mod spatial_edge_tests;

/// Tell the person where "Send…" put their screenshot, or why it did not.
fn notify_screenshot_sent(
    frame_builder: &mut FrameBuilder,
    sent: Result<crate::shell::screenshot_send::SendReceipt, String>,
) {
    match sent {
        Ok(receipt) => frame_builder.with_chrome(|chrome| {
            chrome.push_notification(
                crate::shell::i18n::t("capture-screenshot-sent"),
                receipt.message(),
            );
        }),
        Err(error) => frame_builder.with_chrome(|chrome| {
            chrome.push_error_notification(
                crate::shell::i18n::t("capture-screenshot-not-sent"),
                error,
            );
        }),
    }
}

fn finish_nearby_drop(
    event: &WindowEvent,
    network: &crate::shell::network::ShellNetwork,
    active: &mut Option<SpatialControlSession>,
) -> Option<bool> {
    let session = active.as_ref()?;
    if matches!(
        event,
        WindowEvent::MouseInput {
            state: ElementState::Released,
            button: MouseButton::Left,
            ..
        }
    ) && let Some(path) = session.nearby_drop.clone()
    {
        let target = session.target.clone();
        let matches = file_explorer::external_drag::hand_off()
            .is_some_and(|drag| drag.paths.as_slice() == [path.as_path()]);
        end_spatial_control(network, active);
        if matches {
            let _ = crate::shell::device_fabric_runtime::request_nearby_drop(&target, &path);
        }
        return Some(false);
    }
    None
}

/// Let go of a window dragged across the shared edge, over the other device:
/// it shows there, and the pointer stays there. The press was this device's,
/// so the release is too; the caller ends the drag here where it began.
fn finish_tab_move(
    event: &WindowEvent,
    network: &crate::shell::network::ShellNetwork,
    active: &mut Option<SpatialControlSession>,
) -> bool {
    let Some(session) = active.as_mut() else {
        return false;
    };
    if !matches!(
        event,
        WindowEvent::MouseInput {
            state: ElementState::Released,
            button: MouseButton::Left,
            ..
        }
    ) {
        return false;
    }
    let Some(tab) = session.dragged_tab.take() else {
        return false;
    };
    if let Err(error) =
        crate::shell::device_fabric_runtime::show_tab_on(network, &session.target, tab)
    {
        tracing::warn!(%error, tab, "the window dragged to another device was not shown there");
    }
    true
}

fn forward_spatial_window_event(
    event: &WindowEvent,
    network: &crate::shell::network::ShellNetwork,
    active: &mut Option<SpatialControlSession>,
) -> bool {
    if let Some(consumed) = finish_nearby_drop(event, network, active) {
        return consumed;
    }
    let Some(session) = active.as_ref() else {
        return false;
    };
    let target = session.target.clone();
    let fabric_event = match event {
        WindowEvent::CursorMoved { .. } => return true,
        WindowEvent::MouseInput { state, button, .. } => {
            let Some(button) = crate::shell::input_policy::evdev_button(*button) else {
                return true;
            };
            crate::shell::device_fabric::FabricInputEvent::PointerButton {
                button,
                pressed: *state == ElementState::Pressed,
            }
        }
        WindowEvent::MouseWheel { delta, .. } => {
            use crate::shell::device_fabric::ScrollUnit;
            let (horizontal, vertical, unit) = match delta {
                MouseScrollDelta::LineDelta(x, y) => (
                    spatial_scroll(f64::from(*x)),
                    spatial_scroll(f64::from(*y)),
                    ScrollUnit::Lines,
                ),
                MouseScrollDelta::PixelDelta(position) => (
                    spatial_scroll(position.x),
                    spatial_scroll(position.y),
                    ScrollUnit::Pixels,
                ),
            };
            let event = crate::shell::device_fabric::FabricInputEvent::Scroll {
                horizontal,
                vertical,
                unit,
            };
            if !event.validate() {
                return true;
            }
            event
        }
        WindowEvent::KeyboardInput { event, .. } => {
            // A repeat is no new press: the key is still down over there.
            if event.repeat {
                return true;
            }
            let Some(keycode) = ui::evdev::from_physical(event.physical_key) else {
                return true;
            };
            crate::shell::device_fabric::FabricInputEvent::Key {
                keycode,
                pressed: event.state == ElementState::Pressed,
                modifiers: 0,
            }
        }
        _ => return false,
    };
    if crate::shell::device_fabric_runtime::send_spatial_input_bound(
        network,
        &target,
        session.lease_nonce,
        fabric_event,
    )
    .is_err()
    {
        end_spatial_control(network, active);
    } else if let Some(session) = active.as_mut() {
        match fabric_event {
            crate::shell::device_fabric::FabricInputEvent::PointerButton { button, pressed } => {
                if pressed {
                    session.pressed_buttons.insert(button);
                } else {
                    session.pressed_buttons.remove(&button);
                }
            }
            crate::shell::device_fabric::FabricInputEvent::Key {
                keycode, pressed, ..
            } => {
                if pressed {
                    session.pressed_keys.insert(keycode);
                } else {
                    session.pressed_keys.remove(&keycode);
                }
            }
            crate::shell::device_fabric::FabricInputEvent::PointerMotion { .. }
            | crate::shell::device_fabric::FabricInputEvent::Scroll { .. } => {}
        }
    }
    true
}

fn is_power_key(event: &WindowEvent) -> bool {
    if let WindowEvent::KeyboardInput { event, .. } = event
        && event.state == ElementState::Pressed
    {
        if matches!(event.logical_key, Key::Named(NamedKey::Power)) {
            return true;
        }
        if let PhysicalKey::Code(code) = event.physical_key {
            return code == winit::keyboard::KeyCode::Power;
        }
    }
    false
}

/// Build an OS notification toast from a power-service notification.
fn power_notification_to_incoming(
    bridge: &crate::notifications::NotificationBridge,
    n: tilo_power_mgmt::PowerNotification,
) -> crate::notifications::IncomingNotification {
    crate::notifications::IncomingNotification {
        dbus_id: bridge.alloc_id(),
        app_name: crate::shell::i18n::t("notification-app-power"),
        app_icon: None,
        desktop_entry: None,
        image_data: None,
        summary: n.summary,
        body: n.body,
        actions: Vec::new(),
        // Critical battery lingers until dismissed; the low heads-up uses the server default expiry.
        expire_timeout: if n.critical { 0 } else { -1 },
        urgency: if n.critical { 2 } else { 1 },
        caller: None,
        badge_count: None,
        contact_endpoint: None,
        sender_name: None,
        sound: Some(settings_store::system_sounds::SystemSoundEvent::LowBattery),
        incoming_call: false,
        // Critical battery is about the whole machine: every display shows it.
        machine_wide: n.critical,
    }
}

/// The virtual terminal `Ctrl+Alt+F1..F12` asks for, or `None`.
///
/// On a bare TTY the compositor holds the keyboard in raw mode, so the
/// kernel's VT-switch handling never sees the chord — if the compositor
/// doesn't act, the user can't leave this terminal for the session's life.
/// Pure so the chord table is checked without a seat.
fn vt_switch_chord(event: &WindowEvent, modifiers: Modifiers) -> Option<i32> {
    let WindowEvent::KeyboardInput { event, .. } = event else {
        return None;
    };
    if event.state != ElementState::Pressed {
        return None;
    }
    let mods = modifiers.state();
    if !(mods.control_key() && mods.alt_key()) {
        return None;
    }
    let Key::Named(named) = &event.logical_key else {
        return None;
    };
    // VTs are numbered from 1, function keys from F1 — they line up.
    let n = match named {
        NamedKey::F1 => 1,
        NamedKey::F2 => 2,
        NamedKey::F3 => 3,
        NamedKey::F4 => 4,
        NamedKey::F5 => 5,
        NamedKey::F6 => 6,
        NamedKey::F7 => 7,
        NamedKey::F8 => 8,
        NamedKey::F9 => 9,
        NamedKey::F10 => 10,
        NamedKey::F11 => 11,
        NamedKey::F12 => 12,
        _ => return None,
    };
    Some(n)
}

/// Where the compositor sends an input event before any client or shell
/// chrome is offered it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PriorityRoute {
    /// The session lock captures the event for its password field.
    LockScreen,
    /// A hardware media/brightness key acts on the machine itself.
    MediaKey(crate::shell::media_keys::MediaKey),
    PowerKey,
    VtSwitch(i32),
    RemoteInputKillSwitch,
    LockSession,
    /// Shift+Alt+Num Lock flips mouse keys; it works whichever client has
    /// focus, since the person who needs it may have no other pointer.
    ToggleMouseKeys,
    /// Nothing claims the event ahead of spatial forwarding, input capture
    /// and ordinary client/chrome routing.
    Continue,
}

/// The compositor's fixed input priority order, pure so it can be checked
/// without a seat.
///
/// The power key and media keys are the exceptions to the lock capturing
/// everything. The power key belongs to the machine: on a locked session it
/// still wakes a dark display or does what the power settings say
/// ([`power_key_effect`]), where the lock's password field would drop it.
/// Volume, mute, brightness and playback belong to the machine too, so they
/// keep working on the lock screen. None of them reveals anything behind the
/// lock, and media feedback is drawn on the lock surface itself.
fn priority_route(
    event: &WindowEvent,
    modifiers: Modifiers,
    locked: bool,
    shortcuts: &settings_store::hardware::ShortcutConfig,
) -> PriorityRoute {
    if is_power_key(event) {
        return PriorityRoute::PowerKey;
    }
    let media_key = crate::shell::media_keys::classify(event);
    if locked {
        return media_key
            .filter(|key| key.acts_while_locked())
            .map_or(PriorityRoute::LockScreen, PriorityRoute::MediaKey);
    }
    if let Some(vt) = vt_switch_chord(event, modifiers) {
        PriorityRoute::VtSwitch(vt)
    } else if is_remote_input_kill_switch(event, modifiers) {
        PriorityRoute::RemoteInputKillSwitch
    } else if is_lock_chord(event, modifiers, shortcuts) {
        PriorityRoute::LockSession
    } else if is_mouse_keys_toggle(event, modifiers) {
        PriorityRoute::ToggleMouseKeys
    } else if let Some(key) = media_key {
        PriorityRoute::MediaKey(key)
    } else {
        PriorityRoute::Continue
    }
}

#[path = "native_device_fabric.rs"]
mod device_fabric;
use device_fabric::{
    active_physical_gamepad_binding, end_spatial_control, end_spatial_control_or_forget,
    install_device_fabric_bridge, start_device_fabric,
};

/// Start the authenticated peer transport and install the chrome commands
/// that use it. Kept as one operation so first-login can call the exact same
/// path after the identity ceremony releases the desktop.
fn install_wifi_secret_bridge(
    host: &mut crate::shell::overlay_host::ShellOverlayHost,
    wifi_net: Arc<crate::shell::network::ShellNetwork>,
) {
    host.set_wifi_secret_bridge(std::rc::Rc::new(move |action| match action {
        crate::shell::overlay_host::WifiSecretAction::Lookup { ssid, security } => {
            Ok(wifi_net.shared_wifi_password(&ssid, security))
        }
        crate::shell::overlay_host::WifiSecretAction::Share {
            ssid,
            security,
            password,
        } => {
            if matches!(
                security,
                os_actions::WifiSecurity::Open | os_actions::WifiSecurity::Owe
            ) {
                return Ok(None);
            }
            wifi_net.share_wifi_credential(&ssid, security, &password)?;
            Ok(None)
        }
    }));
}

/// Ask `PipeWire` whether recordings can take desktop sound and the
/// microphone, on the session runtime: the probe runs `pw-record` and
/// `wpctl`, each allowed 5 s. `None` when there is no runtime to ask on;
/// capture then offers no audio tracks.
fn start_recording_audio_probe(
    wake: crate::session_runtime::Wake,
) -> Option<crate::session_runtime::PendingStart<(bool, bool)>> {
    match crate::session_runtime::runtime() {
        Ok(runtime) => Some(crate::session_runtime::start_blocking(
            runtime.handle(),
            wake,
            || Ok(crate::compositor::capture::recording_audio_availability()),
        )),
        Err(error) => {
            tracing::warn!(%error, "recording audio cannot be probed; capture offers no audio");
            None
        }
    }
}

/// Start peer sync for the compositor loop, at boot or when first login
/// completes. Building its runtime, reading the device key and binding the
/// endpoint block, so they run on the session runtime; the finished network
/// is delivered back through `wake` and installed with
/// [`install_session_peer_sync`].
fn start_session_peer_sync(
    tree_handle: crate::shell::state::WorkspaceTreeHandle,
    wake: crate::session_runtime::Wake,
) -> crate::session_runtime::PendingStart<Arc<crate::shell::network::ShellNetwork>> {
    match crate::session_runtime::runtime() {
        Ok(runtime) => crate::session_runtime::start_blocking(runtime.handle(), wake, move || {
            let net = Arc::new(
                crate::shell::network::ShellNetwork::spawn_for_session(tree_handle)
                    .map_err(|error| error.to_string())?
                    .with_continuation_labels(Box::new(
                        crate::shell::doc_open::DesktopContinuationLabels,
                    )),
            );
            // Joining reads the enrolled rooms, this device's identities and
            // the community configuration, and redeems a boot invitation:
            // here, before the loop has the network. Invitations,
            // redemptions, enrollments and sync rounds run on the network's
            // worker from the first join on.
            net.start_peer_sync_worker();
            let _ = net.join_default_workspace();
            tracing::info!("peer-sync: actor spawned, default workspace joined");
            net.join_at_boot();
            net.follow_host_rooms();
            net.follow_community();
            Ok(net)
        }),
        Err(error) => crate::session_runtime::PendingStart::finished(Err(format!(
            "the session runtime is unavailable: {error}"
        ))),
    }
}

/// How windows cross devices: this device names itself the host of its
/// unconfined windows, so rooms of the person's alone carry them as running
/// here rather than as windows their other devices would relaunch (rooms
/// with anyone else in them carry none, [`crate::shell::state::shareable`]),
/// and reads the records a governed room's windows open.
fn install_window_hosting(
    net: &Arc<crate::shell::network::ShellNetwork>,
    tree_handle: &crate::shell::state::WorkspaceTreeHandle,
) {
    if let Some((device, device_label)) = net.local_device_fabric_identity() {
        tree_handle.write().window_hosting = Some(crate::shell::state::WindowHosting {
            device: device.as_str().to_owned(),
            device_label,
        });
    }
    let records = crate::shell::document_import::NativeHostedRooms(Arc::clone(net));
    crate::shell::peer_windows::set_hosted_record_reader(Box::new(move |room, app, record| {
        use crate::shell::document_import::HostedRooms as _;
        records.read(room, app, record)
    }));
}

/// Send legacy-window streams on the network, each on its tab's room.
fn install_legacy_streams(state: &mut State, net: &Arc<crate::shell::network::ShellNetwork>) {
    let stream_net = Arc::clone(net);
    state.set_legacy_stream_publisher(Arc::new(move |route, message| {
        let crate::compositor::state::LegacyStreamRoute { topic, only } = route;
        match (topic, only) {
            (Some(topic), Some(viewers)) => stream_net.publish_legacy_to(topic, message, viewers),
            (Some(topic), None) => stream_net.publish_legacy_message(topic, message),
            (None, _) => stream_net.publish_legacy_active(message),
        }
    }));
}

/// Install the chrome commands that use a freshly started network, which
/// joined its rooms before it reached the loop ([`start_session_peer_sync`]).
fn install_session_peer_sync(
    frame_builder: &mut FrameBuilder,
    state: &mut State,
    tree_handle: &crate::shell::state::WorkspaceTreeHandle,
    net: Arc<crate::shell::network::ShellNetwork>,
) -> Arc<crate::shell::network::ShellNetwork> {
    crate::shell::link_open::attach_network(&net);
    crate::shell::device_certification::attach_network(&net);
    net.attach_session_inhibitors(frame_builder.host().session_inhibitors());
    frame_builder.set_enrolled_action_network(Arc::clone(&net));
    install_wifi_secret_bridge(frame_builder.host_mut(), Arc::clone(&net));
    crate::network_secrets::roaming_wifi_slot().set(Arc::clone(&net) as _);
    install_device_fabric_bridge(
        frame_builder.host_mut(),
        Arc::clone(&net),
        tree_handle.clone(),
    );
    frame_builder.host_mut().set_personal_devices(Arc::new(
        crate::shell::device_fabric_runtime::FabricDeviceDirectory::new(Arc::clone(&net)),
    ));
    install_window_hosting(&net, tree_handle);
    let hosted_net = Arc::clone(&net);
    frame_builder.with_chrome(move |chrome| {
        chrome.window_transfers.set_hosted_rooms(Box::new(
            crate::shell::document_import::NativeHostedRooms(hosted_net),
        ));
    });
    install_legacy_streams(state, &net);
    frame_builder.host_mut().set_peer_sync_worker(
        crate::shell::overlay_host::PeerSyncWorker::for_network(&net),
    );
    net
}

fn attach_collab_broker_bridges(
    frame_builder: &mut FrameBuilder,
    broker: &crate::shell::collab_broker::BrokerHandle,
) {
    let dispatcher = broker.follow_dispatcher();
    let handoff = dispatcher.clone();
    frame_builder.with_chrome(move |chrome| {
        chrome.window_transfers.set_handoff_link(Box::new(
            crate::shell::window_transfer::BrokerHandoff::new(handoff),
        ));
    });
    frame_builder
        .host_mut()
        .set_follow_bridge(std::rc::Rc::new(move |room, peer_id| {
            dispatcher.set(&room.to_string(), peer_id);
        }));
    if let Some(action_broker) = broker.app_action_broker() {
        // Links the shell opens show their resource through this broker.
        crate::shell::link_open::attach_broker(&action_broker);
        // An app reaching for another app's action is asked about on this
        // session's own sheet, which the overlay host draws and answers.
        action_broker.set_app_action_consent(std::sync::Arc::new(
            crate::shell::app_action_consent::SheetPrompter::session(),
        ));
        frame_builder
            .host_mut()
            .attach_app_action_broker(action_broker);
    }
    frame_builder.host_mut().attach_tracker(broker.tracker());
    // The roster reads who is in which voice channel, and the call this
    // device is in, from the session's calls.
    if let Some(media) = broker.native_media_service() {
        frame_builder.host_mut().set_native_media(media);
    }
    if let Some(realtime) = broker.realtime_fabric_bridge() {
        frame_builder
            .host_mut()
            .set_realtime_continuity_bridge(std::rc::Rc::new(move |action| {
                use crate::shell::overlay_host::RealtimeContinuityUiAction;
                match action {
                    RealtimeContinuityUiAction::Snapshot => {}
                    RealtimeContinuityUiAction::Propose { session, target } => {
                        realtime.propose_to(&session, &target)?;
                    }
                    RealtimeContinuityUiAction::ResolveConsent { session, approved } => {
                        realtime.resolve_consent(&session, approved)?;
                    }
                }
                Ok(realtime.ui_view())
            }));
    }
}

fn attach_peer_presence(
    frame_builder: &mut FrameBuilder,
    broker: &crate::shell::collab_broker::BrokerHandle,
    net: &Arc<crate::shell::network::ShellNetwork>,
) {
    let follow_network = Arc::clone(net);
    frame_builder
        .host_mut()
        .set_follow_presence_bridge(std::rc::Rc::new(move |room, target| {
            let key = ui::presence::PresenceKey::Workspace {
                id: room.to_string(),
            };
            // Browser tabs hear it on the room's live presence.
            follow_network.publish_following(&room.to_string(), target.as_deref());
            let payload = ui::collaboration::PresencePayload::Following { target };
            if let Some(frame) = crate::shell::collab_presence::to_peer(&key, &payload) {
                follow_network.publish_app_presence("tilo-shell".into(), room.to_string(), frame);
            }
        }));
    let desktop_network = Arc::clone(net);
    frame_builder
        .host_mut()
        .set_desktop_presence_bridge(std::rc::Rc::new(move |room, payload| {
            let key = ui::presence::PresenceKey::Workspace {
                id: room.to_string(),
            };
            if let Some(frame) = crate::shell::collab_presence::to_peer(&key, &payload) {
                desktop_network.publish_app_presence("tilo-shell".into(), room.to_string(), frame);
            }
        }));
    let wave_network = Arc::clone(net);
    frame_builder
        .host_mut()
        .set_wave_bridge(std::rc::Rc::new(move |room, target| {
            wave_network.publish_wave(&room.to_string(), target)
        }));
    let visibility_network = Arc::clone(net);
    frame_builder
        .host_mut()
        .set_presence_visibility_bridge(std::rc::Rc::new(move |room, visibility| {
            visibility_network.set_presence_visibility(&room.to_string(), visibility);
        }));
    let heard_network = Arc::clone(net);
    let publish_network = Arc::clone(net);
    frame_builder
        .host_mut()
        .set_live_layout_bridge(crate::shell::overlay_host::LiveLayoutBridge {
            heard: std::rc::Rc::new(move |room| heard_network.live_layouts_of(&room.to_string())),
            publish: std::rc::Rc::new(move |room, layout| {
                publish_network.publish_layout(&room.to_string(), layout);
            }),
        });
    let status_network = Arc::clone(net);
    frame_builder
        .host_mut()
        .set_status_presence_bridge(std::rc::Rc::new(move |room, status| {
            let key = ui::presence::PresenceKey::Workspace {
                id: room.to_string(),
            };
            let payload = ui::collaboration::PresencePayload::Status {
                text: status
                    .as_ref()
                    .map_or_else(String::new, |status| status.text.clone()),
                automatic: status.as_ref().is_some_and(|status| {
                    matches!(status.origin, ui::StatusOrigin::Inferred { .. })
                }),
            };
            if let Some(frame) = crate::shell::collab_presence::to_peer(&key, &payload) {
                status_network.publish_app_presence("tilo-shell".into(), room.to_string(), frame);
            }
        }));
    let tab_network = Arc::clone(net);
    frame_builder
        .host_mut()
        .set_active_tab_presence_bridge(std::rc::Rc::new(move |room, tab, present| {
            let key = ui::presence::PresenceKey::Tab {
                id: format!("{tab:x}"),
            };
            let payload = if present {
                ui::collaboration::PresencePayload::Arrived
            } else {
                ui::collaboration::PresencePayload::Left
            };
            if let Some(frame) = crate::shell::collab_presence::to_peer(&key, &payload) {
                tab_network.publish_app_presence("tilo-shell".into(), room.to_string(), frame);
            }
        }));
    let rooms = {
        let chrome = frame_builder.chrome_handle();
        let chrome = chrome.borrow();
        let tree = chrome.tree();
        tree.desktop
            .workspaces
            .iter()
            .map(|workspace| workspace.room().to_string())
            .collect::<Vec<_>>()
    };
    tracing::info!(
        "collab: subscribed to {} room(s) for peer presence",
        rooms.len()
    );
    frame_builder.set_peer_presence(crate::shell::peer_presence::PeerPresence::attach(
        broker, net, rooms,
    ));
}

fn spawn_session_identity_broker(
    chrome: &crate::shell::chrome_state::Chrome,
) -> Option<crate::identity_broker::IdentityBrokerHandle> {
    if let Err(error) = launcher::services::ensure_services_root() {
        tracing::error!(%error, "identity: cannot create service directory");
        return None;
    }
    let path = launcher::services::identity_socket();
    if service_socket_is_live(&path) {
        tracing::error!(path = %path.display(), "identity: another session owns the socket");
        return None;
    }
    let tree = chrome.borrow().tree_handle();
    match crate::identity_broker::spawn(
        path.clone(),
        tree,
        crate::shell::channel_encryption::ChannelKeyProvider::session(),
    ) {
        Ok(handle) => {
            tracing::info!(path = %path.display(), "identity: broker ready");
            Some(handle)
        }
        Err(error) => {
            tracing::error!(%error, path = %path.display(), "identity: broker failed to start");
            None
        }
    }
}

/// Everything the portal backend needs kept alive for the session — dropping
/// it releases the bus name and tears the object server down.
enum PortalCaptureCommand {
    Screenshot {
        request: crate::portal::desktop_services::ScreenshotRequest,
        reply: std::sync::mpsc::SyncSender<crate::portal::desktop_services::PortalResult<String>>,
    },
    PickColor {
        request_handle: String,
        cancelled: Arc<std::sync::Mutex<HashSet<String>>>,
        reply: std::sync::mpsc::SyncSender<
            crate::portal::desktop_services::PortalResult<(f64, f64, f64)>,
        >,
    },
    CancelScreenshot {
        request_handle: String,
    },
}

/// Bounded cross-thread bridge from the D-Bus runtime to the one compositor
/// thread allowed to read pixels.  It has no ambient file authority: every
/// completed image is republished through the authenticated caller's document
/// subtree before a URI is returned.
#[derive(Clone)]
struct NativeScreenshotService {
    sender: std::sync::mpsc::SyncSender<PortalCaptureCommand>,
    pending: Arc<AtomicBool>,
    cancelled: Arc<std::sync::Mutex<HashSet<String>>>,
}

impl NativeScreenshotService {
    fn request<T>(
        &self,
        command: impl FnOnce(
            std::sync::mpsc::SyncSender<crate::portal::desktop_services::PortalResult<T>>,
        ) -> PortalCaptureCommand,
    ) -> crate::portal::desktop_services::PortalResult<T> {
        let (reply_tx, reply_rx) = std::sync::mpsc::sync_channel(1);
        match self.sender.try_send(command(reply_tx)) {
            Ok(()) => self.pending.store(true, Ordering::Release),
            Err(std::sync::mpsc::TrySendError::Full(_)) => {
                return crate::portal::desktop_services::PortalResult::Unavailable(
                    "the compositor is already handling capture requests".into(),
                );
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                return crate::portal::desktop_services::PortalResult::Unavailable(
                    "the compositor capture bridge has stopped".into(),
                );
            }
        }
        reply_rx
            .recv_timeout(Duration::from_secs(120))
            .unwrap_or_else(|_| {
                crate::portal::desktop_services::PortalResult::Unavailable(
                    "the compositor did not answer the capture request".into(),
                )
            })
    }
}

impl crate::portal::desktop_services::ScreenshotService for NativeScreenshotService {
    fn available_targets(&self) -> u32 {
        // Source selection happens in trusted chrome.  The backend never
        // accepts app-provided geometry or window ids.
        1 | 2 | 4 | 8
    }

    fn screenshot(
        &self,
        request: &crate::portal::desktop_services::ScreenshotRequest,
    ) -> crate::portal::desktop_services::PortalResult<String> {
        let request = request.clone();
        self.request(|reply| PortalCaptureCommand::Screenshot { request, reply })
    }

    fn pick_color(
        &self,
        request_handle: &str,
        _app_id: &str,
    ) -> crate::portal::desktop_services::PortalResult<(f64, f64, f64)> {
        let request_handle = request_handle.to_string();
        self.request(|reply| PortalCaptureCommand::PickColor {
            request_handle,
            cancelled: self.cancelled.clone(),
            reply,
        })
    }

    fn cancel(&self, request_handle: &str) {
        let _ = self
            .sender
            .try_send(PortalCaptureCommand::CancelScreenshot {
                request_handle: request_handle.into(),
            });
        self.pending.store(true, Ordering::Release);
    }

    fn cancel_color(&self, request_handle: &str) {
        if let Ok(mut cancelled) = self.cancelled.lock() {
            cancelled.insert(request_handle.into());
        }
    }
}

struct PortalBackend {
    media_grants: crate::portal::GrantStoreHandle,
    media_prompts: crate::portal::PromptQueueHandle,
    /// The on-screen file chooser's queue, shared with the chooser service
    /// that apps without a portal reach.
    pickers: crate::portal::FilePickerQueue,
    /// Never read: retaining the registration keeps its D-Bus connection, bus
    /// name, and object server alive once registered.
    _server: crate::session_runtime::Registration<crate::portal::ServerHandle>,
    remote_input: std::sync::mpsc::Receiver<crate::network::streaming::RemoteAction>,
    input_capture: crate::portal::InputCaptureCompositorHandle,
    screencast: crate::portal::ScreenCastStreamProvisionerHandle,
    capture_requests: std::sync::mpsc::Receiver<PortalCaptureCommand>,
    capture_pending: Arc<AtomicBool>,
    documents: crate::portal::DocumentStoreHandle,
    interactive_screenshot: std::sync::Mutex<Option<PendingInteractiveScreenshot>>,
}

struct PendingInteractiveScreenshot {
    request_handle: String,
    caller: crate::portal::PortalCaller,
    reply: std::sync::mpsc::SyncSender<crate::portal::desktop_services::PortalResult<String>>,
}

impl PortalBackend {
    fn pump_remote_input(&self, state: &mut State, session_locked: bool) {
        for action in self.remote_input.try_iter().take(512) {
            // A portal session may remain connected across a local lock, but
            // it must never be able to type into the password surface or
            // queue input that is replayed immediately after unlock.
            if !session_locked {
                let _accepted = state.apply_remote_desktop_action(action);
            }
        }
    }

    fn observe_input_capture(&self, input: crate::portal::CapturedInput) -> bool {
        self.input_capture.observe(input)
    }

    fn capture_pending(&self) -> bool {
        self.capture_pending.load(Ordering::Acquire)
    }

    fn drain_capture_requests(
        &self,
        capture: &mut CaptureExecutor,
        frame_builder: &mut FrameBuilder,
        scene: &vello::Scene,
        documents: &crate::portal::DocumentStoreHandle,
    ) {
        use crate::portal::desktop_services::PortalResult;
        for command in self.capture_requests.try_iter().take(8) {
            match command {
                PortalCaptureCommand::Screenshot { request, reply } => {
                    if request.interactive
                        || matches!(
                            request.target,
                            Some(
                                crate::portal::desktop_services::ScreenshotTarget::Area
                                    | crate::portal::desktop_services::ScreenshotTarget::Window
                                    | crate::portal::desktop_services::ScreenshotTarget::ActiveWindow
                            )
                        )
                    {
                        let Ok(mut pending) = self.interactive_screenshot.lock() else {
                            let _ = reply.send(PortalResult::Unavailable(
                                "the screenshot selector is unavailable".into(),
                            ));
                            continue;
                        };
                        if pending.is_some() || !capture.arm_portal_screenshot() {
                            let _ = reply.send(PortalResult::Unavailable(
                                "another screenshot selection is already open".into(),
                            ));
                            continue;
                        }
                        let target = request.target.unwrap_or(
                            crate::portal::desktop_services::ScreenshotTarget::Area,
                        );
                        if !frame_builder.host_mut().open_portal_screenshot_selector(target) {
                            capture.cancel_portal_screenshot();
                            let _ = reply.send(PortalResult::Cancelled);
                            continue;
                        }
                        *pending = Some(PendingInteractiveScreenshot {
                            request_handle: request.request_handle,
                            caller: request.caller,
                            reply,
                        });
                        continue;
                    }
                    // The file is written off the loop; the caller is
                    // answered once it is complete.
                    capture.capture_portal_screenshot(
                        frame_builder,
                        scene,
                        request
                            .target
                            .unwrap_or(crate::portal::desktop_services::ScreenshotTarget::Screen),
                        false,
                        answer_portal_screenshot(documents.clone(), request.caller, reply),
                    );
                }
                PortalCaptureCommand::PickColor {
                    request_handle,
                    cancelled,
                    reply,
                } => {
                    if cancelled
                        .lock()
                        .is_ok_and(|mut cancelled| cancelled.remove(&request_handle))
                    {
                        let _ = reply.send(PortalResult::Cancelled);
                        continue;
                    }
                    let _ = reply.send(match capture.pick_portal_color(frame_builder, scene) {
                        Ok(color) => PortalResult::Success(color),
                        Err(error) => PortalResult::Failed(error),
                    });
                }
                PortalCaptureCommand::CancelScreenshot { request_handle } => {
                    let Ok(mut pending) = self.interactive_screenshot.lock() else {
                        continue;
                    };
                    if pending
                        .as_ref()
                        .is_some_and(|pending| pending.request_handle == request_handle)
                    {
                        capture.cancel_portal_screenshot();
                        if let Some(pending) = pending.take() {
                            let _ = pending.reply.send(PortalResult::Cancelled);
                        }
                    }
                }
            }
        }
        self.capture_pending.store(false, Ordering::Release);
    }

    fn finish_interactive_screenshot(&self, outcome: &CaptureOutcome) -> bool {
        use crate::portal::desktop_services::PortalResult;
        let Ok(mut pending) = self.interactive_screenshot.lock() else {
            return false;
        };
        let Some(pending_request) = pending.take() else {
            return false;
        };
        let result = match outcome {
            CaptureOutcome::PortalScreenshotSaved(host_path) => self
                .documents
                .publish(
                    &pending_request.caller,
                    std::slice::from_ref(host_path),
                    crate::shell::state::FsMode::Read,
                )
                .into_iter()
                .next()
                .and_then(|path| crate::portal::file_chooser::file_uri(&path))
                .map_or_else(
                    || PortalResult::Failed("the document store rejected the screenshot".into()),
                    PortalResult::Success,
                ),
            CaptureOutcome::PortalScreenshotCancelled => PortalResult::Cancelled,
            _ => {
                *pending = Some(pending_request);
                return false;
            }
        };
        let _ = pending_request.reply.send(result);
        true
    }
}

/// Answer a non-interactive portal screenshot request once its file is
/// complete (on the capture worker): the file is republished through the
/// caller's document subtree before its URI is returned.
fn answer_portal_screenshot(
    documents: crate::portal::DocumentStoreHandle,
    caller: crate::portal::PortalCaller,
    reply: std::sync::mpsc::SyncSender<crate::portal::desktop_services::PortalResult<String>>,
) -> impl FnOnce(Result<std::path::PathBuf, String>) + Send + 'static {
    use crate::portal::desktop_services::PortalResult;
    move |saved| {
        let outcome = saved.and_then(|host_path| {
            let published =
                documents.publish(&caller, &[host_path], crate::shell::state::FsMode::Read);
            let path = published
                .into_iter()
                .next()
                .ok_or_else(|| "the document store rejected the screenshot".to_string())?;
            crate::portal::file_chooser::file_uri(&path).ok_or_else(|| {
                "the document store returned a non-absolute screenshot path".to_string()
            })
        });
        let _ = reply.send(match outcome {
            Ok(uri) => PortalResult::Success(uri),
            Err(error) => PortalResult::Failed(error),
        });
    }
}

/// Stand the portal backend up inside the compositor.
///
/// ## Why here and not in a separate daemon
///
/// The portal answers questions about *this session* — workspace, prior
/// grants, the decision now — all live in the `WorkspaceTree` this process
/// owns. The standalone daemon that used to serve it (`shell
/// --portal-server-only`) had none: its `WorkspaceTree::default()` missed
/// every grant lookup, and its `PromptResolver::Channel` drained to nowhere
/// since it had no output to render a dialog on.
///
/// Giving that daemon IPC back to the compositor was rejected: it would
/// split "the permissions" across two processes — the drift
/// `crates/packaging/launcher/src/capabilities.rs` warns against — and it
/// still couldn't draw the dialog, making it a proxy for one process,
/// which is that process. `shell-portal.service`, its D-Bus activation and
/// the `--portal-server-only` mode are removed; the binary now refuses the
/// flag (see `compositor::removed_run_mode_refusal`).
///
/// Registration on the session bus completes in the background. With no
/// session bus (headless rig, no user bus) it fails there, and the compositor
/// still runs, just logging that the portal is unavailable. `None` only when
/// the session runtime itself cannot start.
#[allow(
    clippy::too_many_lines,
    reason = "portal startup wires the complete backend atomically so no partially registered capability can be advertised"
)]
fn start_portal_backend(
    frame_builder: &mut FrameBuilder,
    tree_handle: crate::shell::state::WorkspaceTreeHandle,
    notifications: crate::notifications::NotificationBridge,
    zones: Vec<crate::portal::CaptureZone>,
    inhibit_registry: tilo_power_mgmt::InhibitRegistry,
    selection: crate::compositor::selection::SharedSelection,
) -> Option<PortalBackend> {
    let runtime = match crate::session_runtime::runtime() {
        Ok(rt) => rt,
        Err(e) => {
            tracing::warn!("portal: no runtime ({e}); file open/save will not work");
            return None;
        }
    };

    let mut desktop_services =
        crate::portal::DesktopPortalServices::for_session(tree_handle.clone());
    let grants = crate::portal::GrantStoreHandle::new(tree_handle);
    let prompts = crate::portal::PromptQueueHandle::new(crate::portal::PromptResolver::Channel);
    let pickers = crate::portal::FilePickerQueue::new();
    let documents = crate::portal::documents_for_session();
    let (remote_input_tx, remote_input_rx) = std::sync::mpsc::channel();
    let (capture_tx, capture_rx) = std::sync::mpsc::sync_channel(8);
    let capture_pending = Arc::new(AtomicBool::new(false));
    desktop_services.screenshot = crate::portal::desktop_services::ScreenshotServiceHandle::new(
        Arc::new(NativeScreenshotService {
            sender: capture_tx,
            pending: capture_pending.clone(),
            cancelled: Arc::new(std::sync::Mutex::new(HashSet::new())),
        }),
    );
    let input_capture = crate::portal::InputCaptureCompositorHandle::native(zones);
    let screencast = match crate::portal::RealPipewireProvisioner::new() {
        Ok(provisioner) => {
            crate::portal::ScreenCastStreamProvisionerHandle::new(Arc::new(provisioner))
        }
        Err(error) => {
            tracing::warn!(%error, "portal: PipeWire screen capture is unavailable");
            crate::portal::ScreenCastStreamProvisionerHandle::new(Arc::new(
                crate::portal::UnavailableScreenCastProvisioner::new(error.to_string()),
            ))
        }
    };

    let ctx = crate::portal::PortalContext {
        grants: grants.clone(),
        prompts: prompts.clone(),
        caller_map: crate::portal::CallerMap::new(),
        file_picker: crate::portal::FilePickerSpawnerHandle::new(Arc::new(pickers.clone())),
        screencast_provisioner: screencast.clone(),
        remote_input_injector: crate::portal::RemoteInputInjectorHandle::new(Arc::new(
            crate::portal::ChannelInjector::new(remote_input_tx),
        )),
        input_capture: input_capture.clone(),
        documents: documents.clone(),
        // The spooler: unlike the picker it needs no chrome/seat (a process
        // invocation), and reports no destinations on a machine with no CUPS.
        print_spooler: crate::portal::PrintSpoolerHandle::cups(),
        notifications,
        desktop_services,
        authenticator: None,
    };

    // Registering on the bus is a round trip to the bus daemon; it happens on
    // the session runtime while the compositor starts. Until it completes,
    // nothing can reach the portal, so nothing is queued.
    let described_documents = documents.describe();
    let server = crate::session_runtime::Registration::start(
        runtime.handle(),
        "portal",
        async move {
            let conn = zbus::connection::Builder::session()?.build().await?;
            crate::portal::serve_with_inhibit_and_clipboard(conn, ctx, inhibit_registry, {
                let broker = crate::portal::clipboard::CompositorClipboardBroker::new(selection);
                let wake = broker.wake.clone();
                crate::portal::clipboard::ClipboardBrokerHandle::new(Arc::new(broker))
                    .with_selection_wake(wake)
            })
            .await
        },
        move |server: &crate::portal::ServerHandle| {
            // Losing the name isn't warning-shaped: xdg-desktop-portal would
            // route FileChooser elsewhere, so every Open would hit a backend
            // with no session state — logged at journal level.
            if server.owns_name() {
                tracing::info!(
                    "portal: serving FileChooser+ScreenCast+RemoteDesktop+Access on {}; documents via {described_documents}",
                    crate::portal::BUS_NAME,
                );
            } else {
                tracing::error!(
                    "portal: another process already owns {} — this session's file \
                     dialog will never be reached. Check `busctl --user status {}` \
                     for the owner and disable whatever started it.",
                    crate::portal::BUS_NAME,
                    crate::portal::BUS_NAME
                );
            }
        },
    );

    // The queues are useless until drained; attaching them to the overlay host
    // here (not at construction) keeps "portal up" and "prompts reach a screen" one fact.
    frame_builder
        .host_mut()
        .set_portal_queues(prompts.clone(), pickers.clone());

    if !documents.reaches_confined_apps() {
        // Say it out loud: a sandboxed app getting ENOENT after picking a file
        // looks like an app bug, and the journal is the only place to connect the dots.
        tracing::warn!(
            "portal: the document store cannot reach sandboxed apps — a file \
             picked by a bwrap'd app will not be readable by it. Unconfined \
             apps are unaffected."
        );
    }
    Some(PortalBackend {
        media_grants: grants,
        media_prompts: prompts,
        pickers,
        _server: server,
        remote_input: remote_input_rx,
        input_capture,
        screencast,
        capture_requests: capture_rx,
        capture_pending,
        documents,
        interactive_screenshot: std::sync::Mutex::new(None),
    })
}

/// Construct the backend selected by `--backend drm|winit|headless` — split
/// out of [`run`] to keep that function's line count down. `expect`-free:
/// same fatal-on-`Err` posture as the rest of `run`'s startup, just spelled
/// as a logged `eprintln!` + `process::exit(1)` instead of a panic, since
/// this crate builds with `panic = "abort"`.
#[allow(
    clippy::exit,
    reason = "only ever called from `run`'s own top-level entry-point setup; a fatal backend \
              init failure has always ended the process here"
)]
fn build_backend(kind: BackendKind) -> Box<dyn Backend> {
    match kind {
        BackendKind::Winit => {
            let mut b = match WinitBackend::new() {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("tilo compositor: --backend winit failed: {e}");
                    std::process::exit(1);
                }
            };
            // Match the FrameBuilder's configured theme so the host window
            // clears to chrome background before the first frame paints.
            b.set_base_color(ui::theme::Theme::configured().tokens.colors.surfaces.base);
            Box::new(b)
        }
        BackendKind::Drm => match DrmBackend::new() {
            Ok(b) => Box::new(b),
            Err(e) => {
                eprintln!("tilo compositor: --backend drm failed: {e}");
                std::process::exit(1);
            }
        },
        BackendKind::Headless => {
            // Initial size from `TILO_HEADLESS_SIZE` (shots-default fallback), keeping
            // headless env-var-driven. Skip `set_base_color`: no window to clear.
            let outputs = match headless::resolve_initial_outputs() {
                Ok(outputs) => outputs,
                Err(e) => {
                    eprintln!("tilo compositor: --backend headless: {e}");
                    std::process::exit(1);
                }
            };
            match HeadlessBackend::with_outputs(outputs) {
                Ok(b) => Box::new(b),
                Err(e) => {
                    eprintln!("tilo compositor: --backend headless failed: {e}");
                    std::process::exit(1);
                }
            }
        }
    }
}

/// Widen a `(u32, u32)` pixel size to `(i32, i32)` for `State::output_size`.
/// Clamped rather than wrapped: a client-visible fullscreen target derived
/// from a bogus size is far less harmful than the sign flip a bare `as`
/// cast would produce, and no real display size comes anywhere near `i32::MAX`.
/// A headless session has no seat anyone sits at: blanking, locking or
/// suspending it on idle protects nothing, and a lock strands the agents
/// driving it, since nobody there can type the unlock password. Explicit
/// locking stays available.
fn idle_policy_for_backend(
    policy: tilo_power_mgmt::PowerPolicy,
    headless: bool,
) -> tilo_power_mgmt::PowerPolicy {
    if !headless {
        return policy;
    }
    tilo_power_mgmt::PowerPolicy {
        idle_blank: None,
        idle_lock: None,
        idle_suspend: None,
        ..policy
    }
}

/// Derive the idle policy from the session's settings and hand it to the idle
/// state machine and the power service, so a write to the settings store
/// changes what the running session does. The profile is the one the power
/// service last read, not a fresh ask: `powerprofilesctl` is a process start
/// the frame thread would wait on.
fn reapply_power_policy(
    session: &mut SessionManager,
    power_bridge: &tilo_power_mgmt::PowerBridge,
    headless: bool,
) -> tilo_power_mgmt::PowerPolicy {
    let policy = idle_policy_for_backend(
        tilo_power_mgmt::config::load_policy_for_profile(
            power_bridge.active_profile().as_deref(),
            power_bridge.on_battery(),
        ),
        headless,
    );
    session.set_blank_timeout(policy.idle_blank);
    session.set_lock_timeout(policy.idle_lock);
    session.set_suspend_timeout(policy.idle_suspend);
    power_bridge.update_policy(policy);
    policy
}

#[cfg(test)]
mod idle_policy_tests {
    use super::idle_policy_for_backend;
    use std::time::Duration;

    #[test]
    fn only_a_headless_session_drops_idle_blank_lock_and_suspend() {
        let configured = tilo_power_mgmt::PowerPolicy {
            idle_blank: Some(Duration::from_secs(60)),
            idle_lock: Some(Duration::from_secs(300)),
            idle_suspend: Some(Duration::from_secs(900)),
            ..tilo_power_mgmt::config::default_policy()
        };
        let seated = idle_policy_for_backend(configured, false);
        assert_eq!(seated.idle_lock, configured.idle_lock);
        assert_eq!(seated.idle_blank, configured.idle_blank);
        assert_eq!(seated.idle_suspend, configured.idle_suspend);
        let headless = idle_policy_for_backend(configured, true);
        assert_eq!(
            (
                headless.idle_blank,
                headless.idle_lock,
                headless.idle_suspend
            ),
            (None, None, None)
        );
        assert_eq!(headless.lid_close_action, configured.lid_close_action);
    }
}

#[cfg(test)]
mod live_power_settings_tests {
    use super::reapply_power_policy;
    use crate::compositor::backend::drm::input_config::SettingsChange;
    use crate::compositor::idle::{IdleConfig, SessionManager, SessionTransition};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// A write through the settings store reaches the running session: the
    /// compositor learns of it from the live document, derives the idle
    /// policy from that same document and the idle state machine stops
    /// locking. Reading the `settings.json` copy instead lost this race,
    /// because the session's source publishes a change before it has
    /// rewritten the copy.
    #[test]
    #[serial_test::serial]
    fn lock_after_never_stops_auto_lock_in_the_running_session() {
        let _home = tilo_test_env::TestHome::new().unwrap();
        let paths = settings_store::SessionPaths::configured().unwrap();
        let session_settings = Arc::new(settings_store::SessionSettings::open(paths).unwrap());
        let settings =
            crate::shell::test_support::install_settings(session_settings.host_view().unwrap());
        let live = settings_store::live::LiveSettings::start(|| {});
        let mut follower = settings_store::live::SectionFollower::new(&live);
        let bridge = tilo_power_mgmt::PowerBridge::new();
        let t0 = Instant::now();
        let fresh = || {
            SessionManager::new(
                IdleConfig::from_env_values(Some("0"), Some("300"), Some("0")),
                t0,
            )
        };

        // Phase `the_default_locks_after_five_minutes`.
        {
            let mut session = fresh();
            reapply_power_policy(&mut session, &bridge, false);
            assert!(
                session
                    .tick(t0 + Duration::from_secs(6 * 60))
                    .contains(&SessionTransition::Locked)
            );
        }

        // Phase `setting_it_to_never_is_applied_live`.
        {
            let mut running = fresh();
            let before = live.generation();
            settings_store::SectionWrite::new()
                .set(&settings_store::power::LOCK_MINUTES, &0)
                .commit_by(&settings_store::ChangeOrigin::action("settings.power"))
                .expect("the setting is stored");

            let snapshot = live.wait_for_change(before, tilo_test_env::hang_bound());
            assert_eq!(snapshot.section("power")["lock_minutes"], 0);
            assert!(
                SettingsChange::from(follower.changed_sections(&live)).power(),
                "the write names the power section"
            );
            // What the compositor does on that edge.
            reapply_power_policy(&mut running, &bridge, false);
            assert!(
                !running
                    .tick(t0 + Duration::from_secs(24 * 60 * 60))
                    .contains(&SessionTransition::Locked),
                "never means the session does not lock on its own"
            );
            assert!(!running.is_locked());
            assert_eq!(
                running.request_lock(Instant::now()),
                vec![SessionTransition::Locked],
                "locking by hand still works"
            );
        }
        drop(settings);
    }
}

fn size_to_signed(size: (u32, u32)) -> (i32, i32) {
    (
        i32::try_from(size.0).unwrap_or(i32::MAX),
        i32::try_from(size.1).unwrap_or(i32::MAX),
    )
}

/// Bind the primary public Wayland socket (`wayland-1..32`, first free
/// name), print the two lines a human or test relies on, and publish the
/// name to `$XDG_RUNTIME_DIR/tilo-session/wayland-display`. Split out of
/// [`run`] to keep that function's line count down.
#[allow(
    clippy::exit,
    reason = "only ever called from `run`'s own top-level entry-point setup; a fatal socket-bind \
              failure has always ended the process here"
)]
fn bind_and_publish_wayland_socket() -> (
    ListeningSocketSource,
    std::ffi::OsString,
    Option<UnlinkOnDrop>,
) {
    let listening_socket = match ListeningSocketSource::new_auto() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("tilo compositor: bind listening wayland socket failed: {e}");
            std::process::exit(1);
        }
    };
    let socket_name = listening_socket.socket_name().to_os_string();
    // A smoke test asserts on the exact text of this line, quotes included —
    // `OsString` has no `Display` impl, and `extract_socket_name` in
    // `tests/compositor_test.rs` strips the Debug form's surrounding `"`.
    #[allow(
        clippy::unnecessary_debug_formatting,
        reason = "OsString has no Display impl, and the quoted Debug form is a strict parsing \
                  contract in tests/compositor_test.rs::extract_socket_name — switching to \
                  `to_string_lossy()` would drop the quotes that test strips"
    )]
    let banner = format!("tilo compositor listening on {socket_name:?}");
    eprintln!("{banner}");
    // The two lines below are for a human who just ran `cargo run -p shell` —
    // publishing to a file too lets the next command read it instead of copying from a log.
    let display_file = publish_wayland_display(&socket_name);
    eprintln!(
        "  WAYLAND_DISPLAY={} — e.g. `WAYLAND_DISPLAY={} cargo run -p terminal`",
        socket_name.to_string_lossy(),
        socket_name.to_string_lossy(),
    );
    if let Some(guard) = display_file.as_ref() {
        eprintln!("  also written to {}", guard.0.display());
    }
    (listening_socket, socket_name, display_file)
}

#[cfg(test)]
fn backend_nested_loop_uses_edge_wake(source: &str) -> bool {
    let Some(start) = source.rfind("let backend_event_wake = if let Some(fd) = backend.event_fd()")
    else {
        return false;
    };
    let tail = &source[start..];
    let Some(end) = tail.find("let (capture_ping, capture_ping_source)") else {
        return false;
    };
    let registration = &tail[..end];
    registration.contains("Generic::new(fd, Interest::READ, Mode::Edge)")
        && !registration.contains("Generic::new(fd, Interest::READ, Mode::Level)")
}

/// Entry point for the smithay-on-Wayland compositor. Fatal setup errors are
/// logged and end the process immediately, but an ordinary logout returns so
/// backend/libseat/KMS resources unwind through RAII before the display manager
/// acquires the seat again.
#[allow(
    clippy::exit,
    reason = "this is the compositor's real process entry point (invoked directly from `main`); \
              every exit call below is a fatal, logged setup failure, the same posture smithay's \
              own examples take"
)]
#[allow(
    clippy::too_many_lines,
    reason = "the per-frame dispatch loop below is one continuous tick over ~30 pieces of live \
              session state (backend, chrome, session/idle/lock, metrics, test-control); \
              mechanically splitting it into mutable-reference-passing phase functions with no \
              compiler available to verify the transformation risks a subtle correctness bug in \
              a panic-abort, session-critical entry point. Setup that could be safely verified by \
              inspection (backend construction, the wayland socket bind+publish) is factored out \
              into `build_backend`/`bind_and_publish_wayland_socket` below"
)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "rooms and here below are each fully chained from the workspace tree read guard; \
              nothing to drop earlier without cloning the tree data out for no real gain, and \
              the block already ends right after both are computed"
)]
pub fn run() {
    // This thread answers input and flips frames: a settings write it makes
    // must not wait for the disk (lock, syncs, rename). Readers see it at
    // once and the settings worker makes it durable; session exit flushes it.
    settings_store::defer_writes_on_this_thread();

    // Arm recovery before creating services, clients or a renderer.
    let recovery = super::session_recovery::SessionRecovery::from_env().unwrap_or_else(|error| {
        eprintln!("tilo compositor: cannot establish session lock recovery: {error}");
        std::process::exit(1);
    });
    // Startup frees its scratch (renderer warm-up, identity, network, search) over its first
    // seconds, never all at once, and glibc keeps it: one trim once it is over.
    crate::shell::heap_trim::request_after(crate::shell::heap_trim::STARTUP_SETTLED);
    // Calloop drives the whole compositor; smithay is callback-oriented and
    // expects every wakeup to fan out from a single `EventLoop::<State>`.
    let mut event_loop: EventLoop<'static, State> = match EventLoop::try_new() {
        Ok(el) => el,
        Err(e) => {
            eprintln!("tilo compositor: create calloop event loop failed: {e}");
            std::process::exit(1);
        }
    };

    let (termination, termination_source) =
        super::session_termination::SessionTermination::source().unwrap_or_else(|error| {
            eprintln!("tilo compositor: cannot establish ordered signal teardown: {error}");
            std::process::exit(1);
        });
    event_loop
        .handle()
        .insert_source(termination_source, |_, fd, _| {
            let mut value = [0_u8; 8];
            // SAFETY: the buffer is writable for its stated length; fd is the
            // live nonblocking eventfd owned by this event source.
            unsafe {
                libc::read(fd.as_raw_fd(), value.as_mut_ptr().cast(), value.len());
            }
            Ok(PostAction::Continue)
        })
        .unwrap_or_else(|error| {
            eprintln!("tilo compositor: cannot observe termination signals: {error}");
            std::process::exit(1);
        });

    // The `Display` owns the wayland-server protocol state. Its poll fd is an
    // epoll instance containing the individual client sockets. Register a
    // duplicate as an edge-triggered *wake token* below, while retaining
    // ownership and doing all actual protocol dispatch here. The edge is safe
    // because `dispatch_clients` drains the inner epoll fully; level-triggered
    // epoll nesting can consume outer readiness without draining the inner
    // client event and then strand a request. If registration is unavailable,
    // retain the bounded 4 ms service fallback rather than breaking clients.
    let mut display: Display<State> = match Display::new() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("tilo compositor: create wayland display failed: {e}");
            std::process::exit(1);
        }
    };
    let display_handle = display.handle();

    let wayland_client_wake = {
        let duplicate =
            unsafe { libc::fcntl(display.as_fd().as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
        if duplicate < 0 {
            tracing::warn!(
                error = %std::io::Error::last_os_error(),
                "could not duplicate Wayland display wake fd; using bounded polling fallback"
            );
            false
        } else {
            // SAFETY: F_DUPFD_CLOEXEC returned a new owned descriptor.
            let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(duplicate) };
            match event_loop
                .handle()
                .insert_source(Generic::new(fd, Interest::READ, Mode::Edge), |_, _, _| {
                    Ok(PostAction::Continue)
                }) {
                Ok(_) => true,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        "could not register Wayland display wake fd; using bounded polling fallback"
                    );
                    false
                }
            }
        }
    };

    // The session's one settings document: read once per change on its own
    // thread and shared, by generation, with every subsystem that follows a
    // setting (input devices, switch control, rotation lock, automatic
    // brightness, input methods, continuity policy). A change wakes an idle
    // loop through this ping; nothing here reads the file between changes.
    let live_settings = {
        let wake = match calloop::ping::make_ping() {
            Ok((ping, source)) => match event_loop.handle().insert_source(source, |(), (), _| {}) {
                Ok(_) => Some(ping),
                Err(error) => {
                    tracing::warn!(%error, "could not register the settings wake source; an idle desktop applies Settings changes on its next wake");
                    None
                }
            },
            Err(error) => {
                tracing::warn!(%error, "could not create the settings wake source; an idle desktop applies Settings changes on its next wake");
                None
            }
        };
        settings_store::live::LiveSettings::start(move || {
            if let Some(ping) = &wake {
                ping.ping();
            }
        })
    };
    if let Some(reason) = live_settings.degraded() {
        tracing::warn!(%reason, "settings changes may arrive late");
    }
    // Continuity checks run per fabric message on this loop: from here on
    // they read this snapshot, never the settings document.
    crate::shell::continuity_policy::follow(&live_settings);
    #[cfg(target_os = "linux")]
    crate::shell::device_vpn::follow(&live_settings);
    #[cfg(target_os = "linux")]
    crate::shell::device_vpn::profile_block::start(&live_settings);
    spawn_auto_brightness_worker(live_settings.clone());

    // `--backend drm|winit` selects the native backend implementation.
    let backend_kind = parse_backend_from_args();
    let mut backend: Box<dyn Backend> = build_backend(backend_kind);
    // DRM owns a nested calloop for libinput/libseat/udev. Register its poll
    // fd in the compositor loop so hardware input wakes us immediately rather
    // than waiting for a periodic backend poll (which made the cursor visibly
    // lag by roughly the old 50 ms interval).
    let backend_event_wake = if let Some(fd) = backend.event_fd() {
        match event_loop
            .handle()
            // `event_fd` is the poll fd of the DRM backend's nested calloop.
            // Treat it as a wake *edge*, then let `Backend::poll_events`
            // drain that inner loop at the start of the next iteration.  A
            // level-triggered registration can remain readable after the
            // inner dispatch consumed its source, making the outer calloop
            // return immediately forever.  The resulting idle storm was
            // measured at ~12,000 iterations per two seconds while no frame
            // was dirty and the presenter counters did not move.
            .insert_source(Generic::new(fd, Interest::READ, Mode::Edge), |_, _, _| {
                Ok(PostAction::Continue)
            }) {
            Ok(_) => true,
            Err(error) => {
                tracing::error!(%error, "could not register backend event wake fd");
                false
            }
        }
    } else {
        !backend.needs_idle_poll()
    };
    // Move presentation onto its own thread where the backend supports it.
    // Headless declines: its bytes are read back synchronously by `snapshot`.
    // Spawned as soon as the backend exists: the render thread builds its
    // renderer while the rest of the compositor starts, not when the first
    // frame arrives.
    let mut present_pipe = backend.take_presenter().map(|p| {
        eprintln!("tilo compositor: presenting on a dedicated render thread");
        // Wake the loop when a frame retires — otherwise a tick that declined to
        // paint for lack of pipe capacity would park out the whole interval, halving throughput.
        let (ping, ping_source) = match calloop::ping::make_ping() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("tilo compositor: create present-retire ping failed: {e}");
                std::process::exit(1);
            }
        };
        if let Err(e) = event_loop.handle().insert_source(ping_source, |(), (), _| {
            // Waking is the whole effect: the loop re-evaluates its paint gate next iteration and now finds room.
        }) {
            eprintln!("tilo compositor: insert present-retire ping source failed: {e}");
            std::process::exit(1);
        }
        present::PresentPipe::spawn(p, Box::new(move || ping.ping()))
    });
    let (capture_ping, capture_ping_source) = match calloop::ping::make_ping() {
        Ok(pair) => pair,
        Err(error) => {
            tracing::error!(%error, "could not create foreign capture completion wake source");
            std::process::exit(1);
        }
    };
    if let Err(error) = event_loop
        .handle()
        .insert_source(capture_ping_source, |(), (), _| {})
    {
        tracing::error!(%error, "could not register foreign capture completion wake source");
        std::process::exit(1);
    }
    crate::compositor::foreign::install_completion_ping(&capture_ping);
    // Convertibles: the accelerometer turns the built-in panel. The ping
    // only wakes the loop; the change itself is drained below.
    let device_rotation = match calloop::ping::make_ping() {
        Ok((ping, source)) => match event_loop.handle().insert_source(source, |(), (), _| {}) {
            Ok(_) => crate::compositor::device_rotation::spawn(
                move || ping.ping(),
                live_settings.clone(),
            ),
            Err(error) => {
                tracing::warn!(%error, "could not register the device rotation wake source");
                None
            }
        },
        Err(error) => {
            tracing::warn!(%error, "could not create the device rotation wake source");
            None
        }
    };

    // Accept readiness is also part of the outer loop. Accepted streams get
    // their own source below; this source prevents the first request from
    // waiting for an unrelated compositor deadline.

    // Per-frame scene production, shared with offscreen tooling via
    // `FrameBuilder` — chrome in the host window is byte-identical to `cargo run --bin shots`.
    //
    // The shell lays out the logical desktop spanning every output; each
    // output then draws its part into its own sub-rect of the scene.
    let initial_size = match logical_geometry(backend.outputs()) {
        (0, 0) => (1600, 900),
        size => size,
    };
    let peer_sync_requested = crate::compositor::parse_peer_sync_from_args();
    // A device enrolled without the root (a node's session) completes its
    // side of the join first, so the store below finds the person it holds.
    if let Err(error) = crate::shell::identity_provisioning::complete_enrolled_device() {
        tracing::warn!(%error, "the enrolled device could not take up its identity");
    }
    let identity_store = match crate::shell::identity_provisioning::DiskIdentityStore::from_env() {
        Ok(store) => store,
        Err(error) => {
            tracing::error!(
                %error,
                "cannot resolve the user identity directory; refusing to start an identity-less desktop"
            );
            std::process::exit(1);
        }
    };
    // Developer E2E harnesses drive an already-booted desktop through the
    // debug-only control socket. Their deliberately empty XDG roots are test
    // isolation, not a first-login fixture; letting OOBE capture input would
    // make every real-client input test time out before its first keypress.
    // Release builds ignore TILO_TEST_CONTROL entirely via DEV_ENV_OVERRIDES.
    // A hosted machine an agent drives has no person at first login either;
    // its identity is provisioned without the ceremony.
    // A dev image's login wrapper hands a person's own login the socket too,
    // and says so: that login keeps first use.
    let test_control_session = test_control_path(
        crate::compositor::dev_env::DEV_ENV_OVERRIDES,
        std::env::var_os(TEST_CONTROL_ENV),
    )
    .is_some()
        && std::env::var_os(TEST_CONTROL_FIRST_USE_ENV).is_none_or(|value| value != "keep")
        || agent_control_path(
            cfg!(feature = "agent-control"),
            backend_kind == BackendKind::Headless,
            std::env::var_os(AGENT_CONTROL_ENV),
        )
        .is_some();
    let progress_pending = identity_store
        .path()
        .parent()
        .is_some_and(crate::onboarding_view::has_pending_progress);
    // A valid identity does not acknowledge or consume an installed setup.
    // Keep trusted first-use until its actual prepared/import/migration owner
    // has observed the necessary completions.
    let prepared_pending = crate::onboarding_prepared::session_home()
        .is_some_and(|home| crate::onboarding_prepared::session_pending(&home));
    // `main` probes the existing schema-2 record before entering the
    // compositor. If Secret Service is locked, that probe must not terminate
    // the login: trusted recovery chrome owns the next step. This marker is
    // process-local and cannot make an identity valid or bypass verification.
    let identity_recovery =
        std::env::var_os(crate::shell::identity_provisioning::IDENTITY_RECOVERY_ENV)
            .is_some_and(|value| value == "1");
    let identity_missing =
        crate::shell::identity_provisioning::needs_first_run_flow(&identity_store);
    // The normal admin socket remains the real seat's. An owner can request
    // a separate semantic endpoint for headless work without enabling the
    // development input socket or changing first-use and lock handling.
    let headless_owner_control = backend_kind == BackendKind::Headless
        && std::env::args().any(|argument| argument == "--headless-owner-control");
    let mut admin_control_due = (backend_kind == BackendKind::Drm || headless_owner_control)
        .then(|| identity_store.path().parent().map(Path::to_path_buf))
        .flatten();
    // The session's agent door, bound beside it once first login is
    // complete: the real seat's always, a development compositor's only
    // where its developer named one.
    let mut agent_door_due = crate::compositor::agent_door::door_path(
        backend_kind == BackendKind::Drm,
        crate::compositor::dev_env::DEV_ENV_OVERRIDES,
        std::env::var_os(crate::compositor::agent_door::AGENT_DOOR_ENV),
        std::env::var_os("XDG_RUNTIME_DIR"),
    );
    let onboarding_identity_dir = if should_activate_first_use(
        identity_missing,
        progress_pending || prepared_pending,
        identity_recovery,
        test_control_session,
    ) {
        if let Some(identity_dir) = identity_store
            .path()
            .parent()
            .map(std::path::Path::to_path_buf)
        {
            Some(identity_dir)
        } else {
            tracing::error!(
                path = %identity_store.path().display(),
                "identity path has no parent; refusing to start an identity-less desktop"
            );
            std::process::exit(1);
        }
    } else {
        None
    };
    // A first-login compositor must not create or advertise a transport
    // identity before the identity ceremony completes. Keep a one-shot
    // deferred start so that completion brings this session online, rather
    // than requiring the person to restart the desktop they just set up.
    // An identity whose certificate the clock refuses is intact: the
    // session starts without first-use setup or recovery, says so, and
    // starts its network and broker once the clock admits the certificate,
    // as a finished first login does.
    let identity_clock_refusal = onboarding_identity_dir
        .is_none()
        .then(crate::shell::identity_provisioning::identity_clock_refusal)
        .flatten();
    let identity_pending = onboarding_identity_dir.is_some() || identity_clock_refusal.is_some();
    let enable_peer_sync = peer_sync_requested && !identity_pending;
    let mut deferred_peer_sync = peer_sync_requested && identity_pending;
    // Without peer sync the broker is local-only, but it still reads the
    // session's identity and device key once when it starts. Start it after
    // first-login setup too, or it keeps the pre-identity view all session.
    let mut deferred_local_broker = !peer_sync_requested && identity_pending;
    if deferred_peer_sync {
        tracing::info!("peer-sync: waiting for first-login identity setup to complete");
    }

    // The desktop's own state, loaded from disk not bootstrapped fresh — room
    // identity, owner and capability list all have to survive a restart.
    let LoadedSession {
        tree: tree_handle,
        peer_sync_possible: enable_peer_sync,
        notice: state_notice,
    } = load_session_tree(enable_peer_sync);
    let mut state = State::new_with_workspace_tree(
        event_loop.handle(),
        display_handle.clone(),
        tree_handle.clone(),
    );
    // Advertise the scale the Settings app wrote.
    state.scale = crate::compositor::state::display_scale_from(&live_settings.snapshot().document);
    // Point `ui`'s clipboard at the compositor's selection mirror — everything
    // hosted here isn't a Wayland client, so without this they'd share no buffer.
    crate::compositor::selection::install_host_services(
        state.selection_store(),
        tree_handle.clone(),
    );
    // Building the desktop mounts the hosted tabs it restores, which connect
    // to the collaboration broker and identity service bound below: they
    // wait for them rather than find no socket and stay offline.
    crate::shell::in_process::set_session_services(
        crate::shell::in_process::SessionServices::Starting,
    );
    let mut frame_builder = FrameBuilder::with_handle_and_records(
        tree_handle.clone(),
        enable_peer_sync,
        initial_size,
        crate::shell::session_record_store(),
    );
    frame_builder
        .host_mut()
        .set_nested_shortcuts(backend_kind == BackendKind::Winit);
    state
        .global_shortcuts
        .set_shortcut_config(frame_builder.host().shortcut_config().clone());
    frame_builder.set_strip_pan_presenter(
        backend.pixel_aligned_strip_pan(),
        backend.composites_strip_layers(),
    );
    if let Some(width) = backend.strip_layer_canvas() {
        frame_builder.keep_strip_canvases(width);
    }
    frame_builder.set_scroll_copy_presenter(backend.copies_scrolls());
    // A peer stream's picture finishes on a media thread after its frame's
    // push returned (the loop never waits for a decode); this wakes the loop
    // to show it, with no polling while a stream plays.
    match calloop::ping::make_ping() {
        Ok((ping, source)) => match event_loop.handle().insert_source(source, |(), (), _| {}) {
            Ok(_) => frame_builder
                .host_mut()
                .set_peer_picture_wake(Arc::new(move || ping.ping())),
            Err(error) => {
                tracing::warn!(%error, "could not register the peer picture wake source; peer streams poll for decoded pictures");
            }
        },
        Err(error) => {
            tracing::warn!(%error, "could not create the peer picture wake source; peer streams poll for decoded pictures");
        }
    }
    // An app's History descriptions come from a worker thread; this wakes
    // the loop to send them.
    match calloop::ping::make_ping() {
        Ok((ping, source)) => match event_loop.handle().insert_source(source, |(), (), _| {}) {
            Ok(_) => frame_builder
                .host_mut()
                .set_history_describer_wake(Arc::new(move || ping.ping())),
            Err(error) => {
                tracing::warn!(%error, "could not register the history description wake source; descriptions are sent on the next wake");
            }
        },
        Err(error) => {
            tracing::warn!(%error, "could not create the history description wake source; descriptions are sent on the next wake");
        }
    }
    let clipboard_store = state.selection_store();
    frame_builder
        .host_mut()
        .set_clipboard_manager_bridge(std::rc::Rc::new(move |action| {
            use crate::shell::overlay_host::ClipboardManagerAction;
            match action {
                ClipboardManagerAction::Snapshot => {}
                ClipboardManagerAction::Activate(id) => {
                    let _ =
                        crate::compositor::selection::activate_history_item(&clipboard_store, id);
                }
                ClipboardManagerAction::SetPinned { id, pinned } => {
                    let _ = crate::compositor::selection::pin_history_item(
                        &clipboard_store,
                        id,
                        pinned,
                    );
                }
                ClipboardManagerAction::Remove(id) => {
                    let _ = crate::compositor::selection::remove_history_item(&clipboard_store, id);
                }
                ClipboardManagerAction::ClearUnpinned => {
                    crate::compositor::selection::clear_clipboard_history(&clipboard_store);
                }
            }
            crate::compositor::selection::clipboard_history(&clipboard_store)
        }));
    // A fresh machine has no display server before `shell`, so first-login
    // cannot be a standalone winit app or a pre-graphical user unit. Host the
    // ceremony in trusted compositor chrome and withhold every desktop
    // affordance until it has produced a real identity.
    if let Some(identity_dir) = onboarding_identity_dir {
        if crate::compositor::parse_install_mode_from_args()
            == crate::compositor::InstallMode::HostDesktop
        {
            eprintln!("tilo compositor: showing host-desktop identity setup from the first frame");
            if identity_recovery {
                frame_builder.enable_host_desktop_recovery_onboarding(identity_dir);
            } else {
                frame_builder.enable_host_desktop_onboarding(identity_dir);
            }
        } else if identity_recovery {
            eprintln!("tilo compositor: showing existing-identity recovery from the first frame");
            frame_builder.enable_recovery_onboarding(identity_dir);
        } else {
            eprintln!("tilo compositor: showing full first-login setup from the first frame");
            frame_builder.enable_onboarding(identity_dir);
        }
    }
    frame_builder.follow_settings(live_settings.clone());
    frame_builder.enable_configured_theme();
    let mut capture_executor = CaptureExecutor::default();
    let floating_surface_service = match FloatingSurfaceService::start() {
        Ok(service) => Some(service),
        Err(error) => {
            tracing::warn!(%error, "shell-ui floating-surface service unavailable");
            None
        }
    };
    let mut external_floating_ids: HashMap<u64, Vec<String>> = HashMap::new();
    let mut window_appearances = crate::compositor::floating_service::AppearanceDelivery::default();
    // Whether recordings can take sound is asked of PipeWire below, off the
    // loop (three processes, up to 5 s each); until it answers, capture
    // offers no audio tracks.
    if let Some(notice) = &state_notice {
        crate::shell::notification::announce_state_notice(&frame_builder.chrome_handle(), notice);
    }
    // The scene spans every output but a window's width is a fraction of its
    // monitor, so strips get real per-display geometry, not the bounding box.
    frame_builder.set_output_geometry(output_geometry(backend.outputs()));
    // Room record durability and discovery are switched on here, beside the
    // rest of the durable state, so a test/screenshot compositor touches nobody's.
    frame_builder
        .host()
        .room_records()
        .borrow_mut()
        .attach_store();

    // The xdg-desktop-portal backend, served from *this* process on the tree
    // it owns — see `start_portal_backend` for why not the separate daemon.
    let notif_bridge = crate::notifications::NotificationBridge::new();
    frame_builder.with_chrome(|st| {
        st.set_notification_bridge(notif_bridge.clone());
        // Clicking an app's notification brings the app forward: each action
        // the person activates is handed on with a token this loop mints.
        st.mint_notification_activation_tokens();
        if let Some(store) =
            crate::shell::notification_history::NotificationHistoryStore::for_user()
        {
            st.attach_notification_history_store(store);
        }
    });
    if let Some(store) = crate::shell::emoji_recents::FileRecentsStore::for_user() {
        frame_builder.host_mut().attach_emoji_recents_store(store);
    }
    let inhibit_registry = tilo_power_mgmt::InhibitRegistry::new();
    frame_builder
        .host_mut()
        .set_session_inhibitors(inhibit_registry.clone());
    let portal_backend = start_portal_backend(
        &mut frame_builder,
        tree_handle.clone(),
        notif_bridge.clone(),
        input_capture_zones(backend.outputs()),
        inhibit_registry.clone(),
        state.selection_store(),
    );

    // Peer sync belongs to this compositor, the one that boots as the Tilo
    // session. Building it blocks (a runtime, the device key, the endpoint
    // bind, identity reads that may save), so it starts off the loop once
    // the loop runs (`network_start` below), the way first login starts it;
    // the loop installs it, then the broker over it.
    let mut peer_sync: Option<Arc<crate::shell::network::ShellNetwork>> = None;
    // Started once, with the first network the loop installs: a thread that
    // could not start is reported, not retried every turn.
    let mut device_fabric_worker: Option<
        crate::shell::device_fabric_runtime::worker::FabricWorker<
            crate::shell::device_fabric_runtime::FabricTurn,
        >,
    > = None;
    let mut device_fabric_tried = false;
    let mut spatial_control: Option<SpatialControlSession> = None;
    let mut spatial_pointer_parked = false;
    // Loop iterations since the last quiet trim was asked for.
    let mut ticks_since_trim: u32 = 0;
    // When the presenter's strip layers are drawn again, once the desktop
    // rests after a frame that changed it ([`LAYER_WARM_SETTLE`]).
    let mut layer_warm_at: Option<Instant> = None;
    // The session's CRDT fan-out broker. Lives in this process, not a unit
    // of its own: a listener + fanout table over `ShellNetwork`, since the
    // shell already owns the network stack, identity and permissions. Held
    // to the end of `run()` so the socket outlives every app and unlinks with
    // the session.
    //
    // Binding it opens its invocation journal, its MLS vault and every
    // channel's sealed state, so it binds on the session-start worker
    // (`broker_start` below) and the loop installs it when it is up. An app
    // resolves its collab descriptor once at spawn, so launches are held
    // until then, and restored hosted tabs wait for it (`Starting`).
    // Onboarding withholds every app, so defer its broker too. That lets the
    // freshly authenticated transport be its outbound route as soon as the
    // person completes setup, with no stale local-only broker to replace.
    let mut collab_broker: Option<crate::shell::collab_broker::BrokerHandle> = None;
    let mut local_session_lifecycle: Option<LocalSessionLifecycle> = None;
    let mut realtime_fabric_bridge: Option<
        crate::shell::collab_broker_capnp::RealtimeFabricAppBridge,
    > = None;
    let mut remote_capture_runtime: Option<
        std::sync::Arc<
            std::sync::Mutex<crate::shell::remote_capture::runtime::RemoteCaptureFabricRuntime>,
        >,
    > = None;
    // A session that is not onboarding starts its services on the first
    // tick: the network first when peer sync is on, else the local broker.
    let boot_network = enable_peer_sync && !deferred_peer_sync;
    let mut local_broker_due = !enable_peer_sync && !deferred_peer_sync && !deferred_local_broker;
    if boot_network || local_broker_due {
        crate::shell::launcher::hold_launches();
    }
    let identity_broker = spawn_session_identity_broker(&frame_builder.chrome_handle());
    // Settings can request a small, typed set of contextual tools through a
    // connected descriptor. The shell owns package resolution and launching;
    // failure only removes the advanced handoff, never core Settings controls.
    let tool_service = match crate::shell::tool_intents::spawn() {
        Ok(service) => Some(service),
        Err(e) => {
            tracing::error!(error = %e, "typed tool service unavailable");
            None
        }
    };

    // A crash or forced display-manager termination cannot run endpoint
    // guards. Recover those PID-qualified private names before binding this
    // session; clean exits already remove them explicitly below.
    remove_stale_private_wayland_endpoints(&crate::shell::launcher::runtime_dir());

    // Seed the fullscreen-configure size so an early `set_fullscreen` fills the output before the first resize.
    state.output_size = size_to_signed(initial_size);

    // `new_auto` walks `wayland-1..wayland-32`, binding the first free name.
    let (listening_socket, socket_name, display_file) = bind_and_publish_wayland_socket();
    let public_socket_path = crate::shell::launcher::runtime_dir().join(&socket_name);
    // Publish to the app launcher: every app is pointed at this socket
    // explicitly (`tilo-launcher --wayland-socket`), never at the compositor's own env.
    crate::shell::launcher::set_wayland_socket(&socket_name);
    crate::compositor::session_env::export(&public_socket_path);
    // Host tools started before the first open still see Tilo's defaults.
    let _ = std::thread::Builder::new()
        .name("tilo-mimeapps-publish".to_string())
        .spawn(crate::shell::launcher::publish_host_mimeapps);
    // Facts the loop asks for later that only the disk can answer.
    super::admin_control::resolve_admin_group_in_background();
    crate::shell::i18n::prepare_search_words_in_background();
    let mut desktop_continuation = if crate::compositor::parse_install_mode_from_args()
        == crate::compositor::InstallMode::HostDesktop
    {
        // Readiness has no identity/key authority. The live trusted onboarding
        // state below gates both its response and the ordinary host-app launch.
        let runtime = crate::shell::launcher::runtime_dir();
        let uid = rustix::process::getuid().as_raw();
        let pending = tilo_paths::data_local_dir().is_some_and(|data| {
            crate::shell::desktop_continuation::pending_review(
                &data.join("tilo-imager/migrations/desktop-continuation.json"),
                uid,
            )
        });
        match crate::shell::desktop_continuation::SessionContinuation::bind(
            &runtime,
            &socket_name,
            uid,
            pending,
        ) {
            Ok(service) => Some(service),
            Err(error) => {
                tracing::warn!(%error, "Desktop folder continuation readiness unavailable");
                None
            }
        }
    } else {
        None
    };

    let windows_compat = match crate::shell::windows_compat::spawn(
        frame_builder.chrome_handle().borrow().tree_handle(),
    ) {
        Ok(service) => {
            tracing::info!("windows compatibility session service ready");
            Some(service)
        }
        Err(error) => {
            tracing::warn!(%error, "windows compatibility session service unavailable");
            None
        }
    };

    // A connection is attributed on a worker, which reads its process's
    // ancestry and namespaces under /proc; the loop only inserts the
    // client with what the worker found, in the order they connected.
    let admitted = match admit_public_clients(&event_loop.handle()) {
        Ok(admitted) => admitted,
        Err(err) => {
            eprintln!("tilo compositor: cannot start the client admission worker: {err}");
            std::process::exit(1);
        }
    };
    if let Err(err) =
        event_loop
            .handle()
            .insert_source(listening_socket, move |client_stream, (), _| {
                if admitted.send(client_stream).is_err() {
                    tracing::warn!("the client admission worker stopped; a client was refused");
                }
            })
    {
        eprintln!("tilo compositor: insert listening socket source into event loop failed: {err}");
        std::process::exit(1);
    }

    // Legacy X11 software runs through one rootless Xwayland per workspace.
    // A display is only a listening socket until its first client connects;
    // xwayland-satellite, itself a client of the public socket registered
    // above, starts on that connection, on the supervisor's own thread. The
    // wake ping brings the loop round to export a display the active
    // workspace waited for, or to announce a workspace's failing bridge.
    let mut xwayland = match calloop::ping::make_ping() {
        Ok((wake, wake_source)) => {
            if let Err(error) = event_loop
                .handle()
                .insert_source(wake_source, |(), (), _| {})
            {
                tracing::warn!(%error, "X11 display wake source unavailable");
            }
            Some(super::xwayland::XwaylandSupervisor::start(
                wake,
                &socket_name,
            ))
        }
        Err(error) => {
            tracing::warn!(%error, "X11 apps are unavailable: no loop wake");
            None
        }
    };

    // Private displays are the input-method trust boundary. The OSK package
    // is sandboxed exactly like any other app and the composer is the image's
    // own executable; each receives only its own socket as its display, and
    // public clients cannot see the input-method/virtual-keyboard globals at
    // all. The input-source and OSK preferences decide which one owns the
    // seat and runs (see `compositor::input_method`).
    let mut input_methods =
        crate::compositor::input_method_service::InputMethodServices::start(&event_loop.handle());
    input_methods.reload(&mut state, &live_settings.snapshot());
    // The shell's own fields' stand-in for the input method, connected the
    // first time one takes text.
    let mut shell_text_input: Option<crate::compositor::shell_text_input::ShellTextInput> = None;
    // Touch contacts that landed on the on-screen keyboard: they are the
    // keyboard's alone, even over chrome that otherwise captures input.
    let mut osk_touch_slots: std::collections::HashSet<u32> = std::collections::HashSet::new();

    // A second Wayland socket for clients the session trusts with
    // authority-bearing protocols — clipboard-manager access and session
    // locking are visible here, never on the public app socket.
    //
    // The privilege is the socket, not a client request: the sandbox launcher
    // bind-mounts only the public socket (`crates/packaging/launcher/src/sandbox.rs`); bind failure is logged and non-fatal.
    let mut privileged_socket_path = None;
    match bind_privileged_socket() {
        Ok((privileged_socket, path)) => {
            privileged_socket_path = Some(crate::shell::launcher::runtime_dir().join(&path));
            eprintln!(
                "tilo compositor privileged socket on {}",
                Path::new(&path).display()
            );
            if let Err(err) = event_loop.handle().insert_source(
                privileged_socket,
                move |client_stream, (), state| {
                    let executable = peer_executable(&client_stream);
                    if let Err(err) = state
                        .display_handle
                        .insert_client(client_stream, ClientState::privileged_tool_arc(executable))
                    {
                        eprintln!("tilo compositor: failed to insert privileged client: {err}");
                    }
                },
            ) {
                eprintln!("tilo compositor: privileged socket source rejected: {err}");
            }
        }
        Err(e) => {
            eprintln!(
                "tilo compositor: no privileged wayland socket ({e}); clipboard managers will not be able to connect"
            );
        }
    }

    // When `TILO_TEST_STATE_DUMP` is set, serializes `WorkspaceTree` there
    // each tick for `tests/compositor_test.rs`; zero-cost when unset.
    let test_dump_path = test_state_dump_path(
        crate::compositor::dev_env::DEV_ENV_OVERRIDES,
        std::env::var_os(TEST_STATE_DUMP_ENV),
    );

    // Opt-in unix socket for newline-delimited JSON commands
    // (`compositor::test_control`), bound at `TILO_TEST_CONTROL`; unset means never created.
    let (test_control_listener, test_control_unlink) = match bind_test_control_socket() {
        Some((l, g)) => (Some(l), Some(g)),
        None => (None, None),
    };
    // The agent-control socket (`TILO_AGENT_CONTROL`, `agent-control`
    // builds on the headless backend only) serves the input and screenshot
    // subset of the same protocol to an agent driving a hosted machine.
    let (agent_control_listener, agent_control_unlink) =
        match bind_agent_control_socket(backend_kind == BackendKind::Headless) {
            Some((l, g)) => (Some(l), Some(g)),
            None => (None, None),
        };
    // Bound by the loop once first login is complete; see `admin_control`.
    let mut admin_control: Option<(
        UnixListener,
        UnlinkOnDrop,
        Option<calloop::RegistrationToken>,
    )> = None;
    // Bound by the loop once first login is complete; see `agent_door`.
    let mut agent_door: Option<(
        crate::compositor::agent_door::Door,
        UnlinkOnDrop,
        Option<calloop::RegistrationToken>,
    )> = None;
    let mut agent_door_consent: Option<crate::shell::agent_admission::AgentDoorConsent> = None;
    // Connections the agent door handed over for programs running in apps.
    let mut tab_grant_watch = scoped_control::TabGrantWatch::default();
    let mut agent_door_controls: Option<
        std::sync::mpsc::Receiver<crate::compositor::agent_door::ControlHandoff>,
    > = None;
    let mut test_control_conns: Vec<TestControlConn> = Vec::new();
    // A control client is itself an event source. Each accepted stream gets
    // its own wake (see `TestControlConn::watch`), because a connection is
    // accepted as soon as it exists, which is often before the request bytes
    // that follow it have arrived.
    let test_control_listener_wake = test_control_listener
        .as_ref()
        .and_then(|listener| control_listener_wake(listener, &event_loop.handle()));
    // Beside the control socket, answered off the loop: whether the loop
    // still turns, and where every thread is when it does not.
    let test_control_liveness = test_control_listener.as_ref().and_then(|_| {
        test_control_path(
            crate::compositor::dev_env::DEV_ENV_OVERRIDES,
            std::env::var_os(TEST_CONTROL_ENV),
        )
        .and_then(|path| crate::compositor::liveness::serve(&path, &event_loop.handle()))
    });
    let agent_control_listener_wake = agent_control_listener
        .as_ref()
        .and_then(|listener| control_listener_wake(listener, &event_loop.handle()));
    // Per-process input context shared by the physical-seat and optional
    // test-control paths. Keeping one focus cursor makes injected and real
    // keyboard traversal exercise the exact same widgets.
    let mut test_control_ctx = TestControlCtx::new();
    // Nothing is known about the input hardware until the backend announces
    // devices; the chrome must not keep the toolkit's desktop assumption.
    test_control_ctx.publish_input_environment(&mut frame_builder);
    frame_builder.reconcile_native_focus(&mut test_control_ctx.focus);
    // Compositor chrome has no application window for `accesskit_winit` to
    // attach to. Publish its existing `ui::Widget` semantics directly over
    // AT-SPI, including the trusted OOBE and desktop tour shown before apps.
    // The provider's callbacks run on AccessKit's thread, so pair its mailbox
    // with a calloop ping. An idle compositor must wake for a reader's first
    // tree request or action instead of waiting for unrelated Wayland input.
    let (a11y_ping, a11y_ping_source) = match calloop::ping::make_ping() {
        Ok(pair) => pair,
        Err(error) => {
            eprintln!("tilo compositor: create AT-SPI wake source failed: {error}");
            std::process::exit(1);
        }
    };
    if let Err(error) = event_loop
        .handle()
        .insert_source(a11y_ping_source, |(), (), _| {})
    {
        eprintln!("tilo compositor: insert AT-SPI wake source failed: {error}");
        std::process::exit(1);
    }
    let mut native_accessibility = NativeAccessibility::new(initial_size, a11y_ping);
    // Work handed to the session runtime from this loop (the network start
    // after first-login setup, media-player replies) reports back through
    // mailboxes; this ping wakes an idle loop when one is filled.
    let session_wake: crate::session_runtime::Wake = match calloop::ping::make_ping() {
        Ok((ping, source)) => {
            if let Err(error) = event_loop.handle().insert_source(source, |(), (), _| {}) {
                eprintln!("tilo compositor: insert session wake source failed: {error}");
                std::process::exit(1);
            }
            Arc::new(move || ping.ping())
        }
        Err(error) => {
            eprintln!("tilo compositor: create session wake source failed: {error}");
            std::process::exit(1);
        }
    };
    let mut identity_clock_wait = identity_clock_refusal.as_ref().and_then(|refusal| {
        let identity_dir = identity_store.path().parent()?.to_path_buf();
        tracing::warn!(
            valid_from_ns = refusal.valid_from_ns,
            clock_ns = refusal.clock_ns,
            "this device's certificate waits for the clock; workspaces connect when it is right"
        );
        frame_builder.with_chrome(|chrome| chrome.set_identity_waits_for_clock(true));
        // Apps opened meanwhile start with the broker, not before it.
        crate::shell::launcher::hold_launches();
        Some(crate::identity_clock_wait::IdentityClockWait::start(
            refusal,
            identity_dir,
            Arc::clone(&session_wake),
        ))
    });
    let mut dictation_vocabulary =
        crate::compositor::dictation::host::VocabularyBinding::new(Arc::clone(&session_wake));
    dictation_vocabulary.refresh(&mut state);
    if let Some(bridge) = frame_builder.host().status_bridge() {
        bridge.wake_on_changes(Arc::clone(&session_wake));
    }
    let mut retained_files = crate::onboarding_prepared::session_home()
        .map(|home| crate::retained_files::Session::open(home, Arc::clone(&session_wake)));
    state.output_management.set_wake(Arc::clone(&session_wake));
    state.set_share_encode_wake(Arc::clone(&session_wake));
    crate::shell::display_persistence::set_wake(Arc::clone(&session_wake));
    crate::shell::storage_notice::start(Arc::clone(&session_wake));
    crate::shell::crash_notice::start(Arc::clone(&session_wake));
    // NTFS drives open only through the shell's own sheet; see shell::drives.
    crate::shell::drives::start(Arc::clone(&session_wake));
    crate::shell::app_list::set_wake(Arc::clone(&session_wake));
    crate::shell::downloaded_apps::set_wake(Arc::clone(&session_wake));
    crate::shell::downloaded_apps::follow_moves();
    crate::app_icons::set_wake(Arc::clone(&session_wake));
    crate::shell::mpris::set_now_playing_wake(Arc::clone(&session_wake));
    crate::shell::install_component_wake(Arc::clone(&session_wake));
    // An invocation a control client waits on wakes the loop when it is
    // admitted or its job changes, not on the next unrelated wake.
    let _invocation_wake =
        crate::shell::search::invocation_wake::install(Arc::clone(&session_wake));
    notif_bridge.set_wake(Arc::clone(&session_wake));
    frame_builder
        .host_mut()
        .set_quick_settings_wake(Arc::clone(&session_wake));
    // The desk's System monitor is woken by each reading the session's
    // system information service sends it.
    frame_builder
        .host_mut()
        .set_desk_wake(Arc::clone(&session_wake));
    // The network, once the loop installs it, takes the same wake and
    // binds the hosted apps that use it.
    // What the broker asks of the person's records the session holds
    // wakes the loop, which runs it.
    crate::shell::settings_records::wake_with(Arc::clone(&session_wake));
    // So do the reads and writes apps ask of the workspace activity model.
    crate::shell::activity_provider::wake_with(Arc::clone(&session_wake));
    frame_builder.with_chrome(|chrome| {
        // From here on the loop never waits on the disk to open a tab's
        // record: the record store's thread opens it and wakes the loop.
        chrome.open_records_without_waiting(Arc::clone(&session_wake));
        if let Some(apps) = chrome.hosted_inputs.in_process() {
            apps.borrow_mut()
                .bind_messaging_record_wake(Arc::clone(&session_wake));
        }
    });
    let chooser_attended = Arc::new(AtomicBool::new(false));
    let mut published_room = published_room(display_file.as_ref());
    // Held for the session, like the chooser service that gives into it.
    let chosen_documents = mount_documents();
    let chooser_service = start_chooser_service(
        portal_backend.as_ref(),
        Arc::clone(&chooser_attended),
        Arc::clone(&session_wake),
        chosen_documents
            .as_ref()
            .map(|(documents, _)| documents.clone()),
    );
    // Toolkit apps' frames are drawn by their room's render service; the
    // broker picks the service and keeps it running.
    let render_broker =
        match crate::compositor::render_broker::RenderBroker::start(tree_handle.clone()) {
            Ok(broker) => Some(broker),
            Err(error) => {
                tracing::warn!(%error, "no render broker: apps draw on their own GPU device");
                None
            }
        };
    // Held for the session: apps' system readings, the desk widget's too.
    let _system_stats_service =
        start_system_stats_service(portal_backend.as_ref(), Arc::clone(&session_wake));
    // Asked on the launch workers, before the launcher writes the filter.
    if let Some(portal) = portal_backend.as_ref() {
        let permissions = crate::compositor::launch_permissions::LaunchPermissions {
            grants: portal.media_grants.clone(),
            prompts: portal.media_prompts.clone(),
            wake: Arc::clone(&session_wake),
            compositor: std::thread::current().id(),
        };
        crate::shell::launcher::set_launch_permissions(Box::new(move |app, room, launch| {
            permissions.ask(app, room, launch)
        }));
    }
    // An `app.<id>/close` action, from the broker's thread.
    let app_closes = crate::shell::app_close::install(Arc::clone(&session_wake));
    let mut network_start: Option<
        crate::session_runtime::PendingStart<Arc<crate::shell::network::ShellNetwork>>,
    > = boot_network
        .then(|| start_session_peer_sync(tree_handle.clone(), Arc::clone(&session_wake)));
    let mut recording_audio_probe = start_recording_audio_probe(Arc::clone(&session_wake));
    // Whether desktop sound can be recorded, once the probe answers: only
    // then does the device fabric offer this device's audio output.
    let mut system_audio = false;
    // Whether the launches held for the network fail when it does not come:
    // first login asked for sync, while a boot whose network failed carries
    // on local-only, as it always has.
    let mut network_held_launches = false;
    // The session broker binding on the session-start worker, with the
    // network failure its held launches are reported with once it is up.
    let mut broker_start: Option<(
        crate::session_runtime::PendingStart<crate::compositor::session_bootstrap::BrokerStart>,
        Option<String>,
    )> = None;
    // A media player's answer to a transport key, as the OSD to raise.
    let (transport_osd, transport_osds) =
        crate::session_runtime::mailbox::<crate::shell::osd::Osd>(Arc::clone(&session_wake));
    // Displays without a brightness control of their own, dimmed by the
    // compositor to their slider's level.
    let mut software_dim = SoftwareDimSync::default();
    // The volume and mute keys' writes, on a thread of their own from the
    // first press on: `wpctl` must never hold up input or painting.
    let mut media_key_audio: Option<crate::shell::audio_writer::AudioWriter> = None;
    let mut super_tap = crate::shell::input_policy::SuperTap::default();

    // Idle / DPMS-blank / session-lock state machine (pure logic, see
    // `compositor::idle`); transitions become `backend.set_dpms` calls and
    // lock-overlay paints. The power policy from `settings.json` owns the
    // blank and suspend timeouts at startup and on live reload; environment
    // values only seed the headless/default config before that policy applies.
    let headless = backend_kind == BackendKind::Headless;
    // No power profile is known yet: the power service reads it over D-Bus
    // once it runs and sends the profile's idle-suspend timeout from there.
    // The power source is read once here, for the timer of the source the
    // session starts on; the service follows it from then on.
    let started_on_battery =
        matches!(backend_kind, BackendKind::Drm) && tilo_power_mgmt::sysfs_on_battery();
    let mut power_policy = idle_policy_for_backend(
        tilo_power_mgmt::config::load_policy_for_profile(None, started_on_battery),
        headless,
    );
    let idle_config = if crate::compositor::dev_env::DEV_ENV_OVERRIDES
        && (std::env::var_os(crate::compositor::idle::BLANK_SECS_ENV).is_some()
            || std::env::var_os(crate::compositor::idle::LOCK_SECS_ENV).is_some()
            || std::env::var_os(crate::compositor::idle::SUSPEND_SECS_ENV).is_some())
    {
        // Explicit developer/test timeouts must remain authoritative. The
        // production release cannot enter this branch, and otherwise the
        // persisted minute-granularity Settings policy silently overwrites
        // the second-granularity E2E fixture before its first tick.
        IdleConfig::from_env()
    } else {
        IdleConfig::from_env()
            .with_blank(power_policy.idle_blank)
            .with_lock(power_policy.idle_lock)
            .with_suspend(power_policy.idle_suspend)
    };
    let mut session = SessionManager::new(idle_config, Instant::now());
    if recovery.as_ref().is_some_and(|(_, locked)| *locked) {
        session.request_lock(Instant::now());
    }
    frame_builder.set_session_locked(session.is_locked());
    let mut previous_onboarding_active = frame_builder.host().onboarding_active();
    publish_native_session_state(
        local_session_lifecycle.as_ref(),
        &session,
        previous_onboarding_active,
    );
    // Session-lock unlock: input + auth state machine. Verifier is the fixed
    // test password (`TILO_LOCK_TEST_PASSWORD`) or real greetd/PAM; fail-closed otherwise.
    let mut lock = LockController::new(
        lock_auth::verifier_from_env(),
        std::env::var("USER").unwrap_or_else(|_| "user".to_string()),
    );
    // Locked edge, so the controller resets exactly when the lock engages (no stale password/error carried over).
    let mut prev_locked = false;
    let mut retained_lock = lock_screen::RetainedLockScene::default();
    // Other devices' focus changes arrive over the network at human pace;
    // copying the feed into chrome a few times a second is plenty.
    let mut continuity_feed_polled: Option<Instant> = None;
    let mut screen_dimmer = ScreenDimmer::default();
    let mut prev_dim_overlay = None;
    // Set by PrepareForSleep and cleared only after the compositor's lock
    // frame has retired from the presenter (or DPMS is already off).
    let mut sleep_lock_frame_pending = false;
    // The exact presenter-frame serial containing that lock UI.  A count of
    // retired frames is insufficient: a desktop frame already queued when
    // sleep starts can retire after we lock, but it is not evidence that the
    // lock scene was ever displayed.
    let mut sleep_lock_frame_serial = None;
    // Once the presenter dies before that serial retires, this remains set
    // until the next independent sleep request.  In particular, losing the
    // pipe must not make the ordinary synchronous/no-presenter branch issue
    // a false lock-frame acknowledgement.
    let mut sleep_lock_frame_failed = false;
    // Whether the pre-sleep signal found an existing lock. A failed suspend
    // may clear only a lock installed for that sleep request.
    let mut sleep_lock_was_locked = false;
    // Whether the pending sleep is the idle timer's. Its pre-sleep lock is
    // the idle timer's too, which the person's other devices do not follow.
    let mut sleep_requested_by_idle = false;
    // Suspend-inhibitor registry: apps hold a token via
    // `inhibit_registry.acquire(reason)`; power service mirrors it into a real logind lock, idle loop checks `is_inhibited()` before suspending.
    // Mailbox the async power service uses to reach this loop (lock, battery
    // toast, idle-reset on resume) and this loop uses to reach it (request suspend).
    let power_bridge = tilo_power_mgmt::PowerBridge::new();
    // A signal (a resume, a sleep about to start) wakes this loop at once,
    // however long it would have parked.
    power_bridge.set_signal_wake(Arc::clone(&session_wake));
    // The source the session started on: the service's first look compares
    // with it, and a settings reload before then resolves its timer.
    let _ = power_bridge.note_on_battery(started_on_battery);
    // Only a DRM session owns the seat. A headless or nested session runs
    // beside another desktop on the same machine and logind, so it must never
    // suspend, lock-on-lid or power off that machine, nor take its lid and
    // power-key delegation.
    let owns_host_power = matches!(backend_kind, BackendKind::Drm);
    if owns_host_power {
        frame_builder
            .host_mut()
            .set_power_bridge(power_bridge.clone());
    } else {
        frame_builder.host_mut().detach_from_host();
        tracing::info!(
            target: "tilo::power",
            ?backend_kind,
            "session does not own the seat: host power actions and logind power service disabled"
        );
    }

    // The `org.freedesktop.Notifications` D-Bus service — the compositor owns
    // it so sandboxed apps' toasts render as OS-drawn chrome. The bridge is the
    // `Send + Sync` mailbox between the async zbus service (its own tokio
    // runtime) and this loop's sync chrome pump. Failure to reach the session
    // bus downgrades to a warning; shell-internal notifications keep working regardless.
    let priority_contacts = crate::priority_contacts::PriorityContactRegistry::open_default();
    let system_sounds = crate::shell::system_sounds::SystemSounds::new();
    frame_builder.with_chrome(|st| {
        st.set_notification_runtime(system_sounds.clone(), priority_contacts.clone());
    });
    // The session runtime keeps driving zbus dispatch in the background for
    // the life of the process.
    let mut notif_runtime: Option<&'static tokio::runtime::Runtime> = None;
    // RAII: dropping this releases the D-Bus bus name and tears down the
    // object server. It is explicitly dropped in the ordered shutdown block.
    // Registered in the background: the loop starts without waiting on the
    // bus daemon. Until then (or if it fails) shell-internal notifications
    // keep working.
    let mut notif_handle: Option<
        crate::session_runtime::Registration<crate::notifications::ServerHandle>,
    > = None;
    match crate::session_runtime::runtime() {
        Ok(rt) => {
            let bridge = notif_bridge.clone();
            let notification_workspaces = portal_backend
                .as_ref()
                .map(|portal| portal.media_grants.clone());
            notif_handle = Some(crate::session_runtime::Registration::start(
                rt.handle(),
                "notifications",
                async move {
                    let conn = zbus::connection::Builder::session()?.build().await?;
                    let service = crate::notifications::NotificationService::with_connection(
                        bridge.clone(),
                        // v1: a fresh CallerMap. Per-workspace attribution comes once the bridge
                        // accept path populates it (same shape the portal's CallerMap uses); until then unsandboxed callers carry none.
                        crate::portal::CallerMap::new(),
                        conn.clone(),
                        priority_contacts,
                        notification_workspaces,
                    );
                    let handle =
                        crate::notifications::serve_notifications(conn.clone(), service).await?;
                    crate::notifications::spawn_signal_emitter(conn, bridge);
                    Ok::<_, zbus::Error>(handle)
                },
                |_| {
                    tracing::info!(
                        "notifications: serving {} on session bus",
                        crate::notifications::BUS_NAME
                    );
                },
            ));
            // The power service runs regardless of notification outcome: it
            // lives on the *system* bus (logind/UPower), independent of the
            // session-bus daemon, sharing these threads.
            notif_runtime = Some(rt);
        }
        Err(e) => {
            tracing::warn!("notifications: session runtime unavailable ({e}); D-Bus service off");
        }
    }

    // Status-notifier watcher: applications' tray icons for the top bar and
    // the action registry. Held for the session; a second watcher already on
    // the bus (a nested session) leaves this one off.
    let tray_handle: Option<
        crate::session_runtime::Registration<tilo_tray_host::watcher::WatcherHandle>,
    > = notif_runtime.map(|rt| {
        crate::session_runtime::Registration::start(
            rt.handle(),
            "tray",
            async move {
                tilo_tray_host::watcher::serve(
                    zbus::connection::Builder::session()?,
                    std::sync::Arc::clone(crate::tray::global()),
                )
                .await
            },
            |_| {
                tracing::info!(
                    "tray: serving {} on session bus",
                    tilo_tray_host::WATCHER_BUS_NAME
                );
            },
        )
    });

    // polkit authentication agent: administrator prompts from pkexec,
    // GNOME Disks, virt-manager and system Flatpak installs become a sheet
    // only the compositor draws. Registered on the system bus for this login
    // session; without polkit or logind the requests fail as before.
    let polkit_agent: Option<
        crate::session_runtime::Registration<tilo_polkit_agent::service::AgentHandle>,
    > = notif_runtime.map(|rt| {
        crate::session_runtime::Registration::start(
            rt.handle(),
            "polkit agent",
            crate::polkit_agent::serve(crate::shell::portal_dialog::PortalDialogQueue::global()),
            |_| tracing::info!("polkit: authentication agent registered for this session"),
        )
    });

    // NetworkManager secret agent: a Wi-Fi password that changed, 802.1X
    // sign-in, VPN secrets and SIM PINs become a sheet only the compositor
    // draws, instead of a connection that silently fails.
    let network_secret_agent: Option<
        crate::session_runtime::Registration<tilo_network_secret_agent::service::AgentHandle>,
    > = notif_runtime.map(|rt| {
        crate::session_runtime::Registration::start(
            rt.handle(),
            "network secret agent",
            crate::network_secrets::serve(crate::shell::portal_dialog::PortalDialogQueue::global()),
            |_| tracing::info!("network: secret agent registered with NetworkManager"),
        )
    });

    // Bluetooth pairing agent: BlueZ's default agent for this session, so a
    // passkey, PIN, confirmation or service authorisation from any pairing
    // becomes a dialog only the compositor draws, instead of a pairing that
    // silently fails.
    let pairing_prompts = crate::bluetooth::PairingPromptHandle::global();
    pairing_prompts.set_wake(Arc::clone(&session_wake));
    let bluetooth_agent: Option<
        crate::session_runtime::Registration<crate::bluetooth::PairingService>,
    > = notif_runtime.map(|rt| {
        crate::session_runtime::Registration::start(
            rt.handle(),
            "bluetooth pairing agent",
            crate::bluetooth::PairingService::start(pairing_prompts),
            |_| tracing::info!("bluetooth: pairing agent registered with BlueZ"),
        )
    });

    // Power service: async orchestrator suspending via logind, mirroring the
    // inhibitor registry into a logind lock, watching the battery and lid.
    // Spawned on the session runtime; if the system bus/logind/UPower are
    // unreachable it logs and falls back to the pure idle policy (still blanks/locks/honours env timeouts, just can't actuate real suspend).
    if owns_host_power && let Some(rt) = notif_runtime {
        let bridge = power_bridge.clone();
        let registry = inhibit_registry.clone();
        let policy = power_policy;
        rt.spawn(async move {
            match tilo_power_mgmt::PowerService::connect(bridge, registry, policy).await {
                Ok(service) => service.run().await,
                Err(e) => tracing::warn!(
                    target: "tilo::power",
                    "power service unavailable ({e}); suspend/lid/battery inactive"
                ),
            }
        });
    }

    // The time zone follows the device where the person chose so in
    // Settings ("Set automatically"): only the session that owns the
    // machine sets its zone.
    if owns_host_power && let Some(rt) = notif_runtime {
        rt.spawn(crate::time_zone_follower::serve(std::sync::Arc::new(
            crate::time_zone_follower::ThisDevice,
        )));
    }

    let mut close_requested = false;

    // Presentation cadence via `FrameClock` (`compositor::pacing`): interval
    // comes from the backend's display; a fixed post-tick sleep instead
    // measures 21.9ms/45.6Hz regardless of actual rate. `TILO_REFRESH_HZ` overrides for testing.
    let refresh_override = pacing::refresh_override_from_env();
    let mut frame_clock =
        if let Some(interval) = refresh_override.or_else(|| backend.refresh_interval()) {
            eprintln!(
                "tilo compositor: pacing to {:.3} Hz",
                1.0 / interval.as_secs_f64()
            );
            pacing::FrameClock::new(interval, Instant::now())
        } else {
            // No display to follow (headless, or a host that won't say) — fall back rather than invent a rate.
            eprintln!(
                "tilo compositor: backend reports no refresh rate; pacing to {:.3} Hz",
                1.0 / pacing::FALLBACK_INTERVAL.as_secs_f64()
            );
            pacing::FrameClock::fallback(Instant::now())
        };
    let frame_period = frame_clock.interval();
    let mut vblank_scheduler = pacing::VblankScheduler::from_env();
    let mut projection_capture = super::projection_capture::ProjectionCapture::default();

    // Reused per-iteration scratch so backend events process *after*
    // `poll_events` returns — the input arm needs `&mut backend` (for `set_dpms`), unborrowable inside the poll closure.
    let mut pending_events: std::collections::VecDeque<BackendEvent> =
        std::collections::VecDeque::new();
    // The pad whose map is held open, with its saved settings.
    let mut pad_map_hold: Option<(String, Box<settings_store::hardware::TabletConfig>)> = None;

    // `wl_output` globals: advertising the manager without an output means no
    // client learns a display exists. Synced here at startup and on every hotplug.
    let mut output_globals = output_globals::OutputGlobals::new();
    // Tracks the `wl_surface.enter`/`leave` events we have sent. Unlike an
    // output global (which exists session-wide), membership is a property of
    // the surface's current painted geometry.
    let mut surface_output_memberships = SurfaceOutputMemberships::default();
    let display_handle_for_outputs = display_handle;
    {
        let outs = backend.outputs().to_vec();
        state.set_output_layout(&outs);
        output_globals.sync::<State>(
            &display_handle_for_outputs,
            &outs,
            refresh_mhz_of(&*backend),
        );
        state.set_output_heads(backend.output_heads());
    }
    // Set when the output set changed this tick, so derived state is re-derived once per batch, not per udev edge.
    let mut outputs_dirty = false;
    let mut published_output_colors = PublishedOutputColors::default();

    // Pointer/keyboard preferences are read once at startup; this follows
    // each new generation of the settings document so changes take effect
    // this session, not the next, and only for the sections that changed.
    let mut settings_sections = settings_store::live::SectionFollower::new(&live_settings);
    // The outputs were published before this baseline was taken, and a
    // display change staged in between (Settings, or `tilo-vmd display-apply`
    // right after a session start) is part of it, so it raises no change.
    {
        use settings_store::hardware::{DisplayTransaction, DisplayTransactionState as S};
        if DisplayTransaction::load().is_some_and(|t| {
            matches!(
                t.state,
                S::Requested | S::ConfirmRequested | S::RevertRequested
            )
        }) {
            backend.reload_display_settings();
            outputs_dirty = true;
        }
    }
    let mut switch_control = SwitchControlRuntime::new(Instant::now(), &live_settings);
    let mut continuity_policy =
        crate::shell::continuity_policy::ContinuityPolicyCache::new(&live_settings);
    // Local game controllers drive the desktop through the same input
    // funnel; their evdev descriptors wake this loop like hardware input.
    let mut controller_nav = crate::compositor::controller_nav::ControllerNavRuntime::default();
    let mut controller_wakes: HashMap<u64, calloop::RegistrationToken> = HashMap::new();
    // Redraw gating: painting every tick regardless of change would pin a CPU
    // core at 100%. Repaint only when: a backend event arrived
    // (input/resize/focus/redraw); the widget tree is animating; the chrome
    // revision bumped or a session transition fired. Layout still runs every
    // tick for hit-testing; only paint+submit are gated. The former one-second
    // full-scene keepalive consumed a material fraction of a software-rendered
    // VM core despite no changing pixels; every real producer already carries
    // an explicit redraw/revision/deadline edge.
    let mut last_lock_clock_refresh = Instant::now();
    let mut lock_player: Option<crate::shell::now_playing::PlayerSnapshot> = None;
    let mut lock_pointer: Option<vello::kurbo::Point> = None;
    // The keyboard the lock screen draws for unlocking by touch: shown from
    // the first touch, gone at the next physical key. No input method runs
    // while locked; the passcode never leaves the lock screen.
    let mut lock_keyboard = lock_screen::LockTouchKeyboard::default();
    let mut last_lock_media_poll = Instant::now();
    let mut last_revision = frame_builder.chrome_revision();
    // A change counted on a tick that painted nothing ([`NativeRedrawReasons::owed`]).
    let mut content_owed = false;
    let mut force_first_paint = true;
    let mut animation_settle = AnimationSettle::default();
    let mut animation_trace = AnimationTraceLabel::default();
    // When the frame being made was first wanted, for its trace lateness.
    let mut demand_onset = metrics::DemandOnset::default();
    // Each foreign surface's frame counter at the last traced paint, so a
    // painted tick can name the apps whose commits it carries.
    let mut trace_surface_frames = std::collections::HashMap::new();
    // `output_presentation_frame_changed` observes live foreign-frame
    // sequences, but removal leaves no live entry to report. Keep a local
    // repaint edge until its full-frame boundary is actually submitted.
    let mut output_presentation_repaint_pending = false;
    let mut output_presentation_surface_count = state.output_presentation_surfaces.len();
    // Start announcements can beat the replicated `PeerWindow` tab they
    // describe, and a workspace rebuild can replace a mounted runtime
    // widget. Retain descriptors until explicit stream end and retry their
    // cheap idempotent mount every network tick.
    let mut pending_legacy_streams: HashMap<(u64, u64), crate::shell::peer_view::PeerStreamStart> =
        HashMap::new();
    let mut early_legacy_frames = crate::shell::peer_view::EarlyFrames::default();
    let legacy_share_config = crate::config::load_config().flatpak_share;
    let mut hosted_legacy_checked = Instant::now();
    let mut legacy_shares = LegacySharePlan::default();
    // Whether this loop, not a viewer's watch request, made the headless
    // backend watched: for legacy windows its room's host relays.
    let mut watched_for_host = false;
    // Optional paint-rate diagnostics (`TILO_PAINT_STATS=1`).
    let paint_stats = std::env::var("TILO_PAINT_STATS").is_ok();
    // `TILO_PAINT_STATS=ticks` also prints a `paint-tick:` line on every
    // iteration, so a reader can tell when each one ran: a summary covers
    // everything since the previous one, which may lie long before the
    // period the reader measures (the idle gate counts only its window).
    let paint_ticks = std::env::var("TILO_PAINT_STATS").is_ok_and(|value| value.trim() == "ticks");
    // What ended the park before this iteration, for `paint-tick:`.
    let mut last_wake: &'static str = "start";
    // The last park ended on the frame cadence and no client request came
    // in after the frame: nothing outside an animation asked for this tick.
    let mut woke_for_cadence_only = false;
    // The widget pump changed something the last time it ran: an animation
    // stepped in `update` (rather than placed by layout) is running.
    let mut widget_pump_changed = true;
    // Slow iterations are exceptional and always worth preserving in the
    // session flight record: a compositor that stops pumping input for 100 ms
    // is already visibly broken. `TILO_LOOP_TIMING` additionally enables the
    // verbose aggregate line for development runs. A profile the admin
    // starts (`metrics::profile`) times the same phases for its duration,
    // recording every span instead of logging the slow ones.
    let loop_timing_env = std::env::var("TILO_LOOP_TIMING").is_ok();
    // Set at the top of every tick.
    let mut loop_timing: bool;
    // A developer override: every painted frame takes as long as on a slow
    // machine, so per-frame layout steps see real long frame times.
    let mut frame_stall = super::frame_stall::FrameStall::from_process_env();
    let mut pump_checkpoint = Instant::now();
    let mut pump_slowest = ("start", Duration::ZERO);
    macro_rules! pump_checkpoint {
        ($name:literal) => {{
            if loop_timing {
                let now = Instant::now();
                let elapsed = now.saturating_duration_since(pump_checkpoint);
                if elapsed > pump_slowest.1 {
                    pump_slowest = ($name, elapsed);
                }
                metrics::profile::record_phase($name, elapsed, None);
                pump_checkpoint = now;
            }
        }};
    }
    macro_rules! pump_phase {
        ($name:literal, $body:expr) => {{
            if loop_timing {
                let started = Instant::now();
                let value = $body;
                let elapsed = started.elapsed();
                metrics::profile::record_phase($name, elapsed, None);
                if loop_timing_env && elapsed >= Duration::from_millis(8) {
                    eprintln!(
                        "PUMP-PHASE name={} elapsed_us={}",
                        $name,
                        elapsed.as_micros()
                    );
                }
                value
            } else {
                $body
            }
        }};
        ($name:literal, $count:expr, $body:expr) => {{
            if loop_timing {
                let started = Instant::now();
                let value = $body;
                let elapsed = started.elapsed();
                let items: usize = $count;
                metrics::profile::record_phase($name, elapsed, Some(items));
                if loop_timing_env && elapsed >= Duration::from_millis(8) {
                    eprintln!(
                        "PUMP-PHASE name={} elapsed_us={} items={}",
                        $name,
                        elapsed.as_micros(),
                        items
                    );
                }
                value
            } else {
                $body
            }
        }};
    }
    // Frame-loop instrumentation (`TILO_FRAME_TRACE`/`TILO_FRAME_SUMMARY`), free
    // when neither is set. See `compositor::metrics`.
    let mut metrics = metrics::FrameRecorder::from_env();
    metrics.set_target_period(frame_period);
    // Bench hook: shut down cleanly after N seconds so the frame trace/summary
    // flush via the normal exit path instead of a SIGKILL. Unset in production.
    // `checked_add`: an absurd env value overflowing `Instant` degrades to
    // "no deadline" rather than panicking a session over a bench-only knob.
    let run_deadline = metrics::run_seconds_from_env().and_then(|d| {
        eprintln!(
            "tilo compositor: {}={:?}; will exit cleanly after that",
            metrics::RUN_SECONDS_ENV,
            d
        );
        Instant::now().checked_add(d)
    });
    let mut stats_since = Instant::now();
    // Report windows in which nothing painted, folded into the next report
    // that has something to say: an idle session must not write two lines
    // every two seconds into a bounded session log.
    let mut paint_stats_idle = Duration::ZERO;
    let (mut frames_total, mut frames_painted) = (0u32, 0u32);
    let (mut cause_events, mut cause_revision, mut cause_anim) = (0u32, 0u32, 0u32);
    let (mut cause_session, mut cause_keepalive) = (0u32, 0u32);
    let mut animation_sources_seen = crate::shell::frame::AnimationDemand::default();
    let mut animating_content_leaves: Vec<String> = Vec::new();
    let mut wakes_seen = WakeTally::default();
    let mut widget_wake_owner: Option<String> = None;
    // Test-control commands are intentionally serviced after this tick's
    // paint gate. Preserve their redraw edge for the following iteration;
    // dropping it here records input latency forever without ever producing
    // the reflecting frame.
    let mut injected_redraw_pending = false;
    let mut frame_clock_retired_serial = 0u64;
    // Park spent waiting on a full presenter. Applied on the next tick,
    // before animation demand is sampled, so the transition does not finish
    // in the model with nothing presented.
    let mut transition_hold_sleep = Duration::ZERO;
    let mut credit_transition_hold;
    // The person's session on their other devices: this one follows a lock
    // or unlock there and tells them about its own.
    let mut session_follow = crate::shell::owner_session::SessionFollow::default();
    // The fabric samples this device's presence once a second; following
    // the others more often would change nothing.
    let mut session_followed: Option<Instant> = None;
    // Every session transition reaches the backend, presenter, logind lifecycle,
    // dimmer and output management alike. A lock or unlock that comes here is
    // the person's own, which their other devices follow; the idle timer's
    // and the followed ones are applied apart.
    macro_rules! apply_session_transitions {
        ($transitions:expr) => {
            for t in $transitions {
                if matches!(t, SessionTransition::Locked | SessionTransition::Unlocked) {
                    session_follow.changed_by_person(tilo_clock::unix_now_ms());
                }
                apply_session_transition(
                    t,
                    backend.as_mut(),
                    present_pipe.as_ref(),
                    local_session_lifecycle.as_ref(),
                    &mut screen_dimmer,
                    Some(&mut state.output_management),
                );
            }
        };
    }
    // Read input between the phases of a turn, and send at once each release
    // of a key a client holds (`client_input::deliver_held_key_release`): a
    // client repeats a held key until its release arrives, and the phases
    // after this turn's input dispatch can outlast the repeat delay. All
    // other input, presses included, waits in `pending_events`, in order,
    // for the next turn's full dispatch.
    macro_rules! deliver_held_key_releases {
        () => {{
            let placements = crate::compositor::backend::layout::placements(backend.outputs());
            backend.poll_input(&mut |event| {
                pending_events.push_back(crate::compositor::backend::layout::event_to_logical(
                    &placements,
                    event,
                ));
            });
            if !pending_events.is_empty()
                && !session.is_locked()
                && client_input_allowed(&frame_builder)
            {
                let mut delivered = false;
                for event in &pending_events {
                    if let BackendEvent::Input(wev) | BackendEvent::PhysicalKey { event: wev, .. } =
                        event
                    {
                        delivered |= client_input::deliver_held_key_release(&mut state, wev);
                    }
                }
                if delivered && let Err(err) = display.flush_clients() {
                    tracing::warn!(%err, "flushing an early key release failed");
                }
            }
        }};
    }
    // From here the compositor thread turns this loop. A turn that stops for
    // good is recorded (in the journal and the crash directory, which the
    // next start reads to tell the person) and ends the process, so a hang hands the display back
    // and the session returns to the login screen instead of needing the
    // power button (`watchdog`). Start-up before it may wait without a bound
    // on purpose: the seat is not ours until logind activates the session.
    let watchdog = crate::compositor::watchdog::Watchdog::watch_this_thread(
        crate::compositor::watchdog::Policy::COMPOSITOR,
        crate::compositor::watchdog::record_and_end_process,
    )
    .map_err(|error| eprintln!("tilo compositor: the main-loop watchdog did not start: {error}"))
    .ok();
    // Nor does a turn wait on storage: apps running in-process ask their
    // stores to commit, and the stores' own threads do the waiting. A store
    // still asked to wait here is reported to the main-thread audit.
    super::liveness::audit_storage_waits();
    let no_storage_waits = durable_store::waiting::forbid_on_this_thread();
    while !close_requested {
        dictation_vocabulary.refresh(&mut state);
        if termination.requested() {
            break;
        }
        let iter_t0 = Instant::now();
        let injected_redraw = std::mem::take(&mut injected_redraw_pending);
        metrics::profile::expire(iter_t0);
        metrics.begin_tick(iter_t0);
        loop_timing = loop_timing_env || metrics::profile::active();
        // A setting this loop changed that the settings worker could not store
        // went back to its previous value: say so (one atomic load when none did).
        if settings_store::deferred_failure_pending() {
            frame_builder.with_chrome(crate::shell::settings_failures::drain);
        }
        if let Some(pipe) = present_pipe.as_ref() {
            for (serial, proof) in pipe.take_physical_results() {
                state.presentation_feedback.completed(serial, proof);
            }
        }
        state.presentation_feedback.expire(Instant::now());

        let mut input_steps = crate::compositor::metrics::SlowestStep::starting_at(iter_t0);
        if !transition_hold_sleep.is_zero() {
            frame_builder
                .host_mut()
                .defer_running_transitions(transition_hold_sleep);
            transition_hold_sleep = Duration::ZERO;
        }
        if let Some(pipe) = present_pipe.as_mut()
            && let Some(retired) = pipe.latest_retirement()
        {
            let serial = retired.serial;
            metrics.note_retirement(serial, retired.at);
            // A frame that queued its own flip reached the panel at that
            // flip's vblank, not when the presenter returned.
            let shown_at = retired
                .display
                .flip_queued_at
                .and(retired.display.vblank)
                .unwrap_or(retired.at);
            let (keys, summary) = {
                let mut tracker = crate::compositor::key_latency::tracker();
                (
                    tracker.frames_retired(serial, shown_at),
                    tracker.summary(shown_at),
                )
            };
            for sample in keys {
                eprintln!("{sample}");
            }
            if let Some(summary) = summary {
                eprintln!("{summary}");
            }
            if let Some(switch) = frame_builder
                .host_mut()
                .workspace_switch_latency_mut()
                .frames_retired(serial, shown_at)
            {
                eprintln!("{switch}");
            }
            if serial > frame_clock_retired_serial {
                // A vblank-paced presenter re-anchors the grid on the vblank,
                // one lead before the next. Otherwise a virtual present can
                // retire several intervals after the frame was admitted, and
                // the grid point after that late time would hold the waiting
                // frame for another interval (`note_blocked_retirement`).
                let min_depth = vblank_scheduler.note_retired(
                    &retired,
                    &mut frame_clock,
                    pipe.queue_depth(),
                    iter_t0,
                );
                pipe.set_min_queue_depth(min_depth);
                frame_clock_retired_serial = serial;
            }
        }
        // The previous `event_loop.dispatch` only establishes readiness for
        // nested DRM/libinput and test-control fds. Drain the backend now,
        // before any frame decision, so its level-triggered poll fd is cleared
        // in this same iteration instead of immediately waking calloop again.
        // Backends report positions in the scene; the shell and clients work
        // in the logical desktop, so input crosses over here, once.
        let input_placements = crate::compositor::backend::layout::placements(backend.outputs());
        backend.poll_events(&mut |event| {
            pending_events.push_back(crate::compositor::backend::layout::event_to_logical(
                &input_placements,
                event,
            ));
        });
        // Keys a control client queued as a physical keyboard's arrive here, with the
        // backend's own events.
        pending_events.extend(test_control_ctx.seat_events.drain(..));
        // Whether this batch's input found the display blanked: the power key
        // in it then has done its visible part already, by waking it.
        let blanked_before_input = session.is_blanked();
        // Any input is activity, whoever it is routed to below (a pen or a
        // pinch can go straight to an app): reset the idle timer and wake the
        // display if it was dimmed or blanked.
        apply_session_transitions!(crate::compositor::activity::note_person_input(
            &mut session,
            &pending_events,
            Instant::now(),
        ));
        input_steps.step("backend-poll");
        let t_backend_poll_done = iter_t0.elapsed();
        if slow_prepaint_phase(t_backend_poll_done) {
            tracing::warn!(
                target: "tilo::latency",
                elapsed_ms = t_backend_poll_done.as_secs_f64() * 1_000.0,
                queued_events = pending_events.len(),
                "backend poll stalled compositor input"
            );
        }
        // Capture workers wake this loop through their eventfd. Apply released
        // Smithay transactions before deciding whether there is app content
        // to paint; doing this after painting strands the newly committed
        // frame until an unrelated event, and a continuously committing client
        // can deadlock behind its first transaction barrier.
        let foreign_capture_changed =
            state.pump_foreign_capture_barriers(&display_handle_for_outputs);
        input_steps.step("capture-barriers");
        // Logout is requested from an off-thread power-menu action. Exit the
        // compositor through this normal cleanup path; its package wrapper
        // then stops Tilo's graphical targets and clears the shared user-manager
        // environment before GDM admits the next desktop.
        if os_actions::take_logout_request() {
            tracing::info!(target: "tilo::session", "logout request observed by compositor loop");
            close_native_session_authority(local_session_lifecycle.as_ref());
            close_requested = true;
            continue;
        }
        // Poll before processing launch actions so closed workspaces have
        // released their displays and a waiting X11 client gets its bridge.
        if let Some(xwayland) = xwayland.as_mut() {
            xwayland.poll(
                &frame_builder.chrome_handle(),
                &state.mapped_xwayland_pids(),
            );
        }
        if let Some(service) = floating_surface_service.as_ref()
            && pump_floating_surface_service(
                service,
                &mut state,
                &mut frame_builder,
                &mut capture_executor,
                &mut external_floating_ids,
                &mut window_appearances,
                backend.outputs(),
                initial_size,
            )
        {
            force_first_paint = true;
        }
        // Each app window wears the temperature of the workspace it is in
        // and the person's colour, told again whenever either moves.
        if let Some(service) = floating_surface_service.as_ref() {
            window_appearances.publish_with_rooms(
                |app_id| frame_builder.window_appearance_for_app(app_id),
                |app_id| frame_builder.room_sync_for_app(app_id),
                frame_builder.configured_app_theme(),
                frame_builder
                    .host()
                    .status_bridge()
                    .map(crate::shell::status::StatusBridge::network_status)
                    .unwrap_or_default(),
                |connection, message| service.send(connection, message),
            );
        }
        if let Some(service) = floating_surface_service.as_ref()
            && pump_floating_toplevel_events(service, &mut state)
        {
            force_first_paint = true;
        }
        if let Some(service) = chooser_service.as_ref() {
            pump_chooser_opens(service, &frame_builder.chrome_handle());
            if pump_identity_choosers(service, &mut frame_builder) {
                force_first_paint = true;
            }
            if pump_app_choosers(service, &mut frame_builder) {
                force_first_paint = true;
            }
        }
        if let Some(broker) = render_broker.as_ref() {
            broker.sync_rooms();
        }
        // Contextual routes from Personal Settings cross a worker-thread
        // service, then land here because ChromeState intentionally remains
        // compositor-thread owned.
        if let Some(service) = tool_service.as_ref() {
            let chrome = frame_builder.chrome_handle();
            service.pump_workspace_settings(&chrome);
            service.pump_identity_backup(&chrome, || {
                frame_builder.host_mut().open_portable_identity_backup()
            });
            let broker = collab_broker
                .as_ref()
                .and_then(crate::shell::collab_broker::BrokerHandle::app_action_broker);
            service.pump_app_opens(&chrome, broker.as_ref());
        }
        input_steps.step("services");
        // A pen goes to a tablet-aware client directly and stands in for the
        // pointer everywhere else, so it expands into ordinary input here,
        // in order, before the batch is dispatched.
        // A pad follows keyboard focus the same way, with its settings'
        // mapping standing in wherever no tablet-aware app is focused.
        // The cursor follows a pen over a tablet-aware client too.
        let mut pen_moved = false;
        if pending_events.iter().any(|event| {
            matches!(
                event,
                BackendEvent::Tablet(_)
                    | BackendEvent::TabletPad(_)
                    | BackendEvent::TabletPadMap { .. }
                    | BackendEvent::TabletPadAdded(_)
                    | BackendEvent::TabletPadRemoved(_)
            )
        }) {
            for event in std::mem::take(&mut pending_events) {
                match event {
                    BackendEvent::Tablet(input) => {
                        pen_moved = true;
                        pending_events.extend(client_input::route_tablet_event(
                            &mut state,
                            &mut frame_builder,
                            &input,
                            session.is_locked(),
                        ));
                    }
                    BackendEvent::TabletPad(pad) => {
                        if let crate::compositor::backend::PadAction::ModeSwitch {
                            mode,
                            modes,
                            ..
                        } = pad.action
                        {
                            frame_builder.with_chrome(|st| {
                                st.show_osd(crate::shell::osd::Osd::pad_mode(mode, modes));
                            });
                        }
                        pending_events.extend(client_input::route_pad_event(
                            &mut state,
                            &frame_builder,
                            pad,
                            session.is_locked(),
                        ));
                    }
                    BackendEvent::TabletPadMap {
                        pad,
                        shown,
                        time_msec,
                    } => match shown {
                        // Not on the lock screen: the map names what the
                        // apps behind it do.
                        Some(config) if !session.is_locked() => {
                            let target =
                                client_input::pad_focus_target(&state, &frame_builder, false);
                            // The app learns the pad is there now, so it can
                            // label the controls while the map is up.
                            state.focus_tablet_pad(&pad, target, time_msec);
                            pad_map_hold = Some((pad, config));
                        }
                        Some(_) => {}
                        None => {
                            if pad_map_hold.as_ref().is_some_and(|(held, _)| *held == pad) {
                                pad_map_hold = None;
                            }
                        }
                    },
                    BackendEvent::TabletPadAdded(description) => {
                        state.add_tablet_pad(description);
                    }
                    BackendEvent::TabletPadRemoved(pad) => state.remove_tablet_pad(&pad),
                    other => pending_events.push_back(other),
                }
            }
        }
        // Rebuilt every tick while held, so labels an app sends after a mode
        // switch replace the ones it gave for the last mode.
        if pad_map_hold.is_some() && session.is_locked() {
            pad_map_hold = None;
        }
        let pad_map = pad_map_hold
            .as_ref()
            .and_then(|(pad, config)| state.tablet_pad_map(pad, config));
        if pad_map.is_none() {
            pad_map_hold = None;
        }
        if pad_map.is_some() || frame_builder.chrome().pad_map.is_some() {
            frame_builder.with_chrome(|st| st.set_pad_map(pad_map));
        }
        input_steps.step("tablet");
        // A finger held still on chrome long enough is a secondary click;
        // no touch event marks that moment, the clock does.
        pending_events.extend(
            test_control_ctx
                .touch_pointer
                .poll(Instant::now())
                .into_iter()
                .map(BackendEvent::Input),
        );
        let had_backend_events = !pending_events.is_empty();
        let mut dispatched_input = DispatchedInput::default();
        let client_keys_before = state.client_keys_delivered;
        let mut session_changed = false;
        let mut accessibility_changed = false;
        let mut visible_input_changed = pen_moved;
        for request in native_accessibility.drain() {
            match request {
                AccessibilityRequest::InitialTree => accessibility_changed = true,
                AccessibilityRequest::Action(request) => {
                    // This endpoint publishes exactly the root AccessKit tree.
                    // Do not let a malformed or stale request for another
                    // tree alias a compositor-owned node id.
                    if request.target_tree != ui::accesskit::TreeId::ROOT {
                        accessibility_changed = true;
                        continue;
                    }
                    if session.is_locked() {
                        // The lock owns a deliberately tiny, non-leaking tree.
                        // Only its password field and the now-playing
                        // transport are actionable; stale ids from the hidden
                        // desktop are rejected here.
                        if let Some(transport) =
                            lock_screen::transport_for_node(request.target_node)
                        {
                            if request.action == ui::accesskit::Action::Click {
                                session_changed |=
                                    send_lock_transport(lock_player.as_ref(), transport);
                            }
                        } else if request.target_node == lock_screen::LOCK_PASSWORD_ID {
                            match (request.action, request.data.as_ref()) {
                                (
                                    ui::accesskit::Action::SetValue,
                                    Some(ui::accesskit::ActionData::Value(value)),
                                ) if !lock.is_verifying() => {
                                    lock.clear_input();
                                    lock.push_str(value);
                                    session_changed = true;
                                }
                                (ui::accesskit::Action::Click, None) if !lock.is_verifying() => {
                                    let _ = lock.handle_key(LockKey::Submit);
                                    session_changed = true;
                                }
                                _ => {}
                            }
                        }
                    } else if let Some(action) = ui::accesskit_bridge::accesskit_action_to_ours(
                        request.action,
                        request.data.as_ref(),
                    ) && frame_builder.dispatch_a11y_action(
                        request.target_node,
                        &action,
                        &mut ui::widget::Edits::lend(&mut test_control_ctx.doc),
                        &test_control_ctx.root_obj,
                        &mut test_control_ctx.focus,
                    ) {
                        session_changed = true;
                    }
                    // Even an unsupported or stale action gets a fresh tree:
                    // the AT can immediately discard the obsolete reference.
                    accessibility_changed = true;
                }
                AccessibilityRequest::Deactivated => {}
            }
        }
        // Popped rather than consumed: `pending_events` is declared once
        // outside this loop and reused every tick to avoid a per-frame
        // allocation on this hot path, and an event may expand into further
        // events that must run next, before anything the backend queued later.
        // A Super held alone past its hold peeks on the wake its deadline
        // asked for, with no new event to carry it.
        if let Some(gesture) = super_tap.poll(Instant::now())
            && !session.is_locked()
        {
            frame_builder.host_mut().super_gesture(
                gesture,
                &test_control_ctx.doc,
                &test_control_ctx.root_obj,
            );
            session_changed = true;
        }
        let mut event_step = "accessibility";
        while let Some(event) = pending_events.pop_front() {
            // Another of the person's devices' input is this device's own,
            // except that it does not say someone is here.
            let (event, origin) = match event {
                BackendEvent::Sibling(event) => (*event, InputOrigin::Sibling),
                event => (event, InputOrigin::Here),
            };
            state.input_origin = origin;
            dispatched_input.note(&event);
            state.physical_key_input_time_ns = match &event {
                BackendEvent::PhysicalKey { input_time_ns, .. } => Some(*input_time_ns),
                _ => None,
            };
            input_steps.step(event_step);
            event_step = input_event_step(&event);
            match event {
                BackendEvent::CloseRequested => {
                    close_requested = true;
                }
                BackendEvent::OutputResized {
                    id: _,
                    size,
                    scale: _,
                } => {
                    if size.0 > 0 && size.1 > 0 {
                        outputs_dirty = true;
                        frame_builder.set_viewport(size);
                        // A resize is also how the compositor learns a monitor changed, so
                        // re-derive per-display widths alongside the scene extent.
                        frame_builder.set_output_geometry(output_geometry(backend.outputs()));
                        if let Some(portal) = portal_backend.as_ref() {
                            portal
                                .input_capture
                                .set_zones(input_capture_zones(backend.outputs()));
                        }
                        // Keep the fullscreen-configure size in sync so a fullscreened client fills the current output.
                        state.output_size = size_to_signed(logical_geometry(backend.outputs()));
                        // The swap chain lives on the render thread now, so the reconfigure
                        // travels with the frames rather than inline, keeping it ordered against any frame already queued at the old size.
                        if let Some(pipe) = present_pipe.as_ref() {
                            pipe.resize(size);
                        }
                        // A resize also means the window moved monitors or the mode changed;
                        // retarget the clock so "pace to the display" stays true. The env override still wins.
                        if refresh_override.is_none()
                            && let Some(interval) = backend.refresh_interval()
                            && frame_clock.set_interval(interval)
                        {
                            eprintln!(
                                "tilo compositor: display changed; now pacing to {:.3} Hz",
                                1.0 / interval.as_secs_f64()
                            );
                            metrics.set_target_period(interval);
                        }
                    }
                }
                BackendEvent::Input(wev) | BackendEvent::PhysicalKey { event: wev, .. } => {
                    // A physical switch still enters through the same trusted
                    // libinput/winit funnel. Only the configured pair is
                    // translated, and disabled mode returns `None`, preserving
                    // the original key exactly.
                    let translated_switch = switch_control
                        .translate(&wev, Instant::now())
                        .map(|(action, pressed)| switch_action_event(action, pressed));
                    if translated_switch.is_some() {
                        state.physical_key_input_time_ns = None;
                    }
                    let wev = translated_switch.as_ref().unwrap_or(&wev);
                    let mut key_steps =
                        matches!(wev, WindowEvent::KeyboardInput { .. }).then(KeySteps::start);
                    // Pointer motion changes the compositor-owned cursor even
                    // when the focused Wayland client consumes the event.  It
                    // therefore needs a frame independently of whether chrome
                    // state changed.  Buttons/keys do not alter shell pixels
                    // unless their downstream handler reports a change.
                    visible_input_changed |= matches!(wev, WindowEvent::CursorMoved { .. });
                    // Instrumentation: the clock starts when the loop takes the event off the
                    // backend; the first painted frame after closes the input-to-present measurement.
                    metrics.note_input(Instant::now());
                    // Strip layers warming at rest stop at their next step:
                    // the frame this input makes is about to need the GPU.
                    if let Some(pipe) = present_pipe.as_ref() {
                        pipe.note_input();
                    }
                    // Physical priority: the host's own keyboard/mouse blocks every remote
                    // peer for the next half second, stamped at the funnel so lock/VT/power-key branches count as takeover too.
                    if state.input_origin == InputOrigin::Here {
                        state.remote_input_firewall.note_local_input(Instant::now());
                    }
                    // Super going down asks for a fullscreen window's bar,
                    // as reaching for the top edge does; the key never
                    // reaches the app, so this takes nothing from it.
                    if let WindowEvent::KeyboardInput { event: key, .. } = wev
                        && key.state == ElementState::Pressed
                        && !key.repeat
                        && matches!(
                            key.logical_key,
                            Key::Named(NamedKey::Super | NamedKey::Meta)
                        )
                        && !session.is_locked()
                    {
                        frame_builder.host_mut().reveal_fullscreen_bar_by_key();
                    }
                    // Super alone, tapped or held, opens the Super surface.
                    // Observed ahead of every branch below, so a chord one of
                    // them handles still disarms it.
                    if let Some(gesture) = super_tap.observe(wev, Instant::now())
                        && !session.is_locked()
                    {
                        frame_builder.host_mut().super_gesture(
                            gesture,
                            &test_control_ctx.doc,
                            &test_control_ctx.root_obj,
                        );
                        session_changed = true;
                    }

                    let locked = session.is_locked();
                    // Whether the chrome handled this event itself; for raw
                    // touch that means a shell gesture or a touch-aware widget.
                    let mut chrome_handled = false;
                    let route = priority_route(
                        wev,
                        test_control_ctx.modifiers,
                        locked,
                        frame_builder.host().shortcut_config(),
                    );
                    if let Some(steps) = key_steps.as_mut() {
                        steps.steps.mark("route");
                    }
                    if locked {
                        end_spatial_control_or_forget(peer_sync.as_deref(), &mut spatial_control);
                        if let WindowEvent::KeyboardInput { event: key_ev, .. } = wev {
                            // Keep both chord routing and Smithay's XKB state in step even
                            // while the lock owns the visible key.  Dropping a release here
                            // would leave the pre-lock modifier pressed after unlock.
                            client_input::apply_modifier_key(
                                &mut test_control_ctx.modifiers,
                                key_ev,
                            );
                            client_input::consume_keyboard_while_captured(&mut state, wev);
                        }
                    }
                    if route == PriorityRoute::LockScreen {
                        // Locked: every input is captured by the lock surface, into the password
                        // field, never apps/chrome; unlocks only when `lock.poll_verification()` observes success.
                        // A key of the keyboard drawn for touch, when one
                        // was pressed; every other event goes to the prompt.
                        let mut key = None;
                        match wev {
                            WindowEvent::CursorMoved { position, .. } => {
                                lock_pointer =
                                    Some(vello::kurbo::Point::new(position.x, position.y));
                            }
                            WindowEvent::Touch(touch)
                                if touch.phase == winit::event::TouchPhase::Started =>
                            {
                                lock_keyboard.touch_started();
                            }
                            WindowEvent::KeyboardInput { event, .. }
                                if event.state == ElementState::Pressed =>
                            {
                                lock_keyboard.physical_key();
                            }
                            WindowEvent::MouseInput {
                                state: ElementState::Pressed,
                                button: MouseButton::Left,
                                ..
                            } => {
                                let outputs = lock_outputs(
                                    backend.outputs(),
                                    frame_builder.scene_size(),
                                    focused_display_id(&frame_builder),
                                );
                                let view = LockView {
                                    now_playing: lock_player.as_ref(),
                                    keyboard: lock_keyboard.shown(),
                                    ..LockView::default()
                                };
                                let pressed = lock_pointer.and_then(|point| {
                                    lock_screen::keyboard_key_at(
                                        &outputs,
                                        frame_builder.theme(),
                                        &view,
                                        point,
                                    )
                                });
                                if let Some(pressed) = pressed {
                                    key = Some(lock_keyboard.press(pressed));
                                } else if let Some(transport) = lock_pointer.and_then(|point| {
                                    lock_screen::transport_at(
                                        &outputs,
                                        frame_builder.theme(),
                                        &view,
                                        point,
                                    )
                                }) {
                                    send_lock_transport(lock_player.as_ref(), transport);
                                }
                            }
                            _ => {}
                        }
                        match key {
                            Some(key) => {
                                lock.handle_key(key);
                            }
                            None => {
                                lock.handle_event(wev, &test_control_ctx.modifiers);
                            }
                        }
                        // Any keystroke changes the masked field / error, so force a repaint this tick.
                        session_changed = true;
                    } else if route == PriorityRoute::PowerKey {
                        // The hardware power key always does something visible, locked or
                        // not (logind delegates it via the service's block inhibitor): it
                        // lights a display that could not be lit, wakes a blanked one, or
                        // does what the power settings say. A deliberate press bypasses the
                        // idle inhibitor.
                        let relit = backend.retry_failed_outputs("power key");
                        let effect = power_key_effect(
                            relit,
                            blanked_before_input,
                            session.is_locked(),
                            power_policy.on_power_key(),
                        );
                        eprintln!(
                            "tilo compositor: power key: locked={} blanked={blanked_before_input} effect={effect:?}",
                            session.is_locked()
                        );
                        session_changed = true;
                        match effect {
                            PowerKeyEffect::RelitOutputs
                            | PowerKeyEffect::WokeDisplay
                            | PowerKeyEffect::Nothing => {}
                            PowerKeyEffect::Suspend => {
                                sleep_lock_was_locked = session.is_locked();
                                sleep_requested_by_idle = false;
                                power_bridge.request_suspend();
                            }
                            PowerKeyEffect::Lock => {
                                apply_session_transitions!(session.request_lock(Instant::now()));
                            }
                            PowerKeyEffect::Blank => {
                                apply_session_transitions!(session.blank_now(Instant::now()));
                            }
                        }
                    } else if let PriorityRoute::VtSwitch(vt) = route {
                        // Ahead of every other chord, never offered to a client: leaving the
                        // terminal must work even with a grabbed keyboard, or a client can strand the user.
                        backend.switch_vt(vt);
                    } else if route == PriorityRoute::RemoteInputKillSwitch {
                        // Super+Esc: every peer's input stops and stays stopped. Ahead of the
                        // client router and shell's Escape handling — a panic key a client can swallow isn't one.
                        client_input::stop_remote_input(
                            &mut state,
                            &mut frame_builder,
                            spatial_control.is_some(),
                        );
                        end_spatial_control_or_forget(peer_sync.as_deref(), &mut spatial_control);
                        session_changed = true;
                    } else if (state.pointer_lock.is_engaged()
                        || state.shortcut_inhibit.is_active())
                        && crate::compositor::pointer_lock::is_release_chord(
                            wev,
                            test_control_ctx.modifiers,
                        )
                    {
                        // Ctrl+Alt+Esc: always gets the pointer back from a
                        // game that locked or confined it, and the shell
                        // shortcuts back from an app that captured them —
                        // ahead of the client router, so the app cannot
                        // swallow it.
                        if let Some(change) = state.release_pointer_constraint() {
                            apply_pointer_constraint_change(backend.as_mut(), change);
                            frame_builder.with_chrome(|chrome| {
                                chrome.show_osd(crate::shell::osd::Osd::pointer_held(false));
                            });
                        }
                        if state.suspend_shortcut_inhibit() {
                            frame_builder.with_chrome(|chrome| {
                                chrome.show_osd(crate::shell::osd::Osd::keys_captured(false));
                            });
                        }
                        session_changed = true;
                    } else if route == PriorityRoute::LockSession {
                        // Super+L locks immediately.
                        apply_session_transitions!(session.request_lock(Instant::now()));
                    } else if route == PriorityRoute::ToggleMouseKeys {
                        // The compositor applies the flipped preference on its
                        // next settings poll; the readout confirms it now.
                        if let Some(osd) = crate::shell::media_keys::toggle_mouse_keys() {
                            frame_builder.with_chrome(|st| publish_media_osd(st, osd));
                            session_changed = true;
                        }
                    } else if let PriorityRoute::MediaKey(mk) = route {
                        // Hardware media/brightness/machine keys belong to the machine, not
                        // whichever client holds focus — handled before client routing for that
                        // reason. While locked only `MediaKey::acts_while_locked` keys get
                        // here, and the lock paint draws their OSD.
                        let airplane_mode =
                            frame_builder.chrome().quick_settings.radios.airplane_mode;
                        let brightness_key = matches!(
                            mk,
                            crate::shell::media_keys::MediaKey::BrightnessUp
                                | crate::shell::media_keys::MediaKey::BrightnessDown
                        );
                        let displays = if brightness_key {
                            brightness_displays(
                                os_actions::BrightnessService::global().levels(),
                                backend.outputs(),
                            )
                        } else {
                            Vec::new()
                        };
                        // The display the person works on, not the one the
                        // pointer happens to rest on.
                        let focused_display = frame_builder.host().focused_connector();
                        let audio: &crate::shell::audio_writer::AudioWriter = media_key_audio
                            .get_or_insert_with(|| {
                                crate::shell::audio_writer::AudioWriter::system(
                                    frame_builder.host().status_bridge().cloned(),
                                )
                            });
                        let response = crate::shell::media_keys::apply(
                            mk,
                            &crate::shell::media_keys::Context {
                                actions: frame_builder.host().qs_actions(),
                                audio,
                                status: frame_builder.host().status_bridge(),
                                transport_osd: &transport_osd,
                                airplane_mode,
                                brightness: crate::shell::media_keys::BrightnessKeys {
                                    displays: &displays,
                                    focused: focused_display.as_deref(),
                                    all: test_control_ctx.modifiers.state().shift_key(),
                                },
                            },
                        );
                        match response {
                            Some(crate::shell::media_keys::Response::Osd(osd)) => {
                                frame_builder.with_chrome(|st| publish_media_osd(st, osd));
                                session_changed = true;
                            }
                            Some(crate::shell::media_keys::Response::Suspend) => {
                                tracing::info!(target: "tilo::power", "sleep key: requesting suspend");
                                sleep_lock_was_locked = session.is_locked();
                                sleep_requested_by_idle = false;
                                session_changed = true;
                                power_bridge.request_suspend();
                            }
                            Some(crate::shell::media_keys::Response::OpenDisplayModes) => {
                                frame_builder.with_chrome(|st| {
                                    st.open_projection();
                                    st.bump();
                                });
                                session_changed = true;
                            }
                            Some(crate::shell::media_keys::Response::Launch {
                                package_id,
                                display_name,
                            }) => {
                                frame_builder.host().launch_app(package_id, display_name);
                                session_changed = true;
                            }
                            None => {}
                        }
                    } else if peer_sync
                        .as_ref()
                        .is_some_and(|network| finish_tab_move(wev, network, &mut spatial_control))
                    {
                        frame_builder.with_chrome(
                            crate::shell::chrome_state::ChromeState::request_tab_drag_cancel,
                        );
                    } else if peer_sync.as_ref().is_some_and(|network| {
                        forward_spatial_window_event(wev, network, &mut spatial_control)
                    }) {
                        // The physical keyboard/pointer is logically on the
                        // adjacent certified device until it crosses back:
                        // Escape there is that device's. Reserved shell chords
                        // above stay local and remain an unconditional escape
                        // path (Super+Esc ends control).
                    } else {
                        // InputCapture deliberately sits after the compositor's
                        // power/VT/lock/panic controls but before client/chrome
                        // dispatch. Once a barrier activates, the event goes to
                        // the session's EIS socket and nowhere else.
                        if let Some(portal) = portal_backend.as_ref()
                            && portal_capture_event(wev, &state)
                                .is_some_and(|event| portal.observe_input_capture(event))
                        {
                            continue;
                        }
                        // Backends that don't pair a `ModifiersChanged` with every modifier
                        // keystroke would leave the mask stale, which decides chord ownership.
                        if let WindowEvent::KeyboardInput { event: key_ev, .. } = wev {
                            client_input::apply_modifier_key(
                                &mut test_control_ctx.modifiers,
                                key_ev,
                            );
                        }
                        // Offer the event to the focused Wayland client first. `route_input`
                        // returns `false` for anything the shell owns, so only client-bound events skip the chrome dispatch below.
                        let to_client = client_input_allowed(&frame_builder)
                            && client_input::route_input(
                                &mut state,
                                &mut frame_builder,
                                wev,
                                &test_control_ctx.modifiers,
                            );
                        if let Some(steps) = key_steps.as_mut() {
                            steps.steps.mark("client");
                        }
                        // A finger on the on-screen keyboard types; the chrome
                        // under the keyboard never sees it.
                        let keyboard_contact = matches!(
                            wev,
                            WindowEvent::Touch(touch)
                                if u32::try_from(touch.id)
                                    .is_ok_and(|slot| osk_touch_slots.contains(&slot))
                        );
                        if !to_client && !keyboard_contact {
                            session_changed |= client_input::finish_chrome_drag_into_shell(
                                &mut state,
                                &mut frame_builder,
                                wev,
                            );
                            if let Some(steps) = key_steps.as_mut() {
                                steps.steps.mark("drag-handoff");
                            }
                            // Dispatch real input through the same wire the headless `execute_planned_action` path uses.
                            chrome_handled = frame_builder.dispatch_event(
                                wev,
                                &test_control_ctx.modifiers,
                                &mut ui::widget::Edits::lend(&mut test_control_ctx.doc),
                                &test_control_ctx.root_obj,
                                &mut test_control_ctx.focus,
                            );
                            session_changed |= chrome_handled;
                            if let Some(steps) = key_steps.as_mut() {
                                steps.steps.mark("chrome");
                            }
                            // Chrome input can change the active tab/group/workspace. The
                            // backend may have already queued the next physical key in this
                            // same batch, so move the Wayland seat now rather than only after
                            // the batch: click-tab then type must never send the first key to
                            // the tab that was active before the click.
                            frame_builder.host_mut().flush_active_tab_writeback();
                            client_input::sync_keyboard_focus(&mut state, &frame_builder);
                            if let Some(steps) = key_steps.as_mut() {
                                steps.steps.mark("focus-sync");
                            }
                        }
                    }
                    // Touch nobody took raw becomes pointer input, processed
                    // right now through the physical pointer path: the chrome,
                    // the lock screen and touch-less clients all understand it.
                    if let WindowEvent::Touch(touch) = wev {
                        for event in crate::compositor::touch_pointer::pointer_events_for_touch(
                            &mut test_control_ctx.touch_pointer,
                            touch,
                            chrome_handled,
                            state.touch_down_taken(),
                            &frame_builder,
                        )
                        .into_iter()
                        .rev()
                        {
                            pending_events.push_front(BackendEvent::Input(event));
                        }
                    }
                }
                BackendEvent::RelativePointerMotion {
                    dx,
                    dy,
                    dx_unaccel,
                    dy_unaccel,
                    time_usec,
                } => {
                    if session.is_locked() {
                        end_spatial_control_or_forget(peer_sync.as_deref(), &mut spatial_control);
                    } else if state.pointer_lock.is_engaged() {
                        // A game holding the pointer reads every delta; the
                        // pointer must not wander onto another device.
                        state.forward_client_relative_motion(
                            (dx, dy),
                            (dx_unaccel, dy_unaccel),
                            time_usec,
                        );
                    } else {
                        let motion = peer_sync.as_ref().map_or(
                            crate::compositor::spatial_pointer::SpatialMotion::Local,
                            |network| {
                                move_spatial_pointer(
                                    &state,
                                    network,
                                    &mut spatial_control,
                                    (dx, dy),
                                )
                            },
                        );
                        match motion {
                            crate::compositor::spatial_pointer::SpatialMotion::Local => {
                                state.forward_client_relative_motion(
                                    (dx, dy),
                                    (dx_unaccel, dy_unaccel),
                                    time_usec,
                                );
                            }
                            crate::compositor::spatial_pointer::SpatialMotion::Remote => {
                                // A window dragged across the edge goes along.
                                if let Some(session) = spatial_control.as_mut()
                                    && session.dragged_tab.is_none()
                                {
                                    session.dragged_tab = frame_builder.host().dragged_tab();
                                }
                            }
                            crate::compositor::spatial_pointer::SpatialMotion::Returned(point) => {
                                if let Some(point) = point {
                                    let placements = crate::compositor::backend::layout::placements(
                                        backend.outputs(),
                                    );
                                    let (x, y) =
                                        crate::compositor::backend::layout::logical_to_scene(
                                            &placements,
                                            point,
                                        );
                                    backend.warp_pointer(x, y);
                                    queue_spatial_return_event(
                                        &mut pending_events,
                                        point,
                                        spatial_pointer_parked
                                            .then(|| state.client_input.pointer_position())
                                            .flatten(),
                                    );
                                }
                            }
                        }
                    }
                }
                BackendEvent::ModifiersChanged(modifiers) => {
                    // Carry the modifier mask forward so the next event dispatched (or the
                    // lock-chord check above) sees the right Ctrl/Shift/Alt/Super state.
                    test_control_ctx.modifiers = modifiers;
                    // An open search previews on its selected row what Enter
                    // does with the modifier just pressed or released.
                    if frame_builder
                        .host_mut()
                        .note_held_modifiers(modifiers.state())
                    {
                        session_changed = true;
                    }
                }
                BackendEvent::KeyboardConfiguration { scope, settings } => {
                    frame_builder.host_mut().set_keyboard_scope(scope.clone());
                    if let Some(xwayland) = xwayland.as_ref() {
                        xwayland.apply_keyboard_settings(&settings);
                    }
                    state.apply_keyboard_settings(*settings);
                    input_methods.reload_for_scope(&mut state, &live_settings.snapshot(), &scope);
                }
                BackendEvent::InputStateReset => {
                    // A device/VT/keymap boundary can omit releases. Retire
                    // Smithay's actual pressed-key state, never just the
                    // chrome's modifier snapshot, before accepting another
                    // physical event.
                    state.reset_client_keyboard_state();
                    end_spatial_control_or_forget(peer_sync.as_deref(), &mut spatial_control);
                    test_control_ctx.modifiers = Modifiers::default();
                }
                BackendEvent::OutputsChanged => {
                    // A display changed — everything derived from the output set re-derives
                    // at once: scene extent, per-display widths, fullscreen size, cadence, `wl_output` globals.
                    outputs_dirty = true;
                    session_changed = true;
                    if let Some(portal) = portal_backend.as_ref() {
                        portal
                            .input_capture
                            .set_zones(input_capture_zones(backend.outputs()));
                    }
                }
                BackendEvent::ShellGesture(action, at) => {
                    // A 3/4-finger gesture, already resolved into its navigation meaning —
                    // never also delivered to a client, so no double-handling to guard against.
                    if !session.is_locked() && client_input_allowed(&frame_builder) {
                        apply_shell_gesture(&mut frame_builder, action, at);
                        session_changed = true;
                    }
                }
                BackendEvent::Gesture(gesture) => {
                    // A one- or two-finger gesture: the focused client's.
                    if !session.is_locked() && client_input_allowed(&frame_builder) {
                        state.forward_client_gesture(gesture);
                    }
                }
                BackendEvent::Touch(touch) => {
                    // Direct touch reaches the shell as raw touch first; a
                    // confirmed one-finger tap later gets one synthesized
                    // pointer click for chrome. This half is `wl_touch` for
                    // real multi-touch clients.
                    // A direct-touch stream can be transferred to a shell
                    // three-finger gesture after its first contacts reached a
                    // client. Its mandatory `wl_touch.cancel` must still get
                    // through even if that gesture also opened chrome which
                    // now captures ordinary input.
                    use crate::compositor::backend::TouchEvent as Contact;
                    let on_keyboard = match touch {
                        Contact::Down { slot, x, y } => {
                            let hit = frame_builder.osk_surface_hit((x, y)).is_some();
                            if hit {
                                osk_touch_slots.insert(slot);
                            }
                            hit
                        }
                        Contact::Motion { slot, .. } => osk_touch_slots.contains(&slot),
                        Contact::Up { slot } => osk_touch_slots.remove(&slot),
                        Contact::Cancel => {
                            osk_touch_slots.clear();
                            false
                        }
                        Contact::Frame => false,
                    };
                    client_input::route_client_touch(
                        &mut state,
                        &mut frame_builder,
                        touch,
                        session.is_locked(),
                        on_keyboard,
                    );
                }
                BackendEvent::InputDeviceAdded { id, capabilities } => {
                    if test_control_ctx.input.device_added(id, capabilities) {
                        test_control_ctx.publish_input_environment(&mut frame_builder);
                        session_changed = true;
                    }
                }
                BackendEvent::InputDeviceRemoved { id } => {
                    if test_control_ctx.input.device_removed(&id) {
                        test_control_ctx.publish_input_environment(&mut frame_builder);
                        session_changed = true;
                    }
                }
                BackendEvent::TabletMode(on) => {
                    test_control_ctx.input.set_tablet_mode(on);
                }
                BackendEvent::PointerMotionUsed => {
                    if test_control_ctx.input.note_pointer_motion() {
                        test_control_ctx.publish_input_environment(&mut frame_builder);
                        session_changed = true;
                    }
                }
                BackendEvent::InputUsed(modality) => {
                    if test_control_ctx.input.note_used(modality) {
                        test_control_ctx.publish_input_environment(&mut frame_builder);
                        session_changed = true;
                    }
                }
                BackendEvent::Focus(focused) => {
                    if !focused {
                        end_spatial_control_or_forget(peer_sync.as_deref(), &mut spatial_control);
                    }
                }
                BackendEvent::VisualAlert => {
                    frame_builder.with_chrome(|st| st.flash_visual_alert(Instant::now()));
                    session_changed = true;
                }
                BackendEvent::Redraw
                | BackendEvent::Tablet(_)
                | BackendEvent::TabletPad(_)
                | BackendEvent::TabletPadMap { .. }
                | BackendEvent::TabletPadAdded(_)
                | BackendEvent::TabletPadRemoved(_)
                | BackendEvent::Sibling(_) => {
                    // Redraw is implicit: the native loop paints every tick.
                    // Pen and pad events were expanded into client or
                    // ordinary input before the batch, another device's
                    // input unwrapped above.
                }
            }
        }
        state.physical_key_input_time_ns = None;

        input_steps.step(event_step);
        // Automatic scanning is generated by the same session owner, then
        // offered to the focused Wayland client or shell through the exact
        // normal Tab press/release route. It never runs on the lock screen.
        if !session.is_locked()
            && let Some(action) = switch_control.tick(Instant::now())
        {
            for pressed in [true, false] {
                let event = switch_action_event(action, pressed);
                let to_client = client_input_allowed(&frame_builder)
                    && client_input::route_input(
                        &mut state,
                        &mut frame_builder,
                        &event,
                        &test_control_ctx.modifiers,
                    );
                if !to_client {
                    let _ = frame_builder.dispatch_event(
                        &event,
                        &test_control_ctx.modifiers,
                        &mut ui::widget::Edits::lend(&mut test_control_ctx.doc),
                        &test_control_ctx.root_obj,
                        &mut test_control_ctx.focus,
                    );
                }
            }
            session_changed = true;
        }

        if pump_controller_navigation(
            &mut controller_nav,
            &mut state,
            &mut frame_builder,
            &mut test_control_ctx,
            session.is_locked(),
        ) {
            session_changed = true;
            apply_session_transitions!(session.note_activity(Instant::now()));
        }
        for wake in controller_nav.take_wakes() {
            use crate::compositor::controller_nav::ControllerWake;
            match wake {
                ControllerWake::Watch { key, fd } => match event_loop.handle().insert_source(
                    // Edge-triggered: the runtime reads until the kernel
                    // queue is empty or asks for an immediate next pass.
                    Generic::new(fd, Interest::READ, Mode::Edge),
                    |_, _, _| Ok(PostAction::Continue),
                ) {
                    Ok(token) => {
                        controller_wakes.insert(key, token);
                    }
                    Err(error) => {
                        tracing::warn!(%error, "controller input cannot wake the compositor");
                    }
                },
                ControllerWake::Unwatch { key } => {
                    if let Some(token) = controller_wakes.remove(&key) {
                        event_loop.handle().remove(token);
                    }
                }
            }
        }

        input_steps.step("controllers");
        state.expire_host_launch_activations();
        frame_builder.with_chrome(|chrome| {
            chrome.flush_notification_actions(|| Some(state.mint_activation_token()));
        });
        crate::shell::launcher::expire_pending_placements();
        session_changed |=
            crate::compositor::selection::pump_pending_shell_drop(&mut state, &mut frame_builder);
        session_changed |= launch_queued_host_apps(&mut state, &mut frame_builder, &session_wake);
        session_changed |= post_codec_pack_notices(&mut frame_builder);
        frame_builder.with_chrome(|chrome| {
            session_changed |= chrome.post_queued(crate::shell::storage_notice::drain());
            session_changed |= chrome.post_queued(crate::shell::graphics_notice::drain());
            session_changed |= chrome.post_queued(crate::shell::crash_notice::drain());
            session_changed |=
                chrome.post_consent_notices(crate::shell::app_action_consent::drain_notices());
        });
        session_changed |= post_system_update_asks(&mut frame_builder, &session);
        session_changed |= post_collaborator_version_notices(&mut frame_builder);
        session_changed |= post_drive_events(&mut frame_builder);
        run_autostart_once(&frame_builder);
        if let Some(published) = published_room.as_mut() {
            published.follow(
                frame_builder
                    .chrome()
                    .tree()
                    .desktop
                    .active_workspace()
                    .map(|workspace| workspace.room().to_string()),
            );
        }

        // Keyboard releases are latency-sensitive protocol traffic: an Xwayland
        // client starts auto-repeat after a press and only stops once it sees the
        // matching release.  Scene construction can occasionally block on a GPU
        // fence for hundreds of milliseconds, so waiting until the end-of-frame
        // flush below turns an ordinary tap into a burst (or apparently stuck
        // repeat) under render pressure.  Flush events queued by this input batch
        // before doing any layout/render work.  The later flush remains necessary
        // for protocol events produced while dispatching client requests.
        input_steps.step("launches");
        if had_backend_events && let Err(err) = display.flush_clients() {
            eprintln!("tilo compositor: input flush_clients error: {err}");
            break;
        }
        input_steps.step("input-flush");
        // Click-to-focus persistence copies the full workspace tree for the
        // state writer. Keep even that copy out of `route_mouse_input`: queue
        // and flush wl_keyboard.enter/wl_pointer.button first so the focused
        // application sees the click, or the next already-buffered key,
        // before any of it.
        if state.client_input.take_focus_persistence_pending() {
            state.workspace_tree.read().queue_save();
        }

        input_steps.step("focus-persist");
        // A settings write wakes the loop through `live_settings`, which has
        // already read the new document on its own thread; between changes
        // this is one atomic load. Only the work the changed sections need
        // runs: a notification toggle must not reconfigure input devices or
        // reconcile outputs on this input path.
        if state.output_management.poll_staging() {
            // The staged transaction was written off this thread; look now
            // rather than on the next poll.
            live_settings.refresh();
        }
        let settings_change = crate::compositor::backend::drm::input_config::SettingsChange::from(
            settings_sections.changed_sections(&live_settings),
        );
        if !settings_change.is_empty() {
            input_steps.step("settings-poll");
            tracing::info!(
                ?settings_change,
                "compositor: settings changed on disk; re-applying"
            );
            if settings_change.keyboard() {
                state.reload_keyboard_settings();
                if let Some(xwayland) = xwayland.as_mut() {
                    xwayland.reload_keyboard_settings();
                }
            }
            input_steps.step("settings-keyboard");
            if settings_change.input_devices() {
                backend.reload_input_settings();
            }
            input_steps.step("settings-input-devices");
            if settings_change.displays() {
                state.scale = crate::compositor::state::display_scale_from(
                    &live_settings.snapshot().document,
                );
                // Re-applies from the last hotplug probe; never force-probes.
                backend.reload_display_settings();
                // Preserve Keep/Revert tracking after an Apply receipt was drained.
                state.output_management.resolve_transactions(
                    settings_store::hardware::DisplayTransaction::load().as_ref(),
                );
                outputs_dirty = true;
            }
            input_steps.step("settings-displays");
            if settings_change.shortcuts() {
                // Shortcut ownership is decided before a focused client sees the
                // press. Refresh the host's validated snapshot in the same settings
                // transaction so changes made in Settings take effect live rather
                // than only after the next compositor session.
                if let Err(error) = frame_builder
                    .host_mut()
                    .set_shortcut_config(settings_store::hardware::ShortcutConfig::load())
                {
                    tracing::warn!(?error, "compositor: rejected invalid shortcut settings");
                }
                // A GlobalShortcuts grant may not take a chord the user has just
                // assigned to a desktop action.
                state
                    .global_shortcuts
                    .set_shortcut_config(frame_builder.host().shortcut_config().clone());
            }
            if settings_change.desktop_control_stop() {
                tab_grant_watch.stop_requested(&frame_builder);
            }
            if settings_change.desktop_controls() {
                frame_builder.host_mut().set_desktop_controls_config(
                    settings_store::hardware::DesktopControlsConfig::load(),
                );
            }
            if settings_change.display_identify()
                && frame_builder.host_mut().poll_display_identify()
                && let Some(network) = peer_sync.as_ref()
            {
                // Identify can follow an arrangement edit in the same tick;
                // resolve that explicit request instead of numbering from an
                // older periodic sample.
                crate::shell::device_fabric_runtime::person_layout::refresh(
                    network,
                    continuity_policy.current(),
                );
                if let Some(view) =
                    crate::shell::device_fabric_runtime::person_layout::current(network)
                {
                    frame_builder
                        .host_mut()
                        .number_identified_displays(&view.layout);
                }
            }
            input_steps.step("settings-shortcuts");
            if settings_change.appearance() {
                // An appearance write must rebuild chrome on this tick and
                // wake tiled apps so they paint the new Theme instead of
                // keeping the last buffer.
                frame_builder
                    .with_chrome(crate::shell::chrome_state::ChromeState::note_appearance_change);
                for toplevel in state.xdg_shell_state.toplevel_surfaces() {
                    if toplevel.is_initial_configure_sent() {
                        toplevel.send_configure();
                    }
                }
                force_first_paint = true;
            }
            input_steps.step("settings-appearance");
            if settings_change.agent_providers() {
                crate::shell::settings_records::provider_settings_changed();
            }
            if settings_change.power() {
                power_policy = reapply_power_policy(&mut session, &power_bridge, headless);
            }
            input_steps.step("settings-power");
            if settings_change.keyboard() {
                input_methods.reload(&mut state, &live_settings.snapshot());
            }
            input_steps.step("settings-input-methods");
        }
        // Drain backend receipts even when the storage completion followed
        // its file-change edge. Only the matching reconciled transaction
        // acknowledges the admitted request.
        if state.output_management.has_pending_apply() {
            state.output_management.resolve_transactions(
                settings_store::hardware::DisplayTransaction::load().as_ref(),
            );
        }
        input_steps.step("settings");
        input_methods.follow_input(&mut state, &test_control_ctx.input);
        input_steps.step("input-method-follow");
        input_methods.tick_with_steps(&mut state, |step| input_steps.step(step));
        if let Some(rotation) = device_rotation
            .as_ref()
            .and_then(|changes| changes.try_iter().last())
        {
            backend.set_device_rotation(Some(rotation));
        }
        input_steps.step("device-rotation");
        if let Some(confirmation) = state.output_management.take_confirmation() {
            frame_builder
                .host_mut()
                .show_display_confirmation(confirmation.transaction, confirmation.tool);
        }
        input_steps.step("display-confirmation");
        for notice in state.output_management.take_notices() {
            match notice {
                crate::compositor::output_management::DisplayToolNotice::ReappliedAfterRevert {
                    tool,
                } => frame_builder
                    .host_mut()
                    .report_refused_display_reapply(tool.as_ref()),
            }
        }

        // Re-derive everything depending on the output set once, after the whole
        // batch — a dock plug raises several udev edges, and per-edge re-derivation would advertise globals the next edge immediately supersedes.
        input_steps.step("display-notices");
        let output_geometry_changed = std::mem::take(&mut outputs_dirty);
        if output_geometry_changed {
            let outs = backend.outputs().to_vec();
            let combined = combined_geometry(&outs);
            tracing::info!(
                outputs = outs.len(),
                combined = ?combined,
                "compositor: display configuration changed"
            );
            if combined.0 > 0 && combined.1 > 0 {
                let logical = logical_geometry(&outs);
                frame_builder.set_viewport(combined);
                native_accessibility.set_viewport(logical);
                state.output_size = size_to_signed(logical);
                if let Some(pipe) = present_pipe.as_ref() {
                    pipe.resize(combined);
                }
            }
            // Windows aren't pinned to an output; re-pushing geometry re-homes
            // everything on a display that went away, so nothing's left with no panel behind it.
            frame_builder.set_output_geometry(output_geometry(&outs));
            if rehome_native_floating_surfaces(&mut state, &outs, initial_size) {
                force_first_paint = true;
            }
            if let Some(service) = floating_surface_service.as_ref() {
                service.broadcast(
                    &ui::floating_surface::FloatingSurfaceHostMessage::DesktopBounds {
                        bounds: ui::floating_surface::protocol_rect_array(floating_desktop_bounds(
                            &outs,
                            initial_size,
                        )),
                    },
                );
            }
            // Per-output scale: `wp_fractional_scale` is the scale of the output a
            // surface is on, so a changed layout changes what each client renders at.
            state.set_output_layout(&outs);
            // A leave has to precede removal of the corresponding `wl_output`
            // global. Once the global is withdrawn there is no client output
            // object left to put in `wl_surface.leave`.
            send_surface_output_changes(
                &state,
                &output_globals,
                surface_output_memberships.leave_removed_outputs(&outs),
            );
            output_globals.sync::<State>(
                &display_handle_for_outputs,
                &outs,
                refresh_mhz_of(&*backend),
            );
            state.set_output_heads(backend.output_heads());
            if refresh_override.is_none()
                && let Some(interval) = backend.refresh_interval()
                && frame_clock.set_interval(interval)
            {
                eprintln!(
                    "tilo compositor: display configuration changed; now pacing to {:.3} Hz",
                    1.0 / interval.as_secs_f64()
                );
                metrics.set_target_period(interval);
            }
        }
        // Remove disconnected heads before applying queued owner power requests.
        for request in state.output_management.take_power_requests() {
            if apply_output_power_request(
                request,
                &mut session,
                backend.as_mut(),
                present_pipe.as_ref(),
                local_session_lifecycle.as_ref(),
                &mut screen_dimmer,
                &mut state.output_management,
                Instant::now(),
            ) {
                force_first_paint = true;
                session_changed = true;
            }
        }
        input_steps.step("outputs");
        let t_input_done = iter_t0.elapsed();
        metrics.mark_input(t_input_done);
        metrics.mark_input_step(input_steps);
        let input_dispatch = t_input_done.saturating_sub(t_backend_poll_done);
        if slow_prepaint_phase(input_dispatch) {
            let (slowest_step, slowest_took) = input_steps.slowest();
            tracing::warn!(
                target: "tilo::latency",
                elapsed_ms = input_dispatch.as_secs_f64() * 1_000.0,
                backend_poll_ms = t_backend_poll_done.as_secs_f64() * 1_000.0,
                slowest_step,
                slowest_step_ms = slowest_took.as_secs_f64() * 1_000.0,
                had_backend_events,
                "pre-paint input and service dispatch stalled compositor input"
            );
        }
        if loop_timing {
            pump_checkpoint = Instant::now();
            pump_slowest = ("pump-start", Duration::ZERO);
        }

        let pump_bookkeeping_started = Instant::now();
        // The power menu is shell UI only.  Its Lock row requests this
        // compositor-owned transition through ChromeState; drain the edge
        // here so it takes the exact same SessionManager path as Super+L and
        // hardware/power-service locks (including focus and input cutoff).
        let mut shell_lock_requested = false;
        frame_builder.with_chrome(|chrome| {
            shell_lock_requested = chrome.take_session_lock_request();
        });
        if shell_lock_requested {
            apply_session_transitions!(session.request_lock(Instant::now()));
            session_changed = true;
        }

        if let Some(net) = peer_sync.as_ref().filter(|_| {
            session_followed.is_none_or(|followed| followed.elapsed() >= Duration::from_secs(1))
        }) {
            session_followed = Some(Instant::now());
            let followed = follow_owner_session(net, &mut session, &mut session_follow);
            if followed == Some(crate::shell::owner_session::Follow::Unlock) {
                // As after the person's own unlock: the next lock starts bare.
                lock_keyboard = lock_screen::LockTouchKeyboard::default();
                frame_builder.set_lock_keyboard_keys(Vec::new());
                frame_builder.set_lock_surfaces(Vec::new());
            }
            let transitions = match followed {
                Some(crate::shell::owner_session::Follow::Lock) => {
                    session.request_lock(Instant::now())
                }
                Some(crate::shell::owner_session::Follow::Unlock) => {
                    session.request_unlock(Instant::now())
                }
                None => Vec::new(),
            };
            for t in transitions {
                session_changed = true;
                apply_session_transition(
                    t,
                    backend.as_mut(),
                    present_pipe.as_ref(),
                    local_session_lifecycle.as_ref(),
                    &mut screen_dimmer,
                    Some(&mut state.output_management),
                );
            }
        }

        // Advance the idle timers and apply any timeout-driven transitions (blank, auto-lock, suspend).
        for t in session.tick_inhibited(Instant::now(), &inhibit_registry) {
            if t == SessionTransition::Locked {
                session_follow.locked_by_idle();
            }
            if t == SessionTransition::Suspend {
                // Idle-suspend is gated by the inhibitor registry (media playing, a task
                // running). `tick_inhibited` already restarts the period while a hold is
                // seen; one taken since then un-latches here, and the period restarts
                // from the hold's last tick once it lets go.
                if inhibit_registry.is_inhibited() {
                    session.cancel_suspend();
                    tracing::debug!(
                        target: "tilo::power",
                        reasons = ?inhibit_registry.active_reasons(),
                        "idle-suspend deferred: inhibitor held"
                    );
                } else {
                    tracing::info!(target: "tilo::power", "idle threshold reached: requesting suspend");
                    sleep_lock_was_locked = session.is_locked();
                    sleep_requested_by_idle = true;
                    power_bridge.request_idle_suspend();
                }
            } else {
                apply_session_transition(
                    t,
                    backend.as_mut(),
                    present_pipe.as_ref(),
                    local_session_lifecycle.as_ref(),
                    &mut screen_dimmer,
                    Some(&mut state.output_management),
                );
            }
            // Any session transition (blank/lock/suspend) changes what should be on screen — force a repaint.
            session_changed = true;
        }

        // Apply signals the async power service raised (lid/critical lock, battery
        // toasts, resume); suspend requests flow the other way via `power_bridge.request_suspend()` above.
        for sig in power_bridge.drain_signals() {
            session_changed = true;
            match sig {
                tilo_power_mgmt::PowerSignal::Lock => {
                    apply_session_transitions!(session.request_lock(Instant::now()));
                }
                tilo_power_mgmt::PowerSignal::PreparingForSleep => {
                    // Preserve a lock that was already present at request
                    // time, and also observe a manual/lid lock that arrived
                    // while the async suspend request was in flight.
                    sleep_lock_was_locked |= session.is_locked();
                    let transitions =
                        session.prepare_for_sleep(Instant::now(), power_policy.lock_on_sleep);
                    if sleep_requested_by_idle {
                        // The idle timer's sleep locks like the idle timer:
                        // the lock stays on this device.
                        for t in transitions {
                            if t == SessionTransition::Locked {
                                session_follow.locked_by_idle();
                            }
                            apply_session_transition(
                                t,
                                backend.as_mut(),
                                present_pipe.as_ref(),
                                local_session_lifecycle.as_ref(),
                                &mut screen_dimmer,
                                Some(&mut state.output_management),
                            );
                        }
                    } else {
                        apply_session_transitions!(transitions);
                    }
                    sleep_lock_frame_pending = true;
                    sleep_lock_frame_serial = None;
                    sleep_lock_frame_failed = false;
                    // A machine that never wakes (a flat battery) must still
                    // come back to the layout it went to sleep with.
                    crate::shell::persist::wait_for_saves_within(Duration::from_secs(2));
                }
                tilo_power_mgmt::PowerSignal::SuspendFailed => {
                    let was_prepared = sleep_lock_frame_pending;
                    sleep_lock_frame_pending = false;
                    sleep_lock_frame_serial = None;
                    sleep_lock_frame_failed = false;
                    session.cancel_suspend();
                    sleep_requested_by_idle = false;
                    // A lid/critical request can fail before it ever emits
                    // PreparingForSleep. Only undo a lock that this pending
                    // sleep actually installed; never unlock an unrelated
                    // user lock when CanSuspend is refused.
                    if should_unlock_after_suspend_failure(was_prepared, sleep_lock_was_locked) {
                        apply_session_transitions!(session.request_unlock(Instant::now()));
                    }
                    sleep_lock_was_locked = false;
                    tracing::warn!(target: "tilo::power", "suspend request failed; session remains awake");
                }
                tilo_power_mgmt::PowerSignal::ResumedFromSleep => {
                    // Reset the idle timer so a stale pre-sleep timestamp doesn't immediately re-blank/re-suspend on resume.
                    apply_session_transitions!(session.resume_from_sleep(Instant::now()));
                    // Put the display back — a suspend doesn't pause the seat, so the DRM device
                    // returns with no mode/framebuffer/cursor plane; without this, lid close/open is an unrecoverable black screen.
                    backend.reset_display();
                    backend.note_wake("resume");
                    sleep_lock_frame_pending = false;
                    sleep_lock_frame_serial = None;
                    sleep_lock_frame_failed = false;
                    sleep_lock_was_locked = false;
                    sleep_requested_by_idle = false;
                    // A resumed panel is scanning out whatever was left in its buffers, so the next frame must be a full one.
                    outputs_dirty = false;
                    session_changed = true;
                }
                tilo_power_mgmt::PowerSignal::IdleSuspendChanged(timeout) => {
                    let timeout = timeout.filter(|_| !headless);
                    session.set_suspend_timeout(timeout);
                    tracing::info!(target: "tilo::power", ?timeout, "live idle-suspend timeout updated");
                }
                tilo_power_mgmt::PowerSignal::Notify(n) => {
                    let inc = power_notification_to_incoming(&notif_bridge, n);
                    notif_bridge.push_incoming(inc);
                }
            }
        }
        if sleep_lock_frame_pending && session.is_locked() && session.is_blanked() {
            // DPMS-off already guarantees no client content is visible. Do
            // not flash the panel on merely to paint before suspend; the
            // locked state will be the first full frame after resume.
            power_bridge.lock_screen_drawn();
            sleep_lock_frame_pending = false;
            sleep_lock_frame_serial = None;
            sleep_lock_frame_failed = false;
        } else if sleep_lock_frame_pending
            && session.is_locked()
            && let Some((serial, pipe)) = sleep_lock_frame_serial.zip(present_pipe.as_ref())
        {
            match sleep_lock_frame_status(serial, pipe.presented(), pipe.is_finished()) {
                SleepLockFrameStatus::Awaiting => {}
                SleepLockFrameStatus::Retired => {
                    power_bridge.lock_screen_drawn();
                    sleep_lock_frame_pending = false;
                    sleep_lock_frame_serial = None;
                    sleep_lock_frame_failed = false;
                }
                SleepLockFrameStatus::PresenterLost => {
                    if !sleep_lock_frame_failed {
                        tracing::error!(
                            target: "tilo::power",
                            serial,
                            "presenter exited before the required lock frame retired; retaining sleep delay inhibitor"
                        );
                        sleep_lock_frame_failed = true;
                    }
                }
            }
        }

        // `ext_session_lock_v1`: fold a protocol client's lock into the one
        // authoritative session lock. Client `lock` drives the same locked state as
        // chord/idle. `unlock_and_destroy` only releases the protocol object:
        // because Tilo paints the lock UI itself, it is not evidence that the
        // user authenticated. The stashed
        // `SessionLocker` confirms after the overlay paints, below, so `locked` fires only once content is hidden.
        if state.session_lock.take_lock_requested() {
            session_changed = true;
            apply_session_transitions!(session.request_lock(Instant::now()));
        }
        // Async unlock: pick up an in-flight credential verify's result from the
        // worker thread (started by lock-screen Enter). The loop never blocks —
        // it keeps ticking and unlocks only on verified success; a wrong/failed
        // result surfaces a non-leaky error and stays locked. Polled every tick (~16ms).
        if let Some(outcome) = lock.poll_verification() {
            // The field/status changed (unlocked, or error shown) — repaint.
            session_changed = true;
            if outcome == UnlockOutcome::Unlocked {
                // The next lock starts without the keyboard and its layer.
                lock_keyboard = lock_screen::LockTouchKeyboard::default();
                frame_builder.set_lock_keyboard_keys(Vec::new());
                frame_builder.set_lock_surfaces(Vec::new());
                apply_session_transitions!(session.request_unlock(Instant::now()));
            }
        }

        // Reset lock-screen input the instant the lock engages, so a fresh lock
        // starts with an empty field and no stale error from a previous episode.
        if session.is_locked() && !prev_locked {
            // Locking itself does not make a physical release disappear: keep
            // Smithay's pressed state so a modifier held across this boundary
            // is still represented until its locked release arrives.  Only
            // revoke delivery to the now-hidden client. VT/device/keymap loss
            // separately emits `InputStateReset`, because those paths really
            // can lose an up edge.
            state.set_client_keyboard_focus(None);
            // A lock cuts off the client-facing input stream before its
            // physical release is guaranteed. Cancel both sides now: the DRM
            // backend queues `wl_touch.cancel` + frame, while the overlay
            // clears contacts without committing a terminal shell gesture.
            backend.cancel_touch();
            frame_builder.host_mut().cancel_direct_touch_gestures();
            lock.reset();
        }
        if session.is_locked() != prev_locked {
            retained_lock.clear();
            crate::portal::inhibit::publish_screensaver_state(session.is_locked());
            if session.is_locked() {
                let _ = state.release_all_personal_fabric_input();
                end_spatial_control_or_forget(peer_sync.as_deref(), &mut spatial_control);
            }
        }
        // Authority may disappear without another physical input event. A
        // replacement lease cannot inherit a running gesture or held inputs.
        if let Some(network) = peer_sync.as_ref()
            && spatial_control.as_ref().is_some_and(|control| {
                crate::shell::device_fabric_runtime::spatial_pointer_lease(network, &control.target)
                    .is_none_or(|lease| lease.nonce != control.lease_nonce)
                    || control.output.as_deref().is_some_and(|output| {
                        crate::shell::device_fabric_runtime::person_layout::current(network)
                            .is_none_or(|view| view.layout.find(control.target.as_str(), output)
                                .is_none_or(|display| !crate::shell::device_fabric_runtime::person_layout::reachable(network, display)))
                    })
            })
        {
            end_spatial_control(network, &mut spatial_control);
        }
        prev_locked = session.is_locked();
        // The dimming overlay is part of the scene, so switching it needs a frame.
        if screen_dimmer.overlay_alpha() != prev_dim_overlay {
            prev_dim_overlay = screen_dimmer.overlay_alpha();
            session_changed = true;
        }

        // Pump the D-Bus notification bridge into the toast stack and mirror lock
        // state into the chrome so it can suppress content while locked; cheap no-ops when unchanged.
        crate::shell::app_action_consent::set_session_attended(!session.is_locked());
        let device_sounds = frame_builder.host().drain_audio_device_sounds();
        frame_builder.with_chrome(|st| {
            for event in device_sounds {
                let _ = st.play_system_sound(event);
            }
            st.set_session_locked(session.is_locked());
            st.set_active_exposures(
                state
                    .share_registry
                    .disclosure()
                    .into_iter()
                    .map(|(peer, surface, can_control)| {
                        crate::shell::chrome_state::ActiveExposure {
                            peer,
                            surface,
                            can_control,
                        }
                    })
                    .collect(),
            );
            st.pump_dbus_notifications();
            for request in st.take_notification_launch_requests() {
                if let Err(error) = crate::shell::notification::launch_scheduled_notification(
                    request,
                    &st.tree_handle(),
                ) {
                    tracing::warn!(%error, "scheduled notification activation failed");
                }
            }
        });
        if let Some(network) = peer_sync.as_ref()
            && continuity_feed_polled
                .is_none_or(|polled| polled.elapsed() >= Duration::from_millis(500))
        {
            continuity_feed_polled = Some(Instant::now());
            let feed = network.continuity_feed();
            let shown = crate::shell::device_fabric_runtime::take_shown_tab();
            frame_builder.with_chrome(|st| {
                st.set_continuity_feed(feed);
                // Another of the person's devices moved a window here. A
                // tab whose workspace has not arrived yet is not shown.
                if let Some(tab) = shown
                    && !st.show_tab(tab)
                {
                    tracing::info!(tab, "a window moved here is not in this desktop");
                }
            });
        }

        // The window with the person's focus is no longer asking for it.
        frame_builder.with_chrome(|chrome| {
            crate::shell::attention::clear_focused(&chrome.tree());
        });
        // Client fullscreen is a per-output overlay drawn above everything,
        // laid out in the desktop beneath the shell only while the shell
        // takes the input over it (`FrameBuilder::set_client_fullscreen_group`).
        frame_builder.set_client_fullscreen_group(state.client_fullscreen_group());
        // A window the person opened or went to ends a full screen: the
        // full-screen window steps back into the strip, which scrolls to it.
        let user_fullscreen = frame_builder.chrome().user_fullscreen_group();
        if state.leave_fullscreen_when_focus_moved(user_fullscreen) {
            frame_builder.with_chrome(|chrome| chrome.set_user_fullscreen_group(None));
        }

        {
            // A close affordance mutates the rendered/persisted tree first, then
            // queues the protocol surface here. Remove it from the compositor's
            // authoritative foreign tree before reconciliation, otherwise the
            // still-live client can be adopted straight back while it handles
            // xdg_toplevel.close. Sending close asks the application to exit in
            // the normal desktop way; it is not a process kill.
            let mut close_requests = Vec::new();
            frame_builder.with_chrome(|chrome| {
                if let Some(apps) = chrome.hosted_inputs.in_process() {
                    apps.borrow_mut().pump_messaging_records();
                }
                for request in app_closes.drain() {
                    let tabs = crate::shell::app_close::tabs_of(&chrome.tree().desktop, &request);
                    eprintln!(
                        "app-close-requested: app={} room={} windows={}",
                        request.app_id,
                        request.room,
                        tabs.len()
                    );
                    for tab in tabs {
                        let _ = crate::shell::window_actions::close_tab(chrome, tab);
                    }
                }
                // Windows a remote viewer closed: through the same tab close.
                for surface in state.take_remote_closes() {
                    let tabs: Vec<_> = chrome
                        .tree()
                        .desktop
                        .workspaces
                        .iter()
                        .flat_map(|workspace| workspace.strips.iter())
                        .flat_map(|strip| strip.window_groups.iter())
                        .flat_map(|group| group.tabs.iter())
                        .filter(|tab| {
                            matches!(&tab.tile, crate::shell::state::TileContent::Foreign { surface_id, .. }
                                if *surface_id == surface)
                        })
                        .map(|tab| tab.id)
                        .collect();
                    for tab in tabs {
                        let _ = crate::shell::window_actions::close_tab(chrome, tab);
                    }
                }
                close_requests = chrome.take_surface_close_requests();
            });
            for surface_id in close_requests {
                state.close_surface_on_request(surface_id);
            }
            // Apps that stopped answering, and what the person chose on
            // their windows' cards.
            if state.advance_app_liveness() {
                frame_builder.with_chrome(super::super::shell::chrome_state::ChromeState::bump);
            }
            let mut fullscreen_toggle = false;
            frame_builder.with_chrome(|chrome| {
                fullscreen_toggle = chrome.take_fullscreen_toggle_request();
            });
            if fullscreen_toggle {
                toggle_focused_client_fullscreen(&mut state, &mut frame_builder);
            }
            let mut input_source = None;
            frame_builder.with_chrome(|chrome| {
                input_source = chrome.take_input_source_request();
            });
            if let Some(index) = input_source {
                state.reload_keyboard_settings();
                state.activate_keyboard_source(index);
                backend.set_keyboard_source(index);
                if let Some(xwayland) = xwayland.as_ref() {
                    xwayland.reload_keyboard_settings();
                }
                // The saved choice may be an input-method engine (or leave
                // one): hand typing to its owner now, not on the next
                // settings change. The choice was saved on this thread just
                // now, so read it here rather than wait for the refresh.
                input_methods.reload(&mut state, &live_settings.refresh_now());
            }

            // Protocol callbacks and the frame builder share one tree handle, so
            // adoption/destruction is immediately visible to chrome and persistence.
            // The input router's queues remain bookkeeping and are drained each
            // tick so they cannot grow across a long session.
            // Re-derived only after a tree write or a surface (un)registering,
            // which is also what every pending entry implies.
            let had_pending = state.client_input.clear_pending();
            let placements_changed = state.client_input.refresh_from_tree(&state.workspace_tree);
            if had_pending || placements_changed {
                frame_builder.with_chrome(super::super::shell::chrome_state::ChromeState::bump);
            }
        }
        // Reconcile the seat's keyboard focus with the focused tab every tick, so a
        // tab/group/workspace switch or overlay open moves the keyboard immediately, not on the next keystroke.
        if session.is_locked() {
            // `chrome_captures_input` deliberately does not know about the
            // lock scene (it is rendered outside chrome), so do not let its
            // ordinary active-tab reconciliation re-enter an app here.
            state.set_client_keyboard_focus(None);
        } else {
            client_input::sync_keyboard_focus(&mut state, &frame_builder);
        }
        // Focus is decided for this tick: a launch whose window did not get it yields its boost.
        ::launcher::priority::settle_windows();
        // A picked emoji goes to the field that just got the keyboard back,
        // through the seat's input method; what arrived feeds the recents.
        let _ = crate::compositor::text_commit::relay(
            &mut state,
            frame_builder.host_mut(),
            Instant::now(),
        );
        // A clipboard history choice for an app running inside the shell
        // takes that app's own paste: Ctrl+V into its focused widget, which
        // reads the clipboard the choice just set.
        for _ in 0..frame_builder.host_mut().take_in_process_pastes() {
            let ctrl = winit::event::Modifiers::from(winit::keyboard::ModifiersState::CONTROL);
            let v = winit::keyboard::Key::Character("v".into());
            let events = [
                winit::event::WindowEvent::ModifiersChanged(ctrl),
                ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
                    v.clone(),
                    None,
                    winit::event::ElementState::Pressed,
                )),
                ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
                    v,
                    None,
                    winit::event::ElementState::Released,
                )),
                winit::event::WindowEvent::ModifiersChanged(winit::event::Modifiers::default()),
            ];
            for event in &events {
                frame_builder.dispatch_event(
                    event,
                    &ctrl,
                    &mut ui::widget::Edits::lend(&mut test_control_ctx.doc),
                    &test_control_ctx.root_obj,
                    &mut test_control_ctx.focus,
                );
            }
            injected_redraw_pending = true;
        }
        // An app's idle inhibitor holds the screen on only while its window is visible.
        crate::compositor::idle_inhibit::sync_idle_inhibitors(
            &mut state,
            &frame_builder,
            &inhibit_registry,
        );
        let awake_holders =
            inhibit_registry.active_reasons_for(tilo_power_mgmt::inhibit::INHIBIT_IDLE);
        frame_builder.with_chrome(|chrome| chrome.set_awake_holders(awake_holders));
        // A chrome field with the focus, while no client holds the keyboard,
        // is served to the seat's input method like an application field.
        let deferred_field =
            frame_builder.focused_ime_requires_deferred_ack(test_control_ctx.focus);
        // A field that takes text only through deferred receipts (a browser
        // page field) is served to the input method only for dictation.
        let receipts_only =
            frame_builder.focused_ime_refuses_ordinary_commits(test_control_ctx.focus);
        let speech_field_needed = frame_builder
            .host_mut()
            .dictation_surface_mut()
            .needs_original_field();
        // Which field the seat's input method serves, and as what. While the
        // session is locked that is the lock screen's own password field and
        // nothing else — the one case a field must be served despite the
        // session being locked, because a password in a composed script cannot
        // be typed at all otherwise. `field_owner` is that decision, written
        // and tested on its own.
        let session_locked = session.is_locked();
        let owner = crate::compositor::shell_text_input::field_owner(
            session_locked,
            state.client_input.keyboard_focus().is_some(),
            session_locked.then(|| lock.ime_field()).flatten(),
            frame_builder.host().shell_prompt_up(),
            receipts_only,
            speech_field_needed,
            frame_builder.focused_ime_field(test_control_ctx.focus),
            test_control_ctx.focus.map(u64::from),
            frame_builder.host().palette_visible(),
        );
        let shell_field = owner.as_ref().map(|(field, _)| field.clone());
        let owner_id = owner.map(|(_, id)| id);
        if shell_field.is_some() && shell_text_input.is_none() {
            match crate::compositor::shell_text_input::ShellTextInput::connect(&mut state) {
                Ok(proxy) => shell_text_input = Some(proxy),
                Err(error) => {
                    tracing::warn!(target: "tilo::input_method", %error, "the shell's fields cannot reach the input method");
                }
            }
        }
        if let Some(proxy) = shell_text_input.as_mut() {
            let identity = owner_id.clone();
            let typed = proxy.sync_for_field(&mut state, shell_field, identity);
            let keys = state.take_shell_field_keys();
            // Text carries the keyboard's modifiers; a virtual keyboard's key
            // carries the ones it held for it, so its Ctrl+C is a chord.
            let delivered = typed
                .iter()
                .map(|event| (event, test_control_ctx.modifiers))
                .chain(keys.iter().map(|(event, modifiers)| (event, *modifiers)))
                .collect::<Vec<_>>();
            for (event, modifiers) in &delivered {
                if session_locked {
                    // While locked, the field that took the input method is the
                    // lock screen's, so that is where its text goes — through
                    // the same entry point every physical key uses.
                    let _ = lock.handle_event(event, modifiers);
                } else {
                    frame_builder.dispatch_event(
                        event,
                        modifiers,
                        &mut ui::widget::Edits::lend(&mut test_control_ctx.doc),
                        &test_control_ctx.root_obj,
                        &mut test_control_ctx.focus,
                    );
                }
            }
            if !delivered.is_empty() {
                injected_redraw_pending = true;
            }
            if state.has_dictation_target() && shell_field.is_some() {
                let stamp = frame_builder
                    .focused_ime_edit_stamp(test_control_ctx.focus, &test_control_ctx.doc);
                proxy.observe_edit_stamp(&mut state, stamp);
            }
        }
        frame_builder.set_osk_surface(
            state.osk_surface,
            state.client_input.keyboard_focus(),
            state.shell_field_active.then_some(shell_field).flatten(),
            state.text_input_active() && input_methods.osk_shown() && !receipts_only,
        );
        dictation_vocabulary.refresh(&mut state);
        state.set_native_dictation_deferred(deferred_field && state.shell_field_active);
        let dictation_blocked =
            session.is_locked() || !frame_builder.host().dictation_input_available();
        let original_start = {
            let surface = frame_builder.host_mut().dictation_surface_mut();
            surface
                .pending_consent()
                .then(|| surface.session())
                .flatten()
        };
        let field_start = (!dictation_blocked
            && original_start
                .is_some_and(|id| state.native_dictation_start_required(id, Instant::now())))
        .then(|| {
            if !deferred_field || !state.shell_field_active {
                return state.standalone_dictation_start();
            }
            frame_builder.start_deferred_text(
                test_control_ctx.focus,
                &mut test_control_ctx.doc,
                &test_control_ctx.root_obj,
            )
        })
        .flatten();
        let dictation_changed = {
            let surface = frame_builder.host_mut().dictation_surface_mut();
            crate::compositor::dictation::frontend::service(
                &mut state,
                surface,
                Instant::now(),
                dictation_blocked,
                field_start,
            );
            surface.take_changed()
        };
        state.private_text_input.service();
        if dictation_changed {
            frame_builder.with_chrome(crate::shell::chrome_state::ChromeState::bump);
        }
        if state.take_native_dictation_commit() {
            let events = display.flush_clients().ok().and_then(|()| {
                shell_text_input.as_mut().and_then(
                    crate::compositor::shell_text_input::ShellTextInput::drain_dictation_commit,
                )
            });
            if let Some(events) = events {
                let receipt = events.iter().find_map(|event| match event {
                    WindowEvent::Ime(winit::event::Ime::Commit(text)) => frame_builder
                        .commit_deferred_text(
                            test_control_ctx.focus,
                            text,
                            &mut test_control_ctx.doc,
                            &test_control_ctx.root_obj,
                        ),
                    _ => None,
                });
                let receipt = receipt.or_else(|| {
                    state.native_dictation_requires_ack().then(|| {
                        ui::webview::text_input::Receipt::failed(
                            ui::webview::text_input::Failure::Rejected,
                        )
                    })
                });
                if receipt.is_none() {
                    for event in &events {
                        frame_builder.dispatch_event(
                            event,
                            &test_control_ctx.modifiers,
                            &mut ui::widget::Edits::lend(&mut test_control_ctx.doc),
                            &test_control_ctx.root_obj,
                            &mut test_control_ctx.focus,
                        );
                    }
                }
                state.acknowledge_native_dictation(receipt);
                injected_redraw_pending = true;
            } else {
                state.acknowledge_native_dictation(Some(ui::webview::text_input::Receipt::failed(
                    ui::webview::text_input::Failure::Unknown,
                )));
                state.poll_native_dictation_delivery(Instant::now());
                // Disconnect the old bridge so an incomplete result cannot
                // arrive next turn after the target's selection has changed.
                shell_text_input = None;
                state.shell_field_active = false;
                frame_builder
                    .host_mut()
                    .dictation_surface_mut()
                    .failure(crate::compositor::dictation::DictationError::RecognizerUnavailable);
                frame_builder.with_chrome(crate::shell::chrome_state::ChromeState::bump);
            }
        }
        frame_builder.set_osk_commits(state.osk_commits);
        pump_checkpoint!("session-focus-and-surfaces");
        deliver_held_key_releases!();
        let floating_surfaces = state
            .floating_toplevels
            .iter()
            .filter(|surface| {
                !surface.suspended && surface.presented && surface.transfer_source.is_none()
            })
            .map(|surface| {
                (
                    surface.surface_id,
                    surface.app_id.clone(),
                    surface.rect,
                    surface.input_rect,
                )
            })
            .collect::<Vec<_>>();
        frame_builder.set_floating_surfaces_with_input(&floating_surfaces);
        // Live calls and navigation float where the person left them; desk
        // widgets share the edges with them and step past them.
        frame_builder.host_mut().set_system_activity_rects(
            floating_surfaces
                .iter()
                .map(|(_, _, rect, _)| *rect)
                .collect(),
        );
        state.sync_capture_protection();
        state.set_capture_placeholder_fill(
            crate::shell::capture_protection::placeholder_fill_rgba8(frame_builder.theme()),
        );
        let mut transient_surfaces = state.popup_surfaces();
        transient_surfaces.extend(state.input_method_popup_surfaces());
        transient_surfaces.extend(state.system_modal_surfaces());
        frame_builder.set_system_modal_scrim(state.has_system_modal());
        frame_builder.set_transient_surfaces(&transient_surfaces);
        frame_builder.set_floating_transfer_surfaces(
            &state.pending_floating_toplevels,
            &state.floating_toplevels,
        );
        let fullscreen_output = reconcile_fullscreen_overlay(
            &mut state,
            &mut frame_builder,
            &output_globals,
            backend.outputs(),
        );
        // Adaptive sync engages where a fullscreen client is shown; while it
        // is on, frames follow the content instead of the fixed refresh grid.
        backend.set_fullscreen_output(fullscreen_output);
        publish_output_colors(&state, backend.as_ref(), &mut published_output_colors);
        if frame_clock.set_adaptive(backend.adaptive_sync_active()) {
            eprintln!(
                "tilo compositor: pacing {}",
                if frame_clock.is_adaptive() {
                    "by content readiness (adaptive sync engaged)".to_owned()
                } else {
                    format!("to {:.3} Hz", 1.0 / frame_clock.interval().as_secs_f64())
                }
            );
        }
        // A file dragged out of the hosted file manager over a client's window becomes a real Wayland drag — see `pump_chrome_drag`.
        client_input::pump_chrome_drag(&mut state, &mut frame_builder);

        // Hand the remote-input firewall this frame's screen: foreign tile
        // positions, keyboard holder, chrome-drawn-over, locked — refreshed before remote events drain.
        client_input::publish_remote_input_view(&mut state, &frame_builder, session.is_locked());
        if spatial_pointer_parked && spatial_control.is_none() {
            // Restore before syncing any newly eligible client constraint.
            backend
                .set_pointer_constraint(crate::compositor::backend::PointerConstraintRegion::Free);
        }
        if let Some(image) = client_input::sync_pointer_image(
            &state,
            &mut frame_builder,
            session.is_locked(),
            spatial_control.is_some()
                || test_control_ctx.input.last_used() == Some(ui::InputModality::Touch),
        ) {
            backend.set_pointer_image(image);
            // A software cursor is drawn into the frame, so it needs one.
            injected_redraw_pending = true;
        }
        // A parked pointer belongs to the certified remote gesture. A
        // client cannot acquire its local lock until the pointer returns.
        if let Some(change) = client_input::sync_pointer_constraint(
            &mut state,
            &frame_builder,
            session.is_locked() || spatial_control.is_some(),
        ) {
            if change.engaged {
                frame_builder.with_chrome(|chrome| {
                    chrome.show_osd(crate::shell::osd::Osd::pointer_held(true));
                });
                injected_redraw_pending = true;
            }
            apply_pointer_constraint_change(backend.as_mut(), change);
        }
        spatial_pointer_parked = spatial_control.is_some();
        if spatial_pointer_parked {
            // A client release above must never unpark the remote gesture.
            backend.set_pointer_constraint(
                crate::compositor::backend::PointerConstraintRegion::Locked,
            );
        }
        let inhibit_allowed = !session.is_locked()
            && !crate::shell::input_policy::chrome_captures_input(&frame_builder);
        if state.sync_shortcut_inhibit(inhibit_allowed) == Some(true) {
            frame_builder.with_chrome(|chrome| {
                chrome.show_osd(crate::shell::osd::Osd::keys_captured(true));
            });
            injected_redraw_pending = true;
        }

        // Per-output HiDPI: tell each client the scale of its current display.
        // Every tick, since a window dragged to another monitor's density is otherwise unobserved.
        push_surface_scales(&mut state, &frame_builder, &transient_surfaces);
        // An output change has already installed the backend's new geometry,
        // but the frame builder still exposes the previous frame's tile rects
        // until `relayout` below. Diffing that mismatched pair would send a
        // false transition and then reverse it after layout. Keep the last
        // correct memberships until the new geometry is actually paintable.
        if surface_output_geometry_is_current(output_geometry_changed) {
            reconcile_surface_output_memberships(
                &mut surface_output_memberships,
                &state,
                &frame_builder,
                backend.outputs(),
                &output_globals,
            );
        }

        // Decide whether this frame needs repainting before the expensive layout
        // pass: `needs_animation_frame` walks the widget tree, and the chrome
        // revision is a counter bumped by state mutations, so skipping
        // `relayout`+`paint` on idle frames holds near-zero CPU at rest.
        // Pump the widget tree's per-frame state (sync + update) before
        // deciding whether to paint — drains a hosted terminal's PTY;
        // returns true when changed, itself a paint reason. Drain the
        // network actor into the replica: peer changes fold in before this frame's pump reads them, bumping the revision to repaint.
        // Artwork the chrome asked for arrived from the icon resolver's
        // thread: build the chrome again so tabs and rows show it.
        if crate::app_icons::take_arrived() {
            crate::shell::tab_binding::note_tab_artwork_changed(&mut tree_handle.write());
        }
        let pump_bookkeeping_done = Instant::now();
        let pump_network_started = pump_bookkeeping_done;
        // A tick only an animation's cadence woke, with no input since, has
        // nothing new from the network, the device fabric or peers' presence
        // to fold in: their sockets and timers wake the loop themselves. It
        // leaves them to the next tick anything else wakes, so an animation's
        // frames spend their time on the animation.
        let cadence_only_tick = woke_for_cadence_only
            && !had_backend_events
            && !force_first_paint
            && !session_changed
            && !session.is_locked();
        if let Some(net) = peer_sync.as_ref().filter(|_| !cadence_only_tick) {
            if loop_timing {
                let steps = pump_phase!("network-tick", net.timed_frame_tick());
                for (step, elapsed) in steps {
                    metrics::profile::record_phase(step, elapsed, None);
                    if loop_timing_env && elapsed >= Duration::from_millis(8) {
                        eprintln!("PUMP-PHASE name={step} elapsed_us={}", elapsed.as_micros());
                    }
                }
            } else {
                net.frame_tick();
            }
            deliver_held_key_releases!();
            if !std::mem::replace(&mut device_fabric_tried, true) {
                device_fabric_worker = start_device_fabric(net, &notif_bridge);
            }
            // The desktop a cross-device pointer crosses into: the logical
            // one it moves in.
            let (width, height) = logical_geometry(backend.outputs());
            let display =
                (width > 0 && height > 0).then_some(crate::shell::device_fabric::DisplayRect {
                    x: 0,
                    y: 0,
                    width,
                    height,
                });
            let app_focus = active_physical_gamepad_binding(&tree_handle.read());
            // The pump runs on its own thread; this hands it the turn's facts
            // and returns at once.
            if let Some(worker) = device_fabric_worker.as_ref() {
                pump_phase!(
                    "device-fabric-runtime",
                    worker.tick(crate::shell::device_fabric_runtime::FabricTurn {
                        selection: state.selection_store(),
                        local: crate::shell::device_fabric_runtime::LocalFabricCapabilities {
                            display,
                            pointer: true,
                            keyboard: true,
                            audio_output: system_audio,
                        },
                        app_focus: app_focus.clone(),
                        realtime_adapter: realtime_fabric_bridge.as_ref().and_then(
                            crate::shell::collab_broker_capnp::RealtimeFabricAppBridge::adapter,
                        ),
                        remote_capture: remote_capture_runtime.as_ref().map(std::sync::Arc::clone),
                    })
                );
            }
            deliver_held_key_releases!();
            pump_phase!(
                "physical-gamepads",
                crate::shell::physical_gamepad::pump_physical_gamepads(
                    net,
                    app_focus.as_ref(),
                    continuity_policy.current(),
                )
            );
            if let Some(bridge) = realtime_fabric_bridge.as_ref() {
                pump_phase!("realtime-app-events", bridge.pump_app_events());
            }
            for batch in crate::shell::device_fabric_runtime::take_spatial_input_batches() {
                use crate::shell::device_fabric_runtime::SpatialInputBatch;
                match batch {
                    SpatialInputBatch::Releases(controllers) => {
                        let count = controllers.len();
                        pump_phase!(
                            "spatial-input-releases",
                            count,
                            dispatch_spatial_input_batch(
                                &mut state,
                                SpatialInputBatch::Releases(controllers),
                            )
                        );
                    }
                    SpatialInputBatch::Inputs(events) => {
                        let count = events.len();
                        pump_phase!(
                            "spatial-input",
                            count,
                            dispatch_spatial_input_batch(
                                &mut state,
                                SpatialInputBatch::Inputs(events)
                            )
                        );
                    }
                }
            }
            queue_personal_fabric_input(&mut state, backend.as_mut(), &mut pending_events);
            let nearby_drop_events = crate::shell::device_fabric_runtime::take_nearby_drop_events();
            let nearby_drop_event_count = nearby_drop_events.len();
            pump_phase!(
                "nearby-drop-events",
                nearby_drop_event_count,
                for event in nearby_drop_events {
                    use crate::shell::device_fabric_runtime::NearbyDropEvent;
                    use crate::shell::i18n::{t, t_args};
                    let fabric = net.device_fabric();
                    let peer_label = |peer: &crate::shell::personal_continuity::DeviceId| {
                        fabric.snapshots().get(peer).map_or_else(
                            || t("nearby-drop-your-device"),
                            |snapshot| snapshot.presence.label.clone(),
                        )
                    };
                    let line = |key: &str, file: String, peer: String| {
                        t_args(key, &[("file", file.into()), ("device", peer.into())])
                    };
                    let (title, body) = match event {
                        NearbyDropEvent::Sending { file_name, peer } => (
                            t("nearby-drop-sending"),
                            line("nearby-drop-sending-body", file_name, peer_label(&peer)),
                        ),
                        NearbyDropEvent::Sent { file_name, peer } => (
                            t("nearby-drop-sent"),
                            line("nearby-drop-sent-body", file_name, peer_label(&peer)),
                        ),
                        NearbyDropEvent::Failed { file_name, peer } => (
                            t("nearby-drop-failed"),
                            line("nearby-drop-failed-body", file_name, peer_label(&peer)),
                        ),
                        NearbyDropEvent::Received { path, peer } => (
                            t("nearby-drop-received"),
                            line(
                                "nearby-drop-received-body",
                                path.file_name()
                                    .and_then(|name| name.to_str())
                                    .map_or_else(|| t("doc-open-unnamed-file"), str::to_string),
                                peer_label(&peer),
                            ),
                        ),
                    };
                    frame_builder.with_chrome(|chrome| {
                        chrome.push_notification(title, body);
                    });
                }
            );
            let personal_viewers =
                crate::shell::device_fabric_runtime::window_viewers(continuity_policy.current());
            let viewer_hosts: RoomHosts = if backend_kind == BackendKind::Headless {
                let rooms = {
                    let tree_handle = frame_builder.chrome_handle().borrow().tree_handle();
                    let tree = tree_handle.read();
                    tree.desktop
                        .workspaces
                        .iter()
                        .map(crate::shell::state::Workspace::room)
                        .collect::<Vec<_>>()
                };
                rooms
                    .into_iter()
                    .map(|room| (room, net.viewer_hosts(room)))
                    .filter(|(_, hosts)| !hosts.is_empty())
                    .collect()
            } else {
                RoomHosts::new()
            };
            let tree_handle = frame_builder.chrome_handle().borrow().tree_handle();
            let watched_before = backend.watched();
            // Members of each room viewing this device's windows there.
            let member_viewers = net.room_window_viewers();
            let replanned = legacy_shares.refresh(
                &tree_handle,
                (viewer_hosts, personal_viewers, member_viewers),
                watched_before,
                |for_host| {
                    if for_host != watched_for_host
                        && (for_host != backend.watched() || watched_for_host)
                    {
                        if backend.set_watched(for_host) {
                            injected_redraw_pending = true;
                        }
                        watched_for_host = for_host;
                    }
                    backend.watched()
                },
            );
            // What the shares feed is a function of them and their viewers,
            // so it is told only when they were derived again.
            if replanned
                && let Some((viewer_hosts, personal_viewers, member_viewers)) =
                    legacy_shares.viewers()
            {
                let desired = &legacy_shares.desired;
                let viewer_hosts = all_hosts(viewer_hosts);
                state.set_headless_host_viewers(
                    desired
                        .iter()
                        .flat_map(|(surface, (share, _))| {
                            share
                                .viewers
                                .iter()
                                .filter(|peer| viewer_hosts.contains(peer))
                                .map(|peer| (*peer, *surface))
                        })
                        .collect(),
                );
                // A window hidden here is sized for the devices showing it.
                crate::compositor::foreign::store().set_remote_extents(remote_extents(
                    desired,
                    personal_viewers,
                    member_viewers,
                ));
                state.set_member_stream_viewers(
                    desired
                        .iter()
                        .flat_map(|(surface, (share, _))| {
                            share.read_only.iter().map(|peer| (*peer, *surface))
                        })
                        .collect(),
                );
                state.set_personal_stream_viewers(
                    desired
                        .iter()
                        .flat_map(|(surface, (share, _))| {
                            personal_viewers
                                .get(&share.tab)
                                .into_iter()
                                .flatten()
                                .map(|viewer| (viewer.peer, *surface))
                        })
                        .collect(),
                );
                state.route_legacy_streams(desired.iter().map(|(surface, (_, room))| {
                    (*surface, crate::shell::network::room_topic_id(*room))
                }));
            }
            // This session's legacy windows, as its own tabs in the rooms
            // other devices govern, so their members have a tab to show.
            // Only those rooms' tabs are copied, and only their own legacy ones.
            if hosted_legacy_checked.elapsed() >= std::time::Duration::from_millis(500) {
                hosted_legacy_checked = Instant::now();
                let rooms = tree_handle
                    .read()
                    .desktop
                    .workspaces
                    .iter()
                    .map(crate::shell::state::Workspace::room)
                    .collect::<Vec<_>>();
                for room in rooms {
                    net.publish_hosted_legacy_windows(room, &tree_handle);
                    net.follow_room_app_versions(room);
                }
                let generation = tree_handle.generation();
                net.set_room_window_hosting(
                    &rooms_hosting_windows(&tree_handle.read()),
                    generation,
                );
            }
            pump_phase!(
                "legacy-share-reconcile",
                state.reconcile_legacy_share_sessions(
                    net.local_peer_id_hash(),
                    &legacy_shares.shares,
                    &legacy_share_config,
                )
            );
            // A window that changed and went still has its last frame sent
            // once the encode cap allows, not when it next changes.
            let _ = state.flush_share_sessions();
            let inbound_legacy = net.take_inbound_legacy();
            // A member that started showing a window through a room's relay
            // starts at a keyframe of it as it stands.
            for (peer, surface) in net.take_room_window_keyframes() {
                state.start_share_reader(peer, surface);
            }
            let inbound_legacy_count = inbound_legacy.len();
            pump_phase!(
                "legacy-inbound",
                inbound_legacy_count,
                for inbound in inbound_legacy {
                    match inbound.message {
                        crate::network::NeighbourMsg::SurfaceStreamStart {
                            surface,
                            source_peer,
                            app_id,
                            codec,
                            hdr_static_metadata,
                            width,
                            height,
                            tab,
                            ..
                        } => {
                            let reply_net = Arc::clone(net);
                            let local_peer = net.local_peer_id_hash();
                            let workspace_topic = inbound.workspace_topic;
                            let keyframe_net = Arc::clone(net);
                            let input_sink = Arc::new(move |input| {
                                if let Ok(event) = postcard::to_allocvec(&input) {
                                    let _ = reply_net.publish_legacy_message(
                                        workspace_topic,
                                        crate::network::NeighbourMsg::RemoteInput {
                                            peer: local_peer,
                                            surface,
                                            event,
                                        },
                                    );
                                }
                            });
                            let keyframe_sink = Arc::new(move |seq| {
                                let _ = keyframe_net.publish_legacy_message(
                                    workspace_topic,
                                    crate::network::NeighbourMsg::SurfaceKeyframeRequest {
                                        peer: local_peer,
                                        surface,
                                        seq,
                                    },
                                );
                            });
                            let stream = crate::shell::peer_view::PeerStreamStart {
                                source_peer,
                                surface,
                                local_peer,
                                app_id,
                                codec,
                                display_size: Some((width, height)),
                                hdr_static_metadata,
                                input_sink,
                                keyframe_sink,
                                tab,
                            };
                            let mounted = !stream.app_id.is_empty()
                                && frame_builder.host_mut().mount_peer_stream(&stream);
                            if mounted {
                                for (seq, frame) in early_legacy_frames.take((source_peer, surface))
                                {
                                    let _ = frame_builder.host_mut().push_peer_stream_frame(
                                        source_peer,
                                        surface,
                                        seq,
                                        &frame,
                                        None,
                                    );
                                }
                            } else {
                                tracing::warn!(
                                    source_peer,
                                    surface,
                                    app_id = stream.app_id,
                                    "legacy stream is waiting for its peer-window tab"
                                );
                            }
                            pending_legacy_streams.insert((source_peer, surface), stream);
                        }
                        crate::network::NeighbourMsg::SurfaceFrame {
                            surface,
                            source_peer,
                            encoded_h264,
                            seq,
                            ..
                        } => {
                            if frame_builder.host().has_peer_stream(source_peer, surface) {
                                let _ = frame_builder.host_mut().push_peer_stream_frame(
                                    source_peer,
                                    surface,
                                    seq,
                                    &encoded_h264,
                                    None,
                                );
                            } else {
                                early_legacy_frames.hold((source_peer, surface), seq, encoded_h264);
                            }
                        }
                        crate::network::NeighbourMsg::SurfaceStreamEnd {
                            surface,
                            source_peer,
                            ..
                        } => {
                            pending_legacy_streams.remove(&(source_peer, surface));
                            early_legacy_frames.forget((source_peer, surface));
                            let _ = frame_builder
                                .host_mut()
                                .unmount_peer_stream(source_peer, surface);
                        }
                        crate::network::NeighbourMsg::RemoteInput {
                            peer,
                            surface,
                            event,
                        } => {
                            match postcard::from_bytes::<crate::network::streaming::InputEvent>(
                                &event,
                            ) {
                                Ok(event) => {
                                    let _ = state.apply_network_remote_input(peer, surface, event);
                                }
                                Err(error) => tracing::warn!(
                                    peer,
                                    surface,
                                    %error,
                                    "dropping malformed remote input"
                                ),
                            }
                        }
                        crate::network::NeighbourMsg::SurfaceKeyframeRequest {
                            peer,
                            surface,
                            seq,
                        } => {
                            state.request_share_keyframe(peer, surface, seq);
                        }
                        _ => {
                            tracing::debug!("ignoring non-stream message in the legacy inbox");
                        }
                    }
                }
            );
            let pending_legacy_count = pending_legacy_streams.len();
            pump_phase!(
                "legacy-remount",
                pending_legacy_count,
                for (&(source_peer, surface), stream) in &pending_legacy_streams {
                    if !stream.app_id.is_empty()
                        && frame_builder.host_mut().mount_peer_stream(stream)
                    {
                        for (seq, frame) in early_legacy_frames.take((source_peer, surface)) {
                            let _ = frame_builder.host_mut().push_peer_stream_frame(
                                source_peer,
                                surface,
                                seq,
                                &frame,
                                None,
                            );
                        }
                    }
                }
            );
            // A picture that outlasted its frame's push is shown once it
            // finishes, even when no later frame arrives to carry it out; the
            // view's wake deadline brings the loop back for it.
            let _ = frame_builder.host_mut().collect_late_peer_pictures();
            // Which legacy windows running on the person's other devices this
            // one shows: each host streams them here only while it does.
            // Nothing shows under the lock.
            let window_views = if session.is_locked() {
                Vec::new()
            } else {
                let tiles = frame_builder.host().tab_tiles();
                let tree_handle = frame_builder.chrome_handle().borrow().tree_handle();
                let tree = tree_handle.read();
                shown_hosted_windows(&tree, &tiles)
            };
            // The windows other members of a room host that this device shows:
            // each host streams them here, read only, while it does. The
            // person's own devices were told above.
            let room_views = if session.is_locked() {
                Vec::new()
            } else {
                let tiles = frame_builder.host().tab_tiles();
                let tree_handle = frame_builder.chrome_handle().borrow().tree_handle();
                let tree = tree_handle.read();
                shown_room_hosted_windows(&tree, &tiles)
            };
            net.publish_room_window_views(room_views);
            crate::shell::device_fabric_runtime::publish_window_views(
                net,
                window_views,
                continuity_policy.current(),
            );
        }
        pump_checkpoint!("device-fabric-and-network");
        deliver_held_key_releases!();
        // Keep the presence relay current, then fold in what peers sent. Pointer
        // is read level-triggered (covers every source: seat, remote peer, test
        // socket); the room set is re-read after every tree write, since a
        // workspace can join a room mid-session and the subscription must follow.
        let pump_network_done = Instant::now();
        let pump_presence_started = pump_network_done;
        // The pointer as the input state knows it — "nowhere", not the origin, for
        // a session that never moved one; being level-triggered, `cursor()` would paint a cursor for nobody.
        frame_builder.note_local_pointer(state.client_input.pointer_position());
        // Idle after five quiet minutes, away while locked: what the people
        // in the workspace see on this person's face.
        frame_builder
            .host_mut()
            .note_local_activity(session.is_locked(), session.idle_for(Instant::now()));
        let peers_moved =
            if let Some(relay) = frame_builder.peer_presence().filter(|_| !cadence_only_tick) {
                // Clone the tree handle and drop the `ChromeState` borrow before
                // the relay locks the tree — nothing here needs the RefCell guard.
                let tree_handle = frame_builder.chrome_handle().borrow().tree_handle();
                relay.follow_tree(&tree_handle);
                frame_builder.pump_peer_presence()
            } else {
                false
            };
        pump_checkpoint!("peer-presence");
        deliver_held_key_releases!();
        let pump_presence_done = Instant::now();
        let pump_apps_started = pump_presence_done;
        for osd in transport_osds.drain() {
            frame_builder.with_chrome(|st| publish_media_osd(st, osd));
            session_changed = true;
        }
        if software_dim.sync(backend.as_mut(), &session_wake) {
            // The sliders follow every level the service announces: keys,
            // Quick Settings, Settings, a firmware hotkey, another program.
            let levels = os_actions::BrightnessService::global().levels();
            frame_builder.with_chrome(|st| {
                if crate::shell::quick_settings::follow_brightness_levels(
                    &mut st.quick_settings,
                    &levels,
                ) {
                    st.bump();
                }
            });
        }
        let markers_suppressed = session.is_locked() || frame_builder.host().onboarding_active();
        let markers_retired =
            revalidate_native_agent_markers(&mut state, &mut frame_builder, markers_suppressed);
        // On a tick only the cadence woke, an animation that layout places
        // (a strip's pan) moves in `relayout`; the widgets' `update` has
        // nothing new to read, unless it stepped an animation of its own last
        // time, which then keeps it running every tick.
        if !cadence_only_tick || widget_pump_changed {
            widget_pump_changed =
                frame_builder.pump_apps(&mut test_control_ctx.doc, &test_control_ctx.root_obj);
        }
        let mut apps_changed = foreign_capture_changed | markers_retired | widget_pump_changed;
        crate::shell::launcher::record_presented_launches(|surface| {
            crate::compositor::foreign::store().frame_seq(surface)
        });
        let onboarding_active = frame_builder.host().onboarding_active();
        let identity_setup_active = frame_builder.host().identity_setup_active();
        if onboarding_active != previous_onboarding_active {
            if let Some(local_session) = local_session_lifecycle.as_ref() {
                local_session.set_onboarding_active(onboarding_active);
            }
            previous_onboarding_active = onboarding_active;
        }
        if desktop_continuation
            .as_mut()
            .is_some_and(|continuation| continuation.pump(onboarding_active))
            && let Err(error) = crate::shell::desktop_continuation::launch_request()
                .and_then(crate::shell::host_apps::queue_launch)
        {
            tracing::warn!(%error, "Could not queue Desktop folder continuation; reopen Add folders to Tilo");
        }
        let pump_apps_done = Instant::now();
        pump_checkpoint!("widget-pump");
        deliver_held_key_releases!();
        if let Some(wait) = identity_clock_wait.as_mut() {
            match wait.pump() {
                crate::identity_clock_wait::ClockWaitStep::Waiting => {}
                crate::identity_clock_wait::ClockWaitStep::Verified => {
                    identity_clock_wait = None;
                    tracing::info!(
                        "the clock admits this device's certificate; starting the session's services"
                    );
                    frame_builder.with_chrome(|chrome| chrome.set_identity_waits_for_clock(false));
                    if let Some(identity_dir) = identity_store.path().parent() {
                        crate::firstboot::keep_device_certificates_renewed(identity_dir);
                    }
                }
                crate::identity_clock_wait::ClockWaitStep::NeedsRecovery => {
                    identity_clock_wait = None;
                    frame_builder.with_chrome(|chrome| chrome.set_identity_waits_for_clock(false));
                    if let Some(identity_dir) = identity_store.path().parent() {
                        frame_builder.enable_recovery_onboarding(identity_dir.to_path_buf());
                    }
                }
            }
        }
        let identity_setup_active = identity_setup_active || identity_clock_wait.is_some();
        if take_deferred_peer_sync_activation(&mut deferred_peer_sync, identity_setup_active) {
            // `pump_apps` just observed the trusted onboarding completion.
            // Apps were withheld until this edge. The network starts off the
            // loop; launches wait until it and the broker over it exist, so
            // an app's first service descriptor sees the live broker.
            frame_builder.host_mut().set_peer_sync_enabled(true);
            crate::shell::launcher::hold_launches();
            network_held_launches = true;
            network_start = Some(start_session_peer_sync(
                tree_handle.clone(),
                Arc::clone(&session_wake),
            ));
        }
        if take_deferred_peer_sync_activation(&mut deferred_local_broker, identity_setup_active) {
            local_broker_due = true;
        }
        if let Some(files) = retained_files.as_mut() {
            let chrome = frame_builder.chrome_handle();
            if let Some(operation) = frame_builder.host_mut().take_existing_files_release() {
                files.release(&operation, &chrome);
            }
            if let Some(request) = frame_builder.host_mut().take_existing_files_request() {
                let operation = request.operation.clone();
                match files.start(request) {
                    Ok(true) => frame_builder
                        .host_mut()
                        .existing_files_outcome(&operation, true),
                    Ok(false) => (),
                    Err(_) => frame_builder
                        .host_mut()
                        .existing_files_outcome(&operation, false),
                }
            }
            if let Some((operation, available)) = files.pump(&chrome) {
                frame_builder
                    .host_mut()
                    .existing_files_outcome(&operation, available);
                injected_redraw_pending = true;
            }
        }
        // Admission runs off-loop; the visible migration overlay and its
        // semantic actions continue to own progress/cancel/retry.
        let copy_opening = frame_builder
            .chrome_handle()
            .borrow()
            .native_app_opening
            .clone();
        if let Some(opening) = copy_opening
            && !identity_setup_active
            && let Some(request) = frame_builder.host_mut().take_original_copy_request()
            && let Err(error) = crate::shell::launcher::original_copy::start(
                request,
                tree_handle.clone(),
                opening,
                Arc::clone(&session_wake),
            )
        {
            tracing::warn!(%error, "original Copy admission worker unavailable");
        }
        // First use uses the same semantic socket, restricted to its trusted
        // providers and admitted only by an actual root-delivered setup or owner.
        if let Some(identity_dir) = admin_control_due.take() {
            let bound = if onboarding_active {
                crate::compositor::admin_control::bind_first_use(
                    &identity_dir,
                    headless_owner_control,
                )
            } else if headless_owner_control {
                crate::compositor::admin_control::bind_headless_owner(&identity_dir)
            } else {
                crate::compositor::admin_control::bind(&identity_dir)
            };
            match bound {
                Ok((listener, path)) => {
                    tracing::info!(path = %path.display(), "admin control: listening");
                    let wake = control_listener_wake(&listener, &event_loop.handle());
                    admin_control = Some((listener, UnlinkOnDrop(path), wake));
                }
                Err(reason) => {
                    tracing::info!(%reason, "admin control: not served in this session");
                    // A normal first-use ceremony can establish the certified
                    // owner later. Preserve the opportunity to bind then.
                    if onboarding_active {
                        admin_control_due = Some(identity_dir);
                    }
                }
            }
        }
        let started = if std::mem::take(&mut local_broker_due) {
            Some(None)
        } else {
            network_start
                .as_ref()
                .and_then(crate::session_runtime::PendingStart::poll)
                .map(Some)
        };
        if let Some(outcome) = started {
            network_start = None;
            let start_failure = match outcome {
                None => None,
                Some(Ok(network)) => {
                    peer_sync = Some(install_session_peer_sync(
                        &mut frame_builder,
                        &mut state,
                        &tree_handle,
                        network,
                    ));
                    if let Some(net) = peer_sync.as_ref() {
                        net.set_wake(Arc::clone(&session_wake));
                        frame_builder.with_chrome(|chrome| {
                            if let Some(apps) = chrome.hosted_inputs.in_process() {
                                let mut apps = apps.borrow_mut();
                                apps.bind_private_messages_network(net);
                                apps.bind_feedback_network(Arc::clone(net));
                            }
                        });
                    }
                    None
                }
                Some(Err(error)) => {
                    tracing::warn!(%error, "peer-sync: spawn failed; continuing without sync");
                    // Invites and joins need it: their sheets say so and
                    // what the person can do, instead of waiting for it.
                    frame_builder.host_mut().set_peer_sync_failed();
                    // Launches held for the network first login asked for
                    // are reported; a boot carries on local-only.
                    network_held_launches.then_some(error)
                }
            };
            // Binding the broker opens its journal, vault and channel state:
            // on the session-start worker, installed below when it is up.
            // Launches stay held until then.
            crate::shell::launcher::hold_launches();
            broker_start = Some((
                start_session_collab_broker(
                    peer_sync.as_ref(),
                    tree_handle.clone(),
                    Arc::clone(&session_wake),
                ),
                start_failure,
            ));
        }
        if let Some(answer) = recording_audio_probe
            .as_ref()
            .and_then(crate::session_runtime::PendingStart::poll)
        {
            recording_audio_probe = None;
            match answer {
                Ok((desktop_sound, microphone)) => {
                    system_audio = desktop_sound;
                    frame_builder.set_capture_audio_available(desktop_sound, microphone);
                }
                Err(error) => {
                    tracing::warn!(%error, "recording audio probe ended without an answer; capture offers no audio");
                }
            }
        }
        if let Some(outcome) = broker_start.as_ref().and_then(|(start, _)| start.poll()) {
            let start_failure = broker_start.take().and_then(|(_, failure)| failure);
            collab_broker = finish_session_collab_broker(&frame_builder.chrome_handle(), outcome);
            crate::shell::in_process::set_session_services(
                crate::shell::in_process::SessionServices::Listening,
            );
            if let Some(broker) = collab_broker.as_ref() {
                local_session_lifecycle = Some(install_native_session_lifecycle(
                    broker,
                    &session,
                    onboarding_active,
                ));
                attach_collab_broker_bridges(&mut frame_builder, broker);
                #[cfg(feature = "agent-control")]
                spawn_agent_mcp(broker, &tree_handle, backend_kind == BackendKind::Headless);
                if let (Some(portal), Some(service)) =
                    (portal_backend.as_ref(), broker.native_media_service())
                {
                    service
                        .install_consent(portal.media_grants.clone(), portal.media_prompts.clone());
                }
                realtime_fabric_bridge = broker.realtime_fabric_bridge();
                remote_capture_runtime = realtime_fabric_bridge
                    .as_ref()
                    .and_then(crate::shell::collab_broker_capnp::RealtimeFabricAppBridge::realtime_capture_broker)
                    .map(|capture| {
                        std::sync::Arc::new(std::sync::Mutex::new(
                            crate::shell::remote_capture::runtime::RemoteCaptureFabricRuntime::production(
                                capture,
                            ),
                        ))
                    });
                if let Some(remote_capture) = remote_capture_runtime.as_ref() {
                    crate::shell::remote_capture::runtime::install_active_runtime(remote_capture);
                }
                if let Some(net) = peer_sync.as_ref() {
                    attach_peer_presence(&mut frame_builder, broker, net);
                }
            }
            // The broker now exists (local-only if the network failed).
            // Launches that waited for a network that never came are
            // reported, not silently started without it; later launches
            // run normally.
            match start_failure {
                None => crate::shell::launcher::release_held_launches(),
                Some(error) => {
                    crate::shell::launcher::fail_held_launches(&crate::shell::i18n::t_args(
                        "launch-held-network-failed",
                        &[("error", error.into())],
                    ));
                }
            }
            apps_changed = true;
        }
        // The agent door serves agents through the broker, so it opens once
        // the broker's start has ended: an agent that connected earlier
        // would be refused as unavailable while the broker was binding.
        let broker_starting = network_start.is_some() || broker_start.is_some() || local_broker_due;
        if !onboarding_active
            && !broker_starting
            && let Some(path) = agent_door_due.take()
        {
            match crate::compositor::agent_door::bind(&path) {
                Ok(listener) => {
                    tracing::info!(path = %path.display(), "agent door: listening");
                    let wake = control_listener_wake(&listener, &event_loop.handle());
                    let (requests, consent) = crate::shell::agent_admission::channel(
                        crate::shell::agent_admission::default_always_path(),
                        Some(Arc::clone(&session_wake)),
                    );
                    let (controls, handed_over) = std::sync::mpsc::channel();
                    agent_door_controls = Some(handed_over);
                    agent_door = Some((
                        crate::compositor::agent_door::Door::new(
                            listener,
                            requests,
                            controls,
                            Arc::clone(&session_wake),
                        ),
                        UnlinkOnDrop(path),
                        wake,
                    ));
                    agent_door_consent = Some(consent);
                }
                Err(reason) => {
                    tracing::info!(%reason, "agent door: not served in this session");
                }
            }
        }
        if desktop_continuation
            .as_mut()
            .is_some_and(|continuation| continuation.pump(onboarding_active))
            && let Err(error) = crate::shell::desktop_continuation::launch_request()
                .and_then(crate::shell::host_apps::queue_launch)
        {
            tracing::warn!(%error, "Could not queue Desktop folder continuation; reopen Add folders to Tilo");
        }
        let pump_finalize_started = Instant::now();
        let revision = frame_builder.chrome_revision();
        let revision_changed = revision != last_revision;
        let animation_demand = frame_builder.animation_demand();
        // Host demand clears when the crossfade settles (e2e). Keep
        // painting until this many compositor frames have actually run (W16).
        let transition_run = frame_builder.chrome().transition_paints_remaining() > 0;
        let still_moving = animation_demand.any() || transition_run;
        let animating = animation_settle.due(still_moving);
        let app_tick = NativeAppTick::after_pump(&frame_builder, apps_changed, iter_t0);
        let output_presentation_count = state.output_presentation_surfaces.len();
        let presentation_frame_changed = update_output_presentation_repaint_pending(
            &mut output_presentation_repaint_pending,
            &mut output_presentation_surface_count,
            output_presentation_count,
            state.output_presentation_frame_changed(),
        );
        let keepalive_due = false;
        let lock_clock_due =
            lock_clock_refresh_due(session.is_locked(), iter_t0, &mut last_lock_clock_refresh)
                || (session.is_locked() && retained_lock.feedback_frame_due());
        if session.is_locked() {
            if iter_t0.saturating_duration_since(last_lock_media_poll) >= LOCK_MEDIA_REFRESH {
                last_lock_media_poll = iter_t0;
                let player = lock_now_playing();
                if lock_media_changed(lock_player.as_ref(), player.as_ref()) {
                    session_changed = true;
                }
                lock_player = player;
            }
        } else {
            lock_player = None;
            lock_pointer = None;
        }
        deliver_held_key_releases!();
        let pump_done = Instant::now();
        let pump_total = pump_done
            .duration_since(iter_t0)
            .checked_sub(t_input_done)
            .unwrap_or_default();
        metrics.mark_pump(pump_total);
        if loop_timing_env && pump_slowest.1 > Duration::from_millis(100) {
            eprintln!(
                "SLOW-PUMP slowest={} elapsed={:?}",
                pump_slowest.0, pump_slowest.1
            );
        }
        if slow_prepaint_phase(pump_total) {
            tracing::warn!(
                target: "tilo::latency",
                elapsed_ms = pump_total.as_secs_f64() * 1_000.0,
                bookkeeping_ms = pump_bookkeeping_done
                    .duration_since(pump_bookkeeping_started)
                    .as_secs_f64()
                    * 1_000.0,
                network_ms = pump_network_done
                    .duration_since(pump_network_started)
                    .as_secs_f64()
                    * 1_000.0,
                presence_ms = pump_presence_done
                    .duration_since(pump_presence_started)
                    .as_secs_f64()
                    * 1_000.0,
                apps_ms = pump_apps_done
                    .duration_since(pump_apps_started)
                    .as_secs_f64()
                    * 1_000.0,
                finalize_ms = pump_done
                    .duration_since(pump_finalize_started)
                    .as_secs_f64()
                    * 1_000.0,
                "pre-paint pump stalled compositor input"
            );
        }
        // Is there anything new to show? This redraw gating lets a static desktop
        // idle at near-zero CPU, orthogonal to *when* a frame may show (the clock's job below).
        let redraw_reasons = NativeRedrawReasons {
            force: force_first_paint,
            backend_event: dispatched_input
                .asks_for_frame(state.client_keys_delivered.wrapping_sub(client_keys_before)),
            visible_input: visible_input_changed,
            session: session_changed || injected_redraw,
            revision: revision_changed,
            hidden_desktop_animation: animating,
            // A client frame callback is not a present. A configure ack or a
            // title carries one and changes no pixels; painting to release
            // it kept the shell busy while apps sat still. The callback is
            // fired after dispatch when this tick will not build a frame,
            // and that answer does not schedule another pump.
            apps: app_tick.redraw,
            peers: peers_moved,
            presentation: presentation_frame_changed,
            capture: capture_executor.has_live_mirrors()
                || capture_executor.is_recording()
                || capture_executor.has_finished_io()
                || portal_backend
                    .as_ref()
                    .is_some_and(PortalBackend::capture_pending)
                || portal_backend
                    .as_ref()
                    .is_some_and(|portal| portal.screencast.has_streams()),
            keepalive: keepalive_due,
            lock_clock: lock_clock_due,
            lock_osd: lock_screen::lock_osd_frame_due(frame_builder.chrome().osd, iter_t0),
            owed: content_owed,
        };
        let display_dark = session.is_blanked();
        // May a frame be shown yet? Off-cadence wakeups (protocol traffic,
        // test-control, input) would submit inside a discarded interval; first frame is exempt.
        // A window whose first frame may still land inside its frame budget
        // holds the frame back until then, so a window that answers in time
        // opens with no placeholder ([`ui::arriving`]); the hold's end is a
        // wake deadline, and what changed meanwhile is owed to that frame.
        ui::arriving::set_frame_budget(frame_clock.interval());
        let held_until = (!force_first_paint)
            .then(|| frame_builder.holds_frame_until(iter_t0))
            .flatten();
        let cadence_due = force_first_paint || (frame_clock.due(iter_t0) && held_until.is_none());
        // Is there room in the pipe? Building another scene while the render
        // thread is behind would only grow the queue (and input-to-photon latency); capped at the presenter's queue depth plus the frame presenting, the tick still services input/clients without rendering.
        // A frame still waiting for the render thread is replaced by the one built now (latest
        // wins), so a full pipe whose newest frame has not started takes it too.
        let pipe_ready = present_pipe
            .as_ref()
            .is_none_or(|pipe| pipe.has_capacity() || pipe.can_replace_queued());
        credit_transition_hold = present_hold_credit(
            !pipe_ready,
            cadence_due,
            frame_builder.host().transition_present_pending(),
        );
        // A change this tick saw but could not paint is still to be shown: the
        // next tick that may paint does, even if nothing new happens by then.
        let TickPaint {
            content_changed,
            should_paint,
            owed,
        } = tick_paint(redraw_reasons, &session, cadence_due, pipe_ready);
        content_owed = owed;
        demand_onset.note_tick(content_changed, iter_t0);
        if content_changed
            && cadence_due
            && !pipe_ready
            && let Some(p) = present_pipe.as_mut()
        {
            p.note_backpressure();
            metrics.mark_backpressured();
        }
        // If content became dirty off-cadence, keep cadence in the next park;
        // otherwise the event edge has already been consumed and a long idle
        // deadline could strand that frame. A full presenter is itself the
        // gate: its retirement ping wakes the loop when capacity returns.
        let cadence_needed = held_until.is_none()
            && cadence_wait_needed(
                pipe_ready,
                (content_changed && !cadence_due)
                    || (!session.is_locked() && !display_dark && animating)
                    || (session.is_locked() && !display_dark && retained_lock.feedback_frame_due()),
            );

        // Diagnostic: `TILO_PAINT_STATS=1` logs every ~2s how many frames painted
        // vs idled and why, confirming the loop actually idles without guessing.
        if paint_stats {
            frames_total = frames_total.saturating_add(1);
            if paint_ticks {
                crate::stderr_guard::write_line(format_args!(
                    "paint-tick: painted={should_paint} woke={last_wake}"
                ));
            }
            if should_paint {
                frames_painted = frames_painted.saturating_add(1);
                if redraw_reasons.backend_event {
                    cause_events = cause_events.saturating_add(1);
                }
                if session_changed {
                    cause_session = cause_session.saturating_add(1);
                }
                if revision_changed {
                    cause_revision = cause_revision.saturating_add(1);
                }
                if animating {
                    cause_anim = cause_anim.saturating_add(1);
                    animation_sources_seen.host |= animation_demand.host;
                    animation_sources_seen.floating |= animation_demand.floating;
                    animation_sources_seen.transient |= animation_demand.transient;
                    animation_sources_seen.osk |= animation_demand.osk;
                    if animation_demand.host & crate::shell::animation_diagnostics::CONTENT_BIT != 0
                        && animating_content_leaves.is_empty()
                    {
                        animating_content_leaves = frame_builder.content_animating_leaves();
                    }
                }
                if keepalive_due
                    && !(redraw_reasons.backend_event
                        || session_changed
                        || revision_changed
                        || animating)
                {
                    cause_keepalive = cause_keepalive.saturating_add(1);
                }
            }
            if stats_since.elapsed() >= Duration::from_secs(2) && frames_painted == 0 {
                // Idle: keep tallying wakes, report once painting resumes.
                paint_stats_idle = paint_stats_idle.saturating_add(stats_since.elapsed());
                frames_total = 0;
                stats_since = Instant::now();
            } else if stats_since.elapsed() >= Duration::from_secs(2) {
                if !paint_stats_idle.is_zero() {
                    crate::stderr_guard::write_line(format_args!(
                        "paint-stats: idle {:.0}s before this, nothing painted",
                        paint_stats_idle.as_secs_f64()
                    ));
                    paint_stats_idle = Duration::ZERO;
                }
                let animation_host_labels =
                    crate::shell::animation_diagnostics::host_labels(animation_sources_seen.host)
                        .collect::<Vec<_>>()
                        .join(",");
                crate::stderr_guard::write_line(format_args!(
                    "paint-stats: {frames_painted}/{frames_total} painted | \
                     events={cause_events} revision={cause_revision} anim={cause_anim} \
                     session={cause_session} keepalive={cause_keepalive} | \
                     locked={} blanked={} | \
                     anim_sources host=0x{:04x} host_labels={:?} content={:?} floating={} transient={} osk={} | \
                     {:.3} Hz target | present={} backpressure={}",
                    session.is_locked(),
                    session.is_blanked(),
                    animation_sources_seen.host,
                    animation_host_labels,
                    animating_content_leaves.join(","),
                    animation_sources_seen.floating,
                    animation_sources_seen.transient,
                    animation_sources_seen.osk,
                    1.0 / frame_clock.interval().as_secs_f64(),
                    present_pipe
                        .as_ref()
                        .map_or_else(|| "inline".to_string(), |p| p.presented().to_string()),
                    present_pipe
                        .as_ref()
                        .map_or(0, super::present::PresentPipe::backpressured),
                ));
                frames_total = 0;
                frames_painted = 0;
                cause_events = 0;
                cause_revision = 0;
                cause_anim = 0;
                cause_session = 0;
                cause_keepalive = 0;
                crate::stderr_guard::write_line(format_args!(
                    "paint-stats: wakes {} | widget-deadline owner {:?}",
                    wakes_seen.take_summary(),
                    widget_wake_owner.take().unwrap_or_default(),
                ));
                animation_sources_seen = crate::shell::frame::AnimationDemand::default();
                animating_content_leaves.clear();
                stats_since = Instant::now();
            }
        }

        if should_paint && metrics.is_enabled() {
            let advanced =
                crate::shell::foreign::store().surfaces_advanced(&mut trace_surface_frames);
            let clients = if advanced.is_empty() {
                ""
            } else {
                let chrome = frame_builder.chrome();
                let tree = chrome.tree();
                paint_client_label(&tree, &advanced)
            };
            metrics.mark_paint_reasons(
                redraw_reasons.paint_reason_bits(session.is_locked(), transition_run),
                clients,
            );
            if animating && !session.is_locked() {
                let owners = animation_trace.label(
                    (animation_demand, transition_run, revision),
                    Instant::now(),
                    || frame_builder.animation_owners(),
                );
                metrics.mark_paint_animations(owners);
            }
        }
        if should_paint {
            frame_builder.with_chrome(
                super::super::shell::chrome_state::ChromeState::consume_transition_paint,
            );
            // Lay out only when about to paint — any structural change (spliced group,
            // tab switch, resize) is applied here, right before the scene is built.
            let t_layout_start = Instant::now();
            let paint_cpu = metrics.cpu_snapshot();
            frame_builder.relayout();
            frame_builder.reconcile_native_focus(&mut test_control_ctx.focus);
            let relaid_out = Instant::now();
            metrics::profile::record_phase(
                "layout/relayout",
                relaid_out.duration_since(t_layout_start),
                None,
            );
            // `relayout` publishes a new tile rect (for a tab switch, move,
            // fullscreen or hotplug) immediately before painting it. Publish it
            // here as well as above so the client gets the output transition and
            // its new scale in the same frame, ahead of the configure below.
            publish_layout_to_clients(
                &mut surface_output_memberships,
                &mut state,
                &frame_builder,
                &transient_surfaces,
                backend.outputs(),
                &output_globals,
            );
            let published = Instant::now();
            metrics::profile::record_phase(
                "layout/clients",
                published.duration_since(relaid_out),
                None,
            );
            if session.is_locked() {
                let clock = LockClock::now();
                let alert = frame_builder.chrome().machine_alert();
                let view = clock.view(
                    &lock,
                    frame_builder.chrome().notification_count(),
                    lock_player.as_ref(),
                    lock_keyboard.shown(),
                    alert.as_deref(),
                );
                publish_accessibility_tree(
                    &mut native_accessibility,
                    &frame_builder,
                    backend.outputs(),
                    Some(&view),
                    test_control_ctx.focus,
                );
                if frame_builder.narrator_enabled() {
                    frame_builder.narrate(&lock_screen::narration_tree(&view));
                }
            } else {
                publish_accessibility_tree(
                    &mut native_accessibility,
                    &frame_builder,
                    backend.outputs(),
                    None,
                    test_control_ctx.focus,
                );
                frame_builder.note_keyboard_focus(test_control_ctx.focus);
                frame_builder.narrate_chrome();
            }
            metrics::profile::record_phase("layout/accessibility", published.elapsed(), None);
            let layout_took = t_layout_start.elapsed();
            metrics.mark_layout(layout_took);
            metrics.mark_cpu(crate::compositor::metrics::CpuPhase::Layout, paint_cpu);
            let t_scene_start = Instant::now();
            let scene_cpu = metrics.cpu_snapshot();
            // When locked, paint the compositor-owned lock overlay instead of the chrome — app content is never submitted while locked.
            let retained_damage_supported = backend.supports_retained_scene_damage();
            frame_builder.set_session_locked(session.is_locked());
            let (scene_width, scene_height) = frame_builder.scene_size();
            foreign::store().begin_presentation_frame(Rect::new(
                0.0,
                0.0,
                scene_width,
                scene_height,
            ));
            let mut frame_paint = if session.is_locked() {
                let theme = frame_builder.theme().clone();
                // The lock overlay covers every known output (single on winit/headless;
                // DRM lays monitors side-by-side).
                let lock_outputs = lock_outputs(
                    backend.outputs(),
                    frame_builder.scene_size(),
                    focused_display_id(&frame_builder),
                );
                let clock = LockClock::now();
                // Critical battery is about the whole machine: every display
                // of the lock screen says so.
                let alert = frame_builder.chrome().machine_alert();
                let view = clock.view(
                    &lock,
                    // Count-only summary on the lock screen — no bodies leak.
                    frame_builder.chrome().notification_count(),
                    lock_player.as_ref(),
                    lock_keyboard.shown(),
                    alert.as_deref(),
                );
                frame_builder.set_lock_keyboard_keys(
                    lock_screen::keyboard_keys(&lock_outputs, &theme, &view)
                        .into_iter()
                        .map(|(key, rect)| (passcode_key_name(key), rect))
                        .collect(),
                );
                frame_builder.set_lock_surfaces(lock_screen::lock_surfaces(
                    &lock_outputs,
                    &theme,
                    view.keyboard.is_some(),
                ));
                let wallpaper = {
                    let chrome = frame_builder.chrome();
                    let tree = chrome.tree();
                    tree.desktop.active_workspace().and_then(|workspace| {
                        crate::shell::desktop_backdrop::wallpaper_image_for_workspace(
                            &workspace.id.0.to_string(),
                            "lockscreen",
                        )
                    })
                };
                // Media keys stay live while locked; their readout is drawn
                // here because the desktop chrome that normally paints it is
                // hidden. Expiring on paint mirrors the unlocked host.
                let mut lock_osd = None;
                frame_builder.with_chrome(|chrome| {
                    chrome.expire_osd(iter_t0);
                    lock_osd = chrome.osd;
                });
                retained_lock.draw(
                    &mut frame_builder,
                    &lock_screen::LockComposition {
                        outputs: &lock_outputs,
                        theme: &theme,
                        view: &view,
                        wallpaper: wallpaper.as_ref(),
                        osd: lock_osd,
                        now: iter_t0,
                    },
                )
            } else {
                frame_builder.set_frame_only_moved(
                    redraw_reasons.only_motion(session.is_locked()) && !transition_run,
                );
                let mut frame_paint = if retained_damage_supported {
                    frame_builder.paint_with_damage()
                } else {
                    let scene = frame_builder.paint();
                    FramePaint::full(scene)
                        .with_hdr_layers(frame_builder.take_hdr_layers())
                        .with_backdrop(frame_builder.take_backdrop())
                };
                // Audience presentation surfaces bypass WorkspaceTree and
                // paint only into their requested output rectangles. The DRM
                // backend splits this combined scene per scanout afterwards.
                if !state.output_presentation_surfaces.is_empty() {
                    let mut presentation = vello::Scene::new();
                    for surface in &state.output_presentation_surfaces {
                        foreign::paint_output_presentation_surface(
                            &mut presentation,
                            surface.surface_id,
                            surface.rect,
                            frame_builder.theme(),
                        );
                    }
                    frame_builder.compose_layout_scene(&presentation, &mut frame_paint.scene);
                }
                if presentation_frame_changed || !state.output_presentation_surfaces.is_empty() {
                    // The output-specific projection is appended after the
                    // retained sections; removal is also a full boundary so
                    // a prior audience projection cannot leave stale pixels.
                    frame_paint.force_full();
                }
                // On top of everything, since a drag is over whatever it's over — drawn
                // here (not by either app) because once the pointer leaves the source window, neither end can paint the carried item.
                if state.selection.dnd_active
                    || state.selection.chrome_drag.is_some()
                    || file_explorer::external_drag::unclaimed().is_some()
                {
                    let visual = state.selection.dnd_label.snapshot();
                    let (cx, cy) = state.client_input.cursor();
                    let theme = frame_builder.theme().clone();
                    let mut feedback = vello::Scene::new();
                    drag_feedback::draw(
                        &mut feedback,
                        vello::kurbo::Point::new(cx, cy),
                        &visual,
                        &theme,
                        frame_builder.text_renderer_mut(),
                    );
                    frame_builder.compose_layout_scene(&feedback, &mut frame_paint.scene);
                    // The feedback was appended outside the retained sections.
                    frame_paint.force_full();
                }
                frame_paint
            };
            metrics.mark_scene(t_scene_start.elapsed());
            metrics.mark_cpu(crate::compositor::metrics::CpuPhase::Scene, scene_cpu);
            // The selector queued semantic work and closed before this clean
            // scene was built. Execute screenshots here, before ownership of
            // the scene moves to the presentation thread; this is the only
            // point shared by winit, DRM and headless backends with both live
            // widget geometry and GPU-readable pixels available.
            if !session.is_locked() {
                if let Some(portal) = portal_backend.as_ref() {
                    portal.drain_capture_requests(
                        &mut capture_executor,
                        &mut frame_builder,
                        &frame_paint.scene,
                        &portal.documents,
                    );
                }
                let (outcomes, capture_overlays_appended) =
                    capture_executor.execute_ready(&mut frame_builder, &mut frame_paint.scene);
                if capture_overlays_appended {
                    // Capture chrome is appended after retained sections, including on an
                    // ordinary live-mirror refresh that has no semantic outcome.
                    frame_paint.force_full();
                }
                for outcome in outcomes {
                    if portal_backend
                        .as_ref()
                        .is_some_and(|portal| portal.finish_interactive_screenshot(&outcome))
                    {
                        continue;
                    }
                    match outcome {
                        CaptureOutcome::Saved(path) => {
                            // The captured miniature moves into a non-modal
                            // shell receipt, so a second notification would
                            // obscure work and split the next action.
                            tracing::info!(path = %path.display(), "screenshot saved");
                            frame_builder.host_mut().set_capture_send_devices(
                                peer_sync.as_deref().map_or_else(Vec::new, |network| {
                                    crate::shell::screenshot_send::online_devices(network)
                                }),
                            );
                        }
                        CaptureOutcome::ScreenshotSendRequested { device, screenshot } => {
                            // The capture worker read the screenshot; the
                            // offer reads no file on the loop.
                            let mut bridge = crate::shell::screenshot_send::FabricSendBridge {
                                network: peer_sync.as_deref(),
                                workspace_root: crate::shell::screenshot_send::workspace_files_root(
                                ),
                            };
                            let sent = crate::shell::screenshot_send::offer(
                                &device,
                                screenshot.take(),
                                &mut bridge,
                            );
                            notify_screenshot_sent(&mut frame_builder, sent);
                        }
                        CaptureOutcome::ScreenshotSent(sent) => {
                            notify_screenshot_sent(&mut frame_builder, sent);
                        }
                        CaptureOutcome::Copied { width, height } => {
                            tracing::info!(width, height, "screenshot copied");
                        }
                        CaptureOutcome::Floated { id } => {
                            tracing::debug!(%id, "live capture mirror created");
                        }
                        CaptureOutcome::Pinned { id } => {
                            tracing::debug!(%id, "screenshot pinned as a frozen reference");
                        }
                        CaptureOutcome::FloatingClosed { id } => {
                            if let Some((connection, public_id)) = external_floating_parts(&id) {
                                if let Some(service) = floating_surface_service.as_ref() {
                                    service.send(
                                        connection,
                                        &ui::floating_surface::FloatingSurfaceHostMessage::Closed {
                                            id: public_id.to_string(),
                                        },
                                    );
                                }
                                if let Some(ids) = external_floating_ids.get_mut(&connection) {
                                    ids.retain(|candidate| candidate != &id);
                                }
                            }
                            tracing::debug!(%id, "live capture mirror closed");
                        }
                        CaptureOutcome::RecordingStarted(path) => {
                            // The compositor-owned pill supplies immediate,
                            // spatial confirmation and then settles into the
                            // top bar. A toast here would duplicate it and
                            // unnecessarily cover application content.
                            tracing::debug!(path = %path.display(), "recording started");
                        }
                        CaptureOutcome::RecordingPaused { path, paused } => {
                            tracing::debug!(
                                path = %path.display(),
                                paused,
                                "recording state changed"
                            );
                        }
                        CaptureOutcome::RecordingAudioLost { path, reason } => {
                            frame_builder.with_chrome(|chrome| {
                                chrome.push_notification(
                                    crate::shell::i18n::t("capture-recording-audio-lost"),
                                    crate::shell::i18n::t_args(
                                        "capture-recording-audio-lost-body",
                                        &[
                                            ("path", path.display().to_string().into()),
                                            ("reason", reason.into()),
                                        ],
                                    ),
                                );
                            });
                        }
                        CaptureOutcome::RecordingStopped { path, frames } => {
                            // The recording pill morphs in-place into a
                            // clickable saved receipt. Avoid a second surface
                            // competing for attention in the top-right corner.
                            tracing::info!(path = %path.display(), frames, "recording saved");
                        }
                        CaptureOutcome::RecordingOpened(path) => {
                            tracing::debug!(path = %path.display(), "recording opened");
                        }
                        CaptureOutcome::RecordingRevealed(path) => {
                            tracing::debug!(path = %path.display(), "recording folder opened");
                        }
                        CaptureOutcome::ReplayStarted => {
                            tracing::info!("recent capture enabled");
                        }
                        CaptureOutcome::ReplaySaved { path, seconds } => {
                            tracing::info!(path = %path.display(), seconds, "recent capture saved");
                        }
                        CaptureOutcome::ReplayStopped => {
                            tracing::info!("recent capture disabled");
                        }
                        CaptureOutcome::ReplayAudioLost(reason) => {
                            frame_builder.with_chrome(|chrome| {
                                chrome.push_warning_notification(
                                    crate::shell::i18n::t("capture-recent-audio-lost"),
                                    reason,
                                );
                            });
                        }
                        CaptureOutcome::ScreenshotOpened(path) => {
                            tracing::debug!(path = %path.display(), "screenshot opened");
                        }
                        CaptureOutcome::ScreenshotMarkupOpened(path) => {
                            tracing::debug!(path = %path.display(), "screenshot markup opened");
                        }
                        CaptureOutcome::ScreenshotMarkupClosed => {
                            tracing::debug!("screenshot markup closed");
                        }
                        CaptureOutcome::ScreenshotRevealed(path) => {
                            tracing::debug!(path = %path.display(), "screenshot folder opened");
                        }
                        CaptureOutcome::PortalScreenshotSaved(path) => {
                            tracing::warn!(path = %path.display(), "portal screenshot completed after its request expired");
                        }
                        CaptureOutcome::PortalScreenshotCancelled => {
                            tracing::debug!(
                                "portal screenshot selector cancelled after its request expired"
                            );
                        }
                        CaptureOutcome::Uncapturable(source) => {
                            tracing::warn!(?source, "capture found nothing to read");
                            frame_builder.with_chrome(|chrome| {
                                chrome.push_error_notification(
                                    crate::shell::i18n::t("capture-failed"),
                                    crate::shell::i18n::t(match source {
                                        ui::widget::SourceMode::Window => {
                                            "capture-uncapturable-window"
                                        }
                                        ui::widget::SourceMode::Region => {
                                            "capture-uncapturable-selection"
                                        }
                                        ui::widget::SourceMode::Strip => {
                                            "capture-uncapturable-strip"
                                        }
                                        ui::widget::SourceMode::Display => {
                                            "capture-uncapturable-screen"
                                        }
                                    }),
                                );
                            });
                        }
                        CaptureOutcome::Failed(error) => {
                            // The reason is for the log; the person is told
                            // the outcome and what to try.
                            tracing::warn!(%error, "capture failed");
                            frame_builder.with_chrome(|chrome| {
                                chrome.push_error_notification(
                                    crate::shell::i18n::t("capture-failed"),
                                    crate::shell::i18n::t("capture-failed-body"),
                                );
                            });
                        }
                    }
                }
                if let Some(portal) = portal_backend.as_ref()
                    && portal_screencast_should_publish(
                        portal.screencast.has_streams(),
                        session.is_locked(),
                    )
                {
                    match capture_executor
                        .render_portal_screencast_frame(&mut frame_builder, &frame_paint.scene)
                    {
                        Ok((width, height, rgba)) => {
                            portal.screencast.publish_frame(width, height, rgba);
                        }
                        Err(error) => {
                            tracing::warn!(%error, "portal: screen-share frame render failed");
                        }
                    }
                }
            }
            // A call sharing this screen gets the composed desktop at its own
            // pace, never the lock screen.
            {
                let share = crate::network::native_media::screen_share();
                if session.is_locked() {
                    share.withhold();
                } else if share.wants_frame(std::time::Instant::now()) {
                    match capture_executor
                        .render_portal_screencast_frame(&mut frame_builder, &frame_paint.scene)
                    {
                        Ok((width, height, rgba)) => share.publish_rgba(width, height, rgba),
                        Err(error) => {
                            tracing::warn!(%error, "call screen-share frame render failed");
                        }
                    }
                }
            }
            // Neither capture path transmits the lock screen: receiver
            // sessions may remain connected, but no fresh frame reveals
            // password entry, notification counts, or unlock timing.
            if !session.is_locked() && frame_builder.host().projection_capture_requested() {
                let viewport = frame_builder.scene_size();
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "clamped to [1.0, u32::MAX] and .round()-ed first, so the casts to \
                              u32 below neither truncate a fraction nor lose a sign"
                )]
                let clamped_viewport = (
                    viewport.0.round().clamp(1.0, f64::from(u32::MAX)) as u32,
                    viewport.1.round().clamp(1.0, f64::from(u32::MAX)) as u32,
                );
                let source = frame_builder.capture_scene(&frame_paint.scene);
                let _ = projection_capture.submit_scene(&source, clamped_viewport);
            }
            // The idle dim goes on last, after captures and casts took their clean copy, so only
            // the local panel darkens.
            if let Some(alpha) = screen_dimmer.overlay_alpha() {
                let rect = backend
                    .outputs()
                    .iter()
                    .map(|o| {
                        vello::kurbo::Rect::new(
                            f64::from(o.position.0),
                            f64::from(o.position.1),
                            f64::from(o.position.0) + f64::from(o.size.0),
                            f64::from(o.position.1) + f64::from(o.size.1),
                        )
                    })
                    .reduce(|a, b| a.union(b))
                    .unwrap_or_else(|| {
                        let (w, h) = frame_builder.viewport();
                        vello::kurbo::Rect::new(0.0, 0.0, w, h)
                    });
                crate::compositor::screen_dim::paint_overlay(&mut frame_paint.scene, rect, alpha);
                frame_paint.force_full();
            }
            // The scene is final: whatever was appended above has voided the
            // candidate if it covers it. The fullscreen client learns what
            // the display would scan out, or that it no longer would.
            state.update_scanout_feedback(crate::compositor::scanout_feedback::offer(
                frame_paint.scanout_candidate(),
                backend.outputs(),
                |output| backend.scanout_plane(output),
            ));
            let feedback_output = (backend.outputs().len() == 1)
                .then(|| {
                    backend
                        .outputs()
                        .first()
                        .and_then(|output| output_globals.handle(output.id))
                        .cloned()
                })
                .flatten();
            let feedback = state
                .presentation_feedback
                .composed(foreign::store(), feedback_output);
            let t_submit_start = Instant::now();
            let submit_cpu = metrics.cpu_snapshot();
            let mut render_thread_lost = false;
            let mut submitted_serial = None;
            let async_present = if let Some(pipe) = present_pipe.as_mut() {
                let submission = pipe.submit_frame_with_input_age(
                    frame_paint,
                    OutputId(0),
                    metrics.pending_input_age(t_submit_start),
                );
                submitted_serial = submission.map(|submission| submission.serial);
                if let Some(submission) = submission {
                    state.presentation_feedback.submitted_replacing(
                        submission.serial,
                        feedback,
                        submission.replaced,
                    );
                }
                render_thread_lost = submitted_serial.is_none();
                // This serial belongs to the scene submitted above.  Do not
                // use a before/after presentation count here: an older
                // desktop frame can be in flight when sleep locks, and its
                // retirement must never release logind's sleep-delay lock.
                if !render_thread_lost
                    && sleep_lock_frame_pending
                    && sleep_lock_frame_serial.is_none()
                    && session.is_locked()
                {
                    sleep_lock_frame_serial = Some(pipe.submitted());
                }
                true
            } else {
                backend.submit_frame_with_damage(&frame_paint, OutputId(0));
                feedback.complete(None);
                false
            };
            if render_thread_lost {
                // Presentation is dead but the session isn't — clients stay served and
                // protocol state is kept, a far better failure mode than tearing the seat down.
                eprintln!(
                    "tilo compositor: render thread gone; presentation stopped \
                     (clients keep running)"
                );
                if sleep_lock_frame_pending {
                    // The just-submitted lock frame (or an earlier accepted
                    // one) might have been lost with the worker.
                    // `present_pipe = None` normally means synchronous
                    // presentation and is acknowledged below; latch this
                    // exception so that fallback cannot release the sleep
                    // delay on an unproven lock scene.
                    sleep_lock_frame_failed = true;
                }
                present_pipe = None;
            } else {
                output_presentation_repaint_pending = false;
            }
            let submit_done = Instant::now();
            // Once the desktop rests after a frame that changed it, the
            // layers the presenter composites strip pans from catch up.
            layer_warm_at = frame_builder
                .layers_stale()
                .then(|| submit_done.checked_add(LAYER_WARM_SETTLE))
                .flatten();
            // Capture lateness against the deadline that admitted this frame.
            // `note_presented` advances the clock immediately below, so doing
            // this afterwards would compare against the *next* interval and
            // hide an overrun by roughly one refresh period.
            metrics.mark_deadline_lateness(
                demand_onset.lateness(submit_done, frame_clock.next_deadline()),
            );
            demand_onset.painted();
            metrics.mark_submit(
                submit_done.saturating_duration_since(t_submit_start),
                async_present,
                submit_done,
                submitted_serial,
            );
            metrics.mark_cpu(crate::compositor::metrics::CpuPhase::Submit, submit_cpu);
            metrics.mark_cpu(crate::compositor::metrics::CpuPhase::Paint, paint_cpu);
            metrics.mark_paint_wall(submit_done.saturating_duration_since(t_layout_start));
            if let Some(serial) = submitted_serial {
                let presented = {
                    let mut keys = crate::compositor::key_latency::tracker();
                    keys.frame_submitted(serial, submit_done);
                    if async_present {
                        Vec::new()
                    } else {
                        keys.frames_retired(serial, submit_done)
                    }
                };
                for sample in presented {
                    eprintln!("{sample}");
                }
            }
            // A synchronous presenter has shown the frame already; an
            // asynchronous one dates it when it retires that serial. A lost
            // render thread queued nothing.
            if !async_present || submitted_serial.is_some() {
                let holding = frame_builder.host().workspace_switch_owed();
                if let Some(switch) = frame_builder
                    .host_mut()
                    .workspace_switch_latency_mut()
                    .frame_queued(
                        submitted_serial,
                        submit_done,
                        frame_clock.interval(),
                        holding,
                    )
                {
                    eprintln!("{switch}");
                }
            }
            // An asynchronous presenter advances the cadence only when its
            // physical retirement is observed at the top of a later loop
            // iteration. Advancing here at scene handoff used to run the
            // software clock ahead of KMS page-flip completion, repeatedly
            // filling the bounded queue and collapsing a 144 Hz panel toward
            // 60 Hz. A grid already locked to the vblank steps to its next
            // point, which the retirement re-anchors on that vblank anyway.
            // Synchronous/headless presentation still retires here.
            if async_present {
                if let Some(serial) = submitted_serial {
                    vblank_scheduler.note_built(serial, iter_t0, &frame_clock);
                }
                frame_clock.note_submitted(submit_done);
            } else {
                frame_clock.note_presented(submit_done);
            }
            if sleep_lock_frame_pending
                && !sleep_lock_frame_failed
                && session.is_locked()
                && present_pipe.is_none()
            {
                power_bridge.lock_screen_drawn();
                sleep_lock_frame_pending = false;
                sleep_lock_frame_serial = None;
                sleep_lock_frame_failed = false;
            }
            // Re-read after `relayout`: its `resync_strips_from_tree` pass can bump
            // the revision, so capture the post-paint value or the next idle tick sees a phantom change.
            last_revision = frame_builder.chrome_revision();
            force_first_paint = false;
            // The layout pass above published each foreign tab's rect — tell clients
            // what size to render at, then release the frame throttle for on-screen ones; both no-ops with no external client.
            foreign::flush_configures(&mut state);
            foreign::release_frame_throttle(&mut state);
            state.client_cursor.send_frame_callbacks(
                u32::try_from(state.start_time.elapsed().as_millis()).unwrap_or(u32::MAX),
            );
            if let Some(stall) = frame_stall.as_mut() {
                stall.stall();
            }
        } else if had_backend_events || accessibility_changed {
            // Input was dispatched but the frame deferred — lay out anyway so the *next*
            // event hit-tests post-input rects, not the pre-overlay tree. Layout is ~0.01ms.
            frame_builder.relayout();
            frame_builder.reconcile_native_focus(&mut test_control_ctx.focus);
            // The post-dispatch `flush_configures` below sends this layout's sizes.
            publish_layout_to_clients(
                &mut surface_output_memberships,
                &mut state,
                &frame_builder,
                &transient_surfaces,
                backend.outputs(),
                &output_globals,
            );
            // An assistive client waits on this tree, not on pixels: a static
            // screen may not paint again for as long as it stays unchanged.
            if accessibility_changed {
                let clock = session.is_locked().then(LockClock::now);
                let alert = frame_builder.chrome().machine_alert();
                let view = clock.as_ref().map(|clock| {
                    clock.view(
                        &lock,
                        frame_builder.chrome().notification_count(),
                        lock_player.as_ref(),
                        lock_keyboard.shown(),
                        alert.as_deref(),
                    )
                });
                publish_accessibility_tree(
                    &mut native_accessibility,
                    &frame_builder,
                    backend.outputs(),
                    view.as_ref(),
                    test_control_ctx.focus,
                );
            }
        }
        if retain_session_redraw(session_changed || injected_redraw, should_paint) {
            injected_redraw_pending = true;
        }
        let set_going_by_paint = animation_settle.after_tick(still_moving, should_paint, || {
            frame_builder.animation_demand().any()
        });
        deliver_held_key_releases!();
        let t_paint_done = iter_t0.elapsed();

        // Confirm an `ext_session_lock_v1` client's lock only after the overlay is
        // presented and locked, so `locked` never fires while app content could still show; a failed lock notifies the client automatically.
        if session.is_locked()
            && let Some(locker) = state.session_lock.take_pending_locker()
        {
            locker.lock();
        }

        let testctl_cpu = metrics.thread_cpu();
        // Accept new test-control connections and drain them in this same
        // iteration, so a request already written is answered without
        // another calloop sleep. One written after this drain wakes the loop
        // through the connection's own source.
        for (listener, agent) in [
            (test_control_listener.as_ref(), false),
            (agent_control_listener.as_ref(), true),
        ] {
            let Some(listener) = listener else { continue };
            loop {
                match listener.accept() {
                    Ok((stream, _addr)) => {
                        if let Some(mut conn) = TestControlConn::new(stream, agent) {
                            conn.watch(&event_loop.handle());
                            test_control_conns.push(conn);
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => {
                        eprintln!("tilo compositor: control accept: {e}");
                        break;
                    }
                }
            }
        }
        if let Some((listener, _, _)) = admin_control.as_ref() {
            loop {
                match listener.accept() {
                    Ok((stream, _addr)) => {
                        match crate::compositor::admin_control::authorize(&stream) {
                            Ok(()) => {
                                if let Some(identity_dir) = identity_store.path().parent()
                                    && let Some(mut conn) =
                                        TestControlConn::admin(stream, identity_dir)
                                {
                                    conn.watch(&event_loop.handle());
                                    test_control_conns.push(conn);
                                }
                            }
                            Err(refusal) => {
                                tracing::warn!(?refusal, "admin control: connection refused");
                                crate::compositor::admin_control::refuse(stream, &refusal);
                            }
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => {
                        eprintln!("tilo compositor: admin control accept: {e}");
                        break;
                    }
                }
            }
        }
        if let Some((door, _, _)) = agent_door.as_ref() {
            let broker = collab_broker
                .as_ref()
                .and_then(crate::shell::collab_broker::BrokerHandle::app_action_broker);
            door.accept(broker.as_ref());
        }
        // A tab the person let control the desktop closed: its grant ends.
        tab_grant_watch.sync(&frame_builder, &event_loop.handle());
        if let Some(handed_over) = agent_door_controls.as_ref() {
            scoped_control::accept(
                handed_over,
                &mut test_control_conns,
                &event_loop.handle(),
                &frame_builder,
                &state,
            );
        }
        for conn in &mut test_control_conns {
            let injected = pump_test_control_conn(
                conn,
                &mut frame_builder,
                &mut test_control_ctx,
                &session,
                &mut lock,
                &mut state,
            );
            if injected {
                metrics.note_input(Instant::now());
                injected_redraw_pending = true;
            } else if std::mem::take(&mut test_control_ctx.client_input_only) {
                metrics.note_input(Instant::now());
            }
            conn.watch_output(&event_loop.handle());
            // An agent at work is the person's activity on a hosted machine:
            // without this it locks under its agents after the idle timeout.
            if injected && conn.agent {
                apply_session_transitions!(session.note_activity(Instant::now()));
            }
        }
        for request in std::mem::take(&mut test_control_ctx.display_requests) {
            match request {
                DisplayRequest::Blank => {
                    apply_session_transitions!(session.blank_now(Instant::now()));
                }
                DisplayRequest::WakeFault(wake_stage) => {
                    if let Err(error) = backend.inject_wake_fault(wake_stage) {
                        eprintln!("tilo compositor: test control: wake fault refused: {error}");
                    }
                }
            }
        }
        if let Some(watching) = test_control_ctx.watch_request.take() {
            // A viewer's own lease governs from here on.
            watched_for_host = false;
            if backend.set_watched(watching) {
                injected_redraw_pending = true;
            }
        }
        // An agent driving the screen answers the shell's choosers; a viewer
        // only watching cannot click one.
        chooser_attended.store(!test_control_ctx.agents.is_empty(), Ordering::Release);
        test_control_conns.retain_mut(|conn| {
            if conn.closed {
                conn.unwatch(&event_loop.handle());
            }
            !conn.closed
        });
        // The person's answers to agent door clients, whether clicked or
        // given through a semantic action above.
        if let Some(consent) = agent_door_consent.as_mut() {
            consent.pump(&frame_builder.chrome_handle());
        }
        deliver_held_key_releases!();
        let t_testctl_done = iter_t0.elapsed();
        metrics.mark_testctl(t_testctl_done.checked_sub(t_paint_done).unwrap_or_default());
        metrics.mark_testctl_cpu(testctl_cpu);

        // Selection bookkeeping after dispatch, not in the protocol handler:
        // smithay raises `new_selection` before installing the new source, so an inline pull would read the previous owner.
        let mut client_steps = ClientSteps::start();
        if let Some(portal) = portal_backend.as_ref() {
            portal.pump_remote_input(&mut state, session.is_locked());
        }
        state.pump_selection();
        client_steps.mark("selection");
        let pump_epoch = foreign::client_pump_epoch();
        let dispatch_started = Instant::now();
        let clients_dispatched = match dispatch_wayland_clients_fairly(&mut display, &mut state) {
            Ok(dispatched) => {
                let elapsed = dispatch_started.elapsed();
                if dispatched > 0 || elapsed > Duration::from_millis(10) {
                    tracing::debug!(
                        target: "tilo::wayland_dispatch",
                        dispatched,
                        elapsed_ms = elapsed.as_secs_f64() * 1000.0,
                        "Wayland client requests dispatched"
                    );
                }
                dispatched > 0
            }
            Err(err) => {
                eprintln!("tilo compositor: dispatch_clients error: {err}");
                break;
            }
        };
        // A client commit can enqueue a GPU readback while this dispatch is
        // running, and the worker may complete before we return. Re-apply
        // released transactions here as well as at tick start; otherwise a
        // continuously chatty client can keep its first buffer stranded until
        // the next outer-loop iteration even though capture already finished.
        client_steps.mark("dispatch");
        let capture_ready = state.pump_foreign_capture_barriers(&display_handle_for_outputs);
        if capture_ready {
            injected_redraw_pending = true;
        }
        client_steps.mark("capture_barriers");
        // A screenshot after input waits for the clients in it to answer
        // the input; their answers were just dispatched, and the pings it
        // sends go out with this flush.
        publish_settled_snapshot(&mut test_control_ctx, &mut frame_builder, &mut state);
        // Answer the request whose screenshot just went out, or whose
        // invocation was answered during this tick, without waiting for an
        // unrelated wake. A request read behind it is read on the next tick.
        let mut control_followup = false;
        for conn in &mut test_control_conns {
            if conn.pending_snapshot.is_some() || conn.pending_invocation.is_some() {
                let mut jobs = FrameInvocationProgress {
                    fb: &frame_builder,
                    ctx: &mut test_control_ctx,
                };
                control_followup |= pump_pending_test_control_conn(conn, &mut jobs)
                    == PendingConnectionProgress::WroteResponse;
            }
        }
        client_steps.mark("snapshot");
        if let Err(err) = display.flush_clients() {
            eprintln!("tilo compositor: flush_clients error: {err}");
            break;
        }
        client_steps.mark("flush");
        // A newly dispatched xdg_toplevel request can arrive after this
        // tick's paint decision. Its tab may already have a placement from
        // the retained chrome tree, but waiting for a *future painted frame*
        // to send the opening configure deadlocks ordinary clients: the
        // client cannot commit pixels until configured, and no pixels means
        // no new content edge to paint. Flush the configure handshake after
        // dispatch as well; it is idempotent for unchanged sizes.
        let configured_after_dispatch = foreign::flush_configures(&mut state);
        if configured_after_dispatch > 0 {
            if let Err(err) = display.flush_clients() {
                eprintln!("tilo compositor: opening configure flush error: {err}");
                break;
            }
            injected_redraw_pending = true;
        }
        client_steps.mark("configures");
        let client_changed = foreign::client_pump_epoch() != pump_epoch;
        // A frame callback that arrived without a new buffer, title, or
        // configure is not a present and not another pump. Release it here,
        // on the tick that saw the commit, so the client is not left waiting
        // on FIFO and the shell does not wake itself to do the same work again.
        if release_idle_client_callback(
            session.is_locked() || display_dark,
            content_changed,
            should_paint,
        ) && !client_changed
            && configured_after_dispatch == 0
            && !capture_ready
            && foreign::release_idle_frame_callbacks(&mut state)
            && let Err(err) = display.flush_clients()
        {
            eprintln!("tilo compositor: idle frame callback flush error: {err}");
            break;
        }
        let client_pump_needed = client_followup_needed(
            clients_dispatched,
            client_changed
                || configured_after_dispatch > 0
                || capture_ready
                || !state.selection.pending_pulls.is_empty(),
        );
        let t_work_done = iter_t0.elapsed();
        metrics.mark_clients(t_work_done.checked_sub(t_testctl_done).unwrap_or_default());
        if t_work_done > Duration::from_millis(100) {
            client_steps.mark("idle_callbacks");
            eprintln!(
                "SLOW-ITER total_work={:?} | input={:?} pump=+{:?} post_pump_paint=+{:?} testctl=+{:?} dispatch_clients=+{:?} ({})",
                t_work_done,
                t_input_done,
                pump_total,
                t_paint_done
                    .checked_sub(pump_done.duration_since(iter_t0))
                    .unwrap_or_default(),
                t_testctl_done.checked_sub(t_paint_done).unwrap_or_default(),
                t_work_done.checked_sub(t_testctl_done).unwrap_or_default(),
                client_steps,
            );
        }

        // Pump pending calloop sources (including the listening socket) and
        // sleep until real work is due. Cadence participates only while a
        // visual change is waiting; registered fds wake this early for input,
        // clients, accessibility, and presenter retirement.
        let park_now = Instant::now();
        // A blanked lock shows no clock, so it does not wake for one.
        let lock_clock_deadline = (session.is_locked() && !session.is_blanked())
            .then(|| last_lock_clock_refresh.checked_add(LOCK_CLOCK_REFRESH))
            .flatten();
        let lock_feedback_deadline = (session.is_locked() && !session.is_blanked())
            .then(|| retained_lock.next_feedback_deadline())
            .flatten();
        // Follow the players while the lock is visible; a blanked display
        // shows nothing, so it does not wake for them.
        let lock_media_deadline = (session.is_locked() && !session.is_blanked())
            .then(|| last_lock_media_poll.checked_add(LOCK_MEDIA_REFRESH))
            .flatten();
        // A redraw requested after this tick's paint decision (control-socket
        // input, a configure flush, a released capture barrier) has no fd
        // left to wake the loop; its frame is due on the next cadence.
        // A motion this tick's paint set going wants its next frame on the
        // next cadence, as one that was already moving does.
        let cadence_needed = cadence_needed
            || cadence_wait_needed(
                pipe_ready,
                injected_redraw_pending
                    || (set_going_by_paint && !session.is_locked())
                    // A newly observed answer starts moving in the lock's
                    // draw, after the earlier cadence decision. Its very next
                    // frame must be a cadence tick, not only the resting deadline.
                    || (session.is_locked()
                        && !session.is_blanked()
                        && retained_lock.feedback_frame_due()),
            );
        // Nothing changed on screen and no frame is waiting, here or behind
        // the presenter: the inputs handled so far are done without a frame.
        // Their latency ends here rather than at the next painted frame,
        // which answers a later input.
        if unpainted_input_settled(
            should_paint,
            content_changed,
            cadence_needed || injected_redraw_pending,
        ) {
            metrics.settle_unpainted_input();
        }
        // The desktop rested since a frame changed it: draw it as layers for
        // the presenter to warm while nothing else is drawn, so the next
        // strip pan starts by copying. Never under a frame on its way, and
        // only once per change.
        if layer_warm_at.is_some_and(|at| park_now >= at) {
            layer_warm_at = None;
            if !cadence_needed
                && !session.is_locked()
                && let Some(pipe) = present_pipe.as_ref()
                && pipe.inflight() == 0
                && let Some(layers) = frame_builder.layers_at_rest()
            {
                let _ = pipe.warm(FramePaint::layers_at_rest(layers));
            }
        }
        let (park, park_deadline) = native_loop_park_named(
            park_now,
            backend_event_wake,
            display.backend().has_pending_client_dispatch(),
            cadence_needed,
            frame_clock.next_deadline(),
            [
                ("widget", app_tick.park_deadline(&frame_builder, pipe_ready)),
                ("lock-clock", lock_clock_deadline),
                ("lock-feedback", lock_feedback_deadline),
                ("lock-media", lock_media_deadline),
                ("run-deadline", run_deadline),
                ("profile-end", metrics::profile::deadline()),
                ("controller-nav", controller_nav.next_deadline(park_now)),
                ("backend-service", backend.next_service_deadline()),
                (
                    "wayland-client-fallback",
                    wayland_client_service_deadline(wayland_client_wake, park_now),
                ),
                ("text-commit", state.text_commit_deadline()),
                ("app-liveness", state.app_liveness_deadline()),
                ("super-hold", super_tap.deadline()),
                (
                    "network-retry",
                    peer_sync
                        .as_ref()
                        .and_then(|network| network.next_deadline()),
                ),
                (
                    "ask-agent",
                    frame_builder.host().ask_agent_deadline(park_now),
                ),
                (
                    "feedback-point",
                    frame_builder.host().feedback_point_deadline(park_now),
                ),
                (
                    "feedback-pick",
                    frame_builder.host().feedback_pick_deadline(park_now),
                ),
                (
                    "dictation-service",
                    state.dictation_service_deadline(park_now),
                ),
                (
                    "presentation-feedback",
                    state.presentation_feedback.next_deadline(),
                ),
                ("touch-hold", test_control_ctx.touch_pointer.deadline()),
                ("legacy-stream", state.legacy_stream_deadline()),
                ("layer-warm", layer_warm_at),
                (
                    "client-followup",
                    client_followup_deadline(client_pump_needed, park_now),
                ),
                (
                    "snapshot-settle",
                    test_control_ctx
                        .pending_snapshot
                        .as_ref()
                        .map(|pending| pending.settle.deadline()),
                ),
                (
                    "control-invocation",
                    control_invocation_deadline(&test_control_conns, park_now),
                ),
                ("control-followup", control_followup.then_some(park_now)),
                // Input read between phases waits here for the next turn;
                // its fd has already been drained, so nothing else wakes it.
                (
                    "queued-input",
                    (!pending_events.is_empty()).then_some(park_now),
                ),
                (
                    "seat-input",
                    (!test_control_ctx.seat_events.is_empty()).then_some(park_now),
                ),
            ],
        );
        // Work (startup, a burst of input, windows closing) leaves freed heap glibc keeps. A
        // loop going to sleep for a while after it asks for one trim once it stays quiet; a
        // minute's clock repaint is not work enough to ask.
        ticks_since_trim = ticks_since_trim.saturating_add(1);
        if ticks_since_trim >= QUIET_TRIM_AFTER_TICKS && park.is_none_or(|park| park >= QUIET_PARK)
        {
            crate::shell::heap_trim::request_when_quiet();
            ticks_since_trim = 0;
        }
        let t_sleep_start = Instant::now();
        if let Some(watchdog) = &watchdog {
            watchdog.parking();
        }
        super::liveness::trace_wake(
            "dispatch-enter-ms",
            park.map_or(u64::MAX, |duration| {
                u64::try_from(duration.as_millis()).unwrap_or(u64::MAX - 1)
            }),
        );
        let dispatched = event_loop.dispatch(park, &mut state);
        super::liveness::trace_wake("dispatch-return", u64::from(dispatched.is_ok()));
        if let Some(watchdog) = &watchdog {
            watchdog.unparked();
        }
        if let Err(err) = dispatched {
            eprintln!("tilo compositor: event loop dispatch error: {err}");
            break;
        }
        let slept = t_sleep_start.elapsed();
        metrics.mark_sleep(slept);
        if credit_transition_hold {
            transition_hold_sleep = slept;
        }
        let wake = park_wake_reason(park, park_deadline, slept);
        metrics.mark_wake(wake);
        woke_for_cadence_only = wake == "cadence" && !clients_dispatched;
        if paint_stats {
            wakes_seen.note(wake);
            last_wake = wake;
            if wake == "widget" && widget_wake_owner.is_none() {
                widget_wake_owner = Some(frame_builder.wake_deadline_owners().join(","));
            }
        }
        metrics.end_tick();
        if let Some(deadline) = run_deadline
            && Instant::now() >= deadline
        {
            close_requested = true;
        }
        let _ = &state.display_handle;

        // Serialize the live `WorkspaceTree` to the requested path, via a
        // tempfile-in-the-same-dir + rename so a concurrently-polling test always sees a consistent snapshot.
        if let Some(path) = test_dump_path.as_ref()
            && let Ok(json) = serde_json::to_vec(&*state.workspace_tree.read())
            && let Err(e) =
                tilo_files::native::atomic::write_creating_parent(std::path::Path::new(path), &json)
        {
            tracing::warn!(error = %e, path = ?path, "workspace tree save failed");
        }
    }
    // Teardown may wait for the stores' last commits: the loop is over.
    drop(no_storage_waits);

    // The session is ending (logout, a close request, a run deadline): every
    // layout change queued for the state writer reaches the disk before the
    // next session can read it.
    // The watchdog still watches teardown and counts this wait among the
    // bounded ones it lets pass.
    crate::shell::persist::wait_for_saves_within(
        crate::compositor::watchdog::SESSION_END_SAVE_FLUSH,
    );

    // Stop the render thread and wait for the frame in flight, so a
    // half-presented frame cannot outlive its device or race backend teardown.
    if let Some(mut pipe) = present_pipe.take() {
        pipe.shutdown();
    }
    if let Some(network) = peer_sync.as_ref() {
        crate::shell::physical_gamepad::stop_all_physical_gamepads(network);
    }

    // Flush the frame trace / write the summary before backend teardown; a
    // no-op unless instrumentation was enabled.
    metrics.finish();

    // Tear down the AT-SPI adapter while the compositor event loop and its
    // session-bus context still exist. This unregisters the native chrome
    // tree before Wayland clients and the display are severed, so a restarting
    // shell cannot leave screen readers holding a dead endpoint.
    drop(native_accessibility);

    // Sever clients before service endpoints disappear. The backend and
    // event loop remain in scope until the function returns below, ensuring
    // the libseat notifier, DRM handles and device leases unwind normally.
    drop(display);
    // Close accepted control clients and their duplicated wake descriptors,
    // not merely the socket names, before publishing an unlocked handoff.
    for conn in &mut test_control_conns {
        conn.unwatch(&event_loop.handle());
    }
    drop(test_control_conns);
    for token in [
        test_control_listener_wake,
        agent_control_listener_wake,
        admin_control.as_mut().and_then(|(_, _, wake)| wake.take()),
        agent_door.as_mut().and_then(|(_, _, wake)| wake.take()),
    ]
    .into_iter()
    .flatten()
    {
        event_loop.handle().remove(token);
    }
    drop(test_control_listener);
    drop(agent_control_listener);
    drop(admin_control);
    drop(agent_door);
    drop(agent_door_consent);
    // Explicitly drop service guards in dependency order so the next session
    // cannot observe stale endpoints while backend teardown finishes.
    drop(test_control_liveness);
    drop(test_control_unlink);
    drop(agent_control_unlink);
    // Same reasoning for the collab broker: its `Drop` quiesces the drain
    // thread and unlinks both sockets, else the next session must tell a stale file from a live one.
    drop(collab_broker);
    drop(tool_service);
    drop(xwayland);
    drop(windows_compat);
    drop(identity_broker);
    drop(floating_surface_service);
    let input_method_sockets = input_methods.sockets().to_vec();
    drop(input_methods);
    drop(display_file);
    unlink_wayland_endpoint(&public_socket_path);
    for path in &input_method_sockets {
        unlink_wayland_endpoint(path);
    }
    if let Some(path) = privileged_socket_path.as_deref() {
        unlink_wayland_endpoint(path);
    }
    // Releases the D-Bus bus name and tears down the notification object
    // server before returning to the session wrapper.
    drop(notif_handle);
    drop(tray_handle);
    drop(polkit_agent);
    drop(network_secret_agent);
    drop(bluetooth_agent);
    // No old scene, client authority or control endpoint can outlive an
    // unlocked handoff. An incomplete/failed teardown keeps the armed record.
    if let Some((recovery, _)) = recovery
        && let Err(error) = recovery.finish(session.is_locked())
    {
        tracing::error!(%error, "session lock handoff failed; next startup validates the saved record");
    }
}

/// Publish a successful machine-level change to every shell surface that
/// mirrors it before raising the transient readout. Quick Settings may be
/// open while a hardware key is pressed, so waiting for its next device read
/// would leave the visible slider behind the value in the OSD.
/// Hand a pointer lock/confinement change to the backend, placing the
/// pointer where a released lock asked.
fn apply_pointer_constraint_change(
    backend: &mut dyn Backend,
    change: crate::compositor::pointer_lock::ConstraintChange,
) {
    backend.set_pointer_constraint(change.region);
    if let Some((x, y)) = change.warp_to {
        backend.warp_pointer(x, y);
    }
}

fn publish_media_osd(
    chrome: &mut crate::shell::chrome_state::ChromeState,
    osd: crate::shell::osd::Osd,
) {
    sync_quick_settings_from_osd(&mut chrome.quick_settings, osd);
    // The volume you just picked, heard at that volume. A muted output has
    // nothing to hear.
    if matches!(
        osd.kind,
        crate::shell::osd::OsdKind::Volume { muted: false }
    ) {
        let _ =
            chrome.play_system_sound(settings_store::system_sounds::SystemSoundEvent::VolumeChange);
    }
    chrome.show_osd(osd);
}

fn sync_quick_settings_from_osd(
    quick_settings: &mut crate::shell::chrome_state::QuickSettingsState,
    osd: crate::shell::osd::Osd,
) {
    match osd.kind {
        crate::shell::osd::OsdKind::Volume { muted } => {
            quick_settings.status.volume = Some(os_actions::VolumeState {
                level: osd.level,
                muted,
            });
        }
        crate::shell::osd::OsdKind::Brightness { display: None } => {
            quick_settings.brightness = osd.level;
        }
        crate::shell::osd::OsdKind::AirplaneMode { on } => {
            quick_settings.radios.airplane_mode = on;
        }
        // A named display's slider follows the brightness service's levels,
        // read back after the key.
        crate::shell::osd::OsdKind::Brightness { display: Some(_) }
        | crate::shell::osd::OsdKind::MicMute { .. }
        | crate::shell::osd::OsdKind::Transport(_)
        | crate::shell::osd::OsdKind::KeyboardBacklight
        | crate::shell::osd::OsdKind::NoBacklight
        | crate::shell::osd::OsdKind::NoAgent
        | crate::shell::osd::OsdKind::StartingAgent
        | crate::shell::osd::OsdKind::FeedbackPointing
        | crate::shell::osd::OsdKind::Touchpad { .. }
        | crate::shell::osd::OsdKind::PointerHeld { .. }
        | crate::shell::osd::OsdKind::MouseKeys { .. }
        | crate::shell::osd::OsdKind::Magnifier { .. }
        | crate::shell::osd::OsdKind::InputSource(_)
        | crate::shell::osd::OsdKind::KeysCaptured { .. }
        | crate::shell::osd::OsdKind::PadMode { .. } => {}
    }
}

#[cfg(test)]
mod lock_focus_tests;

#[cfg(test)]
mod media_osd_publication_tests;

#[cfg(test)]
mod blanked_paint_tests;

/// Session-owned ambient-light loop. Settings only records policy; the
/// compositor owns the display and therefore owns applying that policy after
/// Settings exits. Missing sensors/backlights simply produce no write.
///
/// Follow the ambient light sensor every two seconds while automatic
/// brightness is on. Turning it on or off in Settings wakes the worker at
/// once rather than on its next sample.
fn spawn_auto_brightness_worker(settings: settings_store::live::LiveSettings) {
    let _ = std::thread::Builder::new()
        .name("tilo-auto-brightness".into())
        .spawn(move || {
            let mut controller = os_actions::AmbientBrightnessController::default();
            let mut snapshot = settings.snapshot();
            loop {
                let enabled = settings_store::hardware::AUTO_BRIGHTNESS.from_document(&snapshot.document);
                if enabled && !crate::compositor::screen_dim::backlight_held() {
                    if let Some(lux) = os_actions::ambient_lux()
                        && let Some(percent) = controller.update(lux)
                        && let Err(error) = os_actions::set_brightness(percent)
                    {
                        tracing::warn!(target: "tilo::display", %error, "automatic brightness write failed");
                    }
                } else {
                    controller.reset();
                }
                let wait = auto_brightness_wait(enabled);
                #[cfg(test)]
                AUTO_BRIGHTNESS_WAITS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(wait);
                snapshot = match wait {
                    Some(timeout) => settings.wait_for_change(snapshot.generation, timeout),
                    None => settings.wait_for_next(snapshot.generation),
                };
            }
        });
}

/// How long the automatic-brightness worker waits for a settings change
/// before looking again. While the setting is on, the sensor is read every
/// 2 s, and the idle dim's hold (`backlight_held`) ends with no settings
/// write, so it is looked at again on that cadence. Off, there is nothing to
/// do until the settings change: no timeout.
fn auto_brightness_wait(enabled: bool) -> Option<std::time::Duration> {
    enabled.then_some(std::time::Duration::from_secs(2))
}

/// Every wait the automatic-brightness worker entered, in order.
#[cfg(test)]
static AUTO_BRIGHTNESS_WAITS: std::sync::Mutex<Vec<Option<std::time::Duration>>> =
    std::sync::Mutex::new(Vec::new());

/// Keeps the backend's software dimming in line with the brightness service:
/// a display without a backlight or DDC/CI is dimmed by the compositor to its
/// slider's level ([`os_actions::software_luminance`]), whether the level was
/// set here (keys, Quick Settings, search) or in Settings, whose writes the
/// service follows ([`os_actions::BrightnessService::follow_memory`]).
#[derive(Default)]
struct SoftwareDimSync {
    started: bool,
    /// The service generation last applied.
    generation: Option<u64>,
    /// The outputs it was applied to, by id: a new one is dimmed as it
    /// appears.
    outputs: Vec<u32>,
    /// The software dims last applied.
    dims: Vec<(String, f32)>,
}

impl SoftwareDimSync {
    /// Apply what changed since the last call, and say whether the
    /// service's levels did. Costs a comparison when nothing did; the
    /// service wakes the loop through `wake` when a level changes on another
    /// thread.
    fn sync(&mut self, backend: &mut dyn Backend, wake: &crate::session_runtime::Wake) -> bool {
        // Only a backend that drives the machine's displays (DRM) dims in
        // software and speaks for its connectors; nested and headless ones
        // leave the brightness service alone.
        if !backend.software_dim_supported() {
            return false;
        }
        let service = os_actions::BrightnessService::global();
        if !self.started {
            self.started = true;
            service.on_change(Arc::clone(wake));
            service.follow_memory();
            service.follow_backlight();
        }
        let generation = service.generation();
        let same_outputs = backend
            .outputs()
            .iter()
            .map(|output| output.id.0)
            .eq(self.outputs.iter().copied());
        if same_outputs && self.generation == Some(generation) {
            return false;
        }
        if !same_outputs {
            self.outputs = backend.outputs().iter().map(|output| output.id.0).collect();
            // Brightness controls are for the displays this compositor
            // shows, never every connector the kernel calls connected (a
            // development VM's virtual card, another GPU's port). The
            // service re-discovers them off this thread; a monitor that just
            // appeared is probed once and the generation moves when it lands.
            service.show_only(session_displays(backend.outputs()));
        }
        self.generation = Some(generation);
        let dims: Vec<(String, f32)> = service
            .software_levels()
            .into_iter()
            .map(|(connector, level)| (connector, os_actions::software_luminance(level)))
            .collect();
        // A hardware level moves the generation too; the gamma is left
        // alone unless a software level or the outputs changed.
        if !same_outputs || dims != self.dims {
            backend.set_software_dim(&dims);
            self.dims = dims;
        }
        true
    }
}

/// The displays this compositor shows, by the DRM connector label the
/// brightness service knows them by: a card-qualified key (`<bus>/DP-2`,
/// used only when two cards share a label) is matched by its label. Called
/// for a backend that dims in software.
fn session_displays(outputs: &[crate::compositor::backend::Output]) -> os_actions::SessionDisplays {
    os_actions::SessionDisplays {
        connectors: outputs
            .iter()
            .map(|output| {
                let key = output.identity.connector.as_str();
                key.rsplit_once('/')
                    .map_or(key, |(_, label)| label)
                    .to_owned()
            })
            .collect(),
        software_dim: true,
    }
}

/// Every display whose brightness can be set, named for the readout and
/// Quick Settings: "Built-in display" for the laptop panel, else the
/// monitor's EDID name, else its connector.
fn brightness_displays(
    levels: Vec<os_actions::MonitorLevel>,
    outputs: &[crate::compositor::backend::Output],
) -> Vec<crate::shell::chrome_state::DisplayBrightness> {
    levels
        .into_iter()
        .map(|level| {
            let identity = outputs
                .iter()
                .map(|output| &output.identity)
                .find(|identity| identity.connector == level.connector);
            let name = if os_actions::brightness::is_builtin_connector(&level.connector) {
                crate::shell::i18n::t("display-identify-builtin")
            } else {
                identity
                    .map(|identity| {
                        settings_store::hardware::MonitorIdentity {
                            make: identity.make.clone(),
                            model: identity.model.clone(),
                            serial: identity.serial.clone(),
                            physical_size_mm: None,
                        }
                        .display_name()
                    })
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| level.connector.clone())
            };
            crate::shell::chrome_state::DisplayBrightness {
                connector: level.connector,
                name,
                level: level.level,
            }
        })
        .collect()
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "deterministic control-path geometry fixtures use exactly representable values"
)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
mod tests {
    //! Unit coverage for the test-control execution path. The integration test
    //! (`tests/control_socket_test.rs`) needs a Wayland display and is
    //! `#[ignore]` by default; these exercise the same `execute_planned_action`
    //! → `FrameBuilder::dispatch_event` wire without a process, so CI without winit still gates here.
    use super::*;
    use crate::shell::collab_broker_capnp::{
        native_consent_fixture_root, run_isolated_native_consent_test,
        wait_for_local_session_worker_teardown,
    };
    use tilo_lock::MutexRecover as _;

    /// Automatic brightness is off by default, so on almost every desktop its
    /// worker has nothing to do: it waits for a settings change instead of
    /// waking every 2 s (#3983 measured 0.5 wakeups/s on an idle desktop).
    #[test]
    fn auto_brightness_worker_sleeps_while_the_setting_is_off() {
        let source = std::sync::Arc::new(settings_store::HostDocument::new(
            serde_json::Map::new(),
            |_| Ok(()),
        ));
        let settings = settings_store::live::LiveSettings::start_from(source, || {});
        spawn_auto_brightness_worker(settings);
        let waits = tilo_test_env::wait_for("the auto-brightness worker to wait", || {
            let waits = AUTO_BRIGHTNESS_WAITS.lock_recover().clone();
            (!waits.is_empty()).then_some(waits)
        });
        assert_eq!(
            waits,
            vec![None],
            "the disabled auto-brightness worker waits for a settings change, with no timer"
        );
    }

    #[test]
    fn explicit_display_wake_restores_backend_and_presenter_without_unlock_or_input() {
        use crate::compositor::backend::{BackendEvent, Output};
        use crate::compositor::idle::IdleConfig;
        use std::sync::{Arc, Mutex};

        #[derive(Default)]
        struct PowerBackend {
            dpms: Vec<bool>,
            output_power: Vec<(OutputId, bool)>,
            outputs: Vec<Output>,
        }
        impl Backend for PowerBackend {
            fn submit_frame(&mut self, _scene: &vello::Scene, _output: OutputId) {}
            fn poll_events(&mut self, _callback: &mut dyn FnMut(BackendEvent)) {}
            fn outputs(&self) -> &[Output] {
                &self.outputs
            }
            fn set_dpms(&mut self, on: bool) {
                self.dpms.push(on);
            }
            fn set_output_power(&mut self, output: OutputId, on: bool) {
                self.output_power.push((output, on));
            }
        }
        struct PowerPresenter(Arc<Mutex<Vec<bool>>>);
        impl present::Presenter for PowerPresenter {
            fn present(&mut self, _scene: &vello::Scene, _output: OutputId) {}
            fn set_dpms(&mut self, on: bool) {
                self.0.lock().expect("power log").push(on);
            }
        }
        struct NoBacklight;
        impl Backlight for NoBacklight {
            fn brightness(&self) -> Option<f32> {
                None
            }
            fn set_brightness(&self, _fraction: f32) -> bool {
                false
            }
        }
        let display = smithay::reexports::wayland_server::Display::<State>::new().expect("display");
        let mut management =
            super::super::output_management::OutputManagementState::new(&display.handle());
        let start = Instant::now();
        let mut session = SessionManager::new(
            IdleConfig {
                blank_timeout: Some(Duration::from_secs(5)),
                lock_timeout: Some(Duration::from_secs(2)),
                suspend_timeout: None,
            },
            start,
        );
        let mut backend = PowerBackend::default();
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let mut pipe = present::PresentPipe::spawn(
            Box::new(PowerPresenter(Arc::clone(&recorded))),
            Box::new(|| {}),
        );
        let mut dimmer = ScreenDimmer::new(NoBacklight);
        for transition in session.tick(start + Duration::from_secs(5)) {
            apply_session_transition(
                transition,
                &mut backend,
                Some(&pipe),
                None,
                &mut dimmer,
                Some(&mut management),
            );
        }
        assert!(session.is_locked() && session.is_blanked());
        for seconds in [6, 7] {
            assert!(apply_output_power_request(
                (OutputId(0), true),
                &mut session,
                &mut backend,
                Some(&pipe),
                None,
                &mut dimmer,
                &mut management,
                start + Duration::from_secs(seconds)
            ));
        }
        pipe.shutdown();
        assert_eq!(backend.dpms, vec![false, true, true]);
        assert_eq!(
            *recorded.lock().expect("power log"),
            vec![false, true, true]
        );
        assert_eq!(backend.output_power, vec![(OutputId(0), true); 2]);
        assert!(session.is_locked() && !session.is_blanked());
        assert_eq!(
            session.idle_for(start + Duration::from_secs(7)),
            Duration::from_secs(7)
        );
        assert!(
            !session
                .tick(start + Duration::from_secs(11))
                .contains(&SessionTransition::Blanked)
        );
    }

    #[test]
    fn pointer_hover_preserves_touch_keyboard_until_pointer_activation() {
        use crate::compositor::input_method::{OskInput, osk_wanted};
        use tilo_accessibility_settings::OnScreenKeyboard;

        let mut ctx = TestControlCtx::new();
        let mut fb = FrameBuilder::new((800, 600));
        ctx.note_injected_input(
            &PlannedAction::Touch {
                device: "panel".into(),
                slot: 0,
                phase: TouchPhaseKind::Down,
                x: 100.0,
                y: 100.0,
            },
            &mut fb,
        );
        assert!(osk_wanted(
            OnScreenKeyboard::Auto,
            OskInput::from_environment(&ctx.input)
        ));
        ctx.note_injected_input(&PlannedAction::CursorMoved { x: 110.0, y: 100.0 }, &mut fb);
        assert!(
            osk_wanted(
                OnScreenKeyboard::Auto,
                OskInput::from_environment(&ctx.input)
            ),
            "incidental pointer hover must not dismiss a touch-started keyboard"
        );
        ctx.note_injected_input(&PlannedAction::MouseLeftButton { pressed: true }, &mut fb);
        assert!(!osk_wanted(
            OnScreenKeyboard::Auto,
            OskInput::from_environment(&ctx.input)
        ));
    }

    #[test]
    fn action_gate_releases_membership_lock_before_real_callback() {
        use crate::shell::state::{FreshId as _, WorkspaceId};
        use tilo_action_broker::services::WorkspaceAuthority as _;

        let id = WorkspaceId::new();
        let mut tree = crate::shell::state::WorkspaceTree::default();
        tree.desktop
            .workspaces
            .push(crate::shell::state::Workspace::new(id, "Gate test"));
        tree.desktop.active = id;
        let handle = tree.shared();
        let chrome = crate::shell::chrome_state::ChromeState::new_with_fixture_handle(
            handle.clone(),
            crate::shell::hosted_inputs::HostedTileInputs::default(),
        );
        with_action_gate(&chrome, |_, gate| {
            let write = handle.try_write().expect(
                "action callback must not inherit a membership read guard that blocks writers",
            );
            drop(write);
            let current = crate::shell::broker_services::NativeWorkspaceAuthority(handle.clone())
                .snapshot()
                .expect("the real certified-admission membership source remains readable");
            assert_eq!(current.active, id);
            assert!(
                !gate.can_see(
                    &ResourceRef::new("applications", "so.tilo.chat"),
                    &crate::shell::search::ResourceKind::Application,
                ),
                "snapshot visibility must still deny an ungranted application write"
            );
            assert!(
                gate.can_see(
                    &ResourceRef::new("notes", "note"),
                    &crate::shell::search::ResourceKind::Note,
                ),
                "ordinary view-only resources retain their existing visibility"
            );
        })
        .unwrap();
    }

    #[test]
    fn action_gate_missing_workspace_never_calls_the_callback() {
        let chrome = crate::shell::chrome_state::ChromeState::new_with_fixture_handle(
            crate::shell::state::WorkspaceTree::default().shared(),
            crate::shell::hosted_inputs::HostedTileInputs::default(),
        );
        assert!(
            with_action_gate(&chrome, |_, _| panic!(
                "missing workspace cannot expose actions"
            ))
            .is_err()
        );
    }

    #[test]
    fn dispatched_client_requests_get_a_follow_up_tick() {
        let now = Instant::now();
        let frame = now + Duration::from_millis(8);
        assert_eq!(
            native_loop_park(
                now,
                true,
                false,
                false,
                frame,
                [client_followup_deadline(true, now)]
            ),
            Some(Duration::ZERO),
            "a new toplevel's first configure must not wait for an unrelated wake"
        );
        assert_eq!(
            native_loop_park(
                now,
                true,
                false,
                false,
                frame,
                [client_followup_deadline(false, now)]
            ),
            None,
            "a tick that dispatched nothing parks"
        );
        assert!(
            !client_followup_needed(true, false),
            "a wl_surface.frame with no new pixels, title, or configure must not schedule another shell tick"
        );
        assert!(
            client_followup_needed(true, true),
            "a buffer or protocol change still pumps on the next tick"
        );
        assert!(
            release_idle_client_callback(false, false, false),
            "an unchanged desktop releases a waiting callback without presenting"
        );
        assert!(
            !present_for_app_poll(false, true),
            "a poll whose tree, layout, and animations did not change must not present"
        );
        assert!(present_for_app_poll(true, true));
        assert!(!present_for_app_poll(false, false));
        assert_eq!(
            client_followup_deadline(client_followup_needed(true, false), now),
            None,
            "answering that callback must not schedule another shell tick"
        );
        assert!(
            !release_idle_client_callback(false, true, false),
            "a real change still presents, and that present releases the callback"
        );
    }

    #[test]
    fn a_park_names_the_deadline_that_ends_it() {
        let now = Instant::now();
        let frame = now + Duration::from_millis(8);
        let (park, name) = native_loop_park_named(
            now,
            true,
            false,
            false,
            frame,
            [
                ("lock-clock", Some(now + Duration::from_secs(30))),
                ("widget", Some(now + Duration::from_millis(250))),
                ("text-commit", None),
            ],
        );
        assert_eq!(park, Some(Duration::from_millis(250)));
        assert_eq!(name, Some("widget"), "the earliest deadline owns the wake");
        assert_eq!(
            park_wake_reason(park, name, Duration::from_millis(250)),
            "widget",
            "a park that ran its full timeout was ended by that deadline"
        );
        assert_eq!(
            park_wake_reason(park, name, Duration::from_millis(40)),
            super::metrics::WAKE_IO,
            "a park cut short was ended by an fd, not the deadline"
        );
        assert_eq!(
            native_loop_park_named(now, true, false, true, frame, []).1,
            Some("cadence")
        );
        assert_eq!(
            native_loop_park_named(now, false, false, false, frame, []).1,
            Some("idle-backend-poll"),
            "the bounded poll that stands in for a missing wake fd is named too"
        );
        assert_eq!(
            park_wake_reason(None, None, Duration::from_secs(5)),
            super::metrics::WAKE_IO,
            "only an fd can end an unbounded park"
        );
        let mut tally = WakeTally::default();
        for wake in ["widget", "io", "widget"] {
            tally.note(wake);
        }
        assert_eq!(tally.take_summary(), "widget=2,io=1");
        assert_eq!(
            tally.take_summary(),
            "",
            "each report covers its own window"
        );
    }

    /// An idle desktop that keeps painting "animation" must say whose motion
    /// it is in the frame trace, the one record an acceptance run keeps of
    /// its idle window. The label stays one per owner set, not one per frame.
    #[test]
    fn a_traced_animation_names_its_owner_without_its_geometry() {
        let owners = vec![
            "notification".to_string(),
            "content/DesktopSurface/GroupRow/WindowGroup/TileHost@1748,92 1051x976".to_string(),
            "content/DesktopSurface/GroupRow/WindowGroup/TileHost@20,92 600x976".to_string(),
        ];
        assert_eq!(
            animation_trace_label(&owners, false),
            "notification+content/DesktopSurface/GroupRow/WindowGroup/TileHost"
        );
        assert_eq!(
            animation_trace_label(&[], true),
            "transition",
            "a compositor transition's frame run is named"
        );
        assert_eq!(
            animation_trace_label(&[], false),
            "resting-frame",
            "the frame owed after motion ended is named, never blank"
        );
        assert!(!animation_trace_label(&["a,b".to_string()], false).contains(','));

        let mut trace = AnimationTraceLabel::default();
        let demand = crate::shell::frame::AnimationDemand {
            osk: true,
            ..Default::default()
        };
        let mut walks = 0;
        let start = Instant::now();
        for _ in 0..3 {
            let label = trace.label((demand, false, 7), start, || {
                walks += 1;
                vec!["osk".to_string()]
            });
            assert_eq!(label, "osk");
        }
        assert_eq!(walks, 1, "the owners are looked up once per set of sources");
        let _ = trace.label((demand, false, 8), start, || {
            walks += 1;
            vec!["osk".to_string()]
        });
        assert_eq!(walks, 2, "a chrome change looks them up again");

        // The same sources, but the motion has passed from a window that
        // settled to another widget: within a second the label says so.
        let later = start
            .checked_add(ANIMATION_LABEL_REFRESH)
            .expect("a second after now");
        let label = trace.label((demand, false, 8), later, || {
            walks += 1;
            vec!["content/DesktopSurface/TileHost@0,0 10x10".to_string()]
        });
        assert_eq!(walks, 3, "a label is not kept past its refresh");
        assert_eq!(label, "content/DesktopSurface/TileHost");
    }

    #[test]
    fn an_animation_ends_on_a_painted_resting_frame() {
        let mut settle = AnimationSettle::default();
        let still = || false;
        assert!(!settle.due(false), "nothing moving owes nothing");
        // The last moving tick finds the presenter full and cannot paint.
        settle.after_tick(true, false, still);
        assert!(
            settle.due(false),
            "the tick after motion ends still owes the resting frame"
        );
        // Off-cadence: still not painted, still owed.
        settle.after_tick(false, false, still);
        assert!(settle.due(false));
        // Painted at rest: settled.
        settle.after_tick(false, true, still);
        assert!(!settle.due(false));
        // A frame painted while still moving samples mid-motion; the
        // resting frame remains owed.
        settle.after_tick(true, true, still);
        assert!(settle.due(false));
    }

    /// A frame painted at rest may set a motion going itself: a tooltip's
    /// arrival begins as the frame that reveals it draws its first,
    /// invisible pose. Nothing moved before that paint, so only a read
    /// after it sees the motion; the loop then owes its frames and its
    /// resting frame, or the tooltip never shows.
    #[test]
    fn a_motion_the_paint_sets_going_gets_its_frames() {
        let mut settle = AnimationSettle::default();
        assert!(
            settle.after_tick(false, true, || true),
            "the paint set a motion going: the loop comes back on the next cadence"
        );
        assert!(settle.due(false), "and that tick paints it");
        // The motion is seen moving before the next paint: no second read.
        assert!(!settle.after_tick(true, true, || unreachable!()));
        assert!(settle.due(false), "its resting frame is still owed");
        settle.after_tick(false, true, || false);
        assert!(!settle.due(false), "painted at rest: settled");
        // An unpainted tick changes nothing on screen and sets nothing going.
        assert!(!settle.after_tick(false, false, || unreachable!()));
    }

    /// A key the focused client alone took asks for no frame: the client's
    /// commit does, and a frame painted for the key itself would spend the
    /// frame clock's deadline on an unchanged desktop and hold the commit's
    /// frame back a refresh interval. Keys the shell kept, and every other
    /// backend event, still ask for one.
    #[test]
    fn a_key_only_a_client_took_asks_for_no_frame_of_its_own() {
        let key = |pressed| {
            BackendEvent::Input(keyboard_input_event(
                &winit::keyboard::Key::Character("a".into()),
                pressed,
            ))
        };
        let mut typed = DispatchedInput::default();
        typed.note(&key(true));
        typed.note(&key(false));
        assert!(
            !typed.asks_for_frame(2),
            "a press and release forwarded to the client change no shell pixels"
        );
        assert!(
            typed.asks_for_frame(1),
            "a key the shell kept (a shortcut, typing into the chrome) asks for a frame"
        );
        let mut physical = DispatchedInput::default();
        physical.note(&BackendEvent::PhysicalKey {
            event: keyboard_input_event(&winit::keyboard::Key::Character("a".into()), true),
            input_time_ns: 1,
        });
        assert!(
            !physical.asks_for_frame(1),
            "a physical key counts as a key"
        );
        let mut pointer = typed;
        pointer.note(&BackendEvent::Redraw);
        assert!(
            pointer.asks_for_frame(2),
            "any other backend event in the same tick still asks for a frame"
        );
        assert!(
            !DispatchedInput::default().asks_for_frame(0),
            "no input, no frame"
        );
    }

    #[test]
    fn redraw_edges_and_idle_parking_follow_real_wake_sources() {
        // Phase `consumed_shell_input_is_a_redraw_edge`.
        assert!(
            NativeRedrawReasons {
                session: true,
                ..NativeRedrawReasons::default()
            }
            .content_changed(false)
        );
        // Phase `client_routed_pointer_motion_still_redraws_the_compositor_cursor`.
        assert!(
            NativeRedrawReasons {
                visible_input: true,
                ..NativeRedrawReasons::default()
            }
            .content_changed(false),
            "the cursor belongs to the compositor, so a client consuming its motion must not suppress the reflecting frame"
        );
        // Phase `idle_park_ignores_display_cadence`.
        {
            let now = Instant::now();
            let frame = now + Duration::from_millis(8);
            let keepalive = now + Duration::from_secs(1);
            assert_eq!(
                native_loop_park(now, true, false, false, frame, [Some(keepalive)]),
                Some(Duration::from_secs(1)),
                "a registered backend wake fd lets a static desktop sleep until semantic work"
            );
        }
        // Phase `idle_park_without_any_semantic_deadline_still_services_nested_backend`.
        {
            let now = Instant::now();
            assert_eq!(
                native_loop_park(now, true, false, false, now + Duration::from_millis(8), []),
                None,
                "a static DRM session can block indefinitely because libinput wakes the outer loop"
            );
        }
        // Phase `failed_backend_wake_registration_keeps_the_bounded_idle_poll`.
        {
            let now = Instant::now();
            assert_eq!(
                native_loop_park(now, false, false, false, now + Duration::from_millis(8), []),
                Some(IDLE_BACKEND_POLL),
                "a backend whose wake fd could not be registered must still be serviced"
            );
        }
        // Phase `backend_owned_repeat_deadline_wakes_an_otherwise_idle_loop`.
        {
            let now = Instant::now();
            let repeat = now + Duration::from_millis(275);
            assert_eq!(
                native_loop_park(
                    now,
                    true,
                    false,
                    false,
                    now + Duration::from_millis(8),
                    [Some(repeat)],
                ),
                Some(Duration::from_millis(275)),
                "registered backend fds do not replace timers synthesized inside the backend"
            );
        }
        // Phase `registered_wayland_wake_removes_periodic_idle_poll`.
        {
            let now = Instant::now();

            assert_eq!(wayland_client_service_deadline(true, now), None);
            assert_eq!(
                native_loop_park(
                    now,
                    true,
                    false,
                    false,
                    now + Duration::from_millis(8),
                    [wayland_client_service_deadline(true, now)],
                ),
                None,
                "a static desktop must block on registered fds instead of waking every 4 ms"
            );
        }
        // Phase `failed_wayland_wake_registration_keeps_bounded_service_fallback`.
        {
            let now = Instant::now();

            assert_eq!(
                wayland_client_service_deadline(false, now),
                Some(now + WAYLAND_CLIENT_SERVICE_FALLBACK),
                "an unsupported edge-triggered wake must not strand Wayland client requests"
            );
        }
        // Phase `visual_work_parks_on_frame_cadence`.
        {
            let now = Instant::now();
            let frame = now + Duration::from_millis(8);
            let keepalive = now + Duration::from_secs(1);
            assert_eq!(
                native_loop_park(now, true, false, true, frame, [Some(keepalive)]),
                Some(Duration::from_millis(8))
            );
        }
        // Phase `decoded_wayland_work_gets_a_fair_follow_up_without_spinning`.
        {
            let now = Instant::now();
            assert_eq!(
                native_loop_park(now, true, true, false, now + Duration::from_millis(8), []),
                Some(WAYLAND_PENDING_RETRY_FLOOR),
                "decoded requests left by a bounded turn need another turn even after epoll is drained"
            );
        }
    }

    #[test]
    fn wayland_socket_attribution_and_cleanup_fail_closed() {
        // Phase `stale_private_wayland_endpoints_are_recovered_without_touching_live_pids`.
        {
            let runtime = tempfile::tempdir().expect("temporary runtime");
            let dead_pid = u32::MAX;
            let live_pid = std::process::id();
            for name in [
                format!("tilo-privileged-{dead_pid}"),
                format!("tilo-osk-{dead_pid}-old"),
                format!("tilo-privileged-{live_pid}"),
                format!("tilo-osk-{live_pid}-live"),
            ] {
                std::fs::write(runtime.path().join(&name), b"").expect("socket fixture");
                std::fs::write(runtime.path().join(format!("{name}.lock")), b"")
                    .expect("lock fixture");
            }

            remove_stale_private_wayland_endpoints(runtime.path());

            for name in [
                format!("tilo-privileged-{dead_pid}"),
                format!("tilo-osk-{dead_pid}-old"),
            ] {
                assert!(!runtime.path().join(&name).exists());
                assert!(!runtime.path().join(format!("{name}.lock")).exists());
            }
            for name in [
                format!("tilo-privileged-{live_pid}"),
                format!("tilo-osk-{live_pid}-live"),
            ] {
                assert!(runtime.path().join(&name).exists());
                assert!(runtime.path().join(format!("{name}.lock")).exists());
            }
        }
        // Phase `public_socket_attribution_reads_kernel_peer_pid`.
        {
            let runtime = tempfile::tempdir().unwrap();
            let path = runtime.path().join("peer.sock");
            let listener = UnixListener::bind(path.clone()).unwrap();
            let client = UnixStream::connect(path).unwrap();
            let (server, _) = listener.accept().unwrap();
            assert_eq!(
                wayland_peer_credentials(&server).map(|credentials| credentials.pid),
                i32::try_from(std::process::id()).ok()
            );
            assert!(
                public_client_state(&server)
                    .expect("same-namespace host clients remain compatible")
                    .package_id
                    .is_none()
            );
            drop(client);
        }
        // Phase `unattributed_public_clients_fail_closed_outside_host_namespaces`.
        {
            let uid = rustix::process::geteuid().as_raw();
            assert!(unattributed_host_client_allowed(uid, true));
            assert!(!unattributed_host_client_allowed(uid, false));
            assert!(!unattributed_host_client_allowed(
                uid.saturating_add(1),
                true
            ));
        }
        // Phase `unlink_on_drop_removes_socket_file_on_drop`.
        // Dropping an `UnlinkOnDrop` removes the file at its path — `run` holds
        // one around the test-control socket so it unlinks on clean exit + panic-unwind.
        {
            let tmpdir = tempfile::tempdir().expect("tempdir");
            let path = tmpdir.path().join("tilo-test-control.sock");
            // Use an arbitrary regular-file stand-in — the guard removes anything
            // `remove_file` accepts, no real unix socket needed to exercise the contract.
            std::fs::write(&path, b"").expect("create stand-in file");
            assert!(path.exists(), "test setup: file should exist after write");
            let guard = UnlinkOnDrop(path.clone());
            drop(guard);
            assert!(
                !path.exists(),
                "UnlinkOnDrop must remove the file at its path on drop \
                 (D-3); got lingering file at {path:?}"
            );
        }
    }

    #[test]
    fn a_full_presenter_holds_a_pending_transition() {
        assert!(present_hold_credit(true, true, true));
        assert!(!present_hold_credit(false, true, true));
        assert!(
            !present_hold_credit(true, true, false),
            "an idle desktop must not freeze its clock on a full presenter"
        );
        assert!(
            !present_hold_credit(true, false, true),
            "a vblank-paced presenter is full from each handoff to its flip; \
             crediting that wait would slow every transition by the lead"
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn presenter_backpressure_and_deadlines_schedule_work() {
        // Phase `an_input_that_changes_nothing_settles_without_a_frame`.
        assert!(
            unpainted_input_settled(false, false, false),
            "a key release that changes nothing is answered when the loop parks"
        );
        assert!(
            !unpainted_input_settled(true, false, false),
            "a painted tick closes the latency on its own frame"
        );
        assert!(
            !unpainted_input_settled(false, true, cadence_wait_needed(false, true)),
            "a frame held back by a full presenter still owes the input its latency"
        );
        assert!(
            !unpainted_input_settled(false, false, cadence_wait_needed(true, true)),
            "a frame waiting for its cadence still owes the input its latency"
        );
        // A redraw a control-socket input asked for after the paint decision,
        // behind a full presenter (so no cadence wait is armed for it).
        let control_redraw = true;
        assert!(
            !unpainted_input_settled(
                false,
                false,
                cadence_wait_needed(false, control_redraw) || control_redraw
            ),
            "a redraw asked for after the paint decision is still owed"
        );
        // Phase `prepaint_stall_threshold_reports_fifty_milliseconds_and_above`.
        assert!(!slow_prepaint_phase(Duration::from_millis(49)));
        assert!(slow_prepaint_phase(Duration::from_millis(50)));
        assert!(slow_prepaint_phase(Duration::from_secs(2)));
        // Phase `full_presenter_parks_instead_of_spinning_on_overdue_deadlines`.
        {
            let now = Instant::now();
            let stale = now
                .checked_sub(Duration::from_millis(1))
                .expect("monotonic clock is past its origin");
            // Animation stays dirty and both the frame and widget deadlines are
            // already due, but the presenter has no capacity.
            let cadence_needed = cadence_wait_needed(false, true);
            assert_eq!(
                native_loop_park(
                    now,
                    false,
                    false,
                    cadence_needed,
                    stale,
                    [widget_deadline_for_presenter(false, Some(stale))],
                ),
                Some(IDLE_BACKEND_POLL),
                "a saturated presenter must wait for its retirement wake, not poll with a zero timeout"
            );
        }
        // Phase `lock_state_edge_survives_presenter_backpressure_until_submitted`.
        assert!(retain_session_redraw(true, false));
        assert!(
            !retain_session_redraw(true, true),
            "a submitted replacement frame consumes the lock-state edge"
        );
        assert!(
            !retain_session_redraw(false, false),
            "ordinary idle ticks must not manufacture redraw work"
        );
        // Phase `an_idle_frame_callback_does_not_present`.
        assert!(
            release_idle_client_callback(false, false, false),
            "a callback with no tree, layout, or animation change is released without a frame"
        );
        assert!(
            !release_idle_client_callback(false, true, false),
            "a change whose frame is still owed presents, and that present releases the callback"
        );
        assert!(
            release_idle_client_callback(false, true, true),
            "a callback asked for after this tick's present is released now: no later \
             present is coming for it, and the client waiting on it draws nothing until it fires"
        );
        assert!(
            !release_idle_client_callback(true, false, false),
            "a locked session does not answer hidden clients with an idle release"
        );
        // Phase `available_presenter_keeps_overdue_work_immediate`.
        {
            let now = Instant::now();
            let stale = now
                .checked_sub(Duration::from_millis(1))
                .expect("monotonic clock is past its origin");
            assert_eq!(
                widget_deadline_for_presenter(true, Some(stale)),
                Some(stale)
            );
            let frame = now + Duration::from_millis(7);
            assert_eq!(
                native_loop_park(
                    now,
                    false,
                    false,
                    cadence_wait_needed(true, true),
                    frame,
                    []
                ),
                Some(Duration::from_millis(7)),
                "the back-pressure guard must not weaken normal animation pacing"
            );
        }
        // Phase `earliest_non_frame_deadline_wins_and_overdue_is_immediate`.
        {
            let now = Instant::now();
            let frame = now + Duration::from_millis(8);
            let widget = now + Duration::from_millis(300);
            let keepalive = now + Duration::from_secs(1);
            assert_eq!(
                native_loop_park(
                    now,
                    true,
                    false,
                    false,
                    frame,
                    [Some(keepalive), Some(widget)],
                ),
                Some(Duration::from_millis(300))
            );
            assert_eq!(
                native_loop_park(
                    now + Duration::from_millis(1),
                    true,
                    false,
                    false,
                    frame,
                    [Some(now)]
                ),
                Some(Duration::ZERO)
            );
        }
        // Phase `apply_transition_drives_backend_dpms`.
        // `apply_session_transition` maps Blanked/Woke to `set_dpms`;
        // Locked/Unlocked are pure state and touch no backend — asserted via a spy backend recording `set_dpms` calls.
        {
            use crate::compositor::backend::{Backend, BackendEvent, Output};
            use vello::Scene;

            #[derive(Default)]
            struct SpyBackend {
                dpms_calls: Vec<bool>,
                outputs: Vec<Output>,
            }
            impl Backend for SpyBackend {
                fn submit_frame(&mut self, _scene: &Scene, _output: OutputId) {}
                fn poll_events(&mut self, _cb: &mut dyn FnMut(BackendEvent)) {}
                fn outputs(&self) -> &[Output] {
                    &self.outputs
                }
                fn set_dpms(&mut self, on: bool) {
                    self.dpms_calls.push(on);
                }
            }

            struct NoBacklight;
            impl crate::compositor::screen_dim::Backlight for NoBacklight {
                fn brightness(&self) -> Option<f32> {
                    None
                }
                fn set_brightness(&self, _fraction: f32) -> bool {
                    false
                }
            }

            let mut backend = SpyBackend::default();
            let mut dimmer = ScreenDimmer::new(NoBacklight);
            for t in [
                SessionTransition::Dimmed,
                SessionTransition::Blanked,
                SessionTransition::Woke,
                SessionTransition::Locked,
                SessionTransition::Unlocked,
            ] {
                apply_session_transition(t, &mut backend, None, None, &mut dimmer, None);
            }
            // Blanked → set_dpms(false), Woke → set_dpms(true); Locked/Unlocked produce no backend calls.
            assert_eq!(backend.dpms_calls, vec![false, true]);
            // Without a backlight the dim falls back to the paint overlay, lifted by Undimmed.
            assert!(dimmer.overlay_alpha().is_some());
            apply_session_transition(
                SessionTransition::Undimmed,
                &mut backend,
                None,
                None,
                &mut dimmer,
                None,
            );
            assert_eq!(dimmer.overlay_alpha(), None);
        }
    }

    /// A client commit is an edge the loop sees once. Seen on a tick that may
    /// not paint (the frame clock is not due, or the presenter is full), it
    /// stays owed until a frame is painted: before, the frame waited for the
    /// next unrelated event, a key's release a tenth of a second later.
    #[test]
    fn a_change_seen_on_a_tick_that_could_not_paint_is_still_owed() {
        use crate::compositor::metrics::paint_reason as r;
        let owed = NativeRedrawReasons {
            owed: true,
            ..NativeRedrawReasons::default()
        };
        assert!(owed.content_changed(false));
        assert!(
            owed.content_changed(true),
            "only changes the tick counted are owed, so the lock does not filter them again"
        );
        assert_eq!(owed.paint_reason_bits(false, false), r::OWED);
    }

    /// Only a frame an animation alone asks for, on an unlocked desktop,
    /// may show windows at rest as they were drawn.
    #[test]
    fn only_an_animation_alone_is_a_frame_that_only_moved() {
        let animation = NativeRedrawReasons {
            hidden_desktop_animation: true,
            ..NativeRedrawReasons::default()
        };
        assert!(animation.only_motion(false));
        assert!(!animation.only_motion(true), "not on a locked desktop");
        assert!(!NativeRedrawReasons::default().only_motion(false));
        for with in [
            NativeRedrawReasons {
                apps: true,
                ..animation
            },
            NativeRedrawReasons {
                backend_event: true,
                ..animation
            },
            NativeRedrawReasons {
                revision: true,
                ..animation
            },
            NativeRedrawReasons {
                owed: true,
                ..animation
            },
            NativeRedrawReasons {
                peers: true,
                ..animation
            },
        ] {
            assert!(!with.only_motion(false), "{with:?}");
        }
    }

    #[test]
    fn paint_reasons_name_exactly_what_content_changed_counts() {
        use crate::compositor::metrics::paint_reason as r;
        let quiet = NativeRedrawReasons::default();
        assert!(!quiet.content_changed(false));
        assert_eq!(quiet.paint_reason_bits(false, false), 0);
        let commit = NativeRedrawReasons {
            apps: true,
            ..NativeRedrawReasons::default()
        };
        assert_eq!(commit.paint_reason_bits(false, false), r::APPS);
        assert_eq!(
            commit.paint_reason_bits(true, false),
            0,
            "a hidden desktop's commit is not why a locked tick paints"
        );
        let fade = NativeRedrawReasons {
            hidden_desktop_animation: true,
            revision: true,
            ..NativeRedrawReasons::default()
        };
        assert_eq!(
            fade.paint_reason_bits(false, true),
            r::REVISION | r::ANIMATION | r::TRANSITION
        );
        assert_eq!(
            fade.paint_reason_bits(false, false),
            r::REVISION | r::ANIMATION,
            "an animation that is not a compositor transition is not named one"
        );
        let lock = NativeRedrawReasons {
            lock_clock: true,
            ..NativeRedrawReasons::default()
        };
        assert_eq!(lock.paint_reason_bits(true, false), r::LOCK_CLOCK);
        assert_eq!(lock.paint_reason_bits(false, false), 0);
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn locked_redraw_repaints_only_lock_edges() {
        // Phase `locked_redraw_ignores_hidden_desktop_work_but_keeps_lock_edges`.
        for hidden in [
            NativeRedrawReasons {
                hidden_desktop_animation: true,
                ..NativeRedrawReasons::default()
            },
            NativeRedrawReasons {
                apps: true,
                ..NativeRedrawReasons::default()
            },
            NativeRedrawReasons {
                peers: true,
                ..NativeRedrawReasons::default()
            },
            NativeRedrawReasons {
                presentation: true,
                ..NativeRedrawReasons::default()
            },
            NativeRedrawReasons {
                capture: true,
                ..NativeRedrawReasons::default()
            },
            NativeRedrawReasons {
                keepalive: true,
                ..NativeRedrawReasons::default()
            },
        ] {
            assert!(!hidden.content_changed(true));
            assert!(
                hidden.content_changed(false),
                "unlocked redraw behaviour must remain unchanged"
            );
        }

        for visible_lock_edge in [
            NativeRedrawReasons {
                backend_event: true,
                ..NativeRedrawReasons::default()
            },
            NativeRedrawReasons {
                session: true,
                ..NativeRedrawReasons::default()
            },
            NativeRedrawReasons {
                revision: true,
                ..NativeRedrawReasons::default()
            },
            NativeRedrawReasons {
                lock_clock: true,
                ..NativeRedrawReasons::default()
            },
        ] {
            assert!(visible_lock_edge.content_changed(true));
        }
        // Phase `an_answered_widget_deadline_does_not_spin_the_loop`.
        {
            let due_at = Instant::now();
            let tick_started = due_at + Duration::from_millis(1);
            let now = tick_started + Duration::from_millis(1);
            let frame = now + Duration::from_millis(8);
            assert_eq!(
                native_loop_park(
                    now,
                    false,
                    false,
                    false,
                    frame,
                    [widget_deadline_for_presenter(
                        true,
                        widget_park_deadline(tick_started, Some(due_at)),
                    )],
                ),
                Some(IDLE_BACKEND_POLL),
                "a deadline this tick already woke for parks the loop instead of spinning it"
            );
            let became_due = tick_started + Duration::from_micros(500);
            assert_eq!(
                widget_park_deadline(tick_started, Some(became_due)),
                Some(became_due),
                "a deadline that fell due during this tick still wakes the next one"
            );
            let future = now + Duration::from_millis(42);
            assert_eq!(
                widget_park_deadline(tick_started, Some(future)),
                Some(future)
            );
        }
        // Phase `lock_osd_fade_repaints_only_while_the_lock_is_visible`.
        {
            let fading = NativeRedrawReasons {
                lock_osd: true,
                ..NativeRedrawReasons::default()
            };
            assert!(fading.content_changed(true));
            assert!(
                !fading.content_changed(false),
                "the unlocked host drives its own OSD frames"
            );
        }
        // Phase `locked_clock_gets_an_explicit_minute_refresh_edge`.
        {
            let start = Instant::now();
            let mut last = start;
            assert!(!lock_clock_refresh_due(true, start, &mut last));
            assert!(!lock_clock_refresh_due(
                true,
                start + Duration::from_millis(59_999),
                &mut last,
            ));
            assert!(lock_clock_refresh_due(
                true,
                start + LOCK_CLOCK_REFRESH,
                &mut last,
            ));
            assert!(!lock_clock_refresh_due(
                true,
                start + LOCK_CLOCK_REFRESH + Duration::from_secs(1),
                &mut last,
            ));

            let unlocked = start + Duration::from_secs(75);
            assert!(!lock_clock_refresh_due(false, unlocked, &mut last));
            assert_eq!(last, unlocked);
        }
        // Phase `force_and_password_feedback_edges_always_repaint_the_lock`.
        for visible in [
            NativeRedrawReasons {
                force: true,
                ..NativeRedrawReasons::default()
            },
            // Lock input and async verification completion both set the
            // session edge, updating the masked field, error, or unlock.
            NativeRedrawReasons {
                session: true,
                ..NativeRedrawReasons::default()
            },
        ] {
            assert!(visible.content_changed(true));
        }
    }

    #[test]
    fn onboarding_and_lock_gate_session_services_and_input() {
        // Phase `deferred_peer_sync_starts_in_the_session_that_finishes_onboarding`.
        {
            let mut deferred = true;
            assert!(!take_deferred_peer_sync_activation(&mut deferred, true));
            assert!(deferred, "onboarding retains the pending start");
            assert!(take_deferred_peer_sync_activation(&mut deferred, false));
            assert!(!deferred, "the current-session start is one-shot");
            assert!(!take_deferred_peer_sync_activation(&mut deferred, false));
        }
        // Phase `unfinished_post_identity_setup_reactivates_trusted_first_use_chrome`.
        assert!(should_activate_first_use(false, true, false, false));
        assert!(
            !should_activate_first_use(false, false, false, false),
            "an established user without a journal must not see first-use again"
        );
        assert!(
            should_activate_first_use(false, false, true, false),
            "an existing identity whose Secret Service key is unavailable must enter recovery"
        );
        assert!(
            !should_activate_first_use(true, true, true, true),
            "the explicit development control session remains isolated"
        );
        // Phase `onboarding_blocks_every_client_input_route`.
        {
            let mut fb = FrameBuilder::new((800, 600));
            assert!(client_input_allowed(&fb));
            fb.enable_test_onboarding(crate::onboarding_view::OnboardingView::isolated(
                std::path::PathBuf::from("/tmp/tilo-onboarding-test"),
                false,
            ));
            assert!(fb.host().onboarding_active());
            assert!(
                !client_input_allowed(&fb),
                "trusted first-login chrome must own input before any Wayland client"
            );
        }
        // Phase `portal_screencast_never_publishes_a_locked_frame`.
        assert!(portal_screencast_should_publish(true, false));
        assert!(!portal_screencast_should_publish(true, true));
        assert!(!portal_screencast_should_publish(false, false));
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn output_membership_waits_for_submitted_frames_and_relayout() {
        // Phase `output_presentation_removal_stays_pending_until_a_full_frame_submits`.
        {
            let mut pending = false;
            let mut previous_surface_count = 1;

            assert!(update_output_presentation_repaint_pending(
                &mut pending,
                &mut previous_surface_count,
                0,
                false,
            ));
            assert_eq!(previous_surface_count, 0);
            assert!(
                pending,
                "an off-cadence removal must survive until the next submitted frame"
            );

            pending = false;
            assert!(!update_output_presentation_repaint_pending(
                &mut pending,
                &mut previous_surface_count,
                0,
                false,
            ));
            assert!(update_output_presentation_repaint_pending(
                &mut pending,
                &mut previous_surface_count,
                0,
                true,
            ));
        }
        // Phase `switching_tabs_leaves_the_hidden_surface_and_enters_the_visible_one`.
        {
            use crate::compositor::tab_binding::{self, Placement};

            const FIRST_SURFACE: u64 = 71_001;
            const SECOND_SURFACE: u64 = 71_002;

            let mut fb = FrameBuilder::new((1600, 900));
            let _ = fb.paint_settled();

            let mut ids = None;
            fb.with_chrome(|chrome| {
                let mut tree = chrome.tree_mut();
                let first = tab_binding::adopt_toplevel_at(
                    &mut tree,
                    FIRST_SURFACE,
                    Some("org.example.first".into()),
                    Placement::NewGroup,
                );
                let (strip, group) = tree
                    .desktop
                    .active_workspace()
                    .and_then(|workspace| {
                        workspace.strips.iter().find_map(|strip| {
                            strip
                                .window_groups
                                .iter()
                                .find(|group| group.tabs.iter().any(|tab| tab.id == first))
                                .map(|group| (strip.id, group.id))
                        })
                    })
                    .expect("the first foreign tab has a group");
                let second = tab_binding::adopt_toplevel_at(
                    &mut tree,
                    SECOND_SURFACE,
                    Some("org.example.second".into()),
                    Placement::TabIn(group),
                );
                ids = Some((strip, group, first, second));
                drop(tree);
                chrome.bump();
            });
            let (strip, group, first, _second) = ids.expect("fixture ids");
            let _ = fb.paint_settled();

            let output = Output {
                id: OutputId(0),
                position: (0, 0),
                size: (1600, 900),
                scale: 1.0,
                transform: settings_store::hardware::OutputTransform::Normal,
                color_profile: None,
                refresh_mhz: None,
                logical_position: (0, 0),
                identity: crate::compositor::backend::OutputIdentity::default(),
            };
            let mut memberships = SurfaceOutputMemberships::default();
            assert_eq!(
                visible_tiled_surface_rects(&fb)
                    .into_iter()
                    .map(|(surface, _)| surface)
                    .collect::<Vec<_>>(),
                vec![SECOND_SURFACE],
                "only the active tab is painted"
            );
            assert_eq!(
                memberships.reconcile(
                    std::slice::from_ref(&output),
                    &visible_tiled_surface_rects(&fb)
                ),
                vec![SurfaceOutputChange::Enter {
                    surface_id: SECOND_SURFACE,
                    output: OutputId(0),
                }]
            );

            assert!(
                fb.host_mut().focus_window_on_strip(strip, group, first),
                "the existing tab can be focused through the production focus path"
            );
            let _ = fb.paint_settled();
            assert_eq!(
                visible_tiled_surface_rects(&fb)
                    .into_iter()
                    .map(|(surface, _)| surface)
                    .collect::<Vec<_>>(),
                vec![FIRST_SURFACE]
            );
            let changes = memberships.reconcile(&[output], &visible_tiled_surface_rects(&fb));
            assert_eq!(changes.len(), 2);
            assert!(changes.contains(&SurfaceOutputChange::Leave {
                surface_id: SECOND_SURFACE,
                output: OutputId(0),
            }));
            assert!(changes.contains(&SurfaceOutputChange::Enter {
                surface_id: FIRST_SURFACE,
                output: OutputId(0),
            }));
        }
        // Phase `changed_output_geometry_waits_for_relayout_before_membership_diff`.
        {
            let output = |id, x| Output {
                id: OutputId(id),
                position: (x, 0),
                size: (100, 100),
                scale: 1.0,
                transform: settings_store::hardware::OutputTransform::Normal,
                color_profile: None,
                refresh_mhz: None,
                logical_position: (x, 0),
                identity: crate::compositor::backend::OutputIdentity::default(),
            };
            let old_outputs = [output(0, 0), output(1, 100)];
            let moved_outputs = [output(0, 100), output(1, 0)];
            let old_rect = Rect::new(10.0, 10.0, 60.0, 60.0);
            let relaid_rect = Rect::new(110.0, 10.0, 160.0, 60.0);
            let mut memberships = SurfaceOutputMemberships::default();
            memberships.reconcile(&old_outputs, &[(7, old_rect)]);

            let mut counterexample = SurfaceOutputMemberships::default();
            counterexample.reconcile(&old_outputs, &[(7, old_rect)]);
            assert_eq!(
                counterexample.reconcile(&moved_outputs, &[(7, old_rect)]),
                vec![
                    SurfaceOutputChange::Leave {
                        surface_id: 7,
                        output: OutputId(0),
                    },
                    SurfaceOutputChange::Enter {
                        surface_id: 7,
                        output: OutputId(1),
                    },
                ],
                "old tile coordinates compared with new outputs create false wire transitions"
            );

            assert!(
                !surface_output_geometry_is_current(true),
                "the pre-layout reconciliation must be skipped after an output change"
            );
            assert!(
                memberships
                    .reconcile(&moved_outputs, &[(7, relaid_rect)])
                    .is_empty(),
                "post-layout geometry keeps the surface on the same physical output"
            );
        }
    }

    #[test]
    fn agent_control_binds_only_for_an_agent_control_build_on_the_headless_backend() {
        let raw = || Some(std::ffi::OsString::from("/run/tilo/agent.sock"));
        assert_eq!(agent_control_path(false, true, raw()), None);
        assert_eq!(agent_control_path(true, false, raw()), None);
        assert_eq!(
            agent_control_path(true, true, raw()),
            Some(PathBuf::from("/run/tilo/agent.sock"))
        );
        assert_eq!(agent_control_path(true, true, None), None);
        assert_eq!(
            agent_control_path(true, true, Some("relative.sock".into())),
            None
        );
    }

    #[test]
    fn shared_legacy_windows_are_encoded_only_while_watched() {
        use crate::shell::state::{Tab, TileContent, WindowGroup, WindowWidth, WorkspaceTree};
        let mut tree = WorkspaceTree::bootstrap_default();
        let workspace = &mut tree.desktop.workspaces[0];
        workspace.strips[0].window_groups.push(WindowGroup {
            id: 11,
            tabs: vec![Tab {
                id: 12,
                title: "GIMP".into(),
                tile: TileContent::Foreign {
                    app_id: "org.gimp.GIMP".into(),
                    surface_id: 7,
                },
                app_title: None,
            }],
            active_tab: 12,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
        workspace.strips[0].window_groups.push(WindowGroup {
            id: 13,
            tabs: vec![Tab {
                id: 14,
                title: "Chromium".into(),
                tile: TileContent::Foreign {
                    app_id: "org.chromium.Chromium".into(),
                    surface_id: 8,
                },
                app_title: None,
            }],
            active_tab: 14,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
        workspace.shared_surfaces.share(7, 9);
        let room = workspace.room();
        let none = RoomHosts::new();
        let nobody = std::collections::BTreeMap::new();
        let share = |app_id: &str, tab: u64, viewers: &[u64], paint: bool| {
            crate::compositor::state::LegacyShare {
                app_id: app_id.to_string(),
                tab,
                viewers: viewers.iter().copied().collect(),
                read_only: std::collections::HashSet::new(),
                decode_limits: HashMap::new(),
                paint,
            }
        };

        let watched = desired_legacy_shares(&tree, true, &none, &nobody, &MemberViewers::new());
        assert_eq!(
            watched.get(&7).map(|(share, _)| share),
            Some(&share("org.gimp.GIMP", 12, &[9], false))
        );
        assert!(
            !watched.contains_key(&8),
            "without a viewer host only explicit shares stream"
        );
        let hosts: RoomHosts = [(room, [42].into_iter().collect())].into_iter().collect();
        let through_host =
            desired_legacy_shares(&tree, true, &hosts, &nobody, &MemberViewers::new());
        assert_eq!(
            through_host.get(&7).map(|(share, _)| share),
            Some(&share("org.gimp.GIMP", 12, &[9, 42], false))
        );
        assert_eq!(
            through_host.get(&8).map(|(share, _)| share),
            Some(&share("org.chromium.Chromium", 14, &[42], true)),
            "a viewer host receives every legacy window of the workspace whose room it hosts"
        );
        let elsewhere: RoomHosts = [(
            tilo_desktop_model::RoomId::new(),
            [43].into_iter().collect(),
        )]
        .into_iter()
        .collect();
        assert!(
            !desired_legacy_shares(&tree, true, &elsewhere, &nobody, &MemberViewers::new())
                .contains_key(&8),
            "another room's host receives none of this room's windows"
        );
        assert!(
            desired_legacy_shares(&tree, false, &hosts, &nobody, &MemberViewers::new()).is_empty(),
            "unwatched, every encoder stops"
        );
    }

    #[test]
    fn legacy_shares_are_derived_again_only_after_an_input_changed() {
        use crate::shell::device_fabric_runtime::WindowViewer;
        use crate::shell::state::{Tab, TileContent, WindowGroup, WindowWidth, WorkspaceTree};
        let mut tree = WorkspaceTree::bootstrap_default();
        tree.desktop.workspaces[0].strips[0]
            .window_groups
            .push(WindowGroup {
                id: 11,
                tabs: vec![Tab {
                    id: 12,
                    title: "GIMP".into(),
                    tile: TileContent::Foreign {
                        app_id: "org.gimp.GIMP".into(),
                        surface_id: 7,
                    },
                    app_title: None,
                }],
                active_tab: 12,
                width: WindowWidth::Normal,
                maximized: false,
                focus_history: Vec::new(),
            });
        tree.desktop.workspaces[0].shared_surfaces.share(7, 9);
        let tree = tree.shared();
        let nobody = || {
            (
                RoomHosts::new(),
                std::collections::BTreeMap::new(),
                MemberViewers::new(),
            )
        };
        let viewers_of = |plan: &LegacySharePlan| {
            plan.shares.get(&7).map(|share| {
                share
                    .viewers
                    .iter()
                    .copied()
                    .collect::<std::collections::BTreeSet<_>>()
            })
        };
        let mut plan = LegacySharePlan::default();
        let mut asked = 0;

        // Phase `idle_wakeups_walk_nothing`.
        assert!(plan.refresh(&tree, nobody(), true, |_| {
            asked += 1;
            true
        }));
        assert_eq!(viewers_of(&plan), Some([9].into()));
        for _ in 0..100 {
            assert!(!plan.refresh(&tree, nobody(), true, |_| {
                asked += 1;
                true
            }));
        }
        assert_eq!(
            (plan.derivations, asked),
            (1, 1),
            "a hundred wakeups without a change walk the tabs once"
        );

        // Phase `a_tree_write_is_derived_on_the_next_wakeup`.
        tree.write().desktop.workspaces[0]
            .shared_surfaces
            .share(7, 10);
        assert!(plan.refresh(&tree, nobody(), true, |_| true));
        assert_eq!(viewers_of(&plan), Some([9, 10].into()));

        // Phase `a_new_viewer_is_derived_on_the_next_wakeup`.
        let personal = std::collections::BTreeMap::from([(
            12,
            vec![WindowViewer {
                device: crate::shell::personal_continuity::DeviceId::new(hex::encode([1; 32]))
                    .unwrap(),
                peer: 501,
                width: 800,
                height: 600,
                decode_limits: std::collections::BTreeMap::new(),
            }],
        )]);
        let with_device = || (RoomHosts::new(), personal.clone(), MemberViewers::new());
        assert!(plan.refresh(&tree, with_device(), true, |for_host| {
            assert!(
                for_host,
                "a device showing the window wants the backend watched"
            );
            true
        }));
        assert_eq!(viewers_of(&plan), Some([9, 10, 501].into()));
        assert_eq!(
            plan.viewers().map(|(_, personal, _)| personal.len()),
            Some(1),
            "what the shares feed is told the viewers they were derived for"
        );
        assert!(!plan.refresh(&tree, with_device(), true, |_| true));

        // Phase `the_backend_going_unwatched_is_derived_on_the_next_wakeup`.
        assert!(plan.refresh(&tree, nobody(), false, |_| false));
        assert!(plan.shares.is_empty() && plan.desired.is_empty());
        assert!(!plan.refresh(&tree, nobody(), false, |_| false));
        assert_eq!(plan.derivations, 4);
    }

    /// A desktop of 8 workspaces with 24 legacy windows each, every one
    /// shared, and none viewed.
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "small fixture ids derived from loop indices"
    )]
    fn eight_busy_workspaces() -> crate::shell::state::WorkspaceTree {
        use crate::shell::state::{
            RoomId, Tab, TileContent, WindowGroup, WindowWidth, WorkspaceId, WorkspaceTree,
        };
        let mut tree = WorkspaceTree::bootstrap_default();
        let template = tree.desktop.workspaces[0].clone();
        tree.desktop.workspaces.clear();
        for desk in 0..8_u64 {
            let mut workspace = crate::shell::state::rehomed(&template, RoomId::new());
            workspace.id = WorkspaceId(100 + desk);
            workspace.strips.truncate(1);
            workspace.strips[0].window_groups = (0..24_u64)
                .map(|window| {
                    let id = desk * 1_000 + window * 10;
                    WindowGroup {
                        id,
                        tabs: vec![Tab {
                            id: id + 1,
                            title: "Terminal".into(),
                            tile: TileContent::Foreign {
                                app_id: "org.gnome.Ptyxis".into(),
                                surface_id: id + 2,
                            },
                            app_title: None,
                        }],
                        active_tab: id + 1,
                        width: WindowWidth::Normal,
                        maximized: false,
                        focus_history: Vec::new(),
                    }
                })
                .collect();
            workspace.shared_surfaces.share(desk * 1_000 + 2, 9);
            tree.desktop.workspaces.push(workspace);
        }
        tree.desktop.active = WorkspaceId(100);
        tree
    }

    /// An idle wakeup costs the legacy-share pass a comparison, not a walk of
    /// every tab of every workspace: the shares are derived once, and no
    /// wakeup after that derives them again.
    #[test]
    fn an_idle_wakeup_does_not_derive_the_legacy_shares_again() {
        let tree = eight_busy_workspaces().shared();
        let nobody = RoomHosts::new();
        let no_device = std::collections::BTreeMap::new();
        let mut plan = LegacySharePlan::default();
        assert!(plan.refresh(
            &tree,
            (nobody.clone(), no_device.clone(), MemberViewers::new()),
            true,
            |_| true
        ));
        for _ in 0..2_000 {
            assert!(!plan.refresh(
                &tree,
                (nobody.clone(), no_device.clone(), MemberViewers::new()),
                true,
                |_| true
            ));
        }
        assert_eq!(plan.derivations, 1);
    }

    /// What an idle wakeup cost the legacy-share pass before (walking every
    /// tab of every workspace) against now, on a desktop of 8 workspaces
    /// with 24 windows each.
    #[test]
    #[ignore = "measurement; run with --ignored --nocapture"]
    fn idle_wakeup_legacy_share_pass_timing() {
        const WAKEUPS: u32 = 2_000;
        let tree = eight_busy_workspaces();
        let nobody = RoomHosts::new();
        let no_device = std::collections::BTreeMap::new();

        let started = Instant::now();
        for _ in 0..WAKEUPS {
            assert!(!hosts_viewed_window(
                &tree,
                &no_device,
                &MemberViewers::new()
            ));
            assert_eq!(
                desired_legacy_shares(&tree, true, &nobody, &no_device, &MemberViewers::new())
                    .len(),
                1
            );
        }
        let walked = started.elapsed();
        let tree = tree.shared();
        let mut plan = LegacySharePlan::default();
        assert!(plan.refresh(
            &tree,
            (nobody.clone(), no_device.clone(), MemberViewers::new()),
            true,
            |_| true
        ));
        let started = Instant::now();
        for _ in 0..WAKEUPS {
            assert!(!plan.refresh(
                &tree,
                (nobody.clone(), no_device.clone(), MemberViewers::new()),
                true,
                |_| true
            ));
        }
        let gated = started.elapsed();
        eprintln!(
            "legacy-share pass per idle wakeup, 8 workspaces x 24 windows: walked {:?}, gated {:?}",
            walked / WAKEUPS,
            gated / WAKEUPS,
        );
    }

    #[test]
    fn a_legacy_window_streams_to_each_own_device_showing_its_tab() {
        use crate::shell::device_fabric_runtime::WindowViewer;
        use crate::shell::state::{
            RoomId, Tab, TileContent, WindowGroup, WindowWidth, WorkspaceId, WorkspaceTree,
        };
        let mut tree = WorkspaceTree::bootstrap_default();
        // A second workspace this device is not showing.
        let mut background =
            crate::shell::state::rehomed(&tree.desktop.workspaces[0], RoomId::new());
        background.id = WorkspaceId(90);
        background.strips.truncate(1);
        background.strips[0].window_groups = vec![WindowGroup {
            id: 91,
            tabs: vec![
                Tab {
                    id: 92,
                    title: "Terminal".into(),
                    tile: TileContent::Foreign {
                        app_id: "org.gnome.Ptyxis".into(),
                        surface_id: 30,
                    },
                    app_title: None,
                },
                Tab {
                    id: 93,
                    title: "Terminal".into(),
                    tile: TileContent::Foreign {
                        app_id: "org.gnome.Ptyxis".into(),
                        surface_id: 31,
                    },
                    app_title: None,
                },
            ],
            active_tab: 92,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }];
        let room = background.room();
        tree.desktop.workspaces.push(background);
        let viewer = |device: u8, peer: u64, width: u32, height: u32| WindowViewer {
            device: crate::shell::personal_continuity::DeviceId::new(hex::encode([device; 32]))
                .unwrap(),
            peer,
            width,
            height,
            decode_limits: std::collections::BTreeMap::from([(
                media::StreamCodec::H264,
                (1920, 1088),
            )]),
        };
        let personal = std::collections::BTreeMap::from([
            (
                93,
                vec![viewer(1, 501, 1200, 800), viewer(2, 502, 640, 480)],
            ),
            // A tab this session does not host is somebody else's to stream.
            (777, vec![viewer(1, 501, 300, 300)]),
        ]);
        let none = RoomHosts::new();

        let desired = desired_legacy_shares(&tree, true, &none, &personal, &MemberViewers::new());
        assert_eq!(
            desired.len(),
            1,
            "only the shown window streams, not its sibling tab"
        );
        let (share, share_room) = &desired[&31];
        assert_eq!(share.tab, 93);
        assert_eq!(share.viewers, [501, 502].into_iter().collect());
        assert_eq!(
            share.decode_limits[&502],
            std::collections::BTreeMap::from([(media::StreamCodec::H264, (1920, 1088))]),
            "each viewer's decoder limit reaches the encoder"
        );
        assert_eq!(*share_room, room, "the stream goes to the tab's own room");
        assert!(hosts_viewed_window(&tree, &personal, &MemberViewers::new()));
        assert_eq!(
            remote_extents(&desired, &personal, &MemberViewers::new()).get(&31),
            Some(&(1200, 800)),
            "a hidden window is sized for its largest viewer"
        );
        assert!(
            desired_legacy_shares(&tree, false, &none, &personal, &MemberViewers::new()).is_empty()
        );
    }

    #[test]
    fn a_device_reports_the_hosted_windows_it_lays_out_to_their_hosts() {
        use crate::shell::state::{Tab, TileContent, WindowGroup, WindowWidth, WorkspaceTree};
        let desk = "ab".repeat(32);
        let mut tree = WorkspaceTree::bootstrap_default();
        tree.desktop.workspaces[0].strips[0]
            .window_groups
            .push(WindowGroup {
                id: 61,
                tabs: vec![
                    Tab {
                        id: 62,
                        title: "Inkscape".into(),
                        tile: TileContent::HostedWindow {
                            app_id: "org.inkscape.Inkscape".into(),
                            label: "Poster.svg".into(),
                            host: desk.clone(),
                            host_label: "Desk".into(),
                        },
                        app_title: None,
                    },
                    Tab {
                        id: 63,
                        title: "GIMP".into(),
                        tile: TileContent::Foreign {
                            app_id: "org.gimp.GIMP".into(),
                            surface_id: 4,
                        },
                        app_title: None,
                    },
                    Tab {
                        id: 65,
                        title: "Notes".into(),
                        tile: TileContent::PeerWindow {
                            app_id: "so.tilo.notes".into(),
                            label: "Notes".into(),
                            note: String::new(),
                            missing: None,
                        },
                        app_title: None,
                    },
                ],
                active_tab: 62,
                width: WindowWidth::Normal,
                maximized: false,
                focus_history: Vec::new(),
            });
        // This device's own window and a Tilo app's tab stream from nowhere.
        let tiles = [
            (62, vello::kurbo::Rect::new(10.0, 20.0, 810.4, 620.0)),
            (63, vello::kurbo::Rect::new(0.0, 0.0, 100.0, 100.0)),
            (65, vello::kurbo::Rect::new(0.0, 0.0, 100.0, 100.0)),
        ];
        assert_eq!(
            shown_hosted_windows(&tree, &tiles),
            vec![(
                desk,
                crate::shell::device_fabric::WindowView {
                    tab: 62,
                    width: 800,
                    height: 600,
                }
            )]
        );
    }

    #[test]
    fn the_agent_control_socket_serves_input_screenshots_and_the_watch_signal_only() {
        use test_control::Command;
        for permitted in [
            r#"{"op":"click_at","x":1,"y":2}"#,
            r#"{"op":"type","text":"hi"}"#,
            r#"{"op":"key_down","key":"Control"}"#,
            r#"{"op":"snapshot","path":"/tmp/x.png"}"#,
            r#"{"op":"pointer_button","pressed":true,"button":"right"}"#,
            r#"{"op":"clipboard_paste"}"#,
            r#"{"op":"watch","watching":true}"#,
            r#"{"op":"watch","watching":false}"#,
        ] {
            let command: Command = serde_json::from_str(permitted).unwrap();
            assert!(command.agent_permitted(), "{permitted}");
        }
        for refused in [
            r#"{"op":"grant","subject":"agent:x","scope":"workspace","access":"write"}"#,
            r#"{"op":"revoke","id":"x"}"#,
            r#"{"op":"state_dump"}"#,
            r#"{"op":"actions_invoke","provider":"actions","id":"x"}"#,
            r#"{"op":"mint_host_launch_activation","desktop_id":"x","display_name":"x"}"#,
            r#"{"op":"seed_files_drag","paths":[]}"#,
        ] {
            let command: Command = serde_json::from_str(refused).unwrap();
            assert!(!command.agent_permitted(), "{refused}");
        }
    }

    // developer surfaces are absent from a shipped build

    #[test]
    fn test_control_surfaces_exist_only_in_dev_builds() {
        // Phase `shipped_build_never_binds_a_test_control_socket`.
        assert_eq!(
            test_control_path(false, Some(std::ffi::OsString::from("/tmp/ctl.sock"))),
            None,
            "a release compositor must not open a remote-control socket"
        );
        // Phase `dev_build_binds_the_test_control_socket_the_harness_asks_for`.
        assert_eq!(
            test_control_path(true, Some(std::ffi::OsString::from("/tmp/ctl.sock"))),
            Some(PathBuf::from("/tmp/ctl.sock")),
        );
        assert_eq!(test_control_path(true, None), None);
        // Phase `shipped_build_never_dumps_its_workspace_tree`.
        assert_eq!(
            test_state_dump_path(false, Some(std::ffi::OsString::from("/tmp/tree.json"))),
            None,
        );
        assert_eq!(
            test_state_dump_path(true, Some(std::ffi::OsString::from("/tmp/tree.json"))),
            Some(std::ffi::OsString::from("/tmp/tree.json")),
        );
        // Phase `the_gate_the_compositor_actually_uses_is_the_build_profile`.
        // The two functions above are only meaningful if `run` passes the real
        // constant — tied here so flipping the production call site to a literal `true` fails.
        assert_eq!(
            crate::compositor::dev_env::DEV_ENV_OVERRIDES,
            cfg!(debug_assertions) || cfg!(feature = "dev-overrides")
        );
    }

    pub(super) fn socket_response(reader: &mut BufReader<UnixStream>) -> test_control::Response {
        use std::io::BufRead as _;
        let mut line = String::new();
        reader.read_line(&mut line).expect("read response frame");
        assert!(
            line.ends_with('\n'),
            "every response remains newline framed"
        );
        serde_json::from_str(line.trim_end()).expect("valid response JSON")
    }

    fn assert_socket_pending(reader: &mut BufReader<UnixStream>) {
        use std::io::BufRead as _;
        match reader.fill_buf() {
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Ok([]) => panic!("socket disconnected while admission pending"),
            Ok(bytes) => panic!(
                "admission produced an early response: {}",
                String::from_utf8_lossy(bytes)
            ),
            Err(error) => panic!("checking pending admission response: {error}"),
        }
    }

    fn directory_test_tree() -> crate::shell::state::WorkspaceTreeHandle {
        let mut tree = crate::shell::state::WorkspaceTree::default();
        let mut workspace = crate::shell::state::Workspace::new(
            crate::shell::state::WorkspaceId(71),
            "Directory socket room",
        );
        workspace.set_owner(crate::shell::state::local_user_id_for_room(
            &workspace.room(),
        ));
        tree.desktop.workspaces.push(workspace);
        tree.set_active(crate::shell::state::WorkspaceId(71));
        tree.shared()
    }

    fn attach_directory_fixture(
        frame_builder: &mut FrameBuilder,
        directory: &crate::shell::collab_broker_capnp::tests::LocalDirectoryFixture,
    ) {
        attach_fixture_broker(frame_builder, &directory.broker);
    }

    fn attach_fixture_broker(
        frame_builder: &mut FrameBuilder,
        broker: &crate::shell::collab_broker::AppActionBroker,
    ) {
        frame_builder
            .host_mut()
            .attach_app_action_broker(broker.clone());
        // This fixture has a test-owned room authority, not a signed desktop
        // session. Bind the search caller to that same explicit fixture; never
        // let a socket test depend on the machine's selected identity.
        crate::shell::overlay_host::install_fixture_search_provider(
            frame_builder.host_mut(),
            std::rc::Rc::new(
                crate::shell::search::providers::LiveAppActionsProvider::new_with_fixture_identity(
                    broker.clone(),
                ),
            ),
        );
    }

    /// [`pump_pending_test_control_conn`] as the loop calls it.
    fn pump_pending(
        conn: &mut TestControlConn,
        frame_builder: &FrameBuilder,
        ctx: &mut TestControlCtx,
    ) -> PendingConnectionProgress {
        pump_pending_test_control_conn(
            conn,
            &mut FrameInvocationProgress {
                fb: frame_builder,
                ctx,
            },
        )
    }

    /// One control client: the compositor's end, and the client's writer
    /// and non-blocking reader.
    fn control_client() -> (TestControlConn, UnixStream, BufReader<UnixStream>) {
        let (server, client) = UnixStream::pair().expect("socket pair");
        let writer = client.try_clone().expect("client writer");
        let reader = BufReader::new(client);
        reader
            .get_ref()
            .set_nonblocking(true)
            .expect("nonblocking client");
        let conn = TestControlConn::new(server, false).expect("server connection");
        (conn, writer, reader)
    }

    /// A wake that pings `event_loop`, as the session's own wake does, and
    /// whether that ping's source has run since.
    fn loop_wake(
        event_loop: &EventLoop<'static, State>,
    ) -> (
        crate::session_runtime::Wake,
        std::rc::Rc<std::cell::Cell<bool>>,
    ) {
        let (ping, source) = calloop::ping::make_ping().expect("ping");
        let woken = std::rc::Rc::new(std::cell::Cell::new(false));
        let seen = std::rc::Rc::clone(&woken);
        event_loop
            .handle()
            .insert_source(source, move |(), (), _| seen.set(true))
            .map_err(|error| error.to_string())
            .expect("ping source");
        let wake: crate::session_runtime::Wake = Arc::new(move || ping.ping());
        (wake, woken)
    }

    /// Park `event_loop` as the compositor does, with no deadline but
    /// `within`, until the wake from [`loop_wake`] ends a park.
    fn park_until_woken(
        event_loop: &mut EventLoop<'static, State>,
        state: &mut State,
        woken: &std::cell::Cell<bool>,
        within: Duration,
    ) -> bool {
        let deadline = Instant::now().checked_add(within).expect("deadline");
        while !woken.get() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            event_loop.dispatch(Some(left), state).expect("dispatch");
        }
        true
    }

    const PROBE_ID: &str = "probe.run";

    /// A provider whose one action answers `receipt`, counting how often its
    /// address is resolved and how often the action runs.
    struct CountingProvider {
        resolves: std::rc::Rc<std::cell::Cell<u32>>,
        runs: std::rc::Rc<std::cell::Cell<u32>>,
        receipt: crate::shell::search::ActionReceipt,
    }

    impl CountingProvider {
        fn row(&self) -> crate::shell::search::SearchResult {
            let runs = std::rc::Rc::clone(&self.runs);
            let receipt = self.receipt.clone();
            let mut row = crate::shell::search::SearchResult::view_with_output(
                crate::shell::search::ResourceKind::Note,
                "probe",
                PROBE_ID,
                "Probe",
                None,
                0,
                |_| Ok(serde_json::Value::Null),
            );
            row.on_invoke = Some(std::rc::Rc::new(move |_, _, _, _| {
                runs.set(runs.get().saturating_add(1));
                Ok(receipt.clone())
            }));
            row
        }
    }

    impl crate::shell::search::SearchProvider for CountingProvider {
        fn id(&self) -> &'static str {
            "probe"
        }

        fn name(&self) -> &'static str {
            "Probe"
        }

        fn resource_kinds(&self) -> &[crate::shell::search::ResourceKind] {
            &[crate::shell::search::ResourceKind::Note]
        }

        fn query(
            &self,
            _ctx: crate::shell::search::SearchContext<'_>,
        ) -> Vec<crate::shell::search::SearchResult> {
            vec![self.row()]
        }

        fn resolve(
            &self,
            id: &str,
            _ctx: crate::shell::search::SearchContext<'_>,
        ) -> Option<crate::shell::search::SearchResult> {
            self.resolves.set(self.resolves.get().saturating_add(1));
            (id == PROBE_ID).then(|| self.row())
        }
    }

    /// The headless pieces one control request runs against, dropped in
    /// the reverse of the order a test would declare them.
    struct ControlFixture {
        state: State,
        _display: Display<State>,
        event_loop: EventLoop<'static, State>,
        lock: LockController,
        session: SessionManager,
        ctx: TestControlCtx,
        frame_builder: FrameBuilder,
    }

    impl ControlFixture {
        fn new() -> Self {
            let frame_builder = FrameBuilder::with_handle(directory_test_tree(), false, (800, 600));
            let event_loop: EventLoop<'static, State> = EventLoop::try_new().expect("calloop");
            let display: Display<State> = Display::new().expect("display");
            let state = State::new(event_loop.handle(), display.handle());
            Self {
                state,
                _display: display,
                event_loop,
                lock: LockController::new(lock_auth::verifier_from_env(), "fixture"),
                session: SessionManager::new(IdleConfig::from_env(), Instant::now()),
                ctx: TestControlCtx::new(),
                frame_builder,
            }
        }

        fn pump(&mut self, conn: &mut TestControlConn) {
            pump_test_control_conn(
                conn,
                &mut self.frame_builder,
                &mut self.ctx,
                &self.session,
                &mut self.lock,
                &mut self.state,
            );
        }

        fn assert_waiting_after_wake(
            &mut self,
            conn: &mut TestControlConn,
            reader: &mut BufReader<UnixStream>,
            settle_by: Instant,
        ) {
            self.event_loop
                .dispatch(Some(Duration::ZERO), &mut self.state)
                .expect("dispatch");
            self.pump(conn);
            assert_socket_pending(reader);
            assert!(!conn.closed, "the original connection must remain open");
            assert_eq!(
                control_invocation_deadline(std::slice::from_ref(conn), Instant::now()),
                Some(settle_by),
                "an early wake must preserve the original settle deadline"
            );
        }

        fn wait_for_invoke_response(
            &mut self,
            conn: &mut TestControlConn,
            woken: &std::cell::Cell<bool>,
            deadline: Instant,
        ) {
            loop {
                assert!(
                    Instant::now() < deadline,
                    "settled response must be observed before the original deadline"
                );
                self.pump(conn);
                assert!(!conn.closed, "the original connection must remain open");
                if conn.pending_invocation.is_none() {
                    assert!(
                        conn.output.is_empty(),
                        "the settled response must be flushed"
                    );
                    return;
                }
                // A shared loop ping can arrive before this job's watcher.
                // Keep serving the same request as the compositor does.
                woken.set(false);
                assert!(
                    park_until_woken(
                        &mut self.event_loop,
                        &mut self.state,
                        woken,
                        deadline.saturating_duration_since(Instant::now()),
                    ),
                    "the pending invocation must wake the loop before its deadline"
                );
            }
        }
    }

    #[test]
    fn an_invocation_is_resolved_once_per_request() {
        use std::io::Write as _;

        let mut fixture = ControlFixture::new();
        let resolves = std::rc::Rc::new(std::cell::Cell::new(0));
        let runs = std::rc::Rc::new(std::cell::Cell::new(0));
        crate::shell::overlay_host::install_fixture_search_provider(
            fixture.frame_builder.host_mut(),
            std::rc::Rc::new(CountingProvider {
                resolves: std::rc::Rc::clone(&resolves),
                runs: std::rc::Rc::clone(&runs),
                receipt: crate::shell::search::ActionReceipt::completed(),
            }),
        );
        let (mut conn, mut writer, mut reader) = control_client();
        let invoke =
            serde_json::json!({"op": "actions_invoke", "provider": "probe", "id": PROBE_ID});
        for request in 1..=2 {
            writer
                .write_all(format!("{invoke}\n").as_bytes())
                .expect("invoke");
            fixture.pump(&mut conn);
            let test_control::Response::Answer(answer) = socket_response(&mut reader) else {
                panic!("the probe action runs");
            };
            assert_eq!(answer["action"]["id"], PROBE_ID);
            assert_eq!(answer["receipt"]["update"]["state"], "succeeded");
            assert_eq!(runs.get(), request, "each request runs the action once");
            assert_eq!(
                resolves.get(),
                request,
                "dispatch resolves the address and the execution runs from that resolution; \
                 a later request resolves afresh"
            );
        }

        // A refused address plans nothing and leaves nothing behind for the
        // next request to run.
        writer
            .write_all(b"{\"op\":\"actions_invoke\",\"provider\":\"probe\",\"id\":\"nope\"}\n")
            .expect("unknown address");
        fixture.pump(&mut conn);
        assert!(matches!(
            socket_response(&mut reader),
            test_control::Response::Err { ref code, .. } if code == "unknown_action"
        ));
        assert_eq!(runs.get(), 2);
    }

    // Hold only this test process's session workers so an unrelated loop ping
    // deterministically precedes the real broker-completion watcher. Drop
    // releases every worker even if an assertion fails.
    struct HeldSessionWorkers(Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>);

    impl Drop for HeldSessionWorkers {
        fn drop(&mut self) {
            *self.0.0.lock_recover() = true;
            self.0.1.notify_all();
        }
    }

    fn hold_session_workers() -> HeldSessionWorkers {
        let runtime = crate::session_runtime::runtime().expect("session runtime");
        let count = runtime.metrics().num_workers();
        let held = HeldSessionWorkers(Arc::new((
            std::sync::Mutex::new(false),
            std::sync::Condvar::new(),
        )));
        let (ready, started) = std::sync::mpsc::channel();
        for _ in 0..count {
            let gate = Arc::clone(&held.0);
            let ready = ready.clone();
            drop(runtime.spawn(async move {
                ready.send(()).expect("worker ready");
                let released = gate.0.lock_recover();
                drop(
                    gate.1
                        .wait_while(released, |released| !*released)
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                );
            }));
        }
        for _ in 0..count {
            tilo_test_env::recv("each session worker to be held", &started);
        }
        held
    }

    #[test]
    fn wait_ms_answers_the_settled_outcome_in_one_call() {
        wait_ms_answer_with_loop_wake(false, false);
    }

    #[test]
    fn wait_ms_tolerates_an_unrelated_wake_before_the_completion_watch() {
        wait_ms_answer_with_loop_wake(true, false);
    }

    #[test]
    fn wait_ms_tolerates_the_status_index_wake_before_job_progress() {
        wait_ms_answer_with_loop_wake(false, true);
    }

    fn wait_ms_answer_with_loop_wake(unrelated_wake: bool, early_index_wake: bool) {
        use std::io::Write as _;

        let mut fixture = ControlFixture::new();
        let broker = crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue();
        attach_fixture_broker(&mut fixture.frame_builder, &broker);
        let actor = fixture.frame_builder.chrome().active_local_user();
        let job = 41;
        let invocation = tilo_actions::InvocationId::new();
        let accepted = tilo_actions::InvocationUpdate::Accepted { invocation };
        broker.record_completion_for_test(job, &actor, accepted.clone());
        let resolves = std::rc::Rc::new(std::cell::Cell::new(0));
        let runs = std::rc::Rc::new(std::cell::Cell::new(0));
        crate::shell::overlay_host::install_fixture_search_provider(
            fixture.frame_builder.host_mut(),
            std::rc::Rc::new(CountingProvider {
                resolves: std::rc::Rc::clone(&resolves),
                runs: std::rc::Rc::clone(&runs),
                receipt: crate::shell::search::ActionReceipt {
                    broker_invocation_id: Some(job),
                    update: accepted.clone(),
                },
            }),
        );
        let (wake, woken) = loop_wake(&fixture.event_loop);
        let _installed = crate::shell::search::invocation_wake::install(wake.clone());
        let (mut conn, mut writer, mut reader) = control_client();

        let invoke = serde_json::json!({
            "op": "actions_invoke", "provider": "probe", "id": PROBE_ID, "wait_ms": 20_000,
        });
        writer
            .write_all(format!("{invoke}\n").as_bytes())
            .expect("invoke");
        let deadline = tilo_test_env::hang_deadline();
        fixture.pump(&mut conn);
        assert_socket_pending(&mut reader);
        let settle_by = control_invocation_deadline(std::slice::from_ref(&conn), Instant::now())
            .expect("the original request has a settle deadline");
        if early_index_wake {
            // Resolving the status starts the production action index worker.
            // Observe its ping before recording any job progress, rather than
            // relying on which thread wins the initial zero-time dispatch.
            assert!(park_until_woken(
                &mut fixture.event_loop,
                &mut fixture.state,
                &woken,
                deadline.saturating_duration_since(Instant::now()),
            ));
        }
        // Index readiness shares the invocation loop ping. A wake is a reason
        // to serve the original request again, not proof that its job moved.
        fixture.assert_waiting_after_wake(&mut conn, &mut reader, settle_by);
        assert_eq!(
            broker.status_for_actor(job, &actor),
            Some(accepted),
            "index readiness must not change the job's accepted receipt"
        );
        woken.set(false);

        let held = unrelated_wake.then(hold_session_workers);
        if held.is_some() {
            wake();
        }
        broker.record_completion_for_test(
            job,
            &actor,
            tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput {
                    value: Some(serde_json::json!("done")),
                    resources: Vec::new(),
                },
            },
        );
        // Completion alone wakes the ordinary case. The delayed-watcher case
        // first receives its unrelated ping and still has no response.
        assert!(park_until_woken(
            &mut fixture.event_loop,
            &mut fixture.state,
            &woken,
            deadline.saturating_duration_since(Instant::now()),
        ));
        fixture.pump(&mut conn);
        if held.is_some() {
            assert_socket_pending(&mut reader);
        }
        drop(held);
        fixture.wait_for_invoke_response(&mut conn, &woken, deadline);
        let test_control::Response::Answer(answer) = socket_response(&mut reader) else {
            panic!("the settled outcome is the answer");
        };
        assert_eq!(answer["action"]["provider"], "probe");
        assert_eq!(answer["receipt"]["broker_invocation_id"], job);
        assert_eq!(answer["receipt"]["update"]["state"], "succeeded");
        assert_eq!(answer["receipt"]["update"]["output"]["value"], "done");
        assert_eq!(resolves.get(), 1, "one request resolves its action once");
        assert_eq!(
            runs.get(),
            1,
            "extra loop wakes never invoke the action again"
        );
    }

    #[test]
    fn a_completed_admission_wakes_the_idle_loop_to_answer_it() {
        use std::io::Write as _;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        tokio::task::LocalSet::new().block_on(&runtime, async {
            let mut fixture = ControlFixture::new();
            let directory =
                crate::shell::collab_broker_capnp::tests::local_directory_fixture_with_membership(
                    fixture.frame_builder.chrome().tree_handle(),
                );
            attach_directory_fixture(&mut fixture.frame_builder, &directory);
            let (wake, woken) = loop_wake(&fixture.event_loop);
            let (mut conn, mut writer, mut reader) = control_client();

            let enumerate = b"{\"op\":\"actions_enumerate\",\"query\":\"open atlas\"}\n";
            writer.write_all(enumerate).expect("cold directory query");
            fixture.pump(&mut conn);
            let _ = socket_response(&mut reader);
            crate::shell::collab_broker_capnp::tests::drain_directory_bridge(&directory).await;
            writer.write_all(enumerate).expect("warm directory query");
            fixture.pump(&mut conn);
            let test_control::Response::Answer(rows) = socket_response(&mut reader) else {
                panic!("warm query must enumerate the directory action");
            };
            let action_id = rows["actions"]
                .as_array()
                .expect("actions array")
                .iter()
                .find(|row| row["provider"] == "app-actions" && row["title"] == "Open Atlas")
                .and_then(|row| row["id"].as_str())
                .expect("directory action id")
                .to_string();
            fixture
                .event_loop
                .dispatch(Some(Duration::ZERO), &mut fixture.state)
                .expect("dispatch");
            woken.set(false);
            // Install the loop's wake only now. The listings above start the
            // local index workers, and a finished build wakes whichever loop
            // was installed when it started; one landing after the clear
            // above would read as the admission waking the loop early. The
            // admission ticket takes this wake when the invocation makes it.
            let _installed = crate::shell::search::invocation_wake::install(wake);

            let invoke = serde_json::json!({
                "op": "actions_invoke",
                "provider": "app-actions",
                "id": action_id,
            });
            writer
                .write_all(format!("{invoke}\n").as_bytes())
                .expect("queue invocation");
            fixture.pump(&mut conn);
            assert_socket_pending(&mut reader);
            fixture
                .event_loop
                .dispatch(Some(Duration::ZERO), &mut fixture.state)
                .expect("dispatch");
            assert!(!woken.get(), "an unanswered admission wakes nobody");

            crate::shell::collab_broker_capnp::tests::drain_directory_bridge(&directory).await;
            // The answer itself ends the loop's park; no other wake is needed
            // for the next pump to answer the client.
            assert!(park_until_woken(
                &mut fixture.event_loop,
                &mut fixture.state,
                &woken,
                tilo_test_env::hang_bound(),
            ));
            fixture.pump(&mut conn);
            let test_control::Response::Answer(admitted) = socket_response(&mut reader) else {
                panic!("the admission receipt answers the invocation");
            };
            assert_ne!(
                admitted["receipt"]["broker_invocation_id"]
                    .as_u64()
                    .expect("real broker invocation id"),
                0
            );
        });
    }

    /// The answer on `reader`, if the connection has given one.
    fn socket_answer(reader: &mut BufReader<UnixStream>) -> Option<test_control::Response> {
        use std::io::BufRead as _;
        match reader.fill_buf() {
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => None,
            Ok([]) => panic!("socket disconnected while waiting"),
            Ok(_) => Some(socket_response(reader)),
            Err(error) => panic!("reading the answer: {error}"),
        }
    }

    /// `wait_ms` on a listing holds it until it is complete — the app's
    /// directory page in, the index built — and answers it in that one
    /// request; with `after_revision`, until it differs. A job read with
    /// `wait_ms` answers once the job has settled.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one fixture walks a listing, a changed listing and a job read in order"
    )]
    fn waited_listings_and_job_reads_answer_once_settled() {
        use std::io::Write as _;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        tokio::task::LocalSet::new().block_on(&runtime, async {
            let mut fixture = ControlFixture::new();
            let mut directory =
                crate::shell::collab_broker_capnp::tests::local_directory_fixture_with_membership(
                    fixture.frame_builder.chrome().tree_handle(),
                );
            attach_directory_fixture(&mut fixture.frame_builder, &directory);
            let (wake, woken) = loop_wake(&fixture.event_loop);
            let _installed = crate::shell::search::invocation_wake::install(wake);
            let (mut conn, mut writer, mut reader) = control_client();

            // Unwaited, a cold listing says it is incomplete rather than
            // passing for the whole answer.
            writer
                .write_all(b"{\"op\":\"actions_enumerate\",\"query\":\"open atlas\"}\n")
                .expect("cold listing");
            fixture.pump(&mut conn);
            let test_control::Response::Answer(cold) = socket_response(&mut reader) else {
                panic!("an unwaited listing answers at once");
            };
            assert_eq!(
                cold["complete"], false,
                "the directory page is owed: {cold}"
            );
            assert!(cold["revision"].is_string());
            assert!(
                cold.get("settled").is_none(),
                "only a waited answer settles"
            );

            // Waited, the same listing is held until it is complete, and the
            // app's action is in its one answer.
            writer
                .write_all(
                    b"{\"op\":\"actions_enumerate\",\"query\":\"open atlas\",\"wait_ms\":30000}\n",
                )
                .expect("waited listing");
            fixture.pump(&mut conn);
            assert_socket_pending(&mut reader);
            assert!(conn.pending_listing.is_some(), "held, not answered");
            let deadline = Instant::now()
                .checked_add(tilo_test_env::hang_bound())
                .expect("a representable deadline");
            let listing = loop {
                woken.set(false);
                crate::shell::collab_broker_capnp::tests::drain_directory_bridge(&directory).await;
                // What it waits for wakes the loop by itself: the page, the
                // index build.
                assert!(
                    park_until_woken(
                        &mut fixture.event_loop,
                        &mut fixture.state,
                        &woken,
                        tilo_test_env::hang_bound(),
                    ),
                    "the held listing's progress never woke the loop"
                );
                fixture.pump(&mut conn);
                if let Some(answer) = socket_answer(&mut reader) {
                    break answer;
                }
                assert!(Instant::now() < deadline, "the listing never settled");
            };
            let test_control::Response::Answer(listing) = listing else {
                panic!("a waited listing answers with the listing");
            };
            assert_eq!(listing["complete"], true, "{listing}");
            assert_eq!(listing["completion"]["complete"], true, "{listing}");
            assert_eq!(listing["settled"], true, "{listing}");
            let action_id = listing["actions"]
                .as_array()
                .expect("actions array")
                .iter()
                .find(|row| row["provider"] == "app-actions" && row["title"] == "Open Atlas")
                .and_then(|row| row["id"].as_str())
                .expect("the directory's action, without asking again")
                .to_string();

            // A wait past this revision settles only on a different listing;
            // one that runs out first answers unsettled with the same one.
            // (Other sources settle on their own clocks, so either may happen;
            // each must be truthful.)
            let past = serde_json::json!({
                "op": "actions_enumerate", "query": "open atlas", "wait_ms": 40,
                "after_revision": listing["revision"],
            });
            writer
                .write_all(format!("{past}\n").as_bytes())
                .expect("listing past a revision");
            let deadline = Instant::now()
                .checked_add(tilo_test_env::hang_bound())
                .expect("a representable deadline");
            let next = loop {
                fixture.pump(&mut conn);
                if let Some(answer) = socket_answer(&mut reader) {
                    break answer;
                }
                assert!(Instant::now() < deadline, "the wait never ran out");
                // Its deadline is the loop's park deadline.
                let park = control_invocation_deadline(std::slice::from_ref(&conn), Instant::now())
                    .expect("a held listing has a deadline");
                fixture
                    .event_loop
                    .dispatch(
                        Some(park.saturating_duration_since(Instant::now())),
                        &mut fixture.state,
                    )
                    .expect("dispatch");
            };
            let test_control::Response::Answer(next) = next else {
                panic!("a wait past a revision answers with a listing");
            };
            if next["settled"] == true {
                assert_ne!(next["revision"], listing["revision"], "{next}");
            } else {
                assert_eq!(next["settled"], false, "{next}");
                assert_eq!(next["revision"], listing["revision"], "{next}");
            }

            // A job read with `wait_ms` is held until the job settles.
            let invoke = serde_json::json!({
                "op": "actions_invoke", "provider": "app-actions", "id": action_id,
            });
            writer
                .write_all(format!("{invoke}\n").as_bytes())
                .expect("invoke");
            fixture.pump(&mut conn);
            let deadline = Instant::now()
                .checked_add(tilo_test_env::hang_bound())
                .expect("a representable deadline");
            let admitted = loop {
                crate::shell::collab_broker_capnp::tests::drain_directory_bridge(&directory).await;
                fixture.pump(&mut conn);
                if let Some(answer) = socket_answer(&mut reader) {
                    break answer;
                }
                assert!(
                    Instant::now() < deadline,
                    "the invocation was never admitted"
                );
            };
            let test_control::Response::Answer(admitted) = admitted else {
                panic!("the admission receipt answers the invocation");
            };
            let job = admitted["receipt"]["broker_invocation_id"]
                .as_u64()
                .expect("a broker job");
            let (delivered, invocation) = directory
                .take_invocation_identity()
                .expect("the app received the invocation");
            assert_eq!(delivered, job);

            let status = serde_json::json!({
                "op": "actions_status", "invocation_id": job, "wait_ms": 30_000,
            });
            writer
                .write_all(format!("{status}\n").as_bytes())
                .expect("waited job read");
            fixture.pump(&mut conn);
            assert_socket_pending(&mut reader);
            woken.set(false);
            directory
                .complete_invocation(
                    job,
                    &tilo_actions::InvocationUpdate::Succeeded {
                        invocation,
                        output: tilo_actions::ActionOutput::default(),
                    },
                )
                .await
                .expect("the app reports its result");
            // The job settling wakes the parked loop by itself. An earlier
            // wake still queued (the index, a directory page) may end a park
            // first; each pump then answers only once the job has settled.
            let deadline = Instant::now()
                .checked_add(tilo_test_env::hang_bound())
                .expect("a representable deadline");
            let settled = loop {
                assert!(
                    park_until_woken(
                        &mut fixture.event_loop,
                        &mut fixture.state,
                        &woken,
                        tilo_test_env::hang_bound(),
                    ),
                    "the settled job never woke the loop"
                );
                woken.set(false);
                fixture.pump(&mut conn);
                if let Some(answer) = socket_answer(&mut reader) {
                    break answer;
                }
                assert!(Instant::now() < deadline, "the job read never settled");
            };
            let test_control::Response::Answer(settled) = settled else {
                panic!("the settled job answers the read");
            };
            assert_eq!(settled["receipt"]["broker_invocation_id"], job);
            assert_eq!(
                settled["receipt"]["update"]["state"], "succeeded",
                "{settled}"
            );
        });
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn socket_dispatch_preserves_directory_admission_and_request_order() {
        // Phase `socket_dispatch_retains_real_directory_admission_and_request_order`.
        {
            use std::io::Write as _;

            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            tokio::task::LocalSet::new().block_on(&runtime, async {
                let tree = directory_test_tree();
                let mut directory =
                    crate::shell::collab_broker_capnp::tests::local_directory_fixture_with_membership(
                        tree.clone(),
                    );
                let mut frame_builder = FrameBuilder::with_handle(tree, false, (800, 600));
                attach_directory_fixture(&mut frame_builder, &directory);
                let mut ctx = TestControlCtx::new();
                let session = SessionManager::new(IdleConfig::from_env(), Instant::now());
                let mut lock = LockController::new(lock_auth::verifier_from_env(), "fixture");
                let event_loop: EventLoop<'static, State> = EventLoop::try_new().expect("calloop");
                let display: Display<State> = Display::new().expect("display");
                let mut state = State::new(event_loop.handle(), display.handle());

                let (server, client) = UnixStream::pair().expect("socket pair");
                let mut writer = client.try_clone().expect("client writer");
                let mut reader = BufReader::new(client);
                reader
                    .get_mut()
                    .set_nonblocking(true)
                    .expect("nonblocking client");
                let mut conn = TestControlConn::new(server, false).expect("server connection");

                writer
                    .write_all(b"{\"op\":\"actions_enumerate\",\"query\":\"open atlas\"}\n")
                    .expect("queue cold directory query");
                pump_test_control_conn(
                    &mut conn,
                    &mut frame_builder,
                    &mut ctx,
                    &session,
                    &mut lock,
                    &mut state,
                );
                let _cold = socket_response(&mut reader);
                crate::shell::collab_broker_capnp::tests::drain_directory_bridge(&directory).await;

                writer
                    .write_all(b"{\"op\":\"actions_enumerate\",\"query\":\"open atlas\"}\n")
                    .expect("queue warm directory query");
                pump_test_control_conn(
                    &mut conn,
                    &mut frame_builder,
                    &mut ctx,
                    &session,
                    &mut lock,
                    &mut state,
                );
                let warm = socket_response(&mut reader);
                let test_control::Response::Answer(rows) = warm else {
                    panic!("warm directory query must enumerate the action");
                };
                let action_id = rows["actions"]
                    .as_array()
                    .expect("enumeration array")
                    .iter()
                    .find(|row| row["provider"] == "app-actions" && row["title"] == "Open Atlas")
                    .and_then(|row| row["id"].as_str())
                    .expect("directory action id")
                    .to_string();

                let invoke = serde_json::json!({
                    "op": "actions_invoke",
                    "provider": "app-actions",
                    "id": action_id,
                });
                let later = serde_json::json!({"op": "actions_enumerate", "query": "settings"});
                writer
                    .write_all(format!("{invoke}\n{later}\n").as_bytes())
                    .expect("pipeline invoke and later request");
                pump_test_control_conn(
                    &mut conn,
                    &mut frame_builder,
                    &mut ctx,
                    &session,
                    &mut lock,
                    &mut state,
                );
                assert_socket_pending(&mut reader);

                let (other_server, other_client) = UnixStream::pair().expect("second socket pair");
                let mut other_writer = other_client.try_clone().expect("second writer");
                let mut other_reader = BufReader::new(other_client);
                other_reader
                    .get_mut()
                    .set_nonblocking(true)
                    .expect("nonblocking second client");
                let mut other_conn = TestControlConn::new(other_server, false).expect("second connection");
                other_writer
                    .write_all(b"{\"op\":\"actions_enumerate\",\"query\":\"settings\"}\n")
                    .expect("second connection request");
                pump_test_control_conn(
                    &mut other_conn,
                    &mut frame_builder,
                    &mut ctx,
                    &session,
                    &mut lock,
                    &mut state,
                );
                assert!(matches!(
                    socket_response(&mut other_reader),
                    test_control::Response::Answer(_)
                ));

                crate::shell::collab_broker_capnp::tests::drain_directory_bridge(&directory).await;
                pump_test_control_conn(
                    &mut conn,
                    &mut frame_builder,
                    &mut ctx,
                    &session,
                    &mut lock,
                    &mut state,
                );
                let test_control::Response::Answer(admitted) = socket_response(&mut reader) else {
                    panic!("broker admission must be returned first");
                };
                let broker_id = admitted["receipt"]["broker_invocation_id"]
                    .as_u64()
                    .expect("real broker invocation id");
                assert_ne!(broker_id, 0);
                assert!(matches!(
                    socket_response(&mut reader),
                    test_control::Response::Answer(_)
                ));
                assert_eq!(
                    directory.resolve_count(),
                    1,
                    "one socket command starts admission once"
                );
                assert!(
                    directory.take_invocation_actor().is_some(),
                    "the real application stream receives the invocation"
                );

                // The deliberately pipelined later query replaced the provider's
                // one retained search page. Rediscover the opaque address before
                // testing authority revocation; a stale address must fail as
                // unknown, while this phase is specifically proving that a
                // currently disclosed address is re-resolved by the broker.
                writer
                    .write_all(b"{\"op\":\"actions_enumerate\",\"query\":\"open atlas\"}\n")
                    .expect("refresh directory query after pipelined request");
                pump_test_control_conn(
                    &mut conn,
                    &mut frame_builder,
                    &mut ctx,
                    &session,
                    &mut lock,
                    &mut state,
                );
                let _ = socket_response(&mut reader);
                crate::shell::collab_broker_capnp::tests::drain_directory_bridge(&directory).await;
                writer
                    .write_all(b"{\"op\":\"actions_enumerate\",\"query\":\"open atlas\"}\n")
                    .expect("read refreshed directory query");
                pump_test_control_conn(
                    &mut conn,
                    &mut frame_builder,
                    &mut ctx,
                    &session,
                    &mut lock,
                    &mut state,
                );
                let test_control::Response::Answer(refreshed) = socket_response(&mut reader) else {
                    panic!("refreshed directory query must enumerate the action");
                };
                assert!(
                    refreshed["actions"]
                        .as_array()
                        .expect("refreshed enumeration array")
                        .iter()
                        .any(|row| row["provider"] == "app-actions" && row["id"] == action_id)
                );

                directory.set_resolve_mode(
                    crate::shell::collab_broker_capnp::tests::DirectoryResolveMode::NotFound,
                );
                writer
                    .write_all(format!("{invoke}\n").as_bytes())
                    .expect("queue revoked invocation");
                pump_test_control_conn(
                    &mut conn,
                    &mut frame_builder,
                    &mut ctx,
                    &session,
                    &mut lock,
                    &mut state,
                );
                crate::shell::collab_broker_capnp::tests::drain_directory_bridge(&directory).await;
                pump_test_control_conn(
                    &mut conn,
                    &mut frame_builder,
                    &mut ctx,
                    &session,
                    &mut lock,
                    &mut state,
                );
                let rejection = socket_response(&mut reader);
                assert!(
                    matches!(
                        rejection,
                        test_control::Response::Err { ref code, ref message }
                            if code == "action_failed" && message.contains("no such action is offered")
                    ),
                    "unexpected revoked-action response: {rejection:?}"
                );
                assert_eq!(directory.resolve_count(), 2);
                assert!(
                    directory.take_invocation_actor().is_none(),
                    "revoked authority must not reach the application"
                );
            });
        }
        // Phase `socket_write_half_close_still_receives_real_directory_admission`.
        {
            use std::io::Write as _;

            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            tokio::task::LocalSet::new().block_on(&runtime, async {
                let tree = directory_test_tree();
                let mut directory =
                    crate::shell::collab_broker_capnp::tests::local_directory_fixture_with_membership(
                        tree.clone(),
                    );
                let mut frame_builder = FrameBuilder::with_handle(tree, false, (800, 600));
                attach_directory_fixture(&mut frame_builder, &directory);
                let mut ctx = TestControlCtx::new();
                let session = SessionManager::new(IdleConfig::from_env(), Instant::now());
                let mut lock = LockController::new(lock_auth::verifier_from_env(), "fixture");
                let event_loop: EventLoop<'static, State> = EventLoop::try_new().expect("calloop");
                let display: Display<State> = Display::new().expect("display");
                let mut state = State::new(event_loop.handle(), display.handle());

                let (server, client) = UnixStream::pair().expect("socket pair");
                let mut writer = client.try_clone().expect("client writer");
                let mut reader = BufReader::new(client);
                reader
                    .get_mut()
                    .set_nonblocking(true)
                    .expect("nonblocking client");
                let mut conn = TestControlConn::new(server, false).expect("server connection");
                let enumerate = b"{\"op\":\"actions_enumerate\",\"query\":\"open atlas\"}\n";
                writer.write_all(enumerate).expect("cold directory query");
                pump_test_control_conn(
                    &mut conn,
                    &mut frame_builder,
                    &mut ctx,
                    &session,
                    &mut lock,
                    &mut state,
                );
                let _ = socket_response(&mut reader);
                crate::shell::collab_broker_capnp::tests::drain_directory_bridge(&directory).await;
                writer.write_all(enumerate).expect("warm directory query");
                pump_test_control_conn(
                    &mut conn,
                    &mut frame_builder,
                    &mut ctx,
                    &session,
                    &mut lock,
                    &mut state,
                );
                let test_control::Response::Answer(rows) = socket_response(&mut reader) else {
                    panic!("warm query must enumerate the directory action");
                };
                let action_id = rows["actions"]
                    .as_array()
                    .expect("actions array")
                    .iter()
                    .find(|row| row["provider"] == "app-actions" && row["title"] == "Open Atlas")
                    .and_then(|row| row["id"].as_str())
                    .expect("directory action id");
                let invoke = serde_json::json!({
                    "op": "actions_invoke",
                    "provider": "app-actions",
                    "id": action_id,
                });
                writer
                    .write_all(format!("{invoke}\n").as_bytes())
                    .expect("queue invocation");
                pump_test_control_conn(
                    &mut conn,
                    &mut frame_builder,
                    &mut ctx,
                    &session,
                    &mut lock,
                    &mut state,
                );
                assert_socket_pending(&mut reader);

                writer
                    .shutdown(std::net::Shutdown::Write)
                    .expect("client finishes its request stream");
                assert_eq!(
                    pump_pending(&mut conn, &frame_builder, &mut ctx),
                    PendingConnectionProgress::Waiting,
                    "write-side EOF must not discard a response the client can still read"
                );
                crate::shell::collab_broker_capnp::tests::drain_directory_bridge(&directory).await;
                assert_eq!(
                    pump_pending(&mut conn, &frame_builder, &mut ctx),
                    PendingConnectionProgress::WroteResponse
                );
                let test_control::Response::Answer(admitted) = socket_response(&mut reader) else {
                    panic!("half-closed client must receive the broker admission receipt");
                };
                assert_ne!(
                    admitted["receipt"]["broker_invocation_id"]
                        .as_u64()
                        .expect("real broker invocation id"),
                    0
                );
                assert!(directory.take_invocation_actor().is_some());
            });
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn socket_admission_times_out_and_partial_responses_are_retained() {
        // Phase `socket_directory_admission_times_out_and_disconnects_without_restart`.
        {
            use std::io::Write as _;

            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            tokio::task::LocalSet::new().block_on(&runtime, async {
                let tree = directory_test_tree();
                let mut directory =
                    crate::shell::collab_broker_capnp::tests::local_directory_fixture_with_membership(
                        tree.clone(),
                    );
                let mut frame_builder = FrameBuilder::with_handle(tree, false, (800, 600));
                attach_directory_fixture(&mut frame_builder, &directory);
                let mut ctx = TestControlCtx::new();
                let session = SessionManager::new(IdleConfig::from_env(), Instant::now());
                let mut lock = LockController::new(lock_auth::verifier_from_env(), "fixture");
                let event_loop: EventLoop<'static, State> = EventLoop::try_new().expect("calloop");
                let display: Display<State> = Display::new().expect("display");
                let mut state = State::new(event_loop.handle(), display.handle());
                let (server, client) = UnixStream::pair().expect("socket pair");
                let mut writer = client.try_clone().expect("client writer");
                let mut reader = BufReader::new(client);
                reader
                    .get_mut()
                    .set_nonblocking(true)
                    .expect("nonblocking client");
                let mut conn = TestControlConn::new(server, false).expect("server connection");

                let enumerate = b"{\"op\":\"actions_enumerate\",\"query\":\"open atlas\"}\n";
                writer.write_all(enumerate).expect("cold directory query");
                pump_test_control_conn(
                    &mut conn,
                    &mut frame_builder,
                    &mut ctx,
                    &session,
                    &mut lock,
                    &mut state,
                );
                let _ = socket_response(&mut reader);
                crate::shell::collab_broker_capnp::tests::drain_directory_bridge(&directory).await;
                writer.write_all(enumerate).expect("warm directory query");
                pump_test_control_conn(
                    &mut conn,
                    &mut frame_builder,
                    &mut ctx,
                    &session,
                    &mut lock,
                    &mut state,
                );
                let test_control::Response::Answer(rows) = socket_response(&mut reader) else {
                    panic!("warm query must enumerate the directory action");
                };
                let action_id = rows["actions"]
                    .as_array()
                    .expect("actions array")
                    .iter()
                    .find(|row| row["provider"] == "app-actions" && row["title"] == "Open Atlas")
                    .and_then(|row| row["id"].as_str())
                    .expect("directory action id")
                    .to_string();
                let invoke = serde_json::json!({
                    "op": "actions_invoke",
                    "provider": "app-actions",
                    "id": action_id,
                });

                crate::shell::search::providers::live_app_actions::set_directory_start_timeout_for_test(
                    Some(Duration::ZERO),
                );
                writer
                    .write_all(format!("{invoke}\n").as_bytes())
                    .expect("queue timeout invocation");
                pump_test_control_conn(
                    &mut conn,
                    &mut frame_builder,
                    &mut ctx,
                    &session,
                    &mut lock,
                    &mut state,
                );
                pump_test_control_conn(
                    &mut conn,
                    &mut frame_builder,
                    &mut ctx,
                    &session,
                    &mut lock,
                    &mut state,
                );
                let timeout_response = socket_response(&mut reader);
                assert!(
                    matches!(
                        timeout_response,
                    test_control::Response::Err { ref code, ref message }
                        if code == "action_failed" && message.contains("timed out")
                    ),
                    "unexpected timeout response: {timeout_response:?}"
                );
                crate::shell::search::providers::live_app_actions::set_directory_start_timeout_for_test(
                    None,
                );
                crate::shell::collab_broker_capnp::tests::drain_directory_bridge(&directory).await;
                assert!(directory.take_invocation_actor().is_none());

                writer
                    .write_all(enumerate)
                    .expect("refresh invalidated directory page");
                pump_test_control_conn(
                    &mut conn,
                    &mut frame_builder,
                    &mut ctx,
                    &session,
                    &mut lock,
                    &mut state,
                );
                let _ = socket_response(&mut reader);
                crate::shell::collab_broker_capnp::tests::drain_directory_bridge(&directory).await;
                writer
                    .write_all(enumerate)
                    .expect("read refreshed directory page");
                pump_test_control_conn(
                    &mut conn,
                    &mut frame_builder,
                    &mut ctx,
                    &session,
                    &mut lock,
                    &mut state,
                );
                let _ = socket_response(&mut reader);

                writer
                    .write_all(format!("{invoke}\n").as_bytes())
                    .expect("queue invocation before disconnect");
                pump_test_control_conn(
                    &mut conn,
                    &mut frame_builder,
                    &mut ctx,
                    &session,
                    &mut lock,
                    &mut state,
                );
                assert_socket_pending(&mut reader);
                drop(reader);
                drop(writer);
                assert_eq!(
                    pump_pending(&mut conn, &frame_builder, &mut ctx),
                    PendingConnectionProgress::Closed
                );
                crate::shell::collab_broker_capnp::tests::drain_directory_bridge(&directory).await;
                assert!(directory.take_invocation_actor().is_none());
            });
            crate::shell::search::providers::live_app_actions::set_directory_start_timeout_for_test(
                None,
            );
        }
        // Phase `nonblocking_socket_retains_a_partial_response_until_the_frame_is_complete`.
        {
            use std::io::Read as _;

            let (server, mut client) = UnixStream::pair().expect("socket pair");
            client.set_nonblocking(true).expect("nonblocking client");
            let mut conn = TestControlConn::new(server, false).expect("server connection");
            let response = test_control::Response::Answer(serde_json::json!({
                "blob": "x".repeat(8 * 1024 * 1024),
            }));
            let mut expected = serialise_response(&response).into_bytes();
            expected.push(b'\n');

            assert_eq!(
                queue_test_control_response(&mut conn, &response),
                PendingConnectionProgress::OutputPending,
                "a payload larger than the Unix receive buffer must exercise WouldBlock"
            );
            assert!(
                conn.output_offset > 0,
                "the first write must be a real short write"
            );
            assert!(conn.output_offset < conn.output.len());

            let mut received = Vec::with_capacity(expected.len());
            let mut chunk = vec![0_u8; 64 * 1024];
            for _ in 0..1024 {
                loop {
                    match client.read(&mut chunk) {
                        Ok(0) => panic!("socket closed before the retained frame completed"),
                        Ok(read) => received.extend_from_slice(&chunk[..read]),
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(error) => panic!("read retained response: {error}"),
                    }
                }
                match flush_test_control_output(&mut conn) {
                    PendingConnectionProgress::OutputPending => {}
                    PendingConnectionProgress::WroteResponse => break,
                    progress => panic!("unexpected output progress: {progress:?}"),
                }
            }
            loop {
                match client.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(read) => received.extend_from_slice(&chunk[..read]),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) => panic!("read final retained response: {error}"),
                }
            }
            assert!(
                conn.output.is_empty(),
                "the entire queued frame was flushed"
            );
            assert_eq!(
                received, expected,
                "backpressure must not truncate or duplicate JSON bytes"
            );
        }
    }

    #[test]
    fn fragmented_control_request_waits_for_its_complete_line() {
        use std::io::{Read as _, Write as _};

        let mut frame_builder = FrameBuilder::with_handle(directory_test_tree(), false, (800, 600));
        let mut ctx = TestControlCtx::new();
        let session = SessionManager::new(IdleConfig::from_env(), Instant::now());
        let mut lock = LockController::new(lock_auth::verifier_from_env(), "fixture");
        let event_loop: EventLoop<'static, State> = EventLoop::try_new().expect("calloop");
        let display: Display<State> = Display::new().expect("display");
        let mut state = State::new(event_loop.handle(), display.handle());
        let (server, mut client) = UnixStream::pair().expect("socket pair");
        client.set_nonblocking(true).expect("nonblocking observer");
        let mut conn = TestControlConn::new(server, false).expect("server connection");
        client.write_all(b"{\"op\":").expect("request prefix");
        pump_test_control_conn(
            &mut conn,
            &mut frame_builder,
            &mut ctx,
            &session,
            &mut lock,
            &mut state,
        );
        let mut probe = [0_u8; 1];
        assert_eq!(
            client.read(&mut probe).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock,
            "an incomplete request must not be dispatched"
        );
        client
            .write_all(b"\"state_dump\"}\n")
            .expect("request suffix");
        pump_test_control_conn(
            &mut conn,
            &mut frame_builder,
            &mut ctx,
            &session,
            &mut lock,
            &mut state,
        );
        let response = socket_response(&mut BufReader::new(client));
        let test_control::Response::State { chrome, .. } = response else {
            panic!("a fragmented complete state request must be served: {response:?}")
        };
        assert_eq!(chrome["lock_auth"]["verifying"], false);
    }

    /// The device admin's socket answers the action model and read-only
    /// state, and refuses synthetic input, fixtures and agent seats with an
    /// error that names the route that serves them.
    #[test]
    fn the_admin_socket_serves_the_action_model_and_refuses_the_rest() {
        use std::io::Write as _;

        let mut frame_builder = FrameBuilder::with_handle(directory_test_tree(), false, (800, 600));
        let mut ctx = TestControlCtx::new();
        let session = SessionManager::new(IdleConfig::from_env(), Instant::now());
        let mut lock = LockController::new(lock_auth::verifier_from_env(), "fixture");
        let event_loop: EventLoop<'static, State> = EventLoop::try_new().expect("calloop");
        let display: Display<State> = Display::new().expect("display");
        let mut state = State::new(event_loop.handle(), display.handle());
        // This test isolates dispatch on an already admitted transport. The
        // actual constructor's identity recheck is exercised first below.
        let (server, _client) = UnixStream::pair().expect("socket pair");
        let mut conn =
            TestControlConn::admin(server, Path::new("/nonexistent-tilo-admin-identity"))
                .expect("admin connection");
        pump_test_control_conn(
            &mut conn,
            &mut frame_builder,
            &mut ctx,
            &session,
            &mut lock,
            &mut state,
        );
        assert!(
            conn.closed,
            "an unverified identity cannot use the admin transport"
        );
        let (server, client) = UnixStream::pair().expect("admitted transport pair");
        let mut writer = client.try_clone().expect("admitted client writer");
        let mut reader = BufReader::new(client);
        let mut conn = TestControlConn::new(server, false).expect("transport connection");
        conn.admin = true;
        let mut ask = |line: &str| {
            writer
                .write_all(format!("{line}\n").as_bytes())
                .expect("write request");
            pump_test_control_conn(
                &mut conn,
                &mut frame_builder,
                &mut ctx,
                &session,
                &mut lock,
                &mut state,
            );
            socket_response(&mut reader)
        };

        let listed = ask(r#"{"op":"actions_enumerate","query":"settings"}"#);
        let test_control::Response::Answer(rows) = listed else {
            panic!("enumeration is served: {listed:?}");
        };
        assert!(
            rows["count"].as_u64().is_some_and(|count| count > 0),
            "{rows}"
        );
        let completion = &rows["completion"];
        assert!(
            completion["complete"].is_boolean(),
            "admin wire preserves completion: {rows}"
        );
        assert!(
            completion["incomplete"].is_array(),
            "admin wire preserves provider outcomes: {rows}"
        );
        assert_eq!(
            completion["complete"].as_bool(),
            completion["incomplete"].as_array().map(Vec::is_empty)
        );
        assert_eq!(rows["complete"], completion["complete"]);
        assert!(matches!(
            ask(r#"{"op":"state_dump"}"#),
            test_control::Response::State { .. }
        ));
        for refused in [
            r#"{"op":"click_at","x":10,"y":10}"#,
            r#"{"op":"type","text":"hello"}"#,
            r#"{"op":"snapshot","path":"admin.png"}"#,
            r#"{"op":"grant","subject":"agent:x","scope":"workspace","access":"write"}"#,
            r#"{"op":"provision_test_identity"}"#,
            r#"{"op":"actions_enumerate","query":"","agent":"bot"}"#,
            r#"{"op":"state_dump","window":"so.tilo.notes"}"#,
        ] {
            let test_control::Response::Err { code, message } = ask(refused) else {
                panic!("{refused} must be refused on the admin socket");
            };
            assert_eq!(code, "not_permitted", "{refused}");
            assert!(message.contains("TILO_TEST_CONTROL"), "{message}");
        }
    }

    /// A full-size high-density screenshot lands whole, and the staging file
    /// that made it atomic is gone.
    #[test]
    fn a_written_screenshot_is_complete_and_leaves_no_staging_file() {
        let dir = tempfile::tempdir().expect("scratch directory");
        let path = dir.path().join("shot.png");
        std::fs::write(&path, b"stale").expect("an older file to replace");
        let (width, height) = (2944_u32, 1840_u32);
        let rgba = vec![0x7f; usize::try_from(width * height * 4).expect("fits")];
        let png = ui::png::encode_rgba8(width, height, &rgba).expect("encode");
        write_whole(&path, &png).expect("write");
        let decoded =
            ui::png::decode_rgba8(&std::fs::read(&path).expect("read back")).expect("complete PNG");
        assert_eq!((decoded.width, decoded.height), (width, height));
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .expect("list")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("shot.png")]);
    }

    /// A request that could not do what it asked says so: a screenshot that
    /// was not written, and an action or screenshot while the session is
    /// locked, answer an error instead of `ok`.
    #[test]
    fn unwritten_snapshots_and_locked_actions_answer_errors() {
        use std::io::Write as _;

        let mut frame_builder = FrameBuilder::with_handle(directory_test_tree(), false, (800, 600));
        let mut ctx = TestControlCtx::new();
        let mut session = SessionManager::new(IdleConfig::from_env(), Instant::now());
        let mut lock = LockController::new(lock_auth::verifier_from_env(), "fixture");
        let event_loop: EventLoop<'static, State> = EventLoop::try_new().expect("calloop");
        let display: Display<State> = Display::new().expect("display");
        let mut state = State::new(event_loop.handle(), display.handle());
        let (server, client) = UnixStream::pair().expect("socket pair");
        let mut writer = client.try_clone().expect("client writer");
        let mut reader = BufReader::new(client);
        let mut conn = TestControlConn::new(server, false).expect("connection");
        let missing = tempfile::tempdir()
            .expect("scratch")
            .path()
            .join("gone")
            .join("shot.png");
        let snapshot = serde_json::json!({"op": "snapshot", "path": missing}).to_string();
        let invoke = r#"{"op":"actions_invoke","provider":"session","id":"lock"}"#;

        for (line, locked, code) in [
            (snapshot.as_str(), false, "snapshot_failed"),
            (snapshot.as_str(), true, "session_locked"),
            (invoke, true, "session_locked"),
        ] {
            if locked && !session.is_locked() {
                let _ = session.request_lock(Instant::now());
                assert!(session.is_locked(), "the fixture session locks");
            }
            writer
                .write_all(format!("{line}\n").as_bytes())
                .expect("write request");
            // A screenshot is written off the loop: its answer goes out on
            // the pump after the writer reported.
            tilo_test_env::wait_until("the screenshot's answer to go out", || {
                pump_test_control_conn(
                    &mut conn,
                    &mut frame_builder,
                    &mut ctx,
                    &session,
                    &mut lock,
                    &mut state,
                );
                conn.pending_snapshot.is_none()
            });
            let test_control::Response::Err { code: answered, .. } = socket_response(&mut reader)
            else {
                panic!("{line} (locked: {locked}) must answer an error");
            };
            assert_eq!(answered, code, "{line} (locked: {locked})");
        }
        assert!(!missing.exists());
    }

    #[test]
    fn owner_socket_publishes_and_submits_locked_session_authentication() {
        use std::io::Write as _;
        let mut fb = FrameBuilder::with_handle(directory_test_tree(), false, (800, 600));
        let mut ctx = TestControlCtx::new();
        let mut session = SessionManager::new(IdleConfig::from_env(), Instant::now());
        let _ = session.request_lock(Instant::now());
        assert!(session.is_locked());
        let mut lock =
            LockController::new(std::sync::Arc::new(lock_auth::UnavailableVerifier), "owner");
        let event_loop: EventLoop<'static, State> = EventLoop::try_new().unwrap();
        let display: Display<State> = Display::new().unwrap();
        let mut state = State::new(event_loop.handle(), display.handle());
        let (server, client) = UnixStream::pair().unwrap();
        let mut writer = client.try_clone().unwrap();
        let mut reader = BufReader::new(client);
        let mut conn = TestControlConn::new(server, false).unwrap();
        // The production accept path sets this only after device-owner and
        // same-UID, unconfined kernel peer checks (admin_control tests).
        conn.admin = true;
        writer
            .write_all(b"{\"op\":\"actions_enumerate\"}\n")
            .unwrap();
        pump_test_control_conn(
            &mut conn, &mut fb, &mut ctx, &session, &mut lock, &mut state,
        );
        let test_control::Response::Answer(catalogue) = socket_response(&mut reader) else {
            panic!("owner enumeration must return the lock resource");
        };
        assert_eq!(catalogue["actions"][0]["provider"], "session-lock");
        assert_eq!(catalogue["actions"][0]["id"], "unlock@session-lock");
        writer.write_all(b"{\"op\":\"actions_invoke\",\"provider\":\"session-lock\",\"id\":\"unlock@session-lock\",\"args\":{\"password\":\"private-test-password\"}}\n").unwrap();
        assert!(pump_test_control_conn(
            &mut conn, &mut fb, &mut ctx, &session, &mut lock, &mut state
        ));
        let response = socket_response(&mut reader);
        assert!(
            matches!(response,test_control::Response::Answer(ref value) if value["authentication"]=="verifying")
        );
        assert!(
            !serde_json::to_string(&response)
                .unwrap()
                .contains("private-test-password")
        );
        assert_eq!(lock.buffer_len(), 0);
        assert!(
            session.is_locked(),
            "action acceptance cannot unlock a session"
        );
        writer.write_all(b"{\"op\":\"actions_invoke\",\"provider\":\"session-lock\",\"id\":\"unlock@session-lock\",\"args\":{\"password\":\"private-test-password\"},\"agent\":\"peer\"}\n").unwrap();
        pump_test_control_conn(
            &mut conn, &mut fb, &mut ctx, &session, &mut lock, &mut state,
        );
        assert!(
            matches!(socket_response(&mut reader),test_control::Response::Err{code,..} if code=="not_permitted")
        );
        assert!(session.is_locked());
    }

    #[test]
    fn session_diagnostics_require_the_unlocked_authenticated_owner_connection() {
        use std::io::Write as _;
        let mut fb = FrameBuilder::with_handle(directory_test_tree(), false, (800, 600));
        let mut ctx = TestControlCtx::new();
        let mut session = SessionManager::new(IdleConfig::from_env(), Instant::now());
        let mut lock =
            LockController::new(std::sync::Arc::new(lock_auth::UnavailableVerifier), "owner");
        let event_loop: EventLoop<'static, State> = EventLoop::try_new().unwrap();
        let display: Display<State> = Display::new().unwrap();
        let mut state = State::new(event_loop.handle(), display.handle());
        let (server, client) = UnixStream::pair().unwrap();
        let mut writer = client.try_clone().unwrap();
        let mut reader = BufReader::new(client);
        let mut conn = TestControlConn::new(server, false).unwrap();
        let request = serde_json::json!({"op":"actions_invoke", "provider":"session-diagnostics", "id":"configure@session-diagnostics", "args":{"presentation":true,"gpu_stages":false,"gpu_timestamps":false,"gpu_dispatch_timestamps":false}});
        writer.write_all(format!("{request}\n").as_bytes()).unwrap();
        pump_test_control_conn(
            &mut conn, &mut fb, &mut ctx, &session, &mut lock, &mut state,
        );
        assert!(matches!(
            socket_response(&mut reader),
            test_control::Response::Err { code, .. } if code == "not_permitted"
        ));
        assert!(
            crate::shell::launcher::LaunchSession::current()
                .diagnostics
                .lock()
                .unwrap()
                .is_none()
        );
        conn.admin = true; // Set by authenticated production accept, tested in admin_control.
        writer.write_all(format!("{request}\n").as_bytes()).unwrap();
        pump_test_control_conn(
            &mut conn, &mut fb, &mut ctx, &session, &mut lock, &mut state,
        );
        assert!(
            matches!(socket_response(&mut reader), test_control::Response::Answer(value) if value["diagnostics"]["presentation"] == true)
        );
        writer.write_all(b"{\"op\":\"actions_invoke\",\"provider\":\"session-diagnostics\",\"id\":\"inspect@session-diagnostics\",\"args\":{}}\n").unwrap();
        pump_test_control_conn(
            &mut conn, &mut fb, &mut ctx, &session, &mut lock, &mut state,
        );
        assert!(
            matches!(socket_response(&mut reader), test_control::Response::Answer(value) if value["launch_history"]["attempts"].is_array())
        );
        writer
            .write_all(b"{\"op\":\"actions_enumerate\",\"query\":\"session-diagnostics\"}\n")
            .unwrap();
        pump_test_control_conn(
            &mut conn, &mut fb, &mut ctx, &session, &mut lock, &mut state,
        );
        assert!(
            matches!(socket_response(&mut reader), test_control::Response::Answer(value) if value["actions"].as_array().unwrap().iter().any(|row| row["id"] == "configure@session-diagnostics"))
        );
        let mut agent = request.clone();
        agent["agent"] = serde_json::json!("peer");
        writer.write_all(format!("{agent}\n").as_bytes()).unwrap();
        pump_test_control_conn(
            &mut conn, &mut fb, &mut ctx, &session, &mut lock, &mut state,
        );
        assert!(
            matches!(socket_response(&mut reader), test_control::Response::Err { code, .. } if code == "not_permitted")
        );
        let _ = session.request_lock(Instant::now());
        writer.write_all(format!("{request}\n").as_bytes()).unwrap();
        pump_test_control_conn(
            &mut conn, &mut fb, &mut ctx, &session, &mut lock, &mut state,
        );
        assert!(matches!(
            socket_response(&mut reader),
            test_control::Response::Err { .. }
        ));
    }

    /// Profiling the live compositor is the device admin's alone: the
    /// development test-control socket, the agent socket, an agent acting
    /// over the admin socket and a locked session are all refused before
    /// anything is recorded.
    #[test]
    fn compositor_profiling_is_refused_to_all_but_the_unlocked_owner_connection() {
        use std::io::Write as _;
        let mut fb = FrameBuilder::with_handle(directory_test_tree(), false, (800, 600));
        let mut ctx = TestControlCtx::new();
        let mut session = SessionManager::new(IdleConfig::from_env(), Instant::now());
        let mut lock =
            LockController::new(std::sync::Arc::new(lock_auth::UnavailableVerifier), "owner");
        let event_loop: EventLoop<'static, State> = EventLoop::try_new().unwrap();
        let display: Display<State> = Display::new().unwrap();
        let mut state = State::new(event_loop.handle(), display.handle());
        let start = serde_json::json!({"op":"actions_invoke", "provider":"session-diagnostics", "id":"profile-start@session-diagnostics", "args":{"seconds":5}});
        let mut as_agent = start.clone();
        as_agent["agent"] = serde_json::json!("peer");
        for (agent_socket, admin, request) in [
            (false, false, &start),
            (true, false, &start),
            (false, true, &as_agent),
        ] {
            let (server, client) = UnixStream::pair().unwrap();
            let mut writer = client.try_clone().unwrap();
            let mut reader = BufReader::new(client);
            let mut conn = TestControlConn::new(server, agent_socket).unwrap();
            conn.admin = admin;
            writer.write_all(format!("{request}\n").as_bytes()).unwrap();
            pump_test_control_conn(
                &mut conn, &mut fb, &mut ctx, &session, &mut lock, &mut state,
            );
            assert!(
                matches!(
                    socket_response(&mut reader),
                    test_control::Response::Err { code, .. } if code == "not_permitted"
                ),
                "agent socket {agent_socket}, admin {admin}, request {request}"
            );
        }
        let _ = session.request_lock(Instant::now());
        let (server, client) = UnixStream::pair().unwrap();
        let mut writer = client.try_clone().unwrap();
        let mut reader = BufReader::new(client);
        let mut conn = TestControlConn::new(server, false).unwrap();
        conn.admin = true;
        writer.write_all(format!("{start}\n").as_bytes()).unwrap();
        pump_test_control_conn(
            &mut conn, &mut fb, &mut ctx, &session, &mut lock, &mut state,
        );
        assert!(
            matches!(
                socket_response(&mut reader),
                test_control::Response::Err { .. }
            ),
            "a locked session refuses the owner too"
        );
        assert!(
            !crate::compositor::metrics::profile::active(),
            "no refused request started a profile"
        );
    }

    fn test_control_wakes() -> usize {
        TEST_CONTROL_WAKES.with(std::cell::Cell::get)
    }

    /// Run one iteration of `event_loop`, waiting at most `timeout`, and
    /// return how many test-control wakes fired in it.
    fn dispatch_counting_wakes(
        event_loop: &mut EventLoop<'static, State>,
        state: &mut State,
        timeout: Duration,
    ) -> usize {
        let before = test_control_wakes();
        event_loop.dispatch(Some(timeout), state).expect("dispatch");
        test_control_wakes().saturating_sub(before)
    }

    /// A driver's request often lands after its connection was accepted and
    /// found empty. The accepted stream must then wake an otherwise idle
    /// loop itself, rather than leave the request for an unrelated event.
    #[test]
    fn a_request_written_after_accept_wakes_an_idle_loop() {
        use std::io::Write as _;

        let mut event_loop: EventLoop<'static, State> = EventLoop::try_new().expect("calloop");
        let display: Display<State> = Display::new().expect("display");
        let mut state = State::new(event_loop.handle(), display.handle());
        let (server, mut client) = UnixStream::pair().expect("socket pair");
        let mut conn = TestControlConn::new(server, false).expect("server connection");
        conn.watch(&event_loop.handle());

        // Nothing to read yet, so nothing is ready.
        assert_eq!(
            dispatch_counting_wakes(&mut event_loop, &mut state, Duration::ZERO),
            0,
            "the loop must be idle before the request, or this proves nothing"
        );

        client
            .write_all(b"{\"op\":\"list_targets\"}\n")
            .expect("write the request");
        assert_eq!(
            dispatch_counting_wakes(&mut event_loop, &mut state, tilo_test_env::hang_bound()),
            1,
            "the request did not wake the loop"
        );
    }

    /// A response larger than the socket's send buffer (a full action
    /// listing) goes out in pieces. Each piece the driver drains must wake an
    /// otherwise idle loop to send the next, so the whole answer arrives
    /// promptly; and a driver that is not reading must not keep it awake.
    #[test]
    fn a_response_larger_than_the_socket_buffer_reaches_a_reading_driver_while_idle() {
        use std::io::Read as _;

        let mut event_loop: EventLoop<'static, State> = EventLoop::try_new().expect("calloop");
        let display: Display<State> = Display::new().expect("display");
        let mut state = State::new(event_loop.handle(), display.handle());
        let (server, mut client) = UnixStream::pair().expect("socket pair");
        client.set_nonblocking(true).expect("nonblocking driver");
        let mut conn = TestControlConn::new(server, false).expect("server connection");
        conn.watch(&event_loop.handle());
        let response = test_control::Response::Answer(serde_json::json!({
            "blob": "x".repeat(4 * 1024 * 1024),
        }));
        let mut expected = serialise_response(&response).into_bytes();
        expected.push(b'\n');
        assert_eq!(
            queue_test_control_response(&mut conn, &response),
            PendingConnectionProgress::OutputPending,
            "the response must not fit the socket, or this proves nothing"
        );
        conn.watch_output(&event_loop.handle());

        // The driver has not read yet: the full socket must not wake the loop.
        assert_eq!(
            dispatch_counting_wakes(&mut event_loop, &mut state, Duration::ZERO),
            0,
            "a full socket woke the loop before the driver read anything"
        );

        let mut received = Vec::with_capacity(expected.len());
        let mut chunk = vec![0_u8; 64 * 1024];
        loop {
            loop {
                match client.read(&mut chunk) {
                    Ok(0) => panic!("the connection closed mid-response"),
                    Ok(read) => received.extend_from_slice(&chunk[..read]),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) => panic!("read the response: {error}"),
                }
            }
            if received.len() == expected.len() {
                break;
            }
            // Nothing but the drained socket can wake this loop.
            assert_eq!(
                dispatch_counting_wakes(&mut event_loop, &mut state, tilo_test_env::hang_bound()),
                1,
                "the drained socket did not wake the idle loop"
            );
            let _ = flush_test_control_output(&mut conn);
            conn.watch_output(&event_loop.handle());
        }
        assert_eq!(
            received, expected,
            "the response arrived whole and in order"
        );
        assert!(
            conn.output_wake.is_none(),
            "a finished response keeps no output wake"
        );

        // Done: nothing keeps the loop awake.
        assert_eq!(
            dispatch_counting_wakes(&mut event_loop, &mut state, Duration::ZERO),
            0,
            "the loop kept waking after the response was through"
        );
        conn.unwatch(&event_loop.handle());
    }

    // publishing the socket name for the nested dev loop

    #[test]
    fn a_second_compositor_leaves_the_display_file_of_a_running_session() {
        let runtime = tempfile::tempdir().unwrap();
        let display = session_display_path(Some(runtime.path().into())).unwrap();
        std::fs::create_dir_all(display.parent().unwrap()).unwrap();
        std::fs::write(&display, "wayland-1").unwrap();
        let ours = std::ffi::OsStr::new("wayland-2");

        let running =
            std::os::unix::net::UnixListener::bind(runtime.path().join("wayland-1")).unwrap();
        assert!(
            another_live_session_published(&display, ours),
            "the desktop on wayland-1 still runs: its file is its own"
        );
        assert!(
            !another_live_session_published(&display, std::ffi::OsStr::new("wayland-1")),
            "a file naming this compositor's own socket is its own"
        );
        drop(running);
        assert!(
            !another_live_session_published(&display, ours),
            "a session that is gone leaves its file to be replaced"
        );
        std::fs::write(&display, "../wayland-1").unwrap();
        assert!(!another_live_session_published(&display, ours));
    }

    #[test]
    fn display_file_is_published_under_the_runtime_dir() {
        // Phase `display_path_lands_under_the_runtime_dir`.
        assert_eq!(
            session_display_path(Some(std::ffi::OsString::from("/run/user/1000"))),
            Some(PathBuf::from("/run/user/1000/tilo-session/wayland-display")),
        );
        // Phase `display_path_declines_an_unset_or_relative_runtime_dir`.
        assert_eq!(session_display_path(None), None);
        assert_eq!(
            session_display_path(Some(std::ffi::OsString::from("relative/dir"))),
            None,
            "a session-scoped file must not be placed by a relative path"
        );
        // Phase `published_display_file_is_exactly_the_socket_name`.
        {
            use std::os::unix::fs::PermissionsExt;

            // Round-trip through the real writer: what lands on disk must be usable as
            // `WAYLAND_DISPLAY=$(cat …)` — no trailing newline, no quoting.
            let dir =
                std::env::temp_dir().join(format!("tilo-display-test-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("create temp runtime dir");
            let path = session_display_path(Some(dir.clone().into_os_string()))
                .expect("absolute temp dir yields a path");
            std::fs::create_dir_all(path.parent().unwrap()).expect("create tilo-session dir");
            tilo_files::native::atomic::write_with_mode(&path, b"wayland-7", 0o600)
                .expect("write display file");

            let read = std::fs::read_to_string(&path).expect("read back");
            assert_eq!(read, "wayland-7");

            let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "session file must not be readable by others");

            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// These unit tests exercise the *chrome* half of the executor with no
    /// compositor `State`; client-routing half is covered by `compositor::client_input`'s own tests.
    fn execute_planned_action(
        action: &PlannedAction,
        fb: &mut FrameBuilder,
        ctx: &mut TestControlCtx,
    ) {
        super::execute_planned_action(action, fb, ctx, None);
        // A screenshot is written off the loop; the tests read it as the
        // request's answer would, once it is written.
        if let Some(outcome) = ctx.deferred_snapshot_reply.take() {
            tilo_test_env::wait_until("the screenshot to be written", || {
                outcome.lock_recover().is_some()
            });
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn test_control_input_actions_drive_the_frame_context() {
        // Phase `cursor_moved_action_updates_ctx_cursor`.
        // Sanity: `CursorMoved` updates the cached cursor and dispatches without
        // panicking — matters because `MouseInput` (no position on winit) hit-tests against `last_cursor` inside the host.
        {
            let mut fb = FrameBuilder::new((1600, 900));
            let mut ctx = TestControlCtx::new();
            execute_planned_action(
                &PlannedAction::CursorMoved { x: 250.0, y: 75.0 },
                &mut fb,
                &mut ctx,
            );
            assert_eq!(ctx.cursor.x, 250.0);
            assert_eq!(ctx.cursor.y, 75.0);
        }
        // Phase `full_click_sequence_dispatches_without_panic`.
        // The full click sequence (`CursorMoved` + down + up) dispatches all
        // three without panicking — `click_at` returns them, and all three must reach `ShellOverlayHost::handle_event`.
        {
            let mut fb = FrameBuilder::new((1600, 900));
            let mut ctx = TestControlCtx::new();
            let actions = vec![
                PlannedAction::CursorMoved { x: 800.0, y: 450.0 },
                PlannedAction::MouseLeftButton { pressed: true },
                PlannedAction::MouseLeftButton { pressed: false },
            ];
            for action in &actions {
                execute_planned_action(action, &mut fb, &mut ctx);
            }
            // No panic + relayout still works = wire intact.
            fb.relayout();
            let scene = fb.paint();
            assert!(!scene.encoding().is_empty());
        }
        // Phase `synthetic_touchpad_gestures_reach_the_shell_through_the_tracker`.
        // The same tracker as libinput's path decides what the fingers meant.
        {
            use crate::shell::test_control::TouchpadEdge;
            let mut fb = FrameBuilder::new((1600, 900));
            let mut ctx = TestControlCtx::new();
            let mut run = |fb: &mut FrameBuilder, edges: &[TouchpadEdge]| {
                for edge in edges {
                    execute_planned_action(&PlannedAction::Touchpad(*edge), fb, &mut ctx);
                }
            };
            run(
                &mut fb,
                &[
                    TouchpadEdge::PinchBegin { fingers: 3 },
                    TouchpadEdge::PinchUpdate { scale: 0.7 },
                    TouchpadEdge::PinchEnd,
                ],
            );
            assert!(
                fb.chrome().overlays.overview,
                "pinching in opens the overview"
            );
            run(
                &mut fb,
                &[
                    TouchpadEdge::PinchBegin { fingers: 3 },
                    TouchpadEdge::PinchUpdate { scale: 1.4 },
                    TouchpadEdge::PinchEnd,
                ],
            );
            assert!(!fb.chrome().overlays.overview, "pinching out closes it");

            let first = fb.chrome().tree().desktop.active;
            run(
                &mut fb,
                &[
                    TouchpadEdge::SwipeBegin { fingers: 4 },
                    TouchpadEdge::SwipeUpdate { dx: 0.0, dy: 80.0 },
                    TouchpadEdge::SwipeUpdate { dx: 0.0, dy: 80.0 },
                    TouchpadEdge::SwipeEnd { cancelled: false },
                ],
            );
            assert_ne!(
                fb.chrome().tree().desktop.active,
                first,
                "a four-finger downward swipe switches workspace"
            );
        }
        // Phase `set_modifiers_action_updates_ctx_mask`.
        // `SetModifiers` mutates `ctx.modifiers` directly; the host reads the
        // snapshot on the next chord, so this is what chord injection (Ctrl+K, …) depends on.
        {
            let mut fb = FrameBuilder::new((1600, 900));
            let mut ctx = TestControlCtx::new();
            execute_planned_action(
                &PlannedAction::SetModifiers {
                    ctrl: true,
                    shift: false,
                    alt: false,
                    logo: true,
                },
                &mut fb,
                &mut ctx,
            );
            assert!(ctx.modifiers.state().control_key());
            assert!(ctx.modifiers.state().super_key());
            assert!(!ctx.modifiers.state().shift_key());
            assert!(!ctx.modifiers.state().alt_key());
        }
        // Phase `keyboard_key_press_release_dispatches_without_panic`.
        // A keyboard press+release pair dispatches without panicking — same bar
        // as `full_click_sequence_dispatches_without_panic`, for both press and release.
        {
            let mut fb = FrameBuilder::new((1600, 900));
            let mut ctx = TestControlCtx::new();
            for pressed in [true, false] {
                execute_planned_action(
                    &PlannedAction::KeyboardKey {
                        key: KeyTag::Named(NamedKeyTag::Escape),
                        pressed,
                    },
                    &mut fb,
                    &mut ctx,
                );
            }
            fb.relayout();
            let _ = fb.paint();
        }
        // Phase `cmd_tab_chord_via_test_control_opens_overview`.
        // Cmd+Tab via `test_control` opens the overview overlay — proves
        // modifiers + named-key chords reach the host's chord handler, like `ShellDriver::press_key` in-process.
        {
            let mut fb = FrameBuilder::new((1600, 900));
            let _ = fb.paint();
            assert!(
                !fb.chrome().overlays.overview,
                "bootstrap layout starts with overview closed"
            );
            let mut ctx = TestControlCtx::new();
            // Plant the Super modifier, then dispatch Tab press+release.
            execute_planned_action(
                &PlannedAction::SetModifiers {
                    ctrl: false,
                    shift: false,
                    alt: false,
                    logo: true,
                },
                &mut fb,
                &mut ctx,
            );
            execute_planned_action(
                &PlannedAction::KeyboardKey {
                    key: KeyTag::Named(NamedKeyTag::Tab),
                    pressed: true,
                },
                &mut fb,
                &mut ctx,
            );
            execute_planned_action(
                &PlannedAction::KeyboardKey {
                    key: KeyTag::Named(NamedKeyTag::Tab),
                    pressed: false,
                },
                &mut fb,
                &mut ctx,
            );
            assert!(
                fb.chrome().overlays.overview,
                "Super+Tab via test_control should toggle overview on"
            );
        }
    }

    // drag / key / type / modifiers executor coverage

    // snapshot executor coverage

    #[test]
    #[serial_test::serial(snapshot_renderer)]
    fn snapshot_renderer_writes_pngs_and_survives_its_builder_thread() {
        // Phase `execute_snapshot_with_bogus_dir_does_not_crash`.
        // Hostile path inputs — the executor must log and return without
        // panicking: the dispatcher's `Ok` is already on the wire, so a client typo must not kill the compositor.
        fn execute_snapshot_with_bogus_dir_does_not_crash() {
            let mut fb = FrameBuilder::new((640, 360));
            let mut ctx = TestControlCtx::new();
            // Path under a nonexistent parent — File::create fails ENOENT; the executor must log and return.
            execute_planned_action(
                &PlannedAction::Snapshot {
                    path: std::path::PathBuf::from("/nonexistent/dir/x.png"),
                },
                &mut fb,
                &mut ctx,
            );
            // No panic == pass; sanity-check the FrameBuilder is still usable after
            // the failed snapshot (not left half-painted).
            fb.relayout();
            let _ = fb.paint();
        }
        // Phase `execute_snapshot_writes_a_png_at_path`.
        // Sanity: `Snapshot` against a fresh `FrameBuilder` writes a real PNG,
        // asserting the magic bytes and a non-trivial byte count (a
        // corrupted/truncated PNG would pass the magic check but be tiny).
        //
        // Skipped without a wgpu adapter (CI without llvmpipe), where the
        // executor logs and returns with no file. Serialised with the other [`SNAPSHOT_RENDERER`] tests, now a shared process-wide cache.
        fn execute_snapshot_writes_a_png_at_path() {
            let tmpdir = tempfile::tempdir().expect("create tempdir for snapshot test");
            let path = tmpdir.path().join("test.png");
            let mut fb = FrameBuilder::new((640, 360));
            let mut ctx = TestControlCtx::new();
            execute_planned_action(
                &PlannedAction::Snapshot { path: path.clone() },
                &mut fb,
                &mut ctx,
            );
            // If the renderer wasn't available the executor logged + returned with no
            // file — treat as a soft skip, as the snapshot tests do when `OffscreenRenderer::try_new` fails.
            if !path.exists() {
                eprintln!(
                    "execute_snapshot_writes_a_png_at_path: no wgpu adapter, \
                     skipping pixel assertions"
                );
                return;
            }
            let bytes = std::fs::read(&path).expect("read snapshot PNG");
            // PNG magic: \x89PNG\r\n\x1a\n.
            assert_eq!(
                &bytes[..8],
                &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A],
                "expected PNG magic at start of {path:?}"
            );
            assert!(
                bytes.len() > 1024,
                "expected non-trivial PNG output (>1KB); got {} bytes",
                bytes.len()
            );
        }
        // Phase `snapshot_renderer_outlives_the_thread_that_built_it`.
        // The snapshot renderer must outlive the thread that built it — see
        // [`SNAPSHOT_RENDERER`] for why a per-thread cache aborts the process. An
        // abort can't be observed from inside the binary it kills, so this
        // asserts the preventing property instead: a snapshot on a thread that
        // then exits must leave its renderer behind, so the next snapshot at the
        // same viewport builds nothing new. A per-thread cache fails this on the count, before it can abort anything.
        fn snapshot_renderer_outlives_the_thread_that_built_it() {
            use std::sync::atomic::Ordering;

            fn take_snapshot() -> bool {
                let tmpdir = tempfile::tempdir().expect("create tempdir for snapshot test");
                let path = tmpdir.path().join("shot.png");
                let mut fb = FrameBuilder::new((640, 360));
                let mut ctx = TestControlCtx::new();
                execute_planned_action(
                    &PlannedAction::Snapshot { path: path.clone() },
                    &mut fb,
                    &mut ctx,
                );
                path.exists()
            }

            // Warm the cache on a thread that then exits — with a per-thread cache, this join is where the process died.
            let warmed = std::thread::spawn(take_snapshot)
                .join()
                .expect("the snapshot thread should not panic");
            if !warmed {
                eprintln!(
                    "snapshot_renderer_outlives_the_thread_that_built_it: \
                     no wgpu adapter, skipping"
                );
                return;
            }

            let built = SNAPSHOT_RENDERERS_BUILT.load(Ordering::Relaxed);
            assert!(
                take_snapshot(),
                "a snapshot on this thread should still write a PNG"
            );
            assert_eq!(
                SNAPSHOT_RENDERERS_BUILT.load(Ordering::Relaxed),
                built,
                "the renderer built on the now-dead thread must be reused, not \
                 rebuilt — a per-thread cache is what aborts the suite"
            );
        }
        // Phase `execute_snapshot_writes_a_png_at_path` (defined above).
        execute_snapshot_writes_a_png_at_path();
        // Phase `snapshot_renderer_outlives_the_thread_that_built_it` (defined above).
        snapshot_renderer_outlives_the_thread_that_built_it();
        // Phase `execute_snapshot_with_bogus_dir_does_not_crash` (defined above).
        execute_snapshot_with_bogus_dir_does_not_crash();
    }

    #[test]
    fn native_touch_on_a_tab_pill_carries_the_window_group() {
        use crate::shell::state::{
            AppKind, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
            WorkspaceId, WorkspaceTree,
        };

        let group = |id: u64| WindowGroup {
            id,
            tabs: vec![Tab {
                id: id + 100,
                title: format!("Touch window {id}"),
                tile: TileContent::Stub {
                    kind: AppKind::Other("Test".into()),
                    label: format!("Touch window {id}"),
                },
                app_title: None,
            }],
            active_tab: id + 100,
            width: WindowWidth::Fraction(250),
            maximized: false,
            focus_history: Vec::new(),
        };
        let ws_id = WorkspaceId(7);
        let strip = StripId(77);
        let mut ws = Workspace::new(ws_id, "Touch desk");
        ws.strips = vec![Strip {
            id: strip,
            name: "Main".into(),
            window_groups: vec![group(1), group(2), group(3)],
        }];
        let mut tree = WorkspaceTree::default();
        tree.desktop.workspaces = vec![ws];
        tree.desktop.active = ws_id;
        tree.desktop.active_strip.insert(ws_id, strip);
        let mut fb = FrameBuilder::with_tree(tree, (1472, 920));
        fb.relayout();
        let rects = fb.widget_registry();
        let from = rects["tab:Touch window 1"].center();
        let last = rects["tab:Touch window 3[3]"].center();
        let to = vello::kurbo::Point::new(last.x + 400.0, from.y + 40.0);
        let mut ctx = TestControlCtx::new();
        // A vertical lift selects window movement; an immediate horizontal
        // swipe intentionally pans the strip instead.
        for (phase, point) in [
            (TouchPhaseKind::Down, from),
            (
                TouchPhaseKind::Motion,
                vello::kurbo::Point::new(from.x, to.y),
            ),
            (TouchPhaseKind::Motion, to),
            (TouchPhaseKind::Up, to),
        ] {
            execute_planned_action(
                &PlannedAction::Touch {
                    device: "native-frame-test".into(),
                    slot: 0,
                    phase,
                    x: point.x,
                    y: point.y,
                },
                &mut fb,
                &mut ctx,
            );
        }
        // The normal compositor model tick harvests the row's completed
        // drop into the authoritative workspace tree.
        fb.pump_apps(&mut ctx.doc, &ctx.root_obj);
        let chrome = fb.chrome();
        let groups = &chrome.tree().desktop.workspaces[0].strips[0].window_groups;
        assert_eq!(
            groups.iter().map(|group| group.id).collect::<Vec<_>>(),
            [2, 3, 1]
        );
        assert_eq!(
            groups[2].tabs[0].id, 101,
            "the existing window moved intact"
        );
    }

    // state_dump

    #[test]
    fn state_dump_reads_the_live_wayland_seat_without_input_content() {
        let fb = FrameBuilder::new((800, 600));
        let mut ctx = TestControlCtx::new();
        let event_loop: EventLoop<'static, State> = EventLoop::try_new().expect("calloop");
        let display: Display<State> = Display::new().expect("display");
        let mut state = State::new(event_loop.handle(), display.handle());
        let lock = LockController::new(lock_auth::verifier_from_env(), "fixture");
        let (response, actions) = dispatch_control_command(
            test_control::Command::StateDump {
                include_history: false,
            },
            &fb,
            &mut ctx,
            &mut state,
            &lock,
        );
        let test_control::Response::State { chrome, .. } = response else {
            panic!("state dump must answer with native state");
        };
        assert!(actions.is_empty(), "observing focus must not change it");
        assert_eq!(
            chrome["seat_focus"],
            serde_json::json!({
                "keyboard": null,
                "routed_keyboard_surface": null,
                "pointer_observable": true,
                "pointer": null,
                "routed_pointer_surface": null,
                "pointer_grab": {"kind": "none", "surface": null},
                "pointer_buttons_down": 0,
                "text_input": null,
                "text_input_active": false,
                "input_method_connected": false,
                "input_method_keyboard_grabbed": false,
                "input_method_preedit_active": false,
                "on_screen_keyboard_surface": null,
            })
        );
    }

    /// The state dump says who asks for animation frames, and asking must
    /// not trip over the chrome the dump itself is reading: the top bar
    /// borrows it to answer whether it needs drawing, and a dump that held
    /// the borrow while asking aborted the compositor.
    #[test]
    fn state_dump_names_what_animates_with_the_top_bar_drawn() {
        use crate::shell::state::{
            AppKind, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
            WorkspaceId, WorkspaceTree,
        };
        let group = |id: u64, title: &str| WindowGroup {
            id,
            tabs: vec![Tab {
                id: id + 1,
                title: title.into(),
                tile: TileContent::Stub {
                    kind: AppKind::Other("Test".into()),
                    label: title.into(),
                },
                app_title: None,
            }],
            active_tab: id + 1,
            focus_history: Vec::new(),
            width: WindowWidth::Normal,
            maximized: false,
        };
        let mut tree = WorkspaceTree::default();
        let mut workspace = Workspace::new(WorkspaceId(73), "Animating");
        workspace.strips.push(Strip::new(StripId(74), "Main"));
        workspace.strips[0].window_groups.push(group(75, "Open"));
        tree.desktop.workspaces.push(workspace);
        tree.set_active(WorkspaceId(73));
        // One instant for the whole frame: the window opening below is still
        // opening when the dump asks, however long the machine takes.
        let at = ui::time::now();
        ui::time::testing::with_monotonic_time(at, || {
            let mut fb = FrameBuilder::with_handle(tree.shared(), false, (1600, 900));
            let _ = fb.paint();
            // A window opens: its slot grows from nothing, so the content
            // asks for frames and the dump walks the content tree, top bar
            // included.
            fb.with_chrome(|chrome| {
                chrome.tree_mut().desktop.workspaces[0].strips[0]
                    .window_groups
                    .push(group(77, "Opening"));
                chrome.bump();
            });
            let mut doc = automerge::AutoCommit::new();
            fb.pump_apps(&mut doc, &automerge::ROOT);
            fb.relayout();
            let _ = fb.paint();
            let state = FrameBuilderControlState {
                fb: &fb,
                doc: None,
                shares: None,
                lock: None,
                seat: None,
                resolved: None,
            };
            let animating = state.chrome_state_json()["animating"].clone();
            assert!(
                animating
                    .as_array()
                    .is_some_and(
                        |owners| owners.iter().any(|owner| owner
                            .as_str()
                            .is_some_and(|owner| owner.starts_with("content/")
                                && owner.contains("WindowGroup")))
                    ),
                "the dump names the opening window: {animating}"
            );
        });
    }

    /// `chrome.displays` names what is painted: every connected display and
    /// the groups on it, including the one display left after an unplug.
    #[test]
    fn state_dump_lists_the_remaining_display_after_an_unplug() {
        use crate::compositor::backend::{combined_geometry, headless, output_geometry};
        use crate::shell::state::{
            AppKind, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
            WorkspaceId, WorkspaceTree,
        };

        let group = |id: u64| WindowGroup {
            id,
            tabs: vec![Tab {
                id: id + 100,
                title: format!("W{id}"),
                tile: TileContent::Stub {
                    kind: AppKind::Other("Test".into()),
                    label: format!("W{id}"),
                },
                app_title: None,
            }],
            active_tab: id + 100,
            width: WindowWidth::Fraction(600),
            maximized: false,
            focus_history: Vec::new(),
        };
        let ws_id = WorkspaceId(7);
        let strip = StripId(77);
        let mut ws = Workspace::new(ws_id, "Desk");
        ws.strips = vec![Strip {
            id: strip,
            name: "Main".into(),
            window_groups: vec![group(1), group(2)],
        }];
        let mut tree = WorkspaceTree::default();
        tree.desktop.workspaces = vec![ws];
        tree.desktop.active = ws_id;
        tree.desktop.active_strip.insert(ws_id, strip);

        let outputs = headless::output_fixture("docked-laptop").expect("fixture");
        let mut fb = FrameBuilder::with_tree(tree, combined_geometry(&outputs));
        fb.set_output_geometry(output_geometry(&outputs));
        fb.relayout();
        assert!(
            fb.chrome_handle()
                .borrow()
                .tree_mut()
                .move_group_to_display(2, 1),
            "group 2 moves to the second display"
        );
        fb.relayout();
        let displays = |fb: &FrameBuilder| {
            FrameBuilderControlState {
                fb,
                doc: None,
                shares: None,
                lock: None,
                seat: None,
                resolved: None,
            }
            .chrome_state_json()["displays"]
                .clone()
        };
        let both = displays(&fb);
        assert_eq!(both[0]["groups"], serde_json::json!([1]), "{both:#}");
        assert_eq!(both[1]["groups"], serde_json::json!([2]), "{both:#}");

        // Unplug the second display: its group moves onto the first, and the
        // dump says so although the desktop is no longer partitioned.
        fb.set_output_geometry(output_geometry(&outputs[..1]));
        fb.relayout();
        let one = displays(&fb);
        assert_eq!(
            one,
            serde_json::json!([{"id": 0, "primary": true, "focused": true, "groups": [1, 2]}]),
            "the remaining display holds every group"
        );

        // A device that has only ever had one display carries no partition;
        // its display still holds the whole strip.
        let mut single = FrameBuilder::new((1600, 900));
        single.set_output_geometry(output_geometry(&outputs[..1]));
        single.relayout();
        let only = displays(&single);
        assert_eq!(only.as_array().map(Vec::len), Some(1), "{only:#}");
        assert_eq!(only[0]["primary"], true, "{only:#}");
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn state_dump_and_action_enumeration_describe_chrome_and_workspaces() {
        // Phase `state_dump_chrome_includes_overlays_field`.
        // `chrome_state_json` returns an object with an `overlays` key carrying
        // the four flags — matches what `tilo-control state-dump` queries, so a regression here silently breaks the CLI.
        {
            let fb = FrameBuilder::new((1600, 900));
            let state = FrameBuilderControlState {
                fb: &fb,
                doc: None,
                shares: None,
                lock: None,
                seat: None,
                resolved: None,
            };
            let chrome = state.chrome_state_json();
            let overlays = chrome
                .get("overlays")
                .expect("chrome_state_json should carry `overlays` key");
            // The four bool flags should be reachable through the `overlays`
            // sub-object — addressing by string mirrors how the CLI/harness reads them.
            for key in ["overview", "quick_settings", "ws_settings", "collaborators"] {
                assert!(
                    overlays
                        .get(key)
                        .and_then(serde_json::Value::as_bool)
                        .is_some(),
                    "overlays should carry boolean `{key}`; got {overlays:?}"
                );
            }
        }
        // Phase `state_dump_identifies_the_exact_nested_search_resource`.
        {
            let fb = FrameBuilder::new((1600, 900));
            fb.chrome_handle().borrow_mut().search_resource_scope =
                Some(crate::shell::chrome_state::SearchResourceScope {
                    peer_id: 0x42,
                    cluster_id: "room:design".into(),
                    app_id: "so.tilo.reader".into(),
                    resource: "book:architecture/chapter:7".into(),
                    label: "Rendering and state".into(),
                    kind: String::new(),
                });
            let state = FrameBuilderControlState {
                fb: &fb,
                doc: None,
                shares: None,
                lock: None,
                seat: None,
                resolved: None,
            };

            let scope = &state.chrome_state_json()["search_resource_scope"];
            assert_eq!(scope["peer_id"], 0x42);
            assert_eq!(scope["cluster_id"], "room:design");
            assert_eq!(scope["app_id"], "so.tilo.reader");
            assert_eq!(scope["resource"], "book:architecture/chapter:7");
            assert_eq!(scope["label"], "Rendering and state");
        }
        // Phase `action_enumeration_preserves_directory_resource_hierarchy`.
        {
            use crate::shell::collab_broker_capnp::ActionDirectoryRoot;
            use crate::shell::search::providers::live_app_actions::fixture_with_directory_page;

            let mut fb = FrameBuilder::new((800, 600));
            let (room, actor) = {
                let chrome = fb.chrome();
                let room = chrome.tree().desktop.active_workspace().unwrap().room();
                (room.to_string(), chrome.local_user_for_room(&room))
            };
            fb.chrome()
                .tree_handle()
                .write()
                .desktop
                .active_workspace_mut()
                .unwrap()
                .set_owner(actor.clone());
            let broker = crate::shell::broker_services::test_broker_with_membership(
                fb.chrome().tree_handle(),
            );
            broker.replace_action_directory_roots_for_test(vec![ActionDirectoryRoot {
                peer_id: 4096,
                app_id: "dev.tilo.calculator".into(),
                cluster_id: room.clone(),
                record_id: "calculator-document".into(),
            }]);
            let page = tilo_actions::remote::CataloguePage {
                revision: "revision-3".into(),
                rows: vec![tilo_actions::remote::CatalogueRow {
                    provider: "4096".into(),
                    app_id: "dev.tilo.calculator".into(),
                    parent_resource: String::new(),
                    resource: tilo_actions::remote::CatalogueResource {
                        id: "calculator.graph:curve2d:stable-17".into(),
                        kind: "calculator.graph".into(),
                        label: "A presentation label without routing metadata".into(),
                        presentation: None,
                    },
                    action: Some(tilo_actions::ActionDescriptor {
                        semantic_identity: None,
                        address: tilo_actions::ActionAddress::new(
                            "calculator.graph:curve2d:stable-17",
                            "toggle-visibility",
                        ),
                        resource_kind: "calculator.graph".into(),
                        resource_label: "A presentation label without routing metadata".into(),
                        action_label: "Hide".into(),
                        description: Some("Do not infer identity from this text".into()),
                        locus: tilo_actions::ExecutionLocus::default(),
                        kind: tilo_actions::OperationKind::View,
                        contract: tilo_actions::ActionContract::view(),
                        input: tilo_actions::InputSpec::None,
                        params: Vec::new(),
                        view: None,
                    }),
                    has_children: false,
                    visibility: None,
                }],
                exhausted: true,
                ..tilo_actions::remote::CataloguePage::default()
            };
            let provider = fixture_with_directory_page(broker, 4096, actor, page);
            crate::shell::overlay_host::install_fixture_search_provider(fb.host_mut(), provider);
            let doc = AutoCommit::new();
            let state = FrameBuilderControlState {
                fb: &fb,
                doc: Some((&doc, &automerge::ROOT)),
                shares: None,
                lock: None,
                seat: None,
                resolved: None,
            };
            let answer = state.enumerate_actions("").expect("enumerated actions");
            let row = answer["actions"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["provider"] == "app-actions" && row["title"] == "Hide")
                .expect("real directory action must be visible");
            assert_eq!(row["application"], "dev.tilo.calculator");
            assert_eq!(row["room"], room);
            assert_eq!(row["record"], "calculator-document");
            assert_eq!(row["resource"], "calculator.graph:curve2d:stable-17");
            assert_eq!(row["operation"], "toggle-visibility");
            assert_eq!(
                row["id"],
                "directory\u{1f}4096\u{1f}revision-3\u{1f}calculator.graph:curve2d:stable-17\u{1f}toggle-visibility"
            );
        }
        // Phase `state_dump_workspace_tree_includes_workspaces_array`.
        // `workspace_tree_json` exposes a `workspaces` array on the bootstrap
        // layout — the CLI's `state-dump` smoke depends on it as the consumer's entrypoint to the workspace list.
        {
            let fb = FrameBuilder::new((1600, 900));
            let state = FrameBuilderControlState {
                fb: &fb,
                doc: None,
                shares: None,
                lock: None,
                seat: None,
                resolved: None,
            };
            let tree = state.workspace_tree_json(false).to_value();
            let workspaces = tree
                .get("workspaces")
                .and_then(|v| v.as_array())
                .expect("workspace_tree_json should carry `workspaces` array");
            // `FrameBuilder::new` uses the demo tree, matching the `bootstrap_demo`
            // assertions in `shell/state.rs`.
            assert!(
                !workspaces.is_empty(),
                "bootstrap workspace tree should not be empty"
            );
        }
        // Phase `dispatch_state_dump_against_frame_builder_returns_state`.
        // Dispatching `Command::StateDump` returns `Response::State` with no
        // actions. Integration round-trip lives in `tests/control_socket_test.rs`; this smoke catches regressions needing no spawned compositor.
        {
            let fb = FrameBuilder::new((1600, 900));
            let state = FrameBuilderControlState {
                fb: &fb,
                doc: None,
                shares: None,
                lock: None,
                seat: None,
                resolved: None,
            };
            let (resp, actions) = test_control::dispatch_command(
                &test_control::Command::StateDump {
                    include_history: false,
                },
                &state,
            );
            assert!(actions.is_empty());
            let test_control::Response::State {
                workspace_tree,
                chrome,
                peers: _,
                sharing: _,
            } = resp
            else {
                panic!("expected Response::State; got something else");
            };
            // Sanity: both blobs are non-null and shaped like objects.
            assert!(
                workspace_tree.to_value().is_object(),
                "workspace_tree should be an object"
            );
            assert!(chrome.is_object(), "chrome should be an object");
        }
    }

    /// A page of the live listing is a slice of the whole one, in its order,
    /// and following `next_cursor` walks all of it.
    #[test]
    fn the_live_listing_pages_through_every_row_once() {
        let fb = FrameBuilder::new((1600, 900));
        let doc = AutoCommit::new();
        let state = FrameBuilderControlState {
            fb: &fb,
            doc: Some((&doc, &automerge::ROOT)),
            shares: None,
            lock: None,
            seat: None,
            resolved: None,
        };
        let whole = state.enumerate_actions("").expect("the whole listing");
        let ids = |answer: &serde_json::Value| -> Vec<String> {
            answer["actions"]
                .as_array()
                .expect("rows")
                .iter()
                .map(|row| format!("{}:{}", row["provider"], row["id"]))
                .collect()
        };
        let expected = ids(&whole);
        assert!(expected.len() > 3, "the demo desktop exports actions");
        let mut seen = Vec::new();
        let mut cursor = None;
        loop {
            let page = state
                .enumerate_actions_with_programmatic(
                    "",
                    false,
                    &test_control::ListingRequest {
                        limit: Some(3),
                        cursor: cursor.take(),
                        detail: test_control::ListingDetail::Compact,
                        ..test_control::ListingRequest::default()
                    },
                )
                .expect("a page");
            assert_eq!(page["total"], expected.len());
            assert!(page["actions"].as_array().expect("rows").len() <= 3);
            assert!(
                page["actions"]
                    .as_array()
                    .expect("rows")
                    .iter()
                    .all(|row| row.get("contract").is_none() && row.get("params").is_some())
            );
            seen.extend(ids(&page));
            match page["next_cursor"].as_str() {
                Some(next) => cursor = Some(next.to_owned()),
                None => break,
            }
        }
        assert_eq!(seen, expected);
    }

    /// The state dump is the desktop as it is: its size does not grow with
    /// the effect receipts every action appends, which stay one explicit
    /// `include_history` away. `windows` reads the same live tree.
    #[test]
    fn state_dump_leaves_history_out_unless_asked_and_windows_reads_the_live_tree() {
        let fb = FrameBuilder::new((1600, 900));
        let record_receipts = |count: u128| {
            let chrome = fb.chrome();
            let tree_handle = chrome.tree_handle();
            drop(chrome);
            let mut tree = tree_handle.write();
            let workspace = tree
                .desktop
                .active_workspace_mut()
                .expect("active workspace");
            workspace.effect_receipts = (0..count)
                .map(|n| crate::shell::state::WorkspaceEffectReceipt {
                    invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(n)),
                    actor: "owner".into(),
                    context: tilo_actions::ActionContext::default(),
                    status: tilo_actions::EffectReceiptStatus::Succeeded,
                    completed_at: 42,
                })
                .collect();
        };
        let state = FrameBuilderControlState {
            fb: &fb,
            doc: None,
            shares: None,
            lock: None,
            seat: None,
            resolved: None,
        };
        // The tree as the reply carries it; the chrome beside it is not history.
        let dump = |include_history| {
            let (resp, _) = test_control::dispatch_command(
                &test_control::Command::StateDump { include_history },
                &state,
            );
            let test_control::Response::State { workspace_tree, .. } = resp else {
                panic!("state_dump answers state");
            };
            workspace_tree.get().to_owned()
        };
        record_receipts(0);
        let before = dump(false);
        record_receipts(10_000);
        let after = dump(false);
        assert_eq!(
            before.len(),
            after.len(),
            "the default dump must not grow with history"
        );
        assert!(!after.contains("effect_receipts"), "{after}");
        let full = serde_json::json!({
            "state": { "workspace_tree": serde_json::from_str::<serde_json::Value>(&dump(true)).unwrap() }
        });
        let receipts = full["state"]["workspace_tree"]["workspaces"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|workspace| workspace["effect_receipts"].as_array())
            .map(Vec::len)
            .sum::<usize>();
        assert_eq!(receipts, 10_000, "include_history keeps every receipt");

        let (resp, actions) =
            test_control::dispatch_command(&test_control::Command::Windows, &state);
        assert!(actions.is_empty());
        let test_control::Response::Answer(answer) = resp else {
            panic!("windows answers rows");
        };
        let chrome = fb.chrome();
        let tree = chrome.tree();
        let tabs = tree
            .desktop
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .map(|group| group.tabs.len())
            .sum::<usize>();
        assert_eq!(answer["windows"].as_array().unwrap().len(), tabs);
        assert_eq!(
            answer["windows"],
            serde_json::Value::Array(test_control::window_rows(&tree.desktop))
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn state_dump_exposes_privacy_relevant_state_without_secrets() {
        // Phase `state_dump_carries_the_roster_it_is_asked_about`.
        // The state dump has to carry the roster — how a harness observes who
        // the shell believes is present. `Workspace::collaborators` is
        // `#[serde(skip_deserializing)]` (a restarted shell must not paint gone
        // sessions), but write stays open: closing it with a plain `skip` would silently empty the roster from the socket, looking like agreement.
        fn state_dump_carries_the_roster_it_is_asked_about() {
            let fb = FrameBuilder::new((1600, 900));
            {
                let chrome = fb.chrome();
                // Drop the `ChromeState` borrow before locking the tree — nothing
                // here needs it once the handle is cloned out.
                let tree_handle = chrome.tree_handle();
                drop(chrome);
                let mut tree = tree_handle.write();
                let room = tree
                    .desktop
                    .active_workspace()
                    .expect("active workspace")
                    .room();
                let noted = tree.admit_collaborator_in_room(
                    room,
                    crate::shell::state::Collaborator::new("alice", "alice").with_session(0x51),
                );
                drop(tree);
                assert!(noted);
            }
            let state = FrameBuilderControlState {
                fb: &fb,
                doc: None,
                shares: None,
                lock: None,
                seat: None,
                resolved: None,
            };
            let tree = state.workspace_tree_json(false).to_value();
            let found = tree
                .get("workspaces")
                .and_then(|v| v.as_array())
                .expect("workspaces array")
                .iter()
                .filter_map(|w| w.get("collaborators")?.as_array())
                .flatten()
                .filter_map(|c| c.get("id")?.as_str())
                .any(|id| id == "alice");
            assert!(
                found,
                "a peer the shell has admitted must appear in the control \
                 socket's state dump; without it a harness cannot tell an \
                 empty roster from an unreported one. Got: {tree}"
            );
        }
        // Phase `state_dump_exposes_retry_ready_lock_auth_without_password_contents`.
        {
            let fb = FrameBuilder::new((1600, 900));
            let mut lock = LockController::new(
                std::sync::Arc::new(crate::compositor::lock_auth::FixedVerifier::new("correct")),
                "alice",
            );
            lock.push_str("secret-value");
            let state = FrameBuilderControlState {
                fb: &fb,
                doc: None,
                shares: None,
                lock: Some(&lock),
                seat: None,
                resolved: None,
            };
            let chrome = state.chrome_state_json();
            let auth = chrome
                .get("lock_auth")
                .expect("native state dump must expose lock authentication status");
            assert_eq!(auth.get("verifying"), Some(&serde_json::Value::Bool(false)));
            assert_eq!(
                auth.get("password_len").and_then(serde_json::Value::as_u64),
                Some(12)
            );
            assert_eq!(auth.get("error"), Some(&serde_json::Value::Null));
            assert!(
                !chrome.to_string().contains("secret-value"),
                "state dump must never disclose the password buffer"
            );
        }
        // Phase `state_dump_exposes_own_presence_privacy_and_follow_state`.
        {
            let fb = FrameBuilder::new((1600, 900));
            let state = FrameBuilderControlState {
                fb: &fb,
                doc: None,
                shares: None,
                lock: None,
                seat: None,
                resolved: None,
            };

            let presence = &state.chrome_state_json()["presence"];
            assert!(
                matches!(
                    presence["status_sharing"].as_str(),
                    Some("off" | "manual" | "automatic")
                ),
                "presence privacy must be an explicit wire value: {presence}"
            );
            assert!(
                presence["own_status"].is_null() || presence["own_status"].is_string(),
                "manual status must remain inspectable: {presence}"
            );
            assert!(presence["following"].is_null());
        }
        // Phase `state_dump_carries_the_roster_it_is_asked_about` (defined above).
        state_dump_carries_the_roster_it_is_asked_about();
        // Phase `state_dump_attributes_agent_screen_exposure_to_its_exact_target`.
        // Agents and automation use the same semantic inspection surface as
        // accessibility tooling. Screen exposure therefore has to retain the
        // viewer's principal kind, exact target, and control authority rather
        // than degrading to an anonymous "sharing" boolean.
        {
            use crate::compositor::tab_binding::{self, Placement};
            use crate::network::streaming::{ShareRegistry, StreamingShare};

            const AGENT_PEER: u64 = 0xA9;
            const SURFACE: u64 = 71_900;

            let fb = FrameBuilder::new((1600, 900));
            {
                let chrome = fb.chrome();
                let tree_handle = chrome.tree_handle();
                drop(chrome);
                let mut tree = tree_handle.write();
                let workspace = tree
                    .desktop
                    .active_workspace_mut()
                    .expect("active workspace");
                workspace.set_owner("owner".to_string());
                workspace.admit_agent_preset(
                    "release-notes",
                    tilo_action_policy::roles::AgentPreset::Operator,
                    &tilo_action_policy::Subject::person("owner".to_string()),
                );
                workspace.collaborators.push(
                    crate::shell::state::Collaborator::new("release-notes", "Release Notes Agent")
                        .with_session(AGENT_PEER),
                );
                tab_binding::adopt_toplevel_at(
                    &mut tree,
                    SURFACE,
                    Some("org.libreoffice.Writer".into()),
                    Placement::NewGroup,
                );
                let tab = tree
                    .desktop
                    .active_workspace_mut()
                    .into_iter()
                    .flat_map(|workspace| &mut workspace.strips)
                    .flat_map(|strip| &mut strip.window_groups)
                    .flat_map(|group| &mut group.tabs)
                    .find(|tab| {
                        matches!(
                            tab.tile,
                            crate::shell::state::TileContent::Foreign { surface_id, .. }
                                if surface_id == SURFACE
                        )
                    })
                    .expect("adopted foreign tab");
                tab.title = "Quarterly Plan".to_string();
                drop(tree);
            }

            let mut shares = ShareRegistry::new();
            shares.insert(AGENT_PEER, SURFACE, StreamingShare::new());
            let state = FrameBuilderControlState {
                fb: &fb,
                doc: None,
                shares: Some(&shares),
                lock: None,
                seat: None,
                resolved: None,
            };
            let rows = state
                .sharing_json()
                .get("exposures")
                .and_then(serde_json::Value::as_array)
                .cloned()
                .expect("state dump exposure rows");
            assert_eq!(rows.len(), 1);
            let row = &rows[0];
            assert_eq!(row["name"], "Release Notes Agent");
            assert_eq!(row["kind"], "agent");
            assert_eq!(row["target"], "Quarterly Plan");
            assert_eq!(row["can_see"], true);
            assert_eq!(row["can_control"], true);
        }
    }

    // session lock chord / unlock detection

    #[test]
    fn native_consent_bootstrap_and_onboarding_publish_actual_state() {
        const TEST: &str = "compositor::native::tests::native_consent_bootstrap_and_onboarding_publish_actual_state";
        run_isolated_native_consent_test(TEST, || {
            let mut frame_builder = FrameBuilder::new((800, 600));
            let broker = spawn_session_collab_broker(&frame_builder.chrome_handle(), None)
                .expect("spawn native collaboration broker");
            let session = SessionManager::new(
                IdleConfig {
                    blank_timeout: None,
                    lock_timeout: None,
                    suspend_timeout: None,
                },
                Instant::now(),
            );
            let lifecycle = install_native_session_lifecycle(
                &broker,
                &session,
                frame_builder.host().onboarding_active(),
            );
            assert_eq!(
                lifecycle.snapshot().expect("native bootstrap").liveness,
                crate::shell::collab_broker_capnp::LocalSessionLiveness::Active
            );

            let fixture = native_consent_fixture_root();
            frame_builder.enable_onboarding(fixture.join("onboarding"));
            publish_native_session_state(
                Some(&lifecycle),
                &session,
                frame_builder.host().onboarding_active(),
            );
            assert_eq!(
                lifecycle
                    .snapshot()
                    .expect("onboarding revocation")
                    .liveness,
                crate::shell::collab_broker_capnp::LocalSessionLiveness::Inactive
            );
            drop(broker);
            wait_for_local_session_worker_teardown(&lifecycle);
        });
    }

    #[test]
    fn native_consent_lock_is_synchronous_and_unlock_uses_fresh_generation() {
        const TEST: &str = "compositor::native::tests::native_consent_lock_is_synchronous_and_unlock_uses_fresh_generation";
        run_isolated_native_consent_test(TEST, || {
            use crate::compositor::backend::{Backend, BackendEvent, Output};
            use vello::Scene;

            #[derive(Default)]
            struct SpyBackend {
                outputs: Vec<Output>,
            }
            impl Backend for SpyBackend {
                fn submit_frame(&mut self, _scene: &Scene, _output: OutputId) {}
                fn poll_events(&mut self, _cb: &mut dyn FnMut(BackendEvent)) {}
                fn outputs(&self) -> &[Output] {
                    &self.outputs
                }
            }

            let frame_builder = FrameBuilder::new((800, 600));
            let broker = spawn_session_collab_broker(&frame_builder.chrome_handle(), None)
                .expect("spawn native collaboration broker");
            let session = SessionManager::new(
                IdleConfig {
                    blank_timeout: None,
                    lock_timeout: None,
                    suspend_timeout: None,
                },
                Instant::now(),
            );
            let lifecycle = install_native_session_lifecycle(
                &broker,
                &session,
                frame_builder.host().onboarding_active(),
            );
            let active = lifecycle.snapshot().expect("active native session");
            let mut backend = SpyBackend::default();

            apply_session_transition(
                SessionTransition::Locked,
                &mut backend,
                None,
                Some(&lifecycle),
                &mut ScreenDimmer::default(),
                None,
            );
            let locked = lifecycle.snapshot().expect("synchronous lock revoke");
            assert_eq!(
                locked.liveness,
                crate::shell::collab_broker_capnp::LocalSessionLiveness::Inactive
            );
            assert!(locked.generation > active.generation);

            apply_session_transition(
                SessionTransition::Unlocked,
                &mut backend,
                None,
                Some(&lifecycle),
                &mut ScreenDimmer::default(),
                None,
            );
            let unlocked = lifecycle.snapshot().expect("fresh unlock");
            assert_eq!(
                unlocked.liveness,
                crate::shell::collab_broker_capnp::LocalSessionLiveness::Active
            );
            assert!(unlocked.generation > locked.generation);
            assert_eq!(unlocked.epoch, active.epoch);
            drop(broker);
            wait_for_local_session_worker_teardown(&lifecycle);
        });
    }

    #[test]
    fn native_consent_deferred_install_publishes_before_continuation() {
        const TEST: &str = "compositor::native::tests::native_consent_deferred_install_publishes_before_continuation";
        run_isolated_native_consent_test(TEST, || {
            let mut deferred = true;
            assert!(take_deferred_peer_sync_activation(&mut deferred, false));
            let frame_builder = FrameBuilder::new((800, 600));
            let broker = spawn_session_collab_broker(&frame_builder.chrome_handle(), None)
                .expect("spawn deferred collaboration broker");
            let session = SessionManager::new(
                IdleConfig {
                    blank_timeout: None,
                    lock_timeout: None,
                    suspend_timeout: None,
                },
                Instant::now(),
            );
            let lifecycle = install_native_session_lifecycle(
                &broker,
                &session,
                frame_builder.host().onboarding_active(),
            );
            assert_eq!(
                lifecycle
                    .snapshot()
                    .expect("deferred native state")
                    .liveness,
                crate::shell::collab_broker_capnp::LocalSessionLiveness::Active,
                "the deferred broker must publish current native state before launch preparation"
            );
            drop(broker);
            wait_for_local_session_worker_teardown(&lifecycle);
        });
    }

    #[test]
    fn native_consent_logout_closes_before_loop_exit() {
        const TEST: &str =
            "compositor::native::tests::native_consent_logout_closes_before_loop_exit";
        run_isolated_native_consent_test(TEST, || {
            let frame_builder = FrameBuilder::new((800, 600));
            let broker = spawn_session_collab_broker(&frame_builder.chrome_handle(), None)
                .expect("spawn native collaboration broker");
            let lifecycle = broker.local_session_lifecycle();
            lifecycle.publish_native_state(false, false);
            close_native_session_authority(Some(&lifecycle));
            assert_eq!(
                lifecycle.snapshot().expect("logout close").liveness,
                crate::shell::collab_broker_capnp::LocalSessionLiveness::Closed
            );
            drop(broker);
            wait_for_local_session_worker_teardown(&lifecycle);
        });
    }

    /// A window that set its own icon reports it as its tab's artwork.
    #[test]
    fn state_dump_reports_the_artwork_each_window_tab_paints() {
        let mut fb = FrameBuilder::new((1600, 900));
        let surface = 0x51c0_0001;
        let mut tab = 0;
        fb.with_chrome(|chrome| {
            tab = crate::shell::tab_binding::adopt_toplevel(
                &mut chrome.tree_mut(),
                surface,
                Some("org.example.UnlistedGame".into()),
            );
            chrome.bump();
        });
        let pixels = vello::peniko::ImageData {
            data: vello::peniko::Blob::new(std::sync::Arc::new(vec![0; 16 * 16 * 4])),
            format: vello::peniko::ImageFormat::Rgba8,
            alpha_type: vello::peniko::ImageAlphaType::AlphaPremultiplied,
            width: 16,
            height: 16,
        };
        crate::compositor::toplevel_icon::set(
            surface,
            1,
            Some(crate::compositor::toplevel_icon::ToplevelIcon::Pixels(
                std::sync::Arc::new(vec![pixels]),
            )),
        );
        let state = FrameBuilderControlState {
            fb: &fb,
            doc: None,
            shares: None,
            lock: None,
            seat: None,
            resolved: None,
        };
        let icons = state.chrome_state_json()["tab_icons"].clone();
        crate::compositor::toplevel_icon::forget(surface);
        let row = icons
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["tab"] == tab))
            .unwrap_or_else(|| panic!("the window's tab reports artwork: {icons}"));
        assert!(
            row["icon"]
                .as_str()
                .is_some_and(|key| key.starts_with(&format!("toplevel:{surface}:"))),
            "{row}"
        );
    }
}

/// The refresh rate to advertise on a `wl_output` whose backend output
/// reports none of its own, in millihertz. Derived from the cadence the
/// compositor is actually pacing to, so client frame timing agrees with real
/// production. `None` when the backend has no display; globals then
/// advertise 60Hz rather than zero, which some toolkits divide by.
fn refresh_mhz_of(backend: &dyn crate::compositor::backend::Backend) -> Option<u32> {
    backend
        .refresh_interval()
        .and_then(crate::compositor::backend::interval_to_mhz)
}

/// Rectangles of Wayland root surfaces that are actually visible in the
/// compositor's current frame. Inactive tabs deliberately do not appear here:
/// keyboard focus and painting both follow the active tab, so associating an
/// inactive tab with an output would advertise a monitor on which it has no
/// pixels. Floating and OSK surfaces use their live widget geometry for the
/// same reason.
fn visible_surface_rects(frame_builder: &FrameBuilder) -> Vec<(u64, Rect)> {
    let mut surfaces = visible_tiled_surface_rects(frame_builder);
    surfaces.extend(frame_builder.floating_surface_rects());
    if let Some(surface) = frame_builder.fullscreen_surface_rect() {
        surfaces.push(surface);
    }
    surfaces.extend(frame_builder.transient_surface_rects());
    if let Some(surface) = frame_builder.osk_surface_rect() {
        surfaces.push(surface);
    }
    surfaces
}

/// Before `FrameBuilder::relayout`, its surface rectangles belong to the
/// preceding output geometry. They can only be compared with the backend's
/// output table when that table did not change this tick.
const fn surface_output_geometry_is_current(output_geometry_changed: bool) -> bool {
    !output_geometry_changed
}

/// Visible tiled clients are the active foreign tab of each painted group,
/// not every client that has ever been placed in that group. Keeping this on
/// the same [`client_input::foreign_tiles`] traversal used for hit testing
/// makes output membership agree with the pixels and input target after a tab
/// switch.
fn visible_tiled_surface_rects(frame_builder: &FrameBuilder) -> Vec<(u64, Rect)> {
    let chrome = frame_builder.chrome();
    let tree = chrome.tree();
    client_input::foreign_tiles(&tree, frame_builder.host())
        .into_iter()
        .map(|hit| (hit.surface_id, hit.rect))
        .collect()
}

/// Emit the diff calculated by [`surface_outputs::Memberships`] against the
/// live Smithay objects. The tracker guarantees at most one transition of a
/// given kind for `(surface, output)`; Smithay's own output object supplies a
/// second idempotence guard at the protocol boundary.
fn send_surface_output_changes(
    state: &State,
    output_globals: &output_globals::OutputGlobals,
    changes: Vec<SurfaceOutputChange>,
) {
    output_globals.send_surface_changes(&state.surface_handles, changes);
}

/// Per-output scale: push each placed client surface the preferred
/// `wp_fractional_scale` of the display it is on — a tiled toplevel by its
/// group's active tile, floating and transient surfaces by their centre.
///
/// Reads the frame builder's current tile rects, so run it again after every
/// `relayout` that precedes a configure (see [`publish_layout_to_clients`]).
/// Only changed scales reach the wire.
pub fn push_surface_scales(
    state: &mut State,
    frame_builder: &FrameBuilder,
    transient_surfaces: &[(u64, Rect)],
) {
    let mut placements: Vec<(u64, (f64, f64))> = state
        .client_input
        .placements()
        .filter_map(|(surface_id, group)| {
            let rect = frame_builder.host().active_tile_rect(group)?;
            Some((surface_id, (rect.x0, rect.y0)))
        })
        .collect();
    placements.extend(state.floating_toplevels.iter().map(|surface| {
        let center = surface.content_rect.center();
        (surface.surface_id, (center.x, center.y))
    }));
    placements.extend(state.output_presentation_placements());
    placements.extend(transient_surfaces.iter().map(|(surface_id, rect)| {
        let center = rect.center();
        (*surface_id, (center.x, center.y))
    }));
    state.update_surface_scales(&placements);
}

/// Tell clients what a fresh `relayout` changed about where they are —
/// the outputs each surface entered or left, and the scale of the one it
/// now sits on — before `foreign::flush_configures` sends the sizes that
/// layout produced.
///
/// The order is the contract: a client that sizes its buffer from a
/// configure (xwayland-satellite sizes the X window as the configured size
/// times the scale it holds at that moment) must already have the new
/// output's scale, or it renders at the previous output's scale until its
/// next configure.
pub fn publish_layout_to_clients(
    memberships: &mut SurfaceOutputMemberships,
    state: &mut State,
    frame_builder: &FrameBuilder,
    transient_surfaces: &[(u64, Rect)],
    outputs: &[Output],
    output_globals: &output_globals::OutputGlobals,
) {
    reconcile_surface_output_memberships(
        memberships,
        state,
        frame_builder,
        outputs,
        output_globals,
    );
    push_surface_scales(state, frame_builder, transient_surfaces);
}

fn reconcile_surface_output_memberships(
    memberships: &mut SurfaceOutputMemberships,
    state: &State,
    frame_builder: &FrameBuilder,
    outputs: &[Output],
    output_globals: &output_globals::OutputGlobals,
) {
    // `OutputGlobals::sync` is synchronous and precedes this call. Filtering
    // is a defensive backstop: never advance the tracker into an Enter that
    // cannot be emitted. If the handle appears on a later sync, that output
    // joins this list and reconciliation produces the still-needed Enter.
    let advertised_outputs: Vec<Output> = outputs
        .iter()
        .filter(|output| output_globals.handle(output.id).is_some())
        .cloned()
        .collect();
    if advertised_outputs.len() != outputs.len() {
        tracing::error!(
            backend_outputs = outputs.len(),
            advertised_outputs = advertised_outputs.len(),
            "wl_output registry is behind the backend output table"
        );
    }
    let changes = memberships.reconcile(&advertised_outputs, &visible_surface_rects(frame_builder));
    send_surface_output_changes(state, output_globals, changes);
}

/// What one synthetic touchpad edge means to `tracker`, as the DRM
/// runtime asks it of every libinput gesture event.
fn feed_touchpad_edge(
    tracker: &mut crate::compositor::backend::drm::gestures::GestureTracker,
    edge: crate::shell::test_control::TouchpadEdge,
) -> Option<crate::compositor::backend::drm::gestures::GestureAction> {
    use crate::shell::test_control::TouchpadEdge;
    match edge {
        TouchpadEdge::SwipeBegin { fingers } => {
            tracker.swipe_begin(fingers);
            None
        }
        TouchpadEdge::SwipeUpdate { dx, dy } => tracker.swipe_update(dx, dy),
        TouchpadEdge::SwipeEnd { cancelled } => tracker.swipe_end_with_cancel(cancelled),
        TouchpadEdge::PinchBegin { fingers } => {
            tracker.pinch_begin(fingers);
            None
        }
        TouchpadEdge::PinchUpdate { scale } => tracker.pinch_update(scale),
        TouchpadEdge::PinchEnd => {
            tracker.pinch_end();
            None
        }
    }
}

/// Perform the navigation a claimed touchpad gesture, reported `at`,
/// resolved to — a second way to ask for something the shell already does,
/// not a parallel model.
fn apply_shell_gesture(
    frame_builder: &mut FrameBuilder,
    action: crate::compositor::backend::drm::gestures::GestureAction,
    at: Instant,
) {
    use crate::compositor::backend::drm::gestures::GestureAction;
    match action {
        GestureAction::StripPanUpdate { travel, began } => {
            frame_builder
                .host_mut()
                .update_active_strip_pan(travel, began, at);
            return;
        }
        GestureAction::StripPanEnd { cancelled } => {
            frame_builder.host_mut().end_active_strip_pan(at, cancelled);
            return;
        }
        GestureAction::StripDeckUpdate { travel, began } => {
            frame_builder
                .host_mut()
                .update_strip_deck_drag(travel, began, at);
            return;
        }
        GestureAction::StripDeckEnd { cancelled } => {
            frame_builder.host_mut().end_strip_deck_drag(at, cancelled);
            return;
        }
        _ => {}
    }
    match action {
        GestureAction::SwitchWorkspace(dir) => {
            let mut switched = false;
            frame_builder.with_chrome(|st| {
                switched = st.switch_workspace(dir);
            });
            if switched {
                frame_builder
                    .host_mut()
                    .start_workspace_switch(ui::tokens::MotionSource::Gesture);
            }
        }
        GestureAction::ShowOverview => {
            frame_builder.with_chrome(|st| {
                if !st.overlays.overview {
                    st.overlays.overview = true;
                    st.bump();
                }
            });
            frame_builder
                .host_mut()
                .latch_overview_transition_from(ui::tokens::MotionSource::Gesture);
        }
        GestureAction::HideOverview => {
            frame_builder.with_chrome(|st| {
                if st.overlays.overview {
                    st.overlays.overview = false;
                    st.bump();
                }
            });
            frame_builder
                .host_mut()
                .latch_overview_transition_from(ui::tokens::MotionSource::Gesture);
        }
        // Handled (and returned on) above; a panic here would take down the
        // whole session for a gesture that already did its job.
        GestureAction::StripPanUpdate { .. }
        | GestureAction::StripPanEnd { .. }
        | GestureAction::StripDeckUpdate { .. }
        | GestureAction::StripDeckEnd { .. } => {}
    }
}

/// Resolve an xdg fullscreen request to one live backend output and paint it
/// above the normal tiled scene.  A compositor pin (the presenter view beside
/// an audience surface) wins, then the requested `wl_output`; if it was
/// removed, use the output currently containing the window's centre, then the
/// first live output as a deterministic hotplug fallback.
/// Super+F on a Tilo window covers the whole output.
/// Hosted Tilo apps are xdg clients too; sending them a protocol
/// fullscreen never sets `chrome.fullscreen_group` (W07). Maximize
/// (Super+M) keeps the top bar. A focused xdg surface with no
/// window group still takes the protocol path.
fn toggle_focused_client_fullscreen(state: &mut State, frame: &mut FrameBuilder) {
    use smithay::wayland::shell::xdg::XdgShellHandler;
    if frame.host_mut().toggle_focused_group_fullscreen() {
        return;
    }
    let toplevel = state.client_input.keyboard_focus().and_then(|surface_id| {
        state
            .xdg_shell_state
            .toplevel_surfaces()
            .iter()
            .find(|toplevel| {
                crate::compositor::state::surface_id(toplevel.wl_surface()) == surface_id
            })
            .cloned()
            .map(|toplevel| (surface_id, toplevel))
    });
    let Some((surface_id, toplevel)) = toplevel else {
        return;
    };
    if state
        .fullscreen_target
        .as_ref()
        .is_some_and(|target| target.surface_id == surface_id)
    {
        state.unfullscreen_request(toplevel);
    } else {
        state.fullscreen_request(toplevel, None);
    }
}

/// What [`publish_output_colors`] last told colour-managed clients.
#[derive(Default)]
struct PublishedOutputColors {
    described: Vec<(
        crate::compositor::backend::OutputId,
        Option<crate::wayland::color_management::OutputDescription>,
    )>,
    /// [`crate::wayland::color_management::ColorManagerState::output_objects`]
    /// at that time; `None` before the first publish.
    objects: Option<u64>,
}

/// Tell colour-managed clients what each output displays: the HDR signal of
/// an output driven in HDR, SDR otherwise. Surfaces are asked to render in
/// HDR only while every output is HDR, since an SDR output can show an HDR
/// surface only tone-mapped.
///
/// Runs on every wakeup but tells clients only when something changed: an
/// output's description (hotplug, an HDR toggle) or a new output object
/// that has not heard its output's description yet.
fn publish_output_colors(
    state: &State,
    backend: &dyn crate::compositor::backend::Backend,
    published: &mut PublishedOutputColors,
) {
    use crate::wayland::color_management::OutputDescription;
    let outputs = backend.outputs();
    // Read before `update_outputs` takes the object list: an object created
    // after this read is counted next time and published then.
    let objects = state.color_manager.output_objects();
    let describe = |output: &crate::compositor::backend::Output| {
        backend
            .output_hdr(output.id)
            .map(|hdr| OutputDescription::hdr_pq(&hdr.display, hdr.sdr_white_nits))
    };
    let unchanged = published.objects == Some(objects)
        && published.described.len() == outputs.len()
        && outputs
            .iter()
            .zip(&published.described)
            .all(|(output, (id, description))| {
                output.id == *id && describe(output) == *description
            });
    if unchanged {
        return;
    }
    published.described = outputs
        .iter()
        .map(|output| (output.id, describe(output)))
        .collect();
    published.objects = Some(objects);
    let described = published.described.as_slice();
    state.color_manager.update_outputs(|resource| {
        let id = output_globals::output_id_of(resource)?;
        described
            .iter()
            .find_map(|(output, description)| (*output == id).then_some(*description))
            .flatten()
    });
    let preferred = match described {
        [(_, Some(first)), rest @ ..] if rest.iter().all(|(_, d)| d.is_some()) => *first,
        _ => OutputDescription::sdr(),
    };
    state.color_manager.set_preferred(preferred);
}

/// Place the fullscreen client, if any, and return the output it covers.
fn reconcile_fullscreen_overlay(
    state: &mut State,
    frame: &mut FrameBuilder,
    globals: &output_globals::OutputGlobals,
    outputs: &[crate::compositor::backend::Output],
) -> Option<crate::compositor::backend::OutputId> {
    let Some(target) = state.fullscreen_target.as_mut() else {
        frame.set_fullscreen_surface(None);
        return None;
    };
    let requested_id = target.pinned_output.or_else(|| {
        target
            .requested_output
            .as_ref()
            .and_then(|output| globals.id_for_resource(output))
    });
    let current_rect = frame.foreign_surface_rect(target.surface_id);
    let centre = current_rect.map(|rect| rect.center());
    let output = select_fullscreen_output(requested_id, centre, outputs);
    let Some(output) = output else {
        frame.set_fullscreen_surface(None);
        return None;
    };
    // Logical, like every other configure: a 2x output's fullscreen client
    // is asked for half its pixels and renders them at scale 2.
    let size = size_to_signed(output.logical_size());
    // A compositor-placed presenter view is sized through its pinned
    // placement by the ordinary configure pass, never told it is fullscreen.
    if target.pinned_output.is_none() && target.configured_size != Some(size) {
        target.surface.with_pending_state(|pending| {
            pending.states.set(
                smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Fullscreen,
            );
            pending.size = Some(size.into());
        });
        target.surface.send_configure();
        target.configured_size = Some(size);
    }
    if target.pinned_output.is_none() {
        crate::shell::foreign::store().note_fullscreen_extent(target.surface_id, size);
    }
    let rect = output.logical_rect();
    frame.set_client_fullscreen_surface(Some((target.surface_id, "Fullscreen".to_string(), rect)));
    Some(output.id)
}

fn select_fullscreen_output(
    requested: Option<crate::compositor::backend::OutputId>,
    current_center: Option<vello::kurbo::Point>,
    outputs: &[crate::compositor::backend::Output],
) -> Option<&crate::compositor::backend::Output> {
    requested
        .and_then(|id| outputs.iter().find(|output| output.id == id))
        .or_else(|| {
            current_center.and_then(|point| {
                outputs
                    .iter()
                    .find(|output| output.logical_rect().contains(point))
            })
        })
        .or_else(|| {
            outputs
                .iter()
                .find(|output| output.size.0 > 0 && output.size.1 > 0)
        })
}

/// Logical output rectangles exposed as `InputCapture` zones.  Invalid outputs
/// are omitted so a transient KMS modeset never offers a zero-sized capture
/// target to a portal client.
fn input_capture_zones(
    outputs: &[crate::compositor::backend::Output],
) -> Vec<crate::portal::CaptureZone> {
    outputs
        .iter()
        .filter(|output| output.size.0 > 0 && output.size.1 > 0)
        .map(|output| {
            let (width, height) = output.logical_size();
            crate::portal::CaptureZone {
                width,
                height,
                x: output.logical_position.0,
                y: output.logical_position.1,
            }
        })
        .collect()
}

#[cfg(test)]
mod vt_chord_tests;

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
mod gesture_wireup_tests {
    use super::{apply_shell_gesture, backend_nested_loop_uses_edge_wake, refresh_mhz_of};
    use crate::compositor::backend::drm::gestures::GestureAction;
    use crate::compositor::backend::{Backend, BackendEvent, HeadlessBackend, Output, OutputId};
    use crate::compositor::frame::FrameBuilder;

    /// A shell gesture has to reach the *chrome*, not a parallel handler —
    /// drives `apply_shell_gesture` against a real `FrameBuilder`.
    fn builder() -> FrameBuilder {
        FrameBuilder::new((1600, 900))
    }

    #[test]
    fn gestures_drive_workspace_switch_and_overview() {
        // Phase `a_swipe_up_switches_the_workspace_the_chrome_is_on`.
        {
            let mut fb = builder();
            // A second workspace to switch to — a one-workspace tree has nowhere to
            // go, so the assertion would pass vacuously. The new one is active, appended last, so "up" is where we came from.
            fb.with_chrome(|st| {
                st.add_workspace("second");
            });
            let before = fb.chrome().tree().desktop.active;
            // What a four-finger swipe up resolves to — same value `gestures::swipe_action` produces for an upward delta.
            apply_shell_gesture(
                &mut fb,
                GestureAction::SwitchWorkspace(-1),
                std::time::Instant::now(),
            );
            let after = fb.chrome().tree().desktop.active;
            assert_ne!(
                before, after,
                "a four-finger swipe must move the active workspace, not just resolve to an intent"
            );
        }
        // Phase `a_three_finger_swipe_down_carries_the_deck_to_the_strip_below`.
        {
            let mut fb = builder();
            fb.with_chrome(|st| {
                let mut tree = st.tree_mut();
                let ws = tree.desktop.active_workspace_mut().expect("a workspace");
                ws.strips.push(crate::shell::state::Strip::new(
                    crate::shell::state::StripId(9_000),
                    "Below".to_owned(),
                ));
                drop(tree);
                st.bump();
            });
            fb.relayout();
            let workspace = fb.chrome().tree().desktop.active;
            let (_, focused) = fb.chrome().strip_focus();
            // The fingers carry the deck most of a screen down, then lift.
            let at = std::time::Instant::now();
            for (i, travel) in [40.0, 200.0, 500.0, 700.0].into_iter().enumerate() {
                apply_shell_gesture(
                    &mut fb,
                    GestureAction::StripDeckUpdate {
                        travel,
                        began: i == 0,
                    },
                    at + std::time::Duration::from_millis(16 * u64::try_from(i).unwrap_or(0)),
                );
            }
            assert_eq!(
                fb.chrome().strip_focus().1,
                focused,
                "the deck follows the fingers; the strip changes when they lift"
            );
            apply_shell_gesture(
                &mut fb,
                GestureAction::StripDeckEnd { cancelled: false },
                at + std::time::Duration::from_millis(64),
            );
            assert_eq!(
                fb.chrome().strip_focus().1,
                focused + 1,
                "a three-finger swipe down must focus the strip below, as Super+↓ does"
            );
            assert_eq!(
                fb.chrome().tree().desktop.active,
                workspace,
                "a strip switch stays on the workspace"
            );
        }
        // Phase `a_pinch_opens_and_closes_the_overview`.
        {
            let mut fb = builder();
            assert!(!fb.chrome().overlays.overview);
            apply_shell_gesture(
                &mut fb,
                GestureAction::ShowOverview,
                std::time::Instant::now(),
            );
            assert!(fb.chrome().overlays.overview);
            apply_shell_gesture(
                &mut fb,
                GestureAction::HideOverview,
                std::time::Instant::now(),
            );
            assert!(!fb.chrome().overlays.overview);
        }
        // Phase `showing_an_already_open_overview_is_a_no_op`.
        {
            // The gesture fires once per swipe; a second swipe in the same direction must not bump the revision for nothing.
            let mut fb = builder();
            apply_shell_gesture(
                &mut fb,
                GestureAction::ShowOverview,
                std::time::Instant::now(),
            );
            let revision = fb.chrome().revision;
            apply_shell_gesture(
                &mut fb,
                GestureAction::ShowOverview,
                std::time::Instant::now(),
            );
            assert_eq!(fb.chrome().revision, revision);
        }
    }

    #[test]
    fn backend_refresh_rate_is_advertised_in_millihertz() {
        // Phase `a_backend_with_no_display_advertises_no_refresh_rate`.
        {
            // The headless backend has no display, so `wl_output` falls back to the conventional rate rather than zero.
            let backend = HeadlessBackend::new(800, 600).expect("headless backend");
            assert_eq!(refresh_mhz_of(&backend as &dyn Backend), None);
        }
        // Phase `a_refresh_interval_becomes_millihertz`.
        {
            struct Paced(std::time::Duration);
            impl Backend for Paced {
                fn submit_frame(&mut self, _s: &vello::Scene, _o: OutputId) {}
                fn poll_events(&mut self, _cb: &mut dyn FnMut(BackendEvent)) {}
                fn outputs(&self) -> &[Output] {
                    &[]
                }
                fn refresh_interval(&self) -> Option<std::time::Duration> {
                    Some(self.0)
                }
            }
            let sixty = Paced(std::time::Duration::from_nanos(16_666_667));
            assert_eq!(refresh_mhz_of(&sixty as &dyn Backend), Some(60_000));
            // 59.94 Hz must not round to 60 000 — the whole reason DRM derives its interval from raw mode timing.
            let ntsc = Paced(std::time::Duration::from_nanos(16_683_333));
            assert_eq!(refresh_mhz_of(&ntsc as &dyn Backend), Some(59_940));
        }
    }

    #[test]
    fn session_loop_wakes_once_per_readiness_and_exits_cleanly() {
        // Phase `drm_nested_loop_readiness_is_coalesced_to_one_outer_wake`.
        {
            let source = include_str!("native.rs");
            assert!(
                backend_nested_loop_uses_edge_wake(source),
                "the nested DRM calloop fd is only a wake token; level readiness can spin the outer loop thousands of times after the inner source was drained"
            );
        }
        // Phase `ordinary_logout_returns_from_run_instead_of_exiting_the_process`.
        // A normal logout must unwind the backend, event loop and libseat
        // handles. Unlinking the public sockets is not enough: terminating the
        // process from inside `run` skips those destructors and can leave the
        // display manager unable to acquire the physical seat.
        {
            let source = include_str!("native.rs");
            let run = source
                .split_once("pub fn run() {")
                .expect("native run entry point")
                .1
                .split_once("/// Session-owned ambient-light loop")
                .expect("end of native run entry point")
                .0;

            assert!(
                !run.contains("std::process::exit(0)"),
                "ordinary logout must return so DRM/libseat resources unwind"
            );
            assert!(
                run.contains("drop(display);") && run.contains("drop(notif_handle);"),
                "service endpoints must still be torn down before returning"
            );
        }
    }
}

#[cfg(test)]
mod output_geometry_tests;

#[cfg(test)]
mod external_floating_security_tests {
    use super::{
        app_local_to_desktop, app_owned_source_region, app_owned_transfer_region,
        clamp_external_floating_geometry, clamp_external_floating_geometry_to_outputs,
        external_floating_id, external_floating_parts, translate_external_source,
        validate_app_owned_source,
    };
    use crate::compositor::backend::{Output, OutputId};
    use ui::floating_surface::FloatingSource;
    use vello::kurbo::Rect;

    fn output(id: u32, position: (i32, i32), size: (u32, u32), scale: f32) -> Output {
        Output {
            id: OutputId(id),
            position,
            size,
            scale,
            transform: settings_store::hardware::OutputTransform::Normal,
            color_profile: None,
            refresh_mhz: None,
            logical_position: position,
            identity: crate::compositor::backend::OutputIdentity::default(),
        }
    }

    #[test]
    fn external_floating_identity_is_authenticated_and_unspoofable() {
        // Phase `authenticated_app_region_is_translated_and_clipped_to_its_surface`.
        {
            let app = Rect::new(300.0, 80.0, 1100.0, 680.0);
            let source = FloatingSource::AppRegion {
                app_id: "so.tilo.chat".into(),
                region: Rect::new(720.0, 20.0, 880.0, 220.0),
            };
            let translated = translate_external_source(source, "so.tilo.chat", app).unwrap();
            let FloatingSource::AppRegion { region, .. } = translated else {
                panic!("app region must remain an app region");
            };
            assert_eq!(region, Rect::new(1020.0, 100.0, 1100.0, 300.0));

            let semantic = FloatingSource::AppElement {
                app_id: "so.tilo.chat".into(),
                element_id: "call:shared-screen".into(),
                region: Rect::new(40.0, 30.0, 440.0, 280.0),
            };
            let translated = translate_external_source(semantic, "so.tilo.chat", app).unwrap();
            let FloatingSource::AppElement {
                app_id,
                element_id,
                region,
            } = translated
            else {
                panic!("semantic app element must retain its identity");
            };
            assert_eq!(app_id, "so.tilo.chat");
            assert_eq!(element_id, "call:shared-screen");
            assert_eq!(region, Rect::new(340.0, 110.0, 740.0, 360.0));
        }
        // Phase `external_mirror_identity_round_trips_connection_and_public_id`.
        {
            let internal = external_floating_id(42, "browser.media-pip:main");
            assert_eq!(
                external_floating_parts(&internal),
                Some((42, "browser.media-pip:main"))
            );
            assert!(external_floating_parts("capture-mirror:7").is_none());
        }
        // Phase `external_client_cannot_spoof_another_app_or_surface_id`.
        {
            let app = Rect::new(300.0, 80.0, 1100.0, 680.0);
            assert!(
                translate_external_source(
                    FloatingSource::AppRegion {
                        app_id: "so.tilo.mail".into(),
                        region: Rect::new(0.0, 0.0, 200.0, 120.0),
                    },
                    "so.tilo.chat",
                    app,
                )
                .is_err()
            );
            assert!(
                translate_external_source(
                    FloatingSource::Surface { surface_id: 42 },
                    "so.tilo.chat",
                    app,
                )
                .is_err()
            );
        }
        // Phase `app_owned_native_surface_accepts_local_source_but_not_spoofed_identity`.
        {
            let source = FloatingSource::AppRegion {
                app_id: "so.tilo.chat".into(),
                region: Rect::new(420.0, 80.0, 780.0, 300.0),
            };
            assert!(validate_app_owned_source(&source, "so.tilo.chat").is_ok());
            assert_eq!(
                app_owned_source_region(
                    &source,
                    "so.tilo.chat",
                    vello::kurbo::Size::new(1_000.0, 700.0)
                ),
                Some(Rect::new(420.0, 80.0, 780.0, 300.0))
            );
            assert!(validate_app_owned_source(&source, "so.tilo.maps").is_err());
            assert!(
                validate_app_owned_source(
                    &FloatingSource::Surface { surface_id: 42 },
                    "so.tilo.chat"
                )
                .is_err()
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn floating_handoff_geometry_moves_chrome_and_content_as_one_shape() {
        // Phase `app_owned_transfer_keeps_visible_soft_chrome_at_the_parent_edge`.
        {
            let source = FloatingSource::AppElement {
                app_id: "so.tilo.chat".into(),
                element_id: "active-call".into(),
                // A live drag has already carried most of the content beyond the
                // primary window. Its final visible strip is still a valid source
                // and must not make the transfer proxy disappear altogether.
                region: Rect::new(1_180.0, 80.0, 1_420.0, 260.0),
            };
            let presentation = Rect::new(1_156.0, 32.0, 1_444.0, 352.0);
            let visible = app_owned_transfer_region(
                &source,
                Some(presentation),
                "so.tilo.chat",
                vello::kurbo::Size::new(1_200.0, 700.0),
            )
            .expect("the visible decorated edge becomes the warm-up proxy");
            assert_eq!(visible, Rect::new(1_156.0, 32.0, 1_200.0, 352.0));
        }
        // Phase `app_owned_transfer_prefers_decoration_over_content_only_source`.
        {
            let source = FloatingSource::AppRegion {
                app_id: "so.tilo.maps".into(),
                region: Rect::new(720.0, 120.0, 1_080.0, 276.0),
            };
            let window = Rect::new(712.0, 112.0, 1_088.0, 284.0);
            assert_eq!(
                app_owned_transfer_region(
                    &source,
                    Some(window),
                    "so.tilo.maps",
                    vello::kurbo::Size::new(1_280.0, 720.0),
                ),
                Some(window),
                "title, controls, silhouette and shadow must not vanish during adoption"
            );
        }
        // Phase `app_owned_handoff_translates_content_and_soft_window_without_a_jump`.
        {
            let app = Rect::new(262.0, 124.0, 1_572.0, 892.0);
            let local_content = Rect::new(938.0, 56.0, 1_266.0, 233.0);
            let local_window = Rect::new(918.0, 8.0, 1_286.0, 397.0);
            let desktop_content = app_local_to_desktop(local_content, app);
            let desktop_window = app_local_to_desktop(local_window, app);
            assert_eq!(
                desktop_content.origin(),
                vello::kurbo::Point::new(1_200.0, 180.0)
            );
            assert_eq!(
                desktop_window.origin(),
                vello::kurbo::Point::new(1_180.0, 132.0)
            );
            assert_eq!(desktop_content.size(), local_content.size());
            assert_eq!(desktop_window.size(), local_window.size());
        }
        // Phase `clamping_moves_soft_window_and_interactive_content_as_one_shape`.
        {
            let desktop = Rect::new(0.0, 0.0, 1_600.0, 900.0);
            let window = Rect::new(1_700.0, 40.0, 2_092.0, 292.0);
            let content = Rect::new(1_708.0, 48.0, 2_084.0, 284.0);
            let (clamped_window, clamped_content) =
                clamp_external_floating_geometry(window, content, desktop);
            assert_eq!(
                clamped_window.origin() - window.origin(),
                clamped_content.origin() - content.origin()
            );
            assert_eq!(
                clamped_content - clamped_window.origin().to_vec2(),
                Rect::new(8.0, 8.0, 384.0, 244.0)
            );
            assert_eq!(clamped_window.x0, desktop.x1 - 64.0);
        }
        // Phase `touching_outputs_remain_one_continuous_drag_surface`.
        {
            let outputs = [
                output(0, (0, 0), (1_920, 1_080), 2.0),
                output(1, (1_920, 0), (2_560, 1_440), 1.0),
            ];
            let window = Rect::new(1_760.0, 220.0, 2_160.0, 480.0);
            let content = Rect::new(1_768.0, 228.0, 2_152.0, 472.0);
            assert_eq!(
                clamp_external_floating_geometry_to_outputs(
                    window,
                    content,
                    &outputs,
                    (4_480, 1_440),
                ),
                (window, content),
                "crossing a real shared edge must not magnetise the float to either display"
            );
        }
        // Phase `l_shaped_desktop_gap_cannot_swallow_a_floating_surface`.
        {
            let outputs = [
                output(0, (0, 0), (1_920, 1_080), 2.0),
                output(1, (1_920, 400), (2_560, 1_440), 1.0),
            ];
            let window = Rect::new(2_050.0, 100.0, 2_450.0, 280.0);
            let content = Rect::new(2_058.0, 108.0, 2_442.0, 272.0);
            let (clamped_window, clamped_content) = clamp_external_floating_geometry_to_outputs(
                window,
                content,
                &outputs,
                (4_480, 1_840),
            );
            assert_eq!(clamped_window.y1, 464.0);
            assert_eq!(
                clamped_window
                    .intersect(Rect::new(1_920.0, 400.0, 4_480.0, 1_840.0))
                    .height(),
                64.0
            );
            assert_eq!(
                clamped_window.origin() - window.origin(),
                clamped_content.origin() - content.origin(),
                "soft chrome and its interactive content must re-home as one shape"
            );
        }
    }
}

#[allow(
    unused_imports,
    reason = "The native WorkspaceExt methods are selected by host-specific workspace code and test configurations."
)]
use crate::shell::state::WorkspaceExt as _;
use tilo_lock::MutexRecover as _;

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "viewport geometry fixtures compare exactly the values the compositor laid out"
)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the fixture owns the compositor's lock and tree guards for the whole scenario, as the socket tests' do"
)]
#[path = "native_agent_seats_tests.rs"]
mod agent_seats_tests;

#[cfg(test)]
#[path = "native_scoped_control_tests.rs"]
mod scoped_control_tests;
