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

pub(crate) enum Released {
    Secret {
        account: String,
        password: Zeroizing<String>,
    },
    Refused {
        account: String,
        error: String,
    },
}

pub(crate) struct SecretRelease {
    source: Source,
    waiting: VecDeque<String>,
    asking: Option<(String, Pending)>,
}

impl SecretRelease {
    /// Custody on the window's route, captured while the window is built.
    pub(crate) fn capture() -> Option<Self> {
        Client::capture().map(|client| Self::from(Source::Route(Box::new(client))))
    }

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

    pub(crate) fn awaits(&self, account: &str) -> bool {
        self.asking
            .as_ref()
            .is_some_and(|(asked, _)| asked == account)
            || self.waiting.iter().any(|waiting| waiting == account)
    }

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

enum Asked {
    Pending(String, Pending),
    Answered(Released),
}

enum Source {
    Route(Box<Client>),
    #[cfg(test)]
    Root(std::path::PathBuf),
}

impl Source {
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
