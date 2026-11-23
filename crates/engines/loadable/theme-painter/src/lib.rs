//! A theme's painter sets loaded from wasm, as their own library in the runtime
//! bundle (`libtilo_theme_painter_engine.so`), on the wasm engine's interpreter.
//!
//! Like `runtime-dylib` it re-exports what it provides, which records it as
//! provided by this library for the apps' `-C prefer-dynamic` link. Outside
//! the thin build it is empty.

// Load the runtime explicitly, as thin apps do: rustc ignores an `--extern`
// nothing names, and without it in the crate graph this library would link
// its own copy of every crate the runtime already provides.
#[cfg(feature = "thin")]
extern crate runtime_dylib;
// The interpreter comes from the wasm engine, which the Typst engine loads too.
#[cfg(feature = "thin")]
extern crate tilo_wasm_engine;

#[cfg(feature = "thin")]
pub use tilo_theme_painter_wasm;

#[cfg(feature = "thin")]
use std::sync::Arc;
#[cfg(feature = "thin")]
use tilo_theme_sdk::{PainterEntry, PartPainter, PartTokens, Refusal};

/// The entry the toolkit opens (`tilo_theme_sdk::PAINTER_ENGINE_ENTRY`): the
/// interpreter, its type checked here against [`PainterEntry`].
#[cfg(feature = "thin")]
#[unsafe(no_mangle)]
pub extern "Rust" fn tilo_theme_painter_engine_entry(
    bytes: &[u8],
) -> Result<Arc<dyn PartPainter>, Refusal> {
    Ok(Arc::new(tilo_theme_painter_wasm::load(bytes)?))
}

#[cfg(feature = "thin")]
const _: PainterEntry = tilo_theme_painter_engine_entry;

/// The engine's self-check ([`self_check`]) for its check binary, by the
/// unmangled name the binary links, as the toolkit opens
/// [`tilo_theme_painter_engine_entry`]: nothing compiles against this library,
/// so it exports no Rust item. Prints the report; whether it passed.
///
/// # Safety
///
/// `module` must point at `len` readable bytes that stay live for the call.
#[cfg(feature = "thin")]
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn tilo_theme_painter_engine_check(
    module: *const u8,
    len: usize,
) -> bool {
    // SAFETY: the check binary passes a live slice of `len` bytes and reads
    // nothing of it after this returns.
    let module = if module.is_null() {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(module, len) }
    };
    match self_check(module) {
        Ok(report) => {
            println!("tilo-theme-painter-engine: {report}");
            true
        }
        Err(error) => {
            eprintln!("tilo-theme-painter-engine: {error}");
            false
        }
    }
}

/// Run the engine's own code: load `module` through the entry the toolkit
/// opens, read the parameters it names, and ask it to draw a part.
///
/// The check binary runs it through its unmangled entry, so a release proves
/// the library loads and runs against the runtime it ships with.
#[cfg(feature = "thin")]
pub fn self_check(module: &[u8]) -> Result<String, String> {
    use tilo_theme_sdk::kurbo::Size;
    use tilo_theme_sdk::{Part, PartState, States};

    let entry = tilo_theme_painter_engine_entry(module).map_err(|error| error.to_string())?;
    let params = entry.params();
    let painted = entry
        .paint(
            Part::ButtonContainer,
            Size::new(120.0, 40.0),
            PartState::settled(States::REST),
            1.0,
            &PartTokens::default(),
        )
        .ok_or("the set drew nothing")?;
    if painted.hit.is_none() {
        return Err("the set drew no shape for the pointer".into());
    }
    Ok(format!(
        "loaded a set of {} parameters and drew a part that takes the pointer",
        params.len()
    ))
}
