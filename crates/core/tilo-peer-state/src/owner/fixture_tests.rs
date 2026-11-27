use crate::NeighbourMsg;
use crate::join::{self, JoinTransportRequest, JoinTransportResponse};
use crate::owner::{DeviceWorkspaces, VerifiedNativeEnrollmentIntent, open_file, testing};
use collab_types::CommitNamed as _;
use ed25519_dalek::SigningKey;
use std::time::SystemTime;
use tilo_identity::policy_change::epoch;
use tilo_user_identity::{DeviceCert, RevocationList};

pub(crate) struct Fixture {
    pub(crate) directory: tempfile::TempDir,
    pub(crate) owner: DeviceWorkspaces,
    pub(crate) key: SigningKey,
    pub(crate) certificate: DeviceCert,
    pub(crate) now: SystemTime,
    pub(crate) workspace: testing::workspace::TestWorkspace,
}

/// A device of a second person, enrolled into a workspace another person hosts:
/// a key holder that neither administers nor hosts the workspace.
pub(crate) fn member_device_fixture() -> Fixture {
    fixture_with(
        &SigningKey::from_bytes(&[96; 32]),
        SigningKey::from_bytes(&[92; 32]),
        SigningKey::from_bytes(&[94; 32]),
        join::Role::Moderator,
        true,
    )
}

/// A device of `root` at `key`, enrolled into the workspace `owner_copy_key`'s
/// device of the workspace owner's root hosts.
fn fixture_with(
    root: &SigningKey,
    key: SigningKey,
    owner_copy_key: SigningKey,
    role: join::Role,
    administers: bool,
) -> Fixture {
    let owner_root = SigningKey::from_bytes(&[91; 32]);
    let directory = tempfile::tempdir().unwrap();
    let owner = open_file(&directory.path().join("owner.json")).unwrap();
    let certificate = testing::certificate(root, &key, "Host");
    let mut owner_copy = testing::workspace::TestWorkspace::empty_interactive(
        testing::certificate(&owner_root, &owner_copy_key, "Host"),
        owner_copy_key,
        testing::WORKSPACE,
    )
    .unwrap();
    let endpoint = iroh_base::EndpointAddr::new(
        iroh_base::EndpointId::from_bytes(
            &SigningKey::from_bytes(&[93; 32]).verifying_key().to_bytes(),
        )
        .unwrap(),
    );
    if administers {
        owner_copy
            .allow_governance(
                &hex::encode(certificate.user_public_key),
                tilo_action_policy::roles::governance::GRANT,
            )
            .unwrap();
    }
    let hint = owner_copy.hint(endpoint);
    let now = SystemTime::now();
    // The owner side runs on `now` throughout; the host answers on the
    // same instant so invitations and challenges never lapse mid-test on a
    // slow machine.
    owner_copy.pin_clock(now);
    let now_ns = testing::ns(now).unwrap();
    let invitation = owner_copy.issue(
        vec![hint.clone()],
        role,
        Some(certificate.user_public_key),
        None,
        now_ns.checked_add(60_000_000_000).unwrap(),
    );
    let verified = join::decode_and_verify_invitation(
        &invitation,
        Some(certificate.user_public_key),
        &RevocationList::new(),
        now_ns,
    )
    .unwrap();
    let response = owner_copy.handle(
        certificate.device_public_key,
        JoinTransportRequest::Challenge(join::JoinChallengeRequest {
            signed_invitation: invitation.clone(),
            device_cert: certificate.clone(),
            request_id: [95; 32],
        }),
    );
    let JoinTransportResponse::Challenge(response) = response else {
        panic!("challenge refused")
    };
    let redemption = join::verify_connection_redemption(
        &invitation,
        &certificate,
        [95; 32],
        &RevocationList::new(),
        now_ns,
    )
    .unwrap();
    let admission = crate::own_admission::own_admission(
        testing::WORKSPACE,
        &response.governance,
        &redemption,
        None,
        &key,
        now,
    )
    .unwrap();
    let proof = join::sign_join_proof(response.challenge, &key).unwrap();
    let complete = join::JoinCompleteRequest { proof, admission };
    let intent = VerifiedNativeEnrollmentIntent::from_proof(
        verified,
        complete.clone(),
        certificate.clone(),
        hint,
        now,
    )
    .unwrap();
    let pending = owner.prepare_enrollment_into(&intent, None, now).unwrap();
    let response = owner_copy.handle(
        certificate.device_public_key,
        JoinTransportRequest::Complete(complete),
    );
    let JoinTransportResponse::Complete(bootstrap) = response else {
        panic!("completion refused")
    };
    assert!(bootstrap.records.is_empty());
    let enrollment = testing::finalize(intent, pending, bootstrap).unwrap();
    owner.install(&enrollment, now).unwrap();
    Fixture {
        directory,
        owner,
        key,
        certificate,
        now,
        workspace: owner_copy,
    }
}

