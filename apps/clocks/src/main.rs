#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

//! Clocks — live window on the app framework; `--shots` stay standalone.

use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};
use ui::widget::Widget;

#[cfg(feature = "thin")]
extern crate runtime_dylib;

fn main() {
    if standalone_capture() {
        ui::launch::run_or_exit(
            &LaunchSpec::new("so.tilo.clocks", clocks::app_root)
                .shot_root(
                    |args: &LaunchArgs| -> Result<Box<dyn Widget>, LaunchError> {
                        let scenario = args.scenario.as_deref().unwrap_or_default();
                        Ok(clocks::app_root_for_shot(
                            scenario.strip_suffix("-narrow").unwrap_or(scenario),
                        ))
                    },
                )
                .scenarios(&[
                    "empty",
                    "running",
                    "ringing",
                    "world",
                    "alarms",
                    "stopwatch",
                    "timers",
                    "running-narrow",
                    "ringing-narrow",
                    "world-narrow",
                    "alarms-narrow",
                    "stopwatch-narrow",
                    "timers-narrow",
                ])
                .shot_viewport(|args| {
                    if args
                        .scenario
                        .as_deref()
                        .is_some_and(|scenario| scenario.ends_with("-narrow"))
                    {
                        (520, 820)
                    } else {
                        (1280, 800)
                    }
                }),
        );
        return;
    }
    ui::framework::run::<clocks::ClocksShell>();
}

fn standalone_capture() -> bool {
    std::env::args().any(|arg| {
        arg == "--shots"
            || arg.starts_with("--shots=")
            || arg == "--responsive-matrix"
            || arg.starts_with("--responsive-matrix=")
    })
}
