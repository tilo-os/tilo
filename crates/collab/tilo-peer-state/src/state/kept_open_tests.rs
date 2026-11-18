//! A key holder keeps a sealed record open between reads: each read opens
//! only the writes admitted since, reads what a whole read of the same
//! history reads, and keeps nothing once the keys or the reader's access
//! no longer allow it.
use super::*;
use crate::KeptDocument;
use std::cell::Cell;

/// The record as `reader` reads it through `kept` with `keys`, saved, and
/// how many writes that read opened.
fn read_kept(
    peer: &StatePeer,
    reader: [u8; 32],
    keys: &[&ContentKeys],
    kept: &mut KeptDocument,
) -> Result<(AutoCommit, usize), crate::PeerDocumentReadError> {
    let opened = Cell::new(0_usize);
    let document = peer.open_document(
        reader,
        (APP, RECORD),
        peer_now(),
        1024 * 1024,
        Some(&mut |op| {
            opened.set(opened.get().saturating_add(1));
            open_any(keys, op)
        }),
        kept,
    )?;
    let document = AutoCommit::load(&document.save()).unwrap();
    Ok((document, opened.get()))
}

/// [`read_kept`] with room for a write larger than one frame.
fn read_large(
    peer: &StatePeer,
    reader: [u8; 32],
    keys: &ContentKeys,
    kept: &mut KeptDocument,
) -> (Result<AutoCommit, crate::PeerDocumentReadError>, usize) {
    let opened = Cell::new(0_usize);
    let read = peer
        .open_document(
            reader,
            (APP, RECORD),
            peer_now(),
            8 * 1024 * 1024,
            Some(&mut |op| {
                opened.set(opened.get().saturating_add(1));
                open(keys, op)
            }),
            kept,
        )
        .map(|document| AutoCommit::load(&document.save()).unwrap());
    (read, opened.get())
}

fn peer_now() -> SystemTime {
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(2000))
        .unwrap()
}

/// Open `op` with whichever of `keys` holds its epoch.
fn open_any(keys: &[&ContentKeys], op: &SignedOp) -> Result<Vec<u8>, String> {
    let epoch = op.encryption.ok_or("not sealed")?.epoch;
    let keys = keys
        .iter()
        .find(|keys| keys.holds_epoch(epoch))
        .ok_or("no key of the write's epoch is held")?;
    open(keys, op)
}

/// The record as a whole read of `peer` opens it with `keys`.
fn read_whole(peer: &StatePeer, reader: [u8; 32], keys: &[&ContentKeys]) -> AutoCommit {
    let capture = peer
        .capture_document(
            reader,
            (APP, RECORD),
            peer_now(),
            1024 * 1024,
            Some(&mut |op| open_any(keys, op)),
        )
        .unwrap();
    AutoCommit::load(&capture.into_bytes()).unwrap()
}

/// The changes of the record after `heads`, as the reader keeping `kept` reads them.
fn changes_after(
    peer: &StatePeer,
    reader: [u8; 32],
    keys: &ContentKeys,
    kept: &mut KeptDocument,
    heads: &[[u8; 32]],
) -> Option<Vec<u8>> {
    let document = peer
        .open_document(
            reader,
            (APP, RECORD),
            peer_now(),
            1024 * 1024,
            Some(&mut |op| open(keys, op)),
            kept,
        )
        .ok()?;
    let base = heads
        .iter()
        .copied()
        .map(automerge::ChangeHash)
        .collect::<Vec<_>>();
    super::changes_since(document, &base, 1024 * 1024).ok()
}

fn sorted_heads(document: &mut AutoCommit) -> Vec<[u8; 32]> {
    let mut heads = document
        .get_heads()
        .iter()
        .map(|head| head.0)
        .collect::<Vec<_>>();
    heads.sort_unstable();
    heads
}

fn same(kept: &mut AutoCommit, whole: &mut AutoCommit) {
    let mut kept_heads = kept.get_heads();
    let mut whole_heads = whole.get_heads();
    kept_heads.sort();
    whole_heads.sort();
    assert_eq!(
        kept_heads, whole_heads,
        "the kept read stands where a whole read does"
    );
    for key in whole.keys(ROOT).chain(kept.keys(ROOT)) {
        assert_eq!(
            kept.get_all(ROOT, key.as_str()).unwrap(),
            whole.get_all(ROOT, key.as_str()).unwrap(),
            "{key} reads alike"
        );
    }
}

