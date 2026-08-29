//! API Tester — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default        — run the live winit event loop via `ui::App`.
//!   * `--shots PATH` — render one offscreen frame to a PNG and exit
//!                       (headless screenshot; identical passes to the live app).
//!
//! Screenshot knobs (shots only):
//!   * `--viewport WxH` — render at an explicit size instead of the default
//!                        wide desktop. A width below the rail breakpoint puts
//!                        the layout into its narrow (overlay) form.
//!   * `--rail-open`    — snap the primary rail fully open before drawing, so a
//!                        single offscreen frame captures the summoned overlay
//!                        flap + scrim rather than a window that has not yet had
//!                        the rail toggled.

use std::collections::HashMap;

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

use api_tester::ApiTesterApp;

const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    if let Some(out) = parse_shots_arg(&args) {
        let viewport = parse_viewport(&args).unwrap_or(DEFAULT_SHOTS_VIEWPORT);
        let mut app = ApiTesterApp::new_demo();
        // Snap the rail to a settled state so the single frame is
        // deterministic: open it for the narrow overlay capture, otherwise
        // leave the docked default (open) alone.
        if parse_flag(&args, "--rail-open") {
            app.snap_rail(true);
        }
        render_shot_to_png(Box::new(app), viewport, &out)?;
        return Ok(());
    }

    // Headless drive (`--headless-drive '<json>'`): run a list of clicks and
    // keystrokes against the real widget tree with no display, persisting
    // through the same store the live app uses.
    if let Some(spec) = ui::headless::spec_from_args(&args) {
        std::process::exit(ui::headless::run(
            spec,
            api_tester::app_root(),
            "dev.qos.api-tester",
        ));
    }

    let (state, autosave) = ui::persist::open("dev.qos.api-tester");
    let mut app = ui::App::new(state, api_tester::app_root()).with_autosave(autosave);
    // Register the API tester's actions so the shell's Ctrl+K palette can decode
    // + replay the same collection / request / environment / variable actions the
    // on-screen affordances dispatch.
    api_tester::actions::register(app.action_registry_mut());
    app.run();
    Ok(())
}

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

/// Parse `--viewport WxH` (e.g. `--viewport 560x800`). Returns `None` if the
/// flag is absent or malformed, so the caller falls back to the default size.
fn parse_viewport(args: &[String]) -> Option<(u32, u32)> {
    let mut iter = args.iter().skip(1);
    let spec = loop {
        let arg = iter.next()?;
        if arg == "--viewport" {
            break iter.next()?.clone();
        }
        if let Some(rest) = arg.strip_prefix("--viewport=") {
            break rest.to_string();
        }
    };
    let (w, h) = spec.split_once(['x', 'X'])?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// Whether a bare flag like `--rail-open` is present.
fn parse_flag(args: &[String], flag: &str) -> bool {
    args.iter().skip(1).any(|a| a == flag)
}

/// Run one offscreen frame against `widget` and write RGBA to `out` as PNG.
/// Mirrors `ui::App`'s cycle: sync → `build_layout` → `compute_layout` → `apply_layout` → draw.
fn render_shot_to_png(
    mut widget: Box<dyn Widget>,
    viewport: (u32, u32),
    out: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    // `QOS_SHOTS_THEME=light|high-contrast` renders the shot under that theme so
    // theme-driven chrome can be proven across palettes; defaults to dark.
    let theme = match std::env::var("QOS_SHOTS_THEME").ok().as_deref() {
        Some("light") => Theme::light(),
        Some("high-contrast") => Theme::high_contrast(),
        _ => Theme::dark(),
    };
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
    println!("api_tester: wrote shot to {}", out.display());
    Ok(())
}
