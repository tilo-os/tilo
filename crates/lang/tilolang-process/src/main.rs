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
    tilolang_process::engines::register_all();
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
        code_engine::held::KIND => run_held(
            origin,
            std::sync::Arc::new(code_engine::held::LanguageServers),
        ),
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

/// Run this process as the app's held connections of `kind` (`tilolang_tilo::held`): each
/// instance the daemon hands over on the socket the launcher gave it reaches them, and a vault
/// item a connection needs is released into this process alone, through the connection to the
/// device's dispatch the daemon hands first. The process ends when the daemon closes its socket,
/// or once nothing is open and no instance is attached.
#[cfg(all(unix, feature = "app-launch"))]
fn run_held(kind: &str, code: std::sync::Arc<dyn tilolang_tilo::held::Kind>) -> Result<(), String> {
    use runtime_portal::origin_connector::{self, Handed};
    use std::sync::Arc;
    let socket = origin_connector::take_socket()
        .ok_or_else(|| format!("the launcher handed the {kind} connections no socket"))?;
    let mut release: Arc<dyn tilolang_tilo::held::Release> =
        Arc::new(tilolang_tilo::held::Releases);
    let mut first = None;
    match origin_connector::receive(&socket) {
        Ok(Some(Handed::Actions(link))) => {
            release = Arc::new(SignInRelease(std::sync::Mutex::new(
                tilo_remote_actions::signing_in::SigningIn::over(link)
                    .map_err(|error| format!("the {kind} connections' dispatch: {error}"))?,
            )));
        }
        Ok(Some(Handed::Reader(reader))) => first = Some(reader),
        Ok(None) => return Ok(()),
        Err(error) => return Err(format!("the {kind} connections' socket: {error}")),
    }
    // It holds its socket and its dispatch: the device that started it may count it as up.
    if let Err(error) = origin_connector::say_ready(&socket) {
        tracing::warn!(%error, "the held connections could not say they are up");
    }
    // Nothing held for anyone ends the process: its socket is shut, so the daemon starts it
    // again when an instance needs it, and the process stops taking instances and returns.
    let ending = std::os::unix::net::UnixStream::from(
        std::os::fd::AsFd::as_fd(&socket)
            .try_clone_to_owned()
            .map_err(|error| format!("the {kind} connections' socket: {error}"))?,
    );
    let rest = origin_connector::attachments(socket, |failed| {
        if let Some(error) = failed {
            tracing::error!(%error, "the held connections' socket failed for good");
        }
    });
    tilolang_tilo::held::serve(
        kind,
        code,
        release,
        first.into_iter().chain(rest),
        &(Arc::new(move || {
            let _ = ending.shutdown(std::net::Shutdown::Both);
        }) as Arc<dyn Fn() + Send + Sync>),
    );
    Ok(())
}

/// The workspace's accounts, as the service that keeps them publishes their actions.
#[cfg(all(unix, feature = "app-launch"))]
mod workspace_accounts {
    tilo_resource::bindings!(
        provider = "tilo.accounts",
        surface = "../../core/workspace-records/accounts-surface.json"
    );
}

/// A vault item released into this process over the connection to the device's dispatch: the
/// login whose entry the item is, released to this process alone.
#[cfg(all(unix, feature = "app-launch"))]
struct SignInRelease(std::sync::Mutex<tilo_remote_actions::signing_in::SigningIn>);

#[cfg(all(unix, feature = "app-launch"))]
impl tilolang_tilo::held::Release for SignInRelease {
    fn release(&self, item: &str) -> Result<tilolang_tilo::held::Released, String> {
        let mut signing = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let told = signing
            .sign_ins::<workspace_accounts::MailSignIns>(workspace_accounts::accounts::sign_ins())
            .map_err(|error| format!("the device's dispatch: {error}"))?;
        // The account that names the item says which one it is: the login the dispatch lists
        // with an account, whose entry is this item.
        let login = told
            .sign_ins
            .iter()
            .find(|login| login.id == item && login.account.is_some())
            .ok_or_else(|| format!("no account of this workspace names {item}"))?;
        let account = login
            .account
            .as_ref()
            .map(|account| account.id.clone())
            .unwrap_or_default();
        let secret = signing
            .secret(&account, &login.id)
            .map_err(|error| format!("the device's dispatch: {error}"))?;
        drop(signing);
        let released: tilolang_tilo::held::Released = Box::new(zeroize::Zeroizing::new(secret));
        Ok(released)
    }
}
