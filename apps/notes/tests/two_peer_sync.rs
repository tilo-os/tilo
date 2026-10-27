//! Collaboration through the notebook record: a collaborator's edit reaches
//! the open note, a duplicate carries its bodies with it, and the window
//! tells others which note it shows.

use crate::framework_support::{body, surface, with_note};
use notes::read;
use notes::sidebar::OutEvent;
use ui::collaboration::PresencePayload;
use ui::presence::PresenceKey;

#[test]
fn presence_reports_the_open_note_as_the_tab() {
    let (mut app, page) = with_note("Shared", "Hello");
    let presence = surface(&mut app, |notes| {
        ui::presence::capability::presence_payload(notes)
    });
    match presence {
        Some((PresenceKey::Tab { id }, PresencePayload::Arrived)) => {
            assert_eq!(id, page.to_string());
        }
        other => panic!("expected Tab location presence, got {other:?}"),
    }
}

#[test]
fn peer_edits_and_duplicates_replicate_before_the_other_peer_opens_them() {
    eprintln!("phase a_collaborators_text_edit_reaches_the_open_note");
    {
        let (mut app, page) = with_note("Shared", "Hello");
        app.merge_peer_edit(|doc| {
            let first = read::page_block_snapshots(doc, page)[0].0.clone();
            crate::component_support::edit_page_body(
                doc,
                page,
                &tilo_document::RichTextAction::SpliceText {
                    region: tilo_document::action::RegionKind::Body,
                    block: first,
                    at: 0,
                    del: 0,
                    ins: "ZZZ-".into(),
                },
            )
            .unwrap();
        });
        let shown = surface(&mut app, |notes| notes.editor_text_for_test());
        assert!(
            shown.contains("ZZZ-Hello"),
            "the open note shows the collaborator's text, got: {shown:?}"
        );
    }

    eprintln!("phase duplicate_carries_every_new_page_body");
    {
        let (mut app, source) = with_note("Welcome", "First\n\nSecond");
        let source_blocks = app.read_document(|doc| read::page_block_snapshots(doc, source));
        // The sidebar's context-menu path: the copy and its bodies are one
        // action, so nobody has to open the copy for its body to exist.
        surface(&mut app, |notes| {
            notes.apply_event_for_test(OutEvent::Duplicate(source));
        });
        let copy = crate::framework_support::shown(&app)
            .pages()
            .iter()
            .find(|page| page.title == "Welcome copy")
            .expect("duplicate metadata")
            .id;
        let copied_blocks = app.read_document(|doc| read::page_block_snapshots(doc, copy));
        assert_eq!(
            body(&app, copy),
            body(&app, source),
            "copy body came with its metadata"
        );
        assert_ne!(
            copied_blocks[0].0, source_blocks[0].0,
            "copies mint independent stable block ids"
        );
        let replica = app.peer_document();
        assert_eq!(read::page_block_snapshots(&replica, copy), copied_blocks);
    }
}
