#![cfg_attr(
    test,
    allow(
        clippy::disallowed_methods,
        reason = "unit tests may drive real threads and wait on the real clock; the rule guards the library"
    )
)]
#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        clippy::float_cmp,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

//! Host services: what a Tilo UI process borrows from whatever hosts it.
//!
//! The toolkit reads the clock, runs background work, reaches the clipboard,
//! file choosers, URI handlers, speech, notifications, media controls,
//! location and screen capture only through this crate. Each capability is a
//! trait in [`host`] with a request/outcome vocabulary beside it
//! ([`portal`], [`mpris`], [`location`], [`capture`], [`clipboard`]), so the
//! same toolkit runs under the native desktop runner, inside a multi-app host
//! such as the compositor, or in a test that records every effect.
//!
//! The user-mediated services' traits and their Linux, macOS and web backends
//! live in platform crates (`tilo-clipboard`, `tilo-dialogs`, …), which this
//! crate re-exports; [`host::HostServices`] is the per-app registry of them.
//! The remaining native backends (media sessions, location, screen capture)
//! live in [`native`] behind the `native` feature.
//!
//! Apps do not depend on this crate: `tilo-ui` re-exports every module here
//! under the same path (`ui::portal`, `ui::host`, `ui::time`, …), and
//! `tilo-app` turns every service on by default.

/// Network hints shared with the app-facing SDK. Host transport integration only.
#[doc(hidden)]
pub use tilo_net::status as network_status;

pub mod action_authority;
pub mod background;
pub mod capture;
pub mod clipboard;
pub mod env;
pub mod host;
pub mod location;
pub mod mpris;
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
pub mod png;
/// File choosers and URI opening: `tilo_dialogs::portal` under the
/// toolkit's module path.
pub use tilo_dialogs::portal;
// Android's Storage Access Framework chooser (main's Android host).
#[cfg(all(feature = "native", target_os = "android"))]
mod saf;
pub mod time;
pub mod wake;
