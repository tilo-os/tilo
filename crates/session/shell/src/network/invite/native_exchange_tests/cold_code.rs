//! A cold Code body crosses genuine signed enrollment and retained Iroh input.
//! The app profile is absent; only the native owner may supply document bytes.
use super::*;
use automerge::{AutoCommit, ReadDoc as _};
use std::path::Path;
use tilo_actions::Action as _;
use tilo_joined_room::{ApplicationInitialization, ApplicationRecord, DocumentRead};

pub(super) const APP: &str = "so.tilo.code-editor";
pub(super) const RECORD: &str = "tab:000000000000002a";
pub(super) const FILE: &str = "fe55ab4e-e645-4499-aa86-0c4141907080";
pub(super) const BODY: &str =
    "fn native_continuation() {\n    // shared text, locally rendered\n}\n";

#[tokio::test]
async fn native_cold_code_body_is_admitted_before_local_rendering() {
    Box::pin(super::actual_exchange(false, true, None)).await;
}

pub(super) fn governor(directory: &Path) -> TestGovernor {
    let key = ed25519_dalek::SigningKey::from_bytes(&[0xC1; 32]);
    let identity_key = ed25519_dalek::SigningKey::from_bytes(&[0xC2; 32]);
    let certificate = tilo_joined_room::testing::certificate(&identity_key, &key, "Code source");
    let room = RoomId::parse(tilo_joined_room::testing::ROOM).unwrap();
    let mut tree = crate::shell::state::WorkspaceTree::bootstrap_default();
    assert!(tree.set_room(tree.desktop.active, room));
    tree.persist_path = Some(directory.join("source-state.json"));
    let owner = crate::shell::native_workspace_owner::open(tree.shared()).unwrap();
    let mut governor =
        tilo_joined_room::testing::enroll_interactive_governor(&owner, &key, &certificate).unwrap();
    let route = ApplicationRecord::declared(
        APP,
        RECORD,
        code_editor::resource::PROJECT_KIND,
        &[code_editor::resource::PROJECT_KIND.into()],
    )
    .unwrap();
    let now = SystemTime::now();
    assert!(
        owner
            .prepare_application_record(room, &certificate, &key, &route, now)
            .unwrap()
    );
    let capture = capture(&owner, &certificate);
    let mut document = source_document(directory);
    owner
        .initialize_application_record(
            room,
            &certificate,
            &key,
            ApplicationInitialization {
                route: &route,
                revision: capture.revision,
                document: &document.save(),
            },
            now,
        )
        .unwrap();
    owner.reconcile_outgoing(room, &key, now).unwrap();
    // Export a covered, signed cut from the actual native creator. Its
    // initial full snapshots need the matching canonical governance coverage.
    let publications = owner
        .with_current(room, |view| {
            assert_eq!(view.device(), &certificate);
            view.peer()
                .clone()
                .durable_bootstrap_messages(&key, now, view.generation())
        })
        .unwrap();
    assert!(
        !publications.is_empty(),
        "the native creator must export its signed cut"
    );
    for message in publications {
        governor
            .receive(
                tilo_peer_state::TransportOrigin::RetainedNativePublication {
                    publisher: certificate.device_public_key,
                },
                message,
            )
            .unwrap();
    }
    assert_eq!(
        body(
            &AutoCommit::load(
                &governor
                    .peer()
                    .document(APP, RECORD)
                    .expect("signed native creator publications must reach governor custody")
            )
            .unwrap()
        ),
        BODY
    );
    governor.refresh_checkpoint().unwrap();
    let host_key = ed25519_dalek::SigningKey::from_bytes(&[0xC3; 32]);
    let mut host =
        tilo_joined_room::testing::certificate(&identity_key, &host_key, "Delivery host");
    host.capabilities.can_act_as_host = true;
    tilo_user_identity::sign_device_cert(&mut host, &identity_key).unwrap();
    governor.with_delivery_host(host, host_key).unwrap()
}

