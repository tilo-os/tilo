//! Disk Imager — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default — run the live winit event loop via `ui::App`, starting blank
//!     with a real `lsblk` drive probe.
//!   * `--demo` — run live but seed sample content (drives, a qOS image,
//!     recent history) for a walkthrough / review.
//!   * `--shots PATH` — render one offscreen frame to a PNG and exit.
//!     `--scenario NAME` picks which wizard station (defaults to `source`);
//!     `--viewport WxH` sizes it.
//!   * `--shots-dir DIR` — render *every* wizard scenario into `DIR` as
//!     `NN-name.png` (the screenshot gallery).

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

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

const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1180, 820);

/// # Errors
///
/// Returns an error if a `--shots`/`--shots-dir` render fails: computing the
/// offscreen layout for a scenario, or writing its PNG to disk.
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

    // `--demo`: seed sample content (drives, a qOS image, recent history) for a
    // walkthrough or review, instead of probing real hardware. The default live
    // run stays blank and enumerates real write targets (lsblk).
    let demo = args.iter().any(|a| a == "--demo");
    if !demo {
        // SAFETY: set once at startup before any threads read the environment.
        unsafe {
            std::env::set_var("IMAGER_LIVE", "1");
        }
    }
    // Headless drive (`--headless-drive '<json>'`): run a list of clicks and
    // keystrokes against the real widget tree with no display, persisting
    // through the same store the live app uses.
    if let Some(spec) = ui::headless::spec_from_args(&args) {
        std::process::exit(ui::headless::run(
            &spec,
            imager::app_root(),
            "dev.qos.imager",
        ));
    }

    let (mut state, autosave) = ui::persist::open("dev.qos.imager");
    if demo {
        imager::doc::ImagerDoc::seed_demo(&mut state.doc);
    }
    let mut app = ui::App::new(state, imager::app_root()).with_autosave(autosave);
    // Register the imager's install-profile actions so the shell's Ctrl+K palette
    // can decode + replay the same profile-customization actions the on-screen
    // toggles and fields dispatch.
    imager::actions::register(app.action_registry_mut());
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
    // `QOS_SHOTS_THEME=light|high-contrast` renders the shot under that theme
    // so the same frame can be graded in both light and dark (default dark).
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

    let mut widget = imager::app_root_for_shot(scenario);
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
