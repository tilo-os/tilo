use super::*;
use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

pub(crate) use super::fixtures::{
    admit_view_reader, door_join, governor, governor_with, restricted_until, signed_pre_admission,
    test_host,
};

#[tokio::test]
async fn a_pre_admission_for_one_person_issues_an_invitation_only_they_can_redeem() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let rooms = relay.rooms();
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
async fn a_crowd_of_pre_admitted_guests_joins_in_one_change_and_a_bad_token_is_refused_alone() {
    use ed25519_dalek::Signer as _;
    use tilo_relay_protocol::rooms::{JoinReply, JoinStep, join_proof_digest};
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let rooms = relay.rooms();
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
async fn a_crowds_shared_writes_hold_each_invitation_grant_and_challenge_across_a_restart() {
    use ed25519_dalek::Signer as _;
    use tilo_relay_protocol::rooms::{JoinReply, JoinStep, join_proof_digest};
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let rooms = relay.rooms();
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
    let relay = test_host(dir.path(), &key, &cert).await;
    let rooms = relay.rooms();
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
async fn every_settled_decision_is_announced_with_its_room() {
    use tilo_action_policy::{Answer, Grant, KindSet, Role, Subject};
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let rooms = relay.rooms();
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

#[cfg(feature = "agents")]
#[tokio::test]
async fn view_read_policy_requires_current_certified_person_and_observes_revocation() {
    use tilo_actions::remote::Caller;
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let rooms = relay.rooms().clone();
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
async fn a_seeded_room_default_record_names_the_default_id() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(scratch.path(), &key, &cert).await;
    let rooms = relay.rooms();
    let created = rooms.create("Seeded default record").await.unwrap();
    let catalogue = tilo_agent_participant::AppCatalogue::native();
    // An app whose windows each show a resource of their own: one that keeps
    // a room record (Scratchpad, Notes) seeds its package primary instead.
    let app = "so.tilo.sheets";
    assert!(!catalogue.get(app).unwrap().keeps_room_record());
    let record = ui::record::RecordId::room_default().as_str().to_owned();
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
async fn governor_application_authoring_survives_publication_tail_rollover() {
    use automerge::{ROOT, ReadDoc, transaction::Transactable};
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(scratch.path(), &key, &cert).await;
    let rooms = relay.rooms();
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

/// A record's large state is a file of its own: a governance change that
/// does not touch it writes the room file alone, a change of it replaces
/// its file, and the room opens to what it committed.
#[test]
fn a_large_record_is_stored_apart_and_written_only_when_it_changes() {
    use automerge::transaction::Transactable as _;
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let mut room =
        GovernedRoom::create(dir.path(), "Team", &cert, &key, SystemTime::now()).unwrap();
    // Incompressible text, so the saved record stays large.
    let mut text = vec![0u8; 48 * 1024];
    blake3::Hasher::new()
        .update(b"large record")
        .finalize_xof()
        .fill(&mut text);
    let large = || {
        let mut doc = automerge::AutoCommit::new();
        doc.put(automerge::ROOT, "text", hex::encode(&text))
            .unwrap();
        doc.save()
    };
    let state = room.collaboration().clone();
    let mut candidate = room.peer().clone();
    candidate
        .publication_mut()
        .publish_document("notes", "plan", large(), &key, SystemTime::now())
        .unwrap();
    room.commit(candidate, &key, SystemTime::now(), |_, _| Ok(state.clone()))
        .unwrap();
    let stored = |room: &GovernedRoom| {
        room.record
            .peer
            .iter()
            .filter(|part| matches!(part, peer_parts::PeerPart::Stored { .. }))
            .cloned()
            .collect::<Vec<_>>()
    };
    let first = stored(&room);
    assert_eq!(first.len(), 1, "the large record is stored apart");
    assert_eq!(room.parts.files(), 1);
    let modified = |room: &GovernedRoom| {
        std::fs::read_dir(room.path.with_extension("parts"))
            .unwrap()
            .map(|entry| entry.unwrap().metadata().unwrap().modified().unwrap())
            .collect::<Vec<_>>()
    };
    let written = modified(&room);

    // A change that leaves the record alone does not write it again.
    let candidate = room.peer().clone();
    room.commit(candidate, &key, SystemTime::now(), |_, _| Ok(state.clone()))
        .unwrap();
    assert_eq!(stored(&room), first);
    assert_eq!(modified(&room), written);

    // Changing it stores it anew and removes what no room file names.
    let mut candidate = room.peer().clone();
    let mut edited =
        automerge::AutoCommit::load(&room.peer().document("notes", "plan").unwrap()).unwrap();
    edited.put(automerge::ROOT, "edited", true).unwrap();
    candidate
        .publication_mut()
        .publish_document("notes", "plan", edited.save(), &key, SystemTime::now())
        .unwrap();
    room.commit(candidate, &key, SystemTime::now(), |_, _| Ok(state.clone()))
        .unwrap();
    let second = stored(&room);
    assert_eq!(second.len(), 1);
    assert_ne!(second, first);
    assert_eq!(room.parts.files(), 1, "the replaced part is removed");

    let restored = GovernedRoom::open(room.path.clone(), &cert, &key).unwrap();
    assert_eq!(
        restored.peer().document("notes", "plan"),
        room.peer().document("notes", "plan")
    );
    assert_eq!(restored.record.generation, room.record.generation);

    // A stored part that does not match its hash refuses to open.
    let peer_parts::PeerPart::Stored { digest, .. } = &second[0] else {
        unreachable!()
    };
    let path = room.path.with_extension("parts").join(hex::encode(digest));
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[0] ^= 1;
    std::fs::write(&path, bytes).unwrap();
    assert!(GovernedRoom::open(room.path.clone(), &cert, &key).is_err());
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
        let relay = test_host(dir.path(), &key, &cert).await;
        let rooms = relay.rooms().clone();
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
        // The grant lasts from its own commit, however long the room took
        // to set up.
        let deadline_ms = tilo_clock::try_unix_now_ms()
            .unwrap()
            .checked_add(5000)
            .unwrap();
        let read_only = Grant::new(
            "legacy-window-read-only-foreign",
            Subject::ForeignDevice,
            tilo_action_policy::TreePath::room(&created.room),
            KindSet::UNDOABLE,
            Answer::Deny,
            Subject::Person(hex::encode(cert.user_public_key)),
        )
        .with_lifetime(Lifetime::Until(deadline_ms));
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
    crate::rooms::fixtures::TestHost::shutdown(relay).await;
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
    let relay = test_host(scratch.path(), &key, &cert).await;
    let rooms = relay.rooms();
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
async fn governor_authors_a_room_record_no_factory_makes() {
    use automerge::{ROOT, ReadDoc, transaction::Transactable};
    const DESKTOP: (&str, &str) = ("so.tilo.browser-desktop", "desktop");
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(scratch.path(), &key, &cert).await;
    let rooms = relay.rooms();
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
#[allow(
    clippy::too_many_lines,
    reason = "one authoring pass: granted, written, read back through the native view and the voice binding"
)]
async fn governor_application_authoring_preserves_native_view_and_voice_binding() {
    use automerge::{ROOT, ReadDoc, transaction::Transactable};
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(scratch.path(), &key, &cert).await;
    let rooms = relay.rooms();
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
                chat::demo::start(doc, "author", "Author", "AU")
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
    let relay = test_host(dir.path(), &key, &cert).await;
    let rooms = relay.rooms().clone();
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
    let relay = test_host(dir.path(), &key, &cert).await;
    let rooms = relay.rooms();
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

#[tokio::test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the relay stays live until its consuming shutdown"
)]
async fn the_owner_restricts_until_a_time_and_lifts_it() {
    use tilo_relay_session::rooms::RemoveMemberRequest;
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let rooms = relay.rooms();
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
        step_timeout_secs: None,
        output: None,
        grant: vec![rule],
        triggers: vec![],
        saved_by: None,
        saved_proof: None,
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
    let relay = test_host(dir.path(), &key, &cert).await;
    let rooms = relay.rooms().clone();
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
    relay: crate::rooms::fixtures::TestHost,
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
    let relay = test_host(&dir.path().join("relay"), &key, &cert).await;
    let rooms = relay.rooms().clone();
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
async fn decisions_settle_while_their_workspace_owner_is_stalled() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let rooms = relay.rooms();
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

/// A Complete whose challenge this host does not hold, and whose request it
/// never completed, is answered as lapsed: the device learns its retained
/// proof is dead and asks for a new challenge, instead of resending it to a
/// refusal forever.
#[tokio::test]
async fn a_proof_whose_challenge_the_host_no_longer_holds_is_answered_as_lapsed() {
    use tilo_delivery_protocol::workspace_join::WorkspaceJoinService as _;
    use tilo_peer_state::join::{
        JOIN_PROOF_LAPSED, JoinChallengeRequest, JoinCompleteRequest, JoinTransportRequest,
        JoinTransportResponse, sign_join_proof,
    };
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let rooms = relay.rooms();
    let created = rooms.create("Lounge").await.unwrap();
    let (device, device_cert) = guest_device();
    let issued = rooms
        .pre_admit(
            &cert,
            PreAdmitRequest {
                request_id: [9; 32],
                grant: signed_pre_admission(&created.room, b"door"),
                recipient_user_public_key: Some(device_cert.user_public_key),
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    let service = rooms.join_service();
    let endpoint = device_cert.device_public_key;
    let JoinTransportResponse::Challenge(answer) = service
        .handle(
            endpoint,
            JoinTransportRequest::Challenge(JoinChallengeRequest {
                signed_invitation: issued.signed_invitation,
                device_cert,
                request_id: [4; 32],
            }),
        )
        .await
    else {
        panic!("no challenge");
    };
    // A challenge the host never issued, as one it has since dropped looks.
    let mut dropped = answer.challenge;
    dropped.nonce = [0x5a; 32];
    let proof = sign_join_proof(dropped, &device).unwrap();
    let response = service
        .handle(
            endpoint,
            JoinTransportRequest::Complete(JoinCompleteRequest { proof }),
        )
        .await;
    assert!(
        matches!(
            response,
            JoinTransportResponse::Error {
                code: JOIN_PROOF_LAPSED,
                ..
            }
        ),
        "{response:?}"
    );
    relay.shutdown().await;
}

/// One window, a Chat tab, on one strip, with ids in the namespace `salt`.
fn one_chat_window(salt: u16) -> [tilo_desktop_model::Strip; 1] {
    use tilo_desktop_model::{Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth};
    let own = |counter: u64| (u64::from(salt) << 48) | counter;
    [Strip {
        id: StripId(own(1)),
        name: "Main".into(),
        window_groups: vec![WindowGroup {
            id: own(2),
            tabs: vec![Tab {
                id: own(3),
                title: "Chat".into(),
                tile: TileContent::Foreign {
                    app_id: "so.tilo.chat".into(),
                    surface_id: 7,
                },
                app_title: None,
            }],
            active_tab: own(3),
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }],
    }]
}

/// A window a member opens after joining is a change to the room document in
/// that device's own topology namespace. The governor takes it in as the
/// room's next generation, so every other member sees the window; before,
/// only record writes were taken in and the window reached nobody.
#[tokio::test]
async fn a_window_a_member_opens_becomes_the_rooms_next_generation() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let rooms = relay.rooms().clone();
    let created = rooms.create("Lounge").await.unwrap();
    let member_key = SigningKey::from_bytes(&[3; 32]);
    let mut member = cert.clone();
    member.device_public_key = member_key.verifying_key().to_bytes();
    member.capabilities = DeviceCapabilities::member();
    sign_device_cert(&mut member, &SigningKey::from_bytes(&[1; 32])).unwrap();
    admit_view_reader(&rooms, &created.room, &key, &member).await;
    // The governor's own namespace has a random salt; take another.
    let salt = {
        let state = rooms.room(&created.room).await.unwrap();
        let owners = tilo_desktop_model::projection::contents_owners_of(
            state.lock().await.peer().admitted_room().unwrap(),
        );
        (1..=u16::MAX)
            .find(|salt| !owners.contains_key(salt))
            .unwrap()
    };
    rooms
        .claim_topology_namespace(&created.room, &member, salt)
        .await
        .unwrap();

    let windows = one_chat_window(salt);
    let (generation, mut doc) = {
        let state = rooms.room(&created.room).await.unwrap();
        let room = state.lock().await;
        let doc = room
            .peer()
            .admitted_room()
            .expect("a created room has a room document")
            .clone();
        (room.record.generation, doc)
    };
    let heads = doc.get_heads();
    doc.set_actor(automerge::ActorId::from(member.device_public_key.to_vec()));
    tilo_desktop_model::projection::project_owned_contents(
        &mut doc,
        &windows,
        salt,
        None,
        tilo_desktop_model::projection::EmptyStrips::Skip,
    )
    .unwrap();
    doc.commit();
    let changes = doc.get_changes(&heads);
    assert!(!changes.is_empty(), "opening a window changes the room");
    let hashes = changes
        .iter()
        .map(automerge::Change::hash)
        .collect::<Vec<_>>();
    let body = postcard::to_stdvec(&neighbour_wire::NeighbourMsg::SyncResp {
        room: created.room.clone(),
        changes: changes
            .iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect(),
    })
    .unwrap();

    rooms
        .absorb(&created.room, member.device_public_key, &body)
        .await
        .unwrap();

    {
        let state = rooms.room(&created.room).await.unwrap();
        let room = state.lock().await;
        assert!(
            room.record.generation > generation,
            "the governor committed the window as the room's next generation"
        );
        let mut admitted = room.peer().admitted_room().unwrap().clone();
        drop(room);
        for hash in &hashes {
            assert!(
                admitted.get_change_by_hash(hash).is_some(),
                "the governed room holds the member's window"
            );
        }
    }
    drop(rooms);
    relay.shutdown().await;
}

/// A device of the governor's person, certified as a member and admitted to
/// `room`, with its own topology namespace: what joining gives each member.
#[expect(
    clippy::significant_drop_tightening,
    reason = "the room and the relay stay held through the assertions; Relay::shutdown consumes the relay"
)]
async fn member_with_namespace(
    rooms: &Rooms,
    room: &str,
    key: &SigningKey,
    cert: &DeviceCert,
    seed: u8,
) -> (DeviceCert, u16) {
    let member_key = SigningKey::from_bytes(&[seed; 32]);
    let mut member = cert.clone();
    member.device_public_key = member_key.verifying_key().to_bytes();
    member.capabilities = DeviceCapabilities::member();
    sign_device_cert(&mut member, &SigningKey::from_bytes(&[1; 32])).unwrap();
    admit_view_reader(rooms, room, key, &member).await;
    let salt = {
        let state = rooms.room(room).await.unwrap();
        let governed = state.lock().await;
        let owners = tilo_desktop_model::projection::contents_owners_of(
            governed.peer().admitted_room().unwrap(),
        );
        (1..=u16::MAX)
            .find(|salt| !owners.contains_key(salt))
            .unwrap()
    };
    rooms
        .claim_topology_namespace(room, &member, salt)
        .await
        .unwrap();
    (member, salt)
}

/// `member`'s change to `room`'s document opening two unconfined terminals
/// in its namespace `salt`, as its device would send it.
async fn terminals_opened(rooms: &Rooms, room: &str, member: &DeviceCert, salt: u16) -> Vec<u8> {
    use tilo_desktop_model::{Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth};
    let own = |counter: u64| (u64::from(salt) << 48) | counter;
    let terminal = |counter: u64, title: &str| WindowGroup {
        id: own(counter),
        tabs: vec![Tab {
            id: own(counter),
            title: title.into(),
            tile: TileContent::Foreign {
                app_id: "Alacritty".into(),
                surface_id: counter,
            },
            app_title: None,
        }],
        active_tab: own(counter),
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    };
    let windows = [Strip {
        id: StripId(own(1)),
        name: "Main".into(),
        window_groups: vec![terminal(2, "ssh prod-db"), terminal(3, "~/private/taxes")],
    }];
    let mut doc = {
        let state = rooms.room(room).await.unwrap();
        let governed = state.lock().await;
        governed.peer().admitted_room().unwrap().clone()
    };
    let heads = doc.get_heads();
    doc.set_actor(automerge::ActorId::from(member.device_public_key.to_vec()));
    tilo_desktop_model::projection::project_owned_contents(
        &mut doc,
        &windows,
        salt,
        None,
        tilo_desktop_model::projection::EmptyStrips::Skip,
    )
    .unwrap();
    doc.commit();
    postcard::to_stdvec(&neighbour_wire::NeighbourMsg::SyncResp {
        room: room.to_owned(),
        changes: doc
            .get_changes(&heads)
            .iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect(),
    })
    .unwrap()
}

/// The window titles the governed room holds: what every member is given.
async fn titles_shared(rooms: &Rooms, room: &str) -> Vec<String> {
    let state = rooms.room(room).await.unwrap();
    let governed = state.lock().await;
    tilo_desktop_model::projection::contents_of(governed.peer().admitted_room().unwrap())
        .into_iter()
        .flat_map(|strip| strip.groups)
        .flat_map(|group| group.tabs)
        .map(|tab| tab.title)
        .collect()
}

/// Two strangers in a public room: A opens two terminals, and B is never
/// given them, their app or their titles. The door refuses A's change, and
/// the governor, should it reach it anyway, commits nothing of it.
#[expect(
    clippy::significant_drop_tightening,
    reason = "the room and the relay stay held through the assertions; Relay::shutdown consumes the relay"
)]
#[tokio::test]
async fn in_a_public_room_b_never_sees_the_windows_a_opens() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let rooms = relay.rooms().clone();
    let created = rooms.create("Lounge").await.unwrap();
    let room = created.room.clone();
    rooms
        .set_layout(&room, tilo_desktop_model::projection::RoomLayout::Personal)
        .await
        .unwrap();
    assert!(rooms.is_public(&room));
    let (a, a_salt) = member_with_namespace(&rooms, &room, &key, &cert, 3).await;
    let (_b, _) = member_with_namespace(&rooms, &room, &key, &cert, 4).await;
    let generation = rooms
        .room(&room)
        .await
        .unwrap()
        .lock()
        .await
        .record
        .generation;

    let body = terminals_opened(&rooms, &room, &a, a_salt).await;
    let refused = rooms
        .decide_layout_write(&room, a.device_public_key, &body)
        .await
        .unwrap_err();
    assert!(refused.contains("public room"), "{refused}");

    rooms
        .absorb(&room, a.device_public_key, &body)
        .await
        .unwrap();
    {
        let state = rooms.room(&room).await.unwrap();
        let governed = state.lock().await;
        assert_eq!(
            governed.record.generation, generation,
            "the governor committed nothing of A's windows"
        );
        let shared = governed.peer().admitted_room().unwrap();
        assert!(
            tilo_desktop_model::projection::contents_of(shared)
                .iter()
                .all(|strip| tilo_desktop_model::peer_salt_of(strip.id) != a_salt),
            "no strip of A's reaches B"
        );
    }
    assert!(
        titles_shared(&rooms, &room).await.is_empty(),
        "B is given no title of A's"
    );
    drop(rooms);
    relay.shutdown().await;
}

/// The room's governor writes as its governance writer: in a room whose
/// layout is personal its own windows pass the door that refuses a
/// member's.
#[tokio::test]
async fn the_governors_own_windows_pass_the_door_of_a_personal_room() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let rooms = relay.rooms().clone();
    let room = rooms.create("Lounge").await.unwrap().room;
    rooms
        .set_layout(&room, tilo_desktop_model::projection::RoomLayout::Personal)
        .await
        .unwrap();
    let (a, a_salt) = member_with_namespace(&rooms, &room, &key, &cert, 3).await;
    let body = terminals_opened(&rooms, &room, &a, a_salt).await;
    assert_eq!(
        rooms
            .decide_layout_write(&room, a.device_public_key, &body)
            .await
            .unwrap_err(),
        LAYOUT_REFUSAL
    );
    rooms
        .decide_layout_write(&room, cert.device_public_key, &body)
        .await
        .unwrap();
    drop(rooms);
    relay.shutdown().await;
}

