//! Mail — standard qOS application entry point.

use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};
use ui::widget::Widget;

fn scenario(args: &LaunchArgs) -> &str {
    args.scenario.as_deref().unwrap_or("wide")
}

fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    Ok(mail::app_root_scenario(scenario(args)))
}

fn shot_viewport(args: &LaunchArgs) -> (u32, u32) {
    mail::shot_viewport(scenario(args))
}

pub fn main() {
    ui::launch::run_or_exit(
        LaunchSpec::new("dev.qos.mail", mail::app_root)
            .shot_root(shot_root)
            .scenarios(&[
                "wide",
                "narrow",
                "narrow_detail",
                "narrow_drawer",
                "mid",
                "compose",
                "setup",
                "setup_add",
                "setup_oauth",
                "firstrun",
                "html_mail",
            ])
            .shot_viewport(shot_viewport)
            .configure_app(|app| mail::actions::register(app.action_registry_mut())),
    )
}
