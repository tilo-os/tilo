//! Photos — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default              — the live winit event loop via `ui::App`.
//!   * `--open PATH`        — start with that photograph open.
//!   * `--shots PATH`       — render one offscreen frame to a PNG and exit.
//!     `--scenario NAME` picks the pose (see [`photos::SHOT_SCENARIOS`]),
//!     `--size WxH` the viewport, and `QOS_SHOTS_THEME=light|dark|
//!     high-contrast` the palette.

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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    ui::launch::run(
        &LaunchSpec::new("dev.qos.photos", photos::app_root)
            .open_state_with_args(open_state)
            .shot_handler(render_shot)
            .allowed_options(&["--size"])
            .allowed_live_options(&["--open"])
            .max_positionals(1)
            .scenarios(photos::SHOT_SCENARIOS)
            .configure_app(configure_app),
    )?;
    Ok(())
}

fn requested_path(args: &LaunchArgs) -> Option<&str> {
    args.value("--open")
        .or_else(|| args.positionals().first().map(String::as_str))
}

fn open_state(app_id: &'static str, args: &LaunchArgs) -> (ui::State, ui::persist::Autosave) {
    let (mut state, autosave) = ui::persist::open(app_id);
    if let Some(path) = requested_path(args) {
        let mut opener = photos::PhotosApp::new();
        photos::strings::install();
        opener.open_path(&mut state.doc, path);
    }
    (state, autosave)
}

fn configure_app(app: &mut ui::App) {
    photos::actions::register(app.action_registry_mut());
    #[cfg(not(debug_assertions))]
    {
        app.debug_mode = false;
    }
}

fn render_shot(args: &LaunchArgs) -> Result<(), LaunchError> {
    let out = args.shots.as_deref().ok_or_else(|| {
        LaunchError::Operation("Photos screenshot handler called without --shots".into())
    })?;
    let scenario = args.scenario.as_deref().unwrap_or("opened");
    let viewport = args
        .value("--size")
        .and_then(parse_size)
        .or(args.viewport)
        .unwrap_or(DEFAULT_SHOTS_VIEWPORT);
    render_scenario(scenario, viewport, out)
        .map_err(|error| LaunchError::Operation(error.to_string()))
}

/// Parse `WxH`.
fn parse_size(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once(['x', 'X', '*'])?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// Render one scenario offscreen. Mirrors `ui::App`'s frame exactly:
/// sync → `build_layout` → `compute_layout` → `apply_layout` → draw.
fn render_scenario(
    scenario: &str,
    viewport: (u32, u32),
    out: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    let theme = match std::env::var("QOS_SHOTS_THEME").ok().as_deref() {
        Some("light") => Theme::light(),
        Some("high-contrast") => Theme::high_contrast(),
        _ => Theme::dark(),
    };
    photos::strings::install();

    // The generated pattern is written beside the output, so a shot run
    // leaves no file anywhere the live app would ever look.
    let staging = out
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("photos-shots");
    let mut state = ui::State::new();
    let mut app = photos::PhotosApp::new();
    photos::seed_scenario(&mut app, &mut state.doc, scenario, &staging);

    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    // Kept concrete rather than boxed as `dyn Widget`: the `cropping`
    // scenario has to zoom the viewer between the two passes, and the viewer
    // only exists once the first pass has synced it into being.
    let mut widget = app;

    // Two passes: the first lays the canvas out so the viewer knows its rect,
    // the second renders with the framing that produced. A single pass would
    // draw the "cropping" scenario before its viewport existed.
    let mut scene = Scene::new();
    for pass in 0..2 {
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

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

        if pass == 0 && scenario == "cropping" {
            widget.set_zoom(photos::CROPPING_SCENARIO_ZOOM);
        }

        scene = Scene::new();
        widget.draw_with_presence(
            &mut scene,
            Affine::IDENTITY,
            &mut text_renderer,
            &theme,
            &presence,
            &permission,
        );
    }

    widget.draw_overlay(
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
    println!("photos: wrote {scenario} shot to {}", out.display());
    Ok(())
}
