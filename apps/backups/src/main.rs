//! Backups — standard qOS application entry point.

use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};
use ui::widget::Widget;

fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    if args.has("--empty") {
        Ok(backups::app_root())
    } else if args.scenario.as_deref() == Some("status") {
        let mut app = backups::BackupsApp::new_demo();
        app.arm_status_bubble_for_shots();
        Ok(Box::new(app))
    } else {
        Ok(backups::app_root_demo())
    }
}

pub fn main() {
    ui::launch::run_or_exit(
        LaunchSpec::new("dev.qos.backups", backups::app_root)
            .shot_root(shot_root)
            .scenarios(&["status"])
            .allowed_flags(&["--empty"])
            .configure_app(|app| backups::actions::register(app.action_registry_mut())),
    )
}
