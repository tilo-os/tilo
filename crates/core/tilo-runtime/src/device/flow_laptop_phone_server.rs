#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard; the server and the devices stay up \
              until it ends"
)]

//! Flow 3 of `docs/architecture/flows.md`, its last step: every device of the person's manages
//! every other through its daemon's `devices` actions. The server is a real Tilo host on this
//! machine's loopback; the laptop and the phone are daemons' device actions over their own
//! workspace services, each entered through its login as a daemon enters.

use std::sync::{Arc, Mutex};

use ed25519_dalek::SigningKey;
use serde_json::json;
use tilo_actions::remote::{DEVICE_SETTINGS, ResourceReads};
use tilo_device_identity::login::UPDATE;
use tilo_server_protocol::servers::person_workspace_id;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList};
use tilo_workspace_service::workspaces::person_workspace::{self, devices};

use super::ticket::{OnThisDevice, Tickets};
use super::ticket_tests::{ask_device, enrol, serve};
use super::workspaces::ServiceWorkspaces;

/// Write the grant `servers add` gives an administrator, where it writes it (the person's own
/// workspace, as the server holds it) and in this machine's own copy of that workspace, which its
/// link to the server would bring it: the two services keep separate copies in this harness.
async fn grant_server_controls(
    hosted: &tilo_workspace_service::workspaces::Workspaces,
    laptop: &Arc<ServiceWorkspaces>,
    person: &str,
    studio: &str,
    admin: &str,
) {
    let device = laptop.device_cert().device_public_key;
    let by = hex::encode(laptop.device_cert().user_public_key);
    hosted
        .grant_server_controls(studio, device)
        .await
        .expect("the server writes its administrators' controls");
    laptop
        .copies()
        .commit_own_change(
            person,
            tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::Member {
                change: tilo_action_policy::Change::Grant(
                    tilo_workspace_service::workspaces::server_controls::grant(
                        person, &device, admin, &by,
                    ),
                ),
            },
            None,
        )
        .await
        .expect("the machine's copy keeps the grant");
}

/// Take back an administrator's control grant, as the person does, from where
/// [`grant_server_controls`] wrote it.
async fn revoke_server_controls(
    hosted: &tilo_workspace_service::workspaces::Workspaces,
    laptop: &Arc<ServiceWorkspaces>,
    person: &str,
    server: &DeviceCert,
    admin: &str,
) {
    let id = tilo_workspace_service::workspaces::server_controls::grant_id(
        &laptop.device_cert().device_public_key,
        admin,
    );
    hosted
        .remove_workspace_grants(person, server, [1; 32], vec![id.clone()])
        .await
        .expect("the person takes the grant back");
    laptop
        .copies()
        .commit_own_change(
            person,
            tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::Member {
                change: tilo_action_policy::Change::Revoke(id),
            },
            None,
        )
        .await
        .expect("the machine's copy takes it back");
}

/// One of the person's devices as its daemon runs it: its workspace service, entered through its
/// login, and its device actions.
struct Daemon {
    own: Arc<ServiceWorkspaces>,
    tickets: Tickets,
    entered: tilo_session_client::device::Device,
    router: iroh::protocol::Router,
}

/// The daemon of the device `key`, `cert`, logged in to `workspace` on `server`, keeping what it
/// keeps under `dir`. `as_server` has its service act as the device, as a machine that serves
/// workspaces for its person does, so that `servers add` can hand it one.
async fn daemon(
    dir: &std::path::Path,
    (key, cert): (&SigningKey, &DeviceCert),
    (server, server_cert): (&tilo_server::daemon::Server, &DeviceCert),
    workspace: &str,
    as_server: bool,
) -> Daemon {
    let login = tilo_device_identity::login::Device {
        site: "https://tilo.test/".into(),
        host_origin: "https://eu1.tilo.test".into(),
        workspace: workspace.to_owned(),
        workspace_name: Some("Work".into()),
        agent: "tilo".into(),
        cert: cert.clone(),
        server_cert: server_cert.clone(),
        server_address: Some(tilo_server_protocol::web::ServerAddress {
            endpoint_addr: server.node_addr(),
            relay_url: None,
            web: None,
        }),
    };
    let server_address = tilo_api::daemon::enter::server_of(&login).await.unwrap();
    let own = if as_server {
        ServiceWorkspaces::open_for_test_as_server(dir, key, cert, &server_address).await
    } else {
        ServiceWorkspaces::open_for_test(dir, key, cert, &server_address).await
    }
    .expect("the device's own service opens");
    let entered = tilo_test_env::within(
        "the device enters its workspace",
        tilo_api::daemon::enter::connect(key, &login, Some(own.lent())),
    )
    .await
    .expect("the device enters");
    tilo_test_env::within(
        "the device's service keeps the workspace",
        own.hold(&entered),
    )
    .await;
    let actions = own.copies().action_service();
    let tickets = Tickets::new(
        (key.clone(), cert.clone()),
        own.endpoint(),
        Some(Arc::new(super::home::Home::new(
            Arc::clone(&own),
            cert.clone(),
            Some(login),
        ))),
        Arc::new(|| Ok(None)),
        None,
        Some(Arc::clone(&own)),
        tilo_device_identity::login::Place::at(
            dir.join("login"),
            tilo_device_identity::IdentityPaths::under(dir.join("keys")),
        ),
        actions.clone(),
    );
    // As the daemon does: what the person's other devices ask of this one.
    own.copies().provide(
        tilo_device_identity::login::PROVIDER,
        Arc::new(OnThisDevice(tickets.clone())),
    );
    let router = serve(&own, actions);
    Daemon {
        own,
        tickets,
        entered,
        router,
    }
}

