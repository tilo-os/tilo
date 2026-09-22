#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

//! Thin binary entry point. All logic lives in the library.
//!
//! Collaboration: launched from the shell, the compositor's broker is
//! reached through the `TILO_SVC_COLLAB_FD` descriptor the launcher injects —
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

#[cfg(feature = "thin")]
extern crate runtime_dylib;

use notes::shots::{self, DEFAULT_SHOTS_VIEWPORT};
use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};

const SHOT_SCENARIOS: &[&str] = &[
    "first_note",
    "drawer",
    "drag",
    "block_drag",
    "block_type_menu",
    "menu",
    "sidebar_hover",
    "note_menu",
    "rename",
    "rename_commit",
    "slash",
    "slash_applied",
    "heading",
    "fenced_code",
    "fenced_code_opening",
    "code_keyboard",
    "code_keyboard_active",
    "typed_lists",
    "multiline_typing",
    "mixed_outline",
    "rich_outline",
    "paragraph_click",
    "code_group_drag",
    "code_group_drop",
    "adjacent_code_fences",
    "quote",
    "title_edit",
    "recipes",
    "formula",
    "transclusion",
    "external_link",
    "table",
    "table_large",
    "table_large_scrolled",
    "table_cell_edit",
    "image",
    "image_edit",
    "status",
    "saving",
    "save_error",
    "import_success",
    "presence",
    "shortcuts",
    "shortcuts_bottom",
    "find",
    "scrolled",
    "sidebar_scroll",
];

/// # Errors
///
/// Returns an error if `--shots` mode fails to render, or if the app fails
/// to spawn against its workspace.
pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    ui::launch::run(
        &LaunchSpec::new_with_args("dev.tilo.notes", live_root)
            .allowed_live_flags(&["--import-legacy-stopped"])
            .allowed_live_options(&["--notebook-dir", "--notebook-state-dir"])
            .max_positionals(1)
            .component_host(prepare_component)
            .native_services()
            .capture_scenario(capture_scenario)
            .shot_viewport(|_| DEFAULT_SHOTS_VIEWPORT)
            .scenarios(SHOT_SCENARIOS),
    )?;
    Ok(())
}

fn peer_id() -> &'static str {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        std::env::var("QUICK_APP_ID").unwrap_or_else(|_| uuid::Uuid::new_v4().to_string())
    })
}

fn live_root(args: &LaunchArgs) -> Result<Box<dyn ui::widget::Widget>, LaunchError> {
    let root = match args.value("--notebook-dir") {
        Some(root) => std::path::PathBuf::from(root),
        None => notes::storage::default_root()
            .map_err(LaunchError::Operation)?
            .ok_or_else(|| LaunchError::Operation("Notes notebook directory unavailable".into()))?,
    };
    let state_root = args
        .value("--notebook-state-dir")
        .map_or_else(|| root.join(".qos-records"), std::path::PathBuf::from);
    let notebook = notes::production::prepare_notebook_with_source(
        &root,
        &state_root,
        &ui::collaboration::local_cluster_id().to_string(),
        args.has("--import-legacy-stopped"),
        args.positionals().first().map(String::as_str),
    )
    .map_err(LaunchError::Operation)?;
    Ok(Box::new(notebook.root))
}

fn prepare_launch(args: &LaunchArgs) -> Result<ui::launch::LegacyLaunchInputs, LaunchError> {
    let mut state = ui::State::new();
    state
        .doc
        .set_actor(automerge::ActorId::from(peer_id().as_bytes()));
    let root = live_root(args)?;
    let autosave = ui::persist::Autosave::disabled("Notes owns its canonical notebook persistence");
    let mut registry = tilo_actions::ActionRegistry::new();
    notes::actions::register(&mut registry);
    Ok(ui::launch::LegacyLaunchInputs::new(state, root, autosave)
        .with_registry(registry)
        .with_widget_owned_persistence())
}

fn render_shot(args: &LaunchArgs) -> Result<(), LaunchError> {
    let out = args.shots.as_deref().ok_or_else(|| {
        LaunchError::Operation("Notes screenshot handler called without --shots".into())
    })?;
    shots::render_to_path(
        args.viewport.unwrap_or(DEFAULT_SHOTS_VIEWPORT),
        out,
        args.scenario.as_deref(),
        ui::launch::shot_theme(args),
    )
    .map_err(|error| LaunchError::Operation(error.to_string()))
}
