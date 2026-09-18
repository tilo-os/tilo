//! A small, responsive internet speed test.

pub mod app;

use ui::widget::Widget;

pub fn app_root() -> Box<dyn Widget> {
    Box::new(app::SpeedTestApp::new())
}

pub fn shot_root(scenario: &str) -> Box<dyn Widget> {
    Box::new(app::SpeedTestApp::demo(scenario))
}
