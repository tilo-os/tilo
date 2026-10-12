use super::*;
use crate::network::HostIdentity;
use crate::shell::native_workspace_owner::{self, NativeWorkspaceOwner};
use std::os::unix::fs::PermissionsExt as _;
use tilo_actions::remote::feedback::FeedbackAdmission;
use ui::record::RecordId;

fn fixture_network(
    owner: &NativeWorkspaceOwner,
    key: &ed25519_dalek::SigningKey,
) -> (
    Arc<ShellNetwork>,
    tokio::sync::mpsc::Receiver<crate::network::NetCmd>,
) {
    let room = crate::shell::state::RoomId::parse(tilo_joined_room::testing::ROOM).unwrap();
    let certificate = owner
        .with_current(room, |view| Ok(view.device().clone()))
        .unwrap();
    let (network, commands) = crate::shell::network::native_read_network_for_test(
        native_workspace_owner::tree_of(owner),
        HostIdentity {
            device_cert: certificate,
            device_signing_key: key.clone(),
            workspace_device_certs: HashMap::new(),
            workspace_revocations: HashMap::new(),
            identity_paths: None,
        },
    );
    (Arc::new(network), commands)
}
fn destination() -> tilo_desktop::feedback::Destination {
    tilo_desktop::feedback::Destination {
        room: tilo_joined_room::testing::ROOM.into(),
        provider: "record:so.tilo.chat:".into(),
        channel: "feedback".into(),
    }
}
fn answer(connection: &Connection) -> FeedbackAnswer {
    let room = crate::shell::state::RoomId::parse(&connection.destination.room).unwrap();
    let (standing, sandbox) = connection
        .network
        .native_owner()
        .unwrap()
        .with_current(room, |view| {
            let standing = view
                .peer()
                .permissions(
                    connection.reporter.device_public_key,
                    std::time::SystemTime::now(),
                )?
                .standing();
            Ok((standing, view.peer().governed_sandbox()?))
        })
        .unwrap();
    let person = standing.person.clone();
    assert_eq!(person, hex::encode(connection.reporter.user_public_key));
    FeedbackAnswer {
        admission: FeedbackAdmission {
            destination: connection.destination.clone(),
            owner_user: hex::encode(connection.owner.user_public_key),
            owner_device: hex::encode(connection.owner.device_public_key),
            reporter_user: person,
            reporter_device: hex::encode(connection.reporter.device_public_key),
            standing: serde_json::to_value(standing).unwrap(),
            sandbox: serde_json::to_value(sandbox).unwrap(),
            unix_time: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        },
        receipt: None,
    }
}
fn admit(delivery: &Delivery) {
    let mut connection = delivery.connection.borrow_mut();
    let response = answer(&connection);
    let workspace = tilo_relay_protocol::room_workspace_id(&connection.destination.room).unwrap();
    let owner = iroh::EndpointId::from_bytes(&connection.owner.device_public_key).unwrap();
    crate::shell::network::native_remote_answer_for_test(
        &connection.network,
        workspace,
        owner,
        RemoteActionMessage::FeedbackResponse {
            correlation: connection.serial,
            result: Ok(response),
        },
    );
    connection.poll();
    assert!(
        connection.current().is_ok(),
        "only an authenticated correlated owner response supplies admission"
    );
    drop(connection);
}
#[test]
fn native_producer_uses_committed_selected_custody_and_reopens_without_enrollment() {
    let (directory, owner, room, _, key) = native_workspace_owner::test_enrolled_owner();
    let (network, commands) = fixture_network(&owner, &key);
    let (reporter, governor) = network.feedback_custody(room).unwrap();
    let author = hex::encode(reporter.user_public_key);
    let mut delivery = Delivery::new(network.clone(), destination(), &author).unwrap();
    assert!(delivery.connection.borrow().current().is_err());
    let mut wrong = answer(&delivery.connection.borrow());
    wrong.admission.reporter_device = hex::encode([99; 32]);
    assert!(delivery.connection.borrow().admit(&wrong).is_err());
    admit(&delivery);
    let current = delivery.connection.borrow().current().unwrap();
    assert_eq!(
        current.owner.device_public_key(),
        Some(&governor.device_public_key)
    );
    let profile = directory.path().join("feedback-private-profile");
    let mut apps = InProcessApps::at(&profile, &author).unwrap();
    assert!(
        delivery.pump(&mut apps).is_err(),
        "this fixture supplies no application broker; publication must remain unavailable"
    );
    assert!(
        delivery.installed,
        "the production producer installed authenticated startup custody before refusing unavailable publication"
    );
    assert!(apps.feedback.is_some());
    let component = apps
        .mount(
            tilo_desktop::feedback::definition(),
            &destination().room,
            11,
            RecordId::for_tab(11),
            |_| {},
        )
        .unwrap();
    let mut frame = ui::headless::FrameHarness::from_component(
        component,
        (900, 600),
        ui::application::ComponentRunnerInputs::from_native_process(),
    )
    .unwrap();
    frame.relayout();
    assert!(frame.capture_protected());
    // The durable native owner is reopened from its actual state file, never
    // from the old transport admission or an invented LocalOwner policy.
    drop(frame);
    drop(apps);
    drop(delivery);
    drop(network);
    drop(commands);
    drop(owner);
    let path = directory.path().join("state.json");
    let before = std::fs::read(&path).unwrap();
    let mut tree = crate::shell::persist::load_tree_from(&path).unwrap();
    tree.persist_path = Some(path.clone());
    let owner = native_workspace_owner::open(tree.shared()).unwrap();
    let (restored, _restored_commands) = fixture_network(&owner, &key);
    let restarted = Delivery::new(restored.clone(), destination(), &author).unwrap();
    assert!(
        restarted.connection.borrow().current().is_err(),
        "restart cannot restore stale admission"
    );
    admit(&restarted);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "startup creates no enrollment, grant or document write"
    );
    restored.native_owner().unwrap().fence_all();
    assert!(
        restarted.connection.borrow().current().is_err(),
        "uncertain owner custody cannot borrow a cached admission"
    );
    assert!(Delivery::new(restored, destination(), &author).is_err());
}
/// An admission stands only while the room's sandbox is the one it was
/// given under, as well as the reporter's standing.
#[test]
fn an_admission_under_another_sandbox_is_not_current() {
    let (_directory, owner, room, _, key) = native_workspace_owner::test_enrolled_owner();
    let (network, _commands) = fixture_network(&owner, &key);
    let (reporter, _) = network.feedback_custody(room).unwrap();
    let author = hex::encode(reporter.user_public_key);
    let delivery = Delivery::new(network, destination(), &author).unwrap();
    admit(&delivery);
    let mut connection = delivery.connection.borrow_mut();
    let mut changed = answer(&connection);
    let mut sandbox: Vec<collab_types::caps::CapOverride> =
        serde_json::from_value(changed.admission.sandbox.clone()).unwrap();
    sandbox.push(
        collab_types::caps::CapOverride::grant(
            collab_types::caps::Subject::User(collab_types::crypto::Ed25519PublicKey(
                reporter.user_public_key,
            )),
            collab_types::caps::Scope::Device("camera".into()),
            collab_types::caps::Access::Read,
        )
        .with_id("feedback-test-camera"),
    );
    changed.admission.sandbox = serde_json::to_value(sandbox).unwrap();
    let admission = connection.admit(&changed).unwrap();
    connection.admission = Some((admission, Instant::now()));
    let current = connection.current();
    drop(connection);
    assert!(
        current.is_err(),
        "an admission given under another sandbox is not current"
    );
}

