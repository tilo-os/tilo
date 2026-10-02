//! Two-peer convergence for the notes app's CRDT integration.
//!
//! These drive `NotesApp::sync` against hand-held Automerge docs to
//! exercise the seed/adopt page-tree logic and per-block text
//! convergence WITHOUT a windowing system. The broker / protocol / merge
//! layers are unit-tested elsewhere (`real_socket_n_peer`,
//! `doc::tests`, `block_editor` tests, `app::crdt_update_merges`); this
//! checks that the notes app wires them together.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use automerge::{AutoCommit, ROOT};
use tilo_automerge::ApplyIncoming;

use notes::NotesApp;
use notes::doc::NotesDoc;
use notes::sidebar::OutEvent;
use ui::Widget;
use ui::collaboration::PresencePayload;
use ui::presence::PresenceKey;

/// A fresh notes app rooted at a temp dir, with the seed grace window
/// collapsed so the first `sync` seeds immediately.
fn spawn_app() -> Result<(NotesApp, tempfile::TempDir), String> {
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let mut app = NotesApp::spawn_with_starter_page(dir.path().to_path_buf())?;
    app.bypass_seed_grace_for_test();
    Ok((app, dir))
}

#[test]
fn peers_seed_adopt_and_report_presence_without_duplicating_welcome() {
    eprintln!("phase sync_seeds_welcome_page_into_doc");
    {
        let (mut app, _dir) = spawn_app().unwrap();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);

        let pages = NotesDoc::list_pages(&doc);
        assert!(
            !pages.is_empty(),
            "sync seeds the welcome page tree into the doc"
        );

        let sel = app
            .selected_for_test()
            .expect("a page is selected after sync");
        assert!(
            NotesDoc::page_block_snapshots(&doc, sel)
                .iter()
                .any(|(_, text)| !text.is_empty()),
            "the selected page is seeded with blocks"
        );
    }

    eprintln!("phase second_peer_adopts_without_duplicating_welcome");
    {
        // Peer A seeds.
        let (mut a, _da) = spawn_app().unwrap();
        let mut doc = AutoCommit::new();
        a.sync(&mut doc, &ROOT);
        let a_count = NotesDoc::list_pages(&doc).len();
        assert!(a_count >= 1);

        // Peer B joins from A's state (the late-join handshake hands B a copy
        // of A's full doc). B must adopt it, not push its own welcome.
        let (mut b, _db) = spawn_app().unwrap();
        let mut doc_b = AutoCommit::load(&doc.save()).unwrap();
        b.sync(&mut doc_b, &ROOT);

        assert_eq!(
            NotesDoc::list_pages(&doc_b).len(),
            a_count,
            "B adopts A's page tree instead of duplicating the welcome page"
        );
    }

    eprintln!("phase presence_payload_reports_current_page_as_tab");
    {
        // The location-presence the App broadcasts (and peers render as
        // indicative mist) is the Tab key for the page this instance views.
        let (mut app, _d) = spawn_app().unwrap();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        let sel = app.selected_for_test().expect("a page is selected");
        match ui::presence::capability::presence_payload(&app) {
            Some((PresenceKey::Tab { id }, PresencePayload::Arrived)) => {
                assert_eq!(id, sel.to_string());
            }
            other => panic!("expected Tab location presence, got {other:?}"),
        }
    }
}

#[test]
fn peer_edits_and_duplicates_replicate_before_the_other_peer_opens_them() {
    eprintln!("phase text_edit_in_peer_a_converges_to_peer_b");
    {
        // Peer A seeds and selects a page. Distinct, fixed actor ids mirror
        // production (main.rs sets the actor from QUICK_APP_ID) so the test
        // exercises the same CRDT author topology two real instances do.
        let (mut a, _da) = spawn_app().unwrap();
        let mut doc_a = AutoCommit::new();
        doc_a.set_actor(automerge::ActorId::from(b"alice".as_slice()));
        a.sync(&mut doc_a, &ROOT);
        let sel = a.selected_for_test().unwrap();

        // Peer B joins from A's full state and adopts.
        let (mut b, _db) = spawn_app().unwrap();
        let mut doc_b = AutoCommit::load(&doc_a.save()).unwrap();
        doc_b.set_actor(automerge::ActorId::from(b"bob".as_slice()));
        b.sync(&mut doc_b, &ROOT);

        // A "types": splice into the selected page's first block Text in A's
        // doc — exactly the CRDT op A's keystroke produces through the bound
        // editor.
        let first = NotesDoc::page_block_snapshots(&doc_a, sel)[0].0.clone();
        NotesDoc::edit_page_body(
            &mut doc_a,
            sel,
            &tilo_document::RichTextAction::SpliceText {
                region: tilo_document::action::RegionKind::Body,
                block: first,
                at: 0,
                del: 0,
                ins: "ZZZ-".into(),
            },
        )
        .unwrap();

        // The broker delivers A's update to B (a merge of A's full state).
        let mut a_loaded = AutoCommit::load(&doc_a.save()).unwrap();
        doc_b.merge_incoming(&mut a_loaded).unwrap();
        b.sync(&mut doc_b, &ROOT);

        let b_text = b.editor_text_for_test();
        assert!(
            b_text.contains("ZZZ-"),
            "B's editor should show A's inserted text, got: {b_text:?}"
        );
    }

    eprintln!("phase duplicate_replicates_every_new_page_body_before_any_peer_opens_it");
    {
        let (mut a, _da) = spawn_app().unwrap();
        let mut doc_a = AutoCommit::new();
        a.sync(&mut doc_a, &ROOT);
        let source = a.selected_for_test().expect("starter page selected");
        let source_blocks = NotesDoc::page_block_snapshots(&doc_a, source);
        assert!(
            !source_blocks.is_empty(),
            "fixture has collaborative content"
        );

        // This is the sidebar context-menu path. It creates the copy and all of
        // its durable block actions in one local action batch; it must not depend
        // on selecting the copy (or each descendant) to make its body available.
        a.dispatch_event_for_test(&mut doc_a, OutEvent::Duplicate(source));
        let copy = a
            .workspace_pages_for_test()
            .into_iter()
            .find(|page| page.title == "Welcome copy")
            .expect("duplicate metadata")
            .id;
        let copied_blocks = NotesDoc::page_block_snapshots(&doc_a, copy);
        assert_eq!(
            copied_blocks
                .iter()
                .map(|(_, text)| text)
                .collect::<Vec<_>>(),
            source_blocks
                .iter()
                .map(|(_, text)| text)
                .collect::<Vec<_>>(),
            "copy body was dispatched with its metadata"
        );
        assert_ne!(
            copied_blocks[0].0, source_blocks[0].0,
            "copies mint independent stable block ids"
        );

        // A peer receives the complete page before it has navigated to the copy.
        let (mut b, _db) = spawn_app().unwrap();
        let mut doc_b = AutoCommit::load(&doc_a.save()).unwrap();
        b.sync(&mut doc_b, &ROOT);
        assert_eq!(
            NotesDoc::page_block_snapshots(&doc_b, copy),
            copied_blocks,
            "late joiner sees duplicate content without a view-dependent seed"
        );
    }
}
