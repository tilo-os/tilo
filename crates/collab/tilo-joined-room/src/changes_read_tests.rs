//! Reads of only the changes since a revision the reader holds.
use super::*;
use crate::{ChangesRead, CommittedDocument};
use automerge::ObjType;
use tilo_automerge::ApplyIncoming as _;

/// Commit `edit` to the shared chat document as the member, through the
/// owner's ordinary signed write.
fn commit(joined: &Enrolled, edit: impl FnOnce(&mut AutoCommit)) {
    let request = joined
        .owner
        .with_current(joined.room, |view| {
            let mut chat = AutoCommit::load(&view.peer().document(APP, "").unwrap()).unwrap();
            let before = chat.get_heads();
            edit(&mut chat);
            chat.commit_edit("fixture", "edit");
            let message = view.peer().clone().publish_incremental(
                APP,
                "",
                chat.save_after(&before),
                &member(),
                joined.now,
            )?;
            let NeighbourMsg::AppDocument { envelope, .. } = message else {
                return Err("test edit was not a signed document write".into());
            };
            Ok(OwnerCommitRequest {
                room: ROOM.into(),
                operation: CommitOperation::Document {
                    app: APP.into(),
                    record: String::new(),
                    envelope,
                },
            })
        })
        .unwrap();
    joined.owner.commit_document(request, joined.now).unwrap();
}

fn full(joined: &Enrolled, reader: [u8; 32]) -> CommittedDocument {
    let Ok(DocumentRead::Captured(document)) = joined.owner.with_current(joined.room, |view| {
        view.read_document(reader, APP, "", None, usize::MAX, joined.now)
    }) else {
        panic!("a member reads the admitted document whole");
    };
    document
}

fn changes(
    joined: &Enrolled,
    reader: [u8; 32],
    heads: &[[u8; 32]],
    maximum: usize,
) -> Result<ChangesRead, String> {
    joined.owner.with_current(joined.room, |view| {
        view.read_changes(reader, APP, "", heads, maximum, joined.now)
    })
}

fn topic(document: &AutoCommit) -> Option<String> {
    document
        .get(ROOT, "topic")
        .unwrap()
        .map(|(value, _)| value.to_string())
}

/// The changes since a held revision, applied to that revision, are the
/// document a whole read hands out now, with its revision; and they are read
/// without restoring the room.
#[test]
fn the_changes_since_a_held_revision_complete_it_to_the_current_read() {
    let joined = enrolled();
    let member = member().verifying_key().to_bytes();
    let held = full(&joined, member);
    commit(&joined, |chat| chat.put(ROOT, "topic", "edited").unwrap());
    crate::document_read::reset_room_restores();
    let Ok(ChangesRead::Changes(read)) = changes(&joined, member, &held.heads, usize::MAX) else {
        panic!("the member is handed the changes since what it holds");
    };
    assert_eq!(
        crate::document_read::room_restores(),
        0,
        "the changes come from the committed document, not a restored room"
    );
    let current = full(&joined, member);
    assert_eq!(read.revision, current.revision);
    assert_eq!(read.heads, current.heads);
    let mut replica = AutoCommit::load(&held.bytes).unwrap();
    replica.load_incoming(&read.bytes).unwrap();
    assert!(replica.get_missing_deps(&[]).is_empty());
    let mut heads = replica
        .get_heads()
        .iter()
        .map(|head| head.0)
        .collect::<Vec<_>>();
    heads.sort_unstable();
    assert_eq!(heads, current.heads);
    assert_eq!(topic(&replica), Some("\"edited\"".into()));
    assert!(
        read.bytes.len() < current.bytes.len(),
        "{} change bytes for one edit, {} for the whole document",
        read.bytes.len(),
        current.bytes.len()
    );
    assert!(matches!(
        changes(&joined, member, &current.heads, usize::MAX),
        Ok(ChangesRead::Unchanged(revision)) if revision == current.revision
    ));
}

