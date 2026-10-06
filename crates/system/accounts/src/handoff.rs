//! The narrowly scoped credential **handoff** from Settings to the storage
//! daemon.
//!
//! Settings keeps custody of every long-lived secret in the
//! [`crate::SecretVault`]: passwords, `OAuth` refresh tokens, `iCloud` trust
//! tokens. The daemon that mounts a cloud account never opens that vault.
//! It reads exactly one account's entry from this separate store, which only
//! ever holds what a single request needs:
//!
//! - a short-lived `OAuth` access token with its expiry,
//! - short-lived web session headers with their expiry, or
//! - for plain `WebDAV` and Nextcloud, the Basic credential the server itself
//!   requires (a Nextcloud app password minted for this device by Login Flow
//!   v2, never the account password).
//!
//! Each account is its own sealed file (`accounts/handoff/<hex id>.enc`,
//! XChaCha20-Poly1305 under this store's own key), so reading one account
//! decrypts nothing of another's.
//!
//! When the entry is missing, expired, or the server rejected it,
//! [`HandoffCredentialSource`] asks a [`CredentialRefresher`] to publish a
//! fresh one. The production refresher runs Settings' credential helper as a
//! separate process ([`CommandRefresher`]); refresh tokens stay inside it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use secret_vault::VaultFormat;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::{AccountId, SecretCustody};

const FORMAT: VaultFormat = VaultFormat {
    name: "account-handoff",
    // Tilo Account HandOff.
    magic: b"QAHO",
    secret_service_label: "Tilo account credential handoff",
    secret_service_attributes: &[("application", "Tilo"), ("purpose", "account-handoff")],
    dpapi_entropy: b"Tilo account handoff key v1",
};

/// A credential for one account's requests.
pub enum HandoffCredential {
    /// The server's own Basic credential (`WebDAV`, a Nextcloud app password).
    Basic {
        username: String,
        password: Zeroizing<String>,
    },
    /// An `OAuth` access token. Never a refresh token.
    Bearer {
        token: Zeroizing<String>,
        expires_unix: Option<u64>,
    },
    /// Web session headers (cookies, session tokens). Never a password or a
    /// long-lived trust token.
    Session {
        headers: Vec<(String, Zeroizing<String>)>,
        expires_unix: Option<u64>,
    },
}

impl std::fmt::Debug for HandoffCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Basic { username, .. } => f
                .debug_struct("Basic")
                .field("username", username)
                .finish_non_exhaustive(),
            Self::Bearer { expires_unix, .. } => f
                .debug_struct("Bearer")
                .field("expires_unix", expires_unix)
                .finish_non_exhaustive(),
            Self::Session {
                headers,
                expires_unix,
            } => f
                .debug_struct("Session")
                .field(
                    "headers",
                    &headers.iter().map(|(name, _)| name).collect::<Vec<_>>(),
                )
                .field("expires_unix", expires_unix)
                .finish_non_exhaustive(),
        }
    }
}

impl HandoffCredential {
    /// When the credential stops working, `None` if it does not expire.
    pub fn expires_unix(&self) -> Option<u64> {
        match self {
            Self::Basic { .. } => None,
            Self::Bearer { expires_unix, .. } | Self::Session { expires_unix, .. } => *expires_unix,
        }
    }

    /// Whether it expires within `skew` of `now`.
    pub fn is_expired(&self, now: u64, skew: u64) -> bool {
        self.expires_unix()
            .is_some_and(|expires| now.saturating_add(skew) >= expires)
    }

    /// The cloud source view of this credential.
    pub fn to_cloud(&self) -> cloud_source::Credential {
        let expires = |unix: Option<u64>| {
            unix.and_then(|secs| UNIX_EPOCH.checked_add(Duration::from_secs(secs)))
        };
        match self {
            Self::Basic { username, password } => cloud_source::Credential::Basic {
                username: username.clone(),
                password: password.clone(),
            },
            Self::Bearer {
                token,
                expires_unix,
            } => cloud_source::Credential::Bearer {
                token: token.clone(),
                expires: expires(*expires_unix),
            },
            Self::Session {
                headers,
                expires_unix,
            } => cloud_source::Credential::Session {
                headers: headers.clone(),
                expires: expires(*expires_unix),
            },
        }
    }
}

/// The on-disk shape. Kept private so only [`HandoffCredential`]'s three
/// variants can ever be written.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Stored {
    Basic {
        username: String,
        password: String,
    },
    Bearer {
        token: String,
        expires_unix: Option<u64>,
    },
    Session {
        headers: Vec<(String, String)>,
        expires_unix: Option<u64>,
    },
}

