//! A workspace record's large state: what leaves the workspace file for a
//! file of its own, and when those files are written.

use super::*;

/// A record's large state is stored in files of its own: a governance change
/// that does not touch it writes the workspace file alone, a change of it
/// replaces the parts it changed, and the workspace opens to what it committed.
#[test]
fn a_large_record_is_stored_apart_and_written_only_when_it_changes() {
    use automerge::transaction::Transactable as _;
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let mut workspace =
        GovernedWorkspace::create(dir.path(), "Team", &cert, &key, None, SystemTime::now())
            .unwrap();
    // Incompressible text, so the saved record stays large.
    let mut text = vec![0u8; 48 * 1024];
    blake3::Hasher::new()
        .update(b"large record")
        .finalize_xof()
        .fill(&mut text);
    let large = || {
        let mut doc = automerge::AutoCommit::new();
        doc.put(automerge::ROOT, "text", hex::encode(&text))
            .unwrap();
        doc.save()
    };
    let state = workspace.collaboration().clone();
    let mut candidate = workspace.peer().clone();
    candidate
        .publication_mut()
        .publish_document("notes", "plan", large(), &key, SystemTime::now())
        .unwrap();
    workspace
        .commit(candidate, &key, SystemTime::now(), |_, _| Ok(state.clone()))
        .unwrap();
    let stored = |workspace: &GovernedWorkspace| {
        workspace
            .record
            .peer
            .iter()
            .filter(|part| matches!(part, peer_parts::PeerPart::Stored { .. }))
            .cloned()
            .collect::<Vec<_>>()
    };
    let first = stored(&workspace);
    assert!(
        !first.is_empty(),
        "the large record is stored apart from the workspace file"
    );
    assert_eq!(
        workspace.parts.files(),
        first.len(),
        "every large part of the record has a file of its own, and only those"
    );
    let modified = |workspace: &GovernedWorkspace| {
        std::fs::read_dir(workspace.path.with_extension("parts"))
            .unwrap()
            .map(|entry| entry.unwrap().metadata().unwrap().modified().unwrap())
            .collect::<Vec<_>>()
    };
    let written = modified(&workspace);

    // A change that leaves the record alone does not write it again.
    let candidate = workspace.peer().clone();
    workspace
        .commit(candidate, &key, SystemTime::now(), |_, _| Ok(state.clone()))
        .unwrap();
    assert_eq!(stored(&workspace), first);
    assert_eq!(modified(&workspace), written);

    // Changing it stores it anew and removes what no workspace file names.
    let mut candidate = workspace.peer().clone();
    let mut edited =
        automerge::AutoCommit::load(&workspace.peer().document("notes", "plan").unwrap()).unwrap();
    edited.put(automerge::ROOT, "edited", true).unwrap();
    candidate
        .publication_mut()
        .publish_document("notes", "plan", edited.save(), &key, SystemTime::now())
        .unwrap();
    workspace
        .commit(candidate, &key, SystemTime::now(), |_, _| Ok(state.clone()))
        .unwrap();
    let second = stored(&workspace);
    assert_eq!(second.len(), first.len());
    assert_ne!(second, first);
    assert_eq!(
        workspace.parts.files(),
        second.len(),
        "the parts it replaced are removed"
    );

    let restored = GovernedWorkspace::open(workspace.path.clone(), &cert, None).unwrap();
    assert_eq!(
        restored.peer().document("notes", "plan"),
        workspace.peer().document("notes", "plan")
    );
    assert_eq!(restored.record.generation, workspace.record.generation);

    // A stored part that does not match its hash refuses to open.
    let peer_parts::PeerPart::Stored { digest, .. } = &second[0] else {
        unreachable!()
    };
    let path = workspace
        .path
        .with_extension("parts")
        .join(hex::encode(digest));
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[0] ^= 1;
    std::fs::write(&path, bytes).unwrap();
    assert!(GovernedWorkspace::open(workspace.path.clone(), &cert, None).is_err());
}
