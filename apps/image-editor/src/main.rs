//! Image Editor — thin binary entry point. All logic lives in the library.
//!
//! A file named on the command line opens straight into the editor; the
//! window, `--shots` with its scenarios and every other capture run on the
//! app framework.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(feature = "thin")]
extern crate runtime_dylib;

fn main() {
    if image_editor::opens_a_file() {
        ui::launch::run_or_exit(&image_editor::file_launch_spec());
        return;
    }
    ui::framework::run::<image_editor::ImageEditorShell>();
}
