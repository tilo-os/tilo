//! Native compositor mode: `--mode native` wires a real smithay event loop
//! over `state::State`, with an auto-named listening socket (`wayland-1`,
//! `wayland-2`, …) handed to `DisplayHandle::insert_client`. Doesn't open a
//! TTY, talk to DRM/KMS, render, or forward input — those live elsewhere.

use crate::shell::input_policy::{
    is_lock_chord, is_mouse_keys_toggle, is_remote_input_kill_switch, winit_event_to_lock_key,
};
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
use crate::shell::search::visibility::CapabilityVisibility;

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

#[allow(
    clippy::significant_drop_tightening,
    reason = "both `chrome` and `tree` back the values handed to `f` at the final call, which is \
              the whole point of this function — there is no earlier point to drop either guard"
)]
fn with_action_gate<R>(
    chrome_handle: &crate::shell::chrome_state::Chrome,
    f: impl FnOnce(&crate::shell::chrome_state::ChromeState, &dyn ActionVisibility) -> R,
) -> Result<R, String> {
    let chrome = chrome_handle.borrow();
    let tree = chrome.tree();
    let workspace = tree
        .desktop
        .active_workspace()
        .ok_or_else(|| "no active workspace to resolve capabilities against".to_string())?;
    let actor_id = chrome.local_user_for_room(&workspace.room);
    let actor = workspace.actor_for_person(&actor_id);
    let gate = CapabilityVisibility::new(workspace, actor);
    Ok(f(&chrome, &gate))
}
use smithay::reexports::calloop::{self, EventLoop, Interest, Mode, PostAction, generic::Generic};
use smithay::reexports::wayland_server::Display;
use smithay::wayland::socket::ListeningSocketSource;
use taffy::NodeId;
use vello::kurbo::Rect;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey, SmolStr};

use crate::compositor::accessibility::{NativeAccessibility, Request as AccessibilityRequest};
use crate::compositor::backend::drm::runtime::{DirectTouchDown, DirectTouchOwnership};
use crate::compositor::backend::{
    Backend, BackendEvent, DrmBackend, HeadlessBackend, Output, OutputId, WinitBackend,
    combined_geometry, headless, logical_geometry, output_geometry,
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
use crate::compositor::lock_screen::{self, LockOutput, LockView};
use crate::compositor::metrics;
use crate::compositor::output_globals;
use crate::compositor::pacing;
use crate::compositor::present;
use crate::compositor::screen_dim::{Backlight, ScreenDimmer};
use crate::compositor::surface_outputs::{
    Change as SurfaceOutputChange, Memberships as SurfaceOutputMemberships,
};

/// The legacy windows to encode for viewers: none while nobody watches the
/// backend, so a headless machine's encoders stop and receivers get
/// `SurfaceStreamEnd` at once.
///
/// `viewer_hosts` are the peer handles of the active room's workspace hosts
/// when the backend is headless: its viewers watch through that host (which
/// decides which of them see and control what), so every legacy window of
/// the active workspace is shared with it, not only the explicitly shared.
fn desired_legacy_shares(
    tree: &crate::shell::state::WorkspaceTree,
    watched: bool,
    viewer_hosts: &std::collections::HashSet<u64>,
) -> HashMap<u64, (String, std::collections::HashSet<u64>)> {
    if !watched {
        return HashMap::new();
    }
    let Some(workspace) = tree
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.id == tree.desktop.active)
    else {
        return HashMap::new();
    };
    let mut desired = HashMap::new();
    for tab in workspace
        .strips
        .iter()
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
    {
        let crate::shell::state::TileContent::Foreign { app_id, surface_id } = &tab.tile else {
            continue;
        };
        let mut peers = workspace
            .shared_surfaces
            .iter()
            .find_map(|(surface, peers)| (surface == *surface_id).then(|| peers.clone()))
            .unwrap_or_default();
        peers.extend(viewer_hosts);
        if !peers.is_empty() {
            desired.insert(*surface_id, (app_id.clone(), peers));
        }
    }
    desired
}
use crate::compositor::switch_control::{SwitchAction, SwitchControlRuntime};

use crate::compositor::state::{ClientState, State};
use crate::compositor::test_control::{
    self, ControlState, KeyTag, NamedKeyTag, PlannedAction, PointerButtonKind, TouchPhaseKind,
    parse_request, serialise_response,
};
use crate::compositor::{BackendKind, parse_backend_from_args};
use crate::shell::overlay_host::PeerSyncRequest;

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
    accessibility: bool,
    revision: bool,
    hidden_desktop_animation: bool,
    apps: bool,
    peers: bool,
    presentation: bool,
    capture: bool,
    keepalive: bool,
    lock_clock: bool,
    lock_animation: bool,
    /// A media-key readout drawn over the lock surface is fading or due to clear.
    lock_osd: bool,
}

impl NativeRedrawReasons {
    fn content_changed(self, session_locked: bool) -> bool {
        self.force
            || self.backend_event
            || self.visible_input
            || self.session
            || self.accessibility
            || self.revision
            || (!session_locked
                && (self.hidden_desktop_animation
                    || self.apps
                    || self.peers
                    || self.presentation
                    || self.capture))
            || (!session_locked && self.keepalive)
            || (session_locked && (self.lock_clock || self.lock_animation || self.lock_osd))
    }
}