/// In a private room the same change is let through and committed, as
/// before.
#[allow(
    clippy::significant_drop_tightening,
    reason = "fires only in some feature sets: the relay stays held through the assertions; Relay::shutdown consumes it"
)]
#[tokio::test]
async fn in_a_private_room_the_windows_a_member_opens_are_still_shared() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let rooms = relay.rooms().clone();
    let room = rooms.create("Team").await.unwrap().room;
    let (a, a_salt) = member_with_namespace(&rooms, &room, &key, &cert, 3).await;
    let body = terminals_opened(&rooms, &room, &a, a_salt).await;
    assert!(!rooms.is_public(&room));
    rooms
        .decide_layout_write(&room, a.device_public_key, &body)
        .await
        .unwrap();
    rooms
        .absorb(&room, a.device_public_key, &body)
        .await
        .unwrap();
    let mut titles = titles_shared(&rooms, &room).await;
    titles.sort();
    assert_eq!(titles, ["ssh prod-db", "~/private/taxes"]);
    drop(rooms);
    relay.shutdown().await;
}

/// `member`'s change to `room`'s document closing every window it has in
/// its namespace `salt`, as its device would send it.
async fn windows_closed(rooms: &Rooms, room: &str, member: &DeviceCert, salt: u16) -> Vec<u8> {
    let mut doc = {
        let state = rooms.room(room).await.unwrap();
        let governed = state.lock().await;
        governed.peer().admitted_room().unwrap().clone()
    };
    let heads = doc.get_heads();
    doc.set_actor(automerge::ActorId::from(member.device_public_key.to_vec()));
    tilo_desktop_model::projection::project_owned_contents(
        &mut doc,
        &[],
        salt,
        None,
        tilo_desktop_model::projection::EmptyStrips::Skip,
    )
    .unwrap();
    doc.commit();
    postcard::to_stdvec(&neighbour_wire::NeighbourMsg::SyncResp {
        room: room.to_owned(),
        changes: doc
            .get_changes(&heads)
            .iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect(),
    })
    .unwrap()
}

