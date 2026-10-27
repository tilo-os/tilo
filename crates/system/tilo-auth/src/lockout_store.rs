//! The TPM's dictionary-attack reset authorisation, wrapped by the admin's
//! user key and by the machine's recovery key instead of lying in a file on
//! the system volume (docs/security/sign-in.md §6.4). A compromise of the
//! running system before anyone signs in can read that file and reset the
//! counter after every batch of guesses; it cannot open these wrappings.
//!
//! The file tilo-rollback wrote is adopted, not deleted on sight: it goes only
//! once both wrappings exist, so the counter can always be reset with
//! whichever of the two keys is at hand. A TPM Windows manages never had that
//! file and is left alone.

use crate::recovery::RecoveryKey;
use crate::userkey::{UserKey, hmac_sha256};
use anyhow::{Context as _, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tilo_files::native::migration::Format;
use tilo_seal::{Context, Label, SymmetricKey, envelope};
use zeroize::Zeroizing;

pub const PATH: &str = "/var/lib/tilo/auth/lockout.json";
pub const LEGACY_PATH: &str = "/var/lib/tilo/rollback/lockout-auth";
const DOMAIN: &[u8] = b"tilo-tpm-lockout-v1";

pub const LOCKOUT_FORMAT: Format = Format {
    name: "TPM lockout authorisation",
    version: 1,
    oldest: 1,
    steps: &[],
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "kebab-case")]
enum Wrapping {
    User { uid: u32, envelope: String },
    Recovery { envelope: String },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Stored {
    version: u32,
    wrappings: Vec<Wrapping>,
}

/// A key that can open a wrapping.
pub enum Opener<'a> {
    User { uid: u32, key: &'a UserKey },
    Recovery(&'a RecoveryKey),
}

impl Opener<'_> {
    fn kek(&self) -> SymmetricKey {
        match self {
            Self::User { key, .. } => SymmetricKey::from_bytes(*key.derive(DOMAIN)),
            Self::Recovery(recovery) => {
                SymmetricKey::from_bytes(*hmac_sha256(recovery.expose(), DOMAIN))
            }
        }
    }

    fn record(&self) -> Vec<u8> {
        match self {
            Self::User { uid, .. } => format!("user:{uid}").into_bytes(),
            Self::Recovery(_) => b"recovery".to_vec(),
        }
    }

    fn matches(&self, wrapping: &Wrapping) -> bool {
        match (self, wrapping) {
            (Self::User { uid, .. }, Wrapping::User { uid: stored, .. }) => uid == stored,
            (Self::Recovery(_), Wrapping::Recovery { .. }) => true,
            _ => false,
        }
    }

    fn wrap(&self, auth: &[u8]) -> Result<Wrapping> {
        let record = self.record();
        let sealed = STANDARD.encode(
            envelope::seal(
                &self.kek(),
                Context::new(Label::TpmLockoutAuth, &record),
                auth,
            )
            .context("wrapping the lockout authorisation")?,
        );
        Ok(match self {
            Self::User { uid, .. } => Wrapping::User {
                uid: *uid,
                envelope: sealed,
            },
            Self::Recovery(_) => Wrapping::Recovery { envelope: sealed },
        })
    }

    fn open(&self, wrapping: &Wrapping) -> Result<Option<Zeroizing<Vec<u8>>>> {
        if !self.matches(wrapping) {
            return Ok(None);
        }
        let (Wrapping::User {
            envelope: sealed, ..
        }
        | Wrapping::Recovery { envelope: sealed }) = wrapping;
        let bytes = STANDARD.decode(sealed).context("a lockout wrapping")?;
        let record = self.record();
        match envelope::open(
            &self.kek(),
            Context::new(Label::TpmLockoutAuth, &record),
            &bytes,
        ) {
            Ok(auth) => Ok(Some(auth)),
            Err(tilo_seal::Error::WrongKey | tilo_seal::Error::Authentication) => Ok(None),
            Err(e) => Err(e).context("opening a lockout wrapping"),
        }
    }
}

pub struct LockoutStore {
    path: PathBuf,
    legacy: PathBuf,
}

/// What adopting did.
#[derive(Debug, PartialEq, Eq)]
pub enum Adopted {
    /// There is no authorisation Tilo knows of.
    Nothing,
    /// Wrapped for this key; the file stays until the other key is there too.
    Wrapped,
    /// Both wrappings exist and the file is gone.
    Complete,
}

impl LockoutStore {
    pub fn new(path: impl Into<PathBuf>, legacy: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            legacy: legacy.into(),
        }
    }

