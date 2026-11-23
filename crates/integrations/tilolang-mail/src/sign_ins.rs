//! The workspace's mail accounts, and the password each signs in with, as the session tells the
//! connector over the account sign-ins it handed it when it started
//! (`runtime_portal::origin_connector`, `accounts::sign_ins`).
//!
//! The accounts are the workspace's: an enabled email account of them is one the connector
//! signs in to. Its password is the vault entry it names (`passwordItem`), which the
//! session releases only where the person's grant on Passwords lets them use it, only for an
//! enabled account that names that entry, only for the servers it names, and with the use
//! written into the entry. What the session refuses is said to the person in its words, and
//! nothing else is tried: the connector reads no file of accounts or credentials.
//!
//! The accounts are followed: when they change (an account is added, disabled or removed, or the
//! entry it names changes), the session answers with the new accounts and their passwords are
//! asked for again. An account the session refused is asked for again at every answer, so a
//! refusal made before this device caught up with the workspace is not kept. Where the
//! workspace's rules ask the person first, the session refuses: no password leaves the vault on
//! a question. A password released stays in this process's memory.

use std::sync::{Arc, Mutex};
use tilo_socket::Stream;

use accounts::sign_ins::{Answer, Ask, Client, NOT_ANSWERED};

use crate::engine::{AccountSource, MailAccount};

/// The accounts the session's account sign-ins `link` tell, followed on a thread of its own that
/// calls `changed` each time they change.
#[must_use]
pub fn follow(link: Stream, changed: impl Fn() + Send + 'static) -> AccountSource {
    let signed: Arc<Mutex<Vec<MailAccount>>> = Arc::default();
    let followed = Arc::clone(&signed);
    let spawned = std::thread::Builder::new()
        .name("mail-accounts".into())
        .spawn(move || match Client::over(link) {
            Ok(client) => following(client, &followed, &changed),
            Err(error) => {
                tracing::error!(%error, "the mail connector's account sign-ins do not open");
            }
        });
    if let Err(error) = spawned {
        tracing::error!(%error, "the mail connector does not follow the workspace's accounts");
    }
    Box::new(move || {
        signed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    })
}

