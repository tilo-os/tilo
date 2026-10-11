//! Assertions an application's tests share, available to downstream crates
//! through the `test-support` feature.

pub mod action_contract;
#[cfg(test)]
mod clicks;
mod dispatch;
mod inert;
#[cfg(test)]
mod owned_text;
#[cfg(test)]
mod widgets;
#[cfg(test)]
pub(crate) use clicks::{clicks, record_click};
pub use dispatch::{Dispatch, Dispatched, dispatch};
pub use inert::inert_widget_methods;
#[cfg(test)]
pub(crate) use owned_text::OwnedText;
#[cfg(test)]
pub(crate) use widgets::{delegate_widget_methods, test_app_widget};
