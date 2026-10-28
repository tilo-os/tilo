//! Everything a session does before its first frame that is not drawing:
//! the workspace id salt, loading the desktop state and the migrations it
//! needs, ownership adoption, and the collaboration broker apps dial.
//!
//! One implementation, called by [`super::native::run`]. A second session
//! entry point once kept its own copy of each step, and the copies drifted —
//! on which migrations ran, which socket the broker bound and whether a live
//! one was protected — so a fix made here is a fix for every session.

/// The desktop state a session starts from, with what loading it decided.
pub(crate) struct LoadedSession {
    /// The tree every subsystem shares, salted, migrated and adopted.
    pub(crate) tree: crate::shell::state::WorkspaceTreeHandle,
    /// Whether peer sync may come up: requested, and the device key loads.
    pub(crate) peer_sync_possible: bool,
    /// Why the stored state was refused, for the chrome to show; `None`
    /// when it loaded (or there was none yet).
    pub(crate) notice: Option<crate::shell::persist::StateNotice>,
}

/// Load this machine's desktop state and bring it up to the invariants
/// later stages assume: a persist path and a durable room identity per
/// workspace. Each room's owner is what its host's admitted document says.
///
/// Peer sync may come up only when it was asked for *and* the device key
/// loads, since the id salt comes from it and an unsalted id space lets peers
/// mint colliding ids.
pub(crate) fn load_session_tree(enable_peer_sync: bool) -> LoadedSession {
    use crate::shell::persist;

    let path = persist::state_json_path();

    // The tree gets its device's id salt **as** it is loaded — load-bearing.
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
    let salt = match crate::shell::session_policy::load_or_create_workspace_salt(
        &path,
        &peer_key_path,
    ) {
        Ok(salt) => {
            tracing::info!("workspace id salt set to 0x{:04x}", salt);
            salt
        }
        Err(error) => {
            peer_sync_possible = false;
            tracing::error!(
                %error,
                "workspace id salt unavailable; peer sync stays off rather than using an unstable id namespace"
            );
            0
        }
    };
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
    let (mut tree, state_notice) = persist::load_device_tree_or_default(&path, salt);
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
        tree.queue_save();
    }

    let claimed = tree.claim_own_unhosted_rooms(crate::shell::state::local_user_id_for_room);
    if !claimed.is_empty() {
        tracing::info!(
            "capabilities: {} workspace(s) of this device's own belong to their selected identities until their host holds them",
            claimed.len()
        );
    }

    // Replay the persisted aggregate on every boot. A previous session may
    // have stopped while the authority socket was unavailable; the
    // background publisher coalesces this with any immediate mutation.
    tree.export_fs_capabilities();
    LoadedSession {
        tree: tree.shared(),
        peer_sync_possible,
        notice: state_notice,
    }
}

/// Is something already listening on `path`? The session's service brokers
/// unlink before they bind, so without this check a second compositor on
/// the same `XDG_RUNTIME_DIR` would steal a socket from the running
/// session, splitting apps across two unreachable brokers. A successful
/// connect means a live listener; `ECONNREFUSED` means a stale inode we do want to clear.
pub(crate) fn service_socket_is_live(path: &std::path::Path) -> bool {
    path.exists() && std::os::unix::net::UnixStream::connect(path).is_ok()
}

/// The route the session's action door places offloadable actions through:
/// the person's home relay that retains the invoking room, over the
/// document media the filesystem daemon keeps for it, reached as the
/// desktop session through the invoking app's scope
/// ([`tilo_runtime::offload::session_action_offload`]). The session's rooms
/// now get their route at once; a room created or joined later is resolved
/// when its first offloadable action is invoked. `None` — every offloadable
/// action runs through its app's own handler — only when offload is off.
/// Never blocks on the network or the daemon.
pub(crate) fn session_action_offload(
    membership: &crate::shell::state::WorkspaceTreeHandle,
) -> Option<std::sync::Arc<dyn tilo_actions::offload::ActionOffload>> {
    let rooms: Vec<String> = membership
        .read()
        .desktop
        .workspaces
        .iter()
        .filter(|workspace| !workspace.room().is_nil())
        .map(|workspace| workspace.room().to_string())
        .collect();
    let rooms: Vec<&str> = rooms.iter().map(String::as_str).collect();
    tilo_runtime::offload::session_action_offload(&rooms, &launcher::services::blob_socket())
}

