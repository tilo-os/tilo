//! freedesktop.org desktop entries — parsing, scanning, and launching.
//!
//! This is how Tilo learns about applications it did not package: the
//! `.desktop` files a distribution, a Flatpak, or the user dropped into
//! an `applications` directory. It is deliberately a *separate* concept
//! from a Tilo package:
//!
//! - a Tilo package is a signed `.tapp` with a manifest, launched into a
//!   bubblewrap sandbox with one writable directory and a room id;
//! - a desktop entry is a text file naming a command line, and running
//!   it runs that command line on the host.
//!
//! Nothing in this crate sandboxes anything, and nothing in it should
//! grow the ability to. See [`launch`] for the consequences and how they
//! are surfaced.
//!
//! ## Modules
//!
//! - [`entry`]  — the file format: groups, keys, locales, escapes.
//! - [`exec`]   — `Exec=` splitting and `%f`/`%F`/`%u`/`%U`/`%i` expansion.
//! - [`locale`] — the `Name[xx]=` fallback chain.
//! - [`mod@scan`]   — the XDG search path, desktop-file IDs, precedence.
//! - [`cache`]  — memoising a scan for a UI that queries per keystroke.
//! - [`icon_theme`] — resolving `Icon=` through the icon-theme search path.
//! - [`launch`] — spawning a host application, and what that costs.
//! - [`autostart`] — the XDG autostart entries a session starts at login.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod autostart;
pub mod cache;
pub mod entry;
pub mod exec;
pub mod icon_theme;
pub mod launch;
pub mod locale;
pub mod scan;

pub use entry::{DesktopAction, DesktopEntry, EntrySource, EntryType};
pub use scan::{Scan, ScanStats, scan, scan_dirs};
