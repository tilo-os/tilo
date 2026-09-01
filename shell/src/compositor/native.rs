//! Native compositor mode: `--mode native` wires a real smithay event loop
//! over [`state::State`], with an auto-named listening socket (`wayland-1`,
//! `wayland-2`, …) handed to `DisplayHandle::insert_client`. Doesn't open a
//! TTY, talk to DRM/KMS, render, or forward input — those live elsewhere.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ObjId};
use rand_core::{OsRng, RngCore};

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
        .active_workspace()
        .ok_or_else(|| "no active workspace to resolve capabilities against".to_string())?;
    let actor_id = chrome.local_user_for_room(&workspace.room);
    let actor = workspace.actor_for_person(&actor_id);
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
use crate::compositor::frame::FrameBuilder;
use crate::compositor::idle::{IdleConfig, SessionManager, SessionTransition};
use crate::compositor::lock_auth;
use crate::compositor::lock_controller::{LockController, LockKey, UnlockOutcome};
use crate::compositor::lock_screen::{self, LockOutput, LockView};
use crate::compositor::metrics;
use crate::compositor::output_globals;
use crate::compositor::pacing;
use crate::compositor::present;
use crate::compositor::switch_control::{SwitchAction, SwitchControlRuntime};

use crate::compositor::state::{ClientState, State};
use crate::compositor::test_control::{
    self, ControlState, KeyTag, NamedKeyTag, PlannedAction, PointerButtonKind, parse_command,
    serialise_response,
};
use crate::compositor::{BackendKind, parse_backend_from_args};
use crate::shell::overlay_host::PeerSyncRequest;

/// Env var for the CI test-introspection hook: when set, [`run`] serializes
/// the shell `WorkspaceTree` to that path each frame tick as compact JSON;
/// `tests/compositor_test.rs` polls it to assert foreign-tab adoption +
/// teardown without a public shell-state API. Unset means the compositor
/// never touches the filesystem here.
const TEST_STATE_DUMP_ENV: &str = "NIRIOS_TEST_STATE_DUMP";

/// Env var for the real-input e2e control protocol: when set to a writable
/// path, [`run`] binds a unix socket there accepting newline-delimited JSON
/// commands (`compositor::test_control::Command`). It types, clicks and
/// reads back state as if it were the person at the keyboard — even while
/// locked — so it is a **developer-build** surface, gated on
/// [`crate::compositor::dev_env::DEV_ENV_OVERRIDES`].
const TEST_CONTROL_ENV: &str = "QOS_TEST_CONTROL";

/// RAII guard that unlinks a unix-socket file when dropped. `process::exit`
/// skips destructors, so [`run`] drops this explicitly before exiting —
/// otherwise the stale file lingers in `XDG_RUNTIME_DIR` for the next
/// session.
pub(crate) struct UnlinkOnDrop(pub(crate) PathBuf);

