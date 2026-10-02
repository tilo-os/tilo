use super::*;
use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

fn governor() -> (SigningKey, DeviceCert) {
    let root = SigningKey::from_bytes(&[1; 32]);
    let key = SigningKey::from_bytes(&[2; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "home".into(),
        capabilities: DeviceCapabilities::host(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &root).unwrap();
    (key, cert)
}

#[test]
fn a_created_room_restores_with_its_governance_and_publication() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let created = GovernedRoom::create(dir.path(), "Team", &cert, &key, SystemTime::now()).unwrap();
    assert!(!created.record.outgoing.is_empty());
    assert!(created.record.outgoing.iter().all(|entry| entry.checkpoint));
    let restored = GovernedRoom::open(created.path.clone(), &cert).unwrap();
    assert_eq!(restored.record.room, created.record.room);
    assert_eq!(restored.record.generation, 1);
    assert_eq!(restored.summary().name, "Team");
    assert!(restored.peer().has_governance());
}

#[test]
fn a_room_is_only_restored_by_its_governor() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let created = GovernedRoom::create(dir.path(), "Team", &cert, &key, SystemTime::now()).unwrap();
    let mut other = cert;
    other.device_label = "someone else".into();
    assert!(GovernedRoom::open(created.path, &other).is_err());
}

#[test]
fn a_workspace_name_must_be_printable_and_short() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    for name in ["", "   ", "a\u{7}b", &"x".repeat(81)] {
        assert!(GovernedRoom::create(dir.path(), name, &cert, &key, SystemTime::now()).is_err());
    }
}

#[test]
fn a_long_tail_compacts_into_a_fresh_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let mut room =
        GovernedRoom::create(dir.path(), "Team", &cert, &key, SystemTime::now()).unwrap();
    room.record.published = room.record.outgoing.len();
    for index in 0..=MAX_PUBLICATION_TAIL {
        room.record.outgoing.push(Publication {
            bytes: vec![u8::try_from(index % 200).unwrap()],
            checkpoint: false,
        });
    }
    let candidate = room.peer().clone();
    let state = room.collaboration().clone();
    room.commit(candidate, &key, SystemTime::now(), |_, _| Ok(state))
        .unwrap();
    assert!(room.record.outgoing.iter().all(|entry| entry.checkpoint));
    assert_eq!(
        room.record.published, 0,
        "the fresh checkpoint is republished"
    );
    assert_eq!(room.record.generation, 2);
}

#[tokio::test]
async fn legacy_window_write_respects_current_owner_capability_restrictions() {
    use collab_types::Role;
    use collab_types::caps::{CapOverride, Effect, Scope, Subject, Verb};
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = crate::daemon::Relay::start_with_relay_mode(
        crate::RelayConfig {
            state_root: dir.path().to_owned(),
            ..Default::default()
        },
        crate::daemon::RelayIdentity {
            user_public_key: cert.user_public_key,
            device_signing_key: key.clone(),
            device_cert: cert.clone(),
            revocations: RevocationList::new(),
        },
        iroh::RelayMode::Disabled,
    )
    .await
    .unwrap();
    let rooms = relay.rooms().unwrap();
    let created = rooms.create("Legacy write restriction").await.unwrap();
    assert_eq!(
        rooms
            .legacy_writer_role(&created.room, cert.device_public_key)
            .await,
        Some(Role::Owner)
    );
    let mut restriction = CapOverride::new(
        Subject::Role(Role::Owner),
        Scope::Room(created.room.clone()),
        Effect::Restrict,
        Verb::Read,
    );
    restriction.id = "legacy-window-read-only-owner".into();
    restriction = restriction.with_expiry(
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .checked_add(5)
            .unwrap(),
    );
    let state = rooms.room(&created.room).await.unwrap();
    {
        let mut room = state.lock().await;
        let now = SystemTime::now();
        let mut candidate = room.peer().clone();
        candidate
            .put_governed_capabilities(&[restriction], &key, now)
            .unwrap();
        let collaboration = room.collaboration().clone();
        room.commit(candidate, &key, now, |_, _| Ok(collaboration))
            .unwrap();
    }
    assert_eq!(
        rooms
            .legacy_writer_role(&created.room, cert.device_public_key)
            .await,
        None,
        "room ownership must not bypass a current read-only capability restriction"
    );
    // The restricted owner cannot rewrite governance to remove its own cap.
    // Expiration is an authorized transition and must be observed without restart.
    tilo_test_env::within("the current read-only restriction expires", async {
        loop {
            if rooms
                .legacy_writer_role(&created.room, cert.device_public_key)
                .await
                == Some(Role::Owner)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    crate::daemon::Relay::shutdown(relay).await;
}
