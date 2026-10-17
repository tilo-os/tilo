//! App-facing reviewed source capture and native import integration tests.

pub use browser_continuity::sources::*;

#[cfg(all(test, unix))]
mod native;