/// Why the session's broker did not come up, for the chrome to say.
#[derive(Debug)]
pub(crate) enum BrokerStartFailure {
    /// The services directory could not be created.
    ServicesRoot(String),
    /// Another session on this `XDG_RUNTIME_DIR` is listening on the socket.
    SocketTaken,
    /// The broker could not start: its socket, its invocation journal.
    Broker(String),
}

/// What binding the session's broker produced.
pub(crate) type BrokerStart = Result<crate::shell::collab_broker::BrokerHandle, BrokerStartFailure>;

/// Start binding the session's CRDT fan-out broker on the service socket
/// the launcher hands to apps, on the session-start worker.
///
/// Binding blocks by nature: it opens the broker's invocation journal (a
/// read of the whole journal and its directory and file syncs), its MLS
/// vault (the system secret store, which waits for the keyring) and every
/// channel's sealed state. On the compositor loop that froze the desktop
/// for as long as the disk or keyring took, at session start and on the
/// tick first login completes. The loop polls the returned start each tick
/// and installs the outcome with [`finish_session_collab_broker`]; until
/// then the session says its services are starting and holds launches.
pub(crate) fn start_session_collab_broker(
    peer_sync: Option<&std::sync::Arc<crate::shell::network::ShellNetwork>>,
    membership: crate::shell::state::WorkspaceTreeHandle,
    wake: crate::session_runtime::Wake,
) -> crate::session_runtime::PendingStart<BrokerStart> {
    let peer_sync = peer_sync.cloned();
    crate::session_runtime::start_on_worker(wake, move || {
        Ok(bind_session_collab_broker(peer_sync.as_ref(), membership))
    })
}

/// Bind the session's broker here, waiting for whatever it opens.
fn bind_session_collab_broker(
    peer_sync: Option<&std::sync::Arc<crate::shell::network::ShellNetwork>>,
    membership: crate::shell::state::WorkspaceTreeHandle,
) -> BrokerStart {
    if let Err(e) = launcher::services::ensure_services_root() {
        tracing::error!(
            "collab: cannot create {} ({e}); apps will start without collaboration",
            launcher::services::services_root().display()
        );
        return Err(BrokerStartFailure::ServicesRoot(e.to_string()));
    }

    let capnp_path = launcher::services::collab_socket();
    if service_socket_is_live(&capnp_path) {
        tracing::error!(
            "collab: {} already has a live listener — another session is using this \
             XDG_RUNTIME_DIR; refusing to take its socket. Apps will start without \
             collaboration.",
            capnp_path.display()
        );
        return Err(BrokerStartFailure::SocketTaken);
    }

    // Workspaces waiting for this device's own host are placed before any
    // app starts: placing one is local, so an app restored into it (Notes
    // in Home on a first boot, online or not) opens at once.
    if let Some(network) = peer_sync {
        network.place_before_launches();
    }
    let network = peer_sync.cloned();
    let action_offload = session_action_offload(&membership);
    let spawned = crate::shell::collab_broker::spawn_native_offloading(
        capnp_path.clone(),
        &network,
        membership,
        action_offload,
    );
    drop(network);
    match spawned {
        Ok(handle) => {
            tracing::info!(
                "collab: typed broker listening on {} — peer_sync={}",
                capnp_path.display(),
                peer_sync.is_some()
            );
            Ok(handle)
        }
        Err(e) => {
            tracing::error!(
                "collab: bind {} failed ({e}); apps will start without collaboration",
                capnp_path.display()
            );
            Err(BrokerStartFailure::Broker(e.to_string()))
        }
    }
}

