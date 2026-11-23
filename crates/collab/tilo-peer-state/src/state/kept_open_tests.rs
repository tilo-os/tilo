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
        let mut first = Author::new(&f.replica);
        let mut second = Author::new(&f.replica);
        let mut third = Author::new(&f.replica);
        let mut kept = KeptDocument::default();
        let message = write(&f.replica, &f.owner, &keys, &first.edit("value", "one"), f.now);
        admit(&mut f.replica, &f.owner, message, f.now);
        let (mut replica, opened) = read_kept(&f.replica, reader, &[&keys], &mut kept).unwrap();
        assert_eq!(opened, 1);
        for round in 0..4 {
            // What a reader that merged the last read holds.
            let held = sorted_heads(&mut replica);
            let message = write(
                &f.replica,
                &f.other,
                &keys,
                &second.edit("value", &format!("second {round}")),
                f.now,
            );
            admit(&mut f.replica, &f.other, message, f.now);
            let message = write(
                &f.replica,
                &f.editor,
                &keys,
                &third.edit(&format!("third {round}"), "x"),
                f.now,
            );
            admit(&mut f.replica, &f.editor, message, f.now);
            let message = write(
                &f.replica,
                &f.owner,
                &keys,
                &first.edit("value", &format!("first {round}")),
                f.now,
            );
            admit(&mut f.replica, &f.owner, message, f.now);
            let (mut document, opened) = read_kept(&f.replica, reader, &[&keys], &mut kept).unwrap();
            assert_eq!(opened, 3, "round {round} opens only its own writes");
            let mut whole = read_whole(&f.replica, reader, &[&keys]);
            same(&mut document, &mut whole);
            // A capture of what is kept extends the last one, and loads as the whole read does.
            let captured = kept.capture(1024 * 1024).unwrap().unwrap();
            same(&mut AutoCommit::load(&captured).unwrap(), &mut whole);
            // The changes after what the reader held complete it to the same.
            let changes = changes_after(&f.replica, reader, &keys, &mut kept, &held)
                .expect("the record holds the reader's heads");
            replica.load_incoming(&changes).unwrap();
            assert!(replica.get_missing_deps(&[]).is_empty());
            same(&mut replica, &mut whole);
            assert!(
                changes_after(&f.replica, reader, &keys, &mut kept, &sorted_heads(&mut whole))
                    .is_none_or(|changes| changes.is_empty()),
                "nothing follows the current heads"
            );
        }
        // Nothing admitted: nothing opened.
        let (_, opened) = read_kept(&f.replica, reader, &[&keys], &mut kept).unwrap();
        assert_eq!(opened, 0);
    }

    phase a_compacted_history_is_opened_whole_again_and_reads_alike;
    {
        let mut f = fixture();
        let keys = f.keys();
        let reader = f.authority.device_public_key;
        let mut author = Author::new(&f.replica);
        let mut kept = KeptDocument::default();
        for value in ["one", "two"] {
            let message = write(
                &f.replica,
                &f.owner,
                &keys,
                &author.edit("value", value),
                f.now,
            );
            admit(&mut f.replica, &f.owner, message, f.now);
        }
        read_kept(&f.replica, reader, &[&keys], &mut kept).unwrap();
        mint(&mut f.replica, &keys, &f.owner, f.now).unwrap();
        let message = write(
            &f.replica,
            &f.owner,
            &keys,
            &author.edit("value", "three"),
            f.now,
        );
        admit(&mut f.replica, &f.owner, message, f.now);
        let (mut document, opened) = read_kept(&f.replica, reader, &[&keys], &mut kept).unwrap();
        assert_eq!(opened, 2, "the compaction and the write after it, whole");
        same(&mut document, &mut read_whole(&f.replica, reader, &[&keys]));
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
        let mut author = Author::new(&f.replica);
        let message = write(
            &f.replica,
            &f.owner,
            &before,
            &author.edit("value", "before"),
            f.now,
        );
        admit(&mut f.replica, &f.owner, message, f.now);
        let mut kept = KeptDocument::default();
        read_kept(&f.replica, reader, &[&before], &mut kept).unwrap();
        let after = f.rotate();
        let message = write(
            &f.replica,
            &f.owner,
            &after,
            &author.edit("value", SECRET),
            f.now,
        );
        admit(&mut f.replica, &f.owner, message, f.now);
        // Keys from before the rotation open nothing new, and hide nothing read before.
        let (document, _) = read_kept(&f.replica, reader, &[&before], &mut kept).unwrap();
        assert_eq!(
            document.get(ROOT, "value").unwrap().unwrap().0.as_str(),
            Some("before")
        );
        // The rotated keys open the new write alone, and read as a whole read.
        let (mut document, opened) = read_kept(&f.replica, reader, &[&before, &after], &mut kept).unwrap();
        assert_eq!(opened, 1);
        same(
            &mut document,
            &mut read_whole(&f.replica, reader, &[&before, &after]),
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
        let mut author = Author::new(&f.replica);
        let message = write(
            &f.replica,
            &f.owner,
            &keys,
            &author.edit("value", "seen"),
            f.now,
        );
        admit(&mut f.replica, &f.owner, message, f.now);
        let mut kept = KeptDocument::default();
        read_kept(&f.replica, editor.device_public_key, &[&keys], &mut kept).unwrap();
        assert!(kept.is_open());
        assert!(
            f.replica
                .governance_mut()
                .remove_member(editor.user_public_key, &f.owner, f.now)
                .unwrap()
        );
        let message = write(
            &f.replica,
            &f.owner,
            &keys,
            &author.edit("value", SECRET),
            f.now,
        );
        admit(&mut f.replica, &f.owner, message, f.now);
        // Even holding keys that would open the new write, the removed member's
        // read is refused before anything is opened, and what it kept is gone.
        let opened = Cell::new(0_usize);
        let refused = f.replica.open_document(
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

    phase a_read_refused_while_the_history_is_still_arriving_keeps_what_opened;
    {
        let f = fixture();
        let keys = f.keys();
        let reader = f.authority.device_public_key;
        let Fixture {
            replica: mut peer,
            owner,
            now,
            ..
        } = f;
        let mut author = Author::new(&peer);
        let mut kept = KeptDocument::default();
        for value in ["one", "two", "three"] {
            let message = write(&peer, &owner, &keys, &author.edit("value", value), now);
            admit(&mut peer, &owner, message, now);
        }
        let (_, opened) = read_kept(&peer, reader, &[&keys], &mut kept).unwrap();
        assert_eq!(opened, 3);
        // The workspace's admissions are not all here yet, as while a page catches up: the read
        // is refused, and what it opened before stays kept.
        peer.core.exact_selected = false;
        assert!(matches!(
            read_kept(&peer, reader, &[&keys], &mut kept),
            Err(crate::PeerDocumentReadError::Incomplete)
        ));
        assert!(kept.is_open(), "a history still arriving keeps what opened");
        peer.core.exact_selected = true;
        let message = write(&peer, &owner, &keys, &author.edit("value", "four"), now);
        admit(&mut peer, &owner, message, now);
        let (mut document, opened) = read_kept(&peer, reader, &[&keys], &mut kept).unwrap();
        assert_eq!(opened, 1, "only the write admitted since, not the history again");
        same(&mut document, &mut read_whole(&peer, reader, &[&keys]));
    }

    phase a_write_whose_segments_are_still_arriving_keeps_what_opened_before_it;
    {
        let mut f = fixture();
        let keys = f.keys();
        let reader = f.authority.device_public_key;
        let mut author = Author::new(&f.replica);
        let mut kept = KeptDocument::default();
        let message = write(&f.replica, &f.owner, &keys, &author.edit("value", "one"), f.now);
        admit(&mut f.replica, &f.owner, message, f.now);
        read_kept(&f.replica, reader, &[&keys], &mut kept).unwrap();
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
        let message = write(&f.replica, &f.owner, &keys, first, f.now);
        admit(&mut f.replica, &f.owner, message, f.now);
        // The record reads as it did before the write began to arrive.
        let (read, opened) = read_large(&f.replica, reader, &keys, &mut kept);
        let document = read.unwrap();
        assert!(document.get(ROOT, "file").unwrap().is_none());
        assert_eq!(
            document.get(ROOT, "value").unwrap().unwrap().0.as_str(),
            Some("one")
        );
        assert_eq!(opened, 1, "the first segment alone");
        assert!(kept.is_open(), "what opened before the segments stays kept");
        // Read again while the rest has not come: nothing is opened again.
        let (read, opened) = read_large(&f.replica, reader, &keys, &mut kept);
        assert!(read.is_ok());
        assert_eq!(opened, 0, "a read while segments arrive opens nothing twice");
        for frame in rest {
            let message = write(&f.replica, &f.owner, &keys, frame, f.now);
            admit(&mut f.replica, &f.owner, message, f.now);
        }
        let (read, opened) = read_large(&f.replica, reader, &keys, &mut kept);
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
        let mut author = Author::new(&f.replica);
        let message = write(&f.replica, &f.owner, &keys, &author.edit("value", "one"), f.now);
        admit(&mut f.replica, &f.owner, message, f.now);
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
        let message = write(&f.replica, &f.owner, &keys, &frames[0], f.now);
        admit(&mut f.replica, &f.owner, message, f.now);
        let compacted = |peer: &StatePeer| {
            super::super::sealed_compaction::merge_sealed(
                &peer.core.documents[&route()],
                &mut |op| open(&keys, op),
                &mut |_, _| Ok(()),
            )
        };
        assert!(
            compacted(&f.replica).is_err(),
            "while the rest may still come, a compaction does not leave the write out"
        );
        // The writer, started again, writes on: the rest of the file never comes.
        let message = write(&f.replica, &f.owner, &keys, &again.edit("value", "two"), f.now);
        admit(&mut f.replica, &f.owner, message, f.now);
        let whole = AutoCommit::load(&compacted(&f.replica).unwrap()).unwrap();
        assert_eq!(
            whole.get(ROOT, "value").unwrap().unwrap().0.as_str(),
            Some("two")
        );
        for kept in [&mut KeptDocument::default(), &mut KeptDocument::default()] {
            let (read, _) = read_large(&f.replica, reader, &keys, kept);
            let document = read.expect("the record reads without the write that was stopped");
            assert_eq!(
                document.get(ROOT, "value").unwrap().unwrap().0.as_str(),
                Some("two")
            );
            assert!(document.get(ROOT, "file").unwrap().is_none());
        }
    }

    phase a_sealed_change_made_for_a_caller_is_checked_when_a_key_holder_opens_it;
    {
        let mut f = fixture();
        let keys = f.keys();
        let caller = f
            .replica
            .owner_certificates()
            .into_iter()
            .find(|cert| cert.device_public_key == f.owner.verifying_key().to_bytes())
            .unwrap();
        let reader = f.authority.device_public_key;

        let plaintext = caller_edit(&f.replica, &caller, &f.owner, f.now, "forged", false);
        let message = write(&f.replica, &f.owner, &keys, &plaintext, f.now);
        admit(&mut f.replica, &f.owner, message, f.now);
        let read = read_kept(&f.replica, reader, &[&keys], &mut KeptDocument::default());
        assert!(
            matches!(read, Err(crate::PeerDocumentReadError::Refused(_))),
            "an unsigned caller's sealed change is refused when it is opened: {:?}",
            read.map(|_| ())
        );

        let mut f = fixture();
        let keys = f.keys();
        let caller = f
            .replica
            .owner_certificates()
            .into_iter()
            .find(|cert| cert.device_public_key == f.owner.verifying_key().to_bytes())
            .unwrap();
        let plaintext = caller_edit(&f.replica, &caller, &f.owner, f.now, "signed", true);
        let message = write(&f.replica, &f.owner, &keys, &plaintext, f.now);
        admit(&mut f.replica, &f.owner, message, f.now);
        let (document, _) = read_kept(
            &f.replica,
            f.authority.device_public_key,
            &[&keys],
            &mut KeptDocument::default(),
        )
        .expect("a sealed change its caller signed reads");
        assert_eq!(
            document.get(ROOT, "value").unwrap().unwrap().0.as_str(),
            Some("signed")
        );
    }
}

/// The plaintext of a change by `key`'s device for the caller its entry names,
/// on the record: `cert`'s device signs the invocation when `signed`.
fn caller_edit(
    peer: &StatePeer,
    cert: &DeviceCert,
    key: &SigningKey,
    now: SystemTime,
    value: &str,
    signed: bool,
) -> Vec<u8> {
    use tilo_actions::remote::{InvokeArgument, InvokeRequest, RemoteActionTarget, SignedCall};
    use tilo_actions::{ActionAddress, ActionContext, ActionInvoker, InvocationId, InvokerDevice};

    let mut author = Author::new(peer);
    let heads = author.0.get_heads();
    author.0.put(ROOT, "value", value).unwrap();
    let invocation = InvocationId::new();
    let mut request = InvokeRequest {
        invocation,
        target: RemoteActionTarget {
            provider: "so.tilo.test".into(),
            address: ActionAddress::new(RECORD, "field-edit"),
        },
        argument: InvokeArgument::Text(value.into()),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let signed = signed
        .then(|| {
            request.signed = Some(Box::new(
                tilo_server_protocol::invocation::sign_invocation_now(
                    peer.workspace_id(),
                    &request,
                    cert,
                    key,
                    u64::try_from(
                        now.duration_since(SystemTime::UNIX_EPOCH)
                            .unwrap()
                            .as_nanos(),
                    )
                    .unwrap(),
                )
                .unwrap(),
            ));
            SignedCall::of(&request).map(Box::new)
        })
        .flatten();
    let context = ActionContext {
        invoker: Some(
            ActionInvoker::person(hex::encode(cert.user_public_key)).from_device(Some(
                InvokerDevice::certified(cert.device_public_key, "the caller's device"),
            )),
        ),
        source_invocation: Some(invocation),
        signed,
        ..ActionContext::for_resource(RECORD, "test", "Test")
    };
    author.0.commit_with(
        automerge::transaction::CommitOptions::default()
            .with_message(context.change_message().unwrap()),
    );
    DocumentMutation::Incremental(author.0.save_after(&heads))
        .encode()
        .unwrap()
}

/// What one read of the document made within a turn did.
struct Turn {
    /// The document, once it read whole.
    read: Option<AutoCommit>,
    /// Whether the read stopped for its turn.
    stopped: bool,
    /// How many writes it opened.
    opened: usize,
    /// How long the read held the thread.
    took: std::time::Duration,
}

/// `reader`'s read of the document through `kept`, made within a turn `due` ends, or outside any
/// turn without `due`.
fn read_in_turn(
    peer: &StatePeer,
    reader: [u8; 32],
    keys: &ContentKeys,
    kept: &mut KeptDocument,
    due: Option<fn(std::time::Instant) -> bool>,
) -> Turn {
    let opened = Cell::new(0_usize);
    let began = std::time::Instant::now();
    let counted = &opened;
    let kept_here = &mut *kept;
    let mut read = move || {
        peer.open_document(
            reader,
            (APP, RECORD),
            peer_now(),
            8 * 1024 * 1024,
            Some(&mut |op| {
                counted.set(counted.get().saturating_add(1));
                open(keys, op)
            }),
            kept_here,
        )
        .map(|_| ())
    };
    let (read, stopped) = match due {
        Some(due) => crate::opening::in_turns(move || due(began), read),
        None => (read(), false),
    };
    let took = began.elapsed();
    let read = match read {
        Ok(()) => Some(
            AutoCommit::load(
                &kept
                    .capture(8 * 1024 * 1024)
                    .expect("a read keeps the document")
                    .unwrap(),
            )
            .unwrap(),
        ),
        Err(crate::PeerDocumentReadError::Incomplete) => None,
        Err(error) => panic!("the document reads: {error}"),
    };
    Turn {
        read,
        stopped,
        opened: opened.get(),
        took,
    }
}

/// A newcomer's first read of a document with a long history, made a turn at a time as a page
/// makes it, opens a few writes per turn and keeps them, and reads only once whole: then as one
/// whole read does. Outside a turn the same read opens all of it at once.
#[test]
fn a_long_history_read_in_turns_opens_a_few_writes_a_turn_and_reads_as_a_whole_read() {
    // As many as a document holds before a key holder must compact it, in a test build.
    const WRITES: usize = super::super::MAX_SEALED_OPS - 6;
    let mut f = fixture();
    let keys = f.keys();
    let reader = f.authority.device_public_key;
    // Two writers, each writing on from what it wrote.
    let mut first = Author::new(&f.replica);
    let mut second = Author::new(&f.replica);
    for n in 0..WRITES {
        let (author, signer) = if n % 3 == 0 {
            (&mut second, &f.other)
        } else {
            (&mut first, &f.owner)
        };
        let message = write(
            &f.replica,
            signer,
            &keys,
            &author.edit(&format!("issue {n}"), &format!("written {n}")),
            f.now,
        );
        admit(&mut f.replica, signer, message, f.now);
    }
    let at_once = read_in_turn(
        &f.replica,
        reader,
        &keys,
        &mut KeptDocument::default(),
        None,
    );
    assert_eq!(
        at_once.opened, WRITES,
        "outside a turn, one read opens every write"
    );
    let mut whole = at_once.read.expect("the document reads whole");

    // Every turn over as soon as it is asked: each read opens one look's writes.
    let look = super::super::document_capture::WRITES_PER_LOOK;
    let mut kept = KeptDocument::default();
    let mut turns = 0_usize;
    let mut document = loop {
        turns += 1;
        let turn = read_in_turn(&f.replica, reader, &keys, &mut kept, Some(|_| true));
        assert!(
            turn.opened <= look,
            "turn {turns} opened {} writes",
            turn.opened
        );
        match turn.read {
            Some(document) => {
                assert!(!turn.stopped, "a read that answers did not stop");
                break document;
            }
            None => assert!(turn.stopped, "a read answered not yet stopped for its turn"),
        }
    };
    assert_eq!(
        turns,
        WRITES.div_ceil(look),
        "every turn went on where the last stopped"
    );
    same(&mut document, &mut whole);

    // Turns of a frame's work, as a page's: the longest turn against the read at once.
    let mut kept = KeptDocument::default();
    let mut longest = std::time::Duration::ZERO;
    let mut total = std::time::Duration::ZERO;
    let mut turns = 0_usize;
    let mut document = loop {
        turns += 1;
        let turn = read_in_turn(
            &f.replica,
            reader,
            &keys,
            &mut kept,
            Some(|began| began.elapsed() >= std::time::Duration::from_millis(8)),
        );
        longest = longest.max(turn.took);
        total += turn.took;
        if let Some(document) = turn.read {
            break document;
        }
    };
    eprintln!(
        "{WRITES} writes: at once {:?}; in {turns} turns {total:?}, the longest {longest:?}",
        at_once.took
    );
    same(&mut document, &mut whole);
}