/// The delta path decides nothing a whole read decides differently: a
/// stranger is refused, and a base the document does not hold, heads out of
/// order or changes over the limit leave the answer to a whole read.
#[test]
fn a_changes_read_refuses_a_stranger_and_leaves_the_rest_to_a_whole_read() {
    let joined = enrolled();
    let member = member().verifying_key().to_bytes();
    let stranger = SigningKey::from_bytes(&[99; 32]).verifying_key().to_bytes();
    let held = full(&joined, member);
    commit(&joined, |chat| chat.put(ROOT, "topic", "one").unwrap());
    let between = full(&joined, member);
    commit(&joined, |chat| chat.put(ROOT, "topic", "two").unwrap());
    assert!(
        changes(&joined, stranger, &held.heads, usize::MAX).is_err(),
        "a device outside the room is refused, as by a whole read"
    );
    assert!(full_refused(&joined, stranger));
    assert!(matches!(
        changes(&joined, member, &[[7; 32]], usize::MAX),
        Ok(ChangesRead::Whole)
    ));
    let mut unordered = between.heads.clone();
    unordered.extend(&held.heads);
    unordered.sort_unstable();
    unordered.reverse();
    if unordered.len() > 1 {
        assert!(matches!(
            changes(&joined, member, &unordered, usize::MAX),
            Ok(ChangesRead::Whole)
        ));
    }
    assert!(matches!(
        changes(&joined, member, &held.heads, 1),
        Ok(ChangesRead::Whole)
    ));
    let Ok(ChangesRead::Changes(read)) = changes(&joined, member, &between.heads, usize::MAX)
    else {
        panic!("an intermediate revision is completed too");
    };
    let mut replica = AutoCommit::load(&between.bytes).unwrap();
    replica.load_incoming(&read.bytes).unwrap();
    assert_eq!(topic(&replica), Some("\"two\"".into()));
}

fn full_refused(joined: &Enrolled, reader: [u8; 32]) -> bool {
    joined
        .owner
        .with_current(joined.room, |view| {
            view.read_document(reader, APP, "", None, usize::MAX, joined.now)
        })
        .is_err()
}

/// A changes read between the frames of one checkpoint hands out nothing of
/// its own: it leaves the answer to the whole read, which says pending.
#[test]
fn a_changes_read_between_the_frames_of_one_checkpoint_is_left_to_the_whole_read() {
    let joined = enrolled();
    let member = member().verifying_key().to_bytes();
    let held = full(&joined, member);
    let governor_key = SigningKey::from_bytes(&[61; 32]);
    let covered = edit_with(&joined.owner, joined.room, joined.now, "covered next");
    joined.owner.commit_document(covered, joined.now).unwrap();
    let mut remote = joined
        .owner
        .with_current(joined.room, |view| Ok(view.peer().clone()))
        .unwrap();
    let frames = remote
        .durable_bootstrap_messages(&governor_key, joined.now, 600)
        .unwrap();
    let mut pending = 0;
    for frame in frames {
        joined
            .owner
            .receive(
                joined.room,
                TransportOrigin::RelayDelta(governor_key.verifying_key().to_bytes()),
                frame,
                joined.now,
            )
            .unwrap();
        let whole = joined.owner.with_current(joined.room, |view| {
            view.read_document(member, APP, "", None, usize::MAX, joined.now)
        });
        if matches!(whole, Ok(DocumentRead::CheckpointPending)) {
            pending += 1;
            assert!(
                matches!(
                    changes(&joined, member, &held.heads, usize::MAX),
                    Ok(ChangesRead::Whole)
                ),
                "a pending checkpoint is never answered with changes"
            );
        }
    }
    assert!(
        pending > 0,
        "the checkpoint's frames leave the room between cuts"
    );
}

/// Characters in the large document, one text op each, written in
/// [`LARGE_EDITS`] edits of its own lines, as a collaborator's typing leaves
/// a document's history.
const LARGE_CHARACTERS: usize = 50_000;
const LARGE_EDITS: usize = 100;
/// One-edit change bytes a reader may be handed for a large document.
const CHANGES_BYTES_BUDGET: usize = 1024;
/// How many times cheaper the owner's changes read must be than the whole
/// read it replaces (a room restore and a document save), on the large
/// document.
const OWNER_SPEEDUP_BUDGET: u32 = 10;
/// How many times cheaper taking in the changes must be for the reader
/// than loading and merging the whole document.
const READER_SPEEDUP_BUDGET: u32 = 3;

fn fastest<T>(runs: usize, mut run: impl FnMut() -> T) -> (std::time::Duration, T) {
    let mut best = None;
    let mut last = None;
    for _ in 0..runs {
        let started = std::time::Instant::now();
        let value = run();
        let elapsed = started.elapsed();
        best = Some(best.map_or(elapsed, |best: std::time::Duration| best.min(elapsed)));
        last = Some(value);
    }
    (best.unwrap(), last.unwrap())
}

