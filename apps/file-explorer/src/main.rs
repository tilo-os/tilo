#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

#[cfg(feature = "thin")]
extern crate runtime_dylib;

fn main() {
    tracing_subscriber::fmt::init();
    ui::framework::run::<file_explorer::FileExplorerShell>();
}
