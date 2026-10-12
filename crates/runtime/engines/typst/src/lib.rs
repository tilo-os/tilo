//! Typst, as an engine library beside the shared runtime
//! (`libtilo_typst_engine.so`).
//!
//! Typst Editor is the one app that typesets, and Typst with its bundled
//! fonts is far heavier than that app's tapp may be. Inside
//! `libruntime_dylib.so` every app process mapped and relocated it.
//! Here it is a library of its own in the runtime bundle that only Typst
//! Editor names `DT_NEEDED`; the runtime keeps everything the two share.
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
pub use comemo;
#[cfg(feature = "thin")]
pub use typst;
#[cfg(feature = "thin")]
pub use typst_assets;
#[cfg(feature = "thin")]
pub use typst_layout;
#[cfg(feature = "thin")]
pub use typst_pdf;
#[cfg(feature = "thin")]
pub use typst_render;

/// Parse the engine's bundled Typst fonts without making the app retain
/// `typst-assets`' multi-megabyte static font table in its own executable.
///
/// Returning the concrete vector keeps iteration over the embedded byte table
/// on this side of the library boundary; the app receives only parsed fonts.
#[cfg(feature = "thin")]
pub fn typst_fonts() -> Vec<typst::text::Font> {
    typst_assets::fonts()
        .flat_map(|data| typst::text::Font::iter(typst::foundations::Bytes::new(data)))
        .collect()
}
