//! Video Editor — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default        — run the live winit event loop via `ui::App`.
//!   * `--shots PATH` — render one offscreen frame to a PNG and exit
//!                       (headless screenshot; identical passes to the live app).

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

const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);
/// Narrow capture viewport — below the bin's collapse breakpoint so `--shots`
/// can exercise the overlay-flap layout. Pass `--narrow` to select it.
const NARROW_SHOTS_VIEWPORT: (u32, u32) = (720, 820);

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    // `--scene-digest` runs the capture up to (but not into) the GPU and
    // prints a hash of the scene, so two processes can be asked whether they
    // composed the same picture. It routes through `--shots` itself rather
    // than a parallel path, so the number always describes the real capture.
    let digest = args.iter().any(|a| a == "--scene-digest");
    if let Some(out) = parse_shots_arg(&args).or_else(|| digest.then(std::path::PathBuf::new)) {
        // Point the FUSE-media seam at the bundled sample-media/ dir so the
        // capture resolves the demo project's media to local (hydrated)
        // sources — exercising the hydration branch without a live mount.
        // Honour an existing override if the caller already set one.
        if std::env::var_os("QOS_VE_MEDIA_DIR").is_none() {
            let bundled = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("sample-media");
            if bundled.exists() {
                // Safety: set before any threads spawn for the one-shot render.
                unsafe {
                    std::env::set_var("QOS_VE_MEDIA_DIR", bundled);
                }
            }
        }
        // Scenario selectors (deterministic single-frame captures):
        //   --narrow             collapsed-bin overlay layout
        //   --scenario filters   clip selected with colour filters + fades
        //   --scenario split     timeline just after a split at the playhead
        //   (none)               the full live editor
        let scenario = parse_scenario_arg(&args);
        // `--empty` renders the real live app (a blank project on a fresh
        // machine) so the first-run empty state can be screenshotted; every
        // other shot uses the seeded demo build.
        let empty = args.iter().any(|a| a == "--empty");
        // `--import PATH` seeds a real imported clip (probed for real metadata)
        // and, under `--features decode`, warms the pipeline so the capture
        // shows the clip's true decoded frame — not the representative gradient.
        if let Some(path) = parse_value_arg(&args, "--import") {
            let mut app =
                video_editor::VideoEditorApp::new_import_placed(vec![std::path::PathBuf::from(
                    path,
                )]);
            // Warm up on ONE state so the import + the async-prerolled decoded
            // frame persist into the same document the render then draws from.
            let mut state = ui::State::new();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
            loop {
                app.sync(&mut state.doc, &state.root);
                if app.has_preview_frame() || std::time::Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(40));
            }
            emit(Box::new(app), DEFAULT_SHOTS_VIEWPORT, &out, state, digest)?;
            return Ok(());
        }
        if empty {
            emit(
                video_editor::app_root(),
                DEFAULT_SHOTS_VIEWPORT,
                &out,
                ui::State::new(),
                digest,
            )?;
        } else if args.iter().any(|a| a == "--narrow") {
            emit(
                video_editor::app_root_narrow(),
                NARROW_SHOTS_VIEWPORT,
                &out,
                ui::State::new(),
                digest,
            )?;
        } else {
            // Every scenario resolves through the same `ShotScenario` selector
            // the app itself defines, so adding one there is enough to make it
            // capturable — no second list to keep in step.
            match scenario
                .as_deref()
                .and_then(video_editor::app::ShotScenario::from_arg)
            {
                Some(s) => emit(
                    Box::new(video_editor::VideoEditorApp::new_for_shot(s)),
                    DEFAULT_SHOTS_VIEWPORT,
                    &out,
                    ui::State::new(),
                    digest,
                )?,
                None => emit(
                    video_editor::app_root_demo(),
                    DEFAULT_SHOTS_VIEWPORT,
                    &out,
                    ui::State::new(),
                    digest,
                )?,
            }
        }
        return Ok(());
    }

    // Headless drive (`--headless-drive '<json>'`): run a list of clicks and
    // keystrokes against the real widget tree with no display, persisting
    // through the same store the live app uses.
    if let Some(spec) = ui::headless::spec_from_args(&args) {
        std::process::exit(ui::headless::run(
            &spec,
            video_editor::app_root(),
            "dev.qos.video-editor",
        ));
    }

    let (state, autosave) = ui::persist::open("dev.qos.video-editor");
    let mut app = ui::App::new(state, video_editor::app_root()).with_autosave(autosave);
    // Register the editor's actions so the shell's Ctrl+K palette can decode +
    // replay the same timeline actions the on-screen affordances dispatch.
    video_editor::actions::register(app.action_registry_mut());
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

