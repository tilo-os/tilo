//! Sheets — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default        — run the live winit event loop via `ui::App`.
//!   * `--shots PATH` — render one offscreen frame to a PNG and exit
//!                       (headless screenshot; identical passes to the live app).
//!
//! Shots flags:
//!   * `--viewport WxH`     — override the desktop viewport (e.g. the narrow
//!                            560x720 responsive layout).
//!   * `--scenario NAME`    — snap a specific surface into view for the frame:
//!                            `file`, `format`, `docsettings`, `editing`,
//!                            `pointmode`, `summary`, `multiselect`, `tabmenu`,
//!                            `commandpalette`.
//!
//! The shots path always seeds the sample workbook and disables on-disk
//! persistence, so a screenshot shows real formulas without ever touching the
//! user's stored data.

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

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    if let Some(out) = parse_shots_arg(&args) {
        let viewport = parse_viewport_arg(&args).unwrap_or(DEFAULT_SHOTS_VIEWPORT);
        let scenario = parse_scenario_arg(&args);

        let mut app = sheets::SheetsApp::new();
        app.shots_mode();
        // The scenario hook runs after the document is seeded (in
        // `render_shot_to_png`, post-`sync`), because opening a menu / starting
        // an edit operates on the live state.
        let setup: Box<dyn FnOnce(&mut sheets::SheetsApp)> = match scenario.as_deref() {
            Some("file") => Box::new(|a: &mut sheets::SheetsApp| a.shots_open_file_menu()),
            Some("format") => Box::new(|a: &mut sheets::SheetsApp| a.shots_open_format_menu()),
            Some("docsettings") => Box::new(|a: &mut sheets::SheetsApp| a.shots_open_doc_settings()),
            Some("editing") => Box::new(|a: &mut sheets::SheetsApp| a.shots_begin_edit()),
            Some("pointmode") => Box::new(|a: &mut sheets::SheetsApp| a.shots_build_formula()),
            Some("summary") => Box::new(|a: &mut sheets::SheetsApp| a.shots_select_summary_range()),
            Some("multiselect") => Box::new(|a: &mut sheets::SheetsApp| a.shots_multi_select()),
            Some("tabmenu") => Box::new(|a: &mut sheets::SheetsApp| a.shots_open_tab_menu()),
            Some("fbarselect") => Box::new(|a: &mut sheets::SheetsApp| a.shots_formula_bar_select()),
            Some("commandpalette") => {
                Box::new(|a: &mut sheets::SheetsApp| a.shots_open_command_palette())
            }
            _ => Box::new(|_: &mut sheets::SheetsApp| {}),
        };
        render_shot_to_png(app, viewport, &out, setup)?;
        return Ok(());
    }

    // Headless drive (`--headless-drive '<json>'`): run a list of clicks and
    // keystrokes against the real widget tree with no display, persisting
    // through the same store the live app uses.
    if let Some(spec) = ui::headless::spec_from_args(&args) {
        std::process::exit(ui::headless::run(spec, sheets::app_root(), "dev.qos.sheets"));
    }

    let (state, autosave) = ui::persist::open("dev.qos.sheets");
    let mut app = ui::App::new(state, sheets::app_root()).with_autosave(autosave);
    // Register Sheets' actions so the shell's Ctrl+K palette can decode + replay
    // the same workbook mutations the on-screen affordances dispatch.
    sheets::actions::register(app.action_registry_mut());
    app.run();
    Ok(())
}

/// Parse `--viewport WxH` (e.g. `--viewport 560x720`) for `--shots`.
fn parse_viewport_arg(args: &[String]) -> Option<(u32, u32)> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        let spec = if arg == "--viewport" {
            iter.next().map(String::as_str)
        } else {
            arg.strip_prefix("--viewport=")
        };
        if let Some(spec) = spec {
            let (w, h) = spec.split_once(['x', 'X'])?;
            return Some((w.trim().parse().ok()?, h.trim().parse().ok()?));
        }
    }
    None
}

/// Parse `--scenario NAME` for `--shots`, selecting which surface to snap into
/// view for the captured frame.
fn parse_scenario_arg(args: &[String]) -> Option<String> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--scenario" {
            return iter.next().cloned();
        }
        if let Some(rest) = arg.strip_prefix("--scenario=") {
            return Some(rest.to_string());
        }
    }
    None
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

/// Run one offscreen frame against `app` and write RGBA to `out` as PNG.
/// Mirrors `ui::App`'s cycle: sync → build_layout → compute_layout → apply_layout → draw.
fn render_shot_to_png(
    mut app: sheets::SheetsApp,
    viewport: (u32, u32),
    out: &std::path::Path,
    setup: Box<dyn FnOnce(&mut sheets::SheetsApp)>,
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
    app.sync(&mut state.doc, &state.root);
    // Apply the scenario hook now that the document is seeded.
    setup(&mut app);
    let widget: &mut dyn Widget = &mut app;

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
    println!("sheets: wrote shot to {}", out.display());
    Ok(())
}