/// Every entry the store under `root` archived.
fn archived_bytes(root: &std::path::Path) -> Vec<Vec<u8>> {
    let mut found = Vec::new();
    let mut dirs = vec![
        root.join(crate::store::RETAINED_DIR)
            .join(crate::store::compaction::ARCHIVE_DIR),
    ];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else if let Ok(bytes) = std::fs::read(&path) {
                found.push(bytes);
            }
        }
    }
    found
}

/// Whether `title` occurs in `frame`, a frame a device's replay delivers:
/// in its bytes as encoded for the wire, or in any operation of a room
/// document change it carries once decoded (a change's columns may be
/// compressed, which a search of the raw bytes alone would miss).
fn replayed_frame_carries(frame: &crate::transport::Envelope, title: &str) -> bool {
    let needle = title.as_bytes();
    let wire = crate::frame::encode(frame).unwrap();
    if wire.windows(needle.len()).any(|window| window == needle) {
        return true;
    }
    let crate::transport::Envelope::Delta { delta, .. } = frame else {
        return false;
    };
    let Some(
        crate::coedit::CoeditPayload::Checkpoint { body, .. }
        | crate::coedit::CoeditPayload::RetainedDelta { body, .. },
    ) = crate::coedit::CoeditPayload::decode(delta)
    else {
        return false;
    };
    let Ok(neighbour_wire::NeighbourMsg::SyncResp { changes, .. }) =
        postcard::from_bytes::<neighbour_wire::NeighbourMsg>(&body)
    else {
        return false;
    };
    changes.iter().any(|bytes| {
        automerge::Change::try_from(bytes.as_slice())
            .is_ok_and(|change| format!("{:?}", change.decode()).contains(title))
    })
}

