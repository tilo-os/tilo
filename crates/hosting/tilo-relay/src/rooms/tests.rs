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
    assert_eq!(
        policy.standing.policy.role_of(&policy.standing.person),
        Some(collab_types::Role::Owner)
    );
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
        revoked_at_ns: tilo_clock::unix_now_ns(),
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

#[cfg(feature = "agents")]
#[tokio::test]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the relay stays live through retained publication checks and its consuming shutdown"
)]
async fn governor_application_authoring_survives_publication_tail_rollover() {
    use automerge::{ROOT, ReadDoc, transaction::Transactable};
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = view_read_relay(scratch.path(), &key, &cert).await;
    let rooms = relay.rooms().unwrap();
    let created = rooms.create("Native authoring rollover").await.unwrap();
    let catalogue = tilo_agent_participant::AppCatalogue::native();
    rooms
        .prepare_application(&created.room, &cert, &catalogue, "so.tilo.chat")
        .await
        .unwrap();
    let source = tilo_browser_peer::DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: created.room.clone(),
        record: String::new(),
    };
    let caller =
        tilo_actions::remote::Caller::authenticated(cert.user_public_key, cert.device_public_key);
    for index in 1..=MAX_PUBLICATION_TAIL {
        let mut request_id = [0; 32];
        request_id[..8].copy_from_slice(&u64::try_from(index).unwrap().to_le_bytes());
        rooms
            .grant_app_authorship(
                &created.room,
                &cert,
                GrantAppAuthorshipRequest {
                    request_id,
                    app: format!("so.tilo.rollover-fixture-{index}"),
                },
            )
            .await
            .unwrap();
    }
    let current = rooms.room(&created.room).await.unwrap();
    let tail = current
        .lock()
        .await
        .record
        .outgoing
        .iter()
        .rev()
        .take_while(|publication| !publication.checkpoint)
        .count();
    assert!(
        tail >= MAX_PUBLICATION_TAIL,
        "real governance writes reach rollover"
    );
    rooms
        .author_application(&created.room, &cert, &catalogue, "so.tilo.chat", |doc| {
            doc.put(ROOT, "rollover_counter", 1_u64)
                .map_err(|error| error.to_string())
        })
        .await
        .unwrap();
    let retained = rooms
        .resource_view_document(created.workspace_id, &caller, &source)
        .await
        .expect("the mutation crossing rollover must be immediately replayable");
    let doc = automerge::AutoCommit::load(&retained.bytes).unwrap();
    assert_eq!(
        doc.get(ROOT, "rollover_counter")
            .unwrap()
            .map(|(value, _)| value.as_u64()),
        Some(Some(1)),
        "rollover must publish the new governor mutation into the retained source"
    );
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
#[allow(
    clippy::too_many_lines,
    reason = "one window write decided as the room's grants change keeps its fixture visible"
)]
async fn legacy_window_write_respects_current_member_grants() {
    use collab_types::Role;
    use tilo_action_policy::{Answer, Grant, KindSet, Lifetime, Subject};
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
    // The owner's own device on an operating system Tilo does not control:
    // held to the foreign-device bound, which the room may narrow.
    let owner_root = SigningKey::from_bytes(&[1; 32]);
    let foreign_key = SigningKey::from_bytes(&[6; 32]);
    let mut capabilities = DeviceCapabilities::member();
    capabilities.foreign = true;
    let mut foreign = DeviceCert {
        user_public_key: owner_root.verifying_key().to_bytes(),
        device_public_key: foreign_key.verifying_key().to_bytes(),
        device_label: "borrowed laptop".into(),
        capabilities,
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut foreign, &owner_root).unwrap();
    let state = rooms.room(&created.room).await.unwrap();
    let deadline = tilo_clock::try_unix_now().unwrap().checked_add(5).unwrap();
    {
        let mut room = state.lock().await;
        let now = SystemTime::now();
        let mut candidate = room.peer().clone();
        candidate
            .admit_certificate(foreign.clone(), &key, now)
            .unwrap();
        let collaboration = room.collaboration().clone();
        room.commit(candidate, &key, now, |_, _| Ok(collaboration))
            .unwrap();
    }
    assert_eq!(
        rooms
            .legacy_writer_role(&created.room, foreign.device_public_key)
            .await,
        Some(Role::Owner),
        "a foreign device changes the room's content"
    );
    {
        let mut room = state.lock().await;
        let now = SystemTime::now();
        let mut candidate = room.peer().clone();
        let read_only = Grant::new(
            "legacy-window-read-only-foreign",
            Subject::ForeignDevice,
            tilo_action_policy::TreePath::room(&created.room),
            KindSet::UNDOABLE,
            Answer::Deny,
            Subject::Person(hex::encode(cert.user_public_key)),
        )
        .with_lifetime(Lifetime::Until(deadline.saturating_mul(1000)));
        candidate
            .put_governed_grants(&[read_only], &key, now)
            .unwrap();
        let collaboration = room.collaboration().clone();
        room.commit(candidate, &key, now, |_, _| Ok(collaboration))
            .unwrap();
    }
    assert_eq!(
        rooms
            .legacy_writer_role(&created.room, foreign.device_public_key)
            .await,
        None,
        "ownership must not bypass a current grant denying changes from foreign devices"
    );
    // Expiration is an authorized transition and must be observed without restart.
    tilo_test_env::within("the current read-only grant expires", async {
        loop {
            if rooms
                .legacy_writer_role(&created.room, foreign.device_public_key)
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

#[cfg(feature = "agents")]
#[tokio::test]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the relay remains live through the policy assertions and its consuming shutdown"
)]
async fn app_authorship_grant_uses_current_owner_and_conflict_checked_retries() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = view_read_relay(scratch.path(), &key, &cert).await;
    let rooms = relay.rooms().unwrap();
    let created = rooms.create("Private app authoring").await.unwrap();
    let request = GrantAppAuthorshipRequest {
        request_id: [71; 32],
        app: "so.tilo.chat".into(),
    };
    let result = rooms
        .grant_app_authorship(&created.room, &cert, request.clone())
        .await
        .unwrap();
    assert_eq!(
        rooms
            .grant_app_authorship(&created.room, &cert, request.clone())
            .await
            .unwrap(),
        result,
    );
    let catalogue = tilo_agent_participant::AppCatalogue::native();
    Box::pin(assert_canonical_chat_preparation(
        rooms,
        &created.room,
        &cert,
        &catalogue,
    ))
    .await;
    let state = rooms.room(&created.room).await.unwrap();
    {
        let room = state.lock().await;
        let caps = room.peer().governed_sandbox().unwrap();
        let grant = caps
            .iter()
            .find(|cap| cap.id == "app-author:so.tilo.chat")
            .unwrap();
        assert_eq!(
            grant.subject,
            collab_types::caps::Subject::app("so.tilo.chat")
        );
        assert_eq!(grant.access, collab_types::caps::Access::Write);
        assert_eq!(grant.scope, collab_types::caps::Scope::Workspace);
    }
    let mut conflict = request.clone();
    conflict.app = "so.tilo.ebook".into();
    assert!(
        rooms
            .grant_app_authorship(&created.room, &cert, conflict)
            .await
            .is_err()
    );
    let other_root = SigningKey::from_bytes(&[73; 32]);
    let other_key = SigningKey::from_bytes(&[74; 32]);
    let mut stranger = cert.clone();
    stranger.user_public_key = other_root.verifying_key().to_bytes();
    stranger.device_public_key = other_key.verifying_key().to_bytes();
    sign_device_cert(&mut stranger, &other_root).unwrap();
    assert!(
        rooms
            .grant_app_authorship(&created.room, &stranger, request.clone())
            .await
            .is_err()
    );
    let mut changed_certificate = cert.clone();
    changed_certificate.device_label = "different signed certificate".into();
    sign_device_cert(&mut changed_certificate, &SigningKey::from_bytes(&[1; 32])).unwrap();
    // Exact retries must not accept a different current certificate simply
    // because its key and request identifier were previously successful.
    assert!(
        rooms
            .grant_app_authorship(&created.room, &changed_certificate, request)
            .await
            .is_err()
    );
    assert!(
        rooms
            .prepare_application(
                &created.room,
                &changed_certificate,
                &catalogue,
                "so.tilo.chat"
            )
            .await
            .is_err()
    );
    assert!(
        rooms
            .prepare_application(&created.room, &stranger, &catalogue, "so.tilo.chat")
            .await
            .is_err()
    );
    relay.shutdown().await;
}

#[cfg(feature = "agents")]
async fn assert_canonical_chat_preparation(
    rooms: &Rooms,
    room: &str,
    cert: &DeviceCert,
    catalogue: &tilo_agent_participant::AppCatalogue,
) {
    assert!(
        rooms
            .prepare_application(room, cert, catalogue, "so.tilo.chat")
            .await
            .unwrap()
    );
    let prepared = {
        let current = rooms.room(room).await.unwrap();
        current
            .lock()
            .await
            .peer()
            .document("so.tilo.chat", "")
            .expect("canonical Chat record exists")
    };
    assert!(!prepared.is_empty());
    let caller =
        tilo_actions::remote::Caller::authenticated(cert.user_public_key, cert.device_public_key);
    let source = tilo_browser_peer::DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: room.into(),
        record: String::new(),
    };
    let workspace = source.workspace_id().unwrap();
    let retained = rooms
        .resource_view_document(workspace, &caller, &source)
        .await
        .expect("the already-owned governor primary has a signed retained source");
    let mut original = automerge::AutoCommit::load(&prepared).unwrap();
    let mut arrived = automerge::AutoCommit::load(&retained.bytes).unwrap();
    assert_eq!(arrived.get_heads(), original.get_heads());
    assert_view_reader_refused(
        rooms,
        workspace,
        &tilo_actions::remote::Caller::authenticated([9; 32], cert.device_public_key),
        &source,
    )
    .await;
    assert!(
        !rooms
            .prepare_application(room, cert, catalogue, "so.tilo.chat")
            .await
            .unwrap()
    );
    {
        let current = rooms.room(room).await.unwrap();
        assert_eq!(
            current
                .lock()
                .await
                .peer()
                .document("so.tilo.chat", "")
                .unwrap(),
            prepared,
            "preparing again must preserve the actual original document"
        );
    }
}

