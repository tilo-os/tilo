//! Removing a device takes its future, not its person's past: what the
//! workspace had taken of a device when its removal was recorded is history
//! for everyone ([`crate::removals`]), and everything else it signs is
//! refused, whenever it claims to have been written.

use super::*;
use crate::admission_tests::seeded;
use crate::{InputDisposition, MissingPrerequisite, TransportOrigin, peer_handle};
use collab_types::CommitNamed as _;
use tilo_user_identity::{RevocationEntry, RevocationReason, sign_revocation_entry};

const APP: &str = "test.app";
const DOCUMENT: &str = "tab:1";

/// A workspace its owner's laptop (`a`) hosts. Their phone (`b`) wrote
/// into a document, the laptop built on that, and the phone was then removed.
struct Story {
    /// The host's copy, after it took the removal into its governance.
    owner_copy: StatePeer,
    a: SigningKey,
    b: SigningKey,
    now: SystemTime,
    /// What a device that joins is sent first: the governance and the
    /// document's checkpoint, as they were before the phone wrote.
    founding: Vec<NeighbourMsg>,
    /// The phone's write, and the laptop's on top of it.
    phones: NeighbourMsg,
    laptops: NeighbourMsg,
    /// The host's copy before the phone wrote, and before its removal:
    /// what a phone that never hears of its removal goes on writing from.
    before_phone: StatePeer,
    before_removal: StatePeer,
    removal: RevocationEntry,
}

fn key_of(key: &SigningKey) -> [u8; 32] {
    key.verifying_key().to_bytes()
}

fn live(key: &SigningKey) -> TransportOrigin {
    TransportOrigin::ServerDelta(key_of(key))
}

/// As a holder hands on what it kept: decided as of the write's own time.
fn retained(key: &SigningKey) -> TransportOrigin {
    TransportOrigin::RetainedServerDelta(key_of(key))
}

/// `key`'s device sets `field` in the document as `peer` holds it, dated `at`.
fn write_at(peer: &mut StatePeer, key: &SigningKey, field: &str, at: SystemTime) -> NeighbourMsg {
    let mut doc = AutoCommit::load(&peer.document(APP, DOCUMENT).unwrap()).unwrap();
    let heads = doc.get_heads();
    doc.put(ROOT, field, "written").unwrap();
    doc.commit_edit("fixture", field, &person(peer, key));
    peer.publication_mut()
        .publish_incremental(APP, DOCUMENT, doc.save_after(&heads), key, at)
        .unwrap()
}

use crate::fixture::person;

fn removal_of(device: &SigningKey, reason: RevocationReason, now: SystemTime) -> RevocationEntry {
    let root = SigningKey::from_bytes(&[1; 32]);
    let mut entry = RevocationEntry {
        user_public_key: key_of(&root),
        revoked_device_public_key: key_of(device),
        revoked_at_ns: now_ns(now),
        reason,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut entry, &root).unwrap();
    entry
}

fn story(reason: RevocationReason) -> Story {
    let (mut owner_copy, a, b, _, now) = seeded();
    let founding = owner_copy
        .publication_mut()
        .bootstrap_messages(&a, now)
        .unwrap();
    let before_phone = owner_copy.clone();
    let phones = write_at(&mut owner_copy, &b, "phone", now);
    let laptops = write_at(&mut owner_copy, &a, "laptop", now);
    let before_removal = owner_copy.clone();
    let removal = removal_of(&b, reason, now);
    assert!(
        owner_copy
            .governance_mut()
            .publish_member_revocations(std::slice::from_ref(&removal), &a, now)
            .unwrap()
    );
    Story {
        owner_copy,
        a,
        b,
        now,
        founding,
        phones,
        laptops,
        before_phone,
        before_removal,
        removal,
    }
}

fn certificate_of(peer: &StatePeer, key: &SigningKey) -> DeviceCert {
    peer.owner_certificates()
        .into_iter()
        .find(|cert| cert.device_public_key == key_of(key))
        .unwrap()
}

