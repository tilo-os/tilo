//! A certified holder of a person serves a workspace one of the person's devices
//! governs: the governor attaches it, the workspace's holders name it, it admits
//! whom the workspace's governance admits as that governance changes, carries
//! members' changes to every other holder, and stops when detached.

use tilo_peer_state::CopyRole;
use tilo_server::{Envelope, ServerClient};
use tilo_server_protocol::{WorkFrom, WorkSetting};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestUser, receives, serve_on, session_to};
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
    tilo_test_env::within("both holders to stand still", async {
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
async fn a_certified_host_of_the_person_serves_a_workspace_its_device_governs() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let phone = alice.enrol("phone", DeviceCapabilities::primary());
    let nas = alice.enrol("nas", DeviceCapabilities::member());
    let governor = serve_on(&alice, &laptop, WorkSetting::none()).await;
    let member = serve_on(&alice, &nas, WorkSetting::own_person()).await;
    let workspaces = governor.host.workspaces();
    let workspace = workspaces.create("Notes").await.unwrap();

    let hint = workspaces
        .attach_server(
            &workspace.workspace,
            nas.device_pubkey(),
            member.host.node_addr(),
            CopyRole::KeyHolder,
        )
        .await
        .unwrap();
    let hosts = workspaces.hosts(&workspace.workspace).await.unwrap();
    assert_eq!(hosts.len(), 2, "the governor and the holder serving it");
    assert!(
        hosts.iter().any(|held| held.host == laptop.cert),
        "the governor holds it: {hosts:?}"
    );
    assert!(
        hosts.contains(&hint),
        "and the holder it handed it to: {hosts:?}"
    );

    // The phone enters the workspace on the laptop; the holder follows that
    // governance and admits it.
    workspaces
        .enter(&workspace.workspace, &phone.cert)
        .await
        .unwrap();
    let on_member = session_to(&alice, &phone, &member).await;
    tilo_test_env::within(
        "the holder admits the member its governor admitted",
        async {
            while !admitted(&on_member, workspace.workspace_id).await {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        },
    )
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
            delta: b"from the phone, through the holder".to_vec(),
            signature: [0; 64],
        })
        .unwrap();
    receives(&local, b"from the phone, through the holder").await;

    // With nothing changing, neither holder does anything: no session is
    // opened again, nothing is subscribed, sent or received. The watch
    // starts once the counters stand still, so what a holder set in motion
    // before it (the change just carried, the address it names itself at)
    // is not read as the holder doing something.
    let quiet = still(|| (governor.host.metrics(), member.host.metrics())).await;
    tokio::time::sleep(IDLE_WINDOW).await;
    assert_eq!((governor.host.metrics(), member.host.metrics()), quiet);

    // Detached, the holder is no longer named and admits nobody to the workspace.
    assert!(
        workspaces
            .detach_host(&workspace.workspace, nas.device_pubkey(), None)
            .await
            .unwrap(),
        "the holder confirms it stopped"
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
async fn every_device_of_the_governing_person_holds_the_workspace_and_no_one_elses_does() {
    let alice = TestUser::fresh("Alice");
    let bob = TestUser::fresh("Bob");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let desktop = alice.enrol("desktop", DeviceCapabilities::primary());
    let bobs = bob.enrol("desktop", DeviceCapabilities::primary());
    let governor = serve_on(&alice, &laptop, WorkSetting::none()).await;
    let own = serve_on(&alice, &desktop, WorkSetting::none()).await;
    let others = serve_on(&bob, &bobs, WorkSetting::none()).await;
    let workspaces = governor.host.workspaces();
    let workspace = workspaces.create("Notes").await.unwrap();

    // Another device of the person who governs the workspace holds it, though
    // it takes no work from anyone: the workspace is the person's, not the
    // device's it was made on.
    let held = workspaces
        .attach_server(
            &workspace.workspace,
            desktop.device_pubkey(),
            own.host.node_addr(),
            CopyRole::KeyHolder,
        )
        .await;
    assert!(held.is_ok(), "{held:?}");
    // A device of someone who does not govern it, and takes no one's work,
    // does not.
    let refused = workspaces
        .attach_server(
            &workspace.workspace,
            bobs.device_pubkey(),
            others.host.node_addr(),
            CopyRole::KeyHolder,
        )
        .await;
    assert!(refused.is_err(), "{refused:?}");
    assert_eq!(
        workspaces.hosts(&workspace.workspace).await.unwrap().len(),
        2
    );
    others.stop().await;
    own.stop().await;
    governor.stop().await;
}

/// A holder the workspace's owner detaches lets the workspace go: it no longer holds it
/// open, though its files stay. Attached again, it serves the workspace from
/// those files and catches up on what the workspace published meanwhile.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_detached_host_lets_the_workspace_go_and_serves_it_again_from_its_files() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let phone = alice.enrol("phone", DeviceCapabilities::primary());
    let tablet = alice.enrol("tablet", DeviceCapabilities::primary());
    let nas = alice.enrol("nas", DeviceCapabilities::member());
    let governor = serve_on(&alice, &laptop, WorkSetting::none()).await;
    let member = serve_on(&alice, &nas, WorkSetting::own_person()).await;
    let workspaces = governor.host.workspaces();
    let workspace = workspaces.create("Notes").await.unwrap();
    let records = member.host.workspace_documents();
    let retained = || async { records.retained(workspace.workspace_id).await.ok() };

    workspaces
        .attach_server(
            &workspace.workspace,
            nas.device_pubkey(),
            member.host.node_addr(),
            CopyRole::KeyHolder,
        )
        .await
        .unwrap();
    workspaces
        .enter(&workspace.workspace, &phone.cert)
        .await
        .unwrap();
    let kept = tilo_test_env::within("the holder to keep the workspace's changes", async {
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
    assert!(
        retained().await.is_none(),
        "the holder let the workspace go"
    );

    // The workspace moves on while the holder is away.
    workspaces
        .enter(&workspace.workspace, &tablet.cert)
        .await
        .unwrap();

    workspaces
        .attach_server(
            &workspace.workspace,
            nas.device_pubkey(),
            member.host.node_addr(),
            CopyRole::KeyHolder,
        )
        .await
        .unwrap();
    assert!(
        retained().await.is_some_and(|bodies| bodies.len() >= kept),
        "served again from what it kept"
    );
    tilo_test_env::within("the holder to catch up", async {
        while retained().await.is_none_or(|bodies| bodies.len() <= kept) {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    let on_member = session_to(&alice, &tablet, &member).await;
    tilo_test_env::within("the holder to admit the device entered meanwhile", async {
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
        holders.push(Some(serve_on(&alice, device, work.clone()).await));
    }
    let made_on = holders[0].as_ref().unwrap().host.workspaces();
    let workspace = made_on.create("Notes").await.unwrap();
    for at in 1..3 {
        made_on
            .attach_server(
                &workspace.workspace,
                devices[at].device_pubkey(),
                holders[at].as_ref().unwrap().host.node_addr(),
                CopyRole::KeyHolder,
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
        tilo_test_env::within("the holder admits the phone", async {
            while !admitted(&on_one, workspace.workspace_id).await {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await;
        // Each round's changes differ, so an earlier round's copy reaching a
        // holder that came back is not taken for this one.
        carried(
            &on_one,
            &phone,
            &on_two,
            workspace.workspace_id,
            format!("through one, without holder {gone}").as_bytes(),
        )
        .await;
        carried(
            &on_two,
            &newcomer,
            &on_one,
            workspace.workspace_id,
            format!("through two, without holder {gone}").as_bytes(),
        )
        .await;

        // Both say the same of who holds the workspace and who is in it, and
        // every holder is still named.
        tilo_test_env::within("both holders name the three of them", async {
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
                if on_one == on_two && on_one.0.len() == 3 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await;
        drop((on_one, on_two));

        holders[gone] = Some(
            crate::common::serve_in(&alice, devices[gone], settings[gone].clone(), state).await,
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
/// holder it is handed to takes in.
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

/// A session of `device` to `host_device`, a server of `person`.
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

/// Another person's holder that keeps no workspaces for the people it lets in
/// takes a workspace only when its own operator's work setting takes it: one
/// taking only its own person's work refuses.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn another_persons_host_refuses_a_workspace_its_setting_does_not_take() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let site = TestUser::fresh("Site");
    let server = site.enrol("server", DeviceCapabilities::member());
    let governor = serve_on(&alice, &laptop, WorkSetting::none()).await;
    let workspaces = governor.host.workspaces();
    let workspace = workspaces.create("Notes").await.unwrap();
    let refusing = serve_on(&site, &server, WorkSetting::own_person()).await;
    let refused = workspaces
        .attach_server(
            &workspace.workspace,
            server.device_pubkey(),
            refusing.host.node_addr(),
            CopyRole::KeyHolder,
        )
        .await;
    // The owner hears why: the holder keeps nobody's workspace but its own
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

/// A holder of another person whose operator takes the workspace holds it once
/// its governor grants it ownership: from then on a change made through either
/// holder reaches the other with its author, also after that holder restarts, and
/// detached it lets the workspace go.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
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
    let governor = serve_on(&alice, &laptop, WorkSetting::none()).await;
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
    let holder = serve_on(&site, &server, takes.clone()).await;
    let hint = workspaces
        .attach_server(
            &workspace.workspace,
            server.device_pubkey(),
            holder.host.node_addr(),
            CopyRole::KeyHolder,
        )
        .await
        .unwrap();
    assert_eq!(hint.host, server.cert);
    let hosts = workspaces.hosts(&workspace.workspace).await.unwrap();
    assert_eq!(hosts.len(), 2, "the governor and the other person's holder");

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
    let holder = crate::common::serve_in(&site, &server, takes, state).await;
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

/// A holder that keeps workspaces for the people it lets in takes a workspace
/// another holder governs for an owner of it it let in, and for nobody else:
/// asked before it let the owner in, it refuses and says why, and answers her
/// device's subscribe with `NotAdmitted` and nothing else (no workspace data,
/// members or keys), ending the session with that refusal; asked after, it
/// holds the workspace and a member's change through it reaches the governor.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one scenario whose servers and sessions live until it ends"
)]
async fn a_host_holds_a_workspace_for_an_owner_it_let_in() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let phone = alice.enrol("phone", DeviceCapabilities::primary());
    let site = TestUser::fresh("Site");
    let server = site.enrol("server", DeviceCapabilities::member());
    let governor = serve_on(&alice, &laptop, WorkSetting::none()).await;
    let workspaces = governor.host.workspaces();
    let workspace = workspaces.create("Notes").await.unwrap();
    workspaces
        .enter(&workspace.workspace, &phone.cert)
        .await
        .unwrap();
    let holder = serve_on(&site, &server, WorkSetting::own_person()).await;
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
        workspaces.attach_server(
            &workspace.workspace,
            server.device_pubkey(),
            holder.host.node_addr(),
            CopyRole::KeyHolder,
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

    // Restarted, the site still holds the workspace and follows the laptop's
    // changes: it knows where it was handed the workspace from, as it did
    // before.
    drop((on_holder, outside));
    let state = holder.stop_keeping().await;
    let holder = crate::common::serve_in(&site, &server, WorkSetting::own_person(), state).await;
    let on_holder = session_at(&phone, &site, &server, &holder).await;
    tilo_test_env::within("the restarted site admits the workspace's member", async {
        while !admitted(&on_holder, workspace.workspace_id).await {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    send_change(&local, workspace.workspace_id, &laptop, b"from the laptop");
    assert_eq!(
        change_author(&on_holder, b"from the laptop").await,
        laptop.device_pubkey()
    );

    drop((on_holder, local));
    holder.stop().await;
    governor.stop().await;
}

/// A workspace moves from one server to another: the new one keeps it
/// first, then the old one lets it go, and the workspace's member reaches it
/// at the new one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one scenario whose servers and sessions live until it ends"
)]
async fn a_workspace_moves_from_one_server_to_another() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let phone = alice.enrol("phone", DeviceCapabilities::primary());
    let site = TestUser::fresh("Site");
    let made_on = serve_on(&alice, &laptop, WorkSetting::none()).await;
    let workspaces = made_on.host.workspaces();
    let workspace = workspaces.create("Notes").await.unwrap();
    workspaces
        .enter(&workspace.workspace, &phone.cert)
        .await
        .unwrap();
    // Two servers of the site, each letting Alice in.
    let mut kept = Vec::new();
    for name in ["old", "new"] {
        let device = site.enrol(name, DeviceCapabilities::member());
        let server = serve_on(&site, &device, WorkSetting::own_person()).await;
        let at_site = server.host.workspaces();
        assert!(
            at_site
                .offer_own_workspaces(tilo_server::workspaces::OwnWorkspaceOffer::free(
                    "site.test"
                ))
                .unwrap()
        );
        let home = at_site.create("Home").await.unwrap();
        assert!(at_site.admit_own_through(&home.workspace));
        at_site
            .declare_owner(&home.workspace, alice.user_pk, None)
            .await
            .unwrap();
        kept.push((device, server));
    }
    let mut kept = kept.into_iter();
    let (old, old_server) = kept.next().unwrap();
    let (new, new_server) = kept.next().unwrap();
    workspaces
        .attach_server(
            &workspace.workspace,
            old.device_pubkey(),
            old_server.host.node_addr(),
            tilo_peer_state::CopyRole::KeyHolder,
        )
        .await
        .unwrap();

    let confirmed = workspaces
        .move_holder(
            &workspace.workspace,
            old.device_pubkey(),
            new.device_pubkey(),
            new_server.host.node_addr(),
        )
        .await
        .unwrap();
    assert!(confirmed, "the old server says it let the workspace go");
    let keeping: Vec<String> = workspaces
        .keeping_devices(&workspace.workspace)
        .await
        .unwrap()
        .into_iter()
        .map(|server| server.device)
        .collect();
    assert!(
        keeping.contains(&hex::encode(new.device_pubkey())),
        "{keeping:?}"
    );
    assert!(
        !keeping.contains(&hex::encode(old.device_pubkey())),
        "{keeping:?}"
    );

    // Alice's phone reaches the workspace at the new server, and a change
    // made there reaches the server it was made on.
    let local = made_on
        .host
        .connect_local(
            laptop.cert.clone(),
            laptop.sk.clone(),
            RevocationList::new(),
        )
        .await
        .unwrap();
    assert!(admitted(&local, workspace.workspace_id).await);
    let at_new = session_at(&phone, &site, &new, &new_server).await;
    tilo_test_env::within("the new server admits the workspace's member", async {
        while !admitted(&at_new, workspace.workspace_id).await {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    send_change(
        &at_new,
        workspace.workspace_id,
        &phone,
        b"from the new server",
    );
    assert_eq!(
        change_author(&local, b"from the new server").await,
        phone.device_pubkey()
    );

    // Moving from a server that does not keep it changes nothing.
    let refused = workspaces
        .move_holder(
            &workspace.workspace,
            old.device_pubkey(),
            new.device_pubkey(),
            new_server.host.node_addr(),
        )
        .await;
    assert!(
        refused
            .as_ref()
            .is_err_and(|error| error.contains("does not keep")),
        "{refused:?}"
    );

    drop((at_new, local));
    old_server.stop().await;
    new_server.stop().await;
    made_on.stop().await;
}

/// A workspace handed to a server on a build that does not take in the
/// changes the handing device keeps stays where it was: the handing device
/// never hears that the server keeps them, so it keeps the workspace, names
/// the server no more and says why, and a move to that server leaves the old
/// one keeping it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_server_that_does_not_take_the_changes_in_is_not_handed_the_workspace() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let site = TestUser::fresh("Site");
    let made_on = serve_on(&alice, &laptop, WorkSetting::none()).await;
    let workspaces = made_on.host.workspaces();
    let workspace = workspaces.create("Notes").await.unwrap();
    let behind = site.enrol("behind", DeviceCapabilities::member());
    let behind_server = serve_on(&site, &behind, WorkSetting::own_person()).await;
    let at_site = behind_server.host.workspaces();
    assert!(
        at_site
            .offer_own_workspaces(tilo_server::workspaces::OwnWorkspaceOffer::free(
                "site.test"
            ))
            .unwrap()
    );
    let home = at_site.create("Home").await.unwrap();
    assert!(at_site.admit_own_through(&home.workspace));
    at_site
        .declare_owner(&home.workspace, alice.user_pk, None)
        .await
        .unwrap();
    at_site.speak_no_catch_up();

    let refused = workspaces
        .attach_server(
            &workspace.workspace,
            behind.device_pubkey(),
            behind_server.host.node_addr(),
            CopyRole::KeyHolder,
        )
        .await;
    assert!(
        refused
            .as_ref()
            .is_err_and(|error| error.contains("this device keeps the workspace")),
        "{refused:?}"
    );
    let keeping: Vec<String> = workspaces
        .keeping_devices(&workspace.workspace)
        .await
        .unwrap()
        .into_iter()
        .map(|server| server.device)
        .collect();
    assert!(
        keeping.len() == 1 && !keeping.contains(&hex::encode(behind.device_pubkey())),
        "only the device it was made on keeps the workspace: {keeping:?}"
    );

    behind_server.stop().await;
    made_on.stop().await;
}

/// A device that is handed a workspace keeps its files under the
/// workspace's id. A hand-over that names its workspace by anything else (a
/// path that climbs out of the folder, an absolute path, a name with a
/// separator in it, another workspace's id) is refused, and nothing is
/// written anywhere for it. Under the workspace's own id the same hand-over
/// is taken, and its files are the only ones kept.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hand_over_names_its_workspace_by_its_id_and_writes_nowhere_else() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let nas = alice.enrol("nas", DeviceCapabilities::member());
    let governor = serve_on(&alice, &laptop, WorkSetting::none()).await;
    let state = tempfile::tempdir().unwrap();
    let root = state.path().to_path_buf();
    let member = crate::common::serve_in(&alice, &nas, WorkSetting::own_person(), state).await;
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
        .attach_server(
            &workspace.workspace,
            nas.device_pubkey(),
            at,
            CopyRole::KeyHolder,
        )
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
async fn a_held_workspace_is_not_taken_from_another_governor() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let phone = alice.enrol("phone", DeviceCapabilities::primary());
    let mallory = TestUser::fresh("Mallory");
    let hers = mallory.enrol("laptop", DeviceCapabilities::primary());
    let site = TestUser::fresh("Site");
    let kept_on = site.enrol("server", DeviceCapabilities::member());
    let governor = serve_on(&alice, &laptop, WorkSetting::none()).await;
    let workspaces = governor.host.workspaces();
    let workspace = workspaces.create("Notes").await.unwrap();
    workspaces
        .enter(&workspace.workspace, &phone.cert)
        .await
        .unwrap();
    // The site's operator takes that workspace, whoever hands it over.
    let mut takes = WorkSetting::own_person();
    takes.from = WorkFrom::Workspaces([workspace.workspace_id].into_iter().collect());
    let holder = serve_on(&site, &kept_on, takes).await;
    let at = holder.host.node_addr();
    workspaces
        .attach_server(
            &workspace.workspace,
            kept_on.device_pubkey(),
            at.clone(),
            CopyRole::KeyHolder,
        )
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
    let second = serve_on(&mallory, &hers, WorkSetting::none()).await;
    let of_mallory = second.host.workspaces();
    of_mallory
        .create_workspace("Notes", Some(&workspace.workspace))
        .await
        .unwrap();
    let answered = of_mallory
        .attach_server(
            &workspace.workspace,
            kept_on.device_pubkey(),
            at,
            CopyRole::KeyHolder,
        )
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

/// Wait until `holder`'s copy of `workspace` reads `done` of who holds its
/// keys.
async fn key_holders_once(
    holder: &crate::common::Hosted,
    workspace: &str,
    what: &str,
    done: impl Fn(&std::collections::BTreeSet<[u8; 32]>) -> bool + Sync,
) {
    tilo_test_env::within(what, async {
        loop {
            if let Ok(holders) = holder.host.workspaces().key_holders(workspace).await
                && done(&holders)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
}

/// Wait until `holder` names `maker` as the holder making `workspace`'s
/// compacted base.
async fn base_made_by(holder: &crate::common::Hosted, workspace: &str, maker: [u8; 32]) {
    tilo_test_env::within("the holders agree which of them makes the base", async {
        while holder
            .host
            .workspaces()
            .copies(workspace)
            .await
            .ok()
            .and_then(|status| status.base_maker)
            != Some(maker)
        {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
}

/// Whether `holder` governs `workspace`'s applications.
async fn governs_applications(holder: &crate::common::Hosted, workspace: &str) -> bool {
    holder
        .host
        .workspaces()
        .governs_applications(workspace)
        .await
        .unwrap()
}

/// Wait until `client`'s holder admits it to `workspace_id`.
async fn admitted_soon(client: &ServerClient, workspace_id: WorkspaceId) {
    tilo_test_env::within("the holder admits the member", async {
        while !admitted(client, workspace_id).await {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
}

/// Another person's server held as a key holder reads the workspace and
/// serves it without being a member: every holder gives it the keys, it
/// admits a member's device on its certificate and carries its changes, and
/// what it writes of who is in the workspace counts nowhere.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn another_persons_key_holder_reads_and_serves_but_lets_nobody_in() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let phone = alice.enrol("phone", DeviceCapabilities::primary());
    let tablet = alice.enrol("tablet", DeviceCapabilities::primary());
    let site = TestUser::fresh("Site");
    let server = site.enrol("server", DeviceCapabilities::member());
    let governor = serve_on(&alice, &laptop, WorkSetting::none()).await;
    let workspaces = governor.host.workspaces();
    let workspace = workspaces.create("Notes").await.unwrap();
    workspaces
        .enter(&workspace.workspace, &phone.cert)
        .await
        .unwrap();
    let mut takes = WorkSetting::own_person();
    takes.from = WorkFrom::Workspaces([workspace.workspace_id].into_iter().collect());
    let holder = serve_on(&site, &server, takes).await;
    let hint = workspaces
        .attach_server(
            &workspace.workspace,
            server.device_pubkey(),
            holder.host.node_addr(),
            CopyRole::KeyHolder,
        )
        .await
        .unwrap();
    assert_eq!(tilo_peer_state::holder_role(&hint), CopyRole::KeyHolder);

    // The members' devices give the server the keys, beside their own.
    key_holders_once(
        &governor,
        &workspace.workspace,
        "the server holds the keys",
        |holders| {
            holders.contains(&server.device_pubkey()) && holders.contains(&phone.device_pubkey())
        },
    )
    .await;
    let status = workspaces.copies(&workspace.workspace).await.unwrap();
    assert!(
        status
            .holders
            .iter()
            .any(|held| held.device == server.device_pubkey() && held.role == CopyRole::KeyHolder),
        "{status:?}"
    );

    // It serves the workspace: the phone is admitted there on its
    // certificate and its change reaches the laptop.
    let on_holder = session_at(&phone, &site, &server, &holder).await;
    admitted_soon(&on_holder, workspace.workspace_id).await;
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

    // It lets nobody in: a device it enters is in no other holder's
    // workspace, and so holds no keys there.
    let entered = holder
        .host
        .workspaces()
        .enter(&workspace.workspace, &tablet.cert)
        .await;
    send_change(
        &on_holder,
        workspace.workspace_id,
        &phone,
        b"after the entry",
    );
    receives(&local, b"after the entry").await;
    let holders = workspaces.key_holders(&workspace.workspace).await.unwrap();
    assert!(
        !holders.contains(&tablet.device_pubkey()),
        "a key holder of another person let a device in ({entered:?})"
    );

    drop((on_holder, local));
    holder.stop().await;
    governor.stop().await;
}

/// A server held as a store keeps and forwards the workspace's ciphertext:
/// it admits a member and serves what was written elsewhere, and no holder
/// gives it the keys, also after the key group moves on to admit a device.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_store_serves_history_and_never_holds_the_keys() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let phone = alice.enrol("phone", DeviceCapabilities::primary());
    let tablet = alice.enrol("tablet", DeviceCapabilities::primary());
    let watch = alice.enrol("watch", DeviceCapabilities::primary());
    let nas = alice.enrol("nas", DeviceCapabilities::member());
    let governor = serve_on(&alice, &laptop, WorkSetting::none()).await;
    let store = serve_on(&alice, &nas, WorkSetting::own_person()).await;
    let workspaces = governor.host.workspaces();
    let workspace = workspaces.create("Notes").await.unwrap();
    for device in [&phone, &tablet] {
        workspaces
            .enter(&workspace.workspace, &device.cert)
            .await
            .unwrap();
    }
    let hint = workspaces
        .attach_server(
            &workspace.workspace,
            nas.device_pubkey(),
            store.host.node_addr(),
            CopyRole::Store,
        )
        .await
        .unwrap();
    assert_eq!(tilo_peer_state::holder_role(&hint), CopyRole::Store);
    let status = workspaces.copies(&workspace.workspace).await.unwrap();
    assert!(
        status
            .holders
            .iter()
            .any(|held| held.device == nas.device_pubkey() && held.role == CopyRole::Store),
        "{status:?}"
    );
    for at in [&governor, &store] {
        key_holders_once(
            at,
            &workspace.workspace,
            "the store holds no keys",
            |holders| {
                holders.contains(&tablet.device_pubkey()) && !holders.contains(&nas.device_pubkey())
            },
        )
        .await;
    }

    // A change made on the laptop reaches the tablet through the store.
    let on_store = session_to(&alice, &tablet, &store).await;
    admitted_soon(&on_store, workspace.workspace_id).await;
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
    send_change(
        &local,
        workspace.workspace_id,
        &laptop,
        b"through the store",
    );
    receives(&on_store, b"through the store").await;

    // The group moves on to admit another device; the store is not in it.
    workspaces
        .enter(&workspace.workspace, &watch.cert)
        .await
        .unwrap();
    for at in [&governor, &store] {
        key_holders_once(
            at,
            &workspace.workspace,
            "the store holds no keys after the group moved on",
            |holders| {
                holders.contains(&watch.device_pubkey()) && !holders.contains(&nas.device_pubkey())
            },
        )
        .await;
    }

    drop((on_store, local));
    store.stop().await;
    governor.stop().await;
}

/// A holder's role moves both ways: made a store it leaves the key holders
/// on every holder, its own copy included, so the group's commit removing it
/// rotates the keys; made a key holder again it is let back in.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_holders_role_moves_both_ways() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let nas = alice.enrol("nas", DeviceCapabilities::member());
    let governor = serve_on(&alice, &laptop, WorkSetting::none()).await;
    let held = serve_on(&alice, &nas, WorkSetting::own_person()).await;
    let workspaces = governor.host.workspaces();
    let workspace = workspaces.create("Notes").await.unwrap();
    let device = nas.device_pubkey();
    workspaces
        .attach_server(
            &workspace.workspace,
            device,
            held.host.node_addr(),
            CopyRole::KeyHolder,
        )
        .await
        .unwrap();
    for at in [&governor, &held] {
        key_holders_once(at, &workspace.workspace, "it holds the keys", |holders| {
            holders.contains(&device)
        })
        .await;
    }

    let hint = workspaces
        .set_holder_role(&workspace.workspace, device, CopyRole::Store)
        .await
        .unwrap();
    assert_eq!(tilo_peer_state::holder_role(&hint), CopyRole::Store);
    for at in [&governor, &held] {
        key_holders_once(
            at,
            &workspace.workspace,
            "it leaves the key holders",
            |holders| !holders.contains(&device) && holders.contains(&laptop.device_pubkey()),
        )
        .await;
    }
    assert!(
        workspaces
            .hosts(&workspace.workspace)
            .await
            .unwrap()
            .iter()
            .any(|named| named.host.device_public_key == device
                && tilo_peer_state::holder_role(named) == CopyRole::Store)
    );

    let hint = workspaces
        .set_holder_role(&workspace.workspace, device, CopyRole::KeyHolder)
        .await
        .unwrap();
    assert_eq!(tilo_peer_state::holder_role(&hint), CopyRole::KeyHolder);
    for at in [&governor, &held] {
        key_holders_once(
            at,
            &workspace.workspace,
            "it holds the keys again",
            |holders| holders.contains(&device),
        )
        .await;
    }

    held.stop().await;
    governor.stop().await;
}

/// The workspace's compacted base is made by one holder every holder agrees
/// on: the device it was founded on, and once that one is detached the next
/// key holder of the owner, which the remaining holders name alike; the
/// workspace stays writable through either. That holder governs the
/// workspace's applications too, so a server that took the workspace over
/// from the one it was founded on sets them up once that one is gone.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn detaching_the_base_maker_hands_the_base_to_another_key_holder() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let phone = alice.enrol("phone", DeviceCapabilities::primary());
    let tablet = alice.enrol("tablet", DeviceCapabilities::primary());
    let first = alice.enrol("nas", DeviceCapabilities::member());
    let second = alice.enrol("server", DeviceCapabilities::member());
    let founder = serve_on(&alice, &laptop, WorkSetting::none()).await;
    let one = serve_on(&alice, &first, WorkSetting::own_person()).await;
    let other = serve_on(&alice, &second, WorkSetting::own_person()).await;
    let made_on = founder.host.workspaces();
    let workspace = made_on.create("Notes").await.unwrap();
    for (device, at) in [(&first, &one), (&second, &other)] {
        made_on
            .attach_server(
                &workspace.workspace,
                device.device_pubkey(),
                at.host.node_addr(),
                CopyRole::KeyHolder,
            )
            .await
            .unwrap();
    }
    for device in [&phone, &tablet] {
        made_on
            .enter(&workspace.workspace, &device.cert)
            .await
            .unwrap();
    }
    for at in [&founder, &one, &other] {
        base_made_by(at, &workspace.workspace, laptop.device_pubkey()).await;
    }
    for (at, governs) in [(&founder, true), (&one, false), (&other, false)] {
        assert_eq!(
            governs_applications(at, &workspace.workspace).await,
            governs,
            "the founding device governs the applications while it holds the workspace"
        );
    }

    one.host
        .workspaces()
        .detach_host(&workspace.workspace, laptop.device_pubkey(), None)
        .await
        .unwrap();
    let next = first.device_pubkey().min(second.device_pubkey());
    for at in [&one, &other] {
        base_made_by(at, &workspace.workspace, next).await;
    }
    for (at, device) in [(&one, &first), (&other, &second)] {
        assert_eq!(
            governs_applications(at, &workspace.workspace).await,
            device.device_pubkey() == next,
            "the holder making the base governs the applications once the founder is gone"
        );
    }

    let on_one = session_to(&alice, &phone, &one).await;
    let on_other = session_to(&alice, &tablet, &other).await;
    for session in [&on_one, &on_other] {
        admitted_soon(session, workspace.workspace_id).await;
    }
    send_change(
        &on_one,
        workspace.workspace_id,
        &phone,
        b"after the founder went",
    );
    receives(&on_other, b"after the founder went").await;

    drop((on_one, on_other));
    other.stop().await;
    one.stop().await;
    founder.stop().await;
}
