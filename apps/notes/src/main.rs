#![allow(
    clippy::doc_markdown,
    clippy::useless_conversion,
    reason = "migration stack lint debt is scoped to this target"
)]
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

use notes::shots::DEFAULT_SHOTS_VIEWPORT;
use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};

ui::android_main!();

const SHOT_SCENARIOS: &[&str] = &[
    "first_note",
    "drawer",
    "drag",
    "block_drag",
    "block_type_menu",
    "menu",
    "sidebar_hover",
    "note_menu",
    "title_context_menu",
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
    let args: Vec<String> = std::env::args().collect();
    // Captures, native audit, headless drives and a positional open keep the
    // component-host launcher; the default live window runs on the framework.
    let standalone = args.iter().any(|arg| {
        [
            "--shots",
            "--responsive-matrix",
            "--native-audit",
            "--headless",
        ]
        .iter()
        .any(|mode| arg == *mode || arg.starts_with(&format!("{mode}=")))
    }) || args.iter().skip(1).any(|arg| !arg.starts_with('-'));
    if standalone {
        ui::launch::run(
            &LaunchSpec::from_definition(notes::definition())
                .max_positionals(1)
                .component_host(prepare_component)
                .native_services()
                .capture_scenario(capture_scenario)
                .shot_viewport(|_| DEFAULT_SHOTS_VIEWPORT)
                .scenarios(SHOT_SCENARIOS),
        )?;
    } else {
        ui::framework::run::<notes::NotesShell>();
    }
    Ok(())
}

fn prepare_component(
    _definition: &ui::application::ApplicationDefinition,
    args: &LaunchArgs,
) -> Result<ui::application::PreparedComponentInputs, LaunchError> {
    if args.shots.is_some() || args.responsive_matrix.is_some() {
        let theme = ui::launch::shot_theme(args);
        let mut text_renderer = ui::TextRenderer::new();
        text_renderer.set_text_scale(theme.text_scale);
        let inputs = ui::application::ComponentRunnerInputs {
            theme,
            text_renderer,
        };
        return notes::capture::prepare_fixture(args.scenario.as_deref(), inputs);
    }
    let inputs = ui::application::ComponentRunnerInputs::from_native_process();
    if args.native_audit {
        notes::capture::prepare_fixture(Some("first_note"), inputs)
    } else {
        notes::hosting::set_native_launch_target(args.positionals().first().cloned());
        notes::hosting::prepare_native(inputs)
    }
}

fn capture_scenario(
    driver: &mut ui::headless::FrameHarness,
    args: &LaunchArgs,
) -> Result<(), LaunchError> {
    notes::capture::drive_fixture(driver, args.scenario.as_deref())
}
