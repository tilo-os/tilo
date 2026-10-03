//! A small, responsive internet speed test.

pub mod app;
pub mod resource;

use ui::widget::Widget;

pub fn app_root() -> Box<dyn Widget> {
    Box::new(app::SpeedTestApp::new())
}

/// The app as every host mounts it: the live root.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::launch::LaunchSpec::new("so.tilo.speed-test", app_root).as_definition("Speed Test")
}

pub fn shot_root(scenario: &str) -> Box<dyn Widget> {
    Box::new(app::SpeedTestApp::demo(scenario))
}
pub use resource::{SpeedTest, SpeedTestShell};
