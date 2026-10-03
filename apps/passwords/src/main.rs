#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};

fn shot(args: &LaunchArgs) -> Result<Box<dyn ui::widget::Widget>, LaunchError> {
    let screen = args.value("--screen").unwrap_or("vault");
    if screen == "long-list" {
        return Ok(passwords::app_root_long_list_showcase());
    }
    let mode = match screen {
        "vault" | "desktop" | "narrow" => passwords::ShowcaseMode::Vault,
        "editor" => passwords::ShowcaseMode::Editor,
        "health" => passwords::ShowcaseMode::Health,
        "app-access" => passwords::ShowcaseMode::AppAccess,
        "import" => passwords::ShowcaseMode::Import,
        "export" => passwords::ShowcaseMode::Export,
        "delete-confirm" => passwords::ShowcaseMode::DeleteConfirm,
        value => {
            return Err(LaunchError::Scenario(format!(
                "unknown Passwords screen {value:?}"
            )));
        }
    };
    Ok(passwords::app_root_showcase(mode))
}

fn wants_standalone() -> bool {
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

fn shot_spec() -> LaunchSpec {
    LaunchSpec::new("so.tilo.passwords", passwords::app_root)
        .shot_root(shot)
        .allowed_options(&["--screen"])
        .configure_app(|app| {
            app.set_window_title(Some("Passwords".into()));
            app.set_capture_protected(true);
        })
}

#[cfg(feature = "thin")]
extern crate runtime_dylib;

fn main() {
    if wants_standalone() {
        ui::launch::run_or_exit(&shot_spec());
        return;
    }
    ui::launch::run_or_exit(&passwords::launch_spec());
}
