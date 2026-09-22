//! Disk Imager — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default — run the live winit event loop via `ui::App`, starting blank
//!     with a real `lsblk` drive probe.
//!   * `--demo` — run live but seed sample content (drives, a Tilo image,
//!     recent history) for a walkthrough / review.
//!   * `--shots PATH` — render one offscreen frame to a PNG and exit.
//!     `--scenario NAME` picks which wizard station (defaults to `source`);
//!     `--viewport WxH` sizes it.
//!   * `--shots-dir DIR` — render *every* wizard scenario into `DIR` as
//!     `NN-name.png` (the screenshot gallery).

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.
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
use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};
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

#[cfg(feature = "thin")]
extern crate runtime_dylib;

/// # Errors
///
/// Returns an error if a `--shots`/`--shots-dir` render fails: computing the
/// offscreen layout for a scenario, or writing its PNG to disk.
pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if let Some(code) = imager::write::maybe_run_write_helper(&args)? {
        std::process::exit(code);
    }
    // Packaging smoke probe: reaching this point proves the executable and
    // all runtime-linked libraries load without starting graphics, portals,
    // device discovery, or a privileged path.
    if args.iter().any(|arg| arg == "--tilo-package-smoke") {
        return Ok(());
    }
    if args.iter().any(|arg| arg == "--tilo-flatpak-probe-smoke") {
        if !cfg!(feature = "flatpak-media-writer")
            || std::env::var("TILO_IMAGER_FLATPAK").as_deref() != Ok("1")
        {
            return Err("the Flatpak probe smoke test is outside its sandbox".into());
        }
        let drives = imager::probe::probe()
            .ok_or("the Flatpak could not reconcile lsblk with host UDisks2 inventory")?;
        if drives.is_empty() {
            return Err("the Flatpak returned an empty drive inventory".into());
        }
        return Ok(());
    }
    let viewport = parse_value_arg(&args, "--viewport")
        .and_then(|s| parse_viewport(&s))
        .unwrap_or(DEFAULT_SHOTS_VIEWPORT);

    // Render every scenario into a directory.
    if let Some(dir) = parse_value_arg(&args, "--shots-dir") {
        let dir = std::path::PathBuf::from(dir);
        let text_scale = parse_value_arg(&args, "--text-scale")
            .as_deref()
            .and_then(parse_text_scale);
        std::fs::create_dir_all(&dir).ok();
        for (i, scenario) in imager::SHOT_SCENARIOS.iter().enumerate() {
            let out = dir.join(format!("{:02}-{}.png", i + 1, scenario));
            render_scenario(scenario, viewport, text_scale, &out)?;
        }
        return Ok(());
    }

    ui::launch::run(
        &LaunchSpec::new_with_args("so.tilo.imager", live_root)
            .shot_handler(render_shot)
            .allowed_live_flags(&["--demo"])
            .scenarios(imager::SHOT_SCENARIOS)
            .open_state_with_args(open_state)
            .configure_app(|app| imager::actions::register(app.action_registry_mut())),
    )?;
    Ok(())
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by LaunchSpec::new_with_args"
)]
fn live_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    if !args.has("--demo") {
        // Set once at startup, before the drive probe can start worker threads.
        unsafe { std::env::set_var("IMAGER_LIVE", "1") };
    }
    if args.has("--demo") {
        let mut app = imager::ImagerApp::new();
        app.seed_local_demo_state();
        Ok(Box::new(app))
    } else {
        Ok(imager::app_root())
    }
}

fn open_state(app_id: &'static str, args: &LaunchArgs) -> (ui::State, ui::persist::Autosave) {
    let (mut state, autosave) = ui::persist::open(app_id);
    if args.has("--demo") {
        imager::doc::ImagerDoc::seed_demo(&mut state.doc);
    }
    (state, autosave)
}

fn render_shot(args: &LaunchArgs) -> Result<(), LaunchError> {
    let out = args.shots.as_deref().ok_or(LaunchError::ShotsUnsupported)?;
    render_scenario(
        args.scenario.as_deref().unwrap_or("source"),
        args.viewport.unwrap_or(DEFAULT_SHOTS_VIEWPORT),
        args.text_scale,
        out,
    )
    .map_err(|error| LaunchError::Operation(error.to_string()))
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
    text_scale: Option<f32>,
    out: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    // `TILO_SHOTS_THEME=light|high-contrast` renders the shot under that theme
    // so the same frame can be graded in both light and dark (default dark).
    let mut theme = match std::env::var("TILO_SHOTS_THEME").ok().as_deref() {
        Some("light") => Theme::light(),
        Some("high-contrast") => Theme::high_contrast(),
        _ => Theme::dark(),
    };
    if let Some(scale) = text_scale {
        theme = theme.with_text_scale(scale);
    }
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

fn parse_text_scale(value: &str) -> Option<f32> {
    let scale: f32 = value.parse().ok()?;
    (scale.is_finite() && (0.75..=2.0).contains(&scale)).then_some(scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shot_text_scale_rejects_unsafe_values() {
        assert_eq!(parse_text_scale("1.3"), Some(1.3));
        for value in ["NaN", "0.5", "2.1", "not-a-number"] {
            assert_eq!(parse_text_scale(value), None, "accepted {value}");
        }
    }
}
