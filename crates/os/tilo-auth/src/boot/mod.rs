//! Opening the system volume in early boot, without the daemon. The ways a
//! volume opens, as `tilo-root-discover` offers them while the volume's
//! prompt is up (docs/security/sign-in.md §3.2, docs/device-reach.md):
//!
//! 1. a person's sign-in secret: their user key unwraps from the
//!    `tilo-signin` token, by the TPM-throttled or passphrase wrapper, opens
//!    their keyslot, and the sign-in carries on into their session (this
//!    module, [`BootUnlock`]);
//! 2. the recovery key, through the same token's recovery wrapper, or typed
//!    at the volume's prompt;
//! 3. another of the owner's devices releasing its half of the sibling
//!    keyslot ([`sibling`]): with no prompt for a server whose TPM quotes the
//!    boot chain the owner accepted, otherwise once the owner approves. It
//!    goes on beside the prompt, and whichever answers first opens the volume;
//! 4. the TPM alone, for a signed boot chain where the volume is enrolled
//!    for it (tilo-rollback's token, and systemd-cryptsetup's own tokens).
//!
//! Every way reads the header through [`crate::luks`] and the TPM through
//! [`crate::tpm`].

pub mod sibling;

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
use tilo_unlock_protocol::opened::{OPENED_PATH, Opening, UnlockMethod};

/// Leave how this boot's volume opened where the booted system reads it: in
/// the initrd's `/run`, which survives the switch to the booted system, and
/// which the server reports in its status. Root owns the file, and it holds
/// no secret, so it is world-readable.
///
/// # Errors
///
/// The opening could not be written.
pub fn note_opening(method: UnlockMethod) -> Result<()> {
    note_opening_at(
        Path::new(OPENED_PATH),
        Opening {
            method,
            at_ns: tilo_clock::unix_now_ns(),
        },
    )
}

/// [`note_opening`] at `path`, for a test's own runtime directory.
pub(crate) fn note_opening_at(path: &Path, opened: Opening) -> Result<()> {
    let bytes = opened.encode().context("how the volume opened")?;
    tilo_files::native::atomic::write_with_mode(path, &bytes, 0o644)
        .with_context(|| format!("writing {}", path.display()))
}

pub struct BootUnlock<'a> {
    pub runner: &'a dyn Runner,
    pub ws: &'a Workspace,
    pub device: &'a Path,
    pub volume: &'a str,
    pub uid: Option<u32>,
    pub keys_dir: &'a Path,
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
                        return self.finish(token, &key, UnlockMethod::RecoveryKey);
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
                    return self.finish(token, &key, UnlockMethod::Passphrase);
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
                Some(Outcome::Key(key)) => return self.finish(token, &key, UnlockMethod::Tpm),
                Some(Outcome::Wrong(tries)) => BootOutcome::Wrong(tries),
                None => BootOutcome::Wrong(None),
                Some(Outcome::LockedOut(tries)) => BootOutcome::LockedOut(tries),
            },
        )
    }

    fn finish(
        &self,
        token: &SigninToken,
        key: &UserKey,
        method: UnlockMethod,
    ) -> Result<BootOutcome> {
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
        // The volume is open: how it opened is noted before anything else
        // uses the key, and a note that cannot be written does not stop the
        // sign-in.
        if let Err(error) = note_opening(method) {
            eprintln!("tilo-auth: the volume opens, but how it did is not noted ({error:#})");
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn the_note_says_which_way_the_volume_opened_and_when() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("tilo-unlock.json");
        note_opening_at(
            &path,
            Opening {
                method: UnlockMethod::SiblingServer,
                at_ns: 1_700_000_000_000_000_000,
            },
        )
        .unwrap();
        let read = Opening::decode(&std::fs::read(&path).unwrap()).expect("the boot's note");
        assert_eq!(read.method, UnlockMethod::SiblingServer);
        assert_eq!(read.at_ns, 1_700_000_000_000_000_000);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o644,
            "root owns it and it holds no secret, so the server reads it"
        );
        // A later boot's note replaces this one; nothing piles beside it.
        note_opening_at(
            &path,
            Opening {
                method: UnlockMethod::Tpm,
                at_ns: 1_700_000_000_000_000_001,
            },
        )
        .unwrap();
        let read = Opening::decode(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(read.method, UnlockMethod::Tpm);
    }
}
