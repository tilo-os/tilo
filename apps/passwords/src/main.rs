use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};

fn shot(args: &LaunchArgs) -> Result<Box<dyn ui::widget::Widget>, LaunchError> {
    let mode = match args.value("--screen").unwrap_or("vault") {
        "vault" | "desktop" | "narrow" => passwords::ShowcaseMode::Vault,
        "editor" => passwords::ShowcaseMode::Editor,
        "health" => passwords::ShowcaseMode::Health,
        "import" => passwords::ShowcaseMode::Import,
        "export" => passwords::ShowcaseMode::Export,
        value => {
            return Err(LaunchError::Scenario(format!(
                "unknown Passwords screen {value:?}"
            )));
        }
    };
    Ok(passwords::app_root_showcase(mode))
}

#[cfg(feature = "thin")]
extern crate runtime_dylib;

fn main() {
    ui::launch::run_or_exit(
        &LaunchSpec::new("dev.qos.passwords", passwords::app_root)
            .shot_root(shot)
            .allowed_options(&["--screen"]),
    );
}
