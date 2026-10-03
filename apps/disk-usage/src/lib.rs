//! Disk Usage Analyzer — a responsive showcase for `ui::widget::PieChart`.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "bounded app-local chart and filesystem arithmetic"
)]

pub mod app;
pub mod doc;
pub mod resource;
pub mod scan;

use ui::widget::Widget;

pub fn app_root() -> Box<dyn Widget> {
    Box::new(app::DiskUsageApp::new())
}
pub fn shot_root_scenario(scenario: &str) -> Box<dyn Widget> {
    if scenario.starts_with("home") {
        Box::new(app::DiskUsageApp::sample_home())
    } else if scenario.starts_with("analysis-hover") {
        Box::new(app::DiskUsageApp::sample_with_inspection())
    } else {
        Box::new(app::DiskUsageApp::sample())
    }
}
pub use resource::{DiskUsage, DiskUsageShell};
