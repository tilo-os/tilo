//! Hex editor — standard qOS application entry point.

use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};
use ui::widget::Widget;

fn shot_root(_: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    Ok(hex_editor::app_root())
}

pub fn main() {
    ui::launch::run_or_exit(
        LaunchSpec::new("dev.qos.hex-editor", hex_editor::app_root)
            .shot_root(shot_root)
            .configure_app(|app| hex_editor::actions::register(app.action_registry_mut())),
    )
}
