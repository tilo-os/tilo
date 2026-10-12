//! Hosted compositor mode — runs the shell inside the developer's existing X11/Wayland session
//! via `winit`, as a `ui::App` the toolkit paints. A dev opt-in (`--mode hosted`) for machines with no DRM/KMS and no spare
//! Wayland session; the default is `--mode native`.

use std::sync::Arc;

use crate::network::behaviour::load_or_create_keypair;
use crate::shell::network::{ShellNetwork, peer_key_path};
use crate::shell::state::WorkspaceTree;
use crate::shell::{persist, sync_tab_presence_from_tree};
use ui::presence::PresenceKey;
use ui::widget::Widget;
use ui::{App, State};

/// Boot the hosted test/development shell with optional services disabled.
/// Production entry points resolve their default-on sync policy before calling
/// [`run_with_options`].
pub fn run() {
    run_with_options(false);
}

/// Aggregated boot-time toggles for the hosted entry point, held by value so a new flag is one
/// struct-field edit, not a fan-out across `main.rs` and every test fixture.
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
#[allow(
    clippy::too_many_lines,
    reason = "hosted session startup deliberately keeps the ordered ownership, networking, portal, identity-broker, and event-loop lifetimes visible in one orchestration path"
)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "`peer_sync` and `host` are keep-alives for the event loop that `app.run()` drives"
)]
pub fn run_with_full_options(opts: HostedOptions) {
    let wake_scope = ui::wake::WakeScope::new();
    let _wake_entry = wake_scope.enter();
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
    // The FUSE mount (another process) enforces the ACL list it reads from this path (`tilo-fs --acl`); exporting here is what makes it enforce anything instead of an empty list forever.
    tree.fs_acl_path = Some(persist::fs_capability_export_path());

    adopt_tree_ownership(&mut tree);
    // Replay a persisted ACL after restart even when no capability changes in
    // this session; the asynchronous publisher retries until FS is ready.
    tree.export_fs_capabilities();

    // One `WorkspaceTreeHandle` shared across compositor / portal / peer-sync — chrome, the portal's `GrantStoreHandle`, and the peer-sync replica each hold a clone, so every mutation lands on the same tree.
    let tree_handle: crate::shell::state::WorkspaceTreeHandle = tree.shared();

    // Snapshot for `sync_tab_presence_from_tree`, which only reads the cache once at startup; held briefly under a read lock.
    let tree_snapshot: WorkspaceTree = tree_handle.read().clone();

    // Peer-sync shares the same `WorkspaceTreeHandle` as chrome + portal:
    // `WorkspaceTreeReplica::apply_changes` writes into the shared tree, so the next paint reads the
    // merged state with no copy-back. Held alive for the life of `run` so the actor's tokio runtime
    // doesn't drop mid-session; a load failure is logged and treated as "peer-sync stays off".
    let peer_sync = spawn_peer_sync(enable_peer_sync, tree_handle.clone());
    // Spawn before boxing the shell root so Follow can retain a cloneable
    // session-control dispatcher into the same broker local apps use.
    let collab_broker = spawn_collab_broker(peer_sync.as_ref(), tree_handle.clone());

    tilo_startup_metrics::mark("hosted-run-state-build");
    let state = State::new();
    // The session keeps each open tab's document on disk, one record per tab, so a note typed into one is still there next boot.
    let (host, chrome) = crate::shell::build_tilo_overlay_host_with_records(
        tree_handle.clone(),
        enable_peer_sync,
        crate::shell::session_record_store(),
    );
    let notification_bridge = crate::notifications::NotificationBridge::new();
    chrome
        .borrow_mut()
        .set_notification_bridge(notification_bridge.clone());
    if let Some(store) = crate::shell::notification_history::NotificationHistoryStore::for_user() {
        chrome.borrow_mut().attach_notification_history_store(store);
    }
    if let Some(notice) = &state_notice {
        crate::shell::notification::announce_state_notice(&chrome, notice);
    }
    // Which records each room holds outlives the session too, and the records apps have written
    // are found by looking; both switched on here beside the rest of the session's durable state, so a compositor built for a test or screenshot reads and writes nobody's.
    host.room_records().borrow_mut().attach_store();
    // Give the palette's `/invite` and `/connect` rows something that actually performs them —
    // without this the host records an intent and drops it: reachable in the palette, inert in a running session.
    let mut host = attach_peer_sync_bridge(host, peer_sync.as_ref(), tree_handle.clone());
    if let Some(broker) = collab_broker.as_ref() {
        let dispatcher = broker.follow_dispatcher();
        chrome
            .borrow_mut()
            .window_transfers
            .set_handoff_link(Box::new(crate::shell::window_transfer::BrokerHandoff::new(
                dispatcher.clone(),
            )));
        host.set_follow_bridge(std::rc::Rc::new(move |room, peer_id| {
            dispatcher.set(&room.to_string(), peer_id);
        }));
    }
    if let Some(network) = peer_sync.as_ref() {
        chrome
            .borrow_mut()
            .window_transfers
            .set_hosted_rooms(Box::new(crate::shell::document_import::NativeHostedRooms(
                Arc::clone(network),
            )));
        let follow_network = Arc::clone(network);
        let status_network = Arc::clone(network);
        host.set_follow_presence_bridge(std::rc::Rc::new(move |room, target| {
            let key = ui::presence::PresenceKey::Workspace {
                id: room.to_string(),
            };
            let payload = ui::collaboration::PresencePayload::Following { target };
            if let Some(frame) = crate::shell::collab_presence::to_peer(&key, &payload) {
                follow_network.publish_app_presence("tilo-shell".into(), room.to_string(), frame);
            }
        }));
        host.set_status_presence_bridge(std::rc::Rc::new(move |room, status| {
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
        let tab_network = Arc::clone(network);
        host.set_active_tab_presence_bridge(std::rc::Rc::new(move |room, tab, present| {
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
    }

    // The portal's two queues, made here (before the host is boxed into the app, since attaching
    // them needs the concrete type) and unconditionally — two cheap handles, and an unfilled queue costs nothing to hold.
    let portal_prompts =
        crate::portal::PromptQueueHandle::new(crate::portal::PromptResolver::Channel);
    let portal_pickers = crate::portal::FilePickerQueue::new();
    host.set_portal_queues(portal_prompts.clone(), portal_pickers.clone());
    // Bind before erasing the concrete host behind `dyn Widget`, so the
    // same live app-action provider exists in hosted and native sessions.
    if let Some(action_broker) = collab_broker
        .as_ref()
        .and_then(crate::shell::collab_broker::BrokerHandle::app_action_broker)
    {
        host.attach_app_action_broker(action_broker);
    }
    if let Some(review) = collab_broker
        .as_ref()
        .and_then(crate::shell::collab_broker::BrokerHandle::task_review)
    {
        host.attach_task_review(review);
    }
    // The presence pump runs in the frame tick, which is handed no host:
    // the host keeps its tiles and pointer current here instead.
    let desk = host.share_desk();
    let root: Box<dyn Widget> = Box::new(host) as Box<dyn Widget>;
    tilo_startup_metrics::mark("hosted-app-new-start");
    let mut app = App::from_scoped_parts(wake_scope, state, root);
    tilo_startup_metrics::mark("hosted-app-new-done");
    // Seed per-tab presence from the tree's collaborator roster so tab-pill presence dots render
    // on first draw; network-driven updates later rewrite the same keys via the collab pump, so this doesn't conflict with live peer attribution.
    sync_tab_presence_from_tree(&tree_snapshot, app.presence_ctx_mut());
    // Declare the toolkit's pointer-presence scope (what this window's own collaboration client
    // broadcasts under; the desktop cursor reaches peers through the presence pump below)
    // as the workspace's **room**, not the local `WorkspaceId` — the id is a per-machine handle
    // (two peers in one room hold different ones), so a pointer published under it would arrive
    // addressed to a scope the receiver has never heard of. The room is what the launcher, the overlay, and the native backend already key on.
    set_active_room_presence_scope(&mut app, &tree_snapshot);

    // Cursor-presence pump: when peer-sync is enabled, the native compositor's peer-presence
    // pipeline runs in the frame tick, so a hosted and a native session publish, accept and paint
    // peer pointers the same way. Installed only under `enable_peer_sync` so the legacy headless path stays bit-compatible.
    if let Some(net) = peer_sync.as_ref() {
        install_presence_pump(
            &mut app,
            net,
            collab_broker.as_ref(),
            tree_handle.clone(),
            desk,
        );
    }

    // Bring up the xdg-desktop-portal D-Bus backend if `--enable-portals` was passed; zbus's async
    // dispatch runs on the session runtime.
    //
    // The portal must NOT bypass the existing sandbox / grant enforcement — `flow::resolve` is the
    // only entry point that mutates the workspace's `grants` map, consulting the same store the
    // chrome's permission-prompt pump uses. Failures downgrade to a warning so a broken session bus never wedges the shell.
    let _portal_handle = match bring_up_portal(
        enable_portals,
        tree_handle.clone(),
        portal_prompts,
        portal_pickers,
        notification_bridge,
    ) {
        PortalBringup::Disabled => None,
        PortalBringup::RuntimeUnavailable => {
            app.run();
            return;
        }
        PortalBringup::Started { handle } => Some(handle),
    };

    let _peer_sync = peer_sync.clone(); // keep alive for lifetime of `app.run()`

    let identity_socket = launcher::services::identity_socket();
    let _identity_broker = if std::os::unix::net::UnixStream::connect(&identity_socket).is_ok() {
        tracing::warn!(path = %identity_socket.display(), "identity broker already running");
        None
    } else {
        launcher::services::ensure_services_root()
            .and_then(|_| {
                crate::identity_broker::spawn(
                    identity_socket,
                    tree_handle.clone(),
                    crate::shell::channel_encryption::ChannelKeyProvider::session(),
                )
            })
            .map_err(|error| tracing::warn!(%error, "identity broker unavailable"))
            .ok()
    };

    // Hosted-mode test-control socket: when TILO_TEST_CONTROL is set, bind it like the headless
    // backend and run the App with a proxy injecting clicks/hovers into the event loop — same code
    // path, gate, and redraw as hardware events.
    //
    // Gated on `dev_env::DEV_ENV_OVERRIDES` as well as the cargo feature: this socket drives the shell as if it were the user, so an installed binary must not be able to open one however it was compiled.
    #[cfg(feature = "hosted-test-control")]
    {
        if let Some(path) = crate::compositor::native::test_control_path(
            crate::compositor::dev_env::DEV_ENV_OVERRIDES,
            std::env::var_os(crate::shell::test_control_socket::TEST_CONTROL_ENV),
        ) {
            let socket_path = path;
            tilo_startup_metrics::mark("hosted-event-loop-enter");
            app.run_with_setup(move |proxy| {
                crate::compositor::hosted_test_control::spawn(socket_path, proxy);
            });
            return;
        }
    }
    tilo_startup_metrics::mark("hosted-event-loop-enter");
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

/// Adopts every ownerless workspace for its selected identity.
fn adopt_tree_ownership(tree: &mut WorkspaceTree) {
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
    match ShellNetwork::spawn_for_session(tree_handle) {
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
        crate::shell::overlay_host::PeerSyncRequest::PrepareInvitation => {
            Ok(net.prepare_invitation())
        }
        crate::shell::overlay_host::PeerSyncRequest::ExportIdentity => {
            let active = tree_for_bridge.read().desktop.active;
            net.person_identity_card(active)
                .map(crate::shell::overlay_host::PeerSyncResponse::Message)
        }
        crate::shell::overlay_host::PeerSyncRequest::IssueInviteFor {
            name,
            role,
            recipient_user_public_key,
        } => {
            let active = tree_for_bridge.read().desktop.active;
            net.issue_invite_bound_with_role(
                active,
                INVITE_EXPIRY_SECONDS,
                &name,
                recipient_user_public_key,
                role,
            )
            .map(crate::shell::overlay_host::PeerSyncResponse::Message)
        }
        crate::shell::overlay_host::PeerSyncRequest::IssueInvitation(terms) => {
            let active = tree_for_bridge.read().desktop.active;
            net.issue_invitation(active, &terms)
                .map(crate::shell::overlay_host::PeerSyncResponse::Message)
        }
        crate::shell::overlay_host::PeerSyncRequest::AcceptInvite(token) => {
            let active = tree_for_bridge.read().desktop.active;
            net.accept_peer_invitation(active, &token)
        }
        crate::shell::overlay_host::PeerSyncRequest::PollEnrollment(id) => net.poll_enrollment(id),
        crate::shell::overlay_host::PeerSyncRequest::CancelEnrollment(id) => {
            net.cancel_enrollment(id)
        }
        crate::shell::overlay_host::PeerSyncRequest::ListPendingInvitations => {
            let room = tree_for_bridge
                .read()
                .desktop
                .active_workspace()
                .map(|workspace| workspace.room)
                .ok_or_else(|| "no active workspace".to_string())?;
            Ok(
                crate::shell::overlay_host::PeerSyncResponse::PendingInvitations(
                    net.pending_invitations(room)
                        .into_iter()
                        .map(
                            |invite| crate::shell::overlay_host::PendingInvitationSummary {
                                id: invite.id,
                                name: invite.name,
                                role: invite.role,
                                expiry_unix_seconds: invite.expiry_unix_seconds,
                            },
                        )
                        .collect(),
                ),
            )
        }
        crate::shell::overlay_host::PeerSyncRequest::ResendInvitation(id) => net
            .resend_invitation(&id)
            .map(crate::shell::overlay_host::PeerSyncResponse::Message),
        crate::shell::overlay_host::PeerSyncRequest::CancelInvitation(id) => {
            if net.cancel_invitation(&id) {
                Ok(crate::shell::overlay_host::PeerSyncResponse::Message(
                    "canceled".into(),
                ))
            } else {
                Err("invitation is no longer pending".into())
            }
        }
    }))
}

/// Sets the pointer-presence broadcast scope to the active workspace's **room**, not the local
/// `WorkspaceId` — the id is a per-machine handle (two peers in one room hold different ones), so a
/// pointer published under it would arrive addressed to a scope the receiver has never heard of.
fn set_active_room_presence_scope(app: &mut App, tree_snapshot: &WorkspaceTree) {
    let active_room = tree_snapshot
        .desktop
        .workspaces
        .iter()
        .find(|w| w.id == tree_snapshot.desktop.active)
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

/// Installs the cursor-presence pump: the same [`PeerPresence`](crate::shell::peer_presence::PeerPresence)
/// the native compositor drives from its frame loop, driven from this window's frame tick.
///
/// So a hosted session publishes what a native one does — its pointer anchored to the tile under
/// it, throttled, with a keepalive — and accepts what a native one does: tile-anchored pointers,
/// placed over this window's own tile for each tab ([`SharedDesk`](crate::shell::peer_presence::SharedDesk)),
/// attributed to the authenticated session rather than any name a frame claims, and faded and
/// glided by the one presence model the overlay paints from. A pointer over a tab this window does
/// not show stays hidden, as on a native desk. The keepalive restates a still pointer only when the
/// event loop wakes (input, network, a frame); a hosted window asleep for longer than
/// [`ui::presence::POINTER_IDLE`] lets its peers' copy of a motionless pointer fade.
fn install_presence_pump(
    app: &mut App,
    net: &Arc<ShellNetwork>,
    broker: Option<&crate::shell::collab_broker::BrokerHandle>,
    tree_handle: crate::shell::state::WorkspaceTreeHandle,
    desk: crate::shell::peer_presence::SharedDesk,
) {
    use crate::shell::peer_presence::{PeerPresence, rooms_and_active};
    let (rooms, _) = rooms_and_active(&tree_handle.read());
    let relay = match broker {
        Some(broker) => PeerPresence::attach(broker, net, rooms),
        None => PeerPresence::for_network(net, rooms),
    };
    let net = Arc::clone(net);
    app.set_frame_tick(move |presence_ctx| {
        // Pump the actor→replica event channel first so any `PresenceMoved` events since the last
        // wakeup land in the inbound buffer this frame.
        net.tick();
        relay.frame(presence_ctx, &desk.snapshot(), &tree_handle)
    });
}

/// Outcome of standing up the xdg-desktop-portal D-Bus backend.
enum PortalBringup {
    /// `--enable-portals` was not passed.
    Disabled,
    /// The session runtime could not be started; the caller runs the app immediately without the
    /// remaining hosted-mode setup, matching legacy behaviour when the portal has nowhere to run.
    RuntimeUnavailable,
    Started {
        handle: crate::session_runtime::Registration<crate::portal::ServerHandle>,
    },
}

/// Brings up the xdg-desktop-portal D-Bus backend on the session runtime when enabled. The portal
/// must NOT bypass the existing
/// sandbox / grant enforcement — `flow::resolve` is the only entry point that mutates the
/// workspace's `grants` map, consulting the same store the chrome's permission-prompt pump uses.
fn bring_up_portal(
    enable_portals: bool,
    tree_handle: crate::shell::state::WorkspaceTreeHandle,
    portal_prompts: crate::portal::PromptQueueHandle,
    portal_pickers: crate::portal::FilePickerQueue,
    notifications: crate::notifications::NotificationBridge,
) -> PortalBringup {
    if !enable_portals {
        return PortalBringup::Disabled;
    }
    let runtime = match crate::session_runtime::runtime() {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::warn!("portal: session runtime unavailable ({error}); portal stays off");
            return PortalBringup::RuntimeUnavailable;
        }
    };
    // The portal's grant store shares the same `WorkspaceTreeHandle` as chrome + peer-sync: an
    // `Allow` outcome flips a workspace grant on the same tree the Permissions tab reads, and `doc_insert_grant` mirrors it into the automerge doc so peer-sync ships it next tick.
    let desktop_services = crate::portal::DesktopPortalServices::for_session(tree_handle.clone());
    let grants = crate::portal::GrantStoreHandle::new(tree_handle);
    let ctx = crate::portal::PortalContext {
        grants,
        prompts: portal_prompts,
        caller_map: crate::portal::CallerMap::new(),
        // Hosted mode has chrome to render the dialog, so keep requests
        // active instead of cancelling them.
        file_picker: crate::portal::FilePickerSpawnerHandle::new(std::sync::Arc::new(
            portal_pickers,
        )),
        screencast_provisioner: crate::portal::ScreenCastStreamProvisionerHandle::new(
            std::sync::Arc::new(crate::portal::UnavailableScreenCastProvisioner::new(
                "hosted compositor capture has not been connected to PipeWire",
            )),
        ),
        remote_input_injector: crate::portal::RemoteInputInjectorHandle::null(),
        input_capture: crate::portal::InputCaptureCompositorHandle::unavailable(),
        documents: crate::portal::documents_for_session(),
        print_spooler: crate::portal::PrintSpoolerHandle::cups(),
        notifications,
        desktop_services,
        authenticator: None,
    };
    // Registers in the background; the window opens without waiting on the bus.
    let handle = crate::session_runtime::Registration::start(
        runtime.handle(),
        "portal",
        async move {
            let conn = zbus::connection::Builder::session()?.build().await?;
            crate::portal::serve(conn, ctx).await
        },
        |_| {
            tracing::info!(
                "portal: serving FileChooser+ScreenCast+RemoteDesktop+Access on {}",
                crate::portal::BUS_NAME
            );
        },
    );
    PortalBringup::Started { handle }
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
            crate::shell::channel_encryption::ChannelKeyProvider::session()
                .set(handle.channel_encryption());
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
