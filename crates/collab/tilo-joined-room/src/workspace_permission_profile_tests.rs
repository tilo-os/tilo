use super::*;
use crate::application_record::tests::{Fixture, fixture, fixture_with_expiry};
use collab_types::{Expiry, PermissionProfile, PermissionProfileRule, Scope, Subject, Verb};
use tilo_peer_state::workspace_permission_profile::WorkspacePermissionProfileCommand as Command;

fn profile(actions: &[&str]) -> PermissionProfile {
    PermissionProfile {
        name: "Exact Agent view".into(),
        description: String::new(),
        rules: actions
            .iter()
            .map(|action| {
                PermissionProfileRule::grant(
                    Scope::Action {
                        app: "so.tilo.chat".into(),
                        action: (*action).into(),
                    },
                    Verb::Read,
                )
            })
            .collect(),
    }
}

fn request(fixture: &Fixture, command: Command) -> OwnerCommitRequest {
    OwnerCommitRequest {
        room: crate::testing::ROOM.into(),
        operation: CommitOperation::WorkspacePermissionProfileCommand {
            invocation: uuid::Uuid::new_v4(),
            caller: fixture.certificate.clone(),
            command,
        },
    }
}

fn commit(fixture: &Fixture, request: OwnerCommitRequest) -> Result<OwnerCommitReceipt, String> {
    fixture.owner.commit_workspace_permission_profile_command(
        request,
        &fixture.certificate,
        &fixture.key,
        fixture.now,
    )
}

fn save(fixture: &Fixture, actions: &[&str]) {
    commit(
        fixture,
        request(
            fixture,
            Command::Save {
                profile: profile(actions),
            },
        ),
    )
    .unwrap();
}

fn assign(fixture: &Fixture, agent: &str) -> OwnerCommitRequest {
    request(
        fixture,
        Command::Assign {
            profile_name: "Exact Agent view".into(),
            subject: Subject::agent(agent),
            expiry: Expiry::Never,
        },
    )
}

fn caps(fixture: &Fixture) -> Vec<collab_types::caps::CapOverride> {
    fixture
        .owner
        .with_current(RoomId::parse(crate::testing::ROOM).unwrap(), |view| {
            Ok(
                tilo_peer_state::room::extract_governed_fields(
                    view.peer().admitted_room().unwrap(),
                )
                .2,
            )
        })
        .unwrap()
}

