//! Assertions an application's tests share, available to downstream crates
//! through the `test-support` feature.

pub mod action_contract;
#[cfg(test)]
mod clicks;
mod dispatch;
#[cfg(all(test, unix))]
mod executable;
mod layout;
mod own_process;
#[cfg(test)]
mod owned_text;
#[cfg(test)]
mod widgets;
#[cfg(test)]
pub(crate) use clicks::{clicks, record_click};
pub use dispatch::{Dispatch, Dispatched, answer_controls, dispatch};
#[cfg(all(test, unix))]
pub use executable::write_executable;
pub use layout::{lay_out, lay_out_in};
#[cfg(test)]
pub(crate) use own_process::{in_own_process, test_path};
#[cfg(test)]
pub(crate) use owned_text::OwnedText;
pub use tilo_test_env::{hang_bound, hang_deadline, wait_for, wait_until};

/// Serve `doc`'s history to every History the tests on this thread open, as the session serves the
/// history of the document a window shows.
pub fn serve_history(doc: &crate::automerge::AutoCommit) {
    crate::history_mode::feed::tests::serve(doc);
}
#[cfg(test)]
pub(crate) use widgets::{
    TwoWindows, delegate_widget_methods, inert_widget_methods, test_app_widget,
};
