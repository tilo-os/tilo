//! The prerequisite queue holds an app message while what it waits for is
//! still in flight. It is bounded, and it never refuses the input being
//! admitted: a held message whose prerequisite does not come is let go with
//! why, and the workspace goes on.
use super::*;
use crate::DropReason;
use crate::admission_tests::seeded;
use collab_types::CommitNamed as _;
use ed25519_dalek::SigningKey;

fn incremental(
    author: &mut StatePeer,
    key: &SigningKey,
    now: SystemTime,
    name: &str,
) -> NeighbourMsg {
    let mut doc = AutoCommit::load(&author.document("test.app", "tab:1").unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(
        key.verifying_key().to_bytes().to_vec(),
    ));
    let heads = doc.get_heads();
    doc.put(ROOT, name, name).unwrap();
    doc.commit_edit("fixture", "fixture", &person(author, key));
    author
        .publication_mut()
        .publish_incremental("test.app", "tab:1", doc.save_after(&heads), key, now)
        .unwrap()
}

use crate::fixture::person;

/// `message` as `key`'s write at `seq`: signed by a device the workspace never
/// certifies, so the certificate it waits for never comes.
fn as_author(message: &NeighbourMsg, key: &SigningKey, seq: u64) -> NeighbourMsg {
    let mut message = message.clone();
    if let NeighbourMsg::AppDocument { peer, envelope, .. } = &mut message {
        *peer = peer_handle(&key.verifying_key().to_bytes());
        envelope.author =
            collab_types::Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes());
        envelope.seq = seq;
        collab_types::sign_op(envelope, key).unwrap();
    }
    message
}

fn value(peer: &StatePeer, name: &str) -> Option<String> {
    let doc = AutoCommit::load(&peer.document("test.app", "tab:1")?).ok()?;
    doc.get(ROOT, name)
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_string().ok())
}

