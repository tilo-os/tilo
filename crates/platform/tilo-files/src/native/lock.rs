//! Advisory whole-file locks with one guard type, [`FileLock`]: the native
//! backend of [`Dir::lock`](crate::Dir::lock).
//!
//! The implementation lives in the `advisory-lock` crate; this module is that
//! crate under the platform layer's path.

pub use advisory_lock::*;
