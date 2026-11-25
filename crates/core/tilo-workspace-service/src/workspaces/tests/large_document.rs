//! A workspace record's large state: what leaves the workspace file for a
//! file of its own, and when those files are written.

use super::*;

/// The app whose record the test writes: a record is published only under an application id.
const APP: &str = "so.tilo.notes";

/// A record's large state is stored in files of its own: a governance change
/// that does not touch it writes the workspace file alone, a change of it
/// replaces the parts it changed, and the workspace opens to what it committed.
#[test]
fn a_large_record_is_stored_apart_and_written_only_when_it_changes() {
    use automerge::transaction::Transactable as _;
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let mut workspace = HostedWorkspace::create(
        dir.path(),
        "Team",
        &cert,
        &key,
        Some(&crate::workspaces::fixtures::founding_device()),
        (&[], &[]),
        SystemTime::now(),
    )
    .unwrap();
    // Incompressible text, so the saved record stays large.
    let mut text = vec![0u8; 48 * 1024];
    blake3::Hasher::new()
        .update(b"large record")
        .finalize_xof()
        .fill(&mut text);
    let large = || the_large_record(&text);
    let state = workspace.collaboration().clone();
    let mut candidate = workspace.peer().clone();
    candidate
        .publication_mut()
        .publish_document(APP, "plan", large(), &key, SystemTime::now())
        .unwrap();
    workspace
        .keep(candidate, &key, SystemTime::now(), |_, _| Ok(state.clone()))
        .unwrap();
    let first = stored(&workspace);
    assert!(
        !first.is_empty(),
        "the large record is stored apart from the workspace file"
    );
    assert_eq!(
        workspace.parts.file_count(),
        first.len(),
        "every large part of the record has a file of its own, and only those"
    );
    let written = written_at(&workspace);

    // A change that leaves the record alone does not write it again.
    let candidate = workspace.peer().clone();
    workspace
        .keep(candidate, &key, SystemTime::now(), |_, _| Ok(state.clone()))
        .unwrap();
    assert_eq!(stored(&workspace), first);
    assert_eq!(written_at(&workspace), written);

    // Changing it stores it anew and removes what no workspace file names.
    let mut candidate = workspace.peer().clone();
    let mut edited =
        automerge::AutoCommit::load(&workspace.peer().document(APP, "plan").unwrap()).unwrap();
    edited.put(automerge::ROOT, "edited", true).unwrap();
    candidate
        .publication_mut()
        .publish_document(APP, "plan", edited.save(), &key, SystemTime::now())
        .unwrap();
    workspace
        .keep(candidate, &key, SystemTime::now(), |_, _| Ok(state.clone()))
        .unwrap();
    let second = stored(&workspace);
    assert_eq!(second.len(), first.len());
    assert_ne!(second, first);
    assert_eq!(
        workspace.parts.file_count(),
        second.len(),
        "the parts it replaced are removed"
    );

    let restored = HostedWorkspace::open(workspace.path.clone(), None).unwrap();
    assert_eq!(
        restored.peer().document(APP, "plan"),
        workspace.peer().document(APP, "plan")
    );
    assert_eq!(
        restored.peer().governance().generation(),
        workspace.peer().governance().generation()
    );

    // A stored part that does not match its hash refuses to open.
    a_part_whose_hash_no_longer_matches_refuses_to_open(&workspace, &second);
}

/// The record's `text` as a large document: text no compression makes small.
fn the_large_record(text: &[u8]) -> Vec<u8> {
    use automerge::transaction::Transactable as _;
    let mut doc = automerge::AutoCommit::new();
    doc.put(automerge::ROOT, "text", hex::encode(text)).unwrap();
    doc.save()
}

/// Every part of `workspace`'s record that is stored in a file of its own.
fn stored(workspace: &HostedWorkspace) -> Vec<peer_parts::PeerPart> {
    workspace
        .record
        .peer
        .iter()
        .filter(|part| matches!(part, peer_parts::PeerPart::Stored { .. }))
        .cloned()
        .collect()
}

/// When each of the record's stored parts was last written.
fn written_at(workspace: &HostedWorkspace) -> Vec<SystemTime> {
    std::fs::read_dir(workspace.path.with_extension("parts"))
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().modified().unwrap())
        .collect()
}

/// A stored part whose bytes no longer match its hash keeps `workspace` from opening.
fn a_part_whose_hash_no_longer_matches_refuses_to_open(
    workspace: &HostedWorkspace,
    parts: &[peer_parts::PeerPart],
) {
    let Some(peer_parts::PeerPart::Stored { digest, .. }) = parts.first() else {
        unreachable!("the record is stored in parts")
    };
    let path = workspace
        .path
        .with_extension("parts")
        .join(hex::encode(digest));
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[0] ^= 1;
    std::fs::write(&path, bytes).unwrap();
    assert!(HostedWorkspace::open(workspace.path.clone(), None).is_err());
}
