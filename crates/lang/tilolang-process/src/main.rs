//! `tilolang-process`: the one binary of the tilolang runtime bundle, in the role it was started
//! for (`tilolang_process`):
//!
//! - with its storage (`TILO_TILOLANG_PROCESS_STORAGE`) and a process link
//!   (`TILO_SVC_PROCESS_FD`): the host of a workspace's instances of an app, or of a task's
//!   programs, which the device starts;
//! - with an origin (`TILO_TILOLANG_CONNECTOR`): that origin's connector, which the system runs
//!   for an app whose types hold an outside service's resources;
//! - with a page link (`TILO_TILOLANG_PAGE`): a window a device shows on a web page;
//! - otherwise: the windows of the package the launcher names with `TILO_PACKAGE_ID` and
//!   `TILO_APP_MODULE`. The language's command line (`tilolang build`, `tilolang run`) is a
//!   separate tool, `crates/lang/tilolang-cli`.
//!
//! On Windows the process link is a pipe, and what it carries are handles the device puts into
//! this process; an origin's connector and a page's window are started on Unix alone.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(feature = "thin")]
extern crate runtime_dylib;

mod host;

fn main() -> std::process::ExitCode {
    // The engines this bundle answers its apps with, registered before any of them asks.
    tilo_disk_usage::register();
    publisher_engine::register();
    // Maps' kept areas are this device's pictures, which a window may already have drawn.
    #[cfg(feature = "app-launch")]
    tilo_maps_engine::register();
    // A host is started on its process link with the storage of its workspace and app
    // (`spawn_host` of the device). A window has a process link too, the one its session opens
    // its later windows on, but no storage; a page window has storage but no process link.
    #[cfg(feature = "app-launch")]
    {
        use runtime_portal::process_link::{FD_ENV, STORAGE_ENV};
        if std::env::var_os(FD_ENV).is_none() || std::env::var_os(STORAGE_ENV).is_none() {
            return launched();
        }
    }
    host::main()
}

/// The role an app's launch starts: the connector of the origin it names, or the windows of its
/// package.
#[cfg(feature = "app-launch")]
fn launched() -> std::process::ExitCode {
    // The system runs this app's connector for an outside service in a process of its own,
    // with no window and no package of its own: the app's instances in the tilolang host reach
    // it. It needs no module, so this comes before the package is installed. It has no window
    // to log beside, so its own log is all a failure in it says: the session reads what it
    // writes to stderr.
    #[cfg(unix)]
    if let Ok(origin) = std::env::var(tilolang_mail::CONNECTOR_ORIGIN_ENV) {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new(
                tilo_startup_metrics::log_directives(std::env::var("RUST_LOG").ok().as_deref()),
            ))
            .with_writer(std::io::stderr)
            .init();
        return match run_connector(&origin) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("tilolang: {error}");
                std::process::ExitCode::from(2)
            }
        };
    }
    // A window a device shows on a web page draws nothing: it answers the device on its page
    // link, and needs no package of its own, since its instance runs in the app's process.
    #[cfg(unix)]
    if let Ok(named) = std::env::var(runtime_portal::process_link::PAGE_ENV) {
        return match tilolang_process::window::shown_on_page::serve_from_env(&named) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("tilolang: the page's window: {error}");
                std::process::ExitCode::from(2)
            }
        };
    }
    if let Err(error) = tilolang_process::window::install_from_env() {
        eprintln!("tilolang: {error}");
        return std::process::ExitCode::from(2);
    }
    ui::launch::run_or_exit(&tilolang_process::window::launch_spec());
    std::process::ExitCode::SUCCESS
}

/// Run the connector of the origin `origin` this process was started as
/// (`TILO_TILOLANG_CONNECTOR`), on the API the launcher handed it, until the session closes
/// the API: the origin's own connector, from the crate that implements it, or the library that
/// holds it beside this binary.
#[cfg(all(unix, feature = "app-launch"))]
fn run_connector(origin: &str) -> Result<(), String> {
    match origin {
        "imap" => tilolang_mail::run_connector(origin),
        "caldav" => tilolang_calendar::run_connector(origin),
        // YouTube's client and the connector loop over it are their own library in the bundle:
        // this process is started for that role alone, and opens it here. Every other launch of
        // this binary maps none of it.
        "youtube" => connector_of("libtilo_youtube_engine.so", origin),
        "music" => connector_of("libtilo_music_engine.so", origin),
        other => Err(format!("there is no connector for the origin `{other}`")),
    }
}
/// Run the connector library `library` (its file name, as in the runtime bundle) serves, for the
/// origin `origin` this process was started as.
#[cfg(all(unix, feature = "app-launch"))]
fn connector_of(library: &str, origin: &str) -> Result<(), String> {
    // SAFETY: the library is one this build ships, built with this runtime: a connector
    // exporting the one entry whose `ConnectorApi` shares this build's types.
    let connector = unsafe { tilo_engine_loader::connector(library) }
        .map_err(|why| format!("the {origin} connector did not open: {why}"))?;
    if connector.origin != origin {
        return Err(format!(
            "{library} serves `{}`, not the origin `{origin}` this process was started as",
            connector.origin
        ));
    }
    (connector.run)()
}
