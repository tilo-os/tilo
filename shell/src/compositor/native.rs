//! Native compositor mode — Phase 2 calloop event loop scaffolding.
//!
//! Phase 1 wired the `--mode native` dispatch with a stub. Phase 2 replaces
//! that stub with a real smithay event loop that:
//!
//! 1. Creates a `wayland_server::Display` and a calloop `EventLoop` over our
//!    [`state::State`].
//! 2. Constructs the `State` (which advertises `wl_compositor`, `xdg_wm_base`,
//!    `wl_shm`, `wl_seat` machinery, `wl_output` manager).
//! 3. Binds an auto-named listening socket (`wayland-1`, `wayland-2`, …) and
//!    inserts it into the loop so each accepted connection is handed back to
//!    `DisplayHandle::insert_client`.
//! 4. Runs forever — Phase 4 will tick rendering and dispatch from the loop's
//!    per-iteration callback.
//!
//! What this *doesn't* do: open a TTY, talk to DRM/KMS, render anything, or
//! forward input. Those are explicitly Phase 3+ work. The smoke test for
//! Phase 2 is just "the binary prints a listening-socket line and stays up
//! until killed".

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ObjId};

use crate::shell::search::action_api::{ActionArgs, ActionVisibility, ResourceRef};
use crate::shell::search::visibility::CapabilityVisibility;

/// Run `f` against the live chrome with a capability gate built from the
/// active workspace.
///
/// The gate is what makes the action API answer *for a caller* rather
/// than in the abstract: a resource the workspace's capability model
/// will not let this user act on is absent from enumeration and from
/// selection alike, so it cannot be invoked by an address the caller was
/// never given.
///
/// Failing to resolve a workspace is an error rather than a fallback to
/// "show everything". A gate that opens when it cannot find its policy
/// is not a gate.
/// One action's declared parameters, as the wire carries them.
///
/// The descriptor is what a caller that cannot see the screen builds an
/// invocation from, so it says the type and whether the value is
/// required, not just the name.
fn params_json(
    params: &[crate::shell::search::action_api::ActionParam],
) -> Vec<serde_json::Value> {
    params
        .iter()
        .map(|p| {
            serde_json::json!({
                "name": p.name,
                "type": p.ty.label(),
                "required": p.required,
                "description": p.description,
            })
        })
        .collect()
}

fn with_action_gate<R>(
    chrome_handle: &crate::shell::chrome_state::Chrome,
    f: impl FnOnce(&crate::shell::chrome_state::ChromeState, &dyn ActionVisibility) -> R,
) -> Result<R, String> {
    let chrome = chrome_handle.borrow();
    let tree = chrome.tree();
    let workspace = tree
        .active_workspace()
        .ok_or_else(|| "no active workspace to resolve capabilities against".to_string())?;
    let actor = workspace.actor_for_person(&chrome.local_user_id);
    let gate = CapabilityVisibility::new(workspace, actor);
    Ok(f(&chrome, &gate))
}
use smithay::reexports::calloop::{self, EventLoop};
use smithay::reexports::wayland_server::Display;
use smithay::wayland::socket::ListeningSocketSource;
use taffy::NodeId;
use vello::kurbo::Rect;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, KeyLocation, ModifiersState, NamedKey, PhysicalKey, SmolStr};

use crate::compositor::backend::{
    Backend, BackendEvent, DrmBackend, HeadlessBackend, OutputId, WinitBackend, combined_geometry,
    headless,
};
use crate::compositor::client_input;
use crate::compositor::drag_feedback;
use crate::compositor::foreign;
use crate::compositor::snapshot_encode;
use crate::compositor::frame::FrameBuilder;
use crate::compositor::idle::{IdleConfig, SessionManager, SessionTransition};
use crate::compositor::lock_auth;
use crate::compositor::lock_controller::{LockController, LockKey, UnlockOutcome};
use crate::compositor::lock_screen::{self, LockOutput, LockView};
use crate::compositor::metrics;
use crate::compositor::output_globals;
use crate::compositor::pacing;
use crate::compositor::present;

use crate::compositor::state::{ClientState, State};
use crate::compositor::test_control::{
    self, ControlState, KeyTag, NamedKeyTag, PlannedAction, parse_command, serialise_response,
};
use crate::compositor::{BackendKind, parse_backend_from_args};
use crate::shell::overlay_host::PeerSyncRequest;
use crate::shell::peer_presence::{POINTER_ACTUATED_IDLE, POINTER_IDLE};

/// Env-var name for the Phase 7 CI test introspection hook. When set to a
/// writable path, [`run`] serializes the shell `WorkspaceTree` to that path
/// on each frame tick as compact JSON. The test
/// (`tests/compositor_test.rs`) polls that file to assert foreign-tab
/// adoption + teardown without needing a public shell-state API.
///
/// The hook is gated on the env var being set; without it the
/// compositor never touches the filesystem from the frame loop.
const TEST_STATE_DUMP_ENV: &str = "NIRIOS_TEST_STATE_DUMP";

/// Env-var name for the real-input e2e control protocol. When set to a
/// writable path, [`run`] binds a unix socket at that path that accepts
/// newline-delimited JSON commands (see
/// `compositor::test_control::Command` for the schema). The socket is a
/// **developer-build** surface: it types, clicks and reads back state as if
/// it were the person at the keyboard, including while the session is
/// locked, so a shipped binary must not be able to open one. Reading the
/// variable is gated on [`crate::compositor::dev_env::DEV_ENV_OVERRIDES`].
///
/// Spec: `docs/superpowers/specs/2026-05-16-real-input-e2e-design.md`.
const TEST_CONTROL_ENV: &str = "QOS_TEST_CONTROL";

/// D-3 fix: RAII guard that unlinks a unix-socket file when dropped.
///
/// The compositor previously left its `QOS_TEST_CONTROL` socket file
/// behind on clean exit (and on panic). Subsequent runs depended on the
/// next-boot `remove_file` inside `bind_test_control_socket` to scrub
/// the stale entry; that worked for `bind`, but the orphan file lingered
/// in `XDG_RUNTIME_DIR` between sessions for any other consumer to see.
///
/// Wrapping the path in this struct ensures the file is removed both on
/// normal teardown and on the panic-unwind path (without the explicit
/// `std::process::exit(0)` at the end of `run` skipping over our
/// cleanup, since `exit` does NOT run destructors). The compositor
/// `run` function holds the guard for the entire socket lifetime and
/// explicitly `drop`s it before the final `process::exit`.
pub(crate) struct UnlinkOnDrop(pub(crate) PathBuf);

impl Drop for UnlinkOnDrop {
    fn drop(&mut self) {
        // Errors here are not actionable — the file might already be
        // gone (someone else cleaned up) or we might lack the right
        // to remove it. Either way the production path doesn't care.
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Bind the test-control unix socket. Returns `None` if the env var is
/// unset or the bind fails — bind failures are reported via
/// `eprintln!` but don't abort the compositor (the production path
/// must never depend on the test surface).
///
/// On success, returns the listener plus an `UnlinkOnDrop` guard that
/// removes the socket file on teardown (D-3).
/// Resolve the path the test-control socket should be bound at, as a pure
/// function of the gate and the raw variable.
///
/// `dev_overrides` is [`crate::compositor::dev_env::DEV_ENV_OVERRIDES`].
/// Taking it as an argument (instead of testing the `cfg` inline) is what
/// lets the shipped answer — always `None` — be asserted from a debug test
/// run, where it would otherwise be unobservable.
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
    // Best-effort cleanup of a stale socket file from a previous run —
    // `bind` would otherwise fail with EADDRINUSE.
    let _ = std::fs::remove_file(&path);
    match UnixListener::bind(&path) {
        Ok(listener) => {
            if let Err(e) = listener.set_nonblocking(true) {
                eprintln!("nirios compositor: test_control set_nonblocking: {e}");
                return None;
            }
            eprintln!("nirios compositor test_control listening on {path:?}");
            let guard = UnlinkOnDrop(path);
            Some((listener, guard))
        }
        Err(e) => {
            eprintln!("nirios compositor: test_control bind {path:?}: {e}");
            None
        }
    }
}

/// Directory under `$XDG_RUNTIME_DIR` where a running compositor publishes
/// facts about itself that the *next* command in a developer's terminal
/// needs. Sibling to the Wayland sockets themselves, and torn down with the
/// session.
pub const SESSION_RUNTIME_SUBDIR: &str = "qos-session";

/// File inside [`SESSION_RUNTIME_SUBDIR`] holding the bare socket name (e.g.
/// `wayland-2`) of the most recently started compositor, no trailing
/// newline, so `WAYLAND_DISPLAY=$(cat …)` is exactly right.
pub const WAYLAND_DISPLAY_FILE: &str = "wayland-display";

/// Where [`publish_wayland_display`] writes, given the value of
/// `$XDG_RUNTIME_DIR`. Split out so the path policy is testable without a
/// live session.
///
/// Returns `None` when the variable is unset or not absolute: this is a
/// convenience, and guessing at `/tmp` for it would put a session-scoped
/// file somewhere world-writable.
pub(crate) fn session_display_path(runtime_dir: Option<std::ffi::OsString>) -> Option<PathBuf> {
    let dir = PathBuf::from(runtime_dir?);
    if !dir.is_absolute() {
        return None;
    }
    Some(dir.join(SESSION_RUNTIME_SUBDIR).join(WAYLAND_DISPLAY_FILE))
}

/// Publish the compositor's Wayland socket name to
/// `$XDG_RUNTIME_DIR/qos-session/wayland-display`.
///
/// Last writer wins: nesting a second compositor inside the first overwrites
/// the file, which is the behaviour you want when the file's job is "what do
/// I connect to *now*". The printed line stays authoritative for a specific
/// compositor. Failure is never fatal — the socket is already up and the
/// name is already on stderr.
///
/// Returns the unlink guard so the file disappears with the process.
fn publish_wayland_display(socket_name: &std::ffi::OsStr) -> Option<UnlinkOnDrop> {
    let path = session_display_path(std::env::var_os("XDG_RUNTIME_DIR"))?;
    if let Some(dir) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("nirios compositor: publishing {}: {e}", path.display());
            return None;
        }
    }
    // 0600: the name is not a secret, but nothing else has a reason to write
    // it, and the connect-path it names is the session's own. Applied before
    // the rename so the file is never briefly visible wider than intended.
    match atomic_file::write_with_mode(&path, socket_name.as_encoded_bytes(), 0o600) {
        Ok(()) => Some(UnlinkOnDrop(path)),
        Err(e) => {
            eprintln!("nirios compositor: publishing {}: {e}", path.display());
            None
        }
    }
}

/// Env var naming the compositor's privileged Wayland socket, exported into
/// the environment of anything the session itself starts outside a sandbox.
///
/// A client that wants the clipboard-manager protocols connects to this
/// instead of `WAYLAND_DISPLAY`; everything else keeps using the public
/// socket and never notices the difference.
pub const PRIVILEGED_SOCKET_ENV: &str = "QOS_WAYLAND_PRIVILEGED";

/// Bind the privileged Wayland socket and publish its name.
///
/// The name carries the compositor's pid so two sessions on one machine (or a
/// test compositor next to a live one) cannot collide, and so a stale file
/// from a killed compositor never shadows a live socket. Returns the listener
/// and the socket name.
fn bind_privileged_socket() -> Result<(ListeningSocketSource, std::ffi::OsString), String> {
    let name = format!("qos-privileged-{}", std::process::id());
    let socket = ListeningSocketSource::with_name(&name).map_err(|e| e.to_string())?;
    let socket_name = socket.socket_name().to_os_string();
    // Published for privileged children (clipboard managers, session
    // services). Sandboxed apps are launched with an explicit environment by
    // `qos-launcher` and never inherit this.
    // SAFETY: single-threaded compositor startup, before any thread that
    // reads the environment has been spawned.
    unsafe { std::env::set_var(PRIVILEGED_SOCKET_ENV, &socket_name) };
    Ok((socket, socket_name))
}

/// Per-connection state for the test-control protocol. Holds a buffered
/// reader over a non-blocking `UnixStream` so partial lines accumulate
/// across iterations of the main loop.
struct TestControlConn {
    reader: BufReader<UnixStream>,
    closed: bool,
}

impl TestControlConn {
    fn new(stream: UnixStream) -> Option<Self> {
        if let Err(e) = stream.set_nonblocking(true) {
            eprintln!("nirios compositor: test_control conn set_nonblocking: {e}");
            return None;
        }
        Some(Self {
            reader: BufReader::new(stream),
            closed: false,
        })
    }
}

/// Walk-the-frame-builder `ControlState` impl. Captures a fresh
/// snapshot per `widget_registry()` call so the dispatch path sees
/// the live tree (no caching).
struct FrameBuilderControlState<'a> {
    fb: &'a FrameBuilder,
    /// Where this session's pointer is, or `None` if it has never been
    /// anywhere. Captured by value at construction: the dispatcher only
    /// needs the number, and reading it here keeps the live `State` out
    /// of a struct that otherwise borrows nothing mutable.
    pointer: Option<(f64, f64)>,
    /// The live CRDT the search providers read, plus its root object.
    ///
    /// `None` for the construction sites that only exercise layout and
    /// chrome (the unit tests below), which is why the action methods
    /// refuse rather than answer when it is absent: a fixture that
    /// cannot enumerate must not be able to report an empty surface.
    doc: Option<(&'a AutoCommit, &'a ObjId)>,
}

impl<'a> ControlState for FrameBuilderControlState<'a> {
    fn widget_registry(&self) -> HashMap<String, Rect> {
        self.fb.widget_registry()
    }

    fn pointer(&self) -> Option<(f64, f64)> {
        self.pointer
    }

    /// Serialise the live persisted [`WorkspaceTree`] — same blob
    /// the `NIRIOS_TEST_STATE_DUMP` env-var path writes on every
    /// frame tick. The chrome's `tree` field is the authoritative
    /// copy (the compositor's `State.workspace_tree` is the same
    /// type and stays in sync via the shared `Chrome` cell).
    /// The peers the overlay would paint this frame, read from the
    /// same presence context `draw_real_peers` draws from and keyed
    /// the same way — by room.
    ///
    /// A peer cursor is otherwise only assertable by looking at a
    /// picture, and "nobody arrived" looks exactly like "somebody
    /// arrived and was not drawn".
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
                        // What the pill over this cursor actually
                        // reads. The id above is the identity and is
                        // never blank; the name is what the room calls
                        // that identity, and the two differing is the
                        // whole point — a harness that could only see
                        // the id could not tell a peer the room has
                        // named from one it has not.
                        "name": presence.display_name(user),
                        "x": x,
                        "y": y,
                    })
                })
            })
            .collect();
        // Who the collaborators panel is listing, from the function
        // that paints it.
        //
        // A pointer is not a roster. The two answer different
        // questions — "is somebody moving a mouse in this room" and
        // "who is in it" — and a harness with only the first cannot
        // tell a peer the panel is showing from one it is not. A demo
        // did read `peers()["roster"]`, on every run it ever made, and
        // got an empty list back because nothing emitted the key: the
        // one assertion aimed at this surface was passing by never
        // reaching it.
        //
        // Derived from `collaborator_roster_rows` rather than
        // re-walked here, so what a harness reads and what a person
        // sees cannot drift apart.
        let chrome = self.fb.chrome();
        let roster: Vec<serde_json::Value> =
            crate::shell::overlay_host::collaborator_roster_rows(&chrome.tree())
                .into_iter()
                .map(|row| {
                    serde_json::json!({
                        "label": row.label,
                        "role": row.role,
                        // Kept separate from the label for the reason
                        // the panel keeps the tag separate from the
                        // name: a reader that had to infer which of
                        // these is a person would sometimes infer
                        // wrong, and that is the one mistake this
                        // surface must not invite.
                        "is_agent": row.is_agent,
                        // Named but not arrived. The one thing a
                        // harness watching somebody join needs, and
                        // the thing a screenshot of a dimmed row
                        // cannot be asserted on.
                        "invited": row.invited,
                    })
                })
                .collect();
        serde_json::json!({ "pointers": rows, "roster": roster })
    }

    fn workspace_tree_json(&self) -> serde_json::Value {
        let chrome = self.fb.chrome();
        // `unwrap_or(Null)` — `WorkspaceTree`'s `Serialize` impl is
        // derived and infallible in practice, but the trait method
        // can't `expect` because that would crash the live compositor
        // on a hypothetical bad write. Nulls are still
        // schema-compliant from the wire's perspective.
        serde_json::to_value(&*chrome.tree()).unwrap_or(serde_json::Value::Null)
    }

    /// Report the compositor's own frame bookkeeping for every foreign
    /// toplevel currently in the tree.
    ///
    /// `frame_seq` counts frames the compositor has actually taken from
    /// that client, so `0` means the tile is still the placeholder no
    /// matter what it looks like. `last_error` carries the reason when a
    /// capture failed, which is the case a blank tile would otherwise
    /// hide.
    fn foreign_frames_json(&self) -> serde_json::Value {
        let chrome = self.fb.chrome();
        let store = crate::compositor::foreign::store();
        let frames: Vec<serde_json::Value> =
            crate::compositor::foreign::foreign_tabs(&chrome.tree())
                .into_iter()
                .map(|tab| {
                    // The rect the widget tree laid this tab into, so a
                    // harness can aim at a window instead of guessing
                    // where the layout put it.
                    let placement = store.placement(tab.surface_id).map(|r| {
                        serde_json::json!({
                            "x": r.x0, "y": r.y0,
                            "w": r.width(), "h": r.height(),
                        })
                    });
                    // What the client actually drew, beside the tab it
                    // was drawn into. A window that opens and then
                    // resizes is those two disagreeing for a moment, so
                    // a harness watching an open needs both to say
                    // whether it was clean.
                    let buffer = store.frame_size(tab.surface_id).map(|(w, h)| {
                        serde_json::json!({ "w": w, "h": h })
                    });
                    serde_json::json!({
                        "surface_id": tab.surface_id,
                        "app_id": tab.app_id,
                        "frame_seq": store.frame_seq(tab.surface_id),
                        "last_error": store.last_error(tab.surface_id),
                        "rect": placement,
                        "buffer": buffer,
                        // Whether the shell is painting this window yet.
                        "presented": store.is_presented(tab.surface_id),
                        // And, while it is not, whether the card holding
                        // the window's place is up. Without this a harness
                        // cannot tell an empty desktop from one that is
                        // saying which app is on its way — which is the
                        // whole difference between a wait somebody can
                        // read and one they cannot.
                        "opening": store.is_opening(tab.surface_id),
                    })
                })
                .collect();
        serde_json::Value::Array(frames)
    }

    /// Grant on the active workspace, on behalf of the user sitting
    /// here, through `Workspace::grant_as`.
    ///
    /// `grant_as` is the authority check: it refuses a grantor who
    /// neither owns the room nor holds `Admin` over it. The refusal
    /// surfaces to the caller as an error — this op cannot widen
    /// anyone's access past what the person driving it already holds.
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
        let mut tree = chrome.tree_mut();
        let ws_id = tree
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
        // Through the tree's own entry point rather than `grant_as`
        // directly, so this answers with the same authority the rest of the
        // compositor grants under — including the room's write authority,
        // which `grant_as` alone does not see.
        let granted = tree.grant_in_workspace(ws_id, &user, cap).ok_or_else(|| {
            format!("{user} may not administer this room, so nothing was granted")
        })?;
        Ok(serde_json::json!({ "granted": granted, "grantor": user }))
    }

    /// Revoke on the active workspace through `Workspace::revoke_as` —
    /// the same authority check as a grant.
    fn revoke_capability(&self, id: &str) -> Result<serde_json::Value, String> {
        let chrome = self.fb.chrome();
        let user = chrome.local_user_id.clone();
        let mut tree = chrome.tree_mut();
        let ws_id = tree
            .active_workspace()
            .map(|w| w.id)
            .ok_or_else(|| "no active workspace".to_string())?;
        let ws = tree
            .workspaces
            .iter_mut()
            .find(|w| w.id == ws_id)
            .ok_or_else(|| "no active workspace".to_string())?;
        // Distinguish the two failures. `revoke_as` returns false both
        // for "you may not" and for "no such capability", and a harness
        // that read one as the other would call an unauthorised revoke
        // a successful no-op.
        let may = ws.may_administer(&user);
        if !may {
            return Err(format!(
                "{user} may not administer this room, so nothing was revoked"
            ));
        }
        let removed = ws.revoke_as(&user, id);
        if removed {
            tree.doc_revoke_capability(ws_id, id);
            tree.flush_to_disk();
        }
        Ok(serde_json::json!({ "revoked": removed, "revoker": user, "id": id }))
    }

    /// Answer the in-app action gate for the live workspace.
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
        let tree = chrome.tree();
        let ws = tree
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

    /// Enumerate the exported action surface through the *same*
    /// provider registry that feeds the Ctrl+K palette.
    ///
    /// The palette renders these providers as rows; this renders
    /// nothing. No scene is built, no widget is touched, no synthetic
    /// input is generated — which is the entire claim: an action is not
    /// a pixel, so driving the shell does not require seeing it.
    fn enumerate_actions(&self, query: &str) -> Result<serde_json::Value, String> {
        let (doc, root) = self
            .doc
            .ok_or_else(|| "control state has no live document".to_string())?;
        let chrome_handle = self.fb.chrome_handle();
        // The borrow lives only for the closure. `ActionCatalog` owns its
        // results outright (no borrow of `doc` is retained), so the
        // catalogue outlives the guards it was built through.
        let catalog = with_action_gate(&chrome_handle, |chrome, gate| {
            self.fb.host().action_catalog(query, chrome, doc, root, gate)
        })?;
        let actions: Vec<serde_json::Value> = catalog
            .enumerate()
            .into_iter()
            .map(|a| {
                serde_json::json!({
                    "provider": a.resource.provider,
                    "id": a.resource.id,
                    "kind": a.kind.label(),
                    "title": a.title,
                    "subtitle": a.subtitle,
                    "score": a.score,
                    // What to pass. Empty for the nullary majority, and
                    // still inert either way: a parameter list describes
                    // an invocation, it is not one, so enumeration stays
                    // something a caller can do freely.
                    "params": params_json(&a.params),
                })
            })
            .collect();
        Ok(serde_json::json!({
            "query": query,
            "count": actions.len(),
            "actions": actions,
        }))
    }

    /// Resolve one `(provider, id)` address against a freshly built
    /// catalogue, reporting what it names without running it.
    ///
    /// Selection and invocation are deliberately separate steps: a
    /// caller can inspect what an address resolves to before committing
    /// to the side-effect, and an address that is unknown — or hidden by
    /// the visibility gate — fails here, loudly, instead of becoming an
    /// invocation that does nothing.
    fn resolve_action(
        &self,
        provider: &str,
        id: &str,
        args: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value, test_control::ActionRefusal> {
        let (doc, root) = self
            .doc
            .ok_or_else(|| test_control::ActionRefusal::unknown("control state has no live document"))?;
        let chrome_handle = self.fb.chrome_handle();
        let catalog = with_action_gate(&chrome_handle, |chrome, gate| {
            self.fb.host().action_catalog("", chrome, doc, root, gate)
        })
        .map_err(test_control::ActionRefusal::unknown)?;
        let resource = ResourceRef::new(provider, id);
        let selected = catalog.select(&resource).ok_or_else(|| {
            test_control::ActionRefusal::unknown(format!(
                "no visible action addressed by ({provider}, {id}) in the current catalogue"
            ))
        })?;
        // Bind here, before anything is planned. An argument set that
        // does not fit is refused with the document untouched, and the
        // refusal names the parameter — which is the only thing a caller
        // with no screen has to correct against.
        let values = test_control::arg_values_from_json(args)?;
        ActionArgs::bind(selected.params(), values)
            .map_err(|e| test_control::ActionRefusal::invalid_arguments(e.to_string()))?;
        Ok(serde_json::json!({
            "provider": provider,
            "id": id,
            "kind": selected.kind().label(),
            "title": selected.title(),
            "params": params_json(selected.params()),
        }))
    }

    /// Build a structured view of [`ChromeState`] for the
    /// `state_dump` response. We construct the object manually rather
    /// than `derive(Serialize)` on `ChromeState` because the chrome
    /// owns `Rc<RefCell<…>>` handles (`Chrome`) into widgets that
    /// can't sensibly serialise; hand-rolling the object also lets us
    /// pick a wire-friendly representation (enum-as-string) for
    /// fields like `search_scope` and `ws_settings_tab` without
    /// shoving `#[serde(rename_all)]` attributes deep into the
    /// chrome's dependency graph.
    fn chrome_state_json(&self) -> serde_json::Value {
        let chrome = self.fb.chrome();
        let overlays = &chrome.overlays;
        serde_json::json!({
            "overlays": {
                "overview": overlays.overview,
                "quick_settings": overlays.quick_settings,
                "ws_settings": overlays.ws_settings,
                "collaborators": overlays.collaborators,
                "room_activity": overlays.room_activity,
            },
            "search_scope": format!("{:?}", chrome.search_scope),
            "ws_settings_tab": format!("{:?}", chrome.ws_settings_tab),
            "revision": chrome.revision,
            // What is actually on the screen. Without this a harness can only
            // observe that *a* frame was painted, so "the session told the
            // person their state file was not opened" could not be checked at
            // all — and an assertion that cannot see its subject is the thing
            // this project keeps getting caught by. The dump claims to be a
            // structured view of the chrome; notifications are part of it.
            "notifications": chrome.notifications().iter().map(|n| serde_json::json!({
                "title": n.title,
                "body": n.body,
                "severity": format!("{:?}", n.severity),
                "persistent": n.dismiss_after >= crate::shell::notification::PERSISTENT_DISMISS,
            })).collect::<Vec<_>>(),
            // The hosted file manager's drag, if one is in flight, and
            // whether the compositor has taken it over to a client.
            "files_drag": file_explorer::external_drag::unclaimed().map(|d| d.paths),
            "files_drag_handed_off": file_explorer::external_drag::is_handed_off(),
            "editing_workspace_name": chrome.editing_workspace_name,
            "pending_grants_count": chrome.pending_grants.len(),
            // Mirrored from the compositor's SessionManager each tick; lets a
            // headless test observe lock/unlock transitions over the wire.
            "session_locked": chrome.session_locked(),
            // Who this session is, and whether they may administer the
            // room they are looking at. Both are read straight off the
            // production helpers — a harness must not have to guess
            // either from the workspace's owner field.
            "local_user_id": chrome.local_user_id,
            "may_administer_active": chrome
                .tree()
                .active_workspace()
                .map(|ws| ws.may_administer(&chrome.local_user_id))
                .unwrap_or(false),
            // The peer-sync card's contents. The invite token is on
            // screen for the user to read; exposing it here is the same
            // fact in a form a script can act on.
            "peer_sync": {
                "invite_token": chrome.peer_sync_invite_token,
                "connect_input": chrome.peer_sync_connect_input,
                "status": chrome.peer_sync_status,
                // The invite composer: what has been typed into "who
                // are you inviting?", and who the token on screen was
                // minted for. A script drives the same two facts the
                // card shows.
                "invite_name": chrome.peer_sync_invite_name,
                "invite_for": chrome.peer_sync_invite_for,
            },
            // Whether apps in this session can share edits at all, and
            // why not when they cannot. Exposed because "collaboration
            // is silently off" is precisely the state that used to be
            // indistinguishable from "collaboration is on and idle" —
            // a harness that cannot tell those apart cannot prove
            // either one.
            "collab": {
                "available": chrome.collab_available(),
                "unavailable_reason": chrome.collab_unavailable,
                "socket": launcher::services::collab_socket().display().to_string(),
            },
            // What the Permissions tab's grant composer is composing.
            // A harness driving the pickers by click has no other way to
            // see which subject or capability it has landed on, and
            // guessing from click counts is how a driver ends up
            // asserting against its own arithmetic.
            "ws_grant_draft": {
                "subject": chrome
                    .tree()
                    .active_workspace()
                    .and_then(|ws| {
                        crate::shell::workspace_settings::draft_subject(
                            ws,
                            &chrome.ws_grant_draft,
                        )
                    })
                    .map(|s| format!("{:?}:{}", s.kind, s.id)),
                "capability": chrome.ws_grant_draft.choice().label(),
                "expiry": format!("{:?}", chrome.ws_grant_draft.expiry),
            },
            "palette": palette_json(self.fb),
        })
    }
}