    fn load(&self) -> Result<Stored> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Stored {
                    version: LOCKOUT_FORMAT.version,
                    wrappings: Vec::new(),
                });
            }
            Err(e) => return Err(e).with_context(|| format!("reading {}", self.path.display())),
        };
        let stored: Stored = serde_json::from_slice(&bytes)
            .with_context(|| format!("reading {}", self.path.display()))?;
        let current = LOCKOUT_FORMAT
            .bring_up(&self.path, bytes, stored.version)
            .with_context(|| format!("bringing {} up to date", self.path.display()))?;
        serde_json::from_slice(&current).with_context(|| format!("reading {}", self.path.display()))
    }

    fn save(&self, stored: &Stored) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(stored).context("encoding the lockout wrappings")?;
        tilo_files::native::atomic::write_private(&self.path, &bytes)
            .with_context(|| format!("writing {}", self.path.display()))
    }

    /// The authorisation, opened with `opener`.
    pub fn open(&self, opener: &Opener<'_>) -> Result<Option<Zeroizing<Vec<u8>>>> {
        for wrapping in &self.load()?.wrappings {
            if let Some(auth) = opener.open(wrapping)? {
                return Ok(Some(auth));
            }
        }
        Ok(None)
    }

    /// Wrap the authorisation for `opener`: from tilo-rollback's file when
    /// that still exists, else from a wrapping `existing` opens.
    pub fn adopt(&self, opener: &Opener<'_>, existing: Option<&Opener<'_>>) -> Result<Adopted> {
        let mut stored = self.load()?;
        let auth = match read_legacy(&self.legacy)? {
            Some(auth) => Some(auth),
            None => match existing {
                Some(existing) => self.open(existing)?,
                None => None,
            },
        };
        let Some(auth) = auth else {
            return Ok(Adopted::Nothing);
        };
        let present = match stored.wrappings.iter().find(|w| opener.matches(w)) {
            Some(wrapping) => match opener.open(wrapping)? {
                Some(held) if held.as_slice() == auth.as_slice() => true,
                _ => bail!(
                    "{} already holds a different lockout authorisation for this key; leaving \
                     both alone",
                    self.path.display()
                ),
            },
            None => false,
        };
        if !present {
            stored.wrappings.push(opener.wrap(&auth)?);
            stored.version = LOCKOUT_FORMAT.version;
            self.save(&stored)?;
        }

        let has_user = stored
            .wrappings
            .iter()
            .any(|w| matches!(w, Wrapping::User { .. }));
        let has_recovery = stored
            .wrappings
            .iter()
            .any(|w| matches!(w, Wrapping::Recovery { .. }));
        if !(has_user && has_recovery) {
            return Ok(Adopted::Wrapped);
        }
        match std::fs::remove_file(&self.legacy) {
            Ok(()) => Ok(Adopted::Complete),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Adopted::Complete),
            Err(e) => Err(e).with_context(|| format!("removing {}", self.legacy.display())),
        }
    }
}

fn read_legacy(path: &Path) -> Result<Option<Zeroizing<Vec<u8>>>> {
    match std::fs::read(path) {
        Ok(bytes) if bytes.is_empty() => Ok(None),
        Ok(bytes) => Ok(Some(Zeroizing::new(bytes))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_goes_only_once_both_keys_can_open_the_authorisation() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("rollback/lockout-auth");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, b"0123456789abcdef0123456789abcdef").unwrap();
        let store = LockoutStore::new(dir.path().join("auth/lockout.json"), &legacy);
        let key = UserKey::generate().unwrap();
        let admin = Opener::User {
            uid: 1000,
            key: &key,
        };
        let recovery_key = RecoveryKey::from_bytes([5; 32]);
        let recovery = Opener::Recovery(&recovery_key);

        assert_eq!(store.adopt(&admin, None).unwrap(), Adopted::Wrapped);
        assert!(legacy.exists());
        assert_eq!(
            store.open(&admin).unwrap().unwrap().as_slice(),
            b"0123456789abcdef0123456789abcdef"
        );
        assert!(store.open(&recovery).unwrap().is_none());

        assert_eq!(store.adopt(&recovery, None).unwrap(), Adopted::Complete);
        assert!(!legacy.exists());
        assert_eq!(
            store.open(&recovery).unwrap().unwrap().as_slice(),
            b"0123456789abcdef0123456789abcdef"
        );
        let stranger_key = UserKey::generate().unwrap();
        assert!(
            store
                .open(&Opener::User {
                    uid: 1000,
                    key: &stranger_key
                })
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn without_the_file_a_new_key_is_wrapped_from_an_existing_one() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("lockout-auth");
        std::fs::write(&legacy, b"secret-auth").unwrap();
        let store = LockoutStore::new(dir.path().join("lockout.json"), &legacy);
        let key = UserKey::generate().unwrap();
        let admin = Opener::User {
            uid: 1000,
            key: &key,
        };
        store.adopt(&admin, None).unwrap();
        std::fs::remove_file(&legacy).unwrap();

        let recovery_key = RecoveryKey::from_bytes([6; 32]);
        let recovery = Opener::Recovery(&recovery_key);
        assert_eq!(store.adopt(&recovery, None).unwrap(), Adopted::Nothing);
        assert_eq!(
            store.adopt(&recovery, Some(&admin)).unwrap(),
            Adopted::Complete
        );
        assert_eq!(
            store.open(&recovery).unwrap().unwrap().as_slice(),
            b"secret-auth"
        );
    }

    #[test]
    fn a_machine_without_the_file_has_nothing_to_adopt() {
        let dir = tempfile::tempdir().unwrap();
        let store = LockoutStore::new(dir.path().join("lockout.json"), dir.path().join("none"));
        let key = UserKey::generate().unwrap();
        assert_eq!(
            store
                .adopt(&Opener::User { uid: 1, key: &key }, None)
                .unwrap(),
            Adopted::Nothing
        );
        assert!(!dir.path().join("lockout.json").exists());
    }
}
