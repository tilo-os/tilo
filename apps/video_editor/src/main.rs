//! Video Editor — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default        — run the live winit event loop via `ui::App`.
//!   * `--shots PATH` — render one offscreen frame to a PNG and exit
//!     (headless screenshot; identical passes to the live app).

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
/// Narrow capture viewport — below the bin's collapse breakpoint so `--shots`
/// can exercise the overlay-flap layout. Pass `--narrow` to select it.
const NARROW_SHOTS_VIEWPORT: (u32, u32) = (720, 820);

#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    ui::launch::run_or_exit(
        &LaunchSpec::new("dev.qos.video-editor", video_editor::app_root)
            .shot_handler(shot_handler)
            .allowed_flags(&["--empty", "--narrow"])
            .allowed_options(&["--import"])
            .scenarios(&["filters", "split", "menu", "rename", "drag", "effects"])
            .configure_app(|app| video_editor::actions::register(app.action_registry_mut())),
    );
}

fn shot_handler(args: &LaunchArgs) -> Result<(), LaunchError> {
    render_shot(args).map_err(|error| LaunchError::Operation(error.to_string()))
}

fn render_shot(args: &LaunchArgs) -> Result<(), Box<dyn std::error::Error>> {
    let out = args.shots.as_deref().ok_or("missing screenshot output")?;
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
    let scenario = args.scenario.as_deref();
    // `--empty` renders the real live app (a blank project on a fresh
    // machine) so the first-run empty state can be screenshotted; every
    // other shot uses the seeded demo build.
    let empty = args.has("--empty");
    // `--import PATH` seeds a real imported clip (probed for real metadata)
    // and, under `--features decode`, warms the pipeline so the capture
    // shows the clip's true decoded frame — not the representative gradient.
    if let Some(path) = args.value("--import") {
        let mut app =
            video_editor::VideoEditorApp::new_import_placed(vec![std::path::PathBuf::from(path)]);
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
        render_shot_to_png(
            Box::new(app),
            args.viewport.unwrap_or(DEFAULT_SHOTS_VIEWPORT),
            out,
            state,
        )?;
        return Ok(());
    }
    if empty {
        render_shot_to_png(
            video_editor::app_root(),
            args.viewport.unwrap_or(DEFAULT_SHOTS_VIEWPORT),
            out,
            ui::State::new(),
        )?;
    } else if args.has("--narrow") {
        render_shot_to_png(
            video_editor::app_root_narrow(),
            args.viewport.unwrap_or(NARROW_SHOTS_VIEWPORT),
            out,
            ui::State::new(),
        )?;
    } else {
        let viewport = args.viewport.unwrap_or(DEFAULT_SHOTS_VIEWPORT);
        match scenario {
            Some("filters") => render_shot_to_png(
                video_editor::app_root_filters(),
                viewport,
                out,
                ui::State::new(),
            )?,
            Some("split") => render_shot_to_png(
                video_editor::app_root_split(),
                viewport,
                out,
                ui::State::new(),
            )?,
            Some("menu") => render_shot_to_png(
                video_editor::app_root_menu(),
                viewport,
                out,
                ui::State::new(),
            )?,
            Some("rename") => render_shot_to_png(
                video_editor::app_root_rename(),
                viewport,
                out,
                ui::State::new(),
            )?,
            Some("drag") => render_shot_to_png(
                video_editor::app_root_drag(),
                viewport,
                out,
                ui::State::new(),
            )?,
            Some("effects") => render_shot_to_png(
                video_editor::app_root_effects(),
                viewport,
                out,
                ui::State::new(),
            )?,
            _ => render_shot_to_png(
                video_editor::app_root_demo(),
                viewport,
                out,
                ui::State::new(),
            )?,
        }
    }
    Ok(())
}

/// Run one offscreen frame against `widget` and write RGBA to `out` as PNG.
/// Mirrors `ui::App`'s cycle: sync → `build_layout` → `compute_layout` → `apply_layout` → draw.
///
/// `state` carries the document: a fresh one for the seeded/demo scenarios, or
/// a **warmed** one (already synced through the import + decode warm-up) so the
/// imported media persists into the rendered frame instead of being wiped by a
/// fresh empty sync.
fn render_shot_to_png(
    mut widget: Box<dyn Widget>,
    viewport: (u32, u32),
    out: &std::path::Path,
    mut state: ui::State,
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
    println!("video_editor: wrote shot to {}", out.display());
    Ok(())
}