#[track_caller]
fn capture(
    owner: &NativeWorkspaceOwner,
    certificate: &DeviceCert,
) -> tilo_joined_room::CommittedDocument {
    try_capture(owner, certificate)
        .unwrap()
        .expect("a settled cold native room holds no partial checkpoint")
}

/// `None` is the production read's answer while a checkpoint is still
/// arriving; every refusal stays an error.
fn try_capture(
    owner: &NativeWorkspaceOwner,
    certificate: &DeviceCert,
) -> Result<Option<tilo_joined_room::CommittedDocument>, String> {
    owner.with_current(
        RoomId::parse(tilo_joined_room::testing::ROOM).unwrap(),
        |view| match view.read_document(
            certificate.device_public_key,
            APP,
            RECORD,
            None,
            64 * 1024,
            SystemTime::now(),
        )? {
            DocumentRead::Captured(capture) => Ok(Some(capture)),
            DocumentRead::CheckpointPending => Ok(None),
            DocumentRead::Unchanged(_) => Err("cold native record was not captured".into()),
        },
    )
}

/// Seed only the source through the SDK's original record genesis and action.
/// These bytes become authority only after the native creator's checked CAS.
fn source_document(directory: &Path) -> AutoCommit {
    use ui::record::{RecordCheckout, RecordId, RecordKind, RecordOwners};
    let records = RecordOwners::new(directory.join("source-fixture-profile"))
        .open(APP, tilo_joined_room::testing::ROOM)
        .unwrap();
    let checkout = RecordCheckout::new(
        RecordId::parse(RECORD).unwrap(),
        RecordKind::new(code_editor::resource::PROJECT_KIND),
    );
    records.create_record(&checkout).unwrap();
    records.try_open_record(&checkout).unwrap();
    records
        .with(&checkout.id, |document| {
            code_editor::actions::CodeEditorAction::CreateFile {
                id: FILE.into(),
                name: "native.rs".into(),
                parent: None,
                language: "rust".into(),
                order: 0,
                seed: BODY.into(),
            }
            .apply(document)
            .unwrap();
            assert_eq!(
                body(document),
                BODY,
                "the original source action must materialize its resource"
            );
        })
        .unwrap();
    records.try_flush().unwrap();
    records.read(&checkout.id, Clone::clone).unwrap()
}

pub(super) fn assert_native_body(
    owner: &NativeWorkspaceOwner,
    identity: &HostIdentity,
    directory: &Path,
) {
    let profile = directory.join("cold-code-profile");
    assert!(!profile.exists(), "the receiving app profile must be cold");
    let capture = capture(owner, &identity.device_cert);
    let document = AutoCommit::load(&capture.bytes).unwrap();
    let buffers = document.get(automerge::ROOT, "buffers").unwrap().unwrap().1;
    let text = document.get(&buffers, FILE).unwrap().unwrap().1;
    assert_eq!(document.text(&text).unwrap(), BODY);
    assert_eq!(ui::record::identity(&document).unwrap().id.as_str(), RECORD);
    assert_eq!(
        ui::record::room_scope(&document).as_deref(),
        Some(tilo_joined_room::testing::ROOM)
    );
    mount_cold_component(owner, identity, &profile);
}

fn mount_cold_component(owner: &NativeWorkspaceOwner, identity: &HostIdentity, profile: &Path) {
    let tree = crate::shell::native_workspace_owner::test_tree_of(owner);
    let (network, commands) =
        crate::shell::network::native_read_network_for_test(tree, identity.clone());
    let mut mounted = mount_with_network(owner, identity, profile, &Arc::new(network));
    open_semantic_resource(&mut mounted.app);
    drop(mounted);
    drop(commands);
}

pub(super) struct MountedCode {
    pub app: ui::App,
    _broker: crate::shell::collab_broker::BrokerHandle,
    _runtime: tempfile::TempDir,
}

