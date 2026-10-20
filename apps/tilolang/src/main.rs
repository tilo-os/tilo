//! The tilolang runtime: runs the one installed tilolang app the launcher started it for.
//!
//! The launcher executes this binary from the tilolang runtime bundle with `TILO_PACKAGE_ID`
//! and `TILO_APP_MODULE` set (see `tilolang_app`). The language's command line (`tilolang build`,
//! `tilolang run`) is a separate tool, `crates/lang/tilolang-cli`.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(feature = "thin")]
extern crate runtime_dylib;

#[cfg(not(target_arch = "wasm32"))]
fn main() -> std::process::ExitCode {
    if let Err(error) = tilolang_app::install_from_env() {
        eprintln!("tilolang: {error}");
        return std::process::ExitCode::from(2);
    }
    ui::launch::run_or_exit(&tilolang_app::launch_spec());
    std::process::ExitCode::SUCCESS
}

/// A browser page mounts the definition itself; its binary is empty.
#[cfg(target_arch = "wasm32")]
fn main() {}