/// The command palette's live state, for `state_dump`.
///
/// Which row Enter would activate is the whole content of "what does
/// this keystroke do next", and a socket-driven harness cannot see it
/// any other way.
fn palette_json(fb: &FrameBuilder) -> serde_json::Value {
    let (visible, query, selected, _) = fb.host().palette_snapshot();
    serde_json::json!({
        "visible": visible,
        "query": query,
        "selected_id": selected,
        // Label, subtitle *and* badge. A row for an app this machine
        // does not have differs from one for an app it does across all
        // three — the verb, the subtitle's tail, and the chip — so a
        // harness asserting on how an app is offered has to be able to
        // read every one of them.
        "rows": fb
            .host()
            .palette_rows_detailed()
            .into_iter()
            .map(|(id, label, description, badge)| serde_json::json!({
                "id": id,
                "label": label,
                "description": description,
                "badge": badge,
            }))
            .collect::<Vec<_>>(),
    })
}

/// Stash of per-process state the test-control wire needs in order to
/// execute [`PlannedAction`]s. Mirrors the equivalent fields on
/// `ShellDriver` (`bin/shots/driver.rs`): modifier snapshot, the
/// auxiliary `AutoCommit` doc widgets mutate, the root `ObjId`, and
/// the focus cursor. The compositor doesn't drive any of these from
/// real input today (input wiring is Phase 6), so this is a clean
/// scratch context used only by the test-control surface.
struct TestControlCtx {
    modifiers: Modifiers,
    doc: AutoCommit,
    root_obj: ObjId,
    focus: Option<NodeId>,
    cursor: PhysicalPosition<f64>,
}

impl TestControlCtx {
    fn new() -> Self {
        Self {
            modifiers: Modifiers::default(),
            doc: AutoCommit::new(),
            root_obj: automerge::ROOT,
            focus: None,
            cursor: PhysicalPosition::new(0.0, 0.0),
        }
    }
}

/// Translate a [`KeyTag`] from the wire to a `winit::keyboard::Key`
/// the host event handlers actually match on. Named keys map 1:1 to
/// `NamedKey` variants; chars wrap in `Key::Character`.
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
            NamedKeyTag::Space => NamedKey::Space,
            NamedKeyTag::Shift => NamedKey::Shift,
            NamedKeyTag::Control => NamedKey::Control,
            NamedKeyTag::Alt => NamedKey::Alt,
            NamedKeyTag::Super => NamedKey::Super,
        }),
        KeyTag::Char(c) => Key::Character(SmolStr::new(c.to_string())),
    }
}

/// Build a `WindowEvent::KeyboardInput` for `key` with the given
/// element state. Mirrors `shots/driver.rs::key_press_event`: winit's
/// `KeyEvent` has a `pub(crate)` `platform_specific` field, so we
/// zero-init the struct and overwrite the fields the host actually
/// reads. The `text` field is populated for `Character` keys so the
/// palette + rename text-input branches see the same glyph they would
/// on real hardware.
fn keyboard_input_event(key: Key, pressed: bool) -> WindowEvent {
    let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = key.clone();
    ev.physical_key = PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
    ev.location = KeyLocation::Standard;
    ev.state = if pressed {
        ElementState::Pressed
    } else {
        ElementState::Released
    };
    ev.repeat = false;
    ev.text = if let Key::Character(s) = &key {
        Some(s.clone())
    } else {
        None
    };
    WindowEvent::KeyboardInput {
        device_id: unsafe { std::mem::zeroed() },
        event: ev,
        is_synthetic: false,
    }
}

/// Apply a [`SetModifiers`] action by overwriting `ctx.modifiers`. The
/// host reads the snapshot inline on the next chord event, so we
/// don't need to dispatch a `WindowEvent::ModifiersChanged` to refresh
/// any per-event state. Mirrors `ShellDriver::set_modifiers`.
fn apply_modifier_mask(ctx: &mut TestControlCtx, ctrl: bool, shift: bool, alt: bool, logo: bool) {
    let mut mask = ModifiersState::empty();
    if ctrl {
        mask |= ModifiersState::CONTROL;
    }
    if shift {
        mask |= ModifiersState::SHIFT;
    }
    if alt {
        mask |= ModifiersState::ALT;
    }
    if logo {
        mask |= ModifiersState::SUPER;
    }
    ctx.modifiers = Modifiers::from(mask);
}

/// Synthesise the [`WindowEvent`] that corresponds to a single
/// [`PlannedAction`] and dispatch it through the FrameBuilder's
/// shared `dispatch_event` wire — the same wire `ShellDriver::dispatch`
/// uses for in-process scenarios. `device_id` is zeroed because
/// winit's `DeviceId` is opaque and the host's input path doesn't
/// inspect it.
///
/// `state` is the live compositor state when one exists (the run loop
/// always passes it; the in-process unit tests below pass `None`). When
/// present, the synthesised event is offered to
/// [`client_input::route_input`] first — exactly like the live
/// `BackendEvent::Input` arm — so a headless harness drives foreign client
/// surfaces through the same production routing the winit / DRM backends
/// use, instead of a parallel path that could pass while the real one is
/// broken.
fn execute_planned_action(
    action: &PlannedAction,
    fb: &mut FrameBuilder,
    ctx: &mut TestControlCtx,
    state: Option<&mut State>,
) -> Option<InvokeFailure> {
    /// Offer `event` to the focused Wayland client; returns `true` when it
    /// was consumed and must not also reach the chrome.
    fn to_client(
        state: &mut Option<&mut State>,
        fb: &mut FrameBuilder,
        event: &WindowEvent,
        modifiers: &Modifiers,
    ) -> bool {
        match state {
            Some(st) => client_input::route_input(st, fb, event, modifiers),
            None => false,
        }
    }

    let mut state = state;
    match action {
        PlannedAction::CursorMoved { x, y } => {
            ctx.cursor = PhysicalPosition::new(*x, *y);
            let event = WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed() },
                position: ctx.cursor,
            };
            if to_client(&mut state, fb, &event, &ctx.modifiers) {
                return None;
            }
            fb.dispatch_event(
                &event,
                &ctx.modifiers,
                &mut ctx.doc,
                &ctx.root_obj,
                &mut ctx.focus,
            );
        }
        PlannedAction::SetGroupWidth { permille } => {
            let touched = fb.host_mut().set_group_widths(*permille);
            tracing::info!("test_control: set {} group(s) to {permille}permille", touched.len());
            fb.with_chrome(|st| st.bump());
        }
        PlannedAction::MergeTabIntoGroup {
            surface_id,
            into_surface_id,
        } => {
            // Surfaces in, tab and group ids out. A caller watching the
            // strip through `foreign_frames` has surface ids and nothing
            // else — the tab id a window was adopted into is not on that
            // wire — so the translation belongs here, beside the tree
            // that owns both vocabularies.
            let resolved = {
                let chrome = fb.chrome();
                let tree = chrome.tree();
                let tabs = crate::compositor::foreign::foreign_tabs(&tree);
                let moving = tabs.iter().find(|t| t.surface_id == *surface_id);
                let target = tabs.iter().find(|t| t.surface_id == *into_surface_id);
                match (moving, target) {
                    (Some(m), Some(t)) => crate::compositor::foreign::group_of_tab(&tree, t.tab_id)
                        .map(|gid| (m.tab_id, gid))
                        .ok_or_else(|| {
                            format!("surface {into_surface_id} is in no window group")
                        }),
                    (None, _) => Err(format!("no window on screen is surface {surface_id}")),
                    (_, None) => Err(format!(
                        "no window on screen is surface {into_surface_id}"
                    )),
                }
            };
            match resolved {
                Ok((tab_id, dest_gid)) => match fb.host_mut().merge_tab_into_group(tab_id, dest_gid)
                {
                    Ok(count) => {
                        tracing::info!(
                            "test_control: surface {surface_id} joined the group holding                              surface {into_surface_id}, which now has {count} tab(s)"
                        );
                    }
                    Err(why) => {
                        return Some(InvokeFailure {
                            code: "merge_refused".into(),
                            message: why,
                        });
                    }
                },
                Err(why) => {
                    return Some(InvokeFailure {
                        code: "unknown_surface".into(),
                        message: why,
                    });
                }
            }
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
            if to_client(&mut state, fb, &event, &ctx.modifiers) {
                return None;
            }
            fb.dispatch_event(
                &event,
                &ctx.modifiers,
                &mut ctx.doc,
                &ctx.root_obj,
                &mut ctx.focus,
            );
        }
        PlannedAction::MousePointerButton { pressed, button } => {
            use crate::compositor::test_control::PointerButtonKind;
            let event = WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: if *pressed {
                    ElementState::Pressed
                } else {
                    ElementState::Released
                },
                button: match button {
                    PointerButtonKind::Right => MouseButton::Right,
                    PointerButtonKind::Middle => MouseButton::Middle,
                    PointerButtonKind::Left => MouseButton::Left,
                },
            };
            // Same two-step delivery the left button takes: offer it to a
            // focused client first, and only dispatch into the chrome's own
            // widget tree when no client took it. A context menu opened by
            // an app belongs to the app.
            if to_client(&mut state, fb, &event, &ctx.modifiers) {
                return None;
            }
            fb.dispatch_event(
                &event,
                &ctx.modifiers,
                &mut ctx.doc,
                &ctx.root_obj,
                &mut ctx.focus,
            );
        }
        PlannedAction::KeyboardKey { key, pressed } => {
            let winit_key = keytag_into_winit_key(*key);
            let event = keyboard_input_event(winit_key, *pressed);
            // Keep the running mask in step with the injected key stream:
            // this wire carries no `ModifiersChanged`, so a held modifier
            // would otherwise be invisible to both the chord check and the
            // chrome's own chord handling.
            if let WindowEvent::KeyboardInput { event: key_ev, .. } = &event {
                client_input::apply_modifier_key(&mut ctx.modifiers, key_ev);
            }
            if to_client(&mut state, fb, &event, &ctx.modifiers) {
                return None;
            }
            fb.dispatch_event(
                &event,
                &ctx.modifiers,
                &mut ctx.doc,
                &ctx.root_obj,
                &mut ctx.focus,
            );
        }
        PlannedAction::MouseWheel { dx, dy } => {
            let event = WindowEvent::MouseWheel {
                device_id: unsafe { std::mem::zeroed() },
                delta: winit::event::MouseScrollDelta::LineDelta(*dx, *dy),
                phase: winit::event::TouchPhase::Moved,
            };
            if to_client(&mut state, fb, &event, &ctx.modifiers) {
                return None;
            }
            fb.dispatch_event(
                &event,
                &ctx.modifiers,
                &mut ctx.doc,
                &ctx.root_obj,
                &mut ctx.focus,
            );
        }
        PlannedAction::SetModifiers {
            ctrl,
            shift,
            alt,
            logo,
        } => {
            // Mutate the executor's modifier snapshot directly. The
            // host reads `ctx.modifiers` on the next chord dispatch,
            // so no event needs to flow through `dispatch_event`.
            apply_modifier_mask(ctx, *ctrl, *shift, *alt, *logo);
        }
        PlannedAction::DrainSnapshots => {
            // Only meaningful if something deferred; `get` rather than
            // `get_or_init` so a drain never builds a pool just to find
            // it empty.
            if let Some(encoder) = SNAPSHOT_ENCODER.get() {
                if !encoder.drain() {
                    eprintln!(
                        "nirios compositor: a deferred snapshot failed to write; \
                         the frame directory is incomplete"
                    );
                }
            }
        }
        PlannedAction::Snapshot { path, defer } => {
            // Re-paint the live scene into a fresh OffscreenRenderer
            // sized to the FrameBuilder's current viewport. We use
            // option 2 from the spec (re-render via OffscreenRenderer)
            // rather than tap a backend-cached `last_frame` so the
            // command works for every backend (winit / drm / future
            // headless), not just the headless one.
            //
            // Errors during creation, encoding, or write are logged
            // via `eprintln!` and swallowed — the dispatcher already
            // returned `Ok` from the wire's perspective, and crashing
            // the compositor on a stray bad path would defeat the
            // point of having a test surface. Phase 5 will route
            // these into structured wire responses.
            // A snapshot has to show what the screen shows, and during a
            // drag that includes the thing being carried — which the
            // live present loop draws on top of the painted scene. A
            // capture taken through this path used to miss it, so a
            // drag photographed mid-gesture came out as two windows and
            // no drag.
            //
            // The pointer goes in for the same reason and always, not
            // only during a drag. This path re-renders the scene into
            // an offscreen buffer, so whatever cursor plane the backend
            // owns is by definition not in the readback: a photograph
            // taken here shows a desktop with no pointer on it, and a
            // scripted click therefore looks like the machine clicking
            // itself. Drawn from `pointer_position`, so a session whose
            // pointer has never been anywhere still gets none.
            let pointer = state.as_ref().and_then(|s| {
                let at = s.client_input.pointer_position()?;
                let dragging = s.selection.dnd_active || s.selection.chrome_drag.is_some();
                Some(PointerPaint {
                    at: vello::kurbo::Point::new(at.0, at.1),
                    label: dragging.then(|| s.selection.dnd_label.get()).flatten(),
                    pressed: s.client_input.buttons_down() > 0,
                })
            });
            write_snapshot_png(fb, path, pointer, *defer);
        }
        PlannedAction::InvokeAction { provider, id, args } => {
            // Re-resolve rather than carrying a handle over from
            // dispatch: the document is mutable between planning and
            // execution, so the catalogue the caller was shown may no
            // longer be the catalogue that exists now. Resolving here
            // means we either run the action the address currently
            // names, or nothing at all.
            let chrome_handle = fb.chrome_handle();
            // Built through the same gate the dispatcher resolved
            // against. Re-resolving without it would make the gate
            // advisory: a caller who got past `resolve_action` once
            // could keep invoking after their capability was revoked.
            let catalog = match with_action_gate(&chrome_handle, |chrome, gate| {
                fb.host()
                    .action_catalog("", chrome, &ctx.doc, &ctx.root_obj, gate)
            }) {
                Ok(catalog) => catalog,
                Err(why) => {
                    eprintln!("nirios compositor: cannot invoke ({provider}, {id}): {why}");
                    return None;
                }
            };
            match catalog.select(&ResourceRef::new(provider.as_str(), id.as_str())) {
                // The `Ref` above is dropped with the block, so the
                // provider's callback is free to borrow the chrome cell
                // mutably — which most of them do.
                Some(selected) => {
                    // Re-bound against *this* resolution's parameters.
                    // The catalogue was rebuilt a moment ago and can have
                    // landed on a different action under the same
                    // address; arguments checked against the previous one
                    // would be an assurance about something else.
                    let bound = test_control::arg_values_from_json(args)
                        .map_err(|r| InvokeFailure {
                            code: r.code.to_string(),
                            message: r.message,
                        })
                        .and_then(|values| {
                            selected
                                .invoke(&mut ctx.doc, &ctx.root_obj, &chrome_handle, values)
                                .map_err(|e| InvokeFailure {
                                    code: e.code().to_string(),
                                    message: e.to_string(),
                                })
                        });
                    if let Err(failure) = bound {
                        // Logged for whoever reads the journal, and
                        // returned so the socket's reply can carry it.
                        // Planned actions run before the response is
                        // written, which is what makes an honest answer
                        // possible here: "it ran" and "it could not run"
                        // must not look alike to a caller that cannot see
                        // the screen.
                        eprintln!(
                            "nirios compositor: ({provider}, {id}) did not run: {}",
                            failure.message
                        );
                        return Some(failure);
                    }
                }
                None => {
                    let message = format!(
                        "action ({provider}, {id}) vanished between resolve and invoke; \
                         nothing was run"
                    );
                    eprintln!("nirios compositor: {message}");
                    return Some(InvokeFailure {
                        code: "action_failed".to_string(),
                        message,
                    });
                }
            }
        }
    }
    None
}

/// Why an invocation that was planned did not happen.
///
/// Carries the action layer's own code so the socket's reply says the
/// same thing the palette would: `invalid_arguments` when the arguments
/// did not fit, `action_failed` when the action ran and refused.
struct InvokeFailure {
    code: String,
    message: String,
}

/// Route a test-control [`PlannedAction`] to the lock screen while the session
/// is locked, instead of to the app/chrome. The test-control socket is a real
/// input source, so — exactly like live winit input — it must be captured by
/// the lock surface while locked (never delivered to hidden apps). Keyboard
/// actions feed the password field via the same winit→`LockKey` routing the
/// live backend uses; pointer/modifier actions and snapshots are swallowed (a
/// snapshot must not capture and leak the hidden chrome). A submit (Enter)
/// starts an asynchronous verify; the actual unlock is delivered by the run
/// loop's per-tick `lock.poll_verification()`, not here.
fn execute_locked_action(action: &PlannedAction, lock: &mut LockController) {
    match action {
        PlannedAction::KeyboardKey { key, pressed } => {
            let winit_key = keytag_into_winit_key(*key);
            let event = keyboard_input_event(winit_key, *pressed);
            lock.handle_key(winit_event_to_lock_key(&event));
        }
        // Pointer moves/clicks, modifier sets, snapshots, and action
        // invocations: swallowed while locked. Input goes only to the lock
        // surface.
        //
        // `InvokeAction` belongs in this list for the same reason a
        // click does, and more sharply: the action API reaches chrome
        // side-effects without needing a visible surface, so letting one
        // through here would make the lock screen bypassable by a socket
        // client that never has to render anything.
        PlannedAction::CursorMoved { .. }
        | PlannedAction::MouseLeftButton { .. }
        | PlannedAction::MousePointerButton { .. }
        | PlannedAction::MouseWheel { .. }
        | PlannedAction::SetModifiers { .. }
        | PlannedAction::Snapshot { .. }
        | PlannedAction::SetGroupWidth { .. }
        | PlannedAction::MergeTabIntoGroup { .. }
        | PlannedAction::InvokeAction { .. } => {}
        // Not swallowed, because it is not an input action and shows
        // nothing: it waits for frames captured *before* the lock to
        // finish being written, and answers only whether they did. The
        // lock already stops new ones being taken, so letting the
        // barrier through reveals no hidden chrome — while swallowing it
        // would hand a caller a completed drain over files still being
        // written.
        PlannedAction::DrainSnapshots => {
            if let Some(encoder) = SNAPSHOT_ENCODER.get() {
                encoder.drain();
            }
        }
    }
}

