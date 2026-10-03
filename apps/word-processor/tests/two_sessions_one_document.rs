//! Two people editing "the same" document must be editing the same document.
//!
//! This drives the production entry points — `open_document` for each side, and
//! the toolkit's own full-state merge for the sync — because the defect this
//! covers lived in neither of them individually. Each app that opens a file
//! builds its document from scratch; the merge is then asked to reconcile two
//! documents that share no ancestor, and it resolves the resulting root-key
//! conflict by keeping one side and shadowing the other.
//!
//! What made it survive so long is that typing crosses correctly. Two apps that
//! import *the same file* produce two structurally identical documents, so
//! dropping one of them changes nothing anybody can see, and every subsequent
//! keystroke lands on the surviving containers. The loss only becomes visible
//! when the two sides hold *different* content — which is exactly the case a
//! photograph pasted on one machine creates.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::{Path, PathBuf};
use tilo_automerge::ApplyIncoming;

use ui::Widget;
use word_processor::{WordProcessorWidget, open_document};

fn temp_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "wp-two-sessions-{}-{}-{name}",
        std::process::id(),
        ui::time::wall()
            .duration_since(ui::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    p
}

fn write_markdown(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
}

/// The sync the toolkit performs when a peer answers `Arrived` with its full
/// state: serialize one side, load it, merge it in. This is the same pair of
/// calls `App`'s `CollabEvent::CRDTUpdate` handler makes.
fn sync_full_state(into: &mut ui::State, from: &mut ui::State) {
    let bytes = from.doc.save();
    let mut incoming = ui::State::load(&bytes).expect("a peer's state loads");
    into.doc
        .merge_incoming(&mut incoming.doc)
        .expect("peer states merge");
}

fn texts(state: &mut ui::State, wp: &mut WordProcessorWidget) -> Vec<String> {
    wp.sync(&mut state.doc, &state.root);
    wp.model().block_texts(&state.doc)
}

