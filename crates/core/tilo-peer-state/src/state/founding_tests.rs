//! A replica that joins with nothing kept of its own takes a document only as
//! the key holder's compaction of it, and numbers its own writes past the fold.
use super::*;

/// A replica that founds a document from a key holder's compaction holds the writes the fold
/// covers as decided already: its own next write of the document must be numbered past the fold,
/// never at one the fold decided, which its owner can only answer as a write it holds already
/// but has no ownership of.
#[test]
fn a_replica_founding_a_sealed_document_from_a_compaction_numbers_its_next_write_past_the_fold() {
    let Fixture {
        replica: mut server,
        owner,
        other,
        authority,
        group,
        now,
        ..
    } = fixture();
    let keys = group.content_keys().unwrap();
    let mut author = Author::new(&server);
    let mut covered = 0;
    for index in 0..3 {
        let sealed = write(
            &server,
            &owner,
            &keys,
            &author.edit(&format!("k{index}"), "v"),
            now,
        );
        covered = envelope_of(&sealed).seq;
        admit(&mut server, &owner, sealed, now);
    }
    // The owner's second device compacts the kept history: the fold covers every write the
    // owner's first device made, and this replica held none of them as themselves.
    let mut holder = server.clone();
    let pieces = mint_pieces(&mut holder, &keys, &other, now).unwrap();
    for piece in pieces.iter().skip(1) {
        server
            .receive_from_report(origin(&other), piece.clone(), now)
            .unwrap();
    }
    admit(&mut server, &other, pieces[0].clone(), now);

    // The owner's first device joins with nothing kept of its own: the compaction is its whole
    // history of the document, the writes its own device made only inside the fold.
    let mut fresh =
        StatePeer::new(WORKSPACE, authority, vec![], RevocationList::new(), now).unwrap();
    fresh.select_exact_admissions().unwrap();
    fresh
        .receive_from(origin(&owner), server.workspace_message(), now)
        .unwrap();
    for piece in &pieces {
        fresh
            .receive_from_report(origin(&other), piece.clone(), now)
            .unwrap();
    }
    assert_eq!(
        held(&fresh).len(),
        pieces.len(),
        "the compaction is the document's whole history here"
    );

    // Its next write is numbered past the writes the fold covers.
    let sealed = write(&fresh, &owner, &keys, &author.edit("after", "v"), now);
    assert_eq!(
        envelope_of(&sealed).seq,
        covered.checked_add(1).unwrap(),
        "the next write is numbered past the fold"
    );
    admit(&mut fresh, &owner, sealed, now);
}
