use super::*;
use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

/// A relay certified only to host rooms: it governs them without stewarding
/// their channels.
pub(crate) fn governor() -> (SigningKey, DeviceCert) {
    governor_with(DeviceCapabilities::host())
}

/// A relay device of the person's own, certified with `capabilities`.
pub(crate) fn governor_with(capabilities: DeviceCapabilities) -> (SigningKey, DeviceCert) {
    let root = SigningKey::from_bytes(&[1; 32]);
    let key = SigningKey::from_bytes(&[2; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "home".into(),
        capabilities,
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &root).unwrap();
    (key, cert)
}

pub(crate) async fn view_read_relay(
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

pub(crate) fn signed_pre_admission(
    room: &str,
    token: &[u8],
) -> tilo_relay_protocol::rooms::PreAdmission {
    use ed25519_dalek::Signer as _;
    use sha2::Digest as _;
    let owner = SigningKey::from_bytes(&[1; 32]);
    let mut grant = tilo_relay_protocol::rooms::PreAdmission {
        room: room.to_owned(),
        role: tilo_relay_protocol::rooms::RoomRole::Editor,
        token_sha256: sha2::Sha256::digest(token).into(),
        label: "Ada".into(),
        expires_at_ns: tilo_clock::unix_now_ns().saturating_add(600_000_000_000),
        owner_user_public_key: owner.verifying_key().to_bytes(),
        signature: [0; 64],
    };
    grant.signature = owner.sign(&grant.digest().unwrap()).to_bytes();
    grant
}

#[tokio::test]
#[cfg_attr(
    feature = "agents",
    expect(
        clippy::significant_drop_tightening,
        reason = "Relay::shutdown consumes the relay after the assertions; the suggested earlier drop would use a moved value"
    )
)]
async fn a_pre_admission_for_one_person_issues_an_invitation_only_they_can_redeem() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = view_read_relay(dir.path(), &key, &cert).await;
    let rooms = relay.rooms().expect("a certified host governs rooms");
    let created = rooms.create("Demo").await.unwrap();
    let guest = [7; 32];
    let request = PreAdmitRequest {
        request_id: [3; 32],
        grant: signed_pre_admission(&created.room, b"one-time"),
        recipient_user_public_key: Some(guest),
        inviter_name: Some("Plaza".into()),
    };
    let issued = rooms.pre_admit(&cert, request.clone()).await.unwrap();
    // The same request again is a retry, not a second invitation.
    assert_eq!(rooms.pre_admit(&cert, request).await.unwrap(), issued);
    let pending = rooms.pending(&created.room, &cert).await.unwrap();
    let invitation = pending
        .iter()
        .find(|pending| pending.invite_id == issued.invite_id)
        .unwrap();
    assert_eq!(invitation.recipient_user_public_key, Some(guest));
    relay.shutdown().await;
}

/// A person's device certificate, as a guest's browser makes one.
pub(crate) fn guest_device() -> (SigningKey, DeviceCert) {
    let root = SigningKey::from_bytes(&rand::random());
    let key = SigningKey::from_bytes(&rand::random());
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "guest".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &root).unwrap();
    (key, cert)
}

#[tokio::test]
#[cfg_attr(
    feature = "agents",
    expect(
        clippy::significant_drop_tightening,
        reason = "Relay::shutdown consumes the relay after the assertions; the suggested earlier drop would use a moved value"
    )
)]
async fn a_crowd_of_pre_admitted_guests_joins_in_one_change_and_a_bad_token_is_refused_alone() {
    use ed25519_dalek::Signer as _;
    use tilo_relay_protocol::rooms::{JoinReply, JoinStep, join_proof_digest};
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = view_read_relay(dir.path(), &key, &cert).await;
    let rooms = relay.rooms().unwrap();
    let created = rooms.create("Public").await.unwrap();
    let mut proofs = Vec::new();
    let mut people = Vec::new();
    for index in 0..6_u8 {
        let token = [index + 1; 8];
        let issued = rooms
            .pre_admit(
                &cert,
                PreAdmitRequest {
                    request_id: [index + 1; 32],
                    grant: signed_pre_admission(&created.room, &token),
                    recipient_user_public_key: None,
                    inviter_name: None,
                },
            )
            .await
            .unwrap();
        let (device, device_cert) = guest_device();
        people.push(device_cert.user_public_key);
        let JoinReply::Challenge { challenge } = rooms
            .page_join(JoinStep::Ask {
                invitation: issued.signed_invitation,
                device_cert,
                request_id: rand::random(),
            })
            .await
        else {
            panic!("no challenge");
        };
        let signature = device.sign(&join_proof_digest(&challenge)).to_bytes();
        // The last guest presents someone else's token.
        let token = if index == 5 { [1; 8] } else { token };
        proofs.push(JoinStep::ProvePreAdmitted {
            challenge,
            signature,
            name: format!("Guest {index}"),
            token: token.to_vec(),
        });
    }
    let before = rooms.summary(&created.room).await.unwrap().generation;
    let answers = n0_future::join_all(proofs.into_iter().map(|proof| rooms.page_join(proof))).await;
    for answer in answers.iter().take(5) {
        assert!(matches!(answer, JoinReply::Admitted { .. }), "{answer:?}");
    }
    assert!(
        matches!(answers.last(), Some(JoinReply::Refused(reason)) if reason.contains("token")),
        "{:?}",
        answers.last()
    );
    let summary = rooms.summary(&created.room).await.unwrap();
    assert_eq!(summary.generation, before + 1, "one change admits them all");
    let members: Vec<_> = summary
        .members
        .iter()
        .map(|member| member.user_public_key)
        .collect();
    for person in people.iter().take(5) {
        assert!(members.contains(person));
    }
    assert!(!members.contains(&people[5]));
    relay.shutdown().await;
}