/// Give the member a grant: a change of the workspace's governance this
/// device is allowed to make.
async fn grant(fixture: &Fixture, id: &str) {
    let command = crate::workspace_membership::WorkspaceMembershipCommand::Grant {
        grant: tilo_action_policy::Grant::new(
            id,
            tilo_action_policy::Grantee::person(hex::encode(fixture.certificate.user_public_key)),
            tilo_action_policy::TreePath::workspace(testing::WORKSPACE),
            tilo_action_types::KindSet::READ,
            tilo_action_policy::Answer::Allow,
            tilo_action_policy::Grantee::person(hex::encode(fixture.certificate.user_public_key)),
        ),
    };
    fixture
        .owner
        .commit_membership(
            crate::owner::WorkspaceId::parse(testing::WORKSPACE).unwrap(),
            &fixture.certificate,
            &command,
            &fixture.key,
            fixture.now,
        )
        .await
        .unwrap();
}

/// One write of this device's record, committed to its ownership.
fn publish_write(fixture: &Fixture, workspace: crate::owner::WorkspaceId, key: &str) {
    let envelope = signed_write(fixture, workspace, key);
    fixture
        .owner
        .admit_writes(
            workspace,
            vec![crate::owner::WorkspaceDocumentWrite {
                app: "so.tilo.chat".into(),
                document: String::new(),
                readers: None,
                envelope,
            }],
            fixture.now,
        )
        .unwrap();
}

/// The chat record as the owner holds it, loaded as its app holds it: under
/// one actor, as the app's store keeps it across writes and restarts.
fn app_document(fixture: &Fixture, workspace: crate::owner::WorkspaceId) -> automerge::AutoCommit {
    let held = fixture
        .owner
        .with_current(workspace, |view| {
            view.peer()
                .document("so.tilo.chat", "")
                .ok_or_else(|| "no chat record".to_owned())
        })
        .unwrap();
    automerge::AutoCommit::load(&held)
        .unwrap()
        .with_actor(automerge::ActorId::from(b"so.tilo.chat".as_slice()))
}

/// A write of `key` to the chat record as its app signs it, before the
/// owner's turn admits it.
fn signed_write(
    fixture: &Fixture,
    workspace: crate::owner::WorkspaceId,
    key: &str,
) -> collab_types::SignedOp {
    signed_write_to(
        fixture,
        workspace,
        &mut app_document(fixture, workspace),
        key,
    )
}

/// [`signed_write`] by an app that keeps `record` open between its writes:
/// the write is the change `key` makes to `record`, which holds it from then
/// on, as the owner does once it committed the write.
fn signed_write_to(
    fixture: &Fixture,
    workspace: crate::owner::WorkspaceId,
    record: &mut automerge::AutoCommit,
    key: &str,
) -> collab_types::SignedOp {
    use automerge::transaction::Transactable as _;
    let before = record.get_heads();
    record.put(automerge::ROOT, key, key).unwrap();
    record.commit_edit("fixture", key, &fixture.certificate.user_public_key);
    let changes = record.save_after(&before);
    let message = fixture
        .owner
        .with_current(workspace, |view| {
            view.peer()
                .prepare_signed_mutation(
                    "so.tilo.chat",
                    "",
                    crate::DocumentMutation::Incremental(changes),
                    &fixture.key,
                    fixture.now,
                )
                .map(|(message, _)| message)
        })
        .unwrap();
    let NeighbourMsg::AppDocument { envelope, .. } = message else {
        panic!("a signed write is a document write")
    };
    *envelope
}

