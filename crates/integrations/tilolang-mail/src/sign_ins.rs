//! The workspace's mail accounts, and the password each signs in with, as the connector reads
//! them through the device's one dispatch ([`tilo_actions::remote::signing_in`]).
//!
//! The accounts are the workspace's: an enabled email account of them is one the connector
//! signs in to. Its password is the vault entry it names (`passwordItem`), which the device
//! releases only where the person's grant on Passwords lets the app use it, only for an enabled
//! account that names that entry, only for the servers it names, and with the use written into
//! the entry. A provider's own page leaves a grant instead, which the device hands out as the
//! access token it holds now, renewed where the grant is kept. What the dispatch refuses is said
//! to the person in its words, and nothing else is tried: the connector reads no file of
//! accounts or credentials.
//!
//! The accounts are followed: the action is asked again, and when they change (an account is
//! added, disabled or removed, or the entry it names changes) the accounts are the new ones and
//! their passwords are asked for again. An account the dispatch refused is asked for again, so a
//! refusal made before this device caught up with the workspace is not kept. Where the
//! workspace's rules ask the person first, the dispatch refuses until they answer: no password
//! leaves the vault on a question. A password released stays in this process's memory.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_actions::remote::person_accounts::MailSignIn;
use tilo_actions::remote::signing_in::{NOT_ANSWERED, SigningIn};
use tilo_socket::Stream;

use crate::engine::{AccountSource, MailAccount};

/// How long the connector waits before asking the accounts again.
const EVERY: Duration = Duration::from_secs(30);

/// The accounts the dispatch on `link` tells, followed on a thread of its own that calls
/// `changed` each time they change.
#[must_use]
pub fn follow(link: Stream, changed: impl Fn() + Send + 'static) -> AccountSource {
    let signed: Arc<Mutex<Vec<MailAccount>>> = Arc::default();
    let followed = Arc::clone(&signed);
    let spawned = std::thread::Builder::new()
        .name("mail-accounts".into())
        .spawn(
            move || match SigningIn::over(link) {
                Ok(signing) => following(signing, &followed, &changed),
                Err(error) => {
                    tracing::error!(%error, "the mail connector's connection to this device does not open");
                }
            },
        );
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

fn following(mut signing: SigningIn, signed: &Mutex<Vec<MailAccount>>, changed: &dyn Fn()) {
    loop {
        let told = match signing.sign_ins() {
            Ok(sign_ins) => sign_ins,
            Err(error) => {
                tracing::info!(%error, "the mail connector reads no accounts now");
                for account in signed
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .iter_mut()
                {
                    account.config.password.clear();
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
        let now: Vec<MailAccount> = told
            .sign_ins
            .iter()
            .filter_map(|login| signed_in(&mut signing, login))
            .collect();
        let moved = now.len() != before.len()
            || now.iter().zip(before.iter()).any(|(now, before)| {
                now.config.display_name != before.config.display_name
                    || now.config.email != before.config.email
                    || now.config.imap_host != before.config.imap_host
                    || now.config.imap_port != before.config.imap_port
                    || now.config.smtp_host != before.config.smtp_host
                    || now.config.smtp_port != before.config.smtp_port
                    || now.config.username != before.config.username
                    || now.config.password != before.config.password
                    || now.refused != before.refused
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

/// The account `login` signs in for, as the connector signs in to it: `None` for a login whose
/// account is no enabled email account.
fn signed_in(signing: &mut SigningIn, login: &MailSignIn) -> Option<MailAccount> {
    let account = login.account.as_ref()?;
    if account.kind != accounts::email::TYPE_ID || !account.enabled {
        return None;
    }
    let mut config = accounts::email::AccountConfig::from_value(account.settings.clone()).ok()?;
    let mut refused = None;
    // A password, or for an account its provider's page signed in, the access token its grant
    // holds now: the device never hands out the grant itself.
    if !config.import_pending {
        match signing.secret(&account.id, login) {
            Ok(secret) => config.password = secret,
            Err(error) => refused = Some(error),
        }
    }
    Some(MailAccount { config, refused })
}