/// Concurrent pre-admissions and challenges share their room's writes, and
/// every one of them is written before it is answered: after a restart each
/// guest still proves its challenge and is admitted.
#[tokio::test]
#[cfg_attr(
    feature = "agents",
    expect(
        clippy::significant_drop_tightening,
        reason = "Relay::shutdown consumes each relay after its phase; the suggested earlier drop would use a moved value"
    )
)]
async fn a_crowds_shared_writes_hold_each_invitation_grant_and_challenge_across_a_restart() {
    use ed25519_dalek::Signer as _;
    use tilo_relay_protocol::rooms::{JoinReply, JoinStep, join_proof_digest};
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = view_read_relay(dir.path(), &key, &cert).await;
    let rooms = relay.rooms().unwrap();
    let created = rooms.create("Public").await.unwrap();
    let issued = n0_future::join_all((0..8_u8).map(|index| {
        let rooms = rooms.clone();
        let room = created.room.clone();
        let cert = cert.clone();
        async move {
            rooms
                .pre_admit(
                    &cert,
                    PreAdmitRequest {
                        request_id: [index + 1; 32],
                        grant: signed_pre_admission(&room, &[index + 1; 8]),
                        recipient_user_public_key: None,
                        inviter_name: None,
                    },
                )
                .await
                .unwrap()
        }
    }))
    .await;
    let guests: Vec<_> = issued.iter().map(|_| guest_device()).collect();
    let asked = n0_future::join_all(issued.iter().zip(&guests).map(
        |(issued, (_, device_cert))| {
            rooms.page_join(JoinStep::Ask {
                invitation: issued.signed_invitation.clone(),
                device_cert: device_cert.clone(),
                request_id: rand::random(),
            })
        },
    ))
    .await;
    relay.shutdown().await;
    let relay = view_read_relay(dir.path(), &key, &cert).await;
    let rooms = relay.rooms().unwrap();
    for (index, (asked, (device, _))) in asked.into_iter().zip(&guests).enumerate() {
        let JoinReply::Challenge { challenge } = asked else {
            panic!("no challenge: {asked:?}");
        };
        let signature = device.sign(&join_proof_digest(&challenge)).to_bytes();
        let token = u8::try_from(index + 1).unwrap();
        let answer = rooms
            .page_join(JoinStep::ProvePreAdmitted {
                challenge,
                signature,
                name: format!("Guest {index}"),
                token: vec![token; 8],
            })
            .await;
        assert!(matches!(answer, JoinReply::Admitted { .. }), "{answer:?}");
    }
    relay.shutdown().await;
}

