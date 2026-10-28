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
fn a_session_without_rooms_has_no_start_route() {
    let dir = tempfile::tempdir().unwrap();
    let placements = RegistryPlacements::new(
        dir.path().join("home-peers.json"),
        Box::new(|_: &str| -> io::Result<RoomIdentity> { panic!("no room, no identity read") }),
    );
    assert!(configured(&[], &placements).unwrap().is_none());
}

/// A room the session created after it started.
const LATER_ROOM: &str = "5d1f6c0a-2b7e-4c8f-9a3d-8e2b1c4f6a77";
/// This device's key.
const LAPTOP: [u8; 32] = [3; 32];
/// The person's home relay.
const RELAY: [u8; 32] = [9; 32];

/// The person's home relay at `RELAY`, registered as retaining `rooms`, as
/// enrollment writes the registry at `path`.
fn register(path: &Path, user: [u8; 32], rooms: &[&str]) {
    let registration = HomePeerRegistration {
        schema_version: HomePeerRegistration::CURRENT_SCHEMA,
        device_node_id: hex::encode(RELAY),
        relay_service_node_id: hex::encode(RELAY),
        relay_endpoint: Some(iroh::EndpointAddr::new(
            iroh::SecretKey::from_bytes(&RELAY).public(),
        )),
        label: "home".into(),
        user_public_key: hex::encode(user),
        device_cert: None,
        relay_workspace_ids: rooms
            .iter()
            .map(|room| tilo_relay_protocol::room_workspace_id(room).unwrap())
            .collect(),
        environments: tilo_relay_protocol::RelayEnvironments::default(),
    };
    std::fs::write(path, serde_json::to_vec(&[registration]).unwrap()).unwrap();
}

/// Photos' make-thumbnails, invoked in the room the session got after it
/// started.
fn later_room_thumbnails() -> tilo_actions::offload::OffloadRequest {
    tilo_actions::offload::OffloadRequest {
        room: LATER_ROOM.into(),
        app_id: "so.tilo.photos".into(),
        address: tilo_actions::ActionAddress::new("photos.library:main", "make-thumbnails"),
        offload: tilo_actions::Offload {
            app: "so.tilo.thumbnails".into(),
            entrypoint: "thumbnail-batch".into(),
            gpu: false,
            cpu_time_ms: 1,
            memory_bytes: 1,
            output_bytes: 1,
        },
        inputs: Vec::new(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_room_created_after_start_is_routed_once_its_relay_is_registered_and_never_once_revoked()
{
    let dir = tempfile::tempdir().unwrap();
    let root = ed25519_dalek::SigningKey::from_bytes(&[5; 32]);
    let user = root.verifying_key().to_bytes();
    let identity = hex::encode(user);
    let identities = dir.path().join("identities");
    let system = dir.path().join("system");
    let registry = dir.path().join("home-peers.json");
    let placements = {
        let (identities, identity) = (identities.clone(), identity.clone());
        Arc::new(RegistryPlacements::new(
            registry.clone(),
            Box::new(move |_room: &str| -> io::Result<RoomIdentity> {
                Ok(RoomIdentity {
                    device_signing_key: ed25519_dalek::SigningKey::from_bytes(&LAPTOP),
                    device_cert: tilo_user_identity::DeviceCert {
                        user_public_key: user,
                        device_public_key: LAPTOP,
                        device_label: "laptop".into(),
                        capabilities: tilo_user_identity::DeviceCapabilities::primary(),
                        issued_at_ns: 0,
                        expires_at_ns: None,
                        signature: [0; 64],
                    },
                    identity_id: identity.clone(),
                    fleets: DeviceFleetStore::at(
                        &identities,
                        tilo_user_identity::IdentityPaths::under(system.clone()),
                    ),
                })
            }),
        ))
    };
    // Every route the session starts, by its relay; none dials anything.
    let started: Arc<Mutex<Vec<DeviceOffloadConfig>>> = Arc::default();
    let offload = {
        let started = Arc::clone(&started);
        let runtime = tokio::runtime::Handle::current();
        RoomOffload::new(
            Arc::clone(&placements) as Arc<dyn RoomPlacements>,
            Box::new(move |config: DeviceOffloadConfig| {
                started.lock_recover().push(config.clone());
                DeviceOffload::spawn(
                    config.trust(),
                    async { Err("the test relay is not dialled".to_owned()) },
                    &runtime,
                )
            }),
            tokio::runtime::Handle::current(),
        )
    };
    let request = later_room_thumbnails();
    let run_here = |answer: tilo_actions::offload::Offloaded| match answer {
        tilo_actions::offload::Offloaded::RunHere { reason } => reason,
        ran @ tilo_actions::offload::Offloaded::Ran { .. } => {
            panic!("nothing here runs elsewhere: {ran:?}")
        }
    };

    // The session started with no relay for the room: it runs here, and the
    // door is told why.
    let reason = run_here(offload.offload(request.clone()).await);
    assert_eq!(reason, "no home relay of this device retains the room");
    assert!(started.lock_recover().is_empty());

    // The person registers their home relay as retaining the room: the next
    // invocation starts the room's route to it, without a new session.
    register(&registry, user, &[LATER_ROOM]);
    let reason = run_here(offload.offload(request.clone()).await);
    assert!(reason.contains("the test relay is not dialled"), "{reason}");
    {
        let started = started.lock_recover().clone();
        assert_eq!(started.len(), 1);
        assert_eq!(started[0].relay.device, RELAY);
        assert_eq!(
            started[0].rooms,
            HashMap::from([(
                LATER_ROOM.to_owned(),
                tilo_relay_protocol::room_workspace_id(LATER_ROOM).unwrap()
            )])
        );
    }

    // The person revokes the relay in their device fleet: the room's
    // actions run here again, with why, and nothing more is started.
    let mut fleet = DeviceFleet::new(identity.clone());
    fleet.revocations = vec![revocation(&root, RELAY)];
    std::fs::create_dir_all(identities.join("fleets")).unwrap();
    std::fs::write(
        identities.join("fleets").join(format!("{identity}.json")),
        serde_json::to_vec(&fleet).unwrap(),
    )
    .unwrap();
    let reason = run_here(offload.offload(request).await);
    assert!(reason.contains("has been revoked"), "{reason}");
    assert_eq!(
        started.lock_recover().len(),
        1,
        "no route to a revoked relay"
    );
    assert_eq!(offload.routes(), 0);
}

#[test]
fn the_registry_is_read_again_when_it_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("home-peers.json");
    let placements = RegistryPlacements::new(
        path.clone(),
        Box::new(|_: &str| -> io::Result<RoomIdentity> {
            Err(io::Error::other("reading the registry reads no identity"))
        }),
    );
    assert!(
        placements.registrations().unwrap().is_empty(),
        "no registry"
    );
    register(&path, [7; 32], &[LATER_ROOM]);
    assert_eq!(placements.registrations().unwrap().len(), 1);
    let read = placements.registrations().unwrap();
    assert!(
        Arc::ptr_eq(&read, &placements.registrations().unwrap()),
        "served from memory while unchanged"
    );
    std::fs::write(&path, b"{ not a registry").unwrap();
    assert!(
        placements.registrations().is_err(),
        "a changed, invalid registry is refused rather than served stale"
    );
}

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
