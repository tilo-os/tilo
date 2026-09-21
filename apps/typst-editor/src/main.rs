//! Typst — thin binary entry point. All logic lives in the library.
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
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
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

/// Entry point: dispatches to the offscreen `--shots` renderer, the headless
/// `--headless-drive` driver, or the live windowed event loop.
///
/// # Errors
/// Returns an error when the `--shots` path fails to lay out or render the
/// offscreen frame (a taffy layout error, or an I/O failure writing the PNG).
#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    ui::launch::run_or_exit(&launch_spec());
}

fn launch_spec() -> LaunchSpec {
    LaunchSpec::new("so.tilo.typst-editor", typst_editor::app_root)
        .history(&typst_editor::framework_history::HISTORY)
        .configure_app(|app| app.set_window_title(Some("Typst".into())))
        .shot_handler(shot_handler)
        .allowed_options(&["--theme"])
        .scenarios(&[
            "preview",
            "menu",
            "problems",
            "scrolled",
            "errors",
            "editor-select",
            "preview-zoom",
            "preview-pan",
            "select",
            "stale",
            "status",
        ])
}

fn shot_handler(args: &LaunchArgs) -> Result<(), LaunchError> {
    let out = args
        .shots
        .as_deref()
        .ok_or_else(|| LaunchError::Operation("missing screenshot output".into()))?;
    let root = args
        .scenario
        .as_deref()
        .map_or_else(typst_editor::demo_root, typst_editor::demo_root_scenario);
    let shared_theme = ui::launch::shot_theme(args);
    let theme = match args.value("--theme") {
        Some("light") => Theme::light()
            .with_text_scale(shared_theme.text_scale)
            .with_reduce_motion(shared_theme.reduce_motion),
        Some("dark") => Theme::dark()
            .with_text_scale(shared_theme.text_scale)
            .with_reduce_motion(shared_theme.reduce_motion),
        Some("high-contrast" | "high_contrast" | "hc") => Theme::high_contrast()
            .with_text_scale(shared_theme.text_scale)
            .with_reduce_motion(shared_theme.reduce_motion),
        None => shared_theme,
        Some(name) => {
            return Err(LaunchError::Scenario(format!(
                "unknown screenshot theme {name:?}; expected \"dark\", \"light\", or \"high-contrast\""
            )));
        }
    };
    let reduced_motion = ["TILO_A11Y_REDUCE_MOTION", "TILO_REDUCE_MOTION"]
        .into_iter()
        .filter_map(|key| std::env::var(key).ok())
        .any(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        });
    let theme = if let Some(scale) = args.text_scale {
        theme.with_text_scale(scale)
    } else {
        theme
    }
    .with_reduce_motion(reduced_motion);
    render_shot_to_png(
        root,
        args.viewport.unwrap_or(DEFAULT_SHOTS_VIEWPORT),
        &theme,
        out,
    )
    .map_err(|error| LaunchError::Operation(error.to_string()))
}

/// Run one offscreen frame against `widget` and write RGBA to `out` as PNG.
/// Mirrors `ui::App`'s cycle: sync → `build_layout` → `compute_layout` → `apply_layout` → draw.
fn render_shot_to_png(
    mut widget: Box<dyn Widget>,
    viewport: (u32, u32),
    theme: &Theme,
    out: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    text_renderer.set_text_scale(theme.text_scale);
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    let mut state = ui::State::new();
    widget.sync(&mut state.doc, &state.root);

    let child = widget.build_layout(&mut taffy, theme, &mut node_map);
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
        bindings.rebuild(widget.as_ref(), &taffy, child, &node_map);
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
        theme,
        &presence,
        &permission,
    );

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut renderer = OffscreenRenderer::new(w, h);
    renderer.render_png(&scene, Color::TRANSPARENT, out);
    println!("typst_editor: wrote shot to {}", out.display());
    Ok(())
}

#[test]
fn production_launch_registers_history() {
    launch_spec().validate_history_declaration().unwrap();
}
