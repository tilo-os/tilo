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

use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};
use ui::widget::Widget;

#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    // Install the rustls crypto provider before any HTTPS fetch. The tile worker
    // and the geocoding/routing client also call this, but doing it here at
    // startup keeps the provider set for the whole process regardless of which
    // request runs first. See `net::ensure_crypto_provider` for why it's needed.
    maps::net::ensure_crypto_provider();

    ui::launch::run_or_exit(
        &LaunchSpec::new("dev.qos.maps", maps::app_root)
            .shot_root(shot_root)
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
            ])
            .allowed_options(&["--panel"])
            .configure_app(|app| maps::actions::register(app.action_registry_mut())),
    );
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is dictated by ui::launch::ShotFactory"
)]
fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
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
    Ok(Box::new(app))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transit_option_preserves_the_directions_plus_transit_mapping() {
        let mut panel = None;
        let mut transit = false;
        apply_panel("transit", &mut panel, &mut transit);
        assert_eq!(panel, Some(maps::Panel::Directions));
        assert!(transit);
    }

    #[test]
    fn unknown_panel_preserves_the_default_capture() {
        let mut panel = None;
        let mut transit = false;
        apply_panel("unknown", &mut panel, &mut transit);
        assert_eq!(panel, None);
        assert!(!transit);
    }
}
