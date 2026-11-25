//! A member's windows that reach the workspace over its direct link stay the
//! workspace's across a restart of the server and of the member: the
//! member's next windows build on them, and are taken in.

#![expect(
    clippy::similar_names,
    reason = "each device in these tests is named for whose it is — the member's, the host's, the page's"
)]

use super::*;
use crate::workspaces::fixtures::{TestHost, list_itself_on_link};
use std::time::Duration;
use tilo_server_client::LinkError;
use tilo_server_client::link::{Dialer, Purpose};
use tilo_server_protocol::delegation::{Delegate, DelegateScope, Delegation};
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, RevocationEntry, RevocationReason, sign_revocation_entry,
};

/// How long a link or the governor may take to do what a test waits for.
const WAIT: Duration = Duration::from_secs(10);

/// A member device of `workspace` with `key`, linked to the server's direct
/// link of it as its runtime links, with what it holds of the workspace from
/// the server: the pairing, which ends with the test, and the member's link.
async fn linked(
    workspaces: &Workspaces,
    workspace_id: WorkspaceId,
    key: SigningKey,
) -> (
    tokio::task::JoinHandle<Result<(), String>>,
    tilo_peer_state::direct::DirectSync,
) {
    let host = workspaces
        .direct()
        .link(workspace_id)
        .expect("the server serves the workspace over a direct link");
    let member = tilo_peer_state::direct::DirectSync::new(host.read(Clone::clone), key);
    let mut caught_up = member.caught_up();
    let linked = member.clone();
    let pairing =
        tokio::spawn(
            async move { tilo_peer_state::direct::DirectSync::pair(&linked, &host).await },
        );
    tokio::time::timeout(WAIT, caught_up.wait_for(|caught_up| *caught_up))
        .await
        .expect("the member catches up with the server")
        .unwrap();
    (pairing, member)
}

