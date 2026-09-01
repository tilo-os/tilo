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

const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);

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
                "--floating-navigation",
                "--floating-navigation-attached",
                "--floating-navigation-hover",
                "--floating-navigation-returning",
                "--floating-navigation-returned",
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
    out: &std::path::Path,
    floating_navigation: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
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
    let widget: &mut dyn Widget = &mut app;
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
    if let Some(stage) = floating_navigation {
        app.debug_stage_navigation_for_shot(stage);
    }
    let widget: &mut dyn Widget = &mut app;
    let mut scene = Scene::new();
    widget.draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        &mut text_renderer,
        &theme,
        &presence,
        &permission,
    );
    widget.draw_floating_surfaces(
        &mut scene,
        Affine::IDENTITY,
        &mut text_renderer,
        &theme,
        &presence,
    );

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut renderer = OffscreenRenderer::new(w, h);
    renderer.render_png(&scene, Color::TRANSPARENT, out);
    println!("maps: wrote shot to {}", out.display());
    Ok(())
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
