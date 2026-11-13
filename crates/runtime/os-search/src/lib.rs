//! The resources a Tilo runtime admits, in the host operating system's
//! search: one index per user, fed by the daemon from the action catalogue the
//! palette, MCP and assistive technology browse, with one adapter per host
//! ([`dbus`] for GNOME Shell and `KRunner`, `spotlight` for the Mac).
//!
//! A host hands back only an opaque [`EntryId`]: opening re-resolves the action
//! as the owner, so a result the owner may no longer open opens nothing.

#[cfg(all(unix, not(target_os = "macos")))]
pub mod dbus;
mod entry;
mod index;
#[cfg(all(unix, not(target_os = "macos")))]
pub mod linux_files;
mod search;
#[cfg(target_os = "macos")]
pub mod spotlight;

pub use entry::{EntryId, NotAnEntryId, SearchEntry};
pub use index::{Changes, ResourceIndex};
pub use search::{OpenError, OsSearch};

#[cfg(test)]
mod tests;
