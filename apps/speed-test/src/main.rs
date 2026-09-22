#[cfg(feature = "thin")]
extern crate runtime_dylib;

use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};

fn shot(args: &LaunchArgs) -> Result<Box<dyn ui::widget::Widget>, LaunchError> {
    Ok(speed_test::shot_root(
        args.scenario.as_deref().unwrap_or("complete"),
    ))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    ui::launch::run(
        &LaunchSpec::new("so.tilo.speed-test", speed_test::app_root)
            .shot_root(shot)
            .scenarios(&[
                "idle",
                "running",
                "complete",
                "complete-narrow",
                "complete-short",
            ])
            .shot_viewport(|args| match args.scenario.as_deref() {
                Some("complete-narrow") => (480, 820),
                Some("complete-short") => (960, 560),
                _ => (1180, 760),
            }),
    )?;
    Ok(())
}
