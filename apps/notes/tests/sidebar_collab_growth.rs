//! Regression: a remote peer adding a page grows the sidebar's row list
//! via `sync`, and a draw can land before the next relayout. The sidebar
//! must not index `row_rects` out of bounds (panicked at sidebar.rs:310
//! in the live two-instance demo). Drives the real
//! `sync → build_layout → compute → apply_layout → draw` cycle headless
//! (no GPU — `draw_with_presence` just fills a vello Scene).

use std::collections::HashMap;

use notes::NotesApp;
use notes::doc::NotesDoc;
use notes::model::PageMeta;
use taffy::prelude::*;
use tempfile::TempDir;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point};

/// Run a full layout pass (sync + build + compute + apply) — the part of
/// the frame that sizes every widget's rects, including the sidebar's
/// per-row `row_rects`.
fn relayout(
    app: &mut NotesApp,
    state: &mut ui::State,
    theme: &Theme,
    tr: &mut TextRenderer,
) -> Result<(), String> {
    app.sync(&mut state.doc, &state.root);
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let child = app.build_layout(&mut taffy, theme, &mut node_map);
    let (w, h) = (1024f32, 768f32);
    let root = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(w),
                    height: Dimension::length(h),
                },
                ..Default::default()
            },
            &[child],
        )
        .map_err(|e| e.to_string())?;
    {
        let measure_map = node_map.clone();
        let trr = &mut *tr;
        taffy
            .compute_layout_with_measure(
                root,
                taffy::Size {
                    width: AvailableSpace::Definite(w),
                    height: AvailableSpace::Definite(h),
                },
                |known, avail, node_id, _, _| {
                    if let Some(&ptr) = measure_map.get(&node_id) {
                        unsafe { (*ptr).measure(known, avail, trr) }
                    } else {
                        taffy::Size::ZERO
                    }
                },
            )
            .map_err(|e| e.to_string())?;
    }
    app.apply_layout(&taffy, child, Point::ORIGIN);
    Ok(())
}

fn draw(app: &mut NotesApp, theme: &Theme, tr: &mut TextRenderer) {
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut scene = Scene::new();
    app.draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        tr,
        theme,
        &presence,
        &permission,
    );
}

#[test]
fn sidebar_survives_remote_page_added_between_layout_and_draw() {
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
    app.bypass_seed_grace_for_test();
    let mut state = ui::State::new();
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();

    // Frame 1: full cycle so the sidebar lays out its welcome row(s).
    relayout(&mut app, &mut state, &theme, &mut tr).unwrap();
    draw(&mut app, &theme, &mut tr);

    // A remote peer adds a second page to the SHARED doc.
    NotesDoc::upsert_page_meta(
        &mut state.doc,
        &PageMeta {
            id: uuid::Uuid::new_v4(),
            title: "Remote Page".into(),
            parent: None,
            order: 1,
        },
    );

    // `sync` pulls the new page in (sidebar flat_rows grows) but we DRAW
    // before the next relayout — the exact ordering that panicked in the
    // live demo. Must not panic.
    app.sync(&mut state.doc, &state.root);
    draw(&mut app, &theme, &mut tr);

    // And after a proper relayout it renders both rows without panic.
    relayout(&mut app, &mut state, &theme, &mut tr).unwrap();
    draw(&mut app, &theme, &mut tr);
}
