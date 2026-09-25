//! Actual governor OpenTab tests for the shared canonical creation leaf.
use collab_types::caps::{CapOverride, Effect, Scope, Subject, Verb};
use qos_peer_state::{NeighbourMsg, TransportOrigin, desktop_command::*, owner_commit::*};
use qos_session_peer::SessionOwner;
use ui::automerge::{ActorId, ROOT, ReadDoc, ScalarValue, transaction::Transactable};

fn opening(owner: &SessionOwner, app: &str) -> OwnerCommitRequest {
    let contents =
        qos_desktop::projection::RoomProjection::contents_of(owner.peer().admitted_room().unwrap());
    OwnerCommitRequest {
        room: owner.peer().room().into(),
        operation: CommitOperation::DesktopCommand {
            invocation: uuid::Uuid::new_v4(),
            caller: owner.peer().authority().clone(),
            command: BoundCommand {
                source: CommandSource {
                    room: owner.peer().room().into(),
                    heads: owner
                        .peer()
                        .admitted_room()
                        .unwrap()
                        .clone()
                        .get_heads()
                        .into_iter()
                        .map(|h| h.0)
                        .collect(),
                },
                command: DesktopCommand::OpenTab {
                    app: app.into(),
                    placement: contents
                        .first()
                        .map_or(TabPlacement::EmptyWorkspace, |strip| {
                            TabPlacement::NewGroup {
                                strip: strip.id,
                                after: None,
                            }
                        }),
                },
            },
        },
    }
}

fn restrict(owner: &mut SessionOwner, scope: Scope, name: &str) {
    let role = owner
        .peer()
        .permissions(
            owner.peer().authority().device_public_key,
            web_time::SystemTime::now(),
        )
        .unwrap()
        .0;
    restrict_subject(owner, Subject::Role(role), scope, name);
}

fn restrict_subject(owner: &mut SessionOwner, subject: Subject, scope: Scope, name: &str) {
    let author = owner.peer().authority().device_public_key;
    let now = web_time::SystemTime::now();
    let mut room = owner.peer().admitted_room().unwrap().clone();
    let heads = room.get_heads();
    room.set_actor(ActorId::from(author.to_vec()));
    let capabilities = room.get(ROOT, "capabilities").unwrap().unwrap().1;
    let restriction = CapOverride::new(subject, scope, Effect::Restrict, Verb::Read);
    room.put(
        &capabilities,
        name,
        ScalarValue::Bytes(serde_json::to_vec(&restriction).unwrap()),
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
            now,
        )
        .unwrap();
    owner
        .commit_mutation_candidate(candidate, owner.directory().to_vec())
        .unwrap();
}

#[test]
fn canonical_creation_denied_action_leaves_real_owner_and_topology_unchanged() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owner");
    let mut owner = SessionOwner::initialize_workspace(&path, "Create policy").unwrap();
    restrict(
        &mut owner,
        Scope::Action {
            app: "so.tilo.notes".into(),
            action: "document.create".into(),
        },
        "deny-create",
    );
    let request = opening(&owner, "so.tilo.notes");
    let before = owner.status();
    let bytes = std::fs::read(path.join("session.bin")).unwrap();
    assert!(
        matches!(
            owner.commit_desktop_command(&request),
            Err(DesktopCommandError::Rejected(reason)) if reason == "caller cannot create an application document"
        ),
        "topology Write must not substitute for application Create"
    );
    assert_eq!(owner.status(), before);
    assert_eq!(std::fs::read(path.join("session.bin")).unwrap(), bytes);
    assert!(owner.status().records.is_empty());
}