impl Drop for UnlinkOnDrop {
    fn drop(&mut self) {
        // Errors are not actionable here — already gone, or no permission — so ignore.
        let _ = std::fs::remove_file(&self.0);
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
    // Best-effort cleanup of a stale socket — otherwise `bind` fails EADDRINUSE.
    let _ = std::fs::remove_file(&path);
    match UnixListener::bind(&path) {
        Ok(listener) => {
            if let Err(e) = listener.set_nonblocking(true) {
                eprintln!("nirios compositor: test_control set_nonblocking: {e}");
                return None;
            }
            eprintln!(
                "nirios compositor test_control listening on {}",
                path.display()
            );
            let guard = UnlinkOnDrop(path);
            Some((listener, guard))
        }
        Err(e) => {
            eprintln!(
                "nirios compositor: test_control bind {}: {e}",
                path.display()
            );
            None
        }
    }
}

/// Directory under `$XDG_RUNTIME_DIR` where a running compositor publishes
/// facts the next terminal command needs; torn down with the session.
pub const SESSION_RUNTIME_SUBDIR: &str = "qos-session";

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
/// `$XDG_RUNTIME_DIR/qos-session/wayland-display`. Last writer wins — the
/// file answers "what do I connect to *now*"; the printed stderr line stays
/// authoritative for a specific compositor. Never fatal on failure. Returns
/// the unlink guard so the file disappears with the process.
fn publish_wayland_display(socket_name: &std::ffi::OsStr) -> Option<UnlinkOnDrop> {
    let path = session_display_path(std::env::var_os("XDG_RUNTIME_DIR"))?;
    if let Some(dir) = path.parent()
        && let Err(e) = std::fs::create_dir_all(dir)
    {
        eprintln!("nirios compositor: publishing {}: {e}", path.display());
        return None;
    }
    // 0600: not a secret, but nothing else should write it; applied before
    // the rename so it's never briefly visible wider than intended.
    match atomic_file::write_with_mode(&path, socket_name.as_encoded_bytes(), 0o600) {
        Ok(()) => Some(UnlinkOnDrop(path)),
        Err(e) => {
            eprintln!("nirios compositor: publishing {}: {e}", path.display());
            None
        }
    }
}

/// Env var naming the compositor's privileged Wayland socket, exported to
/// anything the session starts outside a sandbox — clipboard-manager clients connect here instead of `WAYLAND_DISPLAY`.
pub const PRIVILEGED_SOCKET_ENV: &str = "QOS_WAYLAND_PRIVILEGED";

/// Bind the privileged Wayland socket and publish its name, carrying the
/// compositor's pid so concurrent sessions can't collide.
fn bind_privileged_socket() -> Result<(ListeningSocketSource, std::ffi::OsString), String> {
    let name = format!("qos-privileged-{}", std::process::id());
    let socket = ListeningSocketSource::with_name(&name).map_err(|e| e.to_string())?;
    let socket_name = socket.socket_name().to_os_string();
    // Published for privileged children only; sandboxed apps get an explicit
    // environment from `qos-launcher` and never inherit this.
    // SAFETY: single-threaded compositor startup, before any thread that
    // reads the environment has been spawned.
    unsafe { std::env::set_var(PRIVILEGED_SOCKET_ENV, &socket_name) };
    Ok((socket, socket_name))
}

/// Bind the private display used only by the compositor-launched OSK. The
/// random suffix keeps it out of well-known socket-name guesses; unlike the
/// clipboard socket it is never exported in the session environment.
fn bind_osk_socket() -> Result<(ListeningSocketSource, PathBuf), String> {
    let name = format!(
        "qos-osk-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    );
    let socket = ListeningSocketSource::with_name(&name).map_err(|e| e.to_string())?;
    let path = crate::shell::launcher::runtime_dir().join(socket.socket_name());
    Ok((socket, path))
}

/// Per-connection state for the test-control protocol: a buffered reader
/// over a non-blocking `UnixStream`, so partial lines accumulate across ticks.
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

/// `ControlState` impl walking the frame builder. Captures a fresh snapshot
/// per `widget_registry()` call, so dispatch sees the live tree, uncached.
struct FrameBuilderControlState<'a> {
    fb: &'a FrameBuilder,
    /// The live CRDT the search providers read, plus its root object. `None`
    /// for layout/chrome-only test fixtures, so action methods refuse rather than report an empty surface.
    doc: Option<(&'a AutoCommit, &'a ObjId)>,
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
        let roster: Vec<serde_json::Value> =
            crate::shell::overlay_host::collaborator_roster_rows(&chrome.tree())
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
            format!("{user} may not administer this room, so nothing was granted")
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
            .active_workspace()
            .map(|w| w.id)
            .ok_or_else(|| "no active workspace".to_string())?;
        let ws = tree
            .workspaces
            .iter_mut()
            .find(|w| w.id == ws_id)
            .ok_or_else(|| "no active workspace".to_string())?;
        // Distinguish the two failures: `revoke_as` returns false both for "you
        // may not" and "no such capability" — conflating them hides an unauthorised revoke as a no-op.
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
                serde_json::json!({
                    "provider": a.resource.provider,
                    "id": a.resource.id,
                    "kind": a.kind.label(),
                    "title": a.title,
                    "subtitle": a.subtitle,
                    "score": a.score,
                    // What to pass; empty for the nullary majority. A parameter list describes
                    // an invocation, it is not one, so enumeration stays side-effect-free.
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
        // Bind before anything is planned: a bad argument set is refused with the
        // document untouched, and the refusal names the parameter to correct.
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

    /// Build a structured view of [`ChromeState`] for `state_dump`, constructed
    /// manually rather than `derive(Serialize)` because the chrome owns
    /// `Rc<RefCell<…>>` widget handles that can't sensibly serialise; this also
    /// lets fields like `search_scope` pick a wire-friendly enum-as-string form.
    fn chrome_state_json(&self) -> serde_json::Value {
        let chrome = self.fb.chrome();
        let overlays = &chrome.overlays;
        let peer_sync = self.fb.host().peer_sync_snapshot();
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
            // What is actually on screen — without it a harness can only see that *a*
            // frame painted, not that a specific notification reached it.
            "notifications": chrome.notifications().iter().map(|n| serde_json::json!({
                "title": n.title,
                "body": n.body,
                "severity": format!("{:?}", n.severity),
                "persistent": n.dismiss_after >= crate::shell::notification::PERSISTENT_DISMISS,
            })).collect::<Vec<_>>(),
        // The hosted file manager's drag, if in flight, and whether the compositor took it over to a client.
            "files_drag": file_explorer::external_drag::unclaimed().map(|d| d.paths),
            "files_drag_handed_off": file_explorer::external_drag::is_handed_off(),
            "editing_workspace_name": chrome.editing_workspace_name,
            "pending_grants_count": chrome.pending_grants.len(),
        // Mirrored from the SessionManager each tick; lets a headless test observe lock/unlock transitions.
            "session_locked": chrome.session_locked(),
            // Who this session is, and whether they may administer the room they're
            // in — read straight off the production helpers, not guessed from `owner`.
            "local_user_id": chrome.active_local_user(),
            "may_administer_active": chrome
                .tree()
                .active_workspace()
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
            NamedKeyTag::Space => NamedKey::Space,
            NamedKeyTag::Shift => NamedKey::Shift,
            NamedKeyTag::Control => NamedKey::Control,
            NamedKeyTag::Alt => NamedKey::Alt,
            NamedKeyTag::Super => NamedKey::Super,
        }),
        KeyTag::Char(c) => Key::Character(SmolStr::new(c.to_string())),
    }
}

/// Build a `WindowEvent::KeyboardInput` for `key`. Mirrors
/// `shots/driver.rs::key_press_event`: winit's `KeyEvent` has a
/// `pub(crate)` `platform_specific` field, so we zero-init and overwrite
/// only the fields the host reads; `text` is set for `Character` keys so
/// text-input branches see the same glyph real hardware would send.
fn keyboard_input_event(key: &Key, pressed: bool) -> WindowEvent {
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
    ev.text = if let Key::Character(s) = key {
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

fn switch_action_event(action: SwitchAction, pressed: bool) -> WindowEvent {
    keyboard_input_event(
        &Key::Named(match action {
            SwitchAction::Next => NamedKey::Tab,
            SwitchAction::Activate => NamedKey::Enter,
        }),
        pressed,
    )
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
    fb.dispatch_event(
        event,
        &ctx.modifiers,
        &mut ctx.doc,
        &ctx.root_obj,
        &mut ctx.focus,
    );
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
) -> Option<InvokeFailure> {
    // Re-resolve rather than carry a handle from dispatch: the document is
    // mutable between planning and execution, so we run the current action or nothing.
    let chrome_handle = fb.chrome_handle();
    // Built through the same gate the dispatcher resolved against — without
    // it, a revoked capability wouldn't stop a caller who got past it once.
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
    // The `Ref` above is dropped with the block, so the provider's callback
    // is free to borrow the chrome cell mutably — which most of them do.
    if let Some(selected) = catalog.select(&ResourceRef::new(provider, id)) {
        // Re-bound against *this* resolution's parameters — the catalogue was just
        // rebuilt and may have landed on a different action under the same address.
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
            // Logged and returned in the socket reply. Actions run before the reply is
            // written, so "it ran" and "it could not run" never look alike to a caller with no screen.
            eprintln!(
                "nirios compositor: ({provider}, {id}) did not run: {}",
                failure.message
            );
            return Some(failure);
        }
        None
    } else {
        let message = format!(
            "action ({provider}, {id}) vanished between resolve and invoke; nothing was run"
        );
        eprintln!("nirios compositor: {message}");
        Some(InvokeFailure {
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
fn execute_planned_action(
    action: &PlannedAction,
    fb: &mut FrameBuilder,
    ctx: &mut TestControlCtx,
    state: Option<&mut State>,
) -> Option<InvokeFailure> {
    let mut state = state;
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
        }
        PlannedAction::Snapshot { path } => {
            // Re-render into a fresh OffscreenRenderer rather than tap a backend-cached
            // `last_frame`, so this works for every backend, not just headless.
            // Errors are logged via `eprintln!` and swallowed — the dispatcher already
            // returned `Ok`, and crashing on a stray bad path defeats the point of a
            // test surface. A snapshot must show a drag's carried item too, drawn by
            // the live present loop on top of the scene.
            let drag = state.as_ref().and_then(|s| {
                (s.selection.dnd_active || s.selection.chrome_drag.is_some()).then(|| {
                    let (cx, cy) = s.client_input.cursor();
                    (
                        vello::kurbo::Point::new(cx, cy),
                        s.selection.dnd_label.get(),
                    )
                })
            });
            write_snapshot_png(fb, path, drag);
        }
        PlannedAction::InvokeAction { provider, id, args } => {
            return execute_invoke_action(provider, id, args, fb, ctx);
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
fn execute_locked_action(action: &PlannedAction, lock: &mut LockController) {
    match action {
        PlannedAction::KeyboardKey { key, pressed } => {
            let winit_key = keytag_into_winit_key(*key);
            let event = keyboard_input_event(&winit_key, *pressed);
            lock.handle_key(winit_event_to_lock_key(&event));
        }
        // Pointer, modifiers, snapshots and action invocations are all swallowed
        // while locked. `InvokeAction` matters most here: the action API reaches
        // chrome side-effects with no visible surface, so letting it through would
        // make the lock screen bypassable by a socket client that renders nothing.
        PlannedAction::CursorMoved { .. }
        | PlannedAction::MouseLeftButton { .. }
        | PlannedAction::MousePointerButton { .. }
        | PlannedAction::MouseWheel { .. }
        | PlannedAction::SetModifiers { .. }
        | PlannedAction::Snapshot { .. }
        | PlannedAction::SetGroupWidth { .. }
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
    drag: Option<(vello::kurbo::Point, Option<String>)>,
) {
    use std::io::Write;
    let (w, h) = fb.viewport();
    // Viewport dimensions are always non-negative pixel counts; clamped rather
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
        eprintln!(
            "nirios compositor: test_control snapshot: viewport is {w}x{h}, nothing to write"
        );
        return;
    }
    let mut scene = fb.paint();
    if let Some((cursor, label)) = drag {
        let theme = fb.theme().clone();
        drag_feedback::draw(
            &mut scene,
            cursor,
            label.as_deref(),
            &theme,
            fb.text_renderer_mut(),
        );
    }
    let scene = scene;
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
                    eprintln!(
                        "nirios compositor: test_control snapshot: no offscreen renderer: {e}"
                    );
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
    let file = match std::fs::File::create(path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!(
                "nirios compositor: test_control snapshot: create {}: {e}",
                path.display()
            );
            return;
        }
    };
    let mut writer = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(&mut writer, w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut png_writer = match encoder.write_header() {
        Ok(w) => w,
        Err(e) => {
            eprintln!(
                "nirios compositor: test_control snapshot: png header {}: {e}",
                path.display()
            );
            return;
        }
    };
    if let Err(e) = png_writer.write_image_data(&rgba) {
        eprintln!(
            "nirios compositor: test_control snapshot: png write {}: {e}",
            path.display()
        );
        return;
    }
    // Drop the PNG writer first so the BufWriter holds the last bytes, then
    // flush explicitly — else a fast-following file-size assertion could race a buffered tail.
    drop(png_writer);
    if let Err(e) = writer.flush() {
        eprintln!(
            "nirios compositor: test_control snapshot: flush {}: {e}",
            path.display()
        );
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
pub(crate) const TEST_CONTROL_MAX_LINE_BYTES: usize = 64 * 1024;

/// Read up to one `\n`-terminated line from `reader`, capped at
/// `TEST_CONTROL_MAX_LINE_BYTES`. Returns:
///
/// * `Ok(Some(line))` — complete line, trailing newline included (so
///   `parse_command` needs no change).
/// * `Ok(None)` — EOF with no newline seen (peer closed mid-line).
/// * `Err(TooLarge)` — cap fired before a newline; caller must close.
/// * `Err(Io(e))` — I/O failure, including `WouldBlock`.
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
            // EOF: return accumulated bytes as a best-effort line if any, else None.
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
            Some(idx) => (idx.saturating_add(1), true),
            None => (available.len(), false),
        };
        if buf.len().saturating_add(consumed) > max_bytes {
            // Cap reached: consume up to the cap so we don't leave garbage in the reader, then bail.
            let remaining = max_bytes.saturating_sub(buf.len());
            // `remaining < consumed <= available.len()` by construction, so this
            // never misses — `unwrap_or(available)` is just a defensive fallback.
            let take = available.get(..remaining).unwrap_or(available);
            let taken = take.len();
            buf.extend_from_slice(take);
            reader.consume(taken);
            return Err(ReadLineError::TooLarge);
        }
        // `consumed` is either `idx + 1` (a valid position in `available`) or
        // `available.len()`, so this too never misses.
        let take = available.get(..consumed).unwrap_or(available);
        let taken = take.len();
        buf.extend_from_slice(take);
        reader.consume(taken);
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
    // Whether any command produced a `PlannedAction` — synthesised input the
    // next frame must reflect; starts the input-latency measurement instrumentation uses.
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
                        // Settle layout before anything reads a rect. The relayout after an action
                        // (below) covers a tree the *socket* changed, not one a **client** changed
                        // — a window map/close/resize rearranges the strip with no command sent,
                        // so a caller asking mid-frame would get a stale rect. So layout is
                        // settled per command, not per action: idempotent, and cheap relative to
                        // the round-trip that asked for it.
                        frame_builder.relayout();
                        let state = FrameBuilderControlState {
                            fb: frame_builder,
                            doc: Some((&ctx.doc, &ctx.root_obj)),
                        };
                        test_control::dispatch_command(&cmd, &state)
                    }
                    Err(err_resp) => (err_resp, Vec::new()),
                };
                // A planned action that could not run replaces this command's answer.
                // Actions execute before the reply is written; first failure wins, the rest still run.
                let mut failed: Option<InvokeFailure> = None;
                for action in &actions {
                    if session.is_locked() {
                        // While locked the socket is captured by the lock surface like live input
                        // — actions go to the password field; `poll_verification` delivers unlock.
                        execute_locked_action(action, lock);
                    } else if let Some(failure) =
                        execute_planned_action(action, frame_builder, ctx, Some(state))
                    {
                        failed.get_or_insert(failure);
                    }
                }
                // Re-run layout so the next command/paint sees rects post-mutation — a
                // chrome state change can alter structural layout, leaving stale rects otherwise.
                if !actions.is_empty() {
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

/// Turn a [`SessionTransition`] into a real backend effect.
///
/// `Blanked`/`Woke` drive the backend's DPMS (no-op on winit/headless);
/// `Locked`/`Unlocked` are pure state the paint path reads via
/// `SessionManager::is_locked`, needing no backend call. On a render thread,
/// `pipe` carries the DPMS change on the same channel as frames so a blank can't overtake a queued frame.
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
        // Lock/Unlock are reflected via `SessionManager::is_locked`; Suspend is
        // gated by the inhibitor registry + issued through the power service, never here.
        SessionTransition::Locked | SessionTransition::Unlocked | SessionTransition::Suspend => {}
    }
}

/// Whether an event is the hardware power key being pressed — detected by
/// both the logical [`NamedKey::Power`] and physical key code, keymap-independent.
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
    n: qos_power_mgmt::PowerNotification,
) -> crate::notifications::IncomingNotification {
    crate::notifications::IncomingNotification {
        dbus_id: bridge.alloc_id(),
        app_name: "Power".to_string(),
        summary: n.summary,
        body: n.body,
        actions: Vec::new(),
        // Critical battery lingers until dismissed; the low heads-up uses the server default expiry.
        expire_timeout: if n.critical { 0 } else { -1 },
        urgency: if n.critical { 2 } else { 1 },
        caller: None,
        badge_count: None,
        contact_endpoint: None,
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

/// Is `event` the remote-input kill switch, `Super+Esc`, on key *press*?
/// The panic key for a shared session — stops whatever a guest is doing.
/// Checked ahead of the client router and the shell's own Escape handling
/// so an open overlay or a keyboard-grabbing client can't swallow it.
fn is_remote_input_kill_switch(event: &WindowEvent, modifiers: Modifiers) -> bool {
    let WindowEvent::KeyboardInput { event, .. } = event else {
        return false;
    };
    event.state == ElementState::Pressed
        && modifiers.state().super_key()
        && matches!(event.logical_key, Key::Named(NamedKey::Escape))
}

fn is_lock_chord(event: &WindowEvent, modifiers: Modifiers) -> bool {
    let WindowEvent::KeyboardInput { event, .. } = event else {
        return false;
    };
    if event.state != ElementState::Pressed {
        return false;
    }
    let key = match &event.logical_key {
        Key::Character(s) => s.as_str(),
        Key::Named(NamedKey::Tab) => "tab",
        Key::Named(NamedKey::Escape) => "escape",
        _ => return false,
    };
    let config = settings_store::hardware::ShortcutConfig::load();
    let Some(chord) = config
        .bindings
        .get(&settings_store::hardware::ShortcutAction::Lock)
    else {
        return false;
    };
    let state = modifiers.state();
    (chord.modifiers.contains("Super") == state.super_key())
        && (chord.modifiers.contains("Ctrl") == state.control_key())
        && (chord.modifiers.contains("Alt") == state.alt_key())
        && (chord.modifiers.contains("Shift") == state.shift_key())
        && chord.key.eq_ignore_ascii_case(key)
}

/// Map a live winit key event to the lock screen's normalised [`LockKey`]
/// while locked. Non-press/unhandled keys become [`LockKey::Ignored`] —
/// still swallowed (never routed to apps behind the overlay), but inert on
/// the password buffer. The test-control path produces the same `LockKey`s so both drive one routing.
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
                Some(t) if !t.is_empty() && !t.chars().all(char::is_control) => {
                    LockKey::Text(t.to_string())
                }
                _ => LockKey::Ignored,
            }
        }
    }
}

/// How long an invite minted from the palette stays valid.
const INVITE_EXPIRY_SECONDS: u64 = 3600;

/// Sequence for same-directory workspace-salt claim files. The final file is
/// published with a hard link, so concurrent sessions can never replace the
/// salt another session already used to mint durable object ids.
static WORKSPACE_SALT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn read_workspace_salt(path: &Path) -> std::io::Result<u16> {
    let bytes = std::fs::read(path)?;
    let bytes: [u8; 2] = bytes.try_into().map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "workspace peer salt must contain exactly two bytes",
        )
    })?;
    let salt = u16::from_be_bytes(bytes);
    if salt == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "workspace peer salt must be non-zero",
        ));
    }
    Ok(salt)
}

