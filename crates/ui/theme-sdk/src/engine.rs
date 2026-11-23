//! The interpreter a painter set loaded from wasm runs on: what it refuses,
//! the type it answers in, and where the toolkit finds it.
//!
//! The interpreter is not in the shared runtime every app maps. It is an
//! engine library beside it (`crates/engines/loadable/theme-painter`), which
//! the toolkit opens the first time a theme names a set it draws
//! (`docs/architecture/theming.md`, "Part painters").

use std::sync::Arc;

use crate::PartPainter;

/// The interpreter's library in the runtime bundle.
pub const PAINTER_ENGINE_LIBRARY: &str = "libtilo_theme_painter_engine.so";

/// Its entry: [`PainterEntry`].
pub const PAINTER_ENGINE_ENTRY: &str = "tilo_theme_painter_engine_entry";

/// Load the painter module `bytes`: the set it draws, or why it is not one.
pub type PainterEntry = fn(&[u8]) -> Result<Arc<dyn PartPainter>, Refusal>;

/// Why a module is not a painter set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Not a valid wasm module.
    Invalid(String),
    /// It imports something; a painter may import nothing.
    Imports(String),
    /// It lacks `memory`, `tilo_theme_buffer` or `tilo_theme_call` of the SDK's types.
    Exports(String),
    /// Its start-up trapped or ran past its budget.
    Start(String),
    /// Its parameters are not a well-formed list.
    Params,
    /// This runtime carries no painter interpreter.
    Unavailable(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(why) => write!(f, "not a wasm module: {why}"),
            Self::Imports(name) => write!(f, "a painter imports nothing, this one imports {name}"),
            Self::Exports(name) => write!(f, "a painter exports {name}"),
            Self::Start(why) => write!(f, "the painter's start-up failed: {why}"),
            Self::Params => f.write_str("the painter's parameters are malformed"),
            Self::Unavailable(why) => write!(f, "the painter interpreter is not available: {why}"),
        }
    }
}

impl std::error::Error for Refusal {}
