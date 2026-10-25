#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

//! Thin binary entry point: Notes on the framework. All logic lives in the
//! library.
//!
//! Collaboration: launched from the shell, the compositor's broker is
//! reached through the `TILO_SVC_COLLAB_FD` descriptor the launcher injects —
//! nothing to configure. A raw process outside a session has no collaboration
//! service; the descriptor is inherited rather than discovered by path.

#[cfg(feature = "thin")]
extern crate runtime_dylib;
// The emoji catalogue comes from its engine library, not the tapp.
#[cfg(feature = "runtime-engine")]
extern crate tilo_emoji_engine;

ui::android_main!();

ui::main!(notes::NotesShell);
