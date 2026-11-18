//! A certified holder of a person serves a workspace one of the person's devices
//! governs: the governor attaches it, the workspace's holders name it, it admits
//! whom the workspace's governance admits as that governance changes, carries
//! members' changes to every other holder, and stops when detached.

use tilo_server::{Envelope, ServerClient};
use tilo_server_protocol::{WorkFrom, WorkSetting};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestUser, host_on, receives, session_to};
use tilo_server::WorkspaceId;

/// How long the holders are watched doing nothing: longer than the holder's
/// first two waits before dialing again, so a session that kept ending
/// would show.
const IDLE_WINDOW: std::time::Duration = std::time::Duration::from_secs(4);

/// How long both holders' counters stand still before the idle watch starts.
const SETTLE: std::time::Duration = std::time::Duration::from_millis(500);

/// How long a refused session may take to end: shorter than the second a
/// site's first access audit waits, so a close inside it came with the
/// refusal and not from the audit.
const REFUSED_CLOSE: std::time::Duration = std::time::Duration::from_millis(750);

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

/// Whether `client`'s holder admits it to `workspace_id`.
async fn admitted(client: &ServerClient, workspace_id: WorkspaceId) -> bool {
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

    let hint = workspaces
        .attach_host(
            &workspace.workspace,
            nas.device_pubkey(),
            member.host.node_addr(),
        )
        .await
        .unwrap();
    let hosts = workspaces.hosts(&workspace.workspace).await.unwrap();
    assert_eq!(hosts.len(), 2, "the governor and the host serving it");
    assert!(
        hosts.iter().any(|held| held.host == laptop.cert),
        "the governor holds it: {hosts:?}"
    );
    assert!(
        hosts.contains(&hint),
        "and the host it handed it to: {hosts:?}"
    );

    // The phone enters the workspace on the laptop; the holder follows that
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

    // A change the phone sends the holder reaches the governor's own session.
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
    receives(&local, b"from the phone, through the host").await;

    // With nothing changing, neither holder does anything: no session is
    // opened again, nothing is subscribed, sent or received. The watch
    // starts once the counters stand still, so what a holder set in motion
    // before it (the change just carried, the address it names itself at)
    // is not read as the host doing something.
    let quiet = still(|| (governor.host.metrics(), member.host.metrics())).await;
    tokio::time::sleep(IDLE_WINDOW).await;
    assert_eq!((governor.host.metrics(), member.host.metrics()), quiet);

    // Detached, the host is no longer named and admits nobody to the workspace.
    assert!(
        workspaces
            .detach_host(&workspace.workspace, nas.device_pubkey(), None)
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
    let records = member.host.workspace_documents();
    let retained = || async { records.retained(workspace.workspace_id).await.ok() };

    workspaces
        .attach_host(
            &workspace.workspace,
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
            .detach_host(&workspace.workspace, nas.device_pubkey(), None)
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

/// Every holder of a workspace carries what is written through it to all the
/// others, and none of them is needed: with three holders, whichever one goes
/// away, a device admitted through either other holder reaches the third,
/// changes written through either reach the other, and both say the same of
/// who holds the workspace and who is in it. The holder that went away comes
/// back from its files before the next one goes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one scenario per lost holder: its steps and assertions stay together"
)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "Hosted::stop consumes each host after the assertions"
)]
async fn losing_any_one_of_three_holders_leaves_the_workspace_writable_and_governed() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let phone = alice.enrol("phone", DeviceCapabilities::primary());
    let tablet = alice.enrol("tablet", DeviceCapabilities::primary());
    let first = alice.enrol("nas", DeviceCapabilities::member());
    let second = alice.enrol("server", DeviceCapabilities::member());
    let devices = [&laptop, &first, &second];
    let settings = [
        WorkSetting::none(),
        WorkSetting::own_person(),
        WorkSetting::own_person(),
    ];
    let mut holders = Vec::new();
    for (device, work) in devices.iter().zip(&settings) {
        holders.push(Some(host_on(&alice, device, work.clone()).await));
    }
    let made_on = holders[0].as_ref().unwrap().host.workspaces();
    let workspace = made_on.create("Notes").await.unwrap();
    for at in 1..3 {
        made_on
            .attach_host(
                &workspace.workspace,
                devices[at].device_pubkey(),
                holders[at].as_ref().unwrap().host.node_addr(),
            )
            .await
            .unwrap();
    }
    assert_eq!(made_on.hosts(&workspace.workspace).await.unwrap().len(), 3);
    for device in [&phone, &tablet] {
        made_on
            .enter(&workspace.workspace, &device.cert)
            .await
            .unwrap();
    }

    for gone in 0..3 {
        let state = holders[gone].take().unwrap().stop_keeping().await;
        let [one, two] = [(gone + 1) % 3, (gone + 2) % 3].map(|at| holders[at].as_ref().unwrap());

        // Who is in the workspace still changes, through either holder.
        let newcomer = alice.enrol(&format!("device {gone}"), DeviceCapabilities::primary());
        one.host
            .workspaces()
            .enter(&workspace.workspace, &newcomer.cert)
            .await
            .unwrap();
        let on_two = session_to(&alice, &newcomer, two).await;
        tilo_test_env::within("the other holder admits the device", async {
            while !admitted(&on_two, workspace.workspace_id).await {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await;

        // And what members write reaches each through the other.
        let on_one = session_to(&alice, &phone, one).await;
        assert!(admitted(&on_one, workspace.workspace_id).await);
        tilo_test_env::within(
            "a change through one holder reaches the other",
            carried(
                &on_one,
                &phone,
                &on_two,
                workspace.workspace_id,
                b"through one",
            ),
        )
        .await;
        tilo_test_env::within(
            "and back",
            carried(
                &on_two,
                &newcomer,
                &on_one,
                workspace.workspace_id,
                b"through two",
            ),
        )
        .await;

        // Both say the same of who holds the workspace and who is in it.
        tilo_test_env::within("both holders agree", async {
            loop {
                let said = |holder: &crate::common::Hosted| {
                    let workspaces = holder.host.workspaces().clone();
                    let workspace = workspace.workspace.clone();
                    async move {
                        let mut hosts: Vec<[u8; 32]> = workspaces
                            .hosts(&workspace)
                            .await
                            .unwrap()
                            .iter()
                            .map(|hint| hint.host.device_public_key)
                            .collect();
                        hosts.sort_unstable();
                        let summary = workspaces.summary(&workspace).await.unwrap();
                        (hosts, summary.members, summary.devices)
                    }
                };
                let (on_one, on_two) = (said(one).await, said(two).await);
                if on_one == on_two {
                    assert_eq!(on_one.0.len(), 3, "every holder is still named");
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await;
        drop((on_one, on_two));

        holders[gone] = Some(
            crate::common::host_in(&alice, devices[gone], settings[gone].clone(), state).await,
        );
    }

    for holder in holders.into_iter().flatten() {
        holder.stop().await;
    }
}

/// Send `body`, `author`'s change, on `from` and expect it on `to`.
async fn carried(
    from: &ServerClient,
    author: &crate::common::TestDevice,
    to: &ServerClient,
    workspace_id: WorkspaceId,
    body: &[u8],
) {
    from.send(Envelope::Delta {
        workspace_id,
        author_device_public_key: author.device_pubkey(),
        authored_at_ns: 1,
        delta: body.to_vec(),
        signature: [0; 64],
    })
    .unwrap();
    receives(to, body).await;
}

/// The devices a held workspace's governance names besides its owner's own:
/// a workspace people have used for a while, not a fresh one.
const HELD_DEVICES: usize = 300;

/// `workspace` as a workspace in use for a while is: its governance names
/// hundreds of devices, and its history holds each admission, all of which a
/// host it is handed to takes in.
async fn used_for_a_while(
    workspaces: &tilo_server::workspaces::Workspaces,
    person: &TestUser,
    workspace: &str,
) {
    for device in 0..HELD_DEVICES {
        let device = person.enrol(&format!("device {device}"), DeviceCapabilities::member());
        workspaces.enter(workspace, &device.cert).await.unwrap();
    }
}

/// A session of `device` to `host`, a host of `person`.
async fn session_at(
    device: &crate::common::TestDevice,
    person: &TestUser,
    host_device: &crate::common::TestDevice,
    host: &crate::common::Hosted,
) -> ServerClient {
    ServerClient::connect_v2_to_enrolled_home_peer(
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
async fn change_author(client: &ServerClient, body: &[u8]) -> [u8; 32] {
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
    client: &ServerClient,
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

/// Another person's host that keeps no workspaces for the people it lets in
/// takes a workspace only when its own operator's work setting takes it: one
/// taking only its own person's work refuses.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "Hosted::stop consumes each host after the assertions; the suggested earlier drop would use a moved value"
)]
async fn another_persons_host_refuses_a_workspace_its_setting_does_not_take() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let site = TestUser::fresh("Site");
    let server = site.enrol("server", DeviceCapabilities::member());
    let governor = host_on(&alice, &laptop, WorkSetting::none()).await;
    let workspaces = governor.host.workspaces();
    let workspace = workspaces.create("Notes").await.unwrap();
    let refusing = host_on(&site, &server, WorkSetting::own_person()).await;
    let refused = workspaces
        .attach_host(
            &workspace.workspace,
            server.device_pubkey(),
            refusing.host.node_addr(),
        )
        .await;
    // The owner hears why: the host keeps nobody's workspace but its own
    // person's.
    assert!(
        refused
            .as_ref()
            .is_err_and(|error| error.contains("keeps workspaces only for the people it let in")),
        "{refused:?}"
    );
    assert_eq!(
        workspaces.hosts(&workspace.workspace).await.unwrap().len(),
        1
    );
    refusing.stop().await;
    governor.stop().await;
}

/// A host of another person whose operator takes the workspace holds it once
/// its governor grants it ownership: from then on a change made through either
/// host reaches the other with its author, also after that host restarts, and
/// detached it lets the workspace go.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "Hosted::stop consumes each host after the assertions; the suggested earlier drop would use a moved value"
)]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end scenario: its setup and assertions stay together"
)]
async fn another_persons_host_holds_a_workspace_its_governor_grants_it() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let phone = alice.enrol("phone", DeviceCapabilities::primary());
    let site = TestUser::fresh("Site");
    let server = site.enrol("server", DeviceCapabilities::member());
    let governor = host_on(&alice, &laptop, WorkSetting::none()).await;
    let workspaces = governor.host.workspaces();
    let workspace = workspaces.create("Notes").await.unwrap();
    workspaces
        .enter(&workspace.workspace, &phone.cert)
        .await
        .unwrap();
    used_for_a_while(workspaces, &alice, &workspace.workspace).await;
    let mut takes = WorkSetting::own_person();
    takes.from =
        tilo_server_protocol::WorkFrom::Workspaces([workspace.workspace_id].into_iter().collect());
    let holder = host_on(&site, &server, takes.clone()).await;
    let hint = workspaces
        .attach_host(
            &workspace.workspace,
            server.device_pubkey(),
            holder.host.node_addr(),
        )
        .await
        .unwrap();
    assert_eq!(hint.host, server.cert);
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
    let on_holder = session_at(&phone, &site, &server, &holder).await;
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
    let holder = crate::common::host_in(&site, &server, takes, state).await;
    let again = session_at(&phone, &site, &server, &holder).await;
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
            .detach_host(&workspace.workspace, server.device_pubkey(), None)
            .await
            .unwrap(),
        "the holder confirms it stopped"
    );
    // It keeps nothing of the person's any more, so it answers them no
    // session at all, or one that refuses the workspace.
    let gone = session_at(&phone, &site, &server, &holder).await;
    gone.send(Envelope::Subscribe {
        workspace_id: workspace.workspace_id,
    })
    .unwrap();
    let refused = tilo_test_env::within("the holder answers", async {
        loop {
            match gone.recv().await {
                None => break true,
                Some(Envelope::Ready {
                    workspace_id: ready,
                    ..
                }) if ready == workspace.workspace_id => break false,
                Some(Envelope::NotAuthorised {
                    workspace_id: refused,
                    ..
                }) if refused == workspace.workspace_id => break true,
                Some(_) => {}
            }
        }
    })
    .await;
    assert!(refused, "a detached holder still admits the member");

    drop((again, gone, local));
    holder.stop().await;
    governor.stop().await;
}

