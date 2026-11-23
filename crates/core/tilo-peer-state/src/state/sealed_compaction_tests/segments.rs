//! A record whose merged state no single op can carry: its compaction spans a signed head and the
//! segments it names, a keyless replica takes the set only whole, and a device joining late takes
//! the set from what that replica holds.

use super::*;

/// A record whose merged state no single op can carry, written on past the bound of its history,
/// stays writable: its compaction spans a signed head and the segments it names, a keyless
/// replica takes the set only whole, in place of the writes it covers, and keeps its history
/// within the bound. A device joining late takes the set from what that replica holds, and its
/// key holder reads the whole record with every change of its history.
#[test]
fn a_record_larger_than_one_op_compacts_in_segments_and_stays_writable() {
    const BASE: usize = 10;
    const PART: usize = 1024 * 1024;
    const EDIT: usize = 256 * 1024;
    let mut f = fixture();
    let keys = f.keys();
    let (now, owner, editor) = (f.now, f.owner.clone(), f.editor.clone());
    let mut author = Author::new(&f.replica);
    let mut written = 0usize;
    let mut compactions = Vec::new();
    for index in 0..BASE {
        let edit = author.edit_bytes(&format!("part-{index}"), noise(PART, index as u64));
        let sealed = write(&f.replica, &editor, &keys, &edit, now);
        written += envelope_of(&sealed).payload.len();
        admit(&mut f.replica, &editor, sealed, now);
    }
    for index in 0..64u64 {
        if !f.replica.publication().sealed_to_compact().is_empty() {
            let mut holder = f.replica.clone();
            let pieces = mint_pieces(&mut holder, &keys, &owner, now).unwrap();
            assert!(pieces.len() > 3, "a snapshot of 10 MiB spans several ops");
            // The segments alone change nothing; the set is taken whole.
            let before = held(&f.replica);
            for piece in pieces.iter().skip(1) {
                f.replica
                    .receive_from_report(origin(&owner), piece.clone(), now)
                    .unwrap();
                assert_eq!(held(&f.replica), before, "a segment alone is taken");
            }
            admit(&mut f.replica, &owner, pieces[0].clone(), now);
            let history = held(&f.replica);
            assert_eq!(
                history.len(),
                pieces.len(),
                "the set replaced every write it covers"
            );
            written += history.iter().map(|op| op.payload.len()).sum::<usize>();
            compactions.push(pieces);
        }
        let edit = author.edit_bytes("edit", noise(EDIT, 1000 + index));
        let sealed = write(&f.replica, &editor, &keys, &edit, now);
        written += envelope_of(&sealed).payload.len();
        admit(&mut f.replica, &editor, sealed, now);
        assert!(
            f.replica.core.documents[&route()].sealed.within_bounds(),
            "the keyless history stays within its bound"
        );
    }
    assert!(
        written > MAX_SEALED_BYTES,
        "the record was written past its bound: {written}"
    );
    assert!(
        compactions.len() > 1,
        "the record compacted again and again"
    );

    // A replica that persisted the set restores it.
    let restored = f.restore(&f.replica.durable().unwrap()).unwrap();
    assert_eq!(held(&restored), held(&f.replica));

    joins_late(&f, compactions.last().unwrap(), &mut author.0);
}

/// `f`'s key holder compacts its record, and `f`'s keyless host takes the set, the segments
/// first: the payload bytes of the set.
pub(super) fn compact_set(f: &mut Fixture, keys: &ContentKeys, now: SystemTime) -> usize {
    let owner = f.owner.clone();
    let mut holder = f.replica.clone();
    let pieces = mint_pieces(&mut holder, keys, &owner, now).unwrap();
    for piece in pieces.iter().skip(1) {
        f.replica
            .receive_from_report(origin(&owner), piece.clone(), now)
            .unwrap();
    }
    admit(&mut f.replica, &owner, pieces[0].clone(), now);
    pieces
        .iter()
        .map(|piece| envelope_of(piece).payload.len())
        .sum()
}