tilo_test_env::scenario! {
    fn a_kept_record_opens_what_was_admitted_since_and_reads_as_a_whole_read;

    phase a_kept_record_opens_only_the_writes_admitted_since_and_reads_as_a_whole_read_does;
    {
        let mut f = fixture();
        let keys = f.keys();
        let reader = f.authority.device_public_key;
        // Three writers editing at once, each from what it last saw.
        let mut first = Author::new(&f.host);
        let mut second = Author::new(&f.host);
        let mut third = Author::new(&f.host);
        let mut kept = KeptDocument::default();
        let message = write(&f.host, &f.owner, &keys, &first.edit("value", "one"), f.now);
        admit(&mut f.host, &f.owner, message, f.now);
        let (mut replica, opened) = read_kept(peer(&f), reader, &[&keys], &mut kept).unwrap();
        assert_eq!(opened, 1);
        for round in 0..4 {
            // What a reader that merged the last read holds.
            let held = sorted_heads(&mut replica);
            let message = write(
                &f.host,
                &f.other,
                &keys,
                &second.edit("value", &format!("second {round}")),
                f.now,
            );
            admit(&mut f.host, &f.other, message, f.now);
            let message = write(
                &f.host,
                &f.editor,
                &keys,
                &third.edit(&format!("third {round}"), "x"),
                f.now,
            );
            admit(&mut f.host, &f.editor, message, f.now);
            let message = write(
                &f.host,
                &f.owner,
                &keys,
                &first.edit("value", &format!("first {round}")),
                f.now,
            );
            admit(&mut f.host, &f.owner, message, f.now);
            let (mut document, opened) = read_kept(peer(&f), reader, &[&keys], &mut kept).unwrap();
            assert_eq!(opened, 3, "round {round} opens only its own writes");
            let mut whole = read_whole(&f.host, reader, &[&keys]);
            same(&mut document, &mut whole);
            // A capture of what is kept extends the last one, and loads as the whole read does.
            let captured = kept.capture(1024 * 1024).unwrap().unwrap();
            same(&mut AutoCommit::load(&captured).unwrap(), &mut whole);
            // The changes after what the reader held complete it to the same.
            let changes = changes_after(&f.host, reader, &keys, &mut kept, &held)
                .expect("the record holds the reader's heads");
            replica.load_incoming(&changes).unwrap();
            assert!(replica.get_missing_deps(&[]).is_empty());
            same(&mut replica, &mut whole);
            assert!(
                changes_after(&f.host, reader, &keys, &mut kept, &sorted_heads(&mut whole))
                    .is_none_or(|changes| changes.is_empty()),
                "nothing follows the current heads"
            );
        }
        // Nothing admitted: nothing opened.
        let (_, opened) = read_kept(&f.host, reader, &[&keys], &mut kept).unwrap();
        assert_eq!(opened, 0);
    }

    phase a_compacted_history_is_opened_whole_again_and_reads_alike;
    {
        let mut f = fixture();
        let keys = f.keys();
        let reader = f.authority.device_public_key;
        let mut author = Author::new(&f.host);
        let mut kept = KeptDocument::default();
        for value in ["one", "two"] {
            let message = write(
                &f.host,
                &f.owner,
                &keys,
                &author.edit("value", value),
                f.now,
            );
            admit(&mut f.host, &f.owner, message, f.now);
        }
        read_kept(&f.host, reader, &[&keys], &mut kept).unwrap();
        mint(&mut f.host, &keys, &f.owner, f.now).unwrap();
        let message = write(
            &f.host,
            &f.owner,
            &keys,
            &author.edit("value", "three"),
            f.now,
        );
        admit(&mut f.host, &f.owner, message, f.now);
        let (mut document, opened) = read_kept(&f.host, reader, &[&keys], &mut kept).unwrap();
        assert_eq!(opened, 2, "the compaction and the write after it, whole");
        same(&mut document, &mut read_whole(&f.host, reader, &[&keys]));
        assert_eq!(
            document.get(ROOT, "value").unwrap().unwrap().0.as_str(),
            Some("three")
        );
    }

    phase a_write_sealed_after_a_rotation_waits_for_the_rotated_keys_and_hides_nothing_before_it;
    {
        let mut f = fixture();
        let before = f.keys();
        let reader = f.authority.device_public_key;
        let mut author = Author::new(&f.host);
        let message = write(
            &f.host,
            &f.owner,
            &before,
            &author.edit("value", "before"),
            f.now,
        );
        admit(&mut f.host, &f.owner, message, f.now);
        let mut kept = KeptDocument::default();
        read_kept(&f.host, reader, &[&before], &mut kept).unwrap();
        let after = f.rotate();
        let message = write(
            &f.host,
            &f.owner,
            &after,
            &author.edit("value", SECRET),
            f.now,
        );
        admit(&mut f.host, &f.owner, message, f.now);
        // Keys from before the rotation open nothing new, and hide nothing read before.
        let (document, _) = read_kept(&f.host, reader, &[&before], &mut kept).unwrap();
        assert_eq!(
            document.get(ROOT, "value").unwrap().unwrap().0.as_str(),
            Some("before")
        );
        // The rotated keys open the new write alone, and read as a whole read.
        let (mut document, opened) = read_kept(&f.host, reader, &[&before, &after], &mut kept).unwrap();
        assert_eq!(opened, 1);
        same(
            &mut document,
            &mut read_whole(&f.host, reader, &[&before, &after]),
        );
        assert_eq!(
            document.get(ROOT, "value").unwrap().unwrap().0.as_str(),
            Some(SECRET)
        );
    }

    phase a_member_removed_from_the_workspace_reads_nothing_and_keeps_nothing_open;
    {
        let mut f = fixture();
        let keys = f.keys();
        let (_, editor) = device(110, 111);
        let mut author = Author::new(&f.host);
        let message = write(
            &f.host,
            &f.owner,
            &keys,
            &author.edit("value", "seen"),
            f.now,
        );
        admit(&mut f.host, &f.owner, message, f.now);
        let mut kept = KeptDocument::default();
        read_kept(&f.host, editor.device_public_key, &[&keys], &mut kept).unwrap();
        assert!(kept.is_open());
        assert!(
            f.host
                .governance_mut()
                .remove_member(editor.user_public_key, &f.owner, f.now)
                .unwrap()
        );
        let message = write(
            &f.host,
            &f.owner,
            &keys,
            &author.edit("value", SECRET),
            f.now,
        );
        admit(&mut f.host, &f.owner, message, f.now);
        // Even holding keys that would open the new write, the removed member's
        // read is refused before anything is opened, and what it kept is gone.
        let opened = Cell::new(0_usize);
        let refused = f.host.open_document(
            editor.device_public_key,
            (APP, RECORD),
            f.now,
            1024 * 1024,
            Some(&mut |op| {
                opened.set(opened.get().saturating_add(1));
                open(&keys, op)
            }),
            &mut kept,
        );
        assert!(matches!(
            refused,
            Err(crate::PeerDocumentReadError::Membership(_))
        ));
        assert_eq!(opened.get(), 0, "nothing is opened for a removed member");
        assert!(!kept.is_open(), "nothing stays open for a removed member");
    }

    phase a_write_whose_segments_are_still_arriving_keeps_what_opened_before_it;
    {
        let mut f = fixture();
        let keys = f.keys();
        let reader = f.authority.device_public_key;
        let mut author = Author::new(&f.host);
        let mut kept = KeptDocument::default();
        let message = write(&f.host, &f.owner, &keys, &author.edit("value", "one"), f.now);
        admit(&mut f.host, &f.owner, message, f.now);
        read_kept(&f.host, reader, &[&keys], &mut kept).unwrap();
        // A file larger than one frame goes out as segments, each a sealed write of its own.
        let mut noise = 0x9e37_79b9_u32;
        let file = (0..collab_types::document_transfer::DOCUMENT_SEGMENT_BYTES * 3 / 2)
            .map(|_| {
                noise ^= noise << 13;
                noise ^= noise >> 17;
                noise ^= noise << 5;
                noise.to_le_bytes()[0]
            })
            .collect::<Vec<_>>();
        let frames = DocumentMutation::decode(&author.edit_bytes("file", file.clone()))
            .unwrap()
            .encode_frames()
            .unwrap();
        assert!(frames.len() > 1, "the write goes out as segments");
        let (first, rest) = frames.split_first().unwrap();
        let message = write(&f.host, &f.owner, &keys, first, f.now);
        admit(&mut f.host, &f.owner, message, f.now);
        // The record reads as it did before the write began to arrive.
        let (read, opened) = read_large(&f.host, reader, &keys, &mut kept);
        let document = read.unwrap();
        assert!(document.get(ROOT, "file").unwrap().is_none());
        assert_eq!(
            document.get(ROOT, "value").unwrap().unwrap().0.as_str(),
            Some("one")
        );
        assert_eq!(opened, 1, "the first segment alone");
        assert!(kept.is_open(), "what opened before the segments stays kept");
        // Read again while the rest has not come: nothing is opened again.
        let (read, opened) = read_large(&f.host, reader, &keys, &mut kept);
        assert!(read.is_ok());
        assert_eq!(opened, 0, "a read while segments arrive opens nothing twice");
        for frame in rest {
            let message = write(&f.host, &f.owner, &keys, frame, f.now);
            admit(&mut f.host, &f.owner, message, f.now);
        }
        let (read, opened) = read_large(&f.host, reader, &keys, &mut kept);
        let document = read.unwrap();
        assert_eq!(opened, rest.len(), "only the segments that came since");
        assert_eq!(
            document.get(ROOT, "file").unwrap().unwrap().0.as_bytes(),
            Some(file.as_slice())
        );
        assert_eq!(
            document.get(ROOT, "value").unwrap().unwrap().0.as_str(),
            Some("one")
        );
    }

    phase a_write_its_writer_stopped_sending_is_left_out_once_it_wrote_on;
    {
        let mut f = fixture();
        let keys = f.keys();
        let reader = f.authority.device_public_key;
        let mut author = Author::new(&f.host);
        let message = write(&f.host, &f.owner, &keys, &author.edit("value", "one"), f.now);
        admit(&mut f.host, &f.owner, message, f.now);
        // The record as the workspace holds it, which the writer reads when it starts again.
        let mut again = Author(author.0.fork());
        // A file larger than one frame, of which only the first segment ever reached the
        // workspace: its writer ended while the rest was on its way.
        let file = noise(collab_types::document_transfer::DOCUMENT_SEGMENT_BYTES * 3 / 2, 7);
        let frames = DocumentMutation::decode(&author.edit_bytes("file", file))
            .unwrap()
            .encode_frames()
            .unwrap();
        assert!(frames.len() > 1, "the write goes out as segments");
        let message = write(&f.host, &f.owner, &keys, &frames[0], f.now);
        admit(&mut f.host, &f.owner, message, f.now);
        let compacted = |host: &StatePeer| {
            super::super::sealed_compaction::merge_sealed(
                &host.core.documents[&route()],
                &mut |op| open(&keys, op),
            )
        };
        assert!(
            compacted(&f.host).is_err(),
            "while the rest may still come, a compaction does not leave the write out"
        );
        // The writer, started again, writes on: the rest of the file never comes.
        let message = write(&f.host, &f.owner, &keys, &again.edit("value", "two"), f.now);
        admit(&mut f.host, &f.owner, message, f.now);
        let whole = AutoCommit::load(&compacted(&f.host).unwrap()).unwrap();
        assert_eq!(
            whole.get(ROOT, "value").unwrap().unwrap().0.as_str(),
            Some("two")
        );
        for kept in [&mut KeptDocument::default(), &mut KeptDocument::default()] {
            let (read, _) = read_large(&f.host, reader, &keys, kept);
            let document = read.expect("the record reads without the write that was stopped");
            assert_eq!(
                document.get(ROOT, "value").unwrap().unwrap().0.as_str(),
                Some("two")
            );
            assert!(document.get(ROOT, "file").unwrap().is_none());
        }
    }
}