/// The snapshot renderer, cached for the lifetime of the process.
///
/// **Process-wide, and deliberately never dropped.** This was a
/// `thread_local!`, and that aborted the whole `shell --lib` binary —
/// all ~1800 tests, not just the two that take snapshots. Dropping a
/// wgpu resource calls `wgpu_core::snatch::SnatchLock::read`, which
/// under `debug_assertions` reads wgpu's own `SNATCH_LOCK_TRACE`
/// thread-local. TLS destructors run in reverse registration order, and
/// this cache registers first (it is touched on the way in, before any
/// wgpu call), so at thread exit wgpu's trace TLS is destroyed first and
/// ours then drops a vello `Renderer` against the corpse. `LocalKey`
/// answers "cannot access a Thread Local Storage value during or after
/// destruction", and a panic inside a TLS destructor is not a test
/// failure — it is `abort_on_dtor_unwind`, i.e. SIGABRT, which takes
/// every remaining result down with it and leaves no `test result:`
/// line at all.
///
/// A `static` has no destructor, so the renderer outlives every thread
/// and is never dropped. The leak is the point: this is a
/// process-lifetime cache whose GPU allocations the kernel reclaims at
/// exit, and a viewport resize still drops the previous renderer the
/// ordinary way — mid-execution, where wgpu's TLS is alive.
///
/// In a shipped build nothing reaches here at all: the socket that leads
/// here is compiled out (`compositor::dev_env`), so the cached texture
/// and buffer are never allocated in production.
static SNAPSHOT_RENDERER: std::sync::OnceLock<
    std::sync::Mutex<Option<ui::offscreen::OffscreenRenderer>>,
> = std::sync::OnceLock::new();

/// How many `OffscreenRenderer`s [`write_snapshot_png`] has built in
/// this process: one per distinct viewport size, *not* one per thread.
/// That is what `snapshot_renderer_outlives_the_thread_that_built_it`
/// asserts on, because the abort above cannot be observed from inside
/// the binary it kills.
static SNAPSHOT_RENDERERS_BUILT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Threads that PNG-encode deferred snapshots, built on first use.
///
/// Lazy on purpose: a compositor nobody is filming never asks for a
/// deferred snapshot, so it never pays for the threads. Several sessions
/// run at once on this machine and a pool spawned per compositor at
/// startup would be three pools sitting idle.
///
/// Process-wide for the same reason [`SNAPSHOT_RENDERER`] is — see
/// `snapshot_encode` for what the pool does and why encoding does not
/// belong on the event loop.
static SNAPSHOT_ENCODER: std::sync::OnceLock<snapshot_encode::SnapshotEncoder> =
    std::sync::OnceLock::new();

fn snapshot_encoder() -> &'static snapshot_encode::SnapshotEncoder {
    SNAPSHOT_ENCODER.get_or_init(snapshot_encode::SnapshotEncoder::new)
}

/// What the pointer looks like this frame: where it is, whether its
/// button is down, and what it is carrying if anything.
///
/// One value rather than three arguments because the three are only
/// ever meaningful together — there is nothing to say about a press or
/// a carried label without a position to hang them off.
#[derive(Debug, Clone)]
struct PointerPaint {
    at: vello::kurbo::Point,
    label: Option<String>,
    pressed: bool,
}

/// Re-paint the current scene into a cached `OffscreenRenderer` and
/// encode the resulting RGBA buffer as a PNG at `path`. Best-effort:
/// a bad viewport, a missing GPU adapter, and every I/O and encoder
/// failure log to stderr and return, so an executor invocation does not
/// take the calloop pump down with it.
///
/// Split out of `execute_planned_action` so unit tests can exercise
/// the snapshot path against a hand-built `FrameBuilder` without going
/// through `PlannedAction` dispatch.
/// Capture the live scene and put it on disk as a PNG.
///
/// `defer` splits the work in two. The capture — paint, render, read the
/// pixels back — has to happen here, on the loop that owns the widget
/// tree and the renderer. The encode does not: it is `deflate` over a
/// flat buffer and needs nothing from the compositor.
///
/// With `defer` false the encode happens inline and the file is on disk
/// when this returns, which is the contract every existing harness was
/// written against. With it true the buffer goes to
/// [`snapshot_encode::SnapshotEncoder`] and the loop is free again after
/// the readback — measured at 1920x1014, that is ~6.5 ms of loop time
/// instead of ~31 ms. The caller then owes a `drain_snapshots` before it
/// treats the directory as complete.
fn write_snapshot_png(
    fb: &mut FrameBuilder,
    path: &std::path::Path,
    pointer: Option<PointerPaint>,
    defer: bool,
) {
    let (w, h) = fb.viewport();
    let (w, h) = (w as u32, h as u32);
    if w == 0 || h == 0 {
        eprintln!(
            "nirios compositor: test_control snapshot: viewport is {w}x{h}, nothing to write"
        );
        return;
    }
    let t_begin = std::time::Instant::now();
    let mut scene = fb.paint();
    if let Some(p) = pointer {
        let theme = fb.theme().clone();
        drag_feedback::draw(
            &mut scene,
            p.at,
            p.label.as_deref(),
            p.pressed,
            &theme,
            fb.text_renderer_mut(),
        );
    }
    let scene = scene;
    let t_painted = std::time::Instant::now();
    // The renderer is kept between snapshots and rebuilt only when the
    // viewport changes size.
    //
    // `OffscreenRenderer::try_new` constructs a fresh vello `Renderer`,
    // which initialises every one of vello's shader pipelines — so
    // building one per call put a full pipeline build inside every
    // snapshot. Measured at 1920x1080 that was ~450 ms a frame against
    // ~70 ms of actual render and encode, i.e. the overwhelming
    // majority of a capture. `OffscreenRenderer`'s own documentation
    // says to reuse an instance to amortise exactly this; this is the
    // snapshot path finally doing so, and it is what makes a recording
    // driven over this socket watchable rather than a slideshow.
    //
    // See [`SNAPSHOT_RENDERER`] for why the cache is a process-wide
    // `static` and not a `thread_local!`.

    // `try_new` rather than `new` inside a `catch_unwind`: the shipping
    // release profile sets `panic = "abort"`, so a caught panic is not
    // a thing that happens — the compositor would die at the panic site
    // instead of logging and carrying on. Asking the renderer for a
    // `Result` is what actually keeps it alive on a machine with no
    // wgpu adapter (e.g. CI without llvmpipe). A failure is not cached:
    // the slot is left empty so a later snapshot retries rather than
    // inheriting one bad moment forever.
    let cache = SNAPSHOT_RENDERER.get_or_init(|| std::sync::Mutex::new(None));
    // A panic elsewhere in a snapshot poisons the mutex without damaging
    // what it guards — the slot is either an intact renderer or empty —
    // so recover rather than turn one failure into a second one.
    let mut slot = cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let stale = match slot.as_ref() {
        Some(renderer) => renderer.width != w || renderer.height != h,
        None => true,
    };
    if stale {
        match ui::offscreen::OffscreenRenderer::try_new(w, h) {
            Ok(renderer) => {
                SNAPSHOT_RENDERERS_BUILT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                // Assigning drops any previous renderer here, in the
                // middle of execution — the one place it is safe to.
                *slot = Some(renderer);
            }
            Err(e) => {
                eprintln!("nirios compositor: test_control snapshot: no offscreen renderer: {e}");
                *slot = None;
                return;
            }
        }
    }
    let Some(rgba) = slot.as_mut().map(|renderer| renderer.render_rgba(&scene)) else {
        return;
    };
    // Hand the renderer back before encoding: the PNG write touches no
    // GPU state, and holding the lock across it would serialise
    // concurrent snapshots on file I/O for nothing.
    drop(slot);
    let t_rendered = std::time::Instant::now();
    if defer {
        snapshot_encoder().submit(rgba, w, h, path.to_path_buf());
    } else if let Err(e) = snapshot_encode::encode_png(&rgba, w, h, path) {
        eprintln!("nirios compositor: test_control snapshot: {path:?}: {e}");
    }
    // Where a snapshot's time went, per capture (`QOS_SNAPSHOT_TIMING=1`).
    //
    // This is how the split above was established rather than guessed:
    // the frame rate of a recording is the snapshot's cost, and without
    // a per-phase number a slow take is indistinguishable between a slow
    // renderer, a slow encode and a busy machine. Free when unset.
    if std::env::var_os("QOS_SNAPSHOT_TIMING").is_some() {
        let t_end = std::time::Instant::now();
        eprintln!(
            "snapshot-timing paint={:.1}ms render={:.1}ms encode={:.1}ms total={:.1}ms {}x{} deferred={}",
            (t_painted - t_begin).as_secs_f64() * 1e3,
            (t_rendered - t_painted).as_secs_f64() * 1e3,
            (t_end - t_rendered).as_secs_f64() * 1e3,
            (t_end - t_begin).as_secs_f64() * 1e3,
            w,
            h,
            defer,
        );
    }
}

/// Drain any pending bytes from `conn`'s buffer. For each complete
/// line (terminated by `\n`), parse it as a [`test_control::Command`],
/// dispatch against `frame_builder`, execute any `PlannedAction`s the
/// dispatcher returned, and write the JSON response back to the
/// connection. Partial lines stay buffered for the next tick.
///
/// On EOF / connection error / write failure, marks the connection
/// closed so the outer loop can drop it.
///
/// Phase 2b — `click_at` and `click_named` execute their planned
/// actions through `FrameBuilder::dispatch_event` (same wire as
/// `ShellDriver::dispatch`). Chrome-mutating widgets (overview
/// button, wheel-disc plus slot, …) react identically to real input.
/// D-10 fix: cap on the per-line size for the test-control protocol.
/// `BufRead::read_line` will buffer the entire line into the `String`
/// regardless of size; a malicious or buggy client could feed an
/// unbounded byte stream with no newline and drive the compositor
/// to OOM. 64 KiB is comfortably above any legitimate command
/// (`click_named` carries a widget id + a couple of floats — well
/// under a kilobyte) and below any meaningful memory pressure.
pub(crate) const TEST_CONTROL_MAX_LINE_BYTES: usize = 64 * 1024;

/// D-10: read up to one `\n`-terminated line from `reader`, capping at
/// `TEST_CONTROL_MAX_LINE_BYTES`. Returns:
///
/// * `Ok(Some(line))` — a complete `\n`-terminated line was read; the
///   trailing newline is included so the caller can keep using
///   `parse_command` unchanged.
/// * `Ok(None)` — EOF reached without seeing a newline (peer closed
///   cleanly mid-line).
/// * `Err(ReadLineError::TooLarge)` — the cap fired before a newline
///   was seen. Caller must close the connection.
/// * `Err(ReadLineError::Io(e))` — underlying I/O failure (incl.
///   `WouldBlock`, which the caller handles specially).
pub(crate) enum ReadLineError {
    TooLarge,
    Io(std::io::Error),
}

pub(crate) fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    out: &mut String,
    max_bytes: usize,
) -> Result<Option<()>, ReadLineError> {
    let mut buf = Vec::with_capacity(256);
    loop {
        let available = match reader.fill_buf() {
            Ok(b) => b,
            Err(e) => return Err(ReadLineError::Io(e)),
        };
        if available.is_empty() {
            // EOF. If we accumulated some bytes without a newline,
            // return them as a best-effort line (caller can decide
            // what to do); otherwise signal EOF via None.
            if buf.is_empty() {
                return Ok(None);
            }
            match String::from_utf8(buf) {
                Ok(s) => {
                    out.push_str(&s);
                    return Ok(Some(()));
                }
                Err(e) => {
                    return Err(ReadLineError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        e,
                    )));
                }
            }
        }
        // Find the newline (if any) in the available slice.
        let (consumed, found_nl) = match available.iter().position(|&b| b == b'\n') {
            Some(idx) => (idx + 1, true),
            None => (available.len(), false),
        };
        if buf.len() + consumed > max_bytes {
            // Cap reached. Consume up to the cap so we don't leave
            // garbage in the reader, then bail.
            let remaining = max_bytes.saturating_sub(buf.len());
            buf.extend_from_slice(&available[..remaining]);
            reader.consume(remaining);
            return Err(ReadLineError::TooLarge);
        }
        buf.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);
        if found_nl {
            match String::from_utf8(buf) {
                Ok(s) => {
                    out.push_str(&s);
                    return Ok(Some(()));
                }
                Err(e) => {
                    return Err(ReadLineError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        e,
                    )));
                }
            }
        }
    }
}

fn pump_test_control_conn(
    conn: &mut TestControlConn,
    frame_builder: &mut FrameBuilder,
    ctx: &mut TestControlCtx,
    session: &SessionManager,
    lock: &mut LockController,
    state: &mut State,
) -> bool {
    // Whether any command on this connection produced a `PlannedAction`
    // — i.e. synthesised input the next painted frame has to reflect.
    // The frame-loop instrumentation uses it to start an input-latency
    // measurement, so a socket-driven harness measures the same
    // input-to-present path a physical keystroke takes.
    let mut injected_input = false;
    loop {
        let mut line = String::new();
        match read_bounded_line(&mut conn.reader, &mut line, TEST_CONTROL_MAX_LINE_BYTES) {
            Ok(None) => {
                conn.closed = true;
                return injected_input;
            }
            Ok(Some(())) => {
                let (resp, actions) = match parse_command(&line) {
                    Ok(cmd) => {
                        // Settle layout before anything reads a rect.
                        //
                        // The relayout after an action (below) covers a
                        // tree the *socket* changed. It does not cover a
                        // tree a **client** changed: a window mapping,
                        // closing or resizing rearranges the strip
                        // without any command being sent, and until the
                        // next paint every widget still reports the rect
                        // it had before. A caller that asked where a
                        // window is between those two moments was told
                        // where it used to be — off-screen, or the
                        // position of the only tile there used to be.
                        //
                        // So layout is settled per command rather than
                        // per action. It is idempotent and costs far
                        // less than the round-trip that asked for it.
                        frame_builder.relayout();
                        // Read before the shadowing binding below takes
                        // the name: the click ops plan their approach
                        // from wherever the pointer is right now.
                        let pointer = state.client_input.pointer_position();
                        let state = FrameBuilderControlState {
                            fb: frame_builder,
                            doc: Some((&ctx.doc, &ctx.root_obj)),
                            pointer,
                        };
                        test_control::dispatch_command(&cmd, &state)
                    }
                    Err(err_resp) => (err_resp, Vec::new()),
                };
                // A planned action that could not run replaces this
                // command's answer. Actions execute before the reply is
                // written, so the reply can tell the truth about them —
                // and an invocation the compositor refused must not come
                // back looking like one that happened. First failure
                // wins; the rest still run, exactly as before.
                let mut failed: Option<InvokeFailure> = None;
                for action in &actions {
                    if session.is_locked() {
                        // While locked the test-control socket is captured by
                        // the lock surface, just like live input — actions go
                        // to the password field, never to the hidden chrome. A
                        // submit starts an async verify; the run loop's
                        // per-tick `poll_verification` delivers the unlock.
                        execute_locked_action(action, lock);
                    } else if let Some(failure) =
                        execute_planned_action(action, frame_builder, ctx, Some(state))
                    {
                        failed.get_or_insert(failure);
                    }
                }
                // Re-run layout so the next `list_targets` (or paint
                // tick) sees rects post-mutation. Chrome state
                // changes (overlay open, workspace add, …) can change
                // structural layout; without a relayout here the
                // next command sees stale rects from the previous
                // frame.
                // Snapshots and drains are readers: they plan an action
                // because that is how work reaches the executor, but they
                // change no widget, no chrome and no tree. Relaying out
                // after one re-does the layout this command already
                // settled on the way in, and calling it input starts a
                // latency clock for something nobody typed. During a
                // recording that is a second layout pass per frame, on
                // the loop, for a command whose whole purpose is to
                // observe without disturbing.
                if actions.iter().any(|a| {
                    !matches!(
                        a,
                        PlannedAction::Snapshot { .. } | PlannedAction::DrainSnapshots
                    )
                }) {
                    injected_input = true;
                    frame_builder.relayout();
                }
                let resp = match failed {
                    Some(failure) => test_control::Response::Err {
                        code: failure.code,
                        message: failure.message,
                    },
                    None => resp,
                };
                let mut out = serialise_response(&resp);
                out.push('\n');
                if let Err(e) = conn.reader.get_mut().write_all(out.as_bytes()) {
                    eprintln!("nirios compositor: test_control write: {e}");
                    conn.closed = true;
                    return injected_input;
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
                eprintln!("nirios compositor: test_control read: {e}");
                conn.closed = true;
                return injected_input;
            }
        }
    }
}

/// Track D2: turn a [`SessionTransition`] into a real backend effect.
///
/// `Blanked` / `Woke` drive the backend's DPMS (no-op on winit /
/// headless); `Locked` / `Unlocked` are pure state the paint path reads
/// via `SessionManager::is_locked`, so they need no backend call here.
/// When presentation has been split onto a render thread, the display
/// hardware went with it, so `pipe` is where the DPMS change has to land.
/// It is sent on the same channel as frames, so a blank can never
/// overtake a frame already queued — the panel cannot light up showing
/// content the session believes it suppressed. The backend call is kept
/// for the inline path (and for the parts of a split backend that stayed
/// behind).
fn apply_session_transition(
    transition: SessionTransition,
    backend: &mut dyn Backend,
    pipe: Option<&present::PresentPipe>,
) {
    match transition {
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
        // Lock/Unlock are reflected via `SessionManager::is_locked`;
        // Suspend is gated by the inhibitor registry + issued through the
        // power service at the call site, never here.
        SessionTransition::Locked | SessionTransition::Unlocked | SessionTransition::Suspend => {}
    }
}

/// Whether an event is the hardware power key being pressed. Detected by
/// both the logical [`NamedKey::Power`] and the physical key code so it works
/// regardless of the active keymap.
fn is_power_key(event: &WindowEvent) -> bool {
    if let WindowEvent::KeyboardInput { event, .. } = event {
        if event.state == ElementState::Pressed {
            if matches!(event.logical_key, Key::Named(NamedKey::Power)) {
                return true;
            }
            if let PhysicalKey::Code(code) = event.physical_key {
                return code == winit::keyboard::KeyCode::Power;
            }
        }
    }
    false
}

/// Build an OS notification toast from a power-service notification.
fn power_notification_to_incoming(
    bridge: &crate::notifications::NotificationBridge,
    n: qos_power_mgmt::PowerNotification,
) -> crate::notifications::IncomingNotification {
    crate::notifications::IncomingNotification {
        dbus_id: bridge.alloc_id(),
        app_name: "Power".to_string(),
        summary: n.summary,
        body: n.body,
        actions: Vec::new(),
        // Critical battery lingers until dismissed; the low heads-up uses the
        // server default expiry.
        expire_timeout: if n.critical { 0 } else { -1 },
        urgency: if n.critical { 2 } else { 1 },
        caller: None,
    }
}

/// Track D2: is `event` the Super+L lock chord (on key *press*)?
///
/// The modifier mask comes from the loop's running `Modifiers` snapshot
/// (the same one `dispatch_event` uses), not the key event, because
/// winit reports modifiers via a separate `ModifiersChanged` event.
/// The virtual terminal `Ctrl+Alt+F1..F12` asks for, or `None`.
///
/// On a bare TTY the compositor holds the keyboard in a raw mode, so the
/// kernel's own VT-switch handling never sees the chord. If the
/// compositor does not act on it the user cannot leave this terminal for
/// the life of the session — not to another VT, not back to a text
/// console.
///
/// Pure so the chord table is checked without a seat: it is the one part
/// of VT switching that can be.
fn vt_switch_chord(event: &WindowEvent, modifiers: &Modifiers) -> Option<i32> {
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

/// Is `event` the remote-input kill switch, `Super+Esc`, on key *press*?
///
/// The panic key for a shared session: whatever a guest is doing with the
/// host's seat, this stops it. Checked ahead of the client router and
/// ahead of the shell's own Escape handling — an overlay being open, or a
/// client having grabbed the keyboard, must not be able to swallow it.
fn is_remote_input_kill_switch(event: &WindowEvent, modifiers: &Modifiers) -> bool {
    let WindowEvent::KeyboardInput { event, .. } = event else {
        return false;
    };
    event.state == ElementState::Pressed
        && modifiers.state().super_key()
        && matches!(event.logical_key, Key::Named(NamedKey::Escape))
}

fn is_lock_chord(event: &WindowEvent, modifiers: &Modifiers) -> bool {
    if let WindowEvent::KeyboardInput { event, .. } = event {
        if event.state == ElementState::Pressed && modifiers.state().super_key() {
            if let Key::Character(s) = &event.logical_key {
                return s.eq_ignore_ascii_case("l");
            }
        }
    }
    false
}

/// Track D2: is `event` the explicit unlock action (Enter or Escape on
/// key *press*)?
///
/// This is the minimum "explicit unlock action" the D2 spec requires for
/// this pass; wiring real PAM authentication behind it is a documented
/// follow-up. Only key presses count so an auto-repeat release can't
/// race the transition.
/// Map a live winit key event to the lock screen's normalised [`LockKey`]
/// while the session is locked. Non-press events and unhandled keys become
/// [`LockKey::Ignored`] so they are still *swallowed* (never routed to apps
/// behind the overlay), but have no effect on the password buffer. This is
/// the winit half of "input goes only to the lock surface"; the test-control
/// path produces the same [`LockKey`]s so both drive one routing.
fn winit_event_to_lock_key(event: &WindowEvent) -> LockKey {
    let WindowEvent::KeyboardInput { event, .. } = event else {
        return LockKey::Ignored;
    };
    if event.state != ElementState::Pressed {
        return LockKey::Ignored;
    }
    match &event.logical_key {
        Key::Named(NamedKey::Enter) => LockKey::Submit,
        Key::Named(NamedKey::Escape) => LockKey::Cancel,
        Key::Named(NamedKey::Backspace) => LockKey::Backspace,
        Key::Named(NamedKey::Space) => LockKey::Text(" ".to_string()),
        Key::Character(s) => LockKey::Text(s.to_string()),
        _ => {
            // Some layouts deliver typed text on otherwise-Named keys.
            match &event.text {
                Some(t) if !t.is_empty() && !t.chars().all(|c| c.is_control()) => {
                    LockKey::Text(t.to_string())
                }
                _ => LockKey::Ignored,
            }
        }
    }
}

/// Entry point for the smithay-on-Wayland compositor.
///
/// Diverges (`-> !`) so the caller in `main` can use it next to
/// [`super::hosted::run`] without juggling return types. Any unrecoverable
/// error is `panic`-ed via `expect`; this is a top-level binary entry point,
/// so an `unwrap`-style style is appropriate (and matches smithay's own
/// examples).
/// How long an invite minted from the palette stays valid.
const INVITE_EXPIRY_SECONDS: u64 = 3600;

/// Load this machine's desktop state and bring it up to the invariants
/// every later stage assumes: a persist path to write back to, a
/// durable room identity per workspace, and an owner per room.
///
/// Shares the shape of the hosted entry point's bootstrap, and for the
/// same reasons: an unowned room has no administrators, and a room
/// whose identity is re-minted each boot is a different room every
/// morning.
///
/// Returns the tree alongside whether peer sync may come up. It is
/// refused when the device key will not load, because the id salt comes
/// from that key: syncing an unsalted id space lets two peers mint
/// colliding ids and CRDT sync then folds two distinct workspaces into
/// one row.
fn load_session_tree(
    enable_peer_sync: bool,
) -> (
    crate::shell::state::WorkspaceTreeHandle,
    bool,
    Option<crate::shell::persist::StateNotice>,
) {
    use crate::shell::persist;

    let path = persist::state_json_path();

    // The id salt is set **before** the tree is loaded, and that ordering
    // is load-bearing.
    //
    // Two peers minting ids from the same counter produce colliding ids,
    // and CRDT sync then folds two distinct things into one row. The salt
    // (derived from the libp2p keypair) puts each peer in a disjoint id
    // range, which is what makes an id safe to use as a replication key.
    //
    // Setting it after the load left the ids `bootstrap_default` mints on
    // a **first boot** unsalted — a fresh workspace, its strip, its group
    // and its tabs all in the shared 0-salt range, identical on every
    // machine that ever first-booted. Two such peers joining one room
    // would each project a window group under the same key. Re-seeding
    // the counter afterwards did not help: it fixes ids minted *next*,
    // not the ones already in the tree.
    let mut peer_sync_possible = enable_peer_sync;
    if enable_peer_sync {
        match crate::network::behaviour::load_or_create_keypair(
            &crate::shell::network::peer_key_path(),
        ) {
            Ok(kp) => {
                let salt = crate::network::behaviour::peer_salt_from_keypair(&kp);
                crate::shell::state::WorkspaceTree::set_process_peer_salt(salt);
                tracing::info!("peer-sync: id salt set to 0x{:04x}", salt);
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

    // Carried out to `run`, which has the chrome to put it on the screen with.
    // A session that quietly hands someone a bootstrapped demo tree is
    // indistinguishable from one that lost their work.
    let (mut tree, state_notice) = persist::load_or_default(&path);
    // Past every id already in the tree, so a restart does not re-mint one
    // that is in use. Idempotent, and a no-op on a first boot.
    tree.seed_next_id_from_self();

    // Nothing writes back over a file this session refused to read. Declining
    // to open a state file and then saving a bootstrapped tree onto it is worse
    // than never having checked — the check would announce that the person's
    // workspaces are safe while destroying them. A refused session runs without
    // persistence, the same way `ui::persist` switches an app's writer off over
    // a document it would not open.
    tree.persist_path = if state_notice.is_none() {
        Some(path)
    } else {
        None
    };
    let acl_path = persist::fs_capability_export_path();
    // Said once, at startup: a session exporting somewhere the
    // mount does not read enforces nothing, and does it quietly.
    persist::warn_if_export_is_not_enforced(&acl_path);
    tree.fs_acl_path = Some(acl_path);

    let roomless = tree.adopt_roomless_workspaces();
    if !roomless.is_empty() {
        tracing::info!(
            "collaboration: minted a room identity for {} workspace(s)",
            roomless.len()
        );
    }
    let local_user = crate::shell::state::local_user_id();
    let adopted = tree.adopt_ownerless_workspaces(&local_user);
    if !adopted.is_empty() {
        tracing::info!(
            "capabilities: {} workspace(s) adopted by {local_user}",
            adopted.len()
        );
    }
    (tree.shared(), peer_sync_possible, state_notice)
}

/// Is something already listening on `path`?
///
/// `collab_broker::spawn_full` unlinks before it binds, so without
/// this check a second compositor started against the same
/// `XDG_RUNTIME_DIR` would take the socket away from the running
/// session — apps launched earlier keep a connection to a broker
/// nothing new can reach, and apps launched later talk to a broker the
/// first session's apps cannot see. A successful connect means a live
/// listener; `ECONNREFUSED` means a stale inode from a session that
/// did not shut down cleanly, which is the case we do want to clear.
fn collab_socket_is_live(path: &std::path::Path) -> bool {
    path.exists() && std::os::unix::net::UnixStream::connect(path).is_ok()
}

/// Bind the session's CRDT fan-out broker on the service socket the
/// launcher hands to apps.
///
/// Returns `None` when the broker could not be bound. That is a
/// degraded session, not a normal one — every app manifest in the tree
/// declares `collab`, so nothing collaborates until this succeeds. It
/// is reported at `error!` and surfaced to the user through the
/// chrome; see `chrome_state::set_collab_unavailable`.
fn spawn_session_collab_broker(
    chrome: &crate::shell::chrome_state::Chrome,
    peer_sync: Option<Arc<crate::shell::network::ShellNetwork>>,
) -> Option<crate::shell::collab_broker::BrokerHandle> {
    if let Err(e) = launcher::services::ensure_services_root() {
        tracing::error!(
            "collab: cannot create {} ({e}); apps will start without collaboration",
            launcher::services::services_root().display()
        );
        chrome
            .borrow_mut()
            .set_collab_unavailable(format!("The service directory could not be created: {e}"));
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
        chrome.borrow_mut().set_collab_unavailable(
            "Another session on this machine already owns the collaboration socket.",
        );
        return None;
    }

    // The legacy JSON socket `spawn_full` binds unconditionally. In a
    // real session nothing dials it — apps take the capnp descriptor —
    // so it goes beside the service socket rather than at whatever
    // `QUICK_COLLAB_SOCK` names, keeping it inside the session's own
    // runtime directory. Honour the env var when set so in-tree tools
    // and the legacy two-peer tests keep working.
    let json_path = std::env::var_os("QUICK_COLLAB_SOCK")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| launcher::services::services_root().join("collab-json.sock"));

    match crate::shell::collab_broker::spawn_full(
        json_path.clone(),
        Some(capnp_path.clone()),
        peer_sync.clone(),
    ) {
        Ok(handle) => {
            // `spawn_full` answers for the legacy JSON listener; the
            // capnp socket is the one apps are handed, so ask about it
            // separately rather than reading success into silence.
            if handle.capnp_socket().is_none() {
                tracing::error!(
                    "collab: the app-facing socket {} did not bind; apps will start \
                     without collaboration",
                    capnp_path.display()
                );
                chrome
                    .borrow_mut()
                    .set_collab_unavailable("The app-facing broker socket could not be bound.");
                return None;
            }
            tracing::info!(
                "collab: broker listening on {} (apps) + {} (legacy) — peer_sync={}",
                capnp_path.display(),
                json_path.display(),
                peer_sync.is_some()
            );
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
                .set_collab_unavailable(format!("The broker could not start: {e}"));
            None
        }
    }
}

/// Everything the portal backend needs kept alive for the session.
///
/// Dropping it releases the bus name and tears the object server down,
/// so it is held for the lifetime of `run()`.
struct PortalBackend {
    #[allow(dead_code)]
    runtime: tokio::runtime::Runtime,
    #[allow(dead_code)]
    server: crate::portal::ServerHandle,
}

/// Stand the portal backend up inside the compositor.
///
/// ## Why here and not in `--portal-server-only`
///
/// The portal's whole job is to answer questions about *this session*:
/// which workspace the caller is in, what that workspace has already
/// granted it, and what the person sitting here decides now. All three
/// live in the `WorkspaceTree` this process owns.
///
/// The standalone daemon has none of them. It builds its context on
/// `WorkspaceTree::default()` — an empty tree, so every grant lookup
/// misses — and a `PromptResolver::Channel` queue that nothing drains,
/// because draining means rendering a dialog and it has no output to
/// render on. It could not answer correctly even if it never hung.
///
/// The alternative was to give that daemon IPC back to the compositor.
/// That was rejected: it would put the grant store behind a wire, so
/// two processes would each hold something called "the permissions",
/// and `crates/packaging/launcher/src/capabilities.rs` already states
/// the principle it would break — "a second vocabulary is how the
/// recorded grant and the enforced grant drift apart". It also would
/// not remove the need for the compositor to draw the dialog, so the
/// daemon would end up a proxy that owns a bus name and forwards
/// everything. A proxy for one process is that process.
///
/// So the compositor owns the name. `os/usr/lib/systemd/user/shell-portal.service`
/// and its D-Bus activation file are removed with this change; a portal
/// with no session behind it has nothing to serve.
///
/// Returns `None` when there is no session bus (a headless test rig
/// without one, a VT with no user bus). That is a real configuration,
/// and the compositor still runs — it just logs that file open/save
/// will not work, rather than failing to boot.
fn start_portal_backend(
    frame_builder: &mut FrameBuilder,
    tree_handle: crate::shell::state::WorkspaceTreeHandle,
) -> Option<PortalBackend> {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .thread_name("qos-portal")
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::warn!("portal: no runtime ({e}); file open/save will not work");
            return None;
        }
    };

    let grants = crate::portal::GrantStoreHandle::new(tree_handle);
    let prompts = crate::portal::PromptQueueHandle::new(crate::portal::PromptResolver::Channel);
    let pickers = crate::portal::FilePickerQueue::new();
    let documents = crate::portal::documents_for_session();

    let ctx = crate::portal::PortalContext {
        grants,
        prompts: prompts.clone(),
        caller_map: crate::portal::CallerMap::new(),
        file_picker: crate::portal::FilePickerSpawnerHandle::new(Arc::new(pickers.clone())),
        screencast_provisioner: crate::portal::ScreenCastStreamProvisionerHandle::new(Arc::new(
            crate::portal::MockScreenCastProvisioner::new_with_node(1, "default"),
        )),
        remote_input_injector: crate::portal::RemoteInputInjectorHandle::null(),
        documents: documents.clone(),
        // The real spooler. Unlike the picker it needs no chrome and no
        // live seat — it is a process invocation — and on a machine with
        // no CUPS it reports no destinations rather than a fabricated one.
        print_spooler: crate::portal::PrintSpoolerHandle::cups(),
    };

    let served = runtime.block_on(async {
        let conn = zbus::ConnectionBuilder::session()?.build().await?;
        crate::portal::serve(conn, ctx).await
    });
    let server = match served {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                "portal: could not serve {} ({e}); file open/save will not work",
                crate::portal::BUS_NAME
            );
            return None;
        }
    };

    // Losing the name is not a warning-shaped problem. xdg-desktop-portal
    // routes FileChooser to whoever owns it, so if that is somebody else
    // this compositor's dialog will never be asked for and every Open in
    // the session goes to a backend with no session state. Say so at a
    // level that reaches the journal by default.
    if !server.owns_name() {
        tracing::error!(
            "portal: another process already owns {} — this session's file \
             dialog will never be reached. The standalone \
             `shell --portal-server-only` daemon is the usual culprit; it is \
             no longer part of the image and its unit should be disabled.",
            crate::portal::BUS_NAME
        );
    }

    // The queues are useless until something drains them. Attaching them
    // to the overlay host is what makes the per-frame pumps run, and
    // doing it here — rather than at host construction — is what keeps
    // "the portal is up" and "its prompts reach a screen" the same fact.
    frame_builder
        .host_mut()
        .set_portal_queues(prompts, pickers);

    tracing::info!(
        "portal: serving FileChooser+ScreenCast+RemoteDesktop+Access on {}; documents via {}",
        crate::portal::BUS_NAME,
        documents.describe()
    );
    if !documents.reaches_confined_apps() {
        // Say it out loud. A sandboxed app that picks a file and then
        // gets ENOENT is a bug report about the app, not about this, and
        // the journal is the only place that connection can be made.
        tracing::warn!(
            "portal: the document store cannot reach sandboxed apps — a file \
             picked by a bwrap'd app will not be readable by it. Unconfined \
             apps are unaffected."
        );
    }
    Some(PortalBackend { runtime, server })
}