/// Install a finished [`start_session_collab_broker`] on the loop: the
/// session's channel keys follow the broker's encryption service, and the
/// chrome says whether collaboration is available. `None` means a degraded
/// session, not a normal one — every app manifest declares `collab`, so
/// nothing collaborates until this succeeds; surfaced via
/// `chrome_state::set_collab_unavailable`. `outcome` is an error when the
/// start ended without one (its job panicked).
pub(crate) fn finish_session_collab_broker(
    chrome: &crate::shell::chrome_state::Chrome,
    outcome: Result<BrokerStart, String>,
) -> Option<crate::shell::collab_broker::BrokerHandle> {
    let failure = match outcome {
        Ok(Ok(handle)) => {
            crate::shell::channel_encryption::ChannelKeyProvider::session()
                .set(handle.channel_encryption());
            chrome.borrow_mut().clear_collab_unavailable();
            return Some(handle);
        }
        Ok(Err(failure)) => failure,
        Err(ended) => {
            tracing::error!(%ended, "collab: the broker's start ended without an outcome; apps will start without collaboration");
            BrokerStartFailure::Broker(ended)
        }
    };
    let notice = match failure {
        BrokerStartFailure::ServicesRoot(error) => crate::shell::i18n::t_args(
            "collab-unavailable-services-root",
            &[("error", error.into())],
        ),
        BrokerStartFailure::SocketTaken => crate::shell::i18n::t("collab-unavailable-socket-taken"),
        BrokerStartFailure::Broker(error) => {
            crate::shell::i18n::t_args("collab-unavailable-broker", &[("error", error.into())])
        }
    };
    chrome.borrow_mut().set_collab_unavailable(notice);
    None
}

