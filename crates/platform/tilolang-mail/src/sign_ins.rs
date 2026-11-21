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
//! asked for again. A password released stays in this process's memory.

use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use accounts::sign_ins::{Answer, Ask, Client};
use mail_engine::model::AuthMethod;

use crate::engine::{AccountSource, MailAccount};

/// What a mail account is refused with when the session's account sign-ins ended.
pub const SIGN_INS_ENDED: &str = "The session no longer answers this account's sign-in";

/// The accounts the session's account sign-ins `link` tell, followed on a thread of its own that
/// calls `changed` each time they change.
#[must_use]
pub fn follow(link: UnixStream, changed: impl Fn() + Send + 'static) -> AccountSource {
    let signed: Arc<Mutex<Vec<MailAccount>>> = Arc::default();
    let followed = Arc::clone(&signed);
    let spawned = std::thread::Builder::new()
        .name("mail-accounts".into())
        .spawn(move || following(link, &followed, &changed));
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

fn following(link: UnixStream, signed: &Mutex<Vec<MailAccount>>, changed: &dyn Fn()) {
    let mut client = match Client::over(link) {
        Ok(client) => client,
        Err(error) => {
            tracing::error!(%error, "the mail connector's account sign-ins do not open");
            return;
        }
    };
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
            Ok(Answer::SignIn { .. }) => {
                tracing::warn!("the session answered an ask for accounts with a sign-in");
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
                    account.refused = Some(SIGN_INS_ENDED.to_owned());
                }
                drop(kept);
                changed();
                return;
            }
        };
        if after.as_deref() == Some(revision.as_str()) {
            continue;
        }
        after = Some(revision);
        let now: Vec<MailAccount> = accounts
            .iter()
            .filter_map(|told| signed_in(&mut client, &told.workspace, &told.account))
            .collect();
        *signed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = now;
        changed();
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
    if config.auth == AuthMethod::Password
        && !config.import_pending
        && let Some(entry) = accounts::email::password_item(&account.config)
    {
        let ask = Ask::SignIn {
            workspace: workspace.to_owned(),
            item: entry.to_owned(),
        };
        match client.ask(&ask) {
            Ok(Answer::SignIn { secret }) => config.password = secret.to_string(),
            Ok(Answer::Refused { error }) => refused = Some(error),
            Ok(Answer::Accounts { .. }) | Err(_) => refused = Some(SIGN_INS_ENDED.to_owned()),
        }
    }
    Some(MailAccount { config, refused })
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, Write as _};
    use std::sync::mpsc::{Receiver, channel};
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

    /// The session's end of the account sign-ins: it tells the accounts `told` sends, one list
    /// per answer, and releases Sam's entry while it refuses Robin's as a person without the
    /// grant on Passwords. Each sign-in asked is sent on the receiver returned.
    fn session(link: UnixStream, told: Receiver<Vec<accounts::Account>>) -> Receiver<String> {
        let (each_ask, asks) = channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(link.try_clone().unwrap());
            let mut write = link;
            let mut revision = 0_u32;
            while let Ok(Some(line)) = accounts::sign_ins::read_line(&mut reader) {
                let answer = match serde_json::from_slice::<Ask>(&line).unwrap() {
                    Ask::Accounts { .. } => {
                        let Ok(accounts) = told.recv() else { return };
                        revision = revision.saturating_add(1);
                        Answer::Accounts {
                            revision: revision.to_string(),
                            accounts: accounts
                                .into_iter()
                                .map(|account| accounts::sign_ins::Held {
                                    workspace: WORKSPACE.into(),
                                    account,
                                })
                                .collect(),
                        }
                    }
                    Ask::SignIn {
                        workspace,
                        item: entry,
                    } => {
                        assert_eq!(workspace, WORKSPACE);
                        each_ask.send(entry.clone()).unwrap();
                        if entry == SAMS_ENTRY {
                            Answer::SignIn {
                                secret: "sams app password".to_owned().into(),
                            }
                        } else {
                            Answer::Refused {
                                error: NO_GRANT.into(),
                            }
                        }
                    }
                };
                let said = accounts::sign_ins::answer_line(&answer).unwrap();
                if write.write_all(&said).is_err() {
                    return;
                }
            }
        });
        asks
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
        let (ours, theirs) = UnixStream::pair().unwrap();
        let (tell, told) = channel();
        let asks = session(theirs, told);
        let (on_change, changes) = channel();
        let source = follow(ours, move || on_change.send(()).unwrap());

        tell.send(vec![
            account(SAM, Some(SAMS_ENTRY), true),
            account(ROBIN, Some(ROBINS_ENTRY), true),
            account(OFF, Some(SAMS_ENTRY), false),
            account(UNNAMED, None, true),
        ])
        .unwrap();
        changes.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            by_address(&source),
            vec![
                (ROBIN.into(), String::new(), Some(NO_GRANT.into())),
                (SAM.into(), "sams app password".into(), None),
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
        tell.send(vec![
            account(SAM, Some(SAMS_ENTRY), false),
            account(ROBIN, None, true),
        ])
        .unwrap();
        changes.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            by_address(&source),
            vec![(ROBIN.into(), String::new(), None)]
        );
        assert!(asks.try_iter().next().is_none());

        // The session ends the connection: what was released is forgotten, and said so.
        tell.send(vec![account(ROBIN, Some(ROBINS_ENTRY), true)])
            .unwrap();
        changes.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(tell);
        changes.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            by_address(&source),
            vec![(ROBIN.into(), String::new(), Some(SIGN_INS_ENDED.into()))]
        );
    }
}
