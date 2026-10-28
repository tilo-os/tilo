#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

#[cfg(feature = "thin")]
extern crate runtime_dylib;
// The PDF and archive engines come from their engine libraries, not the
// runtime.
#[cfg(feature = "runtime-engine")]
extern crate tilo_archive_engine;
#[cfg(feature = "runtime-engine")]
extern crate tilo_pdf_engine;

fn main() {
    tracing_subscriber::fmt::init();
    ui::framework::run::<file_explorer::FileExplorerShell>();
}
