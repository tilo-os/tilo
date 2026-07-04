//! Headless `--shots` mode: render one offscreen frame of the notes
//! app to a PNG and exit. Used for visual review and CI snapshots at a
//! chosen viewport, without opening a window.
//!
//! The workspace is seeded into a throwaway temp directory so a shot is
//! hermetic and never touches the user's real `~/notes`. Sample content
//! only ever lives on this path — the shipped app starts from the
//! user's own pages.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use taffy::prelude::*;
use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;

use crate::NotesApp;
use crate::model::Workspace;
use crate::storage;

/// Default viewport when `--viewport` is omitted. A 16:9-ish desktop
/// size that shows the sidebar and a full page.
pub const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 720);

/// Parse a `WxH` string like `800x600` into a `(u32, u32)` viewport.
pub fn parse_viewport(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s
        .split_once('x')
        .or_else(|| s.split_once('X'))?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// Seed a small, realistic workspace into `root`: a handful of pages in
/// the tree plus one content-rich note selected so the shot shows real
/// blocks (headings, lists, checklist, code, an inline link).
fn seed_sample_workspace(root: &Path) {
    let mut ws = Workspace::new();

    let inbox = ws.create_page(None);
    ws.rename(inbox, "Inbox");
    let projects = ws.create_page(None);
    ws.rename(projects, "Projects");
    let launch = ws.create_page(Some(projects));
    ws.rename(launch, "Launch checklist");
    let reading = ws.create_page(None);
    ws.rename(reading, "Reading list");
    let recipes = ws.create_page(None);
    ws.rename(recipes, "Recipes");

    ws.set_selected(Some(launch));

    let bodies: [(uuid::Uuid, &str); 5] = [
        (
            launch,
            "\
# Launch checklist

What has to land before we ship. Owner in **bold**.

## This week

- [x] Finalise the onboarding copy

- [x] Cut the release branch

- [ ] Write the migration guide

- [ ] Record the walkthrough video

## Notes

See [[Reading list]] for sources.

```sh
git tag -a v1.0 -m \"release\"
```
",
        ),
        (
            inbox,
            "\
# Inbox

Quick captures to sort later.

- Call the dentist

- Reply to the venue about the date

- Look into a standing desk
",
        ),
        (
            reading,
            "\
# Reading list

Things worth coming back to.

- *The Timeless Way of Building* — Alexander

- `SICP`, chapter 2

- **Notes on the Synthesis of Form**
",
        ),
        (
            projects,
            "\
# Projects

Active work, one page each.

- [[Launch checklist]]
",
        ),
        (
            recipes,
            "\
# Recipes

## Weeknight pasta

- 200g pasta

- garlic, chili, good olive oil

- a handful of parsley
",
        ),
    ];

    storage::save_workspace(root, &ws).expect("seed workspace save");
    for (id, body) in bodies {
        storage::write_page_body(root, id, body).expect("seed page body");
    }
}

/// Render the notes app at `viewport` into a PNG at `out`. When
/// `open_drawer` is set and the viewport is narrow, the overlay page
/// drawer is shown so the flyout can be reviewed.
pub fn render_to_path(
    viewport: (u32, u32),
    out: &Path,
    open_drawer: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Hermetic workspace root — unique per process so parallel shots
    // don't collide, cleaned up on exit.
    let root: PathBuf = std::env::temp_dir()
        .join(format!("notes-shots-{}", uuid::Uuid::new_v4()));
    storage::ensure_layout(&root)?;
    seed_sample_workspace(&root);

    let mut app = NotesApp::spawn(root.clone()).map_err(|e| e.to_string())?;

    let (w, h) = viewport;
    let theme = Theme::dark();
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();

    let mut state = ui::State::new();
    app.sync(&mut state.doc, &state.root);

    // Run the layout cycle until the responsive structure settles. The
    // first build defaults to the wide tree; `apply_layout` observes the
    // real width and, on a narrow viewport, asks for a rebuild into the
    // drawer layout. Mirrors what the live event loop does across frames.
    for _ in 0..4 {
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let child = app.build_layout(&mut taffy, &theme, &mut node_map);
        let root_node = taffy.new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(w as f32),
                    height: Dimension::length(h as f32),
                },
                ..Default::default()
            },
            &[child],
        )?;
        {
            let measure_map = node_map.clone();
            let tr = &mut text_renderer;
            taffy.compute_layout_with_measure(
                root_node,
                taffy::Size {
                    width: AvailableSpace::Definite(w as f32),
                    height: AvailableSpace::Definite(h as f32),
                },
                |known, available, node_id, _, _| {
                    if let Some(&ptr) = measure_map.get(&node_id) {
                        unsafe { (*ptr).measure(known, available, tr) }
                    } else {
                        taffy::Size::ZERO
                    }
                },
            )?;
        }
        app.apply_layout(&taffy, child, Point::ORIGIN);
        // Settle: once no structural change is pending, this frame's
        // tree matches the observed width.
        if !app.take_structure_dirty() {
            break;
        }
    }

    if open_drawer {
        app.open_drawer_for_shots();
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
    let mut renderer = OffscreenRenderer::new(w, h);
    renderer.render_png(&scene, Color::from_rgba8(15, 17, 21, 255), out);
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}
