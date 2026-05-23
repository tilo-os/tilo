//! Hosted compositor mode — runs the shell inside the developer's existing
//! X11/Wayland session via `winit`. This is the legacy path that every
//! `cargo run -p quick-desktop` invocation has used; it was lifted verbatim
//! out of `main()` as part of Phase 1 of the real-Wayland-compositor plan so
//! the new `--mode native` dispatch has somewhere to land.
//!
//! Behaviour is unchanged from before the split.

use std::sync::Arc;

use crate::network::behaviour::{load_or_create_keypair, peer_salt_from_keypair};
use crate::shell::network::{DEFAULT_KEYPAIR_PATH, ShellNetwork};
use crate::shell::state::WorkspaceTree;
use crate::shell::{
    build_nirios_overlay_host_with_handle, persist, seed_demo_editor_presence,
    sync_tab_presence_from_tree,
};
use ui::collaboration::PresencePayload;
use ui::presence::PresenceKey;
use ui::widget::Widget;
use ui::{App, State};

/// Boot the shell on top of the host's existing display server with
/// the legacy v1 behaviour — peer-sync OFF, portals OFF. Kept as a
/// thin alias of [`run_with_options`] for callers that still hard-code
/// the no-arg entry point (notably integration tests that don't care
/// about the libp2p / portal side of the world).
pub fn run() {
    run_with_options(false);
}

/// Aggregated boot-time toggles for the hosted entry point. Held by
/// value so a future flag (e.g. `--enable-foo`) is one struct-field
/// edit + one branch, not a fan-out across `main.rs` and every test
/// fixture. v1 stays at two fields; the empty default is OFF for both.
#[derive(Clone, Copy, Debug, Default)]
pub struct HostedOptions {
    pub enable_peer_sync: bool,
    /// Sandbox Phase 4.3: spin up the `xdg-desktop-portal` D-Bus
    /// backend (FileChooser / ScreenCast / Access). Off by default —
    /// the portal lifetime is tied to the shell's tokio runtime, and
    /// callers without the flag get the exact pre-4.3 hosted-mode
    /// behaviour.
    pub enable_portals: bool,
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
    run_with_full_options(HostedOptions {
        enable_peer_sync,
        enable_portals: false,
    });
}

