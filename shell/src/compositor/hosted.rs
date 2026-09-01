//! Hosted compositor mode — runs the shell inside the developer's existing X11/Wayland session
//! via `winit`. The legacy path every `cargo run -p quick-desktop` invocation uses; kept separate so `--mode native` has somewhere to land.

use std::sync::Arc;

use crate::network::behaviour::load_or_create_keypair;
use crate::shell::network::{ShellNetwork, peer_key_path};
use crate::shell::state::WorkspaceTree;
use crate::shell::{persist, sync_tab_presence_from_tree};
use ui::collaboration::PresencePayload;
use ui::presence::PresenceKey;
use ui::widget::Widget;
use ui::{App, State};

/// Boot the shell with legacy v1 behaviour — peer-sync OFF, portals OFF. Thin alias of
/// [`run_with_options`] for callers that hard-code the no-arg entry point (mainly integration tests).
pub fn run() {
    run_with_options(false);
}

/// Aggregated boot-time toggles for the hosted entry point, held by value so a new flag is one
/// struct-field edit, not a fan-out across `main.rs` and every test fixture. v1: two fields, default OFF.
#[derive(Clone, Copy, Debug, Default)]
pub struct HostedOptions {
    pub enable_peer_sync: bool,
    /// Spin up the `xdg-desktop-portal` D-Bus backend (`FileChooser` / `ScreenCast` / Access). Off by default; portal lifetime is tied to the shell's tokio runtime.
    pub enable_portals: bool,
}

/// How long an invite minted from the palette stays valid: one hour — long enough to hand a token to someone in the room, short enough that a leaked chat-log token stops working.
const INVITE_EXPIRY_SECONDS: u64 = 3600;

/// Boot the shell on top of the host's existing display server.
///
/// Loads (or defaults) the persisted workspace tree, seeds presence so collaboration chrome
/// paints on the first frame, and hands control to `App::run`. When `enable_peer_sync` is true,
/// also constructs a [`ShellNetwork`] handle on the same `WorkspaceTree`; `false` preserves existing behaviour bit-for-bit.
pub fn run_with_options(enable_peer_sync: bool) {
    run_with_full_options(HostedOptions {
        enable_peer_sync,
        enable_portals: false,
    });
}

