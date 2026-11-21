//! A server does what its entry in its person's admin workspace says: it holds
//! the workspaces the entry names and lets go of the ones it stops naming,
//! offers the work its entry allows, and follows the update policy its entry
//! names.

#![allow(
    clippy::significant_drop_tightening,
    reason = "hosts, routers and fixtures deliberately stay alive until each test ends"
)]

use tilo_resource::Id;
use tilo_server::servers::{self, Servers, admin_workspace_id, applied};
use tilo_server::workspaces::Offer;
use tilo_server_protocol::WorkSetting;
use tilo_user_identity::DeviceCapabilities;

use crate::common::{TestUser, serve_on};

/// The document's entry id of the device `device`.
fn entry(device: &crate::common::TestDevice) -> Id {
    Id::from_raw(hex::encode(device.device_pubkey()))
}

/// Choosing a server as a holder in the admin workspace is the one action an
/// owner takes: the device that governs the workspace hands it to
/// the server and names it, and naming it no more lets it go.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_server_holds_and_releases_the_workspaces_its_entry_names() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let nas = alice.enrol("nas", DeviceCapabilities::member());
    let governor = serve_on(&alice, &laptop, WorkSetting::none()).await;
    let server = serve_on(&alice, &nas, WorkSetting::own_person()).await;
    let workspaces = governor.host.workspaces();
    let admin = admin_workspace_id(&alice.user_pk);
    workspaces
        .create_workspace("Servers", Some(&admin))
        .await
        .unwrap();
    // The server enrolled itself on its first start: its entry names the
    // address it is dialled at.
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

    let workspace = workspaces.create("Notes").await.unwrap();
    let retained = || async {
        server
            .host
            .workspace_documents()
            .retained(workspace.workspace_id)
            .await
            .ok()
    };
    assert!(retained().await.is_none(), "it holds nothing yet");

    servers::apply(
        workspaces,
        Servers::HOLD_WORKSPACE.with((
            entry(&nas),
            workspace.workspace.clone(),
            servers::KEY_HOLDER.into(),
        )),
    )
    .await
    .unwrap();
    // Every holder of the Servers workspace applies its document, so the
    // owner's device hands it over as the change is written.
    applied::reconcile(workspaces).await.unwrap();
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
    assert_eq!(
        servers::read(workspaces).await.unwrap().servers.len(),
        1,
        "the document still names the server"
    );
    // Naming it no more in its entry lets the workspace go.
    servers::apply(
        workspaces,
        Servers::RELEASE_WORKSPACE.with((entry(&nas), workspace.workspace.clone())),
    )
    .await
    .unwrap();
    let after = servers::read(workspaces).await.unwrap();
    let nas_entry = after.servers.get(&entry(&nas)).cloned().unwrap();
    assert_eq!(
        nas_entry.holding(&workspace.workspace),
        None,
        "the release took: {nas_entry:?}"
    );
    applied::reconcile(workspaces).await.unwrap();
    tilo_test_env::within("the server to let go of the workspace", async {
        while retained().await.is_some() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
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
    let admin = admin_workspace_id(&alice.user_pk);
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
    assert!(!workspaces.offer().compute, "it runs no work yet");

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
    assert_eq!(workspaces.offer(), Offer::of(500, true, 2));
}

/// A retired server lets go of everything it holds and offers nothing, and
/// keeps its entry so it is never taken in again as a new server.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_retired_server_lets_go_of_everything() {
    let alice = TestUser::fresh("Alice");
    let nas = alice.enrol("nas", DeviceCapabilities::member());
    let server = serve_on(&alice, &nas, WorkSetting::own_person()).await;
    let policy = tempfile::tempdir().unwrap();
    let policy_request = policy.path().join("update-policy.request");
    let workspaces = server.host.workspaces();
    let workspace = workspaces.create("Kept").await.unwrap();
    let admin = admin_workspace_id(&alice.user_pk);
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
    assert!(workspaces.offer().compute);

    servers::apply(workspaces, Servers::RETIRE_SERVER.with(entry(&nas)))
        .await
        .unwrap();
    let applied = applied::reconcile_into(workspaces, &applied::Requests::at(&policy_request))
        .await
        .unwrap();
    assert!(applied.retired, "its own entry says it was retired");
    assert!(
        !workspaces.offer().compute,
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
    let admin = admin_workspace_id(&alice.user_pk);
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
    let admin = admin_workspace_id(&alice.user_pk);
    let [first, second, third] = [&hosts[0], &hosts[1], &hosts[2]];

    // One of the person's own devices makes it when it is first needed, and
    // the first server is entered in it: no server founds it.
    servers::founded(first.host.workspaces()).await.unwrap();
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
                    if servers::holds_admin(asking.host.workspaces()).await && named == enrolled {
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
        while first.host.workspaces().offer() != Offer::of(500, true, 2) {
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
    servers::founded(holder.host.workspaces()).await.unwrap();
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
    servers::take_in(
        holder.host.workspaces(),
        &desktop.cert,
        &late.host.node_addr(),
    )
    .await
    .unwrap();
    tilo_test_env::within("the other device to hold the workspace", async {
        while !servers::holds_admin(late.host.workspaces()).await {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;

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
    let admin = admin_workspace_id(&alice.user_pk);

    // The laptop founds the workspace and names the person's servers in it.
    servers::founded(holder.host.workspaces()).await.unwrap();
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
    servers::founded(fresh.host.workspaces()).await.unwrap();
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
    let handed = servers::take_in(
        fresh.host.workspaces(),
        &laptop.cert,
        &holder.host.node_addr(),
    )
    .await;
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
    servers::take_in(
        holder.host.workspaces(),
        &tablet.cert,
        &second.host.node_addr(),
    )
    .await
    .unwrap();
    tilo_test_env::within("the other device to read the person's servers", async {
        while named_servers(&second).await != ["laptop", "tablet"] {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
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
