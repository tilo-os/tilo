//! Machines the person planned to install, from making the plan through the
//! session's own action API to the machine finishing its installation: one
//! person with no server founds their Servers workspace by planning a machine,
//! the token names an enrollment endpoint of theirs, the machine is taken into
//! its entry, reads its plan, asks about its disk, another device answers, and
//! it finishes.

#![allow(
    clippy::significant_drop_tightening,
    reason = "hosts and fixtures deliberately stay alive until the test ends"
)]

use tilo_actions::remote::{InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_server::ServerClient;
use tilo_server::servers::{self, Servers, person_workspace_id};
use tilo_server_protocol::WorkSetting;
use tilo_user_identity::{DeviceCapabilities, Dial, RevocationList};

use crate::common::{Hosted, TestDevice, TestUser, device_endpoint, serve_in};

/// The provider and resource a server answers its own operations on.
const HOST_PROVIDER: &str = "tilo.host";
const HOST_RESOURCE: &str = "host.workspaces";

/// `tilo machine`'s verbs, as the command spells them.
const PLAN_MACHINE: &str = "machine.plan";
const ADMIT_MACHINE: &str = "machine.admit";

/// A host on `device`'s own endpoint that keeps the workspaces of people's
/// own, as a device's own workspace service does.
async fn own_host(user: &TestUser, device: &TestDevice) -> Hosted {
    serve_in(
        user,
        device,
        WorkSetting::none(),
        tempfile::tempdir().unwrap(),
    )
    .await
}

/// A session of `device` to its own `host`, as `tilo do` speaks to it.
async fn own_session(user: &TestUser, device: &TestDevice, hosted: &Hosted) -> ServerClient {
    ServerClient::connect_v2_to_own_device(
        device_endpoint(device).await,
        hosted.host.node_addr(),
        user.user_pk,
        hosted.host.device_cert().device_public_key,
        device.cert.clone(),
        device.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap()
}

/// The host action `action` on `session`, and the value it answered.
async fn host_action<T: serde::de::DeserializeOwned>(
    session: &ServerClient,
    action: &str,
    argument: serde_json::Value,
) -> T {
    let serde_json::Value::Object(argument) = argument else {
        panic!("arguments are an object");
    };
    let invoke = InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: HOST_PROVIDER.into(),
            address: ActionAddress::new(HOST_RESOURCE.to_owned(), action),
        },
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    };
    let status = tilo_test_env::within(
        "the host answers its own action",
        tilo_server::workspaces::ask_host(session, invoke),
    )
    .await
    .expect("the host answers");
    let ReceiptStatus::Completed { output } = status else {
        panic!("the host's own actions complete: {status:?}");
    };
    serde_json::from_value(output.value.unwrap_or_default()).expect("the answer reads")
}