/// A host that keeps workspaces for the people it lets in takes a workspace
/// another host governs for an owner of it it let in, and for nobody else:
/// asked before it let the owner in, it refuses and says why, and answers her
/// device's subscribe with `NotAdmitted` and nothing else (no workspace data,
/// members or keys), ending the session with that refusal; asked after, it
/// holds the workspace and a member's change through it reaches the governor.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "Hosted::stop consumes each host after the assertions; the suggested earlier drop would use a moved value"
)]
async fn a_host_holds_a_workspace_for_an_owner_it_let_in() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let phone = alice.enrol("phone", DeviceCapabilities::primary());
    let site = TestUser::fresh("Site");
    let server = site.enrol("server", DeviceCapabilities::member());
    let governor = host_on(&alice, &laptop, WorkSetting::none()).await;
    let workspaces = governor.host.workspaces();
    let workspace = workspaces.create("Notes").await.unwrap();
    workspaces
        .enter(&workspace.workspace, &phone.cert)
        .await
        .unwrap();
    let holder = host_on(&site, &server, WorkSetting::own_person()).await;
    let at_site = holder.host.workspaces();
    assert!(
        at_site
            .offer_own_workspaces(tilo_server::workspaces::OwnWorkspaceOffer::free(
                "site.test"
            ))
            .unwrap()
    );
    let home = at_site.create("Home").await.unwrap();
    assert!(at_site.admit_own_through(&home.workspace));

    let attach = || {
        workspaces.attach_host(
            &workspace.workspace,
            server.device_pubkey(),
            holder.host.node_addr(),
        )
    };
    let refused = attach().await;
    assert!(
        refused
            .as_ref()
            .is_err_and(|error| error.contains("site.test")),
        "the site says it has not let Alice in: {refused:?}"
    );
    assert_eq!(
        workspaces.hosts(&workspace.workspace).await.unwrap().len(),
        1
    );
    let outside = session_at(&phone, &site, &server, &holder).await;
    outside
        .send(Envelope::Subscribe {
            workspace_id: home.workspace_id,
        })
        .unwrap();
    assert_eq!(
        tilo_test_env::within("the site answers", outside.recv()).await,
        Some(Envelope::NotAuthorised {
            workspace_id: home.workspace_id,
            refusal: tilo_server_protocol::refusal::Refusal::NotAdmitted,
        })
    );
    // The refusal is the session's last word: a device the site never let in
    // holds no workspace there, so the site ends the session with it instead
    // of keeping it open.
    let after = tokio::time::timeout(REFUSED_CLOSE, outside.recv()).await;
    assert!(
        matches!(after, Ok(None)),
        "a device the site never let in is still connected: {after:?}"
    );

    // The site lets Alice in, as an owner of its home; now it takes hers.
    at_site
        .declare_owner(&home.workspace, alice.user_pk, None)
        .await
        .unwrap();
    let hint = attach().await.unwrap();
    assert_eq!(hint.host, server.cert);
    assert_eq!(
        workspaces.hosts(&workspace.workspace).await.unwrap().len(),
        2
    );
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
    let on_holder = session_at(&phone, &site, &server, &holder).await;
    tilo_test_env::within("the site admits the workspace's member", async {
        while !admitted(&on_holder, workspace.workspace_id).await {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    send_change(&on_holder, workspace.workspace_id, &phone, b"from the site");
    assert_eq!(
        change_author(&local, b"from the site").await,
        phone.device_pubkey()
    );

    drop((on_holder, local, outside));
    holder.stop().await;
    governor.stop().await;
}

/// A device that is handed a workspace keeps its files under the
/// workspace's id. A hand-over that names its workspace by anything else (a
/// path that climbs out of the folder, an absolute path, a name with a
/// separator in it, another workspace's id) is refused, and nothing is
/// written anywhere for it. Under the workspace's own id the same hand-over
/// is taken, and its files are the only ones kept.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "Hosted::stop consumes each device after the assertions; the suggested earlier drop would use a moved value"
)]
async fn a_hand_over_names_its_workspace_by_its_id_and_writes_nowhere_else() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let nas = alice.enrol("nas", DeviceCapabilities::member());
    let governor = host_on(&alice, &laptop, WorkSetting::none()).await;
    let state = tempfile::tempdir().unwrap();
    let root = state.path().to_path_buf();
    let member = crate::common::host_in(&alice, &nas, WorkSetting::own_person(), state).await;
    let at = member.host.node_addr();
    let workspaces = governor.host.workspaces();
    let workspace = workspaces.create("Notes").await.unwrap();

    // Folders a written name could point into: on a device that keeps many
    // people's files, another person's lie beside this one.
    let kept = root.join("workspaces");
    std::fs::create_dir_all(kept.join("inner")).unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let another = WorkspaceId::new().to_string();
    let names = [
        ("../outside".to_owned(), root.join("outside.workspace")),
        (
            elsewhere.path().join("planted").display().to_string(),
            elsewhere.path().join("planted.workspace"),
        ),
        (
            "inner/within".to_owned(),
            kept.join("inner").join("within.workspace"),
        ),
        (another.clone(), kept.join(format!("{another}.workspace"))),
    ];
    for (name, written) in &names {
        let answered = workspaces
            .hand_over_named(&workspace.workspace, nas.device_pubkey(), at.clone(), name)
            .await;
        assert!(
            answered.is_err(),
            "a hand-over that names its workspace {name:?} was taken: {answered:?}"
        );
        assert!(
            !written.exists(),
            "a hand-over that names its workspace {name:?} wrote {}",
            written.display()
        );
    }

    workspaces
        .attach_host(&workspace.workspace, nas.device_pubkey(), at)
        .await
        .unwrap();
    let files: Vec<String> = std::fs::read_dir(&kept)
        .unwrap()
        .flatten()
        .filter(|entry| entry.path().is_file())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        files.contains(&format!("{}.workspace", workspace.workspace))
            && files
                .iter()
                .all(|file| file.starts_with(&workspace.workspace)),
        "{files:?}"
    );
    assert!(
        std::fs::read_dir(elsewhere.path())
            .unwrap()
            .next()
            .is_none(),
        "a hand-over wrote outside the folder"
    );

    member.stop().await;
    governor.stop().await;
}