#[tokio::test]
#[cfg_attr(
    feature = "agents",
    expect(
        clippy::significant_drop_tightening,
        reason = "Relay::shutdown consumes the relay after the assertions; the suggested earlier drop would use a moved value"
    )
)]
async fn every_settled_decision_is_announced_with_its_room() {
    use tilo_action_policy::{Answer, Grant, KindSet, Role, Subject};
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = view_read_relay(dir.path(), &key, &cert).await;
    let rooms = relay.rooms().unwrap();
    let created = rooms.create("Demo").await.unwrap();
    let mut changes = rooms.governance_changes();
    let deny_outside = Grant::new(
        "public-guests-no-outside",
        Subject::Role(Role::Editor),
        tilo_action_policy::TreePath::room(&created.room),
        KindSet::OUTSIDE,
        Answer::Deny,
        Subject::Person(hex::encode(cert.user_public_key)),
    );
    rooms
        .put_room_grants(&created.room, &cert, [4; 32], vec![deny_outside])
        .await
        .unwrap();
    let announced = tokio::time::timeout(std::time::Duration::from_secs(5), changes.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(announced, created.room);
    assert_eq!(
        rooms.summary(&created.room).await.unwrap().room,
        created.room
    );
    relay.shutdown().await;
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

pub(crate) async fn admit_view_reader(
    rooms: &Rooms,
    room: &str,
    key: &SigningKey,
    member: &DeviceCert,
) {
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
            .governance_mut()
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

/// The room's default record a host seeds is the one every member's first
/// window opens: it names the default id, or members refuse it.
#[cfg(feature = "agents")]
#[tokio::test]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the relay stays live through the record check and its consuming shutdown"
)]
async fn a_seeded_room_default_record_names_the_default_id() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = view_read_relay(scratch.path(), &key, &cert).await;
    let rooms = relay.rooms().unwrap();
    let created = rooms.create("Seeded default record").await.unwrap();
    let catalogue = tilo_agent_participant::AppCatalogue::native();
    // An app that keeps a record per window: one that keeps a room record
    // seeds its package primary, not the default route.
    let app = "so.tilo.scratchpad";
    let record = crate::room_bots::room_record(&catalogue, app);
    assert_eq!(record, ui::record::RecordId::room_default().as_str());
    assert!(
        rooms
            .prepare_application(&created.room, &cert, &catalogue, app, &record)
            .await
            .unwrap()
    );
    let bytes = rooms
        .record_document(&created.room, app, &record)
        .await
        .unwrap()
        .expect("the room holds its default record");
    let doc = automerge::AutoCommit::load(&bytes).unwrap();
    let kind = catalogue.get(app).unwrap().record_kind.clone();
    assert_eq!(
        ui::record::identity(&doc),
        Some(ui::record::RecordCheckout::new(
            ui::record::RecordId::room_default(),
            kind
        ))
    );
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
        .prepare_application(&created.room, &cert, &catalogue, "so.tilo.chat", "")
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
        .author_application(
            &created.room,
            &cert,
            &catalogue,
            "so.tilo.chat",
            "",
            |doc| {
                doc.put(ROOT, "rollover_counter", 1_u64)
                    .map_err(|error| error.to_string())
            },
        )
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
            .governance_mut()
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
        let now = std::time::SystemTime::now();
        let policy = room
            .peer()
            .permissions(cert.device_public_key, now)
            .unwrap()
            .policy;
        let grant = policy
            .grants
            .iter()
            .find(|grant| grant.id.as_str() == "app-author:so.tilo.chat")
            .unwrap();
        assert_eq!(
            grant.subject,
            tilo_action_policy::Subject::App("so.tilo.chat".into())
        );
        assert!(policy.app_authors("so.tilo.chat", &tilo_action_policy::Context::at_time(now),));
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
                "so.tilo.chat",
                ""
            )
            .await
            .is_err()
    );
    assert!(
        rooms
            .prepare_application(&created.room, &stranger, &catalogue, "so.tilo.chat", "")
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
            .prepare_application(room, cert, catalogue, "so.tilo.chat", "")
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
            .prepare_application(room, cert, catalogue, "so.tilo.chat", "")
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

/// The room's copy of `record` of `app`, when it has one.
#[cfg(feature = "agents")]
async fn room_record(
    rooms: &Rooms,
    room: &str,
    (app, record): (&str, &str),
) -> Option<automerge::AutoCommit> {
    let current = rooms.room(room).await.unwrap();
    let bytes = current.lock().await.peer().document(app, record);
    bytes.map(|bytes| automerge::AutoCommit::load(&bytes).unwrap())
}

/// The governor starts a room record no app factory makes, such as the
/// shared desktop, then changes it in place: the record keeps its graph, an
/// unchanged pass publishes nothing, and another certificate is refused.
#[cfg(feature = "agents")]
#[tokio::test]
#[expect(
    clippy::significant_drop_tightening,
    reason = "Relay::shutdown consumes the relay after the authoring assertions; the suggested subsequent drop would use a moved value"
)]
async fn governor_authors_a_room_record_no_factory_makes() {
    use automerge::{ROOT, ReadDoc, transaction::Transactable};
    const DESKTOP: (&str, &str) = ("so.tilo.browser-desktop", "desktop");
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = view_read_relay(scratch.path(), &key, &cert).await;
    let rooms = relay.rooms().unwrap();
    let created = rooms.create("Shared desktop").await.unwrap();
    let room = created.room.as_str();
    let read = || room_record(rooms, room, DESKTOP);
    let text = |doc: &automerge::AutoCommit| {
        doc.get(ROOT, "desktop")
            .unwrap()
            .unwrap()
            .0
            .into_string()
            .unwrap()
    };
    assert!(read().await.is_none());
    rooms
        .author_room_record(room, &cert, DESKTOP, |doc| {
            doc.put(ROOT, "desktop", "first").map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    let mut first = read().await.expect("the record was started");
    assert_eq!(text(&first), "first");
    rooms
        .author_room_record(room, &cert, DESKTOP, |_| Ok(()))
        .await
        .unwrap();
    assert_eq!(read().await.unwrap().get_heads(), first.get_heads());
    rooms
        .author_room_record(room, &cert, DESKTOP, |doc| {
            doc.put(ROOT, "desktop", "second")
                .map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    let second = read().await.unwrap();
    assert_eq!(text(&second), "second");
    let first_head = first.get_heads()[0];
    assert!(
        second.get_change_by_hash(&first_head).is_some(),
        "the record keeps its graph"
    );
    let mut different = cert.clone();
    different.device_label = "different current certificate".into();
    sign_device_cert(&mut different, &SigningKey::from_bytes(&[1; 32])).unwrap();
    assert!(
        rooms
            .author_room_record::<()>(room, &different, DESKTOP, |_| {
                panic!("another certificate must never reach the change")
            })
            .await
            .is_err()
    );
    relay.shutdown().await;
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
        .prepare_application(&created.room, &cert, &catalogue, "so.tilo.chat", "")
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
            .author_application(
                &created.room,
                &cert,
                &catalogue,
                "so.tilo.chat",
                "",
                |_| Ok(42)
            )
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
                "",
                |_| { panic!("another certificate must never reach the trusted change") }
            )
            .await
            .is_err()
    );
    assert!(
        rooms
            .author_application(
                &created.room,
                &cert,
                &catalogue,
                "so.tilo.chat",
                "",
                |doc| {
                    doc.put(ROOT, "rollback_marker", true).unwrap();
                    Err::<(), _>("the authoring callback failed".into())
                }
            )
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
        .author_application(
            &created.room,
            &cert,
            &catalogue,
            "so.tilo.chat",
            "",
            |doc| {
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
            },
        )
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
        .media()
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
                .author_application::<()>(room, cert, catalogue, app, "", |_| {
                    panic!("missing or unregistered records must refuse before the callback")
                })
                .await
                .is_err()
        );
    }
}

async fn is_banned(rooms: &Rooms, room: &str, host: &DeviceCert, person: [u8; 32]) -> bool {
    let state = rooms.room(room).await.unwrap();
    let permissions = state
        .lock()
        .await
        .peer()
        .permissions(host.device_public_key, SystemTime::now())
        .unwrap();
    permissions.policy.is_banned(&hex::encode(person))
}

#[tokio::test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the relay stays live until its consuming shutdown"
)]
async fn a_renamed_room_shows_its_new_name_to_members_and_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = view_read_relay(dir.path(), &key, &cert).await;
    let rooms = relay.rooms().unwrap().clone();
    let created = rooms.create("plaza").await.unwrap();
    let before = created.generation;
    let renamed = rooms.rename(&created.room, " Plaza ").await.unwrap();
    assert_eq!(renamed.name, "Plaza");
    assert!(
        renamed.generation > before,
        "the rename is a governance change"
    );
    let again = rooms.rename(&created.room, "Plaza").await.unwrap();
    assert_eq!(
        again.generation, renamed.generation,
        "the same name changes nothing"
    );
    assert!(rooms.rename(&created.room, " ").await.is_err());
    let path = {
        let state = rooms.room(&created.room).await.unwrap();
        let governed = state.lock().await;
        assert_eq!(
            governed.peer().governance().room_name().as_deref(),
            Some("Plaza")
        );
        governed.path.clone()
    };
    drop(rooms);
    relay.shutdown().await;
    let restored = GovernedRoom::open(path, &cert, &key).unwrap();
    assert_eq!(restored.summary().name, "Plaza");
    assert_eq!(
        restored.peer().governance().room_name().as_deref(),
        Some("Plaza")
    );
}

