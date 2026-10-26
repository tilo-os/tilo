//! Moving people off the old password database, transparently: the first
//! time someone's old password is accepted, the secret they just typed wraps
//! a new user key and becomes the only way their secret is checked.
//!
//! Nothing here deletes or rewrites a keyslot, a token or user data it did
//! not create, or changes a uid. The old disk passphrase is neither asked
//! for nor removed.

use super::volume::{self, SystemVolume};
use super::{Daemon, Work};
use crate::accounts::Account;
use crate::lockout_store::Opener;
use crate::luks::{self, SigninToken};
use crate::protocol::{Refusal, Reply};
use crate::record::{Attention, Record, SecretKind};
use crate::secret::Secret;
use crate::strength;
use crate::tpm;
use crate::userkey::UserKey;
use crate::wrapper::{Outcome, Wrapper};
use anyhow::{Result, bail};

impl Daemon {
    /// Check `typed` against the old password database, and on success move
    /// the person onto a user key. A failure to move them does not fail
    /// their sign-in: their old password stays their way in until it works.
    pub(super) fn verify_legacy(
        &self,
        work: &Work,
        account: &Account,
        typed: &str,
        secret: &Secret,
    ) -> Result<Reply> {
        if secret.is_empty() || !self.legacy.verify(&account.name, typed)? {
            return Ok(Reply::refused(Refusal::Wrong));
        }
        if let Err(e) = self.enroll(work, account, typed, secret) {
            eprintln!(
                "tilo-authd: uid {} signed in with their old password and stays on it: {e:#}",
                account.uid
            );
        }
        Ok(Reply::success())
    }

    fn enroll(&self, work: &Work, account: &Account, typed: &str, secret: &Secret) -> Result<()> {
        let mut record = self
            .store
            .load(account.uid)?
            .unwrap_or_else(|| Record::new(account.uid, &account.name, &account.display_name));
        if record.enrolled() {
            return Ok(());
        }
        record.name.clone_from(&account.name);
        record.display_name.clone_from(&account.display_name);
        let key = UserKey::generate()?;
        let wrapper = if tpm::usable(&work.runner) {
            Wrapper::tpm(work.tpm(), &key, secret)?
        } else if strength::passphrase_is_strong(secret) {
            Wrapper::passphrase(&key, account.uid, secret, self.cfg.passphrase_params)?
        } else {
            record.attention = Some(Attention::WeakSecret);
            self.store.save(&record)?;
            eprintln!(
                "tilo-authd: uid {} stays on their old password: no TPM here, and it is too weak \
                 to protect their data alone",
                account.uid
            );
            return Ok(());
        };
        if !matches!(
            wrapper.open_with_secret(Some(work.tpm()), account.uid, secret)?,
            Some(Outcome::Key(ref opened)) if opened.expose() == key.expose()
        ) {
            bail!("the new wrapper does not open with the secret it was made from");
        }
        record.set_secret_wrapper(wrapper);
        record.secret = Some(SecretKind::of(secret));
        record.attention = None;
        self.store.save(&record)?;
        eprintln!(
            "tilo-authd: uid {} signs in with their user key from now on",
            account.uid
        );
        if let Err(e) = self.legacy.lock_password(&account.name) {
            eprintln!(
                "tilo-authd: could not remove uid {}'s old password hash: {e:#}",
                account.uid
            );
        }
        self.after_sign_in(work, account, record, &key, Some(typed));
        Ok(())
    }

