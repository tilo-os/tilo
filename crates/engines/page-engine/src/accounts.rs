//! The website accounts of the person's that open in a window of their own (Slack, Discord),
//! each on a profile of its own, signed in as that account.
//!
//! The workspace keeps each as an account, its cookies in the workspace's vault. Browser's window
//! asks the vault, on its own connection, which there are, and the app offers each. A window
//! opened on one shows a page whose engine is on its way: the window's host opens the account's
//! profile off the window's thread, signed in, and hands the engine over, and what keeps the
//! profile stays with the page until the window closes.

use std::sync::mpsc;

use tilo_webview::{SharedEngine, WebViewCommand};

use crate::PageSession;
use crate::codec;
use crate::logins::VaultAnswer;

/// One of the person's website accounts, as the vault lists it for Browser's window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebsiteAccount {
    /// The account, by its id.
    pub id: String,
    /// The service it signs in to, as its word (`slack`, `discord`).
    pub service: String,
    /// What the person calls it.
    pub title: String,
}

/// An account's engine, on the profile of its own the host opened signed in as the account.
pub struct Arrived {
    /// The engine the page shows from now on.
    pub engine: SharedEngine,
    /// Where the account opens.
    pub url: String,
    /// What keeps the account's profile, and stops its engine, once the window closes.
    pub held: Box<dyn std::any::Any + Send>,
}

/// The host's answer to a page that waits on an account's engine: the engine, or why it did not
/// open, in the person's words.
pub type Arriving = mpsc::Receiver<Result<Arrived, String>>;

impl PageSession {
    /// The page of a window that opens a website account: it shows `placeholder` until
    /// `arriving` answers, then the account's own engine. It keeps no vault: the account's profile
    /// is signed in as the account, and is offered none of the person's saved logins.
    #[must_use]
    pub fn new_arriving(placeholder: SharedEngine, arriving: Arriving) -> Self {
        let mut session = Self::new(placeholder);
        session.arriving = Some(arriving);
        session
    }

    /// Ask the vault which website accounts of the person's open in a window of their own: the
    /// app is told as a note. A page with no vault (a private one, a visitor's) asks nothing.
    pub fn list_accounts(&mut self) {
        let Some(vault) = self.vault.clone() else {
            return;
        };
        let sender = self.vault_answers.0.clone();
        self.vault_pending = self.vault_pending.saturating_add(1);
        vault.website_accounts(Box::new(move |result| {
            // A window that closed meanwhile has nobody to tell.
            drop(sender.send(VaultAnswer::Accounts(result)));
        }));
    }

    /// Whether the page still waits on its account's engine.
    pub(crate) fn arriving(&self) -> bool {
        self.arriving.is_some()
    }

    /// Take the account's engine once the host handed it over: the page shows it from now on, at
    /// the account's own address. The app is told either way.
    pub(crate) fn poll_arriving(&mut self) {
        let Some(arriving) = self.arriving.as_ref() else {
            return;
        };
        let answered = match arriving.try_recv() {
            Ok(answered) => answered,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                Err("The account's profile could not be opened".to_owned())
            }
        };
        self.arriving = None;
        match answered {
            Ok(arrived) => {
                self.engine = arrived.engine;
                self.engine.send(WebViewCommand::Navigate(arrived.url));
                self.account = Some(arrived.held);
                self.notes.push(codec::account_note(true, ""));
            }
            Err(why) => self.notes.push(codec::account_note(false, &why)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tilo_webview::test_engine::TestEngine;

    /// Says when it is dropped: what keeps an account's profile.
    struct Keeper(Arc<AtomicBool>);
    impl Drop for Keeper {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    fn said(session: &mut PageSession) -> Vec<serde_json::Value> {
        session
            .notes()
            .iter()
            .map(|note| serde_json::from_str(note).unwrap())
            .collect()
    }

    /// A window opened on an account shows its placeholder until the host hands over the
    /// account's engine, then that engine, at the account's address; what keeps the account's
    /// profile goes only with the window.
    #[test]
    fn an_account_window_shows_the_accounts_engine_once_it_arrives() {
        let (send, arriving) = mpsc::channel();
        let mut session = PageSession::new_arriving(TestEngine::new(), arriving);
        session.poll();
        assert!(session.busy(), "the page waits on its account's engine");
        assert!(said(&mut session).is_empty());

        let account = TestEngine::new();
        let dropped = Arc::new(AtomicBool::new(false));
        send.send(Ok(Arrived {
            engine: account.clone() as SharedEngine,
            url: "https://app.slack.com/client".to_owned(),
            held: Box::new(Keeper(dropped.clone())),
        }))
        .unwrap();
        session.poll();
        assert!(Arc::ptr_eq(
            session.engine(),
            &(account.clone() as SharedEngine)
        ));
        assert!(account.commands().iter().any(|command| matches!(
            command,
            WebViewCommand::Navigate(url) if url == "https://app.slack.com/client"
        )));
        let notes = said(&mut session);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert_eq!(notes[0]["note"], "account");
        assert_eq!(notes[0]["open"], true);
        assert!(!session.arriving(), "the page waits on nothing more");
        assert!(
            !dropped.load(Ordering::Acquire),
            "the window still shows it"
        );
        drop(session);
        assert!(
            dropped.load(Ordering::Acquire),
            "the profile goes with the window"
        );
    }

    /// A host that could not open the account says why, and the page keeps its placeholder: the
    /// person's own profile is never shown instead.
    #[test]
    fn an_account_that_did_not_open_says_why() {
        let placeholder = TestEngine::new();
        let (send, arriving) = mpsc::channel();
        let mut session = PageSession::new_arriving(placeholder.clone() as SharedEngine, arriving);
        send.send(Err("Sign in to Slack again".to_owned())).unwrap();
        session.poll();
        assert!(Arc::ptr_eq(
            session.engine(),
            &(placeholder.clone() as SharedEngine)
        ));
        let notes = said(&mut session);
        assert_eq!(notes[0]["open"], false);
        assert_eq!(notes[0]["message"], "Sign in to Slack again");
        assert!(!session.busy());

        // A host gone without answering is said too.
        let (send, arriving) = mpsc::channel::<Result<Arrived, String>>();
        let mut session = PageSession::new_arriving(placeholder as SharedEngine, arriving);
        drop(send);
        session.poll();
        assert_eq!(said(&mut session)[0]["open"], false);
    }
}
