//! The native local data plane (`docs/architecture/storage.md`, §6, Tier 1).
//!
//! Resident files are real files on a dedicated Btrfs store volume. Reads,
//! writes, `stat` and `mmap` of them never leave the kernel. This crate
//! provides the pieces the storage daemon composes around that tree:
//!
//! - [`probe`]: feature probes for the kernel facts Tier 1 depends on (K1–K6)
//!   and the tier selection that follows from them;
//! - [`journal`]: a fanotify filesystem mark turned into an ordered journal
//!   of path-level changes;
//! - [`capture`]: capture epochs — an atomic snapshot, an extent diff
//!   against the previous epoch, and incremental re-chunking of only what
//!   changed (the content format itself is `fs-content`);
//! - [`hydrate`]: placeholder files filled on first access through
//!   pre-content events, and write gates for leased files;
//! - [`access`]: whether a blocked access reads or writes.
//!
//! Nothing here is on the I/O path of a resident file.
//!
//! The kernel interfaces are Linux's; elsewhere the crate is empty.
#![cfg(target_os = "linux")]
#![allow(
    clippy::missing_errors_doc,
    reason = "fallible functions return the io::Error of the syscall they wrap"
)]

pub mod access;
pub mod btrfs;
pub mod capture;
pub mod extents;
pub mod fanotify;
pub mod hydrate;
pub mod journal;
pub mod probe;
/// Byte range sets, shared with the content store's partial files.
pub use fs_content::ranges;
pub mod store;
pub mod sys;
