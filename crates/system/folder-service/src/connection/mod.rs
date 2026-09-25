//! Connected-folder comparison and native endpoint adapters.
//! Reconciliation plans are data; only validated endpoint sessions grant access.
pub mod endpoint;
pub mod model;
pub mod reconcile;

pub mod store;

mod transaction;
pub(crate) mod worker;

/// Early binary startup adapter for the isolated Linux lease receiver only.
/// Ordinary invocations return `None`; no native capability crosses this API.
#[cfg(target_os = "linux")]
#[doc(hidden)]
pub fn maybe_run_lease_receiver(args: &[String]) -> Option<i32> {
    transaction::maybe_run_lease_receiver(args)
}

#[cfg(any(windows, test))]
mod windows_native;