#[derive(Clone, Copy, Debug)]
enum Withdrawal {
    Closed,
    LayoutTurnedPersonal,
}

#[allow(
    clippy::significant_drop_tightening,
    reason = "fires only in some feature sets: the relay stays held through the assertions; Relay::shutdown consumes it"
)]
#[tokio::test]
async fn a_member_admitted_later_is_replayed_no_window_withdrawn_before() {
    const TITLES: [&str; 2] = ["ssh prod-db", "~/private/taxes"];
    for withdrawal in [Withdrawal::Closed, Withdrawal::LayoutTurnedPersonal] {
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = governor();
        let relay = test_host(dir.path(), &key, &cert).await;
        let rooms = relay.rooms().clone();
        let created = rooms.create("Team").await.unwrap();
        let room = created.room.clone();
        let (a, a_salt) = member_with_namespace(&rooms, &room, &key, &cert, 3).await;
        let body = terminals_opened(&rooms, &room, &a, a_salt).await;
        rooms
            .absorb(&room, a.device_public_key, &body)
            .await
            .unwrap();
        assert_eq!(
            titles_shared(&rooms, &room).await.len(),
            2,
            "{withdrawal:?}: the room shares A's terminals while they are open"
        );

        match withdrawal {
            Withdrawal::Closed => {
                let body = windows_closed(&rooms, &room, &a, a_salt).await;
                rooms
                    .absorb(&room, a.device_public_key, &body)
                    .await
                    .unwrap();
            }
            Withdrawal::LayoutTurnedPersonal => rooms
                .set_layout(&room, tilo_desktop_model::projection::RoomLayout::Personal)
                .await
                .unwrap(),
        }
        assert!(
            titles_shared(&rooms, &room).await.is_empty(),
            "{withdrawal:?}: the room's current state no longer holds A's terminals"
        );

        let (c, c_cert) = guest_device();
        door_join(&rooms, &cert, &room, (&c, &c_cert), 21).await;
        // Whatever the room published is retained before C's replay is read.
        rooms
            .inner
            .host
            .records
            .retained(created.workspace_id)
            .await
            .unwrap();
        let workspace = rooms
            .inner
            .host
            .workspaces
            .lock()
            .await
            .get(&created.workspace_id)
            .cloned()
            .expect("the relay hosts the room's workspace");
        let (replayed, archived) = {
            let runtime = workspace.lock().await;
            (
                crate::store::test_support::replay_frames(&runtime.store),
                archived_bytes(runtime.store.root()),
            )
        };
        assert!(
            !replayed.is_empty(),
            "{withdrawal:?}: a device joining the room is replayed the room"
        );
        for title in TITLES {
            assert!(
                !replayed
                    .iter()
                    .any(|frame| replayed_frame_carries(frame, title)),
                "{withdrawal:?}: a member who joins later is replayed {title:?}, \
                 a window withdrawn before they joined"
            );
            assert!(
                archived.iter().any(|bytes| {
                    crate::frame::decode(bytes.get(4..).unwrap_or_default())
                        .is_ok_and(|frame| replayed_frame_carries(&frame, title))
                }),
                "{withdrawal:?}: the relay keeps {title:?} archived for those who held it"
            );
        }
        drop(rooms);
        relay.shutdown().await;
    }
}

