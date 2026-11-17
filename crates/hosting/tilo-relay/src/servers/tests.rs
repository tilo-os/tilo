use super::*;

const WORKSPACE: &str = "0b7e7dfa-3ad5-4b43-9d6c-5f0d7c1f6a11";

fn device() -> Id {
    device_id(&[7; 32])
}

fn enrolled() -> Replica<Servers> {
    let mut record = Replica::<Servers>::new();
    record
        .apply(Servers::ENROLL_SERVER.with((device().as_str().to_owned(), "Attic".into())))
        .unwrap();
    record
}

#[test]
fn a_servers_configuration_is_typed_actions_with_history() {
    let mut record = enrolled();
    record
        .apply(Servers::HOLD_WORKSPACE.with((device(), WORKSPACE.into(), PARTICIPANT.into())))
        .unwrap();
    record
        .apply(Servers::SET_APPETITE.with((device(), 500, true, 2)))
        .unwrap();
    record
        .apply(Servers::SET_UPDATE_CHANNEL.with((device(), "stable".into())))
        .unwrap();
    record
        .apply(Servers::SET_MAINTENANCE_WINDOW.with((device(), 180, 60)))
        .unwrap();
    record
        .apply(Servers::SET_UNLOCK_POLICY.with((device(), "tpm".into())))
        .unwrap();
    let state = record.state().unwrap();
    let server = state.active(&[7; 32]).unwrap();
    assert_eq!(server.label, "Attic");
    assert_eq!(server.holding(WORKSPACE), Some(PARTICIPANT));
    assert_eq!(
        (server.storage_gb, server.compute, server.machines),
        (500, true, 2)
    );
    assert_eq!(server.update_channel, "stable");
    assert_eq!(
        (server.maintenance_start, server.maintenance_minutes),
        (180, 60)
    );
    assert_eq!(server.unlock, "tpm");
    assert_eq!(record.log().len(), 6, "each change is one entry of history");

    record
        .apply(Servers::HOLD_WORKSPACE.with((device(), WORKSPACE.into(), STORE.into())))
        .unwrap();
    assert_eq!(
        record
            .state()
            .unwrap()
            .active(&[7; 32])
            .unwrap()
            .holding(WORKSPACE),
        Some(STORE)
    );
    record
        .apply(Servers::RELEASE_WORKSPACE.with((device(), WORKSPACE.into())))
        .unwrap();
    assert_eq!(
        record
            .state()
            .unwrap()
            .active(&[7; 32])
            .unwrap()
            .holding(WORKSPACE),
        None
    );
}

#[test]
fn a_change_the_record_cannot_mean_is_refused() {
    let mut record = enrolled();
    for refused in [
        Servers::HOLD_WORKSPACE.with((device(), "not-a-workspace".into(), STORE.into())),
        Servers::HOLD_WORKSPACE.with((device(), WORKSPACE.into(), "owner".into())),
        Servers::SET_UPDATE_CHANNEL.with((device(), "nightly-ish".into())),
        Servers::SET_MAINTENANCE_WINDOW.with((device(), 24 * 60, 10)),
        Servers::SET_UNLOCK_POLICY.with((device(), "none".into())),
        Servers::RENAME_SERVER.with((device(), " ".into())),
        Servers::RENAME_SERVER.with((device_id(&[8; 32]), "Other".into())),
        Servers::ENROLL_SERVER.with(("attic".into(), "Attic".into())),
    ] {
        assert!(record.apply(refused).is_err());
    }
    assert_eq!(record.log().len(), 1, "nothing refused is history");
}

#[test]
fn a_retired_server_holds_nothing_and_is_not_taken_in_again() {
    let mut record = enrolled();
    record
        .apply(Servers::HOLD_WORKSPACE.with((device(), WORKSPACE.into(), STORE.into())))
        .unwrap();
    record.apply(Servers::RETIRE_SERVER.with(device())).unwrap();
    assert!(record.state().unwrap().active(&[7; 32]).is_none());
    record
        .apply(Servers::ENROLL_SERVER.with((device().as_str().to_owned(), "Attic again".into())))
        .unwrap();
    assert!(record.state().unwrap().active(&[7; 32]).is_none());
    assert!(
        record
            .apply(Servers::HOLD_WORKSPACE.with((device(), WORKSPACE.into(), STORE.into())))
            .is_err()
    );
}

#[test]
fn each_server_of_a_person_finds_the_same_admin_workspace() {
    let ana = admin_workspace_id(&[1; 32]);
    assert_eq!(ana, admin_workspace_id(&[1; 32]));
    assert_ne!(ana, admin_workspace_id(&[2; 32]));
    assert!(uuid::Uuid::parse_str(&ana).is_ok());
}
