//! Self-critique screenshot harness: render the live app in N
//! states + dump PNGs. Not a CI test — gated on `NOTES_CRITIQUE_DIR`
//! env var.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test that unwraps is stating an invariant"
)]

use std::collections::HashMap;

use notes::NotesApp;
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

fn render_state(app: &mut NotesApp, out: &str, w: u16, h: u16) -> Result<(), String> {
    let theme = Theme::dark();
    let presence = PresenceCtx::empty();
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
                    width: Dimension::length(f32::from(w)),
                    height: Dimension::length(f32::from(h)),
                },
                ..Default::default()
            },
            &[child],
        )
        .map_err(|e| e.to_string())?;
    {
        let measure_map = node_map.clone();
        let tr = &mut text_renderer;
        taffy
            .compute_layout_with_measure(
                root,
                taffy::Size {
                    width: AvailableSpace::Definite(f32::from(w)),
                    height: AvailableSpace::Definite(f32::from(h)),
                },
                |known, avail, node_id, _, _| {
                    if let Some(&ptr) = measure_map.get(&node_id) {
                        unsafe { (*ptr).measure(known, avail, tr) }
                    } else {
                        taffy::Size::ZERO
                    }
                },
            )
            .map_err(|e| e.to_string())?;
    }
    (app as &mut dyn Widget).apply_layout(&taffy, child, Point::ORIGIN);

    let mut scene = Scene::new();
    (app as &mut dyn Widget).draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        &mut text_renderer,
        &theme,
        &presence,
        &permission,
    );
    let mut renderer = OffscreenRenderer::new(u32::from(w), u32::from(h));
    renderer.render_png(&scene, Color::TRANSPARENT, std::path::Path::new(out));
    eprintln!("notes: snapshot written to {out}");
    Ok(())
}

#[test]
fn critique_state_dump() {
    let Ok(dir) = std::env::var("NOTES_CRITIQUE_DIR") else {
        return;
    };
    std::fs::create_dir_all(&dir).unwrap();
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).expect("spawn ok");
    app.tick_for_test();

    // 1) Default landing state — full Welcome page.
    render_state(&mut app, &format!("{dir}/01-welcome.png"), 1280, 800).unwrap();

    // 2) Shortcuts overlay open (the user pressed `?`).
    app.shortcuts_mut().set_visible(true);
    render_state(&mut app, &format!("{dir}/02-shortcuts.png"), 1280, 800).unwrap();
    app.shortcuts_mut().set_visible(false);

    // 3) Cmd-K palette open.
    app.palette_mut().set_visible(true);
    app.palette_mut().set_anchor(Point::new(450.0, 200.0));
    render_state(&mut app, &format!("{dir}/03-palette.png"), 1280, 800).unwrap();
    app.palette_mut().set_visible(false);

    // 4) Empty page (a brand new page with no body yet).
    let new_id = uuid::Uuid::new_v4();
    {
        let panel = app.main_panel_mut();
        panel.load_page(new_id, "New page", String::new());
    }
    app.tick_for_test();
    render_state(&mut app, &format!("{dir}/04-empty-page.png"), 1280, 800).unwrap();

    // 5) Many pages in the sidebar so the tree pattern shows up.
    let many = [
        "Daily 2026-06-01",
        "Daily 2026-06-02",
        "Project: notes",
        "Ideas",
        "Reading list",
        "Inbox",
    ];
    for title in many {
        let id = uuid::Uuid::new_v4();
        app.apply_event_for_test(notes::sidebar::OutEvent::CreateChild { parent: None });
        // Find the most recent page and rename it
        if let Some(last) = app.workspace_pages_for_test().last().cloned() {
            app.apply_event_for_test(notes::sidebar::OutEvent::Rename {
                id: last.id,
                new_title: title.to_string(),
            });
        }
        let _ = id;
    }
    app.tick_for_test();
    render_state(&mut app, &format!("{dir}/05-many-pages.png"), 1280, 800).unwrap();
}
