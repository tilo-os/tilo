//! Disks — thin binary entry point. All logic lives in the library.
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

use disks::DisksApp;
use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::taffy::prelude::*;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::vello::Scene;
use ui::vello::kurbo::{Affine, Point};
use ui::vello::peniko::Color;
use ui::widget::Widget;

const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);
/// A phone-width capture used to exercise the responsive overlay flap. Below
/// the app's 900px breakpoint the drive list collapses to an overlay.
const NARROW_SHOTS_VIEWPORT: (u32, u32) = (640, 800);

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    if let Some(out) = parse_shots_arg(&args) {
        // `--live` probes the real system (lsblk + smartctl) for the capture,
        // so the shot shows this machine's actual drives — degrading to the
        // honest empty state if nothing is found. Without it the shot uses the
        // curated offline sample (fully-populated, deterministic).
        if args.iter().any(|a| a == "--live") {
            // SAFETY: set once at startup before any threads read the env.
            unsafe {
                std::env::set_var("DISKS_LIVE", "1");
            }
        }
        // `--narrow` (or a sub-breakpoint `--viewport WxH`) renders the
        // collapsed layout with the drive-list flap revealed over its scrim.
        let narrow = args.iter().any(|a| a == "--narrow")
            || parse_viewport_arg(&args).is_some_and(|(w, _)| f64::from(w) < 900.0);
        let viewport = parse_viewport_arg(&args).unwrap_or(if narrow {
            NARROW_SHOTS_VIEWPORT
        } else {
            DEFAULT_SHOTS_VIEWPORT
        });
        // Optional preset so a capture can pin a specific drive / partition /
        // tab (used to verify the selection states offscreen).
        let preset = Preset {
            drive: parse_num_arg(&args, "--drive"),
            part: parse_num_arg(&args, "--part"),
            tab: parse_val_arg(&args, "--tab"),
            pending: parse_val_arg(&args, "--pending"),
        };
        render_shot_to_png(DisksApp::new(), viewport, narrow, preset, &out)?;
        return Ok(());
    }

    // Live mode: probe the real system (lsblk + smartctl). The `--shots`
    // path above never sets this, so the screenshot stays on the curated
    // sample topology and is fully populated offline.
    // SAFETY: set once at startup before any threads read the environment.
    unsafe {
        std::env::set_var("DISKS_LIVE", "1");
    }
    // Headless drive (`--headless-drive '<json>'`): run a list of clicks and
    // keystrokes against the real widget tree with no display, persisting
    // through the same store the live app uses.
    if let Some(spec) = ui::headless::spec_from_args(&args) {
        std::process::exit(ui::headless::run(spec, disks::app_root(), "dev.qos.disks"));
    }

    let (state, autosave) = ui::persist::open("dev.qos.disks");
    let app = ui::App::new(state, disks::app_root()).with_autosave(autosave);
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

/// Parse an optional `--viewport WIDTHxHEIGHT` (or `--viewport=WxH`) shots
/// override so a caller can pick any capture size. Returns `None` if absent
/// or malformed.
fn parse_viewport_arg(args: &[String]) -> Option<(u32, u32)> {
    let mut iter = args.iter().skip(1);
    let parse = |s: &str| -> Option<(u32, u32)> {
        let (w, h) = s.split_once(['x', 'X'])?;
        Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
    };
    while let Some(arg) = iter.next() {
        if arg == "--viewport" {
            return iter.next().and_then(|s| parse(s));
        }
        if let Some(rest) = arg.strip_prefix("--viewport=") {
            return parse(rest);
        }
    }
    None
}

/// Optional pinned state for a `--shots` capture.
#[derive(Default)]
struct Preset {
    drive: Option<usize>,
    part: Option<usize>,
    tab: Option<String>,
    /// Pre-arm a confirm modal, e.g. `format:0:2`, to capture the
    /// type-to-confirm gate offscreen.
    pending: Option<String>,
}

/// Parse `--flag N` / `--flag=N` as a `usize`.
fn parse_num_arg(args: &[String], flag: &str) -> Option<usize> {
    parse_val_arg(args, flag).and_then(|s| s.parse().ok())
}

/// Parse `--flag VALUE` / `--flag=VALUE` as a string.
fn parse_val_arg(args: &[String], flag: &str) -> Option<String> {
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

/// Run one offscreen frame against the app and write RGBA to `out` as PNG.
/// Mirrors `ui::App`'s cycle: sync → `build_layout` → `compute_layout` → `apply_layout` → draw.
/// `narrow` snaps the drive-list flap open *after* layout so the collapsed
/// overlay is captured settled rather than mid-animation.
fn render_shot_to_png(
    mut widget: DisksApp,
    viewport: (u32, u32),
    narrow: bool,
    preset: Preset,
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
    // Apply any pinned selection, then re-sync so the widget adopts it.
    if preset.drive.is_some()
        || preset.part.is_some()
        || preset.tab.is_some()
        || preset.pending.is_some()
    {
        if let Some(d) = preset.drive {
            disks::doc::DisksDoc::set_selected(&mut state.doc, d);
        }
        if let Some(p) = preset.part {
            disks::doc::DisksDoc::set_selected_part(&mut state.doc, Some(p));
        }
        if let Some(t) = &preset.tab {
            disks::doc::DisksDoc::set_tab(&mut state.doc, t);
        }
        if let Some(p) = &preset.pending {
            disks::doc::DisksDoc::set_pending(&mut state.doc, p);
        }
        widget.sync(&mut state.doc, &state.root);
    }

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
            ui::taffy::Size {
                width: AvailableSpace::Definite(w as f32),
                height: AvailableSpace::Definite(h as f32),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, tr) }
                } else {
                    ui::taffy::Size::ZERO
                }
            },
        )?;
    }
    widget.apply_layout(&taffy, child, Point::ORIGIN);
    // After layout the width (hence the docked/overlay split) is settled, so
    // snapping the flap here lands a fully-revealed overlay for the capture.
    if narrow {
        widget.snap_sidebar(true);
    }

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
    println!("disks: wrote shot to {}", out.display());
    Ok(())
}
