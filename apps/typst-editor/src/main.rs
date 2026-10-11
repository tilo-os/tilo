//! Typst — thin binary entry point. All logic lives in the library.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(feature = "thin")]
extern crate runtime_dylib;
// Typst comes from its engine library, not this executable.
#[cfg(feature = "runtime-engine")]
extern crate tilo_typst_engine;

ui::main!(typst_editor::TypstEditorShell);
