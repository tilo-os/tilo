//! Typst — thin binary entry point. All logic lives in the library.
//!
//! The live window runs on the app framework (`TypstEditorShell`). Capture
//! modes keep the standalone screenshot pipeline with demo documents.

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

use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};
use ui::theme::Theme;
use ui::widget::Widget;

const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);

#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    if standalone_capture() {
        ui::launch::run_or_exit(&standalone_launch_spec());
        return;
    }
    ui::launch::run_or_exit(&typst_editor::framework_launch_spec());
}

fn standalone_capture() -> bool {
    std::env::args().any(|arg| {
        [
            "--shots",
            "--responsive-matrix",
            "--native-audit",
            "--headless",
        ]
        .iter()
        .any(|mode| arg == *mode || arg.starts_with(&format!("{mode}=")))
    })
}

fn standalone_launch_spec() -> LaunchSpec {
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
fn render_shot_to_png(
    mut widget: Box<dyn Widget>,
    viewport: (u32, u32),
    theme: &Theme,
    out: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    ui::launch::render_widget_png(widget.as_mut(), viewport, theme.clone(), out)?;
    println!("typst_editor: wrote shot to {}", out.display());
    Ok(())
}

#[test]
fn production_launch_registers_history() {
    standalone_launch_spec()
        .validate_history_declaration()
        .unwrap();
}