/// A device that joins afterwards: it holds nothing of the workspace, and is
/// sent the document as it was founded.
fn newcomer(story: &Story) -> StatePeer {
    joining(story, false)
}

/// [`newcomer`] that keeps exact evidence of what it admits, as a holder
/// does: it answers what a write it holds undecided waits for.
fn newcomer_exact(story: &Story) -> StatePeer {
    joining(story, true)
}

fn joining(story: &Story, exact: bool) -> StatePeer {
    let mut peer = StatePeer::new(
        story.owner_copy.workspace_id(),
        certificate_of(&story.owner_copy, &story.a),
        vec![],
        RevocationList::new(),
        story.now,
    )
    .unwrap();
    if exact {
        peer.select_exact_admissions().unwrap();
    }
    for message in story.founding.clone() {
        peer.receive_from(live(&story.a), message, story.now)
            .unwrap();
    }
    peer
}

/// [`newcomer`], sent the governance that holds the removal and then the
/// document's history, as its holder retains it.
fn caught_up(story: &Story) -> StatePeer {
    let mut peer = newcomer(story);
    peer.receive_from(
        live(&story.a),
        story.owner_copy.workspace_message(),
        story.now,
    )
    .unwrap();
    peer.receive_from(retained(&story.b), story.phones.clone(), story.now)
        .unwrap();
    peer.receive_from(retained(&story.a), story.laptops.clone(), story.now)
        .unwrap();
    peer
}

fn says(peer: &StatePeer, field: &str) -> bool {
    AutoCommit::load(&peer.document(APP, DOCUMENT).unwrap())
        .unwrap()
        .get(ROOT, field)
        .unwrap()
        .is_some()
}

fn heads(peer: &StatePeer) -> Vec<automerge::ChangeHash> {
    AutoCommit::load(&peer.document(APP, DOCUMENT).unwrap())
        .unwrap()
        .get_heads()
}

/// `key`'s sealed write number `seq` to the document.
fn sealed(
    peer: &StatePeer,
    key: &SigningKey,
    seq: u64,
    ciphertext: &[u8],
    at: SystemTime,
) -> NeighbourMsg {
    let mut envelope = SignedOp::new_unsigned_with_encryption(
        ciphertext.to_vec(),
        collab_types::document_artifact_id(APP, peer.workspace_id(), DOCUMENT),
        Ed25519PublicKey::from_bytes(key_of(key)),
        collab_types::document_attestation_id(APP, peer.workspace_id(), DOCUMENT),
        OpKind::Change,
        at,
        seq,
        collab_types::EncryptionMetadata::mls_application(1),
    );
    collab_types::sign_op(&mut envelope, key).unwrap();
    NeighbourMsg::AppDocument {
        peer: peer_handle(&key_of(key)),
        app_id: APP.into(),
        cluster_id: peer.workspace_id().into(),
        document_id: DOCUMENT.into(),
        envelope: Box::new(envelope),
        readers: None,
    }
}

