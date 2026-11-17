#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

use super::*;
use fs_protocol::{
    ContentHash, Ed25519PublicKey, FileActor, FileAttributionV1, FileProvenance,
    FileVersionAttributionV1, FileVersionId, FileVersionV1,
};
use ui::prelude::TestApp;

const PERSON: [u8; 32] = [7; 32];
const DEVICE: [u8; 32] = [9; 32];

fn notebook() -> TestApp<notes::NotesShell> {
    TestApp::<notes::NotesShell>::new().expect("Notes opens")
}

/// Every note of every part of the notes `app` holds.
fn notes_of(app: &TestApp<notes::NotesShell>) -> Vec<Note> {
    app.chain_parts::<notes::PagePart>()
        .into_iter()
        .flat_map(|(index, _)| {
            app.read_document_of(&notes::part::chain().part(index), |doc| {
                page_keys(doc)
                    .iter()
                    .filter_map(|page| note(doc, page))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
        })
        .collect()
}

/// Call the notes' `action` with `arguments`, as an agent does.
fn call(
    app: &mut TestApp<notes::NotesShell>,
    action: &str,
    arguments: Value,
) -> Result<Option<Value>, tilo_actions::ApiError> {
    app.call_on(&notes::part::chain_resource(), action, arguments)
}

/// A pass over every note and every note file, as the host's first pass over a workspace.
fn reconcile_all(tree: &mut Namespace, notes: &[Note], mtime: u64) -> Vec<NoteEdit> {
    let mut files = files_of(tree);
    let mut dirty: BTreeMap<String, Option<Note>> =
        files.keys().map(|page| (page.clone(), None)).collect();
    dirty.extend(
        notes
            .iter()
            .map(|note| (note.id.clone(), Some(note.clone()))),
    );
    reconcile(tree, &mut files, &dirty, mtime)
}

fn page_of(app: &TestApp<notes::NotesShell>, title: &str) -> String {
    notes_of(app)
        .into_iter()
        .find(|note| note.title == title)
        .expect("the note")
        .id
}

fn file(tree: &mut Namespace, path: &str) -> Option<String> {
    let ino = tree.resolve_path_ino(path)?;
    let bytes = tree.export_document(ino, &tilo_content::NoContent)?;
    Some(String::from_utf8(bytes).unwrap())
}

/// A version of node `path`'s file written by Claude Code for the person at their device,
/// as the folder records one when a file is written or deleted there.
fn written_by_agent(tree: &mut Namespace, path: &str, provenance: FileProvenance) {
    let ino = tree.resolve_path_ino(path).or_else(|| {
        // A deleted file is no longer at its path: its node is in the trash.
        let id = tree.directly_trashed().into_iter().next()?;
        tree.document_id_for_id(&id)
            .and_then(|record| tree.ino_for_document_id(record))
    });
    let record = tree
        .document_id_for(ino.expect("the file"))
        .expect("its record");
    let version = FileVersionV1 {
        id: FileVersionId(uuid::Uuid::new_v4()),
        document_id: record,
        parents: Vec::new(),
        actor: FileActor {
            person: Ed25519PublicKey(PERSON),
            device: Ed25519PublicKey(DEVICE),
        },
        timestamp_ms: 1_767_225_600_000,
        content_hash: Some(ContentHash([1; 32])),
        metadata_hash: ContentHash([2; 32]),
        provenance,
    };
    tree.append_version(version.clone()).unwrap();
    let attributed = FileVersionAttributionV1 {
        version_id: version.id,
        attribution: FileAttributionV1 {
            writer: fs_protocol::AttributedWriter::agent_session("claude", "s1"),
            label: "Claude Code".into(),
            color: [10, 20, 30],
        },
    };
    tree.attribute_version(record, &attributed).unwrap();
}

/// Apply `edits` as Notes' own actions, and record what each agreed on, as the host does.
fn apply(app: &mut TestApp<notes::NotesShell>, tree: &mut Namespace, edits: Vec<NoteEdit>) {
    for edit in edits {
        call(app, edit.action, Value::Object(edit.arguments)).unwrap();
        app.settle();
        let (node, agreed) = edit.then;
        let (source, _) = tree.projection(&node).unwrap();
        tree.set_projection(&node, &source, &agreed);
    }
}

#[test]
fn a_note_is_a_file_in_the_folder_and_the_hosts_own_write_is_no_change() {
    let mut app = notebook();
    call(
        &mut app,
        "create-page",
        json!({ "title": "Terms", "markdown": "Payment due net 30." }),
    )
    .unwrap();
    app.settle();
    let mut tree = Namespace::new("host").unwrap();
    let edits = reconcile_all(&mut tree, &notes_of(&app), 1);
    assert!(edits.is_empty(), "nothing asked of the notes");
    assert_eq!(
        file(&mut tree, "Notes/Terms.md").as_deref().map(str::trim),
        Some("Payment due net 30.")
    );
    tree.save();
    let edits = reconcile_all(&mut tree, &notes_of(&app), 2);
    assert!(edits.is_empty());
    assert!(
        tree.save_incremental().is_empty(),
        "a second pass over what agrees writes nothing: no echo"
    );
}

#[test]
fn a_file_written_in_the_folder_is_the_note_written_by_the_writer() {
    let mut app = notebook();
    call(
        &mut app,
        "create-page",
        json!({ "title": "Terms", "markdown": "Payment due net 30." }),
    )
    .unwrap();
    app.settle();
    let mut tree = Namespace::new("host").unwrap();
    reconcile_all(&mut tree, &notes_of(&app), 1);
    // An agent writes the file through the mounted folder.
    let ino = tree.resolve_path_ino("Notes/Terms.md").unwrap();
    assert!(tree.import_document(
        ino,
        b"Payment due net 45.\n\n- signed by both\n",
        3,
        &tilo_content::ContentStore::memory(),
    ));
    written_by_agent(&mut tree, "Notes/Terms.md", FileProvenance::Edit);
    let edits = reconcile_all(&mut tree, &notes_of(&app), 4);
    assert_eq!(edits.len(), 1);
    let edit = &edits[0];
    assert_eq!(edit.action, "write-note");
    assert_eq!(edit.page, page_of(&app, "Terms"));
    assert_eq!(
        edit.by.agent_name(),
        Some("Claude Code"),
        "the agent wrote it"
    );
    apply(&mut app, &mut tree, edits);
    let note = notes_of(&app).into_iter().next().unwrap();
    let body = String::from_utf8(note.markdown).unwrap();
    assert!(body.contains("net 45"), "{body}");
    assert!(body.contains("signed by both"), "{body}");
    tree.save();
    assert!(reconcile_all(&mut tree, &notes_of(&app), 5).is_empty());
    assert!(
        tree.save_incremental().is_empty(),
        "the note written from the file comes back to the file as no change"
    );
}

#[test]
fn a_note_edited_in_notes_is_written_into_its_file_and_renamed_with_it() {
    let mut app = notebook();
    call(
        &mut app,
        "create-page",
        json!({ "title": "Terms", "markdown": "Payment due net 30." }),
    )
    .unwrap();
    app.settle();
    let mut tree = Namespace::new("host").unwrap();
    reconcile_all(&mut tree, &notes_of(&app), 1);
    let page = page_of(&app, "Terms");
    call(
        &mut app,
        "append-to-note",
        json!({ "title": "Terms", "markdown": "Late fees apply." }),
    )
    .unwrap();
    call(
        &mut app,
        "rename",
        json!({ "page": page, "title": "Payment terms" }),
    )
    .unwrap();
    app.settle();
    assert!(reconcile_all(&mut tree, &notes_of(&app), 2).is_empty());
    let written = file(&mut tree, "Notes/Payment terms.md").expect("renamed with its note");
    assert!(written.contains("Late fees apply."), "{written}");
    assert!(file(&mut tree, "Notes/Terms.md").is_none());
}

#[test]
fn a_deleted_file_deletes_its_note_and_undo_brings_both_back() {
    let mut app = notebook();
    call(
        &mut app,
        "create-page",
        json!({ "title": "Terms", "markdown": "Payment due net 30." }),
    )
    .unwrap();
    app.settle();
    let mut tree = Namespace::new("host").unwrap();
    reconcile_all(&mut tree, &notes_of(&app), 1);
    let ino = tree.resolve_path_ino("Notes/Terms.md").unwrap();
    written_by_agent(&mut tree, "Notes/Terms.md", FileProvenance::Edit);
    assert!(tree.remove(ino));
    let edits = reconcile_all(&mut tree, &notes_of(&app), 2);
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].action, "delete");
    apply(&mut app, &mut tree, edits);
    assert!(notes_of(&app).is_empty(), "the note is deleted");
    assert!(reconcile_all(&mut tree, &notes_of(&app), 3).is_empty());

    assert!(app.undo(), "Notes undoes the delete");
    app.settle();
    assert_eq!(notes_of(&app).len(), 1);
    assert!(reconcile_all(&mut tree, &notes_of(&app), 4).is_empty());
    assert_eq!(
        file(&mut tree, "Notes/Terms.md").as_deref().map(str::trim),
        Some("Payment due net 30."),
        "the file is back from the trash"
    );
}