/// A room listed in the directory (open admission) whose governor chose no
/// layout scope becomes personal when the relay opens it: the windows the
/// governor committed of a member while it shared them are removed, its
/// governance says it is personal for every member's device to read, and its
/// doors refuse members' windows from then on.
#[allow(
    clippy::significant_drop_tightening,
    reason = "fires only in some feature sets: the relay stays held through the assertions; Relay::shutdown consumes it"
)]
#[tokio::test]
async fn a_listed_room_refuses_members_windows_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let rooms = relay.rooms().clone();
    let room = rooms.create("Lounge").await.unwrap().room;
    let (a, a_salt) = member_with_namespace(&rooms, &room, &key, &cert, 3).await;
    let body = terminals_opened(&rooms, &room, &a, a_salt).await;
    rooms
        .absorb(&room, a.device_public_key, &body)
        .await
        .unwrap();
    assert_eq!(
        titles_shared(&rooms, &room).await.len(),
        2,
        "committed while the relay still took members' windows in"
    );
    // Listed by a relay from before rooms carried their layout scope.
    rooms
        .inner
        .listings
        .set(
            &room,
            Some(tilo_relay_protocol::directory::Listing {
                description: "Come in".into(),
                default_role: tilo_relay_protocol::rooms::RoomRole::Editor,
            }),
        )
        .unwrap();
    drop(rooms);
    relay.shutdown().await;

    let relay = test_host(dir.path(), &key, &cert).await;
    let rooms = relay.rooms().clone();
    assert!(rooms.is_public(&room));
    assert!(
        titles_shared(&rooms, &room).await.is_empty(),
        "what was committed of A's windows is gone"
    );
    {
        let state = rooms.room(&room).await.unwrap();
        let governed = state.lock().await;
        assert_eq!(
            governed.peer().governance().room_layout(),
            tilo_desktop_model::projection::RoomLayout::Personal,
            "every member's device reads that the room is personal"
        );
    }
    assert!(
        rooms
            .decide_layout_write(&room, a.device_public_key, &body)
            .await
            .is_err()
    );
    rooms
        .absorb(&room, a.device_public_key, &body)
        .await
        .unwrap();
    assert!(titles_shared(&rooms, &room).await.is_empty());
    drop(rooms);
    relay.shutdown().await;
}

