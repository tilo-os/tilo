use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};
use ui::widget::Widget;

#[cfg(feature = "thin")]
extern crate runtime_dylib;

fn main() {
    ui::launch::run_or_exit(
        &LaunchSpec::new("dev.qos.clocks", clocks::app_root)
            .shot_root(
                |args: &LaunchArgs| -> Result<Box<dyn Widget>, LaunchError> {
                    Ok(clocks::app_root_for_shot(
                        args.scenario.as_deref().unwrap_or_default(),
                    ))
                },
            )
            .scenarios(&["empty", "running", "world", "alarms", "stopwatch"]),
    );
}
