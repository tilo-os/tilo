//! Two-peer convergence for the notes app's CRDT integration.
//!
//! These drive `NotesApp::sync` against hand-held Automerge docs to
//! exercise the seed/adopt page-tree logic and per-block text
//! convergence WITHOUT a windowing system. The broker / protocol / merge
//! layers are unit-tested elsewhere (`real_socket_n_peer`,
//! `doc::tests`, `block_editor` tests, `app::crdt_update_merges`); this
//! checks that the notes app wires them together.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ReadDoc, ROOT};

use notes::doc::NotesDoc;
use notes::NotesApp;
use ui::Widget;

/// A fresh notes app rooted at a temp dir, with the seed grace window
/// collapsed so the first `sync` seeds immediately.
fn spawn_app() -> (NotesApp, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut app = NotesApp::spawn(dir.path().to_path_buf()).unwrap();
    app.bypass_seed_grace_for_test();
    (app, dir)
}

#[test]
fn sync_seeds_welcome_page_into_doc() {
    let (mut app, _dir) = spawn_app();
    let mut doc = AutoCommit::new();
    app.sync(&mut doc, &ROOT);

    let pages = NotesDoc::list_pages(&doc);
    assert!(!pages.is_empty(), "sync seeds the welcome page tree into the doc");

    let sel = app.selected_for_test().expect("a page is selected after sync");
    let list = NotesDoc::page_blocks_list(&mut doc, sel).unwrap();
    assert!(doc.length(&list) > 0, "the selected page is seeded with blocks");
}

#[test]
fn second_peer_adopts_without_duplicating_welcome() {
    // Peer A seeds.
    let (mut a, _da) = spawn_app();
    let mut doc = AutoCommit::new();
    a.sync(&mut doc, &ROOT);
    let a_count = NotesDoc::list_pages(&doc).len();
    assert!(a_count >= 1);

    // Peer B joins from A's state (the late-join handshake hands B a copy
    // of A's full doc). B must adopt it, not push its own welcome.
    let (mut b, _db) = spawn_app();
    let mut doc_b = AutoCommit::load(&doc.save()).unwrap();
    b.sync(&mut doc_b, &ROOT);

    assert_eq!(
        NotesDoc::list_pages(&doc_b).len(),
        a_count,
        "B adopts A's page tree instead of duplicating the welcome page"
    );
}

#[test]
fn text_edit_in_peer_a_converges_to_peer_b() {
    // Peer A seeds and selects a page. Distinct, fixed actor ids mirror
    // production (main.rs sets the actor from QUICK_APP_ID) so the test
    // exercises the same CRDT author topology two real instances do.
    let (mut a, _da) = spawn_app();
    let mut doc_a = AutoCommit::new();
    doc_a.set_actor(automerge::ActorId::from(b"alice".as_slice()));
    a.sync(&mut doc_a, &ROOT);
    let sel = a.selected_for_test().unwrap();

    // Peer B joins from A's full state and adopts.
    let (mut b, _db) = spawn_app();
    let mut doc_b = AutoCommit::load(&doc_a.save()).unwrap();
    doc_b.set_actor(automerge::ActorId::from(b"bob".as_slice()));
    b.sync(&mut doc_b, &ROOT);

    // A "types": splice into the selected page's first block Text in A's
    // doc — exactly the CRDT op A's keystroke produces through the bound
    // editor.
    let list = NotesDoc::page_blocks_list(&mut doc_a, sel).unwrap();
    let t0 = NotesDoc::block_text_obj(&doc_a, &list, 0).expect("block 0 has text");
    doc_a.splice_text(&t0, 0, 0, "ZZZ-").unwrap();

    // The broker delivers A's update to B (a merge of A's full state).
    let mut a_loaded = AutoCommit::load(&doc_a.save()).unwrap();
    doc_b.merge(&mut a_loaded).unwrap();
    b.sync(&mut doc_b, &ROOT);

    let b_text = b.editor_text_for_test();
    assert!(
        b_text.contains("ZZZ-"),
        "B's editor should show A's inserted text, got: {b_text:?}"
    );
}
