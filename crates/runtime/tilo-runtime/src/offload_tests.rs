use super::*;

#[test]
fn offload_is_turned_off_only_by_an_explicit_off() {
    for off in ["off", "0", "false", "no", " OFF "] {
        assert!(disabled(Some(off)), "{off:?}");
    }
    for on in [None, Some(""), Some("on"), Some("1")] {
        assert!(!disabled(on), "{on:?}");
    }
}

#[test]
fn a_device_without_a_registry_has_no_relay_to_offload_to() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        read_registry(&dir.path().join("home-peers.json"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn an_unreadable_registry_is_refused_rather_than_read_as_empty() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("home-peers.json");
    std::fs::write(&path, b"{ not a registry").unwrap();
    let error = read_registry(&path).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    let oversized = usize::try_from(MAX_REGISTRY_BYTES)
        .unwrap()
        .saturating_add(1);
    std::fs::write(&path, vec![b' '; oversized]).unwrap();
    let error = read_registry(&path).unwrap_err();
    assert!(error.to_string().contains("bound"), "{error}");
}

#[test]
fn a_session_without_rooms_has_no_route() {
    assert!(configured(&[]).unwrap().is_none());
}

/// `root`'s signed revocation of `device`.
fn revocation(
    root: &ed25519_dalek::SigningKey,
    device: [u8; 32],
) -> tilo_user_identity::RevocationEntry {
    let mut entry = tilo_user_identity::RevocationEntry {
        user_public_key: root.verifying_key().to_bytes(),
        revoked_device_public_key: device,
        revoked_at_ns: 1,
        reason: tilo_user_identity::RevocationReason::Stolen,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, root).unwrap();
    entry
}

#[test]
fn the_route_reads_the_device_fleet_and_sees_a_revocation_the_moment_it_lands() {
    let dir = tempfile::tempdir().unwrap();
    let root = ed25519_dalek::SigningKey::from_bytes(&[5; 32]);
    let user = root.verifying_key().to_bytes();
    let identity = hex::encode(user);
    let identities = dir.path().join("identities");
    let store = DeviceFleetStore::at(
        &identities,
        tilo_user_identity::IdentityPaths::under(dir.path().join("system")),
    );
    let revocations = FleetRevocations::new(store, identity.clone(), user);
    assert!(
        revocations.current().unwrap().is_empty(),
        "a person who revoked nothing"
    );
    let write = |entries: Vec<tilo_user_identity::RevocationEntry>| {
        let mut fleet = DeviceFleet::new(identity.clone());
        fleet.revocations = entries;
        std::fs::create_dir_all(identities.join("fleets")).unwrap();
        std::fs::write(
            identities.join("fleets").join(format!("{identity}.json")),
            serde_json::to_vec(&fleet).unwrap(),
        )
        .unwrap();
    };
    // The person revokes their relay: the very next read has it.
    let relay = [9; 32];
    write(vec![revocation(&root, relay)]);
    assert!(revocations.current().unwrap().is_revoked(&relay));
    assert!(
        revocations.current().unwrap().is_revoked(&relay),
        "and keeps it"
    );
    // A revocation another identity signed is no revocation of this
    // person's: the fleet is not trusted at all, and the route trusts no one.
    let stranger = ed25519_dalek::SigningKey::from_bytes(&[6; 32]);
    write(vec![
        revocation(&root, relay),
        revocation(&stranger, [4; 32]),
    ]);
    let error = revocations.current().unwrap_err();
    assert!(error.contains("another identity"), "{error}");
    std::fs::write(
        identities.join("fleets").join(format!("{identity}.json")),
        b"{ not a fleet",
    )
    .unwrap();
    assert!(revocations.current().is_err(), "an unreadable fleet");
}