#[tokio::test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the relay stays live until its consuming shutdown"
)]
async fn the_owner_bans_and_unbans_and_a_stranger_cannot() {
    use tilo_relay_session::rooms::RemoveMemberRequest;
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = view_read_relay(dir.path(), &key, &cert).await;
    let rooms = relay.rooms().unwrap();
    let created = rooms.create("Demo").await.unwrap();
    let guest = [7; 32];
    let request = |request_id| RemoveMemberRequest {
        request_id,
        user_public_key: guest,
    };
    let stranger_root = SigningKey::from_bytes(&[9; 32]);
    let stranger_key = SigningKey::from_bytes(&[10; 32]);
    let mut stranger = DeviceCert {
        user_public_key: stranger_root.verifying_key().to_bytes(),
        device_public_key: stranger_key.verifying_key().to_bytes(),
        device_label: "stranger".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut stranger, &stranger_root).unwrap();
    assert!(
        rooms
            .ban_member(&created.room, &stranger, request([1; 32]))
            .await
            .is_err(),
        "someone not in the room bans nobody"
    );
    rooms
        .ban_member(&created.room, &cert, request([2; 32]))
        .await
        .unwrap();
    assert!(is_banned(rooms, &created.room, &cert, guest).await);
    assert!(
        rooms
            .unban_member(&created.room, &cert, request([2; 32]))
            .await
            .is_err(),
        "one request id names one change"
    );
    rooms
        .unban_member(&created.room, &cert, request([3; 32]))
        .await
        .unwrap();
    assert!(!is_banned(rooms, &created.room, &cert, guest).await);
    relay.shutdown().await;
}