#[test]
fn canonical_creation_denied_exact_artifact_write_preserves_read_and_has_no_effect() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owner");
    let mut owner = SessionOwner::initialize_workspace(&path, "Artifact policy").unwrap();
    let artifact = collab_types::document_artifact_id("so.tilo.chat", owner.peer().room(), "");
    restrict(
        &mut owner,
        Scope::ResourceInstance(artifact),
        "deny-artifact-write",
    );
    owner
        .peer()
        .check_document_read(
            owner.peer().authority().device_public_key,
            "so.tilo.chat",
            "",
            web_time::SystemTime::now(),
        )
        .unwrap();
    let before = owner.status();
    let bytes = std::fs::read(path.join("session.bin")).unwrap();
    assert!(
        matches!(
            owner.commit_desktop_command(&opening(&owner, "so.tilo.chat")),
            Err(DesktopCommandError::Rejected(_))
        ),
        "Read must not substitute for exact Artifact Write"
    );
    assert_eq!(owner.status(), before);
    assert_eq!(std::fs::read(path.join("session.bin")).unwrap(), bytes);
}

#[test]
fn canonical_creation_existing_primary_opens_with_read_after_create_and_write_revoked() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owner");
    let mut owner = SessionOwner::initialize_workspace(&path, "Existing primary").unwrap();
    let first = opening(&owner, "so.tilo.chat");
    owner.commit_desktop_command(&first).unwrap();
    let document = owner.peer().document("so.tilo.chat", "").unwrap();
    let artifact = collab_types::document_artifact_id("so.tilo.chat", owner.peer().room(), "");
    restrict(
        &mut owner,
        Scope::ResourceInstance(artifact),
        "deny-artifact-write",
    );
    restrict(
        &mut owner,
        Scope::Action {
            app: "so.tilo.chat".into(),
            action: "document.create".into(),
        },
        "deny-create",
    );
    let second = opening(&owner, "so.tilo.chat");
    owner
        .commit_desktop_command(&second)
        .unwrap()
        .validate_for(&second)
        .unwrap();
    assert_eq!(owner.peer().document("so.tilo.chat", "").unwrap(), document);
    assert_eq!(owner.status().records.len(), 1);
    drop(owner);
    let restored = SessionOwner::open(&path).unwrap();
    assert_eq!(
        restored.peer().document("so.tilo.chat", "").unwrap(),
        document
    );
}

fn admitted_guest(owner: &mut SessionOwner) -> tilo_user_identity::DeviceCert {
    use qos_peer_state::join::{
        JoinChallengeRequest, JoinCompleteRequest, MemberRole, sign_join_proof,
    };
    use qos_session_peer::collaboration::{CollaborationService, IssueInvitationRequest};
    use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};
    let root = ed25519_dalek::SigningKey::from_bytes(&[91; 32]);
    let device = ed25519_dalek::SigningKey::from_bytes(&[92; 32]);
    let now = tilo_relay::clock::now_ns();
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "Creation caller".into(),
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
                request_id: [91; 32],
                role: MemberRole::Editor,
                recipient_user_public_key: Some(cert.user_public_key),
                invitee_name: Some("Creation guest".into()),
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
                request_id: [92; 32],
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
    cert
}

#[test]
fn canonical_creation_original_guest_write_is_checked_before_governor_factory_publication() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owner");
    let mut owner = SessionOwner::initialize_workspace(&path, "Original caller").unwrap();
    let caller = admitted_guest(&mut owner);
    let artifact = collab_types::document_artifact_id("so.tilo.chat", owner.peer().room(), "");
    restrict_subject(
        &mut owner,
        Subject::person(hex::encode(caller.user_public_key)),
        Scope::ResourceInstance(artifact),
        "guest-artifact-read-only",
    );
    owner
        .peer()
        .check_document_read(
            caller.device_public_key,
            "so.tilo.chat",
            "",
            web_time::SystemTime::now(),
        )
        .unwrap();
    let mut request = opening(&owner, "so.tilo.chat");
    let CommitOperation::DesktopCommand { caller: actual, .. } = &mut request.operation else {
        unreachable!()
    };
    *actual = caller;
    let before = owner.status();
    let bytes = std::fs::read(path.join("session.bin")).unwrap();
    assert!(
        matches!(
            owner.commit_desktop_command(&request),
            Err(DesktopCommandError::Rejected(_))
        ),
        "governor signature cannot replace original guest Artifact Write"
    );
    assert_eq!(owner.status(), before);
    assert_eq!(std::fs::read(path.join("session.bin")).unwrap(), bytes);
}