impl Drop for MountedCode {
    fn drop(&mut self) {
        self.app.close_components();
    }
}

pub(super) fn mount_with_network(
    owner: &NativeWorkspaceOwner,
    identity: &HostIdentity,
    profile: &Path,
    network: &Arc<crate::shell::network::ShellNetwork>,
) -> MountedCode {
    mount_with_network_and_reads(owner, identity, profile, network, None)
}
pub(super) fn mount_with_network_and_reads(
    owner: &NativeWorkspaceOwner,
    identity: &HostIdentity,
    profile: &Path,
    network: &Arc<crate::shell::network::ShellNetwork>,
    reads: Option<crate::shell::collab_broker::NativeReadHarnessWrapper>,
) -> MountedCode {
    use std::{
        cell::{Cell, RefCell},
        os::unix::fs::PermissionsExt as _,
        rc::Rc,
    };
    let tree = crate::shell::native_workspace_owner::test_tree_of(owner);
    assert!(
        tree.read()
            .commits_native_documents(RoomId::parse(tilo_joined_room::testing::ROOM).unwrap())
    );
    let runtime = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(profile.parent().unwrap())
        .unwrap();
    let socket = runtime.path().join("c.sock");
    let broker = crate::shell::collab_broker::spawn_native_owner_harness_with_reads(
        socket.clone(),
        tree,
        vec![identity.device_cert.device_public_key],
        network,
        reads,
    )
    .unwrap();
    let refusal = connect_manager(&socket, identity, false)
        .err()
        .expect("a certified device alone cannot obtain native read custody");
    assert!(
        refusal
            .to_string()
            .contains("committed read requires launcher custody")
    );
    let manager = connect_manager(&socket, identity, true).unwrap();
    let catalog = ui::font_catalog::FontCatalog::controlled_fixture(
        ui::font_catalog::checkout_root(),
        "en-US",
    );
    let inputs = ui::application::ComponentRunnerInputs {
        theme: ui::Theme::default(),
        text_renderer: ui::TextRenderer::with_font_catalog(&catalog).unwrap(),
    };
    let factories = Cell::new(0_usize);
    let admitted = capture(owner, &identity.device_cert);
    let before = admitted.revision;
    let expected_body = body(&AutoCommit::load(&admitted.bytes).unwrap());
    let app = ui::App::from_prepared_component_with_collaboration(
        manager,
        || {
            let mut host = ui::application::ApplicationHost::new(
                profile.into(),
                hex::encode(identity.device_cert.device_public_key),
                tilo_joined_room::testing::ROOM,
                ui::permissions::UserId(ui::permissions::LOCAL_USER.into()),
                Rc::new(RefCell::new(
                    ui::permissions::PermissionContext::permissive(),
                )),
                tilo_actions::ActionContext::default(),
            )
            .unwrap();
            host.prepare_record(
                code_editor::definition(),
                42,
                ui::record::RecordCheckout::new(
                    ui::record::RecordId::parse(RECORD).unwrap(),
                    ui::record::RecordKind::new(code_editor::resource::PROJECT_KIND),
                ),
                inputs,
            )
        },
        |context, target, restore| {
            context
                .read_record(|document| {
                    let buffers = document.get(automerge::ROOT, "buffers").unwrap().unwrap().1;
                    let text = document.get(&buffers, FILE).unwrap().unwrap().1;
                    assert_eq!(
                        document.text(&text).unwrap(),
                        expected_body,
                        "native body must precede the original factory"
                    );
                })
                .unwrap();
            factories.set(factories.get().saturating_add(1));
            code_editor::definition().instantiate(context, target, restore)
        },
    )
    .expect("cold mount must retain the native read and original factory");
    assert_eq!(factories.get(), 1);
    assert!(
        app.controlled_native_input_snapshot_for_test().is_some(),
        "original Code root must be mounted"
    );
    assert_eq!(
        capture(owner, &identity.device_cert).revision,
        before,
        "mounting cannot initialize or rewrite the admitted record"
    );
    MountedCode {
        app,
        _broker: broker,
        _runtime: runtime,
    }
}

