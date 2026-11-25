#![cfg_attr(
    test,
    allow(
        clippy::disallowed_methods,
        reason = "unit tests may drive real threads and wait on the real clock; the rule guards the library"
    )
)]
//! Process services: what an app borrows from its process.
//!
//! The toolkit reads the clock, runs background work, reaches the clipboard,
//! file choosers, URI handlers, speech, notifications, media controls,
//! location and screen capture only through this crate. Each capability is a
//! trait in [`process`] with a request/outcome vocabulary beside it
//! ([`portal`], [`mpris`], [`location`], [`capture`], [`clipboard`]), so the
//! same toolkit runs under the native desktop runner, inside an app process
//! such as the compositor's, or in a test that records every effect.
//!
//! The user-mediated services' traits and their Linux, macOS and web backends
//! live in platform crates (`tilo-clipboard`, `tilo-dialogs`, …), which this
//! crate re-exports; [`process::ProcessServices`] is the per-app registry of them.
//! The remaining native backends (media sessions and screen capture) live in
//! [`native`] behind the `native` feature. Location is the device's own: a
//! process asks the device's action (`devices/<device>/location#locate`) and
//! never the platform's location service, which stays in the device's daemon.
//!
//! Apps do not depend on this crate: `tilo-ui` re-exports every module here
//! under the same path (`ui::portal`, `ui::process`, `ui::time`, …), and
//! `tilo-app` turns every service on by default.

/// Network hints shared with the app-facing SDK. Process transport integration only.
#[doc(hidden)]
pub use tilo_net::status as network_status;

pub mod action_authority;
pub mod background;
pub mod capture;
pub mod clipboard;
pub mod env;
pub mod location;
#[cfg(feature = "models")]
pub mod models;
pub mod mpris;
pub mod process;
/// User notifications: `tilo_notify` under the toolkit's module path.
pub mod notify {
    pub use tilo_notify::*;
}
#[cfg(feature = "native")]
pub mod native;
/// App-scoped secrets: `tilo_secrets` under the toolkit's module path.
pub mod secrets {
    pub use tilo_secrets::*;
}
pub mod jpeg;
pub mod png;
/// File choosers and URI opening: `tilo_dialogs::portal` under the
/// toolkit's module path.
pub use tilo_dialogs::portal;
/// The choosers the session shell draws itself (`tilo_dialogs::shell`), and
/// the connection to it every app launched into a workspace is handed.
pub use tilo_dialogs::shell as shell_choosers;
// Android's Storage Access Framework chooser (the Android build).
#[cfg(all(feature = "native", target_os = "android"))]
mod saf;
pub mod time;
#[cfg(target_os = "linux")]
mod vault_secrets;
pub mod wake;
