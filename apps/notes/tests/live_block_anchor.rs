//! `LIVE_V1`: a Notes caret is anchored to the block's stable CRDT id, which
//! both peers read from the shared notebook, so a remote caret keeps naming
//! the same block after blocks are inserted above it on the receiver.

use crate::component_support::{NotebookFixture, driver, notebook, open_note};
use notes::{NotesApp, NotesShell};
use ui::collaboration::PresencePayload;
use ui::framework::testing::with_surface;
use ui::presence::PresenceKey;
use ui::widget::block_editor::{Block, CursorPlacement};

#[test]
fn remote_notes_caret_names_the_same_block_on_both_peers_across_inserted_blocks() {
    // Two windows over one notebook record, both on the same note.
    let f = NotebookFixture::rich();
    let checkout = notebook(&f);
    let mut a = driver(&f, 11, &checkout);
    let mut b = driver(&f, 12, &checkout);
    for (window, instance) in [(&mut a, 11), (&mut b, 12)] {
        open_note(window, instance, f.child, None);
    }

    let (key, target, text) = with_surface::<NotesShell, NotesApp, _>(&mut a, |notes| {
        let editor_a = notes.main_panel_mut().editor_mut();
        assert!(editor_a.blocks().len() >= 2, "the note has several blocks");
        let target = editor_a.blocks()[1].id.clone();
        let text = editor_a.blocks()[1].line.value().to_string();
        editor_a.set_focus(true);
        editor_a.focus_block(1, CursorPlacement::Byte(2));
        let (key, payload) = ui::presence::capability::presence_payload(editor_a)
            .expect("focused block publishes a caret");
        assert!(
            matches!(&key, PresenceKey::Resource { id, .. } if *id == target),
            "anchored by block id, not index: {key:?}"
        );
        assert_eq!(payload, PresencePayload::Caret { pos: 2 });
        (key, target, text)
    })
    .unwrap();

    with_surface::<NotesShell, NotesApp, _>(&mut b, |notes| {
        // B read the same stable ids from the shared notebook ...
        let editor_b = notes.main_panel_mut().editor_mut();
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
            .position(|block| matches!(&key, PresenceKey::Resource { id, .. } if *id == block.id))
            .expect("the anchor resolves on the receiver");
        assert_eq!(resolved, 2);
        assert_eq!(editor_b.blocks()[resolved].line.value(), text);
    })
    .unwrap();
}
