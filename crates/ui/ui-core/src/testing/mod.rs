//! Assertions an application's tests share, available to downstream crates
//! through the `test-support` feature.

#[cfg(any(test, feature = "test-support"))]
mod clicks;
mod dispatch;
#[cfg(all(test, unix))]
mod executable;
mod layout;
#[cfg(any(test, feature = "test-support"))]
mod menu_rows;
#[path = "own_process_tests.rs"]
mod own_process;
#[cfg(any(test, feature = "test-support"))]
pub mod owned_text;
#[cfg(any(test, feature = "test-support"))]
mod widgets;
#[cfg(any(test, feature = "test-support"))]
pub use crate::{delegate_widget_methods, inert_widget_methods, test_app_widget};
#[cfg(any(test, feature = "test-support"))]
pub use clicks::{clicks, record_click};
pub use dispatch::{Dispatch, Dispatched, dispatch};
#[cfg(all(test, unix))]
pub use executable::write_executable;
pub use layout::{lay_out, lay_out_in};
#[cfg(any(test, feature = "test-support"))]
pub use menu_rows::{MENU_ROW_RESOURCE, MenuRows, bound_press};
#[cfg(any(test, feature = "test-support"))]
pub use own_process::{in_own_process, test_path};
#[cfg(any(test, feature = "test-support"))]
pub use owned_text::OwnedText;
pub use tilo_test_env::{hang_bound, hang_deadline, wait_for, wait_until};
#[cfg(all(any(test, feature = "test-support"), feature = "model"))]
pub use widgets::TwoWindows;
#[cfg(any(test, feature = "test-support"))]
pub use widgets::{Seen, TestLeaf};
