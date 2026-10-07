//! Code — thin binary entry point. All logic lives in the library.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(feature = "thin")]
extern crate runtime_dylib;

ui::main!(code_editor::CodeEditorShell);
