//! Video Editor's device work, as its own library in the runtime bundle
//! (`libtilo_video_editor_engine.so`), opened the first time `so.tilo.video-editor`
//! asks for a still, a probe, a waveform, a render or a live play.
//!
//! The app runs sandboxed with no decoder and no renderer, so it asks this engine
//! through `engine.call`, `engine.start` and `engine.stream`; the process that
//! hosts it opens this library once and files it under the app's name, so no
//! other app is answered by it, and an app that edits no video never maps the
//! decoders it links.
//!
//! Outside the thin build this crate is an empty library.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

/// The entry the process opens this library for: the editor's engine.
#[cfg(feature = "thin")]
#[unsafe(no_mangle)]
pub extern "Rust" fn tilo_engine_entry() -> &'static tilo_engine_loader::EngineApi {
    &video_editor_engine::API
}

/// The engine's check binary's report: whether the self-check passed.
#[cfg(feature = "thin")]
#[unsafe(no_mangle)]
pub extern "C-unwind" fn tilo_video_editor_engine_check() -> bool {
    match video_editor_engine::self_check() {
        Ok(report) => {
            println!("tilo-video-editor-engine: {report}");
            true
        }
        Err(error) => {
            eprintln!("tilo-video-editor-engine: {error}");
            false
        }
    }
}
