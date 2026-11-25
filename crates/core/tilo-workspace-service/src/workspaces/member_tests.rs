//! Member copies: entering a workspace another device serves, what the
//! workspace's governance does to the copy, and how it is kept across a
//! restart.
#![allow(
    clippy::large_futures,
    clippy::significant_drop_tightening,
    reason = "a fixture failing is its test failing, and each test keeps the serving device to the end"
)]

use std::sync::Arc;

use super::super::PreAdmitRequest;
use super::super::WorkspaceContent;
use super::super::WorkspaceSummary;
use super::super::fixtures::{
    TestGroupKeys, TestHost, device_cert, governor, invited, signed_pre_admission, test_host,
    test_host_holding,
};
use super::MemberDevice;
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::{DeviceCapabilities, DeviceCert};

/// A device serving workspace `name`, the device it invited, and the
/// invitation that device redeems.
async fn shared(
    serving_state: &std::path::Path,
    name: &str,
    seed: u8,
) -> (TestHost, (SigningKey, DeviceCert), WorkspaceSummary, String) {
    shared_with(serving_state, name, seed, laptop(seed)).await
}

thread_local! {
    /// The keys of the workspaces' every-member groups, as the devices of one test hold them: a
    /// group hands a device it admits the keys its other devices seal with. Each device holding
    /// keys of its own would seal what the others cannot open.
    static GROUP_KEYS: Arc<TestGroupKeys> = Arc::default();
}

/// The group keys the devices of this test share.
pub(super) fn group_keys() -> Arc<TestGroupKeys> {
    GROUP_KEYS.with(Arc::clone)
}

/// The laptop of the person of `seed`.
fn laptop(seed: u8) -> (SigningKey, DeviceCert) {
    device_cert(
        &SigningKey::from_bytes(&[seed; 32]),
        "laptop",
        DeviceCapabilities::member(),
    )
}

/// [`shared`], inviting `device`.
pub(super) async fn shared_with(
    serving_state: &std::path::Path,
    name: &str,
    seed: u8,
    device: (SigningKey, DeviceCert),
) -> (TestHost, (SigningKey, DeviceCert), WorkspaceSummary, String) {
    let (serving_key, serving_cert) = governor();
    let serving = test_host_holding(serving_state, &serving_key, &serving_cert, group_keys()).await;
    invited(serving, &serving_cert, name, seed, device).await
}

/// A service on `state`, serving the member copies of `(key, cert)` from the moment it opens.
pub(super) async fn member_service(
    state: &std::path::Path,
    (key, cert): (SigningKey, DeviceCert),
) -> TestHost {
    crate::workspaces::fixtures::test_host_holding_as_member(state, &key, &cert, group_keys()).await
}

/// A device that joins names its person as they asked, among the workspace's people: the serving
/// device that let it in writes the name, and its member list shows it.
#[tokio::test]
async fn a_device_that_joins_names_its_person_in_the_workspace() {
    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let (serving, device, created, invitation) = shared(serving_state.path(), "Named", 12).await;
    let member = member_service(member_state.path(), device).await;
    member
        .workspaces()
        .join_member_as(&invitation, Some("Ada Lovelace"))
        .await
        .expect("the invitation is redeemed");
    let named = serving
        .workspaces()
        .members(&created.workspace)
        .await
        .unwrap()
        .iter()
        .any(|member| member["name"] == "Ada Lovelace");
    assert!(named, "the serving device names the person who joined");
}

#[tokio::test]
async fn a_device_joins_a_workspace_its_invitation_names_and_holds_its_governance() {
    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let (_serving, device, created, invitation) = shared(serving_state.path(), "Shared", 11).await;
    let person = device.1.user_public_key;
    let member = member_service(member_state.path(), device).await;
    let entered = member
        .workspaces()
        .join_member(&invitation)
        .await
        .expect("the invitation is redeemed");
    assert_eq!(entered.to_string(), created.workspace);
    assert!(
        member
            .workspaces()
            .is_member(&created.workspace)
            .await
            .unwrap()
    );
    let state = member
        .workspaces()
        .state(&created.workspace)
        .await
        .expect("the member copy is held");
    assert_eq!(
        state.governance().workspace_name().as_deref(),
        Some("Shared")
    );
    // The surface this device's runtime reads its member copies by.
    let held = member.workspaces();
    assert!(held.member_ids().await.contains(&entered));
    let (name, copy) = held
        .member_copy(entered)
        .await
        .expect("the member copy is listed");
    assert_eq!(name, "Shared");
    assert_eq!(
        copy.governance().workspace_name().as_deref(),
        Some("Shared")
    );
    assert_eq!(held.member_actor(entered).await, Some(hex::encode(person)));
}

