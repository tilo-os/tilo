//! The Browser's device work, as its own library in the runtime bundle
//! (`libtilo_browser_engine.so`), opened in the Browser's own process the first time it
//! downloads a file.
//!
//! A file a page offers is fetched here as a job of the app's, not by the page's engine, so the
//! download goes on when the page's window closes, and the job's record says how far it has got.
//!
//! Outside the thin build this crate is an empty library; its tests build the download itself.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

#[cfg(any(feature = "thin", test))]
mod download;

#[cfg(feature = "thin")]
#[path = "engine.rs"]
mod engine;

/// The engine's check binary's report: whether the self-check passed.
#[cfg(feature = "thin")]
#[unsafe(no_mangle)]
pub extern "C-unwind" fn tilo_browser_engine_check() -> bool {
    match engine::self_check() {
        Ok(report) => {
            println!("tilo-browser-engine: {report}");
            true
        }
        Err(error) => {
            eprintln!("tilo-browser-engine: {error}");
            false
        }
    }
}
