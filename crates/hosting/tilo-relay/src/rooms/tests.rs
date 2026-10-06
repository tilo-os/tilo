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

#[cfg(feature = "agents")]
async fn view_read_relay(
    state_root: &Path,
    key: &SigningKey,
    cert: &DeviceCert,
) -> crate::daemon::Relay {
    crate::daemon::Relay::start_with_relay_mode(
        crate::RelayConfig {
            state_root: state_root.to_owned(),
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
    .unwrap()
}

#[cfg(feature = "agents")]
async fn assert_view_reader_refused(
    rooms: &Rooms,
    workspace: u64,
    caller: &tilo_actions::remote::Caller,
    source: &tilo_browser_peer::DocumentRoute,
) {
    assert!(
        rooms
            .resource_view_policy(workspace, caller, &source.app_id, &source.record)
            .await
            .is_none()
    );
    assert_eq!(
        rooms
            .resource_view_document(workspace, caller, source)
            .await
            .err()
            .unwrap(),
        "resource read not permitted",
        "a currently unauthorized caller must be refused before existing-key access"
    );
}

#[cfg(feature = "agents")]
async fn admit_view_reader(rooms: &Rooms, room: &str, key: &SigningKey, member: &DeviceCert) {
    let state = rooms.room(room).await.unwrap();
    let mut room = state.lock().await;
    let now = SystemTime::now();
    let mut candidate = room.peer().clone();
    candidate
        .admit_certificate(member.clone(), key, now)
        .unwrap();
    let collaboration = room.collaboration().clone();
    room.commit(candidate, key, now, |_, _| Ok(collaboration))
        .unwrap();
}

#[cfg(feature = "agents")]
#[tokio::test]
#[expect(
    clippy::significant_drop_tightening,
    reason = "Relay::shutdown consumes the relay after the policy assertions; the suggested subsequent drop would use a moved value"
)]
async fn view_read_policy_requires_current_certified_person_and_observes_revocation() {
    use tilo_actions::remote::Caller;
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = view_read_relay(dir.path(), &key, &cert).await;
    let rooms = relay.rooms().unwrap().clone();
    let created = rooms.create("Read-only resource discovery").await.unwrap();
    let member_key = SigningKey::from_bytes(&[3; 32]);
    let mut member = cert.clone();
    member.device_public_key = member_key.verifying_key().to_bytes();
    member.capabilities = DeviceCapabilities::member();
    sign_device_cert(&mut member, &SigningKey::from_bytes(&[1; 32])).unwrap();
    admit_view_reader(&rooms, &created.room, &key, &member).await;
    let caller = Caller::authenticated(member.user_public_key, member.device_public_key);
    let policy = rooms
        .resource_view_policy(created.workspace_id, &caller, "so.tilo.chat", "")
        .await
        .unwrap();
    assert_eq!(policy.room, created.room);
    assert_eq!(policy.role, collab_types::Role::Owner);
    let source = tilo_browser_peer::DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: created.room.clone(),
        record: "@tilo:package-primary".into(),
    };
    assert_eq!(
        rooms
            .resource_view_document(created.workspace_id, &caller, &source)
            .await
            .err()
            .unwrap(),
        "source stewardship unavailable",
        "the authorized reader reaches the existing-key boundary without creating stewardship"
    );
    assert_eq!(
        rooms
            .resource_view_document(0, &caller, &source)
            .await
            .err()
            .unwrap(),
        "source room mismatch"
    );
    for denied in [
        Caller::authenticated([9; 32], cert.device_public_key),
        Caller::authenticated(cert.user_public_key, [9; 32]),
        Caller::local_person(hex::encode(cert.user_public_key)),
        Caller::local_agent("agent-with-no-delegation"),
        Caller::local_app("so.tilo.chat"),
    ] {
        assert_view_reader_refused(&rooms, created.workspace_id, &denied, &source).await;
    }
    assert!(
        rooms
            .resource_view_policy(0, &caller, "so.tilo.chat", "")
            .await
            .is_none()
    );
    let identity_key = SigningKey::from_bytes(&[1; 32]);
    let mut revoked = tilo_user_identity::RevocationEntry {
        user_public_key: cert.user_public_key,
        revoked_device_public_key: member.device_public_key,
        revoked_at_ns: now_ns(),
        reason: tilo_user_identity::RevocationReason::Compromised,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut revoked, &identity_key).unwrap();
    {
        let queued_read = rooms.resource_view_document(created.workspace_id, &caller, &source);
        tokio::pin!(queued_read);
        let state = rooms.room(&created.room).await.unwrap();
        let mut room = state.lock().await;
        // Poll the actual reader to Pending while the governed room lock is held.
        // It must recheck admission after acquiring this same lock.
        assert!(
            std::future::poll_fn(|cx| {
                use std::future::Future;
                std::task::Poll::Ready(queued_read.as_mut().poll(cx).is_pending())
            })
            .await
        );
        let now = SystemTime::now();
        let mut candidate = room.peer().clone();
        candidate
            .publish_governed_revocations(&[revoked], &key, now)
            .unwrap();
        let collaboration = room.collaboration().clone();
        room.commit(candidate, &key, now, |_, _| Ok(collaboration))
            .unwrap();
        drop(room);
        drop(state);
        assert_eq!(
            queued_read.await.err().unwrap(),
            "resource read not permitted"
        );
    }
    assert_view_reader_refused(&rooms, created.workspace_id, &caller, &source).await;
    drop(rooms);
    relay.shutdown().await;
}

#[test]
fn a_created_room_restores_with_its_governance_and_publication() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let created = GovernedRoom::create(dir.path(), "Team", &cert, &key, SystemTime::now()).unwrap();
    assert!(!created.record.outgoing.is_empty());
    assert!(created.record.outgoing.iter().all(|entry| entry.checkpoint));
    let restored = GovernedRoom::open(created.path.clone(), &cert, &key).unwrap();
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
    assert!(GovernedRoom::open(created.path, &other, &key).is_err());
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
    let (relay, rooms) = {
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
        let rooms = relay.rooms().unwrap().clone();
        (relay, rooms)
    };
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
    drop(rooms);
    crate::daemon::Relay::shutdown(relay).await;
}
