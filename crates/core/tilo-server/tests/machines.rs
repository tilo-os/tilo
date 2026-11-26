//! Machines the person planned to install, from making the plan through the
//! host's own action API to the machine finishing its installation: the
//! device that made a person's root founds their workspace and plans a machine
//! in it, the plan names the ticket's nonce, the machine is taken into
//! its entry, reads its plan, asks about its disk, another device answers, and
//! it finishes.

#![allow(
    clippy::significant_drop_tightening,
    reason = "hosts and fixtures deliberately stay alive until the test ends"
)]

use std::sync::Arc;

use tilo_actions::remote::{
    InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionMessage, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_server::servers::{self, Servers, person_workspace_id};
use tilo_server_protocol::{WorkSetting, WorkspaceId};
use tilo_user_identity::DeviceCapabilities;
use tilo_workspace_service::workspaces::MemberDevice;

use crate::common::{Hosted, Linked, TestDevice, TestUser, join_person_workspace, serve_in};

/// The provider and resource a server answers its own operations on.
const HOST_PROVIDER: &str = "tilo.host";
const HOST_RESOURCE: &str = "host.workspaces";

/// `tilo machine`'s verbs, as the command spells them.
const PLAN_MACHINE: &str = "machine.plan";
const ADMIT_MACHINE: &str = "machine.admit";
const LIST_SERVERS: &str = "machine.servers";
const KEEP_RECOVERY_KEY: &str = "machine.keep-recovery-key";
const RECOVERY_KEYS: &str = "machine.recovery-keys";

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

/// [`own_host`], whose service also keeps workspaces `device` is a member of
/// that another device keeps, as a device's own service does.
async fn member_host(user: &TestUser, device: &TestDevice) -> Hosted {
    let hosted = own_host(user, device).await;
    hosted
        .host
        .workspaces()
        .set_member_device(Arc::new(MemberDevice {
            cert: device.cert.clone(),
            signer: Arc::new(device.sk.clone()),
        }));
    hosted
}

/// `device`'s link to the person's `host`, in their Servers workspace: how a
/// device of theirs asks the host its own operations.
async fn own_link(device: &TestDevice, hosted: &Hosted, workspace: WorkspaceId) -> Linked {
    Linked::open_hosted(hosted, device, workspace)
        .await
        .expect("the host admits the person's device")
}

/// Take `device` into the person's Servers workspace `workspace`, as one of
/// their own devices comes to hold it.
async fn take_in(hosted: &Hosted, workspace: &str, device: &TestDevice) {
    hosted
        .host
        .workspaces()
        .enter(workspace, (&device.cert, &device.sk))
        .await
        .expect("the workspace takes the person's device in");
}

/// The host's answer to `action` with `argument`, asked over `linked`.
async fn host_answer(
    linked: &Linked,
    action: &str,
    argument: serde_json::Value,
) -> RemoteActionMessage {
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
        signed: None,
    };
    tilo_test_env::within(
        "the host answers its own action",
        linked
            .actions
            .ask_message(RemoteActionMessage::Invoke(invoke)),
    )
    .await
    .expect("the host answers")
    .expect("the host answers its own action")
}

