//! Slides — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default        — run the live winit event loop via `ui::App`.
//!   * `--shots PATH` — render one offscreen frame to a PNG and exit
//!                       (headless screenshot; identical passes to the live app).

use std::collections::HashMap;

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

const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);
/// Narrow viewport used by the responsive `--shots` capture (`SLIDES_SHOT=narrow`).
/// Just below the rail's collapse breakpoint, so the rail becomes an overlay
/// flap while the rest of the editor stays legible.
const NARROW_SHOTS_VIEWPORT: (u32, u32) = (820, 800);

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    if let Some(out) = parse_shots_arg(&args) {
        // Scenario selects which screen to capture; `--scenario` wins over the
        // legacy `SLIDES_SHOT` env var.
        let scenario = parse_flag(&args, "--scenario").or_else(|| std::env::var("SLIDES_SHOT").ok());
        // Viewport: explicit `--viewport WxH` wins; otherwise narrow scenarios
        // render at a phone-ish width (so the rail resolves to an overlay
        // flap), everything else at the default 16:10 canvas.
        let viewport = parse_viewport(&args).unwrap_or_else(|| match scenario.as_deref() {
            Some("narrow") | Some("narrow_closed") => NARROW_SHOTS_VIEWPORT,
            _ => DEFAULT_SHOTS_VIEWPORT,
        });
        render_shot_to_png(slides::app_root_demo(scenario.as_deref()), viewport, &out)?;
        return Ok(());
    }

    // Headless drive (`--headless-drive '<json>'`): run a list of clicks and
    // keystrokes against the real widget tree with no display, persisting
    // through the same store the live app uses.
    if let Some(spec) = ui::headless::spec_from_args(&args) {
        std::process::exit(ui::headless::run(spec, slides::app_root(), "dev.qos.slides"));
    }

    let (state, autosave) = ui::persist::open("dev.qos.slides");
    let mut app = ui::App::new(state, slides::app_root()).with_autosave(autosave);
    // Register Slides' actions so the shell's Ctrl+K palette can decode + replay
    // the same deck actions the on-screen affordances dispatch.
    slides::actions::register(app.action_registry_mut());
    // Present mode requests true OS fullscreen: `SlidesApp` reports
    // `fullscreen_intent() = Some(true)` while presenting (via
    // `Widget::fullscreen_intent`), and `ui::App` polls that each frame and
    // drives `window.set_fullscreen(...)`. On the qOS compositor that becomes
    // an `xdg_toplevel.set_fullscreen` request the shell honors by promoting
    // the surface to its full output; on any other host WM winit fullscreens
    // the toplevel directly. Esc / a compositor-driven exit flow back through
    // `Widget::on_fullscreen_changed`.
    app.run();
    Ok(())
}

fn parse_shots_arg(args: &[String]) -> Option<std::path::PathBuf> {
    parse_flag(args, "--shots").map(std::path::PathBuf::from)
}

/// Read a `--name VALUE` or `--name=VALUE` string flag from the argv.
fn parse_flag(args: &[String], name: &str) -> Option<String> {
    let eq = format!("{name}=");
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == name {
            return iter.next().cloned();
        }
        if let Some(rest) = arg.strip_prefix(&eq) {
            return Some(rest.to_string());
        }
    }
    None
}

/// Parse `--viewport WxH` (e.g. `--viewport 560x720`) into a pixel viewport.
fn parse_viewport(args: &[String]) -> Option<(u32, u32)> {
    let spec = parse_flag(args, "--viewport")?;
    let (w, h) = spec.split_once(['x', 'X'])?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// Run one offscreen frame against `widget` and write RGBA to `out` as PNG.
/// Mirrors `ui::App`'s cycle: sync → build_layout → compute_layout → apply_layout → draw.
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
    println!("slides: wrote shot to {}", out.display());
    Ok(())
}
