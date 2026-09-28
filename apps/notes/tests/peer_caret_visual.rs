//! Pin the actual rendered pixels around peer + local caret/selection.
//!
//! The bug the user reported: typing in a block shows a "ghost" caret
//! at the start of the block in addition to the real caret. This
//! test renders that exact state to PNG so we can SEE the pixels
//! rather than guess.
//!
//! Run with `NOTES_CRITIQUE_DIR=/tmp/notes-peer-caret cargo test
//! -p notes --test peer_caret_visual -- --nocapture --ignored`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use notes::NotesApp;
use tempfile::TempDir;
use ui::headless::Framed;
use ui::permissions::UserId;
use ui::presence::PresenceCtx;
use ui::theme::Theme;

fn render(app: &mut Framed<NotesApp>, presence: &PresenceCtx, out: &str) {
    let harness = Framed::harness_mut(app);
    harness.set_presence_context(presence.clone());
    harness.render_png(std::path::Path::new(out));
    eprintln!("notes: snapshot written to {out}");
}

#[test]
#[ignore = "interactive PNG harness"]
fn ghost_caret_repro() {
    let dir =
        std::env::var("NOTES_CRITIQUE_DIR").unwrap_or_else(|_| "/tmp/notes-peer-caret".to_string());
    std::fs::create_dir_all(&dir).unwrap();
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).expect("spawn ok");
    app.tick_for_test();
    let mut app = Framed::new(ui::State::new(), app, (1280, 800), Theme::dark());

    // 1) Baseline: no presence at all. If a "ghost" caret appears here
    //    it can only be the local TextInput drawing two carets — not a
    //    peer-data thing at all.
    let presence_empty = PresenceCtx::empty();
    render(&mut app, &presence_empty, &format!("{dir}/01-baseline.png"));

    // 2) Local user focused, no remote peer at all.
    {
        let panel = app.main_panel_mut();
        // Mark "alice" as the local author so authorship + colours
        // pick the same id Notes will use at runtime.
        panel
            .editor_mut()
            .set_local_author(Some("alice".to_string()));
        panel.editor_mut().set_focus(true);
        // Drop the caret in block 0, byte 5.
        panel
            .editor_mut()
            .focus_block(0, ui::widget::block_editor::CursorPlacement::Byte(5));
    }
    render(
        &mut app,
        &presence_empty,
        &format!("{dir}/02-local-focused.png"),
    );

    // 3) Add a remote peer caret on block 0 byte 10 in Bob's colour.
    let mut presence_with_peer = PresenceCtx::empty();
    let bob = UserId("bob".to_string());
    let first_block = app.main_panel_mut().editor_mut().blocks()[0].id.clone();
    let block_key = ui::widget::block_editor::block_presence_key(&first_block);
    presence_with_peer.set_caret(&block_key, bob, 10);
    render(
        &mut app,
        &presence_with_peer,
        &format!("{dir}/03-with-peer.png"),
    );

    // 4) Add the LOCAL user as a peer too (the suspected bug — the
    //    local user appearing in the remote-presence map).
    let alice = UserId("alice".to_string());
    presence_with_peer.set_caret(&block_key, alice, 5);
    render(
        &mut app,
        &presence_with_peer,
        &format!("{dir}/04-with-both.png"),
    );

    eprintln!("INSPECT: ls -la {dir}");
}