/// A member of the workspace, not one of its owner's devices, restarts with
/// its retained frames and publishes them as it signed them: nothing it sends
/// waits for an owner's checkpoint. A peer handed the later frame first asks
/// for what that frame builds on, and converges once it arrives.
#[tokio::test]
async fn a_member_device_republishes_its_retained_frames_and_a_peer_converges() {
    let fixture = member_device_fixture();
    let workspace = crate::owner::WorkspaceId::parse(testing::WORKSPACE).unwrap();
    let path = fixture.directory.path().join("owner.json");
    grant(&fixture, "chat-read").await;
    testing::start_resource(
        &fixture.owner,
        workspace,
        "so.tilo.chat",
        "",
        &fixture.key,
        fixture.now,
    );
    // Two writes of this device, the second building on the first.
    publish_write(&fixture, workspace, "first");
    publish_write(&fixture, workspace, "second");
    let reopened = open_file(&path).unwrap();
    let (frames, others): (Vec<_>, Vec<_>) = reopened
        .frames(workspace)
        .into_iter()
        .map(|bytes| postcard::from_bytes::<neighbour_wire::NeighbourMsg>(&bytes).unwrap())
        .partition(|frame| {
            matches!(frame, NeighbourMsg::AppDocument { app_id, .. } if app_id == "so.tilo.chat")
        });
    assert_eq!(
        frames.len(),
        3,
        "the record's creation and two writes are frames a peer is sent"
    );
    assert_eq!(
        others.len(),
        1,
        "the grant the device committed goes out as a frame of its own"
    );
    // A peer that never saw the first write receives the second one first.
    let origin = |fixture: &Fixture| {
        crate::TransportOrigin::RetainedServerDelta(fixture.certificate.device_public_key)
    };
    let mut peer = fixture.workspace.peer().clone();
    let held = peer
        .receive_from_report(origin(&fixture), frames[2].clone(), fixture.now)
        .unwrap();
    assert_eq!(
        held.primary(),
        &crate::InputDisposition::RetryLater(crate::MissingPrerequisite::DocumentCheckpoint)
    );
    for frame in &frames[0..=1] {
        peer.receive_from_report(origin(&fixture), frame.clone(), fixture.now)
            .unwrap();
    }
    let landed = peer
        .receive_from_report(origin(&fixture), frames[2].clone(), fixture.now)
        .unwrap();
    assert!(
        matches!(landed.primary(), crate::InputDisposition::Duplicate(_)),
        "the held write landed once what it builds on arrived: {:?}",
        landed.primary()
    );
    let authored = reopened
        .with_current(workspace, |view| {
            Ok(view.peer().document("so.tilo.chat", ""))
        })
        .unwrap();
    assert_eq!(peer.document("so.tilo.chat", ""), authored);
}

/// A write to an app record changes nothing authorization decides on, so its
/// watchers are not told; a change of the workspace's grants is, at once.
///
/// Not announced is read as the process's announcement epoch holding still
/// ([`tilo_identity::policy_change`]), and every owner, device fleet and
/// registry in a process announces into that one epoch: this reads it where
/// no other test owns a workspace that moves it.
#[tokio::test]
async fn only_a_governance_change_is_announced_to_authorization() {
    if !tilo_test_env::in_own_process_here(
        module_path!(),
        "only_a_governance_change_is_announced_to_authorization",
    ) {
        return;
    }
    let fixture = member_device_fixture();
    let workspace = crate::owner::WorkspaceId::parse(testing::WORKSPACE).unwrap();
    let before = epoch();
    grant(&fixture, "chat-read").await;
    assert_ne!(epoch(), before, "a grant is announced");
    testing::start_resource(
        &fixture.owner,
        workspace,
        "so.tilo.chat",
        "",
        &fixture.key,
        fixture.now,
    );
    let before = epoch();
    publish_write(&fixture, workspace, "first");
    publish_write(&fixture, workspace, "second");
    assert_eq!(epoch(), before, "an app record's writes are not announced");
    grant(&fixture, "notes-read").await;
    assert_ne!(epoch(), before, "the next grant is announced");
}
