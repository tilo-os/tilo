//! Assertions an application's tests share, available to downstream crates
//! through the `test-support` feature.

#[cfg(feature = "model")]
pub mod action_contract;
#[cfg(test)]
mod clicks;
pub mod component_harness;
mod dispatch;
#[cfg(all(test, unix))]
mod executable;
mod layout;
#[cfg(test)]
mod menu_rows;
mod own_process;
#[cfg(test)]
mod owned_text;
#[cfg(test)]
mod widgets;
#[cfg(test)]
pub(crate) use clicks::{clicks, record_click};
#[cfg(test)]
pub(crate) use component_harness::harness;
pub use dispatch::{Dispatch, Dispatched, dispatch};
#[cfg(all(test, unix))]
pub use executable::write_executable;
pub use layout::{lay_out, lay_out_in};
#[cfg(test)]
pub(crate) use menu_rows::{MENU_ROW_RESOURCE, MenuRows, bound_press};
#[cfg(test)]
pub(crate) use own_process::{in_own_process, test_path};
#[cfg(test)]
pub(crate) use owned_text::OwnedText;
pub use tilo_test_env::{hang_bound, hang_deadline, wait_for, wait_until};
#[cfg(test)]
#[cfg(feature = "model")]
pub(crate) use widgets::TwoWindows;
#[cfg(test)]
pub(crate) use widgets::{
    Seen, TestLeaf, delegate_widget_methods, inert_widget_methods, test_app_widget,
};
