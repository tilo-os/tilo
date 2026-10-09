//! Assertions an application's tests share, available to downstream crates
//! through the `test-support` feature.

pub mod action_contract;
#[cfg(test)]
mod clicks;
#[cfg(test)]
mod dispatch;
#[cfg(test)]
mod owned_text;
#[cfg(test)]
mod widgets;
#[cfg(test)]
pub(crate) use clicks::{clicks, record_click};
#[cfg(test)]
pub(crate) use dispatch::{Dispatch, dispatch};
#[cfg(test)]
pub(crate) use owned_text::OwnedText;
#[cfg(test)]
pub(crate) use widgets::{delegate_widget_methods, inert_widget_methods, test_app_widget};
