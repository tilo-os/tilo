//! The workspace's calendar accounts, and the password each signs in with, as the connector
//! reads them through the device's one dispatch ([`tilo_actions::remote::signing_in`]).
//!
//! The accounts are the workspace's: an enabled calendar account of them is one the connector
//! signs in to. Its password is the vault entry the account names. The connector calls the
//! published `sign_ins` action as the app it was launched as, which lists the vault's logins with
//! the account each signs in for, and asks `vault#release` for one entry's secret on the
//! connection the daemon handed it, which hands it only to the app's own process: decided under
//! the grants that name the app, with the use written into the entry. What the dispatch refuses
//! is said to the person in its words, and nothing else is tried: the connector reads no file of
//! accounts or credentials.
//!
//! The accounts are followed: the action is asked again, and when they change (an account is
//! added, disabled or removed, or the entry it names changes) the accounts are the new ones and
//! their passwords are asked for again. An account the dispatch refused is asked for again, so a
//! refusal made before this device caught up with the workspace is not kept. A password released
//! stays in this process's memory.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_actions::remote::signing_in::{NOT_ANSWERED, SigningIn};
use tilo_socket::Stream;

use crate::engine::AccountSource;
use crate::engine::CalendarAccount;

/// The account type of the workspace's accounts a calendar server is kept in.
const TYPE_ID: &str = "caldav";

/// How long the connector waits before asking the accounts again.
const EVERY: Duration = Duration::from_secs(30);

/// The accounts the dispatch on `link` tells, followed on a thread of its own that calls
/// `changed` each time they change.
#[must_use]
pub fn follow(link: Stream, changed: impl Fn() + Send + 'static) -> AccountSource {
    let signed: Arc<Mutex<Vec<CalendarAccount>>> = Arc::default();
    let followed = Arc::clone(&signed);
    let spawned = std::thread::Builder::new()
        .name("calendar-accounts".into())
        .spawn(
            move || match SigningIn::over(link) {
                Ok(signing) => following(signing, &followed, &changed),
                Err(error) => {
                    tracing::error!(%error, "the calendar connector's connection to this device does not open");
                }
            },
        );
    if let Err(error) = spawned {
        tracing::error!(%error, "the calendar connector does not follow the workspace's accounts");
    }
    Arc::new(move || {
        signed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    })
}

fn following(mut signing: SigningIn, signed: &Mutex<Vec<CalendarAccount>>, changed: &dyn Fn()) {
    loop {
        let told = match signing.sign_ins() {
            Ok(sign_ins) => sign_ins,
            Err(error) => {
                tracing::info!(%error, "the calendar connector reads no accounts now");
                for account in signed
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .iter_mut()
                {
                    account.password.clear();
                    account.refused = Some(NOT_ANSWERED.to_owned());
                }
                changed();
                std::thread::sleep(EVERY);
                continue;
            }
        };
        let before = signed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let mut now = Vec::new();
        for login in &told.sign_ins {
            let Some(account) = login.account.as_ref() else {
                continue;
            };
            if account.kind != TYPE_ID || !account.enabled {
                continue;
            }
            let username = if account.address.trim().is_empty() {
                login.username.clone()
            } else {
                account.address.clone()
            };
            if account.server.trim().is_empty() || username.trim().is_empty() {
                continue;
            }
            let mut calendar = CalendarAccount {
                id: account.id.clone(),
                name: if account.name.is_empty() {
                    login.title.clone()
                } else {
                    account.name.clone()
                },
                server: account.server.clone(),
                username,
                password: String::new(),
                refused: None,
                read_only: account.read_only,
            };
            // A password released for this entry already is kept: a second release writes
            // another use into it.
            match signing.secret(&calendar.id, login) {
                Ok(secret) => calendar.password = secret,
                Err(error) => calendar.refused = Some(error),
            }
            now.push(calendar);
        }
        let moved = now.len() != before.len()
            || now.iter().zip(before.iter()).any(|(now, before)| {
                now.id != before.id
                    || now.server != before.server
                    || now.username != before.username
                    || now.read_only != before.read_only
                    || now.name != before.name
                    || now.refused != before.refused
                    || now.password != before.password
            });
        *signed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = now;
        if moved {
            changed();
        }
        std::thread::sleep(EVERY);
    }
}
