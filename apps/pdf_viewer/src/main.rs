//! PDF — thin binary entry point. All logic lives in the library.
//!
//! Usage:
//!   * `pdf_viewer [FILE.pdf]`     — run the live winit event loop; opens FILE
//!                                    if given, otherwise the empty state.
//!   * `--shots PATH`              — render one offscreen frame to a PNG and
//!                                    exit (headless screenshot).
//!   * `--viewport WxH`           — override the shot viewport (default 1280x800).
//!   * `--sample`                 — use the bundled demonstration document
//!                                    (shots/testing; seeds example markup).

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

struct Args {
    shots: Option<std::path::PathBuf>,
    viewport: (u32, u32),
    sample: bool,
    /// Screenshot-only scenario selector (e.g. `edit-comment`).
    scenario: Option<String>,
    file: Option<std::path::PathBuf>,
}

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args = parse_args(&std::env::args().collect::<Vec<_>>());

    if let Some(out) = &args.shots {
        render_shot_to_png(build_widget(&args), args.viewport, out)?;
        return Ok(());
    }

    let state = ui::State::new();
    let app = ui::App::new(state, build_widget(&args));
    app.run();
    Ok(())
}

/// Pick the widget for the current invocation: an explicit file wins, then the
/// `--sample` demonstration document, otherwise the empty state.
fn build_widget(args: &Args) -> Box<dyn Widget> {
    // Screenshot scenarios take precedence so they can also target a real file.
    match args.scenario.as_deref() {
        Some("edit-comment") => return pdf_viewer::app_root_sample_editing(),
        Some("selection-hud") => return pdf_viewer::app_root_selection_hud(),
        Some("reorder") => return pdf_viewer::app_root_reorder_demo(args.file.as_deref()),
        Some("import") => return pdf_viewer::app_root_import_demo(args.file.as_deref()),
        _ => {}
    }
    if let Some(file) = &args.file {
        pdf_viewer::app_root_open(file)
    } else if args.sample {
        pdf_viewer::app_root_sample()
    } else {
        pdf_viewer::app_root()
    }
}

fn parse_args(args: &[String]) -> Args {
    let mut out = Args {
        shots: None,
        viewport: DEFAULT_SHOTS_VIEWPORT,
        sample: false,
        scenario: None,
        file: None,
    };
    let mut i = 1;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--shots" {
            out.shots = args.get(i + 1).map(std::path::PathBuf::from);
            i += 2;
        } else if let Some(rest) = arg.strip_prefix("--shots=") {
            out.shots = Some(std::path::PathBuf::from(rest));
            i += 1;
        } else if arg == "--viewport" {
            if let Some(v) = args.get(i + 1).and_then(|s| parse_viewport(s)) {
                out.viewport = v;
            }
            i += 2;
        } else if let Some(rest) = arg.strip_prefix("--viewport=") {
            if let Some(v) = parse_viewport(rest) {
                out.viewport = v;
            }
            i += 1;
        } else if arg == "--sample" {
            out.sample = true;
            i += 1;
        } else if arg == "--scenario" {
            out.scenario = args.get(i + 1).cloned();
            i += 2;
        } else if let Some(rest) = arg.strip_prefix("--scenario=") {
            out.scenario = Some(rest.to_string());
            i += 1;
        } else if !arg.starts_with("--") && out.file.is_none() {
            out.file = Some(std::path::PathBuf::from(arg));
            i += 1;
        } else {
            i += 1;
        }
    }
    out
}

fn parse_viewport(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once(['x', 'X', ','])?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
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

    // Draw until the per-frame raster budget's backlog drains so the captured
    // still is the settled image. The live app pumps these follow-up frames via
    // `needs_animation_frame`; a one-shot capture drains them here, otherwise a
    // deferred page / rail thumbnail would be missing from the PNG.
    let mut scene = Scene::new();
    for _ in 0..32 {
        scene = Scene::new();
        widget.draw_with_presence(
            &mut scene,
            Affine::IDENTITY,
            &mut text_renderer,
            &theme,
            &presence,
            &permission,
        );
        if !widget.needs_animation_frame() {
            break;
        }
    }

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut renderer = OffscreenRenderer::new(w, h);
    renderer.render_png(&scene, Color::TRANSPARENT, out);
    println!("pdf_viewer: wrote shot to {}", out.display());
    Ok(())
}
