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
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::string_slice,
        clippy::panic,
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
//! The native desktop backends live in [`native`] behind the `native` feature
//! and the per-service `portal`, `mpris` and `notifications` features.
//!
//! Apps do not depend on this crate: `tilo-ui` re-exports every module here
//! under the same path (`ui::portal`, `ui::host`, `ui::time`, …), and
//! `tilo-app` turns every service on by default.

pub mod background;
pub mod capture;
pub mod clipboard;
pub mod host;
pub mod location;
pub mod mpris;
#[cfg(feature = "native")]
pub mod native;
pub mod png;
pub mod portal;
#[cfg(any(all(feature = "native", target_os = "android"), test))]
mod saf;
pub mod time;
pub mod wake;