pub(crate) async fn restricted_until(
    rooms: &Rooms,
    room: &str,
    host: &DeviceCert,
    person: [u8; 32],
) -> Option<u64> {
    let state = rooms.room(room).await.unwrap();
    let now = SystemTime::now();
    let permissions = state
        .lock()
        .await
        .peer()
        .permissions(host.device_public_key, now)
        .unwrap();
    permissions.policy.restricted_until(
        &hex::encode(person),
        &tilo_action_policy::Context::at_time(now),
    )
}

#[tokio::test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the relay stays live until its consuming shutdown"
)]
async fn the_owner_restricts_until_a_time_and_lifts_it() {
    use tilo_relay_session::rooms::RemoveMemberRequest;
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = view_read_relay(dir.path(), &key, &cert).await;
    let rooms = relay.rooms().unwrap();
    let created = rooms.create("Demo").await.unwrap();
    let guest = [7; 32];
    let request = |request_id| RemoveMemberRequest {
        request_id,
        user_public_key: guest,
    };
    let until = tilo_clock::unix_now_ms() + 30 * 60_000;
    rooms
        .restrict_member(&created.room, &cert, request([1; 32]), until)
        .await
        .unwrap();
    assert_eq!(
        restricted_until(rooms, &created.room, &cert, guest).await,
        Some(until)
    );
    assert!(
        !is_banned(rooms, &created.room, &cert, guest).await,
        "a restriction is not a ban"
    );
    rooms
        .unrestrict_member(&created.room, &cert, request([2; 32]))
        .await
        .unwrap();
    assert_eq!(
        restricted_until(rooms, &created.room, &cert, guest).await,
        None
    );
    let owner = RemoveMemberRequest {
        request_id: [3; 32],
        user_public_key: cert.user_public_key,
    };
    assert!(
        rooms
            .restrict_member(&created.room, &cert, owner, until)
            .await
            .is_err(),
        "the owner is never restricted"
    );
    relay.shutdown().await;
}

/// A one-step program posting to chat, as canonical text.
#[cfg(feature = "agents")]
fn bridge_program() -> String {
    let rule = tilo_action_policy::Rule::new(
        tilo_action_policy::TreePath::root()
            .any(tilo_action_policy::Level::Room)
            .app("so.tilo.chat")
            .kind("chat.workspace")
            .any(tilo_action_policy::Level::Resource)
            .action("post"),
        tilo_action_policy::KindSet::READ_UNDOABLE,
        tilo_action_policy::Answer::Allow,
    );
    let shortcut = tilo_automation::Shortcut {
        id: "bridge".into(),
        name: "Bridge".into(),
        inputs: vec![],
        steps: vec![tilo_automation::Step::new(
            "post",
            tilo_automation::StepKind::Invoke {
                action: tilo_actions::ActionKey::new("so.tilo.chat", "chat.workspace", "post"),
                resource: tilo_automation::Selector::FirstOfKind("chat.workspace".into()),
                args: BTreeMap::default(),
                appearance: None,
            },
        )],
        output: None,
        grant: vec![rule],
        triggers: vec![],
        saved_by: None,
    };
    String::from_utf8(tilo_automation::program::canonical_bytes(&shortcut).unwrap()).unwrap()
}

/// A capability-free bot device of the fixture's root, and its installer's
/// delegation naming it installation `id` in `room`.
#[cfg(feature = "agents")]
fn bot_device(id: &str, room: &str, seed: u8) -> tilo_peer_state::workspace_membership::BotDevice {
    use tilo_relay_protocol::delegation::{Delegate, DelegateScope, Delegation};
    let installer = SigningKey::from_bytes(&[1; 32]);
    let device = SigningKey::from_bytes(&[seed; 32]);
    let mut cert = DeviceCert {
        user_public_key: installer.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "Bot: Bridge".into(),
        capabilities: DeviceCapabilities {
            can_own_workspaces: false,
            can_admit_devices: false,
            can_act_as_host: false,
            can_read_all_workspaces: false,
            foreign: false,
            can_administer_devices: false,
        },
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &installer).unwrap();
    let now_ns = tilo_clock::unix_now_ns();
    let delegation = Delegation::sign(
        &installer,
        cert.device_public_key,
        Delegate::Bot {
            installation: id.into(),
        },
        "Bridge",
        [1, 2, 3],
        DelegateScope {
            rooms: vec![room.into()],
            apps: vec![],
        },
        now_ns.saturating_sub(1_000_000_000),
        now_ns.saturating_add(3_600_000_000_000),
    )
    .unwrap();
    tilo_peer_state::workspace_membership::BotDevice { cert, delegation }
}

