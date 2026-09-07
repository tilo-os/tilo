//! Slides — standard qOS application entry point.

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
// genuinely can fail, so the alias is fallible even where this one isn't.
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
    let theme = match std::env::var("QOS_SHOTS_THEME").ok().as_deref() {
        Some("light") => ui::Theme::light(),
        Some("high-contrast") => ui::Theme::high_contrast(),
        _ => ui::Theme::dark(),
    }
    .with_reduce_motion(matches!(
        std::env::var("QOS_SHOTS_REDUCE_MOTION").ok().as_deref(),
        Some("1" | "true" | "yes")
    ));
    ui::headless::FrameHarness::new_with_theme(ui::State::new(), shot_root(args)?, viewport, theme)
        .render_png(out);
    println!("dev.qos.slides: wrote shot to {}", out.display());
    Ok(())
}

#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    ui::launch::run_or_exit(
        &LaunchSpec::new("dev.qos.slides", slides::app_root)
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
