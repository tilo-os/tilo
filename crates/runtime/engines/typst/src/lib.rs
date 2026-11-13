//! Typst, as its own library in the runtime bundle (`libtilo_typst_engine.so`).
//!
//! Like `runtime-dylib` it re-exports what it provides, which records it
//! as provided by this library for the apps' `-C prefer-dynamic` link.
//! Outside the thin build it is empty.

#[cfg(feature = "thin")]
extern crate runtime_dylib;
// syntect and the grammars Typst's raw blocks use come from the highlight
// engine, which this library chains on (DT_NEEDED), not from a copy of its
// own.
#[cfg(feature = "thin")]
extern crate tilo_highlight_engine;

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