#[test]
fn a_note_deleted_in_notes_takes_its_file_to_the_trash() {
    let mut app = notebook();
    call(
        &mut app,
        "create-page",
        json!({ "title": "Terms", "markdown": "Payment due net 30." }),
    )
    .unwrap();
    app.settle();
    let mut tree = Namespace::new("host").unwrap();
    reconcile_all(&mut tree, &notes_of(&app), 1);
    let page = page_of(&app, "Terms");
    call(&mut app, "delete", json!({ "page": page })).unwrap();
    app.settle();
    assert!(reconcile_all(&mut tree, &notes_of(&app), 2).is_empty());
    assert!(file(&mut tree, "Notes/Terms.md").is_none());
}

#[test]
fn a_failed_pass_waits_longer_each_time_up_to_five_minutes() {
    let waits: Vec<u64> = [1, 2, 3, 8, 9, 40]
        .into_iter()
        .map(|failures| super::retry_after(failures).as_secs())
        .collect();
    assert_eq!(waits, [2, 4, 8, 256, 300, 300]);
}

#[test]
fn a_pass_after_one_note_changed_reads_that_note_alone() {
    let mut app = notebook();
    for n in 0..20 {
        call(
            &mut app,
            "create-page",
            json!({ "title": format!("Note {n}"), "markdown": "Some text." }),
        )
        .unwrap();
    }
    app.settle();
    let first = notes::part::chain().part(0);
    let mut kept = app.read_document_of(&first, Clone::clone).unwrap();
    let before = kept.get_heads();
    let page = page_of(&app, "Note 7");
    call(
        &mut app,
        "write-note",
        json!({ "page": page, "markdown": "Other text." }),
    )
    .unwrap();
    app.settle();
    let mut after = app.read_document_of(&first, Clone::clone).unwrap();
    let heads = after.get_heads();
    assert_eq!(
        pages_changed(&mut after, &before, &heads),
        BTreeSet::from([page.clone()]),
        "only the written note is read again"
    );
    // The service merges the change into the part it kept and reads what it touched from the
    // patches the merge logged, not by walking the part again: the same note, and no other.
    let change = after.save_after(&before);
    let mut held = HeldPart {
        doc: kept,
        heads: before.iter().map(|head| head.0).collect(),
    };
    assert_eq!(
        held.merge(
            DocumentMutation::Incremental(change),
            heads.iter().map(|head| head.0).collect()
        )
        .unwrap(),
        Some(BTreeSet::from([page])),
        "a merge of one note's change touches that note alone"
    );
}

