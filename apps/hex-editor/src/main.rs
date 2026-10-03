//! Hex editor — standard Tilo application entry point.

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

use ui::launch::LaunchSpec;

/// Whether the command line names an existing file to open: a positional
/// argument that is not the value of a flag.
fn names_a_file() -> bool {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if ["--shots", "--scenario", "--viewport", "--theme"].contains(&arg.as_str()) {
            let _ = args.next();
            continue;
        }
        if !arg.starts_with('-') && std::path::Path::new(&arg).exists() {
            return true;
        }
    }
    false
}

fn file_spec() -> LaunchSpec {
    LaunchSpec::new("so.tilo.hex-editor", hex_editor::app_root)
        .max_positionals(1)
        .configure_app(|app| hex_editor::actions::register(app.action_registry_mut()))
}

#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    // A positional file opens straight into the editor; everything else, the
    // window and `--shots` with its scenarios, is the framework app.
    if names_a_file() {
        ui::launch::run_or_exit(&file_spec());
        return;
    }
    ui::framework::run::<hex_editor::HexEditorShell>();
}
