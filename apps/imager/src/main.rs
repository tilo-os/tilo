//! Image Writer — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default            — run the live winit event loop via `ui::App`.
//!   * `--shots PATH`     — render one offscreen frame to a PNG and exit.
//!                          `--scenario NAME` picks which wizard state
//!                          (defaults to `source`); `--viewport WxH` sizes it.
//!   * `--shots-dir DIR`  — render *every* wizard scenario into `DIR` as
//!                          `NN-name.png` (the screenshot gallery).

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

const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1180, 820);

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    let viewport = parse_value_arg(&args, "--viewport")
        .and_then(|s| parse_viewport(&s))
        .unwrap_or(DEFAULT_SHOTS_VIEWPORT);

    // Render every scenario into a directory.
    if let Some(dir) = parse_value_arg(&args, "--shots-dir") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).ok();
        for (i, scenario) in imager::SHOT_SCENARIOS.iter().enumerate() {
            let out = dir.join(format!("{:02}-{}.png", i + 1, scenario));
            render_scenario(scenario, viewport, &out)?;
        }
        return Ok(());
    }

    // Single-shot PNG.
    if let Some(out) = parse_shots_arg(&args) {
        let scenario = parse_value_arg(&args, "--scenario").unwrap_or_else(|| "source".to_string());
        render_scenario(&scenario, viewport, &out)?;
        return Ok(());
    }

    // Live mode: enumerate real write targets (lsblk). The `--shots` paths
    // above never set this, so a screenshot stays on the curated sample and is
    // fully populated offline.
    // SAFETY: set once at startup before any threads read the environment.
    unsafe {
        std::env::set_var("IMAGER_LIVE", "1");
    }
    let state = ui::State::new();
    let app = ui::App::new(state, imager::app_root());
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

/// Parse a `--key VALUE` or `--key=VALUE` flag.
fn parse_value_arg(args: &[String], key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == key {
            return iter.next().cloned();
        }
        if let Some(rest) = arg.strip_prefix(&prefix) {
            return Some(rest.to_string());
        }
    }
    None
}

/// Parse a `WIDTHxHEIGHT` viewport spec (e.g. `1180x820`).
fn parse_viewport(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once(['x', 'X'])?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// Seed `scenario`, run one offscreen frame, and write the PNG.
fn render_scenario(
    scenario: &str,
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

    let mut widget = imager::app_root();
    let mut state = ui::State::new();
    imager::seed_scenario(&mut state.doc, scenario);
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
    println!("imager: wrote {} shot to {}", scenario, out.display());
    Ok(())
}
