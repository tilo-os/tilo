//! Crash-safe file replacement: the write path of
//! [`Dir::write_atomic`](crate::Dir::write_atomic) on Linux and macOS.
//!
//! The implementation, with its Unix and NTFS publication protocols, lives in
//! the `atomic-file` crate; this module is that crate under the platform
//! layer's path.

pub use atomic_file::*;
