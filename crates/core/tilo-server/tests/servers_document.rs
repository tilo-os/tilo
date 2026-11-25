//! A server does what its entry in its person's admin workspace says: a
//! planned machine is handed the workspaces its plan names once, after which
//! each workspace's governance alone says what it holds; it offers the work its
//! entry allows, and follows the update policy its entry names.

#![allow(
    clippy::significant_drop_tightening,
    reason = "hosts, routers and fixtures deliberately stay alive until each test ends"
)]

use tilo_resource::Id;
use tilo_server::servers::{self, SERVERS, Servers, applied, person_workspace_id};
use tilo_server_protocol::WorkSetting;
use tilo_user_identity::DeviceCapabilities;
use tilo_workspace_service::work::offers_compute;

use crate::common::{TestUser, join_person_workspace, serve_on};

/// The document's entry id of the device `device`.
fn entry(device: &crate::common::TestDevice) -> Id {
    Id::from_raw(hex::encode(device.device_pubkey()))
}

/// A machine planned to hold a workspace is handed it once it enrolled, by
/// the device that governs the workspace; from then on the workspace's
/// governance alone says what it holds, so taking it out there is not undone
/// by the plan.
/// The plan `nas` installs with, as the action takes it: a server named for it that holds
/// `workspace` as a key holder, begun with `nonce` and unlocked by the TPM.
fn plan_holding(workspace: &str, nonce: &str) -> String {
    let plan = install_plan::Choices {
        destination: "tilo-server".to_owned(),
        name: Some("nas".to_owned()),
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
        hosting: Some(vec![install_profile_model::plan::Holding {
            workspace: workspace.to_owned(),
            mode: "key-holder".to_owned(),
        }]),
    }
    .plan(
        "0123456789abcdef0123456789abcdef",
        install_profile_model::plan::Token {
            nonce: nonce.to_owned(),
            expires_at_ns: u64::MAX / 2,
        },
    )
    .unwrap();
    serde_json::to_string(&plan).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_planned_machine_holds_what_its_plan_names_until_its_workspace_lets_it_go() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let nas = alice.enrol("nas", DeviceCapabilities::member());
    let governor = serve_on(&alice, &laptop, WorkSetting::none()).await;
    let server = serve_on(&alice, &nas, WorkSetting::own_person()).await;
    let workspaces = governor.host.workspaces();
    servers::found(workspaces).await.unwrap();
    let workspace = workspaces.create("Notes").await.unwrap();
    let nonce = hex::encode([0x5a_u8; 16]);
    let plan = plan_holding(&workspace.workspace, &nonce);
    servers::apply(workspaces, Servers::PLAN_MACHINE.with((plan, "nas".into())))
        .await
        .unwrap();
    let retained = || async {
        server
            .host
            .workspace_documents()
            .retained(workspace.workspace_id)
            .await
            .ok()
    };
    applied::reconcile(workspaces).await.unwrap();
    assert!(retained().await.is_none(), "a plan holds nothing yet");

    // The machine enrolled with the plan's token and gave the address it is
    // dialled at.
    servers::apply(
        workspaces,
        Servers::BEGIN_INSTALL.with((
            hex::encode(nas.device_pubkey()),
            nonce,
            "[]".into(),
            String::new(),
        )),
    )
    .await
    .unwrap();
    servers::apply(
        workspaces,
        Servers::ENROLL_SERVER.with((
            hex::encode(nas.device_pubkey()),
            "nas".into(),
            serde_json::to_string(&server.host.node_addr()).unwrap(),
        )),
    )
    .await
    .unwrap();
    // Every change of the person's record of their servers names the app whose record it is:
    // a host decides each against that app's surface.
    let admin = person_workspace_id(&workspaces.device_cert().user_public_key);
    let bytes = workspaces
        .resource_document(&admin, SERVERS.0, SERVERS.1)
        .await
        .unwrap()
        .expect("the person's record of their servers is here");
    let mut doc = automerge::AutoCommit::load(&bytes).unwrap();
    let apps: Vec<String> = tilo_resource::history::versions(&mut doc)
        .iter()
        .filter_map(|version| {
            version
                .context
                .as_ref()?
                .app
                .as_ref()
                .map(|app| app.id.clone())
        })
        .collect();
    assert!(
        !apps.is_empty(),
        "the changes reach the record's history"
    );
    assert!(apps.iter().all(|app| app == SERVERS.0), "{apps:?}");
    let attached = applied::reconcile(workspaces).await.unwrap();
    assert!(
        attached.held.contains(&workspace.workspace),
        "the governor hands it the planned workspace: {attached:?}"
    );
    tilo_test_env::within("the server to hold the workspace", async {
        while retained().await.is_none() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    assert_eq!(
        workspaces.hosts(&workspace.workspace).await.unwrap().len(),
        2,
        "the workspace names the governor and its holder"
    );
    let entry_now = servers::read(workspaces)
        .await
        .unwrap()
        .servers
        .get(&entry(&nas))
        .cloned()
        .unwrap();
    assert!(
        entry_now
            .plan
            .hosting
            .iter()
            .all(|(_, planned)| planned.handed),
        "the plan was handed over once: {entry_now:?}"
    );

    // The workspace takes it out of its holders; the plan does not hand it
    // back.
    workspaces
        .remove_server(&workspace.workspace, &hex::encode(nas.device_pubkey()))
        .await
        .unwrap();
    let again = applied::reconcile(workspaces).await.unwrap();
    assert!(
        !again.held.contains(&workspace.workspace),
        "the plan does not hand it back: {again:?}"
    );
    tilo_test_env::within("the server to let go of the workspace", async {
        while retained().await.is_some() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    assert_eq!(
        workspaces.hosts(&workspace.workspace).await.unwrap().len(),
        1,
        "the workspace names its governor alone"
    );
}

/// A server offers exactly what its entry says: an entry naming no compute
/// takes no work, and one naming compute and a machine budget offers it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_server_offers_what_its_entry_says() {
    let alice = TestUser::fresh("Alice");
    let nas = alice.enrol("nas", DeviceCapabilities::member());
    let server = serve_on(&alice, &nas, WorkSetting::own_person()).await;
    let policy = tempfile::tempdir().unwrap();
    let policy_request = policy.path().join("update-policy.request");
    let workspaces = server.host.workspaces();
    let admin = person_workspace_id(&alice.user_pk);
    workspaces
        .create_workspace("Servers", Some(&admin))
        .await
        .unwrap();
    servers::apply(
        workspaces,
        Servers::ENROLL_SERVER.with((
            hex::encode(nas.device_pubkey()),
            "nas".into(),
            serde_json::to_string(&server.host.node_addr()).unwrap(),
        )),
    )
    .await
    .unwrap();
    // A freshly enrolled server offers nothing until its entry says so.
    applied::reconcile_into(workspaces, &applied::Requests::at(&policy_request))
        .await
        .unwrap();
    assert!(!offers_compute(workspaces).await, "it runs no work yet");

    servers::apply(
        workspaces,
        Servers::SET_APPETITE.with((entry(&nas), 500, true, 2)),
    )
    .await
    .unwrap();
    let applied = applied::reconcile_into(workspaces, &applied::Requests::at(&policy_request))
        .await
        .unwrap();
    assert!(applied.offered);
    assert!(offers_compute(workspaces).await, "its entry offers compute");
}

/// A retired server lets go of everything it holds, offers nothing and refuses
/// its own revoked key, and keeps its entry so it is never taken in again as a
/// new server.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_retired_server_lets_go_of_everything() {
    let alice = TestUser::fresh("Alice");
    let nas = alice.enrol("nas", DeviceCapabilities::member());
    let server = serve_on(&alice, &nas, WorkSetting::own_person()).await;
    let policy = tempfile::tempdir().unwrap();
    let policy_request = policy.path().join("update-policy.request");
    let workspaces = server.host.workspaces();
    let workspace = workspaces.create("Kept").await.unwrap();
    let admin = person_workspace_id(&alice.user_pk);
    workspaces
        .create_workspace("Servers", Some(&admin))
        .await
        .unwrap();
    servers::apply(
        workspaces,
        Servers::ENROLL_SERVER.with((
            hex::encode(nas.device_pubkey()),
            "nas".into(),
            serde_json::to_string(&server.host.node_addr()).unwrap(),
        )),
    )
    .await
    .unwrap();
    servers::apply(
        workspaces,
        Servers::SET_APPETITE.with((entry(&nas), 500, true, 2)),
    )
    .await
    .unwrap();
    applied::reconcile_into(workspaces, &applied::Requests::at(&policy_request))
        .await
        .unwrap();
    assert!(offers_compute(workspaces).await);

    let mut revocation = tilo_user_identity::RevocationEntry {
        user_public_key: alice.user_pk,
        revoked_device_public_key: nas.device_pubkey(),
        revoked_at_ns: 1,
        reason: tilo_user_identity::RevocationReason::Retired,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut revocation, &alice.user_sk).unwrap();
    servers::apply(
        workspaces,
        Servers::ASK_RETIREMENT.with((entry(&nas), serde_json::to_string(&revocation).unwrap())),
    )
    .await
    .unwrap();
    let applied = applied::reconcile_into(workspaces, &applied::Requests::at(&policy_request))
        .await
        .unwrap();
    assert!(applied.retired, "asked to retire, it retired its own entry");
    assert!(
        !offers_compute(workspaces).await,
        "a retired server takes no work"
    );
    assert!(
        applied.released.contains(&workspace.workspace),
        "it let go of what it held: {applied:?}"
    );
    let still_there = workspaces.list().await;
    assert!(
        !still_there
            .iter()
            .any(|summary| summary.workspace == workspace.workspace),
        "it holds nothing: {still_there:?}"
    );
    assert!(
        workspaces.revocations().refuses(&nas.cert),
        "and refuses its own key, which its person's root revoked"
    );
}

/// A server writes the machine configuration its entry names where the root-run
/// helper reads it, and what that helper reports becomes the entry's own
/// status, so the person reads what it applied or why not.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_servers_machine_configuration_is_written_and_its_report_read() {
    let alice = TestUser::fresh("Alice");
    let nas = alice.enrol("nas", DeviceCapabilities::member());
    let server = serve_on(&alice, &nas, WorkSetting::own_person()).await;
    let requests = tempfile::tempdir().unwrap();
    let requests_at = || applied::Requests::at(requests.path().join("update-policy.request"));
    let workspaces = server.host.workspaces();
    let admin = person_workspace_id(&alice.user_pk);
    workspaces
        .create_workspace("Servers", Some(&admin))
        .await
        .unwrap();
    servers::apply(
        workspaces,
        Servers::ENROLL_SERVER.with((
            hex::encode(nas.device_pubkey()),
            "nas".into(),
            serde_json::to_string(&server.host.node_addr()).unwrap(),
        )),
    )
    .await
    .unwrap();

    servers::apply(
        workspaces,
        Servers::SET_HOSTNAME.with((entry(&nas), "nas".into())),
    )
    .await
    .unwrap();
    servers::apply(
        workspaces,
        Servers::USE_DISK.with((
            entry(&nas),
            "ROOT1".into(),
            servers::device::SYSTEM.into(),
            String::new(),
        )),
    )
    .await
    .unwrap();
    servers::apply(
        workspaces,
        Servers::SET_INTERFACE.with((
            entry(&nas),
            "eno1".into(),
            servers::device::DHCP.into(),
            String::new(),
            String::new(),
            "1.1.1.1".into(),
            String::new(),
        )),
    )
    .await
    .unwrap();
    let applied = applied::reconcile_into(workspaces, &requests_at())
        .await
        .unwrap();
    let revision = applied
        .device_config
        .expect("the machine configuration was written");
    assert!(!applied.device_status, "nothing was reported yet");
    let request = std::fs::read_to_string(requests.path().join("device-config.request")).unwrap();
    assert!(
        request.starts_with(&format!("revision|{revision}\n")),
        "{request}"
    );
    assert!(request.contains("hostname|nas\n"), "{request}");
    assert!(request.contains("disk|system|ROOT1|0\n"), "{request}");
    assert!(
        request.contains("interface|eno1|dhcp|||1.1.1.1||0\n"),
        "{request}"
    );

    // Writing the same request again changes nothing.
    let applied = applied::reconcile_into(workspaces, &requests_at())
        .await
        .unwrap();
    assert_eq!(applied.device_config, None);

    // What the helper reported is the entry's status, once.
    std::fs::write(
        requests.path().join("device-config.report"),
        format!("revision\t{revision}\nstate\treverted\ndetail\tno device reached this machine\n"),
    )
    .unwrap();
    let applied = applied::reconcile_into(workspaces, &requests_at())
        .await
        .unwrap();
    assert!(applied.device_status);
    let entry = servers::read(workspaces)
        .await
        .unwrap()
        .servers
        .get(&entry(&nas))
        .cloned()
        .unwrap();
    assert_eq!(entry.device.status.revision, revision);
    assert_eq!(entry.device.status.state, "reverted");
    assert_eq!(entry.device.status.detail, "no device reached this machine");
    let applied = applied::reconcile_into(workspaces, &requests_at())
        .await
        .unwrap();
    assert!(!applied.device_status, "the same report is recorded once");
}

/// A person with no desktop enrols three servers one after another: the first
/// makes their admin workspace, each later one is taken in by one that holds
/// it, and all three then hold the one workspace. What is set at one of them
/// is applied at the others.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one scenario: the three servers, their hand-overs and the shared document stay together"
)]
async fn servers_enrolled_one_after_another_share_one_admin_workspace() {
    let alice = TestUser::fresh("Alice");
    let devices =
        ["first", "second", "third"].map(|label| alice.enrol(label, DeviceCapabilities::member()));
    let mut hosts = Vec::new();
    for device in &devices {
        hosts.push(serve_on(&alice, device, WorkSetting::own_person()).await);
    }
    let policy = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let _following = hosts
        .iter()
        .enumerate()
        .map(|(at, hosted)| {
            applied::follow(
                hosted.host.workspaces(),
                applied::Requests::at(policy.path().join(format!("{at}.request"))),
                None,
                state.path().join(format!("state{at}")),
            )
        })
        .collect::<Vec<_>>();
    let admin = person_workspace_id(&alice.user_pk);
    let [first, second, third] = [&hosts[0], &hosts[1], &hosts[2]];

    // One of the person's own devices makes it when it is first needed, and
    // the first server is entered in it: no server founds it.
    servers::found(first.host.workspaces()).await.unwrap();
    servers::apply(
        first.host.workspaces(),
        Servers::ENROLL_SERVER.with((
            hex::encode(devices[0].device_pubkey()),
            devices[0].cert.device_label.clone(),
            serde_json::to_string(&first.host.node_addr()).unwrap(),
        )),
    )
    .await
    .unwrap();
    // The second asks the first, the third the second: any server holding the
    // workspace takes a later one in.
    for (enrolled, (asked, asking, device)) in
        [(first, second, &devices[1]), (second, third, &devices[2])]
            .into_iter()
            .enumerate()
    {
        servers::admit(
            asked.host.workspaces(),
            &device.cert,
            &asking.host.node_addr(),
        )
        .await
        .unwrap();
        // Holding the workspace is its governance alone: the document that
        // names its servers reaches the new holder over the replication after
        // it. The next admission is written by this holder, so it waits for
        // that document too; founding one of its own would diverge from the
        // copy the servers hold.
        let enrolled = enrolled + 2;
        tilo_test_env::within(
            "the later server to hold the admin workspace and read its document",
            async {
                loop {
                    let named = servers::read(asking.host.workspaces())
                        .await
                        .map_or(0, |servers| servers.servers.len());
                    if servers::holds_person_workspace(asking.host.workspaces()).await
                        && named == enrolled
                    {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            },
        )
        .await;
    }
    tilo_test_env::within(
        "every server to name all three holders and read the one document",
        async {
            loop {
                let mut named = Vec::new();
                let mut named_servers = Vec::new();
                for hosted in &hosts {
                    named.push(
                        hosted
                            .host
                            .workspaces()
                            .hosts(&admin)
                            .await
                            .map_or(0, |hosts| hosts.len()),
                    );
                    named_servers.push(
                        servers::read(hosted.host.workspaces())
                            .await
                            .map_or(0, |servers| servers.servers.len()),
                    );
                }
                if named == [3, 3, 3] && named_servers == [3, 3, 3] {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        },
    )
    .await;
    let stranger = TestUser::fresh("Mallory").enrol("theirs", DeviceCapabilities::member());
    assert!(
        servers::admit(
            first.host.workspaces(),
            &stranger.cert,
            &third.host.node_addr(),
        )
        .await
        .is_err(),
        "another person's server is not taken in"
    );

    // Set at the third, applied at the first.
    servers::apply(
        third.host.workspaces(),
        Servers::SET_APPETITE.with((entry(&devices[0]), 500, true, 2)),
    )
    .await
    .unwrap();
    tilo_test_env::within("the first server to offer what the third set", async {
        while !offers_compute(first.host.workspaces()).await {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    assert_eq!(
        servers::read(second.host.workspaces())
            .await
            .unwrap()
            .servers
            .len(),
        3,
        "one document names every server"
    );
}

/// A device that was handed the Servers workspace writes its own entry before
/// the document reached it. Both writes start from the same genesis, so the
/// two copies merge into one document holding both entries, rather than one
/// device's copy winning and taking the other's entry with it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_that_writes_before_the_document_arrives_keeps_every_entry() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let desktop = alice.enrol("desktop", DeviceCapabilities::member());
    let holder = serve_on(&alice, &laptop, WorkSetting::own_person()).await;
    let late = serve_on(&alice, &desktop, WorkSetting::own_person()).await;

    // The person's own device makes the workspace when it is first needed, and
    // the first machine is planned in it.
    servers::found(holder.host.workspaces()).await.unwrap();
    servers::apply(
        holder.host.workspaces(),
        Servers::ENROLL_SERVER.with((
            hex::encode(laptop.device_pubkey()),
            "laptop".into(),
            serde_json::to_string(&holder.host.node_addr()).unwrap(),
        )),
    )
    .await
    .unwrap();

    // The other device is handed the workspace: its governance first, and the
    // document over the replication after it.
    join_person_workspace(&holder, &desktop, &late).await;

    // It writes its own server there at once, before the document it knows
    // nothing of has arrived.
    servers::apply(
        late.host.workspaces(),
        Servers::ENROLL_SERVER.with((
            hex::encode(desktop.device_pubkey()),
            "desktop".into(),
            serde_json::to_string(&late.host.node_addr()).unwrap(),
        )),
    )
    .await
    .unwrap();

    // Once the copies meet, both devices read one document naming both.
    tilo_test_env::within("both entries to reach both devices", async {
        loop {
            let mut counted = Vec::new();
            for hosted in [&holder, &late] {
                counted.push(
                    servers::read(hosted.host.workspaces())
                        .await
                        .map_or(0, |servers| servers.servers.len()),
                );
            }
            if counted == [2, 2] {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
}

/// The servers `hosted` reads in its person's Servers workspace, by name.
async fn named_servers(hosted: &crate::common::Hosted) -> Vec<String> {
    let mut names: Vec<String> = servers::read(hosted.host.workspaces())
        .await
        .map(|servers| {
            servers
                .servers
                .iter()
                .map(|(_, server)| server.label.clone())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// The device that founded `hosted`'s copy of the Servers workspace `admin`.
async fn founder(hosted: &crate::common::Hosted, admin: &str) -> Option<[u8; 32]> {
    hosted
        .host
        .workspaces()
        .state(admin)
        .await
        .and_then(|state| state.governance().founder())
}

/// A Servers workspace an earlier build founded alike on each of its
/// person's devices, naming the person rather than a device as its founder, is
/// founded again once, under the first of its key holders. Every server its
/// document named stays named, and the other holder takes the new founding in
/// place of its old copy.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_servers_workspace_founded_in_its_persons_name_is_founded_again_under_one_device() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let desktop = alice.enrol("desktop", DeviceCapabilities::member());
    let one = serve_on(&alice, &laptop, WorkSetting::own_person()).await;
    let other = serve_on(&alice, &desktop, WorkSetting::own_person()).await;
    let admin = person_workspace_id(&alice.user_pk);
    servers::found(one.host.workspaces()).await.unwrap();
    for (device, hosted, label) in [(&laptop, &one, "laptop"), (&desktop, &other, "desktop")] {
        servers::apply(
            one.host.workspaces(),
            Servers::ENROLL_SERVER.with((
                hex::encode(device.device_pubkey()),
                label.into(),
                serde_json::to_string(&hosted.host.node_addr()).unwrap(),
            )),
        )
        .await
        .unwrap();
    }
    tilo_test_env::within("the other device to read the person's servers", async {
        while named_servers(&other).await != ["desktop", "laptop"] {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;

    // Both copies as the earlier build founded them; the device to found it
    // again last, so the other one's copy is the earlier build's when it is
    // handed the new founding.
    let first = laptop.device_pubkey().min(desktop.device_pubkey());
    let (later, earlier) = if first == laptop.device_pubkey() {
        (&other, &one)
    } else {
        (&one, &other)
    };
    for hosted in [later, earlier] {
        hosted
            .host
            .workspaces()
            .name_person_as_founder(&admin)
            .await;
        assert_eq!(founder(hosted, &admin).await, Some(alice.user_pk));
    }

    // The other holder takes the new copy, its founder and its records in, before the founder
    // names it among the copy's holders: that entry carries the address the holder answered the
    // hand-over with, and reaches the holder after its copy. Each device holds the new founding
    // once both have.
    tilo_test_env::within(
        "both copies to be one founding of the first holder, which names both devices its holders",
        async {
            loop {
                let mut founders = Vec::new();
                let mut named = Vec::new();
                let mut holding = Vec::new();
                for hosted in [&one, &other] {
                    founders.push(founder(hosted, &admin).await);
                    named.push(named_servers(hosted).await);
                    holding.push(servers::holds_person_workspace(hosted.host.workspaces()).await);
                }
                if founders == [Some(first), Some(first)]
                    && named.iter().all(|named| named == &["desktop", "laptop"])
                    && holding == [true, true]
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        },
    )
    .await;
}

/// The person's Servers workspace has one founder, the device that made it,
/// like any other workspace. A fresh device of theirs (one restored from the
/// recovery words, before it heard of the first) that founds it again founds
/// another history under the same id. When it hands that history to the
/// holder, the holder refuses it and keeps every server its document names,
/// and another device of the person that is handed the workspace reads them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fresh_device_that_founds_the_servers_workspace_again_replaces_no_server() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let restored = alice.enrol("restored", DeviceCapabilities::primary());
    let tablet = alice.enrol("tablet", DeviceCapabilities::member());
    let holder = serve_on(&alice, &laptop, WorkSetting::own_person()).await;
    let fresh = serve_on(&alice, &restored, WorkSetting::own_person()).await;
    let second = serve_on(&alice, &tablet, WorkSetting::own_person()).await;
    let admin = person_workspace_id(&alice.user_pk);

    // The laptop founds the workspace and names the person's servers in it.
    servers::found(holder.host.workspaces()).await.unwrap();
    for (device, label) in [(&laptop, "laptop"), (&tablet, "tablet")] {
        servers::apply(
            holder.host.workspaces(),
            Servers::ENROLL_SERVER.with((
                hex::encode(device.device_pubkey()),
                label.into(),
                serde_json::to_string(&holder.host.node_addr()).unwrap(),
            )),
        )
        .await
        .unwrap();
    }
    assert_eq!(
        founder(&holder, &admin).await,
        Some(laptop.device_pubkey()),
        "the device that made the workspace is its founder"
    );

    // The fresh device founds it again under the same id, and writes there.
    servers::found(fresh.host.workspaces()).await.unwrap();
    servers::apply(
        fresh.host.workspaces(),
        Servers::ENROLL_SERVER.with((
            hex::encode(restored.device_pubkey()),
            "restored".into(),
            serde_json::to_string(&fresh.host.node_addr()).unwrap(),
        )),
    )
    .await
    .unwrap();
    assert_eq!(
        founder(&fresh, &admin).await,
        Some(restored.device_pubkey()),
        "a second founding is founded by its own device, not in the person's name"
    );

    // It hands its history to the holder, which refuses it: that history
    // names another founder than the one the holder's document was founded
    // with.
    let handed = fresh
        .host
        .workspaces()
        .attach_server(
            &admin,
            laptop.device_pubkey(),
            holder.host.node_addr(),
            tilo_peer_state::CopyRole::KeyHolder,
        )
        .await
        .map(drop);
    assert_eq!(
        named_servers(&holder).await,
        ["laptop", "tablet"],
        "the holder keeps every server its document names (the hand-over answered {handed:?})"
    );
    assert_eq!(
        founder(&holder, &admin).await,
        Some(laptop.device_pubkey()),
        "the holder keeps the workspace its founder made"
    );

    // Another device of the person that is handed the workspace reads the
    // servers the holder keeps, and none the second founding named.
    join_person_workspace(&holder, &tablet, &second).await;
    tilo_test_env::within("the other device to read the person's servers", async {
        while named_servers(&second).await != ["laptop", "tablet"] {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert_eq!(founder(&second, &admin).await, Some(laptop.device_pubkey()));
    assert_eq!(
        named_servers(&holder).await,
        ["laptop", "tablet"],
        "the holder still names the same servers"
    );
}
