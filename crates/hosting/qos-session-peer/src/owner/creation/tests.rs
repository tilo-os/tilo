use super::*;

fn selected_owner() -> (tempfile::TempDir, SessionOwner) {
    let dir = tempfile::tempdir().unwrap();
    let mut owner =
        SessionOwner::initialize_workspace(&dir.path().join("owner"), "Creation").unwrap();
    owner.activate_delivery().unwrap();
    (dir, owner)
}
fn version(owner: &SessionOwner) -> u32 {
    let bytes = std::fs::read(owner.root().join("session.bin")).unwrap();
    let signed: SignedAggregate = postcard::from_bytes(&bytes).unwrap();
    postcard::take_from_bytes::<u32>(&signed.payload).unwrap().0
}
#[test]
fn creation_activation_is_explicit_and_preserves_v5_bytes_until_selected() {
    let (_dir, mut owner) = selected_owner();
    let path = owner.root().to_owned();
    assert_eq!(version(&owner), 5);
    let bytes = std::fs::read(path.join("session.bin")).unwrap();
    owner.flush().unwrap();
    drop(owner);
    let mut owner = SessionOwner::open(&path).unwrap();
    assert_eq!(std::fs::read(path.join("session.bin")).unwrap(), bytes);
    let generation = owner.status().generation;
    owner.activate_creation().unwrap();
    assert_eq!(version(&owner), 6);
    assert_eq!(owner.status().generation, generation + 1);
    let bytes = std::fs::read(path.join("session.bin")).unwrap();
    drop(owner);
    let mut owner = SessionOwner::open(&path).unwrap();
    owner.flush().unwrap();
    owner.activate_delivery().unwrap();
    owner.activate_creation().unwrap();
    assert_eq!(std::fs::read(path.join("session.bin")).unwrap(), bytes);
}
#[test]
fn creation_activation_requires_existing_native_v5_selection() {
    let dir = tempfile::tempdir().unwrap();
    let mut owner =
        SessionOwner::initialize_workspace(&dir.path().join("owner"), "Legacy").unwrap();
    let before = std::fs::read(owner.root().join("session.bin")).unwrap();
    assert!(owner.activate_creation().is_err());
    assert_eq!(version(&owner), 4);
    assert_eq!(
        std::fs::read(owner.root().join("session.bin")).unwrap(),
        before
    );
}
#[test]
fn creation_activation_fault_cannot_expose_selected_memory_or_acknowledgment() {
    let (_dir, mut owner) = selected_owner();
    let path = owner.root().to_owned();
    let before = owner.status();
    let bytes = std::fs::read(path.join("session.bin")).unwrap();
    let result = crate::store::test_fault::with_fault(
        &path,
        crate::store::test_fault::Point::BeforeWrite,
        || owner.activate_creation(),
    );
    assert!(result.is_err());
    assert_eq!(owner.status(), before);
    assert_eq!(std::fs::read(path.join("session.bin")).unwrap(), bytes);
    drop(owner);
    let owner = SessionOwner::open(&path).unwrap();
    assert_eq!(version(&owner), 5);
}

