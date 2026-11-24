//! The Git app's device work, as its own library in the runtime bundle
//! (`libtilo_git_engine.so`), opened the first time `so.tilo.git-client` asks
//! it to read or change a repository.
//!
//! The app runs sandboxed and holds no repository, so it asks this engine
//! through `engine.call` and `engine.start`; the process that hosts it opens
//! this library once and files it under the app's name, so no other app is
//! answered by it. Reading and changing a repository answers now; a transfer to
//! or from a remote starts as the job the host names, which can be stopped.
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
pub extern "C-unwind" fn tilo_git_engine_check() -> bool {
    match engine::self_check() {
        Ok(report) => {
            println!("tilo-git-engine: {report}");
            true
        }
        Err(error) => {
            eprintln!("tilo-git-engine: {error}");
            false
        }
    }
}