/// The governor decides the layout scope, and the room holds to its word
/// across a restart: made personal, what the room committed of a member's
/// windows goes at once; made shared again, members' windows are taken in
/// as in any invite-only room; and a listed room whose governor chose to
/// share its layout keeps sharing it.
#[allow(
    clippy::significant_drop_tightening,
    reason = "fires only in some feature sets: the relay stays held through the assertions; Relay::shutdown consumes it"
)]
#[tokio::test]
async fn the_governor_chooses_the_layout_scope_and_the_room_keeps_to_it() {
    use tilo_desktop_model::projection::RoomLayout;
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let rooms = relay.rooms().clone();
    let room = rooms.create("Lounge").await.unwrap().room;
    assert_eq!(rooms.layout(&room), RoomLayout::Shared, "invite-only");
    let (a, a_salt) = member_with_namespace(&rooms, &room, &key, &cert, 3).await;
    let body = terminals_opened(&rooms, &room, &a, a_salt).await;
    rooms
        .absorb(&room, a.device_public_key, &body)
        .await
        .unwrap();
    assert_eq!(titles_shared(&rooms, &room).await.len(), 2);

    rooms.set_layout(&room, RoomLayout::Personal).await.unwrap();
    assert!(titles_shared(&rooms, &room).await.is_empty());

    rooms.set_layout(&room, RoomLayout::Shared).await.unwrap();
    assert!(!rooms.is_public(&room));
    // A's device publishes its windows again, now that the room shares them.
    let body = terminals_opened(&rooms, &room, &a, a_salt).await;
    rooms
        .decide_layout_write(&room, a.device_public_key, &body)
        .await
        .unwrap();
    rooms
        .absorb(&room, a.device_public_key, &body)
        .await
        .unwrap();
    assert_eq!(titles_shared(&rooms, &room).await.len(), 2);
    rooms
        .inner
        .listings
        .set(
            &room,
            Some(tilo_relay_protocol::directory::Listing {
                description: "Come in".into(),
                default_role: tilo_relay_protocol::rooms::RoomRole::Editor,
            }),
        )
        .unwrap();
    drop(rooms);
    relay.shutdown().await;

    let relay = test_host(dir.path(), &key, &cert).await;
    let rooms = relay.rooms().clone();
    assert_eq!(
        rooms.layout(&room),
        RoomLayout::Shared,
        "the governor's choice, not the listing's default"
    );
    assert_eq!(titles_shared(&rooms, &room).await.len(), 2);
    drop(rooms);
    relay.shutdown().await;
}

