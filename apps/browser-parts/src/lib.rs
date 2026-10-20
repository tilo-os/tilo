//! The parts of Browser people rarely use, as a library of their own:
//! extensions (installing, unpacking and running them, the toolbar they
//! add), the site controls with the content blocker's lists, the
//! capabilities a site asks for, saved addresses and passwords, the service
//! profiles, destinations and the sessions they open, and the private
//! window; with them the records and vocabulary those parts and the page
//! share (the effects Browser answers, bookmarks, downloads) and Browser's
//! copy. Their tests run in Browser's own tests, which compile them in.
//! Browser's engine library carries it beside the runtime, so the app's
//! package carries the browsing itself.

#![allow(
    clippy::field_reassign_with_default,
    clippy::redundant_field_names,
    clippy::too_many_lines,
    reason = "migration stack lint debt is scoped to this target"
)]
// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]
// What this crate makes public it makes public for Browser alone, not as an
// API: the errors and panics of its items are documented where Browser
// handles them, as they were when the two were one crate.
#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::new_without_default,
    reason = "public only to Browser; see the note above"
)]

// The parts are tested in Browser's own tests, which compile them in with
// the browser they serve; this crate's own test build leaves them out.
#[cfg(not(test))]
pub mod actions;
#[cfg(not(test))]
pub mod address_controls;
#[cfg(not(test))]
pub mod autofill;
#[cfg(not(test))]
pub mod back_hold;
#[cfg(not(test))]
pub mod blocker;
#[cfg(not(test))]
pub mod browsing_mode;
#[cfg(not(test))]
pub mod capabilities;
#[cfg(not(test))]
pub mod credentials;
#[cfg(not(test))]
pub mod crx;
#[cfg(not(test))]
pub mod downloads;
#[cfg(not(test))]
pub mod effects;
#[cfg(not(test))]
pub mod engine_setup;
#[cfg(not(test))]
pub mod extension_controller;
#[cfg(not(test))]
pub mod extension_host;
#[cfg(not(test))]
pub mod extension_toolbar;
#[cfg(not(test))]
pub mod extensions;
#[cfg(not(test))]
pub mod library;
#[cfg(not(test))]
pub mod native_blocker;
#[cfg(not(test))]
pub mod notice;
#[cfg(not(test))]
pub mod page_menu;
#[cfg(not(test))]
pub mod private_window;
#[cfg(not(test))]
pub mod rows;
#[cfg(not(test))]
pub mod service_destinations;
#[cfg(not(test))]
pub mod service_profiles;
#[cfg(not(test))]
pub mod service_sessions;
#[cfg(not(test))]
pub mod site_controls;
#[cfg(not(test))]
pub mod storage;
#[cfg(not(test))]
pub mod web_store;

// Browser's copy, which the app and its parts resolve alike.
ui::embedded_catalog!(pub static CATALOG, ["en-US", "de-DE"]);
