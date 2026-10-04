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

pub use resource::{DiskUsage, DiskUsageShell};