#[test]
fn profile_save_assign_is_durable_and_exact_uuid_retry_never_reexecutes() {
    let fixture = fixture();
    save(&fixture, &["open-agent-conversation"]);
    let original = assign(&fixture, "tide");
    let receipt = commit(&fixture, original.clone()).unwrap();
    let before = std::fs::read(fixture.directory.path().join("owner.json")).unwrap();
    let retried = commit(&fixture, original.clone()).unwrap();
    assert_eq!(retried.binding, receipt.binding);
    assert_eq!(retried.generation, receipt.generation);
    assert_eq!(retried.aggregate_hash, receipt.aggregate_hash);
    assert_eq!(
        std::fs::read(fixture.directory.path().join("owner.json")).unwrap(),
        before
    );
    let mut changed = original;
    if let CommitOperation::WorkspacePermissionProfileCommand { command, .. } =
        &mut changed.operation
    {
        *command = Command::Assign {
            profile_name: "Exact Agent view".into(),
            subject: Subject::agent("another"),
            expiry: Expiry::Never,
        };
    }
    assert!(commit(&fixture, changed).is_err());
    let reopened = crate::open_file(&fixture.directory.path().join("owner.json")).unwrap();
    reopened
        .with_current(RoomId::parse(crate::testing::ROOM).unwrap(), |view| {
            assert_eq!(
                tilo_desktop_model::projection::workspace_permission_profiles_of(
                    view.peer().admitted_room().unwrap()
                ),
                vec![profile(&["open-agent-conversation"])]
            );
            let (_, _, stored) = tilo_peer_state::room::extract_governed_fields(
                view.peer().admitted_room().unwrap(),
            );
            assert!(
                stored
                    .iter()
                    .any(|cap| cap.subject == Subject::agent("tide")
                        && cap.scope
                            == Scope::Action {
                                app: "so.tilo.chat".into(),
                                action: "open-agent-conversation".into()
                            })
            );
            assert!(
                !stored
                    .iter()
                    .any(|cap| cap.subject == Subject::agent("another"))
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn shortened_profile_reassignment_removes_only_same_subject_assignment() {
    let fixture = fixture();
    save(&fixture, &["open-agent-conversation", "message"]);
    commit(&fixture, assign(&fixture, "tide")).unwrap();
    commit(&fixture, assign(&fixture, "other")).unwrap();
    let mut shortened = profile(&["open-agent-conversation"]);
    shortened.name = "EXACT AGENT VIEW".into();
    commit(
        &fixture,
        request(&fixture, Command::Save { profile: shortened }),
    )
    .unwrap();
    commit(&fixture, assign(&fixture, "tide")).unwrap();
    let stored = caps(&fixture);
    let selected = |agent: &str| {
        stored
            .iter()
            .filter(|cap| cap.subject == Subject::agent(agent))
            .collect::<Vec<_>>()
    };
    assert_eq!(selected("tide").len(), 1);
    assert_eq!(selected("other").len(), 2);
    assert!(selected("tide").iter().all(|cap| cap.scope
        != Scope::Action {
            app: "so.tilo.chat".into(),
            action: "message".into()
        }));
    assert!(
        stored
            .iter()
            .any(|cap| cap.id.starts_with("room-member-v1:"))
    );
}

#[test]
fn profile_retry_refuses_changed_caller_signer_and_foreign_room_without_mutation() {
    let fixture = fixture();
    save(&fixture, &["open-agent-conversation"]);
    let original = assign(&fixture, "tide");
    commit(&fixture, original.clone()).unwrap();
    let before = std::fs::read(fixture.directory.path().join("owner.json")).unwrap();
    let mut changed = original.clone();
    if let CommitOperation::WorkspacePermissionProfileCommand { caller, .. } =
        &mut changed.operation
    {
        caller.device_label.push_str(" changed");
    }
    assert!(commit(&fixture, changed).is_err());
    let mut foreign = original.clone();
    foreign.room = uuid::Uuid::new_v4().to_string();
    assert!(commit(&fixture, foreign).is_err());
    assert!(
        fixture
            .owner
            .commit_workspace_permission_profile_command(
                original,
                &fixture.certificate,
                &SigningKey::from_bytes(&[240; 32]),
                fixture.now
            )
            .is_err()
    );
    assert_eq!(
        std::fs::read(fixture.directory.path().join("owner.json")).unwrap(),
        before
    );
}

#[test]
fn expired_original_person_cannot_replay_a_saved_assignment() {
    let fixture = fixture_with_expiry(Some(std::time::Duration::from_secs(2)));
    save(&fixture, &["open-agent-conversation"]);
    let original = assign(&fixture, "tide");
    commit(&fixture, original.clone()).unwrap();
    let before = std::fs::read(fixture.directory.path().join("owner.json")).unwrap();
    assert!(
        fixture
            .owner
            .commit_workspace_permission_profile_command(
                original,
                &fixture.certificate,
                &fixture.key,
                fixture.now + std::time::Duration::from_secs(3)
            )
            .is_err()
    );
    assert_eq!(
        std::fs::read(fixture.directory.path().join("owner.json")).unwrap(),
        before
    );
}

fn update_original_device(fixture: &Fixture, revoke: bool) -> DeviceCert {
    let identity_key = SigningKey::from_bytes(&[91; 32]);
    let caller = crate::testing::certificate(
        &identity_key,
        &SigningKey::from_bytes(&[114; 32]),
        "Original Person",
    );
    let room = RoomId::parse(crate::testing::ROOM).unwrap();
    let messages = fixture
        .owner
        .with_current(room, |view| {
            let before = view.peer().clone();
            let mut peer = before.clone();
            if revoke {
                let mut entry = tilo_user_identity::RevocationEntry {
                    user_public_key: caller.user_public_key,
                    revoked_device_public_key: caller.device_public_key,
                    revoked_at_ns: crate::testing::ns(fixture.now).unwrap(),
                    reason: tilo_user_identity::RevocationReason::Retired,
                    signature: [0; 64],
                };
                tilo_user_identity::sign_revocation_entry(&mut entry, &identity_key).unwrap();
                peer.publish_governed_revocations(&[entry], &fixture.key, fixture.now)?;
            } else {
                peer.admit_certificate(caller.clone(), &fixture.key, fixture.now)?;
            }
            peer.durable_mutation_messages(
                &before,
                &fixture.key,
                fixture.now,
                view.generation().checked_add(1).unwrap(),
            )
        })
        .unwrap();
    for message in messages {
        fixture
            .owner
            .receive(
                room,
                tilo_peer_state::TransportOrigin::RetainedRelayDelta(
                    fixture.certificate.device_public_key,
                ),
                message,
                fixture.now,
            )
            .unwrap();
    }
    caller
}

#[test]
fn revoked_original_device_cannot_replay_or_create_a_profile_assignment() {
    let fixture = fixture();
    save(&fixture, &["open-agent-conversation"]);
    let caller = update_original_device(&fixture, false);
    let mut original = assign(&fixture, "tide");
    if let CommitOperation::WorkspacePermissionProfileCommand {
        caller: retained, ..
    } = &mut original.operation
    {
        *retained = caller;
    }
    commit(&fixture, original.clone()).unwrap();
    update_original_device(&fixture, true);
    let before = std::fs::read(fixture.directory.path().join("owner.json")).unwrap();
    assert!(commit(&fixture, original.clone()).is_err());
    if let CommitOperation::WorkspacePermissionProfileCommand { invocation, .. } =
        &mut original.operation
    {
        *invocation = uuid::Uuid::new_v4();
    }
    assert!(commit(&fixture, original).is_err());
    assert_eq!(
        std::fs::read(fixture.directory.path().join("owner.json")).unwrap(),
        before
    );
}
