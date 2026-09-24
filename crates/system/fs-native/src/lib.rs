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
//!   changed;
//! - [`chunking`]: the size-scaled FastCDC/BLAKE3 content format;
//! - [`hydrate`]: placeholder files filled on first access through
//!   pre-content events.
//!
//! Nothing here is on the I/O path of a resident file.
#![allow(
    clippy::missing_errors_doc,
    reason = "fallible functions return the io::Error of the syscall they wrap"
)]

pub mod btrfs;
pub mod capture;
pub mod chunking;
pub mod extents;
pub mod fanotify;
pub mod hydrate;
pub mod journal;
pub mod ranges;
pub mod store;
pub mod sys;
