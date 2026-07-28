//! Video Editor — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default        — run the live winit event loop via `ui::App`.
//!   * `--shots PATH` — render one offscreen frame to a PNG and exit
//!                       (headless screenshot; identical passes to the live app).

use std::collections::HashMap;

use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use taffy::prelude::*;
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
    if let Some(out) = parse_shots_arg(&args) {
        // Point the FUSE-media seam at the bundled sample-media/ dir so the
        // capture resolves the demo project's media to local (hydrated)
        // sources — exercising the hydration branch without a live mount.
        // Honour an existing override if the caller already set one.
        if std::env::var_os("QOS_VE_MEDIA_DIR").is_none() {
            let bundled = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("sample-media");
            if bundled.exists() {
                // Safety: set before any threads spawn for the one-shot render.
                unsafe { std::env::set_var("QOS_VE_MEDIA_DIR", bundled); }
            }
        }
        // Scenario selectors (deterministic single-frame captures):
        //   --narrow             collapsed-bin overlay layout
        //   --scenario filters   clip selected with colour filters + fades
        //   --scenario split     timeline just after a split at the playhead
        //   (none)               the full live editor
        let scenario = parse_scenario_arg(&args);
        if args.iter().any(|a| a == "--narrow") {
            render_shot_to_png(video_editor::app_root_narrow(), NARROW_SHOTS_VIEWPORT, &out)?;
        } else {
            match scenario.as_deref() {
                Some("filters") => render_shot_to_png(video_editor::app_root_filters(), DEFAULT_SHOTS_VIEWPORT, &out)?,
                Some("split") => render_shot_to_png(video_editor::app_root_split(), DEFAULT_SHOTS_VIEWPORT, &out)?,
                _ => render_shot_to_png(video_editor::app_root(), DEFAULT_SHOTS_VIEWPORT, &out)?,
            }
        }
        return Ok(());
    }

    // Headless drive (`--headless-drive '<json>'`): run a list of clicks and
    // keystrokes against the real widget tree with no display, persisting
    // through the same store the live app uses.
    if let Some(spec) = ui::headless::spec_from_args(&args) {
        std::process::exit(ui::headless::run(spec, video_editor::app_root(), "dev.qos.video-editor"));
    }

    let (state, autosave) = ui::persist::open("dev.qos.video-editor");
    let app = ui::App::new(state, video_editor::app_root()).with_autosave(autosave);
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

/// Run one offscreen frame against `widget` and write RGBA to `out` as PNG.
/// Mirrors `ui::App`'s cycle: sync → build_layout → compute_layout → apply_layout → draw.
fn render_shot_to_png(
    mut widget: Box<dyn Widget>,
    viewport: (u32, u32),
    out: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    let theme = Theme::dark();
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
    println!("video_editor: wrote shot to {}", out.display());
    Ok(())
}