/// The peer of `f` that holds the document.
fn peer(f: &Fixture) -> &StatePeer {
    &f.host
}

/// One more sealed write of the owner, `edit`, admitted.
fn seal(f: &mut Fixture, keys: &ContentKeys, edit: &[u8]) {
    let message = write(peer(f), &f.owner, keys, edit, f.now);
    admit(&mut f.host, &f.owner, message, f.now);
}

/// A document whose sealed history holds `writes` writes of one author, each a change on the
/// one before: with its keys, that author, and the bytes of the writes together.
fn history_of(writes: usize) -> (Fixture, ContentKeys, Author, usize) {
    let mut f = fixture();
    let keys = f.keys();
    let mut author = Author::new(peer(&f));
    let mut bytes = 0_usize;
    for number in 0..writes {
        let edit = author.edit("value", &format!("write {number}"));
        bytes = bytes.saturating_add(edit.len());
        seal(&mut f, &keys, &edit);
    }
    (f, keys, author, bytes)
}

/// What a read of the document through `kept` asks of the isolated worker: how many calls, and
/// the bytes they carry. Counted on this thread alone, by the work done.
fn asked_by_a_read(
    peer: &StatePeer,
    reader: [u8; 32],
    keys: &ContentKeys,
    kept: &mut KeptDocument,
) -> tilo_automerge_isolate::Asked {
    let before = tilo_automerge_isolate::asked_by_this_thread();
    read_kept(peer, reader, &[keys], kept).unwrap();
    let after = tilo_automerge_isolate::asked_by_this_thread();
    tilo_automerge_isolate::Asked {
        calls: after.calls.saturating_sub(before.calls),
        bytes: after.bytes.saturating_sub(before.bytes),
    }
}

