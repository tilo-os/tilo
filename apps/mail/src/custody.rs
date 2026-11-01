//! The secrets of the person's email accounts, released to this window by
//! the session's account custody.
//!
//! Mail runs sandboxed: it reaches neither the person's account vault nor
//! the key custody that seals it. The session holds both. On this window's
//! own route Mail asks for one email account's secret at a time
//! ([`BrowserServiceSessionCommand::ReleaseAccountSecret`]); the session
//! answers only for an enabled account of the type Mail keeps, and only to
//! Mail. A secret it releases stays in this process's memory, never in a
//! file or the document.

use std::collections::VecDeque;

use ui::collaboration::browser_service_sessions::{
    BrowserServiceSessionCommand, BrowserServiceSessionReply, Client, Pending,
};
use zeroize::Zeroizing;

/// What custody answered for one account.
pub(crate) enum Released {
    /// The account's secret.
    Secret {
        account: String,
        password: Zeroizing<String>,
    },
    /// Why custody did not release it.
    Refused { account: String, error: String },
}

/// The window's requests to custody: one under way, the rest waiting.
pub(crate) struct SecretRelease {
    source: Source,
    waiting: VecDeque<String>,
    asking: Option<(String, Pending)>,
}

impl SecretRelease {
    /// Custody on the window's route, captured while the window is built:
    /// `None` outside a session (a test, a `--data-dir` profile), whose
    /// explicit root keeps its own vault.
    pub(crate) fn capture() -> Option<Self> {
        Client::capture().map(|client| Self::from(Source::Route(Box::new(client))))
    }

    /// Custody answered as the session answers, from the person's accounts
    /// and vault under `root` (tests: the root a Settings window keeps).
    #[cfg(test)]
    pub(crate) fn kept_under(root: std::path::PathBuf) -> Self {
        Self::from(Source::Root(root))
    }

    fn from(source: Source) -> Self {
        Self {
            source,
            waiting: VecDeque::new(),
            asking: None,
        }
    }

    /// Ask for `account`'s secret, once however often it is asked for while
    /// waiting.
    pub(crate) fn ask(&mut self, account: &str) {
        if self
            .asking
            .as_ref()
            .is_some_and(|(asked, _)| asked == account)
            || self.waiting.iter().any(|waiting| waiting == account)
        {
            return;
        }
        self.waiting.push_back(account.to_owned());
    }

    /// Whether `account`'s secret is asked for and not answered yet.
    pub(crate) fn awaits(&self, account: &str) -> bool {
        self.asking
            .as_ref()
            .is_some_and(|(asked, _)| asked == account)
            || self.waiting.iter().any(|waiting| waiting == account)
    }

    /// Take custody's answers in and send the next request; `wake` wakes the
    /// window when an answer arrives.
    pub(crate) fn poll(&mut self, wake: Option<&ui::WakeHandle>) -> Vec<Released> {
        let mut released = Vec::new();
        loop {
            if let Some((account, pending)) = &mut self.asking {
                let Some(reply) = pending.poll() else {
                    break;
                };
                let account = account.clone();
                self.asking = None;
                released.push(match reply {
                    Ok(BrowserServiceSessionReply::AccountSecret {
                        account: answered,
                        password,
                    }) if answered == account => Released::Secret { account, password },
                    Ok(_) => Released::Refused {
                        account,
                        error: "account custody answered something else".into(),
                    },
                    Err(error) => Released::Refused { account, error },
                });
            }
            let Some(account) = self.waiting.pop_front() else {
                break;
            };
            match self.source.ask(account, wake) {
                Asked::Pending(account, pending) => self.asking = Some((account, pending)),
                Asked::Answered(answer) => released.push(answer),
            }
        }
        released
    }
}

/// A request to custody, under way or answered at once.
enum Asked {
    Pending(String, Pending),
    Answered(Released),
}

/// Where custody answers.
enum Source {
    /// The session, on the window's own route.
    Route(Box<Client>),
    /// The decision the session makes, over a root's own stores (tests).
    #[cfg(test)]
    Root(std::path::PathBuf),
}

impl Source {
    /// Ask for `account`'s secret.
    fn ask(&self, account: String, wake: Option<&ui::WakeHandle>) -> Asked {
        match self {
            Self::Route(client) => match client.request(
                BrowserServiceSessionCommand::ReleaseAccountSecret {
                    account: account.clone(),
                },
                wake.cloned(),
            ) {
                Ok(pending) => Asked::Pending(account, pending),
                Err(error) => Asked::Answered(Released::Refused { account, error }),
            },
            #[cfg(test)]
            Self::Root(root) => Asked::Answered(release_under(root, account)),
        }
    }
}

#[cfg(test)]
fn release_under(root: &std::path::Path, account: String) -> Released {
    let store = accounts::AccountStore::load_in(root);
    let released = accounts::AccountCredentials::open_in(root).and_then(|credentials| {
        accounts::custody::release(&store, &credentials, crate::account::KEEPER, &account)
    });
    match released {
        Ok(password) => Released::Secret { account, password },
        Err(error) => Released::Refused { account, error },
    }
}
