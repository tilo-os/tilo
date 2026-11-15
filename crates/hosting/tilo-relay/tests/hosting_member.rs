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

/// How long both hosts' counters stand still before the idle watch starts.
const SETTLE: std::time::Duration = std::time::Duration::from_millis(500);

/// `counters` once they have stood still for [`SETTLE`].
async fn still<F, C>(counters: F) -> C
where
    F: Fn() -> C + Send + Sync,
    C: PartialEq + Send,
{
    tilo_test_env::within("both hosts to stand still", async {
        let mut quiet = counters();
        loop {
            tokio::time::sleep(SETTLE).await;
            let now = counters();
            if now == quiet {
                break quiet;
            }
            quiet = now;
        }
    })
    .await
}

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
    // opened again, nothing is subscribed, sent or received. The watch
    // starts once the counters stand still, so what a host set in motion
    // before it (the change just carried, the address it names itself at)
    // is not read as the host doing something.
    let quiet = still(|| (governor.host.metrics(), member.host.metrics())).await;
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

/// A session of `device` to `host`, a host of `person`.
async fn session_at(
    device: &crate::common::TestDevice,
    person: &TestUser,
    host_device: &crate::common::TestDevice,
    host: &crate::common::Hosted,
) -> RelayClient {
    RelayClient::connect_v2_to_enrolled_home_peer(
        crate::common::device_endpoint(device).await,
        host.host.node_addr(),
        person.user_pk,
        host_device.device_pubkey(),
        device.cert.clone(),
        device.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap()
}

/// The device that authored the change `body` once `client` receives it;
/// the workspace's own changes before it are passed over.
async fn change_author(client: &RelayClient, body: &[u8]) -> [u8; 32] {
    tilo_test_env::within("the change arrives", async {
        loop {
            if let Envelope::Delta {
                author_device_public_key,
                delta,
                ..
            } = client.recv().await.expect("the session stays open")
                && delta == body
            {
                break author_device_public_key;
            }
        }
    })
    .await
}

/// `author`'s change `body` to `workspace_id`, sent on `client`.
fn send_change(
    client: &RelayClient,
    workspace_id: WorkspaceId,
    author: &crate::common::TestDevice,
    body: &[u8],
) {
    client
        .send(Envelope::Delta {
            workspace_id,
            author_device_public_key: author.device_pubkey(),
            authored_at_ns: 1,
            delta: body.to_vec(),
            signature: [0; 64],
        })
        .unwrap();
}

/// Another person's host takes a workspace only when its own operator's work
/// setting takes it: one taking only its own person's work refuses.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "Hosted::stop consumes each host after the assertions; the suggested earlier drop would use a moved value"
)]
async fn another_persons_host_refuses_a_workspace_its_setting_does_not_take() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let site = TestUser::fresh("Site");
    let relay = site.enrol("relay", DeviceCapabilities::member());
    let governor = host_on(&alice, &laptop, WorkSetting::none()).await;
    let workspaces = governor.host.workspaces();
    let workspace = workspaces.create("Notes").await.unwrap();
    let refusing = host_on(&site, &relay, WorkSetting::own_person()).await;
    let refused = workspaces
        .attach_host(
            &workspace.workspace,
            &laptop.cert,
            relay.device_pubkey(),
            refusing.host.node_addr(),
        )
        .await;
    assert!(refused.is_err(), "{refused:?}");
    assert_eq!(
        workspaces.hosts(&workspace.workspace).await.unwrap().len(),
        1
    );
    refusing.stop().await;
    governor.stop().await;
}

/// A host of another person whose operator takes the workspace holds it once
/// its governor grants it custody: from then on a change made through either
/// host reaches the other with its author, also after that host restarts, and
/// detached it lets the workspace go.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "Hosted::stop consumes each host after the assertions; the suggested earlier drop would use a moved value"
)]
async fn another_persons_host_holds_a_workspace_its_governor_grants_it() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let phone = alice.enrol("phone", DeviceCapabilities::primary());
    let site = TestUser::fresh("Site");
    let relay = site.enrol("relay", DeviceCapabilities::member());
    let governor = host_on(&alice, &laptop, WorkSetting::none()).await;
    let workspaces = governor.host.workspaces();
    let workspace = workspaces.create("Notes").await.unwrap();
    workspaces
        .enter(&workspace.workspace, &phone.cert)
        .await
        .unwrap();
    let mut takes = WorkSetting::own_person();
    takes.from =
        tilo_relay_protocol::WorkFrom::Workspaces([workspace.workspace_id].into_iter().collect());
    let holder = host_on(&site, &relay, takes.clone()).await;
    let hint = workspaces
        .attach_host(
            &workspace.workspace,
            &laptop.cert,
            relay.device_pubkey(),
            holder.host.node_addr(),
        )
        .await
        .unwrap();
    assert_eq!(hint.host, relay.cert);
    let hosts = workspaces.hosts(&workspace.workspace).await.unwrap();
    assert_eq!(hosts.len(), 2, "the governor and the other person's host");

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
    let on_holder = session_at(&phone, &site, &relay, &holder).await;
    tilo_test_env::within(
        "the holder admits the member its governor admitted",
        async {
            while !admitted(&on_holder, workspace.workspace_id).await {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        },
    )
    .await;

    // A change made through the holder reaches the governor, and one made on
    // the governor reaches the holder, each with its author.
    send_change(
        &on_holder,
        workspace.workspace_id,
        &phone,
        b"from the phone",
    );
    assert_eq!(
        change_author(&local, b"from the phone").await,
        phone.device_pubkey()
    );
    send_change(&local, workspace.workspace_id, &laptop, b"from the laptop");
    assert_eq!(
        change_author(&on_holder, b"from the laptop").await,
        laptop.device_pubkey()
    );

    // The holder restarts and holds the workspace again from its own record.
    drop(on_holder);
    let state = holder.stop_keeping().await;
    let holder = crate::common::host_in(&site, &relay, takes, state).await;
    let again = session_at(&phone, &site, &relay, &holder).await;
    tilo_test_env::within("the restarted holder admits the member", async {
        while !admitted(&again, workspace.workspace_id).await {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    send_change(&again, workspace.workspace_id, &phone, b"after the restart");
    assert_eq!(
        change_author(&local, b"after the restart").await,
        phone.device_pubkey()
    );

    // Detached, the holder lets the workspace go.
    assert!(
        workspaces
            .detach_host(
                &workspace.workspace,
                &laptop.cert,
                relay.device_pubkey(),
                None
            )
            .await
            .unwrap(),
        "the holder confirms it stopped"
    );
    let gone = session_at(&phone, &site, &relay, &holder).await;
    assert!(!admitted(&gone, workspace.workspace_id).await);

    drop((again, gone, local));
    holder.stop().await;
    governor.stop().await;
}
