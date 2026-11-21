//! The secrets of the workspace's email accounts, released to this window by the session.
//!
//! Mail runs sandboxed: it reaches neither the workspace's vault nor the key storage that seals
//! it. It asks the way every app's engine does
//! ([`ui::collaboration::browser_service_sessions::AccountSecrets`]): by the vault entry each
//! account names as its sign-in. A secret released stays in this process's memory, never in a
//! file or the document.

use std::collections::BTreeMap;

use ui::collaboration::browser_service_sessions::{AccountSecret, AccountSecrets, Client};
use zeroize::Zeroizing;

pub enum Released {
    Secret {
        account: String,
        password: Zeroizing<String>,
    },
    Refused {
        account: String,
        error: String,
    },
}

pub struct SecretRelease {
    source: Source,
}

enum Source {
    /// The window's route: each account asked for by its entry, and answered for it.
    Route {
        asks: Box<AccountSecrets>,
        /// The account each entry asked for is the sign-in of.
        accounts: BTreeMap<String, String>,
        /// Accounts that name no entry: refused in words on the next pass.
        unnamed: Vec<String>,
    },
    /// A profile's own stores, outside a session.
    #[cfg(any(test, feature = "test-support"))]
    Root {
        root: std::path::PathBuf,
        waiting: Vec<String>,
    },
}

impl SecretRelease {
    /// The session's release on the window's own route, through `client`: the connection the
    /// window captured while it was built.
    #[must_use]
    pub fn through(client: Client) -> Self {
        Self {
            source: Source::Route {
                asks: Box::new(AccountSecrets::through(client)),
                accounts: BTreeMap::new(),
                unnamed: Vec::new(),
            },
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn kept_under(root: std::path::PathBuf) -> Self {
        Self {
            source: Source::Root {
                root,
                waiting: Vec::new(),
            },
        }
    }

    /// Ask for the password of `account`, which names the vault entry `item` as its sign-in
    /// (`None` where it names none, which is refused in words when polled).
    pub fn ask(&mut self, account: &str, item: Option<&str>) {
        if self.awaits(account) {
            return;
        }
        match &mut self.source {
            Source::Route {
                asks,
                accounts,
                unnamed,
            } => match item {
                Some(item) => {
                    accounts.insert(item.to_owned(), account.to_owned());
                    asks.ask(item);
                }
                None => unnamed.push(account.to_owned()),
            },
            #[cfg(any(test, feature = "test-support"))]
            Source::Root { waiting, .. } => waiting.push(account.to_owned()),
        }
    }

    pub fn awaits(&self, account: &str) -> bool {
        match &self.source {
            Source::Route {
                accounts, unnamed, ..
            } => accounts
                .values()
                .chain(unnamed.iter())
                .any(|asked| asked == account),
            #[cfg(any(test, feature = "test-support"))]
            Source::Root { waiting, .. } => waiting.iter().any(|asked| asked == account),
        }
    }

    pub fn poll(&mut self, wake: Option<&ui::WakeHandle>) -> Vec<Released> {
        match &mut self.source {
            Source::Route {
                asks,
                accounts,
                unnamed,
            } => {
                let mut released: Vec<Released> = std::mem::take(unnamed)
                    .into_iter()
                    .map(|account| Released::Refused {
                        account,
                        error: crate::engine::NO_PASSWORD.into(),
                    })
                    .collect();
                for answer in asks.poll(wake) {
                    let (item, answer) = match answer {
                        AccountSecret::Released { item, secret } => (item, Ok(secret)),
                        AccountSecret::Refused { item, error } => (item, Err(error)),
                    };
                    let Some(account) = accounts.remove(&item) else {
                        continue;
                    };
                    released.push(match answer {
                        Ok(password) => Released::Secret { account, password },
                        Err(error) => Released::Refused { account, error },
                    });
                }
                released
            }
            #[cfg(any(test, feature = "test-support"))]
            Source::Root { root, waiting } => std::mem::take(waiting)
                .into_iter()
                .map(|account| release_under(root, account))
                .collect(),
        }
    }
}

/// The password a profile's own credentials keep for its enabled account `account`, typed for
/// the servers the account names.
#[cfg(any(test, feature = "test-support"))]
fn release_under(root: &std::path::Path, account: String) -> Released {
    let store = accounts::AccountStore::load_in(root);
    let released = accounts::AccountCredentials::open_in(root).and_then(|credentials| {
        store
            .get(&accounts::AccountId::from(account.clone()))
            .filter(|kept| kept.enabled)
            .and_then(|kept| credentials.get_for(kept))
            .map(|secret| Zeroizing::new(secret.password))
            .filter(|password| !password.is_empty())
            .ok_or_else(|| "This profile keeps no password for that account".to_owned())
    });
    match released {
        Ok(password) => Released::Secret { account, password },
        Err(error) => Released::Refused { account, error },
    }
}
