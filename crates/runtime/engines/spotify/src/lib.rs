//! Spotify playback, as its own library in the runtime bundle
//! (`libtilo_spotify_engine.so`).
//!
//! Like `runtime-dylib` it re-exports what it provides, which records it
//! as provided by this library for the apps' `-C prefer-dynamic` link.
//! Outside the thin build it is empty.

#[cfg(feature = "thin")]
extern crate runtime_dylib;
// What librespot shares with rustypipe and the runtime lacks (rand 0.9,
// sha1 0.10) comes from the YouTube engine, which Music loads beside this
// one; without it both libraries would carry those crates and Music's link
// would refuse the second copy.
#[cfg(feature = "thin")]
extern crate tilo_accounts_engine;
#[cfg(feature = "thin")]
extern crate tilo_youtube_engine;

#[cfg(feature = "thin")]
pub use tilo_spotify;