pub(super) struct LocalSurface;

impl ui::app::AppSurface for LocalSurface {
    fn scale_factor(&self) -> f64 {
        1.25
    }
    fn inner_size(&self) -> ui::winit::dpi::PhysicalSize<u32> {
        ui::winit::dpi::PhysicalSize::new(1000, 750)
    }
    fn request_redraw(&self) {}
    fn set_cursor(&self, _: ui::winit::window::CursorIcon) {}
}

/// Exercise the same resource door used by accessibility and automation.
fn open_semantic_resource(app: &mut ui::App) {
    use tilo_resource::Effect as _;
    let mut scene = ui::vello::Scene::new();
    app.surface_frame(&LocalSurface, &mut scene);
    app.refresh_semantic_tree();
    let effect = code_editor::CodeEditorEffect::Open { file: FILE.into() };
    let action = effect.id();
    let node = app
        .semantic_tree()
        .iter()
        .find(|node| node.id.0.contains(FILE) && node.action(action).is_some())
        .expect("the mounted original Code app must publish its open action")
        .id
        .clone();
    assert!(
        app.invoke_semantic_action(&node, action, None)
            .expect("native open action must be admitted")
    );
    tilo_test_env::wait_until(
        "native Code open effect to finish on the original worker",
        || {
            app.surface_frame(&LocalSurface, &mut scene);
            app.controlled_native_input_snapshot_for_test()
                .is_some_and(|snapshot| snapshot["buffer_id"] == FILE && snapshot["ready"] == true)
        },
    );
    let snapshot = app.controlled_native_input_snapshot_for_test().unwrap();
    assert_eq!(
        snapshot["buffer_id"], FILE,
        "mounted Code snapshot: {snapshot}"
    );
    assert_eq!(snapshot["body_bytes"], BODY.len());
    assert_eq!(
        snapshot["ready"], true,
        "native local rendering must bind the admitted resource"
    );
}

fn connect_manager(
    socket: &Path,
    identity: &HostIdentity,
    launcher_custody: bool,
) -> Result<ui::collaboration::CollaborationManager, runtime_portal::NotAvailable> {
    use ed25519_dalek::Signer as _;
    use std::os::fd::IntoRawFd as _;
    let presentation = runtime_collab::CertifiedJoinIdentity {
        workspace_id: tilo_joined_room::testing::ROOM.into(),
        device_certificate: serde_json::to_vec(&identity.device_cert).unwrap(),
        proof: identity
            .device_signing_key
            .sign(&runtime_identity::collaboration_join_signing_bytes(
                tilo_joined_room::testing::ROOM,
            ))
            .to_bytes()
            .to_vec(),
    }
    .encode()
    .unwrap();
    let certified = ui::collaboration::CertifiedEndpointIdentity::new(
        presentation,
        identity.device_signing_key.clone(),
    );
    let mut stream = std::os::unix::net::UnixStream::connect(socket).unwrap();
    if launcher_custody {
        crate::shell::launcher::register_in_process_window(
            APP,
            tilo_joined_room::testing::ROOM,
            Some(42),
        );
        launcher::services::write_collab_handoff(&mut stream, APP, tilo_joined_room::testing::ROOM)
            .unwrap();
    }
    ui::collaboration::CollaborationManager::for_route_fd_with_identity(
        stream.into_raw_fd(),
        APP,
        uuid::Uuid::parse_str(tilo_joined_room::testing::ROOM).unwrap(),
        Some(RECORD.into()),
        Some(hex::encode(identity.device_cert.device_public_key)),
        certified,
    )
}

