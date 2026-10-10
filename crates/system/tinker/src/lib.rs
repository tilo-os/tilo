//! Work layers: an unprivileged, discardable place to be root.
//!
//! A *layer* is an overlayfs upper directory plus a small metadata file. You
//! enter it with a shell that is uid 0 inside its own user namespace, install
//! whatever you like with the base image's own package manager, and then decide
//! afterwards whether the result was worth keeping.
//!
//! The overlay upper directory *is* the diff. Nothing is snapshotted and no
//! change journal is maintained: to see what an install changed, walk the upper directory and compare
//! each entry against the base. Deletions are visible as overlayfs whiteouts,
//! so even removals are recoverable by deleting the whiteout.
//!
//! None of this needs privilege. The host's root filesystem is never written,
//! so a wrecked layer cannot affect the next boot.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod diff;
pub mod idmap;
pub mod session;
pub mod store;

pub use diff::{Change, ChangeKind, Diff};
pub use session::SessionSpec;
pub use store::{Layer, LayerKind, Store};