    /// What a successful sign-in can finish: the admin's system-volume
    /// keyslot and the wrapping of the TPM lockout authorisation.
    pub(super) fn after_sign_in(
        &self,
        work: &Work,
        account: &Account,
        mut record: Record,
        key: &UserKey,
        typed: Option<&str>,
    ) {
        match self.accounts.is_admin(account) {
            Ok(true) => {}
            Ok(false) => return,
            Err(e) => {
                eprintln!("tilo-authd: {e:#}");
                return;
            }
        }
        // Whether this sign-in unlocks the system is a fact about the volume,
        // settled on every sign-in, so a volume found later still gets the
        // admin's keyslot.
        if let Some(volume) = self.cfg.volume.find(&work.runner) {
            record.unlocks_system = true;
            if record.system_keyslot.is_none() {
                self.add_system_slot(work, &volume, &mut record, key, typed);
            } else if let Err(e) = self.store.save(&record) {
                eprintln!("tilo-authd: {e:#}");
            }
            self.mirror(work, &record);
        } else if let Some(slot) = record.system_keyslot {
            eprintln!(
                "tilo-authd: found no LUKS2 volume, though uid {} holds keyslot {slot} on one",
                account.uid
            );
        } else {
            eprintln!(
                "tilo-authd: found no LUKS2 volume under /sysroot, / or the initrd's root \
                 device; uid {} gets no system keyslot",
                account.uid
            );
            if record.unlocks_system {
                record.unlocks_system = false;
                if let Err(e) = self.store.save(&record) {
                    eprintln!("tilo-authd: {e:#}");
                }
            }
        }
        self.adopt_lockout(
            &Opener::User {
                uid: account.uid,
                key,
            },
            None,
        );
    }

    fn add_system_slot(
        &self,
        work: &Work,
        volume: &SystemVolume,
        record: &mut Record,
        key: &UserKey,
        typed: Option<&str>,
    ) {
        let passphrase = key.system_volume_passphrase();
        let slot =
            match self.find_or_add_slot(work, volume, record.uid, passphrase.as_bytes(), typed) {
                Ok(slot) => slot,
                Err(e) => {
                    eprintln!(
                        "tilo-authd: could not add uid {}'s keyslot to {}: {e:#}",
                        record.uid,
                        volume.device.display()
                    );
                    None
                }
            };
        record.system_keyslot = slot;
        record.attention = slot.is_none().then_some(Attention::SystemKeyslotPending);
        if let Err(e) = self.store.save(record) {
            eprintln!("tilo-authd: {e:#}");
        }
        if let Some(slot) = slot {
            eprintln!(
                "tilo-authd: uid {} unlocks {} with keyslot {slot}",
                record.uid,
                volume.device.display()
            );
        }
    }

    fn find_or_add_slot(
        &self,
        work: &Work,
        volume: &SystemVolume,
        uid: u32,
        passphrase: &[u8],
        typed: Option<&str>,
    ) -> Result<Option<u32>> {
        for (_, token) in luks::tokens(&work.runner, &volume.device)? {
            if token.uid == uid
                && let Some(slot) = token.keyslot()
                && luks::opens(&work.runner, &work.ws, &volume.device, passphrase, slot)?
            {
                return Ok(Some(slot));
            }
        }
        let Some(held) = volume::held_credential(
            &work.runner,
            &work.ws,
            volume,
            &self.cfg.proof_file,
            self.cfg.volume_key.as_deref(),
            typed,
        )?
        else {
            eprintln!(
                "tilo-authd: nothing this boot holds opens {}; uid {uid}'s keyslot waits for a \
                 later sign-in",
                volume.device.display()
            );
            return Ok(None);
        };
        luks::add_slot(
            &work.runner,
            &work.ws,
            &volume.device,
            &held.authority(),
            passphrase,
        )
        .map(Some)
    }

    /// Keep the person's `tilo-signin` token in step with their record.
    pub(super) fn mirror(&self, work: &Work, record: &Record) {
        let Some(slot) = record.system_keyslot else {
            return;
        };
        let Some(volume) = self.cfg.volume.find(&work.runner) else {
            return;
        };
        let wanted = SigninToken::mirror(record, slot);
        let result = luks::tokens(&work.runner, &volume.device).and_then(|tokens| {
            if tokens.iter().any(|(_, t)| *t == wanted) {
                return Ok(());
            }
            luks::write_token(&work.runner, &work.ws, &volume.device, &wanted).map(|_| ())
        });
        if let Err(e) = result {
            eprintln!(
                "tilo-authd: could not update uid {}'s sign-in token: {e:#}",
                record.uid
            );
        }
    }
}
