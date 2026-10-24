//! Signing in before the root is open: unwrap a person's user key from the
//! `tilo-signin` tokens, open the system volume through their keyslot, and
//! hand the sign-in on to the session (docs/security/sign-in.md §3.2).
//! Runs in the initrd, without the daemon.

use crate::handoff::{self, Handoff};
use crate::luks::{self, SigninToken};
use crate::record::{Factor, SecretKind};
use crate::recovery::RecoveryKey;
use crate::secret::Secret;
use crate::tpm::Tries;
use crate::userkey::UserKey;
use crate::wrapper::{Kind, Outcome, Tpm};
use anyhow::{Context as _, Result};
use serde::Serialize;
use std::path::Path;
use tilo_rollback::exec::Runner;
use tilo_rollback::policy::Workspace;

pub struct BootUnlock<'a> {
    pub runner: &'a dyn Runner,
    pub ws: &'a Workspace,
    pub device: &'a Path,
    pub volume: &'a str,
    pub uid: Option<u32>,
    /// Where systemd-cryptsetup looks for `<volume>.key`.
    pub keys_dir: &'a Path,
    /// The handoff key's description; `None` leaves no handoff.
    pub handoff: Option<&'a str>,
}

#[derive(Debug)]
pub enum BootOutcome {
    Unlocked {
        uid: u32,
    },
    Wrong(Option<Tries>),
    LockedOut(Option<Tries>),
    /// Nothing here could be tried; the caller falls back to the old prompts.
    CouldNotTry(String),
}

#[derive(Debug, Serialize)]
pub struct Person {
    pub uid: u32,
    pub name: String,
    pub display_name: String,
    pub factors: Vec<Factor>,
    pub secret: SecretKind,
}

pub fn people(runner: &dyn Runner, device: &Path) -> Result<Vec<Person>> {
    Ok(luks::tokens(runner, device)?
        .into_iter()
        .map(|(_, token)| Person {
            uid: token.uid,
            factors: token.factors(),
            secret: token.secret.unwrap_or(SecretKind::Password),
            name: token.name,
            display_name: token.display_name,
        })
        .collect())
}

impl BootUnlock<'_> {
    pub fn run(&self, secret_file_bytes: &[u8]) -> BootOutcome {
        match self.try_run(secret_file_bytes) {
            Ok(outcome) => outcome,
            Err(e) => BootOutcome::CouldNotTry(format!("{e:#}")),
        }
    }

    fn try_run(&self, secret_file_bytes: &[u8]) -> Result<BootOutcome> {
        let candidates: Vec<SigninToken> = luks::tokens(self.runner, self.device)?
            .into_iter()
            .map(|(_, token)| token)
            .filter(|token| self.uid.is_none_or(|uid| token.uid == uid))
            .collect();
        if candidates.is_empty() {
            return Ok(BootOutcome::CouldNotTry(format!(
                "no sign-in token on {} for this person",
                self.device.display()
            )));
        }
        let Some(secret) = Secret::from_file_bytes(secret_file_bytes) else {
            return Ok(BootOutcome::Wrong(None));
        };

        if let Some(recovery) = RecoveryKey::parse(secret.as_str()) {
            for token in &candidates {
                for wrapper in token.known() {
                    if let Some(key) = wrapper.open_with_recovery(token.uid, &recovery)? {
                        return self.finish(token, &key);
                    }
                }
            }
            return Ok(BootOutcome::Wrong(None));
        }

        for token in &candidates {
            for wrapper in token.known().filter(|w| w.kind() == Kind::Passphrase) {
                if let Some(Outcome::Key(key)) =
                    wrapper.open_with_secret(None, token.uid, &secret)?
                {
                    return self.finish(token, &key);
                }
            }
        }

        let with_tpm: Vec<&SigninToken> = candidates
            .iter()
            .filter(|t| t.known().any(|w| w.kind() == Kind::Tpm))
            .collect();
        let token = match with_tpm.as_slice() {
            [] => return Ok(BootOutcome::Wrong(None)),
            [one] => *one,
            _ => {
                return Ok(BootOutcome::CouldNotTry(
                    "several people sign in with the TPM here; say whose secret this is".into(),
                ));
            }
        };
        let tpm = Tpm {
            runner: self.runner,
            ws: self.ws,
        };
        let Some(wrapper) = token.known().find(|w| w.kind() == Kind::Tpm) else {
            return Ok(BootOutcome::Wrong(None));
        };
        Ok(
            match wrapper.open_with_secret(Some(tpm), token.uid, &secret)? {
                Some(Outcome::Key(key)) => return self.finish(token, &key),
                Some(Outcome::Wrong(tries)) => BootOutcome::Wrong(tries),
                None => BootOutcome::Wrong(None),
                Some(Outcome::LockedOut(tries)) => BootOutcome::LockedOut(tries),
            },
        )
    }

    fn finish(&self, token: &SigninToken, key: &UserKey) -> Result<BootOutcome> {
        let Some(slot) = token.keyslot() else {
            return Ok(BootOutcome::CouldNotTry(format!(
                "the sign-in token for {} names no single keyslot",
                token.name
            )));
        };
        let passphrase = key.system_volume_passphrase();
        if !luks::opens(
            self.runner,
            self.ws,
            self.device,
            passphrase.as_bytes(),
            slot,
        )? {
            return Ok(BootOutcome::CouldNotTry(format!(
                "{}'s keyslot {slot} does not open {}",
                token.name,
                self.device.display()
            )));
        }
        let key_file = self.keys_dir.join(format!("{}.key", self.volume));
        tilo_files::native::atomic::create_private_directory(self.keys_dir)
            .with_context(|| format!("creating {}", self.keys_dir.display()))?;
        tilo_files::native::atomic::write_private(&key_file, passphrase.as_bytes())
            .with_context(|| format!("writing {}", key_file.display()))?;
        if let Some(description) = self.handoff {
            let handoff = Handoff {
                uid: token.uid,
                key: key.clone(),
            };
            if let Err(e) = handoff::add(&handoff, description) {
                eprintln!(
                    "tilo-auth: the volume opens, but the sign-in could not be handed to the \
                     session ({e}); the login screen will ask again"
                );
            }
        }
        Ok(BootOutcome::Unlocked { uid: token.uid })
    }
}
