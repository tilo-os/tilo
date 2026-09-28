#![allow(
    clippy::many_single_char_names,
    clippy::redundant_clone,
    clippy::similar_names,
    clippy::single_match_else,
    clippy::too_many_lines,
    reason = "migration stack lint debt is scoped to this target"
)]
#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

//! Slides — standard Tilo application entry point.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic"
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
use ui::widget::Widget;

const NARROW_VIEWPORT: (u32, u32) = (820, 800);

// The signature is fixed by `ui::launch::ShotFactory`: other apps' shot roots
// may fail, so the alias is fallible even where this one isn't.
#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is dictated by ui::launch::ShotFactory, which other apps' fallible shot roots need"
)]
fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    let legacy = std::env::var("SLIDES_SHOT").ok();
    Ok(slides::app_root_demo(
        args.scenario.as_deref().or(legacy.as_deref()),
    ))
}

fn shot_viewport(args: &LaunchArgs) -> (u32, u32) {
    let legacy = std::env::var("SLIDES_SHOT").ok();
    match args.scenario.as_deref().or(legacy.as_deref()) {
        Some(
            "narrow" | "narrow_closed" | "inspector" | "many_rail" | "toolbar_menu"
            | "inspector_widgets",
        ) => NARROW_VIEWPORT,
        _ => ui::launch::DEFAULT_VIEWPORT,
    }
}

/// Render the same scenario under the desktop's accessible palette variants.
/// The live app receives its theme from the compositor; `--shots` has no
/// compositor, so this is the explicit seam the visual/a11y harness uses.
fn render_shot(args: &LaunchArgs) -> Result<(), LaunchError> {
    let out = args.shots.as_deref().ok_or(LaunchError::ShotsUnsupported)?;
    let viewport = args.viewport.unwrap_or_else(|| shot_viewport(args));
    let theme = ui::launch::shot_theme(args);
    ui::headless::FrameHarness::new_with_theme(ui::State::new(), shot_root(args)?, viewport, theme)
        .render_png(out);
    println!("so.tilo.slides: wrote shot to {}", out.display());
    Ok(())
}

#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    // The live deck runs on the app framework. Shots and explicit headless
    // flags keep the standalone path.
    let mut args = std::env::args().skip(1);
    let mut standalone = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => {
                args.next();
            }
            "--shots" | "--responsive-matrix" | "--native-audit" | "--headless" => {
                standalone = true;
            }
            file if !file.starts_with('-') => standalone = true,
            _ => {}
        }
    }
    if !standalone {
        ui::framework::run::<slides::SlidesShell>();
        return;
    }
    ui::launch::run_or_exit(
        &LaunchSpec::new("so.tilo.slides", slides::app_root)
            .history(&slides::framework_history::HISTORY)
            .shot_handler(render_shot)
            .scenarios(&[
                "overview",
                "present",
                "present_notes",
                "presenter",
                "present_white",
                "empty",
                "help",
                "rename",
                "notes_edit",
                "slide_props",
                "toolbar_menu",
                "many",
                "many_rail",
                "overview_end",
                "picker",
                "image",
                "image_loading",
                "open_error",
                "narrow",
                "narrow_closed",
                "inspector",
                "element_menu",
                "slide_menu",
                "reorder",
                "edit_text",
                "delete_zone",
                "inspector_widgets",
            ])
            .configure_app(|app| slides::actions::register(app.action_registry_mut())),
    );
}