/// Wait until the governor of `workspace` committed everything its copy holds
/// (what a restart starts from) and that copy holds `titles` windows.
async fn committed_with(workspaces: &Workspaces, workspace: &str, titles: usize) {
    let state = workspaces.workspace(workspace).await.unwrap();
    tokio::time::timeout(WAIT, async {
        loop {
            {
                let governed = state.lock().await;
                let held = titles_in(governed.peer().admitted_workspace().unwrap()).len();
                if held == titles && governed.peer().workspace_heads() == governed.kept_heads {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!("the governor commits the member's windows the link admitted ({titles} windows)")
    });
}

/// `member`'s change closing the windows it made in its namespace `salt`,
/// made on `doc`, the workspace's document as the member's device holds it.
fn closed_on(
    mut doc: automerge::AutoCommit,
    workspace: &str,
    member: &DeviceCert,
    salt: u16,
) -> neighbour_wire::NeighbourMsg {
    let heads = doc.get_heads();
    doc.set_actor(automerge::ActorId::from(member.device_public_key.to_vec()));
    tilo_desktop_model::shared_resource::device::withdraw(&mut doc, &|id| {
        tilo_desktop_model::peer_salt_of(id) == salt
    })
    .unwrap();
    doc.commit();
    neighbour_wire::NeighbourMsg::SyncResp {
        workspace: workspace.to_owned(),
        changes: doc
            .get_changes(&heads)
            .iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect(),
    }
}

/// The workspace's document as the governor of `workspace` holds it.
async fn governors_copy(workspaces: &Workspaces, workspace: &str) -> automerge::AutoCommit {
    let state = workspaces.workspace(workspace).await.unwrap();
    let governed = state.lock().await;
    governed.peer().admitted_workspace().unwrap().clone()
}

/// The windows a member opens over the link are committed by the governor as
/// they come: the server restarts, the member's device restarts holding them
/// as the workspace's, and the windows it closes next, a change built on
/// them, are taken in. The link took them into the governor's copy, and
/// nothing wrote that copy to disk until some other change came: a restart
/// before one lost them, and the server refused every later change of the
/// member's for history it no longer had, as long as the member ran.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_members_windows_the_link_admitted_outlive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let created = workspaces.create("Team").await.unwrap();
    let workspace = created.workspace.clone();
    let (member, salt) = admitted_member(&workspaces, &workspace, &cert, 3).await;
    let member_key = SigningKey::from_bytes(&[3; 32]);
    let opened = terminals_opened(&workspaces, &workspace, &member, salt).await;
    let (pairing, link) = linked(&workspaces, created.workspace_id, member_key.clone()).await;
    link.send(postcard::from_bytes(&opened).unwrap()).unwrap();
    committed_with(&workspaces, &workspace, 2).await;
    // What the member's device holds of the workspace: its own windows in it.
    let members_copy = governors_copy(&workspaces, &workspace).await;
    pairing.abort();
    drop(link);
    drop(workspaces);
    server.shutdown().await;

    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    assert_eq!(
        titles_in(&governors_copy(&workspaces, &workspace).await).len(),
        2,
        "the server starts again with the member's windows"
    );
    // The member's device restarted too: a new link, from what it kept.
    let (pairing, link) = linked(&workspaces, created.workspace_id, member_key).await;
    link.send(closed_on(members_copy, &workspace, &member, salt))
        .unwrap();
    committed_with(&workspaces, &workspace, 0).await;
    pairing.abort();
    drop(link);
    drop(workspaces);
    server.shutdown().await;
}

/// Dial the host's link as the member whose device is `cert`/`key`, whose iroh endpoint speaks as
/// `endpoint_key` — its device key, or a page's key of its own — proving the device on the link's
/// first stream, and answer its admission. The dialer is the caller's to keep: its endpoint
/// carries the link.
async fn dialed(
    server: &TestHost,
    workspaces: &Workspaces,
    workspace_id: WorkspaceId,
    cert: &DeviceCert,
    key: &SigningKey,
    host_cert: &DeviceCert,
    endpoint_key: [u8; 32],
) -> (
    Arc<Dialer>,
    tilo_peer_state::direct::DirectSync,
    Result<tilo_peer_state::direct::Dialed, tilo_server_protocol::refusal::Refusal>,
) {
    let host = workspaces
        .direct()
        .link(workspace_id)
        .expect("the server serves the workspace over a direct link");
    let member = tilo_peer_state::direct::DirectSync::new(host.read(Clone::clone), key.clone());
    let dialer = Arc::new(Dialer::new(
        tilo_server_client::ServerAddress::at(server.node_addr()),
        Some(endpoint_key),
    ));
    let refused = |error: LinkError| match error {
        LinkError::Refused(reason) => tilo_server_protocol::refusal::Refusal::NotAMember(reason),
        error => tilo_server_protocol::refusal::Refusal::Other(error.to_string()),
    };
    let link = match dialer.link(Purpose::Records).await {
        Ok(link) => link,
        Err(error) => return (dialer, member, Err(refused(error))),
    };
    if let Err(error) = link.prove_member(cert, key, host_cert).await {
        return (dialer, member, Err(refused(error)));
    }
    let answered = member
        .dial_over(Arc::new(link), workspaces.host_device())
        .await;
    (dialer, member, answered)
}

/// Dial the host's link as that member and run it: the dialer that carries it, and the task its
/// link runs until it ends.
async fn dialled_and_running(
    server: &TestHost,
    workspaces: &Workspaces,
    workspace_id: WorkspaceId,
    cert: &DeviceCert,
    key: &SigningKey,
    host_cert: &DeviceCert,
) -> (
    Arc<Dialer>,
    tokio::task::JoinHandle<Result<(), tilo_server_protocol::refusal::Refusal>>,
) {
    let endpoint_key = key.verifying_key().to_bytes();
    let (dialer, link, dialed) = dialed(
        server,
        workspaces,
        workspace_id,
        cert,
        key,
        host_cert,
        endpoint_key,
    )
    .await;
    let dialed = dialed.expect("the host admits its member's device");
    let mut caught_up = link.caught_up();
    let running = tokio::spawn(async move { dialed.run(&link).await });
    tokio::time::timeout(WAIT, caught_up.wait_for(|caught_up| *caught_up))
        .await
        .expect("the member catches up over the link it dialled")
        .unwrap();
    (dialer, running)
}

/// The host's runtime for `workspace_id`, where its list of the workspace's
/// members and how it admits them are read and written.
async fn runtime(
    workspaces: &Workspaces,
    workspace_id: WorkspaceId,
) -> crate::workspace::SharedWorkspace {
    workspaces
        .inner
        .host
        .workspaces
        .lock()
        .await
        .get(&workspace_id)
        .cloned()
        .expect("the host serves the workspace")
}

/// A member's device is admitted on the link it dials, and catches up from
/// what the server keeps: the admission hook answers it from the workspace's
/// replica before the link listens.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_members_device_is_admitted_on_the_link_it_dials() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let created = workspaces.create("Team").await.unwrap();
    let (member, _salt) = admitted_member(&workspaces, &created.workspace, &cert, 3).await;
    let member_key = SigningKey::from_bytes(&[3; 32]);
    let endpoint_key = member_key.verifying_key().to_bytes();
    let (endpoint, link, dialed) = dialed(
        &server,
        &workspaces,
        created.workspace_id,
        &member,
        &member_key,
        &cert,
        endpoint_key,
    )
    .await;
    let dialed = match dialed {
        Ok(dialed) => dialed,
        Err(refusal) => panic!(
            "the server admits a member's device ({}): {refusal}",
            member.device_label
        ),
    };
    let mut caught_up = link.caught_up();
    let running = tokio::spawn(async move { dialed.run(&link).await });
    tokio::time::timeout(WAIT, caught_up.wait_for(|caught_up| *caught_up))
        .await
        .expect("the member catches up over the link it dialled")
        .unwrap();
    running.abort();
    endpoint.close().await;
    drop(workspaces);
    server.shutdown().await;
}

/// A page dials the records link with an endpoint key of its own, which is not its device key:
/// the link proves its device on its first stream, and the workspace admits it as that device.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_page_keyed_by_another_endpoint_is_admitted_as_its_device() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let created = workspaces.create("Team").await.unwrap();
    let (member, _salt) = admitted_member(&workspaces, &created.workspace, &cert, 3).await;
    let member_key = SigningKey::from_bytes(&[3; 32]);
    // The endpoint this page speaks as is no device's key.
    let page = SigningKey::from_bytes(&[0x91; 32]);
    let page_endpoint = page.verifying_key().to_bytes();
    assert_ne!(page_endpoint, member.device_public_key);
    let (dialer, link, dialed) = dialed(
        &server,
        &workspaces,
        created.workspace_id,
        &member,
        &member_key,
        &cert,
        page_endpoint,
    )
    .await;
    let dialed = match dialed {
        Ok(dialed) => dialed,
        Err(refusal) => panic!("the workspace admits the device the page proved: {refusal}"),
    };
    let mut caught_up = link.caught_up();
    let running = tokio::spawn(async move { dialed.run(&link).await });
    tokio::time::timeout(WAIT, caught_up.wait_for(|caught_up| *caught_up))
        .await
        .expect("the page catches up over the link it dialled")
        .unwrap();
    running.abort();
    dialer.close().await;
    drop(workspaces);
    server.shutdown().await;
}