/// The Lounge, 2026-10-20: a laptop's join completed at the host, but the
/// laptop restarted before it installed the answer. By the time it resent
/// its retained proof, others had joined: the host no longer replays the
/// answer, says so with its code, and gives the device, a member now, the
/// room's current state through the membership exchange, which matches
/// what the device's retained proof expects.
#[tokio::test]
async fn a_join_the_host_completed_before_the_room_moved_on_is_finished_through_membership() {
    use tilo_delivery_protocol::workspace_join::WorkspaceJoinService as _;
    use tilo_peer_state::join::{
        JOIN_COMPLETED_REFRESH, JoinTransportRequest, JoinTransportResponse,
        MembershipChallengeRequest, MembershipCompleteRequest, sign_membership_proof,
    };
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let rooms = relay.rooms();
    let created = rooms.create("Lounge").await.unwrap();
    let (laptop, laptop_cert) = guest_device();
    let (complete, first) =
        door_join(rooms, &cert, &created.room, (&laptop, &laptop_cert), 11).await;
    let JoinTransportResponse::Complete(delivered) = first else {
        panic!("the first Complete is admitted: {first:?}");
    };
    // The answer never reached the laptop. Someone else joins meanwhile.
    let (other, other_cert) = guest_device();
    let (_, joined) = door_join(rooms, &cert, &created.room, (&other, &other_cert), 12).await;
    assert!(matches!(joined, JoinTransportResponse::Complete(_)));

    let service = rooms.join_service();
    let endpoint = laptop_cert.device_public_key;
    let resent = service.handle(endpoint, complete).await;
    assert!(
        matches!(
            resent,
            JoinTransportResponse::Error {
                code: JOIN_COMPLETED_REFRESH,
                ..
            }
        ),
        "{resent:?}"
    );
    let JoinTransportResponse::MembershipChallenge(answer) = service
        .handle(
            endpoint,
            JoinTransportRequest::MembershipChallenge(MembershipChallengeRequest {
                room: created.room.clone(),
                workspace_id: created.workspace_id,
                device_cert: laptop_cert.clone(),
                request_id: [13; 32],
            }),
        )
        .await
    else {
        panic!("no membership challenge");
    };
    let proof = sign_membership_proof(answer.challenge, &laptop).unwrap();
    let JoinTransportResponse::MembershipComplete(current) = service
        .handle(
            endpoint,
            JoinTransportRequest::MembershipComplete(MembershipCompleteRequest { proof }),
        )
        .await
    else {
        panic!("no membership");
    };
    // What the device's retained proof checks the answer against.
    assert_eq!(current.version, 1);
    assert_eq!(current.invite_id, delivered.invite_id);
    assert_eq!(current.device_cert, laptop_cert);
    assert_eq!(current.role, delivered.role);
    assert_eq!(current.room, delivered.room);
    assert_eq!(current.workspace_id, delivered.workspace_id);
    assert_eq!(current.authority, delivered.authority);
    assert!(current.generation > 0);
    assert!(current.certified_roster.contains(&laptop_cert));
    assert!(current.certified_roster.contains(&other_cert));
    assert!(current.certified_roster.contains(&current.authority));
    relay.shutdown().await;
}

