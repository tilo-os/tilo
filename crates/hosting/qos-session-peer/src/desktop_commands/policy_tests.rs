use super::*;
use qos_action_broker::peer_actions::OwnerPolicyClock;
fn request(owner: &SessionOwner) -> OwnerCommitRequest {
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
                    app: "dev.qos.notes".into(),
                    placement: TabPlacement::EmptyWorkspace,
                },
            },
        },
    }
}
#[test]
fn desktop_policy_original_retired_origin_refuses_before_factory_or_write() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owner");
    let mut owner = SessionOwner::initialize_workspace(&path, "Policy").unwrap();
    owner.activate_delivery().unwrap();
    owner.activate_creation().unwrap();
    let request = request(&owner);
    let clock = OwnerPolicyClock::new().unwrap();
    let origin = clock.new_origin();
    let time = origin.capture().unwrap();
    origin.invalidate();
    let before = owner.status();
    let bytes = std::fs::read(path.join("session.bin")).unwrap();
    assert!(matches!(
        owner.commit_desktop_command_with_policy_time(&request, &time),
        Err(DesktopCommandError::Rejected(_))
    ));
    assert_eq!(owner.status(), before);
    assert_eq!(std::fs::read(path.join("session.bin")).unwrap(), bytes);
}

#[test]
fn desktop_policy_origin_retired_after_actual_encoding_refuses_before_store_write() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owner");
    let mut owner = SessionOwner::initialize_workspace(&path, "Final fence").unwrap();
    owner.activate_delivery().unwrap();
    owner.activate_creation().unwrap();
    let request = request(&owner);
    let clock = OwnerPolicyClock::new().unwrap();
    let origin = std::rc::Rc::new(clock.new_origin());
    let time = origin.capture().unwrap();
    let retiring = origin.clone();
    let before = owner.status();
    let bytes = std::fs::read(path.join("session.bin")).unwrap();
    let result = crate::owner::policy_fence::with_before_check(
        move || retiring.invalidate(),
        || owner.commit_desktop_command_with_policy_time(&request, &time),
    );
    assert!(
        matches!(result, Err(DesktopCommandError::Rejected(_))),
        "pre-write origin denial is a definite no-effect refusal"
    );
    assert_eq!(owner.status(), before);
    assert_eq!(std::fs::read(path.join("session.bin")).unwrap(), bytes);
}

#[cfg(feature = "test-support")]
fn expiring_guest(owner: &mut SessionOwner) -> tilo_user_identity::DeviceCert {
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
        expires_at_ns: Some(now + 60_000_000_000),
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

#[cfg(feature = "test-support")]
#[test]
fn desktop_policy_clock_advance_during_encoding_rechecks_original_caller_expiry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owner");
    let mut owner = SessionOwner::initialize_workspace(&path, "Expiry fence").unwrap();
    owner.activate_delivery().unwrap();
    owner.activate_creation().unwrap();
    let caller = expiring_guest(&mut owner);
    let mut request = request(&owner);
    let CommitOperation::DesktopCommand { caller: actual, .. } = &mut request.operation else {
        unreachable!()
    };
    *actual = caller;
    let now = tilo_relay::clock::now_ns();
    let (clock, control) = OwnerPolicyClock::for_test(now);
    let origin = clock.new_origin();
    let time = origin.capture().unwrap();
    let before = owner.status();
    let bytes = std::fs::read(path.join("session.bin")).unwrap();
    let result = crate::owner::policy_fence::with_before_check(
        move || control.advance_monotonic(std::time::Duration::from_secs(120)),
        || owner.commit_desktop_command_with_delivery_policy_time(&request, &time),
    );
    assert!(matches!(result, Err(DesktopCommandError::Rejected(_))));
    assert_eq!(owner.status(), before);
    assert_eq!(std::fs::read(path.join("session.bin")).unwrap(), bytes);
    assert!(owner.peer().rows().is_empty());
}
#[test]
fn desktop_policy_live_original_context_commits_and_replays_through_direct_delivery() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owner");
    let mut owner = SessionOwner::initialize_workspace(&path, "Live policy").unwrap();
    owner.activate_delivery().unwrap();
    owner.activate_creation().unwrap();
    let request = request(&owner);
    let clock = OwnerPolicyClock::new().unwrap();
    let origin = clock.new_origin();
    let time = origin.capture().unwrap();
    let (receipt, delivery) = owner
        .commit_desktop_command_with_delivery_policy_time(&request, &time)
        .unwrap();
    receipt.validate_for(&request).unwrap();
    assert!(delivery.is_some());
    let before = owner.status();
    let bytes = std::fs::read(path.join("session.bin")).unwrap();
    let (receipt, delivery) = owner
        .commit_desktop_command_with_delivery_policy_time(&request, &time)
        .unwrap();
    receipt.validate_for(&request).unwrap();
    assert!(delivery.is_some());
    assert_eq!(owner.status(), before);
    assert_eq!(std::fs::read(path.join("session.bin")).unwrap(), bytes);
}

