//! Device location, as the host provides it: `tilo_location` under the
//! toolkit's module path.
//!
//! Apps ask [`crate::host::HostServices::location`] (or
//! [`request_location`]) for a fix and drain the returned channel once per
//! frame; the UI thread never blocks.

pub use tilo_location::*;
