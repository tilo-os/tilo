//! The wasm interpreter, as its own library in the runtime bundle
//! (`libtilo_wasm_engine.so`).
//!
//! Like `runtime-dylib` it re-exports what it provides, so the engines that
//! chain on it link the interpreter from this library. Outside the thin
//! build it is empty.

// Load the runtime explicitly: rustc ignores an `--extern` nothing names, and
// without it in the crate graph this library would link its own copy of every
// crate the runtime already provides.
#[cfg(feature = "thin")]
extern crate runtime_dylib;

#[cfg(feature = "thin")]
pub use tilo_theme_painter_wasm;
#[cfg(feature = "thin")]
pub use wasmi;

/// Compile and start an empty module, for the check binary.
#[cfg(feature = "thin")]
pub fn self_check() -> Result<String, String> {
    let engine = wasmi::Engine::default();
    let module =
        wasmi::Module::new(&engine, b"\0asm\x01\0\0\0").map_err(|error| error.to_string())?;
    let mut store = wasmi::Store::new(&engine, ());
    wasmi::Linker::<()>::new(&engine)
        .instantiate_and_start(&mut store, &module)
        .map_err(|error| error.to_string())?;
    Ok("compiled and started an empty module".to_owned())
}
