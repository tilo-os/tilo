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
use std::ffi::OsString;
use std::path::Path;
use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LaunchRoute {
    Normal,
    Compatibility,
}

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
    match launch_route_from(std::env::args_os().skip(1))? {
        LaunchRoute::Normal => ui::launch::run(&normal_spec())?,
        LaunchRoute::Compatibility => ui::launch::run(&compatibility_spec())?,
    }
    Ok(())
}

fn normal_spec() -> LaunchSpec {
    LaunchSpec::from_definition(notes::definition())
        .max_positionals(1)
        .component_host(prepare_component)
        .native_services()
        .capture_scenario(capture_scenario)
        .shot_viewport(|_| DEFAULT_SHOTS_VIEWPORT)
        .scenarios(SHOT_SCENARIOS)
}

fn compatibility_spec() -> LaunchSpec {
    LaunchSpec::new_with_args(notes::definition().app_id, compatibility_live_root)
        .allowed_live_flags(&["--import-legacy-stopped"])
        .allowed_live_options(&["--notebook-dir", "--notebook-state-dir"])
        .max_positionals(1)
        .prepare_legacy_with_args(prepare_compatibility_launch)
}

fn launch_route_from(args: impl IntoIterator<Item = OsString>) -> Result<LaunchRoute, LaunchError> {
    let mut compatibility = false;
    let mut protected_mode = false;
    let mut args = args.into_iter();
    while let Some(argument) = args.next() {
        let Some(argument) = argument.to_str() else {
            continue;
        };
        if argument == "--" {
            break;
        }

        let (name, inline_value) = argument
            .split_once('=')
            .map_or((argument, false), |(name, _)| (name, true));
        match name {
            "--notebook-dir" | "--notebook-state-dir" => {
                compatibility = true;
                if !inline_value {
                    let _ = args.next();
                }
            }
            "--import-legacy-stopped" if !inline_value => compatibility = true,
            "--shots" | "--responsive-matrix" | "--headless-drive" => {
                protected_mode = true;
                if !inline_value {
                    let _ = args.next();
                }
            }
            "--native-audit" if !inline_value => protected_mode = true,
            // Consume common value options so a value that begins with `--`
            // is never reclassified as a mode or compatibility selector.
            "--viewport" | "--scenario" | "--text-scale" if !inline_value => {
                let _ = args.next();
            }
            _ => {}
        }
    }
    if compatibility && protected_mode {
        return Err(LaunchError::Operation(
            "legacy notebook recovery cannot run in capture, audit, or headless mode".into(),
        ));
    }
    Ok(if compatibility {
        LaunchRoute::Compatibility
    } else {
        LaunchRoute::Normal
    })
}

fn compatibility_live_root(args: &LaunchArgs) -> Result<Box<dyn ui::widget::Widget>, LaunchError> {
    refuse_workspace_compatibility()?;
    let root = args
        .value("--notebook-dir")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| {
            LaunchError::Operation(
                "legacy notebook recovery requires an explicit --notebook-dir".into(),
            )
        })?;
    let stopped_legacy = args.has("--import-legacy-stopped");
    validate_compatibility_source(&root, stopped_legacy)?;
    let state_root = args
        .value("--notebook-state-dir")
        .map_or_else(|| root.join(".qos-records"), std::path::PathBuf::from);
    let notebook = notes::production::prepare_notebook_with_source(
        &root,
        &state_root,
        &ui::collaboration::local_cluster_id().to_string(),
        stopped_legacy,
        args.positionals().first().map(String::as_str),
    )
    .map_err(LaunchError::Operation)?;
    Ok(Box::new(notebook.root))
}

fn refuse_workspace_compatibility() -> Result<(), LaunchError> {
    let workspace_scoped = ["TILO_WORKSPACE_ID", "QUICK_CLUSTER_ID"]
        .iter()
        .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()));
    refuse_workspace_compatibility_from(workspace_scoped)
}

fn refuse_workspace_compatibility_from(workspace_scoped: bool) -> Result<(), LaunchError> {
    if workspace_scoped {
        return Err(LaunchError::Operation(
            "legacy notebook recovery is available only for a standalone personal launch".into(),
        ));
    }
    Ok(())
}

