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
use super::super::fixtures::{
    TestHost, declare_naming_kind, device_cert, group_keys, hosting_device, invited, laptop,
    member_service, names_content, shared, signed_pre_admission, test_host, test_host_holding,
    write_store,
};
use super::MemberDevice;
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::DeviceCapabilities;

/// A device that joins names its person as they asked, among the workspace's people: the serving
/// device that let it in writes the name, its member list shows it, and the joiner's lists them.
#[tokio::test]
async fn a_device_that_joins_names_its_person_in_the_workspace() {
    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let (serving, device, created, invitation) = shared(serving_state.path(), "Named", 12).await;
    let person = hex::encode(device.1.user_public_key);
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
    let offer = super::super::OwnWorkspaceOffer::free("tilo.test");
    member.workspaces().offer_own_workspaces(offer).unwrap();
    let mine = member
        .workspaces()
        .members(&created.workspace)
        .await
        .unwrap();
    assert!(
        mine.iter()
            .any(|row| row["actor"]["member"]["person"] == person),
        "{mine:?}"
    );
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

/// A governance frame the device serving a workspace sends its member copy is that copy's
/// governance moving: its followers are told, so the reading every surface draws from — the
/// member list, whose each device is, what its calls allow — is read again rather than standing
/// as it did when the copy was taken in.
#[tokio::test]
async fn a_device_keeping_a_workspace_tells_its_followers_when_its_members_move() {
    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let (_serving, (key, cert), created, invitation) =
        shared(serving_state.path(), "Told later", 33).await;
    let member = member_service(member_state.path(), (key.clone(), cert.clone())).await;
    member
        .workspaces()
        .join_member(&invitation)
        .await
        .expect("the invitation is redeemed");
    let workspace = created.workspace.clone();
    let workspaces = member.workspaces().clone();
    let mut moved = workspaces.governance_changes();
    let frame = collab_types::MlsGovernanceFrame::signed(
        collab_types::WorkspaceId::parse(&workspace).expect("a workspace id"),
        [0; 32],
        1,
        collab_types::MlsGovernanceAction::Genesis,
        &key,
    )
    .expect("a governance frame is signed");
    let message = NeighbourMsg::MlsGovernance {
        workspace: workspace.clone(),
        frame: Box::new(frame),
        delivery: 0,
    };
    let body = postcard::to_allocvec(&message).expect("a governance frame encodes");
    workspaces
        .keep_member_write(
            &workspace,
            key.verifying_key().to_bytes(),
            &message,
            &body,
            false,
            false,
        )
        .await
        .expect("the member copy keeps the governance frame");
    // Its followers are told the workspace moved, so the reading is read again.
    let told = workspace.clone();
    tilo_test_env::within("the member's followers are told", async move {
        loop {
            match moved.recv().await {
                Ok(moved) if moved == told => return,
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    panic!("the member's workspaces stopped")
                }
            }
        }
    })
    .await;
    // What the reading is made of names this device in the call gate: the part every surface
    // draws a member's face from reads the copy's governance as it now stands.
    let state = workspaces
        .workspace(&workspace)
        .await
        .expect("the member holds its copy");
    let peer = state.lock().await.peer().clone();
    let (_, persons) = peer
        .governance()
        .call_gate(tilo_clock::wall())
        .expect("the copy reads its governance");
    assert!(
        persons.contains_key(&cert.device_public_key),
        "the call gate names the member copy's device: {persons:?}"
    );
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one member copy, the answer it gives its host and the push it keeps"
)]
async fn a_device_holding_a_workspace_answers_content_as_the_device_it_joined_as() {
    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let (serving, (key, cert), created, invitation) =
        shared(serving_state.path(), "Content", 21).await;
    // The workspace's records name the picture, as an app's typed write leaves it: what decides
    // who reads it by its address, on every copy of the workspace.
    let picture = b"a picture the member keeps".to_vec();
    let kind = declare_naming_kind(serving.workspaces(), "test.content");
    let writer = hex::encode(serving.workspaces().device_cert().user_public_key);
    write_store(
        serving.workspaces(),
        &created.workspace,
        "test.content",
        names_content(
            &kind,
            "picture",
            "blob",
            blake3::hash(&picture).to_hex().as_str(),
            collab_types::EVERY_MEMBER,
            &writer,
        ),
    )
    .await
    .expect("the record names the picture");

    // The member's own service as production opens a device's: it signs as the device, whose
    // key its endpoint is bound with and whose link its member copy answers on.
    let member = test_host_holding(member_state.path(), &key, &cert, group_keys()).await;
    // The member's service declares the app its workspace's records name content in, as running
    // the app does: a kind no app here declares names nothing anyone reads.
    declare_naming_kind(member.workspaces(), "test.content");
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
    let member_keeps = Kept(picture);
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
    // What the member pushes to the serving device names the same device, and goes until the
    // serving device says it keeps it.
    let (pushed, mut heard) = tokio::sync::mpsc::unbounded_channel();
    let keeper: Arc<dyn super::super::ContentKeeper> = Arc::new(Heard(pushed));
    serving.workspaces().keep_content(&keeper);
    copies.push_content(&workspace, "so.tilo.photos", &member_keeps.address());
    assert_eq!(
        copies.content_going(&workspace),
        1,
        "the picture is on the member alone until the serving device keeps it"
    );
    let (from, responder) =
        tilo_test_env::within("the member's push reaches the serving device", heard.recv())
            .await
            .expect("the keeper hears the push");
    assert_eq!(
        (from, responder),
        (
            cert.device_public_key,
            tilo_peer_state::peer_handle(&cert.device_public_key)
        ),
        "the push names the device key the member signs its link as, not its service's"
    );
    serving
        .workspaces()
        .tell_members(
            workspace_id,
            NeighbourMsg::DocumentContentKept {
                responder: tilo_peer_state::peer_handle(&serving_device),
                requester: responder,
                app_id: "so.tilo.photos".to_owned(),
                cluster_id: workspace.clone(),
                hash: member_keeps.address(),
            },
        )
        .await
        .expect("the serving device says it keeps the picture");
    assert_eq!(
        copies
            .content_delivered(&workspace, std::time::Duration::from_secs(10))
            .await,
        0,
        "the member's push goes no more once the serving device keeps it"
    );

    drop(copies);
    member.shutdown().await;
    serving.shutdown().await;
}