/// Full options-struct form. New flags land here without changing the legacy `run_with_options(bool)` caller; `main.rs` switches over when wiring multiple flags.
pub fn run_with_full_options(opts: HostedOptions) {
    let HostedOptions {
        enable_peer_sync,
        enable_portals,
    } = opts;

    let path = persist::state_json_path();
    // Native and hosted sessions share this state file, so they must also
    // share the durable namespace used for every newly-minted object id.
    // Resolve it before loading: the default tree allocates ids while it is
    // constructed, and changing the salt afterward cannot repair those ids.
    let enable_peer_sync = prepare_workspace_namespace(enable_peer_sync, &path);
    // The notice is carried to the chrome rather than logged here: "your workspaces are gone" vs.
    // "this build declined to open them" look identical from the desk, and the journal isn't where the person is looking.
    let (mut tree, state_notice) = persist::load_or_default(&path);

    // Permissions must outlive the process: recording where to write the tree turns an `Always`
    // grant into a durable one, since every capability mutation flushes here. NOT set when the file
    // was refused — declining to *read* a state file and then writing a bootstrapped tree over it
    // would announce the workspaces are safe on disk while destroying them, so a refused session runs without persistence.
    tree.persist_path = if state_notice.is_none() {
        Some(path)
    } else {
        None
    };
    // The FUSE mount (another process) enforces the ACL list it reads from this path (`qos-fs --acl`); exporting here is what makes it enforce anything instead of an empty list forever.
    tree.fs_acl_path = Some(persist::fs_capability_export_path());

    adopt_tree_ownership(&mut tree);

    // One `WorkspaceTreeHandle` shared across compositor / portal / peer-sync — chrome, the portal's `GrantStoreHandle`, and the peer-sync replica each hold a clone, so every mutation lands on the same tree.
    let tree_handle: crate::shell::state::WorkspaceTreeHandle = tree.shared();

    // Snapshot for `sync_tab_presence_from_tree`, which only reads the cache once at startup; held briefly under a read lock.
    let tree_snapshot: WorkspaceTree = tree_handle.read().clone();

    // Peer-sync shares the same `WorkspaceTreeHandle` as chrome + portal:
    // `WorkspaceTreeReplica::apply_changes` writes into the shared tree, so the next paint reads the
    // merged state with no copy-back. Held alive for the life of `run` so the actor's tokio runtime
    // doesn't drop mid-session; a load failure is logged and treated as "peer-sync stays off".
    let peer_sync = spawn_peer_sync(enable_peer_sync, tree_handle.clone());

    qos_startup_metrics::mark("hosted-run-state-build");
    let state = State::new();
    // The session keeps each open tab's document on disk, one record per tab, so a note typed into one is still there next boot.
    let (host, chrome) = crate::shell::build_nirios_overlay_host_with_records(
        tree_handle.clone(),
        enable_peer_sync,
        crate::shell::session_record_store(),
    );
    if let Some(notice) = &state_notice {
        crate::shell::notification::announce_state_notice(&chrome, notice);
    }
    // Which records each room holds outlives the session too, and the records apps have written
    // are found by looking; both switched on here beside the rest of the session's durable state, so a compositor built for a test or screenshot reads and writes nobody's.
    host.room_records().borrow_mut().attach_store();
    // Give the palette's `/invite` and `/connect` rows something that actually performs them —
    // without this the host records an intent and drops it: reachable in the palette, inert in a running session.
    let host = attach_peer_sync_bridge(host, peer_sync.as_ref(), tree_handle.clone());

    // The portal's two queues, made here (before the host is boxed into the app, since attaching
    // them needs the concrete type) and unconditionally — two cheap handles, and an unfilled queue costs nothing to hold.
    let portal_prompts =
        crate::portal::PromptQueueHandle::new(crate::portal::PromptResolver::Channel);
    let portal_pickers = crate::portal::FilePickerQueue::new();
    let mut host = host;
    host.set_portal_queues(portal_prompts.clone(), portal_pickers.clone());
    let root: Box<dyn Widget> = Box::new(host) as Box<dyn Widget>;
    qos_startup_metrics::mark("hosted-app-new-start");
    let mut app = App::new(state, root);
    qos_startup_metrics::mark("hosted-app-new-done");
    // Seed per-tab presence from the tree's collaborator roster so tab-pill presence dots render
    // on first draw; network-driven updates later rewrite the same keys via the collab pump, so this doesn't conflict with live peer attribution.
    sync_tab_presence_from_tree(&tree_snapshot, app.presence_ctx_mut());
    // Declare the pointer-presence scope so local cursor moves broadcast to every connected peer
    // under the workspace's **room**, not the local `WorkspaceId` — the id is a per-machine handle
    // (two peers in one room hold different ones), so a pointer published under it would arrive
    // addressed to a scope the receiver has never heard of. The room is what the launcher, the overlay, and the native backend already key on.
    set_active_room_presence_scope(&mut app, &tree_snapshot);

    // Cursor-presence pump: when peer-sync is enabled, pipe every local `broadcast_presence`
    // Pointer event into `NetCmd::Publish { msg: PresenceMoved }` and drain received updates into
    // the chrome's remote-cursors overlay. Installed only under `enable_peer_sync` so the legacy headless path stays bit-compatible.
    if let Some(net) = peer_sync.as_ref() {
        install_presence_pump(&mut app, net, tree_handle.clone());
    }

    // Bring up the xdg-desktop-portal D-Bus backend if `--enable-portals` was passed; needs a tokio
    // runtime for zbus's async dispatch, reusing the peer-sync runtime when present or minting a dedicated one.
    //
    // The portal must NOT bypass the existing sandbox / grant enforcement — `flow::resolve` is the
    // only entry point that mutates the workspace's `grants` map, consulting the same store the
    // chrome's permission-prompt pump uses. Failures downgrade to a warning so a broken session bus never wedges the shell.
    let (_portal_runtime, _portal_handle) = match bring_up_portal(
        enable_portals,
        tree_handle.clone(),
        portal_prompts,
        portal_pickers,
    ) {
        PortalBringup::Disabled => (None, None),
        PortalBringup::RuntimeUnavailable => {
            app.run();
            return;
        }
        PortalBringup::Started { runtime, handle } => (runtime, handle),
    };

    let _peer_sync = peer_sync.clone(); // keep alive for lifetime of `app.run()`

    // Local typed collaboration broker, held for the lifetime of `app.run()`.
    // When peer sync is enabled it also bridges app changes between machines.
    let _collab_broker = spawn_collab_broker(peer_sync.as_ref(), tree_handle.clone());

    let identity_socket = launcher::services::identity_socket();
    let _identity_broker = if std::os::unix::net::UnixStream::connect(&identity_socket).is_ok() {
        tracing::warn!(path = %identity_socket.display(), "identity broker already running");
        None
    } else {
        launcher::services::ensure_services_root()
            .and_then(|_| crate::identity_broker::spawn(identity_socket, tree_handle.clone()))
            .map_err(|error| tracing::warn!(%error, "identity broker unavailable"))
            .ok()
    };

    // Hosted-mode test-control socket: when QOS_TEST_CONTROL is set, bind it like the headless
    // backend and run the App with a proxy injecting clicks/hovers into the event loop — same code
    // path, gate, and redraw as hardware events.
    //
    // Gated on `dev_env::DEV_ENV_OVERRIDES` as well as the cargo feature: this socket drives the shell as if it were the user, so an installed binary must not be able to open one however it was compiled.
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

/// Install the same durable object-id namespace used by native mode before
/// loading the shared state file. Transport identity remains independent: it
/// is created only when peer sync was explicitly requested.
fn prepare_workspace_namespace(enable_peer_sync: bool, state_path: &std::path::Path) -> bool {
    let key_path = peer_key_path();
    let salt = match super::native::load_or_create_workspace_salt(state_path, &key_path) {
        Ok(salt) => salt,
        Err(error) => {
            tracing::error!(
                %error,
                "workspace id salt unavailable; peer sync stays off rather than using an unstable id namespace"
            );
            return false;
        }
    };
    WorkspaceTree::set_process_peer_salt(salt);
    tracing::info!("workspace id salt set to 0x{:04x}", salt);

    if !enable_peer_sync {
        return false;
    }
    match load_or_create_keypair(&key_path) {
        Ok(keypair) => {
            tracing::info!(endpoint = %keypair.public(), "peer-sync: device identity loaded");
            true
        }
        Err(error) => {
            tracing::error!(
                "peer-sync: device key load failed ({error}); peer sync stays off \
                 rather than syncing without a stable transport identity"
            );
            false
        }
    }
}

/// Mints a room identity and an owner for any workspace restored from a state file written before
/// either concept existed — otherwise nobody can grant anything in it, or "yesterday's workspace"
/// and "today's" resolve to different rooms.
fn adopt_tree_ownership(tree: &mut WorkspaceTree) {
    let roomless = tree.adopt_roomless_workspaces();
    if !roomless.is_empty() {
        tracing::info!(
            "collaboration: minted a room identity for {} workspace(s)",
            roomless.len()
        );
    }

    // Someone has to own each room or nobody can grant anything in it.
    // Resolve per room so hosted mode uses the same selected identity that
    // its certified collaboration join presents.
    let adopted = tree.adopt_ownerless_workspaces_with(crate::shell::state::local_user_id_for_room);
    if !adopted.is_empty() {
        tracing::info!(
            "capabilities: {} workspace(s) adopted by their selected identities",
            adopted.len()
        );
    }
}

/// Spawns the peer-sync actor when enabled, joining the default workspace topic so the gossip mesh
/// has somewhere to deliver `NeighbourMsg::SyncReq`s as soon as a peer dials in. A spawn failure is
/// logged and treated as "peer-sync stays off" rather than aborting boot.
fn spawn_peer_sync(
    enable_peer_sync: bool,
    tree_handle: crate::shell::state::WorkspaceTreeHandle,
) -> Option<Arc<ShellNetwork>> {
    if !enable_peer_sync {
        return None;
    }
    match ShellNetwork::spawn(tree_handle, &peer_key_path()) {
        Ok(handle) => {
            let _ = handle.join_default_workspace();
            tracing::info!("peer-sync: actor spawned, default workspace joined");
            Some(Arc::new(handle))
        }
        Err(e) => {
            tracing::warn!("peer-sync: spawn failed ({e}); continuing without sync");
            None
        }
    }
}

/// Wires the palette's `/invite` and `/connect` rows to the live peer-sync actor; without this the
/// host records an intent and drops it — reachable in the palette, inert in a running session.
fn attach_peer_sync_bridge(
    host: crate::shell::overlay_host::ShellOverlayHost,
    peer_sync: Option<&Arc<ShellNetwork>>,
    tree_handle: crate::shell::state::WorkspaceTreeHandle,
) -> crate::shell::overlay_host::ShellOverlayHost {
    let Some(net) = peer_sync else {
        return host;
    };
    let net = Arc::clone(net);
    let tree_for_bridge = tree_handle;
    host.with_peer_sync_bridge(std::rc::Rc::new(move |req| match req {
        crate::shell::overlay_host::PeerSyncRequest::IssueInvite => {
            let active = tree_for_bridge.read().active;
            net.issue_invite(active, INVITE_EXPIRY_SECONDS)
        }
        crate::shell::overlay_host::PeerSyncRequest::IssueInviteFor { name, role } => {
            let active = tree_for_bridge.read().active;
            net.issue_invite_named_with_role(active, INVITE_EXPIRY_SECONDS, &name, role)
        }
        crate::shell::overlay_host::PeerSyncRequest::AcceptInvite(token) => {
            let active = tree_for_bridge.read().active;
            net.accept_invite(active, &token)
                .map(|room| room.to_string())
        }
    }))
}

/// Sets the pointer-presence broadcast scope to the active workspace's **room**, not the local
/// `WorkspaceId` — the id is a per-machine handle (two peers in one room hold different ones), so a
/// pointer published under it would arrive addressed to a scope the receiver has never heard of.
fn set_active_room_presence_scope(app: &mut App, tree_snapshot: &WorkspaceTree) {
    let active_room = tree_snapshot
        .workspaces
        .iter()
        .find(|w| w.id == tree_snapshot.active)
        .map(|w| w.room.to_string());
    if let Some(room) = active_room {
        app.set_presence_broadcast_scope(Some(PresenceKey::Workspace { id: room }));
    } else {
        tracing::warn!(
            "presence: the active workspace has no room; this session's cursor \
             will not reach its peers"
        );
    }
}

/// Installs the cursor-presence pump: pipes every local `broadcast_presence` Pointer event into
/// `NetCmd::Publish { msg: PresenceMoved }` and drains received updates into the chrome's
/// remote-cursors overlay.
fn install_presence_pump(
    app: &mut App,
    net: &Arc<ShellNetwork>,
    tree_handle: crate::shell::state::WorkspaceTreeHandle,
) {
    let send_net = Arc::clone(net);
    app.set_presence_broadcast_sink(move |key, payload| {
        // Only Pointer payloads carry an (x, y); the other variants (Caret, Selection, Arrived, Left) are out of scope here and ride the existing unix-socket path.
        let PresencePayload::Pointer { x, y } = payload else {
            return;
        };
        // Encode the structured `PresenceKey` to JSON so the wire format stays opaque to the
        // shell-side bridge; receivers either round-trip back into `PresenceKey` or use the string as an attribution-only id.
        let key_str = serde_json::to_string(key).unwrap_or_default();
        send_net.publish_presence(key_str, (f64::from(*x), f64::from(*y)));
    });
    let recv_net = Arc::clone(net);
    let inbound = recv_net.inbound_presence_handle();
    // Inbound samples arrive at whatever rate the publisher manages (~30 Hz) and never carry a
    // "peer left" note, so they're fed through the interpolator rather than straight onto the overlay: it eases between samples and fades a quiet peer's cursor out. See `shell::shell::cursor_interp`.
    let mut cursors = crate::shell::cursor_interp::CursorInterpolator::new();
    let tree_for_roster = tree_handle;
    app.set_frame_tick(move |presence_ctx| {
        // Pump the actor→replica event channel first so any `PresenceMoved` events since the last wakeup land in the inbound buffer this frame.
        recv_net.tick();
        // Drain the buffer atomically so a slow frame doesn't double-apply the same delta — anything dropped here is a stale move about to be replaced anyway, since peers republish at ~30 Hz.
        let drained: Vec<_> = {
            let mut buf = inbound
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            std::mem::take(&mut *buf)
        };
        let now = std::time::Instant::now();
        for entry in drained {
            // A frame addressed to a room is this session learning a named person is in it; putting them on
            // that room's roster is what the avatar stack, wheel-disc dots, and collaborators overlay render from.
            if let Ok(ui::presence::PresenceKey::Workspace { id }) =
                serde_json::from_str::<ui::presence::PresenceKey>(&entry.key)
                && let Some(room) = crate::shell::state::RoomId::parse(&id)
            {
                tree_for_roster
                    .write()
                    .note_peer_present(room, entry.peer, &entry.user);
            }
            #[allow(
                clippy::cast_possible_truncation,
                reason = "peer cursor position in screen-space pixels, always far below f32's exact-integer range"
            )]
            let point = [entry.point.0 as f32, entry.point.1 as f32];
            cursors.push_sample(
                &crate::shell::state::session_handle(entry.peer),
                &entry.user,
                point,
                now,
            );
        }
        cursors.apply_to(presence_ctx, now);
    });
}