tilo_test_env::scenario! {
    fn a_read_costs_the_isolated_worker_no_more_for_a_longer_history;

    phase a_history_ten_times_as_long_opens_in_as_many_calls_of_the_isolated_worker;
    {
        let (short, short_keys, _, _) = history_of(12);
        let (long, long_keys, _, long_bytes) = history_of(120);
        let reader = short.authority.device_public_key;
        let of_short = asked_by_a_read(
            peer(&short),
            reader,
            &short_keys,
            &mut KeptDocument::default(),
        );
        let of_long = asked_by_a_read(
            peer(&long),
            reader,
            &long_keys,
            &mut KeptDocument::default(),
        );
        assert_eq!(
            of_short.calls, 1,
            "a history of twelve writes is merged in one call: {of_short:?}"
        );
        assert_eq!(
            of_long.calls, 1,
            "and so is one of a hundred and twenty: {of_long:?}"
        );
        assert!(
            of_long.bytes <= u64::try_from(long_bytes).unwrap(),
            "the history's bytes pass the worker once: {of_long:?} of {long_bytes}"
        );
        // Merged as one, it reads as a whole read of the same history does.
        let (mut document, opened) = read_kept(
            peer(&long),
            reader,
            &[&long_keys],
            &mut KeptDocument::default(),
        )
        .unwrap();
        assert_eq!(opened, 120);
        same(
            &mut document,
            &mut read_whole(peer(&long), reader, &[&long_keys]),
        );
        assert!(
            document.get_changes(&[]).len() >= 120,
            "every write is in it"
        );
    }

    phase a_kept_document_asks_nothing_for_a_read_and_one_call_for_a_write_whatever_its_history;
    {
        for writes in [12, 120] {
            let (mut f, keys, mut author, _) = history_of(writes);
            let reader = f.authority.device_public_key;
            let mut kept = KeptDocument::default();
            asked_by_a_read(peer(&f), reader, &keys, &mut kept);
            assert_eq!(
                asked_by_a_read(peer(&f), reader, &keys, &mut kept),
                tilo_automerge_isolate::Asked::default(),
                "nothing was sealed since: a read of {writes} kept writes asks nothing"
            );
            let edit = author.edit("value", "one more");
            seal(&mut f, &keys, &edit);
            let of_write = asked_by_a_read(peer(&f), reader, &keys, &mut kept);
            assert_eq!(
                of_write.calls, 1,
                "one write sealed since is one call: {of_write:?}"
            );
            assert!(
                of_write.bytes <= u64::try_from(edit.len()).unwrap(),
                "it carries that write alone, nothing of the {writes} before: {of_write:?}"
            );
            // What its holder reads once it let the document go for memory: nothing kept. The
            // same document as the kept read, and one call for its whole history.
            let mut gone = KeptDocument::default();
            let of_gone = asked_by_a_read(peer(&f), reader, &keys, &mut gone);
            assert_eq!(
                of_gone.calls, 1,
                "a document let go is opened again in one call: {of_gone:?}"
            );
            let (mut kept_read, _) = read_kept(peer(&f), reader, &[&keys], &mut kept).unwrap();
            let (mut read_again, _) = read_kept(peer(&f), reader, &[&keys], &mut gone).unwrap();
            same(&mut kept_read, &mut read_again);
        }
    }
}
