//! `LIVE_V1`: a Notes caret is anchored to the block's stable CRDT id, which
//! both peers read from the shared notebook, so a remote caret keeps naming
//! the same block after blocks are inserted above it on the receiver.

use automerge::{AutoCommit, ROOT};

use notes::NotesApp;
use ui::Widget;
use ui::collaboration::PresencePayload;
use ui::widget::block_editor::{Block, CursorPlacement, block_presence_key};

fn spawn_app() -> (NotesApp, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(dir.path().to_path_buf()).unwrap();
    app.bypass_seed_grace_for_test();
    (app, dir)
}

#[test]
fn remote_notes_caret_names_the_same_block_on_both_peers_across_inserted_blocks() {
    let (mut a, _da) = spawn_app();
    let mut doc_a = AutoCommit::new();
    doc_a.set_actor(automerge::ActorId::from(b"alice".as_slice()));
    a.sync(&mut doc_a, &ROOT);
    let (mut b, _db) = spawn_app();
    let mut doc_b = AutoCommit::load(&doc_a.save()).unwrap();
    doc_b.set_actor(automerge::ActorId::from(b"bob".as_slice()));
    b.sync(&mut doc_b, &ROOT);

    // A puts its caret in the second block of the shared starter page.
    let editor_a = a.main_panel_mut().editor_mut();
    assert!(
        editor_a.blocks().len() >= 2,
        "starter page has several blocks"
    );
    let target = editor_a.blocks()[1].id.clone();
    let text = editor_a.blocks()[1].line.value().to_string();
    editor_a.set_focus(true);
    editor_a.focus_block(1, CursorPlacement::Byte(2));
    let (key, payload) = ui::presence::capability::presence_payload(editor_a)
        .expect("focused block publishes a caret");
    assert_eq!(
        key,
        block_presence_key(&target),
        "anchored by block id, not index"
    );
    assert_eq!(payload, PresencePayload::Caret { pos: 2 });

    // B read the same stable ids from the shared notebook ...
    let editor_b = b.main_panel_mut().editor_mut();
    let ids_b: Vec<_> = editor_b
        .blocks()
        .iter()
        .map(|block| block.id.clone())
        .collect();
    assert_eq!(ids_b.get(1), Some(&target));
    // ... and after a block lands above the target the anchor still names it.
    editor_b
        .blocks_mut()
        .insert(0, Block::paragraph("inserted above"));
    let resolved = editor_b
        .blocks()
        .iter()
        .position(|block| block_presence_key(&block.id) == key)
        .expect("the anchor resolves on the receiver");
    assert_eq!(resolved, 2);
    assert_eq!(editor_b.blocks()[resolved].line.value(), text);
}