/// Outcome of standing up the xdg-desktop-portal D-Bus backend.
enum PortalBringup {
    /// `--enable-portals` was not passed.
    Disabled,
    /// A tokio runtime could not be built; the caller runs the app immediately without the
    /// remaining hosted-mode setup, matching legacy behaviour when the portal has nowhere to run.
    RuntimeUnavailable,
    Started {
        runtime: Option<tokio::runtime::Runtime>,
        handle: Option<crate::portal::ServerHandle>,
    },
}

/// Brings up the xdg-desktop-portal D-Bus backend when enabled, reusing the current tokio runtime
/// when already inside one or minting a dedicated one. The portal must NOT bypass the existing
/// sandbox / grant enforcement — `flow::resolve` is the only entry point that mutates the
/// workspace's `grants` map, consulting the same store the chrome's permission-prompt pump uses.
fn bring_up_portal(
    enable_portals: bool,
    tree_handle: crate::shell::state::WorkspaceTreeHandle,
    portal_prompts: crate::portal::PromptQueueHandle,
    portal_pickers: crate::portal::FilePickerQueue,
) -> PortalBringup {
    if !enable_portals {
        return PortalBringup::Disabled;
    }
    let (rt_handle, runtime_owned) = if let Ok(h) = tokio::runtime::Handle::try_current() {
        (h, None)
    } else {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("nirios-portal")
            .build()
            .ok();
        let Some(rt) = rt else {
            tracing::warn!("portal: tokio runtime build failed; portal stays off");
            return PortalBringup::RuntimeUnavailable;
        };
        (rt.handle().clone(), Some(rt))
    };
    // The portal's grant store shares the same `WorkspaceTreeHandle` as chrome + peer-sync: an
    // `Allow` outcome flips a workspace grant on the same tree the Permissions tab reads, and `doc_insert_grant` mirrors it into the automerge doc so peer-sync ships it next tick.
    let grants = crate::portal::GrantStoreHandle::new(tree_handle);
    let ctx = crate::portal::PortalContext {
        grants,
        prompts: portal_prompts,
        caller_map: crate::portal::CallerMap::new(),
        // The real dialog, not the null spawner: hosted mode has a chrome to render it in, so there's no reason to cancel every request.
        file_picker: crate::portal::FilePickerSpawnerHandle::new(std::sync::Arc::new(
            portal_pickers,
        )),
        screencast_provisioner: crate::portal::ScreenCastStreamProvisionerHandle::new(
            std::sync::Arc::new(crate::portal::MockScreenCastProvisioner::new_with_node(
                1, "default",
            )),
        ),
        remote_input_injector: crate::portal::RemoteInputInjectorHandle::null(),
        documents: crate::portal::documents_for_session(),
        print_spooler: crate::portal::PrintSpoolerHandle::cups(),
    };
    let serve_result: Result<crate::portal::ServerHandle, zbus::Error> =
        rt_handle.block_on(async move {
            let conn = zbus::ConnectionBuilder::session()?.build().await?;
            crate::portal::serve(conn, ctx).await
        });
    let handle = match serve_result {
        Ok(h) => {
            tracing::info!(
                "portal: serving FileChooser+ScreenCast+RemoteDesktop+Access on {}",
                crate::portal::BUS_NAME
            );
            Some(h)
        }
        Err(e) => {
            tracing::warn!("portal: serve failed ({e}); continuing without portal");
            None
        }
    };
    PortalBringup::Started {
        runtime: runtime_owned,
        handle,
    }
}

/// Binds the local typed collaboration broker socket when `QUICK_COLLAB_CAPNP_SOCK` is set. When
/// peer sync is enabled it also bridges app changes between machines.
fn spawn_collab_broker(
    peer_sync: Option<&Arc<ShellNetwork>>,
    membership: crate::shell::state::WorkspaceTreeHandle,
) -> Option<crate::shell::collab_broker::BrokerHandle> {
    let path = std::env::var_os("QUICK_COLLAB_CAPNP_SOCK")?;
    let socket_path = std::path::PathBuf::from(path);
    let peer_sync_owned = peer_sync.cloned();
    match crate::shell::collab_broker::spawn_native(
        socket_path.clone(),
        &peer_sync_owned,
        membership,
    ) {
        Ok(handle) => {
            tracing::info!(
                "collab_broker: bound {} (capnp) — peer_sync={}",
                socket_path.display(),
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
}
