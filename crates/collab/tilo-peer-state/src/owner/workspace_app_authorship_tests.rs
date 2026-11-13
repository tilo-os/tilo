use crate::owner::fixture::fixture;
use crate::owner::{WorkspaceId, testing};
use crate::owner_commit::{CommitOperation, OwnerCommitRequest};
use crate::workspace_membership::WorkspaceMembershipCommand;

#[tokio::test]
async fn app_authorship_is_governor_signed_durable_and_exact_retry_only() {
    let fixture = fixture();
    let request = OwnerCommitRequest {
        workspace: testing::WORKSPACE.into(),
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
        .commit_workspace_membership_command(request.clone(), &fixture.key, fixture.now)
        .await
        .unwrap();
    let retry = fixture
        .owner
        .commit_workspace_membership_command(request.clone(), &fixture.key, fixture.now)
        .await
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
            .commit_workspace_membership_command(changed, &fixture.key, fixture.now,)
            .await
            .is_err()
    );
    let reopened = crate::owner::open_file(&fixture.directory.path().join("owner.json")).unwrap();
    reopened
        .with_current(WorkspaceId::parse(testing::WORKSPACE).unwrap(), |view| {
            let now = std::time::SystemTime::now();
            let policy = view
                .peer()
                .permissions(fixture.certificate.device_public_key, now)
                .map_err(|error| error.to_string())?
                .policy;
            let cx = tilo_action_policy::Context::at_time(now);
            assert!(policy.app_authors("so.tilo.chat", &cx));
            assert!(!policy.app_authors("so.tilo.files", &cx));
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn app_authorship_refuses_forged_person_and_invalid_package() {
    let fixture = fixture();
    for (mut caller, app) in [
        (fixture.certificate.clone(), "so.tilo.chat"),
        (fixture.certificate.clone(), "../so.tilo.chat"),
    ] {
        if app == "so.tilo.chat" {
            caller.user_public_key = [7; 32];
        }
        let request = OwnerCommitRequest {
            workspace: testing::WORKSPACE.into(),
            operation: CommitOperation::WorkspaceMembershipCommand {
                invocation: uuid::Uuid::new_v4(),
                caller,
                command: WorkspaceMembershipCommand::GrantAppAuthorship { app: app.into() },
            },
        };
        assert!(
            fixture
                .owner
                .commit_workspace_membership_command(request, &fixture.key, fixture.now,)
                .await
                .is_err()
        );
    }
}

/// The workspace's owner holds it structurally: a grant naming them, even one
/// they signed themselves, never takes their authority over the workspace away.
#[tokio::test]
async fn the_owners_own_grants_never_restrict_their_app_delegation() {
    let fixture = fixture();
    let workspace = WorkspaceId::parse(testing::WORKSPACE).unwrap();
    let messages = fixture
        .owner
        .with_current(workspace, |view| {
            let before = view.peer().clone();
            let person = before
                .permissions(fixture.certificate.device_public_key, fixture.now)
                .map_err(|error| error.to_string())?
                .person;
            let mut candidate = before.clone();
            candidate.governance_mut().put_governed_grants(
                &[tilo_action_policy::Grant::new(
                    "app-open-read-only-person",
                    tilo_action_policy::Grantee::person(person.clone()),
                    tilo_action_policy::TreePath::workspace(testing::WORKSPACE),
                    tilo_action_policy::KindSet::UNDOABLE
                        .union(tilo_action_policy::KindSet::OUTSIDE),
                    tilo_action_policy::Answer::Deny,
                    tilo_action_policy::Grantee::person(person),
                )],
                &fixture.key,
                fixture.now,
            )?;
            candidate.publication_mut().durable_mutation_messages(
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
                workspace,
                crate::TransportOrigin::RetainedRelayDelta(fixture.certificate.device_public_key),
                message,
                fixture.now,
            )
            .unwrap();
    }
    fixture
        .owner
        .with_current(workspace, |view| {
            assert!(
                view.peer()
                    .workspace()
                    .check_workspace_administration(
                        fixture.certificate.device_public_key,
                        fixture.now
                    )
                    .is_ok()
            );
            Ok(())
        })
        .unwrap();
    let request = OwnerCommitRequest {
        workspace: testing::WORKSPACE.into(),
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
            .commit_workspace_membership_command(request, &fixture.key, fixture.now,)
            .await
            .is_ok()
    );
}

/// Letting one app use another app's action is one governance change, signed
/// through the owner's turn: the grant it drafts is the same on every attempt, so
/// the turn that signs it settles and the grant lands — once, however often the
/// same change is committed.
#[tokio::test]
async fn an_app_action_grant_is_owner_signed_and_the_same_grant_however_often_given() {
    use crate::workspace_membership::{AppAction, SettingsChange};
    use tilo_action_policy::Grantee;

    let fixture = fixture();
    let named = AppAction::new(
        "so.tilo.lang.today",
        "so.tilo.lang.notes",
        "tl:so.tilo.lang.notes:root",
        "file-note",
    )
    .expect("an app's action, named");
    let request = OwnerCommitRequest {
        workspace: testing::WORKSPACE.into(),
        operation: CommitOperation::WorkspaceMembershipCommand {
            invocation: uuid::Uuid::new_v4(),
            caller: fixture.certificate.clone(),
            command: WorkspaceMembershipCommand::Settings(SettingsChange::GrantAppAction(
                named.clone(),
            )),
        },
    };
    fixture
        .owner
        .commit_workspace_membership_command(request.clone(), &fixture.key, fixture.now)
        .await
        .expect("the workspace's owner lets Today file its note");
    let everyday = |owner: &crate::owner::NativeWorkspaceOwner| {
        owner
            .with_current(WorkspaceId::parse(testing::WORKSPACE).unwrap(), |view| {
                let now = std::time::SystemTime::now();
                let policy = view
                    .peer()
                    .permissions(fixture.certificate.device_public_key, now)
                    .map_err(|error| error.to_string())?
                    .policy;
                Ok(policy
                    .grants
                    .into_iter()
                    .filter(|grant| grant.grantee == Grantee::app("so.tilo.lang.today"))
                    .collect::<Vec<_>>())
            })
            .unwrap()
    };
    let once = everyday(&fixture.owner);
    let [granted] = once.as_slice() else {
        panic!("one grant: {once:?}");
    };
    assert_eq!(granted.id, named.grant_id());
    fixture
        .owner
        .commit_workspace_membership_command(request, &fixture.key, fixture.now)
        .await
        .expect("the same change again");
    let reopened = crate::owner::open_file(&fixture.directory.path().join("owner.json")).unwrap();
    assert_eq!(
        everyday(&reopened).len(),
        1,
        "the grant the owner signed is the one it keeps"
    );
}
