//! Chat — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default        — the chat window, importing an older workspace file once.
//!   * `--shots PATH` — render one offscreen frame to a PNG and exit
//!     (headless screenshot; identical passes to the live app).
//!
//! Optional flags for `--shots` (used to capture interaction states the
//! single-frame renderer can't reach through live input):
//!   * `--scenario NAME` — stage a frame (`narrow`, `context`); see
//!     [`chat::app_root_for_shot`].
//!   * `--viewport WxH`  — render at a custom size (e.g. `--viewport 760x900`
//!     to exercise the narrow-window overlay flaps).

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.
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

use ui::launch::{LaunchArgs, LaunchError, LaunchSpec, ShotFrame};
use ui::theme::Theme;

const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);

fn shot_handler(args: &LaunchArgs) -> Result<(), LaunchError> {
    let out = args
        .shots
        .as_deref()
        .ok_or_else(|| LaunchError::Operation("missing screenshot output".into()))?;
    let mut app = chat::ChatApp::new_ephemeral();
    let scenario = args.scenario.as_deref().unwrap_or_default();
    app.seed_shot_scenario(scenario);
    render_shot_to_png(
        app,
        args.viewport.unwrap_or(DEFAULT_SHOTS_VIEWPORT),
        ui::launch::shot_theme(args),
        out,
        scenario,
    )
    .map_err(|error| LaunchError::Operation(error.to_string()))
}

#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    // This spec imports an older workspace file once. The ResourceApp shell
    // opens an empty record and would leave that history behind.
    ui::launch::run_or_exit(
        &LaunchSpec::new("so.tilo.chat", chat::app_root)
            .shot_handler(shot_handler)
            .scenarios(&[
                "narrow",
                "sidebar-narrow",
                "history",
                "compact",
                "starter",
                "people",
                "thread-narrow",
                "collaboration",
                "context",
                "msgmenu",
                "inbox",
                "emoji",
                "edit-history",
                "no-access",
                "attach",
                "attach-loading",
                "attach-error",
                "edit",
                "composer",
                "call",
                "share",
                "floating-call-attached",
                "floating-call",
                "floating-call-hover",
                "floating-call-returning",
                "floating-call-returned",
            ])
            .configure_app(|app| chat::actions::register(app.action_registry_mut())),
    );
}

/// Run one offscreen frame against `app` and write RGBA to `out` as PNG,
/// pre-warming the attachment image cache so the single frame paints real
/// decoded thumbnails rather than a loading placeholder.
fn render_shot_to_png(
    mut app: chat::ChatApp,
    viewport: (u32, u32),
    theme: Theme,
    out: &std::path::Path,
    scenario: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut frame = ShotFrame::new(viewport, theme);
    if let Some(permission) = chat::ChatApp::shot_permission(scenario) {
        frame.permission = permission;
    }
    frame.sync(&mut app);
    app.prewarm_images();
    frame.layout(&mut app)?;
    app.stage_after_layout_for_shot(scenario);
    // A menu opens where the pointer is on its row, which is only known
    // once a frame has placed the rows.
    if app.stages_after_draw(scenario) {
        let _ = frame.draw(&mut app);
        app.stage_after_draw_for_shot(scenario);
    }
    let mut scene = frame.draw(&mut app);
    frame.draw_floating_surfaces(&mut app, &mut scene);
    // The offscreen renderer publishes the PNG atomically, so a killed
    // headless renderer never leaves a truncated screenshot at `out` for
    // visual review tools to mistake for an inspected frame.
    frame.write_png(&scene, out);
    println!("chat: wrote shot to {}", out.display());
    Ok(())
}