/// Every device manages every other: from the phone, the laptop and the server are listed,
/// renamed and updated, and the laptop carries out its own update.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flow_every_device_manages_every_other() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let (server_key, server_cert) = enrol(&root, "the-server", DeviceCapabilities::member());
    let (laptop_key, laptop_cert) = enrol(&root, "laptop", DeviceCapabilities::primary());
    let (phone_key, phone_cert) = enrol(&root, "android-phone", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = tilo_server::daemon::Server::start_with_relay_mode(
        tilo_server::ServerConfig::loopback(state.path().join("server")),
        tilo_server::daemon::ServerIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: server_key,
            device_cert: server_cert.clone(),
            revocations: RevocationList::new(),
        },
        iroh::RelayMode::Disabled,
    )
    .await
    .unwrap();
    let hosted = server
        .workspaces()
        .expect("the server has workspaces")
        .clone();
    // The person's own workspace, which every device of theirs holds: the record of their
    // devices is there.
    let person =
        tilo_server_protocol::servers::person_workspace_id(&root.verifying_key().to_bytes());
    if hosted.summary(&person).await.is_err() {
        hosted
            .create_workspace("Servers", Some(&person))
            .await
            .unwrap();
    }
    for cert in [&laptop_cert, &phone_cert] {
        hosted
            .enter(&person, cert)
            .await
            .expect("a device of the person enters their own workspace");
    }
    let laptop = daemon(
        &state.path().join("laptop"),
        (&laptop_key, &laptop_cert),
        (&server, &server_cert),
        &person,
        false,
    )
    .await;
    let phone = daemon(
        &state.path().join("phone"),
        (&phone_key, &phone_cert),
        (&server, &server_cert),
        &person,
        false,
    )
    .await;
    let updates = Arc::new(Mutex::new(0_u32));
    laptop.tickets.update_with({
        let updates = Arc::clone(&updates);
        Arc::new(move || {
            *updates.lock().unwrap() += 1;
            Ok(())
        })
    });
    phone
        .tickets
        .reach_at(laptop_cert.device_public_key, laptop.own.endpoint().addr());

    // The record of the person's devices is kept in their own workspace, which the server holds
    // here, and its keys are the ones their root certified: each is written there by the device
    // that holds the workspace, with the one helper a due entry goes through, as the device that
    // keeps the root does at approve. A device that merely takes part in the workspace, as the
    // phone and the laptop do here, writes nothing of it.
    for cert in [&laptop_cert, &phone_cert, &server_cert] {
        devices::keep(&hosted, cert)
            .await
            .expect("the person's certified devices are in their record");
    }
    let laptop_key = hex::encode(laptop_cert.device_public_key);
    // Each device is called what the record says in every list of the person's, the phone's
    // included.
    for (key, name) in [
        (laptop_key.clone(), "Ada's laptop"),
        (hex::encode(server_cert.device_public_key), "Hallway server"),
    ] {
        person_workspace::apply(
            &hosted,
            devices::DEVICES,
            devices::Devices::NAME_DEVICE.with((key.clone(), name.to_owned())),
        )
        .await
        .unwrap_or_else(|error| panic!("the person's device {key} is named {name}: {error}"));
    }
    // Every list of the person's devices reads their record and joins who is here: the record the
    // workspace is kept with, read as the record API answers it, with the laptop present.
    let record = person_workspace::document(&hosted, devices::DEVICES)
        .await
        .expect("the record of the person's devices reads")
        .expect("the person's workspace keeps their devices");
    let mut held = tilo_actions::remote::HeldCopies::default();
    held.0.insert(devices::DEVICES.1.to_owned(), record);
    let read = held
        .readable(
            &tilo_actions::remote::Caller::local_system(),
            &[tilo_actions::remote::ResourceAsk {
                address: tilo_actions::remote::ResourceAddress {
                    workspace: person_workspace::workspace_id(&hosted),
                    app: devices::DEVICES.0.to_owned(),
                    record: devices::DEVICES.1.to_owned(),
                },
                joined: false,
            }],
        )
        .expect("the record reads as every reader is answered");
    let here = [tilo_actions::remote::Here {
        who: hex::encode(phone_cert.user_public_key),
        name: "Ada".into(),
        kind: "person".into(),
        works_in: None,
        devices: vec![laptop_key.clone()],
    }];
    let devices = tilo_server_protocol::devices::fleet(
        &phone_cert.user_public_key,
        &phone_cert.device_public_key,
        &tilo_server_protocol::devices::FleetRecords {
            devices: tilo_server_protocol::devices::record_value(&read),
            ..Default::default()
        },
        &here,
    );
    let shown = |key: &[u8; 32]| {
        devices
            .iter()
            .find(|listed| listed.key_hex == hex::encode(key))
            .unwrap_or_else(|| panic!("the list shows {}: {devices:?}", hex::encode(key)))
    };
    assert_eq!(
        shown(&laptop_cert.device_public_key).label,
        "Ada's laptop",
        "the phone's name for the laptop is what every list shows"
    );
    assert!(
        shown(&phone_cert.device_public_key).current,
        "the phone's list says which device is the phone"
    );
    assert!(
        shown(&laptop_cert.device_public_key).online
            && !shown(&server_cert.device_public_key).online,
        "a device is online when who is here is present on it: {devices:?}"
    );

    // From the phone, the laptop is updated: the phone's daemon asks the laptop's, which carries
    // it out itself.
    let managed = ask_device(&phone.tickets, UPDATE, json!({"device": laptop_key}))
        .await
        .unwrap_or_else(|error| panic!("the phone updates the laptop: {error}"));
    assert!(
        managed.get("updating").is_some(),
        "the phone's update of the laptop is carried out by the laptop: {managed}"
    );
    assert_eq!(
        managed["device"], laptop_key,
        "the laptop answers for itself: {managed}"
    );
    assert_eq!(
        *updates.lock().unwrap(),
        1,
        "the laptop started its own update once"
    );

    for daemon in [laptop, phone] {
        let _ = daemon.router.shutdown().await;
        daemon.entered.close();
    }
    server.shutdown().await;
}

