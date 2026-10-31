//! Assertions an application's tests share, available to downstream crates
//! through the `test-support` feature.

pub mod action_contract;
#[cfg(test)]
mod clicks;
mod dispatch;
#[cfg(all(test, unix))]
mod executable;
mod own_process;
#[cfg(test)]
mod owned_text;
#[cfg(test)]
mod widgets;
#[cfg(test)]
pub(crate) use clicks::{clicks, record_click};
pub use dispatch::{Dispatch, Dispatched, dispatch};
#[cfg(all(test, unix))]
pub use executable::write_executable;
#[cfg(test)]
pub(crate) use own_process::{in_own_process, test_path};
#[cfg(test)]
pub(crate) use owned_text::OwnedText;
/// The hang bound and bounded waits a test uses, re-exported so an app's
/// tests reach them through `ui` rather than depending on the test
/// environment's own crate.
pub use tilo_test_env::{hang_bound, hang_deadline, wait_for, wait_until};
#[cfg(test)]
pub(crate) use widgets::{delegate_widget_methods, inert_widget_methods, test_app_widget};
