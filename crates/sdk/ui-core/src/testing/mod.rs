//! Assertions an application's tests share, available to downstream crates
//! through the `test-support` feature.

pub mod action_contract;
#[cfg(test)]
mod widgets;
#[cfg(test)]
pub(crate) use widgets::{delegate_widget_methods, inert_widget_methods, test_app_widget};