/// Load the namespace salt used for durable workspace object ids, creating a
/// non-secret random one without creating or advertising a transport identity.
/// Existing installations retain the salt derived from their existing device
/// key on first migration.
pub(super) fn load_or_create_workspace_salt(
    state_path: &Path,
    peer_key_path: &Path,
) -> std::io::Result<u16> {
    let parent = state_path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let path = parent.join("workspace-peer-salt");
    if path.try_exists()? {
        return read_workspace_salt(&path);
    }

    let salt = if peer_key_path.try_exists()? {
        let key = crate::network::behaviour::load_or_create_keypair(peer_key_path)?;
        crate::network::behaviour::peer_salt_from_keypair(&key).max(1)
    } else {
        let mut bytes = [0_u8; 2];
        OsRng.fill_bytes(&mut bytes);
        u16::from_be_bytes(bytes).max(1)
    };

    for _ in 0..16 {
        let sequence = WORKSPACE_SALT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = parent.join(format!(
            ".workspace-peer-salt-{}-{sequence}",
            std::process::id()
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = match options.open(&temporary) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        if let Err(error) = file
            .write_all(&salt.to_be_bytes())
            .and_then(|()| file.sync_all())
        {
            let _ = std::fs::remove_file(&temporary);
            return Err(error);
        }
        let claim = std::fs::hard_link(&temporary, &path);
        let _ = std::fs::remove_file(&temporary);
        match claim {
            Ok(()) => {
                std::fs::File::open(parent)?.sync_all()?;
                return Ok(salt);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return read_workspace_salt(&path);
            }
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a workspace-salt claim file",
    ))
}

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

    let roomless = tree.adopt_roomless_workspaces();
    if !roomless.is_empty() {
        tracing::info!(
            "collaboration: minted a room identity for {} workspace(s)",
            roomless.len()
        );
    }
    let adopted = tree.adopt_ownerless_workspaces_with(crate::shell::state::local_user_id_for_room);
    if !adopted.is_empty() {
        tracing::info!(
            "capabilities: {} workspace(s) adopted by their selected identities",
            adopted.len()
        );
    }
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
    match crate::identity_broker::spawn(path.clone(), tree) {
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
/// `None` with no session bus (headless rig, no user bus) — the compositor
/// still runs, just logging that file open/save won't work.
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
        // The real spooler: unlike the picker it needs no chrome/seat (a process
        // invocation), and reports no destinations on a machine with no CUPS.
        print_spooler: crate::portal::PrintSpoolerHandle::cups(),
    };

    let serve_result = runtime.block_on(async {
        let conn = zbus::ConnectionBuilder::session()?.build().await?;
        crate::portal::serve(conn, ctx).await
    });
    let server = match serve_result {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                "portal: could not serve {} ({e}); file open/save will not work",
                crate::portal::BUS_NAME
            );
            return None;
        }
    };

    // Losing the name isn't warning-shaped: xdg-desktop-portal would route
    // FileChooser elsewhere, so every Open would hit a backend with no session state — logged at journal level.
    if !server.owns_name() {
        tracing::error!(
            "portal: another process already owns {} — this session's file \
             dialog will never be reached. The standalone \
             `shell --portal-server-only` daemon is the usual culprit; it is \
             no longer part of the image and its unit should be disabled.",
            crate::portal::BUS_NAME
        );
    }

    // The queues are useless until drained; attaching them to the overlay host
    // here (not at construction) keeps "portal up" and "prompts reach a screen" one fact.
    frame_builder.host_mut().set_portal_queues(prompts, pickers);

    tracing::info!(
        "portal: serving FileChooser+ScreenCast+RemoteDesktop+Access on {}; documents via {}",
        crate::portal::BUS_NAME,
        documents.describe()
    );
    if !documents.reaches_confined_apps() {
        // Say it out loud: a sandboxed app getting ENOENT after picking a file
        // looks like an app bug, and the journal is the only place to connect the dots.
        tracing::warn!(
            "portal: the document store cannot reach sandboxed apps — a file \
             picked by a bwrap'd app will not be readable by it. Unconfined \
             apps are unaffected."
        );
    }
    Some(PortalBackend { runtime, server })
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
                    eprintln!("nirios compositor: --backend winit failed: {e}");
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
                eprintln!("nirios compositor: --backend drm failed: {e}");
                std::process::exit(1);
            }
        },
        BackendKind::Headless => {
            // Initial size from `NIRIOS_HEADLESS_SIZE` (shots-default fallback), keeping
            // headless env-var-driven. Skip `set_base_color`: no window to clear.
            let (w, h) = headless::resolve_initial_size();
            match HeadlessBackend::new(w, h) {
                Ok(b) => Box::new(b),
                Err(e) => {
                    eprintln!("nirios compositor: --backend headless failed: {e}");
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
fn size_to_signed(size: (u32, u32)) -> (i32, i32) {
    (
        i32::try_from(size.0).unwrap_or(i32::MAX),
        i32::try_from(size.1).unwrap_or(i32::MAX),
    )
}

/// Bind the primary public Wayland socket (`wayland-1..32`, first free
/// name), print the two lines a human or test relies on, and publish the
/// name to `$XDG_RUNTIME_DIR/qos-session/wayland-display`. Split out of
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
            eprintln!("nirios compositor: bind listening wayland socket failed: {e}");
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
    let banner = format!("nirios compositor listening on {socket_name:?}");
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

/// Entry point for the smithay-on-Wayland compositor. Diverges (`-> !`) so
/// `main` can use it next to [`super::hosted::run`] without juggling return
/// types; unrecoverable setup errors are logged and end the process via
/// `std::process::exit`, appropriate for a top-level entry point (matches
/// smithay's own examples) — never a bare `panic`, since this crate builds
/// with `panic = "abort"` and a clean logged exit beats an abort with no message.
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
pub fn run() -> ! {
    // Calloop drives the whole compositor; smithay is callback-oriented and
    // expects every wakeup to fan out from a single `EventLoop::<State>`.
    let mut event_loop: EventLoop<'static, State> = match EventLoop::try_new() {
        Ok(el) => el,
        Err(e) => {
            eprintln!("nirios compositor: create calloop event loop failed: {e}");
            std::process::exit(1);
        }
    };

    // The `Display` owns the wayland-server protocol state, held locally so
    // `dispatch_clients`/`flush_clients` run per tick — mirrors smithay 0.7's examples.
    let mut display: Display<State> = match Display::new() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("nirios compositor: create wayland display failed: {e}");
            std::process::exit(1);
        }
    };
    let display_handle = display.handle();

    let mut state = State::new(event_loop.handle(), display_handle.clone());
    spawn_auto_brightness_worker();
    // Point `ui`'s clipboard at the compositor's selection mirror — everything
    // hosted here isn't a Wayland client, so without this they'd share no buffer.
    crate::compositor::selection::install_host_clipboard(state.selection_store());

    // `--backend drm|winit` selects the impl; `Drm` is currently a skeleton
    // (errors "not yet implemented" at construction) but dispatch is wired so follow-up TTY work plugs in.
    let backend_kind = parse_backend_from_args();
    let mut backend: Box<dyn Backend> = build_backend(backend_kind);

    // Per-frame scene production, shared with offscreen tooling via
    // `FrameBuilder` — chrome in the host window is byte-identical to `cargo run --bin shots`.
    //
    // Scene is sized to the bounding box spanning every output; each output
    // presents its own sub-rect, so single-monitor layout is unchanged.
    let initial_size = match combined_geometry(backend.outputs()) {
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
    let onboarding_identity_dir =
        if crate::shell::identity_provisioning::needs_first_run_flow(&identity_store) {
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
    // A first-login compositor may allocate a non-secret workspace id
    // namespace, but it must not create or advertise a transport identity
    // before the person has completed the identity ceremony. Peer sync starts
    // on the next session, after the certified device chain exists.
    let enable_peer_sync = peer_sync_requested && onboarding_identity_dir.is_none();
    if peer_sync_requested && !enable_peer_sync {
        tracing::info!(
            "peer-sync: deferred until the next session while first-login identity setup is active"
        );
    }

    // The desktop's own state, loaded from disk not bootstrapped fresh — room
    // identity, owner and capability list all have to survive a restart.
    let (tree_handle, enable_peer_sync, state_notice) = load_session_tree(enable_peer_sync);
    let mut frame_builder = FrameBuilder::with_handle_and_records(
        tree_handle.clone(),
        enable_peer_sync,
        initial_size,
        crate::shell::session_record_store(),
    );
    // A fresh machine has no display server before `shell`, so first-login
    // cannot be a standalone winit app or a pre-graphical user unit. Host the
    // ceremony in trusted compositor chrome and withhold every desktop
    // affordance until it has produced a real identity.
    if let Some(identity_dir) = onboarding_identity_dir {
        if crate::compositor::parse_install_mode_from_args()
            == crate::compositor::InstallMode::HostDesktop
        {
            frame_builder.enable_host_desktop_onboarding(identity_dir);
        } else {
            frame_builder.enable_onboarding(identity_dir);
        }
    }
    frame_builder.enable_configured_theme();
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
    let _portal = start_portal_backend(&mut frame_builder, tree_handle.clone());

    // Peer sync, constructed here (not only in the hosted entry point) because
    // this is the compositor that boots as the qOS session.
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
                let bridge_tree = tree_handle;
                frame_builder
                    .host_mut()
                    .set_peer_sync_bridge(std::rc::Rc::new(move |req| {
                        let active = bridge_tree.read().active;
                        match req {
                            PeerSyncRequest::IssueInvite => {
                                bridge_net.issue_invite(active, INVITE_EXPIRY_SECONDS)
                            }
                            PeerSyncRequest::IssueInviteFor {
                                name,
                                role,
                                recipient_user_public_key,
                            } => bridge_net.issue_invite_bound_with_role(
                                active,
                                INVITE_EXPIRY_SECONDS,
                                &name,
                                recipient_user_public_key,
                                role,
                            ),
                            PeerSyncRequest::AcceptInvite(token) => bridge_net
                                .accept_invite(active, &token)
                                .map(|room| room.to_string()),
                        }
                    }));
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
    // The session's CRDT fan-out broker, bound before the wayland socket is
    // published (before any app launches) — an app resolves its collab
    // descriptor once at spawn, so a broker arriving later is one it never gets.
    //
    // Lives in this process, not a unit of its own: a listener + fanout
    // table over `ShellNetwork` (the Iroh handle above), since the shell
    // already owns the network stack, identity and permissions. Held to the end of `run()` so the socket outlives every app and unlinks with the session.
    let collab_broker =
        spawn_session_collab_broker(&frame_builder.chrome_handle(), peer_sync.as_ref());
    let _identity_broker = spawn_session_identity_broker(&frame_builder.chrome_handle());
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

    // Read the session's own broker for pointer presence and relay it to the
    // room's other hosts; subscribing by room is what makes a cursor crossing
    // apps read as one peer moving, since presence is room-scoped. Only stood
    // up when both halves exist — without peer sync, the overlay paints nothing.
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

    // Seed the fullscreen-configure size so an early `set_fullscreen` fills the output before the first resize.
    state.output_size = size_to_signed(initial_size);

    // `new_auto` walks `wayland-1..wayland-32`, binding the first free name.
    let (listening_socket, socket_name, _display_file) = bind_and_publish_wayland_socket();
    // Publish to the app launcher: every app is pointed at this socket
    // explicitly (`qos-launcher --wayland-socket`), never at the compositor's own env.
    crate::shell::launcher::set_wayland_socket(&socket_name);

    if let Err(err) =
        event_loop
            .handle()
            .insert_source(listening_socket, move |client_stream, (), state| {
                // Each accepted connection becomes a wayland-server `Client`; attach a
                // `ClientState` so `client_compositor_state` can fish out the per-client state later.
                if let Err(err) = state
                    .display_handle
                    .insert_client(client_stream, Arc::new(ClientState::default()))
                {
                    eprintln!("nirios compositor: failed to insert client: {err}");
                }
            })
    {
        eprintln!(
            "nirios compositor: insert listening socket source into event loop failed: {err}"
        );
        std::process::exit(1);
    }

    // A third display is the input-method trust boundary. The OSK package is
    // sandboxed exactly like any other app, but receives this one socket as
    // its display; public clients cannot see the input-method/virtual-keyboard
    // globals at all. The preference controls both accepting connections and
    // the supervised service process.
    let mut osk_service = match bind_osk_socket() {
        Ok((osk_socket, path)) => {
            let mut service = crate::compositor::osk_service::OskService::new(path.clone());
            let authorization = service.socket_authorization();
            if let Err(error) = event_loop.handle().insert_source(
                osk_socket,
                move |client_stream, (), state| {
                    if !authorization.allows(&client_stream) {
                        tracing::warn!(target: "qos::osk", "rejected connection to private OSK display");
                        return;
                    }
                    if let Err(error) = state
                        .display_handle
                        .insert_client(client_stream, ClientState::osk_arc())
                    {
                        tracing::warn!(target: "qos::osk", %error, "could not accept OSK Wayland connection");
                    }
                },
            ) {
                tracing::warn!(target: "qos::osk", %error, "private OSK socket source rejected");
                None
            } else {
                tracing::info!(target: "qos::osk", socket = %path.display(), "private OSK display ready");
                service.reload_preference();
                Some(service)
            }
        }
        Err(error) => {
            tracing::warn!(target: "qos::osk", %error, "private OSK display unavailable");
            None
        }
    };

    // A second Wayland socket for clients the session trusts with the
    // clipboard-manager protocols — identical set except
    // `zwlr_data_control_manager_v1`/`ext_data_control_manager_v1` are visible
    // here; see `compositor::selection` for why not on the public one.
    //
    // The privilege is the socket, not a client request: the sandbox launcher
    // bind-mounts only the public socket (`crates/packaging/launcher/src/sandbox.rs`); bind failure is logged and non-fatal.
    match bind_privileged_socket() {
        Ok((privileged_socket, path)) => {
            eprintln!("nirios compositor privileged socket on {}", path.display());
            if let Err(err) = event_loop.handle().insert_source(
                privileged_socket,
                move |client_stream, (), state| {
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
            eprintln!(
                "nirios compositor: no privileged wayland socket ({e}); clipboard managers will not be able to connect"
            );
        }
    }

    // When `NIRIOS_TEST_STATE_DUMP` is set, serializes `WorkspaceTree` there
    // each tick for `tests/compositor_test.rs`; zero-cost when unset.
    let test_dump_path = test_state_dump_path(
        crate::compositor::dev_env::DEV_ENV_OVERRIDES,
        std::env::var_os(TEST_STATE_DUMP_ENV),
    );

    // Opt-in unix socket for newline-delimited JSON commands
    // (`compositor::test_control`), bound at `QOS_TEST_CONTROL`; unset means never created.
    let (test_control_listener, test_control_unlink) = match bind_test_control_socket() {
        Some((l, g)) => (Some(l), Some(g)),
        None => (None, None),
    };
    let mut test_control_conns: Vec<TestControlConn> = Vec::new();
    // Per-process scratch context for executing PlannedActions off the
    // test-control socket — the only place a `Modifiers`/`AutoCommit`/focus-cursor lives, since real winit input isn't yet routed to the host.
    let mut test_control_ctx = TestControlCtx::new();

    // Idle / DPMS-blank / session-lock state machine (pure logic, see
    // `compositor::idle`); transitions become `backend.set_dpms` calls and
    // lock-overlay paints. `QOS_IDLE_BLANK_SECS`/`QOS_IDLE_LOCK_SECS` config
    // (default: blank after 5 min, no auto-lock). Power policy from
    // `settings.json`, read once at session start — the contract is the JSON on disk, not a shared crate.
    let power_policy = qos_power_mgmt::config::load_policy();
    let mut session = SessionManager::new(
        IdleConfig::from_env().with_suspend(power_policy.idle_suspend),
        Instant::now(),
    );
    // Session-lock unlock: input + auth state machine. Verifier is the fixed
    // test password (`QOS_LOCK_TEST_PASSWORD`) or real greetd/PAM; fail-closed otherwise.
    let mut lock = LockController::new(
        lock_auth::verifier_from_env(),
        std::env::var("USER").unwrap_or_else(|_| "user".to_string()),
    );
    // Locked edge, so the controller resets exactly when the lock engages (no stale password/error carried over).
    let mut prev_locked = false;
    // Suspend-inhibitor registry: apps hold a token via
    // `inhibit_registry.acquire(reason)`; power service mirrors it into a real logind lock, idle loop checks `is_inhibited()` before suspending.
    let inhibit_registry = qos_power_mgmt::InhibitRegistry::new();
    // Mailbox the async power service uses to reach this loop (lock, battery
    // toast, idle-reset on resume) and this loop uses to reach it (request suspend).
    let power_bridge = qos_power_mgmt::PowerBridge::new();

    // The `org.freedesktop.Notifications` D-Bus service — the compositor owns
    // it so sandboxed apps' toasts render as OS-drawn chrome. The bridge is the
    // `Send + Sync` mailbox between the async zbus service (its own tokio
    // runtime) and this loop's sync chrome pump. Failure to reach the session
    // bus downgrades to a warning; shell-internal notifications keep working regardless.
    let notif_bridge = crate::notifications::NotificationBridge::new();
    let priority_contacts = crate::priority_contacts::PriorityContactRegistry::open_default();
    let notification_sound = media::notification_sound::NotificationSoundPlayer::new();
    frame_builder.with_chrome(|st| {
        st.set_notification_bridge(notif_bridge.clone());
        st.set_notification_runtime(notification_sound.clone(), priority_contacts.clone());
    });
    // Held for the lifetime of `run` so the runtime's worker threads keep driving zbus dispatch in the background.
    let mut notif_runtime: Option<tokio::runtime::Runtime> = None;
    // RAII: dropping this releases the D-Bus bus name and tears down the
    // object server. Explicitly dropped near `process::exit` below, same as
    // the session's other RAII guards, since `process::exit` skips destructors.
    let mut notif_handle: Option<crate::notifications::ServerHandle> = None;
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
                    Ok(handle)
                });
            match res {
                Ok(h) => {
                    tracing::info!(
                        "notifications: serving {} on session bus",
                        crate::notifications::BUS_NAME
                    );
                    notif_handle = Some(h);
                }
                Err(e) => {
                    tracing::warn!(
                        "notifications: serve failed ({e}); continuing with \
                         shell-internal notifications only"
                    );
                }
            }
            // Keep the runtime alive regardless of notification outcome: the power
            // service lives on the *system* bus (logind/UPower), independent of the session-bus daemon, sharing these threads.
            notif_runtime = Some(rt);
        }
        Err(e) => {
            tracing::warn!("notifications: tokio runtime build failed ({e}); D-Bus service off");
        }
    }

    // Power service: async orchestrator suspending via logind, mirroring the
    // inhibitor registry into a logind lock, watching the battery and lid.
    // Spawned on the notify runtime; if the system bus/logind/UPower are
    // unreachable it logs and falls back to the pure idle policy (still blanks/locks/honours env timeouts, just can't actuate real suspend).
    if let Some(rt) = notif_runtime.as_ref() {
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

    // Presentation cadence via `FrameClock` (`compositor::pacing`): interval
    // comes from the backend's display; a fixed post-tick sleep instead
    // measures 21.9ms/45.6Hz regardless of actual rate. `QOS_REFRESH_HZ` overrides for testing.
    let refresh_override = pacing::refresh_override_from_env();
    let mut frame_clock =
        if let Some(interval) = refresh_override.or_else(|| backend.refresh_interval()) {
            eprintln!(
                "nirios compositor: pacing to {:.3} Hz",
                1.0 / interval.as_secs_f64()
            );
            pacing::FrameClock::new(interval, Instant::now())
        } else {
            // No display to follow (headless, or a host that won't say) — fall back rather than invent a rate.
            eprintln!(
                "nirios compositor: backend reports no refresh rate; pacing to {:.3} Hz",
                1.0 / pacing::FALLBACK_INTERVAL.as_secs_f64()
            );
            pacing::FrameClock::fallback(Instant::now())
        };
    let frame_period = frame_clock.interval();
    let mut projection_capture = super::projection_capture::ProjectionCapture::default();

    // Move presentation onto its own thread where the backend supports it.
    // Headless declines: its bytes are read back synchronously by `snapshot`.
    let mut present_pipe = backend.take_presenter().map(|p| {
        eprintln!("nirios compositor: presenting on a dedicated render thread");
        // Wake the loop when a frame retires — otherwise a tick that declined to
        // paint for lack of pipe capacity would park out the whole interval, halving throughput.
        let (ping, ping_source) = match calloop::ping::make_ping() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("nirios compositor: create present-retire ping failed: {e}");
                std::process::exit(1);
            }
        };
        if let Err(e) = event_loop.handle().insert_source(ping_source, |(), (), _| {
            // Waking is the whole effect: the loop re-evaluates its paint gate next iteration and now finds room.
        }) {
            eprintln!("nirios compositor: insert present-retire ping source failed: {e}");
            std::process::exit(1);
        }
        present::PresentPipe::spawn(p, Box::new(move || ping.ping()))
    });
    // Reused per-iteration scratch so backend events process *after*
    // `poll_events` returns — the input arm needs `&mut backend` (for `set_dpms`), unborrowable inside the poll closure.
    let mut pending_events: Vec<BackendEvent> = Vec::new();

    // `wl_output` globals: advertising the manager without an output means no
    // client learns a display exists. Synced here at startup and on every hotplug.
    let mut output_globals = output_globals::OutputGlobals::new();
    let display_handle_for_outputs = display_handle;
    {
        let outs = backend.outputs().to_vec();
        state.set_output_layout(&outs);
        output_globals.sync::<State>(
            &display_handle_for_outputs,
            &outs,
            refresh_mhz_of(&*backend),
        );
    }
    // Set when the output set changed this tick, so derived state is re-derived once per batch, not per udev edge.
    let mut outputs_dirty = false;

    // Pointer/keyboard preferences are read once at startup; this notices a
    // new Settings write so changes take effect this session, not the next.
    let mut settings_watch = crate::compositor::backend::drm::input_config::SettingsWatch::new();
    let mut switch_control = SwitchControlRuntime::new(Instant::now());
    // Redraw gating: painting every tick regardless of change would pin a CPU
    // core at 100%. Repaint only when: a backend event arrived
    // (input/resize/focus/redraw); the widget tree is animating; the chrome
    // revision bumped; a session transition fired; or the keepalive interval
    // elapsed (safety net, refresh within a second). Layout still runs every tick for hit-testing; only paint+submit are gated.
    let keepalive = Duration::from_secs(1);
    let mut last_paint = Instant::now();
    let mut last_revision = frame_builder.chrome_revision();
    let mut force_first_paint = true;
    // Optional paint-rate diagnostics (`QOS_PAINT_STATS=1`).
    let paint_stats = std::env::var("QOS_PAINT_STATS").is_ok();
    // Optional per-iteration timing (`QOS_LOOP_TIMING=1`): logs any tick over
    // 100ms non-sleeping work with a phase breakdown, so a stall is measured, not guessed.
    let loop_timing = std::env::var("QOS_LOOP_TIMING").is_ok();
    // Frame-loop instrumentation (`QOS_FRAME_TRACE`/`QOS_FRAME_SUMMARY`), free
    // when neither is set. See `compositor::metrics`.
    let mut metrics = metrics::FrameRecorder::from_env();
    metrics.set_target_period(frame_period);
    // Bench hook: shut down cleanly after N seconds so the frame trace/summary
    // flush via the normal exit path instead of a SIGKILL. Unset in production.
    // `checked_add`: an absurd env value overflowing `Instant` degrades to
    // "no deadline" rather than panicking a session over a bench-only knob.
    let run_deadline = metrics::run_seconds_from_env().and_then(|d| {
        eprintln!(
            "nirios compositor: {}={:?}; will exit cleanly after that",
            metrics::RUN_SECONDS_ENV,
            d
        );
        Instant::now().checked_add(d)
    });
    let mut stats_since = Instant::now();
    let (mut frames_total, mut frames_painted) = (0u32, 0u32);
    let (mut cause_events, mut cause_revision, mut cause_anim) = (0u32, 0u32, 0u32);
    let (mut cause_session, mut cause_keepalive) = (0u32, 0u32);
    while !close_requested {
        let iter_t0 = Instant::now();
        metrics.begin_tick(iter_t0);
        // Contextual routes from Personal Settings cross a worker-thread
        // service, then land here because ChromeState intentionally remains
        // compositor-thread owned.
        if let Some(service) = tool_service.as_ref() {
            service.pump_workspace_settings(&frame_builder.chrome_handle());
        }
        backend.poll_events(&mut |event| pending_events.push(event));
        let had_backend_events = !pending_events.is_empty();
        let mut session_changed = false;
        // `drain`, not `into_iter`: `pending_events` is declared once outside
        // this loop and reused every tick to avoid a per-frame allocation on
        // this hot path — `into_iter` would consume it, forcing a fresh `Vec` next iteration.
        #[allow(
            clippy::iter_with_drain,
            reason = "pending_events is reused across ticks precisely to avoid a per-frame \
                      allocation; into_iter() would consume it and defeat that"
        )]
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
                        // A resize is also how the compositor learns a monitor changed, so
                        // re-derive per-display widths alongside the scene extent.
                        frame_builder.set_output_geometry(output_geometry(backend.outputs()));
                        // Keep the fullscreen-configure size in sync so a fullscreened client fills the current output.
                        state.output_size = size_to_signed(size);
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
                                "nirios compositor: display changed; now pacing to {:.3} Hz",
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
                    // Instrumentation: the clock starts when the loop takes the event off the
                    // backend; the first painted frame after closes the input-to-present measurement.
                    metrics.note_input(Instant::now());
                    // Any input is activity — reset the idle timer and wake the display if it was blanked.
                    for t in session.note_activity(Instant::now()) {
                        apply_session_transition(t, backend.as_mut(), present_pipe.as_ref());
                    }
                    // Physical priority: the host's own keyboard/mouse blocks every remote
                    // peer for the next half second, stamped at the funnel so lock/VT/power-key branches count as takeover too.
                    state.remote_input_firewall.note_local_input(Instant::now());

                    if session.is_locked() {
                        // Locked: every input is captured by the lock surface, into the password
                        // field, never apps/chrome; unlocks only when `lock.poll_verification()` observes success.
                        let key = winit_event_to_lock_key(wev);
                        lock.handle_key(key);
                        // Any keystroke changes the masked field / error, so force a repaint this tick.
                        session_changed = true;
                    } else if is_power_key(wev) {
                        // Hardware power key resolves per the power policy (logind delegates via
                        // the service's block inhibitor); a deliberate press bypasses the idle inhibitor.
                        match power_policy.on_power_key() {
                            Some(qos_power_mgmt::PowerAction::Suspend) => {
                                tracing::info!(target: "qos::power", "power key: requesting suspend");
                                power_bridge.request_suspend();
                            }
                            Some(qos_power_mgmt::PowerAction::Lock) => {
                                for t in session.request_lock(Instant::now()) {
                                    apply_session_transition(
                                        t,
                                        backend.as_mut(),
                                        present_pipe.as_ref(),
                                    );
                                }
                            }
                            Some(qos_power_mgmt::PowerAction::Ignore) | None => {}
                        }
                    } else if let Some(vt) = vt_switch_chord(wev, test_control_ctx.modifiers) {
                        // Ahead of every other chord, never offered to a client: leaving the
                        // terminal must work even with a grabbed keyboard, or a client can strand the user.
                        backend.switch_vt(vt);
                    } else if is_remote_input_kill_switch(wev, test_control_ctx.modifiers) {
                        // Super+Esc: every peer's input stops and stays stopped. Ahead of the
                        // client router and shell's Escape handling — a panic key a client can swallow isn't one.
                        client_input::stop_remote_input(&mut state, &mut frame_builder);
                        session_changed = true;
                    } else if is_lock_chord(wev, test_control_ctx.modifiers) {
                        // Super+L locks immediately.
                        for t in session.request_lock(Instant::now()) {
                            apply_session_transition(t, backend.as_mut(), present_pipe.as_ref());
                        }
                    } else if let Some(mk) = crate::shell::media_keys::classify(wev) {
                        // Hardware media/brightness keys belong to the machine, not whichever
                        // client holds focus — handled before client routing for that reason.
                        if let Some(osd) = crate::shell::media_keys::apply(
                            mk,
                            frame_builder.host().qs_actions(),
                            frame_builder.host().status_bridge(),
                        ) {
                            frame_builder.with_chrome(|st| st.show_osd(osd));
                            session_changed = true;
                        }
                    } else {
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
                            // Dispatch real input through the same wire the headless `execute_planned_action` path uses.
                            frame_builder.dispatch_event(
                                wev,
                                &test_control_ctx.modifiers,
                                &mut test_control_ctx.doc,
                                &test_control_ctx.root_obj,
                                &mut test_control_ctx.focus,
                            );
                        }
                    }
                }
                BackendEvent::ModifiersChanged(modifiers) => {
                    // Carry the modifier mask forward so the next event dispatched (or the
                    // lock-chord check above) sees the right Ctrl/Shift/Alt/Super state.
                    test_control_ctx.modifiers = modifiers;
                }
                BackendEvent::OutputsChanged => {
                    // A display changed — everything derived from the output set re-derives
                    // at once: scene extent, per-display widths, fullscreen size, cadence, `wl_output` globals.
                    outputs_dirty = true;
                    session_changed = true;
                }
                BackendEvent::ShellGesture(action) => {
                    // A 3/4-finger gesture, already resolved into its navigation meaning —
                    // never also delivered to a client, so no double-handling to guard against.
                    if !session.is_locked() && client_input_allowed(&frame_builder) {
                        for t in session.note_activity(Instant::now()) {
                            apply_session_transition(t, backend.as_mut(), present_pipe.as_ref());
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
                    // The same finger also arrives as pointer events (backend emits both),
                    // driving shell chrome; this half is `wl_touch` for real multi-touch clients.
                    if !session.is_locked() && client_input_allowed(&frame_builder) {
                        let target = client_input::touch_target(&state, &frame_builder, &touch);
                        state.forward_client_touch(touch, target);
                    }
                }
                BackendEvent::Redraw | BackendEvent::Focus(_) => {
                    // Redraw is implicit (we re-paint every tick); focus changes don't gate input dispatch here.
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
                    frame_builder.dispatch_event(
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

        // The Settings app writes to a file with no notification, so poll cheaply
        // (once/sec) to reach input devices and display scale.
        if settings_watch.changed(Instant::now()) {
            tracing::info!("compositor: settings changed on disk; re-applying");
            state.reload_keyboard_settings();
            backend.reload_input_settings();
            let policy = qos_power_mgmt::config::load_policy();
            session.set_suspend_timeout(policy.idle_suspend);
            power_bridge.update_policy(policy);
            outputs_dirty = true;
            if let Some(service) = osk_service.as_mut() {
                service.reload_preference();
            }
        }
        if let Some(service) = osk_service.as_mut() {
            service.tick();
        }

        // Re-derive everything depending on the output set once, after the whole
        // batch — a dock plug raises several udev edges, and per-edge re-derivation would advertise globals the next edge immediately supersedes.
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
                state.output_size = size_to_signed(combined);
                if let Some(pipe) = present_pipe.as_ref() {
                    pipe.resize(combined);
                }
            }
            // Windows aren't pinned to an output; re-pushing geometry re-homes
            // everything on a display that went away, so nothing's left with no panel behind it.
            frame_builder.set_output_geometry(output_geometry(&outs));
            // Per-output scale: `wp_fractional_scale` is the scale of the output a
            // surface is on, so a changed layout changes what each client renders at.
            state.set_output_layout(&outs);
            output_globals.sync::<State>(
                &display_handle_for_outputs,
                &outs,
                refresh_mhz_of(&*backend),
            );
            if refresh_override.is_none()
                && let Some(interval) = backend.refresh_interval()
                && frame_clock.set_interval(interval)
            {
                eprintln!(
                    "nirios compositor: display configuration changed; now pacing to {:.3} Hz",
                    1.0 / interval.as_secs_f64()
                );
                metrics.set_target_period(interval);
            }
        }
        let t_input_done = iter_t0.elapsed();
        metrics.mark_input(t_input_done);

        // Advance the idle timers and apply any timeout-driven transitions (blank, auto-lock, suspend).
        for t in session.tick(Instant::now()) {
            if t == SessionTransition::Suspend {
                // Idle-suspend is gated by the inhibitor registry (media playing, a task
                // running); when inhibited, un-latch so it re-fires the instant the last inhibitor releases, no fresh input needed.
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
            // Any session transition (blank/lock/suspend) changes what should be on screen — force a repaint.
            session_changed = true;
        }

        // Apply signals the async power service raised (lid/critical lock, battery
        // toasts, resume); suspend requests flow the other way via `power_bridge.request_suspend()` above.
        for sig in power_bridge.drain_signals() {
            session_changed = true;
            match sig {
                qos_power_mgmt::PowerSignal::Lock => {
                    for t in session.request_lock(Instant::now()) {
                        apply_session_transition(t, backend.as_mut(), present_pipe.as_ref());
                    }
                }
                qos_power_mgmt::PowerSignal::ResumedFromSleep => {
                    // Reset the idle timer so a stale pre-sleep timestamp doesn't immediately re-blank/re-suspend on resume.
                    for t in session.note_activity(Instant::now()) {
                        apply_session_transition(t, backend.as_mut(), present_pipe.as_ref());
                    }
                    // Put the display back — a suspend doesn't pause the seat, so the DRM device
                    // returns with no mode/framebuffer/cursor plane; without this, lid close/open is an unrecoverable black screen.
                    backend.reset_display();
                    // A resumed panel is scanning out whatever was left in its buffers, so the next frame must be a full one.
                    outputs_dirty = false;
                    session_changed = true;
                }
                qos_power_mgmt::PowerSignal::IdleSuspendChanged(timeout) => {
                    session.set_suspend_timeout(timeout);
                    tracing::info!(target: "qos::power", ?timeout, "live idle-suspend timeout updated");
                }
                qos_power_mgmt::PowerSignal::Notify(n) => {
                    let inc = power_notification_to_incoming(&notif_bridge, n);
                    notif_bridge.push_incoming(inc);
                }
            }
        }

        // `ext_session_lock_v1`: fold a protocol client's lock/unlock into the one
        // authoritative session lock. Client `lock` drives the same locked state as
        // chord/idle; `unlock_and_destroy` clears it (client authenticated itself —
        // the compositor's password path is only for chord/idle). The stashed
        // `SessionLocker` confirms after the overlay paints, below, so `locked` fires only once content is hidden.
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

        // Async unlock: pick up an in-flight credential verify's result from the
        // worker thread (started by lock-screen Enter). The loop never blocks —
        // it keeps ticking and unlocks only on verified success; a wrong/failed
        // result surfaces a non-leaky error and stays locked. Polled every tick (~16ms).
        if let Some(outcome) = lock.poll_verification() {
            // The field/status changed (unlocked, or error shown) — repaint.
            session_changed = true;
            if outcome == UnlockOutcome::Unlocked {
                for t in session.request_unlock(Instant::now()) {
                    apply_session_transition(t, backend.as_mut(), present_pipe.as_ref());
                }
            }
        }

        // Reset lock-screen input the instant the lock engages, so a fresh lock
        // starts with an empty field and no stale error from a previous episode.
        if session.is_locked() && !prev_locked {
            lock.reset();
        }
        prev_locked = session.is_locked();

        // Pump the D-Bus notification bridge into the toast stack and mirror lock
        // state into the chrome so it can suppress content while locked; cheap no-ops when unchanged.
        frame_builder.with_chrome(|st| {
            st.set_session_locked(session.is_locked());
            st.pump_dbus_notifications();
        });

        // Mirror the live fullscreen state (`XdgShellHandler` fullscreen path) into
        // the rendered scene so a promoted group takes the whole output and exit
        // restores prior tiling. The overlay host guards against an unhosted group
        // id (client tree and rendered tree aren't unified yet) — a safe no-op, not a blanked shell.
        frame_builder.set_fullscreen(state.fullscreen_group);

        {
            // Mirror foreign toplevels adopted at the protocol layer into the rendered
            // tab tree, reaping tabs whose client went away — a diff of protocol tree
            // against rendered, so idempotent and self-healing (no phantom tabs from a
            // dropped event). A real change bumps the chrome revision, driving the strip resync that splices the tab into the widget tree next layout.
            let mirrored = {
                let chrome = frame_builder.chrome();
                let mut rendered = chrome.tree_mut();
                foreign::reconcile_foreign_tabs(&state.workspace_tree, &mut rendered)
            };
            if mirrored {
                frame_builder.with_chrome(super::super::shell::chrome_state::ChromeState::bump);
            }
            // The mirror above is the single source of tab adoption, so the input
            // router's own queues are pure bookkeeping — drained each tick so they can't grow across a long session.
            state.client_input.clear_pending();
        }
        // Reconcile the seat's keyboard focus with the focused tab every tick, so a
        // tab/group/workspace switch or overlay open moves the keyboard immediately, not on the next keystroke.
        client_input::sync_keyboard_focus(&mut state, &frame_builder);
        frame_builder.set_osk_surface(
            state.osk_surface,
            state.client_input.keyboard_focus(),
            state.text_input_active(),
        );

        // A file dragged out of the hosted file manager over a client's window becomes a real Wayland drag — see `pump_chrome_drag`.
        client_input::pump_chrome_drag(&mut state, &frame_builder);

        // Hand the remote-input firewall this frame's screen: foreign tile
        // positions, keyboard holder, chrome-drawn-over, locked — refreshed before remote events drain.
        client_input::publish_remote_input_view(&mut state, &frame_builder, session.is_locked());

        // Per-output HiDPI: tell each client the scale of its current display.
        // Every tick, since a window dragged to another monitor's density is otherwise unobserved.
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

        // Decide whether this frame needs repainting before the expensive layout
        // pass: `needs_animation_frame` walks the widget tree, and the chrome
        // revision is a counter bumped by state mutations, so skipping
        // `relayout`+`paint` on idle frames holds near-zero CPU at rest.
        // Pump the widget tree's per-frame state (sync + update) before
        // deciding whether to paint — drains a hosted terminal's PTY;
        // returns true when changed, itself a paint reason. Drain the
        // network actor into the replica: peer changes fold in before this frame's pump reads them, bumping the revision to repaint.
        if let Some(net) = peer_sync.as_ref() {
            net.tick();
        }
        // Keep the presence relay current, then fold in what peers sent. Pointer
        // is read level-triggered (covers every source: seat, remote peer, test
        // socket); room set is re-read each tick since a workspace can join a
        // room mid-session and the subscription must follow.
        let peers_moved = if let Some(relay) = frame_builder.peer_presence() {
            // The pointer as the input state knows it — "nowhere", not the origin, for
            // a session that never moved one; being level-triggered, `cursor()` would paint a cursor for nobody.
            relay.set_cursor_position(state.client_input.pointer_position());
            let (rooms, here) = {
                // Clone the tree handle and drop the `ChromeState` borrow before
                // locking the tree — nothing here needs the RefCell guard itself.
                let tree_handle = frame_builder.chrome_handle().borrow().tree_handle();
                let tree = tree_handle.read();
                let rooms = tree
                    .workspaces
                    .iter()
                    .map(|w| w.room.to_string())
                    .collect::<Vec<_>>();
                // The room the user is looking at (where their pointer is), distinct from
                // the set above (every room the session listens to).
                let here = tree
                    .workspaces
                    .iter()
                    .find(|w| w.id == tree.active)
                    .map(|w| w.room.to_string());
                (rooms, here)
            };
            relay.set_rooms(rooms);
            // Say where this session's pointer is regardless of what's running — the
            // app-driven republish alone stayed silent on an empty desktop, so peers never saw each other.
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
        metrics.mark_pump(
            iter_t0
                .elapsed()
                .checked_sub(t_input_done)
                .unwrap_or_default(),
        );
        // Is there anything new to show? This redraw gating lets a static desktop
        // idle at near-zero CPU, orthogonal to *when* a frame may show (the clock's job below).
        let content_changed = force_first_paint
            || had_backend_events
            || session_changed
            || revision_changed
            || animating
            || apps_changed
            || peers_moved
            || keepalive_due;
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
        }
        let should_paint = content_changed && cadence_due && pipe_ready;

        // Diagnostic: `QOS_PAINT_STATS=1` logs every ~2s how many frames painted
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
                }
                if keepalive_due
                    && !(had_backend_events || session_changed || revision_changed || animating)
                {
                    cause_keepalive = cause_keepalive.saturating_add(1);
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
                stats_since = Instant::now();
            }
        }

        if should_paint {
            // Lay out only when about to paint — any structural change (spliced group,
            // tab switch, resize) is applied here, right before the scene is built.
            let t_layout_start = Instant::now();
            frame_builder.relayout();
            let layout_took = t_layout_start.elapsed();
            metrics.mark_layout(layout_took);
            let t_scene_start = Instant::now();
            // When locked, paint the compositor-owned lock overlay instead of the chrome — app content is never submitted while locked.
            let scene = if session.is_locked() {
                let theme = frame_builder.theme().clone();
                // The lock overlay covers every known output, centred on each (single on
                // winit/headless; DRM lays monitors side-by-side); falls back to one output spanning the viewport if none reported yet.
                let lock_outputs: Vec<LockOutput> = backend
                    .outputs()
                    .iter()
                    .map(|o| LockOutput {
                        origin: (f64::from(o.position.0), f64::from(o.position.1)),
                        size: (f64::from(o.size.0), f64::from(o.size.1)),
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
                };
                lock_screen::draw_lock_scene(
                    &lock_outputs,
                    &theme,
                    frame_builder.text_renderer_mut(),
                    &view,
                )
            } else {
                let mut scene = frame_builder.paint();
                // On top of everything, since a drag is over whatever it's over — drawn
                // here (not by either app) because once the pointer leaves the source window, neither end can paint the carried item.
                if state.selection.dnd_active || state.selection.chrome_drag.is_some() {
                    let label = state.selection.dnd_label.get();
                    let (cx, cy) = state.client_input.cursor();
                    let theme = frame_builder.theme().clone();
                    drag_feedback::draw(
                        &mut scene,
                        vello::kurbo::Point::new(cx, cy),
                        label.as_deref(),
                        &theme,
                        frame_builder.text_renderer_mut(),
                    );
                }
                scene
            };
            metrics.mark_scene(t_scene_start.elapsed());
            // The lock screen is never transmitted: a receiver session may
            // remain connected while the seat locks, but remote observers do
            // not get password entry, notification counts, or unlock timing.
            if !session.is_locked() && frame_builder.host().projection_capture_requested() {
                let viewport = frame_builder.viewport();
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
                let _ = projection_capture.submit_scene(&scene, clamped_viewport);
            }
            let t_submit_start = Instant::now();
            let mut render_thread_lost = false;
            let async_present = if let Some(pipe) = present_pipe.as_mut() {
                render_thread_lost = !pipe.submit(scene, OutputId(0));
                true
            } else {
                backend.submit_frame(&scene, OutputId(0));
                false
            };
            if render_thread_lost {
                // Presentation is dead but the session isn't — clients stay served and
                // protocol state is kept, a far better failure mode than tearing the seat down.
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
            // Advance the deadline series off the moment the frame was handed to the
            // display — the clock paces *production*; the render thread's vsync paces consumption behind it.
            frame_clock.note_presented(submit_done);
            last_paint = Instant::now();
            // Re-read after `relayout`: its `resync_strips_from_tree` pass can bump
            // the revision, so capture the post-paint value or the next idle tick sees a phantom change.
            last_revision = frame_builder.chrome_revision();
            force_first_paint = false;
            // The layout pass above published each foreign tab's rect — tell clients
            // what size to render at, then release the frame throttle for on-screen ones; both no-ops with no external client.
            foreign::flush_configures(&mut state);
            foreign::release_frame_throttle(&mut state);
        } else if had_backend_events {
            // Input was dispatched but the frame deferred — lay out anyway so the *next*
            // event hit-tests post-input rects, not the pre-overlay tree. Layout is ~0.01ms.
            frame_builder.relayout();
        }
        let t_paint_done = iter_t0.elapsed();

        // Confirm an `ext_session_lock_v1` client's lock only after the overlay is
        // presented and locked, so `locked` never fires while app content could still show; a failed lock notifies the client automatically.
        if session.is_locked()
            && let Some(locker) = state.session_lock.take_pending_locker()
        {
            locker.lock();
        }

        // Accept new test-control connections, drain pending bytes from each open
        // one (parses commands, writes JSON responses); closed ones are collected below. Non-blocking so the frame-pacing loop never stalls.
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
                    // Socket-driven input starts the same latency clock hardware input does.
                    // Serviced after this tick's paint gate, so the reflecting frame is the next one — exactly the latency a caller experiences.
                    metrics.note_input(Instant::now());
                }
            }
            test_control_conns.retain(|c| !c.closed);
        }
        let t_testctl_done = iter_t0.elapsed();
        metrics.mark_testctl(t_testctl_done.checked_sub(t_paint_done).unwrap_or_default());

        // Drain wayland client traffic and push updates back out: `dispatch_clients`
        // advances each client's request queue through the `*Handler` impls, `flush_clients` pumps server events back. Mirrors smithay 0.7's minimal example.
        if let Err(err) = display.dispatch_clients(&mut state) {
            eprintln!("nirios compositor: dispatch_clients error: {err}");
            break;
        }
        // Selection bookkeeping after dispatch, not in the protocol handler:
        // smithay raises `new_selection` before installing the new source, so an inline pull would read the previous owner.
        state.pump_selection();
        if let Err(err) = display.flush_clients() {
            eprintln!("nirios compositor: flush_clients error: {err}");
            break;
        }
        let t_work_done = iter_t0.elapsed();
        metrics.mark_clients(t_work_done.checked_sub(t_testctl_done).unwrap_or_default());
        if loop_timing && t_work_done > Duration::from_millis(100) {
            eprintln!(
                "SLOW-ITER total_work={:?} | input={:?} paint=+{:?} testctl=+{:?} dispatch_clients=+{:?}",
                t_work_done,
                t_input_done,
                t_paint_done.checked_sub(t_input_done).unwrap_or_default(),
                t_testctl_done.checked_sub(t_paint_done).unwrap_or_default(),
                t_work_done.checked_sub(t_testctl_done).unwrap_or_default(),
            );
        }

        // Pump pending calloop sources (including the listening socket) and sleep
        // up to one frame — dispatch is what fans new connections into
        // `insert_client`. Parks until the next presentation deadline (see
        // `FrameClock::park`): a mid-interval wakeup must never push the next frame past its deadline.
        let park = frame_clock.park(Instant::now());
        let t_sleep_start = Instant::now();
        if let Err(err) = event_loop.dispatch(park, &mut state) {
            eprintln!("nirios compositor: event loop dispatch error: {err}");
            break;
        }
        metrics.mark_sleep(t_sleep_start.elapsed());
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
            && let Ok(json) = serde_json::to_vec(&state.workspace_tree)
            && let Err(e) = atomic_file::write_creating_parent(std::path::Path::new(path), &json)
        {
            tracing::warn!(error = %e, path = ?path, "workspace tree save failed");
        }
    }

    // Stop the render thread and wait for the frame in flight, so a
    // half-presented frame can't outlive its device — explicit since `process::exit` skips destructors.
    if let Some(mut pipe) = present_pipe.take() {
        pipe.shutdown();
    }

    // Flush the frame trace / write the summary before `process::exit` skips destructors; a no-op unless instrumentation was enabled.
    metrics.finish();

    // `event_loop.run` only returns when asked to stop (never happens today);
    // kept explicit so the function's `!` return type is honoured. `display` drops here, severing its clients.
    drop(display);
    // Explicitly drop the test-control socket guard so its `Drop` unlinks the
    // file before `process::exit` skips destructors; the panic path already runs `Drop` via unwind.
    drop(test_control_unlink);
    // Same reasoning for the collab broker: its `Drop` quiesces the drain
    // thread and unlinks both sockets, else the next session must tell a stale file from a live one.
    drop(collab_broker);
    drop(tool_service);
    // Same reasoning again: releases the D-Bus bus name and tears down the
    // notification object server before `process::exit` skips its destructor.
    drop(notif_handle);
    std::process::exit(0)
}