#[test]
fn native_configuration_cannot_supply_role_or_reporter_authority() {
    let profile = tempfile::tempdir().unwrap();
    assert!(configured_destination(profile.path()).unwrap().is_none());
    let file = profile.path().join("feedback-destination.json");
    let mut wire = serde_json::to_value(FeedbackDestination {
        room: destination().room,
        provider: destination().provider,
        channel: destination().channel,
    })
    .unwrap();
    wire["role"] = serde_json::json!("Owner");
    std::fs::write(&file, serde_json::to_vec(&wire).unwrap()).unwrap();
    assert!(configured_destination(profile.path()).is_err());
    wire.as_object_mut().unwrap().remove("role");
    std::fs::write(&file, serde_json::to_vec(&wire).unwrap()).unwrap();
    assert_eq!(
        configured_destination(profile.path()).unwrap().unwrap(),
        destination()
    );
}

fn prepare_existing_chat(owner: &NativeWorkspaceOwner, key: &ed25519_dalek::SigningKey) {
    use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};
    let room = crate::shell::state::RoomId::parse(tilo_joined_room::testing::ROOM).unwrap();
    owner
        .commit_document_with_current(
            room,
            |view| {
                let mut document = automerge::AutoCommit::load(
                    &view
                        .peer()
                        .document("so.tilo.chat", "")
                        .ok_or("fixture Chat absent")?,
                )
                .map_err(|error| error.to_string())?;
                let heads = document.get_heads();
                let person = hex::encode(view.device().user_public_key);
                let workspace = room.to_string();
                // The room's seed holds a voice channel its media binding names.
                // The chat's first state is written over what the record holds,
                // keeping that channel's objects, so the binding still resolves.
                {
                    use tilo_resource::{Fields as _, Resource as _};
                    let held = chat::Chat::read_fields(&document, &automerge::ROOT)
                        .map_err(|error| error.to_string())?;
                    held.write_fields(&mut document, &automerge::ROOT)
                        .map_err(|error| error.to_string())?;
                    tilo_resource::evolution::write_version(&mut document, chat::Chat::VERSION)
                        .map_err(|error| error.to_string())?;
                    document.commit_with(
                        automerge::transaction::CommitOptions::default()
                            .with_message(tilo_actions::FrameworkChange::Genesis.change_message()),
                    );
                }
                chat::demo::start_in(
                    &mut document,
                    &person,
                    (&workspace, "Fixture"),
                    "Fixture",
                    "F",
                )
                .map_err(|error| error.to_string())?;
                chat::demo::insert_category(
                    &mut document,
                    &person,
                    "feedback-category",
                    &workspace,
                    "Feedback",
                )
                .map_err(|error| error.to_string())?;
                chat::demo::insert_channel(
                    &mut document,
                    &person,
                    "feedback",
                    (&workspace, "feedback-category"),
                    "Feedback",
                    chat::model::ChannelKind::Text,
                )
                .map_err(|error| error.to_string())?;
                let message = view.peer().clone().publish_incremental(
                    "so.tilo.chat",
                    "",
                    document.save_after(&heads),
                    key,
                    std::time::SystemTime::now(),
                )?;
                let tilo_peer_state::NeighbourMsg::AppDocument { envelope, .. } = message else {
                    return Err("fixture signed write absent".into());
                };
                Ok(OwnerCommitRequest {
                    room: room.to_string(),
                    operation: CommitOperation::Document {
                        app: "so.tilo.chat".into(),
                        record: String::new(),
                        envelope,
                    },
                })
            },
            std::time::SystemTime::now(),
        )
        .unwrap();
}
fn certified_manager(
    socket: &std::path::Path,
    reporter: &DeviceCert,
    key: &ed25519_dalek::SigningKey,
) -> ui::collaboration::CollaborationManager {
    use ed25519_dalek::Signer as _;
    use std::os::fd::IntoRawFd as _;
    let room = tilo_joined_room::testing::ROOM;
    let presentation = runtime_collab::CertifiedJoinIdentity {
        workspace_id: room.into(),
        device_certificate: serde_json::to_vec(reporter).unwrap(),
        proof: key
            .sign(&runtime_identity::collaboration_join_signing_bytes(room))
            .to_bytes()
            .to_vec(),
    }
    .encode()
    .unwrap();
    let identity = ui::collaboration::CertifiedEndpointIdentity::new(presentation, key.clone());
    crate::shell::launcher::register_in_process_launch("so.tilo.chat", room);
    let mut stream = std::os::unix::net::UnixStream::connect(socket).unwrap();
    launcher::services::write_collab_handoff(&mut stream, "so.tilo.chat", room).unwrap();
    ui::collaboration::CollaborationManager::for_route_fd_with_identity(
        stream.into_raw_fd(),
        "so.tilo.chat",
        room.parse().unwrap(),
        None,
        Some(hex::encode(reporter.device_public_key)),
        identity,
    )
    .unwrap()
}
#[test]
fn native_publisher_cold_reads_and_same_id_retry_use_the_original_broker_owner() {
    let (directory, owner, room, _, key) = native_workspace_owner::test_enrolled_owner();
    prepare_existing_chat(&owner, &key);
    let (network, commands) = fixture_network(&owner, &key);
    let (reporter, _) = network.feedback_custody(room).unwrap();
    let author = hex::encode(reporter.user_public_key);
    let delivery = Delivery::new(network.clone(), destination(), &author).unwrap();
    admit(&delivery);
    let admission = delivery.connection.borrow().current().unwrap();
    let runtime = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(directory.path())
        .unwrap();
    let socket = runtime.path().join("broker.sock");
    let broker = crate::shell::collab_broker::spawn_native_owner_harness_with_reads(
        socket.clone(),
        native_workspace_owner::tree_of(&owner),
        vec![reporter.device_public_key],
        &network,
        None,
    )
    .unwrap();
    let profile = directory.path().join("cold-publisher");
    assert!(!profile.exists());
    let mut apps = InProcessApps::at(&profile, &author).unwrap();
    let original_view = save_user_view(&apps);
    let mut publisher = publisher_on(
        &mut apps,
        &admission,
        certified_manager(&socket, &reporter, &key),
    )
    .unwrap();
    let mut preview = tilo_app::feedback::Draft {
        text: "The book failed to open".into(),
        context: vec![],
    }
    .preview()
    .unwrap();
    preview.approve();
    let submission = preview
        .submission("0123456789abcdef0123456789abcdef".into(), destination())
        .unwrap();
    publish_twice(&mut publisher, &network, &submission, room);
    let records = apps
        .owners
        .open("so.tilo.chat", &destination().room)
        .unwrap();
    assert!(
        records
            .read(&RecordId::package_primary(), |document| {
                chat::feedback::report(document, &submission.id)
                    .unwrap()
                    .is_some()
            })
            .unwrap()
    );
    publisher.close_components();
    assert!(
        publisher
            .component_close_reports()
            .iter()
            .all(|report| report.view_error.is_none()
                && report.record_error.is_none()
                && report.teardown_error.is_none())
    );
    assert_eq!(
        apps.store
            .read(&durable_store::StorageKey::view(
                &apps.device_id,
                &destination().room,
                1
            ))
            .unwrap()
            .unwrap()
            .bytes,
        original_view,
        "the model publisher must not replace the user's window presentation"
    );
    drop(records);
    drop(publisher);
    drop(apps);
    drop(delivery);
    drop(broker);
    drop(network);
    drop(commands);
    drop(owner);
    let path = directory.path().join("state.json");
    let mut tree = crate::shell::persist::load_tree_from(&path).unwrap();
    tree.persist_path = Some(path);
    let reopened = native_workspace_owner::open(tree.shared()).unwrap();
    assert_retained_once(&reopened, room, &submission);
}

