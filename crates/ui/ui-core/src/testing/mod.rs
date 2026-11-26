//! Assertions an application's tests share, available to downstream crates
//! through the `test-support` feature.

#[cfg(any(test, feature = "test-support"))]
mod clicks;
pub mod component_harness;
mod dispatch;
#[cfg(all(test, unix))]
mod executable;
mod layout;
#[cfg(any(test, feature = "test-support"))]
mod menu_rows;
#[path = "own_process_tests.rs"]
mod own_process;
#[cfg(test)]
mod owned_text;
#[cfg(any(test, feature = "test-support"))]
mod widgets;
#[cfg(any(test, feature = "test-support"))]
pub use crate::{delegate_widget_methods, inert_widget_methods, test_app_widget};
#[cfg(any(test, feature = "test-support"))]
pub use clicks::{clicks, record_click};
pub use component_harness::harness;
pub use dispatch::{Dispatch, Dispatched, dispatch};
#[cfg(all(test, unix))]
pub use executable::write_executable;
pub use layout::{lay_out, lay_out_in};
#[cfg(any(test, feature = "test-support"))]
pub use menu_rows::{MENU_ROW_RESOURCE, MenuRows, bound_press};
#[cfg(any(test, feature = "test-support"))]
pub use own_process::{in_own_process, test_path};
#[cfg(test)]
pub(crate) use owned_text::OwnedText;
pub use tilo_test_env::{hang_bound, hang_deadline, wait_for, wait_until};
#[cfg(all(test, feature = "model"))]
pub(crate) use widgets::TwoWindows;
#[cfg(any(test, feature = "test-support"))]
pub use widgets::{Seen, TestLeaf};
