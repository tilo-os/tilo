// Thin binary entry point — all logic lives in the library.
//
// Supports two modes:
//   * default — build a `TerminalWidget`, hand it to `ui::App`, run
//     the live event loop (winit window).
//   * `--shots <PATH>` — render the terminal at a default size into a
//     PNG at `<PATH>` and exit. No window, no event loop. Useful for
//     headless screenshot tests and CI snapshots of the cold-start UI.
//
// The `--shots` flow mirrors the same layout/draw passes the live
// `App::run` does, just funnels into `OffscreenRenderer::render_png`
// instead of a wgpu surface.

use std::collections::HashMap;

use terminal::TerminalWidget;
use ui::offscreen::OffscreenRenderer;
use ui::presence::PresenceCtx;
use ui::permissions::PermissionContext;
use ui::theme::Theme;
use ui::text_render::TextRenderer;
use ui::widget::Widget;
use taffy::prelude::*;
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;

/// Default offscreen viewport for `--shots`. Big enough to show a few
/// rows of terminal output at the toolkit's default font size; small
/// enough that the readback buffer stays under a few MB.
const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1200, 720);

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    // Parse a tiny ad-hoc flag set so we don't pull in `clap` for one
    // boolean-with-string. Anything other than `--shots PATH` is treated
    // as "open a window the same way it always did".
    let args: Vec<String> = std::env::args().collect();
    if let Some(out) = parse_shots_arg(&args) {
        let widget = TerminalWidget::spawn_default()?;
        render_shot_to_png(Box::new(widget), DEFAULT_SHOTS_VIEWPORT, &out)?;
        return Ok(());
    }

    let widget = TerminalWidget::spawn_default()?;
    let state = ui::State::new();
    let app = ui::App::new(state, Box::new(widget));
    app.run();
    Ok(())
}

/// Returns `Some(path)` if `--shots <path>` (or `--shots=<path>`) appears
/// in `args`, otherwise `None`.
fn parse_shots_arg(args: &[String]) -> Option<std::path::PathBuf> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--shots" {
            return iter.next().map(std::path::PathBuf::from);
        }
        if let Some(rest) = arg.strip_prefix("--shots=") {
            return Some(std::path::PathBuf::from(rest));
        }
    }
    None
}

/// Run one offscreen frame against `widget` at `viewport` and write the
/// resulting RGBA to `out` as a PNG.
///
/// Mirrors `ui::App`'s layout/draw cycle for a single redraw:
/// `sync` → `build_layout` → `compute_layout` → `apply_layout` → `draw`.
/// Skips the event loop entirely because we render exactly one frame
/// before exiting.
fn render_shot_to_png(
    mut widget: Box<dyn Widget>,
    viewport: (u32, u32),
    out: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    let theme = Theme::dark();
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    // Sync the widget's view-state from its CRDT once.
    let mut state = ui::State::new();
    widget.sync(&mut state.doc, &state.root);

    // Build the layout tree, then wrap it in a viewport-sized root so
    // taffy has a definite extent to size from.
    let child = widget.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy.new_with_children(
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
            root,
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
    widget.apply_layout(&taffy, child, Point::ORIGIN);

    let mut scene = Scene::new();
    widget.draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        &mut text_renderer,
        &theme,
        &presence,
        &permission,
    );

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut renderer = OffscreenRenderer::new(w, h);
    renderer.render_png(&scene, Color::TRANSPARENT, out);
    println!("terminal: wrote shot to {}", out.display());
    Ok(())
}
