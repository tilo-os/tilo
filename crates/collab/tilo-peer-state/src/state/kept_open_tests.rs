//! A key holder keeps a sealed record open between reads: each read opens
//! only the writes admitted since, reads what a whole read of the same
//! history reads, and keeps nothing once the keys or the reader's standing
//! no longer allow it.
use super::*;
use crate::KeptRecord;
use std::cell::Cell;

/// The record as `reader` reads it through `kept` with `keys`, saved, and
/// how many writes that read opened.
fn read_kept(
    peer: &StatePeer,
    reader: [u8; 32],
    keys: &[&ChannelContentKeys],
    kept: &mut KeptRecord,
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

fn peer_now() -> SystemTime {
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(2000))
        .unwrap()
}

/// Open `op` with whichever of `keys` holds its epoch.
fn open_any(keys: &[&ChannelContentKeys], op: &SignedOp) -> Result<Vec<u8>, String> {
    let epoch = op.encryption.ok_or("not sealed")?.epoch;
    let keys = keys
        .iter()
        .find(|keys| keys.holds_epoch(epoch))
        .ok_or("no key of the write's epoch is held")?;
    open(keys, op)
}

/// The record as a whole read of `peer` opens it with `keys`.
fn read_whole(peer: &StatePeer, reader: [u8; 32], keys: &[&ChannelContentKeys]) -> AutoCommit {
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
    keys: &ChannelContentKeys,
    kept: &mut KeptRecord,
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

#[test]
fn a_kept_record_opens_only_the_writes_admitted_since_and_reads_as_a_whole_read_does() {
    let mut f = fixture();
    let keys = f.keys();
    let reader = f.authority.device_public_key;
    // Three writers editing at once, each from what it last saw.
    let mut first = Author::new(&f.host);
    let mut second = Author::new(&f.host);
    let mut third = Author::new(&f.host);
    let mut kept = KeptRecord::default();
    let message = write(&f.host, &f.owner, &keys, &first.edit("value", "one"), f.now);
    admit(&mut f.host, &f.owner, message, f.now);
    let (mut replica, opened) = read_kept(&f.host, reader, &[&keys], &mut kept).unwrap();
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
        let (mut document, opened) = read_kept(&f.host, reader, &[&keys], &mut kept).unwrap();
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

#[test]
fn a_compacted_history_is_opened_whole_again_and_reads_alike() {
    let mut f = fixture();
    let keys = f.keys();
    let reader = f.authority.device_public_key;
    let mut author = Author::new(&f.host);
    let mut kept = KeptRecord::default();
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

#[test]
fn a_write_sealed_after_a_rotation_opens_only_with_the_rotated_keys() {
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
    let mut stale = KeptRecord::default();
    let mut current = KeptRecord::default();
    read_kept(&f.host, reader, &[&before], &mut stale).unwrap();
    read_kept(&f.host, reader, &[&before], &mut current).unwrap();
    let after = f.rotate();
    let message = write(
        &f.host,
        &f.owner,
        &after,
        &author.edit("value", SECRET),
        f.now,
    );
    admit(&mut f.host, &f.owner, message, f.now);
    // Keys from before the rotation (a member it removed holds no others)
    // open nothing new, and keep nothing open.
    assert!(matches!(
        read_kept(&f.host, reader, &[&before], &mut stale),
        Err(crate::PeerDocumentReadError::Opening(_))
    ));
    assert!(
        !stale.is_open(),
        "a read its keys refuse forgets what it kept"
    );
    // The rotated keys open the new write alone, and read as a whole read.
    let (mut document, opened) =
        read_kept(&f.host, reader, &[&before, &after], &mut current).unwrap();
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

#[test]
fn a_member_removed_from_the_workspace_reads_nothing_and_keeps_nothing_open() {
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
    let mut kept = KeptRecord::default();
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
