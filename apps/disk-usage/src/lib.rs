//! Disk Usage Analyzer — a responsive showcase for `ui::widget::PieChart`.

#![allow(
    clippy::cast_precision_loss,
    reason = "bounded app-local chart and filesystem arithmetic"
)]

pub mod resource;
pub mod scan;
mod view;

pub use resource::{DiskUsage, DiskUsageShell};
