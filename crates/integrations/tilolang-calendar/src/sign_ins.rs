//! The workspace's calendar accounts, and the password each signs in with, as the session tells
//! the connector over the account sign-ins it handed it when it started
//! (`runtime_portal::origin_connector`, `accounts::sign_ins`).
//!
//! The accounts are the workspace's: an enabled calendar account of them is one the connector
//! signs in to. Its password is the vault entry it names, which the session releases only where
//! the person's grant on Passwords lets them use it, only for an enabled account that names that
//! entry, only for the server it names, and with the use written into the entry. What the session
//! refuses is said to the person in its words, and nothing else is tried: the connector reads no
//! file of accounts or credentials.
//!
//! The accounts are followed: when they change (an account is added, disabled or removed, or the
//! entry it names changes), the session answers with the new accounts and their passwords are
//! asked for again. An account the session refused is asked for again at every answer, so a
//! refusal made before this device caught up with the workspace is not kept. A password released
//! stays in this process's memory.

use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use accounts::sign_ins::{Answer, Ask, Client, NOT_ANSWERED};

use crate::engine::{AccountSource, CalendarAccount};

/// The account type of the workspace's accounts a calendar server is kept in.
const TYPE_ID: &str = "caldav";

/// The accounts the session's account sign-ins `link` tell, followed on a thread of its own that
/// calls `changed` each time they change.
#[must_use]
pub fn follow(link: UnixStream, changed: impl Fn() + Send + 'static) -> AccountSource {
    let signed: Arc<Mutex<Vec<CalendarAccount>>> = Arc::default();
    let followed = Arc::clone(&signed);
    let spawned = std::thread::Builder::new()
        .name("calendar-accounts".into())
        .spawn(move || match Client::over(link) {
            Ok(client) => following(client, &followed, &changed),
            Err(error) => {
                tracing::error!(%error, "the calendar connector's account sign-ins do not open");
            }
        });
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

fn following(mut client: Client, signed: &Mutex<Vec<CalendarAccount>>, changed: &dyn Fn()) {
    let mut after: Option<String> = None;
    loop {
        let answer = client.ask(&Ask::Accounts {
            after: after.clone(),
        });
        let (revision, accounts) = match answer {
            Ok(Answer::Accounts { revision, accounts }) => (revision, accounts),
            Ok(Answer::Refused { error }) => {
                tracing::warn!(error, "the session told the calendar connector no accounts");
                return;
            }
            Ok(Answer::SignIn { .. }) => {
                tracing::warn!("the session answered an ask for accounts with a sign-in");
                return;
            }
            Err(error) => {
                // The session ended the connection: no account signs in any more.
                tracing::info!(%error, "the calendar connector's account sign-ins ended");
                let mut kept = signed
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                for account in kept.iter_mut() {
                    account.password.clear();
                    account.refused = Some(NOT_ANSWERED.to_owned());
                }
                drop(kept);
                changed();
                return;
            }
        };
        let before = signed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let same = after.as_deref() == Some(revision.as_str());
        after = Some(revision);
        let mut moved = !same;
        let now: Vec<CalendarAccount> = accounts
            .iter()
            .filter_map(|told| {
                let held = before
                    .iter()
                    .find(|held| held.id == told.account.id.as_str());
                // An account signed in is kept as it is while the accounts are the same; one
                // refused is asked for again: the rules may have reached this device since.
                if same
                    && let Some(held) = held
                    && held.refused.is_none()
                {
                    return Some(held.clone());
                }
                let account = signed_in(&mut client, &told.workspace, &told.account)?;
                moved |= held.is_none_or(|held| held.refused != account.refused);
                Some(account)
            })
            .collect();
        *signed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = now;
        if moved {
            changed();
        }
    }
}

/// The settings a calendar account keeps, read from the account the session listed.
#[derive(Clone, Debug, Default, serde::Deserialize)]
struct Stored {
    #[serde(default)]
    server_url: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    read_only: bool,
}

/// `account` of `workspace` as the connector signs in to it, its password asked for on
/// `client`: `None` for one that is no enabled calendar account.
fn signed_in(
    client: &mut Client,
    workspace: &str,
    account: &accounts::Account,
) -> Option<CalendarAccount> {
    if account.type_id.as_str() != TYPE_ID || !account.enabled {
        return None;
    }
    let stored = account.config_as::<Stored>().unwrap_or_default();
    if stored.server_url.trim().is_empty() || stored.username.trim().is_empty() {
        return None;
    }
    let mut signed = CalendarAccount {
        id: account.id.to_string(),
        name: if account.display_name.is_empty() {
            stored.display_name
        } else {
            account.display_name.clone()
        },
        server: stored.server_url,
        username: stored.username,
        password: String::new(),
        refused: None,
        read_only: stored.read_only,
    };
    if let Some(entry) = accounts::email::password_item(&account.config) {
        match client.ask(&Ask::SignIn {
            workspace: workspace.to_owned(),
            item: entry.to_owned(),
        }) {
            Ok(Answer::SignIn { secret }) => signed.password = secret.to_string(),
            Ok(Answer::Refused { error }) => signed.refused = Some(error),
            Ok(Answer::Accounts { .. }) | Err(_) => signed.refused = Some(NOT_ANSWERED.to_owned()),
        }
    }
    Some(signed)
}
