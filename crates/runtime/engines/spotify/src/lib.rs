//! Spotify playback, as an engine library beside the shared runtime
//! (`libtilo_spotify_engine.so`).
//!
//! Music is the one app that plays Spotify, and librespot with its decoders
//! and protocol stack is far heavier than that app's tapp may be. Inside
//! `libruntime_dylib.so` every app process mapped and relocated it. Here it
//! is a library of its own in the runtime bundle that only Music names
//! `DT_NEEDED`; the runtime keeps everything the two share.
//!
//! Like `runtime-dylib`, this crate carries no logic beyond re-exporting the
//! crate it provides, which records it as provided by this library for the
//! app's `-C prefer-dynamic` link. Outside the thin build (`thin`) it is
//! empty: see the manifest.

// Load the runtime explicitly, as thin apps do: rustc ignores an `--extern`
// nothing names, and without it in the crate graph this library would link
// its own copy of every crate the runtime already provides, which the app's
// link then refuses (each crate may show up once).
#[cfg(feature = "thin")]
extern crate runtime_dylib;

#[cfg(feature = "thin")]
pub use tilo_spotify;
