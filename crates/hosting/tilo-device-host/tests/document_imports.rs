//! Documents proposed into a governed room, through the actual owner: a
//! proposal creates nothing, only the owner's acceptance does, and only
//! with the rights an empty creation needs.
tilo_automerge_isolate::test_worker!();

use collab_types::caps::{CapOverride, Effect, Scope, Subject, Verb};
use ed25519_dalek::SigningKey;
use tilo_peer_state::desktop_command::DesktopCommandError;
use tilo_peer_state::document_import::{
    self as import, DecideArgument, DocumentImportDecisionV1, DocumentImportProposalV1,
    DocumentImportTerms, ImportAction, ImportState, ProposeArgument,
};
use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};
use tilo_peer_state::{NeighbourMsg, TransportOrigin};
use tilo_user_identity::DeviceCert;
use tilo_workspace_host::SessionOwner;
use ui::automerge::{ActorId, AutoCommit, ROOT, ReadDoc, ScalarValue, transaction::Transactable};

const APP: &str = "so.tilo.notes";

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// A member device admitted through the room's own invitation.
fn member(owner: &mut SessionOwner, seed: u8, label: &str) -> (DeviceCert, SigningKey) {
    use tilo_peer_state::join::{
        JoinChallengeRequest, JoinCompleteRequest, MemberRole, sign_join_proof,
    };
    use tilo_user_identity::{DeviceCapabilities, sign_device_cert};
    use tilo_workspace_host::collaboration::{CollaborationService, IssueInvitationRequest};
    let root = SigningKey::from_bytes(&[seed; 32]);
    let device = SigningKey::from_bytes(&[seed.wrapping_add(1); 32]);
    let now = tilo_delivery_protocol::clock::now_ns();
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: now - 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &root).unwrap();
    let service = CollaborationService::new(vec![]);
    let issued = service
        .issue(
            owner,
            owner.peer().authority().device_public_key,
            IssueInvitationRequest {
                request_id: [seed; 32],
                role: MemberRole::Editor,
                recipient_user_public_key: Some(cert.user_public_key),
                invitee_name: Some(label.into()),
                expires_at_ns: now + 120_000_000_000,
            },
            now,
        )
        .unwrap();
    let challenge = service
        .challenge(
            owner,
            cert.device_public_key,
            JoinChallengeRequest {
                signed_invitation: issued.signed_invitation,
                device_cert: cert.clone(),
                request_id: [seed.wrapping_add(1); 32],
            },
            now + 1,
        )
        .unwrap()
        .challenge;
    let proof = sign_join_proof(challenge, &device).unwrap();
    service
        .complete(
            owner,
            cert.device_public_key,
            JoinCompleteRequest { proof },
            now + 2,
        )
        .unwrap();
    (cert, device)
}

/// Give `person` a room capability as the room's authority.
fn capability(owner: &mut SessionOwner, capability: CapOverride, name: &str) {
    let author = owner.peer().authority().device_public_key;
    let mut room = owner.peer().admitted_room().unwrap().clone();
    let heads = room.get_heads();
    room.set_actor(ActorId::from(author.to_vec()));
    let capabilities = room.get(ROOT, "capabilities").unwrap().unwrap().1;
    room.put(
        &capabilities,
        name,
        ScalarValue::Bytes(serde_json::to_vec(&capability).unwrap()),
    )
    .unwrap();
    let changes = room
        .get_changes(&heads)
        .into_iter()
        .map(|c| c.raw_bytes().to_vec())
        .collect();
    let mut candidate = owner.peer().clone();
    candidate
        .receive_from(
            TransportOrigin::RelayDelta(author),
            NeighbourMsg::SyncResp {
                room: owner.peer().room().into(),
                changes,
            },
            web_time::SystemTime::now(),
        )
        .unwrap();
    owner
        .commit_mutation_candidate(candidate, owner.directory().to_vec())
        .unwrap();
}