#[cfg(feature = "agents")]
#[tokio::test]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the relay stays live through retained publication checks and its consuming shutdown"
)]
#[allow(
    clippy::too_many_lines,
    reason = "one authoring pass: granted, written, read back through the native view and the voice binding"
)]
async fn governor_application_authoring_preserves_native_view_and_voice_binding() {
    use automerge::{ROOT, ReadDoc, transaction::Transactable};
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = view_read_relay(scratch.path(), &key, &cert).await;
    let rooms = relay.rooms().unwrap();
    let created = rooms.create("Native governor authoring").await.unwrap();
    let catalogue = tilo_agent_participant::AppCatalogue::native();
    assert_unprepared_authoring_refused(rooms, &created.room, &cert, &catalogue).await;
    rooms
        .prepare_application(&created.room, &cert, &catalogue, "so.tilo.chat")
        .await
        .unwrap();
    let source = tilo_browser_peer::DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: created.room.clone(),
        record: String::new(),
    };
    let caller =
        tilo_actions::remote::Caller::authenticated(cert.user_public_key, cert.device_public_key);
    let original = rooms
        .resource_view_document(created.workspace_id, &caller, &source)
        .await
        .unwrap();
    assert_eq!(
        rooms
            .author_application(&created.room, &cert, &catalogue, "so.tilo.chat", |_| Ok(42))
            .await
            .unwrap(),
        42
    );
    let mut different = cert.clone();
    different.device_label = "different current certificate".into();
    sign_device_cert(&mut different, &SigningKey::from_bytes(&[1; 32])).unwrap();
    assert!(
        rooms
            .author_application::<()>(
                &created.room,
                &different,
                &catalogue,
                "so.tilo.chat",
                |_| { panic!("another certificate must never reach the trusted change") }
            )
            .await
            .is_err()
    );
    assert!(
        rooms
            .author_application(&created.room, &cert, &catalogue, "so.tilo.chat", |doc| {
                doc.put(ROOT, "rollback_marker", true).unwrap();
                Err::<(), _>("the authoring callback failed".into())
            })
            .await
            .is_err()
    );
    assert_eq!(
        rooms
            .resource_view_document(created.workspace_id, &caller, &source)
            .await
            .unwrap()
            .bytes,
        original.bytes,
        "a refused or failed authoring callback cannot publish a partial graph"
    );
    let voice = rooms
        .author_application(&created.room, &cert, &catalogue, "so.tilo.chat", |doc| {
            chat::demo::apply_as(
                doc,
                "author",
                chat::Chat::CREATE_WORKSPACE.with(chat::record::chat::CreateWorkspace {
                    name: "Fixture call".into(),
                }),
            )
            .map_err(|error| error.to_string())?;
            doc.put(ROOT, "preserved_native_marker", true)
                .map_err(|error| error.to_string())?;
            Ok(chat::projection::read(doc)
                .channel_rows()
                .into_iter()
                .find(|channel| channel.kind == chat::model::ChannelKind::Voice)
                .unwrap()
                .id)
        })
        .await
        .unwrap();
    let retained = rooms
        .resource_view_document(created.workspace_id, &caller, &source)
        .await
        .expect("native replay accepts the actual governor-authored publication");
    let mut doc = automerge::AutoCommit::load(&retained.bytes).unwrap();
    assert!(doc.get(ROOT, "preserved_native_marker").unwrap().is_some());
    let current = rooms.room(&created.room).await.unwrap();
    let guard = current.lock().await;
    let binding = guard
        .peer()
        .resolve_media_binding("so.tilo.chat", "", &voice)
        .expect("the same native source resolves through production voice admission");
    assert_eq!(binding.channel_id, voice);
    assert_eq!(
        doc.get_heads(),
        guard
            .peer()
            .plaintext_complete_heads("so.tilo.chat", "")
            .unwrap(),
        "native view bytes and the governor's voice binding share the committed graph"
    );
    drop(guard);
    relay.shutdown().await;
}

#[cfg(feature = "agents")]
async fn assert_unprepared_authoring_refused(
    rooms: &Rooms,
    room: &str,
    cert: &DeviceCert,
    catalogue: &tilo_agent_participant::AppCatalogue,
) {
    for app in ["so.tilo.chat", "so.tilo.unregistered"] {
        assert!(
            rooms
                .author_application::<()>(room, cert, catalogue, app, |_| {
                    panic!("missing or unregistered records must refuse before the callback")
                })
                .await
                .is_err()
        );
    }
}