const LOCK_CLOCK_REFRESH: Duration = Duration::from_secs(60);
const IDLE_BACKEND_POLL: Duration = Duration::from_millis(50);
const WAYLAND_CLIENT_SERVICE_FALLBACK: Duration = Duration::from_millis(4);
const WAYLAND_REQUESTS_PER_CLIENT_PER_TICK: usize = 64;
const WAYLAND_PENDING_RETRY_FLOOR: Duration = Duration::from_millis(1);
const SLOW_WAYLAND_CLIENT_DISPATCH: Duration = Duration::from_millis(10);
const SLOW_PREPAINT_PHASE: Duration = Duration::from_millis(50);

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
fn client_followup_deadline(clients_dispatched: bool, now: Instant) -> Option<Instant> {
    clients_dispatched.then_some(now)
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

/// A lock-state edge replaces every pixel on screen.  Keep that edge pending
/// until a frame is actually accepted by the presenter; consuming it while
/// the asynchronous render queue is full would leave the previous lock or
/// desktop frame visible until unrelated input happens to trigger a repaint.
fn retain_session_redraw(session_changed: bool, frame_submitted: bool) -> bool {
    session_changed && !frame_submitted
}

/// Whether the next park must keep the frame deadline for a pending frame.
/// While the bounded presenter is full, an already-due frame deadline would
/// make every park a zero timeout and spin the loop until a frame retires;
/// the presenter's retirement ping wakes calloop as soon as capacity returns.
fn cadence_wait_needed(pipe_ready: bool, frame_pending: bool) -> bool {
    pipe_ready && frame_pending
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

// A due animation remains dirty until it paints, but is no longer a wake
// deadline. Keeping an expired deadline here would busy-spin while waiting
// for display cadence or presenter retirement. Those wake sources already
// carry the pending frame; future animation deadlines still wake the idle loop.
fn lock_animation_park_deadline(now: Instant, deadline: Option<Instant>) -> Option<Instant> {
    deadline.filter(|deadline| *deadline > now)
}

/// How long after the last input a visible lock backdrop keeps moving. It
/// then holds still until someone touches the machine: a locked laptop left
/// on the desk must not repaint at the backdrop's cadence until it blanks.
const LOCK_BACKDROP_MOTION: Duration = Duration::from_secs(20);

/// The lock backdrop animates only while it can be seen and someone is
/// there. Behind a blanked panel the backend drops every frame, so building
/// them would keep an unattended, locked machine awake at the backdrop's
/// cadence.
fn lock_animation_deadline(
    theme: &ui::theme::Theme,
    session: &SessionManager,
    has_wallpaper: bool,
    last_frame: Instant,
    now: Instant,
) -> Option<Instant> {
    lock_screen::animation_deadline(
        theme,
        session.is_locked()
            && !session.is_blanked()
            && session.idle_for(now) < LOCK_BACKDROP_MOTION,
        has_wallpaper,
        last_frame,
    )
}

/// How often a locked session re-reads its players, so the lock card follows
/// a track change or a pause made elsewhere.
const LOCK_MEDIA_REFRESH: Duration = Duration::from_secs(1);

/// Every output the lock overlay covers, or one spanning the viewport before
/// any output has been reported.
fn lock_outputs(
    outputs: &[crate::compositor::backend::Output],
    viewport: (f64, f64),
) -> Vec<LockOutput> {
    let outputs: Vec<LockOutput> = outputs
        .iter()
        .map(|o| LockOutput {
            origin: (f64::from(o.position.0), f64::from(o.position.1)),
            size: (f64::from(o.size.0), f64::from(o.size.1)),
            scale: f64::from(o.scale),
        })
        .collect();
    if outputs.is_empty() {
        vec![LockOutput {
            origin: (0.0, 0.0),
            size: viewport,
            scale: 1.0,
        }]
    } else {
        outputs
    }
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

fn wayland_peer_credentials(stream: &UnixStream) -> Option<libc::ucred> {
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

fn peer_shares_host_security_namespaces(pid: u32) -> bool {
    ["user", "mnt"].into_iter().all(|namespace| {
        let host = std::fs::metadata(format!("/proc/self/ns/{namespace}"));
        let peer = std::fs::metadata(format!("/proc/{pid}/ns/{namespace}"));
        matches!((host, peer), (Ok(host), Ok(peer)) if host.dev() == peer.dev() && host.ino() == peer.ino())
    })
}

fn unattributed_host_client_allowed(peer_uid: libc::uid_t, shares_namespaces: bool) -> bool {
    // SAFETY: `geteuid` has no failure mode.
    peer_uid == unsafe { libc::geteuid() } && shares_namespaces
}

fn public_client_state(stream: &UnixStream) -> Option<Arc<ClientState>> {
    let credentials = wayland_peer_credentials(stream)?;
    let pid = u32::try_from(credentials.pid).ok()?;
    // SAFETY: `geteuid` has no failure mode.
    if credentials.uid != unsafe { libc::geteuid() } {
        return None;
    }
    if let Some(identity) = crate::shell::launcher::verified_launch_for_descendant(pid) {
        return Some(ClientState::attributed_arc(identity, pid));
    }
    unattributed_host_client_allowed(credentials.uid, peer_shares_host_security_namespaces(pid))
        .then(|| {
            Arc::new(ClientState {
                pid: Some(pid),
                ..ClientState::default()
            })
        })
}

fn external_floating_id(connection: u64, id: &str) -> String {
    format!("external:{connection}:{id}")
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

/// Serve the shell's own file choosers when apps have no portal to reach: no
/// session bus, as on a provider's headless guest. `attended` says whether an
/// agent is driving the screen; an unattended session (one that saves into
/// `TILO_SAVE_DIRECTORY`) otherwise leaves its choosers to the apps.
fn start_chooser_service(
    portal: Option<&PortalBackend>,
    attended: Arc<AtomicBool>,
    wake: crate::session_runtime::Wake,
) -> Option<crate::compositor::chooser_service::ChooserService> {
    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some_and(|address| !address.is_empty()) {
        return None;
    }
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

/// Open the files apps asked the chooser service to open, each in the app
/// that owns its type, in the asking app's workspace.
fn pump_chooser_opens(
    service: &crate::compositor::chooser_service::ChooserService,
    chrome: &crate::shell::chrome_state::Chrome,
) {
    for open in service.drain_opens() {
        let active = chrome.borrow().tree().desktop.active;
        let outcome = if open.caller.workspace_id == active {
            crate::shell::doc_open::open_in_workspace(&open.path, chrome)
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
}

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
    outputs: &[Output],
    fallback_size: (u32, u32),
) -> bool {
    let desktop = floating_desktop_bounds(outputs, fallback_size);
    let mut changed = false;
    for event in service.drain() {
        match event {
            FloatingServiceEvent::Connected { connection, app_id } => {
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
                message,
            } => match message {
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
                                frame_builder.foreign_surface_for_app(&app_id).ok_or_else(
                                    || format!("{app_id} has no visible foreign surface"),
                                )?;
                            let (transfer_frame, transfer_source) = if matches!(
                                request.role,
                                ui::floating_surface::FloatingRole::ActiveCall
                                    | ui::floating_surface::FloatingRole::Navigation
                            ) {
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
                            ))
                        });
                    match result {
                        Ok((request, transfer_frame, transfer_source, coordinate_offset)) => {
                            if matches!(
                                request.role,
                                ui::floating_surface::FloatingRole::ActiveCall
                                    | ui::floating_surface::FloatingRole::Navigation
                            ) {
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
                                let token = state
                                    .resume_floating_toplevel(
                                        connection,
                                        &public_id,
                                        rect,
                                        content_rect,
                                        transfer_frame.clone(),
                                        transfer_source,
                                    )
                                    .unwrap_or_else(|| {
                                        let token = uuid::Uuid::new_v4().simple().to_string();
                                        state.authorize_floating_toplevel(
                                            crate::compositor::state::PendingFloatingToplevel {
                                                token: token.clone(),
                                                connection,
                                                public_id: public_id.clone(),
                                                app_id: app_id.clone(),
                                                content_rect,
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
                } => match (
                    ui::floating_surface::protocol_rect(rect),
                    ui::floating_surface::protocol_rect(content_rect),
                ) {
                    (Ok(window_rect), Ok(content_rect)) => {
                        let (rect, content_rect) = clamp_external_floating_geometry_to_outputs(
                            window_rect,
                            content_rect,
                            outputs,
                            fallback_size,
                        );
                        changed |= state.update_floating_toplevel_rect(
                            connection,
                            &id,
                            rect,
                            content_rect,
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
                        changed |= state.update_floating_toplevel_rect(
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
                ui::floating_surface::FloatingSurfaceClientMessage::SetCaptureProtection {
                    protected,
                } => {
                    // The app id is the one the launcher vouched for, so a
                    // request can only ever withhold the asking app's own
                    // windows. It narrows what leaves the display, never
                    // widens it, so any app may ask.
                    foreign::store().request_app_capture_protection(connection, &app_id, protected);
                    tracing::debug!(connection, %app_id, protected, "capture protection requested");
                    changed = true;
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
            },
            FloatingServiceEvent::Disconnected { connection, app_id } => {
                for id in connection_ids.remove(&connection).unwrap_or_default() {
                    capture.dismiss_external_floating(id);
                }
                state.dismiss_floating_connection(connection);
                foreign::store().release_capture_protection(connection);
                tracing::debug!(connection, %app_id, "shell-ui client disconnected");
                changed = true;
            }
        }
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
            (
                workspace.room.to_string(),
                crate::shell::state::local_user_id_for_room(&workspace.room),
            )
        })
    };
    let Some((room, actor)) = principal else {
        eprintln!("tilo compositor: agent MCP endpoint: no active workspace");
        return;
    };
    let scratch = xdg_paths::xdg::cache_home().map(|cache| cache.join("tilo").join("computer"));
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
    let viewer_state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
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

/// Directory under `$XDG_RUNTIME_DIR` where a running compositor publishes
/// facts the next terminal command needs; torn down with the session.
pub const SESSION_RUNTIME_SUBDIR: &str = "tilo-session";

/// File inside [`SESSION_RUNTIME_SUBDIR`] holding the bare socket name (no
/// trailing newline), so `WAYLAND_DISPLAY=$(cat …)` is exactly right.
pub const WAYLAND_DISPLAY_FILE: &str = "wayland-display";

/// Where [`publish_wayland_display`] writes, given `$XDG_RUNTIME_DIR`. Split
/// out so the path policy is testable without a live session.
///
/// `None` when the variable is unset or not absolute — guessing `/tmp`
/// would put a session-scoped file somewhere world-writable.
pub(crate) fn session_display_path(runtime_dir: Option<std::ffi::OsString>) -> Option<PathBuf> {
    let dir = PathBuf::from(runtime_dir?);
    if !dir.is_absolute() {
        return None;
    }
    Some(dir.join(SESSION_RUNTIME_SUBDIR).join(WAYLAND_DISPLAY_FILE))
}

/// Publish the compositor's Wayland socket name to
/// `$XDG_RUNTIME_DIR/tilo-session/wayland-display`. Last writer wins — the
/// file answers "what do I connect to *now*"; the printed stderr line stays
/// authoritative for a specific compositor. Never fatal on failure. Returns
/// the unlink guard so the file disappears with the process.
fn publish_wayland_display(socket_name: &std::ffi::OsStr) -> Option<UnlinkOnDrop> {
    let path = session_display_path(std::env::var_os("XDG_RUNTIME_DIR"))?;
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
    match atomic_file::write_with_mode(&path, socket_name.as_encoded_bytes(), 0o600) {
        Ok(()) => Some(UnlinkOnDrop(path)),
        Err(e) => {
            eprintln!("tilo compositor: publishing {}: {e}", path.display());
            None
        }
    }
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
    /// Accepted on the agent-control socket: only
    /// [`test_control::Command::agent_permitted`] commands are served.
    agent: bool,
    /// Wakes the compositor loop when request bytes arrive on this stream;
    /// removed from the loop together with the connection.
    wake: Option<calloop::RegistrationToken>,
    closed: bool,
    pending_invocation: Option<PendingTestControlInvocation>,
    output: Vec<u8>,
    output_offset: usize,
}

struct PendingTestControlInvocation {
    ticket: crate::shell::search::PendingInvocation,
    action: serde_json::Value,
}

enum PendingTestControlPoll {
    Waiting(PendingTestControlInvocation),
    Ready(test_control::Response),
}

impl PendingTestControlInvocation {
    fn poll(self) -> PendingTestControlPoll {
        match (self.ticket)() {
            Ok(None) => PendingTestControlPoll::Waiting(self),
            Ok(Some(receipt)) => {
                PendingTestControlPoll::Ready(invocation_response(&self.action, &receipt))
            }
            Err(error) => PendingTestControlPoll::Ready(test_control::Response::Err {
                code: error.code().to_string(),
                message: error.to_string(),
            }),
        }
    }
}

impl TestControlConn {
    fn new(stream: UnixStream, agent: bool) -> Option<Self> {
        if let Err(e) = stream.set_nonblocking(true) {
            eprintln!("tilo compositor: test_control conn set_nonblocking: {e}");
            return None;
        }
        Some(Self {
            reader: BufReader::new(stream),
            agent,
            wake: None,
            closed: false,
            pending_invocation: None,
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
            |_, _, _| Ok(PostAction::Continue),
        ) {
            Ok(token) => self.wake = Some(token),
            Err(error) => eprintln!("tilo compositor: test_control conn wake source: {error}"),
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
}

impl FrameBuilderControlState<'_> {
    /// Which windows each display holds, in the order they are laid out,
    /// and which display has focus: the per-device partition as the person
    /// sees it. Empty on a single display.
    fn displays_json(&self) -> Vec<serde_json::Value> {
        if !self.fb.host().is_partitioned() {
            return Vec::new();
        }
        let summary = self.fb.host().display_partition_summary();
        let chrome = self.fb.chrome();
        let tree = chrome.tree();
        let partition = &tree.desktop.outputs;
        let displays = summary
            .into_iter()
            .map(|(id, groups)| {
                serde_json::json!({
                    "id": id,
                    "primary": partition.primary() == Some(id),
                    "focused": partition.focused_display() == Some(id),
                    "groups": groups,
                })
            })
            .collect();
        drop(tree);
        drop(chrome);
        displays
    }
}

impl ControlState for FrameBuilderControlState<'_> {
    fn widget_registry(&self) -> HashMap<String, Rect> {
        self.fb.widget_registry()
    }

    /// The peers the overlay would paint this frame, read from the same
    /// presence context `draw_real_peers` uses, keyed by room.
    fn peers_json(&self) -> serde_json::Value {
        let presence = self.fb.presence();
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
                "label": row.label,
                "role": row.role,
                // Kept separate from the label, same as the panel: inferring which of
                // these is a person from the label alone is the one mistake to avoid.
                "is_agent": row.is_agent,
                // Named but not arrived — the one thing a harness watching a join needs,
                // and what a screenshot of a dimmed row can't be asserted on.
                "invited": row.invited,
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

    fn workspace_tree_json(&self) -> serde_json::Value {
        let chrome = self.fb.chrome();
        // `unwrap_or(Null)`: the derived `Serialize` is infallible in practice, but
        // `expect` here would crash the compositor on a hypothetical bad write.
        serde_json::to_value(&*chrome.tree()).unwrap_or(serde_json::Value::Null)
    }

    /// Report the compositor's own frame bookkeeping for every foreign toplevel.
    /// `frame_seq` counts frames actually taken from that client, so `0` means
    /// the tile is still the placeholder; `last_error` carries why a capture
    /// failed, which a blank tile would otherwise hide.
    fn foreign_frames_json(&self) -> serde_json::Value {
        let chrome = self.fb.chrome();
        let store = crate::compositor::foreign::store();
        let frames: Vec<serde_json::Value> =
            crate::compositor::foreign::foreign_tabs(&chrome.tree())
                .into_iter()
                .map(|tab| {
                    // The rect the widget tree laid this tab into, so a harness can aim at it.
                    let placement = store.placement(tab.surface_id).map(|r| {
                        serde_json::json!({
                            "x": r.x0, "y": r.y0,
                            "w": r.width(), "h": r.height(),
                        })
                    });
                    // What the client actually drew, beside the tab it was drawn into — an
                    // opening-then-resizing window is those two disagreeing for a moment.
                    let buffer = store
                        .frame_size(tab.surface_id)
                        .map(|(w, h)| serde_json::json!({ "w": w, "h": h }));
                    serde_json::json!({
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
                        "frame_seq": store.frame_seq(tab.surface_id),
                        "last_error": store.last_error(tab.surface_id),
                        "rect": placement,
                        "buffer": buffer,
                        // Whether the shell is painting this window yet.
                        "presented": store.is_presented(tab.surface_id),
                        // And, while it is not, whether the placeholder card is up — without this
                        // a harness can't tell an empty desktop from one announcing an app is on its way.
                        "opening": store.is_opening(tab.surface_id),
                    })
                })
                .collect();
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
        verb: &str,
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
        let room_key = ws_id.raw().to_string();
        let subject = test_control::parse_subject(subject)?;
        let scope = test_control::parse_scope(scope, &room_key)?;
        let verb = test_control::parse_verb(verb)?;
        let mut cap = CapOverride::grant(subject, scope, verb);
        if let Some(id) = id {
            cap = cap.with_id(id);
        }
        // Through the tree's entry point, not `grant_as` directly, so this answers
        // with the same authority the rest of the compositor grants under.
        let granted = tree.grant_in_workspace(ws_id, &user, cap).ok_or_else(|| {
            format!("{user} may not administer this workspace, so nothing was granted")
        })?;
        // Nothing past this point touches the tree.
        drop(tree);
        Ok(serde_json::json!({ "granted": granted, "grantor": user }))
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
            .iter_mut()
            .find(|w| w.id == ws_id)
            .ok_or_else(|| "no active workspace".to_string())?;
        // Distinguish the two failures: `revoke_as` returns false both for "you
        // may not" and "no such capability" — conflating them hides an unauthorised revoke as a no-op.
        let may = ws.may_administer(&user);
        if !may {
            return Err(format!(
                "{user} may not administer this workspace, so nothing was revoked"
            ));
        }
        let removed = ws.revoke_as(&user, id);
        if removed {
            tree.doc_revoke_capability(ws_id, id);
            tree.flush_to_disk();
        }
        // Nothing past this point touches the tree.
        drop(tree);
        Ok(serde_json::json!({ "revoked": removed, "revoker": user, "id": id }))
    }

    /// Answer the in-app action gate for the live workspace.
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
        action: &str,
        verb: &str,
        destructive: bool,
    ) -> Result<serde_json::Value, String> {
        use crate::shell::actions::{ActionDecl, ActionRegistry, check_action};
        use crate::shell::state::ResourceRef;
        let chrome = self.fb.chrome();
        // Same early-drop as `grant_capability`: only the tree is read below.
        let tree_handle = chrome.tree_handle();
        drop(chrome);
        let tree = tree_handle.read();
        let ws = tree
            .desktop
            .active_workspace()
            .ok_or_else(|| "no active workspace".to_string())?;
        let verb = test_control::parse_verb(verb)?;
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
        let mut registry = ActionRegistry::new();
        let mut decl = ActionDecl::new(app, action, verb);
        if destructive {
            decl = decl.destructive();
        }
        registry.declare(decl);
        let resource = ResourceRef::Action {
            app: app.to_string(),
            action: action.to_string(),
        };
        let outcome = check_action(ws, &registry, &actor, app, action, &resource);
        Ok(serde_json::json!({
            "outcome": format!("{outcome:?}"),
            "permitted": outcome.is_permitted(),
            "workspace": ws.id.raw(),
            "room": ws.room.to_string(),
        }))
    }

    /// Enumerate the exported action surface through the *same* provider
    /// registry that feeds the Ctrl+K palette — but renders nothing.
    fn enumerate_actions(&self, query: &str) -> Result<serde_json::Value, String> {
        let (doc, root) = self
            .doc
            .ok_or_else(|| "control state has no live document".to_string())?;
        let chrome_handle = self.fb.chrome_handle();
        // The borrow lives only for the closure; `ActionCatalog` owns its results
        // outright, so the catalogue outlives the guards it was built through.
        let catalog = with_action_gate(&chrome_handle, |chrome, gate| {
            self.fb
                .host()
                .action_catalog(query, chrome, doc, root, gate)
        })?;
        let actions: Vec<serde_json::Value> = catalog
            .enumerate()
            .into_iter()
            .map(|a| {
                let hierarchy = a.hierarchy.as_ref();
                serde_json::json!({
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
                    "operation": hierarchy.map(|value| &value.operation),
                })
            })
            .collect();
        Ok(serde_json::json!({
            "query": query,
            "count": actions.len(),
            "actions": actions,
        }))
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
            self.fb
                .host()
                .action_catalog_for_resource(&resource, chrome, doc, root, gate)
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
        Ok(serde_json::json!({
            "provider": provider,
            "id": id,
            "kind": selected.kind().id(),
            "title": selected.title(),
            "params": params_json(selected.params()),
            "contract": selected.contract(),
        }))
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
        let chrome = self.fb.chrome();
        let overlays = &chrome.overlays;
        let peer_sync = self.fb.host().peer_sync_snapshot();
        let (status_sharing, own_status, following) = self.fb.host().presence_snapshot();
        serde_json::json!({
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
            "search_scope": format!("{:?}", chrome.search_scope),
            "displays": self.displays_json(),
            // Applications' status-notifier items and their menus, as the top bar shows them.
            "tray": crate::tray::Tray::global().state_json(),
            // Each tab pill's artwork: `toplevel:` when the window set its
            // own, `theme:` for its application's icon.
            "tab_icons": tab_icons_json(&chrome.tree()),
            // The Alt+Tab switcher while it is open: its windows in order,
            // the selected one, and whether each card shows a live preview
            // (`live`), its app mark because the app holds secrets
            // (`withheld`), or its app mark for now (`none`).
            "window_switcher": self.fb.host().window_switcher_state_json(),
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
            },
            // Who this session is, and whether they may administer the room they're
            // in — read straight off the production helpers, not guessed from `owner`.
            "local_user_id": chrome.active_local_user(),
            "may_administer_active": chrome
                .tree()
                .desktop.active_workspace()
                .is_some_and(|ws| ws.may_administer(&chrome.local_user_for_room(&ws.room))),
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
            // Super+S detaches the workspace wheel from the strip's left
            // edge and places it at this point. `null` means docked. The
            // disc's `test_id` is always `wheel_disc`, so a harness that
            // only diffs target names cannot see the summon.
            "wheel_summon": chrome.wheel_summon().map(|p| serde_json::json!({
                "x": p.x,
                "y": p.y,
            })),
            // Super+F on a Tilo window sets this; a client xdg fullscreen does too.
            // Maximize is a different field (`workspace_tree` group.maximized).
            "fullscreen_group": chrome.fullscreen_group(),
            "fullscreen": chrome.is_fullscreen(),
        })
    }
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

/// Per-process state the test-control wire needs to execute
/// [`PlannedAction`]s, mirroring `ShellDriver`'s fields: modifier snapshot, auxiliary `AutoCommit` doc, root `ObjId`, focus cursor.
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
    /// Synthetic touchpad gestures go through the tracker the DRM runtime
    /// feeds from libinput, so they mean exactly what the fingers would.
    gestures: crate::compositor::backend::drm::gestures::GestureTracker,
    /// Attached input devices and the last kind of input used, from the
    /// backend or from injected test-control input alike. The shell chrome's
    /// shortcut help follows it.
    input: InputEnvironment,
    /// The latest `watch` answer, for the loop to hand to the backend.
    watch_request: Option<bool>,
    /// One seat and strip per agent driving this compositor.
    agents: crate::compositor::agent_seats::AgentSeats,
    /// While an agent's command runs, the screen region its snapshots show:
    /// its strip's viewport.
    snapshot_crop: Option<vello::kurbo::Rect>,
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
            gestures: crate::compositor::backend::drm::gestures::GestureTracker::default(),
            input: InputEnvironment::default(),
            watch_request: None,
            agents: crate::compositor::agent_seats::AgentSeats::new(),
            snapshot_crop: None,
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
            PlannedAction::CursorMoved { .. }
            | PlannedAction::MouseLeftButton { .. }
            | PlannedAction::MousePointerButton { .. }
            | PlannedAction::MouseWheel { .. }
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
                        &mut ctx.doc,
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
        .map(|workspace| workspace.room.to_string())
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
fn launch_queued_host_apps(state: &mut State, frame_builder: &mut FrameBuilder) -> bool {
    let requests = crate::shell::host_apps::drain_queued_launches();
    let mut changed = false;
    for request in requests {
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
        return;
    }
    if let Some(st) = state.as_deref_mut() {
        client_input::finish_chrome_drag_into_shell(st, fb, event);
    }
    fb.dispatch_event(
        event,
        &ctx.modifiers,
        &mut ctx.doc,
        &ctx.root_obj,
        &mut ctx.focus,
    );
}

/// The pointer click a confirmed one-finger tap at `(x, y)` stands for, for
/// taps that did not already reach a client over `wl_touch`.
fn touch_tap_pointer_events(x: f64, y: f64) -> [WindowEvent; 3] {
    [
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: PhysicalPosition::new(x, y),
        },
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        },
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        },
    ]
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
        device_id: unsafe { std::mem::zeroed() },
        phase,
        location: PhysicalPosition::new(x, y),
        force: None,
        id: u64::from(slot),
    })
}

/// Send an already arbitrated direct-touch event across the Wayland
/// `wl_touch` seat. `touch_target` deliberately runs only for `Down`, where
/// Wayland establishes touch focus.
fn forward_test_control_touch(
    state: &mut Option<&mut State>,
    fb: &mut FrameBuilder,
    touch: crate::compositor::backend::TouchEvent,
) {
    let Some(state) = state.as_deref_mut() else {
        return;
    };
    let target = client_input::touch_target(state, fb, &touch);
    if let Some((surface_id, _)) = target
        && matches!(touch, crate::compositor::backend::TouchEvent::Down { .. })
    {
        client_input::focus_touch_target(state, fb, surface_id);
    }
    state.forward_client_touch(touch, target);
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
    let raw = |id, raw_x, raw_y, raw_phase, fb: &mut FrameBuilder, ctx: &mut TestControlCtx| {
        let event = test_touch_input_event(id, raw_x, raw_y, raw_phase);
        // Direct touch belongs to the shell gesture arena. Unlike pointer
        // input it must not first be routed as a client pointer event.
        fb.dispatch_event(
            &event,
            &ctx.modifiers,
            &mut ctx.doc,
            &ctx.root_obj,
            &mut ctx.focus,
        );
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
                raw(id, x, y, TouchPhase::Started, fb, ctx);
                forward_test_control_touch(state, fb, BackendTouchEvent::Down { slot: id, x, y });
            }
            DirectTouchDown::CancelClientForShell(id) => {
                raw(id, x, y, TouchPhase::Started, fb, ctx);
                forward_test_control_touch(state, fb, BackendTouchEvent::Cancel);
                frame(&mut ctx.direct_touch, state, fb);
            }
            DirectTouchDown::ShellOwned(id) => raw(id, x, y, TouchPhase::Started, fb, ctx),
        },
        TouchPhaseKind::Motion => {
            if let Some((id, to_client)) = ctx.direct_touch.motion(&key, (x, y)) {
                raw(id, x, y, TouchPhase::Moved, fb, ctx);
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
            if let Some(((last_x, last_y), id, to_client, emulate_pointer)) =
                ctx.direct_touch.up(&key)
            {
                raw(id, last_x, last_y, TouchPhase::Ended, fb, ctx);
                if to_client {
                    forward_test_control_touch(state, fb, BackendTouchEvent::Up { slot: id });
                }
                if emulate_pointer
                    && state
                        .as_deref()
                        .is_none_or(State::touch_tap_needs_pointer_click)
                {
                    for event in touch_tap_pointer_events(last_x, last_y) {
                        dispatch_synth_event(&event, fb, ctx, state);
                    }
                }
            }
        }
        TouchPhaseKind::Cancel => {
            let forward_cancel = ctx.direct_touch.client_stream_active();
            for (id, (last_x, last_y)) in ctx.direct_touch.cancel() {
                raw(id, last_x, last_y, TouchPhase::Cancelled, fb, ctx);
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
    // Re-resolve rather than carry a handle from dispatch: the document is
    // mutable between planning and execution, so we run the current action or nothing.
    let chrome_handle = fb.chrome_handle();
    // Built through the same gate the dispatcher resolved against — without
    // it, a revoked capability wouldn't stop a caller who got past it once.
    let resource = ResourceRef::new(provider, id);
    let catalog = match with_action_gate(&chrome_handle, |chrome, gate| {
        fb.host()
            .action_catalog_for_resource(&resource, chrome, &ctx.doc, &ctx.root_obj, gate)
    }) {
        Ok(catalog) => catalog,
        Err(why) => {
            eprintln!("tilo compositor: cannot invoke ({provider}, {id}): {why}");
            return Err(InvokeFailure {
                code: "action_failed".into(),
                message: why,
            });
        }
    };
    // The `Ref` above is dropped with the block, so the provider's callback
    // is free to borrow the chrome cell mutably — which most of them do.
    if let Some(selected) = catalog.select(&resource) {
        // Re-bound against *this* resolution's parameters — the catalogue was just
        // rebuilt and may have landed on a different action under the same address.
        let bound = selected
            .invoke_json_disposition(&mut ctx.doc, &ctx.root_obj, &chrome_handle, args)
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
                device_id: unsafe { std::mem::zeroed() },
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
                device_id: unsafe { std::mem::zeroed() },
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
                device_id: unsafe { std::mem::zeroed() },
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
        PlannedAction::Touchpad(edge) => {
            if let Some(gesture) = feed_touchpad_edge(&mut ctx.gestures, *edge) {
                apply_shell_gesture(fb, gesture);
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
        PlannedAction::MouseWheel { dx, dy } => {
            let event = WindowEvent::MouseWheel {
                device_id: unsafe { std::mem::zeroed() },
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
            let drag = state.as_ref().and_then(|s| {
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
            });
            write_snapshot_png(fb, path, drag, ctx.snapshot_crop);
        }
        PlannedAction::InvokeAction { provider, id, args } => {
            return Some(execute_invoke_action(provider, id, args, fb, ctx));
        }
    }
    None
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
) {
    match action {
        PlannedAction::KeyboardKey { key, pressed } => {
            let winit_key = keytag_into_winit_key(*key);
            let event = keyboard_input_event(&winit_key, *pressed);
            if let WindowEvent::KeyboardInput { event: key_ev, .. } = &event {
                client_input::apply_modifier_key(&mut ctx.modifiers, key_ev);
            }
            client_input::consume_keyboard_while_captured(state, &event);
            lock.handle_key(winit_event_to_lock_key(&event));
        }
        // Pointer, modifiers, snapshots and action invocations are all swallowed
        // while locked. `InvokeAction` matters most here: the action API reaches
        // chrome side-effects with no visible surface, so letting it through would
        // make the lock screen bypassable by a socket client that renders nothing.
        PlannedAction::CursorMoved { .. }
        | PlannedAction::MouseLeftButton { .. }
        | PlannedAction::MousePointerButton { .. }
        | PlannedAction::Touch { .. }
        | PlannedAction::Touchpad(_)
        | PlannedAction::MouseWheel { .. }
        | PlannedAction::SetModifiers { .. }
        | PlannedAction::Snapshot { .. }
        | PlannedAction::SetGroupWidth { .. }
        | PlannedAction::SeedFilesDrag { .. }
        | PlannedAction::InvokeAction { .. } => {}
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

/// How many `OffscreenRenderer`s [`write_snapshot_png`] has built: one per
/// distinct viewport size, *not* per thread — what
/// `snapshot_renderer_outlives_the_thread_that_built_it` asserts, since the
/// abort a per-thread cache causes can't be observed from inside the binary it kills.
static SNAPSHOT_RENDERERS_BUILT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Re-paint the current scene into a cached `OffscreenRenderer` and encode
/// it as a PNG at `path`. Best-effort: bad viewport / missing adapter / I/O
/// or encoder failures log to stderr and return rather than take the
/// calloop pump down. Split out of `execute_planned_action` so unit tests
/// can exercise it against a hand-built `FrameBuilder` directly.
fn write_snapshot_png(
    fb: &mut FrameBuilder,
    path: &std::path::Path,
    drag: Option<(
        vello::kurbo::Point,
        crate::compositor::drag_feedback::DragVisual,
    )>,
    crop: Option<vello::kurbo::Rect>,
) {
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
        eprintln!("tilo compositor: test_control snapshot: viewport is {w}x{h}, nothing to write");
        return;
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
    // The renderer is kept between snapshots, rebuilt only on viewport resize.
    //
    // `OffscreenRenderer::try_new` initialises every vello shader pipeline, so
    // building one per call put a full pipeline build inside every snapshot:
    // measured at 1920x1080, ~450ms/frame vs ~70ms actual render+encode.
    // See [`SNAPSHOT_RENDERER`] for why the cache is a `static`, not `thread_local!`.

    // `try_new`, not `new` inside `catch_unwind`: the release profile sets
    // `panic = "abort"`, so a caught panic can't happen — the `Result` is what
    // keeps the compositor alive with no wgpu adapter (CI without llvmpipe).
    // Failure isn't cached: the slot stays empty so a later snapshot retries.
    let cache = SNAPSHOT_RENDERER.get_or_init(|| std::sync::Mutex::new(None));
    // The mutex only needs to guard the renderer slot; encoding the PNG and
    // writing it to disk below are unrelated work and don't need it held, so
    // the guard's scope ends here once `rgba` is copied out.
    let rgba = {
        // A panic elsewhere poisons the mutex without damaging what it guards
        // (an intact renderer or empty slot) — recover rather than compound the failure.
        let mut slot = cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
                    eprintln!("tilo compositor: test_control snapshot: no offscreen renderer: {e}");
                    *slot = None;
                    return;
                }
            }
        }
        let Some(rgba) = slot.as_mut().map(|renderer| renderer.render_rgba(&scene)) else {
            return;
        };
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
        eprintln!("tilo compositor: test_control snapshot: the crop is empty, nothing to write");
        return;
    }
    if let Err(e) = ui::png::write_rgba8(path, w, h, &rgba) {
        eprintln!(
            "tilo compositor: test_control snapshot: write {}: {e}",
            path.display()
        );
    }
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

/// Drain pending bytes from `conn`'s buffer: for each complete `\n`-line,
/// parse a [`test_control::Command`], dispatch against `frame_builder`,
/// execute any resulting `PlannedAction`s, and write the JSON response back.
/// Partial lines stay buffered; EOF/error/write-failure marks the connection
/// closed for the outer loop to drop.
///
/// Cap on the per-line size: `read_line` buffers the whole line regardless
/// of size, so an unbounded no-newline stream could OOM the compositor. 64
/// KiB is well above any legitimate command and below meaningful memory pressure.
fn invocation_response(
    action: &serde_json::Value,
    receipt: &crate::shell::search::ActionReceipt,
) -> test_control::Response {
    test_control::Response::Answer(serde_json::json!({
        "action": action,
        "receipt": receipt,
    }))
}

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

fn pump_pending_test_control_conn(conn: &mut TestControlConn) -> PendingConnectionProgress {
    match flush_test_control_output(conn) {
        PendingConnectionProgress::Idle => {}
        progress => return progress,
    }
    let Some(pending) = conn.pending_invocation.take() else {
        return PendingConnectionProgress::Idle;
    };
    if conn.peer_disconnected() {
        conn.closed = true;
        return PendingConnectionProgress::Closed;
    }
    match pending.poll() {
        PendingTestControlPoll::Waiting(pending) => {
            conn.pending_invocation = Some(pending);
            PendingConnectionProgress::Waiting
        }
        PendingTestControlPoll::Ready(response) => queue_test_control_response(conn, &response),
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
    /// What the agent sees ([`agent_viewport`]): its coordinates' origin and
    /// its screenshots' extent.
    viewport: Rect,
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
        fb.with_chrome(|chrome| {
            chrome.set_single_strip(true);
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
    let viewport = agent_viewport(fb)
        .ok_or_else(|| agent_refusal("no_screen", "the compositor has no screen yet"))?;
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
    fb.with_chrome(|chrome| focused = chrome.focus_strip(strip));
    if focused {
        fb.relayout();
        fb.reconcile_native_focus(&mut ctx.focus);
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
    }))
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
        } => Command::Drag {
            x_from: x_from + ox,
            y_from: y_from + oy,
            x_to: x_to + ox,
            y_to: y_to + oy,
            steps,
        },
        other => other,
    };
    let (response, actions) = dispatch_control_command(command, fb, ctx, state, lock);
    let viewport = scope.viewport;
    let inside =
        |x: f64, y: f64| x >= viewport.x0 && x < viewport.x1 && y >= viewport.y0 && y < viewport.y1;
    if actions
        .iter()
        .any(|action| matches!(action, PlannedAction::CursorMoved { x, y } if !inside(*x, *y)))
    {
        return (
            agent_refusal("outside_viewport", "the pointer would leave the screen"),
            Vec::new(),
        );
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
        cmd => {
            let state = FrameBuilderControlState {
                fb: frame_builder,
                doc: Some((&ctx.doc, &ctx.root_obj)),
                shares: Some(&state.share_registry),
                lock: Some(lock),
            };
            test_control::dispatch_command(&cmd, &state)
        }
    }
}

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
    // Whether any command produced a `PlannedAction` — synthesised input the
    // next frame must reflect; starts the input-latency measurement instrumentation uses.
    let mut injected_input = false;
    loop {
        match pump_pending_test_control_conn(conn) {
            PendingConnectionProgress::Waiting
            | PendingConnectionProgress::OutputPending
            | PendingConnectionProgress::Closed => {
                return injected_input;
            }
            PendingConnectionProgress::Idle | PendingConnectionProgress::WroteResponse => {}
        }
        let mut line = String::new();
        match read_bounded_line(&mut conn.reader, &mut line, TEST_CONTROL_MAX_LINE_BYTES) {
            Ok(None) => {
                conn.closed = true;
                return injected_input;
            }
            Ok(Some(())) => {
                let mut scope: Option<AgentScope> = None;
                let (resp, actions) = match parse_request(&line) {
                    Ok(request) if conn.agent && !request.command.agent_permitted() => (
                        test_control::Response::Err {
                            code: "not_permitted".to_string(),
                            message: AGENT_CONTROL_NOT_PERMITTED.to_string(),
                        },
                        Vec::new(),
                    ),
                    Ok(request) => {
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
                if let Some(scope) = &scope {
                    state.client_input.set_participant(Some(scope.peer));
                    ctx.snapshot_crop = Some(scope.viewport);
                }
                // A planned action that could not run replaces this command's answer.
                // Actions execute before the reply is written; first failure wins, the rest still run.
                let mut failed: Option<InvokeFailure> = None;
                let mut receipt: Option<crate::shell::search::ActionReceipt> = None;
                let mut pending: Option<crate::shell::search::PendingInvocation> = None;
                for action in &actions {
                    if session.is_locked() {
                        // While locked the socket is captured by the lock surface like live input
                        // — actions go to the password field; `poll_verification` delivers unlock.
                        execute_locked_action(action, lock, state, ctx);
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
                if let Some(scope) = &scope {
                    state.client_input.set_participant(None);
                    ctx.snapshot_crop = None;
                    note_agent_focus(state, scope.peer);
                }
                // Re-run layout so the next command/paint sees rects post-mutation — a
                // chrome state change can alter structural layout, leaving stale rects otherwise.
                if !actions.is_empty() {
                    injected_input = true;
                    frame_builder.relayout();
                    frame_builder.reconcile_native_focus(&mut ctx.focus);
                }
                let action_response = match &resp {
                    test_control::Response::Answer(action) => action.clone(),
                    _ => serde_json::Value::Null,
                };
                if let Some(ticket) = pending
                    && failed.is_none()
                {
                    conn.pending_invocation = Some(PendingTestControlInvocation {
                        ticket,
                        action: action_response,
                    });
                    return injected_input;
                }
                let resp = match failed {
                    Some(failure) => test_control::Response::Err {
                        code: failure.code,
                        message: failure.message,
                    },
                    None => match receipt {
                        Some(receipt) => invocation_response(&action_response, &receipt),
                        None => resp,
                    },
                };
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
    match transition {
        SessionTransition::Dimmed => dimmer.dim(),
        SessionTransition::Undimmed => dimmer.restore(),
        SessionTransition::Blanked => {
            backend.set_dpms(false);
            if let Some(p) = pipe {
                p.set_dpms(false);
            }
        }
        SessionTransition::Woke => {
            backend.set_dpms(true);
            if let Some(p) = pipe {
                p.set_dpms(true);
            }
        }
        // Lock/Unlock are reflected via `SessionManager::is_locked`; Suspend is
        // gated by the inhibitor registry + issued through the power service, never here.
        SessionTransition::Locked | SessionTransition::Unlocked | SessionTransition::Suspend => {}
    }
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
        WindowEvent::KeyboardInput { event, .. } => {
            keycode_for_eis(event.physical_key).map(|keycode| CapturedInput::Keyboard {
                keycode,
                pressed: event.state == ElementState::Pressed,
            })
        }
        _ => None,
    }
}

/// USB HID / evdev keycodes for keys whose physical positions winit exposes.
/// This deliberately maps physical keys, never characters, so a non-US layout
/// still sees the correct key event over EIS.
fn keycode_for_eis(key: PhysicalKey) -> Option<u32> {
    let PhysicalKey::Code(code) = key else {
        return None;
    };
    Some(match code {
        KeyCode::Escape => 1,
        KeyCode::Digit1 => 2,
        KeyCode::Digit2 => 3,
        KeyCode::Digit3 => 4,
        KeyCode::Digit4 => 5,
        KeyCode::Digit5 => 6,
        KeyCode::Digit6 => 7,
        KeyCode::Digit7 => 8,
        KeyCode::Digit8 => 9,
        KeyCode::Digit9 => 10,
        KeyCode::Digit0 => 11,
        KeyCode::Minus => 12,
        KeyCode::Equal => 13,
        KeyCode::Backspace => 14,
        KeyCode::Tab => 15,
        KeyCode::KeyQ => 16,
        KeyCode::KeyW => 17,
        KeyCode::KeyE => 18,
        KeyCode::KeyR => 19,
        KeyCode::KeyT => 20,
        KeyCode::KeyY => 21,
        KeyCode::KeyU => 22,
        KeyCode::KeyI => 23,
        KeyCode::KeyO => 24,
        KeyCode::KeyP => 25,
        KeyCode::BracketLeft => 26,
        KeyCode::BracketRight => 27,
        KeyCode::Enter => 28,
        KeyCode::ControlLeft => 29,
        KeyCode::KeyA => 30,
        KeyCode::KeyS => 31,
        KeyCode::KeyD => 32,
        KeyCode::KeyF => 33,
        KeyCode::KeyG => 34,
        KeyCode::KeyH => 35,
        KeyCode::KeyJ => 36,
        KeyCode::KeyK => 37,
        KeyCode::KeyL => 38,
        KeyCode::Semicolon => 39,
        KeyCode::Quote => 40,
        KeyCode::Backquote => 41,
        KeyCode::ShiftLeft => 42,
        KeyCode::Backslash => 43,
        KeyCode::KeyZ => 44,
        KeyCode::KeyX => 45,
        KeyCode::KeyC => 46,
        KeyCode::KeyV => 47,
        KeyCode::KeyB => 48,
        KeyCode::KeyN => 49,
        KeyCode::KeyM => 50,
        KeyCode::Comma => 51,
        KeyCode::Period => 52,
        KeyCode::Slash => 53,
        KeyCode::ShiftRight => 54,
        KeyCode::AltLeft => 56,
        KeyCode::Space => 57,
        KeyCode::CapsLock => 58,
        KeyCode::F1 => 59,
        KeyCode::F2 => 60,
        KeyCode::F3 => 61,
        KeyCode::F4 => 62,
        KeyCode::F5 => 63,
        KeyCode::F6 => 64,
        KeyCode::F7 => 65,
        KeyCode::F8 => 66,
        KeyCode::F9 => 67,
        KeyCode::F10 => 68,
        KeyCode::NumLock => 69,
        KeyCode::ScrollLock => 70,
        KeyCode::F11 => 87,
        KeyCode::F12 => 88,
        KeyCode::ControlRight => 97,
        KeyCode::AltRight => 100,
        KeyCode::Home => 102,
        KeyCode::ArrowUp => 103,
        KeyCode::PageUp => 104,
        KeyCode::ArrowLeft => 105,
        KeyCode::ArrowRight => 106,
        KeyCode::End => 107,
        KeyCode::ArrowDown => 108,
        KeyCode::PageDown => 109,
        KeyCode::Insert => 110,
        KeyCode::Delete => 111,
        KeyCode::SuperLeft => 125,
        KeyCode::SuperRight => 126,
        KeyCode::ContextMenu => 127,
        _ => return None,
    })
}

#[derive(Clone, Debug)]
struct SpatialControlSession {
    target: crate::shell::personal_continuity::DeviceId,
    return_edge: crate::shell::device_fabric::DisplayEdge,
    x: f64,
    y: f64,
    pressed_keys: std::collections::BTreeSet<u32>,
    pressed_buttons: std::collections::BTreeSet<u32>,
    nearby_drop: Option<std::path::PathBuf>,
}

fn end_spatial_control(
    network: &crate::shell::network::ShellNetwork,
    active: &mut Option<SpatialControlSession>,
) {
    let Some(session) = active.take() else {
        return;
    };
    for keycode in session.pressed_keys {
        let _ = crate::shell::device_fabric_runtime::send_spatial_input(
            network,
            &session.target,
            crate::shell::device_fabric::FabricInputEvent::Key {
                keycode,
                pressed: false,
                modifiers: 0,
            },
        );
    }
    for button in session.pressed_buttons {
        let _ = crate::shell::device_fabric_runtime::send_spatial_input(
            network,
            &session.target,
            crate::shell::device_fabric::FabricInputEvent::PointerButton {
                button,
                pressed: false,
            },
        );
    }
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
fn spatial_scroll(value: f64) -> i16 {
    value
        .clamp(f64::from(i16::MIN), f64::from(i16::MAX))
        .round() as i16
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

fn move_spatial_pointer(
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
        if crate::shell::device_fabric_runtime::send_spatial_input(
            network,
            &session.target,
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
    let pending_drag = file_explorer::external_drag::unclaimed();
    let Ok(nearby_drop) =
        nearby_drop_candidate(pending_drag.as_ref().map(|drag| drag.paths.as_slice()))
    else {
        // Keep unsupported Files drags local; crossing would otherwise swallow
        // their release and strand the gesture on the source device.
        return false;
    };
    let session = SpatialControlSession {
        target: transition.to,
        return_edge: edge.opposite(),
        x: f64::from(transition.target_x) + dx,
        y: f64::from(transition.target_y) + dy,
        pressed_keys: std::collections::BTreeSet::new(),
        pressed_buttons: std::collections::BTreeSet::new(),
        nearby_drop,
    };
    if crate::shell::device_fabric_runtime::send_spatial_input(
        network,
        &session.target,
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
mod spatial_edge_tests {
    use super::{nearby_drop_candidate, spatial_departure_edge};
    use crate::shell::device_fabric::DisplayEdge;

    #[test]
    fn pointer_departs_through_edges_and_arms_nearby_drops() {
        // Phase `pointer_can_depart_through_all_four_edges`.
        assert_eq!(
            spatial_departure_edge(0.0, 400.0, 1_000, 800, -2.0, 0.0),
            Some(DisplayEdge::Left)
        );
        assert_eq!(
            spatial_departure_edge(999.0, 400.0, 1_000, 800, 2.0, 0.0),
            Some(DisplayEdge::Right)
        );
        assert_eq!(
            spatial_departure_edge(500.0, 0.0, 1_000, 800, 0.0, -2.0),
            Some(DisplayEdge::Top)
        );
        assert_eq!(
            spatial_departure_edge(500.0, 799.0, 1_000, 800, 0.0, 2.0),
            Some(DisplayEdge::Bottom)
        );
        // Phase `corner_departure_follows_the_dominant_motion_axis`.
        assert_eq!(
            spatial_departure_edge(999.0, 799.0, 1_000, 800, 8.0, 3.0),
            Some(DisplayEdge::Right)
        );
        assert_eq!(
            spatial_departure_edge(999.0, 799.0, 1_000, 800, 3.0, 8.0),
            Some(DisplayEdge::Bottom)
        );
        assert_eq!(
            spatial_departure_edge(500.0, 400.0, 1_000, 800, 8.0, 8.0),
            None
        );
        // Phase `one_safe_file_or_folder_can_arm_a_nearby_drop`.
        {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("note.txt");
            std::fs::write(&path, b"hello").unwrap();
            assert_eq!(
                nearby_drop_candidate(Some(std::slice::from_ref(&path))),
                Ok(Some(path.clone()))
            );
            let folder = temp.path().join("folder");
            std::fs::create_dir(&folder).unwrap();
            assert_eq!(
                nearby_drop_candidate(Some(std::slice::from_ref(&folder))),
                Ok(Some(folder))
            );
            let link = temp.path().join("link");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            assert!(nearby_drop_candidate(Some(&[link])).is_err());
            assert!(nearby_drop_candidate(Some(&[path.clone(), path])).is_err());
            assert!(nearby_drop_candidate(Some(&[])).is_err());
        }
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
            let _ =
                crate::shell::device_fabric_runtime::request_nearby_drop(network, &target, &path);
        }
        return Some(false);
    }
    None
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
    if let WindowEvent::KeyboardInput { event, .. } = event
        && event.state == ElementState::Pressed
        && matches!(event.logical_key, Key::Named(NamedKey::Escape))
    {
        end_spatial_control(network, active);
        return true;
    }
    let target = session.target.clone();
    let fabric_event = match event {
        WindowEvent::CursorMoved { .. } => return true,
        WindowEvent::MouseInput { state, button, .. } => {
            let button = match button {
                MouseButton::Left => 0x110,
                MouseButton::Right => 0x111,
                MouseButton::Middle => 0x112,
                MouseButton::Back => 0x116,
                MouseButton::Forward => 0x115,
                MouseButton::Other(button) => u32::from(*button),
            };
            crate::shell::device_fabric::FabricInputEvent::PointerButton {
                button,
                pressed: *state == ElementState::Pressed,
            }
        }
        WindowEvent::MouseWheel { delta, .. } => {
            let (horizontal, vertical) = match delta {
                MouseScrollDelta::LineDelta(x, y) => {
                    (spatial_scroll(f64::from(*x)), spatial_scroll(f64::from(*y)))
                }
                MouseScrollDelta::PixelDelta(position) => {
                    (spatial_scroll(position.x), spatial_scroll(position.y))
                }
            };
            if horizontal == 0 && vertical == 0 {
                return true;
            }
            crate::shell::device_fabric::FabricInputEvent::Scroll {
                horizontal,
                vertical,
            }
        }
        WindowEvent::KeyboardInput { event, .. } => {
            let Some(keycode) = keycode_for_eis(event.physical_key) else {
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
    if crate::shell::device_fabric_runtime::send_spatial_input(network, &target, fabric_event)
        .is_err()
    {
        *active = None;
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
        summary: n.summary,
        body: n.body,
        actions: Vec::new(),
        // Critical battery lingers until dismissed; the low heads-up uses the server default expiry.
        expire_timeout: if n.critical { 0 } else { -1 },
        urgency: if n.critical { 2 } else { 1 },
        caller: None,
        badge_count: None,
        contact_endpoint: None,
        sound: Some(settings_store::system_sounds::SystemSoundEvent::LowBattery),
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
/// Media keys are the one exception to the lock capturing everything:
/// volume, mute, brightness and playback belong to the machine, not the
/// session, so they keep working on the lock screen. They reveal nothing
/// behind it and their feedback is drawn on the lock surface itself.
fn priority_route(
    event: &WindowEvent,
    modifiers: Modifiers,
    locked: bool,
    shortcuts: &settings_store::hardware::ShortcutConfig,
) -> PriorityRoute {
    let media_key = crate::shell::media_keys::classify(event);
    if locked {
        return media_key
            .filter(|key| key.acts_while_locked())
            .map_or(PriorityRoute::LockScreen, PriorityRoute::MediaKey);
    }
    if is_power_key(event) {
        PriorityRoute::PowerKey
    } else if let Some(vt) = vt_switch_chord(event, modifiers) {
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

/// How long an invite minted from the palette stays valid.
const INVITE_EXPIRY_SECONDS: u64 = 3600;

/// Load this machine's desktop state and bring it up to the invariants
/// later stages assume: a persist path, a durable room identity per
/// workspace, and an owner per room — an unowned room has no administrators.
///
/// Returns the tree plus whether peer sync may come up; refused when the
/// device key won't load, since the id salt comes from it and an unsalted id space lets peers mint colliding ids.
fn load_session_tree(
    enable_peer_sync: bool,
) -> (
    crate::shell::state::WorkspaceTreeHandle,
    bool,
    Option<crate::shell::persist::StateNotice>,
) {
    use crate::shell::persist;

    let path = persist::state_json_path();

    // The id salt is set **before** the tree is loaded — load-bearing order.
    //
    // Two peers minting ids from the same counter collide, and CRDT sync
    // folds them into one row. The salt (from the Iroh keypair) puts each
    // peer in a disjoint id range, making an id safe as a replication key.
    //
    // Setting it after load left `bootstrap_default`'s first-boot ids
    // unsalted — identical on every machine, so two such peers joining one
    // room would collide. Re-seeding afterwards didn't help: it fixes ids minted *next*, not ones already in the tree.
    let mut peer_sync_possible = enable_peer_sync;
    let peer_key_path = crate::shell::network::peer_key_path();
    match load_or_create_workspace_salt(&path, &peer_key_path) {
        Ok(salt) => {
            crate::shell::state::WorkspaceTree::set_process_peer_salt(salt);
            tracing::info!("workspace id salt set to 0x{:04x}", salt);
        }
        Err(error) => {
            peer_sync_possible = false;
            tracing::error!(
                %error,
                "workspace id salt unavailable; peer sync stays off rather than using an unstable id namespace"
            );
        }
    }
    if peer_sync_possible {
        match crate::network::behaviour::load_or_create_keypair(&peer_key_path) {
            Ok(kp) => {
                tracing::info!(endpoint = %kp.public(), "peer-sync: device identity loaded");
            }
            Err(e) => {
                peer_sync_possible = false;
                tracing::error!(
                    "peer-sync: device key load failed ({e}); peer sync stays off \
                     rather than syncing an unsalted id space"
                );
            }
        }
    }

    // Carried out to `run`, which has the chrome to show it — silently handing
    // someone a bootstrapped demo tree is indistinguishable from losing their work.
    let (mut tree, state_notice) = persist::load_or_default(&path);
    // Recover cards written by builds that mistook a host window's toolkit
    // identity for an uninstalled package. The host catalogue is available
    // before the frame loop starts, so this one-shot migration can distinguish
    // a now-resolvable local app from a genuinely missing one without making
    // the latter retry forever.
    let recovered_host_apps =
        crate::shell::peer_windows::retry_resolvable_host_app_errors(&mut tree);
    // Past every id already in the tree, so a restart won't re-mint one in use. No-op on a first boot.
    tree.seed_next_id_from_self();

    // Nothing writes back over a file this session refused to read — a refused
    // session runs without persistence, like `ui::persist` disabling a writer over a document it wouldn't open.
    tree.persist_path = if state_notice.is_none() {
        Some(path)
    } else {
        None
    };
    tree.fs_acl_path = Some(persist::fs_capability_export_path());
    if recovered_host_apps {
        tracing::info!(
            target: "tilo::session_restore",
            "retrying legacy host-app cards that now resolve to installed desktop entries"
        );
        tree.flush_to_disk();
    }

    let adopted = tree.adopt_ownerless_workspaces_with(crate::shell::state::local_user_id_for_room);
    if !adopted.is_empty() {
        tracing::info!(
            "capabilities: {} workspace(s) adopted by their selected identities",
            adopted.len()
        );
    }
    if let Some(account) = std::env::var("USER")
        .ok()
        .filter(|account| !account.is_empty())
    {
        let rebound = tree
            .rebind_account_owned_workspaces(&account, crate::shell::state::local_user_id_for_room);
        if !rebound.is_empty() {
            tracing::info!(
                "capabilities: {} workspace(s) created before this device's identity now belong to it",
                rebound.len()
            );
        }
    }
    // Replay the persisted aggregate on every boot. A previous session may
    // have stopped while the authority socket was unavailable; the
    // background publisher coalesces this with any immediate mutation.
    tree.export_fs_capabilities();
    (tree.shared(), peer_sync_possible, state_notice)
}

/// Is something already listening on `path`? The collaboration broker
/// unlinks before it binds, so without this check a second compositor on
/// the same `XDG_RUNTIME_DIR` would steal the socket from the running
/// session, splitting apps across two unreachable brokers. A successful
/// connect means a live listener; `ECONNREFUSED` means a stale inode we do want to clear.
fn collab_socket_is_live(path: &std::path::Path) -> bool {
    path.exists() && std::os::unix::net::UnixStream::connect(path).is_ok()
}

/// Bind the session's CRDT fan-out broker on the service socket the
/// launcher hands to apps. `None` means a degraded session, not a normal
/// one — every app manifest declares `collab`, so nothing collaborates
/// until this succeeds; reported at `error!` and surfaced via `chrome_state::set_collab_unavailable`.
fn spawn_session_collab_broker(
    chrome: &crate::shell::chrome_state::Chrome,
    peer_sync: Option<&Arc<crate::shell::network::ShellNetwork>>,
) -> Option<crate::shell::collab_broker::BrokerHandle> {
    if let Err(e) = launcher::services::ensure_services_root() {
        tracing::error!(
            "collab: cannot create {} ({e}); apps will start without collaboration",
            launcher::services::services_root().display()
        );
        chrome
            .borrow_mut()
            .set_collab_unavailable(crate::shell::i18n::t_args(
                "collab-unavailable-services-root",
                &[("error", e.to_string().into())],
            ));
        return None;
    }

    let capnp_path = launcher::services::collab_socket();
    if collab_socket_is_live(&capnp_path) {
        tracing::error!(
            "collab: {} already has a live listener — another session is using this \
             XDG_RUNTIME_DIR; refusing to take its socket. Apps will start without \
             collaboration.",
            capnp_path.display()
        );
        chrome
            .borrow_mut()
            .set_collab_unavailable(crate::shell::i18n::t("collab-unavailable-socket-taken"));
        return None;
    }

    let peer_sync_owned = peer_sync.cloned();
    let membership = chrome.borrow().tree_handle();
    match crate::shell::collab_broker::spawn_native(
        capnp_path.clone(),
        &peer_sync_owned,
        membership,
    ) {
        Ok(handle) => {
            tracing::info!(
                "collab: typed broker listening on {} — peer_sync={}",
                capnp_path.display(),
                peer_sync.is_some()
            );
            crate::shell::channel_encryption::ChannelKeyProvider::session()
                .set(handle.channel_encryption());
            chrome.borrow_mut().clear_collab_unavailable();
            Some(handle)
        }
        Err(e) => {
            tracing::error!(
                "collab: bind {} failed ({e}); apps will start without collaboration",
                capnp_path.display()
            );
            chrome
                .borrow_mut()
                .set_collab_unavailable(crate::shell::i18n::t_args(
                    "collab-unavailable-broker",
                    &[("error", e.to_string().into())],
                ));
            None
        }
    }
}

fn active_physical_gamepad_binding(
    tree: &crate::shell::state::WorkspaceTree,
) -> Option<crate::shell::physical_gamepad::PhysicalGamepadBinding> {
    let workspace = tree.desktop.active_workspace()?;
    let strip_id = tree
        .desktop
        .active_strip
        .get(&workspace.id)
        .copied()
        .or_else(|| workspace.strips.first().map(|strip| strip.id))?;
    let strip = workspace.strips.iter().find(|strip| strip.id == strip_id)?;
    let group_id = tree
        .desktop
        .active_group_for(strip_id)
        .filter(|group_id| {
            strip
                .window_groups
                .iter()
                .any(|group| group.id == *group_id)
        })
        .or_else(|| strip.window_groups.first().map(|group| group.id))?;
    let group = strip
        .window_groups
        .iter()
        .find(|group| group.id == group_id)?;
    let tab = group.tabs.iter().find(|tab| tab.id == group.active_tab)?;
    let app_id = tilo_desktop_model::apps::canonical_app_id(&tab.tile)?;
    let workspace_id = workspace.room.to_string();
    Some(crate::shell::physical_gamepad::PhysicalGamepadBinding {
        app_id,
        session_id: format!("{workspace_id}:{}", tab.id),
        workspace_id,
    })
}

#[expect(
    clippy::too_many_lines,
    reason = "one bridge closure maps every Quick Settings fabric action onto its runtime call beside the shared snapshot cache"
)]
fn install_device_fabric_bridge(
    host: &mut crate::shell::overlay_host::ShellOverlayHost,
    fabric_net: Arc<crate::shell::network::ShellNetwork>,
    tree_handle: crate::shell::state::WorkspaceTreeHandle,
) {
    let fabric_cache = std::rc::Rc::new(std::cell::RefCell::new(
        None::<(Instant, crate::shell::overlay_host::DeviceFabricView)>,
    ));
    host.set_device_fabric_bridge({
        let fabric_cache = std::rc::Rc::clone(&fabric_cache);
        std::rc::Rc::new(move |action| {
            use crate::shell::overlay_host::DeviceFabricUiAction;
            if matches!(action, DeviceFabricUiAction::Snapshot)
                && let Some((updated, view)) = fabric_cache.borrow().as_ref()
                && updated.elapsed() < Duration::from_millis(750)
            {
                return Ok(view.clone());
            }
            match action {
                DeviceFabricUiAction::Snapshot | DeviceFabricUiAction::SnapshotFresh => {}
                DeviceFabricUiAction::HandoffMedia { session_id, target } => {
                    let _ = crate::shell::device_fabric_runtime::request_media_handoff(
                        &fabric_net,
                        &session_id,
                        &target,
                    )?;
                }
                DeviceFabricUiAction::CancelMediaHandoff { request_id } => {
                    crate::shell::device_fabric_runtime::cancel_media_handoff(
                        &fabric_net,
                        request_id,
                    )?;
                }
                DeviceFabricUiAction::RetryMediaHandoff { request_id } => {
                    let _ = crate::shell::device_fabric_runtime::retry_media_handoff(
                        &fabric_net,
                        request_id,
                    )?;
                }
                DeviceFabricUiAction::RouteLocalAudio { node_id } => {
                    crate::shell::device_fabric_runtime::route_local_audio(node_id)?;
                }
                DeviceFabricUiAction::SelectLocalInput { node_id } => {
                    crate::shell::device_fabric_runtime::select_local_input(node_id)?;
                }
                DeviceFabricUiAction::StepInputLevel { node_id, raise } => {
                    crate::shell::device_fabric_runtime::step_input_level(node_id, raise)?;
                }
                DeviceFabricUiAction::HandoffBluetooth {
                    address,
                    name,
                    target,
                } => {
                    let _ = crate::shell::device_fabric_runtime::request_bluetooth_handoff(
                        &fabric_net,
                        &address,
                        &name,
                        &target,
                    )?;
                }
                DeviceFabricUiAction::AuthorizeBluetoothHandoff {
                    request_id,
                    approved,
                } => {
                    crate::shell::device_fabric_runtime::authorize_bluetooth_handoff(
                        &fabric_net,
                        request_id,
                        approved,
                    )?;
                }
                DeviceFabricUiAction::CancelBluetoothHandoff { request_id } => {
                    crate::shell::device_fabric_runtime::cancel_bluetooth_handoff(
                        &fabric_net,
                        request_id,
                    )?;
                }
                DeviceFabricUiAction::RetryBluetoothHandoff { request_id } => {
                    let _ = crate::shell::device_fabric_runtime::retry_bluetooth_handoff(
                        &fabric_net,
                        request_id,
                    )?;
                }
                DeviceFabricUiAction::AuthorizeGamepadForwarding {
                    forwarding_id,
                    approved,
                } => {
                    crate::shell::device_fabric_runtime::authorize_typed_gamepad_forwarding(
                        forwarding_id,
                        approved,
                    )?;
                }
                DeviceFabricUiAction::StartPhysicalGamepad {
                    source_token,
                    target,
                } => {
                    let binding = active_physical_gamepad_binding(&tree_handle.read())
                        .ok_or_else(|| crate::shell::i18n::t("fabric-focus-app-for-controller"))?;
                    let _ = crate::shell::physical_gamepad::start_physical_gamepad_forwarding(
                        &fabric_net,
                        source_token,
                        &target,
                        binding,
                    )?;
                }
                DeviceFabricUiAction::StopPhysicalGamepad { session } => {
                    crate::shell::physical_gamepad::stop_physical_gamepad_forwarding(
                        &fabric_net,
                        session,
                    )?;
                }
                DeviceFabricUiAction::StartRemoteAudio {
                    source_token,
                    target,
                } => {
                    let _ = crate::shell::device_fabric_runtime::request_remote_audio(
                        &fabric_net,
                        source_token,
                        &target,
                    )?;
                }
                DeviceFabricUiAction::AuthorizeRemoteAudio {
                    session_id,
                    approved,
                } => {
                    crate::shell::device_fabric_runtime::authorize_remote_audio(
                        &fabric_net,
                        session_id,
                        approved,
                    )?;
                }
                DeviceFabricUiAction::StopRemoteAudio { session_id } => {
                    crate::shell::device_fabric_runtime::stop_remote_audio(
                        &fabric_net,
                        session_id,
                    )?;
                }
                DeviceFabricUiAction::ResolveSpeakerGroupConsent {
                    group,
                    kind,
                    approved,
                } => {
                    crate::shell::device_fabric_runtime::require_speaker_groups()?;
                    crate::shell::speaker_group_runtime::resolve_consent(
                        &fabric_net,
                        group,
                        kind,
                        approved,
                    )?;
                }
                DeviceFabricUiAction::LeaveSpeakerGroup { group, kind } => {
                    crate::shell::speaker_group_runtime::leave_group(&fabric_net, group, kind)?;
                }
                DeviceFabricUiAction::StartSpeakerGroupAudio {
                    source_token,
                    group,
                } => {
                    let _ = crate::shell::device_fabric_runtime::request_speaker_group_audio(
                        &fabric_net,
                        source_token,
                        group,
                    )?;
                }
                DeviceFabricUiAction::PlayMediaOnSpeakerGroup { session_id, group } => {
                    let _ = crate::shell::device_fabric_runtime::play_media_on_speaker_group(
                        &fabric_net,
                        &session_id,
                        group,
                    )?;
                }
                DeviceFabricUiAction::StopSpeakerGroupSession { session_id } => {
                    crate::shell::speaker_group_runtime::stop_session(&fabric_net, session_id)?;
                }
                DeviceFabricUiAction::RequestRemoteCapture { peer, kind } => {
                    let binding =
                        active_physical_gamepad_binding(&tree_handle.read()).ok_or_else(|| {
                            crate::shell::i18n::t("fabric-focus-app-for-remote-input")
                        })?;
                    let app =
                        crate::shell::realtime_session::VerifiedAppRegistration::new_verified(
                            binding.app_id,
                            binding.workspace_id,
                        )
                        .map_err(|error| error.to_string())?;
                    crate::shell::remote_capture::runtime::request_remote_capture(
                        &fabric_net,
                        app,
                        peer,
                        kind,
                    )?;
                }
                DeviceFabricUiAction::AuthorizeRemoteCapture {
                    request_id,
                    approved,
                } => {
                    crate::shell::remote_capture::runtime::decide_remote_capture(
                        &fabric_net,
                        request_id,
                        approved,
                    )?;
                }
                DeviceFabricUiAction::ApproveRemoteCaptureTarget { session_id } => {
                    crate::shell::remote_capture::runtime::approve_remote_capture_target(
                        session_id,
                    )?;
                }
                DeviceFabricUiAction::StopRemoteCapture { session_id } => {
                    crate::shell::remote_capture::runtime::stop_remote_capture(
                        &fabric_net,
                        session_id,
                    )?;
                }
                DeviceFabricUiAction::RequestDisplayMirror { target } => {
                    let _ = crate::shell::device_fabric_runtime::request_display_mirror(
                        &fabric_net,
                        &target,
                    )?;
                }
                DeviceFabricUiAction::ResolveDisplayMirror {
                    session_id,
                    approved,
                } => {
                    crate::shell::device_fabric_runtime::resolve_display_mirror_consent(
                        &fabric_net,
                        session_id,
                        approved,
                    )?;
                }
                DeviceFabricUiAction::StopDisplayMirror { session_id } => {
                    crate::shell::device_fabric_runtime::stop_display_mirror(
                        &fabric_net,
                        session_id,
                    )?;
                }
                DeviceFabricUiAction::RequestPhoneTethering { phone } => {
                    let _ = crate::shell::personal_tethering_runtime::request_phone_tethering(
                        &fabric_net,
                        &phone,
                    )?;
                }
                DeviceFabricUiAction::AuthorizePhoneTethering {
                    request_id,
                    approved,
                } => {
                    crate::shell::personal_tethering_runtime::authorize_phone_tethering(
                        request_id, approved,
                    )?;
                }
                DeviceFabricUiAction::DisconnectPhoneTethering { request_id } => {
                    crate::shell::personal_tethering_runtime::disconnect_phone_tethering(
                        request_id,
                    )?;
                }
                DeviceFabricUiAction::Place { device, edge } => {
                    crate::shell::device_fabric_runtime::place_device(&fabric_net, &device, edge)?;
                }
                DeviceFabricUiAction::ApplyCalibration { device } => {
                    crate::shell::device_fabric_runtime::apply_spatial_calibration(
                        &fabric_net,
                        &device,
                    )?;
                }
                DeviceFabricUiAction::DismissCalibration { device } => {
                    crate::shell::device_fabric_runtime::dismiss_spatial_calibration(&device)?;
                }
                DeviceFabricUiAction::RevertCalibration { device } => {
                    crate::shell::device_fabric_runtime::revert_spatial_calibration(
                        &fabric_net,
                        &device,
                    )?;
                }
                DeviceFabricUiAction::GrantControl { controller } => {
                    let _ = crate::shell::device_fabric_runtime::grant_input_control(
                        &fabric_net,
                        &controller,
                    )?;
                }
                DeviceFabricUiAction::RevokeControl { controller } => {
                    crate::shell::device_fabric_runtime::revoke_input_control(
                        &fabric_net,
                        &controller,
                    )?;
                }
            }
            let mut view = crate::shell::device_fabric_runtime::device_fabric_view(&fabric_net);
            view.local_gamepads =
                crate::shell::physical_gamepad::local_physical_gamepads().unwrap_or_default();
            let focus = active_physical_gamepad_binding(&tree_handle.read());
            view.remote_audio =
                crate::shell::device_fabric_runtime::remote_audio_view(focus.as_ref());
            *fabric_cache.borrow_mut() = Some((Instant::now(), view.clone()));
            Ok(view)
        })
    });
}

/// Start the authenticated peer transport and install the chrome commands
/// that use it. Kept as one operation so first-login can call the exact same
/// path after the identity ceremony releases the desktop.
fn visible_wifi_security(ssid: &str) -> Result<Option<os_actions::WifiSecurity>, String> {
    os_actions::list_wifi_networks().map(|networks| {
        networks
            .into_iter()
            .find(|network| network.ssid == ssid)
            .map(|network| network.security)
    })
}

fn install_wifi_secret_bridge(
    host: &mut crate::shell::overlay_host::ShellOverlayHost,
    wifi_net: Arc<crate::shell::network::ShellNetwork>,
) {
    host.set_wifi_secret_bridge(std::rc::Rc::new(move |action| match action {
        crate::shell::overlay_host::WifiSecretAction::Lookup { ssid } => {
            let Some(security) = visible_wifi_security(&ssid)? else {
                return Ok(None);
            };
            Ok(wifi_net.shared_wifi_password(&ssid, security))
        }
        crate::shell::overlay_host::WifiSecretAction::Share { ssid, password } => {
            let security = visible_wifi_security(&ssid)?
                .ok_or_else(|| "could not verify the connected Wi-Fi security mode".to_string())?;
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

/// Start peer sync before the compositor loop runs, while nothing can launch
/// yet. A failure leaves the session without sync.
fn spawn_session_peer_sync(
    frame_builder: &mut FrameBuilder,
    state: &mut State,
    tree_handle: crate::shell::state::WorkspaceTreeHandle,
) -> Option<Arc<crate::shell::network::ShellNetwork>> {
    match crate::shell::network::ShellNetwork::spawn(
        tree_handle.clone(),
        &crate::shell::network::peer_key_path(),
    ) {
        Ok(handle) => Some(install_session_peer_sync(
            frame_builder,
            state,
            tree_handle,
            handle,
        )),
        Err(error) => {
            tracing::warn!(%error, "peer-sync: spawn failed; continuing without sync");
            None
        }
    }
}

/// Start peer sync from inside the running compositor loop. Building its
/// runtime, reading the device key and binding the endpoint block, so they
/// run on the session runtime; the finished network is delivered back
/// through `wake` and installed with [`install_session_peer_sync`].
fn start_session_peer_sync(
    tree_handle: crate::shell::state::WorkspaceTreeHandle,
    wake: crate::session_runtime::Wake,
) -> crate::session_runtime::PendingStart<crate::shell::network::ShellNetwork> {
    match crate::session_runtime::runtime() {
        Ok(runtime) => crate::session_runtime::start_blocking(runtime.handle(), wake, move || {
            crate::shell::network::ShellNetwork::spawn(
                tree_handle,
                &crate::shell::network::peer_key_path(),
            )
            .map_err(|error| error.to_string())
        }),
        Err(error) => crate::session_runtime::PendingStart::finished(Err(format!(
            "the session runtime is unavailable: {error}"
        ))),
    }
}

/// Join the default workspace with a freshly started network and install
/// the chrome commands that use it.
fn install_session_peer_sync(
    frame_builder: &mut FrameBuilder,
    state: &mut State,
    tree_handle: crate::shell::state::WorkspaceTreeHandle,
    handle: crate::shell::network::ShellNetwork,
) -> Arc<crate::shell::network::ShellNetwork> {
    let _ = handle.join_default_workspace();
    tracing::info!("peer-sync: actor spawned, default workspace joined");
    let net = Arc::new(handle);
    net.join_at_boot();
    install_wifi_secret_bridge(frame_builder.host_mut(), Arc::clone(&net));
    crate::network_secrets::store::RoamingWifiSlot::global().set(Arc::clone(&net) as _);
    install_device_fabric_bridge(
        frame_builder.host_mut(),
        Arc::clone(&net),
        tree_handle.clone(),
    );
    frame_builder.host_mut().set_personal_devices(Arc::new(
        crate::shell::device_fabric_runtime::FabricDeviceDirectory::new(Arc::clone(&net)),
    ));
    let records = crate::shell::document_import::NativeHostedRooms(Arc::clone(&net));
    crate::shell::peer_windows::set_hosted_record_reader(Box::new(move |room, app, record| {
        use crate::shell::document_import::HostedRooms as _;
        records.read(room, app, record)
    }));
    let hosted_net = Arc::clone(&net);
    frame_builder.with_chrome(move |chrome| {
        chrome.window_transfers.set_hosted_rooms(Box::new(
            crate::shell::document_import::NativeHostedRooms(hosted_net),
        ));
    });
    let stream_net = Arc::clone(&net);
    state.set_legacy_stream_publisher(Arc::new(move |message| {
        stream_net.publish_legacy_active(message)
    }));
    let bridge_net = Arc::clone(&net);
    frame_builder
        .host_mut()
        .set_peer_sync_bridge(std::rc::Rc::new(move |req| {
            let active = tree_handle.read().desktop.active;
            match req {
                PeerSyncRequest::IssueInvite => bridge_net
                    .issue_invite(active, INVITE_EXPIRY_SECONDS)
                    .map(crate::shell::overlay_host::PeerSyncResponse::Message),
                PeerSyncRequest::ExportIdentity => bridge_net
                    .person_identity_card(active)
                    .map(crate::shell::overlay_host::PeerSyncResponse::Message),
                PeerSyncRequest::IssueInviteFor {
                    name,
                    role,
                    recipient_user_public_key,
                } => bridge_net
                    .issue_invite_bound_with_role(
                        active,
                        INVITE_EXPIRY_SECONDS,
                        &name,
                        recipient_user_public_key,
                        role,
                    )
                    .map(crate::shell::overlay_host::PeerSyncResponse::Message),
                PeerSyncRequest::AcceptInvite(token) => {
                    bridge_net.accept_peer_invitation(active, &token)
                }
                PeerSyncRequest::PollEnrollment(id) => bridge_net.poll_enrollment(id),
                PeerSyncRequest::CancelEnrollment(id) => bridge_net.cancel_enrollment(id),
                PeerSyncRequest::ListPendingInvitations => {
                    let room = tree_handle
                        .read()
                        .desktop
                        .active_workspace()
                        .map(|workspace| workspace.room)
                        .ok_or_else(|| "no active workspace".to_string())?;
                    Ok(
                        crate::shell::overlay_host::PeerSyncResponse::PendingInvitations(
                            bridge_net
                                .pending_invitations(room)
                                .into_iter()
                                .map(|invite| {
                                    crate::shell::overlay_host::PendingInvitationSummary {
                                        id: invite.id,
                                        name: invite.name,
                                        role: invite.role,
                                        expiry_unix_seconds: invite.expiry_unix_seconds,
                                    }
                                })
                                .collect(),
                        ),
                    )
                }
                PeerSyncRequest::ResendInvitation(id) => bridge_net
                    .resend_invitation(&id)
                    .map(crate::shell::overlay_host::PeerSyncResponse::Message),
                PeerSyncRequest::CancelInvitation(id) => {
                    if bridge_net.cancel_invitation(&id) {
                        Ok(crate::shell::overlay_host::PeerSyncResponse::Message(
                            "canceled".into(),
                        ))
                    } else {
                        Err("invitation is no longer pending".into())
                    }
                }
            }
        }));
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
        // An app reaching for another app's action is asked about on this
        // session's own sheet, which the overlay host draws and answers.
        action_broker.set_app_action_consent(std::sync::Arc::new(
            crate::shell::app_action_consent::SheetPrompter::session(),
        ));
        frame_builder
            .host_mut()
            .attach_app_action_broker(action_broker);
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
            let payload = ui::collaboration::PresencePayload::Following { target };
            if let Some(frame) = crate::shell::collab_presence::to_peer(&key, &payload) {
                follow_network.publish_app_presence("tilo-shell".into(), room.to_string(), frame);
            }
        }));
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
            .map(|workspace| workspace.room.to_string())
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
    if collab_socket_is_live(&path) {
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
    #[allow(
        dead_code,
        reason = "Retaining the registration keeps its D-Bus connection, bus name, and object server alive once registered."
    )]
    server: crate::session_runtime::Registration<crate::portal::ServerHandle>,
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
        use crate::{portal::desktop_services::PortalResult, shell::state::FsMode};
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
                    let outcome = capture
                        .capture_portal_screenshot(
                            frame_builder,
                            scene,
                            request.target.unwrap_or(
                                crate::portal::desktop_services::ScreenshotTarget::Screen,
                            ),
                            false,
                        )
                        .and_then(|host_path| {
                            let published =
                                documents.publish(&request.caller, &[host_path], FsMode::Read);
                            let path = published.into_iter().next().ok_or_else(|| {
                                "the document store rejected the screenshot".to_string()
                            })?;
                            crate::portal::file_chooser::file_uri(&path).ok_or_else(|| {
                                "the document store returned a non-absolute screenshot path"
                                    .to_string()
                            })
                        });
                    let _ = reply.send(match outcome {
                        Ok(uri) => PortalResult::Success(uri),
                        Err(error) => PortalResult::Failed(error),
                    });
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

/// Stand the portal backend up inside the compositor.
///
/// ## Why here and not in `--portal-server-only`
///
/// The portal answers questions about *this session* — workspace, prior
/// grants, the decision now — all live in the `WorkspaceTree` this process
/// owns. The standalone daemon has none: its `WorkspaceTree::default()`
/// misses every grant lookup, and its `PromptResolver::Channel` drains to
/// nowhere since it has no output to render a dialog on.
///
/// Giving that daemon IPC back to the compositor was rejected: it would
/// split "the permissions" across two processes — the drift
/// `crates/packaging/launcher/src/capabilities.rs` warns against — and it
/// still couldn't draw the dialog, making it a proxy for one process,
/// which is that process. `shell-portal.service` and its D-Bus activation
/// are removed with this change.
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
            crate::portal::serve_with_inhibit_and_clipboard(
                conn,
                ctx,
                inhibit_registry,
                crate::portal::clipboard::ClipboardBrokerHandle::new(Arc::new(
                    crate::portal::clipboard::CompositorClipboardBroker::new(selection),
                )),
            )
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
                     dialog will never be reached. The standalone \
                     `shell --portal-server-only` daemon is the usual culprit; it is \
                     no longer part of the image and its unit should be disabled.",
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
        server,
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
            b.set_base_color(ui::theme::Theme::configured().bg_app);
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
    // Calloop drives the whole compositor; smithay is callback-oriented and
    // expects every wakeup to fan out from a single `EventLoop::<State>`.
    let mut event_loop: EventLoop<'static, State> = match EventLoop::try_new() {
        Ok(el) => el,
        Err(e) => {
            eprintln!("tilo compositor: create calloop event loop failed: {e}");
            std::process::exit(1);
        }
    };

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

    spawn_auto_brightness_worker();

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
            Ok(_) => crate::compositor::device_rotation::spawn(move || ping.ping()),
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
    let peer_sync_requested = crate::compositor::peer_sync_enabled_from_env_or_marker();
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
    let test_control_session = test_control_path(
        crate::compositor::dev_env::DEV_ENV_OVERRIDES,
        std::env::var_os(TEST_CONTROL_ENV),
    )
    .is_some()
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
    // `main` probes the existing schema-2 record before entering the
    // compositor. If Secret Service is locked, that probe must not terminate
    // the login: trusted recovery chrome owns the next step. This marker is
    // process-local and cannot make an identity valid or bypass verification.
    let identity_recovery =
        std::env::var_os(crate::shell::identity_provisioning::IDENTITY_RECOVERY_ENV)
            .is_some_and(|value| value == "1");
    let identity_missing =
        crate::shell::identity_provisioning::needs_first_run_flow(&identity_store);
    let onboarding_identity_dir = if should_activate_first_use(
        identity_missing,
        progress_pending,
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
    let enable_peer_sync = peer_sync_requested && onboarding_identity_dir.is_none();
    let mut deferred_peer_sync = peer_sync_requested && onboarding_identity_dir.is_some();
    if deferred_peer_sync {
        tracing::info!("peer-sync: waiting for first-login identity setup to complete");
    }

    // The desktop's own state, loaded from disk not bootstrapped fresh — room
    // identity, owner and capability list all have to survive a restart.
    let (tree_handle, enable_peer_sync, state_notice) = load_session_tree(enable_peer_sync);
    let mut state = State::new_with_workspace_tree(
        event_loop.handle(),
        display_handle.clone(),
        tree_handle.clone(),
    );
    // Point `ui`'s clipboard at the compositor's selection mirror — everything
    // hosted here isn't a Wayland client, so without this they'd share no buffer.
    crate::compositor::selection::install_host_services(state.selection_store());
    let mut frame_builder = FrameBuilder::with_handle_and_records(
        tree_handle.clone(),
        enable_peer_sync,
        initial_size,
        crate::shell::session_record_store(),
    );
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
    let (system_audio, microphone) = crate::compositor::capture::recording_audio_availability();
    frame_builder.set_capture_audio_available(system_audio, microphone);
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

    // Peer sync is constructed here (not only in hosted mode) because this is
    // the compositor that boots as the Tilo session.
    let mut peer_sync = enable_peer_sync
        .then(|| spawn_session_peer_sync(&mut frame_builder, &mut state, tree_handle.clone()))
        .flatten();
    let mut device_fabric_runtime = peer_sync.as_ref().map(|network| {
        crate::shell::device_fabric_runtime::DeviceFabricRuntime::new(network, notif_bridge.clone())
    });
    let mut spatial_control: Option<SpatialControlSession> = None;
    // The session's CRDT fan-out broker, bound before the wayland socket is
    // published (before any app launches) — an app resolves its collab
    // descriptor once at spawn, so a broker arriving later is one it never gets.
    //
    // Lives in this process, not a unit of its own: a listener + fanout
    // table over `ShellNetwork` (the Iroh handle above), since the shell
    // already owns the network stack, identity and permissions. Held to the end of `run()` so the socket outlives every app and unlinks with the session.
    // Onboarding withholds every app, so defer its broker too. That lets the
    // freshly authenticated transport be its outbound route as soon as the
    // person completes setup, with no stale local-only broker to replace.
    let mut collab_broker = (!deferred_peer_sync)
        .then(|| spawn_session_collab_broker(&frame_builder.chrome_handle(), peer_sync.as_ref()))
        .flatten();
    if let Some(broker) = collab_broker.as_ref() {
        attach_collab_broker_bridges(&mut frame_builder, broker);
        #[cfg(feature = "agent-control")]
        spawn_agent_mcp(broker, &tree_handle, backend_kind == BackendKind::Headless);
        if let (Some(portal), Some(service)) =
            (portal_backend.as_ref(), broker.native_media_service())
        {
            service.install_consent(portal.media_grants.clone(), portal.media_prompts.clone());
        }
    }
    let mut local_session_lifecycle = collab_broker
        .as_ref()
        .map(crate::shell::collab_broker::BrokerHandle::local_session_lifecycle);
    let mut realtime_fabric_bridge = collab_broker
        .as_ref()
        .and_then(crate::shell::collab_broker::BrokerHandle::realtime_fabric_bridge);
    if let (Some(runtime), Some(bridge)) = (
        device_fabric_runtime.as_mut(),
        realtime_fabric_bridge.as_ref(),
    ) && let Some(adapter) = bridge.adapter()
    {
        runtime.set_realtime_adapter(adapter);
    }
    let mut remote_capture_runtime = realtime_fabric_bridge
        .as_ref()
        .and_then(
            crate::shell::collab_broker_capnp::RealtimeFabricAppBridge::realtime_capture_broker,
        )
        .map(|capture| {
            std::sync::Arc::new(std::sync::Mutex::new(
                crate::shell::remote_capture::runtime::RemoteCaptureFabricRuntime::production(
                    capture,
                ),
            ))
        });
    if let Some(remote_capture) = remote_capture_runtime.as_ref() {
        crate::shell::remote_capture::runtime::install_active_runtime(remote_capture);
        if let Some(runtime) = device_fabric_runtime.as_mut() {
            runtime.set_remote_capture_runtime(std::sync::Arc::clone(remote_capture));
        }
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

    // Read the session's own broker for pointer presence and relay it to the
    // room's other hosts; subscribing by room is what makes a cursor crossing
    // apps read as one peer moving, since presence is room-scoped. Only stood
    // up when both halves exist — without peer sync, the overlay paints nothing.
    if let (Some(broker), Some(net)) = (collab_broker.as_ref(), peer_sync.as_ref()) {
        attach_peer_presence(&mut frame_builder, broker, net);
    }

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
    let mut desktop_continuation = if crate::compositor::parse_install_mode_from_args()
        == crate::compositor::InstallMode::HostDesktop
    {
        // Readiness has no identity/key authority. The live trusted onboarding
        // state below gates both its response and the ordinary host-app launch.
        let runtime = crate::shell::launcher::runtime_dir();
        // SAFETY: getuid has no arguments or memory contract.
        let uid = unsafe { libc::getuid() };
        let pending = xdg_paths::data_local_dir().is_some_and(|data| {
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

    if let Err(err) =
        event_loop
            .handle()
            .insert_source(listening_socket, move |client_stream, (), state| {
                // Kernel credentials bind a sandboxed connection to the exact
                // compositor-supervised package/workspace launch. A direct host
                // client has no record and remains explicitly unconfined.
                let Some(client_state) = public_client_state(&client_stream) else {
                    tracing::warn!("rejected unattributed Wayland client outside host namespaces");
                    return;
                };
                if let Err(err) = state
                    .display_handle
                    .insert_client(client_stream, client_state)
                {
                    eprintln!("tilo compositor: failed to insert client: {err}");
                }
            })
    {
        eprintln!("tilo compositor: insert listening socket source into event loop failed: {err}");
        std::process::exit(1);
    }

    // Legacy X11 software runs through one rootless Xwayland per workspace.
    // A display is only a listening socket until its first client connects;
    // xwayland-satellite, itself a client of the public socket registered
    // above, starts on that connection. The wake ping lets a launch made
    // outside this loop's tick have its new display watched immediately.
    let mut xwayland = match calloop::ping::make_ping() {
        Ok((wake, wake_source)) => {
            if let Err(error) = event_loop
                .handle()
                .insert_source(wake_source, |(), (), _| {})
            {
                tracing::warn!(%error, "X11 display wake source unavailable");
            }
            Some(super::xwayland::XwaylandSupervisor::start(
                event_loop.handle(),
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
    input_methods.reload(&mut state);

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
    let mut test_control_conns: Vec<TestControlConn> = Vec::new();
    // A control client is itself an event source. Each accepted stream gets
    // its own wake (see `TestControlConn::watch`), because a connection is
    // accepted as soon as it exists, which is often before the request bytes
    // that follow it have arrived.
    let _test_control_listener_wake = test_control_listener
        .as_ref()
        .and_then(|listener| control_listener_wake(listener, &event_loop.handle()));
    let _agent_control_listener_wake = agent_control_listener
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
    if let Some(net) = peer_sync.as_ref() {
        net.set_wake(Arc::clone(&session_wake));
    }
    let chooser_attended = Arc::new(AtomicBool::new(false));
    let chooser_service = start_chooser_service(
        portal_backend.as_ref(),
        Arc::clone(&chooser_attended),
        Arc::clone(&session_wake),
    );
    // An `app.<id>/close` action, from the broker's thread.
    let app_closes = crate::shell::app_close::install(Arc::clone(&session_wake));
    let mut network_start: Option<
        crate::session_runtime::PendingStart<crate::shell::network::ShellNetwork>,
    > = None;
    // A media player's answer to a transport key, as the OSD to raise.
    let (transport_osd, transport_osds) =
        crate::session_runtime::mailbox::<crate::shell::osd::Osd>(Arc::clone(&session_wake));
    let mut super_tap = crate::shell::input_policy::SuperTap::default();

    // Idle / DPMS-blank / session-lock state machine (pure logic, see
    // `compositor::idle`); transitions become `backend.set_dpms` calls and
    // lock-overlay paints. The power policy from `settings.json` owns the
    // blank and suspend timeouts at startup and on live reload; environment
    // values only seed the headless/default config before that policy applies.
    let headless = backend_kind == BackendKind::Headless;
    let mut power_policy =
        idle_policy_for_backend(tilo_power_mgmt::config::load_policy(), headless);
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
    // Suspend-inhibitor registry: apps hold a token via
    // `inhibit_registry.acquire(reason)`; power service mirrors it into a real logind lock, idle loop checks `is_inhibited()` before suspending.
    // Mailbox the async power service uses to reach this loop (lock, battery
    // toast, idle-reset on resume) and this loop uses to reach it (request suspend).
    let power_bridge = tilo_power_mgmt::PowerBridge::new();
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
        crate::session_runtime::Registration<crate::tray::watcher::WatcherHandle>,
    > = notif_runtime.map(|rt| {
        crate::session_runtime::Registration::start(
            rt.handle(),
            "tray",
            async move {
                crate::tray::watcher::serve(
                    zbus::connection::Builder::session()?,
                    std::sync::Arc::clone(crate::tray::Tray::global()),
                )
                .await
            },
            |_| {
                tracing::info!(
                    "tray: serving {} on session bus",
                    crate::tray::WATCHER_BUS_NAME
                );
            },
        )
    });

    // polkit authentication agent: administrator prompts from pkexec,
    // GNOME Disks, virt-manager and system Flatpak installs become a sheet
    // only the compositor draws. Registered on the system bus for this login
    // session; without polkit or logind the requests fail as before.
    let polkit_agent: Option<
        crate::session_runtime::Registration<crate::polkit_agent::service::AgentHandle>,
    > = notif_runtime.map(|rt| {
        crate::session_runtime::Registration::start(
            rt.handle(),
            "polkit agent",
            crate::polkit_agent::service::serve(
                crate::shell::portal_dialog::PortalDialogQueue::global(),
            ),
            |_| tracing::info!("polkit: authentication agent registered for this session"),
        )
    });

    // NetworkManager secret agent: a Wi-Fi password that changed, 802.1X
    // sign-in, VPN secrets and SIM PINs become a sheet only the compositor
    // draws, instead of a connection that silently fails.
    let network_secret_agent: Option<
        crate::session_runtime::Registration<crate::network_secrets::service::AgentHandle>,
    > = notif_runtime.map(|rt| {
        crate::session_runtime::Registration::start(
            rt.handle(),
            "network secret agent",
            crate::network_secrets::service::serve(
                crate::shell::portal_dialog::PortalDialogQueue::global(),
                crate::network_secrets::store::RoamingWifiSlot::global(),
            ),
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
    let mut projection_capture = super::projection_capture::ProjectionCapture::default();

    // Move presentation onto its own thread where the backend supports it.
    // Headless declines: its bytes are read back synchronously by `snapshot`.
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

    // Pointer/keyboard preferences are read once at startup; this notices a
    // new Settings write so changes take effect this session, not the next.
    let mut settings_watch = crate::compositor::backend::drm::input_config::SettingsWatch::new();
    // The poll runs only on a loop tick, and an idle desktop parks with no
    // deadline, so a write from Settings or a display transaction staged by
    // another process wakes the loop and is looked at on that tick.
    let settings_written = Arc::new(AtomicBool::new(false));
    let _settings_file_watch = {
        let written = Arc::clone(&settings_written);
        let wake = Arc::clone(&session_wake);
        settings_store::watch::watch_settings(move || {
            written.store(true, Ordering::Release);
            wake();
        })
        .map_err(|error| {
            tracing::warn!(%error, "settings file watch unavailable; an idle desktop applies Settings changes on its next wake");
        })
        .ok()
    };
    let mut switch_control = SwitchControlRuntime::new(Instant::now());
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
    let lock_animation_epoch = Instant::now();
    let mut last_lock_animation_frame = lock_animation_epoch;
    let mut lock_has_wallpaper = false;
    let mut last_lock_clock_refresh = Instant::now();
    let mut lock_player: Option<crate::shell::now_playing::PlayerSnapshot> = None;
    let mut lock_pointer: Option<vello::kurbo::Point> = None;
    let mut last_lock_media_poll = Instant::now();
    let mut last_revision = frame_builder.chrome_revision();
    let mut force_first_paint = true;
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
    let legacy_share_config = crate::config::load_config().flatpak_share;
    let mut hosted_legacy_checked = Instant::now();
    // Optional paint-rate diagnostics (`TILO_PAINT_STATS=1`).
    let paint_stats = std::env::var("TILO_PAINT_STATS").is_ok();
    // Slow iterations are exceptional and always worth preserving in the
    // session flight record: a compositor that stops pumping input for 100 ms
    // is already visibly broken. `TILO_LOOP_TIMING` additionally enables the
    // verbose aggregate line for development runs.
    let loop_timing = std::env::var("TILO_LOOP_TIMING").is_ok();
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
                if elapsed >= Duration::from_millis(8) {
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
                if elapsed >= Duration::from_millis(8) {
                    eprintln!(
                        "PUMP-PHASE name={} elapsed_us={} items={}",
                        $name,
                        elapsed.as_micros(),
                        $count
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
    while !close_requested {
        let iter_t0 = Instant::now();
        let injected_redraw = std::mem::take(&mut injected_redraw_pending);
        metrics.begin_tick(iter_t0);
        if let Some((serial, retired_at)) = present_pipe
            .as_ref()
            .and_then(present::PresentPipe::latest_retirement)
        {
            metrics.note_retirement(serial, retired_at);
            if serial > frame_clock_retired_serial {
                frame_clock.note_presented(retired_at);
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
                backend.outputs(),
                initial_size,
            )
        {
            force_first_paint = true;
        }
        if let Some(service) = floating_surface_service.as_ref()
            && pump_floating_toplevel_events(service, &mut state)
        {
            force_first_paint = true;
        }
        if let Some(service) = chooser_service.as_ref() {
            pump_chooser_opens(service, &frame_builder.chrome_handle());
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
        }
        // A pen goes to a tablet-aware client directly and stands in for the
        // pointer everywhere else, so it expands into ordinary input here,
        // in order, before the batch is dispatched.
        // A pad follows keyboard focus the same way, with its settings'
        // mapping standing in wherever no tablet-aware app is focused.
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
                        pending_events.extend(client_input::route_tablet_event(
                            &mut state,
                            &frame_builder,
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
        let had_backend_events = !pending_events.is_empty();
        let mut session_changed = false;
        let mut accessibility_changed = false;
        let mut visible_input_changed = false;
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
                        &mut test_control_ctx.doc,
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
        while let Some(event) = pending_events.pop_front() {
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
                BackendEvent::Input(wev) => {
                    // A physical switch still enters through the same trusted
                    // libinput/winit funnel. Only the configured pair is
                    // translated, and disabled mode returns `None`, preserving
                    // the original key exactly.
                    let translated_switch = switch_control
                        .translate(&wev, Instant::now())
                        .map(|(action, pressed)| switch_action_event(action, pressed));
                    let wev = translated_switch.as_ref().unwrap_or(&wev);
                    // Pointer motion changes the compositor-owned cursor even
                    // when the focused Wayland client consumes the event.  It
                    // therefore needs a frame independently of whether chrome
                    // state changed.  Buttons/keys do not alter shell pixels
                    // unless their downstream handler reports a change.
                    visible_input_changed |= matches!(wev, WindowEvent::CursorMoved { .. });
                    // Instrumentation: the clock starts when the loop takes the event off the
                    // backend; the first painted frame after closes the input-to-present measurement.
                    metrics.note_input(Instant::now());
                    // Any input is activity — reset the idle timer and wake the display if it was blanked.
                    for t in session.note_activity(Instant::now()) {
                        apply_session_transition(
                            t,
                            backend.as_mut(),
                            present_pipe.as_ref(),
                            local_session_lifecycle.as_ref(),
                            &mut screen_dimmer,
                        );
                    }
                    // Physical priority: the host's own keyboard/mouse blocks every remote
                    // peer for the next half second, stamped at the funnel so lock/VT/power-key branches count as takeover too.
                    state.remote_input_firewall.note_local_input(Instant::now());
                    // A lone Super tap opens global search. Observed ahead of every
                    // branch below, so a chord one of them handles still disarms it.
                    if super_tap.observe(wev) && !session.is_locked() {
                        frame_builder
                            .host_mut()
                            .open_global_search(&test_control_ctx.doc, &test_control_ctx.root_obj);
                        session_changed = true;
                    }

                    let locked = session.is_locked();
                    let route = priority_route(
                        wev,
                        test_control_ctx.modifiers,
                        locked,
                        frame_builder.host().shortcut_config(),
                    );
                    if locked {
                        if let Some(network) = peer_sync.as_ref() {
                            end_spatial_control(network, &mut spatial_control);
                        } else {
                            spatial_control = None;
                        }
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
                        match wev {
                            WindowEvent::CursorMoved { position, .. } => {
                                lock_pointer =
                                    Some(vello::kurbo::Point::new(position.x, position.y));
                            }
                            WindowEvent::MouseInput {
                                state: ElementState::Pressed,
                                button: MouseButton::Left,
                                ..
                            } => {
                                let outputs =
                                    lock_outputs(backend.outputs(), frame_builder.viewport());
                                let view = LockView {
                                    now_playing: lock_player.as_ref(),
                                    ..LockView::default()
                                };
                                if let Some(transport) = lock_pointer.and_then(|point| {
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
                        let key = winit_event_to_lock_key(wev);
                        lock.handle_key(key);
                        // Any keystroke changes the masked field / error, so force a repaint this tick.
                        session_changed = true;
                    } else if route == PriorityRoute::PowerKey {
                        // Hardware power key resolves per the power policy (logind delegates via
                        // the service's block inhibitor); a deliberate press bypasses the idle inhibitor.
                        match power_policy.on_power_key() {
                            Some(tilo_power_mgmt::PowerAction::Suspend) => {
                                tracing::info!(target: "tilo::power", "power key: requesting suspend");
                                sleep_lock_was_locked = session.is_locked();
                                session_changed = true;
                                power_bridge.request_suspend();
                            }
                            Some(tilo_power_mgmt::PowerAction::Lock) => {
                                for t in session.request_lock(Instant::now()) {
                                    apply_session_transition(
                                        t,
                                        backend.as_mut(),
                                        present_pipe.as_ref(),
                                        local_session_lifecycle.as_ref(),
                                        &mut screen_dimmer,
                                    );
                                }
                            }
                            Some(tilo_power_mgmt::PowerAction::Ignore) | None => {}
                        }
                    } else if let PriorityRoute::VtSwitch(vt) = route {
                        // Ahead of every other chord, never offered to a client: leaving the
                        // terminal must work even with a grabbed keyboard, or a client can strand the user.
                        backend.switch_vt(vt);
                    } else if route == PriorityRoute::RemoteInputKillSwitch {
                        // Super+Esc: every peer's input stops and stays stopped. Ahead of the
                        // client router and shell's Escape handling — a panic key a client can swallow isn't one.
                        client_input::stop_remote_input(&mut state, &mut frame_builder);
                        if let Some(network) = peer_sync.as_ref() {
                            end_spatial_control(network, &mut spatial_control);
                        } else {
                            spatial_control = None;
                        }
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
                        for t in session.request_lock(Instant::now()) {
                            apply_session_transition(
                                t,
                                backend.as_mut(),
                                present_pipe.as_ref(),
                                local_session_lifecycle.as_ref(),
                                &mut screen_dimmer,
                            );
                        }
                    } else if route == PriorityRoute::ToggleMouseKeys {
                        // The compositor applies the flipped preference on its
                        // next settings poll; the readout confirms it now.
                        if let Some(osd) = settings_store::settings_path()
                            .as_deref()
                            .and_then(crate::shell::media_keys::toggle_mouse_keys)
                        {
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
                        let settings = settings_store::settings_path();
                        let displays = if matches!(
                            mk,
                            crate::shell::media_keys::MediaKey::BrightnessUp
                                | crate::shell::media_keys::MediaKey::BrightnessDown
                        ) {
                            os_actions::BrightnessService::global().levels()
                        } else {
                            Vec::new()
                        };
                        let pointer = frame_builder.host().pointer_position();
                        let under_pointer = pointer
                            .and_then(|p| output_connector_at(backend.outputs(), (p.x, p.y)));
                        let response = crate::shell::media_keys::apply(
                            mk,
                            &crate::shell::media_keys::Context {
                                actions: frame_builder.host().qs_actions(),
                                status: frame_builder.host().status_bridge(),
                                transport_osd: &transport_osd,
                                airplane_mode,
                                settings: settings.as_deref(),
                                brightness: crate::shell::media_keys::BrightnessKeys {
                                    displays: &displays,
                                    under_pointer,
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
                                session_changed = true;
                                power_bridge.request_suspend();
                            }
                            Some(crate::shell::media_keys::Response::OpenDisplayModes) => {
                                frame_builder.with_chrome(|st| {
                                    st.overlays.projection = true;
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
                    } else if peer_sync.as_ref().is_some_and(|network| {
                        forward_spatial_window_event(wev, network, &mut spatial_control)
                    }) {
                        // The physical keyboard/pointer is logically on the
                        // adjacent certified device until it crosses back or
                        // Escape is pressed. Reserved shell chords above stay
                        // local and remain an unconditional escape path.
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
                        if !to_client {
                            session_changed |= client_input::finish_chrome_drag_into_shell(
                                &mut state,
                                &mut frame_builder,
                                wev,
                            );
                            // Dispatch real input through the same wire the headless `execute_planned_action` path uses.
                            session_changed |= frame_builder.dispatch_event(
                                wev,
                                &test_control_ctx.modifiers,
                                &mut test_control_ctx.doc,
                                &test_control_ctx.root_obj,
                                &mut test_control_ctx.focus,
                            );
                            // Chrome input can change the active tab/group/workspace. The
                            // backend may have already queued the next physical key in this
                            // same batch, so move the Wayland seat now rather than only after
                            // the batch: click-tab then type must never send the first key to
                            // the tab that was active before the click.
                            frame_builder.host_mut().flush_active_tab_writeback();
                            client_input::sync_keyboard_focus(&mut state, &frame_builder);
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
                        if let Some(network) = peer_sync.as_ref() {
                            end_spatial_control(network, &mut spatial_control);
                        } else {
                            spatial_control = None;
                        }
                    } else if state.pointer_lock.is_engaged() {
                        // A game holding the pointer reads every delta; the
                        // pointer must not wander onto another device.
                        state.forward_client_relative_motion(
                            (dx, dy),
                            (dx_unaccel, dy_unaccel),
                            time_usec,
                        );
                    } else if peer_sync.as_ref().is_some_and(|network| {
                        move_spatial_pointer(&state, network, &mut spatial_control, dx, dy)
                    }) {
                        // The pointer is on an adjacent device.
                    } else {
                        state.forward_client_relative_motion(
                            (dx, dy),
                            (dx_unaccel, dy_unaccel),
                            time_usec,
                        );
                    }
                }
                BackendEvent::ModifiersChanged(modifiers) => {
                    // Carry the modifier mask forward so the next event dispatched (or the
                    // lock-chord check above) sees the right Ctrl/Shift/Alt/Super state.
                    test_control_ctx.modifiers = modifiers;
                }
                BackendEvent::InputStateReset => {
                    // A device/VT/keymap boundary can omit releases. Retire
                    // Smithay's actual pressed-key state, never just the
                    // chrome's modifier snapshot, before accepting another
                    // physical event.
                    state.reset_client_keyboard_state();
                    if let Some(network) = peer_sync.as_ref() {
                        end_spatial_control(network, &mut spatial_control);
                    } else {
                        spatial_control = None;
                    }
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
                BackendEvent::ShellGesture(action) => {
                    // A 3/4-finger gesture, already resolved into its navigation meaning —
                    // never also delivered to a client, so no double-handling to guard against.
                    if !session.is_locked() && client_input_allowed(&frame_builder) {
                        for t in session.note_activity(Instant::now()) {
                            apply_session_transition(
                                t,
                                backend.as_mut(),
                                present_pipe.as_ref(),
                                local_session_lifecycle.as_ref(),
                                &mut screen_dimmer,
                            );
                        }
                        apply_shell_gesture(&mut frame_builder, action);
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
                    if matches!(
                        touch,
                        crate::compositor::backend::TouchEvent::Cancel
                            | crate::compositor::backend::TouchEvent::Frame
                    ) || (!session.is_locked() && client_input_allowed(&frame_builder))
                    {
                        let target = client_input::touch_target(&state, &frame_builder, &touch);
                        if let Some((surface_id, _)) = target
                            && matches!(touch, crate::compositor::backend::TouchEvent::Down { .. })
                        {
                            client_input::focus_touch_target(
                                &mut state,
                                &mut frame_builder,
                                surface_id,
                            );
                        }
                        state.forward_client_touch(touch, target);
                    } else if matches!(touch, crate::compositor::backend::TouchEvent::Down { .. }) {
                        state.note_touch_down_withheld_from_clients();
                    }
                }
                BackendEvent::TouchTap { x, y } => {
                    // A tap whose touch sequence reached a client is that
                    // client's alone. Otherwise it becomes one ordinary click,
                    // processed right now through the physical pointer path.
                    if state.touch_tap_needs_pointer_click() {
                        for event in touch_tap_pointer_events(x, y).into_iter().rev() {
                            pending_events.push_front(BackendEvent::Input(event));
                        }
                    }
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
                BackendEvent::InputUsed(modality) => {
                    if test_control_ctx.input.note_used(modality) {
                        test_control_ctx.publish_input_environment(&mut frame_builder);
                        session_changed = true;
                    }
                }
                BackendEvent::Focus(focused) => {
                    if !focused {
                        if let Some(network) = peer_sync.as_ref() {
                            end_spatial_control(network, &mut spatial_control);
                        } else {
                            spatial_control = None;
                        }
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
                | BackendEvent::TabletPadRemoved(_) => {
                    // Redraw is implicit: the native loop paints every tick.
                    // Pen and pad events were expanded into client or
                    // ordinary input before the batch.
                }
            }
        }

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
                        &mut test_control_ctx.doc,
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
            for t in session.note_activity(Instant::now()) {
                apply_session_transition(
                    t,
                    backend.as_mut(),
                    present_pipe.as_ref(),
                    local_session_lifecycle.as_ref(),
                    &mut screen_dimmer,
                );
            }
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

        state.expire_host_launch_activations();
        crate::shell::launcher::expire_pending_placements();
        session_changed |=
            crate::compositor::selection::pump_pending_shell_drop(&mut state, &mut frame_builder);
        session_changed |= launch_queued_host_apps(&mut state, &mut frame_builder);
        session_changed |= post_codec_pack_notices(&mut frame_builder);
        run_autostart_once(&frame_builder);

        // Keyboard releases are latency-sensitive protocol traffic: an Xwayland
        // client starts auto-repeat after a press and only stops once it sees the
        // matching release.  Scene construction can occasionally block on a GPU
        // fence for hundreds of milliseconds, so waiting until the end-of-frame
        // flush below turns an ordinary tap into a burst (or apparently stuck
        // repeat) under render pressure.  Flush events queued by this input batch
        // before doing any layout/render work.  The later flush remains necessary
        // for protocol events produced while dispatching client requests.
        if had_backend_events && let Err(err) = display.flush_clients() {
            eprintln!("tilo compositor: input flush_clients error: {err}");
            break;
        }
        // Click-to-focus persistence serialises the full workspace tree and
        // performs two durability barriers (file + containing directory).
        // Keep that I/O out of `route_mouse_input`: queue and flush
        // wl_keyboard.enter/wl_pointer.button first so a slow filesystem can
        // never delay the focused application seeing the click or the next
        // already-buffered key. Durability still completes in this iteration,
        // before any paint or event-loop park.
        if state.client_input.take_focus_persistence_pending() {
            let started = Instant::now();
            state.workspace_tree.read().flush_to_disk();
            let elapsed = started.elapsed();
            if elapsed > Duration::from_millis(10) {
                tracing::warn!(
                    target: "tilo::latency",
                    elapsed_ms = elapsed.as_secs_f64() * 1_000.0,
                    "click-focus persistence was slow after input delivery"
                );
            }
        }

        // The Settings app writes to a file with no notification, so poll cheaply
        // (once/sec) to reach input devices and display scale.
        if settings_written.swap(false, Ordering::AcqRel) {
            settings_watch.check_now();
        }
        if settings_watch.changed(Instant::now()) {
            tracing::info!("compositor: settings changed on disk; re-applying");
            state.reload_keyboard_settings();
            backend.reload_input_settings();
            // The backend has now reconciled any staged display plan; tell
            // the output-management clients that asked for it how it went.
            state.output_management.resolve_transactions(
                settings_store::hardware::DisplayTransaction::load().as_ref(),
            );
            if let Some(xwayland) = xwayland.as_mut() {
                xwayland.reload_keyboard_settings();
            }
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
            frame_builder.host_mut().set_desktop_controls_config(
                settings_store::hardware::DesktopControlsConfig::load(),
            );
            frame_builder.host_mut().poll_display_identify();
            let policy = idle_policy_for_backend(tilo_power_mgmt::config::load_policy(), headless);
            power_policy = policy;
            session.set_blank_timeout(policy.idle_blank);
            session.set_lock_timeout(policy.idle_lock);
            session.set_suspend_timeout(policy.idle_suspend);
            power_bridge.update_policy(policy);
            outputs_dirty = true;
            input_methods.reload(&mut state);
        }
        input_methods.follow_input(&mut state, &test_control_ctx.input);
        input_methods.tick();
        for (output, on) in state.output_management.take_power_requests() {
            backend.set_output_power(output, on);
        }
        if let Some(rotation) = device_rotation
            .as_ref()
            .and_then(|changes| changes.try_iter().last())
        {
            backend.set_device_rotation(Some(rotation));
        }
        if let Some(confirmation) = state.output_management.take_confirmation() {
            frame_builder
                .host_mut()
                .show_display_confirmation(confirmation.transaction, confirmation.tool);
        }
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
        let t_input_done = iter_t0.elapsed();
        metrics.mark_input(t_input_done);
        let input_dispatch = t_input_done.saturating_sub(t_backend_poll_done);
        if slow_prepaint_phase(input_dispatch) {
            tracing::warn!(
                target: "tilo::latency",
                elapsed_ms = input_dispatch.as_secs_f64() * 1_000.0,
                backend_poll_ms = t_backend_poll_done.as_secs_f64() * 1_000.0,
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
            for t in session.request_lock(Instant::now()) {
                apply_session_transition(
                    t,
                    backend.as_mut(),
                    present_pipe.as_ref(),
                    local_session_lifecycle.as_ref(),
                    &mut screen_dimmer,
                );
            }
            session_changed = true;
        }

        // Advance the idle timers and apply any timeout-driven transitions (blank, auto-lock, suspend).
        for t in session.tick_inhibited(Instant::now(), &inhibit_registry) {
            if t == SessionTransition::Suspend {
                // Idle-suspend is gated by the inhibitor registry (media playing, a task
                // running); when inhibited, un-latch so it re-fires the instant the last inhibitor releases, no fresh input needed.
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
                    power_bridge.request_suspend();
                }
            } else {
                apply_session_transition(
                    t,
                    backend.as_mut(),
                    present_pipe.as_ref(),
                    local_session_lifecycle.as_ref(),
                    &mut screen_dimmer,
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
                    for t in session.request_lock(Instant::now()) {
                        apply_session_transition(
                            t,
                            backend.as_mut(),
                            present_pipe.as_ref(),
                            local_session_lifecycle.as_ref(),
                            &mut screen_dimmer,
                        );
                    }
                }
                tilo_power_mgmt::PowerSignal::PreparingForSleep => {
                    // Preserve a lock that was already present at request
                    // time, and also observe a manual/lid lock that arrived
                    // while the async suspend request was in flight.
                    sleep_lock_was_locked |= session.is_locked();
                    for t in session.prepare_for_sleep(Instant::now()) {
                        apply_session_transition(
                            t,
                            backend.as_mut(),
                            present_pipe.as_ref(),
                            local_session_lifecycle.as_ref(),
                            &mut screen_dimmer,
                        );
                    }
                    sleep_lock_frame_pending = true;
                    sleep_lock_frame_serial = None;
                    sleep_lock_frame_failed = false;
                }
                tilo_power_mgmt::PowerSignal::SuspendFailed => {
                    let was_prepared = sleep_lock_frame_pending;
                    sleep_lock_frame_pending = false;
                    sleep_lock_frame_serial = None;
                    sleep_lock_frame_failed = false;
                    session.cancel_suspend();
                    // A lid/critical request can fail before it ever emits
                    // PreparingForSleep. Only undo a lock that this pending
                    // sleep actually installed; never unlock an unrelated
                    // user lock when CanSuspend is refused.
                    if should_unlock_after_suspend_failure(was_prepared, sleep_lock_was_locked) {
                        for t in session.request_unlock(Instant::now()) {
                            apply_session_transition(
                                t,
                                backend.as_mut(),
                                present_pipe.as_ref(),
                                local_session_lifecycle.as_ref(),
                                &mut screen_dimmer,
                            );
                        }
                    }
                    sleep_lock_was_locked = false;
                    tracing::warn!(target: "tilo::power", "suspend request failed; session remains awake");
                }
                tilo_power_mgmt::PowerSignal::ResumedFromSleep => {
                    // Reset the idle timer so a stale pre-sleep timestamp doesn't immediately re-blank/re-suspend on resume.
                    for t in session.resume_from_sleep(Instant::now()) {
                        apply_session_transition(
                            t,
                            backend.as_mut(),
                            present_pipe.as_ref(),
                            local_session_lifecycle.as_ref(),
                            &mut screen_dimmer,
                        );
                    }
                    // Put the display back — a suspend doesn't pause the seat, so the DRM device
                    // returns with no mode/framebuffer/cursor plane; without this, lid close/open is an unrecoverable black screen.
                    backend.reset_display();
                    sleep_lock_frame_pending = false;
                    sleep_lock_frame_serial = None;
                    sleep_lock_frame_failed = false;
                    sleep_lock_was_locked = false;
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
            for t in session.request_lock(Instant::now()) {
                apply_session_transition(
                    t,
                    backend.as_mut(),
                    present_pipe.as_ref(),
                    local_session_lifecycle.as_ref(),
                    &mut screen_dimmer,
                );
            }
        }
        // Async unlock: pick up an in-flight credential verify's result from the
        // worker thread (started by lock-screen Enter). The loop never blocks —
        // it keeps ticking and unlocks only on verified success; a wrong/failed
        // result surfaces a non-leaky error and stays locked. Polled every tick (~16ms).
        if let Some(outcome) = lock.poll_verification() {
            // The field/status changed (unlocked, or error shown) — repaint.
            session_changed = true;
            if outcome == UnlockOutcome::Unlocked {
                for t in session.request_unlock(Instant::now()) {
                    apply_session_transition(
                        t,
                        backend.as_mut(),
                        present_pipe.as_ref(),
                        local_session_lifecycle.as_ref(),
                        &mut screen_dimmer,
                    );
                }
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
            crate::portal::inhibit::publish_screensaver_state(session.is_locked());
            if session.is_locked() {
                let _ = state.release_all_personal_fabric_input();
                if let Some(network) = peer_sync.as_ref() {
                    end_spatial_control(network, &mut spatial_control);
                } else {
                    spatial_control = None;
                }
            }
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
        });
        if let Some(network) = peer_sync.as_ref()
            && continuity_feed_polled
                .is_none_or(|polled| polled.elapsed() >= Duration::from_millis(500))
        {
            continuity_feed_polled = Some(Instant::now());
            let feed = network.continuity_feed();
            frame_builder.with_chrome(|st| st.set_continuity_feed(feed));
        }

        // Client fullscreen is a per-output overlay, never a combined-scene
        // layout collapse.  Keep the chrome state's legacy group promotion
        // clear so another monitor's desktop remains visible and interactive.
        frame_builder.set_fullscreen(None);

        {
            // A close affordance mutates the rendered/persisted tree first, then
            // queues the protocol surface here. Remove it from the compositor's
            // authoritative foreign tree before reconciliation, otherwise the
            // still-live client can be adopted straight back while it handles
            // xdg_toplevel.close. Sending close asks the application to exit in
            // the normal desktop way; it is not a process kill.
            let mut close_requests = Vec::new();
            frame_builder.with_chrome(|chrome| {
                for request in app_closes.drain() {
                    let tabs = crate::shell::app_close::tabs_of(&chrome.tree().desktop, &request);
                    for tab in tabs {
                        let _ = crate::shell::window_actions::close_tab(chrome, tab);
                    }
                }
                close_requests = chrome.take_surface_close_requests();
            });
            for surface_id in close_requests {
                state.close_surface_on_request(surface_id);
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
                state.activate_keyboard_source(index);
                backend.set_keyboard_source(index);
                // The saved choice may be an input-method engine (or leave
                // one): hand typing to its owner now, not on the next
                // settings poll.
                input_methods.reload(&mut state);
            }

            // Protocol callbacks and the frame builder share one tree handle, so
            // adoption/destruction is immediately visible to chrome and persistence.
            // The input router's queues remain bookkeeping and are drained each
            // tick so they cannot grow across a long session.
            let had_pending = state.client_input.clear_pending();
            let placements_changed = {
                let tree = state.workspace_tree.read();
                state.client_input.refresh_placements_from_tree(&tree)
            };
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
        // A picked emoji goes to the field that just got the keyboard back,
        // through the seat's input method; what arrived feeds the recents.
        let _ = crate::compositor::text_commit::relay(
            &mut state,
            frame_builder.host_mut(),
            Instant::now(),
        );
        // An app's idle inhibitor holds the screen on only while its window is visible.
        crate::compositor::idle_inhibit::sync_idle_inhibitors(
            &mut state,
            &frame_builder,
            &inhibit_registry,
        );
        let awake_holders =
            inhibit_registry.active_reasons_for(tilo_power_mgmt::inhibit::INHIBIT_IDLE);
        frame_builder.with_chrome(|chrome| chrome.set_awake_holders(awake_holders));
        frame_builder.set_osk_surface(
            state.osk_surface,
            state.client_input.keyboard_focus(),
            state.text_input_active(),
        );
        pump_checkpoint!("session-focus-and-surfaces");
        let floating_surfaces = state
            .floating_toplevels
            .iter()
            .filter(|surface| {
                !surface.suspended && surface.presented && surface.transfer_source.is_none()
            })
            .map(|surface| (surface.surface_id, surface.app_id.clone(), surface.rect))
            .collect::<Vec<_>>();
        frame_builder.set_floating_surfaces(&floating_surfaces);
        state.sync_capture_protection();
        state.set_capture_placeholder_fill(
            crate::shell::capture_protection::placeholder_fill_rgba8(frame_builder.theme()),
        );
        let mut transient_surfaces = state.popup_surfaces();
        transient_surfaces.extend(state.input_method_popup_surfaces());
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
        publish_output_colors(&state, backend.as_ref());
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
        if let Some(image) =
            client_input::sync_pointer_image(&state, &mut frame_builder, session.is_locked())
        {
            backend.set_pointer_image(image);
            // A software cursor is drawn into the frame, so it needs one.
            injected_redraw_pending = true;
        }
        if let Some(change) =
            client_input::sync_pointer_constraint(&mut state, &frame_builder, session.is_locked())
        {
            if change.engaged {
                frame_builder.with_chrome(|chrome| {
                    chrome.show_osd(crate::shell::osd::Osd::pointer_held(true));
                });
                injected_redraw_pending = true;
            }
            apply_pointer_constraint_change(backend.as_mut(), change);
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
        {
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
        let pump_bookkeeping_done = Instant::now();
        let pump_network_started = pump_bookkeeping_done;
        if let Some(net) = peer_sync.as_ref() {
            pump_phase!("network-tick", net.tick());
            let runtime = device_fabric_runtime.get_or_insert_with(|| {
                crate::shell::device_fabric_runtime::DeviceFabricRuntime::new(
                    net,
                    notif_bridge.clone(),
                )
            });
            if let Some(adapter) = realtime_fabric_bridge
                .as_ref()
                .and_then(crate::shell::collab_broker_capnp::RealtimeFabricAppBridge::adapter)
            {
                runtime.set_realtime_adapter(adapter);
            }
            if let Some(remote_capture) = remote_capture_runtime.as_ref() {
                runtime.set_remote_capture_runtime(std::sync::Arc::clone(remote_capture));
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
            runtime.set_local_app_focus(app_focus.clone());
            pump_phase!(
                "device-fabric-runtime",
                runtime.pump(
                    net,
                    &state.selection_store(),
                    crate::shell::device_fabric_runtime::LocalFabricCapabilities {
                        display,
                        pointer: true,
                        keyboard: true,
                        audio_output: system_audio,
                    },
                )
            );
            pump_phase!(
                "physical-gamepads",
                crate::shell::physical_gamepad::pump_physical_gamepads(net, app_focus.as_ref())
            );
            if let Some(bridge) = realtime_fabric_bridge.as_ref() {
                pump_phase!("realtime-app-events", bridge.pump_app_events());
            }
            let spatial_input = crate::shell::device_fabric_runtime::take_spatial_input();
            let spatial_input_count = spatial_input.len();
            pump_phase!(
                "spatial-input",
                spatial_input_count,
                for (controller, event) in spatial_input {
                    let _ = state.apply_personal_fabric_input(&controller, event);
                }
            );
            let spatial_releases =
                crate::shell::device_fabric_runtime::take_spatial_input_releases();
            let spatial_release_count = spatial_releases.len();
            pump_phase!(
                "spatial-input-releases",
                spatial_release_count,
                for controller in spatial_releases {
                    let _ = state.release_personal_fabric_input(&controller);
                }
            );
            let personal_display_events =
                crate::shell::device_fabric_runtime::take_personal_display_events();
            let personal_display_event_count = personal_display_events.len();
            pump_phase!(
                "personal-display-events",
                personal_display_event_count,
                for event in personal_display_events {
                    use crate::shell::device_fabric_runtime::PersonalDisplayEvent;
                    match event {
                        PersonalDisplayEvent::Started {
                            session_id,
                            source,
                            mirror_only,
                            ..
                        } => {
                            let input_net = Arc::clone(net);
                            let input_source = source.clone();
                            let spatial_sink =
                                Arc::new(move |event: crate::network::streaming::InputEvent| {
                                    use crate::network::streaming::InputEvent;
                                    let mapped = match event {
                                InputEvent::PointerMotion { x, y } => Some(
                                    crate::shell::device_fabric::FabricInputEvent::PointerMotion {
                                        x: spatial_coord(x),
                                        y: spatial_coord(y),
                                    },
                                ),
                                InputEvent::PointerButton { button, pressed } => Some(
                                    crate::shell::device_fabric::FabricInputEvent::PointerButton {
                                        button,
                                        pressed,
                                    },
                                ),
                                InputEvent::Key {
                                    keysym,
                                    pressed,
                                    modifiers,
                                } => Some(crate::shell::device_fabric::FabricInputEvent::Key {
                                    keycode: keysym,
                                    pressed,
                                    modifiers,
                                }),
                                InputEvent::PointerEnter { .. }
                                | InputEvent::PointerLeave { .. }
                                | InputEvent::KeyboardFocus { .. } => None,
                            };
                                    if let Some(mapped) = mapped {
                                        let _ =
                                            crate::shell::device_fabric_runtime::send_spatial_input(
                                                &input_net,
                                                &input_source,
                                                mapped,
                                            );
                                    }
                                });
                            let sink: Arc<
                                dyn Fn(crate::network::streaming::InputEvent) + Send + Sync,
                            > = if mirror_only {
                                Arc::new(|_| {})
                            } else {
                                spatial_sink
                            };
                            frame_builder
                                .host_mut()
                                .mount_personal_display(session_id, sink);
                        }
                        PersonalDisplayEvent::Frame {
                            session_id,
                            sequence,
                            annex_b,
                            ..
                        } => {
                            let outcome = frame_builder
                                .host_mut()
                                .push_personal_display_frame(session_id, &annex_b);
                            crate::shell::device_fabric_runtime::report_personal_display_decode(
                                net, session_id, sequence, outcome,
                            );
                        }
                        PersonalDisplayEvent::Ended { session_id } => {
                            let _ = frame_builder
                                .host_mut()
                                .unmount_personal_display(session_id);
                        }
                    }
                }
            );
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
            let desired = {
                let viewer_hosts = if backend_kind == BackendKind::Headless {
                    let active_room = {
                        let tree_handle = frame_builder.chrome_handle().borrow().tree_handle();
                        let tree = tree_handle.read();
                        tree.desktop
                            .active_workspace()
                            .map(|workspace| workspace.room)
                    };
                    active_room.map_or_else(std::collections::HashSet::new, |room| {
                        net.viewer_hosts(room)
                    })
                } else {
                    std::collections::HashSet::new()
                };
                let tree_handle = frame_builder.chrome_handle().borrow().tree_handle();
                let tree = tree_handle.read();
                desired_legacy_shares(&tree, backend.watched(), &viewer_hosts)
            };
            // This session's legacy windows, as its own tabs in the rooms
            // other devices govern, so their members have a tab to show.
            if hosted_legacy_checked.elapsed() >= std::time::Duration::from_millis(500) {
                hosted_legacy_checked = Instant::now();
                let (salt, rooms) = {
                    let tree_handle = frame_builder.chrome_handle().borrow().tree_handle();
                    let tree = tree_handle.read();
                    let rooms = tree
                        .desktop
                        .workspaces
                        .iter()
                        .map(|workspace| (workspace.room, workspace.strips.clone()))
                        .collect::<Vec<_>>();
                    (tree.local_salt(), rooms)
                };
                for (room, strips) in rooms {
                    net.publish_hosted_legacy_windows(room, &strips, salt);
                }
            }
            pump_phase!(
                "legacy-share-reconcile",
                state.reconcile_legacy_share_sessions(
                    net.local_peer_id_hash(),
                    &desired,
                    &legacy_share_config,
                )
            );
            let inbound_legacy = net.take_inbound_legacy();
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
                            ..
                        } => {
                            let reply_net = Arc::clone(net);
                            let local_peer = net.local_peer_id_hash();
                            let workspace_topic = inbound.workspace_topic;
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
                            let stream = crate::shell::peer_view::PeerStreamStart {
                                source_peer,
                                surface,
                                local_peer,
                                app_id,
                                codec,
                                display_size: Some((width, height)),
                                hdr_static_metadata,
                                input_sink,
                            };
                            let mounted = !stream.app_id.is_empty()
                                && frame_builder.host_mut().mount_peer_stream(&stream);
                            if !mounted {
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
                            ..
                        } => {
                            let _ = frame_builder.host_mut().push_peer_stream_frame(
                                source_peer,
                                surface,
                                &encoded_h264,
                                None,
                            );
                        }
                        crate::network::NeighbourMsg::SurfaceStreamEnd {
                            surface,
                            source_peer,
                            ..
                        } => {
                            pending_legacy_streams.remove(&(source_peer, surface));
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
                for stream in pending_legacy_streams.values() {
                    if !stream.app_id.is_empty() {
                        let _ = frame_builder.host_mut().mount_peer_stream(stream);
                    }
                }
            );
        }
        pump_checkpoint!("device-fabric-and-network");
        // Keep the presence relay current, then fold in what peers sent. Pointer
        // is read level-triggered (covers every source: seat, remote peer, test
        // socket); room set is re-read each tick since a workspace can join a
        // room mid-session and the subscription must follow.
        let pump_network_done = Instant::now();
        let pump_presence_started = pump_network_done;
        // The pointer as the input state knows it — "nowhere", not the origin, for
        // a session that never moved one; being level-triggered, `cursor()` would paint a cursor for nobody.
        frame_builder.note_local_pointer(state.client_input.pointer_position());
        let peers_moved = if let Some(relay) = frame_builder.peer_presence() {
            let (rooms, here) = {
                // Clone the tree handle and drop the `ChromeState` borrow before
                // locking the tree — nothing here needs the RefCell guard itself.
                let tree_handle = frame_builder.chrome_handle().borrow().tree_handle();
                let tree = tree_handle.read();
                let rooms = tree
                    .desktop
                    .workspaces
                    .iter()
                    .map(|w| w.room.to_string())
                    .collect::<Vec<_>>();
                // The room the user is looking at (where their pointer is), distinct from
                // the set above (every room the session listens to).
                let here = tree
                    .desktop
                    .workspaces
                    .iter()
                    .find(|w| w.id == tree.desktop.active)
                    .map(|w| w.room.to_string());
                (rooms, here)
            };
            relay.set_rooms(rooms);
            // Say where this session's pointer is whether or not an app is publishing —
            // the app-driven republish alone stays silent over a window whose app draws no cursor.
            if let Some(here) = here.as_deref() {
                relay.publish_local_cursor(here);
            }
            frame_builder.pump_peer_presence()
        } else {
            false
        };
        pump_checkpoint!("peer-presence");
        let pump_presence_done = Instant::now();
        let pump_apps_started = pump_presence_done;
        for osd in transport_osds.drain() {
            frame_builder.with_chrome(|st| publish_media_osd(st, osd));
            session_changed = true;
        }
        let mut apps_changed = foreign_capture_changed
            | frame_builder.pump_apps(&mut test_control_ctx.doc, &test_control_ctx.root_obj);
        crate::shell::launcher::record_presented_launches(|surface| {
            crate::compositor::foreign::store().frame_seq(surface)
        });
        let onboarding_active = frame_builder.host().onboarding_active();
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
        if take_deferred_peer_sync_activation(&mut deferred_peer_sync, onboarding_active) {
            // `pump_apps` just observed the trusted onboarding completion.
            // Apps were withheld until this edge. The network starts off the
            // loop; launches wait until it and the broker over it exist, so
            // an app's first service descriptor sees the live broker.
            frame_builder.host_mut().set_peer_sync_enabled(true);
            crate::shell::launcher::hold_launches();
            network_start = Some(start_session_peer_sync(
                tree_handle.clone(),
                Arc::clone(&session_wake),
            ));
        }
        if let Some(outcome) = network_start
            .as_ref()
            .and_then(crate::session_runtime::PendingStart::poll)
        {
            network_start = None;
            let start_failure = match outcome {
                Ok(network) => {
                    peer_sync = Some(install_session_peer_sync(
                        &mut frame_builder,
                        &mut state,
                        tree_handle.clone(),
                        network,
                    ));
                    if let Some(net) = peer_sync.as_ref() {
                        net.set_wake(Arc::clone(&session_wake));
                    }
                    None
                }
                Err(error) => {
                    tracing::warn!(%error, "peer-sync: spawn failed; continuing without sync");
                    Some(error)
                }
            };
            collab_broker =
                spawn_session_collab_broker(&frame_builder.chrome_handle(), peer_sync.as_ref());
            if let Some(broker) = collab_broker.as_ref() {
                local_session_lifecycle = Some(install_native_session_lifecycle(
                    broker,
                    &session,
                    onboarding_active,
                ));
                attach_collab_broker_bridges(&mut frame_builder, broker);
                if let (Some(portal), Some(service)) =
                    (portal_backend.as_ref(), broker.native_media_service())
                {
                    service
                        .install_consent(portal.media_grants.clone(), portal.media_prompts.clone());
                }
                realtime_fabric_bridge = broker.realtime_fabric_bridge();
                if let (Some(runtime), Some(bridge)) = (
                    device_fabric_runtime.as_mut(),
                    realtime_fabric_bridge.as_ref(),
                ) && let Some(adapter) = bridge.adapter()
                {
                    runtime.set_realtime_adapter(adapter);
                }
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
                    if let Some(runtime) = device_fabric_runtime.as_mut() {
                        runtime.set_remote_capture_runtime(std::sync::Arc::clone(remote_capture));
                    }
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
        let animating = animation_demand.any();
        let widget_deadline_due = frame_builder
            .next_wake_deadline()
            .is_some_and(|deadline| deadline <= iter_t0);
        let output_presentation_count = state.output_presentation_surfaces.len();
        let presentation_frame_changed = update_output_presentation_repaint_pending(
            &mut output_presentation_repaint_pending,
            &mut output_presentation_surface_count,
            output_presentation_count,
            state.output_presentation_frame_changed(),
        );
        let keepalive_due = false;
        let lock_clock_due =
            lock_clock_refresh_due(session.is_locked(), iter_t0, &mut last_lock_clock_refresh);
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
        let pump_done = Instant::now();
        let pump_total = pump_done
            .duration_since(iter_t0)
            .checked_sub(t_input_done)
            .unwrap_or_default();
        metrics.mark_pump(pump_total);
        if loop_timing && pump_slowest.1 > Duration::from_millis(100) {
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
        let content_changed = NativeRedrawReasons {
            force: force_first_paint,
            backend_event: had_backend_events,
            visible_input: visible_input_changed,
            session: session_changed || injected_redraw,
            accessibility: accessibility_changed,
            revision: revision_changed,
            hidden_desktop_animation: animating,
            apps: apps_changed || widget_deadline_due || foreign::frame_callbacks_waiting(&state),
            peers: peers_moved,
            presentation: presentation_frame_changed,
            capture: capture_executor.has_live_mirrors()
                || capture_executor.is_recording()
                || portal_backend
                    .as_ref()
                    .is_some_and(PortalBackend::capture_pending)
                || portal_backend
                    .as_ref()
                    .is_some_and(|portal| portal.screencast.has_streams()),
            keepalive: keepalive_due,
            lock_clock: lock_clock_due,
            lock_animation: lock_animation_deadline(
                frame_builder.theme(),
                &session,
                lock_has_wallpaper,
                last_lock_animation_frame,
                iter_t0,
            )
            .is_some_and(|deadline| iter_t0 >= deadline),
            lock_osd: lock_screen::lock_osd_frame_due(frame_builder.chrome().osd, iter_t0),
        }
        .content_changed(session.is_locked());
        // May a frame be shown yet? Off-cadence wakeups (protocol traffic,
        // test-control, input) would submit inside a discarded interval; first frame is exempt.
        let cadence_due = force_first_paint || frame_clock.due(iter_t0);
        // Is there room in the pipe? Building another scene while the render
        // thread is behind would only grow the queue (and input-to-photon latency); capped at `QUEUE_DEPTH + 1`, the tick still services input/clients without rendering.
        let pipe_ready = present_pipe
            .as_ref()
            .is_none_or(super::present::PresentPipe::has_capacity);
        if content_changed
            && cadence_due
            && !pipe_ready
            && let Some(p) = present_pipe.as_mut()
        {
            p.note_backpressure();
            metrics.mark_backpressured();
        }
        let should_paint = content_changed && cadence_due && pipe_ready;
        // If content became dirty off-cadence, keep cadence in the next park;
        // otherwise the event edge has already been consumed and a long idle
        // deadline could strand that frame. A full presenter is itself the
        // gate: its retirement ping wakes the loop when capacity returns.
        let cadence_needed = cadence_wait_needed(
            pipe_ready,
            (content_changed && !cadence_due) || (!session.is_locked() && animating),
        );

        // Diagnostic: `TILO_PAINT_STATS=1` logs every ~2s how many frames painted
        // vs idled and why, confirming the loop actually idles without guessing.
        if paint_stats {
            frames_total = frames_total.saturating_add(1);
            if should_paint {
                frames_painted = frames_painted.saturating_add(1);
                if had_backend_events {
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
                    if animation_demand.host & (1 << 10) != 0 && animating_content_leaves.is_empty()
                    {
                        animating_content_leaves = frame_builder.content_animating_leaves();
                    }
                }
                if keepalive_due
                    && !(had_backend_events || session_changed || revision_changed || animating)
                {
                    cause_keepalive = cause_keepalive.saturating_add(1);
                }
            }
            if stats_since.elapsed() >= Duration::from_secs(2) {
                let animation_host_labels =
                    crate::shell::animation_diagnostics::host_labels(animation_sources_seen.host)
                        .collect::<Vec<_>>()
                        .join(",");
                eprintln!(
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
                );
                frames_total = 0;
                frames_painted = 0;
                cause_events = 0;
                cause_revision = 0;
                cause_anim = 0;
                cause_session = 0;
                cause_keepalive = 0;
                eprintln!(
                    "paint-stats: wakes {} | widget-deadline owner {:?}",
                    wakes_seen.take_summary(),
                    widget_wake_owner.take().unwrap_or_default(),
                );
                animation_sources_seen = crate::shell::frame::AnimationDemand::default();
                animating_content_leaves.clear();
                stats_since = Instant::now();
            }
        }

        if should_paint {
            // Lay out only when about to paint — any structural change (spliced group,
            // tab switch, resize) is applied here, right before the scene is built.
            let t_layout_start = Instant::now();
            frame_builder.relayout();
            frame_builder.reconcile_native_focus(&mut test_control_ctx.focus);
            // `relayout` publishes a new tile rect (for a tab switch, move,
            // fullscreen or hotplug) immediately before painting it. Reconcile
            // here as well as above so the client gets the output transition in
            // the same frame rather than one idle tick later.
            reconcile_surface_output_memberships(
                &mut surface_output_memberships,
                &state,
                &frame_builder,
                backend.outputs(),
                &output_globals,
            );
            native_accessibility.set_window_focus(true);
            if session.is_locked() {
                let lock_outputs: Vec<LockOutput> = backend
                    .outputs()
                    .iter()
                    .map(|o| LockOutput {
                        origin: (f64::from(o.position.0), f64::from(o.position.1)),
                        size: (f64::from(o.size.0), f64::from(o.size.1)),
                        scale: f64::from(o.scale),
                    })
                    .collect();
                let lock_outputs = if lock_outputs.is_empty() {
                    let vp = frame_builder.scene_size();
                    vec![LockOutput {
                        origin: (0.0, 0.0),
                        size: (vp.0, vp.1),
                        scale: 1.0,
                    }]
                } else {
                    lock_outputs
                };
                let now = chrono::Local::now();
                let clock = now.format("%H:%M").to_string();
                let date = now.format("%A, %B %-d").to_string();
                let view = LockView {
                    password_len: lock.buffer_len(),
                    error: lock.error(),
                    clock: &clock,
                    date: &date,
                    notification_count: frame_builder.chrome().notification_count(),
                    verifying: lock.is_verifying(),
                    now_playing: lock_player.as_ref(),
                };
                native_accessibility.update_if_active(|| {
                    lock_screen::build_accessibility_tree(
                        &lock_outputs,
                        frame_builder.theme(),
                        &view,
                    )
                });
                if frame_builder.narrator_enabled() {
                    frame_builder.narrate(&lock_screen::narration_tree(&view));
                }
            } else {
                native_accessibility.update_if_active(|| {
                    frame_builder.build_a11y_tree_update(test_control_ctx.focus, 1.0)
                });
                frame_builder.note_keyboard_focus(test_control_ctx.focus);
                frame_builder.narrate_chrome();
            }
            let layout_took = t_layout_start.elapsed();
            metrics.mark_layout(layout_took);
            let t_scene_start = Instant::now();
            // When locked, paint the compositor-owned lock overlay instead of the chrome — app content is never submitted while locked.
            let retained_damage_supported = backend.supports_retained_scene_damage();
            frame_builder.set_session_locked(session.is_locked());
            let mut frame_paint = if session.is_locked() {
                let theme = frame_builder.theme().clone();
                // The lock overlay covers every known output, centred on each (single on
                // winit/headless; DRM lays monitors side-by-side); falls back to one output spanning the viewport if none reported yet.
                let lock_outputs: Vec<LockOutput> = backend
                    .outputs()
                    .iter()
                    .map(|o| LockOutput {
                        origin: (f64::from(o.position.0), f64::from(o.position.1)),
                        size: (f64::from(o.size.0), f64::from(o.size.1)),
                        scale: f64::from(o.scale),
                    })
                    .collect();
                let lock_outputs = if lock_outputs.is_empty() {
                    let vp = frame_builder.scene_size();
                    vec![LockOutput {
                        origin: (0.0, 0.0),
                        size: (vp.0, vp.1),
                        scale: 1.0,
                    }]
                } else {
                    lock_outputs
                };
                // Count-only summary on the lock screen — no bodies leak.
                let notif_count = frame_builder.chrome().notification_count();
                // Wall clock/date computed here (not in the pure paint) so the lock surface renders as themed compositor chrome.
                let now = chrono::Local::now();
                let clock = now.format("%H:%M").to_string();
                let date = now.format("%A, %B %-d").to_string();
                let view = LockView {
                    password_len: lock.buffer_len(),
                    error: lock.error(),
                    clock: &clock,
                    date: &date,
                    notification_count: notif_count,
                    verifying: lock.is_verifying(),
                    now_playing: lock_player.as_ref(),
                };
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
                lock_has_wallpaper = wallpaper.is_some();
                last_lock_animation_frame = iter_t0;
                let mut lock_scene = lock_screen::draw_lock_scene_at_time(
                    &lock_outputs,
                    &theme,
                    frame_builder.text_renderer_mut(),
                    &view,
                    wallpaper.as_ref(),
                    iter_t0
                        .saturating_duration_since(lock_animation_epoch)
                        .as_secs_f64(),
                );
                // Media keys stay live while locked; their readout is drawn
                // here because the desktop chrome that normally paints it is
                // hidden. Expiring on paint mirrors the unlocked host.
                let mut lock_osd = None;
                frame_builder.with_chrome(|chrome| {
                    chrome.expire_osd(iter_t0);
                    lock_osd = chrome.osd;
                });
                if let Some(osd) = lock_osd {
                    lock_screen::draw_lock_osd(
                        &mut lock_scene,
                        &lock_outputs,
                        &theme,
                        frame_builder.text_renderer_mut(),
                        osd,
                        iter_t0,
                    );
                }
                FramePaint::full(lock_scene)
            } else {
                let mut frame_paint = if retained_damage_supported {
                    frame_builder.paint_with_damage()
                } else {
                    let scene = frame_builder.paint();
                    FramePaint::full(scene).with_hdr_layers(frame_builder.take_hdr_layers())
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
                        CaptureOutcome::ScreenshotSendRequested { path, target } => {
                            let mut bridge = crate::shell::screenshot_send::FabricSendBridge {
                                network: peer_sync.as_deref(),
                                workspace_root: crate::shell::screenshot_send::workspace_files_root(
                                ),
                            };
                            match crate::shell::screenshot_send::route(&path, &target, &mut bridge)
                            {
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
                        CaptureOutcome::Failed(error) => {
                            tracing::warn!(%error, "capture failed");
                            frame_builder.with_chrome(|chrome| {
                                chrome.push_error_notification(
                                    crate::shell::i18n::t("capture-failed"),
                                    error,
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
            // Neither capture path transmits the lock screen: receiver
            // sessions may remain connected, but no fresh frame reveals
            // password entry, notification counts, or unlock timing.
            let personal_display_profile =
                crate::shell::device_fabric_runtime::personal_display_capture_profile();
            projection_capture.set_personal_profile(personal_display_profile);
            if crate::shell::device_fabric_runtime::take_personal_display_keyframe_request() {
                projection_capture.request_keyframe();
            }
            if let Some(net) = peer_sync.as_ref() {
                while let Some(packet) = projection_capture.take_packet() {
                    let _ = crate::shell::device_fabric_runtime::publish_personal_display_packet(
                        net, &packet,
                    );
                }
            }
            if !session.is_locked()
                && (frame_builder.host().projection_capture_requested()
                    || personal_display_profile.is_some())
            {
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
            let t_submit_start = Instant::now();
            let mut render_thread_lost = false;
            let mut submitted_serial = None;
            let async_present = if let Some(pipe) = present_pipe.as_mut() {
                submitted_serial = pipe.submit_frame_with_input_age(
                    frame_paint,
                    OutputId(0),
                    metrics.pending_input_age(t_submit_start),
                );
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
            // Capture lateness against the deadline that admitted this frame.
            // `note_presented` advances the clock immediately below, so doing
            // this afterwards would compare against the *next* interval and
            // hide an overrun by roughly one refresh period.
            metrics.mark_deadline_lateness(
                submit_done.saturating_duration_since(frame_clock.next_deadline()),
            );
            metrics.mark_submit(
                submit_done.saturating_duration_since(t_submit_start),
                async_present,
                submit_done,
                submitted_serial,
            );
            // An asynchronous presenter advances the cadence only when its
            // physical retirement is observed at the top of a later loop
            // iteration. Advancing here at scene handoff used to run the
            // software clock ahead of KMS page-flip completion, repeatedly
            // filling the bounded queue and collapsing a 144 Hz panel toward
            // 60 Hz. Synchronous/headless presentation still retires here.
            if !async_present {
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
        } else if had_backend_events {
            // Input was dispatched but the frame deferred — lay out anyway so the *next*
            // event hit-tests post-input rects, not the pre-overlay tree. Layout is ~0.01ms.
            frame_builder.relayout();
            frame_builder.reconcile_native_focus(&mut test_control_ctx.focus);
            reconcile_surface_output_memberships(
                &mut surface_output_memberships,
                &state,
                &frame_builder,
                backend.outputs(),
                &output_globals,
            );
        }
        if retain_session_redraw(session_changed || injected_redraw, should_paint) {
            injected_redraw_pending = true;
        }
        let t_paint_done = iter_t0.elapsed();

        // Confirm an `ext_session_lock_v1` client's lock only after the overlay is
        // presented and locked, so `locked` never fires while app content could still show; a failed lock notifies the client automatically.
        if session.is_locked()
            && let Some(locker) = state.session_lock.take_pending_locker()
        {
            locker.lock();
        }

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
            }
            // An agent at work is the person's activity on a hosted machine:
            // without this it locks under its agents after the idle timeout.
            if injected && conn.agent {
                for t in session.note_activity(Instant::now()) {
                    apply_session_transition(
                        t,
                        backend.as_mut(),
                        present_pipe.as_ref(),
                        local_session_lifecycle.as_ref(),
                        &mut screen_dimmer,
                    );
                }
            }
        }
        if let Some(watching) = test_control_ctx.watch_request.take()
            && backend.set_watched(watching)
        {
            injected_redraw_pending = true;
        }
        // An agent driving the screen answers the shell's choosers; a viewer
        // only watching cannot click one.
        chooser_attended.store(!test_control_ctx.agents.is_empty(), Ordering::Release);
        test_control_conns.retain_mut(|conn| {
            if conn.closed
                && let Some(token) = conn.wake.take()
            {
                event_loop.handle().remove(token);
            }
            !conn.closed
        });
        let t_testctl_done = iter_t0.elapsed();
        metrics.mark_testctl(t_testctl_done.checked_sub(t_paint_done).unwrap_or_default());

        // Selection bookkeeping after dispatch, not in the protocol handler:
        // smithay raises `new_selection` before installing the new source, so an inline pull would read the previous owner.
        if let Some(portal) = portal_backend.as_ref() {
            portal.pump_remote_input(&mut state, session.is_locked());
        }
        state.pump_selection();
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
        if state.pump_foreign_capture_barriers(&display_handle_for_outputs) {
            injected_redraw_pending = true;
        }
        if let Err(err) = display.flush_clients() {
            eprintln!("tilo compositor: flush_clients error: {err}");
            break;
        }
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
        let t_work_done = iter_t0.elapsed();
        metrics.mark_clients(t_work_done.checked_sub(t_testctl_done).unwrap_or_default());
        if (loop_timing || t_work_done > Duration::from_millis(100))
            && t_work_done > Duration::from_millis(100)
        {
            eprintln!(
                "SLOW-ITER total_work={:?} | input={:?} pump=+{:?} post_pump_paint=+{:?} testctl=+{:?} dispatch_clients=+{:?}",
                t_work_done,
                t_input_done,
                pump_total,
                t_paint_done
                    .checked_sub(pump_done.duration_since(iter_t0))
                    .unwrap_or_default(),
                t_testctl_done.checked_sub(t_paint_done).unwrap_or_default(),
                t_work_done.checked_sub(t_testctl_done).unwrap_or_default(),
            );
        }

        // Pump pending calloop sources (including the listening socket) and
        // sleep until real work is due. Cadence participates only while a
        // visual change is waiting; registered fds wake this early for input,
        // clients, accessibility, and presenter retirement.
        let park_now = Instant::now();
        let lock_clock_deadline = session
            .is_locked()
            .then(|| last_lock_clock_refresh.checked_add(LOCK_CLOCK_REFRESH))
            .flatten();
        // Follow the players while the lock is visible; a blanked display
        // shows nothing, so it does not wake for them.
        let lock_media_deadline = (session.is_locked() && !session.is_blanked())
            .then(|| last_lock_media_poll.checked_add(LOCK_MEDIA_REFRESH))
            .flatten();
        // A redraw requested after this tick's paint decision (control-socket
        // input, a configure flush, a released capture barrier) has no fd
        // left to wake the loop; its frame is due on the next cadence.
        let cadence_needed =
            cadence_needed || cadence_wait_needed(pipe_ready, injected_redraw_pending);
        let (park, park_deadline) = native_loop_park_named(
            park_now,
            backend_event_wake,
            display.backend().has_pending_client_dispatch(),
            cadence_needed,
            frame_clock.next_deadline(),
            [
                (
                    "widget",
                    widget_deadline_for_presenter(pipe_ready, frame_builder.next_wake_deadline()),
                ),
                (
                    "lock-animation",
                    lock_animation_park_deadline(
                        park_now,
                        lock_animation_deadline(
                            frame_builder.theme(),
                            &session,
                            lock_has_wallpaper,
                            last_lock_animation_frame,
                            park_now,
                        ),
                    ),
                ),
                ("lock-clock", lock_clock_deadline),
                ("lock-media", lock_media_deadline),
                ("run-deadline", run_deadline),
                ("controller-nav", controller_nav.next_deadline(park_now)),
                ("backend-service", backend.next_service_deadline()),
                (
                    "wayland-client-fallback",
                    wayland_client_service_deadline(wayland_client_wake, park_now),
                ),
                ("text-commit", state.text_commit_deadline()),
                (
                    "client-followup",
                    client_followup_deadline(clients_dispatched, park_now),
                ),
            ],
        );
        let t_sleep_start = Instant::now();
        if let Err(err) = event_loop.dispatch(park, &mut state) {
            eprintln!("tilo compositor: event loop dispatch error: {err}");
            break;
        }
        let slept = t_sleep_start.elapsed();
        metrics.mark_sleep(slept);
        let wake = park_wake_reason(park, park_deadline, slept);
        metrics.mark_wake(wake);
        if paint_stats {
            wakes_seen.note(wake);
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
            && let Err(e) = atomic_file::write_creating_parent(std::path::Path::new(path), &json)
        {
            tracing::warn!(error = %e, path = ?path, "workspace tree save failed");
        }
    }

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
    // Explicitly drop service guards in dependency order so the next session
    // cannot observe stale endpoints while backend teardown finishes.
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
        crate::shell::osd::OsdKind::Volume { .. } => {
            quick_settings.volume = osd.level;
        }
        crate::shell::osd::OsdKind::Brightness => {
            quick_settings.brightness = osd.level;
        }
        crate::shell::osd::OsdKind::AirplaneMode { on } => {
            quick_settings.radios.airplane_mode = on;
        }
        crate::shell::osd::OsdKind::MicMute { .. }
        | crate::shell::osd::OsdKind::Transport(_)
        | crate::shell::osd::OsdKind::KeyboardBacklight
        | crate::shell::osd::OsdKind::NoBacklight
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
mod media_osd_publication_tests {
    use super::sync_quick_settings_from_osd;
    use crate::shell::chrome_state::QuickSettingsState;
    use crate::shell::osd::Osd;

    #[test]
    fn successful_level_osds_update_the_open_quick_settings_sliders() {
        let mut quick_settings = QuickSettingsState::default();

        sync_quick_settings_from_osd(&mut quick_settings, Osd::volume(0.73, false));
        assert!((quick_settings.volume - 0.73).abs() < f32::EPSILON);

        sync_quick_settings_from_osd(&mut quick_settings, Osd::brightness(0.28));
        assert!((quick_settings.brightness - 0.28).abs() < f32::EPSILON);
    }

    #[test]
    fn the_wireless_key_keeps_the_quick_settings_toggle_in_step() {
        let mut quick_settings = QuickSettingsState::default();
        sync_quick_settings_from_osd(&mut quick_settings, Osd::airplane_mode(true));
        assert!(quick_settings.radios.airplane_mode);
        sync_quick_settings_from_osd(&mut quick_settings, Osd::airplane_mode(false));
        assert!(!quick_settings.radios.airplane_mode);
    }
}

/// Session-owned ambient-light loop. Settings only records policy; the
/// compositor owns the display and therefore owns applying that policy after
/// Settings exits. Missing sensors/backlights simply produce no write.
/// Connector of the output containing `point` in compositor coordinates.
fn output_connector_at(
    outputs: &[crate::compositor::backend::Output],
    point: (f64, f64),
) -> Option<&str> {
    outputs
        .iter()
        .find(|output| {
            let (x, y) = (f64::from(output.position.0), f64::from(output.position.1));
            point.0 >= x
                && point.1 >= y
                && point.0 < x + f64::from(output.size.0)
                && point.1 < y + f64::from(output.size.1)
        })
        .map(|output| output.identity.connector.as_str())
        .filter(|connector| !connector.is_empty())
}

fn spawn_auto_brightness_worker() {
    let _ = std::thread::Builder::new()
        .name("tilo-auto-brightness".into())
        .spawn(|| {
            let mut controller = os_actions::AmbientBrightnessController::default();
            loop {
                let settings = settings_store::load();
                let enabled = settings_store::bool_at(
                    &settings,
                    "display",
                    "auto_brightness",
                )
                .unwrap_or(false);
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
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        });
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
            // SAFETY: `geteuid` has no failure mode.
            let uid = unsafe { libc::geteuid() };
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
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn presenter_backpressure_and_deadlines_schedule_work() {
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
                apply_session_transition(t, &mut backend, None, None, &mut dimmer);
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
            );
            assert_eq!(dimmer.overlay_alpha(), None);
        }
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
                accessibility: true,
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
        // Phase `due_lock_animation_waits_for_cadence_or_presenter_instead_of_spinning`.
        {
            let due_at = Instant::now();
            let now = due_at + Duration::from_millis(1);
            let due = Some(due_at);
            let frame = now + Duration::from_millis(8);
            assert_eq!(
                native_loop_park(
                    now,
                    false,
                    false,
                    true,
                    frame,
                    [lock_animation_park_deadline(now, due)]
                ),
                Some(Duration::from_millis(8))
            );
            assert_eq!(
                native_loop_park(
                    now,
                    false,
                    false,
                    false,
                    frame,
                    [lock_animation_park_deadline(now, due)]
                ),
                Some(IDLE_BACKEND_POLL)
            );
            let future = Some(now + Duration::from_millis(42));
            assert_eq!(
                native_loop_park(
                    now,
                    false,
                    false,
                    false,
                    frame,
                    [lock_animation_park_deadline(now, future)]
                ),
                Some(Duration::from_millis(42))
            );
        }
        // Phase `blanked_lock_stops_animating_until_the_panel_wakes`.
        {
            let theme = ui::theme::Theme::dark();
            let t0 = Instant::now();
            let config = IdleConfig {
                blank_timeout: Some(Duration::from_secs(10)),
                lock_timeout: Some(Duration::from_secs(5)),
                suspend_timeout: None,
            };
            let mut session = SessionManager::new(config, t0);
            let at = |secs| t0 + Duration::from_secs(secs);
            session.tick(at(6));
            assert!(session.is_locked() && !session.is_blanked());
            assert!(
                lock_animation_deadline(&theme, &session, false, t0, at(6)).is_some(),
                "a visible lock animates its backdrop"
            );
            session.tick(at(11));
            assert!(session.is_locked() && session.is_blanked());
            assert!(
                lock_animation_deadline(&theme, &session, false, t0, at(11)).is_none(),
                "a blanked panel must not keep the loop building lock frames"
            );
            session.note_activity(at(12));
            assert!(session.is_locked() && !session.is_blanked());
            assert!(lock_animation_deadline(&theme, &session, false, t0, at(12)).is_some());
        }
        // Phase `an_untouched_visible_lock_stops_animating`.
        {
            let theme = ui::theme::Theme::dark();
            let t0 = Instant::now();
            let config = IdleConfig {
                blank_timeout: None,
                lock_timeout: Some(Duration::from_secs(5)),
                suspend_timeout: None,
            };
            let mut session = SessionManager::new(config, t0);
            let at = |secs| t0 + Duration::from_secs(secs);
            session.tick(at(6));
            assert!(session.is_locked() && !session.is_blanked());
            assert!(lock_animation_deadline(&theme, &session, false, t0, at(6)).is_some());
            session.tick(at(21));
            assert!(
                lock_animation_deadline(&theme, &session, false, t0, at(21)).is_none(),
                "a lock nobody has touched for {LOCK_BACKDROP_MOTION:?} holds still"
            );
            session.note_activity(at(30));
            assert!(
                lock_animation_deadline(&theme, &session, false, t0, at(30)).is_some(),
                "input moves it again"
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
        // Phase `artwork_repaints_only_while_the_lock_is_visible`.
        {
            let artwork = NativeRedrawReasons {
                lock_animation: true,
                ..NativeRedrawReasons::default()
            };
            assert!(artwork.content_changed(true));
            assert!(!artwork.content_changed(false));
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
            fb.enable_onboarding(std::path::PathBuf::from("/tmp/tilo-onboarding-test"));
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
    #[serial_test::serial(process_env)]
    fn collab_socket_is_bound_once_for_the_launcher() {
        // Phase `compositor_binds_the_socket_the_launcher_dials`.
        // The compositor binds the socket the launcher dials. Assert across both
        // production paths, not either one's idea of the path: bind with the
        // compositor's spawn function, resolve with the launcher's resolver,
        // require an open descriptor. `required@1` makes a divergence an error, not the skip an optional service gets.
        {
            let runtime = tempfile::tempdir().expect("tempdir");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("XDG_RUNTIME_DIR", runtime.path());
            let fb = FrameBuilder::new((800, 600));
            let chrome = fb.chrome_handle();
            let broker = spawn_session_collab_broker(&chrome, None)
                .expect("the compositor must be able to bind its own collab socket");

            assert!(
                chrome.borrow().collab_available(),
                "a session that bound its broker must not report collaboration unavailable: {:?}",
                chrome.borrow().collab_unavailable
            );

            // The launcher's real resolver, on a manifest demanding the service.
            // Use the authenticated launch path: collaboration deliberately rejects
            // a bare socket connection with no app/workspace handoff.
            let mut services = std::collections::BTreeMap::new();
            services.insert(
                launcher::services::COLLAB_SERVICE.to_string(),
                "required@1".to_string(),
            );
            let resolved = launcher::services::resolve_service_binds_for_launch(
                &services,
                false,
                launcher::services::ServiceLaunchContext {
                    app_id: "so.tilo.socket-contract-test",
                    workspace_id: Some("socket-contract-workspace"),
                },
            )
            .expect("the launcher must authenticate with the socket the compositor just bound");

            assert_eq!(
                resolved.len(),
                1,
                "expected exactly one resolved service, got {resolved:?}"
            );
            assert_eq!(resolved[0].name, launcher::services::COLLAB_SERVICE);
            assert!(
                resolved[0].host_fd() >= 0,
                "the launcher must hand the app a real connected descriptor"
            );

            drop(broker);
        }
        // Phase `a_second_session_refuses_to_take_a_live_collab_socket`.
        // A live socket is not taken away from the session that owns it. Two
        // compositors against one `XDG_RUNTIME_DIR` is what a second login (or
        // stray test) produces, and the broker unlinks before binding — without
        // the liveness probe, the newcomer would silently strand the first session's apps. Refusing loudly is correct, and must reach the chrome.
        {
            let runtime = tempfile::tempdir().expect("tempdir");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("XDG_RUNTIME_DIR", runtime.path());

            let first_fb = FrameBuilder::new((800, 600));
            let first_chrome = first_fb.chrome_handle();
            let first =
                spawn_session_collab_broker(&first_chrome, None).expect("first broker binds");

            let second_fb = FrameBuilder::new((800, 600));
            let second_chrome = second_fb.chrome_handle();
            let second = spawn_session_collab_broker(&second_chrome, None);

            assert!(
                second.is_none(),
                "a second session must not bind over a live collab socket"
            );
            assert!(
                !second_chrome.borrow().collab_available(),
                "the refused session must report collaboration as unavailable"
            );
            assert!(
                first_chrome.borrow().collab_available(),
                "the session that owns the socket must be left working"
            );
            // And the first broker still answers — the point of refusing.
            assert!(
                collab_socket_is_live(&launcher::services::collab_socket()),
                "the original broker must still be listening after the refusal"
            );

            drop(second);
            drop(first);
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
            }],
            active_tab: 12,
            width: WindowWidth::Normal,
            maximized: false,
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
            }],
            active_tab: 14,
            width: WindowWidth::Normal,
            maximized: false,
        });
        workspace.shared_surfaces.share(7, 9);
        let none = std::collections::HashSet::new();

        let watched = desired_legacy_shares(&tree, true, &none);
        assert_eq!(
            watched.get(&7),
            Some(&("org.gimp.GIMP".to_string(), [9].into_iter().collect()))
        );
        assert!(
            !watched.contains_key(&8),
            "without a viewer host only explicit shares stream"
        );
        let hosts = [42].into_iter().collect();
        let through_host = desired_legacy_shares(&tree, true, &hosts);
        assert_eq!(
            through_host.get(&7),
            Some(&("org.gimp.GIMP".to_string(), [9, 42].into_iter().collect()))
        );
        assert_eq!(
            through_host.get(&8),
            Some(&(
                "org.chromium.Chromium".to_string(),
                [42].into_iter().collect()
            )),
            "a viewer host receives every legacy window of the active workspace"
        );
        assert!(
            desired_legacy_shares(&tree, false, &hosts).is_empty(),
            "unwatched, every encoder stops"
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
            r#"{"op":"grant","subject":"agent:x","scope":"workspace","verb":"admin"}"#,
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

    fn socket_response(reader: &mut BufReader<UnixStream>) -> test_control::Response {
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
        workspace.owner = Some(crate::shell::state::local_user_id_for_room(&workspace.room));
        tree.desktop.workspaces.push(workspace);
        tree.set_active(crate::shell::state::WorkspaceId(71));
        tree.shared()
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
                frame_builder
                    .host_mut()
                    .attach_app_action_broker(directory.broker.clone());
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
                frame_builder
                    .host_mut()
                    .attach_app_action_broker(directory.broker.clone());
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
                    pump_pending_test_control_conn(&mut conn),
                    PendingConnectionProgress::Waiting,
                    "write-side EOF must not discard a response the client can still read"
                );
                crate::shell::collab_broker_capnp::tests::drain_directory_bridge(&directory).await;
                assert_eq!(
                    pump_pending_test_control_conn(&mut conn),
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
                frame_builder
                    .host_mut()
                    .attach_app_action_broker(directory.broker.clone());
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
                    pump_pending_test_control_conn(&mut conn),
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

        // Nothing to read yet, so the loop sleeps out its whole timeout.
        let idle = Instant::now();
        event_loop
            .dispatch(Some(Duration::from_millis(200)), &mut state)
            .expect("idle dispatch");
        assert!(
            idle.elapsed() >= Duration::from_millis(150),
            "the loop must be idle before the request, or this proves nothing"
        );

        client
            .write_all(b"{\"op\":\"list_targets\"}\n")
            .expect("write the request");
        let woke = Instant::now();
        event_loop
            .dispatch(Some(Duration::from_secs(30)), &mut state)
            .expect("dispatch");
        assert!(
            woke.elapsed() < Duration::from_secs(10),
            "the request did not wake the loop (slept {:?})",
            woke.elapsed()
        );
    }

    // publishing the socket name for the nested dev loop

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
            atomic_file::write_with_mode(&path, b"wayland-7", 0o600).expect("write display file");

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
        // Phase `click_on_wheel_plus_slot_grows_workspace_count`.
        // Click sequence at the wheel-disc's plus slot mutates chrome state —
        // proves planned actions reach the host's click handlers, not a void.
        //
        // The bootstrap tree seeds three demo workspaces; the plus slot calls
        // `chrome.add_workspace("Untitled")`, growing the count to four —
        // asserting on the count is more robust than internal widget state, since chrome is the contract every consumer sees.
        {
            let mut fb = FrameBuilder::new((1600, 900));
            // Paint once so the wheel-disc populates its plus_rect (the press handler
            // hit-tests against it; it's `Rect::ZERO` until the first draw pass).
            let _ = fb.paint();
            // Find the wheel disc rect by its opt-in `test_id`, then search it for the
            // affordance rather than re-deriving where `draw` puts it — scanning the whole footprint keeps the test layout-agnostic.
            let registry = fb.widget_registry();
            let wheel_rect = registry
                .get("wheel_disc")
                .copied()
                .expect("wheel_disc test_id resolves on bootstrap tree");

            let initial_count = fb.chrome().tree().desktop.workspaces.len();

            // Sweep the footprint bottom-up in 4px rows until one click lands on
            // the affordance; misses are harmless (rim press drags back, row press switches workspace).
            let mut ctx = TestControlCtx::new();
            let mut found = false;
            // Integer-counted sweep (not a float-terminated `while`): the `if`
            // breaks below reproduce the original `y > y0` / `x < x1` bounds
            // exactly, so a generously wide static loop cap is safe — it never
            // runs anywhere near it before hitting one of them.
            'sweep: for row in 0..10_000u32 {
                let y = wheel_rect.y1 - 4.0 - f64::from(row) * 4.0;
                if y <= wheel_rect.y0 {
                    break;
                }
                for col in 0..10_000u32 {
                    let x = wheel_rect.x0 + 4.0 + f64::from(col) * 8.0;
                    if x >= wheel_rect.x1 {
                        break;
                    }
                    execute_planned_action(&PlannedAction::CursorMoved { x, y }, &mut fb, &mut ctx);
                    execute_planned_action(
                        &PlannedAction::MouseLeftButton { pressed: true },
                        &mut fb,
                        &mut ctx,
                    );
                    execute_planned_action(
                        &PlannedAction::MouseLeftButton { pressed: false },
                        &mut fb,
                        &mut ctx,
                    );
                    if fb.chrome().tree().desktop.workspaces.len() > initial_count {
                        found = true;
                        break 'sweep;
                    }
                }
            }
            assert!(
                found,
                "sweep of clicks across the wheel disc's footprint should hit the \
                 add-workspace affordance and grow the workspace count from \
                 {initial_count}"
            );
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
            // file — treat as a soft skip, mirrors `sidebar_wheel_disc_snapshot.rs::try_new_renderer`.
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

    // state_dump

    #[test]
    fn state_dump_chrome_includes_wheel_summon() {
        let fb = FrameBuilder::new((1600, 900));
        let state = FrameBuilderControlState {
            fb: &fb,
            doc: None,
            shares: None,
            lock: None,
        };
        assert_eq!(
            state.chrome_state_json().get("wheel_summon"),
            Some(&serde_json::Value::Null),
            "the wheel starts docked"
        );

        fb.chrome_handle()
            .borrow_mut()
            .set_wheel_summon(Some(vello::kurbo::Point::new(1180.0, 450.0)));
        let summoned = &state.chrome_state_json()["wheel_summon"];
        assert_eq!(summoned["x"].as_f64(), Some(1180.0));
        assert_eq!(summoned["y"].as_f64(), Some(450.0));
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
                let room = chrome.tree().desktop.active_workspace().unwrap().room;
                (room.to_string(), chrome.local_user_for_room(&room))
            };
            fb.chrome()
                .tree_handle()
                .write()
                .desktop
                .active_workspace_mut()
                .unwrap()
                .owner = Some(actor.clone());
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
            };
            let tree = state.workspace_tree_json();
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
            };
            let (resp, actions) =
                test_control::dispatch_command(&test_control::Command::StateDump, &state);
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
                workspace_tree.is_object(),
                "workspace_tree should be an object"
            );
            assert!(chrome.is_object(), "chrome should be an object");
        }
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
                    .room;
                let noted = tree.note_peer_present(room, 0x51, "alice");
                drop(tree);
                assert!(noted);
            }
            let state = FrameBuilderControlState {
                fb: &fb,
                doc: None,
                shares: None,
                lock: None,
            };
            let tree = state.workspace_tree_json();
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
            use crate::shell::state::{CapOverride, Subject, Verb};

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
                workspace.owner = Some("owner".to_string());
                let room = workspace.id.raw().to_string();
                workspace
                    .grant_as(
                        &"owner".to_string(),
                        CapOverride::membership(Subject::agent("release-notes"), room, Verb::Write),
                    )
                    .expect("owner admits agent");
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

/// Perform the navigation a claimed touchpad gesture resolved to — a second
/// way to ask for something the shell already does, not a parallel model.
fn apply_shell_gesture(
    frame_builder: &mut FrameBuilder,
    action: crate::compositor::backend::drm::gestures::GestureAction,
) {
    use crate::compositor::backend::drm::gestures::GestureAction;
    match action {
        GestureAction::StripPanUpdate { travel, began } => {
            frame_builder
                .host_mut()
                .update_active_strip_pan(travel, began);
            return;
        }
        GestureAction::StripPanEnd { .. } => {
            frame_builder
                .host_mut()
                .end_active_strip_pan(Instant::now());
            return;
        }
        _ => {}
    }
    match action {
        GestureAction::SwitchStrip(dir) => {
            frame_builder.with_chrome(|st| {
                st.switch_strip(dir);
            });
        }
        GestureAction::SwitchWorkspace(dir) => {
            let mut switched = false;
            frame_builder.with_chrome(|st| {
                switched = st.switch_workspace(dir);
            });
            if switched {
                frame_builder.host_mut().start_workspace_switch_frames();
            }
        }
        GestureAction::ShowOverview => {
            frame_builder.with_chrome(|st| {
                if !st.overlays.overview {
                    st.overlays.overview = true;
                    st.bump();
                }
            });
            frame_builder.host_mut().latch_overview_transition();
        }
        GestureAction::HideOverview => {
            frame_builder.with_chrome(|st| {
                if st.overlays.overview {
                    st.overlays.overview = false;
                    st.bump();
                }
            });
            frame_builder.host_mut().latch_overview_transition();
        }
        // Handled (and returned on) above; a panic here would take down the
        // whole session for a gesture that already did its job.
        GestureAction::StripPanUpdate { .. } | GestureAction::StripPanEnd { .. } => {}
    }
}

/// Resolve an xdg fullscreen request to one live backend output and paint it
/// above the normal tiled scene.  A compositor pin (the presenter view beside
/// an audience surface) wins, then the requested `wl_output`; if it was
/// removed, use the output currently containing the window's centre, then the
/// first live output as a deterministic hotplug fallback.
/// Super+F on a Tilo window covers the output, including the wheel.
/// Hosted Tilo apps are xdg clients too; sending them a protocol
/// fullscreen never sets `chrome.fullscreen_group` (W07). Maximize
/// (Super+M) stays beside the wheel. A focused xdg surface with no
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

/// Tell colour-managed clients what each output displays: the HDR signal of
/// an output driven in HDR, SDR otherwise. Surfaces are asked to render in
/// HDR only while every output is HDR, since an SDR output can show an HDR
/// surface only tone-mapped.
fn publish_output_colors(state: &State, backend: &dyn crate::compositor::backend::Backend) {
    use crate::wayland::color_management::OutputDescription;
    let outputs = backend.outputs();
    let described: Vec<_> = outputs
        .iter()
        .map(|output| {
            let description = backend
                .output_hdr(output.id)
                .map(|hdr| OutputDescription::hdr_pq(&hdr.display, hdr.sdr_white_nits));
            (output.id, description)
        })
        .collect();
    state.color_manager.update_outputs(|resource| {
        let id = output_globals::output_id_of(resource)?;
        described
            .iter()
            .find_map(|(output, description)| (*output == id).then_some(*description))
            .flatten()
    });
    let preferred = match described.as_slice() {
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
    let rect = output.logical_rect();
    frame.set_fullscreen_surface(Some((target.surface_id, "Fullscreen".to_string(), rect)));
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
mod vt_chord_tests {
    use super::{PriorityRoute, priority_route, vt_switch_chord};
    use crate::shell::media_keys::MediaKey;
    use winit::event::{ElementState, KeyEvent, Modifiers, WindowEvent};
    use winit::keyboard::{Key, ModifiersState, NamedKey, NativeKeyCode, PhysicalKey};

    fn key(named: NamedKey, state: ElementState) -> WindowEvent {
        // Same zero-init workaround the other `KeyEvent` builders use: `platform_specific` is pub(crate) and tolerates all-zero.
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(named);
        ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        ev.state = state;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }

    fn mods(state: ModifiersState) -> Modifiers {
        Modifiers::from(state)
    }

    #[test]
    fn ctrl_alt_f_press_alone_is_the_vt_switch_chord() {
        // Phase `ctrl_alt_f_maps_to_the_matching_vt`.
        {
            let m = mods(ModifiersState::CONTROL | ModifiersState::ALT);
            assert_eq!(
                vt_switch_chord(&key(NamedKey::F1, ElementState::Pressed), m),
                Some(1)
            );
            assert_eq!(
                vt_switch_chord(&key(NamedKey::F3, ElementState::Pressed), m),
                Some(3),
                "Ctrl+Alt+F3 is the one the seat diagnosis tells people to use"
            );
            assert_eq!(
                vt_switch_chord(&key(NamedKey::F12, ElementState::Pressed), m),
                Some(12)
            );
        }
        // Phase `either_modifier_alone_is_not_the_chord`.
        // Alt+F4 closes a window on every desktop; claiming it as a VT switch would be a spectacular regression.
        assert_eq!(
            vt_switch_chord(
                &key(NamedKey::F4, ElementState::Pressed),
                mods(ModifiersState::ALT)
            ),
            None
        );
        assert_eq!(
            vt_switch_chord(
                &key(NamedKey::F4, ElementState::Pressed),
                mods(ModifiersState::CONTROL)
            ),
            None
        );
        assert_eq!(
            vt_switch_chord(
                &key(NamedKey::F4, ElementState::Pressed),
                mods(ModifiersState::empty())
            ),
            None
        );
        // Phase `only_the_press_switches`.
        {
            // Acting on the release too would switch twice per chord.
            let m = mods(ModifiersState::CONTROL | ModifiersState::ALT);
            assert_eq!(
                vt_switch_chord(&key(NamedKey::F2, ElementState::Released), m),
                None
            );
        }
        // Phase `other_keys_with_the_modifiers_held_are_not_vt_switches`.
        {
            let m = mods(ModifiersState::CONTROL | ModifiersState::ALT);
            assert_eq!(
                vt_switch_chord(&key(NamedKey::Delete, ElementState::Pressed), m),
                None
            );
            assert_eq!(
                vt_switch_chord(&key(NamedKey::Tab, ElementState::Pressed), m),
                None
            );
        }
    }

    fn character(c: &str) -> WindowEvent {
        let mut event = key(NamedKey::F1, ElementState::Pressed);
        if let WindowEvent::KeyboardInput { event, .. } = &mut event {
            event.logical_key = Key::Character(c.into());
        }
        event
    }

    fn route(event: &WindowEvent, state: ModifiersState, locked: bool) -> PriorityRoute {
        priority_route(
            event,
            mods(state),
            locked,
            &settings_store::hardware::ShortcutConfig::default(),
        )
    }

    #[test]
    fn media_keys_act_while_the_lock_captures_everything_else() {
        let none = ModifiersState::empty();
        for (named, media) in [
            (NamedKey::AudioVolumeUp, MediaKey::VolumeUp),
            (NamedKey::AudioVolumeDown, MediaKey::VolumeDown),
            (NamedKey::AudioVolumeMute, MediaKey::VolumeMute),
            (NamedKey::BrightnessUp, MediaKey::BrightnessUp),
            (NamedKey::BrightnessDown, MediaKey::BrightnessDown),
            (NamedKey::MediaPlayPause, MediaKey::PlayPause),
        ] {
            let press = key(named, ElementState::Pressed);
            assert_eq!(route(&press, none, true), PriorityRoute::MediaKey(media));
            assert_eq!(route(&press, none, false), PriorityRoute::MediaKey(media));
            assert_eq!(
                route(&key(named, ElementState::Released), none, true),
                PriorityRoute::LockScreen,
                "a media key release is inert and stays with the lock"
            );
        }

        for captured in [NamedKey::LaunchApplication2, NamedKey::Standby] {
            assert_eq!(
                route(&key(captured, ElementState::Pressed), none, true),
                PriorityRoute::LockScreen,
                "{captured:?} opens UI or suspends, so the lock keeps it"
            );
            assert!(matches!(
                route(&key(captured, ElementState::Pressed), none, false),
                PriorityRoute::MediaKey(_)
            ));
        }

        assert_eq!(
            route(&character("a"), none, true),
            PriorityRoute::LockScreen
        );
        assert_eq!(
            route(&character("l"), ModifiersState::SUPER, true),
            PriorityRoute::LockScreen,
            "the lock chord is inert while already locked"
        );
        assert_eq!(
            route(
                &key(NamedKey::F2, ElementState::Pressed),
                ModifiersState::CONTROL | ModifiersState::ALT,
                true
            ),
            PriorityRoute::LockScreen,
            "a VT switch cannot bypass the lock"
        );
    }

    #[test]
    fn unlocked_priority_order_is_power_vt_panic_lock_media_then_clients() {
        let none = ModifiersState::empty();
        assert_eq!(
            route(&key(NamedKey::Power, ElementState::Pressed), none, false),
            PriorityRoute::PowerKey
        );
        assert_eq!(
            route(
                &key(NamedKey::F3, ElementState::Pressed),
                ModifiersState::CONTROL | ModifiersState::ALT,
                false
            ),
            PriorityRoute::VtSwitch(3)
        );
        assert_eq!(
            route(
                &key(NamedKey::Escape, ElementState::Pressed),
                ModifiersState::SUPER,
                false
            ),
            PriorityRoute::RemoteInputKillSwitch
        );
        assert_eq!(
            route(&character("l"), ModifiersState::SUPER, false),
            PriorityRoute::LockSession
        );
        assert_eq!(
            route(
                &key(NamedKey::NumLock, ElementState::Pressed),
                ModifiersState::SHIFT | ModifiersState::ALT,
                false
            ),
            PriorityRoute::ToggleMouseKeys
        );
        assert_eq!(
            route(&key(NamedKey::NumLock, ElementState::Pressed), none, false),
            PriorityRoute::Continue,
            "a bare Num Lock is the client's"
        );
        assert_eq!(
            route(
                &key(NamedKey::NumLock, ElementState::Pressed),
                ModifiersState::SHIFT | ModifiersState::ALT,
                true
            ),
            PriorityRoute::LockScreen,
            "the lock keeps the chord"
        );
        assert_eq!(route(&character("a"), none, false), PriorityRoute::Continue);
    }
}

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
            apply_shell_gesture(&mut fb, GestureAction::SwitchWorkspace(-1));
            let after = fb.chrome().tree().desktop.active;
            assert_ne!(
                before, after,
                "a four-finger swipe must move the active workspace, not just resolve to an intent"
            );
        }
        // Phase `a_three_finger_vertical_swipe_focuses_the_neighbouring_strip`.
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
            let workspace = fb.chrome().tree().desktop.active;
            let (_, focused) = fb.chrome().strip_focus();
            apply_shell_gesture(&mut fb, GestureAction::SwitchStrip(1));
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
            apply_shell_gesture(&mut fb, GestureAction::ShowOverview);
            assert!(fb.chrome().overlays.overview);
            apply_shell_gesture(&mut fb, GestureAction::HideOverview);
            assert!(!fb.chrome().overlays.overview);
        }
        // Phase `showing_an_already_open_overview_is_a_no_op`.
        {
            // The gesture fires once per swipe; a second swipe in the same direction must not bump the revision for nothing.
            let mut fb = builder();
            apply_shell_gesture(&mut fb, GestureAction::ShowOverview);
            let revision = fb.chrome().revision;
            apply_shell_gesture(&mut fb, GestureAction::ShowOverview);
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
#[cfg(test)]
mod output_geometry_tests {
    use super::{input_capture_zones, output_geometry, select_fullscreen_output};
    use crate::compositor::backend::{Output, OutputId};

    /// A 2x 2560x1600 laptop panel with a 1x 2560x1440 monitor to its right:
    /// logically the monitor starts at 1280, in the scene at 2560.
    fn docked() -> [Output; 2] {
        [
            Output::new(OutputId(0), (2560, 1600), 2.0),
            Output {
                id: OutputId(1),
                position: (2560, 0),
                size: (2560, 1440),
                scale: 1.0,
                transform: settings_store::hardware::OutputTransform::Normal,
                color_profile: None,
                refresh_mhz: None,
                logical_position: (1280, 0),
                identity: crate::compositor::backend::OutputIdentity::default(),
            },
        ]
    }

    /// The shell lays out in the logical desktop and paints each display
    /// into its scene rectangle at its own scale.
    #[test]
    fn a_dense_panel_is_a_logical_display_drawn_at_its_scale() {
        let geo = output_geometry(&docked());
        assert_eq!(
            (
                geo[0].x,
                geo[0].y,
                geo[0].width,
                geo[0].height,
                geo[0].scale
            ),
            (0.0, 0.0, 1280.0, 800.0, 2.0)
        );
        assert_eq!(
            geo[0].scene,
            vello::kurbo::Rect::new(0.0, 0.0, 2560.0, 1600.0)
        );
        assert_eq!(
            (geo[1].x, geo[1].width, geo[1].scale),
            (1280.0, 2560.0, 1.0)
        );
        assert_eq!(
            geo[1].scene,
            vello::kurbo::Rect::new(2560.0, 0.0, 5120.0, 1440.0)
        );
    }

    /// `InputCapture` zones are where a portal client's pointer barriers go:
    /// the logical desktop the pointer moves in, gapless across a 2x|1x seam.
    #[test]
    fn input_capture_zones_are_logical() {
        let zones = input_capture_zones(&docked());
        assert_eq!(
            zones
                .iter()
                .map(|z| (z.x, z.y, z.width, z.height))
                .collect::<Vec<_>>(),
            vec![(0, 0, 1280, 800), (1280, 0, 2560, 1440)]
        );
    }

    #[test]
    fn output_geometry_maps_outputs_to_displays_and_rehomes_fullscreen() {
        // Phase `single_output_maps_to_one_display`.
        // A single monitor maps to one display at the origin — the case every screenshot and test runs in.
        {
            let outs = [Output::new(OutputId(0), (3840, 2160), 1.0)];
            let geo = output_geometry(&outs);
            assert_eq!(geo.len(), 1);
            assert_eq!(
                (geo[0].x, geo[0].width, geo[0].height),
                (0.0, 3840.0, 2160.0)
            );
        }
        // Phase `side_by_side_outputs_stay_separate_displays`.
        // Two monitors side by side stay two displays, each with its own width —
        // what stops a full-width window spanning both (combined geometry here is 3520px, neither display is that wide).
        {
            let outs = [
                Output::new(OutputId(0), (1600, 900), 1.0),
                Output {
                    id: OutputId(1),
                    position: (1600, 0),
                    size: (1920, 1080),
                    scale: 1.0,
                    transform: settings_store::hardware::OutputTransform::Normal,
                    color_profile: None,
                    refresh_mhz: None,
                    logical_position: (1600, 0),
                    identity: crate::compositor::backend::OutputIdentity::default(),
                },
            ];
            let geo = output_geometry(&outs);
            assert_eq!(geo.len(), 2);
            assert_eq!((geo[0].x, geo[0].width), (0.0, 1600.0));
            assert_eq!((geo[1].x, geo[1].width), (1600.0, 1920.0));
        }
        // Phase `zero_sized_outputs_are_dropped`.
        // A connector mid-modeset reports 0x0; a strip can't place a window on it, so it must not appear as a display.
        {
            let outs = [
                Output::new(OutputId(0), (1600, 900), 1.0),
                Output::new(OutputId(1), (0, 0), 1.0),
            ];
            assert_eq!(output_geometry(&outs).len(), 1);
        }
        // Phase `fullscreen_prefers_requested_output_then_rehomes_after_hotplug`.
        {
            let outputs = [
                Output::new(OutputId(0), (1600, 900), 1.0),
                Output {
                    id: OutputId(1),
                    position: (1600, 0),
                    size: (1920, 1080),
                    scale: 1.0,
                    transform: settings_store::hardware::OutputTransform::Normal,
                    color_profile: None,
                    refresh_mhz: None,
                    logical_position: (1600, 0),
                    identity: crate::compositor::backend::OutputIdentity::default(),
                },
            ];
            let centre = vello::kurbo::Point::new(2000.0, 400.0);
            assert_eq!(
                select_fullscreen_output(Some(OutputId(1)), Some(centre), &outputs)
                    .map(|out| out.id),
                Some(OutputId(1)),
                "requested wl_output wins over the old tiled placement"
            );
            let remaining = &outputs[..1];
            assert_eq!(
                select_fullscreen_output(Some(OutputId(1)), Some(centre), remaining)
                    .map(|out| out.id),
                Some(OutputId(0)),
                "removed requested output deterministically rehomes fullscreen"
            );
            assert_eq!(
                select_fullscreen_output(
                    None,
                    Some(vello::kurbo::Point::new(300.0, 400.0)),
                    &outputs
                )
                .map(|out| out.id),
                Some(OutputId(0)),
                "no requested output uses the window's owning display"
            );
        }
    }
}

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