/// The entry of the machine `key` as `host` reads it, once `done` holds for it:
/// the document reaches a holder over the replication, a moment after it is
/// written elsewhere.
async fn entry_where(
    host: &Hosted,
    key: &str,
    mut done: impl FnMut(&servers::Server) -> bool,
) -> servers::Server {
    tilo_test_env::within("the entry to reach the other device", async {
        loop {
            let found = servers::read(host.host.workspaces())
                .await
                .unwrap()
                .servers
                .get(&tilo_resource::Id::from_raw(key.to_owned()))
                .cloned();
            if let Some(found) = found
                && done(&found)
            {
                return found;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one test walks a machine's whole installation from the plan to its finish"
)]
async fn a_person_with_no_server_plans_a_machine_and_walks_it_through_its_installation() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let phone = alice.enrol("phone", DeviceCapabilities::member());
    let tablet = alice.enrol("tablet", DeviceCapabilities::member());
    let laptop_host = own_host(&alice, &laptop).await;
    // The command is typed at one of their devices, reaching the device that
    // holds their Servers workspace.
    let session = own_session(&alice, &phone, &laptop_host).await;
    let admin = person_workspace_id(&alice.user_pk);
    assert!(
        !servers::holds_person_workspace(laptop_host.host.workspaces()).await,
        "the person has no Servers workspace yet"
    );

    // `tilo machine add`: the choices make a plan document, and the session's
    // own action API writes it into the person's Servers workspace, making
    // that workspace first.
    // The machine's own key, as the image hands it one before it is certified.
    let machine_key = hex::encode([0x33_u8; 32]);
    // The token names an enrollment endpoint of theirs: the one derived from
    // the device that made it, where its session answers machines.
    let dial = Dial {
        endpoint: tilo_user_identity::enrollment_endpoint(&laptop.device_pubkey()),
        relay_url: String::new(),
    };
    let token = tilo_identity::enrollment::mint_token(
        alice.user_pk,
        DeviceCapabilities::member(),
        tilo_clock::unix_now_ns(),
        tilo_clock::unix_now_ns() + 600 * 1_000_000_000,
        std::slice::from_ref(&dial),
        &laptop.sk,
    )
    .unwrap();
    let decoded = tilo_user_identity::decode_enrollment_token(&token).unwrap();
    let token_nonce = decoded.nonce;
    assert!(decoded.dial.iter().any(|dial| {
        dial.endpoint == tilo_user_identity::enrollment_endpoint(&laptop.device_pubkey())
    }));

    let token_file = tempfile::tempdir().unwrap().path().join("cupboard.token");
    let plan = install_plan::Choices {
        destination: "tilo-server".to_owned(),
        name: Some("cupboard".to_owned()),
        network: Some(install_profile_model::plan::Network {
            addressing: "dhcp".to_owned(),
            ..install_profile_model::plan::Network::default()
        }),
        disk: Some(install_profile_model::plan::DiskRule {
            kind: "empty".to_owned(),
            identity: String::new(),
        }),
        unlock: Some("tpm".to_owned()),
        updates: Some("stable".to_owned()),
        hosting: Some(Vec::new()),
    }
    .plan(
        "0123456789abcdef0123456789abcdef",
        install_profile_model::plan::Token {
            nonce: hex::encode(token_nonce),
            expires_at_ns: decoded.expires_at_ns,
        },
    )
    .unwrap();
    let said: String = host_action(
        &session,
        PLAN_MACHINE,
        serde_json::json!({
            "plan": serde_json::to_string(&plan).unwrap(),
            "label": "cupboard",
            "token_file": token_file.display().to_string(),
            "installer_url": install_plan::INSTALLER_URL,
        }),
    )
    .await;
    assert!(
        said.contains("plan 0123456789abcdef0123456789abcdef"),
        "{said}"
    );
    assert!(said.contains("token "), "the token file is named: {said}");
    tilo_test_env::within("the Servers workspace to be founded", async {
        while !servers::holds_person_workspace(laptop_host.host.workspaces()).await {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    let planned = servers::read(laptop_host.host.workspaces()).await.unwrap();
    assert_eq!(planned.waiting().count(), 1, "the machine waits to install");

    // The machine enrolled with the token and the person approved it: their
    // device takes it into its entry under its device key. The holders hand it
    // the Servers workspace from there.
    let _: serde_json::Value = host_action(
        &session,
        ADMIT_MACHINE,
        serde_json::json!({ "device": machine_key, "nonce": hex::encode(token_nonce) }),
    )
    .await;
    let admitted = servers::read(laptop_host.host.workspaces()).await.unwrap();
    let entry = admitted
        .servers
        .get(&tilo_resource::Id::from_raw(machine_key.clone()))
        .expect("the machine's entry is its own");
    assert_eq!(
        entry.plan.status,
        tilo_workspace_service::workspaces::servers::plan::INSTALLING
    );

    // Another of their devices holds the workspace too, and answers what the
    // machine asks there.
    let tablet_host = own_host(&alice, &tablet).await;
    servers::take_in(
        laptop_host.host.workspaces(),
        &tablet.cert,
        &tablet_host.host.node_addr(),
    )
    .await
    .unwrap();
    tilo_test_env::within("the other device to hold the Servers workspace", async {
        while !servers::holds_person_workspace(tablet_host.host.workspaces()).await {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;

    // The machine, holding the workspace now, reports the drives it really has
    // and asks which one: its entry is the plan it reads.
    let drives = vec![
        install_profile_model::plan::Drive {
            model: "QEMU HARDDISK".into(),
            identity: "serial-a".into(),
            bytes: 40_000_000_000,
            holds_data: false,
            boots: false,
        },
        install_profile_model::plan::Drive {
            model: "QEMU HARDDISK".into(),
            identity: "serial-b".into(),
            bytes: 40_000_000_000,
            holds_data: false,
            boots: false,
        },
    ];
    let entry = servers::read(laptop_host.host.workspaces())
        .await
        .unwrap()
        .servers
        .get(&tilo_resource::Id::from_raw(machine_key.clone()))
        .cloned()
        .unwrap();
    let report =
        tilo_workspace_service::workspaces::servers::machine::report(&entry, &drives).unwrap();
    let asked: std::collections::BTreeMap<String, String> =
        serde_json::from_str(&report.ask).unwrap();
    assert!(
        asked
            .get("disk")
            .is_some_and(|why| why.contains("2 drives")),
        "the machine asks about its disk: {asked:?}"
    );
    servers::apply(
        laptop_host.host.workspaces(),
        Servers::BEGIN_INSTALL.with((
            machine_key.clone(),
            hex::encode(token_nonce),
            report.drives,
            report.ask,
        )),
    )
    .await
    .unwrap();
    let entry = entry_where(&tablet_host, &machine_key, |entry| {
        entry.plan.question("disk").is_some()
    })
    .await;
    assert_eq!(
        tilo_workspace_service::workspaces::servers::machine::step(&entry, &drives).unwrap(),
        tilo_workspace_service::workspaces::servers::machine::Step::Wait(vec!["disk".to_owned()]),
        "it waits while the question is open"
    );

    // The other device answers in the entry.
    servers::apply(
        tablet_host.host.workspaces(),
        Servers::ANSWER_MACHINE_CHOICE.with((
            tilo_resource::Id::from_raw(machine_key.clone()),
            "disk".into(),
            "serial:serial-a".into(),
        )),
    )
    .await
    .unwrap();

    // The machine reads its plan back and installs with the answered choice,
    // then says it finished.
    let entry = entry_where(&laptop_host, &machine_key, |entry| {
        entry.plan.disk == "serial:serial-a" || entry.plan.ask.is_empty()
    })
    .await;
    let tilo_workspace_service::workspaces::servers::machine::Step::Install(installing) =
        tilo_workspace_service::workspaces::servers::machine::step(&entry, &drives).unwrap()
    else {
        panic!("an answered machine installs");
    };
    assert_eq!(installing.disk.encode(), "serial:serial-a");
    assert_eq!(installing.name, "cupboard");
    servers::apply(
        laptop_host.host.workspaces(),
        Servers::FINISH_INSTALL.with(tilo_resource::Id::from_raw(machine_key.clone())),
    )
    .await
    .unwrap();
    let entry = entry_where(&laptop_host, &machine_key, |entry| {
        entry.plan.status == tilo_workspace_service::workspaces::servers::plan::INSTALLED
    })
    .await;
    assert_eq!(
        tilo_workspace_service::workspaces::servers::machine::step(&entry, &drives).unwrap(),
        tilo_workspace_service::workspaces::servers::machine::Step::Finished
    );
    assert_eq!(admin, person_workspace_id(&alice.user_pk));
}

/// A person's own device approves a machine that enrolled with a plan's token,
/// and the machine finishes installing from the plan on its first start: it
/// reports the drives it really has, waits at its entry while the person
/// answers its disk, applies the configuration its entry asks, and says it
/// finished.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one test walks a machine's first start from the approval to the entry saying it installed"
)]
async fn a_machine_that_enrolled_with_a_plan_installs_from_it_on_its_first_start() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let cupboard = alice.enrol("cupboard", DeviceCapabilities::member());
    let laptop_host = own_host(&alice, &laptop).await;
    let machine_host = crate::common::serve_on(
        &alice,
        &cupboard,
        tilo_server_protocol::WorkSetting::own_person(),
    )
    .await;
    let machine_key = hex::encode(cupboard.device_pubkey());
    let nonce = hex::encode([0x5a_u8; 16]);

    // The person plans the machine: its entry names the token's nonce.
    servers::founded(laptop_host.host.workspaces())
        .await
        .unwrap();
    let plan = install_plan::Choices {
        destination: "tilo-server".to_owned(),
        name: Some("cupboard".to_owned()),
        network: Some(install_profile_model::plan::Network {
            addressing: "dhcp".to_owned(),
            ..install_profile_model::plan::Network::default()
        }),
        disk: Some(install_profile_model::plan::DiskRule {
            kind: "empty".to_owned(),
            identity: String::new(),
        }),
        unlock: Some("tpm".to_owned()),
        updates: Some("stable".to_owned()),
        hosting: Some(Vec::new()),
    }
    .plan(
        "0123456789abcdef0123456789abcdef",
        install_profile_model::plan::Token {
            nonce: nonce.clone(),
            expires_at_ns: u64::MAX / 2,
        },
    )
    .unwrap();
    servers::apply(
        laptop_host.host.workspaces(),
        Servers::PLAN_MACHINE.with((serde_json::to_string(&plan).unwrap(), "cupboard".into())),
    )
    .await
    .unwrap();

    // The machine enrolled with the token and the person approved it: its
    // entry is named by its own device key, and the workspace's holders hand
    // it the workspace to install from.
    servers::apply(
        laptop_host.host.workspaces(),
        Servers::BEGIN_INSTALL.with((
            machine_key.clone(),
            nonce.clone(),
            "[]".into(),
            String::new(),
        )),
    )
    .await
    .unwrap();
    servers::take_in(
        laptop_host.host.workspaces(),
        &cupboard.cert,
        &machine_host.host.node_addr(),
    )
    .await
    .unwrap();
    tilo_test_env::within("the machine to hold the Servers workspace", async {
        while !servers::holds_person_workspace(machine_host.host.workspaces()).await {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;

    // What the enumeration wrote: the plan it must finish installing.
    let state = tempfile::tempdir().unwrap();
    std::fs::write(
        state.path().join("install-plan.nonce"),
        format!("{nonce}\n"),
    )
    .unwrap();
    assert_eq!(
        tilo_server::servers::first_install::recorded_nonce(state.path()).as_deref(),
        Some(nonce.as_str())
    );

    // The machine's first start: it reports the two drives it has and asks
    // which to install on, since the plan's rule names no drive.
    let requests = tilo_server::servers::applied::Requests::at(state.path().join("policy.toml"));
    let drives = vec![
        install_profile_model::plan::Drive {
            model: "QEMU HARDDISK".into(),
            identity: "serial-a".into(),
            bytes: 40_000_000_000,
            holds_data: false,
            boots: false,
        },
        install_profile_model::plan::Drive {
            model: "QEMU HARDDISK".into(),
            identity: "serial-b".into(),
            bytes: 40_000_000_000,
            holds_data: false,
            boots: false,
        },
    ];
    let installing = {
        let workspaces = machine_host.host.workspaces().clone();
        let cert = cupboard.cert.clone();
        let nonce = nonce.clone();
        let requests = requests.clone();
        let drives = drives.clone();
        tokio::spawn(async move {
            tilo_server::servers::first_install::install(
                &workspaces,
                &cert,
                &nonce,
                &drives,
                &requests,
            )
            .await
        })
    };
    let entry = entry_where(&laptop_host, &machine_key, |entry| {
        entry.plan.question("disk").is_some()
    })
    .await;
    assert_eq!(
        tilo_workspace_service::workspaces::servers::machine::step(&entry, &drives).unwrap(),
        tilo_workspace_service::workspaces::servers::machine::Step::Wait(vec!["disk".to_owned()]),
        "the machine waits while its disk is open"
    );

    // The person answers the machine's disk, on their own device.
    servers::apply(
        laptop_host.host.workspaces(),
        Servers::ANSWER_MACHINE_CHOICE.with((
            tilo_resource::Id::from_raw(machine_key.clone()),
            "disk".into(),
            "serial:serial-a".into(),
        )),
    )
    .await
    .unwrap();

    // The machine applies the configuration its entry asks of it, through the
    // root helper, and says it finished once the helper applied it.
    let request = tilo_test_env::within("the machine to apply its configuration", async {
        loop {
            if let Ok(request) = std::fs::read_to_string(&requests.device.request) {
                return request;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    let revision = request
        .lines()
        .find_map(|line| line.strip_prefix("revision|"))
        .expect("the request names its revision")
        .to_owned();
    std::fs::write(
        requests.device.report.clone(),
        format!("revision\t{revision}\nstate\tapplied\ndetail\tnothing to change\n"),
    )
    .unwrap();
    tilo_test_env::within("the machine to finish installing", installing)
        .await
        .expect("the install task ends")
        .expect("the machine installed from its plan");

    let entry = entry_where(&laptop_host, &machine_key, |entry| {
        entry.plan.status == tilo_workspace_service::workspaces::servers::plan::INSTALLED
    })
    .await;
    assert_eq!(
        tilo_workspace_service::workspaces::servers::machine::step(&entry, &drives).unwrap(),
        tilo_workspace_service::workspaces::servers::machine::Step::Finished
    );
    assert_eq!(
        entry.plan.disk, "serial:serial-a",
        "the entry keeps the drive the person answered"
    );
}

/// A person whose only device is a desktop approves their first server: the
/// desktop holds their Servers workspace, and it hands that workspace to the
/// server it names there. Every holder of the workspace attaches the servers
/// the document names, so the desktop does it without a server of theirs
/// running anything, and the server reads its own entry there.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_desktop_alone_hands_its_first_server_the_servers_workspace() {
    let alice = TestUser::fresh("Alice");
    let desktop = alice.enrol("desktop", DeviceCapabilities::primary());
    let nas = alice.enrol("nas", DeviceCapabilities::member());
    let desktop_host = own_host(&alice, &desktop).await;
    let server_host = crate::common::serve_on(
        &alice,
        &nas,
        tilo_server_protocol::WorkSetting::own_person(),
    )
    .await;

    // The person's own device makes the workspace when it is first needed.
    servers::founded(desktop_host.host.workspaces())
        .await
        .unwrap();
    assert!(
        !servers::holds_person_workspace(server_host.host.workspaces()).await,
        "the server holds nothing yet"
    );

    // Their desktop enters the server among their servers, as approving it
    // does. No other holder of the workspace takes part here.
    servers::apply(
        desktop_host.host.workspaces(),
        Servers::ENROLL_SERVER.with((
            hex::encode(nas.device_pubkey()),
            "nas".into(),
            serde_json::to_string(&server_host.host.node_addr()).unwrap(),
        )),
    )
    .await
    .unwrap();

    // The desktop attaches it: the workspace names the server among its
    // holders, and the server holds a copy of it and reads its entry.
    tilo_test_env::within(
        "the server to hold the workspace and read its entry",
        async {
            loop {
                let held = servers::holds_person_workspace(server_host.host.workspaces()).await;
                let named = servers::read(server_host.host.workspaces())
                    .await
                    .is_ok_and(|servers| {
                        servers
                            .servers
                            .get(&tilo_resource::Id::from_raw(hex::encode(
                                nas.device_pubkey(),
                            )))
                            .is_some_and(|entry| entry.label == "nas")
                    });
                if held && named {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        },
    )
    .await;
    let admin = person_workspace_id(&alice.user_pk);
    let holders = desktop_host.host.workspaces().hosts(&admin).await.unwrap();
    assert_eq!(holders.len(), 2, "the desktop and the server hold it");
    assert!(
        holders
            .iter()
            .any(|hint| hint.host.device_public_key == nas.device_pubkey()),
        "the server is named among the workspace's holders"
    );
}

/// A device of the person without the person's workspace asks the device that
/// introduces it to take it in, over that device's action: it holds the
/// workspace from then on, and a resource written there reaches it. A device
/// of another person is not taken in.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_asks_its_introducer_into_its_persons_workspace() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let desktop = alice.enrol("desktop", DeviceCapabilities::member());
    let laptop_host = own_host(&alice, &laptop).await;
    let asking = own_host(&alice, &desktop).await;
    servers::founded(laptop_host.host.workspaces()).await.unwrap();
    servers::apply(
        laptop_host.host.workspaces(),
        Servers::ENROLL_SERVER.with((
            hex::encode(laptop.device_pubkey()),
            "laptop".into(),
            serde_json::to_string(&laptop_host.host.node_addr()).unwrap(),
        )),
    )
    .await
    .unwrap();
    assert!(!servers::holds_person_workspace(asking.host.workspaces()).await);

    // The asking device speaks from the endpoint its own service answers on,
    // where the laptop hands it the workspace.
    let session = ServerClient::connect_v2_to_own_device(
        asking.router.endpoint().clone(),
        laptop_host.host.node_addr(),
        alice.user_pk,
        laptop_host.host.device_cert().device_public_key,
        desktop.cert.clone(),
        desktop.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    let taken: serde_json::Value = host_action(
        &session,
        "person.take-in",
        serde_json::json!({
            "endpoint": serde_json::to_string(&asking.host.node_addr()).unwrap(),
        }),
    )
    .await;
    assert_eq!(taken["workspace"], person_workspace_id(&alice.user_pk));
    tilo_test_env::within("the asking device to hold the workspace", async {
        while !servers::holds_person_workspace(asking.host.workspaces()).await {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    entry_where(&asking, &hex::encode(laptop.device_pubkey()), |_| true).await;

    // Bob's device is no device of Alice's: her host takes it into nothing.
    let bob = TestUser::fresh("Bob");
    let intruder = bob.enrol("laptop", DeviceCapabilities::primary());
    let refused = servers::take_in(
        laptop_host.host.workspaces(),
        &intruder.cert,
        &asking.host.node_addr(),
    )
    .await;
    assert!(refused.is_err(), "another person's device is not taken in");
}
