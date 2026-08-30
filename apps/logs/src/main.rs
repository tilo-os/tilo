//! Logs — standard qOS application entry point.

use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};
use ui::widget::Widget;

fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    let entries = if std::env::var("QOS_LOGS_EMPTY").is_ok() {
        Vec::new()
    } else {
        logs::journal::sample_entries()
    };
    let mut app = logs::LogsApp::new(entries, false);
    if args.has("--detail") {
        app.shots_show_detail();
    }
    Ok(Box::new(app))
}

pub fn main() {
    ui::launch::run_or_exit(
        LaunchSpec::new("dev.qos.logs", logs::app_root)
            .shot_root(shot_root)
            .allowed_flags(&["--detail"]),
    )
}
