//! Mail servers as an outside origin of tilolang packages: the IMAP and SMTP connector over
//! native Mail's protocol code ([`connector`]) and the origin that keeps a package's `imap`
//! types in step with it ([`origin`]).
//!
//! The accounts are the workspace's settings' (the account store Settings writes), and their
//! credentials stay in custody, read by the connector in this native process: package code
//! never sees them.

pub mod connector;
pub mod origin;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use connector::{AccountSource, EngineConnector};
use origin::ImapOrigin;

/// The settings' mail accounts under the profile's configuration root.
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

/// Set `reconnected` and wake whenever the device comes online again: what could not reach
/// its server then is tried again. An event of the platform's network status, never a timer.
fn follow_the_network(reconnected: Arc<AtomicBool>, wake: mail::engine::Wake) {
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