/// Two sessions that each opened a file keep both documents, a photograph pasted on one side reaches the
/// other, and no container is shadowed after a merge.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn sessions_keep_their_documents_and_share_content() {
    // Phase: two_sessions_that_each_opened_a_file_keep_both_documents.
    // Two sessions, two files, one room. Neither side's paragraphs may disappear,
    // and the outcome may not depend on which actor id sorted higher — repeated
    // because that's a coin-flip failure a single run would rarely catch.
    {
        const ROUNDS: usize = 24;
        for round in 0..ROUNDS {
            let mine_path = temp_path(&format!("mine-{round}.md"));
            let theirs_path = temp_path(&format!("theirs-{round}.md"));
            write_markdown(&mine_path, "the quarterly figures\n");
            write_markdown(&theirs_path, "notes from the call\n");

            let (mut mine, mut mine_wp, _) =
                open_document(&mine_path, tilo_content::ContentStore::memory()).unwrap();
            let (mut theirs, mut theirs_wp, _) =
                open_document(&theirs_path, tilo_content::ContentStore::memory()).unwrap();

            sync_full_state(&mut mine, &mut theirs);
            sync_full_state(&mut theirs, &mut mine);

            let m = texts(&mut mine, &mut mine_wp);
            let t = texts(&mut theirs, &mut theirs_wp);

            for who in ["quarterly figures", "notes from the call"] {
                assert!(
                    m.iter().any(|s| s.contains(who)),
                    "round {round}: '{who}' is missing from my page after the merge; got {m:?}"
                );
                assert!(
                    t.iter().any(|s| s.contains(who)),
                    "round {round}: '{who}' is missing from their page after the merge; got {t:?}"
                );
            }
            assert_eq!(m, t, "round {round}: the two sessions converge");

            let _ = std::fs::remove_file(&mine_path);
            let _ = std::fs::remove_file(&theirs_path);
        }
    }

    // Phase: a_photograph_pasted_on_one_side_reaches_the_other.
    // The case that exposed it: only one side has the picture, so if that side
    // loses the root-key conflict there is no image block left on the far page to
    // draw — which reads as a transport failure and is not one.
    {
        const ROUNDS: usize = 24;
        // A 1x1 PNG — real bytes, so the app's own decode path runs and stamps
        // width/height the way a photograph would.
        const PNG: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];

        for round in 0..ROUNDS {
            let mine_path = temp_path(&format!("with-photo-{round}.md"));
            let theirs_path = temp_path(&format!("no-photo-{round}.md"));
            write_markdown(&mine_path, "the quarterly figures\n");
            write_markdown(&theirs_path, "nothing here yet\n");

            // Both sessions run on one device, so they share its content store; reaching another
            // device's store is the store backend's job, not the document's.
            let content = tilo_content::ContentStore::memory();
            let (mut mine, mut mine_wp, _) = open_document(&mine_path, content.clone()).unwrap();
            let (mut theirs, mut theirs_wp, _) =
                open_document(&theirs_path, content.clone()).unwrap();

            // Paste the photograph through the app's own image path.
            mine_wp.sync(&mut mine.doc, &mine.root);
            mine_wp.model_mut().insert_image_bytes(&mut mine.doc, PNG);

            sync_full_state(&mut mine, &mut theirs);
            sync_full_state(&mut theirs, &mut mine);

            theirs_wp.sync(&mut theirs.doc, &theirs.root);
            let containers = tilo_doc::model::open(&theirs.doc, &theirs.root).expect("qdoc schema");
            let snap = tilo_doc::model::materialize(&theirs.doc, &containers);
            let image = snap
                .blocks
                .iter()
                .find(|b| b.kind == "image")
                .unwrap_or_else(|| {
                    panic!(
                        "round {round}: the far page has no image block at all — the \
                         photograph cannot render because it is not in the merged document"
                    )
                });
            let hash = match image.attrs.get("blob") {
                Some(tilo_doc::AttrVal::Str(h)) => h.clone(),
                other => panic!("round {round}: image block carries no blob hash: {other:?}"),
            };
            assert!(
                content.read(&hash).is_some(),
                "round {round}: the far side has the image block but not its bytes"
            );
            assert!(
                texts(&mut theirs, &mut theirs_wp)
                    .iter()
                    .any(|s| s.contains("nothing here yet")),
                "round {round}: the far side kept its own paragraph too"
            );

            let _ = std::fs::remove_file(&mine_path);
            let _ = std::fs::remove_file(&theirs_path);
        }
    }

    // Phase: no_container_is_shadowed_after_a_merge.
    // The schema must be reached through exactly one object. More than one
    // candidate at a container key means content is sitting in the change graph
    // that no read will ever return — the shape of the silent loss.
    {
        let mine_path = temp_path("shadow-mine.md");
        let theirs_path = temp_path("shadow-theirs.md");
        write_markdown(&mine_path, "mine\n");
        write_markdown(&theirs_path, "theirs\n");

        let (mut mine, _, _) =
            open_document(&mine_path, tilo_content::ContentStore::memory()).unwrap();
        let (mut theirs, _, _) =
            open_document(&theirs_path, tilo_content::ContentStore::memory()).unwrap();
        sync_full_state(&mut mine, &mut theirs);

        assert_eq!(
            tilo_doc::model::shadowed_containers(&mine.doc, &mine.root),
            Vec::<String>::new(),
            "every schema container is reached through one object after a merge"
        );

        let _ = std::fs::remove_file(&mine_path);
        let _ = std::fs::remove_file(&theirs_path);
    }
}

/// A third participant, joining after the first two already have content, and a
/// peer whose document is adopted rather than created. Neither may displace
/// anyone.
#[test]
fn a_third_session_joining_late_displaces_nobody() {
    const ROUNDS: usize = 12;
    for round in 0..ROUNDS {
        let paths: Vec<PathBuf> = ["alice", "bob", "carol"]
            .iter()
            .map(|who| {
                let p = temp_path(&format!("{who}-{round}.md"));
                write_markdown(&p, &format!("written by {who}\n"));
                p
            })
            .collect();

        let (mut a, mut a_wp, _) =
            open_document(&paths[0], tilo_content::ContentStore::memory()).unwrap();
        let (mut b, mut b_wp, _) =
            open_document(&paths[1], tilo_content::ContentStore::memory()).unwrap();
        sync_full_state(&mut a, &mut b);
        sync_full_state(&mut b, &mut a);

        // Carol opens her copy only now, with content already in the room.
        let (mut c, mut c_wp, _) =
            open_document(&paths[2], tilo_content::ContentStore::memory()).unwrap();
        sync_full_state(&mut c, &mut a);
        sync_full_state(&mut a, &mut c);
        sync_full_state(&mut b, &mut a);

        let pages = [
            texts(&mut a, &mut a_wp),
            texts(&mut b, &mut b_wp),
            texts(&mut c, &mut c_wp),
        ];
        for (i, page) in pages.iter().enumerate() {
            for who in ["alice", "bob", "carol"] {
                assert!(
                    page.iter().any(|s| s.contains(who)),
                    "round {round}: session {i} lost {who}'s paragraph; got {page:?}"
                );
            }
        }
        assert_eq!(pages[0], pages[1], "round {round}: a and b converge");
        assert_eq!(pages[0], pages[2], "round {round}: a and c converge");

        for p in &paths {
            let _ = std::fs::remove_file(p);
        }
    }
}
