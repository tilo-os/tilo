//! The camera's, the scanner's and the document page's device work, as its own
//! library in the runtime bundle (`libtilo_camera_engine.so`), opened the first
//! time `so.tilo.camera` asks it for a picture, a code, a page or a clip.
//!
//! The app runs sandboxed with no camera, no scanner and no decoder, so it asks
//! this engine through `engine.call`, `engine.start` and `engine.stream`; the
//! process that hosts it opens this library once and files it under the app's
//! name, so no other app is answered by it. What the engine writes back is kept
//! in the app's content store and named by its address, which the app shows as
//! the file it is; the frames of a recording or a viewfinder reach the app's
//! window through the host as they arrive.
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
pub extern "C-unwind" fn tilo_camera_engine_check() -> bool {
    match engine::self_check() {
        Ok(report) => {
            println!("tilo-camera-engine: {report}");
            true
        }
        Err(error) => {
            eprintln!("tilo-camera-engine: {error}");
            false
        }
    }
}
