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
    // The system runs this app's connector for an outside service in a process of its own,
    // with no window and no package of its own: the app's instances in the tilolang host reach
    // it. It needs no module, so this comes before the package is installed.
    if let Ok(origin) = std::env::var(tilolang_mail::CONNECTOR_ORIGIN_ENV) {
        return match run_connector(&origin) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("tilolang: {error}");
                std::process::ExitCode::from(2)
            }
        };
    }
    if let Err(error) = tilolang_app::install_from_env() {
        eprintln!("tilolang: {error}");
        return std::process::ExitCode::from(2);
    }
    // In a development session, later builds of the package take over while it runs.
    if let Some(bundle) = tilolang_app::installed_bundle() {
        tilolang_app::updates::start(&bundle.meta.app);
    }
    ui::launch::run_or_exit(&tilolang_app::launch_spec());
    std::process::ExitCode::SUCCESS
}

/// Run the connector of the origin `origin` this process was started as
/// (`TILO_TILOLANG_CONNECTOR`), on the door the launcher handed it, until the session closes
/// the door: the origin's own connector, from the crate that implements it.
#[cfg(not(target_arch = "wasm32"))]
fn run_connector(origin: &str) -> Result<(), String> {
    match origin {
        "imap" => tilolang_mail::run_connector(origin),
        "youtube" => {
            let service: std::sync::Arc<dyn tilolang_youtube::Service> =
                std::sync::Arc::new(tilolang_youtube::Live::new());
            tilolang_youtube::connector::run(&service)
        }
        other => Err(format!("there is no connector for the origin `{other}`")),
    }
}

/// A browser page mounts the definition itself; its binary is empty.
#[cfg(target_arch = "wasm32")]
fn main() {}
