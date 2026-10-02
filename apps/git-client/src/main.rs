//! Git — standard Tilo application entry point.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
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

const WIDE_VIEWPORT: (u32, u32) = (1280, 800);
const NARROW_VIEWPORT: (u32, u32) = (680, 860);

#[cfg(feature = "thin")]
extern crate runtime_dylib;

#[allow(
    clippy::disallowed_methods,
    reason = "only reached from the screenshot entry points on the main thread, before \
              the widget that reads these variables is built and before any app worker \
              thread exists"
)]
fn prepare_shot(narrow: bool, scenario: Option<&str>) -> Box<dyn Widget> {
    // An explicit repository is the only way screenshots may inspect the
    // surrounding checkout; otherwise use the deterministic sample fixture.
    if std::env::var_os("GIT_CLIENT_REPO").is_none() {
        unsafe { std::env::set_var("GIT_CLIENT_SAMPLE", "1") };
    }
    if narrow {
        unsafe { std::env::set_var("GIT_CLIENT_FLAP_OPEN", "1") };
    }
    git_client::app_root_shot(scenario)
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by ui::launch::ShotFactory"
)]
fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    Ok(prepare_shot(false, args.scenario.as_deref()))
}

fn command(args: &LaunchArgs) -> Result<bool, LaunchError> {
    if let Some(message) = args.value("--stage-commit") {
        stage_and_commit(message)?;
        return Ok(true);
    }
    let Some(out) = args.value("--shots-narrow") else {
        return Ok(false);
    };
    render(
        args,
        prepare_shot(true, args.scenario.as_deref()),
        NARROW_VIEWPORT,
        std::path::Path::new(out),
    );
    // Preserve the old behavior where both screenshot flags render both files.
    Ok(args.shots.is_none())
}

fn render(args: &LaunchArgs, root: Box<dyn Widget>, viewport: (u32, u32), out: &std::path::Path) {
    // The same palette / text-scale / motion policy as `--shots`, so the
    // narrow capture honours `TILO_SHOTS_THEME` and `TILO_SHOTS_TEXT_SCALE`.
    let theme = ui::launch::shot_theme(args);
    ui::headless::FrameHarness::new_with_theme(ui::State::new(), root, viewport, theme)
        .render_png(out);
    println!("git_client: wrote shot to {}", out.display());
}

fn stage_and_commit(message: &str) -> Result<(), LaunchError> {
    let path = std::env::var("GIT_CLIENT_REPO").map_or_else(
        |_| std::env::current_dir().unwrap_or_else(|_| ".".into()),
        std::path::PathBuf::from,
    );
    git_client::repo::stage_all(&path)
        .map_err(|error| LaunchError::Operation(format!("could not stage changes: {error}")))?;
    let id = git_client::repo::commit(&path, message)
        .map_err(|error| LaunchError::Operation(format!("could not create commit: {error}")))?;
    println!("git_client: committed {id} to {}", path.display());
    Ok(())
}

fn install_locales() -> Result<(), ui::i18n::I18nError> {
    ui::i18n::I18nManager::install_system(
        "en-US",
        &[
            ("en-US", include_str!("../locales/en-US.ftl")),
            ("de-DE", include_str!("../locales/de-DE.ftl")),
        ],
    )
}

fn wants_standalone() -> bool {
    std::env::args().any(|arg| {
        [
            "--shots",
            "--responsive-matrix",
            "--native-audit",
            "--headless",
            "--shots-narrow",
            "--stage-commit",
        ]
        .iter()
        .any(|mode| arg == *mode || arg.starts_with(&format!("{mode}=")))
    })
}

fn shot_spec() -> LaunchSpec {
    LaunchSpec::new("so.tilo.git-client", git_client::app_root)
        .shot_root(shot_root)
        .shot_viewport(|_| WIDE_VIEWPORT)
        .scenarios(git_client::SCENARIOS)
        .allowed_live_options(&["--stage-commit", "--shots-narrow"])
        .command_handler(command)
        .configure_app(|app| {
            app.set_window_title(Some("Git Client".into()));
            git_client::actions::register(app.action_registry_mut());
        })
}

pub fn main() {
    if let Err(error) = install_locales() {
        eprintln!("Git Client locale initialization failed: {error}");
        std::process::exit(1);
    }
    if wants_standalone() {
        ui::launch::run_or_exit(&shot_spec());
        return;
    }
    ui::launch::run_or_exit(&git_client::launch_spec());
}

#[cfg(test)]
mod tests {
    #[test]
    fn startup_installs_remote_status_catalog() {
        super::install_locales().unwrap();
        for (key, value) in [
            ("git-remote-running", ui::i18n!("git-remote-running")),
            ("git-remote-busy", ui::i18n!("git-remote-busy")),
            (
                "git-remote-worker-stopped",
                ui::i18n!("git-remote-worker-stopped"),
            ),
        ] {
            assert_ne!(key, value);
            assert!(!value.is_empty());
        }
    }
}