impl Stored {
    fn from_credential(credential: &HandoffCredential) -> Self {
        match credential {
            HandoffCredential::Basic { username, password } => Self::Basic {
                username: username.clone(),
                password: password.to_string(),
            },
            HandoffCredential::Bearer {
                token,
                expires_unix,
            } => Self::Bearer {
                token: token.to_string(),
                expires_unix: *expires_unix,
            },
            HandoffCredential::Session {
                headers,
                expires_unix,
            } => Self::Session {
                headers: headers
                    .iter()
                    .map(|(name, value)| (name.clone(), value.to_string()))
                    .collect(),
                expires_unix: *expires_unix,
            },
        }
    }

    fn into_credential(self) -> HandoffCredential {
        match self {
            Self::Basic { username, password } => HandoffCredential::Basic {
                username,
                password: Zeroizing::new(password),
            },
            Self::Bearer {
                token,
                expires_unix,
            } => HandoffCredential::Bearer {
                token: Zeroizing::new(token),
                expires_unix,
            },
            Self::Session {
                headers,
                expires_unix,
            } => HandoffCredential::Session {
                headers: headers
                    .into_iter()
                    .map(|(name, value)| (name, Zeroizing::new(value)))
                    .collect(),
                expires_unix,
            },
        }
    }
}

/// The per-account handoff files.
#[derive(Clone, Debug)]
pub struct HandoffStore {
    dir: PathBuf,
    custody: SecretCustody,
    config_root: Option<PathBuf>,
}

/// The handoff directory under a config root (`root/accounts/handoff`).
pub fn handoff_dir_in(config_root: &Path) -> PathBuf {
    crate::accounts_dir_in(config_root).join("handoff")
}

impl HandoffStore {
    /// The store at the default config root with the session's key custody.
    /// `None` when no config directory resolves.
    pub fn open() -> Option<Self> {
        crate::config_root().map(|root| Self {
            dir: handoff_dir_in(&root),
            custody: SecretCustody::for_session(),
            config_root: None,
        })
    }

    /// The store under an explicit config root, with the same key custody
    /// as [`crate::SecretVault::open_in`].
    pub fn open_in(config_root: &Path) -> Self {
        Self {
            dir: handoff_dir_in(config_root),
            custody: crate::vault::explicit_root_custody(),
            config_root: Some(config_root.to_path_buf()),
        }
    }

    /// The explicit config root this store was opened under, if any.
    pub fn config_root(&self) -> Option<&Path> {
        self.config_root.as_deref()
    }

    fn file(&self, id: &AccountId) -> PathBuf {
        self.dir.join(format!("{}.enc", hex::encode(id.as_str())))
    }

    /// Replace the account's handoff credential.
    ///
    /// # Errors
    /// A description of the key, encryption or write failure.
    pub fn publish(&self, id: &AccountId, credential: &HandoffCredential) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|error| format!("could not create the handoff directory: {error}"))?;
        let key = FORMAT
            .load_or_create_key(&self.dir, self.custody)
            .ok_or_else(|| "no handoff key is available".to_string())?;
        let plaintext = Zeroizing::new(
            serde_json::to_vec(&Stored::from_credential(credential))
                .map_err(|error| format!("could not serialize the handoff credential: {error}"))?,
        );
        let sealed = FORMAT
            .seal(&key, &plaintext)
            .ok_or_else(|| "handoff encryption failed".to_string())?;
        tilo_files::native::atomic::write_creating_parent_with_mode(&self.file(id), &sealed, 0o600)
            .map_err(|error| format!("could not write the handoff credential: {error}"))
    }

    /// The account's handoff credential, `None` when none is published.
    ///
    /// # Errors
    /// A description of a key or integrity failure.
    pub fn read(&self, id: &AccountId) -> Result<Option<HandoffCredential>, String> {
        let sealed = match std::fs::read(self.file(id)) {
            Ok(sealed) => sealed,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("could not read the handoff credential: {error}")),
        };
        let key = FORMAT
            .load_or_create_key(&self.dir, self.custody)
            .ok_or_else(|| "the handoff key is unavailable".to_string())?;
        let plaintext = FORMAT
            .open(&key, &sealed)
            .ok_or_else(|| "handoff integrity check failed".to_string())?;
        let stored: Stored = serde_json::from_slice(&plaintext)
            .map_err(|_| "the handoff credential is invalid".to_string())?;
        Ok(Some(stored.into_credential()))
    }

    /// Remove the account's handoff credential (sign-out, removal, a
    /// revoked grant).
    ///
    /// # Errors
    /// A description of the removal failure.
    pub fn withdraw(&self, id: &AccountId) -> Result<(), String> {
        match std::fs::remove_file(self.file(id)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("could not remove the handoff credential: {error}")),
        }
    }
}