/// Placed `(note, parent)` pairs, all in part 0.
fn placed(pairs: &[(&str, Option<&str>)]) -> Placed {
    pairs
        .iter()
        .map(|(id, parent)| ((*id).to_owned(), (0, parent.map(str::to_owned))))
        .collect()
}

/// The part `index` of the notes `app` holds, as the service keeps it between passes: its
/// document and the heads it merged last.
fn held_part(app: &TestApp<notes::NotesShell>, index: u64) -> HeldPart {
    let route = notes::part::chain().part(index);
    let doc = app.read_document_of(&route, Clone::clone).unwrap();
    let heads = doc.clone().get_heads().iter().map(|head| head.0).collect();
    HeldPart { doc, heads }
}

/// The notes and files as a first pass over `app`'s notes leaves them: every part
/// read and every note's file made.
fn followed_whole(app: &TestApp<notes::NotesShell>) -> Followed {
    let mut followed = Followed {
        parts: BTreeMap::new(),
        placed: Placed::new(),
        hidden: BTreeSet::new(),
        tree: Namespace::new("notes").unwrap(),
        chunks: None,
        files: Files::new(),
        last: 0,
    };
    let mut dirty = BTreeSet::new();
    for (index, _) in app.chain_parts::<notes::PagePart>() {
        let part = held_part(app, index);
        for page in page_keys(&part.doc) {
            if let Some(parent) = parent_in(&part.doc, &page) {
                followed.placed.insert(page.clone(), (index, parent));
            }
            dirty.insert(page);
        }
        followed.parts.insert(index, part);
    }
    followed.files = files_of(&followed.tree);
    followed.hidden = hidden(&followed.placed);
    dirty.extend(followed.files.keys().cloned());
    let notes = dirty
        .iter()
        .map(|page| (page.clone(), followed.note(page)))
        .collect();
    reconcile(&mut followed.tree, &mut followed.files, &notes, 1);
    followed
}

/// `heads` as the documents name them.
fn change_heads(heads: &[[u8; 32]]) -> Vec<automerge::ChangeHash> {
    heads.iter().copied().map(automerge::ChangeHash).collect()
}

