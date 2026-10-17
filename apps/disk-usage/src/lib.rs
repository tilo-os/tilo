//! Disk Usage Analyzer — a responsive showcase for `ui::widget::PieChart`.

#![allow(
    clippy::cast_precision_loss,
    reason = "bounded app-local chart and filesystem arithmetic"
)]

pub mod app;
pub mod resource;
pub mod scan;

pub use resource::{DiskUsage, DiskUsageShell};