fn assert_retained_once(
    reopened: &NativeWorkspaceOwner,
    room: crate::shell::state::RoomId,
    submission: &Submission,
) {
    assert!(
        reopened
            .with_current(room, |view| {
                let document = automerge::AutoCommit::load(
                    &view
                        .peer()
                        .document("so.tilo.chat", "")
                        .ok_or("retained Chat absent")?,
                )
                .map_err(|error| error.to_string())?;
                assert_eq!(
                    chat::projection::read(&document)
                        .message_rows("feedback")
                        .iter()
                        .filter(|message| message.id == format!("m_feedback_{}", submission.id))
                        .count(),
                    1,
                    "same-ID retry must present exactly one retained canonical message"
                );
                chat::feedback::report(&document, &submission.id)
                    .map(|report| report.is_some())
                    .map_err(|error| error.to_string())
            })
            .unwrap(),
        "the registered action's canonical report survives native owner restart"
    );
}

fn publish_twice(
    publisher: &mut ui::App,
    network: &Arc<ShellNetwork>,
    submission: &Submission,
    room: crate::shell::state::RoomId,
) {
    use tilo_app::feedback::destination;
    let address = tilo_actions::ActionAddress::new(
        destination::item(destination::CHANNELS, "feedback"),
        destination::SUBMIT,
    );
    let arguments = serde_json::json!({"submission":submission});
    let deadline = std::time::Instant::now()
        .checked_add(tilo_test_env::hang_bound())
        .unwrap();
    for attempt in 0..2 {
        publisher.pump_hosted_model();
        publisher
            .invoke_local_resource_action(&address, arguments.as_object().unwrap())
            .unwrap();
        loop {
            publisher.pump_hosted_model();
            let retained = network
                .native_owner()
                .unwrap()
                .with_current(room, |view| {
                    let document = automerge::AutoCommit::load(
                        &view
                            .peer()
                            .document("so.tilo.chat", "")
                            .ok_or("Chat missing")?,
                    )
                    .map_err(|error| error.to_string())?;
                    chat::feedback::report(&document, &submission.id)
                        .map(|report| report.is_some())
                        .map_err(|error| error.to_string())
                })
                .unwrap();
            if retained {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "native publication attempt {attempt} never reached the original owner"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn save_user_view(apps: &InProcessApps) -> Vec<u8> {
    let views =
        InstanceViewStore::with_store(apps.store.clone(), &apps.device_id, &destination().room)
            .unwrap();
    views
        .save(&ui::application_view::SavedInstanceView {
            version: 2,
            app_id: "so.tilo.chat".into(),
            device_id: apps.device_id.clone(),
            workspace_id: destination().room,
            instance_id: 1,
            record_id: RecordId::package_primary().as_str().into(),
            record_kind: chat::definition().record_kind.as_str().into(),
            view: ui::view::View::default(),
            application: None,
        })
        .unwrap();
    apps.store
        .read(&durable_store::StorageKey::view(
            &apps.device_id,
            &destination().room,
            1,
        ))
        .unwrap()
        .unwrap()
        .bytes
}
