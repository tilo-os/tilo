// Thin binary entry point — all logic lives in the library.
//
// Supports two modes:
//   * default — build a `BrowserWidget`, hand it to `ui::App`,
//     run the live event loop (winit window). Servo is booted from
//     `BrowserWidget::sync` on the first frame.
//   * `--shots <PATH>` — render the chrome at a default size into a
//     PNG at `<PATH>` and exit. No window, no Servo event loop. Used
//     for headless screenshot tests and CI snapshots of the cold-start
//     UI. The Servo surface paints its placeholder/loading state in
//     this mode since no live frame is available before exit.

use std::collections::HashMap;

use browser::BrowserWidget;
use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use taffy::prelude::*;
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;

/// Default offscreen viewport for `--shots`. Wide enough to show the
/// chrome row + a sizeable surface area.
const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1200, 720);

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    // rustls init lives here instead of `BrowserWidget::new` so the
    // `--shots` path doesn't pay for it.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let args: Vec<String> = std::env::args().collect();
    if let Some(out) = parse_shots_arg(&args) {
        // Headless mode: skip Servo engine boot — the snapshot is just
        // the chrome at cold start.
        let widget = BrowserWidget::new_headless();
        render_shot_to_png(Box::new(widget), DEFAULT_SHOTS_VIEWPORT, &out)?;
        return Ok(());
    }

    let widget = BrowserWidget::new();
    let state = ui::State::new();
    let app = ui::App::new(state, Box::new(widget));
    app.run();

    // Fast exit to side-step Servo's noisy thread teardown — matches
    // the iced-era behaviour in Phase A.
    unsafe { libc::_exit(0) };
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

/// Render one offscreen frame against `widget` at `viewport` and write
/// the resulting RGBA to `out` as a PNG. Mirrors `ui::App`'s
/// `sync → build_layout → compute_layout → apply_layout → draw` cycle.
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

    let mut state = ui::State::new();
    widget.sync(&mut state.doc, &state.root);

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
    println!("browser: wrote shot to {}", out.display());
    Ok(())
}
