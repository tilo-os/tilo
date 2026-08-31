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
        Some("narrow" | "narrow_closed") => NARROW_VIEWPORT,
        _ => ui::launch::DEFAULT_VIEWPORT,
    }
}

pub fn main() {
    ui::launch::run_or_exit(
        &LaunchSpec::new("dev.qos.slides", slides::app_root)
            .shot_root(shot_root)
            .shot_viewport(shot_viewport)
            .scenarios(&[
                "overview",
                "present",
                "present_notes",
                "presenter",
                "present_white",
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
                "narrow",
                "narrow_closed",
                "inspector",
                "element_menu",
                "slide_menu",
                "reorder",
            ])
            .configure_app(|app| slides::actions::register(app.action_registry_mut())),
    );
}
