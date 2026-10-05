//! Video Editor — thin binary entry point. All logic lives in the library.
//!
//! Every mode runs the editor on the app framework: the live window, and the
//! `--shots` / `--responsive-matrix` / `--native-audit` / `--headless`
//! captures, whose scenarios are [`video_editor::VideoEditor`]'s.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

ui::main!(video_editor::VideoEditor);