/// A revoked device's own links end, and are refused when it dials again,
/// while another member's link to the same workspace stays up: the re-check
/// ends the links of the device that lost its access alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revoked_devices_link_ends_and_another_members_stays_up() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let created = workspaces.create("Team").await.unwrap();
    let (revoked, _salt) = admitted_member(&workspaces, &created.workspace, &cert, 3).await;
    let (kept, _salt) = admitted_member(&workspaces, &created.workspace, &cert, 4).await;
    let revoked_key = SigningKey::from_bytes(&[3; 32]);
    let kept_key = SigningKey::from_bytes(&[4; 32]);
    let (revoked_endpoint, revoked_link) = dialled_and_running(
        &server,
        &workspaces,
        created.workspace_id,
        &revoked,
        &revoked_key,
        &cert,
    )
    .await;
    let (kept_endpoint, kept_link) = dialled_and_running(
        &server,
        &workspaces,
        created.workspace_id,
        &kept,
        &kept_key,
        &cert,
    )
    .await;
    let host = workspaces
        .direct()
        .link(created.workspace_id)
        .expect("the server serves the workspace over a direct link");

    // The person revokes that device, as their root does, and the host is told
    // what admission reads may have narrowed.
    let mut entry = RevocationEntry {
        user_public_key: revoked.user_public_key,
        revoked_device_public_key: revoked.device_public_key,
        revoked_at_ns: 1,
        reason: RevocationReason::Lost,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut entry, &SigningKey::from_bytes(&[1; 32])).unwrap();
    workspaces
        .inner
        .host
        .revocations
        .write()
        .unwrap()
        .insert(entry)
        .unwrap();
    crate::authority_epoch::bump();

    tilo_test_env::within("the revoked device's links to end", async {
        while host.ended_link(revoked.device_public_key).is_none() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(
        host.ended_link(kept.device_public_key).is_none(),
        "another member's links stay up"
    );
    let ended = tokio::time::timeout(WAIT, revoked_link)
        .await
        .expect("the revoked device's link ends")
        .expect("its task ends with it");
    match ended {
        Err(tilo_server_protocol::refusal::Refusal::DeviceRevoked) => {}
        Err(refusal) => panic!("a revoked device's link ends as revoked: {refusal}"),
        Ok(()) => panic!("a revoked device's link is ended, not left running"),
    }
    assert!(
        !kept_link.is_finished(),
        "the other member's link keeps running"
    );

    // Its dial again is refused before it listens.
    let revoked_endpoint_key = revoked_key.verifying_key().to_bytes();
    let (again, _link, dialled) = dialed(
        &server,
        &workspaces,
        created.workspace_id,
        &revoked,
        &revoked_key,
        &cert,
        revoked_endpoint_key,
    )
    .await;
    match dialled {
        Err(tilo_server_protocol::refusal::Refusal::NotAMember(reason)) => assert!(
            reason.contains("revoked"),
            "the refusal says the device was revoked: {reason}"
        ),
        Err(refusal) => panic!("a revoked device is refused as no member: {refusal}"),
        Ok(_) => panic!("a revoked device's link is refused before it listens"),
    }
    kept_link.abort();
    again.close().await;
    revoked_endpoint.close().await;
    kept_endpoint.close().await;
    drop(workspaces);
    server.shutdown().await;
}

/// A member person's new device lists itself on the first link it dials: the
/// host admits that link to carry the device's own row and nothing else, ends
/// it once the row is in, and the workspace's member list, which its
/// governance keeps and the host projects, lists the device from then on. The
/// host writes no row for it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_member_peoples_new_device_lists_itself_on_its_first_link() {
    use automerge::ReadDoc as _;
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let created = workspaces.create("Team").await.unwrap();
    let (member_key, member) = further_device(&cert, 3);
    let link = workspaces
        .direct()
        .link(created.workspace_id)
        .expect("the server serves the workspace over a direct link");
    let reached = workspaces
        .admit_link(member.device_public_key, Some(&member), &link)
        .expect("the workspace lets its member's device list itself");
    assert!(
        reached.lists_itself && !reached.key_access && reached.own_frames_only,
        "a device the list does not list yet reaches its own row and nothing more: {reached:?}"
    );
    let running = list_itself_on_link(&server, &created.workspace, (&member_key, &member)).await;
    tilo_test_env::within("the member's device to be listed", async {
        loop {
            let held = runtime(&workspaces, created.workspace_id).await;
            if held
                .lock()
                .await
                .members
                .member_devices
                .contains(&member.device_public_key)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    let state = workspaces.state(&created.workspace).await.unwrap();
    let doc = state.admitted_workspace().unwrap();
    let (_, identities) = doc
        .get(automerge::ROOT, "peer_identities_v1")
        .unwrap()
        .expect("the workspace keeps its identities");
    let (_, row) = doc
        .get(&identities, hex::encode(member.device_public_key))
        .unwrap()
        .expect("the member list holds the device's row");
    let automerge::ObjId::Id(_, written_by, _) = row else {
        panic!("a row is a change of some device's");
    };
    let own = [
        automerge::ActorId::from(member.device_public_key.to_vec()),
        tilo_peer_state::workspace_host_actor(&member.device_public_key),
    ];
    assert!(
        own.contains(&written_by),
        "the device's row is its own signed change, not the host's"
    );
    running.abort();
    drop(workspaces);
    server.shutdown().await;
}

/// A hosted browser environment's device reaches its one record over the link
/// it dials, and no other: the register's scope is the link's.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hosted_environments_link_reads_its_one_record() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let created = workspaces.create("Team").await.unwrap();
    // The workspace's VM is certified for this one workspace, and the host that placed it
    // confines it in its register.
    let (_browser_key, browser) = device_cert(
        &SigningKey::from_bytes(&[1; 32]),
        "browser",
        DeviceCapabilities::workspace_device(*created.workspace_id.as_uuid().as_bytes()),
    );
    let scope = workspaces
        .inner
        .host
        .environments
        .confine(&browser)
        .unwrap()
        .expect("a device of one workspace is that workspace's VM");
    let link = workspaces
        .direct()
        .link(created.workspace_id)
        .expect("the server serves the workspace over a direct link");
    let reached = workspaces
        .admit_link(browser.device_public_key, None, &link)
        .expect("the server admits the environment it placed");
    assert_eq!(
        reached.reads,
        Some(vec![scope.channel]),
        "the environment's device reads its own record and no other"
    );
    assert!(
        reached.own_frames_only,
        "an environment's device passes no other device's frames"
    );
    drop(workspaces);
    server.shutdown().await;
}

/// An agent's device outside its delegation's workspaces is refused the
/// workspace's link: the admission hook reads the server's live delegations.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_agent_outside_its_grant_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let created = workspaces.create("Team").await.unwrap();
    let other = WorkspaceId::from_u128(u128::MAX - 7);
    let (agent_key, agent) = device_cert(
        &SigningKey::from_bytes(&[1; 32]),
        "agent",
        DeviceCapabilities::member(),
    );
    workspaces
        .enter(&created.workspace, (&agent, &agent_key))
        .await
        .expect("the agent's device lists itself");
    workspaces.inner.host.agents.write().unwrap().insert(
        agent.device_public_key,
        Some(Delegation {
            user_public_key: agent.user_public_key,
            device_public_key: agent.device_public_key,
            delegate: Delegate::Agent {
                agent_id: "agent".into(),
            },
            label: "Agent".into(),
            color: [0; 3],
            scope: DelegateScope {
                workspaces: vec![other],
                apps: Vec::new(),
            },
            issued_at_ns: 1,
            expires_at_ns: tilo_clock::unix_now_ns().saturating_add(60_000_000_000),
            signature: [0; 64],
        }),
    );
    let link = workspaces
        .direct()
        .link(created.workspace_id)
        .expect("the server serves the workspace over a direct link");
    match workspaces.admit_link(agent.device_public_key, None, &link) {
        Err(tilo_server_protocol::refusal::Refusal::NotAMember(reason)) => assert!(
            reason.contains("delegated"),
            "the refusal says the agent is not delegated this workspace: {reason}"
        ),
        Err(refusal) => panic!("an agent outside its grant is refused as no member: {refusal}"),
        Ok(_) => panic!("an agent outside its grant never links to the workspace"),
    }
    drop(workspaces);
    server.shutdown().await;
}

/// A guest's device admitted to `workspace` by its signed membership: a person
/// the operator's policy does not list, invited by the workspace's owner
/// `owner`, whose device signs its own admission, and whose person `settle`
/// adds to what the handshake trusts. With `further`, the person is in the
/// workspace already and the device lists itself as theirs.
async fn admitted_guest(
    workspaces: &Workspaces,
    workspace: &str,
    owner: &DeviceCert,
    (root, further): (&SigningKey, bool),
    label: &str,
) -> (DeviceCert, SigningKey) {
    let (member_key, member) =
        crate::workspaces::fixtures::device_cert(root, label, DeviceCapabilities::member());
    if further {
        workspaces
            .enter(workspace, (&member, &member_key))
            .await
            .expect("the device lists itself");
    } else {
        let token = [0x47; 8];
        let issued = workspaces
            .pre_admit(
                owner,
                crate::workspaces::PreAdmitRequest {
                    request_id: rand::random(),
                    grant: crate::workspaces::tests::signed_pre_admission(workspace, &token),
                    recipient_user_public_key: None,
                    inviter_name: None,
                },
            )
            .await
            .expect("the owner invites the guest");
        let reply = workspaces
            .admit_proven(
                issued.signed_invitation,
                (member.clone(), &member_key),
                rand::random(),
                label,
                Some(token.as_slice()),
            )
            .await;
        assert!(
            matches!(
                reply,
                tilo_server_protocol::workspaces::JoinReply::Admitted { .. }
            ),
            "{reply:?}"
        );
    }
    // What a settled governance decision does: the workspace's members reach
    // the host's handshake by their roots as well as its policy's own.
    let state = workspaces.workspace(workspace).await.unwrap();
    let mut governed = state.lock().await;
    workspaces
        .settle(&mut governed)
        .await
        .expect("the workspace settles the guest's admission");
    (member, member_key)
}

/// The operator's record of blocked keys, which their own workspace holds.
async fn operator_record(workspaces: &Workspaces) {
    crate::workspaces::person_workspace::found(workspaces)
        .await
        .expect("the operator's own workspace is founded here");
}

fn blocked_device(device: &[u8; 32]) -> tilo_resource::Invoke<crate::workspaces::blocked::Blocked> {
    crate::workspaces::blocked::Blocked::BLOCK_DEVICE.with(hex::encode(device))
}

fn unblocked_device(
    device: &[u8; 32],
) -> tilo_resource::Invoke<crate::workspaces::blocked::Blocked> {
    crate::workspaces::blocked::Blocked::UNBLOCK_DEVICE.with(hex::encode(device))
}

fn blocked_user(user: &[u8; 32]) -> tilo_resource::Invoke<crate::workspaces::blocked::Blocked> {
    crate::workspaces::blocked::Blocked::BLOCK_USER.with(hex::encode(user))
}

fn unblocked_user(user: &[u8; 32]) -> tilo_resource::Invoke<crate::workspaces::blocked::Blocked> {
    crate::workspaces::blocked::Blocked::UNBLOCK_USER.with(hex::encode(user))
}

/// Wait until the host holds no ended link to any of `devices`, as lifting a block finds it.
async fn until_no_links_remain(
    host: &tilo_peer_state::direct::DirectSync,
    devices: [[u8; 32]; 2],
    message: &str,
) {
    tilo_test_env::within(message, async {
        while devices
            .into_iter()
            .any(|device| host.ended_link(device).is_some())
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
}

/// The blocked device's next dial is refused before it listens.
async fn a_blocked_devices_next_dial_is_refused(
    (server, workspaces): (&TestHost, &Workspaces),
    workspace_id: WorkspaceId,
    (guest, guest_key): (&DeviceCert, &SigningKey),
    host_cert: &DeviceCert,
) {
    let (again, _link, dialed) = dialed(
        server,
        workspaces,
        workspace_id,
        guest,
        guest_key,
        host_cert,
        guest_key.verifying_key().to_bytes(),
    )
    .await;
    assert!(
        dialed.is_err(),
        "the host refuses the blocked device's dial"
    );
    again.close().await;
}

/// A second device of the guest's person, certified by their root, admitted to the workspace and
/// dialling it: the device and the link it keeps running.
async fn a_second_device_dialling(
    (server, workspaces): (&TestHost, &Workspaces),
    (workspace, workspace_id): (&str, WorkspaceId),
    (owner, root): (&DeviceCert, &SigningKey),
) -> (
    DeviceCert,
    Arc<Dialer>,
    tokio::task::JoinHandle<Result<(), tilo_server_protocol::refusal::Refusal>>,
) {
    let (second, second_key) =
        admitted_guest(workspaces, workspace, owner, (root, true), "guest-2").await;
    let (endpoint, link) = dialled_and_running(
        server,
        workspaces,
        workspace_id,
        &second,
        &second_key,
        owner,
    )
    .await;
    (second, endpoint, link)
}

/// The operator blocks `device`: the record of their own workspace says so, the host re-checks
/// what admission reads, and the link ends refused as this host's, not left running.
async fn blocking_the_device_ends_its_link(
    workspaces: &Workspaces,
    host: &tilo_peer_state::direct::DirectSync,
    device: [u8; 32],
    (endpoint, link): (
        Arc<Dialer>,
        tokio::task::JoinHandle<Result<(), tilo_server_protocol::refusal::Refusal>>,
    ),
) {
    workspaces
        .apply_blocked(blocked_device(&device))
        .await
        .expect("the block is written");
    tilo_test_env::within("the blocked device's link to end", async {
        while host.ended_link(device).is_none() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    let ended = tokio::time::timeout(WAIT, link)
        .await
        .expect("the blocked device's link ends")
        .expect("its task ends with it");
    match ended {
        Err(tilo_server_protocol::refusal::Refusal::NotAllowed(reason)) => assert_eq!(
            reason, "this host refuses this device",
            "the refusal says the host refuses the device"
        ),
        Err(refusal) => panic!("a blocked device's link ends refused: {refusal}"),
        Ok(()) => panic!("a blocked device's link is ended, not left running"),
    }
    endpoint.close().await;
}

/// Blocking `person` refuses every device of theirs: the host ends each link it holds as this
/// host's, and each ends refused.
async fn the_persons_blocked_key_ends_every_device_of_theirs(
    (workspaces, host): (&Workspaces, &tilo_peer_state::direct::DirectSync),
    person: &[u8; 32],
    devices: [[u8; 32]; 2],
    links: [(
        &'static str,
        tokio::task::JoinHandle<Result<(), tilo_server_protocol::refusal::Refusal>>,
    ); 2],
) {
    workspaces
        .apply_blocked(blocked_user(person))
        .await
        .expect("the person is blocked");
    tilo_test_env::within("the person's blocked devices' links to end", async {
        while devices
            .into_iter()
            .any(|device| host.ended_link(device).is_none())
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    for device in devices {
        assert_eq!(
            host.ended_link(device),
            Some(tilo_server_protocol::refusal::Refusal::NotAllowed(
                "this host refuses this device".into()
            )),
            "the person's blocked key ends every device of theirs"
        );
    }
    for (label, link) in links {
        let ended = tokio::time::timeout(WAIT, link)
            .await
            .expect("the person's blocked links end")
            .expect("its task ends with it");
        assert!(
            matches!(
                &ended,
                Err(tilo_server_protocol::refusal::Refusal::NotAllowed(reason))
                    if reason == "this host refuses this device"
            ),
            "{label}'s link to a device of the blocked person ends refused: {ended:?}"
        );
    }
}

/// A guest the workspace's membership holds reaches it over the link it dials.
/// The operator blocks its device: the link ends, refused as this host's, and
/// the guest's next dial is refused. Lifting the block admits it again. The
/// same person's other device is refused by blocking the person's root key.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_blocked_guest_link_ends_and_is_refused_until_the_operator_unblocks_it() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let created = workspaces.create("Team").await.unwrap();
    operator_record(&workspaces).await;

    let guest_root = SigningKey::from_bytes(&[0x51; 32]);
    let (guest, guest_key) = admitted_guest(
        &workspaces,
        &created.workspace,
        &cert,
        (&guest_root, false),
        "guest",
    )
    .await;
    let (guest_endpoint, guest_link) = dialled_and_running(
        &server,
        &workspaces,
        created.workspace_id,
        &guest,
        &guest_key,
        &cert,
    )
    .await;
    let host = workspaces
        .direct()
        .link(created.workspace_id)
        .expect("the server serves the workspace over a direct link");

    // The operator blocks the guest's device: the record of their own
    // workspace says so, and the host re-checks what admission reads.
    blocking_the_device_ends_its_link(
        &workspaces,
        &host,
        guest.device_public_key,
        (guest_endpoint, guest_link),
    )
    .await;

    // Its next dial is refused before it listens.
    a_blocked_devices_next_dial_is_refused(
        (&server, &workspaces),
        created.workspace_id,
        (&guest, &guest_key),
        &cert,
    )
    .await;

    // Lifted, the host opens the link to the device again and its next dial is
    // admitted.
    workspaces
        .apply_blocked(unblocked_device(&guest.device_public_key))
        .await
        .expect("the block is lifted");
    tilo_test_env::within("the device's link to be opened again", async {
        while host.ended_link(guest.device_public_key).is_some() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    let (guest_endpoint, guest_link) = dialled_and_running(
        &server,
        &workspaces,
        created.workspace_id,
        &guest,
        &guest_key,
        &cert,
    )
    .await;

    // The person's key refuses every device of theirs: the guest's own device
    // and a second one their root certified, admitted to the workspace.
    let (second, second_endpoint, second_link) = a_second_device_dialling(
        (&server, &workspaces),
        (&created.workspace, created.workspace_id),
        (&cert, &guest_root),
    )
    .await;
    the_persons_blocked_key_ends_every_device_of_theirs(
        (&workspaces, &host),
        &guest.user_public_key,
        [guest.device_public_key, second.device_public_key],
        [("guest", guest_link), ("guest-2", second_link)],
    )
    .await;
    guest_endpoint.close().await;
    second_endpoint.close().await;

    workspaces
        .apply_blocked(unblocked_user(&guest.user_public_key))
        .await
        .expect("the person is admitted again");
    until_no_links_remain(
        &host,
        [guest.device_public_key, second.device_public_key],
        "the person's links to be opened again",
    )
    .await;
    let (guest_endpoint, guest_link) = dialled_and_running(
        &server,
        &workspaces,
        created.workspace_id,
        &guest,
        &guest_key,
        &cert,
    )
    .await;
    guest_link.abort();
    guest_endpoint.close().await;
    drop(workspaces);
    server.shutdown().await;
}
