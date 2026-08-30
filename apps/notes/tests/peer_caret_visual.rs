//! Pin the actual rendered pixels around peer + local caret/selection.
//!
//! The bug the user reported: typing in a block shows a "ghost" caret
//! at the start of the block in addition to the real caret. This
//! test renders that exact state to PNG so we can SEE the pixels
//! rather than guess.
//!
//! Run with `NOTES_CRITIQUE_DIR=/tmp/notes-peer-caret cargo test
//! -p notes --test peer_caret_visual -- --nocapture --ignored`.

use std::collections::HashMap;

use notes::NotesApp;
use taffy::prelude::*;
use tempfile::TempDir;
use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::permissions::UserId;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;

fn render(app: &mut NotesApp, presence: &PresenceCtx, out: &str, w: u32, h: u32) {
    let theme = Theme::dark();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let mut state = ui::State::new();
    (app as &mut dyn Widget).sync(&mut state.doc, &state.root);
    let child = (app as &mut dyn Widget).build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(w as f32),
                    height: Dimension::length(h as f32),
                },
                ..Default::default()
            },
            &[child],
        )
        .unwrap();
    {
        let measure_map = node_map.clone();
        let tr = &mut text_renderer;
        taffy
            .compute_layout_with_measure(
                root,
                taffy::Size {
                    width: AvailableSpace::Definite(w as f32),
                    height: AvailableSpace::Definite(h as f32),
                },
                |known, avail, node_id, _, _| {
                    if let Some(&ptr) = measure_map.get(&node_id) {
                        unsafe { (*ptr).measure(known, avail, tr) }
                    } else {
                        taffy::Size::ZERO
                    }
                },
            )
            .unwrap();
    }
    (app as &mut dyn Widget).apply_layout(&taffy, child, Point::ORIGIN);

    let mut scene = Scene::new();
    (app as &mut dyn Widget).draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        &mut text_renderer,
        &theme,
        presence,
        &permission,
    );
    let mut renderer = OffscreenRenderer::new(w, h);
    renderer.render_png(&scene, Color::TRANSPARENT, std::path::Path::new(out));
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

    // 1) Baseline: no presence at all. If a "ghost" caret appears here
    //    it can only be the local TextInput drawing two carets — not a
    //    peer-data thing at all.
    let presence_empty = PresenceCtx::empty();
    render(
        &mut app,
        &presence_empty,
        &format!("{dir}/01-baseline.png"),
        1280,
        800,
    );

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
        1280,
        800,
    );

    // 3) Add a remote peer caret on block 0 byte 10 in Bob's colour.
    let mut presence_with_peer = PresenceCtx::empty();
    let bob = UserId("bob".to_string());
    let block_key = ui::widget::block_editor::block_presence_key(0);
    presence_with_peer.set_caret(&block_key, bob, 10);
    render(
        &mut app,
        &presence_with_peer,
        &format!("{dir}/03-with-peer.png"),
        1280,
        800,
    );

    // 4) Add the LOCAL user as a peer too (the suspected bug — the
    //    local user appearing in the remote-presence map).
    let alice = UserId("alice".to_string());
    presence_with_peer.set_caret(&block_key, alice, 5);
    render(
        &mut app,
        &presence_with_peer,
        &format!("{dir}/04-with-both.png"),
        1280,
        800,
    );

    eprintln!("INSPECT: ls -la {dir}");
}
