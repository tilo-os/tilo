//! The copies of a user key, each wrapped by one factor.

use crate::recovery::RecoveryKey;
use crate::secret::Secret;
use crate::tpm::{self, Tries};
use crate::userkey::{UserKey, hmac_sha256};
use anyhow::{Context as _, Result};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use tilo_rollback::exec::Runner;
use tilo_rollback::policy::Workspace;
use tilo_seal::envelope::{self, PasswordParams};
use tilo_seal::{Context, Label, SymmetricKey};

const RECOVERY_DOMAIN: &[u8] = b"tilo-recovery-v1";

/// The passphrase wrapper's cost: 256 MiB, three passes, one lane. A
/// passphrase only exists where no TPM throttles guesses, so each one has to
/// cost an attacker real memory and time.
pub const PASSPHRASE_PARAMS: PasswordParams = PasswordParams {
    m_cost_kib: 256 * 1024,
    t_cost: 3,
    p_cost: 1,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Wrapper {
    Tpm { public: String, private: String },
    Passphrase { envelope: String },
    Recovery { envelope: String },
}

/// A wrapper as stored: kinds this build does not know are kept verbatim, so
/// rewriting a record never drops a factor a newer version enrolled.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Entry {
    Known(Wrapper),
    Unknown(serde_json::Value),
}

impl Entry {
    pub fn known(&self) -> Option<&Wrapper> {
        match self {
            Self::Known(w) => Some(w),
            Self::Unknown(_) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Tpm,
    Passphrase,
    Recovery,
}

pub enum Outcome {
    Key(UserKey),
    Wrong(Option<Tries>),
    LockedOut(Option<Tries>),
}

#[derive(Clone, Copy)]
pub struct Tpm<'a> {
    pub runner: &'a dyn Runner,
    pub ws: &'a Workspace,
}

fn record(uid: u32) -> [u8; 4] {
    uid.to_le_bytes()
}

fn recovery_kek(recovery: &RecoveryKey) -> SymmetricKey {
    SymmetricKey::from_bytes(*hmac_sha256(recovery.expose(), RECOVERY_DOMAIN))
}

impl Wrapper {
    pub fn kind(&self) -> Kind {
        match self {
            Self::Tpm { .. } => Kind::Tpm,
            Self::Passphrase { .. } => Kind::Passphrase,
            Self::Recovery { .. } => Kind::Recovery,
        }
    }

    pub fn tpm(tpm: Tpm<'_>, key: &UserKey, secret: &Secret) -> Result<Self> {
        let sealed = tpm::seal(tpm.runner, tpm.ws, key, &secret.auth_value())?;
        Ok(Self::Tpm {
            public: STANDARD.encode(sealed.public),
            private: STANDARD.encode(sealed.private),
        })
    }

    pub fn passphrase(
        key: &UserKey,
        uid: u32,
        secret: &Secret,
        params: PasswordParams,
    ) -> Result<Self> {
        let rid = record(uid);
        let sealed = envelope::seal_with_password(
            secret.as_bytes(),
            params,
            Context::new(Label::SignInPassphrase, &rid),
            key.expose(),
        )
        .context("wrapping the user key under the passphrase")?;
        Ok(Self::Passphrase {
            envelope: STANDARD.encode(sealed),
        })
    }

    pub fn recovery(key: &UserKey, uid: u32, recovery: &RecoveryKey) -> Result<Self> {
        let rid = record(uid);
        let sealed = envelope::wrap_key(
            &recovery_kek(recovery),
            Context::new(Label::SignInRecovery, &rid),
            &key.as_symmetric(),
        )
        .context("wrapping the user key under the recovery key")?;
        Ok(Self::Recovery {
            envelope: STANDARD.encode(sealed),
        })
    }

    /// Offer a typed secret. `None` for a wrapper a secret cannot open (the
    /// recovery wrapper), or a TPM wrapper on a machine with no TPM access.
    pub fn open_with_secret(
        &self,
        tpm: Option<Tpm<'_>>,
        uid: u32,
        secret: &Secret,
    ) -> Result<Option<Outcome>> {
        match self {
            Self::Tpm { public, private } => {
                let Some(tpm) = tpm else { return Ok(None) };
                let sealed = tpm::Sealed {
                    public: STANDARD
                        .decode(public)
                        .context("a TPM wrapper's public blob")?,
                    private: STANDARD
                        .decode(private)
                        .context("a TPM wrapper's private blob")?,
                };
                Ok(Some(
                    match tpm::unseal(tpm.runner, tpm.ws, &sealed, &secret.auth_value())? {
                        tpm::Unseal::Key(key) => Outcome::Key(key),
                        tpm::Unseal::Wrong(tries) => Outcome::Wrong(tries),
                        tpm::Unseal::LockedOut(tries) => Outcome::LockedOut(tries),
                    },
                ))
            }
            Self::Passphrase { envelope: sealed } => {
                let bytes = STANDARD.decode(sealed).context("a passphrase wrapper")?;
                let rid = record(uid);
                match envelope::open_with_password(
                    secret.as_bytes(),
                    Context::new(Label::SignInPassphrase, &rid),
                    &bytes,
                ) {
                    Ok(plain) => Ok(Some(Outcome::Key(
                        UserKey::from_slice(&plain).context("a passphrase wrapper's contents")?,
                    ))),
                    Err(tilo_seal::Error::Authentication) => Ok(Some(Outcome::Wrong(None))),
                    Err(e) => Err(e).context("opening a passphrase wrapper"),
                }
            }
            Self::Recovery { .. } => Ok(None),
        }
    }

    pub fn open_with_recovery(&self, uid: u32, recovery: &RecoveryKey) -> Result<Option<UserKey>> {
        let Self::Recovery { envelope: sealed } = self else {
            return Ok(None);
        };
        let bytes = STANDARD.decode(sealed).context("a recovery wrapper")?;
        let rid = record(uid);
        match envelope::unwrap_key(
            &recovery_kek(recovery),
            Context::new(Label::SignInRecovery, &rid),
            &bytes,
        ) {
            Ok(key) => Ok(Some(UserKey::from_bytes(*key.expose_secret()))),
            Err(tilo_seal::Error::WrongKey | tilo_seal::Error::Authentication) => Ok(None),
            Err(e) => Err(e).context("opening a recovery wrapper"),
        }
    }
}
