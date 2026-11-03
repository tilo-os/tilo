use super::*;

fn caller(workspace: WorkspaceId) -> Caller {
    Caller::authenticated([4; 32], [5; 32])
        .in_workspace(workspace.to_string())
        .in_new_execution()
}

#[test]
fn runtime_execution_fence_closes_retained_callers_synchronously_and_only_in_its_workspace() {
    let directory = tempfile::tempdir().unwrap();
    let owner =
        tilo_peer_state::owner::open_file(&directory.path().join("workspaces.json")).unwrap();
    let executions = ExecutionPins::new(&owner);
    let workspace =
        tilo_desktop_model::WorkspaceId::parse("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
    let other_workspace =
        tilo_desktop_model::WorkspaceId::parse("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb").unwrap();
    let a = caller(workspace);
    let b = caller(other_workspace);
    let first = executions
        .register_checked(workspace, &a, || Ok(()))
        .unwrap();
    let second = executions
        .register_checked(other_workspace, &b, || Ok(()))
        .unwrap();
    owner.invalidate_identity(workspace);
    assert!(
        !a.execution_is_live(),
        "the actual owner callback has returned, so old custody must already be closed"
    );
    assert!(b.execution_is_live());
    assert!(!first.caller.execution_is_live());
    drop(first);
    assert_eq!(executions.len(), 1);
    drop(second);
    assert!(!b.execution_is_live());
    assert_eq!(executions.len(), 0);
}

#[test]
fn runtime_execution_subscription_precedes_recheck_and_failure_cannot_refresh_the_lease() {
    let directory = tempfile::tempdir().unwrap();
    let owner =
        tilo_peer_state::owner::open_file(&directory.path().join("workspaces.json")).unwrap();
    let executions = ExecutionPins::new(&owner);
    let workspace =
        tilo_desktop_model::WorkspaceId::parse("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
    let old = caller(workspace);
    let result = executions.register_checked(workspace, &old, || {
        owner.invalidate_identity(workspace);
        Ok(())
    });
    assert!(result.is_err());
    assert!(!old.execution_is_live());
    assert_eq!(executions.len(), 0);
    assert!(
        executions
            .register_checked(workspace, &old, || Ok(()))
            .is_err()
    );
}

#[test]
fn runtime_execution_registration_capacity_and_owner_drop_are_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let owner =
        tilo_peer_state::owner::open_file(&directory.path().join("workspaces.json")).unwrap();
    let executions = ExecutionPins::new(&owner);
    let workspace =
        tilo_desktop_model::WorkspaceId::parse("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
    let pins = executions;
    let mut held = Vec::new();
    for _ in 0..MAX_EXECUTIONS {
        held.push(
            pins.register_checked(workspace, &caller(workspace), || Ok(()))
                .unwrap(),
        );
    }
    let excess = caller(workspace);
    assert!(
        pins.register_checked(workspace, &excess, || Ok(()))
            .is_err()
    );
    assert!(!excess.execution_is_live());
    assert_eq!(pins.len(), MAX_EXECUTIONS);
    let retained = held[0].caller.clone();
    drop(pins);
    assert!(
        !retained.execution_is_live(),
        "dropping the observer closes its retained runs"
    );
}
