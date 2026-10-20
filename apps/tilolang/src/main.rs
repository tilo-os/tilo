//! The tilolang host: one Tilo app that runs every tilolang app package.
//!
//! Started with one of the `tilolang` commands (`check`, `build`, `fmt`, `ids`, `run`), the same
//! binary is the language's command line, and `run` opens a package under development in a
//! window of its own (`tilolang_app::dev`). Anything else launches the app.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(feature = "thin")]
extern crate runtime_dylib;

#[cfg(not(target_arch = "wasm32"))]
fn main() -> std::process::ExitCode {
    use tilolang_tilo::tilolang::cli;

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args
        .first()
        .is_some_and(|command| cli::COMMANDS.contains(&command.as_str()))
    {
        return cli::main(&args, Some(&tilolang_app::dev::run));
    }
    ui::launch::run_or_exit(&ui::framework::definition_launch_spec(
        tilolang_app::definition(),
    ));
    std::process::ExitCode::SUCCESS
}

/// A browser page mounts the definition itself; its binary is empty.
#[cfg(target_arch = "wasm32")]
fn main() {}
