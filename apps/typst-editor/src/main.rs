//! Typst — thin binary entry point. All logic lives in the library.
//!
//! The window runs on the app framework (`TypstEditorShell`), which also
//! renders `--shots` captures: a plain capture shows the bundled documents,
//! and `--scenario NAME` stages one of the scenarios the app declares.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    ui::framework::run::<typst_editor::TypstEditorShell>();
}
