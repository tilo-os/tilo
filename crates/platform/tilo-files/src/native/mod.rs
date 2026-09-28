//! Path-based operations for desktop-only code whose paths are a contract
//! with other programs: identity files, sockets' lock files, documents the
//! user named. They are the Linux and macOS backends of [`Dir`](crate::Dir).
//!
//! They compile on every target, like the rest of the crate; on the web the
//! standard library has no file system and every call returns an error.
//! Portable code opens a [`Dir`](crate::Dir) instead.

pub mod atomic;
pub mod lock;
pub mod private;
