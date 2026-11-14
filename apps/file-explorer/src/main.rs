#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

// A page links the app's library and mounts it in the shell; the file manager
// is the program a computer runs.
#[cfg(all(feature = "desktop", feature = "thin"))]
extern crate runtime_dylib;
#[cfg(all(feature = "desktop", feature = "runtime-engine"))]
extern crate tilo_archive_engine;
#[cfg(all(feature = "desktop", feature = "runtime-engine"))]
extern crate tilo_pdf_engine;

#[cfg(feature = "desktop")]
fn main() {
    tracing_subscriber::fmt::init();
    ui::framework::run::<file_explorer::FileExplorerShell>();
}

#[cfg(not(feature = "desktop"))]
fn main() {}
