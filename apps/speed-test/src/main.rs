#[cfg(feature = "thin")]
extern crate runtime_dylib;

use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};

#[expect(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by `LaunchSpec::shot_root`"
)]
fn shot(args: &LaunchArgs) -> Result<Box<dyn ui::widget::Widget>, LaunchError> {
    Ok(speed_test::shot_root(
        args.scenario.as_deref().unwrap_or("complete"),
    ))
}

fn wants_standalone(args: impl Iterator<Item = String>) -> bool {
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => {
                let _ = args.next();
            }
            "--shots" | "--responsive-matrix" | "--native-audit" | "--headless" => return true,
            flag if flag.starts_with('-') => {}
            _ => {}
        }
    }
    false
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if !wants_standalone(std::env::args().skip(1)) {
        ui::framework::run::<speed_test::SpeedTestShell>();
        return Ok(());
    }
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
