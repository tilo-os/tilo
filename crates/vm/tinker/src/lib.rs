//! Work layers: an unprivileged, discardable place to be root.
//!
//! None of this needs privilege. The host's root filesystem is never written,
//! so a wrecked layer cannot affect the next boot.

pub mod diff;
pub mod idmap;
pub mod session;
pub mod store;

pub use diff::{Change, ChangeKind, Diff};
pub use session::SessionSpec;
pub use store::{Layer, LayerKind, Store};
