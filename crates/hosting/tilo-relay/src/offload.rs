//! The content and media seams an offloaded job's two devices meet on.
//!
//! An offloadable action is a pure content transform, so an invocation of one
//! travels as the content it reads: the device that asks names each input by
//! digest and keeps its outputs by the addresses the answer names. Those
//! bytes are the workspace's own content ([`WorkspaceMedia`], reached only through
//! the invoking app's scope in its workspace), and a device that already
//! holds an input says so ([`ReplicaContent`]). Nothing here chooses where an
//! invocation runs: the offer round does, and the device that wins decides it
//! through its own dispatcher.

pub use crate::offload_content::{BindError, ReplicaContent};
pub use crate::offload_media::{MediaFuture, WorkspaceMedia};
