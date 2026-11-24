//! A record whose stored sealed history is tampered with is set aside when the workspace opens:
//! the rest of the workspace reads, the record's history is not, and a device keeping the record
//! brings it back through the same admission every write takes.

use super::*;
use crate::{AdmissionError, NotAdmitted, Quarantined};

const OTHER: &str = "tab:2";

/// `restored`, a state restored from `stored`, whose record [`RECORD`]'s stored history its
/// checks refuse: the workspace opens, the record is set aside for a reason that holds `reason`,
/// its history is not read, and what is stored of it again is the history as it was stored.
pub(super) fn assert_set_aside(
    stored: &[u8],
    restored: Result<StatePeer, String>,
    why: &str,
    reason: &str,
) {
    let restored = restored.unwrap_or_else(|error| panic!("{why}: the workspace opens: {error}"));
    let quarantined = restored.quarantined();
    assert_eq!(quarantined.len(), 1, "{why}: one record is set aside");
    assert_eq!(
        (quarantined[0].app.as_str(), quarantined[0].record.as_str()),
        (APP, RECORD),
        "{why}: the tampered record is set aside"
    );
    assert!(
        quarantined[0].reason.contains(reason),
        "{why}: refused for {:?}, not for {reason:?}",
        quarantined[0].reason
    );
    assert!(
        restored
            .publication()
            .sealed_history(APP, RECORD)
            .is_empty(),
        "{why}: its history is not read"
    );
    assert!(restored.has_governance(), "{why}: the workspace reads");
    let before: Durable<'_> = postcard::from_bytes(stored).unwrap();
    let again = restored.durable().unwrap();
    let after: Durable<'_> = postcard::from_bytes(&again).unwrap();
    assert_eq!(
        after.sealed, before.sealed,
        "{why}: the refused history is stored again as it was"
    );
}

fn artifact_of(record: &str) -> collab_types::ArtifactId {
    collab_types::document_artifact_id(APP, WORKSPACE, record)
}

/// `key`'s next sealed write of `plaintext` to `record` of `peer`.
fn write_to(
    peer: &StatePeer,
    record: &str,
    key: &SigningKey,
    keys: &ContentKeys,
    plaintext: &[u8],
    now: SystemTime,
) -> NeighbourMsg {
    let (metadata, payload) = keys
        .seal_content(artifact_of(record), ContentRetention::Delta, plaintext)
        .unwrap();
    let seq = peer.core.documents[&Route {
        app: APP.into(),
        record: record.into(),
    }]
        .channel
        .last_seq_per_author
        .get(&Ed25519PublicKey::from_bytes(
            key.verifying_key().to_bytes(),
        ))
        .copied()
        .unwrap_or(0)
        + 1;
    let mut op = SignedOp::new_unsigned(
        payload,
        artifact_of(record),
        Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
        collab_types::document_attestation_id(APP, WORKSPACE, record),
        OpKind::Change,
        now,
        seq,
    );
    op.encryption = Some(metadata);
    collab_types::sign_op(&mut op, key).unwrap();
    NeighbourMsg::AppDocument {
        peer: peer_handle(op.author.as_bytes()),
        app_id: APP.into(),
        cluster_id: WORKSPACE.into(),
        document_id: record.into(),
        envelope: Box::new(op),
        readers: None,
    }
}

