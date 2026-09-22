#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

//! Sheets — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default        — run the live winit event loop via `ui::App`.
//!   * `--shots PATH` — render one offscreen frame to a PNG and exit
//!     (headless screenshot; identical passes to the live app).
//!
//! Shots flags:
//!   * `--viewport WxH`     — override the desktop viewport (e.g. the narrow
//!     560x720 responsive layout).
//!   * `--scenario NAME`    — snap a specific surface into view for the frame:
//!     `file`, `format`, `more`, `docsettings`, `editing`,
//!     `pointmode`, `summary`, `multiselect`, `tabmenu`, `commandpalette`,
//!     `loading`, `error`, `conflict`, `reducedmotion`.
//!
//! The shots path always seeds the sample workbook and disables on-disk
//! persistence, so a screenshot shows formulas without touching the
//! user's stored data.

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
const APP_ID: &str = "so.tilo.sheets";
const SCENARIOS: &[&str] = &[
    "file",
    "format",
    "more",
    "docsettings",
    "editing",
    "pointmode",
    "summary",
    "multiselect",
    "tabmenu",
    "fbarselect",
    "commandpalette",
    "fillseries",
    "scrollbars",
    "pasteformula",
    "loading",
    "error",
    "conflict",
    "reducedmotion",
    "charts",
    "comments",
];

fn configure(app: &mut ui::App) {
    sheets::actions::register(app.action_registry_mut());
}

#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    ui::launch::run_or_exit(
        &LaunchSpec::new(APP_ID, sheets::app_root)
            .shot_handler(render_shot)
            .scenarios(SCENARIOS)
            .configure_app(configure),
    );
}

/// Render one `--shots` scenario to a PNG.
///
/// # Errors
///
/// Returns an error if `--shots` was not given a path, or if
/// `render_shot_to_png` fails to build the layout or write the file.
fn render_shot(args: &LaunchArgs) -> Result<(), LaunchError> {
    type Setup = Box<dyn FnOnce(&mut sheets::SheetsApp, &mut automerge::AutoCommit)>;

    let out = args.shots.as_deref().ok_or(LaunchError::ShotsUnsupported)?;
    let viewport = args.viewport.unwrap_or(DEFAULT_SHOTS_VIEWPORT);
    let scenario = args.scenario.as_deref();

    let mut app = sheets::SheetsApp::new();
    app.shots_mode();
    // The scenario hook runs after the document is seeded (in
    // `render_shot_to_png`, post-`sync`), because opening a menu, starting an
    // edit, or driving a fill/paste operates on the live state.
    let setup: Setup = match scenario {
        Some("file") => Box::new(|a, _d| a.shots_open_file_menu()),
        Some("format") => Box::new(|a, _d| a.shots_open_format_menu()),
        Some("more") => Box::new(|a, _d| a.shots_open_more_menu()),
        Some("docsettings") => Box::new(|a, _d| a.shots_open_doc_settings()),
        Some("editing") => Box::new(|a, _d| a.shots_begin_edit()),
        Some("pointmode") => Box::new(|a, _d| a.shots_build_formula()),
        Some("summary") => Box::new(|a, _d| a.shots_select_summary_range()),
        Some("multiselect") => Box::new(|a, _d| a.shots_multi_select()),
        Some("tabmenu") => Box::new(|a, _d| a.shots_open_tab_menu()),
        Some("fbarselect") => Box::new(|a, _d| a.shots_formula_bar_select()),
        Some("commandpalette") => Box::new(|a, d| a.shots_open_command_palette(d)),
        Some("scrollbars") => Box::new(|a, _d| a.shots_scroll_both_axes()),
        Some("fillseries") => Box::new(sheets::SheetsApp::shots_fill_series),
        Some("pasteformula") => Box::new(sheets::SheetsApp::shots_paste_formula),
        Some("loading") => Box::new(|a, _d| a.shots_show_loading()),
        Some("error") => Box::new(|a, _d| a.shots_show_error()),
        Some("conflict") => Box::new(|a, _d| a.shots_show_conflict()),
        Some("reducedmotion") => Box::new(|a, _d| a.shots_show_reduced_motion()),
        Some("charts" | "comments") => Box::new(sheets::SheetsApp::shots_seed_charts_and_comments),
        _ => Box::new(|_a, _d| {}),
    };
    render_shot_to_png(app, viewport, ui::launch::shot_theme(args), out, setup)
        .map_err(|error| LaunchError::Operation(format!("Sheets screenshot failed: {error}")))
}

/// One-shot hook run after `sync` seeds the demo document, before the first
/// layout/draw pass — used to stage a `--shots` scenario's extra state.
type ShotSetup = Box<dyn FnOnce(&mut sheets::SheetsApp, &mut automerge::AutoCommit)>;

/// Run one offscreen frame against `app` and write RGBA to `out` as PNG.
/// Mirrors `ui::App`'s cycle: sync → `build_layout` → `compute_layout` → `apply_layout` → draw.
///
/// # Errors
///
/// Returns an error if Taffy fails to build or compute the layout tree.
fn render_shot_to_png(
    mut app: sheets::SheetsApp,
    viewport: (u32, u32),
    theme: Theme,
    out: &std::path::Path,
    setup: ShotSetup,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    let presence = PresenceCtx::empty();
    let permission_ctx = PermissionContext::permissive();
    // `Theme::with_text_scale` has already expanded every token size.  The
    // custom Sheets canvas passes those token sizes directly to the renderer,
    // so applying the multiplier a second time here would turn a requested
    // 2× accessibility size into 4× text and make the interactive chrome
    // unusable.  (Toolkit widgets hand the renderer their unscaled base
    // sizes, which is why their harness sets this separately.)
    let mut text_renderer = TextRenderer::new();
    text_renderer.set_text_scale(theme.text_scale);
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    let mut state = ui::State::new();
    app.sync(&mut state.doc, &state.root);
    // Apply the scenario hook now that the document is seeded; fill/paste
    // scenarios dispatch actions against the document.
    setup(&mut app, &mut state.doc);
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

    let mut bindings = ui::bound_dispatch::BoundNodeMap::new();
    // SAFETY: the bound nodes come from this exact exclusively owned build.
    unsafe {
        bindings.rebuild(widget, &taffy, child, &node_map);
        ui::widget::compute_widget_layout(
            &mut taffy,
            root,
            taffy::Size {
                width: AvailableSpace::Definite(w as f32),
                height: AvailableSpace::Definite(h as f32),
            },
            &node_map,
            &bindings,
            &mut text_renderer,
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
        &permission_ctx,
    );

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut renderer = OffscreenRenderer::new(w, h);
    renderer.render_png(&scene, Color::TRANSPARENT, out);
    println!("sheets: wrote shot to {}", out.display());
    Ok(())
}
