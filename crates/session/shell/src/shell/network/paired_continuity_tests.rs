//! Continuity crosses real, root-certified Iroh sessions before OS projection.

use super::*;
use crate::compositor::selection::{self, SharedSelection, Target};
use crate::shell::device_fabric_runtime::{DeviceFabricRuntime, LocalFabricCapabilities};
use crate::shell::personal_continuity::DeviceId;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

struct Session {
    network: Arc<ShellNetwork>,
    endpoint: iroh::Endpoint,
    actor: Option<tokio::task::JoinHandle<()>>,
    cert: DeviceCert,
    deferred: Mutex<VecDeque<NetEvent>>,
}

impl Session {
    fn network_mut(&mut self) -> &mut ShellNetwork {
        Arc::get_mut(&mut self.network).expect("configure a fixture before sharing its network")
    }

    async fn new(root: &ed25519_dalek::SigningKey, tag: u8, tree: WorkspaceTree) -> Self {
        let device = iroh::SecretKey::from_bytes(&[tag; 32]);
        let lookup = MemoryLookup::new();
        let endpoint = iroh::Endpoint::builder(presets::N0)
            .secret_key(device.clone())
            .address_lookup(lookup.clone())
            .bind()
            .await
            .unwrap();
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: *device.public().as_bytes(),
            device_label: format!("Fixture {tag}"),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, root).unwrap();
        let identity = || HostIdentity {
            device_cert: cert.clone(),
            device_signing_key: ed25519_dalek::SigningKey::from_bytes(&device.to_bytes()),
            workspace_device_certs: std::collections::HashMap::new(),
            workspace_revocations: std::collections::HashMap::new(),
            identity_paths: None,
        };
        let (commands, actor) = Actor::spawn_reporting_addrs(
            endpoint.clone(),
            lookup,
            Some(identity()),
            Arc::new(Mutex::new(Vec::new())),
        );
        let (mut network, _) = shell_network_for_test(
            tree.shared(),
            crate::network::behaviour::peer_liveness_handle(&device.public()),
        );
        network.cmd_tx = commands;
        network.keypair = device.clone();
        network.local_peer_id = device.public();
        network.require_peer_identity = true;
        network.personal_continuity_user = Some(cert.user_public_key);
        network.personal_continuity_device =
            Some(DeviceId::new(hex::encode(cert.device_public_key)).unwrap());
        network.host_identity = Some(identity());
        network.continuity_policy_override = Some(policy());
        Self {
            network: Arc::new(network),
            endpoint,
            actor: Some(actor),
            cert,
            deferred: Mutex::default(),
        }
    }

    async fn join(&self, topic: u64) {
        self.network.joined_topics.lock().unwrap().insert(topic);
        self.network
            .cmd_tx
            .send(NetCmd::JoinPersonalTopic {
                ws: topic,
                on_event: self.network.event_tx.clone(),
            })
            .await
            .unwrap();
    }

    async fn hello(&self, topic: u64) {
        self.network
            .cmd_tx
            .send(NetCmd::Publish {
                at: tokio::time::Instant::now(),
                ws: topic,
                msg: NeighbourMsg::IdentityHello {
                    protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
                    cert: Box::new(self.cert.clone()),
                    revocations: vec![],
                    successions: Vec::new(),
                },
            })
            .await
            .unwrap();
    }

    fn authenticated_on(&self, topic: u64, peer: iroh::EndpointId) -> bool {
        if topic == personal_continuity_topic_id(&self.cert.user_public_key) {
            return self
                .network
                .authenticated_personal_routes
                .lock()
                .unwrap()
                .contains_key(&(topic, peer));
        }
        self.network
            .workspace_control_direct
            .lock()
            .unwrap()
            .contains(&(topic, peer))
            && self
                .network
                .peer_identities
                .get(&crate::shell::state::PeerOrigin::peer(&peer))
                .is_some_and(|identity| identity.is_same_user(&self.cert.user_public_key))
    }

    fn admit_without_app_cache(&self) {
        // Root/transport admission runs normally. Hold actual checkpoint
        // events until the receiver's process-local report cache is cleared.
        loop {
            let event = self.network.event_rx.lock().unwrap().try_recv().ok();
            let Some(event) = event else { break };
            if matches!(
                &event,
                NetEvent::Msg {
                    msg: NeighbourMsg::PersonalContinuityCheckpoint { .. },
                    ..
                }
            ) {
                self.deferred.lock().unwrap().push_back(event);
            } else {
                self.network.dispatch_net_event(event);
            }
        }
    }

    async fn close(&mut self) {
        self.endpoint.close().await;
        if let Some(actor) = self.actor.take() {
            actor.abort();
            let _ = actor.await;
        }
    }
}

fn policy() -> crate::shell::continuity_policy::ContinuityPolicySnapshot {
    crate::shell::continuity_policy::ContinuityPolicySnapshot::from_document(
        serde_json::json!({"continuity": {
            "enabled": true, "clipboard": true, "app_continuations": true,
            "notifications": false, "wifi_settings": false, "media": false,
            "display_input": false, "file_transfer": false,
            "realtime_sessions": false, "typed_gamepad": false
        }})
        .as_object()
        .unwrap(),
    )
}

async fn pair(left: &Session, right: &Session) {
    let topic = personal_continuity_topic_id(&left.cert.user_public_key);
    pair_on_topic(left, right, topic).await;
}