fn validate_compatibility_source(root: &Path, stopped_legacy: bool) -> Result<(), LaunchError> {
    let exists = |path: &Path| match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(LaunchError::Operation(format!(
            "{}: {error}",
            path.display()
        ))),
    };
    if exists(&root.join(".notes-canonical-v1"))? {
        return Ok(());
    }
    let source = notes::storage::workspace_file_path(root);
    if !exists(&source)? {
        return Err(LaunchError::Operation(
            "no retired canonical or legacy notebook exists at --notebook-dir; refusing to create an alternate empty notebook".into(),
        ));
    }
    if !stopped_legacy {
        return Err(LaunchError::Operation(
            "legacy Markdown storage requires --import-legacy-stopped after every older Notes process and external editor is closed".into(),
        ));
    }
    Ok(())
}

fn compatibility_peer_id() -> &'static str {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        std::env::var("QUICK_APP_ID").unwrap_or_else(|_| uuid::Uuid::new_v4().to_string())
    })
}

fn prepare_compatibility_launch(
    args: &LaunchArgs,
) -> Result<ui::launch::LegacyLaunchInputs, LaunchError> {
    let mut state = ui::State::new();
    state
        .doc
        .set_actor(automerge::ActorId::from(compatibility_peer_id().as_bytes()));
    let root = compatibility_live_root(args)?;
    let autosave = ui::persist::Autosave::disabled("Notes owns its canonical notebook persistence");
    let mut registry = tilo_actions::ActionRegistry::new();
    notes::actions::register(&mut registry);
    Ok(ui::launch::LegacyLaunchInputs::new(state, root, autosave)
        .with_registry(registry)
        .with_widget_owned_persistence())
}

fn prepare_component(
    _definition: &ui::application::ApplicationDefinition,
    args: &LaunchArgs,
) -> Result<ui::application::PreparedComponentInputs, LaunchError> {
    let inputs = ui::application::ComponentRunnerInputs::from_native_process();
    if args.shots.is_some() || args.responsive_matrix.is_some() {
        notes::capture::prepare_fixture(args.scenario.as_deref(), inputs)
    } else if args.native_audit {
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

#[cfg(test)]
mod launch_selection_tests {
    use super::*;
    use std::ffi::OsString;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn explicit_legacy_selectors_choose_the_compatibility_runner() {
        for values in [
            vec!["--notebook-dir", "/old-notes"],
            vec!["--notebook-dir=/old-notes"],
            vec!["--notebook-state-dir", "/old-state"],
            vec!["--notebook-state-dir=/old-state"],
            vec!["--import-legacy-stopped"],
        ] {
            assert_eq!(
                launch_route_from(args(&values)).unwrap(),
                LaunchRoute::Compatibility
            );
        }
    }

    #[test]
    fn classifier_honours_option_values_and_double_dash() {
        assert_eq!(
            launch_route_from(args(&["--notebook-dir", "--shots"])).unwrap(),
            LaunchRoute::Compatibility
        );
        assert_eq!(
            launch_route_from(args(&["--", "--notebook-dir=/a-filename"])).unwrap(),
            LaunchRoute::Normal
        );
        assert_eq!(
            launch_route_from(args(&["notes--notebook-dir=ordinary.md"])).unwrap(),
            LaunchRoute::Normal
        );
    }

    #[test]
    fn compatibility_selectors_cannot_enter_capture_or_audit_modes() {
        for values in [
            vec!["--shots", "/tmp/out", "--notebook-dir=/old"],
            vec!["--responsive-matrix=/tmp/out", "--import-legacy-stopped"],
            vec!["--headless-drive", "actions.json", "--notebook-dir=/old"],
            vec!["--native-audit", "--notebook-state-dir=/old-state"],
        ] {
            assert!(launch_route_from(args(&values)).is_err());
        }
    }

    #[test]
    fn workspace_launches_refuse_private_compatibility_custody() {
        assert!(refuse_workspace_compatibility_from(true).is_err());
        refuse_workspace_compatibility_from(false).unwrap();
    }

    #[test]
    fn compatibility_preflight_refuses_an_empty_alternate_notebook() {
        let root = tempfile::tempdir().unwrap();
        assert!(validate_compatibility_source(root.path(), false).is_err());
        assert!(!root.path().join(".qos-records").exists());
    }

    #[test]
    fn compatibility_preflight_accepts_only_retired_or_acknowledged_legacy_sources() {
        let retired = tempfile::tempdir().unwrap();
        std::fs::write(retired.path().join(".notes-canonical-v1"), b"{}").unwrap();
        validate_compatibility_source(retired.path(), false).unwrap();

        let legacy = tempfile::tempdir().unwrap();
        std::fs::write(
            notes::storage::workspace_file_path(legacy.path()),
            b"legacy",
        )
        .unwrap();
        assert!(validate_compatibility_source(legacy.path(), false).is_err());
        validate_compatibility_source(legacy.path(), true).unwrap();
    }
}