pub fn run() -> ! {
    // Calloop drives the whole compositor; smithay is callback-oriented and
    // expects every wakeup to fan out from a single `EventLoop::<State>`.
    let mut event_loop: EventLoop<'static, State> =
        EventLoop::try_new().expect("create calloop event loop");

    // The `Display` owns the wayland-server protocol state. We hold it in a
    // local so `dispatch_clients`/`flush_clients` can be called from the
    // per-iteration tick once Phase 4 needs to wake clients up.
    //
    // For Phase 2 the loop only fires when the listening socket has a new
    // connection, so the `Display` exists primarily to mint `DisplayHandle`s.
    //
    // Phase 7: the loop body must also call `display.dispatch_clients` +
    // `display.flush_clients` each tick — without that, the per-iteration
    // calloop dispatch never pulls bytes off accepted client sockets and
    // the client side of `wl_display.get_registry` blocks forever.
    // Mirrors smithay 0.7's `examples/minimal.rs` and `examples/seat.rs`
    // loop bodies.
    let mut display: Display<State> = Display::new().expect("create wayland display");
    let display_handle = display.handle();

    let mut state = State::new(event_loop.handle(), display_handle.clone());
    // Point `ui`'s clipboard at the compositor's selection mirror. Everything
    // hosted in this process — the chrome, the terminal, the file explorer,
    // notes and the editor — copies and pastes through `ui`, and none of them
    // is a Wayland client. Without this they would share a buffer that no
    // other process can see.
    crate::compositor::selection::install_host_clipboard(state.selection_store());

    // Phase 3: construct the winit backend so the compositor has a host
    // window to render into. Phase 4 routes per-frame scenes through
    // `backend.submit_frame`. We `expect` here for the same reason the
    // socket bind below does — a top-level `-> !` entry point treats any
    // backend init failure as fatal (same posture as smithay's `anvil`).
    //
    // Phase 5 Task 5.1: `--backend drm|winit` selects which `Backend`
    // impl runs. `Drm` is currently a skeleton — it errors at
    // construction with a "not yet implemented" message — but the
    // dispatch is wired so the follow-up TTY work plugs in without
    // touching this entry point.
    let backend_kind = parse_backend_from_args();
    let mut backend: Box<dyn Backend> = match backend_kind {
        BackendKind::Winit => {
            let mut b = WinitBackend::new().expect("init winit backend");
            // Match the FrameBuilder's internal theme so the host
            // window clears to chrome bg before the first frame
            // paints. Runtime theme changes would re-call this; for
            // now `Theme::dark()` is the only one in play and the
            // FrameBuilder picks the same constant.
            b.set_base_color(ui::theme::Theme::dark().bg_app);
            Box::new(b)
        }
        BackendKind::Drm => match DrmBackend::new() {
            Ok(b) => Box::new(b),
            Err(e) => {
                eprintln!("nirios compositor: --backend drm failed: {e}");
                std::process::exit(1);
            }
        },
        BackendKind::Headless => {
            // Initial size comes from `NIRIOS_HEADLESS_SIZE` (with a
            // shots-default fallback) — keeps the headless backend
            // env-var-driven like `QOS_TEST_CONTROL` /
            // `NIRIOS_TEST_STATE_DUMP` instead of adding a third CLI
            // flag for size. Skip `set_base_color` because headless
            // has no window — `HeadlessBackend::submit_frame` clears
            // to the same constant the winit backend uses.
            let (w, h) = headless::resolve_initial_size();
            match HeadlessBackend::new(w, h) {
                Ok(b) => Box::new(b),
                Err(e) => {
                    eprintln!("nirios compositor: --backend headless failed: {e}");
                    std::process::exit(1);
                }
            }
        }
    };

    // Asked once: a backend does not grow or lose a cursor plane at
    // runtime, and the paint loop reads this on every frame.
    let backend_has_cursor_plane = backend.has_cursor_plane();

    // Phase 4: per-frame scene production lifted out of `bin/shots`.
    // The compositor and the offscreen tooling share `FrameBuilder`,
    // so the chrome rendered into the host window is byte-identical
    // to the screenshots `cargo run --bin shots` produces.
    // Size the scene to the bounding box spanning every output (Track
    // B3): the compositor paints one scene across the combined geometry
    // and each output presents its own sub-rect. For a single output this
    // equals that output's size, so single-monitor layout is unchanged.
    let initial_size = match combined_geometry(backend.outputs()) {
        (0, 0) => (1600, 900),
        size => size,
    };
    let enable_peer_sync = crate::compositor::peer_sync_enabled_from_env_or_marker();

    // The desktop's own state, loaded from disk rather than bootstrapped
    // fresh. Everything the room model rests on — a workspace's durable
    // room identity, its owner, its capability list — is a fact about
    // *this* machine that has to survive a restart, and until now this
    // entry point had none of it: `FrameBuilder::new` handed the
    // compositor `WorkspaceTree::bootstrap_demo()`, so every boot was
    // a new desktop with unowned rooms and nothing was ever written back.
    let (tree_handle, enable_peer_sync, state_notice) = load_session_tree(enable_peer_sync);
    let mut frame_builder = FrameBuilder::with_handle_and_records(
        tree_handle.clone(),
        enable_peer_sync,
        initial_size,
        crate::shell::session_record_store(),
    );
    if let Some(notice) = &state_notice {
        crate::shell::notification::announce_state_notice(&frame_builder.chrome_handle(), notice);
    }
    // The scene spans every output, but a window's width is a fraction of
    // the monitor it is on — so hand the strips the real per-display
    // geometry rather than letting them size against the bounding box.
    frame_builder.set_output_geometry(output_geometry(backend.outputs()));
    // Which records each room holds outlives the session, and the records
    // apps have written are found by looking. Both are switched on here,
    // beside the rest of the session's durable state, so building a
    // compositor for a test or a screenshot reads and writes nobody's.
    frame_builder.host().room_records().borrow_mut().attach_store();

    // The xdg-desktop-portal backend. Served from *this* process, on
    // the session tree this process owns — see `start_portal_backend`
    // for why it is not the separate `--portal-server-only` daemon.
    let _portal = start_portal_backend(&mut frame_builder, tree_handle.clone());

    // Peer sync. Constructed here rather than only in the hosted entry
    // point because this is the compositor that boots as the qOS
    // session: a collaboration stack wired into the other one is a
    // collaboration stack no session runs.
    let peer_sync: Option<Arc<crate::shell::network::ShellNetwork>> = if enable_peer_sync {
        match crate::shell::network::ShellNetwork::spawn(
            tree_handle.clone(),
            &crate::shell::network::peer_key_path(),
        ) {
            Ok(handle) => {
                let _ = handle.join_default_workspace();
                tracing::info!("peer-sync: actor spawned, default workspace joined");
                let net = Arc::new(handle);
                let bridge_net = Arc::clone(&net);
                let bridge_tree = tree_handle.clone();
                frame_builder.host_mut().set_peer_sync_bridge(std::rc::Rc::new(
                    move |req| {
                        let active = bridge_tree.read().active;
                        match req {
                            PeerSyncRequest::IssueInvite => {
                                bridge_net.issue_invite(active, INVITE_EXPIRY_SECONDS)
                            }
                            PeerSyncRequest::IssueInviteFor(name) => bridge_net
                                .issue_invite_named(active, INVITE_EXPIRY_SECONDS, &name),
                            PeerSyncRequest::AcceptInvite(token) => bridge_net
                                .accept_invite(active, &token)
                                .map(|room| room.to_string()),
                        }
                    },
                ));
                Some(net)
            }
            Err(e) => {
                tracing::warn!("peer-sync: spawn failed ({e}); continuing without sync");
                None
            }
        }
    } else {
        None
    };
    // The session's CRDT fan-out broker. Bound here, before the
    // wayland socket is published and therefore before any app can be
    // launched, because an app resolves its collab descriptor once at
    // spawn: a broker that arrives later is a broker that app never
    // gets.
    //
    // It lives in this process rather than in a unit of its own. The
    // broker is a listener plus a fanout table over `ShellNetwork` —
    // the libp2p handle built directly above — and the shell is where
    // the network stack, the identity and the permission decisions
    // already live (`plan/collaboration.md` §1). Moving it out of
    // process would mean re-exporting all three across an IPC boundary
    // to reach the same decisions, and would put session ordering in
    // the hands of a unit manager that does not know when this
    // compositor has published its socket.
    //
    // Held to the end of `run()` so the socket outlives every app and
    // is unlinked with the session.
    let collab_broker =
        spawn_session_collab_broker(&frame_builder.chrome_handle(), peer_sync.clone());

    // A participant without a window can ask this session to open an
    // application. The broker takes the question off the socket; the
    // desktop answers it on the frame loop below, where the workspace
    // whose capabilities decide is reachable. Installed even in a
    // session with no peer sync — an agent on the local socket is a
    // participant whether or not the room has anybody else in it.
    let pending_opens = crate::shell::agent_opens::PendingOpens::new();
    if let Some(broker) = collab_broker.as_ref() {
        pending_opens.install(broker);
    }

    // An application asking for the "say something about this" surface. Same
    // arrangement as the opens above, and for the same reason: the broker takes
    // the question off the socket, and the frame loop answers it, because where
    // that window sits and who is in the room are things only the thread that
    // draws can say.
    let pending_composes = crate::shell::remark_surface::PendingComposes::new();
    if let Some(broker) = collab_broker.as_ref() {
        pending_composes.install(broker);
    }

    // Read the session's own broker for pointer presence, and relay it
    // to the room's other hosts.
    //
    // The compositor hosts the broker but was not otherwise a reader of
    // it, so the pointer every app publishes reached the other apps in
    // the room and stopped there — the overlay that draws peers over
    // these windows never saw one. Subscribing by room is what makes a
    // cursor that crosses from one app to another a single peer moving:
    // presence is room-scoped, so both apps reach the same subscription.
    //
    // Both halves are needed, so this is only stood up when both exist.
    // Without peer sync there is nobody to relay to or hear from, and
    // the overlay correctly paints nothing.
    if let (Some(broker), Some(net)) = (collab_broker.as_ref(), peer_sync.as_ref()) {
        let rooms = {
            let chrome = frame_builder.chrome_handle();
            let chrome = chrome.borrow();
            let tree = chrome.tree();
            tree.workspaces
                .iter()
                .map(|w| w.room.to_string())
                .collect::<Vec<_>>()
        };
        tracing::info!(
            "collab: subscribed to {} room(s) for peer presence",
            rooms.len()
        );
        frame_builder.set_peer_presence(crate::shell::peer_presence::PeerPresence::attach(
            broker,
            Arc::clone(net),
            rooms,
        ));
    }

    // Seed the size used for fullscreen configures so an early
    // `set_fullscreen` request fills the output even before the first resize.
    state.output_size = (initial_size.0 as i32, initial_size.1 as i32);

    // `new_auto` walks `wayland-1..wayland-32` and binds the first free name.
    // The smoke test in the plan asserts on the line below ("nirios compositor
    // listening on …"), so the format here is part of the contract.
    let listening_socket =
        ListeningSocketSource::new_auto().expect("bind listening wayland socket");
    let socket_name = listening_socket.socket_name().to_os_string();
    eprintln!("nirios compositor listening on {socket_name:?}");
    // The line above is the contract; the two below are for the person who
    // just ran `cargo run -p shell` and now wants to point something at it.
    // Publishing the name to a file as well means the next command can read
    // it instead of the human copying it out of a log.
    let _display_file = publish_wayland_display(&socket_name);
    eprintln!(
        "  WAYLAND_DISPLAY={} — e.g. `WAYLAND_DISPLAY={} cargo run -p terminal`",
        socket_name.to_string_lossy(),
        socket_name.to_string_lossy(),
    );
    if let Some(guard) = _display_file.as_ref() {
        eprintln!("  also written to {}", guard.0.display());
    }
    // Publish it to the app launcher: every app we start is pointed at
    // this socket explicitly (`qos-launcher --wayland-socket`), never at
    // whatever display the compositor's own environment happens to name.
    crate::shell::launcher::set_wayland_socket(&socket_name);

    event_loop
        .handle()
        .insert_source(listening_socket, move |client_stream, _, state| {
            // Each accepted connection becomes a wayland-server `Client`. We
            // attach a `ClientState` so `CompositorHandler::client_compositor_state`
            // can fish out the per-client `CompositorClientState` later.
            if let Err(err) = state
                .display_handle
                .insert_client(client_stream, Arc::new(ClientState::default()))
            {
                eprintln!("nirios compositor: failed to insert client: {err}");
            }
        })
        .expect("insert listening socket source into event loop");

    // A second Wayland socket, for clients the session trusts with the
    // clipboard-manager protocols. It carries the identical protocol set
    // except that `zwlr_data_control_manager_v1` / `ext_data_control_manager_v1`
    // are visible on it — see `compositor::selection` for why those two are
    // not on the public socket.
    //
    // The privilege is the socket, not a request a client can make. The
    // sandbox launcher bind-mounts exactly one Wayland socket into a workspace
    // sandbox (`crates/packaging/launcher/src/sandbox.rs`), the public one, so
    // a sandboxed app has no filesystem path to this one regardless of what it
    // knows. A bind failure is logged and the session continues: the effect is
    // that clipboard managers cannot connect, not that the desktop is down.
    match bind_privileged_socket() {
        Ok((privileged_socket, path)) => {
            eprintln!("nirios compositor privileged socket on {path:?}");
            if let Err(err) = event_loop.handle().insert_source(
                privileged_socket,
                move |client_stream, _, state| {
                    if let Err(err) = state
                        .display_handle
                        .insert_client(client_stream, ClientState::privileged_arc())
                    {
                        eprintln!("nirios compositor: failed to insert privileged client: {err}");
                    }
                },
            ) {
                eprintln!("nirios compositor: privileged socket source rejected: {err}");
            }
        }
        Err(e) => {
            eprintln!("nirios compositor: no privileged wayland socket ({e}); clipboard managers will not be able to connect");
        }
    }

    // Phase 7 test introspection: when `NIRIOS_TEST_STATE_DUMP` is set, the
    // compositor serializes its `WorkspaceTree` to that path on every frame
    // tick. The integration test in `tests/compositor_test.rs` reads this
    // file to assert foreign-tab adoption + reaping. Production runs never
    // set the env var, so the dump is a true zero-cost noop there.
    let test_dump_path = test_state_dump_path(
        crate::compositor::dev_env::DEV_ENV_OVERRIDES,
        std::env::var_os(TEST_STATE_DUMP_ENV),
    );

    // Real-input e2e Phase 1b: opt-in unix socket that accepts
    // newline-delimited JSON commands (see `compositor::test_control`).
    // Bound to whatever path `QOS_TEST_CONTROL` points at; unset
    // means the socket is never created (zero-cost in production).
    // Each loop tick polls the listener for new connections and drains
    // pending bytes from existing ones.
    let (test_control_listener, _test_control_unlink) = match bind_test_control_socket() {
        Some((l, g)) => (Some(l), Some(g)),
        None => (None, None),
    };
    let mut test_control_conns: Vec<TestControlConn> = Vec::new();
    // Phase 2b: per-process scratch context for executing PlannedActions
    // off the test-control socket. The compositor doesn't (yet) route
    // real winit input to the host, so this is the only place a
    // `Modifiers` snapshot / `AutoCommit` doc / focus cursor lives in
    // the live binary. Production runs never set the env var that
    // enables the socket, so this is zero-cost in shipped builds.
    let mut test_control_ctx = TestControlCtx::new();

    // Phase 4 tick body. Each iteration:
    //
    //   1. Drain backend events (resize, close, redraw, focus). A
    //      `CloseRequested` flips a flag we read after the closure to
    //      tell the calloop loop to stop.
    //   2. Run a layout pass on the frame builder.
    //   3. Paint the scene.
    //   4. Submit the scene to the backend's primary output.
    //   5. (Test-only) dump shell state if `NIRIOS_TEST_STATE_DUMP` is set.
    //
    // `submit_frame` is currently a no-op (vello + smithay glow interop
    // is the next follow-up); the host window therefore stays blank.
    // The scene is still produced every frame so smoke tests can
    // confirm the layout pipeline runs without a GPU surface.
    // Track D2: idle / DPMS-blank / session-lock state machine. Pure
    // logic (see `compositor::idle`); the loop below turns its
    // transitions into `backend.set_dpms` calls and lock-overlay paints.
    // Config comes from `QOS_IDLE_BLANK_SECS` / `QOS_IDLE_LOCK_SECS`
    // (defaults: blank after 5 min, no auto-lock).
    // Power policy from the shared Settings store (`settings.json`); drives
    // the idle-suspend timeout, the lid / power-key actions, and the battery
    // thresholds. Read once at session start, the same way the display scale
    // is — the contract is the JSON on disk, not a shared crate.
    let power_policy = qos_power_mgmt::config::load_policy();
    let mut session = SessionManager::new(
        IdleConfig::from_env().with_suspend(power_policy.idle_suspend),
        Instant::now(),
    );
    // Session-lock unlock: the lock screen's input + auth state machine. The
    // verifier is chosen from the environment — the injected fixed password
    // for the headless harness (`QOS_LOCK_TEST_PASSWORD`), else the real
    // greetd/PAM path (`greeter::greetd_client::verify_password`) — so the
    // lock/unlock flow runs the same code in tests and production, differing
    // only in how a credential is checked. Fail-closed when neither is set.
    let mut lock = LockController::new(
        lock_auth::verifier_from_env(),
        std::env::var("USER").unwrap_or_else(|_| "user".to_string()),
    );
    // Locked edge, so the controller resets exactly when the lock engages
    // (clearing any half-typed password / stale error from a prior lock).
    let mut prev_locked = false;
    // The suspend-inhibitor registry: apps/tasks hold a token via
    // `inhibit_registry.acquire(reason)` to keep the machine awake. The power
    // service mirrors it into a real logind `idle:sleep` lock; the idle loop
    // consults `is_inhibited()` before honouring an idle-suspend.
    let inhibit_registry = qos_power_mgmt::InhibitRegistry::new();
    // The mailbox the async power service uses to reach this loop (lock the
    // session, post a battery toast, reset the idle timer on resume) and this
    // loop uses to reach it (request a suspend).
    let power_bridge = qos_power_mgmt::PowerBridge::new();

    // Track D1: the `org.freedesktop.Notifications` D-Bus service. The
    // compositor owns the notification daemon so sandboxed apps' toasts
    // render as OS-drawn chrome. The bridge is the `Send + Sync` mailbox
    // between the async zbus service (driven by a dedicated tokio
    // runtime) and the synchronous chrome pump in this loop. Failure to
    // reach the session bus (none present / another daemon owns the
    // name) is downgraded to a warning — the compositor must keep
    // running, and shell-internal notifications still work without the
    // external service.
    let notif_bridge = crate::notifications::NotificationBridge::new();
    frame_builder.with_chrome(|st| st.set_notification_bridge(notif_bridge.clone()));
    // Held for the lifetime of `run` so the runtime's worker threads keep
    // driving zbus dispatch + the signal-emitter task in the background.
    let mut _notif_runtime: Option<tokio::runtime::Runtime> = None;
    let mut _notif_handle: Option<crate::notifications::ServerHandle> = None;
    match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(1)
        .thread_name("nirios-notify")
        .build()
    {
        Ok(rt) => {
            let bridge = notif_bridge.clone();
            let res: Result<crate::notifications::ServerHandle, zbus::Error> =
                rt.block_on(async move {
                    let conn = zbus::ConnectionBuilder::session()?.build().await?;
                    let service = crate::notifications::NotificationService::new(
                        bridge.clone(),
                        // v1: a fresh CallerMap. The per-workspace bridge
                        // accept path populates unique-name → workspace
                        // entries once that lands (same shape the portal's
                        // CallerMap uses); until then unsandboxed callers
                        // simply carry no workspace attribution.
                        crate::portal::CallerMap::new(),
                    );
                    let handle =
                        crate::notifications::serve_notifications(conn.clone(), service).await?;
                    crate::notifications::spawn_signal_emitter(conn, bridge);
                    Ok(handle)
                });
            match res {
                Ok(h) => {
                    tracing::info!(
                        "notifications: serving {} on session bus",
                        crate::notifications::BUS_NAME
                    );
                    _notif_handle = Some(h);
                }
                Err(e) => {
                    tracing::warn!(
                        "notifications: serve failed ({e}); continuing with \
                         shell-internal notifications only"
                    );
                }
            }
            // Keep the runtime alive regardless of the notification outcome:
            // the power service lives on the *system* bus (logind/UPower),
            // independent of the *session*-bus notification daemon, and shares
            // these background threads.
            _notif_runtime = Some(rt);
        }
        Err(e) => {
            tracing::warn!("notifications: tokio runtime build failed ({e}); D-Bus service off");
        }
    }

    // Power service: the async orchestrator that suspends via logind, mirrors
    // the inhibitor registry into a logind lock, watches the battery, and
    // reacts to the lid. Spawned on the notify runtime (same background
    // threads). If the system bus / logind / UPower are unreachable it logs
    // and the compositor keeps running with the pure idle policy only — a
    // headless / bus-less session still blanks, locks, and honours env
    // suspend timeouts, it just cannot actuate real suspend.
    if let Some(rt) = _notif_runtime.as_ref() {
        let bridge = power_bridge.clone();
        let registry = inhibit_registry.clone();
        let policy = power_policy;
        rt.spawn(async move {
            match qos_power_mgmt::PowerService::connect(bridge, registry, policy).await {
                Ok(service) => service.run().await,
                Err(e) => tracing::warn!(
                    target: "qos::power",
                    "power service unavailable ({e}); suspend/lid/battery inactive"
                ),
            }
        });
    }

    let mut close_requested = false;

    // Presentation cadence. The loop used to park a hard-coded 16 ms
    // *after* each tick's work, so the real period was `work + 16 ms`
    // (measured: 21.9 ms, i.e. 45.6 Hz) and no display's actual rate ever
    // entered into it. `FrameClock` replaces both halves: the interval
    // comes from the backend's display, and the park is the remainder of
    // the interval rather than a fresh one. See `compositor::pacing`.
    // `QOS_REFRESH_HZ` overrides the display's rate. Diagnostic only —
    // it is what makes "does the loop hold a 144 Hz cadence" measurable
    // on a machine with no 144 Hz panel attached.
    let refresh_override = pacing::refresh_override_from_env();
    let mut frame_clock = match refresh_override.or_else(|| backend.refresh_interval()) {
        Some(interval) => {
            eprintln!(
                "nirios compositor: pacing to {:.3} Hz",
                1.0 / interval.as_secs_f64()
            );
            pacing::FrameClock::new(interval, Instant::now())
        }
        None => {
            // No display to follow — the headless backend, or a host that
            // won't say. Fall back rather than invent a rate.
            eprintln!(
                "nirios compositor: backend reports no refresh rate; pacing to {:.3} Hz",
                1.0 / pacing::FALLBACK_INTERVAL.as_secs_f64()
            );
            pacing::FrameClock::fallback(Instant::now())
        }
    };
    let frame_period = frame_clock.interval();

    // Move presentation onto its own thread where the backend supports
    // it (winit, drm). The headless backend deliberately declines — its
    // rendered bytes are read back synchronously by the test-control
    // `snapshot` command — and then `backend.submit_frame` keeps running
    // inline exactly as before. See `compositor::present`.
    let mut present_pipe = backend.take_presenter().map(|p| {
        eprintln!("nirios compositor: presenting on a dedicated render thread");
        // Wake the loop when a frame retires. Without this a tick that
        // declined to paint for lack of pipe capacity would park out the
        // whole frame interval, even though the GPU freed up microseconds
        // later — which halves throughput on a GPU-bound frame. The ping
        // is a calloop source, so the wakeup lands in the same
        // `event_loop.dispatch` the loop was already parked in.
        let (ping, ping_source) =
            calloop::ping::make_ping().expect("create present-retire ping");
        event_loop
            .handle()
            .insert_source(ping_source, |_, _, _| {
                // Waking is the whole effect: the loop re-evaluates its
                // paint gate on the next iteration and now finds room.
            })
            .expect("insert present-retire ping source");
        present::PresentPipe::spawn(p, Box::new(move || ping.ping()))
    });
    // Reused per-iteration scratch so backend events can be processed
    // *after* `poll_events` returns — the input arm needs `&mut backend`
    // (for `set_dpms`) which can't be borrowed inside the poll closure.
    let mut pending_events: Vec<BackendEvent> = Vec::new();

    // `wl_output` globals. The compositor advertised the manager but never
    // an output, so no client could learn there was a display at all. The
    // registry is synced here at startup and again on every hotplug.
    let mut output_globals = output_globals::OutputGlobals::new();
    let display_handle_for_outputs = display_handle.clone();
    {
        let outs = backend.outputs().to_vec();
        state.set_output_layout(&outs);
        output_globals.sync::<State>(
            &display_handle_for_outputs,
            &outs,
            refresh_mhz_of(&*backend),
        );
    }
    // Set when the output set changed this tick, so everything derived
    // from it is re-derived once per batch rather than once per udev edge.
    let mut outputs_dirty = false;

    // Pointer / keyboard preferences are read once at startup; this
    // notices when the Settings app writes a new value so pointer speed,
    // natural scroll and tap-to-click take effect in the session the user
    // changed them in rather than the next one.
    let mut settings_watch =
        crate::compositor::backend::drm::input_config::SettingsWatch::new();
    // Redraw gating. The loop used to `paint()` + `submit_frame()` on
    // every tick regardless of whether anything changed, pinning a CPU
    // core at 100% (and driving needless GPU work) even on a completely
    // static desktop. We now repaint only when there's a reason to:
    //   - a backend event arrived (input / resize / focus / redraw),
    //   - the widget tree has an animation in flight,
    //   - the chrome revision bumped (a state mutation: overlay toggle,
    //     notification, tab switch, …),
    //   - a session transition fired (blank / lock / unlock),
    //   - or the keepalive interval elapsed (a safety net so any missed
    //     dirty source still refreshes within a second).
    // Layout still runs every tick so input hit-testing stays current;
    // only the expensive paint + GPU submit are gated.
    let keepalive = Duration::from_millis(1000);
    let mut last_paint = Instant::now();
    let mut last_revision = frame_builder.chrome_revision();
    let mut force_first_paint = true;
    // Optional paint-rate diagnostics (`QOS_PAINT_STATS=1`).
    let paint_stats = std::env::var("QOS_PAINT_STATS").is_ok();
    // Optional per-iteration timing (`QOS_LOOP_TIMING=1`): logs any tick
    // whose non-sleeping work exceeds 100 ms, with a phase breakdown, so a
    // main-thread stall (e.g. a synchronous drop/detach handler) shows up
    // as an actual measurement instead of a guess.
    let loop_timing = std::env::var("QOS_LOOP_TIMING").is_ok();
    // Frame-loop instrumentation (`QOS_FRAME_TRACE` / `QOS_FRAME_SUMMARY`).
    // Disabled — and free — when neither env var is set. See
    // `compositor::metrics`.
    let mut metrics = metrics::FrameRecorder::from_env();
    metrics.set_target_period(frame_period);
    // Bench hook: shut the loop down cleanly after N seconds so the
    // frame trace and summary are flushed by the normal exit path rather
    // than lost to a SIGKILL. Unset in production, where the loop runs
    // until the session ends.
    let run_deadline = metrics::run_seconds_from_env().map(|d| {
        eprintln!("nirios compositor: {}={:?}; will exit cleanly after that", metrics::RUN_SECONDS_ENV, d);
        Instant::now() + d
    });
    // While a control-socket caller is driving, the loop parks in short
    // hops instead of sleeping to the next presentation deadline.
    //
    // The socket is polled from this loop body rather than registered as
    // an event source, so nothing about a command arriving can wake the
    // park it arrives during: the loop notices on its next pass, up to a
    // whole interval later. That was ~18 ms of every round trip, and for
    // a recording — which is one command per frame — it set the frame
    // rate as surely as the work did.
    //
    // Armed by traffic rather than by the socket existing, so a session
    // nobody is driving keeps its ordinary cadence instead of spinning.
    // Only a development build reaches any of this: the socket is
    // compiled out of shipped builds (`compositor::dev_env`).
    const TEST_CONTROL_PARK: Duration = Duration::from_millis(2);
    const TEST_CONTROL_ACTIVE_FOR: Duration = Duration::from_millis(500);
    let mut last_test_control: Option<Instant> = None;
    let mut stats_since = Instant::now();
    let (mut frames_total, mut frames_painted) = (0u32, 0u32);
    let (mut cause_events, mut cause_revision, mut cause_anim) = (0u32, 0u32, 0u32);
    let (mut cause_session, mut cause_keepalive) = (0u32, 0u32);
    while !close_requested {
        let iter_t0 = Instant::now();
        metrics.begin_tick(iter_t0);
        backend.poll_events(&mut |event| pending_events.push(event));
        let had_backend_events = !pending_events.is_empty();
        let mut session_changed = false;
        for event in pending_events.drain(..) {
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
                        frame_builder.set_viewport(size);
                        // A resize is also how the compositor learns a
                        // monitor was plugged in, unplugged or re-moded, so
                        // re-derive the per-display widths groups size
                        // against alongside the scene extent.
                        frame_builder
                            .set_output_geometry(output_geometry(backend.outputs()));
                        // Keep the size used for fullscreen configures in sync
                        // so a fullscreened client fills the current output.
                        state.output_size = (size.0 as i32, size.1 as i32);
                        // The swap chain lives on the render thread now, so
                        // the reconfigure travels with the frames rather
                        // than happening inline in the backend's event
                        // handler — that keeps it ordered against any frame
                        // already queued at the old size.
                        if let Some(pipe) = present_pipe.as_ref() {
                            pipe.resize(size);
                        }
                        // A resize is also how the compositor learns the
                        // window moved to a different monitor, or that a
                        // mode changed. Re-ask the backend what it is
                        // presenting to and retarget the clock, so
                        // "pace to the display" keeps being true after
                        // startup rather than only at it. The env
                        // override still wins — it exists precisely to
                        // pin a rate the hardware isn't offering.
                        if refresh_override.is_none() {
                            if let Some(interval) = backend.refresh_interval() {
                                if frame_clock.set_interval(interval) {
                                    eprintln!(
                                        "nirios compositor: display changed; now pacing to {:.3} Hz",
                                        1.0 / interval.as_secs_f64()
                                    );
                                    metrics.set_target_period(interval);
                                }
                            }
                        }
                    }
                }
                BackendEvent::Input(wev) => {
                    // Instrumentation: the clock starts when the loop takes
                    // the event off the backend. The first painted frame
                    // after this closes the input-to-present measurement.
                    metrics.note_input(Instant::now());
                    // Track D2: any input is activity — reset the idle
                    // timer and wake the display if it was blanked.
                    for t in session.note_activity(Instant::now()) {
                        apply_session_transition(t, backend.as_mut(), present_pipe.as_ref());
                    }
                    // Physical priority: the host reaching for their own
                    // keyboard or mouse blocks every remote peer for the
                    // next half second. Stamped at the funnel rather than
                    // further in, so the branches below that never reach
                    // the client router — the lock screen, the VT chord,
                    // the power key — count as the host taking over too.
                    state
                        .remote_input_firewall
                        .note_local_input(Instant::now());

                    if session.is_locked() {
                        // Locked: every input is captured by the lock
                        // surface — routed into the password field, never to
                        // apps/chrome behind the overlay. Enter starts an
                        // asynchronous credential verify on a worker thread;
                        // the session only unlocks when the per-tick
                        // `lock.poll_verification()` below observes a verified
                        // success. No key can bypass that. This is the input
                        // half of the lock's security boundary.
                        let key = winit_event_to_lock_key(&wev);
                        lock.handle_key(key);
                        // Any keystroke changes the masked field / error, so
                        // force a repaint this tick.
                        session_changed = true;
                    } else if is_power_key(&wev) {
                        // The hardware power key resolves per the power
                        // policy (logind delegates it to us via the service's
                        // block inhibitor). A deliberate press bypasses the
                        // idle inhibitor — the user asked for it.
                        match power_policy.on_power_key() {
                            Some(qos_power_mgmt::PowerAction::Suspend) => {
                                tracing::info!(target: "qos::power", "power key: requesting suspend");
                                power_bridge.request_suspend();
                            }
                            Some(qos_power_mgmt::PowerAction::Lock) => {
                                for t in session.request_lock(Instant::now()) {
                                    apply_session_transition(t, backend.as_mut(), present_pipe.as_ref());
                                }
                            }
                            Some(qos_power_mgmt::PowerAction::Ignore) | None => {}
                        }
                    } else if let Some(vt) = vt_switch_chord(&wev, &test_control_ctx.modifiers) {
                        // Ahead of every other chord and never offered to a
                        // client: leaving the terminal has to work even when
                        // an app has grabbed the keyboard, or a misbehaving
                        // client can strand the user on this VT.
                        backend.switch_vt(vt);
                    } else if is_remote_input_kill_switch(&wev, &test_control_ctx.modifiers) {
                        // Super+Esc: every peer's input stops now and
                        // stays stopped. Ahead of the client router and
                        // ahead of the shell's Escape handling, because a
                        // panic key an overlay or a focused client can
                        // swallow is not a panic key.
                        client_input::stop_remote_input(&mut state, &mut frame_builder);
                        session_changed = true;
                    } else if is_lock_chord(&wev, &test_control_ctx.modifiers) {
                        // Super+L locks immediately.
                        for t in session.request_lock(Instant::now()) {
                            apply_session_transition(t, backend.as_mut(), present_pipe.as_ref());
                        }
                    } else if let Some(mk) = crate::shell::media_keys::classify(&wev) {
                        // The hardware media / brightness row belongs to
                        // the machine, not to whichever client happens to
                        // hold focus — a speaker glyph means "make this
                        // machine louder" wherever you press it. Handled
                        // before client routing for that reason.
                        if let Some(osd) = crate::shell::media_keys::apply(
                            mk,
                            frame_builder.host().qs_actions(),
                            frame_builder.host().status_bridge(),
                        ) {
                            frame_builder.with_chrome(|st| st.show_osd(osd));
                            session_changed = true;
                        }
                    } else {
                        // Backends that don't pair a `ModifiersChanged`
                        // with every modifier keystroke would otherwise
                        // leave the mask stale, which is what decides
                        // whether a chord belongs to the shell.
                        if let WindowEvent::KeyboardInput { event: key_ev, .. } = &wev {
                            client_input::apply_modifier_key(
                                &mut test_control_ctx.modifiers,
                                key_ev,
                            );
                        }
                        // Offer the event to the focused Wayland client
                        // first. `route_input` returns `false` for anything
                        // the shell owns — compositor chords, overlay-
                        // captured input, and every pointer position that
                        // isn't inside a client tile — so chrome keeps
                        // priority and only genuinely client-bound events
                        // are withheld from the chrome dispatch below.
                        let to_client = client_input::route_input(
                            &mut state,
                            &mut frame_builder,
                            &wev,
                            &test_control_ctx.modifiers,
                        );
                        if !to_client {
                            // Dispatch real input through the same wire the
                            // headless `execute_planned_action` path uses.
                            frame_builder.dispatch_event(
                                &wev,
                                &test_control_ctx.modifiers,
                                &mut test_control_ctx.doc,
                                &test_control_ctx.root_obj,
                                &mut test_control_ctx.focus,
                            );
                        }
                    }
                }
                BackendEvent::ModifiersChanged(modifiers) => {
                    // Carry the modifier mask forward so the next event
                    // dispatched into `FrameBuilder::dispatch_event` (or
                    // the lock-chord check above) sees the right
                    // Ctrl/Shift/Alt/Super state.
                    test_control_ctx.modifiers = modifiers;
                }
                BackendEvent::OutputsChanged => {
                    // A display was plugged in, unplugged or re-moded.
                    // Everything derived from the output set has to be
                    // re-derived at once — the scene extent, the per-
                    // display widths strips size against, the size a
                    // fullscreen client is configured to, the cadence, and
                    // the `wl_output` globals clients see.
                    outputs_dirty = true;
                    session_changed = true;
                }
                BackendEvent::ShellGesture(action) => {
                    // A three- or four-finger touchpad gesture, already
                    // resolved into the navigation it means. Never also
                    // delivered to a client, so there is no double-handling
                    // to guard against here.
                    if !session.is_locked() {
                        for t in session.note_activity(Instant::now()) {
                            apply_session_transition(t, backend.as_mut(), present_pipe.as_ref());
                        }
                        apply_shell_gesture(&mut frame_builder, action);
                        session_changed = true;
                    }
                }
                BackendEvent::Gesture(gesture) => {
                    // A one- or two-finger gesture: the focused client's.
                    if !session.is_locked() {
                        state.forward_client_gesture(gesture);
                    }
                }
                BackendEvent::Touch(touch) => {
                    // The same finger also arrives as pointer events (the
                    // backend emits both), which is what drives the shell
                    // chrome. This half is the `wl_touch` protocol for
                    // clients that want real multi-touch.
                    if !session.is_locked() {
                        let target = client_input::touch_target(&state, &frame_builder, &touch);
                        state.forward_client_touch(touch, target);
                    }
                }
                BackendEvent::Redraw | BackendEvent::Focus(_) => {
                    // Redraw is implicit because we re-paint every tick;
                    // Focus changes don't gate input dispatch here.
                }
            }
        }

        // The Settings app writes to a file; nothing tells the compositor.
        // Poll it (cheaply, once a second) so a preference the user just
        // changed reaches the input devices and the display scale.
        if settings_watch.changed(Instant::now()) {
            tracing::info!("compositor: settings changed on disk; re-applying");
            backend.reload_input_settings();
            outputs_dirty = true;
        }

        // Re-derive everything that depends on the output set, once, after
        // the whole event batch — a dock being plugged in raises several
        // udev edges in a row, and doing this per edge would re-advertise
        // globals the next edge immediately supersedes.
        if std::mem::take(&mut outputs_dirty) {
            let outs = backend.outputs().to_vec();
            let combined = combined_geometry(&outs);
            tracing::info!(
                outputs = outs.len(),
                combined = ?combined,
                "compositor: display configuration changed"
            );
            if combined.0 > 0 && combined.1 > 0 {
                frame_builder.set_viewport(combined);
                state.output_size = (combined.0 as i32, combined.1 as i32);
                if let Some(pipe) = present_pipe.as_ref() {
                    pipe.resize(combined);
                }
            }
            // Windows are not pinned to an output: the shell places groups
            // across whatever displays it is told about, so re-pushing the
            // geometry is what re-homes everything that was on the display
            // that went away. Nothing is left at a coordinate with no panel
            // behind it.
            frame_builder.set_output_geometry(output_geometry(&outs));
            // Per-output scale: a surface's preferred `wp_fractional_scale`
            // is the scale of the output it is on, so a changed layout
            // changes what each client should be rendering at.
            state.set_output_layout(&outs);
            output_globals.sync::<State>(
                &display_handle_for_outputs,
                &outs,
                refresh_mhz_of(&*backend),
            );
            if refresh_override.is_none() {
                if let Some(interval) = backend.refresh_interval() {
                    if frame_clock.set_interval(interval) {
                        eprintln!(
                            "nirios compositor: display configuration changed; now pacing to {:.3} Hz",
                            1.0 / interval.as_secs_f64()
                        );
                        metrics.set_target_period(interval);
                    }
                }
            }
        }
        let t_input_done = iter_t0.elapsed();
        metrics.mark_input(t_input_done);

        // Track D2: advance the idle timers and apply any timeout-driven
        // transitions (blank, auto-lock, suspend) for this tick.
        for t in session.tick(Instant::now()) {
            if t == SessionTransition::Suspend {
                // Idle-suspend is gated by the inhibitor registry: an app
                // holding a token (media playing, a task running) keeps the
                // machine awake. When inhibited we un-latch so it re-fires as
                // soon as the last inhibitor releases, without waiting for
                // fresh input.
                if inhibit_registry.is_inhibited() {
                    session.cancel_suspend();
                    tracing::debug!(
                        target: "qos::power",
                        reasons = ?inhibit_registry.active_reasons(),
                        "idle-suspend deferred: inhibitor held"
                    );
                } else {
                    tracing::info!(target: "qos::power", "idle threshold reached: requesting suspend");
                    power_bridge.request_suspend();
                }
            } else {
                apply_session_transition(t, backend.as_mut(), present_pipe.as_ref());
            }
            // Any session transition (blank / lock / suspend) changes what
            // should be on screen, so force this tick to repaint.
            session_changed = true;
        }

        // Apply signals the async power service raised (lid/critical lock,
        // battery toasts, resume). Suspend requests flow the other way, via
        // `power_bridge.request_suspend()` above.
        for sig in power_bridge.drain_signals() {
            session_changed = true;
            match sig {
                qos_power_mgmt::PowerSignal::Lock => {
                    for t in session.request_lock(Instant::now()) {
                        apply_session_transition(t, backend.as_mut(), present_pipe.as_ref());
                    }
                }
                qos_power_mgmt::PowerSignal::ResumedFromSleep => {
                    // Reset the idle timer so a stale pre-sleep timestamp does
                    // not immediately re-blank/re-suspend on resume.
                    for t in session.note_activity(Instant::now()) {
                        apply_session_transition(t, backend.as_mut(), present_pipe.as_ref());
                    }
                    // Put the display back. A suspend does not pause the
                    // seat, so no session event fires on the way out of it
                    // — and the DRM device comes back with no mode set, no
                    // framebuffer bound and a cleared cursor plane. Without
                    // this, closing and opening the lid returns to a black
                    // screen that no amount of input recovers.
                    backend.reset_display();
                    // A resumed panel is scanning out whatever was left in
                    // its buffers, so the next frame has to be a full one.
                    outputs_dirty = false;
                    session_changed = true;
                }
                qos_power_mgmt::PowerSignal::Notify(n) => {
                    let inc = power_notification_to_incoming(&notif_bridge, n);
                    notif_bridge.push_incoming(inc);
                }
            }
        }

        // `ext_session_lock_v1`: fold a protocol client's lock/unlock into
        // the one authoritative session lock. A client `lock` request drives
        // the same locked state (and overlay) as the chord/idle path; a client
        // `unlock_and_destroy` clears it (the client authenticated on its own
        // side — the compositor's password path is for chord/idle locks). The
        // stashed `SessionLocker` is confirmed after the overlay is painted,
        // below, so the client's `locked` event only fires once content is
        // hidden.
        if state.session_lock.take_lock_requested() {
            session_changed = true;
            for t in session.request_lock(Instant::now()) {
                apply_session_transition(t, backend.as_mut(), present_pipe.as_ref());
            }
        }
        if state.session_lock.take_unlock_requested() {
            session_changed = true;
            for t in session.request_unlock(Instant::now()) {
                apply_session_transition(t, backend.as_mut(), present_pipe.as_ref());
            }
        }

        // Async unlock: pick up the result of any in-flight credential verify
        // running on the worker thread (started by an Enter on the lock
        // screen). The compositor loop is never blocked while the verify runs
        // — it keeps ticking and only unlocks here, once the worker reports a
        // verified success. A wrong/failed result surfaces a non-leaky error
        // and stays locked. Polling every tick (~16 ms) picks the result up
        // promptly without busy-waiting.
        if let Some(outcome) = lock.poll_verification() {
            // The field/status changed (unlocked, or error shown) — repaint.
            session_changed = true;
            if outcome == UnlockOutcome::Unlocked {
                for t in session.request_unlock(Instant::now()) {
                    apply_session_transition(t, backend.as_mut(), present_pipe.as_ref());
                }
            }
        }

        // Reset the lock screen's input the instant the lock engages, so a
        // fresh lock always starts with an empty field and no stale error
        // carried over from a previous lock episode.
        if session.is_locked() && !prev_locked {
            lock.reset();
        }
        prev_locked = session.is_locked();

        // Track D1: pump the D-Bus notification bridge into the live
        // toast stack and mirror the lock state into the chrome so the
        // stack can suppress content while locked. Cheap no-ops when no
        // bridge is installed / state is unchanged.
        frame_builder.with_chrome(|st| {
            st.set_session_locked(session.is_locked());
            st.pump_dbus_notifications();
        });

        // Mirror the compositor's live fullscreen state (set by the
        // `XdgShellHandler` fullscreen/unfullscreen path) into the rendered
        // scene, so the promoted group takes over the whole output and a later
        // exit restores the exact prior tiling. The overlay host guards against
        // a group id it doesn't host — the live client tree and the rendered
        // tree are not yet unified (Phase 4), so an unknown id is a safe no-op
        // rather than blanking the shell.
        frame_builder.set_fullscreen(state.fullscreen_group);

        {
            // Mirror foreign toplevels adopted at the protocol layer into the
            // rendered tab tree, and reap tabs whose client went away. This is
            // a diff of the protocol tree against the rendered one, so it is
            // idempotent and self-healing — a dropped event can't leave a
            // phantom tab behind. A real change bumps the chrome revision,
            // which drives the strip resync that splices the tab (and
            // therefore the surface's widget) into the live widget tree on the
            // next layout pass.
            let mirrored = {
                let chrome = frame_builder.chrome();
                let mut rendered = chrome.tree_mut();
                foreign::reconcile_foreign_tabs(&state.workspace_tree, &mut rendered)
            };
            if mirrored {
                frame_builder.with_chrome(|st| st.bump());
            }
            // The mirror above is the single source of tab adoption, so the
            // input router's own adoption queues are pure bookkeeping here.
            // Drain them each tick so they can't grow across a long session.
            state.client_input.clear_pending();
        }
        // Reconcile the seat's keyboard focus with the focused tab. Runs
        // every tick so a tab / group / workspace switch, an overlay
        // opening, or a window closing all move the keyboard immediately —
        // not only on the next keystroke.
        client_input::sync_keyboard_focus(&mut state, &frame_builder);

        // A file dragged out of the hosted file manager over a client's
        // window becomes a real Wayland drag here — see `pump_chrome_drag`.
        client_input::pump_chrome_drag(&mut state, &frame_builder);

        // Hand the remote-input firewall this frame's screen: which
        // foreign tiles are up and where, which one holds the keyboard,
        // whether chrome is drawn over the lot, and whether the session
        // is locked. Every coordinate and focus check a peer's input
        // faces is answered from this, so it has to be refreshed after
        // focus is reconciled and before any remote event is drained.
        client_input::publish_remote_input_view(&mut state, &frame_builder, session.is_locked());

        // Per-output HiDPI: tell each client the scale of the display its
        // tile is currently on. Runs every tick because a window dragged
        // from the laptop panel to an external monitor has to be told to
        // re-render at the other one's density, and nothing else observes
        // that move.
        {
            let placements: Vec<(u64, (f64, f64))> = state
                .client_input
                .placements()
                .filter_map(|(surface_id, group)| {
                    let rect = frame_builder.host().active_tile_rect(group)?;
                    Some((surface_id, (rect.x0, rect.y0)))
                })
                .collect();
            state.update_surface_scales(&placements);
        }

        // Decide whether this frame needs repainting *before* doing the
        // expensive layout pass. Both queries below are cheap and don't
        // need a fresh layout: `needs_animation_frame` just walks the
        // widget tree, and the chrome revision is a plain counter bumped
        // by state mutations (input handlers, the notification pump, the
        // fullscreen mirror above). Skipping `relayout` + `paint` entirely
        // on idle frames is what lets the loop fall to near-zero CPU at
        // rest instead of burning a core re-laying-out an unchanged tree.
        // Hit-testing stays correct because a tick that dispatched input
        // always relayouts — either as part of the paint below, or
        // explicitly afterwards when the cadence gate deferred the paint.
        // Pump the widget tree's per-frame state (sync + update) before
        // deciding whether to paint. This is what drains a hosted terminal's
        // PTY into its grid; without it a live app that produces output
        // asynchronously would sit frozen on its empty state. Returns true
        // when something actually changed, which is itself a paint reason.
        // Answer whatever asked to be opened since the last frame. Here
        // rather than on the broker's reader thread because the answer is
        // the workspace's — its capabilities decide, and its launcher is
        // what acts — and both live on this one.
        if let Some(broker) = collab_broker.as_ref() {
            crate::shell::agent_opens::pump(
                &frame_builder.chrome_handle(),
                broker,
                &pending_opens,
            );
            // Draw whatever asked for a compose surface, then put whatever was
            // written into one in the room. Both here rather than on the
            // reader thread: the first needs to know where the asking window
            // is, and the second needs the roster and the room.
            for req in crate::shell::remark_surface::take_requests(&pending_composes) {
                let origin = frame_builder.host().focused_app_origin();
                match origin {
                    Some(origin) => {
                        let peer = req.peer;
                        if frame_builder
                            .host_mut()
                            .open_remark(req.reference, req.beside, origin)
                        {
                            // Remembered so the application can be told when
                            // the panel goes. It has a context menu open
                            // beside a panel it cannot see and will never see
                            // the key that ends it.
                            frame_builder.host_mut().set_remark_peer(peer);
                        }
                    }
                    // Nothing focused hosts a window, so there is nothing to
                    // sit beside. Said rather than placed at a guess: a panel
                    // in an arbitrary corner is worse than none, because it
                    // reads as belonging to whatever it happens to land on.
                    None => tracing::warn!(
                        "an application asked for a compose surface, but no \
                         focused window could be found to place it against",
                    ),
                }
            }
            let written = frame_builder.host_mut().take_remarks();
            crate::shell::remark_surface::deliver(
                &frame_builder.chrome_handle(),
                broker,
                written,
            );
            for peer in frame_builder.host_mut().take_remark_dismissals() {
                broker.remark_surface_closed(peer);
            }
        }
        // Drain the network actor into the replica. Peer changes land
        // in the shared tree, so a room's new name or a capability the
        // owner just revoked is folded in before this frame's pump
        // reads it — and the revision bump that follows is what makes
        // the frame repaint.
        if let Some(net) = peer_sync.as_ref() {
            net.tick();
        }
        // Keep the presence relay current, then fold in what peers sent.
        //
        // The pointer is read level-triggered from the input state
        // rather than hooked onto motion, so every source that can move
        // it — a real seat, a remote peer's injected input, the test
        // control socket — is covered by one place. The room set is
        // re-read for the same reason: a workspace joins a peer's room
        // when an invite is accepted, and the subscription has to follow
        // it there or the session stops hearing the room it just joined.
        let peers_moved = if let Some(relay) = frame_builder.peer_presence() {
            // Where this session is pointing *now* — not merely where
            // its pointer was last put. This publisher is
            // level-triggered and restates a stationary pointer on a
            // keepalive, so whatever is answered here is announced to
            // the room for as long as it keeps being answered.
            //
            // Two different non-answers, and both had to be taught.
            // `cursor()` reports the origin for a pointer that has
            // never been anywhere, which put a cursor in the corner of
            // every peer's screen for somebody who was not there.
            // `pointer_position()` fixed that and reports the truth —
            // but the truth outlives its usefulness: a session gets a
            // real position the first time anything presses a button
            // on it, and pressing a button is how a session joins a
            // room, so one press left a pill sitting on every peer's
            // desktop at a control that had long since been dismissed.
            //
            // `live_pointer` is the question the room is actually
            // asking, and it weighs the two ways a pointer arrives
            // somewhere differently. A pointer somebody *moved* and
            // left keeps the full idle horizon, because a cursor really
            // is sitting there while they read. A pointer whose last
            // word was a *press* is asked to prove itself again within
            // seconds, because a press routinely destroys the control it
            // acted on — and one press on a **Join** button is how a
            // session enters a room at all.
            //
            // One horizon could not do both. In the run this was
            // measured on, the press had to be retired inside about
            // eight seconds while a person who had moved their mouse
            // had to survive nearly seven, and tuning a single number
            // into a gap that narrow is a coin toss, not a fix. Any
            // motion at all — the least contact with a real mouse —
            // puts a position back on the long horizon.
            // Published where it is in the *workspace*, not on this
            // screen.
            //
            // The pointer arrives in scene space, and scene space moves
            // when the strip scrolls. Sending it raw meant a peer painted
            // the dot at a fixed spot on their own display, so scrolling
            // slid every window out from under it and somebody else's
            // cursor appeared to follow yours about. Adding this strip's
            // own offset makes the number mean a place in the workspace,
            // which is the only thing two machines with different
            // viewports can agree on; the overlay takes its own offset
            // back off when it paints. See `ChromeState::strip_scroll_x`.
            let scroll_x = {
                let chrome = frame_builder.chrome_handle();
                let chrome = chrome.borrow();
                chrome.strip_scroll_x
            };
            relay.set_cursor_position(
                state
                    .client_input
                    .live_pointer(
                        std::time::Instant::now(),
                        POINTER_IDLE,
                        POINTER_ACTUATED_IDLE,
                    )
                    .map(|(x, y)| (x + scroll_x, y)),
            );
            let (rooms, here) = {
                let chrome = frame_builder.chrome_handle();
                let chrome = chrome.borrow();
                let tree = chrome.tree();
                let rooms = tree
                    .workspaces
                    .iter()
                    .map(|w| w.room.to_string())
                    .collect::<Vec<_>>();
                // The room the user is looking at, which is where their
                // pointer is. Distinct from the set above: that is every
                // room the session listens to.
                let here = tree
                    .workspaces
                    .iter()
                    .find(|w| w.id == tree.active)
                    .map(|w| w.room.to_string());
                (rooms, here)
            };
            relay.set_rooms(rooms);
            // Say where this session's pointer is, whether or not
            // anything is running in front of it. The app-driven
            // republish only speaks while a local app publishes a
            // pointer, so on a desktop with nothing open it never
            // spoke — and two people in one room watched each other
            // not appear.
            if let Some(here) = here.as_deref() {
                relay.publish_local_cursor(here);
            }
            frame_builder.pump_peer_presence()
        } else {
            false
        };
        let apps_changed =
            frame_builder.pump_apps(&mut test_control_ctx.doc, &test_control_ctx.root_obj);
        let revision = frame_builder.chrome_revision();
        let revision_changed = revision != last_revision;
        let animating = frame_builder.needs_animation_frame();
        let keepalive_due = last_paint.elapsed() >= keepalive;
        metrics.mark_pump(iter_t0.elapsed() - t_input_done);
        // Is there anything new to show? This is the redraw gating that
        // lets a static desktop idle at near-zero CPU; it is orthogonal
        // to *when* a frame may be shown, which is the clock's job below.
        let content_changed = force_first_paint
            || had_backend_events
            || session_changed
            || revision_changed
            || animating
            || apps_changed
            || peers_moved
            || keepalive_due;
        // May a frame be shown yet? Protocol traffic, test-control
        // commands and input all wake the loop off-cadence; presenting
        // on those wakeups would submit frames inside a refresh interval
        // that the display simply discards — and, with a render thread,
        // would fill the queue with them. The first frame is exempt so
        // startup isn't delayed by an interval.
        let cadence_due = force_first_paint || frame_clock.due(iter_t0);
        // Is there room in the pipe? When presentation runs on its own
        // thread and the GPU is behind, building another scene would only
        // grow the queue — and with it the input-to-photon path. Declining
        // here caps that at `QUEUE_DEPTH + 1` frames; the tick still
        // services input and clients, it just doesn't render.
        let pipe_ready = present_pipe.as_ref().is_none_or(|p| p.has_capacity());
        if content_changed && cadence_due && !pipe_ready {
            if let Some(p) = present_pipe.as_mut() {
                p.note_backpressure();
            }
        }
        let should_paint = content_changed && cadence_due && pipe_ready;

        // Diagnostic: `QOS_PAINT_STATS=1` logs, every ~2 s, how many
        // frames painted vs idled and which condition forced the repaints.
        // Lets us confirm the render loop actually idles (and pinpoint any
        // perpetual-animation source) without guessing.
        if paint_stats {
            frames_total += 1;
            if should_paint {
                frames_painted += 1;
                if had_backend_events {
                    cause_events += 1;
                }
                if session_changed {
                    cause_session += 1;
                }
                if revision_changed {
                    cause_revision += 1;
                }
                if animating {
                    cause_anim += 1;
                }
                if keepalive_due
                    && !(had_backend_events || session_changed || revision_changed || animating)
                {
                    cause_keepalive += 1;
                }
            }
            if stats_since.elapsed() >= Duration::from_secs(2) {
                eprintln!(
                    "paint-stats: {frames_painted}/{frames_total} painted | \
                     events={cause_events} revision={cause_revision} anim={cause_anim} \
                     session={cause_session} keepalive={cause_keepalive} | \
                     {:.3} Hz target | present={} backpressure={}",
                    1.0 / frame_clock.interval().as_secs_f64(),
                    present_pipe
                        .as_ref()
                        .map(|p| p.presented().to_string())
                        .unwrap_or_else(|| "inline".to_string()),
                    present_pipe.as_ref().map(|p| p.backpressured()).unwrap_or(0),
                );
                frames_total = 0;
                frames_painted = 0;
                cause_events = 0;
                cause_revision = 0;
                cause_anim = 0;
                cause_session = 0;
                cause_keepalive = 0;
                stats_since = Instant::now();
            }
        }

        if should_paint {
            // Lay out only when we're about to paint. Any structural change
            // (spliced group, tab switch, resize) is applied here, right
            // before the scene is built.
            let t_layout_start = Instant::now();
            frame_builder.relayout();
            let layout_took = t_layout_start.elapsed();
            metrics.mark_layout(layout_took);
            let t_scene_start = Instant::now();
            // When locked, paint the compositor-owned lock overlay instead
            // of the chrome — app content is never submitted while locked.
            let scene = if session.is_locked() {
                let theme = frame_builder.theme().clone();
                // The lock overlay covers **every** output the compositor
                // knows about, drawing a centred widget on each (single-output
                // today on winit/headless; the DRM backend lays real monitors
                // out side-by-side). Fall back to a single output spanning the
                // current viewport if the backend reports none yet.
                let lock_outputs: Vec<LockOutput> = backend
                    .outputs()
                    .iter()
                    .map(|o| LockOutput {
                        origin: (o.position.0 as f64, o.position.1 as f64),
                        size: (o.size.0 as f64, o.size.1 as f64),
                    })
                    .collect();
                let lock_outputs = if lock_outputs.is_empty() {
                    let vp = frame_builder.viewport();
                    vec![LockOutput {
                        origin: (0.0, 0.0),
                        size: (vp.0, vp.1),
                    }]
                } else {
                    lock_outputs
                };
                // Count-only summary on the lock screen — no bodies leak.
                let notif_count = frame_builder.chrome().notification_count();
                // Wall clock/date computed here (not in the pure paint) so the
                // lock surface renders as compositor chrome, themed.
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
                };
                lock_screen::draw_lock_scene(
                    &lock_outputs,
                    &theme,
                    frame_builder.text_renderer_mut(),
                    &view,
                )
            } else {
                let mut scene = frame_builder.paint();
                // On top of everything, because a drag is over whatever
                // it is over. Drawn here rather than by either app: once
                // the pointer has left the source window neither end can
                // paint the thing being carried, so without this a drag
                // across the desktop is invisible.
                //
                // And the pointer itself whenever the backend has no
                // plane of its own to put one on. Without it a
                // display-free session renders a desktop nothing is
                // pointing at, while input lands on it all the same:
                // overlays open and buttons actuate with no visible
                // cause. `pointer_position` rather than `cursor`, so a
                // session whose pointer has genuinely never moved does
                // not get an arrow parked in the top-left corner.
                let dragging =
                    state.selection.dnd_active || state.selection.chrome_drag.is_some();
                if dragging || !backend_has_cursor_plane {
                    if let Some((cx, cy)) = state.client_input.pointer_position() {
                        let label = dragging.then(|| state.selection.dnd_label.get()).flatten();
                        let pressed = state.client_input.buttons_down() > 0;
                        let theme = frame_builder.theme().clone();
                        drag_feedback::draw(
                            &mut scene,
                            vello::kurbo::Point::new(cx, cy),
                            label.as_deref(),
                            pressed,
                            &theme,
                            frame_builder.text_renderer_mut(),
                        );
                    }
                }
                scene
            };
            metrics.mark_scene(t_scene_start.elapsed());
            let t_submit_start = Instant::now();
            let mut render_thread_lost = false;
            let async_present = match present_pipe.as_mut() {
                Some(pipe) => {
                    render_thread_lost = !pipe.submit(scene, OutputId(0));
                    true
                }
                None => {
                    backend.submit_frame(&scene, OutputId(0));
                    false
                }
            };
            if render_thread_lost {
                // Presentation is dead but the session isn't: clients stay
                // served and the compositor keeps its protocol state, which
                // is a far better failure mode than tearing the seat down.
                eprintln!(
                    "nirios compositor: render thread gone; presentation stopped \
                     (clients keep running)"
                );
                present_pipe = None;
            }
            let submit_done = Instant::now();
            metrics.mark_submit(
                submit_done.saturating_duration_since(t_submit_start),
                async_present,
                submit_done,
            );
            // Advance the deadline series off the moment the frame was
            // handed to the display. On the inline path that is the
            // present itself; on the threaded path it is the hand-off,
            // which is the last instant the logic thread can observe —
            // the clock's job is to pace *production*, and the render
            // thread's own vsync wait paces consumption behind it.
            frame_clock.note_presented(submit_done);
            last_paint = Instant::now();
            // Re-read after `relayout`: its `resync_strips_from_tree` pass
            // can bump the revision, and we must capture that post-paint
            // value so the next idle tick doesn't see a phantom change.
            last_revision = frame_builder.chrome_revision();
            force_first_paint = false;
            // The layout pass above published each foreign tab's rect.
            // Tell the clients what size to render at, then release the
            // frame throttle for the ones that are actually on screen so
            // they produce their next buffer. Both are no-ops when no
            // external client is connected.
            foreign::flush_configures(&mut state);
            foreign::release_frame_throttle(&mut state);
        } else if had_backend_events {
            // Input was dispatched but the frame was deferred (waiting for
            // the next presentation deadline, or for the render thread to
            // drain). Lay out anyway so the *next* event hit-tests against
            // rects that reflect what this one did — a click that opens an
            // overlay must not be followed by a click resolved against the
            // pre-overlay tree. Layout is ~0.01 ms; deferring it to save
            // that would be trading correctness for nothing.
            frame_builder.relayout();
        }
        let t_paint_done = iter_t0.elapsed();

        // Confirm an `ext_session_lock_v1` client's lock only after the lock
        // overlay has been presented and the session is actually locked, so
        // the protocol's `locked` event (sent by `SessionLocker::lock`) never
        // fires while app content could still be on screen. If the lock did
        // not take (dropping the locker), the protocol notifies the client of
        // the failure automatically.
        if session.is_locked() {
            if let Some(locker) = state.session_lock.take_pending_locker() {
                locker.lock();
            }
        }

        // Real-input e2e Phase 1b: accept any new test-control
        // connections, then drain pending bytes from each open one
        // (each call may complete zero or more commands, parses
        // them via `test_control::parse_command`, and writes JSON
        // responses back). Closed connections (EOF or error) get
        // collected at the bottom of the loop. The listener stays
        // non-blocking so the frame-pacing loop never stalls on
        // test traffic.
        if let Some(listener) = &test_control_listener {
            loop {
                match listener.accept() {
                    Ok((stream, _addr)) => {
                        if let Some(conn) = TestControlConn::new(stream) {
                            test_control_conns.push(conn);
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => {
                        eprintln!("nirios compositor: test_control accept: {e}");
                        break;
                    }
                }
            }
            for conn in test_control_conns.iter_mut() {
                let injected = pump_test_control_conn(
                    conn,
                    &mut frame_builder,
                    &mut test_control_ctx,
                    &session,
                    &mut lock,
        &mut state,
                );
                if injected {
                    // Socket-driven input starts the same latency clock a
                    // hardware event does, so a headless harness measures
                    // the real input-to-present path. The socket is
                    // serviced *after* this tick's paint gate, so the
                    // frame reflecting it is the next one — which is
                    // exactly the latency a caller experiences, and is
                    // what the measurement should report.
                    metrics.note_input(Instant::now());
                }
            }
            if !test_control_conns.is_empty() {
                last_test_control = Some(Instant::now());
            }
            test_control_conns.retain(|c| !c.closed);
        }
        let t_testctl_done = iter_t0.elapsed();
        metrics.mark_testctl(t_testctl_done - t_paint_done);

        // Phase 7 fix: drain wayland client traffic + push pending
        // protocol updates back out. `dispatch_clients` advances each
        // client's request queue (registry, get_compositor, get_xdg_surface,
        // commit, …) and routes them through the `*Handler` impls.
        // `flush_clients` pumps server-originated events (configure, …)
        // back out to clients. Mirrors smithay 0.7's `examples/minimal.rs`.
        if let Err(err) = display.dispatch_clients(&mut state) {
            eprintln!("nirios compositor: dispatch_clients error: {err}");
            break;
        }
        // Selection bookkeeping, after the dispatch that may have produced it.
        // Publishes anything in-process chrome copied since the last tick, and
        // pulls anything a client copied into the compositor's mirror. Both
        // halves have to run here rather than inside the protocol handler:
        // smithay raises `new_selection` *before* it installs the new source,
        // so an inline pull would read the previous owner.
        state.pump_selection();
        if let Err(err) = display.flush_clients() {
            eprintln!("nirios compositor: flush_clients error: {err}");
            break;
        }
        let t_work_done = iter_t0.elapsed();
        metrics.mark_clients(t_work_done - t_testctl_done);
        if loop_timing && t_work_done > Duration::from_millis(100) {
            eprintln!(
                "SLOW-ITER total_work={:?} | input={:?} paint=+{:?} testctl=+{:?} dispatch_clients=+{:?}",
                t_work_done,
                t_input_done,
                t_paint_done - t_input_done,
                t_testctl_done - t_paint_done,
                t_work_done - t_testctl_done,
            );
        }

        // Pump any pending calloop sources (the listening socket
        // included) and sleep up to one frame. The calloop loop itself
        // dispatches sources on each iteration; this is what lets
        // newly-accepted connections fan out into `insert_client` calls.
        // Park until the next presentation deadline. See
        // `FrameClock::park` for why this is a question about the
        // deadline rather than about whether this tick painted: anything
        // that wakes the loop mid-interval (the render thread retiring a
        // frame, protocol traffic, input) must not be able to push the
        // next frame past its deadline.
        let t_sleep_start = Instant::now();
        let mut park = frame_clock.park(t_sleep_start);
        if last_test_control
            .is_some_and(|t| t_sleep_start.duration_since(t) < TEST_CONTROL_ACTIVE_FOR)
        {
            park = park.min(TEST_CONTROL_PARK);
        }
        if let Err(err) = event_loop.dispatch(park, &mut state) {
            eprintln!("nirios compositor: event loop dispatch error: {err}");
            break;
        }
        metrics.mark_sleep(t_sleep_start.elapsed());
        metrics.end_tick();
        if let Some(deadline) = run_deadline {
            if Instant::now() >= deadline {
                close_requested = true;
            }
        }
        let _ = &state.display_handle;

        // Phase 7 test introspection: serialize the live `WorkspaceTree`
        // to the requested path. We write to a tempfile-in-the-same-dir
        // and rename to make readers see a consistent snapshot (the test
        // polls the file concurrently with the frame loop writing it).
        if let Some(path) = test_dump_path.as_ref() {
            if let Ok(json) = serde_json::to_vec(&state.workspace_tree) {
                if let Err(e) =
                    atomic_file::write_creating_parent(std::path::Path::new(path), &json)
                {
                    tracing::warn!(error = %e, path = ?path, "workspace tree save failed");
                }
            }
        }
    }

    // Stop the render thread and wait for the frame in flight, so a
    // half-presented frame can't outlive the device it was rendering
    // into. `process::exit` below skips destructors, so this is explicit.
    if let Some(mut pipe) = present_pipe.take() {
        pipe.shutdown();
    }

    // Flush the frame trace / write the summary before `process::exit`
    // skips destructors. A no-op unless instrumentation was enabled.
    metrics.finish();

    // `event_loop.run` only returns when the loop is asked to stop; we never
    // do that today, but keep an explicit exit so the function's `!` return
    // type is honoured. `display` is dropped here, severing its clients.
    drop(display);
    // D-3: explicitly drop the test-control socket guard so its
    // `Drop` impl unlinks the socket file before we hand control to
    // `process::exit` (which skips destructors). On the panic path
    // the unwind already runs `Drop` so we get cleanup either way.
    drop(_test_control_unlink);
    // Same reasoning for the collab broker: its `Drop` quiesces the
    // drain thread and unlinks both sockets, and `process::exit`
    // would skip it — leaving a socket file behind that the next
    // session has to tell apart from a live one.
    drop(collab_broker);
    std::process::exit(0)
}

