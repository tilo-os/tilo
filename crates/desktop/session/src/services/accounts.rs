//! The connections adding an account makes to the account's own servers, made here for Settings
//! ([`tilo_account_probe::connect`]).
//!
//! Settings' sandbox has no network. The launcher hands Settings alone a connection to this
//! service (`launcher::services::ACCOUNTS_SERVICE`), and each request on it is one account's
//! sign-in, one account's calendars, or one Nextcloud sign-in: the session reaches exactly the
//! servers the account names, and answers only what they said.

use std::io::ErrorKind;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_account_probe::LiveAccountTester;
use tilo_account_probe::connect::SignInPages;

use tilo_lock::MutexRecover as _;

/// How long a sign-in waits for the session's loop to ask for its page's window before Settings
/// opens the page itself.
const SHOW_WAIT: Duration = Duration::from_secs(5);

/// What keeps the service running: the socket, accepted on a thread of its own, and the sign-in
/// pages its connections leave for the session's loop.
pub(crate) struct Served {
    pages: Arc<Pages>,
}

impl Served {
    /// Open and close the sign-in pages asked for since the last pass, on the loop.
    pub(crate) fn follow(&self, apps: &crate::apps::Apps) {
        let asked = std::mem::take(&mut *self.pages.asks.lock_recover());
        for ask in asked {
            match ask {
                PageAsk::Show { page, url, reply } => {
                    let _ = reply.send(apps.show_sign_in(page, &url));
                }
                PageAsk::Over(page) => apps.sign_in_over(page),
            }
        }
    }
}

/// What a connection asks of a sign-in's page, for the loop.
enum PageAsk {
    Show {
        page: u64,
        url: String,
        reply: SyncSender<bool>,
    },
    Over(u64),
}

/// The sign-in pages the connections ask the loop to show and close.
struct Pages {
    asks: Mutex<Vec<PageAsk>>,
    next: AtomicU64,
    wake: tilo_service::Wake,
}

impl Pages {
    fn ask(&self, ask: PageAsk) {
        self.asks.lock_recover().push(ask);
        (self.wake)();
    }
}

impl SignInPages for Pages {
    fn show(&self, url: &str) -> Option<u64> {
        let page = self.next.fetch_add(1, Ordering::Relaxed);
        let (reply, answered) = std::sync::mpsc::sync_channel(1);
        self.ask(PageAsk::Show {
            page,
            url: url.to_owned(),
            reply,
        });
        if answered.recv_timeout(SHOW_WAIT) == Ok(true) {
            Some(page)
        } else {
            // Settings opens the page: a window the loop opens late is closed with the sign-in.
            self.ask(PageAsk::Over(page));
            None
        }
    }

    fn over(&self, page: u64) {
        self.ask(PageAsk::Over(page));
    }
}

/// Bind the service's socket under the launcher's services root and serve it.
///
/// # Errors
/// The services root or the socket cannot be made, or the acceptor cannot start.
#[allow(
    clippy::disallowed_methods,
    reason = "session helper: the accounts service accepts and serves on its own threads"
)]
pub(crate) fn serve(wake: tilo_service::Wake) -> Result<Served, String> {
    launcher::services::ensure_services_root()
        .map_err(|error| format!("create the services directory: {error}"))?;
    let path = launcher::services::socket_for(launcher::services::ACCOUNTS_SERVICE);
    if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|error| format!("remove the stale accounts socket: {error}"))?;
    }
    let listener = tilo_files::native::socket::bind(&path)
        .map_err(|error| format!("bind the accounts socket {}: {error}", path.display()))?;
    let pages = Arc::new(Pages {
        asks: Mutex::new(Vec::new()),
        next: AtomicU64::new(1),
        wake,
    });
    let accepting = Arc::clone(&pages);
    std::thread::Builder::new()
        .name("tilo-accounts".into())
        .spawn(move || accept(&listener, &accepting))
        .map_err(|error| format!("start the accounts service: {error}"))?;
    tracing::info!(path = %path.display(), "connecting to accounts' servers for Settings");
    Ok(Served { pages })
}

#[allow(
    clippy::disallowed_methods,
    reason = "session helper: the accounts service accepts and serves on its own threads"
)]
fn accept(listener: &UnixListener, pages: &Arc<Pages>) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let pages = Arc::clone(pages);
                if let Err(error) = std::thread::Builder::new()
                    .name("tilo-accounts-settings".into())
                    .spawn(move || serve_one(stream, &pages))
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

fn serve_one(stream: UnixStream, pages: &Pages) {
    tilo_account_probe::connect::serve(stream, &LiveAccountTester, pages);
}