tilo_test_env::scenario! {
    fn a_held_message_is_let_go_applied_or_asked_for_again;

    phase a_held_message_whose_prerequisite_never_comes_is_let_go_with_why_and_the_workspace_goes_on;
    {
        let (mut receiver, key, _, _, now) = seeded();
        receiver.select_exact_admissions().unwrap();
        let mut author = receiver.clone();
        let write = incremental(&mut author, &key, now, "kept");
        let stranger = SigningKey::from_bytes(&[77; 32]);
        let origin = crate::fixture::origin(&stranger);
        let first = receiver
            .receive_from_report(origin, as_author(&write, &stranger, 1), now)
            .unwrap();
        assert_eq!(
            first.primary(),
            &InputDisposition::RetryLater(MissingPrerequisite::Certificate)
        );
        // Fill the queue behind it: every one is held, none refused.
        for seq in 2..=u64::try_from(MAX_HELD_INPUTS).unwrap() {
            let report = receiver
                .receive_from_report(origin, as_author(&write, &stranger, seq), now)
                .unwrap();
            assert!(report.summary().dropped.is_empty());
        }
        assert_eq!(receiver.pending_count(), MAX_HELD_INPUTS);
        let next = u64::try_from(MAX_HELD_INPUTS).unwrap() + 1;
        let report = receiver
            .receive_from_report(origin, as_author(&write, &stranger, next), now)
            .expect("a full prerequisite queue must never refuse the input being admitted");
        assert_eq!(
            report.primary(),
            &InputDisposition::RetryLater(MissingPrerequisite::Certificate)
        );
        let [dropped] = report.summary().dropped.as_slice() else {
            panic!(
                "the oldest held message is let go: {:?}",
                report.summary().dropped
            );
        };
        assert_eq!(dropped.author, stranger.verifying_key().to_bytes());
        assert_eq!(dropped.sequence, 1, "the message held longest goes first");
        assert_eq!(dropped.waiting_for, Some(MissingPrerequisite::Certificate));
        assert_eq!(dropped.reason, DropReason::Superseded);
        assert!(
            dropped.to_string().contains("its author's certificate"),
            "the reason names what it waited for: {dropped}"
        );
        assert_eq!(receiver.pending_count(), MAX_HELD_INPUTS);
        // The queue survives a restart within its bounds.
        let restored = StatePeer::restore(
            &receiver.durable().unwrap(),
            receiver.workspace_id(),
            receiver.owner_certificates()[0].clone(),
            vec![],
            RevocationList::new(),
            now,
        )
        .unwrap();
        assert_eq!(restored.pending_count(), MAX_HELD_INPUTS);
        // A member's write is still admitted, and nothing let go was applied.
        let admitted = receiver
            .receive_from_report(
                crate::fixture::origin(&key),
                write,
                now,
            )
            .unwrap();
        assert!(matches!(admitted.primary(), InputDisposition::Accepted(_)));
        assert_eq!(value(&receiver, "kept").as_deref(), Some("kept"));
    }

    phase one_writer_filling_the_queue_pushes_out_only_its_own_writes;
    {
        let (mut receiver, owner_key, member_key, member, now) = seeded();
        receiver.select_exact_admissions().unwrap();
        let mut author = receiver.clone();
        let filler = incremental(&mut receiver.clone(), &owner_key, now, "filler");
        // The member's certificate has not reached this replica yet.
        receiver.core.certificates.remove(&member.device_public_key);
        let write = incremental(&mut author, &member_key, now, "late");
        let held = receiver
            .receive_from_report(
                TransportOrigin::ServerDelta(member.device_public_key),
                write,
                now,
            )
            .unwrap();
        assert_eq!(
            held.primary(),
            &InputDisposition::RetryLater(MissingPrerequisite::Certificate)
        );
        // A stranger fills the rest of the queue, then writes past it: only its own writes go.
        let stranger = SigningKey::from_bytes(&[78; 32]);
        let origin = crate::fixture::origin(&stranger);
        let mut let_go = Vec::new();
        for seq in 1..=u64::try_from(MAX_HELD_INPUTS).unwrap() + 4 {
            let report = receiver
                .receive_from_report(origin, as_author(&filler, &stranger, seq), now)
                .unwrap();
            let_go.extend(report.summary().dropped.iter().cloned());
        }
        assert_eq!(let_go.len(), 5, "the stranger's writes past the queue are let go");
        assert!(
            let_go
                .iter()
                .all(|input| input.author == stranger.verifying_key().to_bytes()),
            "another writer's held write is never pushed out: {let_go:?}"
        );
        assert_eq!(receiver.pending_count(), MAX_HELD_INPUTS);
        // Its certificate arrives: the member's write held all along is taken in, with nothing
        // else admitted beside it.
        receiver
            .receive_from(
                TransportOrigin::UntrustedPresence(member.device_public_key),
                NeighbourMsg::IdentityHello {
                    protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
                    cert: Box::new(member),
                    revocations: vec![],
                },
                now,
            )
            .unwrap();
        assert_eq!(value(&receiver, "late").as_deref(), Some("late"));
        let admitted = receiver
            .receive_from_report(crate::fixture::origin(&owner_key), filler, now)
            .unwrap();
        assert!(matches!(admitted.primary(), InputDisposition::Accepted(_)));
        assert_eq!(value(&receiver, "late").as_deref(), Some("late"));
    }

    phase a_write_waiting_for_its_authors_certificate_is_taken_in_with_its_own_admission;
    {
        let (mut receiver, _, member_key, _, now) = seeded();
        receiver.select_exact_admissions().unwrap();
        let workspace = receiver.workspace_id().to_owned();
        // A further device of the person the workspace's member list names, whose certificate
        // this replica has not been sent.
        let (newcomer_key, newcomer_cert) = crate::fixture::device(1, 4);
        let mut author = receiver.clone();
        let write = incremental(&mut author, &member_key, now, "by the newcomer");
        let held = receiver
            .receive_from_report(
                TransportOrigin::ServerDelta(newcomer_cert.device_public_key),
                as_author(&write, &newcomer_key, 1),
                now,
            )
            .unwrap();
        assert_eq!(
            held.primary(),
            &InputDisposition::RetryLater(MissingPrerequisite::Certificate)
        );
        assert!(
            held.summary().replay_wanted,
            "the workspace's state, which names the device, is asked for while its write waits"
        );
        // Its own admission reaches this replica as every member's does, under its own name:
        // the write held for its certificate is decided again with it, not on the next change
        // that happens by.
        let governance =
            crate::join_log::GovernanceMessages::governance_messages(&receiver).unwrap();
        let row = crate::own_admission::further_device_row(
            &workspace,
            &governance,
            &newcomer_cert,
            &newcomer_key,
            now,
        )
        .unwrap();
        assert!(!row.is_empty(), "the newcomer signs its own row");
        let admitted = receiver
            .receive_from_report(
                TransportOrigin::ServerDelta(newcomer_cert.device_public_key),
                NeighbourMsg::SyncResp {
                    changes: row,
                    workspace,
                },
                now,
            )
            .unwrap();
        assert!(
            matches!(admitted.primary(), InputDisposition::MetadataOnly | InputDisposition::Accepted(_)),
            "{:?}",
            admitted.primary()
        );
        assert_eq!(
            value(&receiver, "by the newcomer").as_deref(),
            Some("by the newcomer"),
            "a write held for its author's certificate is taken in with its author's own row"
        );
    }

    phase a_message_let_go_is_asked_for_again_once_what_it_waited_for_lands_and_then_applies;
    {
        let (mut receiver, key, _, _, now) = seeded();
        receiver.select_exact_admissions().unwrap();
        let mut author = receiver.clone();
        let first = incremental(&mut author, &key, now, "first");
        let second = incremental(&mut author, &key, now, "second");
        let member = crate::fixture::origin(&key);
        let held = receiver
            .receive_from_report(member, second.clone(), now)
            .unwrap();
        assert_eq!(
            held.primary(),
            &InputDisposition::RetryLater(MissingPrerequisite::CausalDependencies)
        );
        // The member's own later writes fill the queue behind it, and the change they build on
        // has not come.
        let stranger = SigningKey::from_bytes(&[79; 32]);
        let origin = crate::fixture::origin(&stranger);
        let mut let_go = Vec::new();
        for seq in 1..=u64::try_from(MAX_HELD_INPUTS).unwrap() {
            let later = incremental(&mut author, &key, now, &format!("later {seq}"));
            let report = receiver.receive_from_report(member, later, now).unwrap();
            assert!(
                !report.summary().replay_wanted,
                "nothing it waited for landed yet"
            );
            let_go.extend(report.summary().dropped.iter().cloned());
        }
        assert!(
            let_go
                .iter()
                .any(|input| input.author == key.verifying_key().to_bytes()
                    && input.waiting_for == Some(MissingPrerequisite::CausalDependencies)),
            "the member's change was let go: {let_go:?}"
        );
        assert_eq!(value(&receiver, "second"), None);
        // What it built on arrives: the workspace's host is asked to replay.
        let landed = receiver.receive_from_report(member, first, now).unwrap();
        assert!(matches!(landed.primary(), InputDisposition::Accepted(_)));
        assert!(
            landed.summary().replay_wanted,
            "a message let go must not be gone for good: its prerequisite landed, so it is asked for again"
        );
        // The host's replay delivers it again, and now it applies.
        let replayed = receiver.receive_from_report(member, second, now).unwrap();
        assert!(matches!(replayed.primary(), InputDisposition::Accepted(_)));
        assert_eq!(value(&receiver, "second").as_deref(), Some("second"));
        // Asked once: what landed is forgotten.
        let again = receiver
            .receive_from_report(
                origin,
                as_author(&incremental(&mut author, &key, now, "x"), &stranger, 900),
                now,
            )
            .unwrap();
        assert!(!again.summary().replay_wanted);
    }
}
