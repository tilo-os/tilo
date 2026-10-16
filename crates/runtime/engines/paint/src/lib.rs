//! Canvas's painting engines, as an engine library beside the shared runtime
//! (`libtilo_paint_engine.so`).
//!
//! Canvas is the one app that paints: the wet-media simulation (watercolour,
//! oil) and the subject matte it refines (`tilo-subject` over
//! `tilo-segment`). Inside `libruntime_dylib.so` every app process mapped and
//! relocated them. Here they are a library of their own in the runtime bundle
//! that only Canvas names `DT_NEEDED`; the runtime keeps everything the two
//! share.
//!
//! Like `runtime-dylib`, this crate carries no logic beyond re-exporting the
//! crates it provides, which records them as provided by this library for the
//! app's `-C prefer-dynamic` link. Outside the thin build (`thin`) it is
//! empty: see the manifest.

// Load the runtime explicitly, as thin apps do: rustc ignores an `--extern`
// nothing names, and without it in the crate graph this library would link
// its own copy of every crate the runtime already provides, which the app's
// link then refuses (each crate may show up once).
#[cfg(feature = "thin")]
extern crate runtime_dylib;

#[cfg(feature = "thin")]
pub use tilo_subject;
#[cfg(feature = "thin")]
pub use tilo_wet_media;
