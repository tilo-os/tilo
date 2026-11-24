//! Painter sets loaded from wasm: a theme's own painters, run with nothing to
//! reach (`docs/architecture/theming.md`, "Part painters"). A module built
//! against `tilo_theme_sdk` (`export_painter!`) exports a buffer and one call,
//! and imports nothing; loading refuses one that imports anything, so a
//! painter has no clock, no I/O, no files and nothing to ask.
//!
//! The interpreter (`tilo-theme-painter-wasm`) is not in the shared runtime
//! every app maps: `lazy-painter` opens it from
//! `libtilo_theme_painter_engine.so` the first time a theme names a set,
//! and every other build links it here.

#[cfg(not(feature = "lazy-painter"))]
pub use tilo_theme_painter_wasm::{WasmPainter, load};
pub use tilo_theme_sdk::Refusal;

#[cfg(not(feature = "lazy-painter"))]
use std::sync::Arc;

#[cfg(not(feature = "lazy-painter"))]
use super::PainterSet;

/// The set `name` drawn by the painter module `bytes` ([`load`]), ready to register
/// ([`super::register_painter_set`]).
#[cfg(not(feature = "lazy-painter"))]
pub fn painter_set(name: &str, bytes: &[u8]) -> Result<PainterSet, Refusal> {
    painter_set_with(name, bytes, None)
}

/// The set `name` drawn by the painter module `bytes`, running the machine code the device's
/// daemon compiled for that module when `compiled` is given: a theme changed in conversation
/// starts on the new module's compiled code, without a restart. A set the daemon has not
/// compiled, and every browser, runs on the interpreter.
#[cfg(not(feature = "lazy-painter"))]
pub fn painter_set_with(
    name: &str,
    bytes: &[u8],
    compiled: Option<&[u8]>,
) -> Result<PainterSet, Refusal> {
    Ok(PainterSet::new(
        name,
        Arc::new(tilo_theme_painter_wasm::load_with(bytes, compiled)?),
    ))
}

#[cfg(feature = "lazy-painter")]
pub use lazy::{painter_set, painter_set_with};

#[cfg(feature = "lazy-painter")]
mod lazy {
    use std::sync::{Arc, OnceLock};

    use tilo_theme_sdk::{
        PAINTER_ENGINE_ENTRY, PAINTER_ENGINE_LIBRARY, PainterEntry, PartPainter, Refusal,
    };

    use crate::part_paint::PainterSet;

    fn entry() -> Option<PainterEntry> {
        static ENTRY: OnceLock<Option<PainterEntry>> = OnceLock::new();
        *ENTRY.get_or_init(|| {
            // SAFETY: the engine's entry is checked against `PainterEntry`
            // where it is defined (crates/engines/loadable/theme-painter), in
            // the same build.
            let entry = unsafe {
                tilo_engine_loader::entry::<PainterEntry>(
                    PAINTER_ENGINE_LIBRARY,
                    PAINTER_ENGINE_ENTRY,
                )
            };
            match entry {
                Ok(entry) => Some(entry),
                Err(error) => {
                    tracing::error!(%error, "a theme's painter sets are not loaded");
                    None
                }
            }
        })
    }

    /// The set `name` drawn by the painter module `bytes`, ready to register
    /// ([`super::register_painter_set`]).
    pub fn painter_set(name: &str, bytes: &[u8]) -> Result<PainterSet, Refusal> {
        painter_set_with(name, bytes, None)
    }

    /// The set `name` drawn by `bytes`, with the compiled entry the device's daemon made for it.
    /// The engine library takes a module's bytes; a compiled entry is used by a build that links
    /// the set itself, and the set here runs on the engine's interpreter either way.
    pub fn painter_set_with(
        name: &str,
        bytes: &[u8],
        compiled: Option<&[u8]>,
    ) -> Result<PainterSet, Refusal> {
        let _ = compiled;
        let entry = entry().ok_or_else(|| {
            Refusal::Unavailable(format!("{PAINTER_ENGINE_LIBRARY} did not open"))
        })?;
        let painter: Arc<dyn PartPainter> = entry(bytes)?;
        Ok(PainterSet::new(name, painter))
    }
}