/// Install `program` as bot `id` with `appearance` in `peer`, as the
/// governor `cert`.
#[cfg(feature = "agents")]
fn install_bot(
    peer: &mut StatePeer,
    (key, cert): (&SigningKey, &DeviceCert),
    program: &str,
    (id, seed): (&str, u8),
    appearance: tilo_peer_state::AppearancePolicy,
) {
    use tilo_peer_state::workspace_membership::{BotInstall, WorkspaceMembershipCommand};
    let command = WorkspaceMembershipCommand::InstallBot(Box::new(BotInstall {
        installation: tilo_peer_state::Installation {
            id: tilo_peer_state::InstallationId::parse(id).unwrap(),
            program: tilo_automation::program::program_hash(program.as_bytes()),
            name: "Bridge".into(),
            icon: None,
            installed_by: hex::encode(cert.user_public_key),
            installed_at_ms: 1,
            appearance,
            source: None,
            armed_at: vec![],
            connector_cursors_from_ms: 1,
            inputs: BTreeMap::default(),
            paused: false,
        },
        program: program.into(),
        device: Some(bot_device(id, peer.room(), seed)),
    }));
    assert_eq!(
        peer.workspace_mut().apply_workspace_membership_command(
            cert,
            &command,
            key,
            SystemTime::now()
        ),
        Ok(true)
    );
}

/// A bot installed with personas shows other names; one installed to post
/// as itself, an unknown installation and a room not governed here do not.
#[cfg(feature = "agents")]
#[tokio::test]
async fn only_a_live_bot_installed_with_personas_shows_other_names() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = view_read_relay(dir.path(), &key, &cert).await;
    let rooms = relay.rooms().unwrap().clone();
    let created = rooms.create("Bridged").await.unwrap();
    let governed = rooms.room(&created.room).await.unwrap();
    let mut peer = governed.lock().await.peer().clone();
    let program = bridge_program();
    let personas = "6f1c2a8e-3b4d-4c5e-9f60-718293a4b5c6";
    let own = "7f1c2a8e-3b4d-4c5e-9f60-718293a4b5c6";
    install_bot(
        &mut peer,
        (&key, &cert),
        &program,
        (personas, 8),
        tilo_peer_state::AppearancePolicy::Personas,
    );
    install_bot(
        &mut peer,
        (&key, &cert),
        &program,
        (own, 9),
        tilo_peer_state::AppearancePolicy::Own,
    );
    let now = SystemTime::now();
    assert!(super::views::bot_shows_personas(&peer, personas, now));
    assert!(!super::views::bot_shows_personas(&peer, own, now));
    assert!(!super::views::bot_shows_personas(
        &peer,
        "8f1c2a8e-3b4d-4c5e-9f60-718293a4b5c6",
        now
    ));
    assert!(
        !rooms.bot_personas(created.workspace_id, personas).await,
        "the governed copy has no such installation"
    );
    assert!(!rooms.bot_personas(0, personas).await);
    drop(rooms);
    drop(relay);
}

/// A member device of a room governed by a relay certified with some
/// capabilities, which has asked the room's governor for one record's
/// channel key.
struct AskedForAChannel {
    /// The relay's state and the member's channel records.
    state: tempfile::TempDir,
    relay: crate::daemon::Relay,
    rooms: Rooms,
    room: String,
    governor: collab_types::Ed25519PublicKey,
    member: collab_types::Ed25519PublicKey,
    channel: collab_types::ChannelId,
}

/// Start a relay certified with `capabilities`, create a room on it, admit
/// a member device of the person, and hand the room that device's MLS
/// access request for a fresh record channel, as its runtime sends one.
async fn a_member_asks_for_a_channel(capabilities: DeviceCapabilities) -> AskedForAChannel {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor_with(capabilities);
    let relay = view_read_relay(&dir.path().join("relay"), &key, &cert).await;
    let rooms = relay
        .rooms()
        .expect("a certified host governs rooms")
        .clone();
    let created = rooms.create("Sealed").await.unwrap();
    let member_key = SigningKey::from_bytes(&[3; 32]);
    let mut member = cert.clone();
    member.device_public_key = member_key.verifying_key().to_bytes();
    member.capabilities = DeviceCapabilities::member();
    sign_device_cert(&mut member, &SigningKey::from_bytes(&[1; 32])).unwrap();
    admit_view_reader(&rooms, &created.room, &key, &member).await;
    let governor = collab_types::Ed25519PublicKey::from_bytes(cert.device_public_key);
    let device = collab_types::Ed25519PublicKey::from_bytes(member.device_public_key);
    let channel = collab_types::ChannelId::new();
    let vault = channel_crypto::VaultKey::new([3; 32]);
    let member_dir = dir.path().join("member");
    std::fs::create_dir_all(&member_dir).unwrap();
    let request =
        channel_crypto::ChannelMember::open(&member_dir, channel, device, governor, &vault)
            .unwrap()
            .access_request(&member_key, 1, &vault)
            .unwrap()
            .expect("a member waiting for the channel asks for it");
    let body = postcard::to_stdvec(&neighbour_wire::NeighbourMsg::MlsAccessRequest {
        room: created.room.clone(),
        request: Box::new(request),
        delivery: 1,
    })
    .unwrap();
    rooms
        .absorb(&created.room, member.device_public_key, &body)
        .await
        .unwrap();
    let asked = AskedForAChannel {
        state: dir,
        relay,
        rooms,
        room: created.room,
        governor,
        member: device,
        channel,
    };
    // Nothing yielded since: the steward pass `absorb` spawned has not run.
    assert!(
        asked.waiting(),
        "the room queued the member's request for its steward"
    );
    asked
}