/// A workspace that moves to its person's host keeps its room: the host
/// takes over governing the room under the identity it already has, and
/// refuses a room it holds already, rather than minting another.
#[tokio::test]
async fn a_room_moved_here_keeps_its_identity() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let rooms = relay.rooms();
    let room = "6f1c2b9e-4d3a-4f5e-8a7b-1c2d3e4f5a6b";
    let moved = rooms.create_room("Mine", Some(room)).await.unwrap();
    assert_eq!(moved.room, room, "the room keeps its identity");
    assert_eq!(
        moved.workspace_id,
        crate::room_workspace_id(room).unwrap(),
        "and its workspace the id every device derives from it"
    );
    assert!(
        rooms.create_room("Again", Some(room)).await.is_err(),
        "a room the host holds is never made a second time"
    );
    assert!(rooms.create_room("Bad", Some("not a room")).await.is_err());
    let fresh = rooms.create_room("Fresh", None).await.unwrap();
    assert_ne!(fresh.room, room);
    relay.shutdown().await;
}

/// A member whose person's root renewed its device certificate proves its
/// membership with the renewal: the relay holds the renewal in place of the
/// certificate it replaces, answers the device as the same member with its
/// role, and refuses the older certificate from then on.
#[tokio::test]
async fn a_member_with_a_renewed_certificate_stays_the_same_member() {
    use tilo_delivery_protocol::workspace_join::WorkspaceJoinService as _;
    use tilo_peer_state::join::{
        JoinTransportRequest, JoinTransportResponse, MembershipChallengeRequest,
        MembershipCompleteRequest, sign_membership_proof,
    };
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let rooms = relay.rooms();
    let created = rooms.create("Lounge").await.unwrap();
    let root = SigningKey::from_bytes(&[71; 32]);
    let laptop = SigningKey::from_bytes(&[72; 32]);
    let certify = |issued_at_ns: u64, expires_at_ns: Option<u64>| {
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: laptop.verifying_key().to_bytes(),
            device_label: "laptop".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns,
            expires_at_ns,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).unwrap();
        cert
    };
    let held = certify(1, None);
    let (_, first) = door_join(rooms, &cert, &created.room, (&laptop, &held), 21).await;
    let JoinTransportResponse::Complete(delivered) = first else {
        panic!("the join is admitted: {first:?}");
    };
    let now = tilo_clock::unix_now_ns();
    let renewed = certify(
        now.saturating_sub(1_000_000_000),
        Some(now.saturating_add(3_600_000_000_000)),
    );

    let service = rooms.join_service();
    let endpoint = laptop.verifying_key().to_bytes();
    let challenge = |device_cert: DeviceCert, seed: u8| {
        service.handle(
            endpoint,
            JoinTransportRequest::MembershipChallenge(MembershipChallengeRequest {
                room: created.room.clone(),
                workspace_id: created.workspace_id,
                device_cert,
                request_id: [seed; 32],
            }),
        )
    };
    let JoinTransportResponse::MembershipChallenge(answer) = challenge(renewed.clone(), 22).await
    else {
        panic!("the renewal is challenged as the device it renews");
    };
    let proof = sign_membership_proof(answer.challenge, &laptop).unwrap();
    let JoinTransportResponse::MembershipComplete(current) = service
        .handle(
            endpoint,
            JoinTransportRequest::MembershipComplete(MembershipCompleteRequest { proof }),
        )
        .await
    else {
        panic!("the renewed member is answered");
    };
    assert_eq!(current.device_cert, renewed);
    assert_eq!(
        current.role, delivered.role,
        "the same member, with its role"
    );
    assert!(current.certified_roster.contains(&renewed));
    assert!(!current.certified_roster.contains(&held));
    assert!(
        matches!(
            challenge(held, 23).await,
            JoinTransportResponse::Error { .. }
        ),
        "the older certificate is refused after the renewal"
    );
    relay.shutdown().await;
}
