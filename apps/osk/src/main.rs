//! Thin binary entry point for the qOS on-screen keyboard.
//!
//! Two modes, mirroring the other `ui` apps (e.g. file-explorer):
//!   * default — build an [`OskWidget`], hand it to `ui::App`, run the live
//!     winit event loop.
//!   * `--shots <PATH>` — render the keyboard at a default size into a PNG at
//!     `<PATH>` and exit. No window, no event loop. Used for headless
//!     screenshot tests and CI snapshots.
//!
//! ## Integration seams (out of scope for W8, wired by later workstreams)
//!
//! * **W7 — layer-shell + `input-method-v2`:** in production the OSK runs as a
//!   `wlr-layer-shell` surface (overlay layer, bottom-anchored, exclusive
//!   zone) *and* a `zwp_input_method_v2` client. After each frame the host
//!   loop will drain [`OskWidget::take_pending_text`] (and/or observe
//!   [`OskWidget::on_key`]) and commit the string to the focused surface via
//!   the IM client. `ui::App` does not yet create a layer-shell surface, so
//!   today this binary opens a normal window; swap the surface role in here
//!   when W7 lands — the widget is unchanged.
//!
//! * **W9 — FUTO swipe + prediction:** a swipe decoder will consume the
//!   pointer path during a press-drag, normalise it against
//!   [`OskWidget::key_geometry`] (the same geometry used to render), and feed
//!   candidate words into the toolbar's candidate ribbon. The decoder lives
//!   in a separate (GPL) process/crate; this binary will spawn/talk to it.
//!   No swipe code is linked here.

use std::collections::HashMap;

use osk::OskWidget;
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

/// Default offscreen viewport for `--shots`. Phone-portrait-ish width with
/// just enough height for the toolbar + suggestion bar + four key rows
/// (48 + 40 + 4×56 = 312, rounded up).
const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (720, 360);

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    if let Some(out) = parse_shots_arg(&args) {
        let widget = OskWidget::new();
        render_shot_to_png(Box::new(widget), DEFAULT_SHOTS_VIEWPORT, &out)?;
        return Ok(());
    }

    // --- W7 seam: in production this surface is a wlr-layer-shell overlay and
    // an input-method-v2 client; for now it is a plain ui::App window. ---
    let widget = OskWidget::new();
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
/// resulting RGBA to `out` as a PNG. Mirrors `ui::App`'s
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
    println!("osk: wrote shot to {}", out.display());
    Ok(())
}