/// A member whose room role is Owner: the room's owner for proposals.
fn room_owner(owner: &mut SessionOwner) -> (DeviceCert, SigningKey) {
    let (cert, key) = member(owner, 81, "Room owner");
    capability(
        owner,
        CapOverride::new(
            Subject::person(hex::encode(cert.user_public_key)),
            Scope::Workspace,
            Effect::Grant,
            Verb::Admin,
        ),
        "second-owner",
    );
    assert_eq!(
        owner
            .peer()
            .permissions(cert.device_public_key, web_time::SystemTime::now())
            .unwrap()
            .0,
        collab_types::Role::Owner
    );
    (cert, key)
}

/// A note with history, already rehomed as `record` of `room`.
fn document(
    room: &str,
    record: &ui::record::RecordId,
) -> (Vec<u8>, Vec<ui::automerge::ChangeHash>) {
    let mut doc = AutoCommit::new();
    notes::notebook::initialize_empty(&mut doc).unwrap();
    doc.commit();
    doc.put(ROOT, "body", "first draft").unwrap();
    doc.commit();
    doc.put(ROOT, "body", "second draft").unwrap();
    doc.commit();
    let history = doc.get_heads();
    assert!(ui::record::init_scoped(
        &mut doc,
        record,
        &ui::record::RecordKind::new("notes.notebook"),
        "3f2b1c00-0000-4000-8000-00000000000a",
        Some(room),
    ));
    doc.commit();
    (doc.save(), history)
}

struct Proposed {
    proposal: DocumentImportProposalV1,
    request: OwnerCommitRequest,
    history: Vec<ui::automerge::ChangeHash>,
}

fn propose(
    owner: &SessionOwner,
    caller: &DeviceCert,
    key: &SigningKey,
    id: u8,
    lifetime: u64,
) -> Proposed {
    let room = owner.peer().room().to_owned();
    let record = ui::record::RecordId::minted();
    let (bytes, history) = document(&room, &record);
    let issued = now_unix();
    let proposal = DocumentImportProposalV1::new(
        &DocumentImportTerms {
            room: room.clone(),
            proposal_id: [id; 32],
            app: APP.into(),
            record: record.as_str().into(),
            title: "Groceries".into(),
            document: bytes.clone(),
            issued_unix: issued,
            expires_unix: issued + lifetime,
        },
        key,
    )
    .unwrap();
    Proposed {
        request: OwnerCommitRequest {
            room,
            operation: CommitOperation::DocumentImport {
                invocation: uuid::Uuid::new_v4(),
                caller: caller.clone(),
                action: ImportAction::Propose(ProposeArgument::new(proposal.clone(), &bytes)),
            },
        },
        proposal,
        history,
    }
}

fn decide(
    owner: &SessionOwner,
    proposal: &DocumentImportProposalV1,
    caller: &DeviceCert,
    key: &SigningKey,
    accepted: bool,
) -> OwnerCommitRequest {
    let argument = DecideArgument {
        proposal_id: proposal.proposal_id,
        decision: DocumentImportDecisionV1::new(proposal, accepted, now_unix(), key).unwrap(),
    };
    OwnerCommitRequest {
        room: owner.peer().room().into(),
        operation: CommitOperation::DocumentImport {
            invocation: uuid::Uuid::new_v4(),
            caller: caller.clone(),
            action: if accepted {
                ImportAction::Accept(argument)
            } else {
                ImportAction::Decline(argument)
            },
        },
    }
}

fn entry(owner: &SessionOwner, id: u8) -> Option<import::ImportEntry> {
    import::entries(owner.peer().admitted_room().unwrap()).remove(&import::key(&[id; 32]))
}