/// A keeper that tells each push it is handed: the device that sent it, and the
/// device its pieces say they come from.
struct Heard(tokio::sync::mpsc::UnboundedSender<([u8; 32], neighbour_wire::PeerId)>);

impl super::super::ContentKeeper for Heard {
    fn take(
        &self,
        _workspace: &str,
        from: [u8; 32],
        message: NeighbourMsg,
    ) -> Result<(), Box<NeighbourMsg>> {
        let NeighbourMsg::DocumentContentPieces { responder, .. } = &message else {
            return Err(Box::new(message));
        };
        self.0
            .send((from, *responder))
            .expect("the test hears every push");
        Ok(())
    }
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

/// A further device of a member, taken in while another member's copy is linked, reaches that
/// copy: its own row came to the serving device with its membership proof, over no link, and the
/// serving device carries it on to the copies linked before, whose key groups then take the
/// device in.
#[tokio::test]
async fn a_further_device_of_a_member_reaches_the_copies_linked_before_it() {
    let serving_state = tempfile::tempdir().unwrap();
    let first_state = tempfile::tempdir().unwrap();
    let second_state = tempfile::tempdir().unwrap();
    let (serving, first, created, invitation) =
        shared(serving_state.path(), "Two devices", 31).await;
    let workspace_id = WorkspaceId::parse(&created.workspace).expect("a workspace id");
    let first = member_service(first_state.path(), first).await;
    first
        .workspaces()
        .join_member(&invitation)
        .await
        .expect("the invitation is redeemed");
    first
        .workspaces()
        .wait_member_link(workspace_id)
        .await
        .expect("the first copy's link is up");
    let hint = first
        .workspaces()
        .member_hint(workspace_id)
        .await
        .expect("the copy names the device serving it");

    // The same person's desk, which no invitation brings: a device of someone the workspace has.
    let desk = device_cert(
        &SigningKey::from_bytes(&[31; 32]),
        "desk",
        DeviceCapabilities::member(),
    );
    let desk_device = desk.1.device_public_key;
    let desk = member_service(second_state.path(), desk).await;
    desk.workspaces()
        .hold_as_member(workspace_id, &hint)
        .await
        .expect("the person's further device is taken in");

    let copies = first.workspaces().clone();
    let workspace = created.workspace.clone();
    tilo_test_env::within(
        "the first copy lists the person's further device",
        async move {
            loop {
                let listed = copies.state(&workspace).await.is_some_and(|state| {
                    state
                        .governance()
                        .key_holders(web_time::SystemTime::now())
                        .is_ok_and(|holders| holders.contains(&desk_device))
                });
                if listed {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        },
    )
    .await;
    drop(serving);
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

/// The device serving `workspace` writes `(app, record)` holding `value` under `key`, in the
/// workspace's store, its kind declared first.
async fn serving_writes_record(
    serving: &TestHost,
    workspace: &str,
    (app, record): (&str, &str),
    (key, value): (&str, &str),
) {
    super::super::fixtures::write_record(serving.workspaces(), workspace, (app, record), |doc| {
        use automerge::transaction::Transactable as _;
        doc.put(automerge::ROOT, key, value)
            .map(drop)
            .map_err(|error| error.to_string())
    })
    .await
    .expect("the serving device writes its record");
}

/// The change setting `key` to `value` in `copies`' copy of `(app, record)` of `workspace`, as
/// this device makes it: its own write, as every change the device makes in its own name.
async fn record_change_of(
    copies: &super::Workspaces,
    workspace: &str,
    (app, record): (&str, &str),
    (key, value): (&str, &str),
) -> Vec<u8> {
    use automerge::transaction::Transactable as _;
    use collab_types::claim::CommitNamed as _;

    // The serving device's record reaches the copy by sync, after the workspace's history.
    let bytes = tilo_test_env::within("the copy holds the record", async {
        loop {
            if let Some(bytes) = copies
                .resource_document(workspace, app, record)
                .await
                .expect("the record reads")
            {
                break bytes;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    let person = copies.signing_device().cert.user_public_key;
    let mut opened = automerge::AutoCommit::load(&bytes).expect("the record opens");
    let heads = opened.get_heads();
    opened
        .put(automerge::ROOT, key, value)
        .expect("the record's field");
    // A change names who made it: the store refuses one that names nobody.
    opened.commit_edit(app, key, &person);
    opened.save_after(&heads)
}

/// The store's document holding the content of `app`'s record `record`, as `served` names it.
fn record_document(app: &str, record: &str) -> String {
    tilo_resource_store::content_id(
        collab_types::EVERY_MEMBER,
        &super::super::fixtures::record_kind(app),
        record,
    )
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

/// Wait until the serving device's copy of a record holds `expected` under `key`: the answer it
/// gives keeps the write, and its copy takes it in from the link's reader beside that answer.
async fn served_value_became(
    workspaces: &super::Workspaces,
    workspace: &str,
    (app, document): (&str, &str),
    key: &str,
    expected: &str,
) {
    tilo_test_env::within("the serving device's copy holds the write", async {
        loop {
            if served_value(workspaces, workspace, (app, document), key)
                .await
                .as_deref()
                == Some(expected)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
}

/// A member copy writes a record the serving device founds; `served` answers only once that
/// device holds the write, and reading its copy of the record shows the change there. The write
/// is sealed under a key the founding device does not hold yet: it is taken in once that arrives.
#[tokio::test]
async fn served_answers_once_the_serving_device_holds_the_write() {
    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let founding = Arc::new(super::super::fixtures::TestGroupKeys::default());
    let serving = test_host_holding(serving_state.path(), &key, &cert, Arc::clone(&founding)).await;
    let (serving, device, created, invitation) =
        invited(serving, &cert, "Answered", 41, laptop(41)).await;
    let workspace = created.workspace.clone();
    serving_writes_record(
        &serving,
        &workspace,
        ("test.app", "record"),
        ("value", "by-the-serving-device"),
    )
    .await;
    group_keys().hand_every_member_of(&founding, &workspace);
    let member = member_service(member_state.path(), device).await;
    let copies = member.workspaces().clone();
    let kind = super::super::fixtures::declare_record_kind(&copies, "test.app");
    let document = record_document("test.app", "record");
    let workspace_id = copies
        .join_member(&invitation)
        .await
        .expect("the invitation is redeemed");
    catching_up(&copies, workspace_id).await;
    founding.hold_nothing();
    copies
        .own_record_write(
            &workspace,
            ("test.app", Some(kind.as_str()), "record"),
            &record_change_of(
                &copies,
                &workspace,
                ("test.app", "record"),
                ("value", "written"),
            )
            .await,
        )
        .await
        .expect("the write is taken here");
    tilo_test_env::within("the serving device answers the write", async {
        copies
            .served(&workspace, &[("test.app", &document)])
            .expect("a member copy is answered by the device serving it")
            .await
            .expect("the serving device answers the write");
    })
    .await;
    let mut told = serving.workspaces().document_writes();
    founding.hand_every_member_of(&group_keys(), &workspace);
    served_value_became(
        serving.workspaces(),
        &workspace,
        ("test.app", "record"),
        "value",
        "written",
    )
    .await;
    // Whoever follows the record there is told of it, as of a write taken in as it came.
    tilo_test_env::within("the serving device tells its followers", async {
        while told.recv().await.expect("the service runs").app != "test.app" {}
    })
    .await;
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
    serving_writes_record(
        &serving,
        &workspace,
        ("test.app", "record"),
        ("value", "by-the-serving-device"),
    )
    .await;
    let member = member_service(member_state.path(), device).await;
    let copies = member.workspaces().clone();
    let kind = super::super::fixtures::declare_record_kind(&copies, "test.app");
    let document = record_document("test.app", "record");
    let workspace_id = copies
        .join_member(&invitation)
        .await
        .expect("the invitation is redeemed");
    catching_up(&copies, workspace_id).await;
    let change = record_change_of(
        &copies,
        &workspace,
        ("test.app", "record"),
        ("value", "written-while-away"),
    )
    .await;
    copies.set_member_link(workspace_id, false);

    let written = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        copies.own_record_write(
            &workspace,
            ("test.app", Some(kind.as_str()), "record"),
            &change,
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
                .served(&workspace, &[("test.app", &document)])
                .expect("the copy waits on its serving device")
                .await
                .expect("the serving device keeps the write");
        },
    )
    .await;
    served_value_became(
        serving.workspaces(),
        &workspace,
        ("test.app", "record"),
        "value",
        "written-while-away",
    )
    .await;
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

/// The device's own change, sent to the device serving the workspace, is still this copy's after
/// the device stops at once and starts again: its next change follows it in the device's history
/// rather than taking its place, so the serving device takes that one too. A copy that came back
/// without it wrote its next change under the same number, and the serving device refused every
/// change of this device from then on.
#[tokio::test]
async fn a_change_sent_just_before_a_restart_is_followed_by_the_next() {
    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let (serving, device, created, invitation) =
        shared(serving_state.path(), "Restarted", 51).await;
    let workspace = created.workspace.clone();
    let held = device.clone();
    let member = member_service(member_state.path(), device).await;
    let copies = member.workspaces().clone();
    let workspace_id = copies
        .join_member(&invitation)
        .await
        .expect("the invitation is redeemed");
    catching_up(&copies, workspace_id).await;
    let save = |id: &str| {
        let mut shortcut = crate::workspaces::fixtures::on_issue_change();
        shortcut.id = id.into();
        tilo_automation::ShortcutChange::Save(Box::new(shortcut))
    };
    let reaches = |id: &'static str| {
        let serving = &serving;
        let workspace = &workspace;
        async move {
            tilo_test_env::within("the serving device takes the device's change", async {
                while !serving_shortcuts(serving, workspace)
                    .await
                    .iter()
                    .any(|held| held == id)
                {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            })
            .await;
        }
    };
    copies
        .change_own_shortcuts(&workspace, save("tidy"))
        .await
        .expect("the person saves a shortcut");
    reaches("tidy").await;
    // The device stops the moment the serving device holds the change.
    drop(copies);
    member.shutdown().await;

    let member = member_service(member_state.path(), held).await;
    let copies = member.workspaces().clone();
    copies
        .change_own_shortcuts(&workspace, save("sweep"))
        .await
        .expect("the person saves another shortcut");
    reaches("sweep").await;
    drop(copies);
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
    serving_writes_record(
        &serving,
        &workspace,
        ("test.app", "record"),
        ("value", "by-the-serving-device"),
    )
    .await;
    let member = member_service(member_state.path(), device).await;
    let copies = member.workspaces().clone();
    let kind = super::super::fixtures::declare_record_kind(&copies, "test.app");
    let document = record_document("test.app", "record");

    let early = copies
        .own_record_write(
            &workspace,
            ("test.app", Some(kind.as_str()), "record"),
            &snapshot("value", "written-while-joining"),
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
    let change = record_change_of(
        &copies,
        &workspace,
        ("test.app", "record"),
        ("value", "written-while-joining"),
    )
    .await;
    copies
        .own_record_write(
            &workspace,
            ("test.app", Some(kind.as_str()), "record"),
            &change,
        )
        .await
        .expect("the write is kept once the workspace is held here");
    tilo_test_env::within("the write reaches the serving device", async {
        copies
            .served(&workspace, &[("test.app", &document)])
            .expect("the copy waits on its serving device")
            .await
            .expect("the serving device keeps the write");
    })
    .await;
    served_value_became(
        serving.workspaces(),
        &workspace,
        ("test.app", "record"),
        "value",
        "written-while-joining",
    )
    .await;
    drop(copies);
    member.shutdown().await;
    serving.shutdown().await;
}

/// A member copy says when the history of the device serving it is in hand, and a workspace this
/// device hosts has no such link to wait on.
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
    let hosted = WorkspaceId::parse(&created.workspace).expect("the serving device's workspace id");
    assert!(
        serving.workspaces().member_caught_up(hosted).is_none(),
        "a workspace this device hosts answers through no serving device"
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
        Some(crate::content::fixtures::hosts(false, false).a),
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

/// A person's own workspace is founded on their computer at their first call, after the files
/// daemon already opened its folder: the folder waits for it, and stops waiting once this
/// device founded and hosts it, with no member copy to catch up with.
#[cfg(feature = "files")]
#[tokio::test]
async fn a_folder_opened_before_its_workspace_is_founded_here_stops_waiting_once_it_is() {
    let state = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let host = test_host_holding(state.path(), &key, &cert, group_keys()).await;
    let copies = host.workspaces().clone();
    let own = WorkspaceId::parse(&crate::workspaces::person_workspace::workspace_id(&copies))
        .expect("the person's own workspace id");
    let kept = tempfile::tempdir().unwrap();
    let store = Arc::new(
        tilo_fs::ContentStore::open(kept.path().join("content"))
            .await
            .expect("a content store for the folder"),
    );
    let folder = crate::files::start(
        &copies,
        own,
        &kept.path().join("tree"),
        (cert.device_public_key, cert.user_public_key),
        Some(store),
        Some(crate::content::fixtures::hosts(false, false).a),
    )
    .await
    .expect("the folder opens");
    tilo_test_env::within(
        "the folder waits for the person's workspace, not founded yet",
        async {
            while !folder.status().waiting_for_workspace {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        },
    )
    .await;
    crate::workspaces::person_workspace::found(&copies)
        .await
        .expect("this device founds its person's workspace");
    tilo_test_env::within(
        "the folder stops waiting once this device hosts the workspace",
        async {
            while folder.status().waiting_for_workspace {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        },
    )
    .await;
    drop(folder);
    drop(copies);
    host.shutdown().await;
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
    // The copy took the serving device's write into its own record, which reads it back.
    assert_eq!(
        served_value(&copies, &workspace, ("test.app", "record"), "value")
            .await
            .as_deref(),
        Some("from-the-serving-device"),
        "the copy's replica holds the serving device's write"
    );
    // The serving device's own copy is the workspace's first member, and the copy's link to it
    // is up.
    assert!(
        serving.workspaces().state(&workspace).await.is_some(),
        "the serving device holds the workspace"
    );
    assert!(
        serving
            .host()
            .is_subscribed(
                workspace_id,
                &member.workspaces().device_cert().device_public_key
            )
            .await,
        "the copy's link to the serving device is up"
    );
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
    let (serving_key, serving_cert) = hosting_device();
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
    let (serving_key, serving_cert) = hosting_device();
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
            crate::workspaces::authored::own_edit(&copies.signing_device().cert, &layout),
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
    let (serving_key, serving_cert) = hosting_device();
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

/// The apps whose records a device's own roles write, each running from a package that declares
/// only a resource of its own, as on a device: the records are the system's, judged by the types
/// the system declares for them, which no package declares.
#[cfg(feature = "files")]
const DEVICE_WRITERS: [(&str, &str); 3] = [
    (fs_protocol::tree::FILES_APP, "files.folder-view"),
    (crate::workspaces::vault::APP, "passwords.item"),
    (tilo_desktop_model::ownership::DESKTOP_APP, "desktop.item"),
];

/// Decide every change against the surfaces its app runs with, as a daemon does: the whole
/// process's.
#[cfg(feature = "files")]
fn decide_as_a_daemon() {
    let known = collab_types::app_surface::Surfaces::process();
    crate::workspaces::person_workspace::declare_kinds(known).unwrap();
    for (app, kind) in DEVICE_WRITERS {
        let surface = format!(
            r#"{{"format":2,"resources":[{{"fields":[{{"path":"title","readonly":true,"type":"text"}}],"kind":"{kind}","label":"Item","verbs":[{{"id":"type","kind":"undoable"}}],"version":1}}]}}"#
        );
        known.register_package(app, surface.as_bytes());
    }
    tilo_peer_state::decide_with_surfaces(
        collab_types::Surfaces::from_source(Arc::new(collab_types::surface::ContentSurfaces::new(
            |record, hash| {
                let running = collab_types::app_surface::hash_of(record.app)?;
                (hex::encode(running) == hash)
                    .then(|| collab_types::app_surface::bytes_of(record.app).map(<[u8]>::to_vec))
                    .flatten()
            },
        ))),
        collab_types::app_surface::hash_of,
    );
}

/// The device's own writes of `workspace`'s records, each as the role that keeps it writes it:
/// the Files tree's record as the mount writes it (its derived founding and the mount's edit,
/// then the whole record again with the next edit), the shared desktop as the layout writes it
/// (the whole record each time, a view asked for in it), and the vault as Passwords keeps an
/// entry and then another. None of the tree's or the layout's changes is a store's stamp.
#[cfg(feature = "files")]
async fn write_as_the_devices_roles(owners: &super::super::Workspaces, workspace: &str) {
    use automerge::transaction::CommitOptions;
    use tilo_automerge::ApplyIncoming as _;
    use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_DOCUMENT, DESKTOP_KIND};

    let person = hex::encode(owners.signing_device().cert.user_public_key);
    let member = tilo_actions::MemberId::person(person.clone());
    let (app, kind) = (fs_protocol::tree::FILES_APP, fs_protocol::tree::TREE_KIND);
    let record = fs_protocol::tree::chunk_document_id(0);
    let mut tree = automerge::AutoCommit::new();
    tree.apply_incoming([fs_protocol::tree::founding(workspace, &record).unwrap()])
        .unwrap();
    for (chunk, kind) in [(1, Some(kind)), (2, None)] {
        fs_protocol::tree::list_chunk(&mut tree, chunk).unwrap();
        tree.commit_with(CommitOptions::default().with_message(
            tilo_actions::ActionContext::whole_edit_by(
                &tilo_actions::Acting::member(member.clone()),
                &tree,
            ),
        ));
        owners
            .own_record_write(workspace, (app, kind, &record), &tree.save_after(&[]))
            .await
            .expect("the workspace takes the tree's write");
    }

    let mut layout = tilo_desktop_model::record::founded(workspace).unwrap();
    for (asked, kind) in [("files", Some(DESKTOP_KIND)), ("notes", None)] {
        let view = tilo_desktop_model::asked::AskedView {
            app_id: format!("so.tilo.{asked}"),
            title: asked.to_owned(),
            document: String::new(),
            person: person.clone(),
            opened_by: None,
        };
        tilo_desktop_model::asked::ask(&mut layout, &format!("{person}:{asked}:"), &view).unwrap();
        layout.commit_with(CommitOptions::default().with_message(
            tilo_desktop_model::shared_resource::housekeeping_message(&person, &layout),
        ));
        owners
            .own_record_write(
                workspace,
                (DESKTOP_APP, kind, DESKTOP_DOCUMENT),
                &layout.save(),
            )
            .await
            .expect("the workspace takes the layout's write");
    }

    for (title, password) in [("Example", "correct horse"), ("Other", "battery staple")] {
        let vault = owners.open_vault(workspace).await.unwrap();
        owners
            .write_vault(
                &vault,
                vec![accounts::vault_items::VaultChange::Keep(
                    accounts::SecretItem::login(title, "ada", password),
                )],
                None,
                None,
            )
            .await
            .expect("the workspace takes the vault's write");
    }
}

/// The device serving a workspace that runs Files, Passwords and the desktop from their packages
/// writes their records as its own roles do, sealed: the Files tree, the shared desktop and the
/// vault. The workspace takes every write, and a member device opens them and reads what they
/// hold.
#[cfg(feature = "files")]
#[tokio::test]
async fn the_records_a_devices_roles_write_are_taken_and_opened_by_a_member() {
    if !tilo_test_env::in_own_process_here(
        module_path!(),
        "the_records_a_devices_roles_write_are_taken_and_opened_by_a_member",
    ) {
        return;
    }
    decide_as_a_daemon();
    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let (serving, device, created, invitation) = shared(serving_state.path(), "Roles", 49).await;
    let workspace = created.workspace.clone();
    for (app, _) in DEVICE_WRITERS {
        super::super::fixtures::install_app(serving.workspaces(), &workspace, app)
            .await
            .expect("the workspace runs the app");
    }
    write_as_the_devices_roles(serving.workspaces(), &workspace).await;

    let member = member_service(member_state.path(), device).await;
    let copies = member.workspaces().clone();
    let workspace_id = copies
        .join_member(&invitation)
        .await
        .expect("the invitation is redeemed");
    catching_up(&copies, workspace_id).await;
    let read = |app: &'static str, record: String| {
        let (copies, workspace) = (copies.clone(), workspace.clone());
        async move {
            copies
                .resource_document(&workspace, app, &record)
                .await
                .expect("the record reads")
                .map(|bytes| automerge::AutoCommit::load(&bytes).expect("the record opens"))
        }
    };
    tilo_test_env::within("the member opens every write", async {
        loop {
            let tree = read(
                fs_protocol::tree::FILES_APP,
                fs_protocol::tree::chunk_document_id(0),
            )
            .await;
            let layout = read(
                tilo_desktop_model::ownership::DESKTOP_APP,
                tilo_desktop_model::ownership::DESKTOP_DOCUMENT.to_owned(),
            )
            .await;
            let vault = copies.open_vault(&workspace).await;
            if tree.is_some_and(|tree| fs_protocol::tree::listed_chunks(&tree) == [1, 2])
                && layout.is_some_and(|layout| tilo_desktop_model::asked::asked(&layout).len() == 2)
                && vault.is_ok_and(|vault| vault.entries().len() == 2)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    drop(copies);
    member.shutdown().await;
    serving.shutdown().await;
}

/// Waits until `workspaces`' copy of `workspace` admits by the apps its own store holds, which
/// run `version` of the notes app (`None`: none) and say it ran.
async fn reads_its_stores_apps(
    workspaces: &super::Workspaces,
    workspace: &str,
    version: Option<&str>,
) {
    tilo_test_env::within("the copy admits by the apps its store holds", async {
        loop {
            let (read, held) = {
                let state = workspaces.workspace(workspace).await.unwrap();
                let hosted = state.lock().await;
                (hosted.peer().apps(), hosted.apps_in_store())
            };
            let running = read
                .running()
                .get(NOTES_APP)
                .map(|installed| installed.release.version.clone());
            // The app ran, installed now or not: a change of its records is set aside, never
            // refused.
            if held.as_ref() == Some(&*read)
                && running.as_deref() == version
                && !read.ran(NOTES_APP).is_empty()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
}

const NOTES_APP: &str = "so.tilo.notes";

/// The apps a workspace runs are records of its store. Installing, uninstalling and undoing the
/// uninstall each leave both devices, running in this one process, admitting by the apps their
/// own store holds, and so does the member's copy opening again.
#[tokio::test]
async fn every_copy_admits_by_the_apps_its_own_store_holds() {
    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let (serving, device, created, invitation) = shared(serving_state.path(), "Apps", 61).await;
    let workspace = created.workspace.clone();
    let workspace_id = WorkspaceId::parse(&workspace).expect("a workspace id");
    let member = member_service(member_state.path(), device.clone()).await;
    let copies = member.workspaces().clone();
    copies
        .join_member(&invitation)
        .await
        .expect("the invitation is redeemed");
    catching_up(&copies, workspace_id).await;
    let host = serving.workspaces();
    let installed = tilo_peer_state::InstalledApp {
        release: tilo_peer_state::workspace_membership::AppRelease {
            app: NOTES_APP.into(),
            version: "1.0.0".into(),
            surface: hex::encode([5; 32]),
            package: "00".repeat(32),
            publisher: "00".repeat(32),
            file: None,
            dev: true,
            pinned: false,
            payload: tilo_peer_state::workspace_membership::AppPayload::Tapp,
        },
        installed_by: hex::encode(host.signing_device().cert.user_public_key),
        installed_at_ns: 1,
    };

    host.write_app_records(&workspace, vec![(NOTES_APP.into(), Some(installed))])
        .await
        .expect("the owner installs the app");
    reads_its_stores_apps(host, &workspace, Some("1.0.0")).await;
    reads_its_stores_apps(&copies, &workspace, Some("1.0.0")).await;

    let uninstall = host
        .write_app_records(&workspace, vec![(NOTES_APP.into(), None)])
        .await
        .expect("the owner uninstalls it");
    reads_its_stores_apps(host, &workspace, None).await;
    reads_its_stores_apps(&copies, &workspace, None).await;

    host.undo_apps_change(&workspace, &uninstall)
        .await
        .expect("the uninstall is taken back");
    reads_its_stores_apps(host, &workspace, Some("1.0.0")).await;
    reads_its_stores_apps(&copies, &workspace, Some("1.0.0")).await;

    drop(copies);
    member.shutdown().await;
    let reopened = member_service(member_state.path(), device).await;
    reads_its_stores_apps(reopened.workspaces(), &workspace, Some("1.0.0")).await;
    reopened.shutdown().await;
    serving.shutdown().await;
}