#[cfg(test)]
mod tests {
    //! Phase 2b unit coverage for the test-control execution path.
    //!
    //! The integration test (`tests/control_socket_test.rs`) needs a
    //! Wayland display and is `#[ignore]` by default; these unit tests
    //! exercise the same `execute_planned_action` → `FrameBuilder::dispatch_event`
    //! wire without spinning up a process, so a CI run that can't open
    //! winit still gates against regressions here.
    use super::*;

    /// The compositor binds the socket the launcher dials.
    ///
    /// This is the check that was missing when the two sides drifted:
    /// the broker bound whatever `QUICK_COLLAB_SOCK` happened to name
    /// while the launcher resolved `$XDG_RUNTIME_DIR/qos/services/
    /// collab.sock`, and because every manifest asks for `collab` as
    /// `optional`, the mismatch surfaced as a log line and an app that
    /// quietly never collaborated.
    ///
    /// So assert across both production paths rather than on either
    /// one's idea of the path: bind with the compositor's own spawn
    /// function, then resolve with the launcher's own resolver, and
    /// require it to come back with an open descriptor. Declaring the
    /// requirement `required@1` here is what makes a divergence an
    /// error instead of the skip an optional service is entitled to —
    /// a test that cannot observe its subject must fail.
    #[test]
    #[serial_test::serial(process_env)]
    fn compositor_binds_the_socket_the_launcher_dials() {
        let runtime = tempfile::tempdir().expect("tempdir");
        // SAFETY: `serial(process_env)` keeps this off any other
        // test's env. The group name is load-bearing: serial_test only
        // excludes tests that name the *same* group, and every other
        // test in this crate that writes `XDG_RUNTIME_DIR` names
        // `process_env`. Under the unnamed group this ran alongside
        // them, read the variable after one of them had pointed it at
        // an already-dropped tempdir, and failed to bind a socket in a
        // directory that no longer existed.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", runtime.path()) };
        // Do not let an inherited value pick the legacy socket's home.
        unsafe { std::env::remove_var("QUICK_COLLAB_SOCK") };

        let fb = FrameBuilder::new((800, 600));
        let chrome = fb.chrome_handle();
        let broker = spawn_session_collab_broker(&chrome, None)
            .expect("the compositor must be able to bind its own collab socket");

        assert!(
            chrome.borrow().collab_available(),
            "a session that bound its broker must not report collaboration unavailable: {:?}",
            chrome.borrow().collab_unavailable
        );

        // The launcher's real resolver, on a manifest that demands the
        // service. `resolve_service_binds` returns `RequiredMissing` if
        // it looks somewhere the compositor did not bind.
        let mut services = std::collections::BTreeMap::new();
        services.insert(
            launcher::services::COLLAB_SERVICE.to_string(),
            "required@1".to_string(),
        );
        let resolved = launcher::services::resolve_service_binds(&services, false)
            .expect("the launcher must find the socket the compositor just bound");

        assert_eq!(
            resolved.len(),
            1,
            "expected exactly one resolved service, got {resolved:?}"
        );
        assert_eq!(resolved[0].name, launcher::services::COLLAB_SERVICE);
        assert!(
            resolved[0].host_fd >= 0,
            "the launcher must hand the app a real connected descriptor"
        );

        drop(broker);
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };
    }

    /// A live socket is not taken away from the session that owns it.
    ///
    /// Two compositors against one `XDG_RUNTIME_DIR` is the shape a
    /// second login (or a stray test) produces, and `spawn_full`
    /// unlinks before it binds — so without the liveness probe the
    /// newcomer would silently strand the first session's apps on a
    /// broker nothing can reach any more. Refusing loudly is the
    /// correct outcome, and the refusal has to reach the chrome.
    #[test]
    #[serial_test::serial(process_env)]
    fn a_second_session_refuses_to_take_a_live_collab_socket() {
        let runtime = tempfile::tempdir().expect("tempdir");
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", runtime.path()) };
        unsafe { std::env::remove_var("QUICK_COLLAB_SOCK") };

        let first_fb = FrameBuilder::new((800, 600));
        let first_chrome = first_fb.chrome_handle();
        let first = spawn_session_collab_broker(&first_chrome, None).expect("first broker binds");

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
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };
    }

    // ---- developer surfaces are absent from a shipped build ------------

    #[test]
    fn shipped_build_never_binds_a_test_control_socket() {
        assert_eq!(
            test_control_path(false, Some(std::ffi::OsString::from("/tmp/ctl.sock"))),
            None,
            "a release compositor must not open a remote-control socket"
        );
    }

    #[test]
    fn dev_build_binds_the_test_control_socket_the_harness_asks_for() {
        assert_eq!(
            test_control_path(true, Some(std::ffi::OsString::from("/tmp/ctl.sock"))),
            Some(PathBuf::from("/tmp/ctl.sock")),
        );
        assert_eq!(test_control_path(true, None), None);
    }

    #[test]
    fn shipped_build_never_dumps_its_workspace_tree() {
        assert_eq!(
            test_state_dump_path(false, Some(std::ffi::OsString::from("/tmp/tree.json"))),
            None,
        );
        assert_eq!(
            test_state_dump_path(true, Some(std::ffi::OsString::from("/tmp/tree.json"))),
            Some(std::ffi::OsString::from("/tmp/tree.json")),
        );
    }

    #[test]
    fn the_gate_the_compositor_actually_uses_is_the_build_profile() {
        // The two functions above are only meaningful if `run` passes the
        // real constant to them. Tie the assertions to it so flipping the
        // production call site to a literal `true` fails here.
        assert_eq!(
            crate::compositor::dev_env::DEV_ENV_OVERRIDES,
            cfg!(debug_assertions)
        );
    }

    // ---- publishing the socket name for the nested dev loop ------------

    #[test]
    fn display_path_lands_under_the_runtime_dir() {
        assert_eq!(
            session_display_path(Some(std::ffi::OsString::from("/run/user/1000"))),
            Some(PathBuf::from("/run/user/1000/qos-session/wayland-display")),
        );
    }

    #[test]
    fn display_path_declines_an_unset_or_relative_runtime_dir() {
        assert_eq!(session_display_path(None), None);
        assert_eq!(
            session_display_path(Some(std::ffi::OsString::from("relative/dir"))),
            None,
            "a session-scoped file must not be placed by a relative path"
        );
    }

    #[test]
    fn published_display_file_is_exactly_the_socket_name() {
        // Round-trip through the real writer: what lands on disk has to be
        // usable as `WAYLAND_DISPLAY=$(cat …)`, i.e. no trailing newline
        // and no quoting.
        let dir = std::env::temp_dir().join(format!("qos-display-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp runtime dir");
        let path = session_display_path(Some(dir.clone().into_os_string()))
            .expect("absolute temp dir yields a path");
        std::fs::create_dir_all(path.parent().unwrap()).expect("create qos-session dir");
        atomic_file::write_with_mode(&path, b"wayland-7", 0o600).expect("write display file");

        let read = std::fs::read_to_string(&path).expect("read back");
        assert_eq!(read, "wayland-7");

        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "session file must not be readable by others");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// These unit tests exercise the *chrome* half of the executor with no
    /// compositor `State` attached — building one needs a live wayland
    /// `Display`. Shadowing the production function here keeps the call
    /// sites below unchanged while still running the real executor. The
    /// client-routing half is covered by `compositor::client_input`'s own
    /// tests and by `tests/client_input_routing.rs`.
    fn execute_planned_action(
        action: &PlannedAction,
        fb: &mut FrameBuilder,
        ctx: &mut TestControlCtx,
    ) {
        super::execute_planned_action(action, fb, ctx, None);
    }

    /// Sanity: `CursorMoved` planned action updates the context's cached
    /// cursor and dispatches through the FrameBuilder without panicking.
    /// The cursor cache matters because subsequent `MouseInput` events
    /// (which don't carry positions on winit) get hit-tested against
    /// `last_cursor` inside the host.
    #[test]
    fn cursor_moved_action_updates_ctx_cursor() {
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

    /// The full click sequence (`CursorMoved` + button-down + button-up)
    /// dispatches all three events without panicking. This is the
    /// minimum bar: Phase 2a returns these three actions for a
    /// `click_at` command; Phase 2b's contract is that all three reach
    /// `ShellOverlayHost::handle_event` and the host doesn't crash on
    /// any of them.
    #[test]
    fn full_click_sequence_dispatches_without_panic() {
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

    /// Click sequence aimed at the wheel-disc's plus slot mutates the
    /// chrome state — proves the planned actions reach the host's
    /// click handlers, not just dispatch into a void.
    ///
    /// The bootstrap workspace tree seeds three demo workspaces; pressing
    /// the wheel-disc plus slot calls `chrome.add_workspace("Untitled")`,
    /// which grows that count to four. Asserting on the count is more
    /// robust than asserting on internal widget state because the chrome
    /// is the contract every consumer sees.
    #[test]
    fn click_on_wheel_plus_slot_grows_workspace_count() {
        let mut fb = FrameBuilder::new((1600, 900));
        // Paint once so the wheel-disc populates its plus_rect — the
        // press handler hit-tests against it, and it's `Rect::ZERO`
        // until the first draw pass runs.
        let _ = fb.paint();
        // Find the wheel disc rect by its opt-in `test_id`, then search
        // it for the affordance rather than re-deriving where `draw`
        // puts it. Sweeping only the footprint's vertical centre-line
        // was enough while the plus slot sat on that line; it is not a
        // property of the design, and the sweep silently stopped
        // crossing the affordance when the slot became the tail row of
        // the rail column. Scanning the whole footprint asserts the
        // same thing about a layout the test does not have to know.
        let registry = fb.widget_registry();
        let wheel_rect = registry
            .get("wheel_disc")
            .copied()
            .expect("wheel_disc test_id resolves on bootstrap tree");

        let initial_count = fb.chrome().tree().workspaces.len();

        // Sweep the footprint from the bottom up in 4 px rows, 8 px
        // across, clicking until one lands on the affordance. Clicks
        // that miss are harmless here: a press on the rim starts a drag
        // that a release with no motion snaps straight back, and a press
        // on a workspace row switches workspace — neither creates one,
        // so only the affordance can satisfy the assertion.
        let mut ctx = TestControlCtx::new();
        let mut found = false;
        let mut y = wheel_rect.y1 - 4.0;
        'sweep: while y > wheel_rect.y0 {
            let mut x = wheel_rect.x0 + 4.0;
            while x < wheel_rect.x1 {
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
                if fb.chrome().tree().workspaces.len() > initial_count {
                    found = true;
                    break 'sweep;
                }
                x += 8.0;
            }
            y -= 4.0;
        }
        assert!(
            found,
            "sweep of clicks across the wheel disc's footprint should hit the \
             add-workspace affordance and grow the workspace count from \
             {initial_count}"
        );
    }

    // ===================================================================
    // Phase 3 — drag / key / type / modifiers executor coverage
    // ===================================================================

    /// `SetModifiers` action mutates `ctx.modifiers` directly. The
    /// host reads the snapshot on the next chord, so this is the
    /// state the harness depends on for chord injection (Ctrl+K, …).
    #[test]
    fn set_modifiers_action_updates_ctx_mask() {
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

    /// A keyboard press+release pair dispatches through the host
    /// without panicking. The minimum bar is the same as
    /// `full_click_sequence_dispatches_without_panic` — the wire
    /// stays intact for both press and release.
    #[test]
    fn keyboard_key_press_release_dispatches_without_panic() {
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

    // ===================================================================
    // Phase 4 — snapshot executor coverage
    // ===================================================================

    /// Sanity: `Snapshot` action against a fresh FrameBuilder writes a
    /// real PNG to disk. Asserts both the magic byte signature and a
    /// non-trivial byte count — a corrupted/early-terminated PNG would
    /// pass the magic check but be tiny, so the >1KB floor catches the
    /// "wrote only a header" failure mode.
    ///
    /// Skipped on environments without a wgpu adapter (CI without
    /// llvmpipe): the executor logs and returns rather than writing a
    /// file, so the pixel assertions would have nothing to run against.
    /// We check for the file's existence and skip the rest gracefully
    /// if the renderer was unavailable.
    #[test]
    #[serial_test::serial(snapshot_renderer)]
    fn execute_snapshot_writes_a_png_at_path() {
        let tmpdir = tempfile::tempdir().expect("create tempdir for snapshot test");
        let path = tmpdir.path().join("test.png");
        let mut fb = FrameBuilder::new((640, 360));
        let mut ctx = TestControlCtx::new();
        execute_planned_action(
            &PlannedAction::Snapshot {
                path: path.clone(),
                defer: false,
            },
            &mut fb,
            &mut ctx,
        );
        // If the renderer wasn't available the executor would have
        // logged + returned without creating the file. Treat that as
        // a soft skip — mirrors `tests/sidebar_wheel_disc_snapshot.rs::
        // try_new_renderer`.
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

    /// The snapshot renderer must outlive the thread that built it.
    ///
    /// It used to live in a `thread_local!`, so its wgpu resources were
    /// dropped from a TLS destructor at thread exit — which aborts the
    /// process rather than failing a test, taking every other result in
    /// the binary with it (see [`SNAPSHOT_RENDERER`] for the mechanism).
    /// An abort cannot be observed from inside the binary it kills, so
    /// assert the property that prevents it: a snapshot taken on a
    /// thread that then *exits* must leave its renderer behind, so the
    /// next snapshot at the same viewport builds nothing new.
    ///
    /// Reintroducing a per-thread cache fails this on the count before
    /// it gets the chance to abort anything.
    #[test]
    #[serial_test::serial(snapshot_renderer)]
    fn snapshot_renderer_outlives_the_thread_that_built_it() {
        use std::sync::atomic::Ordering;

        fn take_snapshot() -> bool {
            let tmpdir = tempfile::tempdir().expect("create tempdir for snapshot test");
            let path = tmpdir.path().join("shot.png");
            let mut fb = FrameBuilder::new((640, 360));
            let mut ctx = TestControlCtx::new();
            execute_planned_action(
                &PlannedAction::Snapshot {
                    path: path.clone(),
                    defer: false,
                },
                &mut fb,
                &mut ctx,
            );
            path.exists()
        }

        // Warm the cache on a thread that then exits. With a per-thread
        // cache, this join is where the process died.
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

    /// The deferred path writes the same file, just not on this thread.
    ///
    /// `defer` exists to take encoding off the compositor's event loop,
    /// and the way that could go wrong without anyone noticing is for
    /// the frame to be captured and then never written — a recording
    /// that runs at a lovely frame rate and produces fewer files than
    /// frames. So the assertion is on the file, after the drain that the
    /// wire's `drain_snapshots` performs.
    #[test]
    #[serial_test::serial(snapshot_renderer)]
    fn a_deferred_snapshot_is_on_disk_once_drained() {
        let tmpdir = tempfile::tempdir().expect("create tempdir for snapshot test");
        let path = tmpdir.path().join("deferred.png");
        let mut fb = FrameBuilder::new((640, 360));
        let mut ctx = TestControlCtx::new();
        execute_planned_action(
            &PlannedAction::Snapshot {
                path: path.clone(),
                defer: true,
            },
            &mut fb,
            &mut ctx,
        );
        execute_planned_action(&PlannedAction::DrainSnapshots, &mut fb, &mut ctx);
        // Same soft skip as the synchronous test: with no wgpu adapter
        // there is no buffer to encode and nothing was ever submitted.
        if SNAPSHOT_RENDERER
            .get()
            .and_then(|c| c.lock().ok().map(|s| s.is_none()))
            .unwrap_or(true)
        {
            eprintln!("a_deferred_snapshot_is_on_disk_once_drained: no wgpu adapter, skipping");
            return;
        }
        let meta = std::fs::metadata(&path)
            .unwrap_or_else(|e| panic!("deferred snapshot missing after drain: {e}"));
        assert!(meta.len() > 0, "deferred snapshot is empty after drain");
    }

    /// Hostile path inputs — the executor must log the error and
    /// return without panicking. The dispatcher's `Ok` response was
    /// already on the wire; the compositor must not die because a
    /// client typo asked for `/nonexistent/dir/x.png`. Phase 5 will
    /// upgrade these logs into structured wire errors.
    #[test]
    #[serial_test::serial(snapshot_renderer)]
    fn execute_snapshot_with_bogus_dir_does_not_crash() {
        let mut fb = FrameBuilder::new((640, 360));
        let mut ctx = TestControlCtx::new();
        // Path under a nonexistent parent — File::create will fail with
        // ENOENT; the executor must log and return.
        execute_planned_action(
            &PlannedAction::Snapshot {
                path: std::path::PathBuf::from("/nonexistent/dir/x.png"),
                defer: false,
            },
            &mut fb,
            &mut ctx,
        );
        // No panic == pass. Sanity-check the FrameBuilder is still
        // usable after the failed snapshot (we didn't leave it in a
        // half-painted state).
        fb.relayout();
        let _ = fb.paint();
    }

    // ===================================================================
    // Phase 5 — state_dump
    // ===================================================================

    /// `FrameBuilderControlState::chrome_state_json` returns an object
    /// with an `overlays` key carrying the four flags. The shape
    /// matches what `qos-control state-dump` clients query, so a
    /// regression here would silently break the CLI.
    #[test]
    fn state_dump_chrome_includes_overlays_field() {
        let fb = FrameBuilder::new((1600, 900));
        let state = FrameBuilderControlState { fb: &fb, doc: None, pointer: None };
        let chrome = state.chrome_state_json();
        let overlays = chrome
            .get("overlays")
            .expect("chrome_state_json should carry `overlays` key");
        // The four bool flags should all be reachable through the
        // `overlays` sub-object — addressing them by string mirrors
        // how the CLI / shell-test harness will read them.
        for key in ["overview", "quick_settings", "ws_settings", "collaborators"] {
            assert!(
                overlays.get(key).and_then(|v| v.as_bool()).is_some(),
                "overlays should carry boolean `{key}`; got {overlays:?}"
            );
        }
    }

    /// `FrameBuilderControlState::workspace_tree_json` exposes a
    /// `workspaces` array on the bootstrap layout. The CLI's
    /// `state-dump` smoke depends on this — without it the JSON would
    /// still be valid but the consumer would have no entrypoint to
    /// the workspace list.
    #[test]
    fn state_dump_workspace_tree_includes_workspaces_array() {
        let fb = FrameBuilder::new((1600, 900));
        let state = FrameBuilderControlState { fb: &fb, doc: None, pointer: None };
        let tree = state.workspace_tree_json();
        let workspaces = tree
            .get("workspaces")
            .and_then(|v| v.as_array())
            .expect("workspace_tree_json should carry `workspaces` array");
        // `FrameBuilder::new` uses the demo tree, which carries
        // workspaces — matches the `bootstrap_demo` assertions in
        // `shell/state.rs`.
        assert!(
            !workspaces.is_empty(),
            "bootstrap workspace tree should not be empty"
        );
    }

    /// The state dump has to carry the roster, because that is how a
    /// harness observes who the shell believes is present.
    ///
    /// `Workspace::collaborators` is `#[serde(skip_deserializing)]` —
    /// presence must not be read back off disk, or a restarted shell
    /// paints people whose sessions are gone. The write direction is
    /// deliberately left open: the same derived `Serialize` produces
    /// this dump, so closing it with a plain `skip` would silently
    /// remove the roster from the control socket and every observation
    /// of it would come back empty and look like agreement.
    #[test]
    fn state_dump_carries_the_roster_it_is_asked_about() {
        let fb = FrameBuilder::new((1600, 900));
        {
            let chrome = fb.chrome();
            let mut tree = chrome.tree_mut();
            let room = tree.active_workspace().expect("active workspace").room;
            assert!(tree.note_peer_present(room, 0x51, "alice"));
        }
        let state = FrameBuilderControlState { fb: &fb, doc: None, pointer: None };
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

    /// Dispatching `Command::StateDump` against a `FrameBuilderControlState`
    /// returns `Response::State` and produces no actions. The
    /// integration round-trip lives in `tests/control_socket_test.rs`;
    /// this unit-level smoke catches regressions that don't need a
    /// spawned compositor.
    #[test]
    fn dispatch_state_dump_against_frame_builder_returns_state() {
        let fb = FrameBuilder::new((1600, 900));
        let state = FrameBuilderControlState { fb: &fb, doc: None, pointer: None };
        let (resp, actions) =
            test_control::dispatch_command(&test_control::Command::StateDump, &state);
        assert!(actions.is_empty());
        let test_control::Response::State {
            workspace_tree,
            chrome,
            peers: _,
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

    /// Cmd+Tab via the test_control wire opens the overview overlay —
    /// proves modifiers + named-key chords reach the host's chord
    /// handler exactly like `ShellDriver::press_key` does for
    /// in-process scenarios.
    #[test]
    fn cmd_tab_chord_via_test_control_opens_overview() {
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

    /// D-3 contract: dropping an `UnlinkOnDrop` removes the file at
    /// its path. The compositor's `run` holds a guard around the
    /// test-control socket file so it gets unlinked on clean exit
    /// + panic-unwind. Previously the socket lingered until the next
    /// boot's `bind_test_control_socket` scrubbed it.
    #[test]
    fn unlink_on_drop_removes_socket_file_on_drop() {
        let tmpdir = tempfile::tempdir().expect("tempdir");
        let path = tmpdir.path().join("nirios-test-control.sock");
        // Use an arbitrary regular-file stand-in; the guard removes
        // anything `remove_file` accepts, so we don't need a real
        // unix socket to exercise the contract.
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

    /// D-10 contract: `read_bounded_line` caps a malicious / buggy
    /// client's input at `TEST_CONTROL_MAX_LINE_BYTES`. The previous
    /// implementation called `BufRead::read_line` with no limit, so a
    /// 100k-byte newline-less stream would buffer in full and could
    /// drive the compositor toward OOM.
    ///
    /// We construct an in-memory reader of 100k bytes with no
    /// newline and assert the bounded reader returns `TooLarge`
    /// rather than allocating unbounded. The caller (`pump_…`) closes
    /// the connection on this error; that contract is exercised
    /// transitively by the existing test_control integration tests
    /// — here we focus on the new helper's bound.
    #[test]
    fn pump_drops_oversized_lines() {
        use std::io::BufReader;
        let stream: Vec<u8> = vec![b'A'; 100_000];
        let mut reader = BufReader::new(&stream[..]);
        let mut out = String::new();
        let res = read_bounded_line(&mut reader, &mut out, TEST_CONTROL_MAX_LINE_BYTES);
        match res {
            Err(ReadLineError::TooLarge) => {
                // Expected. `out` is intentionally still empty here
                // (we don't surface the truncated bytes to the parser
                // because they'd parse as garbage anyway).
                assert!(
                    out.is_empty(),
                    "read_bounded_line must not hand a truncated line \
                     to the parser; got {} bytes",
                    out.len()
                );
            }
            Err(ReadLineError::Io(e)) => {
                panic!("expected TooLarge on a 100k newline-less stream; got io: {e}")
            }
            Ok(opt) => panic!(
                "expected TooLarge on a 100k newline-less stream; got Ok({:?})",
                opt.is_some()
            ),
        }
    }

    // ===================================================================
    // Track D2 — session lock chord / unlock detection
    // ===================================================================

    fn mods_with_super(on: bool) -> Modifiers {
        let mut mask = ModifiersState::empty();
        if on {
            mask |= ModifiersState::SUPER;
        }
        Modifiers::from(mask)
    }

    /// Super+L on key press is the lock chord.
    #[test]
    fn super_l_press_is_lock_chord() {
        let ev = keyboard_input_event(Key::Character(SmolStr::new("l")), true);
        assert!(is_lock_chord(&ev, &mods_with_super(true)));
        // Upper-case 'L' (Shift+Super+L) also counts.
        let ev_upper = keyboard_input_event(Key::Character(SmolStr::new("L")), true);
        assert!(is_lock_chord(&ev_upper, &mods_with_super(true)));
    }

    /// Without Super held, plain `l` is not the lock chord (it types).
    #[test]
    fn l_without_super_is_not_lock_chord() {
        let ev = keyboard_input_event(Key::Character(SmolStr::new("l")), true);
        assert!(!is_lock_chord(&ev, &mods_with_super(false)));
    }

    /// The lock chord only fires on press, not release — otherwise the
    /// key-up would re-trigger and toggle twice per keystroke.
    #[test]
    fn super_l_release_is_not_lock_chord() {
        let ev = keyboard_input_event(Key::Character(SmolStr::new("l")), false);
        assert!(!is_lock_chord(&ev, &mods_with_super(true)));
    }

    /// `Super+Esc` on press is the remote-input kill switch.
    #[test]
    fn super_escape_press_is_the_remote_input_kill_switch() {
        let ev = keyboard_input_event(Key::Named(NamedKey::Escape), true);
        assert!(is_remote_input_kill_switch(&ev, &mods_with_super(true)));
    }

    /// Escape on its own closes an overlay and reaches modal clients. It
    /// must not stop a shared session by accident.
    #[test]
    fn escape_without_super_is_not_the_kill_switch() {
        let ev = keyboard_input_event(Key::Named(NamedKey::Escape), true);
        assert!(!is_remote_input_kill_switch(&ev, &mods_with_super(false)));
    }

    /// Press only. A release that re-fired would engage the latch a
    /// second time and post a second toast for one keystroke.
    #[test]
    fn super_escape_release_is_not_the_kill_switch() {
        let ev = keyboard_input_event(Key::Named(NamedKey::Escape), false);
        assert!(!is_remote_input_kill_switch(&ev, &mods_with_super(true)));
    }

    /// The panic key is not a key a client can be handed. `Super` is the
    /// compositor modifier, so the shell's own chord table already claims
    /// it — asserted here because the kill switch depends on it: were the
    /// chord routable, a focused client could take the keystroke and the
    /// host would press Escape into a guest-driven window.
    #[test]
    fn super_escape_is_a_shell_chord_and_never_reaches_a_client() {
        let ev = keyboard_input_event(Key::Named(NamedKey::Escape), true);
        assert!(client_input::is_shell_chord(
            &ev,
            &mods_with_super(true),
            false
        ));
        assert!(
            client_input::is_shell_chord(&ev, &mods_with_super(true), true),
            "an open overlay must not swallow the panic key either"
        );
    }

    /// A non-keyboard event (cursor move) is never the lock chord even
    /// with Super held.
    #[test]
    fn cursor_move_is_not_lock_chord() {
        let ev = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: PhysicalPosition::new(10.0, 10.0),
        };
        assert!(!is_lock_chord(&ev, &mods_with_super(true)));
    }

    /// Enter and Escape on press are the explicit unlock action.
    #[test]
    fn enter_and_escape_press_map_to_submit_and_cancel() {
        let enter = keyboard_input_event(Key::Named(NamedKey::Enter), true);
        let escape = keyboard_input_event(Key::Named(NamedKey::Escape), true);
        assert_eq!(winit_event_to_lock_key(&enter), LockKey::Submit);
        assert_eq!(winit_event_to_lock_key(&escape), LockKey::Cancel);
    }

    /// A plain character keypress feeds the password buffer as text — it does
    /// not itself unlock (only a correct password on Submit does).
    #[test]
    fn char_key_maps_to_text() {
        let ev = keyboard_input_event(Key::Character(SmolStr::new("a")), true);
        assert_eq!(winit_event_to_lock_key(&ev), LockKey::Text("a".to_string()));
    }

    /// Backspace maps to a buffer edit; a key *release* is ignored (only
    /// presses type).
    #[test]
    fn backspace_maps_and_release_is_ignored() {
        let bs = keyboard_input_event(Key::Named(NamedKey::Backspace), true);
        assert_eq!(winit_event_to_lock_key(&bs), LockKey::Backspace);
        let release = keyboard_input_event(Key::Named(NamedKey::Enter), false);
        assert_eq!(winit_event_to_lock_key(&release), LockKey::Ignored);
    }

    /// `apply_session_transition` maps Blanked/Woke to the backend's
    /// `set_dpms`; Locked/Unlocked are pure state and touch no backend.
    /// We assert via a spy backend that records `set_dpms` calls.
    #[test]
    fn apply_transition_drives_backend_dpms() {
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

        let mut backend = SpyBackend::default();
        apply_session_transition(SessionTransition::Blanked, &mut backend, None);
        apply_session_transition(SessionTransition::Woke, &mut backend, None);
        apply_session_transition(SessionTransition::Locked, &mut backend, None);
        apply_session_transition(SessionTransition::Unlocked, &mut backend, None);
        // Blanked → set_dpms(false), Woke → set_dpms(true); Locked /
        // Unlocked produce no backend calls.
        assert_eq!(backend.dpms_calls, vec![false, true]);
    }
}

/// The refresh rate to advertise on `wl_output`, in millihertz.
///
/// Derived from the cadence the compositor is actually pacing to, so a
/// client's own frame timing agrees with when frames are really produced.
/// `None` when the backend has no display to report one for; the globals
/// then advertise the conventional 60 Hz rather than zero, which some
/// toolkits divide by.
fn refresh_mhz_of(backend: &dyn crate::compositor::backend::Backend) -> Option<u32> {
    backend
        .refresh_interval()
        .filter(|d| !d.is_zero())
        .map(|d| (1000.0 / d.as_secs_f64()).round() as u32)
}

/// Perform the navigation a claimed touchpad gesture resolved to.
///
/// Each arm is the same chrome operation its keyboard chord runs — a
/// gesture is a second way to ask for something the shell already does,
/// not a parallel interaction model.
fn apply_shell_gesture(
    frame_builder: &mut FrameBuilder,
    action: crate::compositor::backend::drm::gestures::GestureAction,
) {
    use crate::compositor::backend::drm::gestures::GestureAction;
    frame_builder.with_chrome(|st| match action {
        GestureAction::SwitchWorkspace(dir) => {
            st.switch_workspace(dir);
        }
        GestureAction::CycleTab(dir) => {
            st.cycle_active_tab(dir);
        }
        GestureAction::ShowOverview => {
            if !st.overlays.overview {
                st.overlays.overview = true;
                st.bump();
            }
        }
        GestureAction::HideOverview => {
            if st.overlays.overview {
                st.overlays.overview = false;
                st.bump();
            }
        }
    });
}

/// Translate the backend's output list into the geometry the shell's strips
/// resolve window widths against.
///
/// Outputs with a zero dimension are dropped: a connector mid-modeset
/// reports 0×0, and letting that through would hand a strip a display it
/// cannot place a window on.
fn output_geometry(
    outputs: &[crate::compositor::backend::Output],
) -> Vec<crate::shell::overlay_host::OutputGeometry> {
    outputs
        .iter()
        .filter(|o| o.size.0 > 0 && o.size.1 > 0)
        .map(|o| crate::shell::overlay_host::OutputGeometry {
            x: o.position.0 as f64,
            width: o.size.0 as f64,
            height: o.size.1 as f64,
        })
        .collect()
}

#[cfg(test)]
mod vt_chord_tests {
    use super::vt_switch_chord;
    use winit::event::{ElementState, KeyEvent, Modifiers, WindowEvent};
    use winit::keyboard::{Key, ModifiersState, NamedKey, NativeKeyCode, PhysicalKey};

    fn key(named: NamedKey, state: ElementState) -> WindowEvent {
        // Same zero-init workaround the two other `KeyEvent` builders in
        // this crate use: winit's `platform_specific` field is pub(crate)
        // and tolerates the all-zero bit pattern.
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
    fn ctrl_alt_f_maps_to_the_matching_vt() {
        let m = mods(ModifiersState::CONTROL | ModifiersState::ALT);
        assert_eq!(
            vt_switch_chord(&key(NamedKey::F1, ElementState::Pressed), &m),
            Some(1)
        );
        assert_eq!(
            vt_switch_chord(&key(NamedKey::F3, ElementState::Pressed), &m),
            Some(3),
            "Ctrl+Alt+F3 is the one the seat diagnosis tells people to use"
        );
        assert_eq!(
            vt_switch_chord(&key(NamedKey::F12, ElementState::Pressed), &m),
            Some(12)
        );
    }

    #[test]
    fn either_modifier_alone_is_not_the_chord() {
        // Alt+F4 closes a window on every desktop; claiming it as a VT
        // switch would be a spectacular regression.
        assert_eq!(
            vt_switch_chord(
                &key(NamedKey::F4, ElementState::Pressed),
                &mods(ModifiersState::ALT)
            ),
            None
        );
        assert_eq!(
            vt_switch_chord(
                &key(NamedKey::F4, ElementState::Pressed),
                &mods(ModifiersState::CONTROL)
            ),
            None
        );
        assert_eq!(
            vt_switch_chord(
                &key(NamedKey::F4, ElementState::Pressed),
                &mods(ModifiersState::empty())
            ),
            None
        );
    }

    #[test]
    fn only_the_press_switches() {
        // Acting on the release too would switch twice per chord.
        let m = mods(ModifiersState::CONTROL | ModifiersState::ALT);
        assert_eq!(
            vt_switch_chord(&key(NamedKey::F2, ElementState::Released), &m),
            None
        );
    }

    #[test]
    fn other_keys_with_the_modifiers_held_are_not_vt_switches() {
        let m = mods(ModifiersState::CONTROL | ModifiersState::ALT);
        assert_eq!(
            vt_switch_chord(&key(NamedKey::Delete, ElementState::Pressed), &m),
            None
        );
        assert_eq!(
            vt_switch_chord(&key(NamedKey::Tab, ElementState::Pressed), &m),
            None
        );
    }
}

#[cfg(test)]
mod gesture_wireup_tests {
    use super::{apply_shell_gesture, refresh_mhz_of};
    use crate::compositor::backend::drm::gestures::GestureAction;
    use crate::compositor::backend::{Backend, BackendEvent, HeadlessBackend, Output, OutputId};
    use crate::compositor::frame::FrameBuilder;

    /// A shell gesture has to reach the *chrome*, not a parallel handler.
    /// These drive `apply_shell_gesture` — the run loop's own arm — against
    /// a real `FrameBuilder`, so a refactor that leaves the gesture
    /// resolved but unapplied fails here.
    fn builder() -> FrameBuilder {
        FrameBuilder::new((1600, 900))
    }

    #[test]
    fn a_swipe_up_switches_the_workspace_the_chrome_is_on() {
        let mut fb = builder();
        // A second workspace to switch to; a one-workspace tree has
        // nowhere to go and the assertion would pass vacuously. Creating
        // one makes it active and appends it last, so "up" is where the
        // one we came from is.
        fb.with_chrome(|st| {
            st.add_workspace("second");
        });
        let before = fb.chrome().tree().active;
        // What a three-finger swipe up resolves to — the same value
        // `gestures::swipe_action` produces for an upward delta.
        apply_shell_gesture(&mut fb, GestureAction::SwitchWorkspace(-1));
        let after = fb.chrome().tree().active;
        assert_ne!(
            before, after,
            "a three-finger swipe must move the active workspace, not just resolve to an intent"
        );
    }

    #[test]
    fn a_four_finger_swipe_opens_and_closes_the_overview() {
        let mut fb = builder();
        assert!(!fb.chrome().overlays.overview);
        apply_shell_gesture(&mut fb, GestureAction::ShowOverview);
        assert!(fb.chrome().overlays.overview);
        apply_shell_gesture(&mut fb, GestureAction::HideOverview);
        assert!(!fb.chrome().overlays.overview);
    }

    #[test]
    fn showing_an_already_open_overview_is_a_no_op() {
        // The gesture fires once per swipe, but a second swipe in the same
        // direction must not bump the chrome revision for nothing — that
        // is a repaint the user did not ask for.
        let mut fb = builder();
        apply_shell_gesture(&mut fb, GestureAction::ShowOverview);
        let revision = fb.chrome().revision;
        apply_shell_gesture(&mut fb, GestureAction::ShowOverview);
        assert_eq!(fb.chrome().revision, revision);
    }

    #[test]
    fn a_backend_with_no_display_advertises_no_refresh_rate() {
        // The headless backend has no display, so `wl_output` falls back
        // to the conventional rate rather than advertising zero, which
        // some toolkits divide by.
        let backend = HeadlessBackend::new(800, 600).expect("headless backend");
        assert_eq!(refresh_mhz_of(&backend as &dyn Backend), None);
    }

    #[test]
    fn a_refresh_interval_becomes_millihertz() {
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
        // 59.94 Hz must not round to 60 000 — the whole reason the DRM
        // backend derives its interval from raw mode timing.
        let ntsc = Paced(std::time::Duration::from_nanos(16_683_333));
        assert_eq!(refresh_mhz_of(&ntsc as &dyn Backend), Some(59_940));
    }
}

#[cfg(test)]
mod output_geometry_tests {
    use super::output_geometry;
    use crate::compositor::backend::{Output, OutputId};

    /// A single monitor maps to one display at the origin — the case every
    /// screenshot and test runs in.
    #[test]
    fn single_output_maps_to_one_display() {
        let outs = [Output::new(OutputId(0), (3840, 2160), 1.0)];
        let geo = output_geometry(&outs);
        assert_eq!(geo.len(), 1);
        assert_eq!((geo[0].x, geo[0].width, geo[0].height), (0.0, 3840.0, 2160.0));
    }

    /// Two monitors side by side stay two displays, each with its own
    /// width. This is what stops a full-width window spanning both: the
    /// combined geometry here is 3520 px, and neither display is that wide.
    #[test]
    fn side_by_side_outputs_stay_separate_displays() {
        let outs = [
            Output::new(OutputId(0), (1600, 900), 1.0),
            Output {
                id: OutputId(1),
                position: (1600, 0),
                size: (1920, 1080),
                scale: 1.0,
            },
        ];
        let geo = output_geometry(&outs);
        assert_eq!(geo.len(), 2);
        assert_eq!((geo[0].x, geo[0].width), (0.0, 1600.0));
        assert_eq!((geo[1].x, geo[1].width), (1600.0, 1920.0));
    }

    /// A connector mid-modeset reports 0x0; a strip cannot place a window
    /// on it, so it must not appear as a display.
    #[test]
    fn zero_sized_outputs_are_dropped() {
        let outs = [
            Output::new(OutputId(0), (1600, 900), 1.0),
            Output::new(OutputId(1), (0, 0), 1.0),
        ];
        assert_eq!(output_geometry(&outs).len(), 1);
    }
}