#[test]
fn a_member_without_create_rights_is_refused_and_nothing_is_kept() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owner");
    let mut owner = SessionOwner::initialize_workspace(&path, "Imports").unwrap();
    let (caller, key) = member(&mut owner, 71, "Proposer");
    capability(
        &mut owner,
        CapOverride::new(
            Subject::person(hex::encode(caller.user_public_key)),
            Scope::Action {
                app: APP.into(),
                action: "document.create".into(),
            },
            Effect::Restrict,
            Verb::Read,
        ),
        "no-create",
    );
    let proposed = propose(&owner, &caller, &key, 1, 600);
    let before = owner.status();
    let bytes = std::fs::read(path.join("session.bin")).unwrap();
    assert!(
        matches!(
            owner.commit_document_import(&proposed.request, None),
            Err(DesktopCommandError::Rejected(reason)) if reason == "caller cannot create an application document"
        ),
        "a proposal needs exactly the rights an empty creation needs"
    );
    assert_eq!(owner.status(), before);
    assert_eq!(std::fs::read(path.join("session.bin")).unwrap(), bytes);
    assert!(entry(&owner, 1).is_none());
    assert!(
        owner
            .peer()
            .document(APP, &proposed.proposal.record)
            .is_none()
    );
}

#[test]
fn a_document_the_application_would_not_open_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owner");
    let mut owner = SessionOwner::initialize_workspace(&path, "Imports").unwrap();
    let (caller, key) = member(&mut owner, 71, "Proposer");
    let room = owner.peer().room().to_owned();
    let record = ui::record::RecordId::minted();
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "body", "not a notebook").unwrap();
    assert!(ui::record::init_scoped(
        &mut doc,
        &record,
        &ui::record::RecordKind::new("notes.notebook"),
        &room,
        Some(&room),
    ));
    doc.commit();
    let bytes = doc.save();
    let issued = now_unix();
    let proposal = DocumentImportProposalV1::new(
        &DocumentImportTerms {
            room: room.clone(),
            proposal_id: [6; 32],
            app: APP.into(),
            record: record.as_str().into(),
            title: "Broken".into(),
            document: bytes.clone(),
            issued_unix: issued,
            expires_unix: issued + 600,
        },
        &key,
    )
    .unwrap();
    let request = OwnerCommitRequest {
        room,
        operation: CommitOperation::DocumentImport {
            invocation: uuid::Uuid::new_v4(),
            caller,
            action: ImportAction::Propose(ProposeArgument::new(proposal, &bytes)),
        },
    };
    assert!(matches!(
        owner.commit_document_import(&request, None),
        Err(DesktopCommandError::Rejected(reason)) if reason.contains("would not open")
    ));
    assert!(entry(&owner, 6).is_none());
}

#[test]
fn an_unaccepted_proposal_never_creates_anything() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owner");
    let mut owner = SessionOwner::initialize_workspace(&path, "Imports").unwrap();
    let (caller, key) = member(&mut owner, 71, "Proposer");
    let (decider, decider_key) = room_owner(&mut owner);
    let proposed = propose(&owner, &caller, &key, 2, 600);
    let record = proposed.proposal.record.clone();
    owner
        .commit_document_import(&proposed.request, None)
        .expect("a member who may create proposes");
    assert_eq!(
        entry(&owner, 2).unwrap().state(now_unix()),
        ImportState::Pending,
        "the proposal is in the room for its owner to see"
    );
    assert!(
        owner.peer().document(APP, &record).is_none(),
        "a proposal alone creates nothing"
    );

    // Proposing is not accepting: the proposer is no owner of the room.
    let self_accepted = decide(&owner, &proposed.proposal, &caller, &key, true);
    assert!(matches!(
        owner.commit_document_import(&self_accepted, None),
        Err(DesktopCommandError::Rejected(reason)) if reason.contains("owner")
    ));
    assert!(owner.peer().document(APP, &record).is_none());
    // A decision signed by one device cannot be given by another.
    let mut forged = decide(&owner, &proposed.proposal, &caller, &decider_key, true);
    let CommitOperation::DocumentImport { caller: sender, .. } = &mut forged.operation else {
        unreachable!()
    };
    *sender = caller.clone();
    assert!(owner.commit_document_import(&forged, None).is_err());

    owner
        .commit_document_import(
            &decide(&owner, &proposed.proposal, &decider, &decider_key, false),
            None,
        )
        .expect("the owner declines");
    assert_eq!(
        entry(&owner, 2).unwrap().state(now_unix()),
        ImportState::Declined
    );
    assert!(owner.peer().document(APP, &record).is_none());
    assert!(
        owner
            .commit_document_import(
                &decide(&owner, &proposed.proposal, &decider, &decider_key, true),
                None,
            )
            .is_err(),
        "a declined proposal stays declined"
    );
    assert!(owner.peer().document(APP, &record).is_none());

    // Nobody answers in time: acceptance after expiry creates nothing.
    let expiring = propose(&owner, &caller, &key, 3, 1);
    owner
        .commit_document_import(&expiring.request, None)
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2_100));
    assert_eq!(
        entry(&owner, 3).unwrap().state(now_unix()),
        ImportState::Expired
    );
    let late = decide(&owner, &expiring.proposal, &decider, &decider_key, true);
    assert!(owner.commit_document_import(&late, None).is_err());
    assert!(
        owner
            .peer()
            .document(APP, &expiring.proposal.record)
            .is_none()
    );
}

