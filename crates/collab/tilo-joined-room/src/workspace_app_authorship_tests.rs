use crate::application_record::tests::fixture;
use crate::{RoomId, testing};
use collab_types::caps::{Effect, Scope, Subject, Verb};
use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};
use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand;

#[test]
fn app_authorship_is_governor_signed_durable_and_exact_retry_only() {
    let fixture = fixture();
    let request = OwnerCommitRequest {
        room: testing::ROOM.into(),
        operation: CommitOperation::WorkspaceMembershipCommand {
            invocation: uuid::Uuid::new_v4(),
            caller: fixture.certificate.clone(),
            command: WorkspaceMembershipCommand::GrantAppAuthorship {
                app: "so.tilo.chat".into(),
            },
        },
    };
    let receipt = fixture
        .owner
        .commit_workspace_membership_command(
            request.clone(),
            &fixture.certificate,
            &fixture.key,
            fixture.now,
        )
        .unwrap();
    let retry = fixture
        .owner
        .commit_workspace_membership_command(
            request.clone(),
            &fixture.certificate,
            &fixture.key,
            fixture.now,
        )
        .unwrap();
    assert_eq!(receipt.binding, retry.binding);
    assert_eq!(receipt.generation, retry.generation);
    assert_eq!(receipt.aggregate_hash, retry.aggregate_hash);
    let mut changed = request;
    if let CommitOperation::WorkspaceMembershipCommand { command, .. } = &mut changed.operation {
        *command = WorkspaceMembershipCommand::GrantAppAuthorship {
            app: "so.tilo.files".into(),
        };
    }
    assert!(
        fixture
            .owner
            .commit_workspace_membership_command(
                changed,
                &fixture.certificate,
                &fixture.key,
                fixture.now,
            )
            .is_err()
    );
    let reopened = crate::open_file(&fixture.directory.path().join("owner.json")).unwrap();
    reopened
        .with_current(RoomId::parse(testing::ROOM).unwrap(), |view| {
            let (_, caps, _) = view
                .peer()
                .permissions(fixture.certificate.device_public_key, fixture.now)?;
            assert!(
                caps.iter()
                    .any(|cap| cap.subject == Subject::app("so.tilo.chat")
                        && cap.scope == Scope::Workspace
                        && cap.verb == Verb::Write
                        && cap.effect == Effect::Grant)
            );
            assert!(
                !caps
                    .iter()
                    .any(|cap| cap.subject == Subject::app("so.tilo.files"))
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn app_authorship_refuses_forged_person_and_invalid_package() {
    let fixture = fixture();
    for (mut caller, app) in [
        (fixture.certificate.clone(), "so.tilo.chat"),
        (fixture.certificate.clone(), "../so.tilo.chat"),
    ] {
        if app == "so.tilo.chat" {
            caller.user_public_key = [7; 32];
        }
        let request = OwnerCommitRequest {
            room: testing::ROOM.into(),
            operation: CommitOperation::WorkspaceMembershipCommand {
                invocation: uuid::Uuid::new_v4(),
                caller,
                command: WorkspaceMembershipCommand::GrantAppAuthorship { app: app.into() },
            },
        };
        assert!(
            fixture
                .owner
                .commit_workspace_membership_command(
                    request,
                    &fixture.certificate,
                    &fixture.key,
                    fixture.now,
                )
                .is_err()
        );
    }
}

#[test]
fn signed_current_admin_restriction_refuses_direct_owner_app_delegation() {
    let fixture = fixture();
    let room = RoomId::parse(testing::ROOM).unwrap();
    let messages = fixture
        .owner
        .with_current(room, |view| {
            let before = view.peer().clone();
            let (_, _, person) =
                before.permissions(fixture.certificate.device_public_key, fixture.now)?;
            let mut candidate = before.clone();
            candidate.put_governed_capabilities(
                &[collab_types::caps::CapOverride::new(
                    Subject::person(person),
                    Scope::Workspace,
                    Effect::Restrict,
                    Verb::Read,
                )
                .with_id("app-open-read-only-person")],
                &fixture.key,
                fixture.now,
            )?;
            candidate.durable_mutation_messages(
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
    fixture
        .owner
        .with_current(room, |view| {
            assert!(
                view.peer()
                    .check_workspace_administration(
                        fixture.certificate.device_public_key,
                        fixture.now
                    )
                    .is_err()
            );
            Ok(())
        })
        .unwrap();
    let request = OwnerCommitRequest {
        room: testing::ROOM.into(),
        operation: CommitOperation::WorkspaceMembershipCommand {
            invocation: uuid::Uuid::new_v4(),
            caller: fixture.certificate.clone(),
            command: WorkspaceMembershipCommand::GrantAppAuthorship {
                app: "so.tilo.chat".into(),
            },
        },
    };
    assert!(
        fixture
            .owner
            .commit_workspace_membership_command(
                request,
                &fixture.certificate,
                &fixture.key,
                fixture.now,
            )
            .is_err()
    );
}
