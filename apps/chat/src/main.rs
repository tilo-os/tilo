//! Chat — thin binary entry point. All logic lives in the library.
//!
//! The chat window, or a standard launch mode: `--shots PATH` with
//! `--scenario NAME` captures one of the named scenarios of
//! [`chat::ChatShell`], `--shots-dir DIR` every one.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

ui::main!(chat::ChatShell, init = chat::enable_standalone_models);