fn opening(owner: &SessionOwner, app: &str) -> qos_peer_state::owner_commit::OwnerCommitRequest {
    use qos_peer_state::{desktop_command::*, owner_commit::*};
    OwnerCommitRequest {
        room: owner.peer.room().into(),
        operation: CommitOperation::DesktopCommand {
            invocation: uuid::Uuid::new_v4(),
            caller: owner.peer.authority().clone(),
            command: BoundCommand {
                source: CommandSource {
                    room: owner.peer.room().into(),
                    heads: owner
                        .peer
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
                    placement: TabPlacement::EmptyWorkspace,
                },
            },
        },
    }
}
#[test]
fn creation_open_tab_exact_retry_and_restart_retain_one_original_result() {
    let (_dir, mut owner) = selected_owner();
    owner.activate_creation().unwrap();
    let request = opening(&owner, "dev.qos.notes");
    let first = owner.commit_desktop_command(&request).unwrap();
    assert_eq!(owner.aggregate.creation.as_ref().unwrap().len(), 1);
    let row = owner
        .aggregate
        .creation
        .as_ref()
        .unwrap()
        .rows()
        .next()
        .unwrap()
        .clone();
    assert_eq!(row.result.body.owner_generation, first.generation);
    let before = owner.status();
    let bytes = std::fs::read(owner.root().join("session.bin")).unwrap();
    owner.commit_desktop_command(&request).unwrap();
    assert_eq!(owner.status(), before);
    assert_eq!(
        std::fs::read(owner.root().join("session.bin")).unwrap(),
        bytes
    );
    let root = owner.root().to_owned();
    drop(owner);
    let mut owner = SessionOwner::open(&root).unwrap();
    owner.commit_desktop_command(&request).unwrap();
    assert_eq!(owner.status(), before);
    assert_eq!(
        owner.aggregate.creation.as_ref().unwrap().rows().next(),
        Some(&row)
    );
    let mut substitution = request.clone();
    if let qos_peer_state::owner_commit::CommitOperation::DesktopCommand { command, .. } =
        &mut substitution.operation
    {
        command.command = qos_peer_state::desktop_command::DesktopCommand::RenameWorkspace {
            name: "substitution".into(),
        };
    }
    assert!(owner.commit_desktop_command(&substitution).is_err());
    assert_eq!(owner.status(), before);
}
#[test]
fn creation_terminal_barrier_failure_leaves_document_topology_and_result_unpublished() {
    let (_dir, mut owner) = selected_owner();
    owner.activate_creation().unwrap();
    let request = opening(&owner, "dev.qos.notes");
    let root = owner.root().to_owned();
    let before = owner.status();
    let bytes = std::fs::read(root.join("session.bin")).unwrap();
    let result = crate::store::test_fault::with_fault(
        &root,
        crate::store::test_fault::Point::BeforeWrite,
        || owner.commit_desktop_command(&request),
    );
    assert!(matches!(
        result,
        Err(qos_peer_state::desktop_command::DesktopCommandError::Uncertain(_))
    ));
    assert_eq!(owner.status(), before);
    assert!(owner.aggregate.creation.as_ref().unwrap().is_empty());
    assert_eq!(std::fs::read(root.join("session.bin")).unwrap(), bytes);
    drop(owner);
    let mut owner = SessionOwner::open(&root).unwrap();
    owner.commit_desktop_command(&request).unwrap();
    assert_eq!(owner.aggregate.creation.as_ref().unwrap().len(), 1);
    assert_eq!(owner.peer.rows().len(), 1);
}
#[test]
fn creation_v6_requires_complete_canonical_section_and_v5_rejects_extension() {
    let (_dir, mut owner) = selected_owner();
    let v5 = owner.aggregate.encode_payload().unwrap();
    assert_eq!(v5[0], 5);
    let mut missing = v5.clone();
    missing[0] = 6;
    assert!(Aggregate::decode_payload(&missing).is_err());
    owner.activate_creation().unwrap();
    let bytes = owner.aggregate.encode_payload().unwrap();
    assert_eq!(bytes[0], 6);
    assert!(Aggregate::decode_payload(&bytes).is_ok());
    assert!(Aggregate::decode_payload(&bytes[..bytes.len() - 1]).is_err());
    let mut wrong = bytes.clone();
    wrong[0] = 5;
    assert!(Aggregate::decode_payload(&wrong).is_err());
    let mut trailing = bytes;
    trailing.push(0);
    assert!(Aggregate::decode_payload(&trailing).is_err());
}

#[test]
fn creation_retained_result_refreshes_expired_checkpoint_and_coalesces_fresh_retry() {
    let (_dir, mut owner) = selected_owner();
    owner.activate_creation().unwrap();
    let request = opening(&owner, "dev.qos.notes");
    owner.commit_desktop_command(&request).unwrap();
    let row = owner
        .aggregate
        .creation
        .as_ref()
        .unwrap()
        .rows()
        .next()
        .unwrap()
        .clone();
    let route = row.result.body.route.clone();
    let later = SystemTime::now() + web_time::Duration::from_secs(301);
    assert!(!owner.creation_checkpoint_current(&route, later));
    let generation = owner.status().generation;
    with_test_publication_time(later, || owner.replay_desktop_creation(&request, later))
        .unwrap()
        .unwrap();
    assert_eq!(owner.status().generation, generation + 1);
    assert!(owner.creation_checkpoint_current(&route, later));
    assert_eq!(
        owner.aggregate.creation.as_ref().unwrap().rows().next(),
        Some(&row)
    );
    let mut replica = StatePeer::new(
        owner.peer.room(),
        owner.peer.authority().clone(),
        owner.peer.certificates(),
        RevocationList::new(),
        later,
    )
    .unwrap();
    replica.select_admission_v5().unwrap();
    for publication in owner.outgoing().iter() {
        let message = postcard::from_bytes::<NeighbourMsg>(publication.bytes).unwrap();
        replica
            .receive_from(
                TransportOrigin::RelayDelta(owner.peer.authority().device_public_key),
                message,
                later,
            )
            .unwrap();
    }
    assert_eq!(
        replica.document(&route.app, &route.record),
        owner.peer.document(&route.app, &route.record)
    );
    assert!(
        replica
            .durable_coverage(&route.app, &route.record)
            .unwrap()
            .is_some()
    );
    let bytes = std::fs::read(owner.root().join("session.bin")).unwrap();
    with_test_publication_time(later, || owner.replay_desktop_creation(&request, later))
        .unwrap()
        .unwrap();
    assert_eq!(owner.status().generation, generation + 1);
    assert_eq!(
        std::fs::read(owner.root().join("session.bin")).unwrap(),
        bytes
    );
}