/// Bind and install the session's broker on this thread, for tests that
/// need it before they go on.
#[cfg(test)]
pub(crate) fn spawn_session_collab_broker(
    chrome: &crate::shell::chrome_state::Chrome,
    peer_sync: Option<&std::sync::Arc<crate::shell::network::ShellNetwork>>,
) -> Option<crate::shell::collab_broker::BrokerHandle> {
    let membership = chrome.borrow().tree_handle();
    finish_session_collab_broker(
        chrome,
        Ok(bind_session_collab_broker(peer_sync, membership)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::frame::FrameBuilder;
    use crate::shell::state::{NEXT_ID, Strip, StripId, WorkspaceTree};
    use std::sync::atomic::Ordering;
    use tilo_desktop_model::{compose_id, counter_of, peer_salt_of};

    /// A private state directory for one boot: the state file, the id salt
    /// beside it, a device key that does not exist and an FS export path.
    fn isolated_session_env(
        dir: &std::path::Path,
        salt: u16,
    ) -> (tilo_test_env::ScopedEnv, std::path::PathBuf) {
        let state_path = dir.join("state.json");
        std::fs::write(dir.join("workspace-peer-salt"), salt.to_be_bytes()).expect("write salt");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_STATE_PATH", &state_path);
        env.set("TILO_PEER_KEY_PATH", dir.join("absent-peer-key"));
        env.set("TILO_FS_ACL_PATH", dir.join("fs-capabilities.json"));
        (env, state_path)
    }

    #[test]
    #[serial_test::serial(process_env, next_id_statics)]
    fn load_session_tree_salts_before_loading_and_seeds_ids_past_the_loaded_tree() {
        let dir = tempfile::tempdir().expect("tempdir");
        let salt: u16 = 0x0abc;
        let (_env, state_path) = isolated_session_env(dir.path(), salt);

        // A saved desktop holding an id far past this process's counter —
        // what a previous session leaves behind after minting for a while.
        let persisted_counter = NEXT_ID.load(Ordering::Relaxed).saturating_add(1_000_000);
        let persisted_id = compose_id(salt, persisted_counter);
        let mut saved = WorkspaceTree::bootstrap_default();
        saved.desktop.workspaces[0].strips.push(Strip {
            id: StripId::from(persisted_id),
            name: "Saved".into(),
            window_groups: Vec::new(),
        });
        crate::shell::persist::save_tree_to(&saved, &state_path).expect("save state");

        let loaded = load_session_tree(false);

        assert!(
            loaded.notice.is_none(),
            "a readable state file is not refused"
        );
        assert!(
            !loaded.peer_sync_possible,
            "peer sync that was not requested stays off"
        );
        let tree = loaded.tree.read();
        assert_eq!(
            tree.local_salt(),
            salt,
            "the session mints under the salt stored beside its state"
        );
        assert!(
            tree.desktop
                .workspaces
                .iter()
                .flat_map(|workspace| &workspace.strips)
                .any(|strip| strip.id.raw() == persisted_id),
            "the saved strip survives the load"
        );
        assert_eq!(tree.persist_path.as_deref(), Some(state_path.as_path()));
        assert!(tree.fs_acl_path.is_some(), "capabilities are exported");
        let minted = tree.mint_id();
        drop(tree);

        assert_eq!(peer_salt_of(minted), salt);
        assert!(
            counter_of(minted) > persisted_counter,
            "a fresh id ({minted:#x}) must not re-mint one already in the tree ({persisted_id:#x})"
        );
    }

    #[test]
    #[serial_test::serial(process_env, next_id_statics)]
    fn load_session_tree_never_persists_over_a_refused_state_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_env, state_path) = isolated_session_env(dir.path(), 0x0abd);
        std::fs::write(&state_path, b"not a desktop").expect("write damaged state");

        let loaded = load_session_tree(false);

        assert!(loaded.notice.is_some(), "a damaged state file is reported");
        assert_eq!(
            loaded.tree.read().persist_path,
            None,
            "a session that declined to read its state must not write over it"
        );
        assert_eq!(
            std::fs::read(&state_path).expect("state file is still there"),
            b"not a desktop"
        );
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
            let chrome = FrameBuilder::new((800, 600)).chrome_handle();
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
                tilo_app_package::ServiceReq {
                    protocol: launcher::services::COLLAB_SERVICE.to_string(),
                    requirement: tilo_app_package::Requirement::Required,
                    version: 1,
                },
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

            let first_chrome = FrameBuilder::new((800, 600)).chrome_handle();
            let first =
                spawn_session_collab_broker(&first_chrome, None).expect("first broker binds");

            let second_chrome = FrameBuilder::new((800, 600)).chrome_handle();
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
                service_socket_is_live(&launcher::services::collab_socket()),
                "the original broker must still be listening after the refusal"
            );

            drop(second);
            drop(first);
        }
    }

    /// The person's always-on relay at `address`, registered as retaining
    /// `room`, at `path`, as enrollment writes it.
    fn register_home_relay(
        path: &std::path::Path,
        user: [u8; 32],
        room: &str,
        address: iroh::EndpointAddr,
    ) {
        let relay_device = *address.id.as_bytes();
        let registration = tilo_relay::HomePeerRegistration {
            schema_version: tilo_relay::HomePeerRegistration::CURRENT_SCHEMA,
            device_node_id: hex::encode(relay_device),
            relay_service_node_id: hex::encode(relay_device),
            relay_endpoint: Some(address),
            label: "home".into(),
            user_public_key: hex::encode(user),
            device_cert: None,
            relay_workspace_ids: vec![tilo_relay_protocol::room_workspace_id(room).unwrap()],
            environments: tilo_relay_protocol::RelayEnvironments::default(),
        };
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_vec(&[registration]).unwrap()).unwrap();
    }

    /// The compositor's broker, as the desktop session starts it, holds the
    /// offload route the person's home relays configure — over the media
    /// the filesystem daemon keeps — even for a relay registered after the
    /// session started, and none when offload is off.
    #[test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "each broker holds the session's socket for its whole phase on purpose"
    )]
    fn native_consent_the_desktop_broker_holds_the_route_its_home_relay_configures() {
        const TEST: &str = "compositor::session_bootstrap::tests::native_consent_the_desktop_broker_holds_the_route_its_home_relay_configures";
        crate::shell::collab_broker_capnp::run_isolated_native_consent_test(TEST, || {
            let config =
                std::path::PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").expect("config"));
            let identity_root = std::path::PathBuf::from(
                std::env::var_os("TILO_IDENTITY_ROOT").expect("identity root"),
            );
            crate::firstboot::ensure_collaboration_identity(
                &config.join("tilo/identity"),
                &tilo_user_identity::IdentityPaths::under(identity_root.clone()),
            )
            .expect("provision certified identity");
            let chrome = FrameBuilder::new((800, 600)).chrome_handle();
            let room = chrome.borrow().tree_handle().read().desktop.workspaces[0]
                .room()
                .to_string();
            let route = |chrome: &crate::shell::chrome_state::Chrome| {
                let broker =
                    spawn_session_collab_broker(chrome, None).expect("the session's broker binds");
                let route = broker
                    .app_action_broker()
                    .expect("the native broker")
                    .state()
                    .action_offload()
                    .cloned();
                drop(broker);
                route
            };
            let request = tilo_actions::offload::OffloadRequest {
                room: room.clone(),
                app_id: "so.tilo.photos".into(),
                address: tilo_actions::ActionAddress::new("photos.library:main", "make-thumbnails"),
                offload: tilo_actions::Offload {
                    app: "so.tilo.thumbnails".into(),
                    entrypoint: "thumbnail-batch".into(),
                    gpu: false,
                    cpu_time_ms: 120_000,
                    memory_bytes: 1024 * 1024 * 1024,
                    output_bytes: 16 * 1024 * 1024,
                },
                inputs: vec![hex::encode([1; 32])],
            };
            let run_here = |route: &std::sync::Arc<dyn tilo_actions::offload::ActionOffload>| {
                let answer = tilo_rt::app()
                    .handle()
                    .block_on(route.offload(request.clone()));
                let tilo_actions::offload::Offloaded::RunHere { reason } = answer else {
                    panic!("nothing here can run elsewhere: {answer:?}");
                };
                reason
            };

            // No home relay yet: the broker holds the route, which runs every
            // offloadable action through its app's handler and says why.
            let unconfigured = route(&chrome).expect("the session routes its rooms");
            assert_eq!(
                run_here(&unconfigured),
                "no home relay of this device retains the room"
            );

            // The person's home relay is registered as retaining the room
            // while that session runs: its route reaches the relay from the
            // next invocation on. It never answers, so nothing can be placed
            // elsewhere, and the route says so: the door runs the app's
            // handler.
            let (_, cert) = tilo_identity_host::broker::trusted_current_device(&room)
                .expect("the session's certified device");
            let unreachable =
                iroh::EndpointAddr::new(iroh::SecretKey::from_bytes(&[5; 32]).public())
                    .with_ip_addr(std::net::SocketAddr::from(([127, 0, 0, 1], 9)));
            register_home_relay(
                &tilo_relay::home_peer_registry_path(&identity_root),
                cert.user_public_key,
                &room,
                unreachable,
            );
            let reason = run_here(&unconfigured);
            assert!(reason.contains("offload route"), "{reason}");
            drop(unconfigured);

            // A session started with it registered holds the route at once.
            let held = route(&chrome)
                .expect("the configured home relay routes the session's offloadable actions");
            let reason = run_here(&held);
            assert!(reason.contains("offload route"), "{reason}");
            drop(held);

            let mut env = tilo_test_env::ScopedEnv::new();
            env.set(tilo_runtime::offload::OFFLOAD_ENV, "off");
            assert!(route(&chrome).is_none(), "offload is off: no route");
        });
    }
}