async fn pair_on_topic(left: &Session, right: &Session, topic: u64) {
    left.join(topic).await;
    right.join(topic).await;
    left.network
        .cmd_tx
        .send(NetCmd::Connect {
            addr: right.endpoint.addr(),
        })
        .await
        .unwrap();
    tilo_test_env::within("two continuity sessions joined", async {
        loop {
            left.network.tick();
            right.admit_without_app_cache();
            if left
                .network
                .live_room_peers
                .lock()
                .unwrap()
                .contains(&(topic, right.endpoint.id()))
                && right
                    .network
                    .live_room_peers
                    .lock()
                    .unwrap()
                    .contains(&(topic, left.endpoint.id()))
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    left.hello(topic).await;
    right.hello(topic).await;
    tilo_test_env::within("root-certified continuity routes admitted", async {
        loop {
            left.network.tick();
            right.admit_without_app_cache();
            if left.authenticated_on(topic, right.endpoint.id())
                && right.authenticated_on(topic, left.endpoint.id())
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
}

#[path = "pump_load_tests.rs"]
mod pump_load_tests;

fn selection() -> SharedSelection {
    Arc::new(Mutex::new(selection::SelectionStore::default()))
}

fn pump(runtime: &mut DeviceFabricRuntime, session: &Session, selection: &SharedSelection) {
    session.network.tick();
    runtime.pump(
        &session.network,
        selection,
        LocalFabricCapabilities {
            display: None,
            pointer: false,
            keyboard: false,
            audio_output: false,
        },
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn paired_continuity_clipboard_requires_activation_and_filters_private_selections() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    home.env().set(
        "DBUS_SESSION_BUS_ADDRESS",
        "unix:path=/nonexistent/tilo-continuity-test",
    );
    let paths = settings_store::SessionPaths::beside(home.config().join("settings.json"));
    let _settings = crate::shell::test_support::install_settings(Arc::new(
        settings_store::SessionSettings::open(paths).unwrap(),
    ));
    settings_store::update_section(
        "continuity",
        serde_json::json!({
            "enabled": true, "clipboard": true, "app_continuations": true,
            "notifications": false, "wifi_settings": false, "media": false,
            "display_input": false, "file_transfer": false,
            "realtime_sessions": false, "typed_gamepad": false
        })
        .as_object()
        .unwrap()
        .clone(),
    )
    .unwrap();
    let root = ed25519_dalek::SigningKey::from_bytes(&[0x91; 32]);
    let mut left = Box::pin(Session::new(&root, 0x92, WorkspaceTree::default())).await;
    let mut right = Box::pin(Session::new(&root, 0x93, WorkspaceTree::default())).await;
    pair(&left, &right).await;
    let copied = selection();
    let pasted = selection();
    assert!(selection::set_mirror_text(
        &copied,
        Target::Clipboard,
        "portable copy"
    ));
    assert!(selection::set_mirror_text(
        &copied,
        Target::Primary,
        "private primary"
    ));
    // A password copied in in-process chrome, through the toolkit's secret copy.
    ui::host::scoped(&selection::host_services(copied.clone()), || {
        assert_eq!(
            ui::try_clipboard_copy_secret("private password", std::time::Duration::from_secs(3600))
                .outcome(),
            Some(true)
        );
    });
    assert_eq!(
        selection::mirror_text(&copied, Target::Clipboard).as_deref(),
        Some("private password"),
        "the secret is the live clipboard"
    );
    assert!(selection::set_mirror_text(
        &pasted,
        Target::Clipboard,
        "keep local clipboard"
    ));
    let mut outgoing = DeviceFabricRuntime::new(
        &left.network,
        crate::notifications::NotificationBridge::new(),
    );
    let mut incoming = DeviceFabricRuntime::new(
        &right.network,
        crate::notifications::NotificationBridge::new(),
    );
    let item = tilo_test_env::within("clipboard reaches certified second session", async {
        loop {
            pump(&mut outgoing, &left, &copied);
            pump(&mut incoming, &right, &pasted);
            if let Some(item) = selection::clipboard_history(&pasted)
                .into_iter()
                .find(|item| item.text_preview.as_deref() == Some("portable copy"))
            {
                break item;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    assert_eq!(item.source_device, left.network.personal_continuity_device);
    assert_eq!(
        selection::mirror_text(&pasted, Target::Clipboard).as_deref(),
        Some("keep local clipboard")
    );
    assert!(selection::activate_history_item(&pasted, item.id));
    assert_eq!(
        selection::mirror_text(&pasted, Target::Clipboard).as_deref(),
        Some("portable copy")
    );
    for private in ["private primary", "private password"] {
        assert!(
            !selection::clipboard_history(&pasted)
                .iter()
                .any(|item| item.text_preview.as_deref() == Some(private))
        );
        let checkpoint = left.network.local_device_fabric_checkpoint().unwrap();
        assert!(
            !checkpoint
                .windows(private.len())
                .any(|bytes| bytes == private.as_bytes())
        );
    }
    assert!(
        selection::shareable_clipboard_history(&pasted)
            .iter()
            .all(|shared| shared.id != item.id)
    );
    // An independently certified person can connect to the transport, but
    // cannot turn that connection into this person's clipboard authority.
    let other_root = ed25519_dalek::SigningKey::from_bytes(&[0x97; 32]);
    let mut stranger = Box::pin(Session::new(&other_root, 0x98, WorkspaceTree::default())).await;
    let topic = personal_continuity_topic_id(&left.cert.user_public_key);
    stranger.join(topic).await;
    stranger
        .network
        .cmd_tx
        .send(NetCmd::Connect {
            addr: right.endpoint.addr(),
        })
        .await
        .unwrap();
    tilo_test_env::within("stranger transport connection joined", async {
        loop {
            stranger.network.tick();
            right.network.tick();
            if right
                .network
                .live_room_peers
                .lock()
                .unwrap()
                .contains(&(topic, stranger.endpoint.id()))
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    stranger.hello(topic).await;
    tilo_test_env::within("stranger certificate observed on actual transport", async {
        loop {
            right.network.tick();
            if right
                .network
                .peer_identities
                .get(&crate::shell::state::PeerOrigin::peer(
                    &stranger.endpoint.id(),
                ))
                .is_some()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        !right
            .network
            .authenticated_personal_routes
            .lock()
            .unwrap()
            .contains_key(&(topic, stranger.endpoint.id()))
    );
    let private_copy = selection();
    assert!(selection::set_mirror_text(
        &private_copy,
        Target::Clipboard,
        "other person's copy"
    ));
    let mut stranger_runtime = DeviceFabricRuntime::new(
        &stranger.network,
        crate::notifications::NotificationBridge::new(),
    );
    pump(&mut stranger_runtime, &stranger, &private_copy);
    stranger
        .network
        .cmd_tx
        .send(NetCmd::PublishPersonalFabricTarget {
            ws: topic,
            user_public_key: left.cert.user_public_key,
            target: right.endpoint.id(),
            payload: PersonalFabricPayload::Snapshot(
                stranger.network.local_device_fabric_checkpoint().unwrap(),
            ),
        })
        .await
        .unwrap();
    let untrusted = tilo_test_env::within("untrusted clipboard frame actually arrived", async {
        loop {
            let event = right.network.event_rx.lock().unwrap().try_recv().ok();
            if let Some(event) = event {
                if matches!(&event, NetEvent::PersonalFabric { peer, .. } if *peer == stranger.endpoint.id()) { break event; }
                right.network.dispatch_net_event(event);
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await;
    right.network.dispatch_net_event(untrusted);
    pump(&mut incoming, &right, &pasted);
    assert!(
        !right.network.device_fabric().snapshots().contains_key(
            stranger
                .network
                .personal_continuity_device
                .as_ref()
                .unwrap()
        )
    );
    assert!(
        !selection::clipboard_history(&pasted)
            .iter()
            .any(|item| item.text_preview.as_deref() == Some("other person's copy"))
    );
    stranger.close().await;
    drop(stranger);
    left.close().await;
    drop(left);
    right.close().await;
    drop(right);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn paired_continuity_catchup_preserves_browser_checkpoint_and_launcher_identity() {
    use crate::shell::personal_continuity::{AppInstanceId, LaunchIntent};
    use crate::shell::state::{Tab, TileContent, WindowGroup, WindowWidth};
    use ui::widget::Widget as _;

    let mut home = tilo_test_env::TestHome::new().unwrap();
    let packages = home.data().join("apps");
    let payload = packages.join("payloads/so.tilo.browser");
    std::fs::create_dir_all(&payload).unwrap();
    let manifest = payload.join("manifest.toml");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/browser/tilo-app.toml"),
        &manifest,
    )
    .unwrap();
    std::fs::write(
        packages.join("registry.toml"),
        format!(
            "[[entry]]\nid = 'so.tilo.browser'\npayload_root = {:?}\nmanifest_path = {:?}\n",
            payload.to_str().unwrap(),
            manifest.to_str().unwrap(),
        ),
    )
    .unwrap();
    home.env().set("TILO_APPS_DATA_DIR", &packages);
    let root = ed25519_dalek::SigningKey::from_bytes(&[0x94; 32]);
    let room = RoomId::new();
    let tab = 0xC011_0094;
    let mut tree = WorkspaceTree::bootstrap_default();
    tree.desktop.workspaces[0] = crate::shell::state::rehomed(&tree.desktop.workspaces[0], room);
    let workspace = &mut tree.desktop.workspaces[0];
    workspace.strips[0].window_groups.push(WindowGroup {
        id: crate::shell::state::next_id(),
        tabs: vec![Tab {
            id: tab,
            title: "Browser".into(),
            tile: TileContent::Foreign {
                app_id: "so.tilo.browser".into(),
                surface_id: 94,
            },
            app_title: None,
        }],
        active_tab: tab,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    });
    let source_group = workspace.strips[0].window_groups[0].id;
    tree.desktop
        .active_group
        .insert(workspace.strips[0].id, source_group);
    let mut local_tree = WorkspaceTree::bootstrap_default();
    let local_workspace = &mut local_tree.desktop.workspaces[0];
    let local_workspace_id = local_workspace.id;
    let local_strip = local_workspace.strips[0].id;
    let local_group = crate::shell::state::next_id();
    let local_tab = 0xC011_0096;
    local_workspace.strips[0].window_groups.push(WindowGroup {
        id: local_group,
        tabs: vec![Tab {
            id: local_tab,
            title: "Terminal".into(),
            tile: TileContent::LocalApp {
                app_id: "so.tilo.terminal".into(),
                label: "Terminal".into(),
                note: String::new(),
            },
            app_title: None,
        }],
        active_tab: local_tab,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    });
    local_tree
        .desktop
        .active_group
        .insert(local_strip, local_group);
    let metadata = ui::app::compose_structural_continuation_metadata(
        ui::view::View::default(),
        Some(ui::collaboration::ContinuationMetadata {
            schema_version: ui::collaboration::semantic_schema::WEB_LOCATION_V1,
            bytes: br#"{"url":"https://example.test/reading"}"#.to_vec(),
        }),
    )
    .unwrap();
    let mut left = Box::pin(Session::new(&root, 0x95, tree)).await;
    let mut right = Box::pin(Session::new(&root, 0x96, local_tree)).await;
    let peers_path = home.config().join("home-peers.json");
    home.env().set("TILO_HOME_PEERS_PATH", &peers_path);
    std::fs::write(
        peers_path.with_file_name("personal-device-peers.json"),
        serde_json::to_vec(&[
            hex::encode(left.cert.device_public_key),
            hex::encode(right.cert.device_public_key),
        ])
        .unwrap(),
    )
    .unwrap();
    // Enrolled mid-session: take effect now rather than once the worker reads it.
    left.network.recheck_personal_device_peers();
    right.network.recheck_personal_device_peers();
    let registry = crate::shell::personal_continuity_registry::global_registry();
    {
        let mut reports = registry.lock().unwrap();
        reports.bind_tab(AppInstanceId(tab), AppInstanceId(tab));
        assert!(
            reports
                .report(
                    room,
                    "so.tilo.browser",
                    AppInstanceId(tab),
                    metadata.schema_version,
                    metadata.bytes.clone()
                )
                .unwrap()
        );
        drop(reports);
    }
    let prepared = left.network.local_continuity_snapshot_bytes().unwrap();
    let prepared = crate::shell::personal_continuity::decode_checkpoint(&prepared).unwrap();
    let source = prepared
        .snapshot_for(left.network.personal_continuity_device.as_ref().unwrap())
        .unwrap();
    let app = source
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.apps)
        .find(|app| app.id == AppInstanceId(tab))
        .unwrap();
    assert_eq!(
        app.resume.as_ref().unwrap().bytes(),
        metadata.bytes,
        "source checkpoint carries semantic metadata"
    );
    pair(&left, &right).await;
    // The response is captured on the sending shell, crosses Iroh, then
    // enters a receiving shell with no process-local app report cache.
    right.network.publish_personal_continuity_request();
    let arrived = tilo_test_env::within("session requests its app checkpoint", async {
        loop {
            left.network.tick();
            let event = right
                .deferred
                .lock()
                .unwrap()
                .pop_front()
                .or_else(|| right.network.event_rx.lock().unwrap().try_recv().ok());
            if let Some(event) = event {
                if let NetEvent::Msg {
                    msg: NeighbourMsg::PersonalContinuityCheckpoint { snapshot, .. },
                    ..
                } = &event
                {
                    let decoded =
                        crate::shell::personal_continuity::decode_checkpoint(snapshot).unwrap();
                    if decoded
                        .snapshot_for(left.network.personal_continuity_device.as_ref().unwrap())
                        .is_some_and(|source| {
                            source.workspaces.iter().any(|workspace| {
                                workspace.apps.iter().any(|app| {
                                    app.id == AppInstanceId(tab)
                                        && app
                                            .resume
                                            .as_ref()
                                            .is_some_and(|resume| resume.bytes() == metadata.bytes)
                                })
                            })
                        })
                    {
                        break event;
                    }
                }
                right.network.dispatch_net_event(event);
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    registry
        .lock()
        .unwrap()
        .forget_local_tab(AppInstanceId(tab));
    // The person's list brings the room to the right device; continuity
    // carries only what runs in it.
    right
        .network
        .tree
        .write()
        .join_room("Shared", room)
        .expect("the room's workspace");
    right.network.dispatch_net_event(arrived);
    assert!(right.network.tree.read().continuation_tabs.contains(&tab));
    let tree_handle = right.network.tree.clone();
    assert_eq!(
        tree_handle.read().desktop.active,
        local_workspace_id,
        "arrival preserves local focus"
    );
    assert_eq!(
        tree_handle.read().desktop.active_group[&local_strip],
        local_group
    );
    let chrome = crate::shell::chrome_state::ChromeState::new_with_handle(tree_handle.clone());
    chrome
        .borrow_mut()
        .set_continuity_feed(right.network.continuity_feed());
    assert_eq!(
        chrome
            .borrow()
            .continuity_suggestion(std::time::Instant::now())
            .unwrap()
            .tab,
        tab
    );
    let mut bar = crate::shell::topbar_chrome::TopBarChrome::from_tree(&tree_handle.read().desktop)
        .with_chrome(crate::shell::topbar_chrome::native_source(chrome.clone()));
    let mut document = automerge::AutoCommit::new();
    assert!(bar.perform_semantic_action(
        "topbar:continuity",
        &ui::a11y::Action::Activate,
        &mut ui::widget::Edits::lend(&mut document),
        &automerge::ROOT
    ));
    {
        let accepted = tree_handle.read();
        let active_workspace = accepted.desktop.active_workspace().unwrap();
        let active_strip = accepted.desktop.active_strip[&active_workspace.id];
        let active_group = accepted.desktop.active_group[&active_strip];
        assert!(
            active_workspace
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .any(|group| group.id == active_group && group.active_tab == tab)
        );
        drop(accepted);
    }
    let offered = registry
        .lock()
        .unwrap()
        .continuation_for_tab(
            room,
            AppInstanceId(tab),
            &LaunchIntent::app("so.tilo.browser").unwrap(),
        )
        .unwrap();
    assert_eq!(offered.resume.bytes(), metadata.bytes);
    assert_eq!(offered.record_instance, AppInstanceId(tab));
    crate::shell::launcher::clear_pending_placements();
    crate::shell::launcher::set_pending_placement(
        "so.tilo.browser",
        crate::shell::tab_binding::Placement::IntoTab(tab),
    );
    crate::shell::launcher::stage_continuation_resume(
        room,
        tab,
        offered.resume.schema_version(),
        offered.resume.bytes().to_vec(),
        offered.record_instance.0,
    );
    assert_eq!(
        crate::shell::launcher::take_staged_resume_for_package("so.tilo.browser", room),
        Some((Some((metadata.schema_version, metadata.bytes)), tab))
    );
    crate::shell::launcher::clear_pending_placements();
    registry
        .lock()
        .unwrap()
        .forget_local_tab(AppInstanceId(tab));
    left.close().await;
    drop(left);
    right.close().await;
    drop(right);
}

struct OwnerRegistryScope(
    Option<Arc<Mutex<crate::shell::personal_continuity_registry::ContinuationRegistry>>>,
);
impl Drop for OwnerRegistryScope {
    fn drop(&mut self) {
        crate::shell::personal_continuity_registry::replace_test_registry(self.0.take());
    }
}
fn in_owner_registry<T>(
    registry: &Arc<Mutex<crate::shell::personal_continuity_registry::ContinuationRegistry>>,
    run: impl FnOnce() -> T,
) -> T {
    let _scope = OwnerRegistryScope(
        crate::shell::personal_continuity_registry::replace_test_registry(Some(registry.clone())),
    );
    run()
}

type OwnerFixtureFuture<'local, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = T> + 'local>>;

struct NativeEditorPair {
    home: tilo_test_env::TestHome,
    _settings: crate::shell::test_support::InstalledSettings,
    left: Session,
    right: Session,
    source: tilo_app::framework::testing::TestApp<code_editor::CodeEditorShell>,
    target: tilo_app::framework::testing::TestApp<code_editor::CodeEditorShell>,
    room: RoomId,
    tab: u64,
    peers: PathBuf,
    left_registry: Arc<Mutex<crate::shell::personal_continuity_registry::ContinuationRegistry>>,
    right_registry: Arc<Mutex<crate::shell::personal_continuity_registry::ContinuationRegistry>>,
}

type MountedEditor = tilo_app::framework::testing::TestApp<code_editor::CodeEditorShell>;

fn install_editor_package(home: &mut tilo_test_env::TestHome) {
    let packages = home.data().join("apps");
    let payload = packages.join("payloads/so.tilo.code-editor");
    std::fs::create_dir_all(&payload).unwrap();
    let manifest = payload.join("manifest.toml");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/code-editor/tilo-app.toml"),
        &manifest,
    )
    .unwrap();
    std::fs::write(
        packages.join("registry.toml"),
        format!(
            "[[entry]]\nid = 'so.tilo.code-editor'\npayload_root = {:?}\nmanifest_path = {:?}\n",
            payload.to_str().unwrap(),
            manifest.to_str().unwrap(),
        ),
    )
    .unwrap();
    home.env().set("TILO_APPS_DATA_DIR", &packages);
}

fn mounted_editors(home: &tilo_test_env::TestHome) -> (MountedEditor, MountedEditor) {
    let path = home.data().join("owner-fixture.rs");
    code_editor::native_fixture::seed(&path).unwrap();
    let mut source =
        tilo_app::framework::testing::TestApp::<code_editor::CodeEditorShell>::launched(&[path
            .to_str()
            .unwrap()])
        .unwrap();
    source
        .settings::<code_editor::settings::CodeSettings>()
        .set(&code_editor::settings::CodeSettings {
            language_servers: false,
            ..Default::default()
        })
        .unwrap();
    source.settle();
    let mut document = source.peer_document();
    let mut target =
        tilo_app::framework::testing::TestApp::<code_editor::CodeEditorShell>::launched_replica(
            &[path.to_str().unwrap()],
            &mut document,
        )
        .unwrap();
    target
        .settings::<code_editor::settings::CodeSettings>()
        .set(&code_editor::settings::CodeSettings {
            language_servers: false,
            ..Default::default()
        })
        .unwrap();
    target.settle();
    (source, target)
}

fn claim_editor(
    editor: &mut MountedEditor,
    registry: &Arc<Mutex<crate::shell::personal_continuity_registry::ContinuationRegistry>>,
    room: RoomId,
    instance: u64,
    token: u64,
    line: usize,
) {
    use tilo_action_broker::services::ContinuationServices;
    use ui::widget::Continuable;
    editor
        .effect(&code_editor::CodeEditorEffect::GoToLine {
            value: Some(line.to_string()),
        })
        .unwrap();
    editor.settle();
    let document = editor.peer_document();
    let semantic = editor
        .surface_mut::<code_editor::CodeApp>()
        .continuation_metadata(&document, &automerge::ROOT)
        .unwrap();
    let metadata = ui::app::compose_structural_continuation_metadata(
        ui::view::View::default(),
        Some(semantic),
    )
    .unwrap();
    in_owner_registry(registry, || {
        crate::shell::broker_services::NativeContinuations
            .handoff(
                room,
                "so.tilo.code-editor".into(),
                instance,
                collab_types::handoff::HandoffReport::OwnerTakeover {
                    token,
                    schema: metadata.schema_version,
                    metadata: metadata.bytes,
                }
                .encode(),
            )
            .unwrap();
    });
}

/// A continuation registry that knows `tab` as the same app instance on both sides.
fn registry_bound_to(
    tab: u64,
) -> Arc<Mutex<crate::shell::personal_continuity_registry::ContinuationRegistry>> {
    let registry = crate::shell::personal_continuity_registry::ContinuationRegistry::default();
    let registry = Arc::new(Mutex::new(registry));
    registry.lock().unwrap().bind_tab(
        crate::shell::personal_continuity::AppInstanceId(tab),
        crate::shell::personal_continuity::AppInstanceId(tab),
    );
    registry
}

impl NativeEditorPair {
    fn new() -> OwnerFixtureFuture<'static, Self> {
        Box::pin(async move {
            use crate::shell::state::{Tab, TileContent, WindowGroup, WindowWidth, rehomed};
            use ui::widget::Continuable;
            let mut home = tilo_test_env::TestHome::new().unwrap();
            let peers = home.config().join("home-peers.json");
            home.env().set("TILO_HOME_PEERS_PATH", &peers);
            install_editor_package(&mut home);
            let (source, mut target) = mounted_editors(&home);
            let paths = settings_store::SessionPaths::beside(home.config().join("settings.json"));
            let settings = crate::shell::test_support::install_settings(Arc::new(
                settings_store::SessionSettings::open(paths).unwrap(),
            ));
            settings_store::update_section(
                "continuity",
                serde_json::json!({
                    "enabled": true, "app_continuations": true
                })
                .as_object()
                .unwrap()
                .clone(),
            )
            .unwrap();
            let signer = ed25519_dalek::SigningKey::from_bytes(&[0xB1; 32]);
            let room = RoomId::new();
            let tab = 0xC011_00B1;
            let mut tree = WorkspaceTree::bootstrap_default();
            tree.desktop.workspaces[0] = rehomed(&tree.desktop.workspaces[0], room);
            tree.desktop.workspaces[0].strips[0]
                .window_groups
                .push(WindowGroup {
                    id: crate::shell::state::next_id(),
                    tabs: vec![Tab {
                        id: tab,
                        title: "Code".into(),
                        tile: TileContent::Foreign {
                            app_id: "so.tilo.code-editor".into(),
                            surface_id: 0xB1,
                        },
                        app_title: None,
                    }],
                    active_tab: tab,
                    width: WindowWidth::Normal,
                    maximized: false,
                    focus_history: Vec::new(),
                });
            let mut local = WorkspaceTree::bootstrap_default();
            local.desktop.workspaces[0] = rehomed(&local.desktop.workspaces[0], room);
            let mut left = Box::pin(Session::new(&signer, 0xB1, tree)).await;
            let mut right = Box::pin(Session::new(&signer, 0xB2, local)).await;
            left.network_mut().personal_continuity_path =
                Some(home.data().join("left-continuity.bin"));
            right.network_mut().personal_continuity_path =
                Some(home.data().join("right-continuity.bin"));
            let left_registry = registry_bound_to(tab);
            let right_registry = registry_bound_to(tab);
            let target_document = target.peer_document();
            let target_semantic = target
                .surface_mut::<code_editor::CodeApp>()
                .continuation_metadata(&target_document, &automerge::ROOT)
                .unwrap();
            let target_metadata = ui::app::compose_structural_continuation_metadata(
                ui::view::View::default(),
                Some(target_semantic),
            )
            .unwrap();
            right_registry
                .lock()
                .unwrap()
                .report(
                    room,
                    "so.tilo.code-editor",
                    crate::shell::personal_continuity::AppInstanceId(tab),
                    target_metadata.schema_version,
                    target_metadata.bytes,
                )
                .unwrap();
            pair(&left, &right).await;
            Self {
                home,
                _settings: settings,
                left,
                right,
                source,
                target,
                room,
                tab,
                peers,
                left_registry,
                right_registry,
            }
        })
    }

    fn send_view(
        &mut self,
        line: usize,
    ) -> OwnerFixtureFuture<'_, Vec<collab_types::handoff::OwnerView>> {
        Box::pin(async move {
            use ui::widget::Continuable;
            self.source
                .effect(&code_editor::CodeEditorEffect::GoToLine {
                    value: Some(line.to_string()),
                })
                .unwrap();
            self.source.settle();
            let document = self.source.peer_document();
            let semantic = self
                .source
                .surface_mut::<code_editor::CodeApp>()
                .continuation_metadata(&document, &automerge::ROOT)
                .unwrap();
            let metadata = ui::app::compose_structural_continuation_metadata(
                ui::view::View::default(),
                Some(semantic),
            )
            .unwrap();
            self.left_registry
                .lock()
                .unwrap()
                .report(
                    self.room,
                    "so.tilo.code-editor",
                    crate::shell::personal_continuity::AppInstanceId(self.tab),
                    metadata.schema_version,
                    metadata.bytes.clone(),
                )
                .unwrap();
            in_owner_registry(&self.left_registry, || {
                self.left.network.publish_personal_continuity_if_changed();
            });
            {
                let captured = self.left.network.personal_continuity.lock().unwrap();
                let captured_app = captured
                    .newest_app_state_in_room(
                        self.room,
                        crate::shell::personal_continuity::AppInstanceId(self.tab),
                    )
                    .expect("the real source app must appear in its production capture");
                assert_eq!(
                    captured_app
                        .resume
                        .as_ref()
                        .expect("source has a safe view")
                        .bytes(),
                    metadata.bytes
                );
                drop(captured);
            }
            tilo_test_env::within("native editor owner view crosses certified Iroh", async {
                loop {
                    for (session, registry) in [
                        (&self.left, &self.left_registry),
                        (&self.right, &self.right_registry),
                    ] {
                        in_owner_registry(registry, || {
                            session.network.tick();
                            loop {
                                let event =
                                    session.deferred.lock().unwrap().pop_front().or_else(|| {
                                        session.network.event_rx.lock().unwrap().try_recv().ok()
                                    });
                                let Some(event) = event else {
                                    break;
                                };
                                session.network.dispatch_net_event(event);
                            }
                        });
                    }
                    let reached = self
                        .right
                        .network
                        .personal_continuity
                        .lock()
                        .unwrap()
                        .newest_app_state_in_room(
                            self.room,
                            crate::shell::personal_continuity::AppInstanceId(self.tab),
                        )
                        .is_some_and(|app| {
                            app.resume
                                .as_ref()
                                .is_some_and(|resume| resume.bytes() == metadata.bytes)
                        });
                    if reached {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await;
            in_owner_registry(&self.right_registry, || {
                self.right.network.pump_owner_views()
            })
            .into_iter()
            .map(|(view, _)| view)
            .collect()
        })
    }

    fn apply_target(&mut self, view: &collab_types::handoff::OwnerView, line: usize) {
        use ui::widget::{Continuable, Widget};
        assert_eq!(view.room, self.room.to_string());
        assert_eq!(view.app, "so.tilo.code-editor");
        assert_eq!(view.instance, self.tab);
        assert!(!view.local_claim);
        let projection = view.projection.as_ref().unwrap();
        let mut document = self.target.peer_document();
        let heads = document.get_heads();
        let target = self.target.surface_mut::<code_editor::CodeApp>();
        let before = target
            .native_input_snapshot(&document, &automerge::ROOT)
            .unwrap();
        assert!(target.restore_live_continuation_metadata(
            &ui::collaboration::ContinuationMetadata {
                schema_version: projection.schema_version,
                bytes: projection.bytes.clone(),
            },
            &document,
            &automerge::ROOT
        ));
        let after = target
            .native_input_snapshot(&document, &automerge::ROOT)
            .unwrap();
        assert_eq!(after["caret_line"], line.checked_sub(1).unwrap());
        assert_eq!(after["body_hash"], before["body_hash"]);
        assert_eq!(after["editor_focused"], before["editor_focused"]);
        assert_eq!(document.get_heads(), heads);
    }

    fn claim_target(&mut self, token: u64, line: usize) {
        claim_editor(
            &mut self.target,
            &self.right_registry,
            self.room,
            self.tab,
            token,
            line,
        );
    }

    fn adopt_target(&self) {
        let tree = self.right.network.tree.clone();
        let mut tree = tree.write();
        assert_eq!(
            crate::shell::tab_binding::adopt_toplevel_at(
                &mut tree,
                0xB2,
                Some("so.tilo.code-editor".into()),
                crate::shell::tab_binding::Placement::IntoTab(self.tab)
            ),
            self.tab
        );
        assert!(tree.claim_continuation_tab(self.tab));
        drop(tree);
    }

    fn dispatch(&self) {
        for (session, registry) in [
            (&self.left, &self.left_registry),
            (&self.right, &self.right_registry),
        ] {
            in_owner_registry(registry, || {
                session.network.tick();
                loop {
                    let event = session
                        .deferred
                        .lock()
                        .unwrap()
                        .pop_front()
                        .or_else(|| session.network.event_rx.lock().unwrap().try_recv().ok());
                    let Some(event) = event else {
                        break;
                    };
                    session.network.dispatch_net_event(event);
                }
            });
        }
    }

    fn target_snapshot(&self) -> crate::shell::personal_continuity::AppInstanceState {
        self.right
            .network
            .personal_continuity
            .lock()
            .unwrap()
            .snapshot_for(
                self.right
                    .network
                    .personal_continuity_device
                    .as_ref()
                    .unwrap(),
            )
            .expect("a captured local target")
            .workspaces
            .iter()
            .find(|workspace| workspace.room == self.room)
            .unwrap()
            .apps
            .iter()
            .find(|app| app.id.0 == self.tab)
            .unwrap()
            .clone()
    }

    fn close(&mut self) -> OwnerFixtureFuture<'_, ()> {
        Box::pin(async move {
            self.left.close().await;
            self.right.close().await;
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn paired_native_editor_views_follow_the_persons_root_and_stop_when_routes_or_policy_go() {
    use tilo_action_broker::services::ContinuationServices;
    let mut pair = NativeEditorPair::new().await;
    // No enrolment lists either device: proving certificates of one root to
    // each other makes them the person's devices.
    assert!(
        !pair
            .peers
            .with_file_name("personal-device-peers.json")
            .exists()
    );
    let first = pair.send_view(40).await;
    assert_eq!(first.len(), 1);
    pair.apply_target(&first[0], 40);
    let second = pair.send_view(70).await;
    assert_eq!(second.len(), 1);
    assert!(second[0].generation > first[0].generation);
    pair.apply_target(&second[0], 70);
    in_owner_registry(&pair.right_registry, || {
        crate::shell::broker_services::NativeContinuations
            .handoff(
                pair.room,
                "so.tilo.code-editor".into(),
                pair.tab,
                collab_types::handoff::HandoffReport::OwnerSubscribe { version: 1 }.encode(),
            )
            .unwrap();
    });
    let subscribed = in_owner_registry(&pair.right_registry, || {
        pair.right.network.pump_owner_views()
    });
    assert_eq!(subscribed.len(), 1);
    assert!(subscribed[0].0.generation > second[0].generation);
    pair.apply_target(&subscribed[0].0, 70);
    // A departed authenticated session cannot keep a cached view alive.
    let routes = pair
        .right
        .network
        .authenticated_personal_routes
        .lock()
        .unwrap()
        .clone();
    pair.right
        .network
        .authenticated_personal_routes
        .lock()
        .unwrap()
        .clear();
    let offline = in_owner_registry(&pair.right_registry, || {
        pair.right.network.pump_owner_views()
    });
    assert_eq!(offline.len(), 1);
    assert!(offline[0].0.projection.is_none());
    *pair
        .right
        .network
        .authenticated_personal_routes
        .lock()
        .unwrap() = routes;
    let online = in_owner_registry(&pair.right_registry, || {
        pair.right.network.pump_owner_views()
    });
    assert_eq!(online.len(), 1);
    assert!(online[0].0.generation > offline[0].0.generation);
    pair.apply_target(&online[0].0, 70);
    pair.right.network_mut().continuity_policy_override =
        Some(crate::shell::continuity_policy::ContinuityPolicySnapshot::OFF);
    let revoked = in_owner_registry(&pair.right_registry, || {
        pair.right.network.pump_owner_views()
    });
    assert_eq!(revoked.len(), 1);
    assert!(revoked[0].0.projection.is_none());
    pair.close().await;
    drop(pair);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn paired_native_editor_views_stop_when_the_root_revokes_the_viewer() {
    let mut pair = NativeEditorPair::new().await;
    let first = pair.send_view(40).await;
    assert_eq!(first.len(), 1);
    pair.apply_target(&first[0], 40);
    // Another device of the person brings the root's revocation of the
    // viewing device.
    let root = ed25519_dalek::SigningKey::from_bytes(&[0xB1; 32]);
    let mut entry = tilo_user_identity::RevocationEntry {
        user_public_key: root.verifying_key().to_bytes(),
        revoked_device_public_key: pair.left.cert.device_public_key,
        revoked_at_ns: 1,
        reason: tilo_user_identity::RevocationReason::Stolen,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
    let mut revocations = tilo_user_identity::RevocationList::new();
    revocations.insert(entry).unwrap();
    pair.right.network.learn_own_revocations(&revocations);
    let stopped = in_owner_registry(&pair.right_registry, || {
        pair.right.network.pump_owner_views()
    });
    assert_eq!(stopped.len(), 1);
    assert!(stopped[0].0.projection.is_none());
    assert!(stopped[0].0.generation > first[0].generation);
    assert!(
        in_owner_registry(&pair.right_registry, || pair
            .right
            .network
            .pump_owner_views())
        .is_empty()
    );
    pair.close().await;
    drop(pair);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn paired_native_editor_takeover_is_durable_idempotent_and_survives_restart() {
    let mut pair = NativeEditorPair::new().await;
    let remote = pair.send_view(40).await;
    assert_eq!(remote.len(), 1);
    pair.apply_target(&remote[0], 40);
    pair.adopt_target();
    let durable = pair.right.network.personal_continuity_path.clone().unwrap();
    let blocked = pair.home.data().join("blocked-write");
    std::fs::create_dir(&blocked).unwrap();
    pair.right.network_mut().personal_continuity_path = Some(blocked);
    let token = remote[0].counter.checked_add(1).unwrap();
    pair.claim_target(token, 55);
    in_owner_registry(&pair.right_registry, || {
        assert!(
            pair.right
                .network
                .local_continuity_snapshot_bytes()
                .is_some()
        );
    });
    // The checkpoint is written on the state writer; this one cannot land.
    crate::shell::durable_writer::global().flush();
    let interrupted = pair.target_snapshot();
    assert!(interrupted.stamp.counter > remote[0].counter);
    assert!(
        pair.right_registry
            .lock()
            .unwrap()
            .owner_claimed(pair.room, &interrupted)
    );
    assert!(
        in_owner_registry(&pair.right_registry, || pair
            .right
            .network
            .pump_owner_views())
        .is_empty(),
        "no takeover acknowledgement before durable write"
    );
    pair.right.network_mut().personal_continuity_path = Some(durable.clone());
    in_owner_registry(&pair.right_registry, || {
        assert!(
            pair.right
                .network
                .local_continuity_snapshot_bytes()
                .is_some()
        );
    });
    let confirmed = pair.target_snapshot();
    assert_eq!(
        confirmed.stamp, interrupted.stamp,
        "quiet retry does not invent another owner revision"
    );
    // The acknowledgement waits for the state writer to put the retry on
    // disk.
    crate::shell::durable_writer::global().flush();
    let ack = in_owner_registry(&pair.right_registry, || {
        pair.right.network.pump_owner_views()
    });
    assert_eq!(ack.len(), 1);
    assert!(ack[0].0.local_claim);
    assert_eq!(ack[0].0.counter, confirmed.stamp.counter);
    let revision = pair.right_registry.lock().unwrap().owner_claim_revision();
    pair.claim_target(token, 80);
    assert_eq!(
        pair.right_registry.lock().unwrap().owner_claim_revision(),
        revision
    );
    in_owner_registry(&pair.right_registry, || {
        pair.right
            .network
            .local_continuity_snapshot_bytes()
            .unwrap();
    });
    assert_eq!(
        pair.target_snapshot().resume,
        confirmed.resume,
        "an old token cannot overwrite the confirmed view"
    );
    crate::shell::durable_writer::global().flush();
    let restored = load_personal_continuity(&durable);
    assert_eq!(
        restored
            .continuity
            .snapshot_for(
                pair.right
                    .network
                    .personal_continuity_device
                    .as_ref()
                    .unwrap()
            )
            .unwrap()
            .workspaces
            .iter()
            .find(|workspace| workspace.room == pair.room)
            .unwrap()
            .apps[0],
        confirmed
    );
    *pair.right.network.personal_continuity.lock().unwrap() = restored.continuity;
    *pair
        .right
        .network
        .personal_continuity_counter
        .lock()
        .unwrap() = 0;
    *pair
        .right
        .network
        .personal_continuity_fingerprint
        .lock()
        .unwrap() = None;
    pair.right_registry = Arc::new(Mutex::new(
        crate::shell::personal_continuity_registry::ContinuationRegistry::default(),
    ));
    pair.right_registry.lock().unwrap().bind_tab(
        crate::shell::personal_continuity::AppInstanceId(pair.tab),
        crate::shell::personal_continuity::AppInstanceId(pair.tab),
    );
    let resume = confirmed.resume.clone().unwrap();
    pair.right_registry
        .lock()
        .unwrap()
        .report(
            pair.room,
            "so.tilo.code-editor",
            crate::shell::personal_continuity::AppInstanceId(pair.tab),
            resume.schema_version(),
            resume.bytes().to_vec(),
        )
        .unwrap();
    in_owner_registry(&pair.right_registry, || {
        pair.right
            .network
            .local_continuity_snapshot_bytes()
            .unwrap();
    });
    assert_eq!(
        pair.target_snapshot().stamp,
        confirmed.stamp,
        "restart preserves unchanged owner revision"
    );
    pair.claim_target(confirmed.stamp.counter.checked_add(1).unwrap(), 90);
    in_owner_registry(&pair.right_registry, || {
        pair.right
            .network
            .local_continuity_snapshot_bytes()
            .unwrap();
    });
    assert!(pair.target_snapshot().stamp.counter > confirmed.stamp.counter);
    pair.close().await;
    drop(pair);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn paired_native_editor_concurrent_takeovers_converge_on_one_durable_stamp() {
    let mut pair = NativeEditorPair::new().await;
    let remote = pair.send_view(40).await;
    assert_eq!(remote.len(), 1);
    pair.apply_target(&remote[0], 40);
    pair.adopt_target();
    let token = remote[0].counter.checked_add(1).unwrap();
    claim_editor(
        &mut pair.source,
        &pair.left_registry,
        pair.room,
        pair.tab,
        token,
        65,
    );
    pair.claim_target(token, 55);
    for (session, registry) in [
        (&pair.left, &pair.left_registry),
        (&pair.right, &pair.right_registry),
    ] {
        in_owner_registry(registry, || {
            session.network.publish_personal_continuity_if_changed();
        });
    }
    let left = pair
        .left
        .network
        .personal_continuity
        .lock()
        .unwrap()
        .snapshot_for(
            pair.left
                .network
                .personal_continuity_device
                .as_ref()
                .unwrap(),
        )
        .unwrap()
        .workspaces
        .iter()
        .find(|workspace| workspace.room == pair.room)
        .unwrap()
        .apps[0]
        .clone();
    let right = pair.target_snapshot();
    assert_eq!(
        left.stamp.counter, right.stamp.counter,
        "the fixture actually exercises concurrent equal Lamport counters"
    );
    let winner = if left.stamp.device > right.stamp.device {
        left
    } else {
        right
    };
    tilo_test_env::within(
        "concurrent native owner claims converge across certified Iroh",
        async {
            loop {
                pair.dispatch();
                if [&pair.left, &pair.right].into_iter().all(|session| {
                    session
                        .network
                        .personal_continuity
                        .lock()
                        .unwrap()
                        .newest_app_state_in_room(
                            pair.room,
                            crate::shell::personal_continuity::AppInstanceId(pair.tab),
                        )
                        == Some(&winner)
                }) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        },
    )
    .await;
    // Each claimant's acknowledgement waits for its checkpoint to be on disk.
    crate::shell::durable_writer::global().flush();
    for (session, registry) in [
        (&pair.left, &pair.left_registry),
        (&pair.right, &pair.right_registry),
    ] {
        let controls = in_owner_registry(registry, || session.network.pump_owner_views());
        assert!(
            controls[0].0.local_claim,
            "even the losing concurrent claimant gets its durable ack first"
        );
        if session.network.personal_continuity_device.as_ref() != Some(&winner.stamp.device) {
            assert_eq!(controls.len(), 2);
            assert!(!controls[1].0.local_claim);
            assert_eq!(controls[1].0.source, winner.stamp.device.as_str());
            assert!(controls[1].0.generation > controls[0].0.generation);
        }
        // An unrelated workspace title update must not reelect its old local view.
        let tree = session.network.tree.clone();
        tree.write().desktop.workspaces[0].name = "Local arrangement".into();
        in_owner_registry(registry, || {
            session.network.local_continuity_snapshot_bytes().unwrap();
        });
        assert_eq!(
            session
                .network
                .personal_continuity
                .lock()
                .unwrap()
                .newest_app_state_in_room(
                    pair.room,
                    crate::shell::personal_continuity::AppInstanceId(pair.tab)
                ),
            Some(&winner)
        );
    }
    pair.close().await;
    drop(pair);
}
