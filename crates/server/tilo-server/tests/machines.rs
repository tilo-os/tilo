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
use tilo_server::servers::{self, Servers, admin_workspace_id};
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
    let admin = admin_workspace_id(&alice.user_pk);
    assert!(
        !servers::holds_admin(laptop_host.host.workspaces()).await,
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
        while !servers::holds_admin(laptop_host.host.workspaces()).await {
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
        while !servers::holds_admin(tablet_host.host.workspaces()).await {
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
    assert_eq!(admin, admin_workspace_id(&alice.user_pk));
}