#[test]
fn an_accepted_proposal_becomes_the_record_with_its_history() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owner");
    let mut owner = SessionOwner::initialize_workspace(&path, "Imports").unwrap();
    let (caller, key) = member(&mut owner, 71, "Proposer");
    let (decider, decider_key) = room_owner(&mut owner);
    let proposed = propose(&owner, &caller, &key, 4, 600);
    let record = proposed.proposal.record.clone();
    owner
        .commit_document_import(&proposed.request, None)
        .unwrap();
    owner
        .commit_document_import(
            &decide(&owner, &proposed.proposal, &decider, &decider_key, true),
            None,
        )
        .expect("the owner accepts");
    assert_eq!(
        entry(&owner, 4).unwrap().state(now_unix()),
        ImportState::Accepted
    );
    let check = |owner: &SessionOwner| {
        let created = AutoCommit::load(
            &owner
                .peer()
                .document(APP, &record)
                .expect("the accepted document is the record now"),
        )
        .unwrap();
        for head in &proposed.history {
            assert!(
                created.get_change_by_hash(head).is_some(),
                "the imported history came along"
            );
        }
        assert_eq!(
            created.get(ROOT, "body").unwrap().unwrap().0.as_str(),
            Some("second draft")
        );
    };
    check(&owner);
    assert!(
        owner
            .commit_document_import(
                &decide(&owner, &proposed.proposal, &decider, &decider_key, true),
                None,
            )
            .is_err(),
        "an answered proposal creates once"
    );
    drop(owner);
    let reopened = SessionOwner::open(&path).unwrap();
    check(&reopened);
    assert_eq!(
        entry(&reopened, 4).unwrap().state(now_unix()),
        ImportState::Accepted
    );
}

#[test]
fn an_owner_restart_forgets_the_held_document_and_says_so() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owner");
    let mut owner = SessionOwner::initialize_workspace(&path, "Imports").unwrap();
    let (caller, key) = member(&mut owner, 71, "Proposer");
    let (decider, decider_key) = room_owner(&mut owner);
    let proposed = propose(&owner, &caller, &key, 5, 600);
    owner
        .commit_document_import(&proposed.request, None)
        .unwrap();
    drop(owner);
    let mut reopened = SessionOwner::open(&path).unwrap();
    let accepted = decide(&reopened, &proposed.proposal, &decider, &decider_key, true);
    assert!(matches!(
        reopened.commit_document_import(&accepted, None),
        Err(DesktopCommandError::Rejected(reason)) if reason.contains("proposed again")
    ));
    assert!(
        reopened
            .peer()
            .document(APP, &proposed.proposal.record)
            .is_none()
    );
}