/// Why a refresh did not publish a credential.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshError {
    /// The account needs an interactive sign-in in Settings.
    NeedsAuthentication,
    /// The provider could not be reached.
    Offline,
    /// Anything else; details are in Settings' account health.
    Failed,
}

/// Publishes a fresh handoff credential for one account.
pub trait CredentialRefresher: Send + Sync {
    /// Publish a credential that is valid now. `force` asks for a new one
    /// even if the stored one looks valid (the server rejected it).
    ///
    /// # Errors
    /// See [`RefreshError`].
    fn refresh(&self, id: &AccountId, force: bool) -> Result<(), RefreshError>;
}

/// Exit codes of the credential helper (`tilo-account-credential refresh`).
pub mod exit {
    pub const PUBLISHED: i32 = 0;
    pub const FAILED: i32 = 1;
    pub const NEEDS_AUTHENTICATION: i32 = 2;
    pub const OFFLINE: i32 = 3;
}

/// Runs Settings' credential helper in its own process:
/// `tilo-account-credential refresh <account id> [--force] [--config-root <dir>]`.
///
/// The helper opens the vault, refreshes, publishes the handoff entry and
/// exits; nothing secret crosses its standard streams.
pub struct CommandRefresher {
    program: PathBuf,
    config_root: Option<PathBuf>,
    timeout: Duration,
}

impl CommandRefresher {
    /// The helper binary name, looked up on `PATH` unless
    /// `TILO_ACCOUNT_CREDENTIAL_HELPER` names a path.
    pub const PROGRAM: &'static str = "tilo-account-credential";

    pub fn new(config_root: Option<PathBuf>) -> Self {
        Self {
            program: std::env::var_os("TILO_ACCOUNT_CREDENTIAL_HELPER")
                .map_or_else(|| PathBuf::from(Self::PROGRAM), PathBuf::from),
            config_root,
            timeout: Duration::from_secs(60),
        }
    }

    #[must_use]
    pub fn with_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.program = program.into();
        self
    }
}

impl CredentialRefresher for CommandRefresher {
    fn refresh(&self, id: &AccountId, force: bool) -> Result<(), RefreshError> {
        let mut args: Vec<std::ffi::OsString> = vec!["refresh".into(), id.as_str().into()];
        if force {
            args.push("--force".into());
        }
        if let Some(root) = &self.config_root {
            args.push("--config-root".into());
            args.push(root.clone().into_os_string());
        }
        let output = bounded_command::run_os(&self.program, &args, None, 4096, self.timeout)
            .map_err(|error| {
                tracing::warn!(%error, "accounts: credential helper did not run");
                RefreshError::Failed
            })?;
        match output.status.code() {
            Some(exit::PUBLISHED) => Ok(()),
            Some(exit::NEEDS_AUTHENTICATION) => Err(RefreshError::NeedsAuthentication),
            Some(exit::OFFLINE) => Err(RefreshError::Offline),
            _ => Err(RefreshError::Failed),
        }
    }
}

/// The daemon's [`cloud_source::CredentialSource`] for one account: reads
/// the handoff entry, and asks the refresher for a new one when it is
/// missing, about to expire, or was rejected.
pub struct HandoffCredentialSource {
    id: AccountId,
    store: HandoffStore,
    refresher: Arc<dyn CredentialRefresher>,
    rejected: AtomicBool,
}

/// Refresh this long before the published expiry.
const EXPIRY_SKEW_SECS: u64 = 60;

impl HandoffCredentialSource {
    pub fn new(
        id: AccountId,
        store: HandoffStore,
        refresher: Arc<dyn CredentialRefresher>,
    ) -> Self {
        Self {
            id,
            store,
            refresher,
            rejected: AtomicBool::new(false),
        }
    }

    fn current(&self) -> Option<HandoffCredential> {
        let now = tilo_clock::unix_now();
        match self.store.read(&self.id) {
            Ok(Some(credential)) if !credential.is_expired(now, EXPIRY_SKEW_SECS) => {
                Some(credential)
            }
            Ok(_) => None,
            Err(error) => {
                tracing::warn!(%error, "accounts: handoff credential unreadable");
                None
            }
        }
    }
}

impl cloud_source::CredentialSource for HandoffCredentialSource {
    fn credential(&self) -> cloud_source::Result<cloud_source::Credential> {
        let force = self.rejected.swap(false, Ordering::SeqCst);
        if !force && let Some(credential) = self.current() {
            return Ok(credential.to_cloud());
        }
        match self.refresher.refresh(&self.id, force) {
            Ok(()) => {}
            Err(RefreshError::Offline) => return Err(cloud_source::Error::Offline),
            Err(RefreshError::NeedsAuthentication | RefreshError::Failed) => {
                return Err(cloud_source::Error::Unauthorized);
            }
        }
        self.current()
            .map(|credential| credential.to_cloud())
            .ok_or(cloud_source::Error::Unauthorized)
    }

