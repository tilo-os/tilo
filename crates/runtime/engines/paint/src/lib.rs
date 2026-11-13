//! Canvas's painting engines, as its own library in the runtime bundle
//! (`libtilo_paint_engine.so`).
//!
//! Like `runtime-dylib` it re-exports what it provides, which records it
//! as provided by this library for the apps' `-C prefer-dynamic` link.
//! Outside the thin build it is empty.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

#[cfg(feature = "thin")]
pub use tilo_subject;
#[cfg(feature = "thin")]
pub use tilo_wet_media;
