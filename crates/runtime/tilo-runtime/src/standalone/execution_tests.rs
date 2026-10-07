use super::*;

fn caller(room: RoomId) -> Caller {
    Caller::authenticated([4; 32], [5; 32])
        .in_room(room.to_string())
        .in_new_execution()
}

#[test]
fn runtime_execution_fence_closes_retained_callers_synchronously_and_only_in_its_room() {
    let directory = tempfile::tempdir().unwrap();
    let owner = tilo_joined_room::open_file(&directory.path().join("rooms.json")).unwrap();
    let executions = ExecutionPins::new(&owner);
    let room = tilo_desktop_model::RoomId::parse("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
    let other_room =
        tilo_desktop_model::RoomId::parse("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb").unwrap();
    let a = caller(room);
    let b = caller(other_room);
    let first = executions.register_checked(room, &a, || Ok(())).unwrap();
    let second = executions
        .register_checked(other_room, &b, || Ok(()))
        .unwrap();
    owner.invalidate_identity(room);
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
    let owner = tilo_joined_room::open_file(&directory.path().join("rooms.json")).unwrap();
    let executions = ExecutionPins::new(&owner);
    let room = tilo_desktop_model::RoomId::parse("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
    let old = caller(room);
    let result = executions.register_checked(room, &old, || {
        owner.invalidate_identity(room);
        Ok(())
    });
    assert!(result.is_err());
    assert!(!old.execution_is_live());
    assert_eq!(executions.len(), 0);
    assert!(executions.register_checked(room, &old, || Ok(())).is_err());
}

#[test]
fn runtime_execution_registration_capacity_and_owner_drop_are_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let owner = tilo_joined_room::open_file(&directory.path().join("rooms.json")).unwrap();
    let executions = ExecutionPins::new(&owner);
    let room = tilo_desktop_model::RoomId::parse("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
    let pins = executions;
    let mut held = Vec::new();
    for _ in 0..MAX_EXECUTIONS {
        held.push(
            pins.register_checked(room, &caller(room), || Ok(()))
                .unwrap(),
        );
    }
    let excess = caller(room);
    assert!(pins.register_checked(room, &excess, || Ok(())).is_err());
    assert!(!excess.execution_is_live());
    assert_eq!(pins.len(), MAX_EXECUTIONS);
    let retained = held[0].caller.clone();
    drop(pins);
    assert!(
        !retained.execution_is_live(),
        "dropping the observer closes its retained runs"
    );
}
