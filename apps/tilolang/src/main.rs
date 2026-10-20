//! The tilolang host: one Tilo app that runs every tilolang app package.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(feature = "thin")]
extern crate runtime_dylib;

fn main() {
    // A browser page mounts the definition itself; its binary is empty.
    #[cfg(not(target_arch = "wasm32"))]
    ui::launch::run_or_exit(&tilolang_app::launch_spec());
}
