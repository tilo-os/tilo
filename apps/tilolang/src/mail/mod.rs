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
    let (requests, events) = mail::engine::spawn(content, wake);
    Rc::new(RefCell::new(ImapOrigin::new(
        Box::new(EngineConnector::new(accounts, requests, events)),
        woken,
    )))
}
