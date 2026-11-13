//! freedesktop.org desktop entries — parsing, scanning, and launching.
//!
//! This is how Tilo learns about applications it did not package: the
//! `.desktop` files a distribution, a Flatpak, or the user dropped into an
//! `applications` directory. A desktop entry names a command line, and
//! running it runs that command line on the host, so nothing in this crate
//! sandboxes anything and nothing in it should grow the ability to.

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
pub mod downloaded;
pub mod entry;
pub mod exec;
pub mod icon_theme;
pub mod launch;
pub mod locale;
#[cfg(target_os = "linux")]
pub mod migration_startup;
pub mod scan;

pub use entry::{DesktopAction, DesktopEntry, EntryKind, EntrySource, EntryType, MainWindows};
pub use scan::{Scan, ScanStats, scan_dirs};
