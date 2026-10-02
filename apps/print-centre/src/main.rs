use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};
use ui::widget::Widget;

#[allow(
    clippy::unnecessary_wraps,
    reason = "LaunchSpec requires every shot factory to return Result"
)]
fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    let scenario = match args.scenario.as_deref() {
        Some("active") => print_centre::DemoScenario::Active,
        Some("attention") => print_centre::DemoScenario::Attention,
        Some("empty") => print_centre::DemoScenario::Empty,
        Some("queue") => print_centre::DemoScenario::Queue,
        Some("device") => print_centre::DemoScenario::Device,
        Some("discovery") => print_centre::DemoScenario::Discovery,
        Some("print" | "print-narrow") => print_centre::DemoScenario::PrintSetup,
        Some("scan") => print_centre::DemoScenario::ScanSetup,
        Some("scan-result" | "scan-result-narrow") => print_centre::DemoScenario::ScanResult,
        Some("history") => print_centre::DemoScenario::History,
        Some("diagnostics") => print_centre::DemoScenario::Diagnostics,
        Some("service-error" | "service-error-narrow") => print_centre::DemoScenario::ServiceError,
        _ => print_centre::DemoScenario::Dashboard,
    };
    Ok(print_centre::app_root_demo(scenario))
}

#[cfg(feature = "thin")]
extern crate runtime_dylib;

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

fn main() {
    if !wants_standalone(std::env::args().skip(1)) {
        ui::framework::run::<print_centre::PrintCentreShell>();
        return;
    }
    ui::launch::run_or_exit(
        &LaunchSpec::new("so.tilo.print-centre", print_centre::app_root)
            .shot_root(shot_root)
            .scenarios(&[
                "active",
                "attention",
                "empty",
                "narrow",
                "queue",
                "device",
                "discovery",
                "print",
                "print-narrow",
                "scan",
                "scan-result",
                "scan-result-narrow",
                "history",
                "diagnostics",
                "service-error",
                "service-error-narrow",
            ])
            .shot_viewport(|args| {
                if matches!(
                    args.scenario.as_deref(),
                    Some("narrow" | "print-narrow" | "scan-result-narrow" | "service-error-narrow")
                ) {
                    (520, 820)
                } else {
                    (1280, 800)
                }
            }),
    );
}
