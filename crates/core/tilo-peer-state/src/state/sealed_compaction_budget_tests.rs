use super::*;

/// A key holder's whole-record checkpoint written as a change re-states the record beside the
/// writes it holds: a replica without the key cannot tell what it covers, so it keeps both, and
/// a record a host checkpoints as often as it asks fills its bound with copies of itself long
/// before what its members wrote does. What comes past the bound is refused, and only a key
/// holder's compaction, which names the writes it covers, lets the record go on.
#[test]
fn checkpoints_written_as_changes_fill_a_sealed_history_with_copies_of_the_record() {
    /// Incompressible: a sealed history is as large as what it carries.
    const WRITE: usize = 128 * 1024;
    let mut f = fixture();
    let keys = f.keys();
    let (now, owner, editor) = (f.now, f.owner.clone(), f.editor.clone());
    let mut author = Author::new(&f.replica);
    let mut state = 0x2545_F491_4F6C_DD1D_u64;
    let mut written = 0;
    let refused = loop {
        let value: Vec<u8> = (0..WRITE)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state.to_le_bytes()[0]
            })
            .collect();
        let edit = author.edit_bytes(&format!("value-{written}"), value);
        let sealed = write(&f.replica, &editor, &keys, &edit, now);
        if let Err(error) = f.replica.receive_from_report(origin(&editor), sealed, now) {
            break error.to_string();
        }
        written += 1;
        // The host asks for a checkpoint, and a key holder writes the whole record as a change.
        let snapshot = DocumentMutation::Snapshot(author.0.save())
            .encode()
            .unwrap();
        let (metadata, ciphertext) = keys
            .seal_content(artifact(), ContentRetention::Checkpoint, &snapshot)
            .unwrap();
        let checkpoint = message(signed(
            &owner,
            next_seq(&f.replica, &owner),
            OpKind::Change,
            ciphertext,
            Some(metadata),
            now,
        ));
        if let Err(error) = f
            .replica
            .receive_from_report(origin(&owner), checkpoint, now)
        {
            break error.to_string();
        }
    };
    let error = refused;
    assert!(error.contains("sealed record history is full"), "{error}");
    assert!(
        written * WRITE < MAX_SEALED_BYTES / 4,
        "copies of the record filled the bound after {written} writes of {WRITE} bytes"
    );
    // The key holder's compaction names what it covers, so the replica drops it all for one
    // copy, and the record goes on.
    let mut holder = f.replica.clone();
    let pieces = mint_pieces(&mut holder, &keys, &owner, now).unwrap();
    for piece in pieces.iter().skip(1) {
        f.replica
            .receive_from_report(origin(&owner), piece.clone(), now)
            .unwrap();
    }
    admit(&mut f.replica, &owner, pieces[0].clone(), now);
    assert_eq!(held(&f.replica).len(), pieces.len());
    let sealed = write(&f.replica, &editor, &keys, &author.edit("value", "on"), now);
    admit(&mut f.replica, &editor, sealed, now);
}