#[tokio::test]
async fn an_invitation_for_another_person_is_never_redeemed_here() {
    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let (serving, device, created, _) = shared(serving_state.path(), "Elsewhere", 12).await;
    // The serving device invites somebody else, and this device presents that
    // invitation.
    let serving_cert = serving.host().device_cert().clone();
    let stranger = device_cert(
        &SigningKey::from_bytes(&[9; 32]),
        "phone",
        DeviceCapabilities::member(),
    );
    let issued = serving
        .workspaces()
        .pre_admit(
            &serving_cert,
            PreAdmitRequest {
                request_id: [13; 32],
                grant: signed_pre_admission(&created.workspace, b"stranger"),
                recipient_user_public_key: Some(stranger.1.user_public_key),
                inviter_name: None,
            },
        )
        .await
        .expect("the serving device issues an invitation");
    let invitation = tilo_peer_state::join::encode_invitation_text(&issued.signed_invitation);
    let member = member_service(member_state.path(), device).await;
    assert!(
        member.workspaces().join_member(&invitation).await.is_err(),
        "an invitation for another person is refused"
    );
    assert!(
        !member
            .workspaces()
            .is_member(&created.workspace)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn a_member_copy_reads_the_governance_the_serving_device_publishes() {
    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let (serving, device, created, invitation) = shared(serving_state.path(), "Before", 14).await;
    let member = member_service(member_state.path(), device).await;
    member
        .workspaces()
        .join_member(&invitation)
        .await
        .expect("the invitation is redeemed");
    serving
        .workspaces()
        .rename(&created.workspace, "After")
        .await
        .expect("the serving device renames its workspace");
    let workspaces = member.workspaces().clone();
    let workspace = created.workspace.clone();
    tilo_test_env::within("the member reads the workspace's new name", async move {
        loop {
            let renamed = workspaces.state(&workspace).await.is_some_and(|state| {
                state.governance().workspace_name().as_deref() == Some("After")
            });
            if renamed {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await;
}

#[tokio::test]
async fn a_device_holding_a_workspace_answers_content_as_the_device_it_joined_as() {
    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let (serving, (key, cert), created, invitation) =
        shared(serving_state.path(), "Content", 21).await;

    // The member's own service as production opens a device's: it signs as the device, whose
    // key its endpoint is bound with and whose link its member copy answers on.
    let member = test_host_holding(member_state.path(), &key, &cert, group_keys()).await;
    member
        .workspaces()
        .set_member_device(Arc::new(MemberDevice {
            cert: cert.clone(),
            signer: Arc::new(key.clone()),
        }));
    assert_eq!(
        member.workspaces().device_cert().device_public_key,
        cert.device_public_key,
        "the service signs as the one device it is"
    );
    let member_keeps = Kept(b"a picture the member keeps".to_vec());
    member
        .workspaces()
        .serve_content(Arc::new(member_keeps.clone()));

    let workspace = created.workspace.clone();
    let copies = member.workspaces().clone();
    let workspace_id = copies
        .join_member(&invitation)
        .await
        .expect("the invitation is redeemed");
    catching_up(&copies, workspace_id).await;

    // The serving device asks for the picture, naming itself as every ask does.
    let serving_device = serving.workspaces().device_cert().device_public_key;
    let ask = postcard::to_allocvec(&NeighbourMsg::DocumentContentWants {
        requester: tilo_peer_state::peer_handle(&serving_device),
        cluster_id: workspace.clone(),
        wants: vec![neighbour_wire::content::Wanted {
            app_id: "so.tilo.photos".to_owned(),
            hash: member_keeps.address(),
            have: Vec::new(),
        }],
    })
    .unwrap();
    // What the member answers names the device its link proved: the one key it signs its
    // records and answers its content with.
    let answered = copies.content_answer(&workspace, &serving_device, &ask);
    assert!(!answered.is_empty(), "the member answers what it keeps");
    for frame in answered {
        let NeighbourMsg::DocumentContentPieces {
            responder, pieces, ..
        } = postcard::from_bytes(&frame).unwrap()
        else {
            panic!("the member answers with pieces of what it keeps");
        };
        assert!(!pieces.is_empty(), "an answer carries the pieces");
        assert_eq!(
            responder,
            tilo_peer_state::peer_handle(&cert.device_public_key),
            "the answer is addressed from the device key"
        );
    }
    let mut answers = serving.workspaces().content_answers();
    serving
        .workspaces()
        .ask_members(&workspace, ask)
        .await
        .expect("the workspace's link carries the ask");
    let answer = tilo_test_env::within("the member's answer reaches the serving device", async {
        loop {
            let heard = answers.recv().await.expect("the answers are told");
            if heard.workspace == workspace && !heard.lacks {
                return heard;
            }
        }
    })
    .await;
    assert_eq!(
        answer.from, cert.device_public_key,
        "the answer names the device key the member signs its link as, not its service's"
    );

    drop(copies);
    drop(answers);
    member.shutdown().await;
    serving.shutdown().await;
}

/// The bytes a member keeps, named by their address.
#[derive(Clone)]
struct Kept(Vec<u8>);

impl Kept {
    fn address(&self) -> String {
        blake3::hash(&self.0).to_hex().to_string()
    }
}

impl WorkspaceContent for Kept {
    fn read(&self, _workspace: &str, _app: &str, hash: &str) -> Option<Vec<u8>> {
        (hash == self.address()).then(|| self.0.clone())
    }
}

#[tokio::test]
async fn a_member_copy_is_held_as_one_when_the_service_opens_again() {
    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let (_serving, device, created, invitation) = shared(serving_state.path(), "Kept", 15).await;
    {
        let member = member_service(member_state.path(), device.clone()).await;
        member
            .workspaces()
            .join_member(&invitation)
            .await
            .expect("the invitation is redeemed");
        member.shutdown().await;
    }
    let reopened = member_service(member_state.path(), device).await;
    assert!(
        reopened
            .workspaces()
            .is_member(&created.workspace)
            .await
            .unwrap(),
        "the service keeps the workspace as a member copy"
    );
}

#[tokio::test]
async fn a_member_copy_held_again_with_its_link_up_keeps_that_link() {
    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let (serving, device, created, invitation) = shared(serving_state.path(), "Held", 17).await;
    let workspace = created.workspace.clone();
    let workspace_id =
        tilo_server_protocol::WorkspaceId::parse(&workspace).expect("a workspace id");
    let member = member_service(member_state.path(), device).await;
    let copies = member.workspaces().clone();
    copies
        .join_member(&invitation)
        .await
        .expect("the invitation is redeemed");
    copies
        .wait_member_link(workspace_id)
        .await
        .expect("the member copy's link is up");
    let linked = copies
        .member_syncing(workspace_id)
        .expect("the copy is synced");
    let hint = copies
        .member_hint(workspace_id)
        .await
        .expect("the copy names the device serving it");

    // The serving device renames the workspace, and this copy is held again by the same device
    // before it takes the name in: holding a copy again never drops the link it has.
    serving
        .workspaces()
        .rename(&workspace, "Held still")
        .await
        .expect("the serving device renames its workspace");
    copies
        .hold_as_member(workspace_id, &hint)
        .await
        .expect("the copy is held again");
    assert_eq!(
        copies
            .member_syncing(workspace_id)
            .expect("the copy is synced"),
        linked,
        "the copy kept the link it had to the device serving it"
    );

    // What the serving device changed in between still reaches the copy.
    let workspaces = copies.clone();
    let renamed = workspace.clone();
    tilo_test_env::within("the copy reads the name sent in between", async move {
        loop {
            let renamed = workspaces.state(&renamed).await.is_some_and(|state| {
                state.governance().workspace_name().as_deref() == Some("Held still")
            });
            if renamed {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
}

#[tokio::test]
async fn a_members_write_reaches_the_device_serving_the_workspace() {
    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let (serving, device, created, invitation) = shared(serving_state.path(), "Working", 16).await;
    let member_key = device.1.device_public_key;
    let member = member_service(member_state.path(), device).await;
    member
        .workspaces()
        .join_member(&invitation)
        .await
        .expect("the invitation is redeemed");
    let write = member
        .workspaces()
        .member_membership_write(
            &created.workspace,
            tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::Lease {
                id: "machine".into(),
                ttl_ms: 60_000,
                rank: tilo_peer_state::unranked(),
            },
        )
        .await
        .expect("the lease is written");
    member
        .workspaces()
        .commit_member_document(&created.workspace, write)
        .await
        .expect("the write is kept");
    let workspaces = serving.workspaces().clone();
    let workspace = created.workspace.clone();
    tilo_test_env::within("the serving device reads the member's write", async move {
        loop {
            let holder = workspaces.state(&workspace).await.and_then(|state| {
                state
                    .workspace()
                    .lease("machine", tilo_clock::wall())
                    .holder
            });
            if holder.is_some_and(|claim| claim.device == member_key) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await;
}

/// A document holding `value` under `key`.
fn snapshot(key: &str, value: &str) -> Vec<u8> {
    use automerge::transaction::Transactable as _;

    let mut document = automerge::AutoCommit::new();
    document
        .put(automerge::ROOT, key, value)
        .expect("the record's field");
    document.save()
}

/// The device serving `workspace` founds `(app, document)` holding `value` under `key`.
async fn serving_writes(
    serving: &TestHost,
    workspace: &str,
    (app, document): (&str, &str),
    (key, value): (&str, &str),
) {
    serving
        .workspaces()
        .own_write(
            workspace,
            (app, document),
            None,
            &snapshot(key, value),
            true,
            true,
            None,
        )
        .await
        .expect("the serving device writes its record");
}

/// `workspace`'s member copy caught up with the history its serving device keeps: what it did not
/// hold before it arrived, it holds now.
pub(super) async fn catching_up(copies: &super::Workspaces, workspace: WorkspaceId) {
    let mut caught_up = copies
        .member_caught_up(workspace)
        .expect("the copy is linked to the device serving it");
    tilo_test_env::within(
        "the member copy catches up with its serving device",
        async {
            caught_up
                .wait_for(|caught_up| *caught_up)
                .await
                .expect("the copy's link stays open");
        },
    )
    .await;
}

/// A change of `workspace`'s member copy to `(app, document)`: `value` under `key`, on what the
/// copy reads of it now.
async fn change_of(
    copies: &super::Workspaces,
    workspace: &str,
    (app, document): (&str, &str),
    (key, value): (&str, &str),
    person: [u8; 32],
) -> Vec<u8> {
    use automerge::transaction::Transactable as _;
    use collab_types::claim::CommitNamed as _;
    use tilo_peer_state::workspace_documents::DocumentRead;

    let read = copies
        .own_read(workspace, (app, document), &[], 64 * 1024 * 1024)
        .await
        .expect("the record reads");
    let DocumentRead::Whole { bytes, .. } = read else {
        panic!("a record the copy holds reads whole, not {read:?}");
    };
    let mut opened = automerge::AutoCommit::load(&bytes).expect("the record opens");
    let heads = opened.get_heads();
    opened
        .put(automerge::ROOT, key, value)
        .expect("the record's field");
    // A change names the action it makes: the debug build refuses to sign one that does not.
    opened.commit_edit(app, key, &person);
    opened.save_after(&heads)
}

/// What the serving device's copy of a record holds under `key`.
async fn served_value(
    workspaces: &super::Workspaces,
    workspace: &str,
    (app, document): (&str, &str),
    key: &str,
) -> Option<String> {
    use automerge::ReadDoc as _;

    let bytes = workspaces
        .resource_document(workspace, app, document)
        .await
        .ok()
        .flatten()?;
    let opened = automerge::AutoCommit::load(&bytes).expect("the record opens");
    match opened
        .get(automerge::ROOT, key)
        .expect("the record's field")
    {
        Some((automerge::Value::Scalar(value), _)) => value.as_str().map(str::to_owned),
        _ => None,
    }
}

/// A member copy writes a record the serving device founds; `served` answers only once that
/// device holds the write, and reading its copy of the record shows the change there.
#[tokio::test]
async fn served_answers_once_the_serving_device_holds_the_write() {
    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let (serving, device, created, invitation) = shared(serving_state.path(), "Answered", 41).await;
    let workspace = created.workspace.clone();
    serving_writes(
        &serving,
        &workspace,
        ("test.app", "record"),
        ("value", "by-the-serving-device"),
    )
    .await;
    let person = device.1.user_public_key;
    let member = member_service(member_state.path(), device).await;
    let copies = member.workspaces().clone();
    let workspace_id = copies
        .join_member(&invitation)
        .await
        .expect("the invitation is redeemed");
    catching_up(&copies, workspace_id).await;
    copies
        .own_write(
            &workspace,
            ("test.app", "record"),
            None,
            &change_of(
                &copies,
                &workspace,
                ("test.app", "record"),
                ("value", "written"),
                person,
            )
            .await,
            false,
            false,
            None,
        )
        .await
        .expect("the write is taken here");
    tilo_test_env::within("the serving device answers the write", async {
        copies
            .served(&workspace, &[("test.app", "record")])
            .expect("a member copy is answered by the device serving it")
            .await
            .expect("the serving device answers the write");
    })
    .await;
    assert_eq!(
        served_value(
            serving.workspaces(),
            &workspace,
            ("test.app", "record"),
            "value"
        )
        .await
        .as_deref(),
        Some("written"),
        "the serving device held the write served answered for"
    );
    drop(copies);
    member.shutdown().await;
}

/// The serving device refuses a write; `served` answers with the refusal rather than waiting.
#[tokio::test]
async fn served_answers_with_the_refusal_of_a_write_the_workspace_refused() {
    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let (serving, device, created, invitation) = shared(serving_state.path(), "Refused", 42).await;
    let workspace = created.workspace.clone();
    serving_writes(
        &serving,
        &workspace,
        ("test.app", "record"),
        ("value", "by-the-serving-device"),
    )
    .await;
    let person = device.1.user_public_key;
    let member = member_service(member_state.path(), device).await;
    let copies = member.workspaces().clone();
    let workspace_id = copies
        .join_member(&invitation)
        .await
        .expect("the invitation is redeemed");
    catching_up(&copies, workspace_id).await;
    let change = change_of(
        &copies,
        &workspace,
        ("test.app", "record"),
        ("value", "a-change"),
        person,
    )
    .await;
    let write = copies
        .own_write(
            &workspace,
            ("test.app", "record"),
            None,
            &change,
            false,
            false,
            None,
        )
        .await
        .expect("the change is taken here");
    copies
        .served(&workspace, &[("test.app", "record")])
        .expect("a member copy is answered by the device serving it")
        .await
        .expect("the change is kept");
    // The same signed change sent as another document's: the device serving the workspace refuses
    // it, since its op names the record it may not stand for.
    let elsewhere = neighbour_wire::NeighbourMsg::AppDocument {
        peer: write.peer,
        app_id: write.app_id.clone(),
        cluster_id: write.cluster_id.clone(),
        document_id: "elsewhere".into(),
        envelope: Box::new(write.envelope.clone()),
        readers: None,
    };
    copies
        .commit_member_document(
            &workspace,
            crate::workspace_documents::HostCommit {
                body: postcard::to_stdvec(&elsewhere).expect("the write encodes"),
            },
        )
        .await
        .expect("the write is taken here");
    let refused = tilo_test_env::within("the serving device refuses the write", async {
        copies
            .served(&workspace, &[("test.app", "elsewhere")])
            .expect("a member copy is answered by the device serving it")
            .await
    })
    .await;
    assert!(
        refused.is_err(),
        "the workspace refused the write, so served says so: {refused:?}"
    );
    drop(copies);
    member.shutdown().await;
}

/// An answer lost with the link it was written on leaves the write waiting; the copy asks for it
/// again once a link caught up, and `served` answers then.
#[tokio::test]
async fn served_answers_a_write_whose_answer_was_lost_once_it_is_asked_again() {
    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let (serving, device, created, invitation) = shared(serving_state.path(), "Lost", 43).await;
    let workspace = created.workspace.clone();
    serving_writes(
        &serving,
        &workspace,
        ("test.app", "record"),
        ("value", "by-the-serving-device"),
    )
    .await;
    let person = device.1.user_public_key;
    let member = member_service(member_state.path(), device).await;
    let copies = member.workspaces().clone();
    let workspace_id = copies
        .join_member(&invitation)
        .await
        .expect("the invitation is redeemed");
    catching_up(&copies, workspace_id).await;
    // The serving device keeps the write; its answer is lost on the way back.
    copies.lose_member_answers(workspace_id, 1);
    copies
        .own_write(
            &workspace,
            ("test.app", "record"),
            None,
            &change_of(
                &copies,
                &workspace,
                ("test.app", "record"),
                ("value", "written"),
                person,
            )
            .await,
            false,
            false,
            None,
        )
        .await
        .expect("the write is taken here");
    let unasked = tokio::time::timeout(
        std::time::Duration::from_millis(300),
        copies
            .served(&workspace, &[("test.app", "record")])
            .expect("a member copy is answered by the device serving it"),
    )
    .await;
    assert!(
        unasked.is_err(),
        "a write whose answer was lost waits: {unasked:?}"
    );
    // What a link that caught up does with a write still waiting.
    copies.reask_member_writes(workspace_id);
    tilo_test_env::within("the write is answered once it is asked again", async {
        copies
            .served(&workspace, &[("test.app", "record")])
            .expect("a member copy is answered by the device serving it")
            .await
            .expect("the write is answered once it is asked again");
    })
    .await;
    drop(copies);
    member.shutdown().await;
}

/// A device's write to a workspace another device serves is kept here while that device cannot be
/// reached, never refused: the person is told nothing, and the write reaches the serving device
/// once the link is back.
#[tokio::test]
async fn a_write_made_while_the_serving_device_is_away_is_kept_and_reaches_it_later() {
    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let (serving, device, created, invitation) = shared(serving_state.path(), "Away", 47).await;
    let workspace = created.workspace.clone();
    serving_writes(
        &serving,
        &workspace,
        ("test.app", "record"),
        ("value", "by-the-serving-device"),
    )
    .await;
    let person = device.1.user_public_key;
    let member = member_service(member_state.path(), device).await;
    let copies = member.workspaces().clone();
    let workspace_id = copies
        .join_member(&invitation)
        .await
        .expect("the invitation is redeemed");
    catching_up(&copies, workspace_id).await;
    let change = change_of(
        &copies,
        &workspace,
        ("test.app", "record"),
        ("value", "written-while-away"),
        person,
    )
    .await;
    copies.set_member_link(workspace_id, false);

    let written = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        copies.own_write(
            &workspace,
            ("test.app", "record"),
            None,
            &change,
            false,
            false,
            None,
        ),
    )
    .await
    .expect("a write does not wait for the serving device");
    assert!(
        written.is_ok(),
        "a write made while the serving device is away is kept, not refused: {:?}",
        written.err()
    );
    assert_eq!(
        served_value(&copies, &workspace, ("test.app", "record"), "value")
            .await
            .as_deref(),
        Some("written-while-away"),
        "the write reads back here at once"
    );

    copies.set_member_link(workspace_id, true);
    tilo_test_env::within(
        "the write reaches the serving device once the link is back",
        async {
            copies
                .served(&workspace, &[("test.app", "record")])
                .expect("the copy waits on its serving device")
                .await
                .expect("the serving device keeps the write");
        },
    )
    .await;
    assert_eq!(
        served_value(
            serving.workspaces(),
            &workspace,
            ("test.app", "record"),
            "value"
        )
        .await
        .as_deref(),
        Some("written-while-away")
    );
    drop(copies);
    member.shutdown().await;
    serving.shutdown().await;
}

/// A member's own change of the workspace, saved before its link to the device serving the
/// workspace is made, is kept rather than dropped: the serving device holds it once the link is
/// there, as it holds one written while that device was away.
#[tokio::test]
async fn a_change_saved_before_the_link_is_made_reaches_the_serving_device() {
    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let (serving, device, created, invitation) = shared(serving_state.path(), "Linked", 49).await;
    let workspace = created.workspace.clone();
    let member = member_service(member_state.path(), device).await;
    let copies = member.workspaces().clone();
    let workspace_id = copies
        .join_member(&invitation)
        .await
        .expect("the invitation is redeemed");
    catching_up(&copies, workspace_id).await;

    // The link this copy writes over is not made yet, as it is not while the copy is still
    // taking the workspace in.
    let link = copies
        .take_member_link(workspace_id)
        .expect("the copy has a link to take out");
    let shortcut = crate::workspaces::fixtures::on_issue_change();
    let id = shortcut.id.clone();
    copies
        .change_own_shortcuts(
            &workspace,
            tilo_automation::ShortcutChange::Save(Box::new(shortcut)),
        )
        .await
        .expect("the person saves their shortcut while no link is made");
    assert!(
        !serving_shortcuts(&serving, &workspace).await.contains(&id),
        "the serving device holds a shortcut this copy has not sent"
    );

    copies.give_member_link(workspace_id, link);
    let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
    while !serving_shortcuts(&serving, &workspace).await.contains(&id) {
        assert!(
            std::time::Instant::now() < deadline,
            "the serving device never took the shortcut this copy saved before its link was made"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    drop(copies);
    member.shutdown().await;
    serving.shutdown().await;
}

/// The ids of the shortcuts `host` holds of `workspace`.
async fn serving_shortcuts(host: &TestHost, workspace: &str) -> Vec<String> {
    host.workspaces()
        .described()
        .await
        .iter()
        .filter(|held| held.id().to_string() == workspace)
        .flat_map(|held| held.shortcuts.iter().map(|saved| saved.id().to_owned()))
        .collect()
}

/// A change saved before this device restarted, with no link made yet, reaches the device
/// serving the workspace once the reopened copy makes its link: the frame waits in the device
/// store, not only in the queue a process keeps.
#[tokio::test]
async fn a_change_saved_before_a_restart_reaches_the_serving_device() {
    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let (serving, device, created, invitation) =
        shared(serving_state.path(), "Restarted", 50).await;
    let workspace = created.workspace.clone();
    let held = device.clone();
    let member = member_service(member_state.path(), device).await;
    let copies = member.workspaces().clone();
    let workspace_id = copies
        .join_member(&invitation)
        .await
        .expect("the invitation is redeemed");
    catching_up(&copies, workspace_id).await;
    let _link = copies
        .take_member_link(workspace_id)
        .expect("the copy has a link to take out");
    let shortcut = crate::workspaces::fixtures::on_issue_change();
    let id = shortcut.id.clone();
    copies
        .change_own_shortcuts(
            &workspace,
            tilo_automation::ShortcutChange::Save(Box::new(shortcut)),
        )
        .await
        .expect("the person saves their shortcut while no link is made");
    assert!(
        !serving_shortcuts(&serving, &workspace).await.contains(&id),
        "the serving device holds a shortcut this copy has not sent"
    );
    drop(copies);
    member.shutdown().await;

    // The device starts again on what it kept, without the link it had taken out.
    let member = member_service(member_state.path(), held).await;
    let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
    while !serving_shortcuts(&serving, &workspace).await.contains(&id) {
        assert!(
            std::time::Instant::now() < deadline,
            "the serving device never took the shortcut this copy saved before it restarted"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    member.shutdown().await;
    serving.shutdown().await;
}

/// A write made while this device is still taking a workspace in (it joins, or opens the
/// workspace as it starts) waits rather than being refused: the person hears nothing, and the same
/// write, made again once the device holds the workspace, is kept and reaches its serving device.
#[tokio::test]
async fn a_write_before_the_workspace_is_held_here_waits_and_lands_once_it_is() {
    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let (serving, device, created, invitation) = shared(serving_state.path(), "Joining", 48).await;
    let workspace = created.workspace.clone();
    serving_writes(
        &serving,
        &workspace,
        ("test.app", "record"),
        ("value", "by-the-serving-device"),
    )
    .await;
    let person = device.1.user_public_key;
    let member = member_service(member_state.path(), device).await;
    let copies = member.workspaces().clone();

    let early = copies
        .own_write(
            &workspace,
            ("test.app", "record"),
            None,
            &snapshot("value", "written-while-joining"),
            false,
            false,
            None,
        )
        .await;
    assert_eq!(
        early.err(),
        Some(tilo_peer_state::owner::WriteRefusal::Waiting(
            tilo_peer_state::MissingPrerequisite::Workspace
        )),
        "a write before the workspace is held here waits for it, not refused"
    );

    let workspace_id = copies
        .join_member(&invitation)
        .await
        .expect("the invitation is redeemed");
    catching_up(&copies, workspace_id).await;
    let change = change_of(
        &copies,
        &workspace,
        ("test.app", "record"),
        ("value", "written-while-joining"),
        person,
    )
    .await;
    copies
        .own_write(
            &workspace,
            ("test.app", "record"),
            None,
            &change,
            false,
            false,
            None,
        )
        .await
        .expect("the write is kept once the workspace is held here");
    tilo_test_env::within("the write reaches the serving device", async {
        copies
            .served(&workspace, &[("test.app", "record")])
            .expect("the copy waits on its serving device")
            .await
            .expect("the serving device keeps the write");
    })
    .await;
    assert_eq!(
        served_value(
            serving.workspaces(),
            &workspace,
            ("test.app", "record"),
            "value"
        )
        .await
        .as_deref(),
        Some("written-while-joining")
    );
    drop(copies);
    member.shutdown().await;
    serving.shutdown().await;
}

/// A member copy says when the history of the device serving it is in hand, and a workspace this
/// device governs has no such link to wait on.
#[tokio::test]
async fn a_member_copy_says_when_it_caught_up_with_its_serving_device() {
    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let (serving, device, created, invitation) = shared(serving_state.path(), "Caught", 44).await;
    let member = member_service(member_state.path(), device).await;
    let copies = member.workspaces().clone();
    let workspace_id = copies
        .join_member(&invitation)
        .await
        .expect("the invitation is redeemed");
    catching_up(&copies, workspace_id).await;
    let caught_up = copies
        .member_caught_up(workspace_id)
        .expect("the copy is linked to the device serving it");
    assert!(*caught_up.borrow());
    let governed =
        WorkspaceId::parse(&created.workspace).expect("the serving device's workspace id");
    assert!(
        serving.workspaces().member_caught_up(governed).is_none(),
        "a workspace this device governs answers through no serving device"
    );
    drop(copies);
    member.shutdown().await;
}

/// A folder whose workspace this device does not hold says it waits for it, and starts by
/// itself once the workspace arrives: no timer, the arrival wakes it.
#[cfg(feature = "files")]
#[tokio::test]
async fn a_folder_waits_for_a_workspace_this_device_does_not_hold_yet() {
    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let (serving, device, created, invitation) = shared(serving_state.path(), "Waiting", 46).await;
    let (member_key, member_cert) = device;
    let workspace_id =
        WorkspaceId::parse(&created.workspace).expect("the serving device's workspace id");
    let member = member_service(member_state.path(), (member_key, member_cert.clone())).await;
    let copies = member.workspaces().clone();
    let kept = tempfile::tempdir().unwrap();
    let store = Arc::new(
        tilo_fs::ContentStore::open(kept.path().join("content"))
            .await
            .expect("a content store for the folder"),
    );
    let folder = crate::files::start(
        &copies,
        workspace_id,
        &kept.path().join("tree"),
        (member_cert.device_public_key, member_cert.user_public_key),
        Some(store),
        Some(crate::content::tests::hosts(false, false).a),
    )
    .await
    .expect("the folder opens");
    tilo_test_env::within(
        "the folder says it waits for a workspace this device does not hold",
        async {
            while !folder.status().waiting_for_workspace {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        },
    )
    .await;
    let joined = copies
        .join_member(&invitation)
        .await
        .expect("the invitation is redeemed");
    assert_eq!(joined, workspace_id);
    catching_up(&copies, workspace_id).await;
    tilo_test_env::within(
        "the folder stops waiting when the workspace arrives",
        async {
            while folder.status().waiting_for_workspace {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        },
    )
    .await;
    drop(folder);
    drop(copies);
    drop(serving);
    member.shutdown().await;
}

/// A write the device serving a member copy makes is told to whoever follows the copy's
/// records, naming its app and document: what a folder's sync wakes on instead of reading every
/// record again to notice the serving device's edits.
#[tokio::test]
async fn a_serving_devices_write_reaches_a_member_copys_followers() {
    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let (serving, device, created, invitation) = shared(serving_state.path(), "Followed", 45).await;
    let workspace = created.workspace.clone();
    // The record is one the copy joins with, so the serving device's later change of it is a
    // write the copy takes in as it comes.
    serving_writes(
        &serving,
        &workspace,
        ("test.app", "record"),
        ("value", "as-the-copy-joined"),
    )
    .await;
    let member = member_service(member_state.path(), device).await;
    let copies = member.workspaces().clone();
    let workspace_id = copies
        .join_member(&invitation)
        .await
        .expect("the invitation is redeemed");
    catching_up(&copies, workspace_id).await;
    let mut writes = copies.document_writes();
    let change = change_of(
        serving.workspaces(),
        &workspace,
        ("test.app", "record"),
        ("value", "from-the-serving-device"),
        serving.workspaces().device_cert().user_public_key,
    )
    .await;
    serving
        .workspaces()
        .own_write(
            &workspace,
            ("test.app", "record"),
            None,
            &change,
            false,
            false,
            None,
        )
        .await
        .expect("the serving device changes the record");
    let told = tilo_test_env::within(
        "the serving device's write is told to the copy's followers",
        async {
            loop {
                match writes.recv().await {
                    Ok(write) => {
                        if write.workspace == workspace
                            && write.app == "test.app"
                            && write.record == "record"
                        {
                            return write;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                        panic!("the copy's followers fell {missed} writes behind");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        panic!("the copy stopped telling its followers");
                    }
                }
            }
        },
    )
    .await;
    assert_eq!(told.app, "test.app");
    assert_eq!(told.record, "record");
    drop(copies);
    member.shutdown().await;
}

/// When a device's keys arrive, its copy takes in again every write it kept, and on a large
/// workspace that is minutes of work on the thread that also runs the device's endpoint. The
/// device's other work runs between two kept writes, not after the last: a follower hears of
/// the first while the rest are still being taken in. Otherwise the device answers nothing
/// meanwhile, and its own dials of its server time out as if the server were gone.
#[tokio::test]
async fn a_copy_taking_in_its_kept_writes_lets_the_devices_other_work_run_meanwhile() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    // The serving device seals what it writes, as a device taking part in the workspace's key
    // groups does: a sealed write is what a copy keeps until its keys open it.
    let (serving_key, serving_cert) = governor();
    let serving =
        super::super::fixtures::test_host(serving_state.path(), &serving_key, &serving_cert).await;
    let (serving, device, created, invitation) =
        invited(serving, &serving_cert, "Kept", 46, laptop(46)).await;
    let workspace = created.workspace.clone();
    let member = member_service(member_state.path(), device).await;
    let copies = member.workspaces().clone();
    let workspace_id = copies
        .join_member(&invitation)
        .await
        .expect("the invitation is redeemed");
    catching_up(&copies, workspace_id).await;
    let records = ["first", "second", "third"];
    for record in records {
        let route = tilo_peer_state::document_frame::DocumentRoute {
            app_id: "test.app".into(),
            cluster_id: workspace.clone(),
            document: record.into(),
        };
        let sealed = serving
            .workspaces()
            .own_route_keys(&route, true)
            .await
            .expect("the serving device's key service answers")
            .expect("the group opens");
        serving
            .workspaces()
            .own_write(
                &workspace,
                ("test.app", record),
                None,
                &snapshot("value", record),
                true,
                true,
                Some(&sealed),
            )
            .await
            .expect("the serving device writes its record");
    }
    tilo_test_env::within("the copy keeps the serving device's writes", async {
        loop {
            let kept = copies
                .inner
                .records
                .retained(workspace_id).await
                .expect("the copy's kept writes read")
                .iter()
                .filter(|frame| {
                    matches!(
                        neighbour_wire::NeighbourMsg::decode(&frame.body),
                        Ok(neighbour_wire::NeighbourMsg::AppDocument { app_id, .. }) if app_id == "test.app"
                    )
                })
                .count();
            if kept >= records.len() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;

    let mut writes = copies.document_writes();
    let taken_in = Arc::new(AtomicBool::new(false));
    let follower = {
        let taken_in = Arc::clone(&taken_in);
        let workspace = workspace.clone();
        tokio::spawn(async move {
            loop {
                match writes.recv().await {
                    Ok(write) if write.workspace == workspace && write.app == "test.app" => {
                        return taken_in.load(Ordering::SeqCst);
                    }
                    Ok(_) => {}
                    Err(error) => panic!("the copy stopped telling its followers: {error}"),
                }
            }
        })
    };
    copies.take_in_what_keys_open(&workspace).await;
    taken_in.store(true, Ordering::SeqCst);
    let heard_after_the_last = follower.await.expect("the follower runs");
    assert!(
        !heard_after_the_last,
        "a follower on the device's runtime heard of a kept write only once all of them were in"
    );
    drop(copies);
    member.shutdown().await;
    serving.shutdown().await;
}

/// The front `workspace`'s layout stands on, as its member copy reads it now.
#[cfg(feature = "apps")]
async fn front(copies: &super::Workspaces, workspace: &str) -> Option<String> {
    use automerge::ReadDoc as _;
    use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_DOCUMENT};
    use tilo_peer_state::workspace_documents::DocumentRead;

    let read = copies
        .own_read(
            workspace,
            (DESKTOP_APP, DESKTOP_DOCUMENT),
            &[],
            64 * 1024 * 1024,
        )
        .await
        .expect("the layout reads");
    let DocumentRead::Whole { bytes, .. } = read else {
        return None;
    };
    let document = automerge::AutoCommit::load(&bytes).expect("the layout opens");
    match document
        .get(automerge::ROOT, "front")
        .expect("the layout's front")
    {
        Some((automerge::Value::Scalar(value), _)) => value.as_str().map(str::to_owned),
        _ => None,
    }
}

/// Wait until the member copy reads `want` as its layout's front: the write comes back to the copy
/// from the serving device, as a member's own write does.
#[cfg(feature = "apps")]
async fn waiting_front(copies: &super::Workspaces, workspace: &str, want: &str) {
    tilo_test_env::within("the member copy reads the layout it wrote", async {
        loop {
            if front(copies, workspace).await.as_deref() == Some(want) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
}

/// A member copy's own layout write reaches the workspace and reads back at once, a later change
/// builds on it, and the copy keeps both across a restart of this device's service: what a joined
/// workspace's window positions go through once the session holds it as a member copy.
#[cfg(feature = "apps")]
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one join, the founding write, a change of it and the restart that reads it back"
)]
async fn a_member_copies_layout_write_is_read_back_and_survives_a_restart() {
    use automerge::transaction::Transactable as _;
    use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_DOCUMENT};

    use super::super::fixtures::{TestGroupKeys, test_host_holding};

    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let keys = Arc::new(TestGroupKeys::default());
    let (serving_key, serving_cert) = governor();
    let serving = test_host_holding(
        serving_state.path(),
        &serving_key,
        &serving_cert,
        Arc::clone(&keys),
    )
    .await;
    let created = serving
        .workspaces()
        .create("Layout")
        .await
        .expect("a workspace is created");
    let workspace = created.workspace.clone();
    let (key, cert) = device_cert(
        &SigningKey::from_bytes(&[21; 32]),
        "laptop",
        DeviceCapabilities::member(),
    );
    let issued = serving
        .workspaces()
        .pre_admit(
            &serving_cert,
            PreAdmitRequest {
                request_id: [21; 32],
                grant: signed_pre_admission(&workspace, &[21; 8]),
                recipient_user_public_key: Some(cert.user_public_key),
                inviter_name: None,
            },
        )
        .await
        .expect("the serving device issues an invitation");
    let invitation = tilo_peer_state::join::encode_invitation_text(&issued.signed_invitation);

    // This device's service acts as this device, as a desktop's does, and takes part in the
    // workspace's groups with the keys its key service hands it.
    let member = test_host_holding(member_state.path(), &key, &cert, Arc::clone(&keys)).await;
    let copies = member.workspaces().clone();
    copies.set_member_device(Arc::new(MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key.clone()),
    }));
    assert_eq!(
        copies
            .join_member(&invitation)
            .await
            .expect("the invitation is redeemed")
            .to_string(),
        workspace
    );
    let route = tilo_peer_state::document_frame::DocumentRoute {
        app_id: DESKTOP_APP.into(),
        cluster_id: workspace.clone(),
        document: DESKTOP_DOCUMENT.into(),
    };
    let sealed = copies
        .own_route_keys(&route, true)
        .await
        .expect("the device's key service answers")
        .expect("the group opens");

    let mut told = copies.document_writes();
    let mut layout = automerge::AutoCommit::new();
    layout
        .put(automerge::ROOT, "front", "Main")
        .expect("the layout's front");
    copies
        .own_write(
            &workspace,
            (DESKTOP_APP, DESKTOP_DOCUMENT),
            None,
            &layout.save(),
            true,
            true,
            Some(&sealed),
        )
        .await
        .expect("the layout is written");
    let told = loop {
        let write = told
            .try_recv()
            .expect("the copy tells its followers its own write");
        if write.app == DESKTOP_APP && write.record == DESKTOP_DOCUMENT {
            break write;
        }
    };
    assert_eq!(told.workspace, workspace);
    waiting_front(&copies, &workspace, "Main").await;
    let heads = layout.get_heads();
    layout
        .put(automerge::ROOT, "front", "Moved")
        .expect("the layout's front moves");
    // Committed as the device's own edit, naming the person it is for: a change that names no
    // one is refused.
    layout.commit_with(
        automerge::transaction::CommitOptions::default().with_message(
            crate::workspaces::authored::own_edit(&copies.signing_device().cert),
        ),
    );
    copies
        .own_write(
            &workspace,
            (DESKTOP_APP, DESKTOP_DOCUMENT),
            None,
            &layout.save_after(&heads),
            false,
            false,
            Some(&sealed),
        )
        .await
        .expect("the change is written");
    waiting_front(&copies, &workspace, "Moved").await;

    // The service after a restart reads the copy back.
    drop(copies);
    member.shutdown().await;
    let member = test_host_holding(member_state.path(), &key, &cert, keys).await;
    let copies = member.workspaces().clone();
    copies.set_member_device(Arc::new(MemberDevice {
        cert,
        signer: Arc::new(key),
    }));
    waiting_front(&copies, &workspace, "Moved").await;
    drop(copies);
    member.shutdown().await;
}

/// Copy the tree at `from` to `to`, as a machine's disk is copied while it is off.
fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// A machine copied from the disk of the device serving a workspace keeps the workspace's files
/// and comes up as a device of its own. It keeps answering the workspace from its copy while its
/// source serves it, and takes the workspace in as a member copy of the source once the source
/// admits its device, so what it reads and writes reaches the workspace.
#[tokio::test]
async fn a_copied_disk_holds_its_workspace_as_a_member_copy_once_the_source_admits_it() {
    let source_state = tempfile::tempdir().unwrap();
    let copy_state = tempfile::tempdir().unwrap();
    let (serving_key, serving_cert) = governor();
    let source = test_host(source_state.path(), &serving_key, &serving_cert).await;
    let created = source
        .workspaces()
        .create("Shared")
        .await
        .expect("a workspace is created");
    let branch = device_cert(
        &SigningKey::from_bytes(&[1; 32]),
        "laptop branch",
        DeviceCapabilities::member(),
    );
    let issued = source
        .workspaces()
        .pre_admit(
            &serving_cert,
            PreAdmitRequest {
                request_id: [31; 32],
                grant: signed_pre_admission(&created.workspace, &[31; 8]),
                recipient_user_public_key: Some(branch.1.user_public_key),
                inviter_name: None,
            },
        )
        .await
        .expect("the source issues an invitation");
    let invitation = tilo_peer_state::join::encode_invitation_text(&issued.signed_invitation);
    copy_dir(source_state.path(), copy_state.path());

    // The copy comes up as a device of its own, as a branched machine does.
    let copy = test_host(copy_state.path(), &branch.0, &branch.1).await;
    copy.workspaces().set_member_device(Arc::new(MemberDevice {
        cert: branch.1,
        signer: Arc::new(branch.0),
    }));
    assert!(
        copy.workspaces().state(&created.workspace).await.is_some(),
        "the copy answers the workspace from its copy while its source serves it"
    );
    assert!(
        !copy
            .workspaces()
            .is_member(&created.workspace)
            .await
            .unwrap(),
        "and holds it as no member copy until its source admits its device"
    );

    copy.workspaces()
        .join_member(&invitation)
        .await
        .expect("the source takes the copy's device in");
    assert!(
        copy.workspaces()
            .is_member(&created.workspace)
            .await
            .unwrap(),
        "the copy holds the workspace as a member copy of its source from then on"
    );
    let state = copy
        .workspaces()
        .state(&created.workspace)
        .await
        .expect("the member copy is held");
    assert_eq!(
        state.governance().workspace_name().as_deref(),
        Some("Shared")
    );
}

/// A shared workspace this device joined, and the person's shell on a dispatch over it.
#[cfg(feature = "apps")]
async fn shell_on_a_joined_workspace(
    states: (&std::path::Path, &std::path::Path, &std::path::Path),
    seed: u8,
) -> (
    TestHost,
    TestHost,
    WorkspaceSummary,
    DeviceCert,
    runtime_collab::ShellClient,
    crate::apps::collab_broker::BrokerHandle,
) {
    use runtime_collab::wire::shell::SHELL_MAGIC;
    use std::os::unix::fs::PermissionsExt as _;
    use tokio::io::AsyncWriteExt as _;
    let (serving_state, member_state, dir) = states;
    let (serving, device, created, invitation) = shared(serving_state, "Shared", seed).await;
    let member_cert = device.1.clone();
    let member = member_service(member_state, device).await;
    member.workspaces().join_member(&invitation).await.unwrap();

    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = dir.join("collab.sock");
    let links = Arc::new(crate::apps::shell::ShellLinks::default());
    let dispatch = crate::apps::collab_broker::spawn_with_services(
        socket.clone(),
        None,
        crate::apps::collab_outbound::PeerPublisher::disabled(),
        None,
        crate::apps::services::ProcessServices {
            shell: Some(Arc::new(crate::apps::shell::ShellServices {
                processes: None,
                links: Arc::clone(&links),
                socket: socket.clone(),
                documents: Arc::new(crate::apps::service_reads::ServiceDocuments::of(
                    member.workspaces().clone(),
                )),
                writes: Arc::new(crate::apps::service_writes::ServiceWrites::of(
                    member.workspaces().clone(),
                )),
                document_writes: Arc::new({
                    let copies = member.workspaces().clone();
                    move || copies.document_writes()
                }),
                actions: Arc::new(NoActions),
                standing: Arc::new(NoStanding),
                published: None,
                workspaces: member.workspaces().clone(),
                identity: None,
                api: None,
                live: None,
                speaking: None,
            })),
            ..crate::apps::services::ProcessServices::default()
        },
        Arc::default(),
        Arc::new(crate::apps::collab_broker::FanoutState::new()),
    )
    .unwrap();
    let secret = links.make(crate::apps::shell::ShellCaller {
        person: member_cert.user_public_key,
    });
    let mut link = tokio::net::UnixStream::connect(&socket).await.unwrap();
    let mut opening = SHELL_MAGIC.to_vec();
    opening.extend_from_slice(&secret);
    link.write_all(&opening).await.unwrap();
    let (shell, _) = runtime_collab::ShellClient::open(link);
    (serving, member, created, member_cert, shell, dispatch)
}

/// The person's shell records the receipt of an effect an action it ran had: the device keeps it
/// in its own slice of the member copy, as it keeps its own actions' receipts.
#[cfg(feature = "apps")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_receipt_the_shell_sends_is_kept_in_the_joined_workspace() {
    use runtime_collab::wire::shell::{ShellCall, ShellReply};
    let (serving_state, member_state, dir) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    let (_serving, member, created, member_cert, shell, _dispatch) =
        shell_on_a_joined_workspace((serving_state.path(), member_state.path(), dir.path()), 13)
            .await;
    let ShellReply::Is(keeps) = shell
        .call(ShellCall::KeepsReceipts {
            workspace: created.workspace.clone(),
        })
        .await
        .unwrap()
    else {
        panic!("the daemon says whether it keeps receipts there");
    };
    assert!(keeps, "a member copy keeps this device's receipts");

    let invocation = uuid::Uuid::new_v4();
    let mut effect = tilo_desktop_model::WorkspaceEffectReceipt::redacted(
        tilo_actions::InvocationId(invocation),
        &tilo_actions::MemberId::person(hex::encode(member_cert.user_public_key)).into(),
        tilo_actions::ActionContext::for_resource("layout", "desktop", "Layout"),
        tilo_actions::EffectReceiptStatus::Succeeded,
        tilo_clock::unix_now_i64(),
    );
    effect.completed_at = 100;
    let reply = shell
        .call(ShellCall::Receipt {
            workspace: created.workspace.clone(),
            redacted: serde_json::to_vec(&effect).unwrap(),
        })
        .await
        .unwrap();
    assert!(
        matches!(reply, ShellReply::Receipted(Ok(()))),
        "the daemon records the receipt: {reply:?}"
    );
    let state = member.workspaces().state(&created.workspace).await.unwrap();
    let kept = tilo_desktop_model::projection::workspace_effect_receipts_of(
        state.admitted_workspace().expect("the copy's governance"),
    );
    assert!(
        kept.iter().any(|kept| kept.invocation.0 == invocation),
        "the member copy keeps the shell's receipt"
    );
}

/// What an agent the person's shell admitted reads is decided at the record, for its certificate.
#[cfg(feature = "apps")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_daemon_decides_a_read_for_the_certificate_the_shell_names() {
    use runtime_collab::wire::shell::{ShellCall, ShellReply};
    let (serving_state, member_state, dir) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    let (_serving, _member, created, member_cert, shell, _dispatch) =
        shell_on_a_joined_workspace((serving_state.path(), member_state.path(), dir.path()), 41)
            .await;
    let authorizes = |certificate: &DeviceCert| ShellCall::Authorizes {
        workspace: created.workspace.clone(),
        app: "so.tilo.notes".to_owned(),
        resource: "n1".to_owned(),
        certificate: serde_json::to_string(certificate).unwrap(),
    };
    let reply = shell.call(authorizes(&member_cert)).await.unwrap();
    assert!(matches!(reply, ShellReply::Authorized(Ok(()))), "{reply:?}");
    let reply = shell.call(authorizes(&laptop(99).1)).await.unwrap();
    assert!(
        matches!(&reply, ShellReply::Authorized(Err(why)) if why.contains("certifies")),
        "a device the workspace does not certify reads nothing: {reply:?}"
    );
}

#[cfg(feature = "apps")]
struct NoActions;

#[cfg(feature = "apps")]
impl crate::apps::shell::ShellActions for NoActions {
    fn dispatch(
        &self,
        _: &crate::apps::shell::ShellCaller,
        _: Option<String>,
        _: Option<runtime_collab::wire::shell::ShellPress>,
    ) -> crate::apps::shell::ShellDispatch {
        Box::pin(std::future::ready(Err("no actions here".to_owned())))
    }
}

#[cfg(feature = "apps")]
struct NoStanding;

#[cfg(feature = "apps")]
impl crate::apps::services::PublishedResources for NoStanding {
    fn snapshot(
        &self,
        _: &crate::apps::services::Caller,
        _: &str,
    ) -> tokio::sync::oneshot::Receiver<Result<crate::apps::services::Reading, String>> {
        tokio::sync::oneshot::channel().1
    }
    fn watch(
        &self,
        _: &crate::apps::services::Caller,
        _: &str,
        _: tokio::sync::mpsc::UnboundedSender<runtime_collab::wire::Event>,
    ) -> Result<crate::apps::services::ResourceWatch, String> {
        Err("nothing stands here".to_owned())
    }
    fn unwatch(&self, _: crate::apps::services::ResourceWatch) {}
    fn invoke(
        &self,
        _: &crate::apps::services::Caller,
        _: &str,
        _: &str,
        _: Vec<u8>,
    ) -> tokio::sync::oneshot::Receiver<runtime_collab::wire::Receipt> {
        tokio::sync::oneshot::channel().1
    }
}

/// A computer whose person's workspace another device serves (their server, say) holds a member
/// copy of it: the agent a program of theirs connects as is listed in the workspace's record of
/// its agents all the same, written on that copy and taken by the serving device: the record that
/// admits the agent's session names it on both. Another person's agent is never listed there.
#[cfg(feature = "agents")]
#[tokio::test]
async fn a_member_copy_lists_its_persons_computer_agent() {
    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let (serving, device, created, invitation) = shared(serving_state.path(), "Theirs", 23).await;
    let (person, computer) = (device.1.user_public_key, device.1.device_public_key);
    let member = member_service(member_state.path(), device).await;
    let workspace = member
        .workspaces()
        .join_member(&invitation)
        .await
        .expect("the invitation is redeemed");
    assert_eq!(workspace.to_string(), created.workspace);
    assert!(
        member.workspaces().governed_copy(workspace).await.is_none(),
        "the computer holds the workspace as a member copy, governed elsewhere"
    );

    let name = "mcp";
    let agent_id = tilo_actions::remote::device_agent_id(&person, name);
    let agent = tilo_actions::remote::Caller::delegated_agent(agent_id.clone(), person, [77; 32])
        .in_workspace(created.workspace.clone())
        .named(name);
    let as_person = tilo_actions::remote::Caller::authenticated(person, computer)
        .in_workspace(created.workspace.clone());
    crate::agents::workspace_agent(member.workspaces(), workspace, &agent, &as_person).await;
    assert_eq!(
        member
            .workspaces()
            .agent_name(&created.workspace, &agent_id)
            .await
            .as_deref(),
        Some(name),
        "the member copy lists the computer's agent"
    );
    let serving = serving.workspaces().clone();
    let (listed, agent_id) = (created.workspace.clone(), agent_id.clone());
    tilo_test_env::within("the serving device lists the agent", async move {
        while serving.agent_name(&listed, &agent_id).await.is_none() {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await;

    // Another person's agent is never listed through this person's copy.
    let stranger = tilo_actions::remote::device_agent_id(&[99; 32], name);
    let theirs =
        tilo_actions::remote::Caller::delegated_agent(stranger.clone(), [99; 32], [78; 32])
            .in_workspace(created.workspace.clone())
            .named(name);
    crate::agents::workspace_agent(member.workspaces(), workspace, &theirs, &as_person).await;
    assert!(
        member
            .workspaces()
            .agent_name(&created.workspace, &stranger)
            .await
            .is_none(),
        "another person's agent is not listed"
    );
}