/// Full options-struct form. New flags (Sandbox 4.3's portal toggle)
/// land here without changing the legacy `run_with_options(bool)`
/// caller — `main.rs` switches over when it's wiring multiple flags.
pub fn run_with_full_options(opts: HostedOptions) {
    let HostedOptions {
        enable_peer_sync,
        enable_portals,
    } = opts;

    let path = persist::state_json_path();
    let mut tree = persist::load_or_default(&path);

    // C-1: under `--enable-peer-sync`, derive a deterministic 16-bit
    // peer salt from the libp2p keypair and stamp it into every id
    // minted by `next_id` for the rest of the process lifetime. Two
    // peers running in parallel mint ids with disjoint high bits, so
    // CRDT sync no longer folds two distinct workspaces into a single
    // row when their counters happen to overlap.
    //
    // The salt has to be set BEFORE `load_or_default`'s
    // `seed_next_id_from_self` runs — otherwise the counter is seeded
    // off ids from the *previous* (salt-0) run and the very next
    // `WorkspaceId::new()` call mints a salted id whose counter
    // overlaps an unsalted one already in the tree. The fix is to
    // re-seed after setting the salt; `seed_next_id_from_self` is
    // idempotent.
    //
    // No salt is set under the legacy (peer-sync-off) path so existing
    // tests + restart cases keep producing the familiar `0x0001,
    // 0x0002, …` id sequence. The salt-0 default is the explicit
    // back-compat contract.
    if enable_peer_sync {
        match load_or_create_keypair(std::path::Path::new(DEFAULT_KEYPAIR_PATH)) {
            Ok(kp) => {
                let salt = peer_salt_from_keypair(&kp);
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

    // C-3: one `WorkspaceTreeHandle` shared across compositor / portal /
    // peer-sync. The shell chrome, the portal's `GrantStoreHandle`, and
    // the peer-sync `WorkspaceTreeReplica` each hold a clone of this
    // handle — every mutation lands on the same underlying tree.
    let active_at_boot = tree.active.0;
    let tree_handle: crate::shell::state::WorkspaceTreeHandle = tree.shared();

    // Snapshot for the pre-app seeding helpers (`sync_tab_presence_from_tree`,
    // `seed_demo_editor_presence`) that only read the cache once at
    // startup. Held briefly under a read lock.
    let tree_snapshot: WorkspaceTree = tree_handle.read().clone();

    // C-3: peer-sync shares the same `WorkspaceTreeHandle` as chrome +
    // portal. `WorkspaceTreeReplica::apply_changes` writes into the
    // shared tree → the next paint reads the merged state without any
    // copy-back step.
    //
    // Held alive for the life of `run` so the actor's tokio runtime
    // doesn't drop out from under us mid-session. Failure is logged
    // and treated as "peer-sync stays off" — the shell must keep
    // working even if the keypair store is unreadable.
    let peer_sync: Option<Arc<ShellNetwork>> = if enable_peer_sync {
        match ShellNetwork::spawn(
            tree_handle.clone(),
            std::path::Path::new(DEFAULT_KEYPAIR_PATH),
        ) {
            Ok(handle) => {
                // Subscribe to the default workspace topic so the
                // actor's gossipsub mesh has somewhere to deliver
                // `NeighbourMsg::SyncReq`s as soon as a peer dials in.
                let _ = handle.join_default_workspace();
                tracing::info!("peer-sync: actor spawned, default workspace joined");
                Some(Arc::new(handle))
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
    let (host, _chrome) =
        build_nirios_overlay_host_with_handle(tree_handle.clone(), enable_peer_sync);
    let root: Box<dyn Widget> = Box::new(host) as Box<dyn Widget>;
    let mut app = App::new(state, root);
    // Seed per-tab presence from the tree's collaborator roster so
    // tab-pill presence dots render on first draw. Network-driven
    // presence updates later rewrite the same keys via the collab
    // pump, so this does not conflict with live peer attribution.
    sync_tab_presence_from_tree(&tree_snapshot, app.presence_ctx_mut());
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
        id: format!("{}", active_at_boot),
    }));

    // Phase 3.2 cursor-presence pump: when peer-sync is enabled,
    // pipe every local `broadcast_presence` Pointer event into a
    // `NetCmd::Publish { msg: PresenceMoved }` and drain received
    // presence updates into the chrome's remote-cursors overlay.
    // Sink + frame-tick are only installed under `enable_peer_sync`
    // so the legacy headless path stays fully bit-compatible.
    if let Some(net) = peer_sync.as_ref() {
        let send_net = Arc::clone(net);
        app.set_presence_broadcast_sink(move |key, payload| {
            // Only Pointer payloads carry an (x, y) coordinate; the
            // other variants (Caret, Selection, Arrived, Left) are
            // out of scope for cursor presence and ride the existing
            // unix-socket path the toolkit already invokes alongside.
            let PresencePayload::Pointer { x, y } = payload else {
                return;
            };
            // Encode the structured `PresenceKey` to JSON so the wire
            // format stays opaque to the shell-side bridge. Receivers
            // either round-trip back into PresenceKey (when they care
            // about scope) or use the string as an attribution-only
            // identifier.
            let key_str = serde_json::to_string(key).unwrap_or_default();
            send_net.publish_presence(key_str, (*x as f64, *y as f64));
        });
        let recv_net = Arc::clone(net);
        let inbound = recv_net.inbound_presence_handle();
        app.set_frame_tick(move |presence_ctx| {
            // Pump the actor→replica event channel first so any
            // `PresenceMoved` events that arrived since the last
            // wakeup land in the inbound buffer this frame.
            recv_net.tick();
            // Drain the buffer atomically so a slow frame doesn't
            // double-apply the same delta. Anything we drop here is
            // a stale move that's about to be replaced anyway, since
            // peers republish at ~30 Hz.
            let drained: Vec<_> = {
                let mut buf = inbound.lock().unwrap();
                std::mem::take(&mut *buf)
            };
            for entry in drained {
                presence_ctx.upsert_remote_cursor(
                    format!("p{:016x}", entry.peer),
                    [entry.point.0 as f32, entry.point.1 as f32],
                    None,
                );
            }
        });
    }
    // Sandbox Phase 4.3 — bring up the xdg-desktop-portal D-Bus
    // backend if `--enable-portals` was passed. The portal needs a
    // tokio runtime to drive zbus's async dispatch; reuse the
    // peer-sync runtime when present, otherwise mint a dedicated one
    // (kept alive next to `peer_sync` for the lifetime of `app.run`).
    //
    // The portal must NOT bypass the existing sandbox / grant
    // enforcement — `flow::resolve` is the only entry point that
    // mutates the workspace's `grants` map, and it consults the same
    // store the chrome's permission-prompt pump already uses (Phase
    // 4.2). Failures are downgraded to a warning so a broken session
    // bus never wedges the shell.
    let _portal_runtime: Option<tokio::runtime::Runtime>;
    let _portal_handle: Option<crate::portal::ServerHandle>;
    if enable_portals {
        let (rt_handle, runtime_owned) = match tokio::runtime::Handle::try_current() {
            Ok(h) => (h, None),
            Err(_) => {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .thread_name("nirios-portal")
                    .build()
                    .ok();
                match rt {
                    Some(rt) => (rt.handle().clone(), Some(rt)),
                    None => {
                        tracing::warn!("portal: tokio runtime build failed; portal stays off");
                        _portal_runtime = None;
                        _portal_handle = None;
                        app.run();
                        return;
                    }
                }
            }
        };
        // C-3: the portal's grant store shares the same
        // `WorkspaceTreeHandle` as the shell chrome + peer-sync
        // replica. Portal `Allow` outcomes flip a workspace grant on
        // the same tree the Permissions tab reads from, and
        // `doc_insert_grant` mirrors the change into the automerge doc
        // so peer-sync ships it on the next tick.
        let grants = crate::portal::GrantStoreHandle::new(tree_handle.clone());
        let prompts =
            crate::portal::PromptQueueHandle::new(crate::portal::PromptResolver::Channel);
        let ctx = crate::portal::PortalContext::new_with_null_spawners(
            grants,
            prompts,
            crate::portal::CallerMap::new(),
        );
        let serve_result: Result<crate::portal::ServerHandle, zbus::Error> =
            rt_handle.block_on(async move {
                let conn = zbus::ConnectionBuilder::session()?.build().await?;
                crate::portal::serve(conn, ctx).await
            });
        match serve_result {
            Ok(h) => {
                tracing::info!(
                    "portal: serving FileChooser+ScreenCast+Access on {}",
                    crate::portal::BUS_NAME
                );
                _portal_handle = Some(h);
            }
            Err(e) => {
                tracing::warn!("portal: serve failed ({e}); continuing without portal");
                _portal_handle = None;
            }
        }
        _portal_runtime = runtime_owned;
    } else {
        _portal_runtime = None;
        _portal_handle = None;
    }

    let _peer_sync = peer_sync; // keep alive for lifetime of `app.run()`

    // Hosted-mode test-control socket: when QOS_TEST_CONTROL is set,
    // bind it the same way the headless backend does and run the App
    // with a proxy that injects clicks/hovers into the event loop.
    // Same code path as hardware events; same gate; same redraw.
    #[cfg(feature = "hosted-test-control")]
    {
        if let Some(path) = std::env::var_os("QOS_TEST_CONTROL") {
            let socket_path = std::path::PathBuf::from(path);
            app.run_with_setup(move |proxy| {
                crate::compositor::hosted_test_control::spawn(socket_path, proxy);
            });
            return;
        }
    }
    app.run();
}