/// A workspace of two sealed records, one whose stored history is tampered with, opens: the good
/// record reads and the tampered one is reported set aside, standing nowhere in what the device
/// asks to be replayed. A device sending tampered history, or another write at a number this
/// device admitted, leaves it set aside; a device sending the history this device admitted brings
/// it back, and a restart reads it whole.
#[test]
fn a_tampered_record_is_set_aside_and_comes_back_from_a_device_keeping_it() {
    let mut f = fixture_holding(&[RECORD, OTHER]);
    let (now, keys, other) = (f.now, f.keys(), f.other.clone());
    let mut author = Author::new(&f.replica);
    let mut beside = Author(AutoCommit::load(&f.replica.document(APP, OTHER).unwrap()).unwrap());
    let mut history = Vec::new();
    for value in ["1", "2"] {
        let sealed = write(&f.replica, &other, &keys, &author.edit("value", value), now);
        history.push(sealed.clone());
        admit(&mut f.replica, &other, sealed, now);
        let sealed = write_to(
            &f.replica,
            OTHER,
            &other,
            &keys,
            &beside.edit("value", value),
            now,
        );
        admit(&mut f.replica, &other, sealed, now);
    }
    let kept = held(&f.replica);
    let other_kept = f.replica.publication().sealed_history(APP, OTHER);
    assert_eq!((kept.len(), other_kept.len()), (2, 2));

    let bytes = f.replica.durable().unwrap();
    let mut stored: Durable<'_> = postcard::from_bytes(&bytes).unwrap();
    let entry = stored
        .sealed
        .iter_mut()
        .find(|(entry, _)| *entry == route())
        .unwrap();
    entry.1[1].payload[0] ^= 1;
    let forged = entry.1[1].clone();
    let tampered = postcard::to_allocvec(&stored).unwrap();

    assert_set_aside(
        &tampered,
        f.restore(&tampered),
        "a changed ciphertext",
        "signature",
    );
    // A write its author signed that this device holds no admission evidence for, as a store
    // that kept writes without their evidence holds them: set aside, and the rest opens.
    let mut unevidenced: Durable<'_> = postcard::from_bytes(&bytes).unwrap();
    let entry = unevidenced
        .sealed
        .iter_mut()
        .find(|(entry, _)| *entry == route())
        .unwrap();
    entry.1[1].payload[0] ^= 1;
    collab_types::sign_op(&mut entry.1[1], &other).unwrap();
    let unevidenced = postcard::to_allocvec(&unevidenced).unwrap();
    assert_set_aside(
        &unevidenced,
        f.restore(&unevidenced),
        "a write without admission evidence",
        "sealed history lacks exact admission evidence",
    );
    assert_eq!(
        f.restore(&unevidenced)
            .unwrap()
            .publication()
            .sealed_history(APP, OTHER),
        other_kept,
        "the other record reads beside one without evidence"
    );

    let mut restored = f.restore(&tampered).unwrap();
    assert_eq!(
        restored.publication().sealed_history(APP, OTHER),
        other_kept,
        "the other record reads"
    );
    let floors = restored.replay_floors();
    assert!(
        floors.iter().all(|(_, record, ..)| record != RECORD),
        "the set-aside record is asked for whole: {floors:?}"
    );
    assert!(floors.iter().any(|(_, record, ..)| record == OTHER));
    assert_eq!(
        f.restore(&restored.durable().unwrap())
            .unwrap()
            .quarantined(),
        restored.quarantined(),
        "a restart sets it aside again"
    );

    // A device sending the tampered write is refused as any write that does not verify is.
    assert!(
        restored
            .receive_from_report(origin(&other), message(forged), now)
            .is_err()
    );
    // A write signed by its author at a number this device admitted another write at is refused
    // by the evidence this device kept.
    let mut conflicting = envelope_of(&history[1]);
    conflicting.payload[0] ^= 1;
    collab_types::sign_op(&mut conflicting, &other).unwrap();
    assert!(matches!(
        restored.receive_from_report(origin(&other), message(conflicting), now),
        Err(AdmissionError::ConflictingSequence)
    ));
    assert_eq!(
        restored.quarantined(),
        vec![Quarantined {
            app: APP.into(),
            record: RECORD.into(),
            reason: restored.quarantined()[0].reason.clone(),
        }],
        "refused writes leave it set aside"
    );
    assert!(held(&restored).is_empty());

    // A device keeping the record sends its history again: the full admission takes it in, and
    // once it reaches the record's watermark the record leaves quarantine.
    // Nothing written here while the record is set aside could be kept across a restart: a
    // local write to it is refused at once, and says why.
    let mut aside = Author(author.0.clone());
    let early = write(&restored, &other, &keys, &aside.edit("value", "3"), now);
    let refused = restored
        .admit_new_local_document(APP, RECORD, envelope_of(&early), now)
        .unwrap_err();
    assert!(matches!(refused, NotAdmitted::Restoring), "{refused}");
    assert_eq!(
        refused.to_string(),
        "This record is being restored from another device."
    );
    assert!(held(&restored).is_empty());

    for sealed in history {
        admit(&mut restored, &other, sealed, now);
    }
    assert!(restored.quarantined().is_empty(), "the record is back");
    assert_eq!(held(&restored), kept);
    // Back, the record takes this device's writes again, and they survive a restart.
    let later = write(&restored, &other, &keys, &author.edit("value", "3"), now);
    restored
        .admit_new_local_document(APP, RECORD, envelope_of(&later), now)
        .unwrap();
    let mut kept = kept;
    kept.push(envelope_of(&later));
    assert_eq!(held(&restored), kept);
    let reopened = f.restore(&restored.durable().unwrap()).unwrap();
    assert!(reopened.quarantined().is_empty());
    assert_eq!(held(&reopened), kept);
    assert_eq!(
        reopened.publication().sealed_history(APP, OTHER),
        other_kept
    );
}
