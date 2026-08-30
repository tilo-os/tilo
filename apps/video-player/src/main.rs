//! video-player — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default          — run the live winit event loop via `ui::App`.
//!   * `--open PATH`     — open a local or `/var/workspace` file on launch.
//!   * `--shots PATH`    — render one offscreen frame to a PNG and exit
//!                          (headless screenshot; identical passes to the live app).
//!
//! Optional flags for `--shots`:
//!   * `--viewport WxH`  — render at a custom size (default 1280x800).
//!   * `--open PATH`     — stage a real file for the captured frame (else a
//!                          synthesized demo clip is shown).

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
        reason = "a test that unwraps is stating an invariant"
    )
)]

use std::collections::HashMap;
use std::path::PathBuf;

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
    let open = parse_value_arg(&args, "--open")
        .map(PathBuf::from)
        .or_else(|| {
            (args.len() == 2 && !args[1].starts_with('-')).then(|| PathBuf::from(&args[1]))
        });

    if let Some(out) = parse_value_arg(&args, "--shots").map(PathBuf::from) {
        let viewport = parse_value_arg(&args, "--viewport")
            .and_then(|s| parse_viewport(&s))
            .unwrap_or(DEFAULT_SHOTS_VIEWPORT);
        // Build the staged player and warm it up: real decode prerolls its first
        // frame asynchronously, so pump `sync` until a picture is on the surface
        // (the synthesized fallback produces one on the first pump). This makes a
        // `--features decode --open <file>` capture a genuine decoded frame.
        let scenario = parse_value_arg(&args, "--scenario");
        let mut app = video_player::PlayerApp::for_shot(open, scenario.as_deref());
        {
            let mut warm = ui::State::new();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                app.sync(&mut warm.doc, &warm.root);
                if app.has_media_frame() || std::time::Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(40));
            }
        }
        render_shot_to_png(Box::new(app), viewport, &out)?;
        return Ok(());
    }

    // Headless drive (`--headless-drive '<json>'`): run a list of clicks and
    // keystrokes against the real widget tree with no display, persisting
    // through the same store the live app uses.
    if let Some(spec) = ui::headless::spec_from_args(&args) {
        std::process::exit(ui::headless::run(
            spec,
            video_player::app_root(open),
            "dev.qos.video-player",
        ));
    }

    let (state, autosave) = ui::persist::open("dev.qos.video-player");
    let app = ui::App::new(state, video_player::app_root(open)).with_autosave(autosave);
    app.run();
    Ok(())
}

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

fn parse_viewport(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once(['x', 'X'])?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
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
    };
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    let mut state = ui::State::new();
    let (w, h) = viewport;

    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    widget.sync(&mut state.doc, &state.root);
    let child = widget.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(w as f32),
                    height: Dimension::length(h as f32),
                },
                ..Default::default()
            },
            &[child],
        )
        .expect("shot root");
    {
        let measure_map = node_map.clone();
        let tr = &mut text_renderer;
        taffy
            .compute_layout_with_measure(
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
            )
            .expect("shot layout");
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
