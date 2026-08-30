//! Camera — thin binary entry point. All logic lives in the library.
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

const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();

    // Headless helper: populate a directory with real image files (procedural
    // scenes + a scannable QR poster) so the disk-backed gallery / offline
    // scanner can be screenshotted against real content. Dev/test only.
    if let Some(dir) = parse_flag(&args, "--emit-photos") {
        emit_photos(std::path::Path::new(&dir))?;
        println!("camera: wrote sample photos to {dir}");
        return Ok(());
    }

    if let Some(out) = parse_shots_arg(&args) {
        // Shots-only flags mirror the env hooks the widget reads, so the
        // headless harness can reach every state without relying on the
        // parent shell's environment surviving the sandbox boundary. None of
        // these affect the live event-loop path.
        if let Some(screen) = parse_flag(&args, "--screen") {
            unsafe { std::env::set_var("CAMERA_SCREEN", screen) };
        }
        if let Some(dir) = parse_flag(&args, "--dir") {
            unsafe { std::env::set_var("CAMERA_DIR", dir) };
        }
        if let Some(px) = parse_flag(&args, "--scroll") {
            unsafe { std::env::set_var("CAMERA_SCROLL", px) };
        }
        if has_flag(&args, "--select") {
            unsafe { std::env::set_var("CAMERA_SELECT", "1") };
        }
        if has_flag(&args, "--scan-first") {
            unsafe { std::env::set_var("CAMERA_SCAN_FIRST", "1") };
        }
        let (dw, dh) = DEFAULT_SHOTS_VIEWPORT;
        // `--viewport WxH` is a convenience alias other apps accept; it seeds
        // the width/height, which `--width`/`--height` can still override.
        let (vw, vh) = parse_flag(&args, "--viewport")
            .and_then(|s| parse_viewport(&s))
            .unwrap_or((dw, dh));
        let w = parse_flag(&args, "--width")
            .and_then(|s| s.parse().ok())
            .unwrap_or(vw);
        let h = parse_flag(&args, "--height")
            .and_then(|s| s.parse().ok())
            .unwrap_or(vh);
        // Screenshots + tests default to the hermetic sample source — never
        // the disk or a camera device. `--disk` (or `CAMERA_SHOTS_DISK`) opts
        // into the real live source (honoring `--dir`/`CAMERA_DIR`) to capture
        // the real empty-library / no-camera / offline-scan states.
        let disk = has_flag(&args, "--disk") || std::env::var_os("CAMERA_SHOTS_DISK").is_some();
        let root = if disk {
            camera::app_root()
        } else {
            camera::shots_root()
        };
        render_shot_to_png(root, (w, h), &out)?;
        return Ok(());
    }

    // Headless drive (`--headless-drive '<json>'`): run a list of clicks and
    // keystrokes against the real widget tree with no display, persisting
    // through the same store the live app uses.
    if let Some(spec) = ui::headless::spec_from_args(&args) {
        std::process::exit(ui::headless::run(
            spec,
            camera::app_root(),
            "dev.qos.camera",
        ));
    }

    let (state, autosave) = ui::persist::open("dev.qos.camera");
    let mut app = ui::App::new(state, camera::app_root()).with_autosave(autosave);
    // Register Camera's actions so the shell's Ctrl+K palette can decode + replay
    // the same gallery/scan metadata actions the on-screen affordances dispatch.
    camera::actions::register(app.action_registry_mut());
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

/// True if a bare `--flag` is present.
fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

/// Write a handful of real PNGs into `dir`: procedural scenes plus one QR
/// poster the offline scanner can decode. Timestamped writes are spaced so
/// each lands with a distinct name. Headless dev/test tooling only.
fn emit_photos(dir: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    use camera::model::PhotoFormat;
    std::fs::create_dir_all(dir)?;
    for seed in 0..5u8 {
        let fb = camera::frame::thumbnail(seed, 960, 640);
        camera::library::write_photo(dir, &fb, PhotoFormat::Jpeg)?;
        std::thread::sleep(std::time::Duration::from_millis(4));
    }
    // The QR poster is written lossless (PNG) so the offline scanner decodes
    // it cleanly.
    let (qr, _) = camera::frame::scanner_frame(
        4,
        900,
        640,
        "WIFI:T:WPA2;S:qOS Lounge;P:welcome-aboard;H:false;;",
    );
    camera::library::write_photo(dir, &qr, PhotoFormat::Png)?;
    Ok(())
}

/// Parse a `WxH` viewport spec (e.g. `1280x800`) into `(width, height)`.
/// Accepts a lowercase `x` separator; both halves must be positive integers.
fn parse_viewport(spec: &str) -> Option<(u32, u32)> {
    let (w, h) = spec.split_once('x')?;
    let w: u32 = w.trim().parse().ok()?;
    let h: u32 = h.trim().parse().ok()?;
    if w == 0 || h == 0 {
        return None;
    }
    Some((w, h))
}

/// Read `--flag VALUE` or `--flag=VALUE` from the argument list.
fn parse_flag(args: &[String], flag: &str) -> Option<String> {
    let eq = format!("{flag}=");
    let mut iter = args.iter().skip(1);
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
    println!("camera: wrote shot to {}", out.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewport_alias_parses_width_and_height() {
        assert_eq!(parse_viewport("1280x800"), Some((1280, 800)));
        assert_eq!(parse_viewport("1900x1000"), Some((1900, 1000)));
    }

    #[test]
    fn viewport_rejects_malformed_specs() {
        assert_eq!(parse_viewport("1280"), None);
        assert_eq!(parse_viewport("1280x"), None);
        assert_eq!(parse_viewport("x800"), None);
        assert_eq!(parse_viewport("0x800"), None);
        assert_eq!(parse_viewport("1280x0"), None);
        assert_eq!(parse_viewport("abc"), None);
    }

    #[test]
    fn viewport_flag_seeds_dimensions_and_width_height_override() {
        let args: Vec<String> = ["camera", "--shots", "out.png", "--viewport", "1280x800"]
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        let (dw, dh) = DEFAULT_SHOTS_VIEWPORT;
        let (vw, vh) = parse_flag(&args, "--viewport")
            .and_then(|s| parse_viewport(&s))
            .unwrap_or((dw, dh));
        let w = parse_flag(&args, "--width")
            .and_then(|s| s.parse().ok())
            .unwrap_or(vw);
        let h = parse_flag(&args, "--height")
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(vh);
        assert_eq!((w, h), (1280, 800));

        // An explicit --width still wins over the alias.
        let args2: Vec<String> = ["camera", "--viewport", "1280x800", "--width", "640"]
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        let (vw2, _vh2) = parse_flag(&args2, "--viewport")
            .and_then(|s| parse_viewport(&s))
            .unwrap_or((dw, dh));
        let w2 = parse_flag(&args2, "--width")
            .and_then(|s| s.parse().ok())
            .unwrap_or(vw2);
        assert_eq!(w2, 640);
    }
}