/// One pass over the notes `app` holds now: each part merged with the changes it took since
/// the last pass, and the notes that changed read and brought into step with their files.
/// Answers how long the pass took and how many notes it read. What the workspace hands a
/// pass — each part's changes since it was read — is gathered first, untimed.
fn pass(followed: &mut Followed, app: &TestApp<notes::NotesShell>) -> (std::time::Duration, usize) {
    let mut updates = Vec::new();
    for (index, part) in &followed.parts {
        let route = notes::part::chain().part(*index);
        if app
            .read_document_of(&route, |doc| doc.clone().get_heads())
            .is_some_and(|heads| heads == change_heads(&part.heads))
        {
            continue;
        }
        let mut after = app.read_document_of(&route, Clone::clone).unwrap();
        let change = after.save_after(&change_heads(&part.heads));
        updates.push((
            *index,
            DocumentMutation::Incremental(change),
            after.get_heads(),
        ));
    }
    let began = std::time::Instant::now();
    let mut dirty = BTreeSet::new();
    for (index, mutation, heads) in updates {
        let part = followed.parts.get_mut(&index).unwrap();
        let changed = part
            .merge(mutation, heads.iter().map(|head| head.0).collect())
            .unwrap()
            .unwrap_or_default();
        dirty.extend(changed);
    }
    let notes: BTreeMap<String, Option<Note>> = dirty
        .iter()
        .map(|page| (page.clone(), followed.note(page)))
        .collect();
    reconcile(&mut followed.tree, &mut followed.files, &notes, 2);
    (began.elapsed(), notes.len())
}

/// `tree` names a file for every note `app` holds, reading what the note reads, and the
/// notes' files stay in step with the notes.
fn matches_the_notes(app: &TestApp<notes::NotesShell>, tree: &mut Namespace) -> usize {
    let notes = notes_of(app);
    assert_eq!(
        files_of(tree).len(),
        notes.len(),
        "the folder holds a file for every note and no other"
    );
    for note in &notes {
        let path = format!("Notes/{}", file_name(&note.title));
        let written = file(tree, &path).unwrap_or_default();
        assert_eq!(
            written.trim(),
            String::from_utf8_lossy(&note.markdown).trim(),
            "{path} reads what its note reads"
        );
    }
    notes.len()
}

/// A pass after one note changed in a notebook of `pages` notes: the pass reads the note
/// that changed alone, and the folder still matches the notes.
fn pass_after_one_edit(pages: usize) -> (std::time::Duration, usize) {
    let mut app = notebook();
    // Imported as a window imports a set of files, rather than a note at a time: a thousand
    // notes made one action each would cost the test more than the pass it times.
    for chunk in (0..pages).collect::<Vec<_>>().chunks(50) {
        let files: Vec<Value> = chunk
            .iter()
            .map(|n| {
                json!({
                    "path": format!("wiki/{n}.md"),
                    "title": format!("Note {n}"),
                    "markdown": format!("What note {n} says, in a sentence of its own."),
                })
            })
            .collect();
        call(&mut app, "import-markdown-files", json!({ "files": files })).unwrap();
    }
    app.settle();
    let mut followed = followed_whole(&app);
    let page = page_of(&app, "Note 5");
    call(
        &mut app,
        "write-note",
        json!({ "page": page, "markdown": "Written again, and only this one." }),
    )
    .unwrap();
    app.settle();
    let (took, read) = pass(&mut followed, &app);
    let counting = matches_the_notes(&app, &mut followed.tree);
    assert_eq!(counting, pages);
    (took, read)
}

#[test]
fn a_pass_after_one_note_changed_costs_the_note_not_the_notebook() {
    let (small, small_read) = pass_after_one_edit(10);
    let (large, large_read) = pass_after_one_edit(1_000);
    println!(
        "one note's edit: 10 notes in {small:?} reading {small_read}, 1,000 notes in \
         {large:?} reading {large_read}"
    );
    assert_eq!(small_read, 1, "a pass of ten reads the note that changed");
    assert_eq!(
        large_read, 1,
        "a pass of a thousand reads the same note alone"
    );
}

#[test]
fn a_note_under_one_no_part_keeps_is_hidden_with_the_notes_under_it() {
    let shown = placed(&[("a", None), ("b", Some("a")), ("c", Some("b"))]);
    assert!(hidden(&shown).is_empty());
    // `a` was deleted in its part: `b` and `c`, kept in another, are not shown.
    let orphaned = placed(&[("b", Some("a")), ("c", Some("b")), ("d", None)]);
    assert_eq!(
        hidden(&orphaned),
        BTreeSet::from(["b".to_owned(), "c".to_owned()])
    );
    // Notes under each other in a ring are shown, as Notes shows them.
    let ring = placed(&[("x", Some("y")), ("y", Some("x"))]);
    assert!(hidden(&ring).is_empty());
}
