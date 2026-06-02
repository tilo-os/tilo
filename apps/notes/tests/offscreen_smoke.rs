//! Headless render smoke test: spawn a NotesApp, build its layout,
//! and draw one frame into an offscreen scene. Confirms the whole
//! widget tree composes through the same `sync → build_layout →
//! apply_layout → draw` cycle the live `ui::App` uses, with no panic
//! / no missing layout slot.

use std::collections::HashMap;

use notes::{NotesApp, WELCOME_BODY};
use taffy::prelude::*;
use tempfile::TempDir;
use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;

#[test]
fn render_one_frame_against_seeded_workspace() {
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn(tmp.path().into()).expect("spawn ok");

    // Phase 3: reload the Welcome page from the canonical seed so
    // the snapshot includes every block kind (paragraph, headings,
    // bullets, checklists, code block, divider). Bypasses winit but
    // hits the same `BlockEditor::from_markdown` path the live app
    // uses on selection change.
    {
        let panel = app.main_panel_mut();
        if let Some(id) = panel.page_id() {
            panel.load_page(id, "Welcome".into(), WELCOME_BODY.into());
        }
    }

    let theme = Theme::dark();
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let mut state = ui::State::new();

    app.sync(&mut state.doc, &state.root);
    let child = app.build_layout(&mut taffy, &theme, &mut node_map);
    let (w, h) = (1024u32, 768u32);
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
    app.apply_layout(&taffy, child, Point::ORIGIN);

    // Sanity-assert the editor laid out multiple non-degenerate
    // blocks for the kitchen-sink Welcome body. Pinned so a future
    // regression in MainPanel's flex chain (where an outside
    // `set_style` accidentally drops `flex_direction: Column`,
    // collapsing every block to width 0 — happened in the first
    // round of Phase 2 wiring) gets caught here instead of in a
    // quiet snapshot.
    {
        let panel = app.main_panel_mut();
        let n = panel.editor().blocks().len();
        assert!(
            n >= 6,
            "Welcome seed should parse into at least 6 blocks; got {}",
            n,
        );
        for b in panel.editor().blocks() {
            let r = b.line.get_rect();
            // Dividers have zero text but still get a non-degenerate
            // row (the horizontal rule paints across the whole row).
            assert!(
                r.width() > 0.0 && r.height() > 0.0,
                "block rect must be non-degenerate (got {:?})",
                r
            );
        }
    }

    let mut scene = Scene::new();
    app.draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        &mut text_renderer,
        &theme,
        &presence,
        &permission,
    );
    // We don't compare pixels here — just confirm a non-degenerate
    // rect and that the draw call returned. If anything panics
    // upstream we'd never reach this line.
    assert!(app.get_rect().width() > 0.0);
    assert!(app.get_rect().height() > 0.0);

    // Setting NOTES_SNAPSHOT writes a PNG so a human can eyeball the
    // result. Skipped in CI to avoid the wgpu device init cost on
    // every run.
    if let Ok(path) = std::env::var("NOTES_SNAPSHOT") {
        let mut renderer = OffscreenRenderer::new(w, h);
        renderer.render_png(&scene, Color::TRANSPARENT, std::path::Path::new(&path));
        eprintln!("notes: snapshot written to {}", path);
    }
}
