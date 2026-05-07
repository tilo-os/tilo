//! Hosted compositor mode — runs the shell inside the developer's existing
//! X11/Wayland session via `winit`. This is the legacy path that every
//! `cargo run -p quick-desktop` invocation has used; it was lifted verbatim
//! out of `main()` as part of Phase 1 of the real-Wayland-compositor plan so
//! the new `--mode native` dispatch has somewhere to land.
//!
//! Behaviour is unchanged from before the split.

use std::sync::{Arc, Mutex};

use crate::shell::network::{DEFAULT_KEYPAIR_PATH, ShellNetwork};
use crate::shell::{
    build_nirios_root, persist, seed_demo_editor_presence, sync_tab_presence_from_tree,
};
use quick_ui::presence::PresenceKey;
use quick_ui::{App, State};

/// Boot the shell on top of the host's existing display server with
/// the legacy v1 behaviour — peer-sync OFF. Kept as a thin alias of
/// [`run_with_options`] for callers that still hard-code the no-arg
/// entry point (notably integration tests that don't care about the
/// libp2p side of the world).
pub fn run() {
    run_with_options(false)
}

/// Boot the shell on top of the host's existing display server.
///
/// Mirrors the previous `main()` body verbatim: load (or default) the
/// persisted workspace tree, seed presence so collaboration chrome paints on
/// the first frame, and hand control to `App::run`, which drives the winit
/// event loop until the user quits.
///
/// When `enable_peer_sync` is true, also constructs a
/// [`ShellNetwork`] handle on top of the same `WorkspaceTree`. Phase 3
/// Task 3.1 only owns the construction — wiring `tick()` into the
/// per-frame update loop and routing palette commands lives behind
/// Tasks 3.2 + 3.3. v1 default OFF — passing `false` (or omitting
/// `--enable-peer-sync` on the CLI) preserves the existing behaviour
/// bit-for-bit.
pub fn run_with_options(enable_peer_sync: bool) {
    let path = persist::state_json_path();
    let tree = persist::load_or_default(&path);

    // Optional peer-sync handle — construction only, per Task 3.1.
    // Held alive for the life of `run` so the actor's tokio runtime
    // doesn't drop out from under us mid-session. Failure is logged
    // and treated as "peer-sync stays off" — the shell must keep
    // working even if the keypair store is unreadable.
    let _peer_sync: Option<ShellNetwork> = if enable_peer_sync {
        let shared_tree = Arc::new(Mutex::new(tree.clone()));
        match ShellNetwork::spawn(shared_tree, std::path::Path::new(DEFAULT_KEYPAIR_PATH)) {
            Ok(handle) => {
                // Subscribe to the default workspace topic so the
                // actor's gossipsub mesh has somewhere to deliver
                // `NeighbourMsg::SyncReq`s as soon as a peer dials in.
                let _ = handle.join_default_workspace();
                tracing::info!("peer-sync: actor spawned, default workspace joined");
                Some(handle)
            }
            Err(e) => {
                tracing::warn!("peer-sync: spawn failed ({e}); continuing without sync");
                None
            }
        }
    } else {
        None
    };

    let state = State::new();
    let root = build_nirios_root(&tree);
    let mut app = App::new(state, root);
    // Seed per-tab presence from the tree's collaborator roster so
    // tab-pill presence dots render on first draw. Network-driven
    // presence updates later rewrite the same keys via the collab
    // pump, so this does not conflict with live peer attribution.
    sync_tab_presence_from_tree(&tree, app.presence_ctx_mut());
    // Seed a demo peer caret + selection inside the editor body so the
    // collab chrome in `EditorApp::draw` is visible in single-process
    // development. Network traffic replaces this with real peer state
    // when it arrives.
    seed_demo_editor_presence(app.presence_ctx_mut());
    // Declare the pointer-presence scope so local cursor moves broadcast
    // to every connected peer under the active workspace id. Remote
    // peers receive `CollabEvent::RemotePresenceUpdate` on the same
    // key and render this user's pointer on the correct workspace.
    app.set_presence_broadcast_scope(Some(PresenceKey::Workspace {
        id: format!("{}", tree.active.0),
    }));
    app.run();
}