/// Session-owned ambient-light loop. Settings only records policy; the
/// compositor owns the display and therefore owns applying that policy after
/// Settings exits. Missing sensors/backlights simply produce no write.
fn spawn_auto_brightness_worker() {
    let _ = std::thread::Builder::new()
        .name("qos-auto-brightness".into())
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
                if enabled {
                    if let Some(lux) = os_actions::ambient_lux()
                        && let Some(percent) = controller.update(lux)
                        && let Err(error) = os_actions::set_brightness(percent)
                    {
                        tracing::warn!(target: "qos::display", %error, "automatic brightness write failed");
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
mod tests {
    //! Unit coverage for the test-control execution path. The integration test
    //! (`tests/control_socket_test.rs`) needs a Wayland display and is
    //! `#[ignore]` by default; these exercise the same `execute_planned_action`
    //! → `FrameBuilder::dispatch_event` wire without a process, so CI without winit still gates here.
    use super::*;

    #[test]
    fn fresh_workspace_salt_does_not_create_a_transport_identity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state_path = dir.path().join("config/state.json");
        let peer_key_path = dir.path().join("system/device-key");

        let first = load_or_create_workspace_salt(&state_path, &peer_key_path)
            .expect("create workspace salt");
        let second = load_or_create_workspace_salt(&state_path, &peer_key_path)
            .expect("reload workspace salt");

        assert_ne!(first, 0);
        assert_eq!(first, second);
        assert!(
            !peer_key_path.exists(),
            "allocating an object-id namespace must not mint a network identity"
        );
    }

    #[test]
    fn existing_transport_identity_keeps_its_historical_workspace_salt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state_path = dir.path().join("config/state.json");
        let peer_key_path = dir.path().join("system/device-key");
        let key = crate::network::behaviour::load_or_create_keypair(&peer_key_path)
            .expect("create existing transport identity");
        let expected = crate::network::behaviour::peer_salt_from_keypair(&key).max(1);

        let actual = load_or_create_workspace_salt(&state_path, &peer_key_path)
            .expect("migrate historical salt");

        assert_eq!(actual, expected);
        assert_eq!(
            read_workspace_salt(
                &state_path
                    .parent()
                    .expect("fixture state path has a parent")
                    .join("workspace-peer-salt"),
            )
            .expect("read persisted salt"),
            expected
        );
    }

    #[test]
    fn onboarding_blocks_every_client_input_route() {
        let mut fb = FrameBuilder::new((800, 600));
        assert!(client_input_allowed(&fb));
        fb.enable_onboarding(std::path::PathBuf::from("/tmp/qos-onboarding-test"));
        assert!(fb.host().onboarding_active());
        assert!(
            !client_input_allowed(&fb),
            "trusted first-login chrome must own input before any Wayland client"
        );
    }

    /// The compositor binds the socket the launcher dials. Assert across both
    /// production paths, not either one's idea of the path: bind with the
    /// compositor's spawn function, resolve with the launcher's resolver,
    /// require an open descriptor. `required@1` makes a divergence an error, not the skip an optional service gets.
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
        let fb = FrameBuilder::new((800, 600));
        let chrome = fb.chrome_handle();
        let broker = spawn_session_collab_broker(&chrome, None)
            .expect("the compositor must be able to bind its own collab socket");

        assert!(
            chrome.borrow().collab_available(),
            "a session that bound its broker must not report collaboration unavailable: {:?}",
            chrome.borrow().collab_unavailable
        );

        // The launcher's real resolver, on a manifest demanding the service —
        // `resolve_service_binds` returns `RequiredMissing` if it looks where the compositor didn't bind.
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
            resolved[0].host_fd() >= 0,
            "the launcher must hand the app a real connected descriptor"
        );

        drop(broker);
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };
    }

    /// A live socket is not taken away from the session that owns it. Two
    /// compositors against one `XDG_RUNTIME_DIR` is what a second login (or
    /// stray test) produces, and the broker unlinks before binding — without
    /// the liveness probe, the newcomer would silently strand the first session's apps. Refusing loudly is correct, and must reach the chrome.
    #[test]
    #[serial_test::serial(process_env)]
    fn a_second_session_refuses_to_take_a_live_collab_socket() {
        let runtime = tempfile::tempdir().expect("tempdir");
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", runtime.path()) };

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

    // developer surfaces are absent from a shipped build

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
        // The two functions above are only meaningful if `run` passes the real
        // constant — tied here so flipping the production call site to a literal `true` fails.
        assert_eq!(
            crate::compositor::dev_env::DEV_ENV_OVERRIDES,
            cfg!(debug_assertions)
        );
    }

    // publishing the socket name for the nested dev loop

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
        use std::os::unix::fs::PermissionsExt;

        // Round-trip through the real writer: what lands on disk must be usable as
        // `WAYLAND_DISPLAY=$(cat …)` — no trailing newline, no quoting.
        let dir = std::env::temp_dir().join(format!("qos-display-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp runtime dir");
        let path = session_display_path(Some(dir.clone().into_os_string()))
            .expect("absolute temp dir yields a path");
        std::fs::create_dir_all(path.parent().unwrap()).expect("create qos-session dir");
        atomic_file::write_with_mode(&path, b"wayland-7", 0o600).expect("write display file");

        let read = std::fs::read_to_string(&path).expect("read back");
        assert_eq!(read, "wayland-7");

        let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "session file must not be readable by others");

        let _ = std::fs::remove_dir_all(&dir);
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

    /// Sanity: `CursorMoved` updates the cached cursor and dispatches without
    /// panicking — matters because `MouseInput` (no position on winit) hit-tests against `last_cursor` inside the host.
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

    /// The full click sequence (`CursorMoved` + down + up) dispatches all
    /// three without panicking — `click_at` returns them, and all three must reach `ShellOverlayHost::handle_event`.
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

    /// Click sequence at the wheel-disc's plus slot mutates chrome state —
    /// proves planned actions reach the host's click handlers, not a void.
    ///
    /// The bootstrap tree seeds three demo workspaces; the plus slot calls
    /// `chrome.add_workspace("Untitled")`, growing the count to four —
    /// asserting on the count is more robust than internal widget state, since chrome is the contract every consumer sees.
    #[test]
    fn click_on_wheel_plus_slot_grows_workspace_count() {
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

        let initial_count = fb.chrome().tree().workspaces.len();

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
                if fb.chrome().tree().workspaces.len() > initial_count {
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

    // drag / key / type / modifiers executor coverage

    /// `SetModifiers` mutates `ctx.modifiers` directly; the host reads the
    /// snapshot on the next chord, so this is what chord injection (Ctrl+K, …) depends on.
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

    /// A keyboard press+release pair dispatches without panicking — same bar
    /// as `full_click_sequence_dispatches_without_panic`, for both press and release.
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

    // snapshot executor coverage

    /// Sanity: `Snapshot` against a fresh `FrameBuilder` writes a real PNG,
    /// asserting the magic bytes and a non-trivial byte count (a
    /// corrupted/truncated PNG would pass the magic check but be tiny).
    ///
    /// Skipped without a wgpu adapter (CI without llvmpipe), where the
    /// executor logs and returns with no file. Serialised with the other [`SNAPSHOT_RENDERER`] tests, now a shared process-wide cache.
    #[test]
    #[serial_test::serial(snapshot_renderer)]
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

    /// The snapshot renderer must outlive the thread that built it — see
    /// [`SNAPSHOT_RENDERER`] for why a per-thread cache aborts the process. An
    /// abort can't be observed from inside the binary it kills, so this
    /// asserts the preventing property instead: a snapshot on a thread that
    /// then exits must leave its renderer behind, so the next snapshot at the
    /// same viewport builds nothing new. A per-thread cache fails this on the count, before it can abort anything.
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

    /// Hostile path inputs — the executor must log and return without
    /// panicking: the dispatcher's `Ok` is already on the wire, so a client typo must not kill the compositor.
    #[test]
    #[serial_test::serial(snapshot_renderer)]
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

    // state_dump

    /// `chrome_state_json` returns an object with an `overlays` key carrying
    /// the four flags — matches what `qos-control state-dump` queries, so a regression here silently breaks the CLI.
    #[test]
    fn state_dump_chrome_includes_overlays_field() {
        let fb = FrameBuilder::new((1600, 900));
        let state = FrameBuilderControlState { fb: &fb, doc: None };
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

    /// `workspace_tree_json` exposes a `workspaces` array on the bootstrap
    /// layout — the CLI's `state-dump` smoke depends on it as the consumer's entrypoint to the workspace list.
    #[test]
    fn state_dump_workspace_tree_includes_workspaces_array() {
        let fb = FrameBuilder::new((1600, 900));
        let state = FrameBuilderControlState { fb: &fb, doc: None };
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

    /// The state dump has to carry the roster — how a harness observes who
    /// the shell believes is present. `Workspace::collaborators` is
    /// `#[serde(skip_deserializing)]` (a restarted shell must not paint gone
    /// sessions), but write stays open: closing it with a plain `skip` would silently empty the roster from the socket, looking like agreement.
    #[test]
    fn state_dump_carries_the_roster_it_is_asked_about() {
        let fb = FrameBuilder::new((1600, 900));
        {
            let chrome = fb.chrome();
            // Drop the `ChromeState` borrow before locking the tree — nothing
            // here needs it once the handle is cloned out.
            let tree_handle = chrome.tree_handle();
            drop(chrome);
            let mut tree = tree_handle.write();
            let room = tree.active_workspace().expect("active workspace").room;
            let noted = tree.note_peer_present(room, 0x51, "alice");
            drop(tree);
            assert!(noted);
        }
        let state = FrameBuilderControlState { fb: &fb, doc: None };
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

    /// Dispatching `Command::StateDump` returns `Response::State` with no
    /// actions. Integration round-trip lives in `tests/control_socket_test.rs`; this smoke catches regressions needing no spawned compositor.
    #[test]
    fn dispatch_state_dump_against_frame_builder_returns_state() {
        let fb = FrameBuilder::new((1600, 900));
        let state = FrameBuilderControlState { fb: &fb, doc: None };
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

    /// Cmd+Tab via `test_control` opens the overview overlay — proves
    /// modifiers + named-key chords reach the host's chord handler, like `ShellDriver::press_key` in-process.
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

    /// Dropping an `UnlinkOnDrop` removes the file at its path — `run` holds
    /// one around the test-control socket so it unlinks on clean exit + panic-unwind.
    #[test]
    fn unlink_on_drop_removes_socket_file_on_drop() {
        let tmpdir = tempfile::tempdir().expect("tempdir");
        let path = tmpdir.path().join("nirios-test-control.sock");
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

    /// `read_bounded_line` caps a malicious/buggy client's input at
    /// `TEST_CONTROL_MAX_LINE_BYTES`; the caller closes the connection on `TooLarge`.
    #[test]
    fn pump_drops_oversized_lines() {
        use std::io::BufReader;
        let stream: Vec<u8> = vec![b'A'; 100_000];
        let mut reader = BufReader::new(&stream[..]);
        let mut out = String::new();
        let res = read_bounded_line(&mut reader, &mut out, TEST_CONTROL_MAX_LINE_BYTES);
        match res {
            Err(ReadLineError::TooLarge) => {
                // Expected: `out` is intentionally still empty — truncated bytes aren't
                // surfaced to the parser since they'd parse as garbage anyway.
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

    // session lock chord / unlock detection

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
        let ev = keyboard_input_event(&Key::Character(SmolStr::new("l")), true);
        assert!(is_lock_chord(&ev, mods_with_super(true)));
        // Upper-case 'L' (Shift+Super+L) also counts.
        let ev_upper = keyboard_input_event(&Key::Character(SmolStr::new("L")), true);
        assert!(is_lock_chord(&ev_upper, mods_with_super(true)));
    }

    /// Without Super held, plain `l` is not the lock chord (it types).
    #[test]
    fn l_without_super_is_not_lock_chord() {
        let ev = keyboard_input_event(&Key::Character(SmolStr::new("l")), true);
        assert!(!is_lock_chord(&ev, mods_with_super(false)));
    }

    /// The lock chord only fires on press, not release — else key-up would re-trigger and toggle twice.
    #[test]
    fn super_l_release_is_not_lock_chord() {
        let ev = keyboard_input_event(&Key::Character(SmolStr::new("l")), false);
        assert!(!is_lock_chord(&ev, mods_with_super(true)));
    }

    /// `Super+Esc` on press is the remote-input kill switch.
    #[test]
    fn super_escape_press_is_the_remote_input_kill_switch() {
        let ev = keyboard_input_event(&Key::Named(NamedKey::Escape), true);
        assert!(is_remote_input_kill_switch(&ev, mods_with_super(true)));
    }

    /// Escape on its own closes an overlay and reaches modal clients; must not stop a shared session by accident.
    #[test]
    fn escape_without_super_is_not_the_kill_switch() {
        let ev = keyboard_input_event(&Key::Named(NamedKey::Escape), true);
        assert!(!is_remote_input_kill_switch(&ev, mods_with_super(false)));
    }

    /// Press only — a re-firing release would engage the latch twice and post a second toast for one keystroke.
    #[test]
    fn super_escape_release_is_not_the_kill_switch() {
        let ev = keyboard_input_event(&Key::Named(NamedKey::Escape), false);
        assert!(!is_remote_input_kill_switch(&ev, mods_with_super(true)));
    }

    /// The panic key is not a key a client can be handed — `Super` is the
    /// compositor modifier the shell's chord table already claims, so no client can steal it.
    #[test]
    fn super_escape_is_a_shell_chord_and_never_reaches_a_client() {
        let ev = keyboard_input_event(&Key::Named(NamedKey::Escape), true);
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

    /// A non-keyboard event (cursor move) is never the lock chord even with Super held.
    #[test]
    fn cursor_move_is_not_lock_chord() {
        let ev = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: PhysicalPosition::new(10.0, 10.0),
        };
        assert!(!is_lock_chord(&ev, mods_with_super(true)));
    }

    /// Enter and Escape on press are the explicit unlock action.
    #[test]
    fn enter_and_escape_press_map_to_submit_and_cancel() {
        let enter = keyboard_input_event(&Key::Named(NamedKey::Enter), true);
        let escape = keyboard_input_event(&Key::Named(NamedKey::Escape), true);
        assert_eq!(winit_event_to_lock_key(&enter), LockKey::Submit);
        assert_eq!(winit_event_to_lock_key(&escape), LockKey::Cancel);
    }

    /// A plain character keypress feeds the password buffer as text — it doesn't itself unlock.
    #[test]
    fn char_key_maps_to_text() {
        let ev = keyboard_input_event(&Key::Character(SmolStr::new("a")), true);
        assert_eq!(winit_event_to_lock_key(&ev), LockKey::Text("a".to_string()));
    }

    /// Backspace maps to a buffer edit; a key *release* is ignored (only presses type).
    #[test]
    fn backspace_maps_and_release_is_ignored() {
        let bs = keyboard_input_event(&Key::Named(NamedKey::Backspace), true);
        assert_eq!(winit_event_to_lock_key(&bs), LockKey::Backspace);
        let release = keyboard_input_event(&Key::Named(NamedKey::Enter), false);
        assert_eq!(winit_event_to_lock_key(&release), LockKey::Ignored);
    }

    /// `apply_session_transition` maps Blanked/Woke to `set_dpms`;
    /// Locked/Unlocked are pure state and touch no backend — asserted via a spy backend recording `set_dpms` calls.
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
        // Blanked → set_dpms(false), Woke → set_dpms(true); Locked/Unlocked produce no backend calls.
        assert_eq!(backend.dpms_calls, vec![false, true]);
    }
}

/// The refresh rate to advertise on `wl_output`, in millihertz. Derived
/// from the cadence the compositor is actually pacing to, so client frame
/// timing agrees with real production. `None` when the backend has no
/// display; globals then advertise 60Hz rather than zero, which some toolkits divide by.
fn refresh_mhz_of(backend: &dyn crate::compositor::backend::Backend) -> Option<u32> {
    backend
        .refresh_interval()
        .filter(|d| !d.is_zero())
        .map(|d| {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "clamped to [0.0, u32::MAX] and .round()-ed first, so the cast to u32 \
                          neither truncates a fraction nor loses a sign"
            )]
            let mhz = (1000.0 / d.as_secs_f64())
                .round()
                .clamp(0.0, f64::from(u32::MAX)) as u32;
            mhz
        })
}

/// Perform the navigation a claimed touchpad gesture resolved to — a second
/// way to ask for something the shell already does, not a parallel model.
fn apply_shell_gesture(
    frame_builder: &mut FrameBuilder,
    action: crate::compositor::backend::drm::gestures::GestureAction,
) {
    use crate::compositor::backend::drm::gestures::GestureAction;
    if let GestureAction::StepAlongStrip(dir) = action {
        // Goes through the host, not the chrome: the widget side owns the active tab, undone by the next writeback otherwise.
        frame_builder.host_mut().step_focus_along_strip(dir);
        return;
    }
    frame_builder.with_chrome(|st| match action {
        GestureAction::SwitchWorkspace(dir) => {
            st.switch_workspace(dir);
        }
        // Handled (and returned on) above; a panic here would take down the
        // whole session for a gesture that already did its job.
        GestureAction::StepAlongStrip(_) => {}
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

/// Translate the backend's output list into the geometry the shell's
/// strips resolve window widths against.
///
/// Outputs with a zero dimension are dropped: a connector mid-modeset
/// reports 0×0, which would hand a strip a display it can't place a window on.
fn output_geometry(
    outputs: &[crate::compositor::backend::Output],
) -> Vec<crate::shell::overlay_host::OutputGeometry> {
    outputs
        .iter()
        .filter(|o| o.size.0 > 0 && o.size.1 > 0)
        .map(|o| crate::shell::overlay_host::OutputGeometry {
            x: f64::from(o.position.0),
            width: f64::from(o.size.0),
            height: f64::from(o.size.1),
        })
        .collect()
}

#[cfg(test)]
mod vt_chord_tests {
    use super::vt_switch_chord;
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
    fn ctrl_alt_f_maps_to_the_matching_vt() {
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

    #[test]
    fn either_modifier_alone_is_not_the_chord() {
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
    }

    #[test]
    fn only_the_press_switches() {
        // Acting on the release too would switch twice per chord.
        let m = mods(ModifiersState::CONTROL | ModifiersState::ALT);
        assert_eq!(
            vt_switch_chord(&key(NamedKey::F2, ElementState::Released), m),
            None
        );
    }

    #[test]
    fn other_keys_with_the_modifiers_held_are_not_vt_switches() {
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

#[cfg(test)]
mod gesture_wireup_tests {
    use super::{apply_shell_gesture, refresh_mhz_of};
    use crate::compositor::backend::drm::gestures::GestureAction;
    use crate::compositor::backend::{Backend, BackendEvent, HeadlessBackend, Output, OutputId};
    use crate::compositor::frame::FrameBuilder;

    /// A shell gesture has to reach the *chrome*, not a parallel handler —
    /// drives `apply_shell_gesture` against a real `FrameBuilder`.
    fn builder() -> FrameBuilder {
        FrameBuilder::new((1600, 900))
    }

    #[test]
    fn a_swipe_up_switches_the_workspace_the_chrome_is_on() {
        let mut fb = builder();
        // A second workspace to switch to — a one-workspace tree has nowhere to
        // go, so the assertion would pass vacuously. The new one is active, appended last, so "up" is where we came from.
        fb.with_chrome(|st| {
            st.add_workspace("second");
        });
        let before = fb.chrome().tree().active;
        // What a three-finger swipe up resolves to — same value `gestures::swipe_action` produces for an upward delta.
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
        // The gesture fires once per swipe; a second swipe in the same direction must not bump the revision for nothing.
        let mut fb = builder();
        apply_shell_gesture(&mut fb, GestureAction::ShowOverview);
        let revision = fb.chrome().revision;
        apply_shell_gesture(&mut fb, GestureAction::ShowOverview);
        assert_eq!(fb.chrome().revision, revision);
    }

    #[test]
    fn a_backend_with_no_display_advertises_no_refresh_rate() {
        // The headless backend has no display, so `wl_output` falls back to the conventional rate rather than zero.
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
        // 59.94 Hz must not round to 60 000 — the whole reason DRM derives its interval from raw mode timing.
        let ntsc = Paced(std::time::Duration::from_nanos(16_683_333));
        assert_eq!(refresh_mhz_of(&ntsc as &dyn Backend), Some(59_940));
    }
}

#[cfg(test)]
mod output_geometry_tests {
    use super::output_geometry;
    use crate::compositor::backend::{Output, OutputId};

    /// A single monitor maps to one display at the origin — the case every screenshot and test runs in.
    #[test]
    fn single_output_maps_to_one_display() {
        let outs = [Output::new(OutputId(0), (3840, 2160), 1.0)];
        let geo = output_geometry(&outs);
        assert_eq!(geo.len(), 1);
        assert_eq!(
            (geo[0].x, geo[0].width, geo[0].height),
            (0.0, 3840.0, 2160.0)
        );
    }

    /// Two monitors side by side stay two displays, each with its own width —
    /// what stops a full-width window spanning both (combined geometry here is 3520px, neither display is that wide).
    #[test]
    fn side_by_side_outputs_stay_separate_displays() {
        let outs = [
            Output::new(OutputId(0), (1600, 900), 1.0),
            Output {
                id: OutputId(1),
                position: (1600, 0),
                size: (1920, 1080),
                scale: 1.0,
                transform: settings_store::hardware::OutputTransform::Normal,
                color_profile: None,
            },
        ];
        let geo = output_geometry(&outs);
        assert_eq!(geo.len(), 2);
        assert_eq!((geo[0].x, geo[0].width), (0.0, 1600.0));
        assert_eq!((geo[1].x, geo[1].width), (1600.0, 1920.0));
    }

    /// A connector mid-modeset reports 0x0; a strip can't place a window on it, so it must not appear as a display.
    #[test]
    fn zero_sized_outputs_are_dropped() {
        let outs = [
            Output::new(OutputId(0), (1600, 900), 1.0),
            Output::new(OutputId(1), (0, 0), 1.0),
        ];
        assert_eq!(output_geometry(&outs).len(), 1);
    }
}