#[test]
fn creation_restore_rejects_desktop_source_bytes_not_bound_to_original_intent() {
    let (_dir, mut owner) = selected_owner();
    owner.activate_creation().unwrap();
    let request = opening(&owner, "dev.qos.notes");
    owner.commit_desktop_command(&request).unwrap();
    let mut row = owner
        .aggregate
        .creation
        .as_ref()
        .unwrap()
        .rows()
        .next()
        .unwrap()
        .clone();
    row.original_request.push(0);
    assert!(
        row.validate().is_err(),
        "trailing original request bytes cannot be a retained intent"
    );
}

#[test]
fn creation_duplicate_tab_uses_same_result_ledger_and_exact_retry() {
    use qos_peer_state::{desktop_command::*, owner_commit::*};
    let (_dir, mut owner) = selected_owner();
    owner.activate_creation().unwrap();
    owner
        .commit_desktop_command(&opening(&owner, "dev.qos.notes"))
        .unwrap();
    let contents =
        qos_desktop::projection::RoomProjection::contents_of(owner.peer.admitted_room().unwrap());
    let mut duplicate = opening(&owner, "dev.qos.notes");
    let CommitOperation::DesktopCommand { command, .. } = &mut duplicate.operation else {
        unreachable!()
    };
    command.command = DesktopCommand::DuplicateTab {
        location: TabLocation {
            strip: contents[0].id,
            group: contents[0].groups[0].id,
            tab: contents[0].groups[0].tabs[0].id,
        },
    };
    owner.commit_desktop_command(&duplicate).unwrap();
    assert_eq!(owner.aggregate.creation.as_ref().unwrap().len(), 2);
    assert_eq!(owner.peer.rows().len(), 2);
    let before = owner.status();
    owner.commit_desktop_command(&duplicate).unwrap();
    assert_eq!(owner.status(), before);
}

fn admitted_guest(owner: &mut SessionOwner) -> tilo_user_identity::DeviceCert {
    use crate::collaboration::{CollaborationService, IssueInvitationRequest};
    use qos_peer_state::join::{
        JoinChallengeRequest, JoinCompleteRequest, MemberRole, sign_join_proof,
    };
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
fn creation_retained_result_follows_current_original_membership_not_governor_rights() {
    use crate::collaboration::{CollaborationService, RemoveMemberRequest, SetMemberRoleRequest};
    let (_dir, mut owner) = selected_owner();
    owner.activate_creation().unwrap();
    let caller = admitted_guest(&mut owner);
    let mut request = opening(&owner, "dev.qos.notes");
    let qos_peer_state::owner_commit::CommitOperation::DesktopCommand { caller: actual, .. } =
        &mut request.operation
    else {
        unreachable!()
    };
    *actual = caller.clone();
    owner.commit_desktop_command(&request).unwrap();
    let row = owner
        .aggregate
        .creation
        .as_ref()
        .unwrap()
        .rows()
        .next()
        .unwrap()
        .clone();
    let service = CollaborationService::new(vec![]);
    let administrator = owner.peer.authority().device_public_key;
    service
        .set_member_role(
            &mut owner,
            administrator,
            SetMemberRoleRequest {
                request_id: [93; 32],
                user_public_key: caller.user_public_key,
                role: qos_peer_state::join::MemberRole::Viewer,
            },
            tilo_relay::clock::now_ns(),
        )
        .unwrap();
    owner
        .commit_desktop_command(&request)
        .expect("retained result lookup needs current Read, not a new topology effect");
    assert_eq!(
        owner.aggregate.creation.as_ref().unwrap().rows().next(),
        Some(&row)
    );
    service
        .remove_member(
            &mut owner,
            administrator,
            RemoveMemberRequest {
                request_id: [94; 32],
                user_public_key: caller.user_public_key,
            },
            tilo_relay::clock::now_ns(),
        )
        .unwrap();
    let before = owner.status();
    assert!(owner.commit_desktop_command(&request).is_err());
    assert!(
        owner
            .replay_desktop_creation(&request, SystemTime::now())
            .is_err()
    );
    assert_eq!(owner.status(), before);
    assert_eq!(
        owner.aggregate.creation.as_ref().unwrap().rows().next(),
        Some(&row)
    );
}
#[test]
fn creation_uncertain_after_atomic_write_reopens_exact_whole_result_without_repeating_factory() {
    let (_dir, mut owner) = selected_owner();
    owner.activate_creation().unwrap();
    let request = opening(&owner, "dev.qos.notes");
    let root = owner.root().to_owned();
    let before = owner.status();
    let result = crate::store::test_fault::with_fault(
        &root,
        crate::store::test_fault::Point::AfterAtomicWrite,
        || owner.commit_desktop_command(&request),
    );
    assert!(matches!(
        result,
        Err(qos_peer_state::desktop_command::DesktopCommandError::Uncertain(_))
    ));
    assert_eq!(owner.status(), before);
    assert!(owner.store.is_poisoned());
    drop(owner);
    let mut owner = SessionOwner::open(&root).unwrap();
    assert_eq!(owner.peer.rows().len(), 1);
    assert_eq!(owner.aggregate.creation.as_ref().unwrap().len(), 1);
    let before = owner.status();
    owner.commit_desktop_command(&request).unwrap();
    assert_eq!(owner.status(), before);
}
