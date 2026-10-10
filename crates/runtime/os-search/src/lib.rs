//! The resources a Tilo runtime admits, in the host operating system's search.
//!
//! One index per user, fed by the runtime daemon from the same action
//! catalogue the palette, MCP and assistive technology browse, as the
//! person who owns the runtime ([`OsSearch`]). Each host search is a thin
//! adapter over it:
//!
//! - GNOME Shell's `org.gnome.Shell.SearchProvider2` and `KRunner`'s
//!   `org.kde.krunner1` ask this index to match over the session bus
//!   ([`dbus`]); packaging registers them with the files in [`linux_files`].
//! - Spotlight keeps its own copy, which [`Changes`] keeps in step
//!   (`spotlight`); the app it wakes for a chosen result hands the entry
//!   back to the daemon (`tilo-host-search`).
//!
//! A host hands back only an opaque [`EntryId`]. Opening re-resolves the
//! resource's opening action as the owner and invokes it through the action
//! host, so a result the owner may no longer open opens nothing.

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
