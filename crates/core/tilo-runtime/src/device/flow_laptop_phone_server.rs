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
use tilo_device_identity::login::{RENAME, UPDATE};
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
        Some((key.clone(), login)),
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

    // The record of the person's devices, as the devices their root certified keep themselves in
    // it: nobody is asked to allow their own device's change to it.
    for cert in [&laptop_cert, &phone_cert, &server_cert] {
        person_workspace::apply(
            phone.own.copies(),
            devices::DEVICES,
            devices::Devices::keeping(cert).unwrap(),
        )
        .await
        .expect("the phone keeps the person's device in their record, unasked");
    }

    // The phone lists the laptop and the server and renames them.
    let laptop_key = hex::encode(laptop_cert.device_public_key);
    for (key, name) in [
        (laptop_key.clone(), "Ada's laptop"),
        (hex::encode(server_cert.device_public_key), "Hallway server"),
    ] {
        ask_device(&phone.tickets, RENAME, json!({"device": key, "name": name}))
            .await
            .unwrap_or_else(|error| panic!("the phone names the device {key}: {error}"));
    }
    // Every list of the person's devices reads their record and joins who is here: the phone's
    // copy, read as the record API answers it, with the laptop present.
    let record = person_workspace::document(phone.own.copies(), devices::DEVICES)
        .await
        .expect("the phone reads the person's devices")
        .expect("the phone holds the person's devices");
    let mut held = tilo_actions::remote::HeldCopies::default();
    held.0.insert(devices::DEVICES.1.to_owned(), record);
    let read = held
        .readable(
            &tilo_actions::remote::Caller::local_system(),
            &[tilo_actions::remote::ResourceAsk {
                address: tilo_actions::remote::ResourceAddress {
                    workspace: person_workspace::workspace_id(phone.own.copies()),
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