impl AskedForAChannel {
    /// Whether the member's request still waits for the room's steward pass.
    fn waiting(&self) -> bool {
        self.rooms
            .inner
            .channel_notices
            .lock()
            .unwrap()
            .contains_key(&self.room)
    }

    /// The room's channel stewards on its governor.
    async fn stewardship(&self) -> Option<Arc<stewardship::Steward>> {
        self.rooms
            .room(&self.room)
            .await
            .unwrap()
            .lock()
            .await
            .stewardship
            .clone()
    }

    /// Where the governor would seal the room's channel state.
    fn channels_dir(&self) -> PathBuf {
        self.rooms.inner.dir.join(format!("{}.channels", self.room))
    }

    /// Shut the relay down, then remove its state.
    async fn shut_down(self) {
        let Self {
            state,
            relay,
            rooms,
            ..
        } = self;
        drop(rooms);
        relay.shutdown().await;
        drop(state);
    }
}

/// security.md, "What a workspace host reads and originates": a host
/// certified only to host rooms governs them without stewarding their
/// channels and holds no channel key. A member's request for a record's key
/// reaches the room's steward pass and finds no steward: no MLS group, no
/// channel state on disk, nothing to lend.
#[tokio::test]
#[cfg_attr(
    feature = "agents",
    expect(
        clippy::significant_drop_tightening,
        reason = "the fixture's Relay::shutdown consumes the relay after the assertions; the suggested later drop would use a moved value"
    )
)]
async fn a_plain_host_governs_a_room_without_stewarding_its_channels() {
    let asked = a_member_asks_for_a_channel(DeviceCapabilities::host()).await;
    tilo_test_env::within("the room's steward pass takes the request", async {
        while asked.waiting() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        asked.stewardship().await.is_none(),
        "a plain host certificate must never steward a governed room's channels"
    );
    assert!(
        !asked.channels_dir().exists(),
        "a plain host keeps no channel state for the room"
    );
    asked.shut_down().await;
}

/// The other half of the same claim, so the plain host's test observes its
/// subject: a node certified to own workspaces stewards the channel of every
/// record in the rooms it governs, is a member of each group beside the
/// member devices governance lets read it, and seals its state on disk.
#[tokio::test]
#[cfg_attr(
    feature = "agents",
    expect(
        clippy::significant_drop_tightening,
        reason = "the fixture's Relay::shutdown consumes the relay after the assertions; the suggested later drop would use a moved value"
    )
)]
async fn an_owner_capable_node_stewards_the_channels_of_the_rooms_it_governs() {
    let asked =
        a_member_asks_for_a_channel(tilo_user_identity::cloud_enrollment::host_capabilities())
            .await;
    let steward = asked
        .stewardship()
        .await
        .expect("a node that may own workspaces stewards its rooms' channels");
    let members = tilo_test_env::within("the node admits the member to the channel", async {
        loop {
            let members = steward.channels.lock().await.members(asked.channel);
            if let Some(members) = members.filter(|members| members.contains(&asked.member)) {
                break members;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        members.contains(&asked.governor),
        "the node is a member of the group it stewards, so it holds the channel's keys"
    );
    assert!(asked.channels_dir().exists());
    drop(steward);
    asked.shut_down().await;
}

/// What a test runs inside a room's steward pass, by room.
type PassHooks = std::sync::Mutex<HashMap<String, Arc<dyn Fn() + Send + Sync>>>;

fn pass_hooks() -> &'static PassHooks {
    static HOOKS: std::sync::OnceLock<PassHooks> = std::sync::OnceLock::new();
    HOOKS.get_or_init(PassHooks::default)
}

/// Run the hook a test installed for `room`'s steward pass, where the pass
/// does its MLS work.
pub(crate) fn steward_pass_hook(room: &str) {
    let hook = pass_hooks().lock().unwrap().get(room).cloned();
    if let Some(hook) = hook {
        hook();
    }
}

/// A steward pass's MLS work holds no runtime worker. The pass stalls in
/// its MLS work until the test, on a runtime with a single worker, lets it
/// go: run on that worker, the pass would stall the test with it, and give
/// up waiting unreleased.
#[tokio::test]
#[cfg_attr(
    feature = "agents",
    expect(
        clippy::significant_drop_tightening,
        reason = "the fixture's Relay::shutdown consumes the relay after the assertions; the suggested later drop would use a moved value"
    )
)]
async fn a_steward_pass_does_its_mls_work_off_the_runtime_workers() {
    let asked =
        a_member_asks_for_a_channel(tilo_user_identity::cloud_enrollment::host_capabilities())
            .await;
    let (entered_tx, entered) = std::sync::mpsc::channel::<()>();
    let (release, released_rx) = std::sync::mpsc::channel::<()>();
    let entered_tx = std::sync::Mutex::new(entered_tx);
    let released_rx = std::sync::Mutex::new(released_rx);
    let released = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = Arc::clone(&released);
    pass_hooks().lock().unwrap().insert(
        asked.room.clone(),
        Arc::new(move || {
            let _ = entered_tx.lock().unwrap().send(());
            let in_time = released_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .is_ok();
            observed.store(in_time, std::sync::atomic::Ordering::SeqCst);
        }),
    );
    tilo_test_env::within("the steward pass starts its MLS work", async {
        while entered.try_recv().is_err() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    // Only this runtime's one worker can get here while the pass stalls.
    release.send(()).unwrap();
    let steward = asked.stewardship().await.expect("the node stewards");
    tilo_test_env::within("the node admits the member to the channel", async {
        while !steward
            .channels
            .lock()
            .await
            .members(asked.channel)
            .is_some_and(|members| members.contains(&asked.member))
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    pass_hooks().lock().unwrap().remove(&asked.room);
    assert!(
        released.load(std::sync::atomic::Ordering::SeqCst),
        "the runtime's worker was free while the pass did its MLS work"
    );
    drop(steward);
    asked.shut_down().await;
}

/// A decision does not wait for room in its workspace owner's queue. With
/// the owner stalled on the room's first change and more changes behind it
/// than its queue holds, every decision still settles and answers, and a
/// pre-admission still takes the room; once the owner moves, every change
/// is retained, in the order the room published them.
#[tokio::test]
#[cfg_attr(
    feature = "agents",
    expect(
        clippy::significant_drop_tightening,
        reason = "Relay::shutdown consumes the relay after the assertions; the suggested earlier drop would use a moved value"
    )
)]
async fn decisions_settle_while_their_workspace_owner_is_stalled() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = view_read_relay(dir.path(), &key, &cert).await;
    let rooms = relay.rooms().unwrap();
    let created = rooms.create("Public").await.unwrap();
    let records = rooms.inner.host.records.clone();
    // The creation's own changes are retained before the owner stalls.
    records.retained(created.workspace_id).await.unwrap();
    let root = dir
        .path()
        .join("workspaces")
        .join(created.workspace_id.to_string());
    let (release, reached) = crate::store::test_barrier::install(&root, "state-persist");
    // Each name is one governance change: more than the owner queues.
    for index in 0..48 {
        tilo_test_env::within(
            format!("rename {index} while the owner is stalled"),
            rooms.rename(&created.room, &format!("Public {index}")),
        )
        .await
        .unwrap();
    }
    tilo_test_env::within("the owner stalls on the first change", reached)
        .await
        .unwrap();
    tilo_test_env::within(
        "a pre-admission while the owner is stalled",
        rooms.pre_admit(
            &cert,
            PreAdmitRequest {
                request_id: [9; 32],
                grant: signed_pre_admission(&created.room, &[9; 8]),
                recipient_user_public_key: None,
                inviter_name: None,
            },
        ),
    )
    .await
    .unwrap();
    drop(release);
    let retained = tilo_test_env::within(
        "every change retained once the owner moves",
        records.retained(created.workspace_id),
    )
    .await
    .unwrap();
    let published: Vec<Vec<u8>> = {
        let governed = rooms.room(&created.room).await.unwrap();
        let governed = governed.lock().await;
        governed
            .record
            .outgoing
            .iter()
            .filter(|publication| !publication.checkpoint)
            .map(|publication| publication.bytes.clone())
            .collect()
    };
    assert!(
        published.len() >= 48,
        "{} changes published",
        published.len()
    );
    let host = cert.device_public_key;
    let mut bodies = retained
        .iter()
        .filter(|body| body.author == host)
        .map(|body| &body.body);
    for change in &published {
        assert!(
            bodies.any(|body| body == change),
            "a published change is retained, after the ones published before it"
        );
    }
    relay.shutdown().await;
}