/// Stand in for the machine's root helper (`tilo-server-control`): answer every request written
/// under `state_root` at the files the real helper uses, with a journal line, and note each one
/// as `verb service`.
fn root_helper(
    state_root: &std::path::Path,
    asked: Arc<Mutex<Vec<String>>>,
) -> tokio::task::JoinHandle<()> {
    let request = state_root.join(tilo_server::operations::CONTROL_REQUEST);
    let result = state_root.join(tilo_server::operations::CONTROL_RESULT);
    tokio::spawn(async move {
        let mut seen = String::new();
        loop {
            if let Ok(text) = std::fs::read_to_string(&request)
                && text != seen
            {
                seen.clone_from(&text);
                let field = |key: &str| {
                    text.lines()
                        .find_map(|line| line.strip_prefix(&format!("{key}=")))
                        .unwrap_or_default()
                        .to_owned()
                };
                asked
                    .lock()
                    .unwrap()
                    .push(format!("{} {}", field("verb"), field("service")));
                std::fs::write(
                    &result,
                    format!("{} 1\nNov 18 tilo-runtime: serving\n", field("id")),
                )
                .unwrap();
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
}

/// A fresh idempotency key for a governance request.
fn fresh_id() -> [u8; 32] {
    SigningKey::generate(&mut getrandom::rand_core::UnwrapErr(getrandom::SysRng))
        .verifying_key()
        .to_bytes()
}

/// A server's controls are its daemon's `server` actions: the person's other device restarts a
/// unit on it and reads its journal through the daemons' dispatch, an administrator of a
/// workspace it serves controls it too, and a member who is no administrator there, or someone
/// asking from a workspace they do not administer, is refused with a sentence they can read.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_persons_devices_and_administrators_control_a_server_and_nobody_else() {
    use tilo_actions::remote::{ActionCallError, ActionProvider as _, Caller, InvokeArgument};
    use tilo_server_protocol::transport::{ServerAnswer, ServerOperationResult};
    use tilo_server_protocol::workspaces::Role;
    use tilo_workspace_service::workspaces::{AddMemberRequest, MemberPlace};

    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let (bob, _) = tilo_user_identity::UserIdentity::generate("Bob", 1);
    let (carol, _) = tilo_user_identity::UserIdentity::generate("Carol", 1);
    let (server_key, server_cert) = enrol(&root, "the-server", DeviceCapabilities::member());
    let (laptop_key, laptop_cert) = enrol(&root, "laptop", DeviceCapabilities::primary());
    let (phone_key, phone_cert) = enrol(&root, "android-phone", DeviceCapabilities::member());
    let (_, bob_cert) = enrol(&bob, "Bob", DeviceCapabilities::primary());
    let (_, carol_cert) = enrol(&carol, "Carol", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let server = tilo_server::daemon::Server::start_with_relay_mode(
        tilo_server::ServerConfig::loopback(state.path().join("server")),
        tilo_server::daemon::ServerIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: server_key,
            device_cert: server_cert.clone(),
            revocations: RevocationList::new(),
        },
        iroh::RelayMode::Disabled,
    )
    .await
    .unwrap();
    let hosted = server
        .workspaces()
        .expect("the server has workspaces")
        .clone();
    let person =
        tilo_server_protocol::servers::person_workspace_id(&root.verifying_key().to_bytes());
    if hosted.summary(&person).await.is_err() {
        hosted
            .create_workspace("Servers", Some(&person))
            .await
            .unwrap();
    }
    for cert in [&laptop_cert, &phone_cert] {
        hosted
            .enter(&person, cert)
            .await
            .expect("a device of the person enters their own workspace");
    }
    // A workspace of the person's that the machine serves, where Bob edits and Carol
    // administers.
    let studio = hosted.create("Studio").await.unwrap().workspace;
    for (cert, place) in [
        (&bob_cert, MemberPlace::Role(Role::Editor)),
        (&carol_cert, MemberPlace::Administrator),
    ] {
        hosted
            .add_member(
                &studio,
                &server_cert,
                AddMemberRequest {
                    request_id: fresh_id(),
                    user_public_key: cert.user_public_key,
                    place,
                    name: Some(cert.device_label.clone()),
                },
            )
            .await
            .expect("the owner places the member");
    }
    let laptop = daemon(
        &state.path().join("laptop"),
        (&laptop_key, &laptop_cert),
        (&server, &server_cert),
        &person,
        false,
    )
    .await;
    let phone = daemon(
        &state.path().join("phone"),
        (&phone_key, &phone_cert),
        (&server, &server_cert),
        &person,
        false,
    )
    .await;
    // The laptop is the machine here: its controls govern the server's workspaces, and its
    // root helper answers where its state is kept.
    let machine = tempfile::tempdir().unwrap();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let helper = root_helper(machine.path(), Arc::clone(&asked));
    let controlled = laptop.tickets.clone().with_controls(Some(
        tilo_server::operations::Controls::new(hosted.clone(), machine.path().to_path_buf())
            .waiting(std::time::Duration::from_secs(10)),
    ));
    phone
        .tickets
        .reach_at(laptop_cert.device_public_key, laptop.own.endpoint().addr());
    let machine_key = hex::encode(laptop_cert.device_public_key);

    // From the phone: its daemon hands the call to the machine's, which carries it out.
    let restarted: ServerOperationResult = serde_json::from_value(
        super::ticket_tests::ask_on(
            &phone.tickets,
            (super::ticket::SERVER, "restart"),
            json!({"device": machine_key, "service": "tilo-runtime"}),
        )
        .await
        .unwrap_or_else(|error| panic!("the phone restarts a unit on its server: {error}")),
    )
    .unwrap();
    assert!(
        restarted.done && restarted.summary == "restarted tilo-runtime.service",
        "the machine says what it did: {restarted:?}"
    );
    let journal: ServerOperationResult = serde_json::from_value(
        super::ticket_tests::ask_on(
            &phone.tickets,
            (super::ticket::SERVER, "journal"),
            json!({"device": machine_key, "service": "tilo-runtime.service", "lines": 20}),
        )
        .await
        .unwrap_or_else(|error| panic!("the phone reads its server's journal: {error}")),
    )
    .unwrap();
    assert_eq!(
        journal.answer,
        ServerAnswer::Text("Nov 18 tilo-runtime: serving\n".into()),
        "the journal is what the machine read"
    );
    assert_eq!(
        *asked.lock().unwrap(),
        ["restart tilo-runtime.service", "logs tilo-runtime.service"],
        "the root helper was asked each, once"
    );

    // The laptop keeps its settings, and the machine's controls are decided at their own node in
    // its person's own workspace.
    let laptop_records = Arc::new(super::records::DeviceRecords::at(
        &state.path().join("laptop"),
        laptop_cert.device_public_key,
    ));
    laptop_records.bind(&laptop.own);
    laptop_records.carry_into(&root.verifying_key().to_bytes());
    laptop.own.hold_device_records(Arc::clone(&laptop_records));
    laptop.own.copies().publish_own(
        DEVICE_SETTINGS,
        tilo_workspace_service::workspaces::OwnReaders::OwnRecords,
        laptop_records.clone(),
    );

    // Other people, as the machine's action service hands their calls on to it.
    let on_the_machine = OnThisDevice(controlled);
    let caller_for = |cert: &DeviceCert, workspace: &str| {
        Caller::authenticated(cert.user_public_key, cert.device_public_key).in_workspace(workspace)
    };
    let ask_as = |cert: &DeviceCert, workspace: &str| {
        let caller = caller_for(cert, workspace);
        let request = tilo_actions::remote::InvokeRequest {
            on_behalf_of: None,
            invocation: tilo_actions::InvocationId::new(),
            target: tilo_actions::remote::RemoteActionTarget {
                provider: tilo_device_identity::login::PROVIDER.to_owned(),
                address: tilo_actions::ActionAddress::new(super::ticket::SERVER, "restart"),
            },
            argument: InvokeArgument::Json(serde_json::Map::from_iter([(
                "service".to_owned(),
                "tilo-runtime".into(),
            )])),
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        on_the_machine.invoke(&caller, request)
    };
    let refused = ActionCallError::Refused {
        reason: "Only the person this server belongs to, and the administrators of the \
                 workspaces it serves, control it."
            .into(),
    };
    let carol = hex::encode(carol_cert.user_public_key);
    let root_hex = hex::encode(root.verifying_key().to_bytes());
    let own = person_workspace_id(&root.verifying_key().to_bytes());

    // No one but the person's own is granted anything: an editor of the studio and an
    // administrator without a grant are both refused, in words.
    for (who, cert, workspace) in [
        ("Bob, who edits the studio", &bob_cert, &studio),
        (
            "Carol, who administers it without a grant",
            &carol_cert,
            &studio,
        ),
    ] {
        let answer = ask_as(cert, workspace).await.expect("the machine answers");
        assert_eq!(
            answer.err(),
            Some(refused.clone()),
            "{who} is refused, in words"
        );
    }

    // `servers add` gives Carol, an administrator of the studio, its grant: she controls the
    // machine, and reads that machine's settings, from her own devices.
    grant_server_controls(&hosted, &laptop.own, &own, &studio, &carol).await;
    let carols = ask_as(&carol_cert, &studio)
        .await
        .expect("the machine answers")
        .expect("the grant lets Carol control the machine");
    assert!(
        matches!(
            carols.status,
            tilo_actions::remote::ReceiptStatus::Completed { .. }
        ),
        "the machine restarted the unit for Carol: {carols:?}"
    );
    // The machine's own settings service decides each setting at its node, in the person's own
    // workspace as this device holds it.
    let settings = Arc::clone(&laptop_records);
    let address = |name: &str| tilo_actions::remote::ResourceAddress {
        workspace: own.clone(),
        app: DEVICE_SETTINGS.to_owned(),
        record: name.to_owned(),
    };
    let carol_from_studio = caller_for(&carol_cert, &studio);
    assert!(
        settings
            .read(&carol_from_studio, &address("server"))
            .is_ok(),
        "the grant reads the machine's controls' setting"
    );
    let other = settings
        .read(&carol_from_studio, &address("display"))
        .expect_err("the grant reaches one setting, not another");
    assert!(
        matches!(other, ActionCallError::Refused { .. }),
        "another setting is refused: {other:?}"
    );
    // The person's own other device reads every setting of theirs.
    let phone_person = caller_for(&phone_cert, &person);
    assert!(
        settings.read(&phone_person, &address("display")).is_ok(),
        "the person reads their machine's settings from another of their devices"
    );

    // A read-only grant changes no setting; the grant to control the machine does.
    let setting = |name: &str| {
        serde_json::Map::from_iter([
            ("name".to_owned(), json!(name)),
            ("value".to_owned(), json!("on")),
        ])
    };
    let bob = hex::encode(bob_cert.user_public_key);
    laptop
        .own
        .copies()
        .commit_own_change(
            &own,
            tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::Member {
                change: tilo_action_policy::Change::Grant(tilo_action_policy::Grant::new(
                    "bob-display",
                    tilo_action_policy::Grantee::person(bob.clone()),
                    tilo_workspace_service::workspaces::server_controls::settings_node(
                        &own,
                        &laptop_cert.device_public_key,
                        "display",
                    ),
                    tilo_action_policy::KindSet::READ,
                    tilo_action_policy::Answer::Allow,
                    tilo_action_policy::Grantee::person(root_hex.clone()),
                )),
            },
            None,
        )
        .await
        .expect("the person grants Bob one setting's reading");
    let bob_from_studio = caller_for(&bob_cert, &studio);
    assert!(
        laptop_records
            .act(&bob_from_studio, super::records::SET, &setting("display"))
            .is_err(),
        "a read-only grant changes no setting"
    );
    assert!(
        laptop_records
            .act(&carol_from_studio, super::records::SET, &setting("server"))
            .is_ok(),
        "the grant to control the machine changes its settings"
    );

    // The person's own device controls the machine too, without any grant.
    let person_answer = ask_as(&phone_cert, &person)
        .await
        .expect("the machine answers")
        .expect("the person controls their own machine from any device");
    assert!(
        matches!(
            person_answer.status,
            tilo_actions::remote::ReceiptStatus::Completed { .. }
        ),
        "the person's device restarted the unit: {person_answer:?}"
    );

    // The person takes the grant back: Carol is refused again.
    revoke_server_controls(&hosted, &laptop.own, &own, &server_cert, &carol).await;
    let answer = ask_as(&carol_cert, &studio)
        .await
        .expect("the machine answers");
    assert_eq!(
        answer.err(),
        Some(refused.clone()),
        "a revoked grant controls nothing"
    );

    // The server stops serving the studio: its administrators' controls go with it.
    grant_server_controls(&hosted, &laptop.own, &own, &studio, &carol).await;
    hosted
        .release_server_controls(laptop_cert.device_public_key)
        .await
        .expect("the server's controls are taken back");
    let answer = ask_as(&carol_cert, &studio)
        .await
        .expect("the machine answers");
    assert_eq!(
        answer.err(),
        Some(refused.clone()),
        "a server that serves the studio no more controls nothing for its administrators"
    );

    // Carol stops administering the studio: the controls she held go with that place.
    grant_server_controls(&hosted, &laptop.own, &own, &studio, &carol).await;
    hosted
        .remove_workspace_grants(
            &studio,
            &server_cert,
            [2; 32],
            vec![tilo_action_policy::roles::administrator_grant_id(&carol)],
        )
        .await
        .expect("the studio unmakes Carol an administrator");
    let answer = ask_as(&carol_cert, &studio)
        .await
        .expect("the machine answers");
    assert_eq!(
        answer.err(),
        Some(refused.clone()),
        "an administrator's controls go when they stop administering"
    );

    // An agent of the person's own is refused until the person grants it, and admitted then.
    let agent = Caller::delegated_agent("helper", root.verifying_key().to_bytes(), [33; 32])
        .in_workspace(studio.clone());
    let request = || tilo_actions::remote::InvokeRequest {
        on_behalf_of: None,
        invocation: tilo_actions::InvocationId::new(),
        target: tilo_actions::remote::RemoteActionTarget {
            provider: tilo_device_identity::login::PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(super::ticket::SERVER, "restart"),
        },
        argument: InvokeArgument::Json(serde_json::Map::from_iter([(
            "service".to_owned(),
            "tilo-runtime".into(),
        )])),
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let answer = on_the_machine
        .invoke(&agent, request())
        .await
        .expect("the machine answers");
    assert_eq!(
        answer.err(),
        Some(refused.clone()),
        "an ungranted agent is refused"
    );
    let agent_grant = tilo_action_policy::Grant::new(
        "person-agent",
        tilo_action_policy::Grantee::agent("helper"),
        tilo_workspace_service::workspaces::server_controls::settings_node(
            &own,
            &laptop_cert.device_public_key,
            tilo_workspace_service::workspaces::server_controls::CONTROL,
        ),
        tilo_action_policy::KindSet::READ_UNDOABLE,
        tilo_action_policy::Answer::Allow,
        tilo_action_policy::Grantee::person(root_hex.clone()),
    );
    hosted
        .put_workspace_grants(&own, &server_cert, [3; 32], vec![agent_grant])
        .await
        .expect("the person grants their agent the controls");
    on_the_machine
        .invoke(&agent, request())
        .await
        .expect("the machine answers")
        .expect("a granted agent controls the machine");

    helper.abort();
    for daemon in [laptop, phone] {
        let _ = daemon.router.shutdown().await;
        daemon.entered.close();
    }
    server.shutdown().await;
}

/// A server that stops serving a workspace over the real network path no longer controls that
/// workspace's machine for its administrators, and the workspace refuses the removed server's
/// later writes into it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn detaching_a_server_revokes_its_administrators_and_refuses_its_writes() {
    use tilo_actions::remote::{ActionCallError, ActionProvider as _, Caller, InvokeArgument};
    use tilo_workspace_service::workspaces::{AddMemberRequest, MemberPlace};

    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let (carol, _) = tilo_user_identity::UserIdentity::generate("Carol", 1);
    let (server_key, server_cert) = enrol(&root, "the-server", DeviceCapabilities::member());
    let (laptop_key, laptop_cert) = enrol(&root, "laptop", DeviceCapabilities::primary());
    let (_, carol_cert) = enrol(&carol, "Carol", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let server = tilo_server::daemon::Server::start_with_relay_mode(
        tilo_server::ServerConfig::loopback(state.path().join("server")),
        tilo_server::daemon::ServerIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: server_key,
            device_cert: server_cert.clone(),
            revocations: RevocationList::new(),
        },
        iroh::RelayMode::Disabled,
    )
    .await
    .unwrap();
    let hosted = server
        .workspaces()
        .expect("the server has workspaces")
        .clone();
    let person = person_workspace_id(&root.verifying_key().to_bytes());
    if hosted.summary(&person).await.is_err() {
        hosted
            .create_workspace("Servers", Some(&person))
            .await
            .unwrap();
    }
    hosted
        .enter(&person, &laptop_cert)
        .await
        .expect("a device of the person enters their own workspace");
    let studio = hosted.create("Studio").await.unwrap().workspace;
    hosted
        .add_member(
            &studio,
            &server_cert,
            AddMemberRequest {
                request_id: fresh_id(),
                user_public_key: carol_cert.user_public_key,
                place: MemberPlace::Administrator,
                name: Some(carol_cert.device_label.clone()),
            },
        )
        .await
        .expect("the owner places Carol");
    let laptop = daemon(
        &state.path().join("laptop"),
        (&laptop_key, &laptop_cert),
        (&server, &server_cert),
        &person,
        true,
    )
    .await;

    // The laptop's own line, as its operator hands it over: what `servers add` names.
    let line = serde_json::to_string(&tilo_server_protocol::web::PageHost {
        server_cert: laptop_cert.clone(),
        address: Some(tilo_server_protocol::web::PageHostAddress {
            endpoint_addr: laptop.own.endpoint().addr(),
            relay_url: None,
            web: None,
        }),
        origin: None,
    })
    .expect("a host line writes as JSON");
    hosted
        .add_server(&studio, &line, tilo_peer_state::CopyRole::KeyHolder)
        .await
        .expect("the server hands the studio to the laptop");
    assert!(
        hosted
            .keeping_devices(&studio)
            .await
            .expect("the studio's keeping devices list")
            .iter()
            .any(|holder| holder.device == hex::encode(laptop_cert.device_public_key)),
        "the laptop keeps the studio"
    );
    // `servers add` gave the studio's administrator her grant on the machine, in the machine's
    // person's own workspace: the grant Carol's control below is decided by.
    let carol_hex = hex::encode(carol_cert.user_public_key);
    assert!(
        hosted
            .policy_of(&person)
            .await
            .is_some_and(|policy| policy.grants.iter().any(|grant| grant.id
                == tilo_workspace_service::workspaces::server_controls::grant_id(
                    &laptop_cert.device_public_key,
                    &carol_hex,
                ))),
        "servers add wrote Carol's grant on the laptop"
    );

    // The machine's controls are the laptop's daemon's, which governs the studio as it is
    // handed over.
    let machine = tempfile::tempdir().unwrap();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let helper = root_helper(machine.path(), Arc::clone(&asked));
    let on_the_machine = OnThisDevice(
        laptop.tickets.clone().with_controls(Some(
            tilo_server::operations::Controls::new(hosted.clone(), machine.path().to_path_buf())
                .waiting(std::time::Duration::from_secs(10)),
        )),
    );
    let caller = Caller::authenticated(carol_cert.user_public_key, carol_cert.device_public_key)
        .in_workspace(studio.clone());
    let restart = || tilo_actions::remote::InvokeRequest {
        on_behalf_of: None,
        invocation: tilo_actions::InvocationId::new(),
        target: tilo_actions::remote::RemoteActionTarget {
            provider: tilo_device_identity::login::PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(super::ticket::SERVER, "restart"),
        },
        argument: InvokeArgument::Json(serde_json::Map::from_iter([(
            "service".to_owned(),
            "tilo-runtime".into(),
        )])),
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let allowed = on_the_machine
        .invoke(&caller, restart())
        .await
        .expect("the machine answers")
        .expect("servers add gives Carol, an administrator, the machine's controls");
    assert!(
        matches!(
            allowed.status,
            tilo_actions::remote::ReceiptStatus::Completed { .. }
        ),
        "the machine restarted the unit for Carol: {allowed:?}"
    );

    // A keeping device may say where its copy stands while it serves the workspace. The row is
    // what a server's own writes into the workspace are; it carries a value the workspace does
    // not hold yet, so the write is the row's and not a matching repeat.
    let held = tilo_peer_state::HeldCopy {
        catching_up_from: [server_cert.device_public_key].into(),
        ..tilo_peer_state::HeldCopy::default()
    };
    let says_where_it_stands = |state: &mut tilo_peer_state::StatePeer| {
        state
            .workspace_mut()
            .report_copy(&laptop_cert, &held, std::time::SystemTime::now())
    };
    let mut governed = hosted
        .state(&studio)
        .await
        .expect("the studio is governed here");
    assert!(
        says_where_it_stands(&mut governed).is_ok(),
        "a server named as keeping the studio writes its copy's row"
    );

    // `servers remove` over the real path: the laptop is reached and told to let the studio go,
    // and it stops being named among the devices keeping it.
    let removed = hosted
        .remove_server(&studio, &hex::encode(laptop_cert.device_public_key))
        .await
        .expect("the server stops serving the studio");
    assert!(removed, "the laptop said it let the studio go");
    assert!(
        !hosted
            .keeping_devices(&studio)
            .await
            .expect("the studio's keeping devices list")
            .iter()
            .any(|holder| holder.device == hex::encode(laptop_cert.device_public_key)),
        "the studio no longer names the laptop"
    );

    // The grant `servers add` wrote is taken back with the serving: Carol's next control is
    // refused, in words.
    assert!(
        !hosted
            .policy_of(&person)
            .await
            .is_some_and(|policy| policy.grants.iter().any(|grant| grant.id
                == tilo_workspace_service::workspaces::server_controls::grant_id(
                    &laptop_cert.device_public_key,
                    &carol_hex,
                ))),
        "servers remove took Carol's grant on the laptop back"
    );
    let refused = ActionCallError::Refused {
        reason: "Only the person this server belongs to, and the administrators of the \
                 workspaces it serves, control it."
            .into(),
    };
    let answer = on_the_machine
        .invoke(&caller, restart())
        .await
        .expect("the machine answers");
    assert_eq!(
        answer.err(),
        Some(refused),
        "a server that serves the studio no more controls nothing for its administrators"
    );

    // The removed server's later write into the studio is refused: it left the workspace's
    // roster with the serving.
    let mut governed = hosted
        .state(&studio)
        .await
        .expect("the studio is governed here");
    let later = says_where_it_stands(&mut governed)
        .expect_err("the studio refuses the removed server's write");
    assert!(
        later.contains("absent from governed roster"),
        "the studio no longer holds the removed server as one of its devices: {later}"
    );

    helper.abort();
    let _ = laptop.router.shutdown().await;
    laptop.entered.close();
    server.shutdown().await;
}

/// A server of the workspace's owner that stops serving a workspace writes nothing into it again,
/// on the certificate the workspace still holds: the roster is what makes a device a writer, and
/// the owner's other devices still write.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_removed_owners_server_writes_nothing_into_the_workspace() {
    use tilo_peer_state::TransportOrigin;

    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let (server_key, server_cert) = enrol(&root, "the-server", DeviceCapabilities::member());
    let (laptop_key, laptop_cert) = enrol(&root, "laptop", DeviceCapabilities::primary());
    let (_, phone_cert) = enrol(&root, "android-phone", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = tilo_server::daemon::Server::start_with_relay_mode(
        tilo_server::ServerConfig::loopback(state.path().join("server")),
        tilo_server::daemon::ServerIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: server_key,
            device_cert: server_cert.clone(),
            revocations: RevocationList::new(),
        },
        iroh::RelayMode::Disabled,
    )
    .await
    .unwrap();
    let hosted = server
        .workspaces()
        .expect("the server has workspaces")
        .clone();
    let person = person_workspace_id(&root.verifying_key().to_bytes());
    if hosted.summary(&person).await.is_err() {
        hosted
            .create_workspace("Servers", Some(&person))
            .await
            .unwrap();
    }
    let studio = hosted.create("Studio").await.unwrap().workspace;
    let laptop = daemon(
        &state.path().join("laptop"),
        (&laptop_key, &laptop_cert),
        (&server, &server_cert),
        &person,
        true,
    )
    .await;
    let line = serde_json::to_string(&tilo_server_protocol::web::PageHost {
        server_cert: laptop_cert.clone(),
        address: Some(tilo_server_protocol::web::PageHostAddress {
            endpoint_addr: laptop.own.endpoint().addr(),
            relay_url: None,
            web: None,
        }),
        origin: None,
    })
    .expect("a host line writes as JSON");
    hosted
        .add_server(&studio, &line, tilo_peer_state::CopyRole::KeyHolder)
        .await
        .expect("the server hands the studio to the laptop");
    // The copy the laptop kept while it served the studio, which it holds on to once it is told to
    // let the studio go: a server that stops serving keeps what it kept.
    let mut away = laptop
        .own
        .peer(tilo_desktop_model::WorkspaceId::parse(&studio).expect("the studio's id"))
        .expect("the laptop keeps the studio");

    let removed = hosted
        .remove_server(&studio, &hex::encode(laptop_cert.device_public_key))
        .await
        .expect("the server stops serving the studio");
    assert!(removed, "the laptop said it let the studio go");

    // It writes where its copy stands, and sends what it wrote.
    let later = tilo_peer_state::HeldCopy {
        catching_up_from: [7; 32].into(),
        ..tilo_peer_state::HeldCopy::default()
    };
    assert!(
        away.workspace_mut()
            .report_copy(&laptop_cert, &later, std::time::SystemTime::now())
            .is_ok(),
        "the copy the removed server kept still takes its write"
    );
    let mut governed = hosted
        .state(&studio)
        .await
        .expect("the studio is governed here");
    let refused = governed
        .receive_from(
            TransportOrigin::ServerDelta(laptop_cert.device_public_key),
            away.workspace_message(),
            std::time::SystemTime::now(),
        )
        .expect_err("the studio refuses the removed server's write, however its certificate reads");
    assert!(
        refused.to_string().contains("absent from governed roster"),
        "the removed server is no device of the studio: {refused}"
    );

    // The owner's other devices write: a device of theirs enters the studio and says where its
    // copy stands.
    hosted
        .enter(&studio, &phone_cert)
        .await
        .expect("a device of the owner enters the studio");
    let mut governed = hosted
        .state(&studio)
        .await
        .expect("the studio is governed here");
    let phone = tilo_peer_state::HeldCopy {
        catching_up_from: [9; 32].into(),
        ..tilo_peer_state::HeldCopy::default()
    };
    assert!(
        governed
            .workspace_mut()
            .report_copy(&phone_cert, &phone, std::time::SystemTime::now())
            .is_ok(),
        "the owner's other device still writes the studio"
    );

    let _ = laptop.router.shutdown().await;
    laptop.entered.close();
    server.shutdown().await;
}

