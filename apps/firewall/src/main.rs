//! Firewall — standard qOS application entry point.

use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};
use ui::widget::Widget;

fn shot_root(_: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    Ok(
        if std::env::var("QOS_FIREWALL_SAMPLE").as_deref() == Ok("1") {
            firewall::app_root_demo()
        } else {
            firewall::app_root()
        },
    )
}

pub fn main() {
    ui::launch::run_or_exit(
        LaunchSpec::new("dev.qos.firewall", firewall::app_root)
            .shot_root(shot_root)
            .configure_app(|app| firewall::actions::register(app.action_registry_mut())),
    )
}
