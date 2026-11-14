#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

// A page links the app's library and mounts it in the desk; the file manager
// is the program a computer runs.
#[cfg(not(target_arch = "wasm32"))]
#[cfg(feature = "thin")]
extern crate runtime_dylib;
#[cfg(not(target_arch = "wasm32"))]
#[cfg(feature = "runtime-engine")]
extern crate tilo_archive_engine;
#[cfg(not(target_arch = "wasm32"))]
#[cfg(feature = "runtime-engine")]
extern crate tilo_pdf_engine;

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    tracing_subscriber::fmt::init();
    ui::framework::run::<file_explorer::FileExplorerShell>();
}

#[cfg(target_arch = "wasm32")]
fn main() {}
