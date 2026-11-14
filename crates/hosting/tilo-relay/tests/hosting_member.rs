//! A certified host of a person serves a workspace one of the person's devices
//! governs: the governor attaches it, the workspace's hosts name it, it admits
//! whom the workspace's governance admits as that governance changes, carries
//! members' changes to the governor, and stops when detached.

use tilo_relay::{Envelope, RelayClient};
use tilo_relay_protocol::WorkSetting;
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestUser, host_on, next_delta, session_to};
use tilo_relay::WorkspaceId;

/// How long the hosts are watched doing nothing: longer than the host's
/// first two waits before dialing again, so a session that kept ending
/// would show.
const IDLE_WINDOW: std::time::Duration = std::time::Duration::from_secs(4);

/// Whether `client`'s host admits it to `workspace_id`.
async fn admitted(client: &RelayClient, workspace_id: WorkspaceId) -> bool {
    client.send(Envelope::Subscribe { workspace_id }).unwrap();
    loop {
        match client.recv().await.expect("the session stays open") {
            Envelope::Ready {
                workspace_id: ready,
                ..
            } if ready == workspace_id => return true,
            Envelope::NotAuthorised {
                workspace_id: refused,
                ..
            } if refused == workspace_id => return false,
            _ => {}
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "Hosted::stop consumes each host after the assertions; the suggested earlier drop would use a moved value"
)]
async fn a_certified_host_of_the_person_serves_a_workspace_its_device_governs() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let phone = alice.enrol("phone", DeviceCapabilities::primary());
    let nas = alice.enrol("nas", DeviceCapabilities::member());
    let governor = host_on(&alice, &laptop, WorkSetting::none()).await;
    let member = host_on(&alice, &nas, WorkSetting::own_person()).await;
    let workspaces = governor.host.workspaces();
    let workspace = workspaces.create("Notes").await.unwrap();

    // Which hosts serve the workspace is its owner's decision: another person's
    // device asking is refused.
    let bob = TestUser::fresh("Bob");
    let bobs = bob.enrol("bob-phone", DeviceCapabilities::member());
    let refused = workspaces
        .attach_host(
            &workspace.workspace,
            &bobs.cert,
            nas.device_pubkey(),
            member.host.node_addr(),
        )
        .await;
    assert!(refused.is_err(), "{refused:?}");
    assert_eq!(
        workspaces.hosts(&workspace.workspace).await.unwrap().len(),
        1
    );

    let hint = workspaces
        .attach_host(
            &workspace.workspace,
            &laptop.cert,
            nas.device_pubkey(),
            member.host.node_addr(),
        )
        .await
        .unwrap();
    let hosts = workspaces.hosts(&workspace.workspace).await.unwrap();
    assert_eq!(hosts.len(), 2, "the governor and the host serving it");
    assert_eq!(hosts[0].host, laptop.cert, "the governor comes first");
    assert_eq!(hosts[1], hint);

    // The phone enters the workspace on the laptop; the host follows that
    // governance and admits it.
    workspaces
        .enter(&workspace.workspace, &phone.cert)
        .await
        .unwrap();
    let on_member = session_to(&alice, &phone, &member).await;
    tilo_test_env::within("the host admits the member its governor admitted", async {
        while !admitted(&on_member, workspace.workspace_id).await {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;

    // A change the phone sends the host reaches the governor's own session.
    let local = governor
        .host
        .connect_local(
            laptop.cert.clone(),
            laptop.sk.clone(),
            RevocationList::new(),
        )
        .await
        .unwrap();
    assert!(admitted(&local, workspace.workspace_id).await);
    on_member
        .send(Envelope::Delta {
            workspace_id: workspace.workspace_id,
            author_device_public_key: phone.device_pubkey(),
            authored_at_ns: 1,
            delta: b"from the phone, through the host".to_vec(),
            signature: [0; 64],
        })
        .unwrap();
    assert_eq!(
        next_delta(&local).await,
        b"from the phone, through the host"
    );

    // With nothing changing, neither host does anything: no session is
    // opened again, nothing is subscribed, sent or received.
    let quiet = (governor.host.metrics(), member.host.metrics());
    tokio::time::sleep(IDLE_WINDOW).await;
    assert_eq!((governor.host.metrics(), member.host.metrics()), quiet);

    // Detached, the host is no longer named and admits nobody to the workspace.
    assert!(
        workspaces
            .detach_host(
                &workspace.workspace,
                &laptop.cert,
                nas.device_pubkey(),
                None
            )
            .await
            .unwrap(),
        "the host confirms it stopped"
    );
    assert_eq!(
        workspaces.hosts(&workspace.workspace).await.unwrap().len(),
        1
    );
    let again = session_to(&alice, &phone, &member).await;
    assert!(!admitted(&again, workspace.workspace_id).await);

    drop((on_member, local, again));
    member.stop().await;
    governor.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "Hosted::stop consumes each host after the assertions; the suggested earlier drop would use a moved value"
)]
async fn a_device_serves_no_workspace_another_device_governs() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let desktop = alice.enrol("desktop", DeviceCapabilities::primary());
    let governor = host_on(&alice, &laptop, WorkSetting::none()).await;
    let other = host_on(&alice, &desktop, WorkSetting::none()).await;
    let workspaces = governor.host.workspaces();
    let workspace = workspaces.create("Notes").await.unwrap();

    let refused = workspaces
        .attach_host(
            &workspace.workspace,
            &laptop.cert,
            desktop.device_pubkey(),
            other.host.node_addr(),
        )
        .await;
    assert!(refused.is_err(), "{refused:?}");
    assert_eq!(
        workspaces.hosts(&workspace.workspace).await.unwrap().len(),
        1
    );
    other.stop().await;
    governor.stop().await;
}

/// A host the workspace's owner detaches lets the workspace go: it no longer holds it
/// open, though its files stay. Attached again, it serves the workspace from
/// those files and catches up on what the workspace published meanwhile.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "Hosted::stop consumes each host after the assertions; the suggested earlier drop would use a moved value"
)]
async fn a_detached_host_lets_the_workspace_go_and_serves_it_again_from_its_files() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let phone = alice.enrol("phone", DeviceCapabilities::primary());
    let tablet = alice.enrol("tablet", DeviceCapabilities::primary());
    let nas = alice.enrol("nas", DeviceCapabilities::member());
    let governor = host_on(&alice, &laptop, WorkSetting::none()).await;
    let member = host_on(&alice, &nas, WorkSetting::own_person()).await;
    let workspaces = governor.host.workspaces();
    let workspace = workspaces.create("Notes").await.unwrap();
    let records = member.host.workspace_records();
    let retained = || async { records.retained(workspace.workspace_id).await.ok() };

    workspaces
        .attach_host(
            &workspace.workspace,
            &laptop.cert,
            nas.device_pubkey(),
            member.host.node_addr(),
        )
        .await
        .unwrap();
    workspaces
        .enter(&workspace.workspace, &phone.cert)
        .await
        .unwrap();
    let kept = tilo_test_env::within("the host to keep the workspace's changes", async {
        loop {
            if let Some(bodies) = retained().await.filter(|bodies| !bodies.is_empty()) {
                break bodies.len();
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;

    assert!(
        workspaces
            .detach_host(
                &workspace.workspace,
                &laptop.cert,
                nas.device_pubkey(),
                None
            )
            .await
            .unwrap()
    );
    assert!(retained().await.is_none(), "the host let the workspace go");

    // The workspace moves on while the host is away.
    workspaces
        .enter(&workspace.workspace, &tablet.cert)
        .await
        .unwrap();

    workspaces
        .attach_host(
            &workspace.workspace,
            &laptop.cert,
            nas.device_pubkey(),
            member.host.node_addr(),
        )
        .await
        .unwrap();
    assert!(
        retained().await.is_some_and(|bodies| bodies.len() >= kept),
        "served again from what it kept"
    );
    tilo_test_env::within("the host to catch up", async {
        while retained().await.is_none_or(|bodies| bodies.len() <= kept) {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    let on_member = session_to(&alice, &tablet, &member).await;
    tilo_test_env::within("the host to admit the device entered meanwhile", async {
        while !admitted(&on_member, workspace.workspace_id).await {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;

    drop(on_member);
    member.stop().await;
    governor.stop().await;
}
