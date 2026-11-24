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
use tilo_device_identity::login::UPDATE;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList};
use tilo_workspace_service::workspaces::person_workspace::{self, devices};

use super::ticket::{OnThisDevice, Tickets};
use super::ticket_tests::{ask_device, enrol, serve};
use super::workspaces::ServiceWorkspaces;

/// One of the person's devices as its daemon runs it: its workspace service, entered through its
/// login, and its device actions.
struct Daemon {
    own: Arc<ServiceWorkspaces>,
    tickets: Tickets,
    entered: tilo_session_client::device::Device,
    router: iroh::protocol::Router,
}

/// The daemon of the device `key`, `cert`, logged in to `workspace` on `server`, keeping what it
/// keeps under `dir`.
async fn daemon(
    dir: &std::path::Path,
    (key, cert): (&SigningKey, &DeviceCert),
    (server, server_cert): (&tilo_server::daemon::Server, &DeviceCert),
    workspace: &str,
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
    let own = ServiceWorkspaces::open_for_test(
        dir,
        key,
        cert,
        &tilo_api::daemon::enter::server_of(&login).await.unwrap(),
    )
    .await
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
    )
    .await;
    let phone = daemon(
        &state.path().join("phone"),
        (&phone_key, &phone_cert),
        (&server, &server_cert),
        &person,
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
    )
    .await;
    let phone = daemon(
        &state.path().join("phone"),
        (&phone_key, &phone_cert),
        (&server, &server_cert),
        &person,
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

    // Other people, as the machine's action service hands their calls on to it.
    let on_the_machine = OnThisDevice(controlled);
    let ask_as = |cert: &DeviceCert, workspace: &str| {
        let caller = Caller::authenticated(cert.user_public_key, cert.device_public_key)
            .in_workspace(workspace);
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
    for (who, cert, workspace) in [
        ("Bob, who edits the studio", &bob_cert, &studio),
        (
            "Carol, asking where she administers nothing",
            &carol_cert,
            &person,
        ),
    ] {
        let answer = ask_as(cert, workspace).await.expect("the machine answers");
        assert_eq!(
            answer.err(),
            Some(refused.clone()),
            "{who} is refused, in words"
        );
    }
    let carols = ask_as(&carol_cert, &studio)
        .await
        .expect("the machine answers")
        .expect("Carol administers a workspace this machine serves");
    assert!(
        matches!(
            carols.status,
            tilo_actions::remote::ReceiptStatus::Completed { .. }
        ),
        "the machine restarted the unit for Carol: {carols:?}"
    );
    assert_eq!(
        asked.lock().unwrap().len(),
        3,
        "only the administrator's call reached the root helper"
    );

    helper.abort();
    for daemon in [laptop, phone] {
        let _ = daemon.router.shutdown().await;
        daemon.entered.close();
    }
    server.shutdown().await;
}