/// A workspace a device holds for one governor is not taken from another.
/// Someone else who makes a workspace under the same id on a device of
/// their own and hands that over is refused, and the people the first
/// governor's workspace admits are admitted as before.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "Hosted::stop consumes each device after the assertions; the suggested earlier drop would use a moved value"
)]
async fn a_held_workspace_is_not_taken_from_another_governor() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let phone = alice.enrol("phone", DeviceCapabilities::primary());
    let mallory = TestUser::fresh("Mallory");
    let hers = mallory.enrol("laptop", DeviceCapabilities::primary());
    let site = TestUser::fresh("Site");
    let kept_on = site.enrol("server", DeviceCapabilities::member());
    let governor = host_on(&alice, &laptop, WorkSetting::none()).await;
    let workspaces = governor.host.workspaces();
    let workspace = workspaces.create("Notes").await.unwrap();
    workspaces
        .enter(&workspace.workspace, &phone.cert)
        .await
        .unwrap();
    // The site's operator takes that workspace, whoever hands it over.
    let mut takes = WorkSetting::own_person();
    takes.from = WorkFrom::Workspaces([workspace.workspace_id].into_iter().collect());
    let holder = host_on(&site, &kept_on, takes).await;
    let at = holder.host.node_addr();
    workspaces
        .attach_host(&workspace.workspace, kept_on.device_pubkey(), at.clone())
        .await
        .unwrap();
    let on_holder = session_at(&phone, &site, &kept_on, &holder).await;
    tilo_test_env::within("the site admits the workspace's member", async {
        while !admitted(&on_holder, workspace.workspace_id).await {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;

    // Mallory's own device governs a workspace she made under Alice's id.
    let second = host_on(&mallory, &hers, WorkSetting::none()).await;
    let of_mallory = second.host.workspaces();
    of_mallory
        .create_workspace("Notes", Some(&workspace.workspace))
        .await
        .unwrap();
    let answered = of_mallory
        .attach_host(&workspace.workspace, kept_on.device_pubkey(), at)
        .await;
    assert!(
        answered
            .as_ref()
            .is_err_and(|error| error.contains("one of its holders")),
        "a workspace held for its governor was taken from somebody else: {answered:?}"
    );
    assert!(
        admitted(&on_holder, workspace.workspace_id).await,
        "the member the first governor's workspace admits is admitted no more"
    );

    drop(on_holder);
    second.stop().await;
    holder.stop().await;
    governor.stop().await;
}
