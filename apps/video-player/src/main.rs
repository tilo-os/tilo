//! video-player — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default          — run the live winit event loop via `ui::App`.
//!   * `--open PATH`     — open a local or `/var/workspace` file on launch.
//!   * `--shots PATH`    — render one offscreen frame to a PNG and exit
//!     (headless screenshot; identical passes to the live app).
//!
//! Optional flags for `--shots`:
//!   * `--viewport WxH`  — render at a custom size (default 1280x800).
//!   * `--open PATH`     — stage a real file for the captured frame (else a
//!     synthesized demo clip is shown).

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
use std::path::PathBuf;

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

/// # Errors
///
/// Returns an error if launching the app or running a `--shots` capture fails.
pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    ui::launch::run(
        &LaunchSpec::new_with_args("dev.qos.video-player", live_root)
            .shot_handler(render_shot)
            .allowed_live_options(&["--open"])
            .max_positionals(1)
            .scenarios(&[
                "empty",
                "error",
                "native-error",
                "playing",
                "paused",
                "settings",
                "metadata",
                "library",
                "library-focus",
                "recent",
                "fullscreen",
            ]),
    )?;
    Ok(())
}

fn requested_path(args: &LaunchArgs) -> Option<PathBuf> {
    args.value("--open")
        .map(PathBuf::from)
        .or_else(|| args.positionals().first().map(PathBuf::from))
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by ui::launch::LaunchSpec::new_with_args"
)]
fn live_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    Ok(video_player::app_root(requested_path(args)))
}

fn render_shot(args: &LaunchArgs) -> Result<(), LaunchError> {
    let out = args.shots.as_deref().ok_or_else(|| {
        LaunchError::Operation("video-player screenshot handler called without --shots".into())
    })?;
    let viewport = args.viewport.unwrap_or(DEFAULT_SHOTS_VIEWPORT);
    let mut app = video_player::PlayerApp::for_shot(requested_path(args), args.scenario.as_deref());
    // Real decode prerolls asynchronously; pump until its first picture is
    // ready. The synthesized fallback produces one on the first pass.
    let mut warm = ui::State::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        app.sync(&mut warm.doc, &warm.root);
        if app.has_media_frame() || std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(40));
    }
    render_shot_to_png(Box::new(app), viewport, out)
        .map_err(|error| LaunchError::Operation(error.to_string()))
}

/// Render one offscreen frame of `widget` and write it as a PNG. Mirrors
/// `ui::App`'s cycle: sync → `build_layout` → `compute_layout` → `apply_layout` → draw.
fn render_shot_to_png(
    mut widget: Box<dyn Widget>,
    viewport: (u32, u32),
    out: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let theme = match std::env::var("QOS_SHOTS_THEME").ok().as_deref() {
        Some("light") => Theme::light(),
        Some("high-contrast") => Theme::high_contrast(),
        _ => Theme::dark(),
    }
    .with_reduce_motion(
        ["QOS_A11Y_REDUCE_MOTION", "QOS_REDUCE_MOTION"]
            .into_iter()
            .filter_map(|key| std::env::var(key).ok())
            .any(|value| {
                let value = value.trim();
                !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
            }),
    );
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    let mut state = ui::State::new();
    let (w, h) = viewport;

    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    // The offscreen renderer bypasses the regular app loop, so explicitly
    // propagate the accessibility motion preference before building the scene.
    widget.set_motion_reduced(theme.reduce_motion);
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
    println!("video-player: wrote shot to {}", out.display());
    Ok(())
}
