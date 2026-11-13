//! The native local data plane.
//!
//! Resident files are real files on a dedicated Btrfs store volume, so reads,
//! writes, `stat` and `mmap` of them never leave the kernel; this crate holds
//! the pieces the storage daemon composes around that tree: feature [`probe`]s
//! and the tier they select, the fanotify [`journal`], [`capture`] epochs,
//! [`hydrate`] placeholders, and [`access`] classification. The kernel
//! interfaces are Linux's, so elsewhere the crate is empty.
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
pub mod snapshot_lease;
pub use fs_content::ranges;
pub mod store;
pub mod sys;
