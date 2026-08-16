//! Hosted compositor mode — runs the shell inside the developer's existing
//! X11/Wayland session via `winit`. This is the legacy path that every
//! `cargo run -p quick-desktop` invocation has used; it was lifted verbatim
//! out of `main()` as part of Phase 1 of the real-Wayland-compositor plan so
//! the new `--mode native` dispatch has somewhere to land.
//!
//! Behaviour is unchanged from before the split.

use std::sync::Arc;

use crate::network::behaviour::{load_or_create_keypair, peer_salt_from_keypair};
use crate::shell::network::{ShellNetwork, peer_key_path};
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
/// How long an invite minted from the palette stays valid. One hour —
/// long enough to hand a token to someone in the same room, short
/// enough that a token left in a chat log stops working.
const INVITE_EXPIRY_SECONDS: u64 = 3600;

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
    // The notice is carried all the way to the chrome rather than logged here:
    // "your workspaces are gone" and "your workspaces are on the disk and this
    // build declined to open them" look identical from the desk, and the journal
    // is not where the person is looking.
    let (mut tree, state_notice) = persist::load_or_default(&path);

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
    // Refused rather than downgraded when the device key will not load:
    // the salt comes from that key, and syncing an unsalted id space
    // lets two peers mint colliding ids, which CRDT sync then folds into
    // one row.
    let enable_peer_sync = enable_peer_sync
        && match load_or_create_keypair(&peer_key_path()) {
            Ok(kp) => {
                let salt = peer_salt_from_keypair(&kp);
                tree.set_peer_salt(salt);
                tree.seed_next_id_from_self();
                tracing::info!("peer-sync: id salt set to 0x{:04x}", salt);
                true
            }
            Err(e) => {
                tracing::error!(
                    "peer-sync: device key load failed ({e}); peer sync stays off \
                     rather than syncing an unsalted id space"
                );
                false
            }
        };

    // Permissions must outlive the process. Recording where to write
    // the tree is what turns an `Always` grant into a durable one:
    // every capability mutation flushes here, so a grant survives both
    // a clean exit and a crash.
    //
    // NOT when the file was refused. Declining to *read* a state file and
    // then writing a bootstrapped tree over it is worse than never having
    // checked: the check would announce that the person's workspaces are
    // safe on disk while destroying them. So a refused session runs without
    // persistence, the same way `ui::persist` switches an app's writer off
    // over a document it would not open.
    tree.persist_path = if state_notice.is_none() {
        Some(path.clone())
    } else {
        None
    };
    // The FUSE mount runs in another process and enforces the list it
    // reads from this path (`qos-fs --acl`). Exporting here is what
    // stops the filesystem from enforcing an empty list forever.
    tree.fs_acl_path = Some(persist::fs_capability_export_path());

    // Every workspace needs a durable identity as a collaboration room
    // before anything is launched into it. State files written before
    // rooms existed come back with none; minting here — after
    // `persist_path` is set, so the mint is written down in the same
    // breath — is what makes "the workspace I had yesterday" and "the
    // workspace I have today" the same room rather than two.
    let roomless = tree.adopt_roomless_workspaces();
    if !roomless.is_empty() {
        tracing::info!(
            "collaboration: minted a room identity for {} workspace(s)",
            roomless.len()
        );
    }

    // Someone has to own each room, or nobody can grant anything in it.
    // Workspaces restored from a state file written before ownership
    // existed carry no owner; the local user adopts them here. The
    // alternative — treating an unowned workspace as owned by whoever
    // asks — is the hole this replaces.
    let local_user = crate::shell::state::local_user_id();
    let adopted = tree.adopt_ownerless_workspaces(&local_user);
    if !adopted.is_empty() {
        tracing::info!(
            "capabilities: {} workspace(s) adopted by {local_user}",
            adopted.len()
        );
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
            &peer_key_path(),
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

    qos_startup_metrics::mark("hosted-run-state-build");
    let state = State::new();
    // The session keeps each open tab's document on disk — one record per
    // tab — so a note typed into one is still there next boot.
    let (host, _chrome) = crate::shell::build_nirios_overlay_host_with_records(
        tree_handle.clone(),
        enable_peer_sync,
        crate::shell::session_record_store(),
    );
    if let Some(notice) = &state_notice {
        crate::shell::notification::announce_state_notice(&_chrome, notice);
    }
    // Which records each room holds outlives the session too, and the
    // records apps have written are found by looking. Both are switched
    // on here, beside the rest of the session's durable state, so
    // building a compositor for a test or a screenshot reads and writes
    // nobody's.
    host.room_records().borrow_mut().attach_store();
    // Give the palette's `/invite` and `/connect` rows something that
    // actually performs them. Without this the host records an intent
    // and drops it, which is what both commands did before: reachable
    // in the palette, inert in a running session.
    let host = match peer_sync.as_ref() {
        Some(net) => {
            let net = Arc::clone(net);
            let tree_for_bridge = tree_handle.clone();
            host.with_peer_sync_bridge(std::rc::Rc::new(move |req| match req {
                crate::shell::overlay_host::PeerSyncRequest::IssueInvite => {
                    let active = tree_for_bridge.read().active;
                    net.issue_invite(active, INVITE_EXPIRY_SECONDS)
                }
                crate::shell::overlay_host::PeerSyncRequest::AcceptInvite(token) => {
                    let active = tree_for_bridge.read().active;
                    net.accept_invite(active, &token).map(|room| room.to_string())
                }
            }))
        }
        None => host,
    };
    let root: Box<dyn Widget> = Box::new(host) as Box<dyn Widget>;
    qos_startup_metrics::mark("hosted-app-new-start");
    let mut app = App::new(state, root);
    qos_startup_metrics::mark("hosted-app-new-done");
    // Seed per-tab presence from the tree's collaborator roster so
    // tab-pill presence dots render on first draw. Network-driven
    // presence updates later rewrite the same keys via the collab
    // pump, so this does not conflict with live peer attribution.
    sync_tab_presence_from_tree(&tree_snapshot, app.presence_ctx_mut());
    // Seed a demo peer caret + selection inside the editor body so the
    // collab chrome in `EditorApp::draw` is visible in single-process
    // development. Network traffic replaces this with real peer state
    // when it arrives.
    seed_demo_editor_presence(&tree_snapshot, app.presence_ctx_mut());
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
        // Inbound samples arrive at whatever rate the publisher manages
        // (~30 Hz) and never carry a "peer left" note, so they are fed
        // through the interpolator rather than straight onto the overlay:
        // it eases between consecutive samples and fades a peer's cursor
        // out once they go quiet. See `shell::shell::cursor_interp`.
        let mut cursors = crate::shell::cursor_interp::CursorInterpolator::new();
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
            let now = std::time::Instant::now();
            for entry in drained {
                cursors.push_sample(
                    &format!("p{:016x}", entry.peer),
                    "",
                    [entry.point.0 as f32, entry.point.1 as f32],
                    now,
                );
            }
            cursors.apply_to(presence_ctx, now);
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
        let prompts = crate::portal::PromptQueueHandle::new(crate::portal::PromptResolver::Channel);
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
                    "portal: serving FileChooser+ScreenCast+RemoteDesktop+Access on {}",
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

    let _peer_sync = peer_sync.clone(); // keep alive for lifetime of `app.run()`

    // Local CRDT fan-out broker: when QUICK_COLLAB_SOCK is set, bind
    // the path and detach a listener thread so every app launched
    // with the same env var connects through us and shares CRDT
    // updates live. Held for the lifetime of `app.run()` so the
    // socket file survives the session and reader threads stay
    // alive. See `shell::collab_broker` module docs for the protocol
    // surface and the test-only relay in `ui/tests/real_socket_two_peer.rs`
    // for the original spec.
    //
    // Phase 2: when peer-sync is also enabled, the broker bridges
    // through `ShellNetwork` so two laptops on the same gossipsub
    // topic share app-level CRDT increments via libp2p too.
    let _collab_broker: Option<crate::shell::collab_broker::BrokerHandle> =
        if let Some(path) = std::env::var_os("QUICK_COLLAB_SOCK") {
            let socket_path = std::path::PathBuf::from(path);
            // Optional native Cap'n Proto socket alongside the legacy
            // JSON one. When bound, apps using the `runtime-collab`
            // SDK connect directly to the shell without needing the
            // `collab-bridge` translator. Both transports share
            // fanout: an event delivered on one lands on subscribers
            // of the other.
            let capnp_socket_path =
                std::env::var_os("QUICK_COLLAB_CAPNP_SOCK").map(std::path::PathBuf::from);
            match crate::shell::collab_broker::spawn_full(
                socket_path.clone(),
                capnp_socket_path.clone(),
                peer_sync.clone(),
            ) {
                Ok(handle) => {
                    tracing::info!(
                        "collab_broker: bound {} (json) + {} (capnp) — peer_sync={}",
                        socket_path.display(),
                        capnp_socket_path
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "<none>".to_string()),
                        peer_sync.is_some()
                    );
                    Some(handle)
                }
                Err(e) => {
                    tracing::warn!(
                        "collab_broker: bind {} failed ({e}); apps will run without collab",
                        socket_path.display()
                    );
                    None
                }
            }
        } else {
            None
        };

    // Hosted-mode test-control socket: when QOS_TEST_CONTROL is set,
    // bind it the same way the headless backend does and run the App
    // with a proxy that injects clicks/hovers into the event loop.
    // Same code path as hardware events; same gate; same redraw.
    //
    // Gated on `dev_env::DEV_ENV_OVERRIDES` as well as the cargo feature,
    // for the same reason the native path is: this socket drives the shell
    // as if it were the user, so an installed binary must not be able to
    // open one however it was compiled.
    #[cfg(feature = "hosted-test-control")]
    {
        if let Some(path) = crate::compositor::native::test_control_path(
            crate::compositor::dev_env::DEV_ENV_OVERRIDES,
            std::env::var_os("QOS_TEST_CONTROL"),
        ) {
            let socket_path = path;
            qos_startup_metrics::mark("hosted-event-loop-enter");
            app.run_with_setup(move |proxy| {
                crate::compositor::hosted_test_control::spawn(socket_path, proxy);
            });
            return;
        }
    }
    qos_startup_metrics::mark("hosted-event-loop-enter");
    app.run();
}
