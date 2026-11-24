//! A certified holder of a person serves a workspace one of the person's devices
//! governs: the governor attaches it, the workspace's holders name it, it admits
//! whom the workspace's governance admits as that governance changes, carries
//! members' changes to every other holder, and stops when detached.

#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]

use std::time::Duration;

use automerge::{AutoCommit, ROOT, ReadDoc as _, transaction::Transactable as _};
use neighbour_wire::NeighbourMsg;
use tilo_peer_state::{CopyRole, StatePeer};
use tilo_server_protocol::{WorkFrom, WorkSetting};
use tilo_user_identity::DeviceCapabilities;

use crate::common::{Hosted, Linked, TestDevice, TestUser, next, receives, serve_on};
use tilo_server::WorkspaceId;

/// The app the tests write records in. Each step writes a record of its own, so a change is read
/// back as the one that step made.
const APP: &str = "test.app";

/// How long the holders are watched doing nothing: longer than the holder's
/// first two waits before dialing again, so a link that kept ending would show.
const IDLE_WINDOW: Duration = Duration::from_secs(4);

/// How long both holders' counters stand still before the idle watch starts.
const SETTLE: Duration = Duration::from_millis(500);

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

/// Write `body` into `record` as `device`, through `linked`.
fn write(linked: &Linked, device: &TestDevice, record: &str, body: &str) -> Result<(), String> {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "body", body).unwrap();
    let bytes = doc.save();
    linked
        .publish(|peer| {
            peer.publication_mut().publish_document(
                APP,
                record,
                bytes,
                &device.sk,
                std::time::SystemTime::now(),
            )
        })
        .map(|_| ())
}

/// Write `body` into `record` as `hosted`'s own device: the change the device serving the
/// workspace makes in process.
async fn host_writes(hosted: &Hosted, workspace: WorkspaceId, record: &str, body: &str) {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "body", body).unwrap();
    let bytes = doc.save();
    let workspaces = hosted.host.workspaces();
    let mut state = workspaces
        .state(&workspace.to_string())
        .await
        .expect("the host serves the workspace");
    let documents = hosted.host.workspace_documents();
    let change = state
        .publication_mut()
        .publish_document(
            APP,
            record,
            bytes,
            documents.signing_key(),
            std::time::SystemTime::now(),
        )
        .expect("the host signs its own change");
    let signed = postcard::to_allocvec(&change).expect("the change encodes");
    documents
        .commit(
            workspace,
            &tilo_server::workspace_documents::HostCommit { body: signed },
        )
        .expect("the host keeps its own change");
}

/// Whether the record `bytes` carries `body`.
fn says(bytes: &[u8], body: &str) -> bool {
    AutoCommit::load(bytes).is_ok_and(|doc| {
        doc.get(ROOT, "body")
            .ok()
            .flatten()
            .is_some_and(|(said, _)| said.to_str() == Some(body))
    })
}

/// Whether `peer`'s copy of `record` carries `body`.
fn copy_says(peer: &StatePeer, record: &str, body: &str) -> bool {
    peer.document(APP, record)
        .is_some_and(|bytes| says(&bytes, body))
}

/// Whether `hosted`'s own copy of `record` carries `body`.
async fn host_says(hosted: &Hosted, workspace: WorkspaceId, record: &str, body: &str) -> bool {
    hosted
        .host
        .workspaces()
        .state(&workspace.to_string())
        .await
        .is_some_and(|state| copy_says(&state, record, body))
}

/// Wait until `hosted`'s own copy of `record` carries `body`.
async fn reaches_host(hosted: &Hosted, workspace: WorkspaceId, record: &str, body: &str) {
    tilo_test_env::within("the change reaches the host's own copy", async {
        while !host_says(hosted, workspace, record, body).await {
            tokio::task::yield_now().await;
        }
    })
    .await;
}

/// Whether the members `hosted` reads of `workspace` name `device`.
async fn names(hosted: &Hosted, workspace: &str, device: &TestDevice) -> bool {
    hosted
        .host
        .workspaces()
        .summary(workspace)
        .await
        .is_ok_and(|summary| {
            summary
                .devices
                .values()
                .any(|devices| devices.contains(&device.device_pubkey()))
        })
}

