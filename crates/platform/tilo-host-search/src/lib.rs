//! A host search result, handed from the app the operating system woke to
//! the Tilo runtime that indexed it.
//!
//! Some host searches continue a chosen result in the app of the bundle
//! that owns it rather than in the runtime daemon: Spotlight wakes
//! `Tilo.app`'s bundle executable. [`forward_to_runtime`] takes the result
//! there and [`open`] hands its entry to the daemon over the runtime's native
//! socket; the daemon re-resolves the entry and opens it through its action
//! host, as it does for results it serves itself. The entry is opaque here.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(unix)]
mod native;

#[cfg(unix)]
pub use native::{OPEN, open};

/// Hand every host search result this app is asked to continue to the
/// runtime it attached to (`TILO_RUNTIME_PROFILE`). Call once, on the main
/// thread, after the event loop exists and before the app finishes
/// launching, so the result that launched it arrives too. Does nothing on a
/// platform whose search asks the runtime itself, or in an app attached to
/// no runtime.
pub fn forward_to_runtime() {
    #[cfg(target_os = "macos")]
    macos::forward_to_runtime();
}
