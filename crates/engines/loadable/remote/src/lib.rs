//! Remote's device work as its own library in the runtime bundle (`libtilo_remote_engine.so`),
//! opened the first time `so.tilo.remote` opens an SSH shell or a VNC desktop.
//!
//! The app runs sandboxed with no sockets of its own, so it asks this engine through
//! `engine.stream`: an SSH shell whose grid is the shared `ui.Terminal`'s (the session this
//! engine keeps is the toolkit's own, registered where its window reads it), or a VNC desktop
//! whose newest framebuffer the app shows by `tilo-live://<job>` and whose pointer and keys the
//! app's picture sends to the destination this engine provides for that job. A file moves over
//! an SSH session as a job (`put`, `fetch`), in the app's own process, under the app's network
//! grant.
//!
//! Outside the thin build this crate is an empty library.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

#[cfg(feature = "thin")]
#[path = "engine.rs"]
mod engine;

/// The engine's check binary's report: whether the self-check passed.
#[cfg(feature = "thin")]
#[unsafe(no_mangle)]
pub extern "C-unwind" fn tilo_remote_engine_check() -> bool {
    match engine::self_check() {
        Ok(report) => {
            println!("tilo-remote-engine: {report}");
            true
        }
        Err(error) => {
            eprintln!("tilo-remote-engine: {error}");
            false
        }
    }
}
