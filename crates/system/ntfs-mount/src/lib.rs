//! Consent-gated access to NTFS drives: Windows partitions next to Tilo and
//! external drives, opened in place instead of copied.
//!
//! The kernel's NTFS driver is never loaded by a mere mount attempt (the
//! image blacklists its filesystem aliases); a root service loads and uses a
//! driver only after the person agreed in the shell's own prompt, choosing
//! "Open" (the kernel driver, fast) or "Open safely" (a confined userspace
//! driver, for drives they do not know). Remembered choices are bound to
//! where a drive sits in the hardware ([`policy`]), never to what the drive
//! claims to be.
//!
//! - [`protocol`]: what the shell and Settings send and receive, and
//!   [`protocol::call`].
//! - [`policy`]: when to ask, what to pre-select, what may be remembered.
//! - [`topology`], [`identity`], [`boot`]: the facts those rules read.
//! - [`health`]: the disk-check flag, read in the confined preflight.
//! - `service` (feature `service`): the root service itself.

pub mod boot;
pub mod grants;
pub mod health;
pub mod identity;
pub mod policy;
pub mod protocol;
pub mod topology;

#[cfg(feature = "service")]
pub mod service;