    fn rejected(&self) {
        self.rejected.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cloud_source::CredentialSource as _;
    use std::sync::Mutex;

    fn temp_root() -> PathBuf {
        std::env::temp_dir().join(format!("tilo-accounts-handoff-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn each_account_is_its_own_sealed_file() {
        let root = temp_root();
        let store = HandoffStore::open_in(&root);
        let a = AccountId::from("drive-a");
        let b = AccountId::from("drive/b@example");
        store
            .publish(
                &a,
                &HandoffCredential::Bearer {
                    token: Zeroizing::new("access-a".into()),
                    expires_unix: Some(10),
                },
            )
            .unwrap();
        store
            .publish(
                &b,
                &HandoffCredential::Basic {
                    username: "ada".into(),
                    password: Zeroizing::new("app-password".into()),
                },
            )
            .unwrap();
        let files: Vec<_> = std::fs::read_dir(handoff_dir_in(&root))
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "enc"))
            .collect();
        assert_eq!(files.len(), 2);
        for path in &files {
            let bytes = std::fs::read(path).unwrap();
            let text = String::from_utf8_lossy(&bytes);
            assert!(!text.contains("access-a") && !text.contains("app-password"));
        }
        match store.read(&a).unwrap() {
            Some(HandoffCredential::Bearer {
                token,
                expires_unix,
            }) => {
                assert_eq!(token.as_str(), "access-a");
                assert_eq!(expires_unix, Some(10));
            }
            other => panic!("unexpected {other:?}"),
        }
        store.withdraw(&a).unwrap();
        assert!(store.read(&a).unwrap().is_none());
        assert!(store.read(&b).unwrap().is_some());
        std::fs::remove_dir_all(root).unwrap();
    }

    struct Publishing {
        store: HandoffStore,
        calls: Mutex<Vec<bool>>,
        outcome: Result<(), RefreshError>,
    }

    impl CredentialRefresher for Publishing {
        fn refresh(&self, id: &AccountId, force: bool) -> Result<(), RefreshError> {
            let count = {
                let mut calls = self.calls.lock().unwrap();
                calls.push(force);
                calls.len()
            };
            self.outcome?;
            self.store
                .publish(
                    id,
                    &HandoffCredential::Bearer {
                        token: Zeroizing::new(format!("access-{count}")),
                        expires_unix: Some(u64::MAX),
                    },
                )
                .unwrap();
            Ok(())
        }
    }

    #[test]
    fn source_refreshes_when_missing_and_after_a_rejection() {
        let root = temp_root();
        let store = HandoffStore::open_in(&root);
        let refresher = Arc::new(Publishing {
            store: store.clone(),
            calls: Mutex::new(Vec::new()),
            outcome: Ok(()),
        });
        let source = HandoffCredentialSource::new(
            AccountId::from("drive"),
            store,
            Arc::clone(&refresher) as Arc<dyn CredentialRefresher>,
        );
        let bearer = |credential: cloud_source::Credential| match credential {
            cloud_source::Credential::Bearer { token, .. } => token.to_string(),
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(bearer(source.credential().unwrap()), "access-1");
        assert_eq!(bearer(source.credential().unwrap()), "access-1");
        source.rejected();
        assert_eq!(bearer(source.credential().unwrap()), "access-2");
        assert_eq!(*refresher.calls.lock().unwrap(), vec![false, true]);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn source_maps_refresh_failures() {
        let root = temp_root();
        let store = HandoffStore::open_in(&root);
        for (outcome, expected) in [
            (RefreshError::NeedsAuthentication, "sign-in required"),
            (RefreshError::Offline, "the server is unreachable"),
        ] {
            let source = HandoffCredentialSource::new(
                AccountId::from("drive"),
                store.clone(),
                Arc::new(Publishing {
                    store: store.clone(),
                    calls: Mutex::new(Vec::new()),
                    outcome: Err(outcome),
                }),
            );
            assert_eq!(source.credential().unwrap_err().to_string(), expected);
        }
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn expired_entries_are_not_handed_out() {
        let credential = HandoffCredential::Session {
            headers: vec![("Cookie".into(), Zeroizing::new("X-APPLE-WEBAUTH=1".into()))],
            expires_unix: Some(100),
        };
        assert!(credential.is_expired(41, 60));
        assert!(!credential.is_expired(39, 60));
        assert!(!format!("{credential:?}").contains("WEBAUTH"));
    }
}