/// Generic `--flag VALUE` / `--flag=VALUE` reader.
fn parse_value_arg(args: &[String], flag: &str) -> Option<String> {
    let mut iter = args.iter().skip(1);
    let eq = format!("{flag}=");
    while let Some(arg) = iter.next() {
        if arg == flag {
            return iter.next().cloned();
        }
        if let Some(rest) = arg.strip_prefix(&eq) {
            return Some(rest.to_string());
        }
    }
    None
}

/// `--scenario <name>` (or `--scenario=<name>`) selects a deterministic
/// capture scenario. Returns the lowercased name if present.
fn parse_scenario_arg(args: &[String]) -> Option<String> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--scenario" {
            return iter.next().map(|s| s.to_lowercase());
        }
        if let Some(rest) = arg.strip_prefix("--scenario=") {
            return Some(rest.to_lowercase());
        }
    }
    None
}

/// `QOS_SHOTS_SIZE=WxH` as a viewport, if it parses to a sane size.
fn parse_size_env() -> Option<(u32, u32)> {
    let raw = std::env::var("QOS_SHOTS_SIZE").ok()?;
    let (w, h) = raw.split_once(['x', 'X'])?;
    let w: u32 = w.trim().parse().ok()?;
    let h: u32 = h.trim().parse().ok()?;
    (w >= 160 && h >= 160 && w <= 8192 && h <= 8192).then_some((w, h))
}

/// Run one offscreen frame against `widget` and write RGBA to `out` as PNG.
/// Mirrors `ui::App`'s cycle: sync → `build_layout` → `compute_layout` → `apply_layout` → draw.
///
/// `state` carries the document: a fresh one for the seeded/demo scenarios, or
/// a **warmed** one (already synced through the import + decode warm-up) so the
/// imported media persists into the rendered frame instead of being wiped by a
/// fresh empty sync.
/// Write the capture, or print its fingerprint when only the number is wanted.
fn emit(
    widget: Box<dyn Widget>,
    viewport: (u32, u32),
    out: &std::path::Path,
    state: ui::State,
    digest: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if digest {
        print_scene_digest(widget, viewport, state)
    } else {
        render_shot_to_png(widget, viewport, out, state)
    }
}

fn render_shot_to_png(
    mut widget: Box<dyn Widget>,
    viewport: (u32, u32),
    out: &std::path::Path,
    mut state: ui::State,
) -> Result<(), Box<dyn std::error::Error>> {
    let (size, theme) = shot_config(viewport);
    let mut text_renderer = TextRenderer::new();
    let scene =
        video_editor::shot::compose(&mut *widget, size, &theme, &mut state, &mut text_renderer)?;

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut renderer = OffscreenRenderer::new(size.0, size.1);
    renderer.render_png(&scene, Color::TRANSPARENT, out);
    println!("video_editor: wrote shot to {}", out.display());
    Ok(())
}

/// Fingerprint the scene a capture would rasterise, and print it.
///
/// The picture itself cannot be compared byte for byte — vello's rasteriser is
/// allowed a one-step wobble on antialiased edges (see `video_editor::shot`) —
/// but the scene behind it can, and that is the layer the editor decides. Two
/// runs of this printing two different numbers means the app composed two
/// different pictures, which is a bug in the app rather than in the GPU.
fn print_scene_digest(
    mut widget: Box<dyn Widget>,
    viewport: (u32, u32),
    mut state: ui::State,
) -> Result<(), Box<dyn std::error::Error>> {
    let (size, theme) = shot_config(viewport);
    let mut text_renderer = TextRenderer::new();
    let scene =
        video_editor::shot::compose(&mut *widget, size, &theme, &mut state, &mut text_renderer)?;
    println!("{:016x}", video_editor::shot::fingerprint(&scene));
    Ok(())
}

/// The viewport and theme a capture runs at: the scenario's own, unless
/// `QOS_SHOTS_SIZE=WxH` / `QOS_SHOTS_THEME` say otherwise. Without the size
/// override the app could only ever be photographed at two widths, and a
/// layout that collapsed at a third had nothing watching it.
fn shot_config(viewport: (u32, u32)) -> ((u32, u32), Theme) {
    let size = parse_size_env().unwrap_or(viewport);
    let theme = match std::env::var("QOS_SHOTS_THEME").ok().as_deref() {
        Some("light") => Theme::light(),
        Some("high-contrast") => Theme::high_contrast(),
        _ => Theme::dark(),
    };
    (size, theme)
}