/// What one remote edit of a large shared document costs a reader that
/// already holds it: before, a whole read (the owner restores the room and
/// saves the document; the reader loads and merges all of it); now, the
/// changes since (the owner reads the edit's changes; the reader takes them
/// in). Budgeted: bytes per edit, and the speedup on both sides.
#[test]
fn a_remote_edit_of_a_large_document_costs_its_reader_the_edit() {
    let joined = enrolled();
    let member = member().verifying_key().to_bytes();
    commit(&joined, |chat| {
        chat.put_object(ROOT, "body", ObjType::Text).unwrap();
    });
    let line_characters = LARGE_CHARACTERS / LARGE_EDITS;
    for edit in 0..LARGE_EDITS {
        commit(&joined, |chat| {
            let body = chat.get(ROOT, "body").unwrap().unwrap().1;
            let mut line = format!("{edit:04} {:x} ", edit * 7919);
            while line.len() < line_characters - 1 {
                line.push(char::from(
                    b'a' + u8::try_from((line.len() * 31 + edit) % 26).unwrap(),
                ));
            }
            line.push('\n');
            let end = chat.length(&body);
            chat.splice_text(&body, end, 0, &line).unwrap();
        });
    }
    let held = full(&joined, member);
    commit(&joined, |chat| {
        let body = chat.get(ROOT, "body").unwrap().unwrap().1;
        chat.splice_text(&body, LARGE_CHARACTERS / 2, 0, "x")
            .unwrap();
    });
    let (whole_owner, whole) = fastest(3, || {
        joined
            .owner
            .with_current(joined.room, |view| {
                view.read_document(member, APP, "", Some(held.revision), usize::MAX, joined.now)
            })
            .unwrap()
    });
    let DocumentRead::Captured(whole) = whole else {
        panic!("a whole read hands out the edited document");
    };
    let (changes_owner, read) = fastest(3, || changes(&joined, member, &held.heads, usize::MAX));
    let Ok(ChangesRead::Changes(read)) = read else {
        panic!("the reader is handed the edit's changes");
    };
    assert_eq!(read.revision, whole.revision);
    let base = AutoCommit::load(&held.bytes).unwrap();
    let mut replicas = std::iter::repeat_with(|| base.clone())
        .take(6)
        .collect::<Vec<_>>();
    let (whole_reader, ()) = fastest(3, || {
        let mut replica = replicas.pop().unwrap();
        let mut incoming = AutoCommit::load(&whole.bytes).unwrap();
        replica.merge_incoming(&mut incoming).unwrap();
    });
    let (changes_reader, mut replica) = fastest(3, || {
        let mut replica = replicas.pop().unwrap();
        replica.load_incoming(&read.bytes).unwrap();
        replica
    });
    let mut heads = replica
        .get_heads()
        .iter()
        .map(|head| head.0)
        .collect::<Vec<_>>();
    heads.sort_unstable();
    assert_eq!(heads, whole.heads, "both reads end at the same document");
    eprintln!(
        "COMMITTED-READ-LARGE characters={LARGE_CHARACTERS} edits={LARGE_EDITS} whole_bytes={} changes_bytes={} owner_whole_us={} owner_changes_us={} reader_whole_us={} reader_changes_us={}",
        whole.bytes.len(),
        read.bytes.len(),
        whole_owner.as_micros(),
        changes_owner.as_micros(),
        whole_reader.as_micros(),
        changes_reader.as_micros(),
    );
    assert!(
        read.bytes.len() <= CHANGES_BYTES_BUDGET,
        "one edit handed {} change bytes (budget {CHANGES_BYTES_BUDGET}); the whole document is {}",
        read.bytes.len(),
        whole.bytes.len()
    );
    assert!(
        changes_owner * OWNER_SPEEDUP_BUDGET <= whole_owner,
        "the owner read the changes in {changes_owner:?}, the whole document in {whole_owner:?}"
    );
    assert!(
        changes_reader * READER_SPEEDUP_BUDGET <= whole_reader,
        "the reader took the changes in in {changes_reader:?}, the whole document in {whole_reader:?}"
    );
}
