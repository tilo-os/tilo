#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

#[cfg(feature = "thin")]
extern crate runtime_dylib;

use anyhow::{Context, Result};
use std::path::PathBuf;
use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    ui::launch::run(
        &LaunchSpec::new_with_args("so.tilo.scratchpad", |_| {
            Err(LaunchError::Operation(
                "Scratchpad requires its paired saved state and root; use --shots or --responsive-matrix for capture".into(),
            ))
        })
        .allowed_options(&["--data-dir"])
        .allowed_live_options(&["--data-dir"])
        .disable_headless()
        .shot_viewport(|_| (800, 600))
        .shot_handler(run_session)
        .live_handler(run_session),
    )?;
    Ok(())
}

fn run_session(args: &LaunchArgs) -> Result<(), LaunchError> {
    run_session_inner(args).map_err(|error| LaunchError::Operation(error.to_string()))
}

// The coordinated state/root pair preserves explicit Save and the exclusive
// data-directory lock; the launcher's automatic document autosave is not used.
fn run_session_inner(args: &LaunchArgs) -> Result<()> {
    let directory = match args.value("--data-dir") {
        Some(path) => PathBuf::from(path),
        None => dirs::data_local_dir()
            .context("no local data directory")?
            .join("scratchpad"),
    };
    let scope = ui::wake::WakeScope::new();
    let _entry = scope.enter();
    ui::i18n::I18nManager::init("en-US");
    let (state, root) = scratchpad::open(&directory)?;
    if let Some(out) = &args.shots {
        let mut driver =
            ui::headless::Driver::new(state, root, args.viewport.unwrap_or((800, 600)));
        driver.set_theme(ui::launch::shot_theme(args));
        scratchpad::actions::register(driver.action_registry_mut());
        driver.render_png(out);
        println!("Rendered {}", out.display());
    } else {
        let mut app =
            ui::App::from_scoped_parts(scope, state, root).with_window_title("Scratchpad");
        if let Some(scale) = args.text_scale {
            app.set_text_scale(scale);
        }
        scratchpad::actions::register(app.action_registry_mut());
        app.run();
    }
    Ok(())
}