/// The host action `action` on `linked`'s host, and the value it answered.
async fn host_action<T: serde::de::DeserializeOwned>(
    linked: &Linked,
    action: &str,
    argument: serde_json::Value,
) -> T {
    let RemoteActionMessage::Receipt(receipt) = host_answer(linked, action, argument).await else {
        panic!("an invocation is answered with a receipt");
    };
    match receipt.status {
        ReceiptStatus::Completed { output } => {
            serde_json::from_value(output.value.unwrap_or_default()).expect("the answer reads")
        }
        other => panic!("the host's own actions complete: {other:?}"),
    }
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
    let admin = person_workspace_id(&alice.user_pk);
    let workspace = WorkspaceId::parse(&admin).unwrap();

    // `tilo machine add`: the choices make a plan document, and the host's own
    // action API writes it into the person's Servers workspace. A device that
    // holds none plans nothing: it waits to be taken in.
    // The machine's own key, as the image hands it one before it is certified.
    let machine_key = hex::encode([0x33_u8; 32]);
    // The plan names the ticket's nonce and expiry only; the ticket itself is
    // the issuing device's to serve.
    let token = install_plan::token().unwrap();
    let token_nonce = token.nonce.clone();
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
    .plan("0123456789abcdef0123456789abcdef", token)
    .unwrap();
    let arguments = serde_json::json!({
        "plan": serde_json::to_string(&plan).unwrap(),
        "label": "cupboard",
        "token_file": token_file.display().to_string(),
        "installer_url": install_plan::INSTALLER_URL,
    });
    // A device that holds none plans nothing: the host holds no workspace of
    // hers, and its link refuses the phone.
    let unlinked = Linked::open_hosted(&laptop_host, &phone, workspace).await;
    let refused = unlinked.err();
    assert!(
        refused.is_some(),
        "a device without the person's workspace founds none to plan in: {refused:?}"
    );
    assert!(!servers::holds_person_workspace(laptop_host.host.workspaces()).await);

    // The laptop made the person's root, so it founds their workspace. The
    // phone is one of their devices; it comes to hold the workspace as one of
    // theirs does, and then asks the host what to plan.
    servers::found(laptop_host.host.workspaces()).await.unwrap();
    take_in(&laptop_host, &admin, &phone).await;
    let linked = own_link(&phone, &laptop_host, workspace).await;
    let said: String = host_action(&linked, PLAN_MACHINE, arguments).await;
    assert!(
        said.contains("plan 0123456789abcdef0123456789abcdef"),
        "{said}"
    );
    assert!(said.contains("token "), "the token file is named: {said}");
    let planned = servers::read(laptop_host.host.workspaces()).await.unwrap();
    assert_eq!(planned.waiting().count(), 1, "the machine waits to install");

    // The machine enrolled with the token and the person approved it: their
    // device takes it into its entry under its device key. The holders hand it
    // the Servers workspace from there.
    let _: serde_json::Value = host_action(
        &linked,
        ADMIT_MACHINE,
        serde_json::json!({ "device": machine_key, "nonce": token_nonce.clone() }),
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
    join_person_workspace(&laptop_host, &tablet, &tablet_host).await;

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
        Servers::BEGIN_INSTALL.with((machine_key.clone(), token_nonce, report.drives, report.ask)),
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
    servers::found(laptop_host.host.workspaces()).await.unwrap();
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
    join_person_workspace(&laptop_host, &cupboard, &machine_host).await;

    // The machine's first start: its entry is its own, under its device key.
    let state = tempfile::tempdir().unwrap();

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
        let requests = requests.clone();
        let drives = drives.clone();
        tokio::spawn(async move {
            tilo_server::servers::first_install::install(&workspaces, &cert, &drives, &requests)
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
    servers::found(desktop_host.host.workspaces())
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
    let workspace = WorkspaceId::parse(&admin).unwrap();
    let holders = desktop_host.host.workspaces().hosts(&admin).await.unwrap();
    assert_eq!(holders.len(), 2, "the desktop and the server hold it");
    assert!(
        holders
            .iter()
            .any(|hint| hint.host.device_public_key == nas.device_pubkey()),
        "the server is named among the workspace's holders"
    );

    // Another device of the person lists the server from the document, with
    // where it is dialled: what `tilo server` and Settings read.
    let phone = alice.enrol("phone", DeviceCapabilities::member());
    take_in(&desktop_host, &admin, &phone).await;
    let linked = own_link(&phone, &desktop_host, workspace).await;
    let listed: Vec<tilo_server_protocol::servers::PersonServer> =
        host_action(&linked, LIST_SERVERS, serde_json::json!({})).await;
    assert_eq!(
        listed,
        vec![tilo_server_protocol::servers::PersonServer {
            device: hex::encode(nas.device_pubkey()),
            label: "nas".into(),
            endpoint: Some(server_host.host.node_addr()),
        }]
    );
}

/// A newly certified device of the person opens their workspace as it opens
/// any workspace of theirs: the workspace's id follows from their root, a
/// device that keeps it signs where it is reached, and the new device proves it
/// is the person's there and keeps it from then on. A server and an ordinary
/// device alike, and nothing asks a server to let either in. A device of
/// another person proves nothing there.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_device_of_the_person_opens_the_persons_workspace_as_a_member() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let laptop_host = own_host(&alice, &laptop).await;
    let id = servers::found(laptop_host.host.workspaces()).await.unwrap();
    assert_eq!(id, person_workspace_id(&alice.user_pk));
    let workspace = WorkspaceId::parse(&id).unwrap();
    let hint = laptop_host
        .host
        .workspaces()
        .hosts(&id)
        .await
        .unwrap()
        .into_iter()
        .find(|hint| hint.host.device_public_key == laptop.device_pubkey())
        .expect("the laptop signs where it keeps the workspace");

    for label in ["nas", "phone"] {
        let device = alice.enrol(label, DeviceCapabilities::member());
        let copy = member_host(&alice, &device).await;
        let opened = tilo_test_env::within(
            "the new device opens the person's workspace",
            copy.host.workspaces().hold_as_member(workspace, &hint),
        )
        .await;
        assert_eq!(opened, Ok(workspace), "{label} is the person's device");
        assert!(copy.host.workspaces().is_member(&id).await.unwrap());
        let (name, _) = copy
            .host
            .workspaces()
            .member_copy(workspace)
            .await
            .expect("the new device keeps the workspace");
        assert_eq!(
            name,
            tilo_workspace_service::workspaces::person_workspace::NAME
        );
    }

    // Bob's device is no device of Alice's: her workspace does not let it in.
    let bob = TestUser::fresh("Bob");
    let intruder = bob.enrol("laptop", DeviceCapabilities::primary());
    let outside = member_host(&bob, &intruder).await;
    let refused = tilo_test_env::within(
        "the other person's device is answered",
        outside.host.workspaces().hold_as_member(workspace, &hint),
    )
    .await;
    assert!(refused.is_err(), "another person's device is refused");
    assert!(!outside.host.workspaces().is_member(&id).await.unwrap());
}

/// A Wi-Fi passphrase one device of the person kept, and what that device says
/// of itself, reach a device of theirs taken in later through their own
/// workspace: the new device reads both as the first one wrote them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_device_reads_the_wifi_and_devices_its_person_kept() {
    use tilo_workspace_service::workspaces::person_workspace::{
        self,
        devices::{DEVICES, Devices},
    };
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let desktop = alice.enrol("desktop", DeviceCapabilities::member());
    let laptop_host = own_host(&alice, &laptop).await;
    let desktop_host = own_host(&alice, &desktop).await;
    servers::found(laptop_host.host.workspaces()).await.unwrap();
    let network = "ab".repeat(32);
    let key = hex::encode(laptop.device_pubkey());
    person_workspace::apply(
        laptop_host.host.workspaces(),
        DEVICES,
        Devices::KEEP_WIFI.with((network.clone(), "0a0b".into())),
    )
    .await
    .unwrap();
    person_workspace::apply(
        laptop_host.host.workspaces(),
        DEVICES,
        Devices::KEEP_FACT.with((key.clone(), "00".into(), "about".into(), "0c".into())),
    )
    .await
    .unwrap();

    join_person_workspace(&laptop_host, &desktop, &desktop_host).await;
    tilo_test_env::within("the new device to read what the person kept", async {
        loop {
            let read = person_workspace::read::<Devices>(desktop_host.host.workspaces(), DEVICES)
                .await
                .unwrap();
            let wifi = read
                .wifi
                .get(&tilo_resource::Id::from_raw(network.clone()))
                .map(|kept| kept.credential.clone());
            let about = read
                .devices
                .get(&tilo_resource::Id::from_raw(key.clone()))
                .and_then(|kept| kept.facts.get("about").cloned());
            if wifi.as_deref() == Some("0a0b") && about.as_deref() == Some("0c") {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
}

/// A key `user` made for an agent of theirs to act in all their workspaces.
fn agent_key(user: &TestUser) -> TestDevice {
    let sk = ed25519_dalek::SigningKey::from_bytes(&[0x4b; 32]);
    let now = tilo_clock::unix_now_ns();
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: user.user_pk,
        device_public_key: sk.verifying_key().to_bytes(),
        device_label: "agent".into(),
        capabilities: DeviceCapabilities::agent_grant(
            tilo_user_identity::AgentScope::AllWorkspaces,
        ),
        issued_at_ns: now,
        // A grant holds for a bounded time.
        expires_at_ns: Some(now.saturating_add(24 * 60 * 60 * 1_000_000_000)),
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, &user.user_sk).unwrap();
    TestDevice { sk, cert }
}

/// A machine that installed with nobody at its screen hands its disk's
/// recovery key to the host its login enters, as itself, and keeps no copy:
/// the vault of the person's Servers workspace keeps it under the machine's
/// device key, and any other device of theirs reads it back there, as
/// `tilo server recovery-key` does. A machine that installs again replaces its
/// key. An agent's key of the same person and another person's device read
/// nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_machine_keeps_its_disk_recovery_key_and_only_its_person_reads_it() {
    use tilo_server_protocol::servers::RecoveryKey;

    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let cupboard = alice.enrol("cupboard", DeviceCapabilities::member());
    let phone = alice.enrol("phone", DeviceCapabilities::member());
    // The laptop takes part in its workspaces' channels, as a person's own device does: its
    // keys open the vault.
    let laptop_host = crate::common::serve_with_keys(
        &alice,
        &laptop,
        WorkSetting {
            from: tilo_server_protocol::WorkFrom::Anyone,
            ..WorkSetting::own_person()
        },
        tempfile::tempdir().unwrap(),
        Some(tilo_workspace_service::workspaces::DeviceKeyGroups {
            keys: Arc::<tilo_workspace_service::workspaces::fixtures::TestGroupKeys>::default(),
            sink: None,
        }),
    )
    .await;
    servers::found(laptop_host.host.workspaces()).await.unwrap();
    let admin = person_workspace_id(&alice.user_pk);
    let workspace = WorkspaceId::parse(&admin).unwrap();
    let machine_key = hex::encode(cupboard.device_pubkey());

    // The installer, right after the machine logged in with its ticket: one of
    // the person's devices, which comes to hold their Servers workspace as one
    // of theirs does.
    take_in(&laptop_host, &admin, &cupboard).await;
    let machine = own_link(&cupboard, &laptop_host, workspace).await;
    let kept: String = host_action(
        &machine,
        KEEP_RECOVERY_KEY,
        serde_json::json!({ "key": "111111-222222-333333-444444\n" }),
    )
    .await;
    assert_eq!(
        kept, machine_key,
        "the key is kept under the machine's own key"
    );

    // `tilo server recovery-key cupboard` on another device of the person.
    take_in(&laptop_host, &admin, &phone).await;
    let phone_link = own_link(&phone, &laptop_host, workspace).await;
    let keys: Vec<RecoveryKey> =
        host_action(&phone_link, RECOVERY_KEYS, serde_json::json!({})).await;
    assert_eq!(
        keys,
        vec![RecoveryKey {
            machine: machine_key.clone(),
            label: "cupboard".into(),
            key: "111111-222222-333333-444444".into(),
        }]
    );

    // Installed again, the machine hands its new key: one entry, the new key.
    let _: String = host_action(
        &machine,
        KEEP_RECOVERY_KEY,
        serde_json::json!({ "key": "555555-666666-777777-888888" }),
    )
    .await;
    let keys: Vec<RecoveryKey> =
        host_action(&phone_link, RECOVERY_KEYS, serde_json::json!({})).await;
    assert_eq!(keys.len(), 1, "{keys:?}");
    assert_eq!(keys[0].key, "555555-666666-777777-888888");

    // An agent's key of the same person is no device of theirs the workspace
    // holds: the host's link refuses it, so it reads no recovery key and keeps
    // none.
    let agent = agent_key(&alice);
    let unlinked = Linked::open_hosted(&laptop_host, &agent, workspace).await;
    let refused = unlinked.err();
    assert!(
        refused.is_some(),
        "an agent's key of the same person reaches no recovery key: {refused:?}"
    );

    // Another person's device is refused the link, and reads nothing there.
    let bob = TestUser::fresh("Bob");
    let bobs = bob.enrol("laptop", DeviceCapabilities::primary());
    let unlinked = Linked::open_hosted(&laptop_host, &bobs, workspace).await;
    let refused = unlinked.err();
    assert!(
        refused.is_some(),
        "another person's device reaches no recovery key: {refused:?}"
    );
    let keys: Vec<RecoveryKey> =
        host_action(&phone_link, RECOVERY_KEYS, serde_json::json!({})).await;
    assert_eq!(
        keys[0].key, "555555-666666-777777-888888",
        "nothing else changed it"
    );
}

/// The planned machine every planned-ticket test plans.
const PLANNED: &str = "0123456789abcdef0123456789abcdef";

/// A device of the person's as the ticket server it runs knows it: its own workspace service,
/// which holds their Servers workspace, where it is reached, its certificate and key, and their
/// root.
struct Issuing {
    workspaces: tilo_workspace_service::workspaces::Workspaces,
    addr: iroh::EndpointAddr,
    cert: tilo_user_identity::DeviceCert,
    key: ed25519_dalek::SigningKey,
    root: ed25519_dalek::SigningKey,
}

impl Issuing {
    fn reached(&self) -> tilo_device_identity::login::Reached {
        tilo_device_identity::login::Reached {
            endpoint: *self.addr.id.as_bytes(),
            relay: None,
            direct: self.addr.ip_addrs().copied().collect(),
        }
    }
}

impl tilo_device_tickets::Issuer for Issuing {
    fn serving(
        &self,
    ) -> Result<
        (
            tilo_device_identity::login::Reached,
            tilo_peer_state::actions::ActionService,
        ),
        String,
    > {
        Ok((self.reached(), self.workspaces.action_service()))
    }

    fn device(
        &self,
    ) -> Result<
        (
            tilo_user_identity::DeviceCert,
            Arc<dyn collab_types::signer::DeviceSigner>,
        ),
        String,
    > {
        Ok((self.cert.clone(), Arc::new(self.key.clone())))
    }

    fn root(&self) -> Result<Option<tilo_device_tickets::Root>, String> {
        Ok(Some(Arc::new(self.root.clone())))
    }

    fn entry(
        &self,
    ) -> std::pin::Pin<
        Box<dyn Future<Output = Result<tilo_device_identity::login::Entry, String>> + Send + '_>,
    > {
        Box::pin(std::future::ready(Ok(tilo_device_identity::login::Entry {
            workspace: person_workspace_id(&self.cert.user_public_key),
            workspace_name: None,
            server_cert: self.workspaces.device_cert().clone(),
            server: self.reached(),
            site: "https://tilo.test/".to_owned(),
            host_origin: "iroh:laptop".to_owned(),
        })))
    }

    fn servers(&self) -> Option<tilo_workspace_service::workspaces::Workspaces> {
        Some(self.workspaces.clone())
    }
}

/// The laptop that plans machines, on its own workspace service over `state`, whose device takes
/// part in its workspaces' channels through `keys`: what a restart opens again.
async fn planning_laptop(
    alice: &TestUser,
    laptop: &TestDevice,
    state: tempfile::TempDir,
    keys: &Arc<tilo_workspace_service::workspaces::fixtures::TestGroupKeys>,
) -> Hosted {
    crate::common::serve_with_keys(
        alice,
        laptop,
        WorkSetting::own_person(),
        state,
        Some(tilo_workspace_service::workspaces::DeviceKeyGroups {
            keys: Arc::clone(keys) as _,
            sink: None,
        }),
    )
    .await
}

/// The ticket server `device` runs on `hosted`, as it starts, and how many kept tickets it
/// serves again.
async fn ticket_server(
    alice: &TestUser,
    device: &TestDevice,
    hosted: &Hosted,
    addr: iroh::EndpointAddr,
) -> (tilo_device_tickets::TicketServer, usize) {
    let tickets = tilo_device_tickets::TicketServer::new(Arc::new(Issuing {
        workspaces: hosted.host.workspaces().clone(),
        addr,
        cert: device.cert.clone(),
        key: device.sk.clone(),
        root: alice.user_sk.clone(),
    }));
    let serving = tickets.restore().await.expect("the kept tickets read");
    (tickets, serving)
}

/// Plan [`PLANNED`] on `hosted`, its ticket running out at `expires_at_ns`.
async fn plan_cupboard(hosted: &Hosted, expires_at_ns: u64) {
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
        PLANNED,
        install_profile_model::plan::Token {
            nonce: hex::encode([0x6b_u8; 16]),
            expires_at_ns,
        },
    )
    .unwrap();
    servers::apply(
        hosted.host.workspaces(),
        Servers::PLAN_MACHINE.with((serde_json::to_string(&plan).unwrap(), "cupboard".into())),
    )
    .await
    .unwrap();
}

/// The plan's ticket, as `tilo machine add` asks the laptop for it.
async fn planned_ticket(
    tickets: &tilo_device_tickets::TicketServer,
) -> tilo_device_identity::login::Ticket {
    tickets
        .issue(
            Some(tilo_device_tickets::Role::Plan(PLANNED.to_owned())),
            false,
        )
        .await
        .expect("the plan's ticket is issued")
}

/// A machine with the key `seed` asks the device at `issuer` to certify it with `ticket`,
/// as the installer's `tilo login` does: its key once certified, or how it was refused.
async fn machine_certified(
    issuer: &iroh::EndpointAddr,
    ticket: &tilo_device_identity::login::Ticket,
    seed: u8,
) -> Result<[u8; 32], String> {
    use tilo_device_identity::login::{CERTIFY, CertifyInput, PROVIDER, RESOURCE, SCOPE};

    let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
    let dialed = tilo_server_client::ServerAddress::at(issuer.clone());
    let endpoint = dialed.bind(Some(key.to_bytes())).await.unwrap();
    let service = tilo_peer_state::actions::ActionService::new(
        key.clone(),
        Arc::new(tilo_peer_state::actions::NoActions),
    );
    let device = Arc::new(service.remote(&endpoint, &dialed.addr(), SCOPE));
    let input = CertifyInput {
        secret: ticket.secret_hex(),
        device_public_key: hex::encode(key.verifying_key().to_bytes()),
        agent: "tilo".into(),
        label: Some("cupboard".into()),
        tilo_os: false,
    };
    let serde_json::Value::Object(argument) = serde_json::to_value(&input).unwrap() else {
        panic!("the input is an object");
    };
    let request = InvokeRequest {
        on_behalf_of: None,
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: PROVIDER.to_owned(),
            address: ActionAddress::new(RESOURCE, CERTIFY),
        },
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let settled = device
        .invoke_until_settled(request, std::time::Duration::from_secs(10))
        .await;
    endpoint.close().await;
    match settled {
        Some(Ok(tilo_actions::InvocationUpdate::Succeeded { .. })) => {
            Ok(key.verifying_key().to_bytes())
        }
        other => Err(format!("{other:?}")),
    }
}

/// The device key of the machine that took [`PLANNED`], once one did.
async fn taken_by(hosted: &Hosted) -> Option<[u8; 32]> {
    let servers = servers::read(hosted.host.workspaces()).await.unwrap();
    servers.servers.iter().find_map(|(device, server)| {
        let mut key = [0; 32];
        (server.plan.plan == PLANNED && hex::decode_to_slice(device.as_str(), &mut key).is_ok())
            .then_some(key)
    })
}

/// The person plans a machine on their laptop and its ticket is printed; the laptop restarts
/// before the machine installs. The restarted laptop serves the ticket again from the vault of
/// the person's Servers workspace, and the machine joins with it and takes the plan's entry.
/// Another device of the person's, reading the same vault, does not serve it, and once the plan
/// is taken nothing serves it again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_planned_machines_ticket_outlives_a_restart_of_the_device_that_printed_it() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let phone = alice.enrol("phone", DeviceCapabilities::primary());
    let keys = Arc::<tilo_workspace_service::workspaces::fixtures::TestGroupKeys>::default();
    let hosted = planning_laptop(&alice, &laptop, tempfile::tempdir().unwrap(), &keys).await;
    servers::found(hosted.host.workspaces()).await.unwrap();
    plan_cupboard(&hosted, u64::MAX / 2).await;
    let (printed, _) = ticket_server(&alice, &laptop, &hosted, hosted.host.node_addr()).await;
    let ticket = planned_ticket(&printed).await;

    // The laptop stops, and nothing it held in memory is left.
    drop(printed);
    let state = hosted.stop_keeping().await;
    let hosted = planning_laptop(&alice, &laptop, state, &keys).await;
    let (restarted, serving) =
        ticket_server(&alice, &laptop, &hosted, hosted.host.node_addr()).await;
    assert_eq!(
        serving, 1,
        "the restarted laptop serves the plan's ticket again"
    );

    // The phone reads the same vault and leaves the ticket: the ticket names the laptop.
    let phone_addr = iroh::EndpointAddr::from_parts(
        iroh::EndpointId::from_bytes(&phone.cert.device_public_key).unwrap(),
        Vec::new(),
    );
    let (_phone, serving) = ticket_server(&alice, &phone, &hosted, phone_addr).await;
    assert_eq!(
        serving, 0,
        "another device of the person's does not serve the laptop's ticket"
    );

    let machine = machine_certified(&hosted.host.node_addr(), &ticket, 0x51)
        .await
        .unwrap_or_else(|why| panic!("the machine joins with the kept ticket: {why}"));
    assert_eq!(
        taken_by(&hosted).await,
        Some(machine),
        "the machine took the plan"
    );

    // Taken, the plan's ticket is let go of: after another restart it serves no one.
    drop(restarted);
    let state = hosted.stop_keeping().await;
    let hosted = planning_laptop(&alice, &laptop, state, &keys).await;
    let (_again, serving) = ticket_server(&alice, &laptop, &hosted, hosted.host.node_addr()).await;
    assert_eq!(serving, 0, "a taken plan's ticket is not served again");
    let refused = machine_certified(&hosted.host.node_addr(), &ticket, 0x52).await;
    assert!(
        refused.is_err(),
        "a taken plan's ticket is refused: {refused:?}"
    );
    assert_eq!(
        taken_by(&hosted).await,
        Some(machine),
        "the plan stays the first machine's"
    );
    hosted.stop().await;
}

/// A planned machine's ticket that ran out while the laptop was stopped is not served again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_planned_machines_ticket_that_ran_out_is_refused_after_a_restart() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let keys = Arc::<tilo_workspace_service::workspaces::fixtures::TestGroupKeys>::default();
    let hosted = planning_laptop(&alice, &laptop, tempfile::tempdir().unwrap(), &keys).await;
    servers::found(hosted.host.workspaces()).await.unwrap();
    let expires_at_ns = tilo_clock::unix_now_ns().saturating_add(2_000_000_000);
    plan_cupboard(&hosted, expires_at_ns).await;
    let (printed, _) = ticket_server(&alice, &laptop, &hosted, hosted.host.node_addr()).await;
    let ticket = planned_ticket(&printed).await;
    drop(printed);
    let state = hosted.stop_keeping().await;
    while tilo_clock::unix_now_ns() <= expires_at_ns {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    let hosted = planning_laptop(&alice, &laptop, state, &keys).await;
    let (_restarted, serving) =
        ticket_server(&alice, &laptop, &hosted, hosted.host.node_addr()).await;
    assert_eq!(serving, 0, "a ticket that ran out is not served again");
    let refused = machine_certified(&hosted.host.node_addr(), &ticket, 0x61).await;
    assert!(
        refused.is_err(),
        "a ticket that ran out is refused: {refused:?}"
    );
    assert_eq!(taken_by(&hosted).await, None, "no machine took the plan");
    hosted.stop().await;
}
