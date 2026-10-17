//! Music — thin binary entry point. All logic lives in the library.
//!
//! The binary is the framework app: the live window, `--shots` scenarios,
//! `--headless` driving, a local file or `music://` link to open, and the
//! `--probe`/`--bench` commands (see `music::commands`).

#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

#[cfg(feature = "thin")]
extern crate runtime_dylib;
// Spotify playback and the YouTube client come from their engine libraries,
// not this executable.
#[cfg(feature = "runtime-engine")]
extern crate tilo_spotify_engine;
#[cfg(feature = "runtime-engine")]
extern crate tilo_youtube_engine;

ui::main!(music::MusicShell);
