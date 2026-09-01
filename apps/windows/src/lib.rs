//! Windows Apps is qOS' compatibility control plane.
//!
//! It is intentionally a *manager*, never a second launcher implementation:
//! the compatibility service decides the actual Wine/UMU/VM invocation.  This
//! crate renders that decision, exposes an auditable override, and publishes
//! actions for the system palette.  [`provider::CompatibilityProvider`] is the
//! only backend seam; its deterministic sample implementation supports offline
//! screenshots and contract tests until the system service is wired.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::too_many_arguments,
    clippy::unnecessary_literal_bound,
    reason = "app-local rendering geometry and Widget trait signatures"
)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "test assertions"
    )
)]

pub mod actions;
pub mod app;
pub mod model;
pub mod provider;

pub use app::WindowsApp;

pub fn app_root() -> Box<dyn ui::widget::Widget> {
    Box::new(WindowsApp::new_live())
}
pub fn app_root_offline() -> Box<dyn ui::widget::Widget> {
    Box::new(WindowsApp::new_offline())
}
