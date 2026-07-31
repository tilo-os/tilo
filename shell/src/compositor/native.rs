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
use crate::compositor::foreign;
use crate::compositor::frame::FrameBuilder;
use crate::compositor::idle::{IdleConfig, SessionManager, SessionTransition};
use crate::compositor::lock_auth;
use crate::compositor::lock_controller::{LockController, LockKey, UnlockOutcome};
use crate::compositor::lock_screen::{self, LockOutput, LockView};
use crate::compositor::metrics;
use crate::compositor::pacing;
use crate::compositor::present;

use crate::compositor::state::{ClientState, State};
use crate::compositor::test_control::{
    self, ControlState, KeyTag, NamedKeyTag, PlannedAction, parse_command, serialise_response,
};
use crate::compositor::{BackendKind, parse_backend_from_args};
use crate::shell::overlay_host::PeerSyncRequest;

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
/// `compositor::test_control::Command` for the schema). Production
/// runs never set the env var; the socket is opt-in for test harnesses.
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
fn bind_test_control_socket() -> Option<(UnixListener, UnlinkOnDrop)> {
    let path = std::env::var_os(TEST_CONTROL_ENV)?;
    let path = PathBuf::from(path);
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
}

impl<'a> ControlState for FrameBuilderControlState<'a> {
    fn widget_registry(&self) -> HashMap<String, Rect> {
        self.fb.widget_registry()
    }

    /// Serialise the live persisted [`WorkspaceTree`] — same blob
    /// the `NIRIOS_TEST_STATE_DUMP` env-var path writes on every
    /// frame tick. The chrome's `tree` field is the authoritative
    /// copy (the compositor's `State.workspace_tree` is the same
    /// type and stays in sync via the shared `Chrome` cell).
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
                    serde_json::json!({
                        "surface_id": tab.surface_id,
                        "app_id": tab.app_id,
                        "frame_seq": store.frame_seq(tab.surface_id),
                        "last_error": store.last_error(tab.surface_id),
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
            // Which documents this session has open, and the window each
            // one landed in. A tab records the app it hosts, not the
            // file it is showing, so this is the only place the binding
            // between a window and its document is legible from outside.
            "open_documents": crate::shell::doc_open::open_documents()
                .into_iter()
                .map(|(record, package_id, surface)| serde_json::json!({
                    "record": record,
                    "package_id": package_id,
                    "surface_id": surface,
                }))
                .collect::<Vec<_>>(),
            "palette": palette_json(self.fb),
            "room_history": room_history_json(self.fb),
        })
    }
}

/// The room's own change history, and the sheet that renders it.
///
/// `changes` is read straight off the room document — `qdoc::history`
/// over the same Automerge change graph a peer reconstructs history
/// from, so an actor appearing here appears because it authored a change
/// in *this replica* of the document, not because the local pane
/// remembered writing one.
///
/// `rows` is what the sheet is showing, and exists for one reason a
/// script cannot work around: the room-activity sheet paints itself
/// outside the widget-registry walk, so `list_targets` sees none of it
/// and `revert` — the affordance a user clicks to restore a version —
/// has no addressable name. Publishing each row's revert-chip rect lets
/// a click land on it through the ordinary pointer path.
fn room_history_json(fb: &FrameBuilder) -> serde_json::Value {
    let chrome = fb.chrome();
    let changes: Vec<serde_json::Value> = {
        let mut tree = chrome.tree_mut();
        qdoc::history::versions(&mut tree.doc)
            .into_iter()
            .map(|v| {
                serde_json::json!({
                    "index": v.index,
                    "actor": v.actor_hex,
                    "hash": v.change_hash,
                    "time_ms": v.time_ms,
                })
            })
            .collect()
    };
    let pane = fb.host().room_activity();
    let timeline = pane.timeline();
    let rows: Vec<serde_json::Value> = (0..timeline.row_count())
        .map(|i| {
            let entry = timeline.row_entry(i);
            serde_json::json!({
                "index": i,
                "group_title": timeline.group_title_at_row(i),
                "summary": entry.map(|e| e.summary.clone()),
                "author": entry.map(|e| e.author.name.clone()),
                "revert_rect": timeline.revert_chip_rect(i).map(|r| {
                    [r.x0, r.y0, r.width(), r.height()]
                }),
            })
        })
        .collect();
    serde_json::json!({
        "changes": changes,
        "contributors": pane.contributors(),
        "rows": rows,
    })
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
        "rows": fb.host()
            .palette_row_details()
            .into_iter()
            .map(|(id, label, description, group)| serde_json::json!({
                "id": id,
                "label": label,
                // The snippet a content match came back with. Without it
                // "the search found this document" and "the search found
                // these words inside it" are the same observation.
                "description": description,
                "group": group,
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
) {
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
                return;
            }
            fb.dispatch_event(
                &event,
                &ctx.modifiers,
                &mut ctx.doc,
                &ctx.root_obj,
                &mut ctx.focus,
            );
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
                return;
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
                return;
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
                return;
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
        PlannedAction::Snapshot { path } => {
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
            write_snapshot_png(fb, path);
        }
    }
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
        // Pointer moves/clicks, modifier sets, and snapshots: swallowed while
        // locked. Input goes only to the lock surface.
        PlannedAction::CursorMoved { .. }
        | PlannedAction::MouseLeftButton { .. }
        | PlannedAction::MouseWheel { .. }
        | PlannedAction::SetModifiers { .. }
        | PlannedAction::Snapshot { .. } => {}
    }
}

