//! The part of browser continuity the UI toolkit and the runtime need: the
//! service-session vocabulary and the engine helper's launch boundary.
//!
//! It has no profile capture and no optional features. The shared app
//! runtime (`libruntime_dylib.so`) links this crate; capture, with `SQLite`,
//! stays in `browser-continuity`, linked only by the apps that capture.

pub mod engine_host;
pub mod sessions;

/// Fixed launcher-owned read-only projection location; no permission is
/// implied by this name or by metadata referring to it.
pub const PROJECTION_DIRECTORY: &str = "/run/tilo/browser-continuity";
