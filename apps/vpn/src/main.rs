//! VPN — standard qOS application entry point.

use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};
use ui::widget::Widget;

fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    Ok(if args.scenario.as_deref() == Some("confirm") {
        vpn::app_root_demo_confirm()
    } else if std::env::var("QOS_VPN_SAMPLE").as_deref() == Ok("1") {
        vpn::app_root_demo()
    } else {
        vpn::app_root()
    })
}

pub fn main() {
    ui::launch::run_or_exit(
        LaunchSpec::new("dev.qos.vpn", vpn::app_root)
            .shot_root(shot_root)
            .scenarios(&["confirm"])
            .configure_app(|app| vpn::actions::register(app.action_registry_mut())),
    )
}