/// Whether `message` carries a change to `record`.
fn record_of(message: &NeighbourMsg, record: &str) -> bool {
    matches!(
        message,
        NeighbourMsg::AppDocument {
            app_id,
            document_id,
            ..
        } if app_id.as_str() == APP && document_id.as_str() == record
    )
}

/// The device that authored the next change to `record` `linked`'s copy takes:
/// the workspace's own changes before it are passed over.
async fn change_author(linked: &mut Linked, record: &str) -> [u8; 32] {
    next(linked, |heard| record_of(&heard.message, record))
        .await
        .author
}

/// Whether `hosted` keeps a change to `record` that `author` signed.
async fn carried_from(
    hosted: &Hosted,
    workspace: WorkspaceId,
    record: &str,
    author: [u8; 32],
) -> bool {
    hosted
        .host
        .workspace_documents()
        .retained(workspace)
        .await
        .is_ok_and(|bodies| {
            bodies.iter().any(|body| {
                body.author == author
                    && NeighbourMsg::decode(&body.body)
                        .is_ok_and(|message| record_of(&message, record))
            })
        })
}

/// Wait until `hosted` admits `device` to `workspace`: the link it opens.
async fn member_at(hosted: &Hosted, device: &TestDevice, workspace: WorkspaceId) -> Linked {
    tilo_test_env::within("the host admits the member", async {
        loop {
            if let Ok(linked) = Linked::open_hosted(hosted, device, workspace).await {
                return linked;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
}

/// Wait until `hosted` admits the member `linked` already belongs to, over the endpoint its
/// links run from.
async fn reopened_at(linked: &Linked, hosted: &Hosted, workspace: WorkspaceId) -> Linked {
    tilo_test_env::within("the host admits the member again", async {
        loop {
            if let Ok(opened) = linked.reopen_hosted(hosted, workspace).await {
                return opened;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
}

/// Wait until `hosted` refuses `device` `workspace`.
async fn refused_soon(hosted: &Hosted, device: &TestDevice, workspace: WorkspaceId) {
    tilo_test_env::within("the host refuses the member", async {
        loop {
            if Linked::open_hosted(hosted, device, workspace)
                .await
                .is_err()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
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
        .enter(&workspace.workspace, (&phone.cert, &phone.sk))
        .await
        .unwrap();
    let on_member = member_at(&member, &phone, workspace.workspace_id).await;

    // A change the phone sends the holder reaches the governor's own copy.
    write(
        &on_member,
        &phone,
        "note:1",
        "from the phone, through the holder",
    )
    .unwrap();
    reaches_host(
        &governor,
        workspace.workspace_id,
        "note:1",
        "from the phone, through the holder",
    )
    .await;

    // With nothing changing, neither holder does anything: no link is
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
    refused_soon(&member, &phone, workspace.workspace_id).await;

    drop(on_member);
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
        .enter(&workspace.workspace, (&phone.cert, &phone.sk))
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
        .enter(&workspace.workspace, (&tablet.cert, &tablet.sk))
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
    let on_member = member_at(&member, &tablet, workspace.workspace_id).await;

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
#[allow(
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
            .enter(&workspace.workspace, (&device.cert, &device.sk))
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
            .enter(&workspace.workspace, (&newcomer.cert, &newcomer.sk))
            .await
            .unwrap();
        let mut on_two = member_at(two, &newcomer, workspace.workspace_id).await;

        // And what members write reaches each through the other.
        let mut on_one = member_at(one, &phone, workspace.workspace_id).await;
        // Each round's changes differ, so an earlier round's copy reaching a
        // holder that came back is not taken for this one.
        carried(
            &on_one,
            &phone,
            &mut on_two,
            &format!("note:one-{gone}"),
            &format!("through one, without holder {gone}"),
        )
        .await;
        carried(
            &on_two,
            &newcomer,
            &mut on_one,
            &format!("note:two-{gone}"),
            &format!("through two, without holder {gone}"),
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

/// Write a change as `author` into `record` through `from`, and expect `to` to take it in,
/// signed by `author`.
async fn carried(from: &Linked, author: &TestDevice, to: &mut Linked, record: &str, body: &str) {
    write(from, author, record, body).unwrap();
    receives(to, |peer| copy_says(peer, record, body)).await;
    assert_eq!(
        change_author(to, record).await,
        author.device_pubkey(),
        "the other holder reads the change as its author's"
    );
}

/// The devices a workspace's governance names besides its owner's own:
/// a workspace people have used for a while, not a fresh one.
const NAMED_DEVICES: usize = 300;

/// `workspace` as a workspace in use for a while is: its governance names
/// hundreds of devices, and its history holds each admission, all of which a
/// holder it is handed to takes in.
async fn used_for_a_while(
    workspaces: &tilo_server::workspaces::Workspaces,
    person: &TestUser,
    workspace: &str,
) {
    for device in 0..NAMED_DEVICES {
        let device = person.enrol(&format!("device {device}"), DeviceCapabilities::member());
        workspaces
            .enter(workspace, (&device.cert, &device.sk))
            .await
            .unwrap();
    }
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
#[allow(
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
        .enter(&workspace.workspace, (&phone.cert, &phone.sk))
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

    assert!(
        names(&governor, &workspace.workspace, &laptop).await,
        "the governor's workspace names its own device"
    );
    let mut on_holder = member_at(&holder, &phone, workspace.workspace_id).await;

    // A change made through the holder reaches the governor, and one made on
    // the governor reaches the holder, each with its author.
    write(&on_holder, &phone, "note:phone", "from the phone").unwrap();
    reaches_host(
        &governor,
        workspace.workspace_id,
        "note:phone",
        "from the phone",
    )
    .await;
    assert!(
        carried_from(
            &governor,
            workspace.workspace_id,
            "note:phone",
            phone.device_pubkey()
        )
        .await,
        "the governor reads the change as the phone's"
    );
    host_writes(
        &governor,
        workspace.workspace_id,
        "note:laptop",
        "from the laptop",
    )
    .await;
    receives(&mut on_holder, |peer| {
        copy_says(peer, "note:laptop", "from the laptop")
    })
    .await;
    assert_eq!(
        change_author(&mut on_holder, "note:laptop").await,
        laptop.device_pubkey(),
        "the holder reads the change as the laptop's"
    );

    // The holder restarts and holds the workspace again from its own record.
    let state = holder.stop_keeping().await;
    let holder = crate::common::serve_in(&site, &server, takes, state).await;
    let again = reopened_at(&on_holder, &holder, workspace.workspace_id).await;
    write(&again, &phone, "note:restart", "after the restart").unwrap();
    reaches_host(
        &governor,
        workspace.workspace_id,
        "note:restart",
        "after the restart",
    )
    .await;
    assert!(
        carried_from(
            &governor,
            workspace.workspace_id,
            "note:restart",
            phone.device_pubkey()
        )
        .await,
        "the governor reads the change as the phone's"
    );

    // Detached, the holder lets the workspace go: it answers them no link at all.
    assert!(
        workspaces
            .detach_host(&workspace.workspace, server.device_pubkey(), None)
            .await
            .unwrap(),
        "the holder confirms it stopped"
    );
    refused_soon(&holder, &phone, workspace.workspace_id).await;

    drop((again, on_holder));
    holder.stop().await;
    governor.stop().await;
}

/// A holder that keeps workspaces for the people it lets in takes a workspace
/// another holder governs for an owner of it it let in, and for nobody else:
/// asked before it let the owner in, it refuses and says why, and answers her
/// device's link with no admission at all (no workspace data, members or keys);
/// asked after, it holds the workspace and a member's change through it reaches
/// the governor.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario whose servers and links live until it ends"
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
        .enter(&workspace.workspace, (&phone.cert, &phone.sk))
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
    // The refusal is the last word: a device the site never let in holds no
    // workspace there, so no link to it opens.
    let outside = Linked::open_hosted(&holder, &phone, home.workspace_id).await;
    assert!(
        outside.is_err(),
        "a device the site never let in is connected: {outside:?}"
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
    assert!(
        names(&governor, &workspace.workspace, &laptop).await,
        "the governor's workspace names its own device"
    );
    let on_holder = member_at(&holder, &phone, workspace.workspace_id).await;
    write(&on_holder, &phone, "note:site", "from the site").unwrap();
    reaches_host(
        &governor,
        workspace.workspace_id,
        "note:site",
        "from the site",
    )
    .await;
    assert!(
        carried_from(
            &governor,
            workspace.workspace_id,
            "note:site",
            phone.device_pubkey()
        )
        .await,
        "the governor reads the change as the phone's"
    );

    // Restarted, the site still holds the workspace and follows the laptop's
    // changes: it knows where it was handed the workspace from, as it did
    // before.
    let state = holder.stop_keeping().await;
    let holder = crate::common::serve_in(&site, &server, WorkSetting::own_person(), state).await;
    let mut on_holder = reopened_at(&on_holder, &holder, workspace.workspace_id).await;
    host_writes(
        &governor,
        workspace.workspace_id,
        "note:laptop",
        "from the laptop",
    )
    .await;
    receives(&mut on_holder, |peer| {
        copy_says(peer, "note:laptop", "from the laptop")
    })
    .await;
    assert_eq!(
        change_author(&mut on_holder, "note:laptop").await,
        laptop.device_pubkey(),
        "the restarted site reads the change as the laptop's"
    );

    drop(on_holder);
    holder.stop().await;
    governor.stop().await;
}

/// A workspace moves from one server to another: the new one keeps it
/// first, then the old one lets it go, and the workspace's member reaches it
/// at the new one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario whose servers and links live until it ends"
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
        .enter(&workspace.workspace, (&phone.cert, &phone.sk))
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
    assert!(
        names(&made_on, &workspace.workspace, &laptop).await,
        "the workspace names the device it was made on"
    );
    let at_new = member_at(&new_server, &phone, workspace.workspace_id).await;
    write(&at_new, &phone, "note:new-server", "from the new server").unwrap();
    reaches_host(
        &made_on,
        workspace.workspace_id,
        "note:new-server",
        "from the new server",
    )
    .await;
    assert!(
        carried_from(
            &made_on,
            workspace.workspace_id,
            "note:new-server",
            phone.device_pubkey()
        )
        .await,
        "the server it was made on reads the change as the phone's"
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

    drop(at_new);
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
        .enter(&workspace.workspace, (&phone.cert, &phone.sk))
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
    let on_holder = member_at(&holder, &phone, workspace.workspace_id).await;

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
        on_holder
            .reopen_hosted(&holder, workspace.workspace_id)
            .await
            .is_ok(),
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
        .enter(&workspace.workspace, (&phone.cert, &phone.sk))
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
    let on_holder = member_at(&holder, &phone, workspace.workspace_id).await;
    assert!(
        names(&governor, &workspace.workspace, &laptop).await,
        "the governor's workspace names its own device"
    );

    // It lets nobody in: a device it enters is in no other holder's
    // workspace, and so holds no keys there.
    let entered = holder
        .host
        .workspaces()
        .enter(&workspace.workspace, (&tablet.cert, &tablet.sk))
        .await;
    write(&on_holder, &phone, "note:entry", "after the entry").unwrap();
    reaches_host(
        &governor,
        workspace.workspace_id,
        "note:entry",
        "after the entry",
    )
    .await;
    let holders = workspaces.key_holders(&workspace.workspace).await.unwrap();
    assert!(
        !holders.contains(&tablet.device_pubkey()),
        "a key holder of another person let a device in ({entered:?})"
    );

    drop(on_holder);
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
            .enter(&workspace.workspace, (&device.cert, &device.sk))
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
    let mut on_store = member_at(&store, &tablet, workspace.workspace_id).await;
    assert!(
        names(&governor, &workspace.workspace, &laptop).await,
        "the governor's workspace names its own device"
    );
    host_writes(
        &governor,
        workspace.workspace_id,
        "note:store",
        "through the store",
    )
    .await;
    receives(&mut on_store, |peer| {
        copy_says(peer, "note:store", "through the store")
    })
    .await;

    // The group moves on to admit another device; the store is not in it.
    workspaces
        .enter(&workspace.workspace, (&watch.cert, &watch.sk))
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

    drop(on_store);
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
            .enter(&workspace.workspace, (&device.cert, &device.sk))
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

    let on_one = member_at(&one, &phone, workspace.workspace_id).await;
    let mut on_other = member_at(&other, &tablet, workspace.workspace_id).await;
    write(&on_one, &phone, "note:founder", "after the founder went").unwrap();
    receives(&mut on_other, |peer| {
        copy_says(peer, "note:founder", "after the founder went")
    })
    .await;

    drop((on_one, on_other));
    other.stop().await;
    one.stop().await;
    founder.stop().await;
}
