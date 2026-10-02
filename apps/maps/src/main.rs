#![allow(
    clippy::manual_clamp,
    clippy::manual_midpoint,
    clippy::needless_pass_by_value,
    reason = "migration stack lint debt is scoped to this target"
)]

//! Maps — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default        — run the live winit event loop via `ui::App`.
//!   * `--shots PATH` — render one offscreen frame to a PNG and exit
//!     (headless screenshot; identical passes to the live app).
//!     `--viewport WxH` sets the frame size and `--nav-closed`
//!     forces the overlay drawer shut — both affect only the
//!     screenshot path, never the live app. Scenario flags
//!     (`--pois`, `--search`, `--offline`, `--panel …`, …)
//!     stage a specific surface for the shot.

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

use ui::launch::{LaunchArgs, LaunchError, LaunchSpec, ShotFrame};
use ui::theme::Theme;
use ui::widget::Widget;

const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);

#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    if standalone_capture() {
        ui::launch::run_or_exit(&shot_launch_spec());
        return;
    }
    ui::framework::run::<maps::MapsShell>();
}

fn standalone_capture() -> bool {
    std::env::args().any(|arg| {
        arg == "--shots"
            || arg.starts_with("--shots=")
            || arg == "--responsive-matrix"
            || arg.starts_with("--responsive-matrix=")
    })
}

fn shot_launch_spec() -> LaunchSpec {
    LaunchSpec::new("so.tilo.maps", maps::app_root)
        .shot_root(shot_root)
        .shot_handler(shot_handler)
        .allowed_flags(&[
            "--nav-closed",
            "--empty",
            "--edit-from",
            "--transit-arrive",
            "--search",
            "--pois",
            "--traffic",
            "--offline",
            "--menu",
            "--attribution",
            "--error",
            "--floating-navigation",
            "--floating-navigation-attached",
            "--floating-navigation-hover",
            "--floating-navigation-returning",
            "--floating-navigation-returned",
        ])
        .allowed_options(&["--panel"])
        .configure_app(|app| maps::actions::register(app.action_registry_mut()))
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is dictated by ui::launch::ShotFactory"
)]
fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    Ok(Box::new(shot_app(args)))
}

fn shot_app(args: &LaunchArgs) -> maps::MapsApp {
    // Headless: no event loop and no network — use an offline app so the
    // tile worker is never spawned and nothing blocks on I/O. The default
    // offline app renders the bundled sample city; `--empty` mirrors the
    // live first-run state (empty model, plain basemap).
    let mut app = if args.has("--empty") {
        maps::MapsApp::offline_empty()
    } else {
        maps::MapsApp::offline()
    };
    if args.has("--nav-closed") {
        app.set_nav_open(false);
    }
    let mut panel = None;
    let mut transit_mode = false;
    if let Some(value) = args.value("--panel") {
        apply_panel(value, &mut panel, &mut transit_mode);
    }
    if let Some(panel) = panel {
        app.set_active_panel(panel);
    }
    if transit_mode {
        app.set_travel_mode(maps::TravelMode::Transit);
    }
    if args.has("--edit-from") {
        app.set_demo_edit_origin();
    }
    if args.has("--transit-arrive") {
        app.set_demo_transit_arrive();
    }
    if args.has("--search") {
        app.set_demo_search();
    }
    if args.has("--pois") {
        app.set_demo_pois();
    }
    if args.has("--traffic") {
        app.set_demo_traffic();
    }
    if args.has("--offline") {
        app.set_demo_offline();
    }
    if args.has("--menu") {
        app.set_demo_overflow();
    }
    if args.has("--attribution") {
        app.set_demo_attribution();
    }
    if args.has("--error") {
        app.set_demo_error();
    }
    app
}

fn floating_navigation_stage(args: &LaunchArgs) -> Option<&'static str> {
    [
        ("--floating-navigation", "detached"),
        ("--floating-navigation-attached", "attached"),
        ("--floating-navigation-hover", "hover"),
        ("--floating-navigation-returning", "returning"),
        ("--floating-navigation-returned", "returned"),
    ]
    .into_iter()
    .find_map(|(flag, stage)| args.has(flag).then_some(stage))
}

fn shot_handler(args: &LaunchArgs) -> Result<(), LaunchError> {
    let out = args.shots.as_deref().ok_or(LaunchError::ShotsUnsupported)?;
    render_shot_to_png(
        shot_app(args),
        args.viewport.unwrap_or(DEFAULT_SHOTS_VIEWPORT),
        ui::launch::shot_theme(args),
        out,
        floating_navigation_stage(args),
    )
    .map_err(|error| LaunchError::Operation(error.to_string()))
}

/// Resolve a `--panel` value. `transit` is not a panel — it opens Directions in
/// Transit travel mode, mirroring how the live app models it.
fn apply_panel(s: &str, panel: &mut Option<maps::Panel>, transit_mode: &mut bool) {
    match s.trim().to_ascii_lowercase().as_str() {
        "explore" => *panel = Some(maps::Panel::Explore),
        "directions" => *panel = Some(maps::Panel::Directions),
        "transit" => {
            *panel = Some(maps::Panel::Directions);
            *transit_mode = true;
        }
        _ => {}
    }
}

/// Run one offscreen frame and include app-owned floating surfaces in the same
/// order as the live compositor.
fn render_shot_to_png(
    mut app: maps::MapsApp,
    viewport: (u32, u32),
    theme: Theme,
    out: &std::path::Path,
    floating_navigation: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    app.set_motion_reduced(theme.reduce_motion);
    let mut frame = ShotFrame::new(viewport, theme);
    frame.sync(&mut app);
    frame.layout(&mut app)?;
    if let Some(stage) = floating_navigation {
        app.debug_stage_navigation_for_shot(stage);
    }
    let mut scene = frame.draw(&mut app);
    frame.draw_floating_surfaces(&mut app, &mut scene);
    frame.write_png(&scene, out);
    println!("maps: wrote shot to {}", out.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_capture_options_map_to_panels() {
        // Phase `transit_option_preserves_the_directions_plus_transit_mapping`.
        {
            let mut panel = None;
            let mut transit = false;
            apply_panel("transit", &mut panel, &mut transit);
            assert_eq!(panel, Some(maps::Panel::Directions));
            assert!(transit);
        }

        // Phase `unknown_panel_preserves_the_default_capture`.
        {
            let mut panel = None;
            let mut transit = false;
            apply_panel("unknown", &mut panel, &mut transit);
            assert_eq!(panel, None);
            assert!(!transit);
        }
    }
}
