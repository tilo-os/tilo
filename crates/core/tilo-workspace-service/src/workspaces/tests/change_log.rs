//! A workspace's changes kept in its change log: appended as they are made, read back over the
//! workspace file at a start whichever step of writing the file whole a part ended in.

use super::*;

/// The app whose records the test writes.
const APP: &str = "so.tilo.notes";

/// A workspace made in `dir`, as its host makes one.
fn made(dir: &std::path::Path) -> (HostedWorkspace, SigningKey) {
    let (key, cert) = hosting_device();
    let workspace = HostedWorkspace::create(
        dir,
        "Team",
        &cert,
        &key,
        Some(&crate::workspaces::fixtures::founding_device()),
        (&[], &[]),
        SystemTime::now(),
    )
    .unwrap();
    (workspace, key)
}

/// Write `record` into `workspace`'s copy as a member's write lands there, and keep it as such
/// a write is kept before it is answered. `large` makes it a record stored in a part of its own.
fn write(workspace: &mut HostedWorkspace, key: &SigningKey, record: &str, large: bool) {
    use automerge::transaction::Transactable as _;
    let mut doc = automerge::AutoCommit::new();
    let text = if large {
        let mut text = vec![0u8; 24 * 1024];
        blake3::Hasher::new()
            .update(record.as_bytes())
            .finalize_xof()
            .fill(&mut text);
        hex::encode(text)
    } else {
        record.to_owned()
    };
    doc.put(automerge::ROOT, "text", text).unwrap();
    workspace
        .peer()
        .publication_mut()
        .publish_document(APP, record, doc.save(), key, SystemTime::now())
        .unwrap();
    workspace.unkept = true;
    workspace.keep_followed();
}

/// The records `path`'s workspace holds once it opens again.
fn opened(path: &std::path::Path, records: &[&str]) -> HostedWorkspace {
    let opened = HostedWorkspace::open(path.to_owned(), None).unwrap();
    for record in records {
        assert!(
            opened.peer().document(APP, record).is_some(),
            "{record} is held after the workspace opens again"
        );
    }
    opened
}

/// The segments in `path`'s change log, by number.
fn segments(path: &std::path::Path) -> Vec<u64> {
    let mut found: Vec<u64> = std::fs::read_dir(path.with_extension("changes"))
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
                .collect()
        })
        .unwrap_or_default();
    found.sort_unstable();
    found
}

/// A member's write leaves the workspace file as it was and appends to its change log; a start
/// reads the changes over the file, large records' parts with them, and writes it whole again.
#[test]
fn a_write_is_appended_and_read_back_over_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let (mut workspace, key) = made(dir.path());
    let path = workspace.path.clone();
    let file = std::fs::read(&path).unwrap();
    write(&mut workspace, &key, "first", false);
    write(&mut workspace, &key, "large", true);
    write(&mut workspace, &key, "second", false);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        file,
        "a write leaves the workspace file as it was"
    );
    assert_eq!(
        segments(&path).len(),
        1,
        "the writes are in one segment of the log"
    );
    // The part ends as a killed process does.
    drop(workspace);

    let reopened = opened(&path, &["first", "large", "second"]);
    assert!(
        segments(&path).is_empty(),
        "the start wrote the file whole, and its log holds nothing more"
    );
    drop(reopened);
    opened(&path, &["first", "large", "second"]);
}

/// A part that ended as the file was written whole starts on everything it answered: before the
/// file was replaced, the log is read over the file before it; after, the segments the new file
/// holds are skipped by its mark.
#[test]
fn a_part_ending_inside_a_whole_write_loses_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (mut workspace, key) = made(dir.path());
    let path = workspace.path.clone();
    write(&mut workspace, &key, "first", false);
    write(&mut workspace, &key, "second", false);
    let log = path.with_extension("changes");
    let [segment] = segments(&path)[..] else {
        panic!("the writes are in one segment of the log")
    };

    // Ended after marking the segment, before the file was replaced: the mark names a file that
    // is not there, so the segment is read over the file that is.
    std::fs::write(log.join(format!("{segment}.done")), [7u8; 32]).unwrap();
    let mut reopened = opened(&path, &["first", "second"]);

    // Ended after the file was replaced, before its segments went: the mark names the file, so
    // its segment is not read again, and the change logged after it is.
    write(&mut reopened, &key, "third", false);
    let [segment] = segments(&path)[..] else {
        panic!("the start left the log empty, and the write is in one segment")
    };
    let held = std::fs::read(log.join(segment.to_string())).unwrap();
    let written = reopened.file_write().unwrap();
    let holds = written.holds;
    assert_eq!(holds, segment, "the file written whole holds the segment");
    reopened.parts.files().write(&written).unwrap();
    write(&mut reopened, &key, "fourth", false);
    std::fs::write(log.join(holds.to_string()), held).unwrap();
    std::fs::write(
        log.join(format!("{holds}.done")),
        blake3::hash(&std::fs::read(&path).unwrap()).as_bytes(),
    )
    .unwrap();
    drop(reopened);
    opened(&path, &["first", "second", "third", "fourth"]);
}

/// A whole write of a copy opened before the workspace was opened again is refused: the later
/// opening read the file and log as they were, and the older copy's file would replace what
/// it writes from then on.
#[test]
fn a_copy_opened_earlier_does_not_replace_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let (mut workspace, key) = made(dir.path());
    let path = workspace.path.clone();
    write(&mut workspace, &key, "first", false);
    let late = workspace.file_write().unwrap();

    let mut reopened = opened(&path, &["first"]);
    write(&mut reopened, &key, "second", false);
    let file = std::fs::read(&path).unwrap();
    assert!(
        workspace.parts.files().write(&late).is_err(),
        "the earlier copy's whole write is refused"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        file,
        "the file is the later opening's"
    );
    drop((workspace, reopened));
    opened(&path, &["first", "second"]);
}

/// A change cut short as the part ended was never answered: the start reads every change before
/// it.
#[test]
fn a_change_cut_short_is_left_out() {
    let dir = tempfile::tempdir().unwrap();
    let (mut workspace, key) = made(dir.path());
    let path = workspace.path.clone();
    write(&mut workspace, &key, "first", false);
    write(&mut workspace, &key, "second", false);
    drop(workspace);
    let [segment] = segments(&path)[..] else {
        panic!("the writes are in one segment of the log")
    };
    let segment = path.with_extension("changes").join(segment.to_string());
    let mut bytes = std::fs::read(&segment).unwrap();
    bytes.truncate(bytes.len() - 3);
    std::fs::write(&segment, bytes).unwrap();
    let reopened = opened(&path, &["first"]);
    assert!(reopened.peer().document(APP, "second").is_none());
}
