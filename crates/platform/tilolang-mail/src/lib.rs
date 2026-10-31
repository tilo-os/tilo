//! Mail servers as an outside origin of tilolang packages: the IMAP and SMTP connector over
//! native Mail's protocol code ([`connector`]) and the origin that keeps a package's `imap`
//! types in step with it ([`origin`]).
//!
//! The accounts are the workspace's settings' (the account store Settings writes), and their
//! credentials stay in custody, read by the connector in native code: package code never sees
//! them. Where an instance runs in a tilolang host, which holds no network and no credential,
//! the connector runs in a process of its own that the system starts for the app ([`serve`])
//! and the instance's origin reaches it over a socket ([`remote`], [`wire`]).

pub mod connector;
#[cfg(feature = "engine")]
pub mod engine;
pub mod origin;
pub mod remote;
#[cfg(feature = "engine")]
pub mod serve;
pub mod wire;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
#[cfg(feature = "engine")]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(feature = "engine")]
use engine::{AccountSource, EngineConnector};
use origin::ImapOrigin;

/// The settings' mail accounts under the profile's configuration root.
#[cfg(feature = "engine")]
#[must_use]
pub fn settings_accounts(config_root: Option<std::path::PathBuf>) -> AccountSource {
    Box::new(move || {
        config_root
            .as_deref()
            .map(mail::account::AccountStore::load_in)
            .map(|store| store.accounts.into_iter().map(|a| a.config).collect())
            .unwrap_or_default()
    })
}

/// The origin over the live engine, waking the window's frame loop with `wake` when a server
/// answers.
#[cfg(feature = "engine")]
#[must_use]
pub fn live_origin(
    accounts: AccountSource,
    wake: tilolang_tilo::background::Waker,
) -> Rc<RefCell<ImapOrigin>> {
    let content = tilo_content::ContentStore::memory();
    let woken = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&woken);
    let wake: mail::engine::Wake = Arc::new(move || {
        flag.store(true, Ordering::Release);
        wake();
    });
    let reconnected = Arc::new(AtomicBool::new(false));
    follow_the_network(Arc::clone(&reconnected), Arc::clone(&wake));
    let (requests, events) = mail::engine::spawn(content, wake);
    Rc::new(RefCell::new(
        ImapOrigin::new(
            Box::new(EngineConnector::new(accounts, requests, events)),
            woken,
        )
        .reconnecting_on(reconnected),
    ))
}

/// The origin of an instance in a tilolang host, over the mail connector process at the far end
/// of `stream`, waking the instance with `wake` when it says something.
///
/// # Errors
/// The socket cannot be shared with the thread that reads it.
pub fn remote_origin(
    stream: std::os::unix::net::UnixStream,
    wake: tilolang_tilo::background::Waker,
) -> std::io::Result<Rc<RefCell<ImapOrigin>>> {
    let woken = Arc::new(AtomicBool::new(true));
    let reconnected = Arc::new(AtomicBool::new(false));
    let connector =
        remote::RemoteConnector::new(stream, Arc::clone(&woken), wake, Arc::clone(&reconnected))?;
    Ok(Rc::new(RefCell::new(
        ImapOrigin::new(Box::new(connector), woken).reconnecting_on(reconnected),
    )))
}

/// Set `reconnected` and wake whenever the device comes online again: what could not reach
/// its server then is tried again. An event of the platform's network status, never a timer.
#[cfg(feature = "engine")]
pub(crate) fn follow_the_network(reconnected: Arc<AtomicBool>, wake: mail::engine::Wake) {
    let spawned = std::thread::Builder::new()
        .name("mail-network".into())
        .spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread().build() else {
                return;
            };
            runtime.block_on(async move {
                let mut network = tilo_net::status::watch();
                let mut online = network.last().online();
                loop {
                    let now = network.changed().await.online();
                    if now && !online {
                        reconnected.store(true, Ordering::Release);
                        wake();
                    }
                    online = now;
                }
            });
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "mail does not follow the network: it retries only when asked");
    }
}

/// The variable the launcher names the origin a connector process connects with.
#[cfg(feature = "engine")]
pub const CONNECTOR_ORIGIN_ENV: &str = runtime_portal::origin_connector::ORIGIN_ENV;

/// Run the connector for `origin` this process was started as (`TILO_TILOLANG_CONNECTOR`), on
/// the door the launcher handed it, for the settings' accounts, until the session closes the
/// door.
///
/// # Errors
/// An origin this crate has no connector for, or no door to take instances from.
#[cfg(feature = "engine")]
pub fn run_connector(origin: &str) -> Result<(), String> {
    if origin != "imap" {
        return Err(format!("there is no connector for the origin `{origin}`"));
    }
    let door = runtime_portal::origin_connector::take_door()
        .ok_or_else(|| "the launcher handed the connector no door".to_owned())?;
    let attachments = std::iter::from_fn(move || {
        match runtime_portal::origin_connector::receive_attachment(&door) {
            Ok(Some(connection)) => Some(std::os::unix::net::UnixStream::from(connection)),
            Ok(None) => {
                tracing::info!("the session closed the mail connector's door");
                None
            }
            Err(error) => {
                tracing::error!(%error, "the mail connector's door failed: it takes no more instances");
                None
            }
        }
    });
    serve::serve(settings_store::config_root(), attachments);
    Ok(())
}