/// A record whose merged state is twice the bound of the writes a sealed history keeps stays
/// writable: its compaction set counts toward its own bound, so a compaction comes due from the
/// writes since the last one alone, never from the record's size, and the compaction work each
/// write brings stays bounded. A keyless replica still refuses writes past the bound since the
/// last compaction.
#[test]
fn a_record_twice_the_bound_of_its_writes_compacts_only_for_its_new_writes() {
    const PARTS: usize = 30;
    const PART: usize = 1024 * 1024;
    const WRITES: usize = 100;
    const EDIT: usize = 128 * 1024;
    let mut f = fixture();
    let keys = f.keys();
    let (now, editor) = (f.now, f.editor.clone());
    let mut author = Author::new(&f.replica);
    for index in 0..PARTS {
        if !f.replica.publication().sealed_to_compact().is_empty() {
            compact_set(&mut f, &keys, now);
        }
        let edit = author.edit_bytes(&format!("part-{index}"), noise(PART, index as u64));
        let sealed = write(&f.replica, &editor, &keys, &edit, now);
        admit(&mut f.replica, &editor, sealed, now);
    }
    let set = compact_set(&mut f, &keys, now);
    assert!(
        set > 2 * MAX_SEALED_BYTES - 4 * PART,
        "the record's merged state is {set} bytes"
    );

    let (mut compactions, mut compacted) = (0usize, 0usize);
    for index in 0..WRITES {
        if !f.replica.publication().sealed_to_compact().is_empty() {
            compactions += 1;
            compacted += compact_set(&mut f, &keys, now);
        }
        let edit = author.edit_bytes("edit", noise(EDIT, 5000 + index as u64));
        let sealed = write(&f.replica, &editor, &keys, &edit, now);
        admit(&mut f.replica, &editor, sealed, now);
        assert!(f.replica.core.documents[&route()].sealed.within_bounds());
    }
    eprintln!(
        "{WRITES} writes of {EDIT} bytes to a record of {set} bytes: {compactions} compactions, \
         {compacted} bytes of compaction sets"
    );
    // The writes since the last compaction alone bring it due: one per three quarters of their
    // bound, whatever the record's size.
    assert!(
        compactions <= WRITES * EDIT / (MAX_SEALED_BYTES / 4 * 3) + 1,
        "{compactions} compactions in {WRITES} writes"
    );

    // Writes past the bound since the last compaction are refused, the set held as it was.
    let head = held(&f.replica)[0].clone();
    let mut index = 0u64;
    let error = loop {
        let edit = author.edit_bytes("more", noise(PART, 9000 + index));
        let sealed = write(&f.replica, &editor, &keys, &edit, now);
        if let Err(error) = f.replica.receive_from_report(origin(&editor), sealed, now) {
            break error.to_string();
        }
        index += 1;
        assert!(index < 64, "writes past the bound are refused");
    };
    assert!(error.contains("sealed record history is full"), "{error}");
    let sealed = &f.replica.core.documents[&route()].sealed;
    let (_, since) = sealed.writes();
    assert!(
        since <= MAX_SEALED_BYTES && since + 2 * PART > MAX_SEALED_BYTES,
        "refused after {since} bytes of writes since the compaction"
    );
    assert_eq!(held(&f.replica)[0], head, "the compaction set stays");
}

/// A keyless replica refuses a compaction set larger than one document transfer: its pieces
/// alone change nothing it holds, and a stored history that leads with one does not restore.
#[test]
fn a_keyless_replica_refuses_a_compaction_set_past_its_bound() {
    const SEGMENT: usize = 1024 * 1024;
    let mut f = fixture();
    let keys = f.keys();
    let (now, owner, editor) = (f.now, f.owner.clone(), f.editor.clone());
    let mut author = Author::new(&f.replica);
    let sealed = write(&f.replica, &editor, &keys, &author.edit("title", "x"), now);
    admit(&mut f.replica, &editor, sealed, now);
    let before = held(&f.replica);

    // A set of `count` segments of the key holder, over `before`: a keyless replica cannot
    // open them.
    let forge = |count: usize| -> Vec<SignedOp> {
        let segments: Vec<(EncryptionMetadata, Vec<u8>)> = (0..count)
            .map(|index| {
                let (metadata, ciphertext) = keys
                    .seal_content(
                        artifact(),
                        ContentRetention::Delta,
                        &noise(SEGMENT, index as u64),
                    )
                    .unwrap();
                (
                    metadata,
                    collab_types::SealedCompactionSegment::new(ciphertext)
                        .encode()
                        .unwrap(),
                )
            })
            .collect();
        let payloads: Vec<Vec<u8>> = segments
            .iter()
            .map(|(_, payload)| payload.clone())
            .collect();
        let (wrapper, metadata) = wrapper(&f.replica, &keys, &before);
        // Numbered as the last write of its author the set folds: the history
        // is the editor's, so none of it is the owner's.
        let seq = wrapper
            .covered(&Ed25519PublicKey::from_bytes(
                owner.verifying_key().to_bytes(),
            ))
            .unwrap_or(0);
        let head = signed(
            &owner,
            seq,
            OpKind::Compaction,
            wrapper.segmented(&payloads).encode().unwrap(),
            Some(metadata),
            now,
        );
        std::iter::once(head)
            .chain(segments.into_iter().map(|(metadata, payload)| {
                signed(
                    &owner,
                    seq,
                    OpKind::Compaction,
                    payload,
                    Some(metadata),
                    now,
                )
            }))
            .collect()
    };
    let deliver = |host: &mut StatePeer, set: &[SignedOp]| {
        for piece in set.iter().skip(1).chain(set.first()) {
            let _ = host.receive_from_report(origin(&owner), message(piece.clone()), now);
        }
    };

    // One whose segments carry more than one transfer may.
    let set = forge(collab_types::document_transfer::MAX_DOCUMENT_TRANSFER_BYTES / SEGMENT + 2);
    assert!(
        set.iter().map(|op| op.payload.len()).sum::<usize>()
            > collab_types::document_transfer::MAX_DOCUMENT_TRANSFER_BYTES + SEGMENT
    );
    let mut taking = f.replica.clone();
    deliver(&mut taking, &set);
    assert_eq!(held(&taking), before, "the set is not taken");
    let sealed = write(&taking, &editor, &keys, &author.edit("title", "y"), now);
    admit(&mut taking, &editor, sealed, now);
    // The same set within its bound is taken.
    let within = forge(3);
    let mut taken = f.replica.clone();
    deliver(&mut taken, &within);
    assert_eq!(held(&taken), within, "a set within its bound is taken");

    let error = f.replica.clone().restore_sealed(&route(), set).unwrap_err();
    assert!(error.contains("outside its bounds"), "{error}");
}