#[cfg(feature = "test-support")]
#[test]
fn desktop_policy_retained_retry_refresh_uses_original_clock_without_another_factory() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owner");
    let mut owner = SessionOwner::initialize_workspace(&path, "Retained clock").unwrap();
    owner.activate_delivery().unwrap();
    owner.activate_creation().unwrap();
    let request = request(&owner);
    let (clock, control) = OwnerPolicyClock::for_test(tilo_relay::clock::now_ns());
    let origin = clock.new_origin();
    let time = origin.capture().unwrap();
    owner
        .commit_desktop_command_with_policy_time(&request, &time)
        .unwrap();
    let generation = owner.status().generation;
    let rows = owner.peer().rows();
    control.advance_monotonic(std::time::Duration::from_secs(301));
    owner
        .commit_desktop_command_with_policy_time(&request, &time)
        .unwrap();
    assert_eq!(owner.status().generation, generation + 1);
    assert_eq!(owner.peer().rows(), rows);
    assert_eq!(
        owner
            .delivery_ledger()
            .unwrap()
            .replay
            .last_key_value()
            .unwrap()
            .1
            .committed_at_ns,
        time.now_ns().unwrap(),
        "the checkpoint receipt uses the same original clock as its signed publication"
    );
    let now = crate::owner::policy_fence::sample(Some(&time)).unwrap();
    let mut replica = qos_peer_state::StatePeer::new(
        owner.peer().room(),
        owner.peer().authority().clone(),
        owner.peer().certificates(),
        tilo_user_identity::RevocationList::new(),
        now,
    )
    .unwrap();
    replica.select_admission_v5().unwrap();
    for publication in owner.outgoing().iter() {
        replica
            .receive_from(
                qos_peer_state::TransportOrigin::RelayDelta(
                    owner.peer().authority().device_public_key,
                ),
                postcard::from_bytes(publication.bytes).unwrap(),
                now,
            )
            .unwrap();
    }
    assert_eq!(replica.rows(), rows);
    let bytes = std::fs::read(path.join("session.bin")).unwrap();
    owner
        .commit_desktop_command_with_policy_time(&request, &time)
        .unwrap();
    assert_eq!(owner.status().generation, generation + 1);
    assert_eq!(std::fs::read(path.join("session.bin")).unwrap(), bytes);
}

#[test]
fn desktop_policy_retained_read_rechecks_origin_after_encoding_without_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owner");
    let mut owner = SessionOwner::initialize_workspace(&path, "Read fence").unwrap();
    owner.activate_delivery().unwrap();
    owner.activate_creation().unwrap();
    let request = request(&owner);
    let clock = OwnerPolicyClock::new().unwrap();
    let origin = std::rc::Rc::new(clock.new_origin());
    let time = origin.capture().unwrap();
    owner
        .commit_desktop_command_with_policy_time(&request, &time)
        .unwrap();
    let before = owner.status();
    let bytes = std::fs::read(path.join("session.bin")).unwrap();
    let retiring = origin.clone();
    let result = crate::owner::policy_fence::with_before_check(
        move || retiring.invalidate(),
        || owner.commit_desktop_command_with_policy_time(&request, &time),
    );
    assert!(matches!(result, Err(DesktopCommandError::Rejected(_))));
    assert_eq!(owner.status(), before);
    assert_eq!(std::fs::read(path.join("session.bin")).unwrap(), bytes);
}
