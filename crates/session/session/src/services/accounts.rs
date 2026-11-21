//! The connections adding an account makes to the account's own servers, made here for Settings
//! ([`tilo_account_probe::connect`]).
//!
//! Settings' sandbox has no network. The launcher hands Settings alone a connection to this
//! service (`launcher::services::ACCOUNTS_SERVICE`), and each request on it is one account's
//! sign-in, one account's calendars, or one Nextcloud sign-in: the session reaches exactly the
//! servers the account names, and answers only what they said.

use std::io::ErrorKind;
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::Duration;

use tilo_account_probe::LiveAccountTester;

/// What keeps the service running: the socket, accepted on a thread of its own.
pub(crate) struct Served;

/// Bind the service's socket under the launcher's services root and serve it.
///
/// # Errors
/// The services root or the socket cannot be made, or the acceptor cannot start.
#[allow(
    clippy::disallowed_methods,
    reason = "session helper: the accounts service accepts and serves on its own threads"
)]
pub(crate) fn serve() -> Result<Served, String> {
    launcher::services::ensure_services_root()
        .map_err(|error| format!("create the services directory: {error}"))?;
    let path = launcher::services::socket_for(launcher::services::ACCOUNTS_SERVICE);
    if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|error| format!("remove the stale accounts socket: {error}"))?;
    }
    let listener = tilo_files::native::socket::bind(&path)
        .map_err(|error| format!("bind the accounts socket {}: {error}", path.display()))?;
    std::thread::Builder::new()
        .name("tilo-accounts".into())
        .spawn(move || accept(&listener))
        .map_err(|error| format!("start the accounts service: {error}"))?;
    tracing::info!(path = %path.display(), "connecting to accounts' servers for Settings");
    Ok(Served)
}

#[allow(
    clippy::disallowed_methods,
    reason = "session helper: the accounts service accepts and serves on its own threads"
)]
fn accept(listener: &UnixListener) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(error) = std::thread::Builder::new()
                    .name("tilo-accounts-settings".into())
                    .spawn(move || serve_one(stream))
                {
                    tracing::warn!(%error, "an accounts connection could not be served");
                }
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => {
                tracing::warn!(%error, "the accounts service's accept failed");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn serve_one(stream: UnixStream) {
    tilo_account_probe::connect::serve(stream, &LiveAccountTester);
}
