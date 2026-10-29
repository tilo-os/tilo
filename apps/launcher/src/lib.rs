//! Search: the launcher the desktop opens with Super, Super+Space and an
//! app's own search, as an ordinary app.
//!
//! It draws the search card (the field, the filter chips and the results)
//! and publishes what can be done with it as its actions; what it finds and
//! what choosing a row does come from the source its host lends it
//! ([`source::LauncherService`]). The shell mounts it in its own process
//! with a source over everything the desktop can find; on its own, in a
//! capture or a test, it searches a fixed desktop ([`fixture::FixtureSource`]).

pub mod fixture;
pub mod i18n;
pub mod resource;
pub mod source;
pub mod surface;

pub use resource::{Launcher, LauncherEffect, LauncherShell};
pub use source::{
    Answer, Ask, CardFit, LauncherService, LauncherSource, Link, ResourceKind, Search, SearchScope,
};
pub use surface::LauncherSurface;

/// The launcher's package id.
pub const APP_ID: &str = "so.tilo.launcher";

// Also the launcher's application definition (`definition()`), the one
// every host mounts.
ui::dev_reload_entry!(LauncherShell);