tilo_test_env::scenario! {
    fn removing_a_device_takes_its_future_and_keeps_its_persons_past;

    phase what_a_removed_device_wrote_before_is_history_whatever_the_reason;
    {
        for reason in [
            RevocationReason::Retired,
            RevocationReason::Lost,
            RevocationReason::Stolen,
            RevocationReason::Compromised,
        ] {
            let story = story(reason);
            let mut joiner = newcomer(&story);
            joiner
                .receive_from(live(&story.a), story.owner_copy.workspace_message(), story.now)
                .unwrap();
            // The device itself is gone for the newcomer as for everyone.
            assert!(
                matches!(
                    joiner.permissions(key_of(&story.b), story.now),
                    Err(MembershipError::Revoked)
                ),
                "{reason:?}"
            );
            // What it wrote before is taken, and what was built on it too.
            let taken = joiner
                .receive_from(retained(&story.b), story.phones.clone(), story.now)
                .unwrap();
            assert!(taken.changed, "{reason:?}: the phone's write is history");
            joiner
                .receive_from(retained(&story.a), story.laptops.clone(), story.now)
                .unwrap();
            assert!(says(&joiner, "phone") && says(&joiner, "laptop"), "{reason:?}");
            assert_eq!(heads(&joiner), heads(&story.owner_copy), "{reason:?}");
            assert_eq!(joiner.incomplete_reason(), None, "{reason:?}");
        }
    }

    phase what_a_removed_device_signs_after_is_refused_whatever_it_claims;
    {
        let story = story(RevocationReason::Stolen);
        let joiner = caught_up(&story);
        let earlier = story.now.checked_sub(Duration::from_secs(1000)).unwrap();
        // The device never hears of its removal: it writes on from what it
        // held, and dates one write to before the removal.
        for (what, at) in [
            ("written after the removal", story.now),
            ("dated before the removal", earlier),
        ] {
            let forged = write_at(&mut story.before_removal.clone(), &story.b, "thief", at);
            for (who, peer) in [
                ("the host", &story.owner_copy),
                ("a device that joined after", &joiner),
            ] {
                // Sent by the device, or handed on from what a holder kept.
                for origin in [live(&story.b), retained(&story.b)] {
                    let mut peer = peer.clone();
                    let before = heads(&peer);
                    let refused = peer
                        .receive_from(origin, forged.clone(), story.now)
                        .unwrap_err()
                        .to_string();
                    assert!(refused.contains("revoked"), "{who}, {what}: {refused}");
                    assert_eq!(heads(&peer), before, "{who}, {what}");
                    assert!(!says(&peer, "thief"), "{who}, {what}");
                }
            }
        }

        // A document it never wrote into before is not its to found now.
        let mut thief = story.before_removal.clone();
        let mut other = AutoCommit::new();
        other.put(ROOT, "thief", "written").unwrap();
        other.commit_edit("fixture", "thief", &person(&thief, &story.b));
        let founded = thief
            .publication_mut()
            .publish_document(APP, "tab:2", other.save(), &story.b, story.now)
            .unwrap();
        let mut peer = joiner.clone();
        let refused = peer
            .receive_from(retained(&story.b), founded, story.now)
            .unwrap_err()
            .to_string();
        assert!(refused.contains("revoked"), "{refused}");
        assert!(peer.document(APP, "tab:2").is_none());

        // An old write sent again is the duplicate it always was.
        let mut peer = joiner.clone();
        let before = heads(&peer);
        let again = peer.receive_from(retained(&story.b), story.phones.clone(), story.now);
        assert!(!again.is_ok_and(|outcome| outcome.changed));
        assert_eq!(heads(&peer), before);

        // Another write under a number it used is not taken in its place.
        let other = write_at(
            &mut story.before_phone.clone(),
            &story.b,
            "another",
            story.now,
        );
        let mut peer = joiner;
        let swapped = peer.receive_from(retained(&story.b), other, story.now);
        assert!(!swapped.is_ok_and(|outcome| outcome.changed));
        assert_eq!(heads(&peer), before);
        assert!(!says(&peer, "another"));
    }

    phase a_removal_known_without_its_cut_holds_the_devices_writes_until_it_comes;
    {
        let story = story(RevocationReason::Retired);
        let mut joiner = newcomer_exact(&story);
        // It hears of the removal from a device of the person before the
        // governance that holds it arrives.
        joiner
            .receive_from(
                TransportOrigin::UntrustedPresence(key_of(&story.a)),
                NeighbourMsg::IdentityHello {
                    protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
                    cert: Box::new(certificate_of(&story.owner_copy, &story.a)),
                    revocations: vec![story.removal.clone()],
                },
                story.now,
            )
            .unwrap();
        let waits = InputDisposition::RetryLater(MissingPrerequisite::RemovalCut);
        let held = joiner
            .receive_from_report(retained(&story.b), story.phones.clone(), story.now)
            .unwrap();
        assert_eq!(held.primary(), &waits, "history is held, not refused");
        let forged = write_at(&mut story.before_removal.clone(), &story.b, "thief", story.now);
        let held = joiner
            .receive_from_report(retained(&story.b), forged, story.now)
            .unwrap();
        assert_eq!(held.primary(), &waits, "and nothing of the device is taken unseen");
        assert!(!says(&joiner, "phone") && !says(&joiner, "thief"));

        // The governance arrives with the cut: history lands, the rest does not.
        joiner
            .receive_from(live(&story.a), story.owner_copy.workspace_message(), story.now)
            .unwrap();
        assert!(says(&joiner, "phone"));
        joiner
            .receive_from(retained(&story.a), story.laptops.clone(), story.now)
            .unwrap();
        assert!(says(&joiner, "laptop") && !says(&joiner, "thief"));
        assert_eq!(heads(&joiner), heads(&story.owner_copy));
        assert!(joiner.core.pending.is_empty(), "nothing of the device is left waiting");
    }

    phase a_removal_recorded_before_cuts_were_kept_is_completed_by_the_hosting_device;
    {
        let story = story(RevocationReason::Retired);
        // A removal recorded with no cut yet: the map is founded with the
        // workspace, and the host has not written what it had
        // taken of the device.
        let mut owner_copy = story.owner_copy.clone();
        {
            let doc = owner_copy.core.workspace_doc.as_deref_mut().unwrap();
            let map = doc
                .get(ROOT, crate::removals::FIELD_V1)
                .unwrap()
                .unwrap()
                .1;
            let keys: Vec<String> = doc.keys(&map).collect();
            for key in keys {
                doc.delete(&map, key.as_str()).unwrap();
            }
            doc.commit();
        }
        assert!(owner_copy.governance().lacks_removal_cuts());
        // A device joining now takes the workspace in as it stands, the
        // removal recorded and no cut written, and the document as founded.
        let mut joiner = StatePeer::new(
            story.owner_copy.workspace_id(),
            certificate_of(&story.owner_copy, &story.a),
            vec![],
            RevocationList::new(),
            story.now,
        )
        .unwrap();
        joiner.select_exact_admissions().unwrap();
        joiner
            .receive_from(live(&story.a), owner_copy.workspace_message(), story.now)
            .unwrap();
        for message in story.founding.clone() {
            joiner
                .receive_from(live(&story.a), message, story.now)
                .unwrap();
        }
        assert!(joiner.governance().lacks_removal_cuts());
        let held = joiner
            .receive_from_report(retained(&story.b), story.phones.clone(), story.now)
            .unwrap();
        assert_eq!(
            held.primary(),
            &InputDisposition::RetryLater(MissingPrerequisite::RemovalCut)
        );
        assert!(!says(&joiner, "phone"));

        // The host opens the workspace again: it writes the cut
        // from what it holds, which is what it had taken before it took the
        // removal in.
        assert!(
            owner_copy
                .governance_mut()
                .publish_member_revocations(&[], &story.a, story.now)
                .unwrap()
        );
        assert!(!owner_copy.governance().lacks_removal_cuts());
        assert!(
            !owner_copy
                .governance_mut()
                .publish_member_revocations(&[], &story.a, story.now)
                .unwrap(),
            "a cut is written once"
        );
        joiner
            .receive_from(live(&story.a), owner_copy.workspace_message(), story.now)
            .unwrap();
        assert!(says(&joiner, "phone"), "the document reads again, with nothing done by anyone");
        joiner
            .receive_from(retained(&story.a), story.laptops.clone(), story.now)
            .unwrap();
        assert_eq!(heads(&joiner), heads(&story.owner_copy));
        // And the repair keeps the device out.
        let forged = write_at(&mut story.before_removal.clone(), &story.b, "thief", story.now);
        assert!(
            joiner
                .receive_from(retained(&story.b), forged, story.now)
                .unwrap_err()
                .to_string()
                .contains("revoked")
        );
    }

    phase a_cut_is_nobodys_to_take_back_and_never_the_removed_devices_to_write;
    {
        let story = story(RevocationReason::Compromised);
        let mut joiner = caught_up(&story);
        let erased = {
            let mut doc = story.owner_copy.core.workspace_doc.clone().unwrap();
            let before = doc.get_heads();
            doc.delete(ROOT, crate::removals::FIELD_V1).unwrap();
            // With the operation naming what it changes, as the owner's
            // devices write governance.
            let op = crate::members::SignedMembershipOp::signed(
                &certificate_of(&story.owner_copy, &story.a),
                &story.a,
                crate::members::heads(&crate::members::ops(&doc)),
                tilo_clock::unix_now_ns(),
                crate::members::MembershipOpCommand::Governance {
                    rows: vec![crate::workspace::GovernanceRow::Removals],
                },
            )
            .unwrap();
            crate::members::put_op(&mut doc, &op).unwrap();
            doc.commit();
            NeighbourMsg::SyncResp {
                changes: doc
                    .get_changes(&before)
                    .into_iter()
                    .map(crate::workspace::sent_change)
                    .collect(),
                workspace: story.owner_copy.workspace_id().to_owned(),
            }
        };
        // Not even a device of the owner's takes history back.
        let refused = joiner
            .receive_from(live(&story.a), erased.clone(), story.now)
            .unwrap_err()
            .to_string();
        assert!(refused.contains("cannot be erased"), "{refused}");
        // And the removed device writes no governance at all.
        let refused = joiner
            .receive_from(live(&story.b), erased, story.now)
            .unwrap_err()
            .to_string();
        assert!(refused.contains("revoked"), "{refused}");
        assert!(!joiner.governance().lacks_removal_cuts());
        assert!(says(&joiner, "phone"));
    }

    phase history_is_read_as_its_persons_and_the_device_stays_refused;
    {
        let story = story(RevocationReason::Lost);
        let joiner = caught_up(&story);
        let phone = key_of(&story.b);
        let person = hex::encode(key_of(&SigningKey::from_bytes(&[1; 32])));
        let wrote = |message: &NeighbourMsg| {
            let NeighbourMsg::AppDocument { envelope, .. } = message else {
                panic!("a document write")
            };
            (**envelope).clone()
        };
        let phones = wrote(&story.phones);
        let may = joiner
            .permissions_as_written(APP, DOCUMENT, &phones, story.now)
            .unwrap();
        assert_eq!(may.person_id(), Some(person.as_str()));
        // What it signed after its removal is nobody's, and its history is
        // its own document's alone.
        let forged = wrote(&write_at(
            &mut story.before_removal.clone(),
            &story.b,
            "thief",
            story.now,
        ));
        for (document, write) in [(DOCUMENT, &forged), ("tab:2", &phones)] {
            assert!(matches!(
                joiner.permissions_as_written(APP, document, write, story.now),
                Err(MembershipError::Revoked)
            ));
        }
        assert!(matches!(
            joiner.permissions(phone, story.now),
            Err(MembershipError::Revoked)
        ));
    }

    phase a_removed_devices_sealed_writes_are_history_up_to_the_cut;
    {
        let (mut native, a, b, bc, now) = seeded();
        let founding = native.publication_mut().bootstrap_messages(&a, now).unwrap();
        // A replica that keeps exact evidence of what it admits, as a holder
        // does, and holds a document's sealed writes as its history.
        let exact = |roster: Vec<DeviceCert>| {
            let mut peer = StatePeer::new(
                native.workspace_id(),
                certificate_of(&native, &a),
                roster,
                RevocationList::new(),
                now,
            )
            .unwrap();
            peer.select_exact_admissions().unwrap();
            for message in founding.clone() {
                peer.receive_from(live(&a), message, now).unwrap();
            }
            peer
        };
        let mut owner_copy = exact(vec![bc]);
        let phones = sealed(&owner_copy, &b, 1, b"sealed by the phone", now);
        owner_copy.receive_from(live(&b), phones.clone(), now).unwrap();
        assert!(
            owner_copy
                .governance_mut()
                .publish_member_revocations(&[removal_of(&b, RevocationReason::Retired, now)], &a, now)
                .unwrap()
        );

        let mut joiner = exact(vec![]);
        joiner
            .receive_from(live(&a), owner_copy.workspace_message(), now)
            .unwrap();
        let report = joiner
            .receive_from_report(retained(&b), phones, now)
            .unwrap();
        assert!(
            matches!(report.primary(), InputDisposition::Accepted(_)),
            "the phone's sealed write is the document's history: {:?}",
            report.primary()
        );
        assert_eq!(
            super::sealed_compaction_tests::held(&joiner),
            super::sealed_compaction_tests::held(&owner_copy)
        );
        // Its next number, sealed after the removal, joins no history.
        let earlier = now.checked_sub(Duration::from_secs(1000)).unwrap();
        for at in [now, earlier] {
            let forged = sealed(&joiner, &b, 2, b"sealed by a thief", at);
            for origin in [live(&b), retained(&b)] {
                let refused = joiner
                    .receive_from(origin, forged.clone(), now)
                    .unwrap_err()
                    .to_string();
                assert!(refused.contains("revoked"), "{refused}");
            }
        }
        assert_eq!(super::sealed_compaction_tests::held(&joiner).len(), 1);
    }

    phase a_removed_devices_compaction_is_history_only_as_the_one_the_workspace_had_taken;
    {
        use super::sealed_compaction_tests as sealing;
        let fx = sealing::fixture();
        let keys = fx.keys();
        let (owner, phone, now) = (fx.owner.clone(), fx.other.clone(), fx.now);
        // What a device that joins later holds: the governance and the
        // document's base, and nothing of its sealed history.
        let joining = fx.replica;
        let mut owner_copy = joining.clone();
        let mut author = sealing::Author::new(&owner_copy);
        for field in ["first", "second"] {
            let write = sealing::write(&owner_copy, &phone, &keys, &author.edit(field, "written"), now);
            sealing::admit(&mut owner_copy, &phone, write, now);
        }
        // A device that was away holds the phone's two writes as it wrote them.
        let away = owner_copy.clone();
        // The phone folds them into a checkpoint, which leads the document's
        // history from then on, and is removed.
        let checkpoint = sealing::mint(&mut owner_copy, &keys, &phone, now).unwrap();
        assert!(
            owner_copy
                .governance_mut()
                .publish_member_revocations(
                    &[removal_of(&phone, RevocationReason::Stolen, now)],
                    &owner,
                    now
                )
                .unwrap()
        );
        let governance = owner_copy.workspace_message();

        // Removed, the phone signs another checkpoint of the same two writes,
        // under the number it used for the first: what a number alone would
        // call history.
        let forged = || {
            let folded = sealing::held(&away);
            sealing::forged(&away, &phone, &sealing::wrapper(&away, &keys, &folded), now)
        };
        for (who, mut peer) in [
            ("a device that joins after", joining),
            ("a device that was away", away.clone()),
        ] {
            peer.receive_from(live(&owner), governance.clone(), now)
                .unwrap();
            let before = sealing::held(&peer);
            let refused = peer
                .receive_from(retained(&phone), forged(), now)
                .unwrap_err()
                .to_string();
            assert!(refused.contains("revoked"), "{who}: {refused}");
            assert_eq!(sealing::held(&peer), before, "{who}");
            // The checkpoint the workspace had taken is its history.
            let report = peer
                .receive_from_report(retained(&phone), checkpoint.clone(), now)
                .unwrap();
            assert!(
                matches!(report.primary(), InputDisposition::Accepted(_)),
                "{who}: {:?}",
                report.primary()
            );
            assert_eq!(sealing::held(&peer), sealing::held(&owner_copy), "{who}");
            // And once it leads the history, nothing of the phone's replaces it.
            assert!(
                peer.receive_from(retained(&phone), forged(), now).is_err(),
                "{who}"
            );
            assert_eq!(sealing::held(&peer), sealing::held(&owner_copy), "{who}");
        }
        let refused = owner_copy
            .clone()
            .receive_from(live(&phone), forged(), now)
            .unwrap_err()
            .to_string();
        assert!(refused.contains("revoked"), "{refused}");
    }
}