/// Both device owners commit while no native actor session is connected.
pub(super) fn prepare_offline_edits(
    receiving: &NativeWorkspaceOwner,
    identity: &HostIdentity,
    directory: &Path,
    authority: &ActualAuthority,
) {
    let path = directory.join("source-state.json");
    let mut tree = crate::shell::persist::load_tree_from(&path).unwrap();
    tree.persist_path = Some(path);
    let source = crate::shell::native_workspace_owner::open(tree.shared()).unwrap();
    let mut authority = authority.lock().unwrap();
    let governor = authority.as_mut().unwrap();
    let source_certificate = governor.governor().clone();
    let source_key = governor.governor_key().clone();
    let room = RoomId::parse(governor.room()).unwrap();
    for (bytes, _) in governor.outgoing() {
        source
            .receive(
                room,
                tilo_peer_state::TransportOrigin::RetainedNativePublication {
                    publisher: source_certificate.device_public_key,
                },
                postcard::from_bytes(bytes).unwrap(),
                SystemTime::now(),
            )
            .unwrap();
    }
    let source_message = offline_edit(&source, &source_key, &format!("// source offline\n{BODY}"));
    let receiving_message = offline_edit(
        receiving,
        &identity.device_signing_key,
        &format!("{BODY}// receiving offline\n"),
    );
    for (publisher, message) in [
        (source_certificate.device_public_key, source_message),
        (identity.device_cert.device_public_key, receiving_message),
    ] {
        governor
            .receive(
                tilo_peer_state::TransportOrigin::RetainedRelayDelta(publisher),
                message,
            )
            .unwrap();
    }
    governor.refresh_checkpoint().unwrap();
}

/// Sign `changes` to the Code record the way every SDK document signer signs
/// a write outside its record owner's turn: with the process-wide unordered
/// sequence, never the dense `floor + 1` an owner recovery projection may
/// reserve in its own turn first.
pub(super) fn unordered_document_message(
    room: RoomId,
    changes: Vec<u8>,
    key: &ed25519_dalek::SigningKey,
    now: SystemTime,
) -> NeighbourMsg {
    let room = room.to_string();
    let author = collab_types::Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes());
    let mut envelope = collab_types::SignedOp::new_unsigned(
        collab_types::DocumentMutation::Incremental(changes)
            .encode()
            .unwrap(),
        collab_types::document_artifact_id(APP, &room, RECORD),
        author,
        collab_types::document_attestation_id(APP, &room, RECORD),
        collab_types::OpClass::Splice,
        now,
        collab_types::next_unordered_document_sequence(now),
    );
    collab_types::sign_op(&mut envelope, key).unwrap();
    NeighbourMsg::AppDocument {
        peer: tilo_peer_state::peer_handle(author.as_bytes()),
        app_id: APP.into(),
        cluster_id: room,
        record_id: RECORD.into(),
        envelope: Box::new(envelope),
    }
}

fn offline_edit(
    owner: &NativeWorkspaceOwner,
    key: &ed25519_dalek::SigningKey,
    text: &str,
) -> NeighbourMsg {
    let room = RoomId::parse(tilo_joined_room::testing::ROOM).unwrap();
    let now = SystemTime::now();
    let changes = owner
        .with_current(room, |view| {
            let mut document =
                AutoCommit::load(&view.peer().document(APP, RECORD).unwrap()).unwrap();
            document.set_actor(automerge::ActorId::from(
                key.verifying_key().to_bytes().to_vec(),
            ));
            let heads = document.get_heads();
            code_editor::actions::text_edit_action(FILE.into(), BODY, text)
                .apply(&mut document)
                .unwrap();
            Ok(document.save_after(&heads))
        })
        .unwrap();
    let message = unordered_document_message(room, changes, key, now);
    owner
        .commit_document(tilo_joined_room::testing::request(&message).unwrap(), now)
        .unwrap();
    owner
        .with_current(room, |view| {
            let document = AutoCommit::load(&view.peer().document(APP, RECORD).unwrap()).unwrap();
            assert_eq!(
                body(&document),
                text,
                "the offline edit must be durable before transport"
            );
            Ok(())
        })
        .unwrap();
    message
}

