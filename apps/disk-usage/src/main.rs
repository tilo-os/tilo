#[cfg(feature = "thin")]
extern crate runtime_dylib;

use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    ui::launch::run(
        &LaunchSpec::new("so.tilo.disk-usage", disk_usage::app_root)
            .shot_root(shot_root)
            .scenarios(&[
                "analysis",
                "analysis-narrow",
                "analysis-hover",
                "analysis-hover-narrow",
                "home",
                "home-narrow",
            ])
            .shot_viewport(|args| {
                if args
                    .scenario
                    .as_deref()
                    .is_some_and(|name| name.ends_with("narrow"))
                {
                    (560, 720)
                } else {
                    (1180, 760)
                }
            }),
    )?;
    Ok(())
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "LaunchSpec requires every shot factory to return Result"
)]
fn shot_root(args: &LaunchArgs) -> Result<Box<dyn ui::widget::Widget>, LaunchError> {
    Ok(disk_usage::shot_root_scenario(
        args.scenario.as_deref().unwrap_or("analysis"),
    ))
}
