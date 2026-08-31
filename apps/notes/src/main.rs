//! Thin binary entry point. All logic lives in the library.
//!
//! Collaboration: launched from the shell, the compositor's broker is
//! reached through the `QOS_SVC_COLLAB_FD` descriptor the launcher injects —
//! nothing to configure. A raw process outside a session has no collaboration
//! service; the descriptor is inherited rather than discovered by path.

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

use notes::NotesApp;
use notes::shots::{self, DEFAULT_SHOTS_VIEWPORT};
use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};

const SHOT_SCENARIOS: &[&str] = &[
    "drawer",
    "drag",
    "menu",
    "rename",
    "rename_commit",
    "slash",
    "slash_applied",
    "heading",
    "title_edit",
    "recipes",
    "formula",
    "status",
    "presence",
];

/// # Errors
///
/// Returns an error if `--shots` mode fails to render, or if the app fails
/// to spawn against its workspace.
pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    ui::launch::run(
        &LaunchSpec::new_with_args("dev.qos.notes", live_root)
            .open_state_with_args(open_state)
            .shot_handler(render_shot)
            .scenarios(SHOT_SCENARIOS)
            .configure_app(configure_app),
    )?;
    Ok(())
}

fn peer_id() -> &'static str {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        std::env::var("QUICK_APP_ID").unwrap_or_else(|_| uuid::Uuid::new_v4().to_string())
    })
}

fn live_root(_args: &LaunchArgs) -> Result<Box<dyn ui::widget::Widget>, LaunchError> {
    let mut app = NotesApp::spawn_default().map_err(LaunchError::Operation)?;
    app.set_local_author(Some(peer_id().to_owned()));
    Ok(Box::new(app))
}

fn open_state(_app_id: &'static str, _args: &LaunchArgs) -> (ui::State, ui::persist::Autosave) {
    let mut state = ui::State::new();
    state
        .doc
        .set_actor(automerge::ActorId::from(peer_id().as_bytes()));
    (
        state,
        ui::persist::Autosave::disabled("Notes owns its workspace persistence"),
    )
}

fn configure_app(app: &mut ui::App) {
    notes::actions::register(app.action_registry_mut());
}

fn render_shot(args: &LaunchArgs) -> Result<(), LaunchError> {
    let out = args.shots.as_deref().ok_or_else(|| {
        LaunchError::Operation("Notes screenshot handler called without --shots".into())
    })?;
    shots::render_to_path(
        args.viewport.unwrap_or(DEFAULT_SHOTS_VIEWPORT),
        out,
        args.scenario.as_deref(),
    )
    .map_err(|error| LaunchError::Operation(error.to_string()))
}
