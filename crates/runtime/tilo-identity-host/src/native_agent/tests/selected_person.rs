use super::*;

#[test]
fn actors_use_the_selected_person_not_the_default_or_a_generated_root() {
    let fixture = Fixture::new();
    let request = request();
    let first = provision(&fixture.room, &request).expect("default person's actor");
    let registry = IdentityRegistryStore::from_env().expect("registry");
    let second_dir = registry
        .identity_dir()
        .parent()
        .expect("parent")
        .join("second-person");
    let paths = crate::provisioning::collaboration_identity_paths();
    crate::provisioning::ensure_collaboration_identity(
        &second_dir,
        &paths,
        crate::provisioning::IdentityStorage::Files,
    )
    .expect("second real person on same native device");
    let second_registry = IdentityRegistryStore::at(&second_dir)
        .load_reconciled()
        .expect("second registry");
    let second = second_registry
        .identities
        .values()
        .next()
        .expect("second person");
    let second = registry
        .import(&second.record_path)
        .expect("register second person");
    DeviceFleetStore::from_env()
        .expect("fleet")
        .register_current_device(&second.id)
        .expect("enroll shared native device");
    let other_room = tilo_desktop_model::RoomId::new().to_string();
    registry
        .bind_workspace(&other_room, &second.id)
        .expect("explicit person selection");
    assert!(
        first.delegate_room(&other_room).is_err(),
        "a private room cannot switch person"
    );
    let other = provision(&other_room, &request).expect("selected second person's actor");
    assert_eq!(other.certificate().user_public_key, second.user_public_key);
    assert_ne!(other.certificate().user_public_key, fixture.user_public_key);
    assert_ne!(
        other.certificate().device_public_key,
        first.certificate().device_public_key
    );
    other
        .delegation()
        .verify(&second.user_public_key, tilo_clock::unix_now_ns())
        .expect("second person's real signature");
    assert!(
        other
            .delegation()
            .verify(&fixture.user_public_key, tilo_clock::unix_now_ns())
            .is_err()
    );
    registry
        .bind_workspace(&fixture.room, &second.id)
        .expect("switch original room's person");
    assert!(
        first.join_proof().is_err(),
        "retained first custody cannot follow selection"
    );
}
