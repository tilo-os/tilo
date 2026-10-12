//! Confined helper processes and the shared memory they exchange, with one
//! API on every target.
//!
//! A crate that parses hostile input (PDFs, fonts, media containers) runs the
//! parser in a helper process that confines itself before it reads a byte,
//! and passes bulk data to and from it through shared memory rather than the
//! socket. This crate owns both halves so the crate using it names no
//! platform:
//!
//! - [`confine`] — what a helper does at start-up: detach from inherited
//!   descriptors, die with its parent, cap its resources and confine itself
//!   to reading a list of paths, with no network and no `exec`.
//!   - Linux: Landlock (all ABI-5 filesystem rights, no TCP) and the seccomp
//!     parser policy from `sandbox-core`. A partially enforced ruleset is a
//!     failure.
//!   - macOS: a Seatbelt profile through `sandbox_init` that denies by
//!     default and allows reading the listed paths.
//!   - Web: a page has no processes; [`confine::confine`] reports
//!     [`confine::ConfineError::Unsupported`], and callers parse in process.
//! - `shm` (Unix only) — anonymous memory files.
//!   - Linux: sealed memfds, so a reader can map what it received as
//!     immutable bytes.
//!   - macOS: unlinked POSIX shared memory. It cannot be sealed, so
//!     [`shm::map_sealed`] takes a private copy instead of mapping, and sizes
//!     are rounded up to the page size, so readers carry lengths explicitly.

pub mod confine;
#[cfg(unix)]
pub mod shm;

/// A capability of this crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feature {
    /// A helper process can confine itself with [`confine::confine`].
    ProcessSandbox,
    /// Shared memory can be sealed so the receiver maps it immutably.
    SealedMemory,
}

pub use tilo_platform_caps::{Availability, availability};

impl tilo_platform_caps::Feature for Feature {
    /// The feature's availability on Linux, macOS and the web, in that order.
    fn targets(self) -> [Availability; 3] {
        use Availability::{Available, Unsupported};
        match self {
            Self::ProcessSandbox => [
                Available,
                Available,
                Unsupported("a page has no helper processes"),
            ],
            Self::SealedMemory => [
                Available,
                Unsupported("POSIX shared memory cannot be sealed; readers copy"),
                Unsupported("a page has no shared memory files"),
            ],
        }
    }

    /// Whether the feature works on the running target. Only Linux and
    /// macOS can confine helpers; every other target reads the web column.
    fn availability(self) -> Availability {
        const COLUMN: usize = if cfg!(target_os = "linux") {
            0
        } else if cfg!(target_os = "macos") {
            1
        } else {
            2
        };
        self.targets()[COLUMN]
    }
}