pub(super) fn body(document: &AutoCommit) -> String {
    let buffers = document.get(automerge::ROOT, "buffers").unwrap().unwrap().1;
    let text = document.get(&buffers, FILE).unwrap().unwrap().1;
    document.text(&text).unwrap()
}

pub(super) fn assert_reconnected_body(
    owner: &NativeWorkspaceOwner,
    identity: &HostIdentity,
    authority: &ActualAuthority,
) {
    let capture = capture(owner, &identity.device_cert);
    let mut target = AutoCommit::load(&capture.bytes).unwrap();
    assert_eq!(
        body(&target),
        format!("// source offline\n{BODY}// receiving offline\n")
    );
    let authority = authority.lock().unwrap();
    let mut source = AutoCommit::load(
        &authority
            .as_ref()
            .unwrap()
            .peer()
            .document(APP, RECORD)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        target.get_heads(),
        source.get_heads(),
        "the signed native relay input must converge the complete record"
    );
}

pub(super) async fn restart_after_offline_edits(
    live: refresh::LiveExchange<'_>,
    owner: NativeWorkspaceOwner,
    room: RoomId,
    authority: ActualAuthority,
) {
    use crate::network::actor::{Actor, NetCmd, NetEvent};
    prepare_offline_edits(&owner, live.identity, live.directory.path(), &authority);
    let publications = {
        let authority = authority.lock().unwrap();
        authority
            .as_ref()
            .unwrap()
            .outgoing()
            .map(|(bytes, _)| bytes.to_vec())
            .collect::<Vec<_>>()
    };
    for bytes in publications {
        let message: NeighbourMsg = postcard::from_bytes(&bytes).unwrap();
        let (channel, _) = tilo_peer_state::transport::retention(&message, true).unwrap();
        live.publisher.publish_checkpoint(channel, bytes).unwrap();
    }
    let (commands, actor) = Actor::spawn_reporting_addrs(
        live.endpoint.clone(),
        iroh::address_lookup::memory::MemoryLookup::new(),
        Some(live.identity.clone()),
        Arc::new(Mutex::new(Vec::new())),
    );
    let topic = tilo_relay::room_workspace_id(&room.to_string()).unwrap();
    let (events, mut received) = tokio::sync::mpsc::channel(64);
    commands
        .send(NetCmd::JoinWorkspace {
            ws: topic,
            on_event: events,
        })
        .await
        .unwrap();
    commands
        .send(NetCmd::NativeConnect(
            crate::network::actor::native_ingress::Connect::new(owner.clone(), room).unwrap(),
        ))
        .await
        .unwrap();
    tilo_test_env::within("signed concurrent Code edits to reconnect", async {
        loop {
            let event = received
                .recv()
                .await
                .expect("native reconnect cannot lose its event source");
            if let NetEvent::NativeInput(input) = event {
                input
                    .admit(live.identity)
                    .expect("reconnected signed frame must cross native custody");
                // A canonical cut arrives in multiple signed frames. Until
                // its covered body arrives, the production read must report
                // the pending checkpoint and hand out nothing.
                let Some(capture) = try_capture(&owner, &live.identity.device_cert)
                    .expect("a reconnecting read is pending or captured, never refused")
                else {
                    continue;
                };
                let document = AutoCommit::load(&capture.bytes).unwrap();
                if body(&document) == format!("// source offline\n{BODY}// receiving offline\n") {
                    break;
                }
            }
        }
    })
    .await;
    assert_reconnected_body(&owner, live.identity, &authority);
    commands
        .send(NetCmd::LeaveWorkspace { ws: topic })
        .await
        .unwrap();
    tilo_test_env::within("the reconnected Code device to unsubscribe", async {
        while live
            .relay
            .is_subscribed(topic, &live.identity.device_cert.device_public_key)
            .await
        {
            tokio::task::yield_now().await;
        }
    })
    .await;
    drop(commands);
    actor.abort();
    let _ = actor.await;
}