/// Re-paint the current scene into a fresh `OffscreenRenderer` and
/// encode the resulting RGBA buffer as a PNG at `path`. Best-effort:
/// every failure path logs to stderr and returns, so an executor
/// invocation never panics or unwinds back into the calloop pump.
///
/// Split out of `execute_planned_action` so unit tests can exercise
/// the snapshot path against a hand-built `FrameBuilder` without going
/// through `PlannedAction` dispatch.
fn write_snapshot_png(fb: &mut FrameBuilder, path: &std::path::Path) {
    use std::io::Write;
    // The renderer panics if no wgpu adapter is available (e.g. CI
    // without llvmpipe); catch_unwind keeps the compositor alive in
    // that environment too.
    let (w, h) = fb.viewport();
    let (w, h) = (w as u32, h as u32);
    let scene = fb.paint();
    let render_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut renderer = ui::offscreen::OffscreenRenderer::new(w, h);
        renderer.render_rgba(&scene)
    }));
    let rgba = match render_result {
        Ok(rgba) => rgba,
        Err(_) => {
            eprintln!(
                "nirios compositor: test_control snapshot: \
                 OffscreenRenderer panicked (no wgpu adapter?)"
            );
            return;
        }
    };
    let file = match std::fs::File::create(path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("nirios compositor: test_control snapshot: create {path:?}: {e}");
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
            eprintln!("nirios compositor: test_control snapshot: png header {path:?}: {e}");
            return;
        }
    };
    if let Err(e) = png_writer.write_image_data(&rgba) {
        eprintln!("nirios compositor: test_control snapshot: png write {path:?}: {e}");
        return;
    }
    // Drop the PNG writer first so the underlying BufWriter holds the
    // last bytes; then flush explicitly. Without this, a swiftly-following
    // assertion on file size could race against a still-buffered tail.
    drop(png_writer);
    if let Err(e) = writer.flush() {
        eprintln!("nirios compositor: test_control snapshot: flush {path:?}: {e}");
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
                        let state = FrameBuilderControlState { fb: frame_builder };
                        test_control::dispatch_command(&cmd, &state)
                    }
                    Err(err_resp) => (err_resp, Vec::new()),
                };
                for action in &actions {
                    if session.is_locked() {
                        // While locked the test-control socket is captured by
                        // the lock surface, just like live input — actions go
                        // to the password field, never to the hidden chrome. A
                        // submit starts an async verify; the run loop's
                        // per-tick `poll_verification` delivers the unlock.
                        execute_locked_action(action, lock);
                    } else {
                        execute_planned_action(action, frame_builder, ctx, Some(state));
                    }
                }
                // Re-run layout so the next `list_targets` (or paint
                // tick) sees rects post-mutation. Chrome state
                // changes (overlay open, workspace add, …) can change
                // structural layout; without a relayout here the
                // next command sees stale rects from the previous
                // frame.
                if !actions.is_empty() {
                    injected_input = true;
                    frame_builder.relayout();
                }
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
fn load_session_tree(enable_peer_sync: bool) -> crate::shell::state::WorkspaceTreeHandle {
    use crate::shell::persist;

    let path = persist::state_json_path();
    let mut tree = persist::load_or_default(&path);

    // Two peers minting ids from the same counter produce colliding
    // ids, and CRDT sync then folds two distinct workspaces into one
    // row. The salt is derived from the libp2p keypair, so it has to be
    // set before the id counter is seeded off what is already in the
    // tree — hence the re-seed.
    if enable_peer_sync {
        match crate::network::behaviour::load_or_create_keypair(
            &crate::shell::network::peer_key_path(),
        ) {
            Ok(kp) => {
                let salt = crate::network::behaviour::peer_salt_from_keypair(&kp);
                tree.set_peer_salt(salt);
                tree.seed_next_id_from_self();
                tracing::info!("peer-sync: id salt set to 0x{:04x}", salt);
            }
            Err(e) => {
                tracing::warn!(
                    "peer-sync: keypair load failed ({e}); ids remain unsalted \
                     (cross-peer collisions possible)"
                );
            }
        }
    }

    tree.persist_path = Some(path);
    tree.fs_acl_path = Some(persist::fs_capability_export_path());

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
    tree.shared()
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
    // compositor `WorkspaceTree::bootstrap_default()`, so every boot was
    // a new desktop with unowned rooms and nothing was ever written back.
    let tree_handle = load_session_tree(enable_peer_sync);
    let mut frame_builder = FrameBuilder::with_handle_and_records(
        tree_handle.clone(),
        enable_peer_sync,
        initial_size,
        crate::shell::session_record_store(),
    );
    // The scene spans every output, but a window's width is a fraction of
    // the monitor it is on — so hand the strips the real per-display
    // geometry rather than letting them size against the bounding box.
    frame_builder.set_output_geometry(output_geometry(backend.outputs()));

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

    // Phase 7 test introspection: when `NIRIOS_TEST_STATE_DUMP` is set, the
    // compositor serializes its `WorkspaceTree` to that path on every frame
    // tick. The integration test in `tests/compositor_test.rs` reads this
    // file to assert foreign-tab adoption + reaping. Production runs never
    // set the env var, so the dump is a true zero-cost noop there.
    let test_dump_path = std::env::var_os(TEST_STATE_DUMP_ENV);

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
                    } else if is_lock_chord(&wev, &test_control_ctx.modifiers) {
                        // Super+L locks immediately.
                        for t in session.request_lock(Instant::now()) {
                            apply_session_transition(t, backend.as_mut(), present_pipe.as_ref());
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
                BackendEvent::Redraw | BackendEvent::Focus(_) => {
                    // Redraw is implicit because we re-paint every tick;
                    // Focus changes don't gate input dispatch here.
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
        // Drain the network actor into the replica. Peer changes land
        // in the shared tree, so a room's new name or a capability the
        // owner just revoked is folded in before this frame's pump
        // reads it — and the revision bump that follows is what makes
        // the frame repaint.
        if let Some(net) = peer_sync.as_ref() {
            net.tick();
        }
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
                frame_builder.paint()
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
        let park = frame_clock.park(Instant::now());
        let t_sleep_start = Instant::now();
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
        // Find the wheel disc rect by its opt-in `test_id`. We then
        // approximate the plus slot's position: it's a 28 px disc
        // centred on the visible footprint, 8 px below the visible
        // arc's bottom tip (see `sidebar_wheel_disc::draw`). Rather
        // than re-derive that arithmetic, click slightly below the
        // wheel's centre — close enough to land inside the plus rect
        // on the bootstrap layout.
        let registry = fb.widget_registry();
        let wheel_rect = registry
            .get("wheel_disc")
            .copied()
            .expect("wheel_disc test_id resolves on bootstrap tree");

        let initial_count = fb.chrome().tree().workspaces.len();

        // Walk down the wheel rect's vertical axis in 4 px increments
        // until a click lands on the plus slot (adds a workspace).
        // The plus slot is near the bottom of the wheel disc; this
        // sweep avoids hard-coding the exact y the wheel's `draw`
        // pass uses internally.
        let mut ctx = TestControlCtx::new();
        let cx = (wheel_rect.x0 + wheel_rect.x1) / 2.0;
        let mut found = false;
        let mut y = wheel_rect.y0 + 8.0;
        while y < wheel_rect.y1 {
            execute_planned_action(&PlannedAction::CursorMoved { x: cx, y }, &mut fb, &mut ctx);
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
                break;
            }
            y += 4.0;
        }
        assert!(
            found,
            "sweep of clicks down the wheel disc's vertical axis \
             should hit the plus slot and grow the workspace count \
             from {initial_count}"
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
    /// llvmpipe) because `OffscreenRenderer::new` panics; the executor
    /// itself catches that panic so the assertion would silently pass
    /// without producing a file — we explicitly check for the file's
    /// existence and skip the rest gracefully if the renderer was
    /// unavailable.
    #[test]
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

    /// Hostile path inputs — the executor must log the error and
    /// return without panicking. The dispatcher's `Ok` response was
    /// already on the wire; the compositor must not die because a
    /// client typo asked for `/nonexistent/dir/x.png`. Phase 5 will
    /// upgrade these logs into structured wire errors.
    #[test]
    fn execute_snapshot_with_bogus_dir_does_not_crash() {
        let mut fb = FrameBuilder::new((640, 360));
        let mut ctx = TestControlCtx::new();
        // Path under a nonexistent parent — File::create will fail with
        // ENOENT; the executor must log and return.
        execute_planned_action(
            &PlannedAction::Snapshot {
                path: std::path::PathBuf::from("/nonexistent/dir/x.png"),
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
        let state = FrameBuilderControlState { fb: &fb };
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
        let state = FrameBuilderControlState { fb: &fb };
        let tree = state.workspace_tree_json();
        let workspaces = tree
            .get("workspaces")
            .and_then(|v| v.as_array())
            .expect("workspace_tree_json should carry `workspaces` array");
        // The bootstrap tree seeds at least one demo workspace —
        // matches the assertions in `bootstrap_default` tests in
        // `shell/state.rs`.
        assert!(
            !workspaces.is_empty(),
            "bootstrap workspace tree should not be empty"
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
        let state = FrameBuilderControlState { fb: &fb };
        let (resp, actions) =
            test_control::dispatch_command(&test_control::Command::StateDump, &state);
        assert!(actions.is_empty());
        let test_control::Response::State {
            workspace_tree,
            chrome,
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