fn following(mut client: Client, signed: &Mutex<Vec<MailAccount>>, changed: &dyn Fn()) {
    let mut after: Option<String> = None;
    loop {
        let answer = client.ask(&Ask::Accounts {
            after: after.clone(),
        });
        let (revision, accounts) = match answer {
            Ok(Answer::Accounts { revision, accounts }) => (revision, accounts),
            Ok(Answer::Refused { error }) => {
                tracing::warn!(error, "the session told the mail connector no accounts");
                return;
            }
            Ok(Answer::SignIn { .. } | Answer::Reported) => {
                tracing::warn!("the session answered an ask for accounts with something else");
                return;
            }
            Err(error) => {
                // The session ended the connection: no account signs in any more.
                tracing::info!(%error, "the mail connector's account sign-ins ended");
                let mut kept = signed
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                for account in kept.iter_mut() {
                    account.config.password.clear();
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
        let now: Vec<MailAccount> = accounts
            .iter()
            .filter_map(|told| {
                let held = before
                    .iter()
                    .find(|held| held.config.email == told.account.id.as_str());
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

/// `account` of `workspace` as the connector signs in to it, its password asked for on
/// `client`: `None` for one that is no enabled email account.
fn signed_in(
    client: &mut Client,
    workspace: &str,
    account: &accounts::Account,
) -> Option<MailAccount> {
    if account.type_id.as_str() != accounts::email::TYPE_ID || !account.enabled {
        return None;
    }
    let mut config = mail_engine::account::config_of(account)?;
    let mut refused = None;
    // A password, or for an account its provider's page signed in, the access token its grant
    // holds now: the session never hands out the grant itself.
    if !config.import_pending
        && let Some(entry) = accounts::email::password_item(&account.config)
    {
        let ask = Ask::SignIn {
            workspace: workspace.to_owned(),
            item: entry.to_owned(),
        };
        match client.ask(&ask) {
            Ok(Answer::SignIn { secret }) => config.password = secret.to_string(),
            Ok(Answer::Refused { error }) => refused = Some(error),
            Ok(Answer::Accounts { .. } | Answer::Reported) | Err(_) => {
                refused = Some(NOT_ANSWERED.to_owned());
            }
        }
    }
    Some(MailAccount { config, refused })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::{BufReader, Write as _};
    use std::sync::mpsc::{Receiver, Sender, channel};
    use std::time::Duration;

    use super::*;

    const WORKSPACE: &str = "00000000-0000-4000-8000-000000000007";
    const SAM: &str = "sam@rivera.example";
    const ROBIN: &str = "robin@rivera.example";
    const OFF: &str = "off@rivera.example";
    const UNNAMED: &str = "unnamed@rivera.example";
    const SAMS_ENTRY: &str = "vault.item:0123456789abcdef0123456789abcdef";
    const ROBINS_ENTRY: &str = "vault.item:fedcba9876543210fedcba9876543210";
    const NO_GRANT: &str = "You may not use this workspace's Passwords";

    fn account(address: &str, item: Option<&str>, enabled: bool) -> accounts::Account {
        let mut stored = accounts::Account::new(address, accounts::email::TYPE_ID);
        stored.enabled = enabled;
        stored.set_config(&mail_engine::model::AccountConfig {
            email: address.into(),
            imap_host: "imap.rivera.example".into(),
            smtp_host: "smtp.rivera.example".into(),
            ..Default::default()
        });
        accounts::email::set_password_item(&mut stored.config, item);
        stored
    }

    /// What the session's end takes in: an ask of the connector's, new accounts to tell, the
    /// wait of a held ask for accounts running out, or the session ending.
    enum In {
        Asked(Ask),
        Tell(Vec<accounts::Account>),
        WaitRanOut,
        End,
    }

    /// How the session answers the sign-in of an entry: given the entry, and how often it was
    /// asked before.
    type Release = fn(&str, usize) -> Answer;

    /// The session's end of the account sign-ins, as the daemon's account sign-ins answer: an ask for
    /// accounts waits for the next accounts the test tells on the sender returned, or for its
    /// wait to run out. Each sign-in asked is sent on the receiver returned.
    fn session(link: Stream, release: Release) -> (Sender<In>, Receiver<String>) {
        let (each_ask, asks) = channel();
        let (inbox, taken) = channel();
        let reading = inbox.clone();
        let mut reader = BufReader::new(link.try_clone().unwrap());
        std::thread::spawn(move || {
            while let Ok(Some(line)) = accounts::sign_ins::read_line(&mut reader) {
                let ask = serde_json::from_slice::<Ask>(&line).unwrap();
                if reading.send(In::Asked(ask)).is_err() {
                    return;
                }
            }
        });
        std::thread::spawn(move || {
            let mut write = link;
            let (mut revision, mut accounts, mut fresh, mut waiting) =
                (0_u32, Vec::new(), false, false);
            let mut asked: BTreeMap<String, usize> = BTreeMap::new();
            let mut say = |answer: &Answer| {
                let said = accounts::sign_ins::answer_line(answer).unwrap();
                write.write_all(&said).is_ok()
            };
            let held = |revision: u32, accounts: &Vec<accounts::Account>| Answer::Accounts {
                revision: revision.to_string(),
                accounts: accounts
                    .iter()
                    .cloned()
                    .map(|account| accounts::sign_ins::Held {
                        workspace: WORKSPACE.into(),
                        account,
                        signed_in: false,
                    })
                    .collect(),
            };
            while let Ok(taken) = taken.recv() {
                let answer = match taken {
                    In::End => {
                        drop(write.shutdown(std::net::Shutdown::Both));
                        return;
                    }
                    // A wait that ran out with no ask held lets the next one go at once.
                    In::WaitRanOut if !waiting => {
                        fresh = true;
                        continue;
                    }
                    In::WaitRanOut => {
                        waiting = false;
                        held(revision, &accounts)
                    }
                    In::Tell(told) => {
                        (revision, accounts, fresh) = (revision.saturating_add(1), told, true);
                        if !waiting {
                            continue;
                        }
                        waiting = false;
                        fresh = false;
                        held(revision, &accounts)
                    }
                    In::Asked(Ask::Accounts { .. }) if fresh => {
                        fresh = false;
                        held(revision, &accounts)
                    }
                    In::Asked(Ask::Accounts { .. }) => {
                        waiting = true;
                        continue;
                    }
                    In::Asked(Ask::SignIn { workspace, item }) => {
                        assert_eq!(workspace, WORKSPACE);
                        each_ask.send(item.clone()).unwrap();
                        let times = asked.entry(item.clone()).or_default();
                        let answer = release(&item, *times);
                        *times = times.saturating_add(1);
                        answer
                    }
                    In::Asked(Ask::ReportSync { .. }) => Answer::Reported,
                    In::Asked(Ask::Keep { .. } | Ask::Forget { .. }) => Answer::Refused {
                        error: "Mail keeps no sign-in of its own".into(),
                    },
                };
                if !say(&answer) {
                    return;
                }
            }
        });
        (inbox, asks)
    }

    fn released() -> Answer {
        Answer::SignIn {
            secret: "the app password".to_owned().into(),
        }
    }

    fn by_address(source: &AccountSource) -> Vec<(String, String, Option<String>)> {
        let mut accounts: Vec<_> = source()
            .into_iter()
            .map(|signed| (signed.config.email, signed.config.password, signed.refused))
            .collect();
        accounts.sort();
        accounts
    }

    #[test]
    fn an_enabled_account_signs_in_with_its_entry_and_a_refusal_is_said_in_the_sessions_words() {
        let (ours, theirs) = tilo_socket::pair().unwrap();
        // Sam's entry is released; Robin's is refused as a person's without the grant on
        // Passwords.
        let (tell, asks) = session(theirs, |item, _| {
            if item == SAMS_ENTRY {
                released()
            } else {
                Answer::Refused {
                    error: NO_GRANT.into(),
                }
            }
        });
        let (on_change, changes) = channel();
        let source = follow(ours, move || on_change.send(()).unwrap());

        tell.send(In::Tell(vec![
            account(SAM, Some(SAMS_ENTRY), true),
            account(ROBIN, Some(ROBINS_ENTRY), true),
            account(OFF, Some(SAMS_ENTRY), false),
            account(UNNAMED, None, true),
        ]))
        .unwrap();
        changes.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            by_address(&source),
            vec![
                (ROBIN.into(), String::new(), Some(NO_GRANT.into())),
                (SAM.into(), "the app password".into(), None),
                (UNNAMED.into(), String::new(), None),
            ],
            "a disabled account is not signed in to, and one that names no entry asks for none"
        );
        assert_eq!(
            asks.try_iter().collect::<Vec<_>>(),
            vec![SAMS_ENTRY.to_owned(), ROBINS_ENTRY.to_owned()]
        );

        // Sam's account is disabled in the workspace's accounts: the connector stops signing in
        // to it, and asks for no entry of it again.
        tell.send(In::Tell(vec![
            account(SAM, Some(SAMS_ENTRY), false),
            account(ROBIN, None, true),
        ]))
        .unwrap();
        changes.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            by_address(&source),
            vec![(ROBIN.into(), String::new(), None)]
        );
        assert!(asks.try_iter().next().is_none());

        // The session ends the connection: what was released is forgotten, and said so.
        tell.send(In::Tell(vec![account(ROBIN, Some(ROBINS_ENTRY), true)]))
            .unwrap();
        changes.recv_timeout(Duration::from_secs(5)).unwrap();
        tell.send(In::End).unwrap();
        changes.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            by_address(&source),
            vec![(ROBIN.into(), String::new(), Some(NOT_ANSWERED.into()))]
        );
    }

    #[test]
    fn a_refusal_is_asked_again_with_the_next_answer_and_a_release_then_signs_in() {
        let (ours, theirs) = tilo_socket::pair().unwrap();
        // The device had not caught up with the workspace's rules the first time it was asked.
        let (tell, asks) = session(theirs, |_, times| {
            if times == 0 {
                Answer::Refused {
                    error: NO_GRANT.into(),
                }
            } else {
                released()
            }
        });
        let (on_change, changes) = channel();
        let source = follow(ours, move || on_change.send(()).unwrap());
        tell.send(In::Tell(vec![account(ROBIN, Some(ROBINS_ENTRY), true)]))
            .unwrap();
        changes.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            by_address(&source),
            vec![(ROBIN.into(), String::new(), Some(NO_GRANT.into()))]
        );

        // The session's next answer holds the same accounts (its wait for a change ran out): the
        // refused account is asked for again, and signs in.
        tell.send(In::WaitRanOut).unwrap();
        changes.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            by_address(&source),
            vec![(ROBIN.into(), "the app password".into(), None)]
        );
        assert_eq!(
            asks.try_iter().collect::<Vec<_>>(),
            vec![ROBINS_ENTRY.to_owned(), ROBINS_ENTRY.to_owned()]
        );
    }
}
