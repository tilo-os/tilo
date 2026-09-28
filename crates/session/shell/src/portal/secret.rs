//! Per-application master-secret portal.
//!
//! Secrets are kept in a user-private, mode-0600 local store.  Deployments
//! should place this state directory on Tilo's encrypted user volume (or replace
//! the backend with the Secret Service provider); the portal never places the
//! value in workspace state, diagnostics, or a D-Bus reply.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use getrandom::{SysRng, rand_core::UnwrapErr};
use rand_core::Rng;
use serde::{Deserialize, Serialize};
#[cfg(target_os = "linux")]
use zbus::interface;
#[cfg(target_os = "linux")]
use zbus::names::OwnedUniqueName;
#[cfg(target_os = "linux")]
use zbus::zvariant::{ObjectPath, OwnedFd, OwnedValue};
use zeroize::{Zeroize, Zeroizing};

use tilo_seal::{Context, Label, SymmetricKey};

#[cfg(target_os = "linux")]
use super::{PortalContext, auth::authenticate_call};

const SECRET_BYTES: usize = 32;
const MAX_APP_ID: usize = 255;
#[cfg(target_os = "linux")]
const MAX_TOKEN: usize = 512;
const STORE_RECORD: &[u8] = b"portal-secrets";

#[derive(Clone, Default, Serialize, Deserialize)]
struct SecretFile {
    values: BTreeMap<String, String>,
}

#[derive(Clone)]
pub struct SecretStore {
    state: Arc<Mutex<SecretFile>>,
    path: Arc<PathBuf>,
    key: Arc<Option<Zeroizing<[u8; SECRET_BYTES]>>>,
}
#[cfg(target_os = "linux")]
#[derive(Clone)]
pub struct SecretBackend {
    ctx: PortalContext,
    store: SecretStore,
}
#[cfg(target_os = "linux")]
impl std::ops::Deref for SecretBackend {
    type Target = SecretStore;
    fn deref(&self) -> &SecretStore {
        &self.store
    }
}
#[cfg(target_os = "linux")]
impl SecretBackend {
    pub fn new(ctx: PortalContext) -> Self {
        Self::at_path(ctx, default_path())
    }
    pub fn at_path(ctx: PortalContext, path: impl Into<PathBuf>) -> Self {
        Self {
            ctx,
            store: SecretStore::at_path(path),
        }
    }
}
impl SecretStore {
    pub fn at_path(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let key = load_or_create_key(&path);
        let state = key
            .as_deref()
            .and_then(|key| fs::read(&path).ok().and_then(|data| open_store(key, &data)))
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default();
        Self {
            state: Arc::new(Mutex::new(state)),
            path: Arc::new(path),
            key: Arc::new(key),
        }
    }
    fn secret_for(&self, app_id: &str) -> zbus::fdo::Result<[u8; SECRET_BYTES]> {
        valid_identifier(app_id, MAX_APP_ID, "application id")?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| failed("secret store lock poisoned"))?;
        if let Some(existing) = state.values.get(app_id) {
            return decode(existing);
        }
        let mut secret = [0_u8; SECRET_BYTES];
        UnwrapErr(SysRng).fill_bytes(&mut secret);
        state.values.insert(app_id.into(), hex::encode(secret));
        if let Err(error) = self.commit(&state) {
            state.values.remove(app_id);
            secret.zeroize();
            return Err(error);
        }
        Ok(secret)
    }
    fn commit(&self, state: &SecretFile) -> zbus::fdo::Result<()> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| failed("secret state path has no parent"))?;
        private_fs::directory(parent)
            .map_err(|e| failed(&format!("cannot protect secret directory: {e}")))?;
        let plaintext = Zeroizing::new(
            serde_json::to_vec(state)
                .map_err(|e| failed(&format!("cannot encode secret store: {e}")))?,
        );
        let key = self
            .key
            .as_deref()
            .ok_or_else(|| failed("secret encryption key is unavailable"))?;
        let sealed =
            seal_store(key, &plaintext).ok_or_else(|| failed("cannot encrypt secret store"))?;
        private_fs::write_atomic(self.path.as_ref(), &sealed)
            .map_err(|e| failed(&format!("cannot write secret store: {e}")))
    }
}

fn key_path(path: &std::path::Path) -> PathBuf {
    path.with_extension("key")
}

/// Load the store key, minting one only when none exists yet.
///
/// A key that is present but unreadable, malformed or not private is never
/// replaced: a new key would orphan every secret sealed under the old one.
fn load_or_create_key(path: &std::path::Path) -> Option<Zeroizing<[u8; SECRET_BYTES]>> {
    let key_path = key_path(path);
    match read_key(&key_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        existing => return existing.ok(),
    }
    private_fs::directory(key_path.parent()?).ok()?;
    let mut key = Zeroizing::new([0; SECRET_BYTES]);
    UnwrapErr(SysRng).fill_bytes(key.as_mut());
    // `false` means a concurrent starter published first; everyone uses the
    // key that won, so re-read rather than returning ours.
    private_fs::create_atomic(&key_path, key.as_ref()).ok()?;
    read_key(&key_path).ok()
}

fn read_key(path: &std::path::Path) -> std::io::Result<Zeroizing<[u8; SECRET_BYTES]>> {
    use std::io::Read;
    let mut bytes = Zeroizing::new(Vec::with_capacity(SECRET_BYTES));
    private_fs::read(path)?.read_to_end(&mut bytes)?;
    let mut key = Zeroizing::new([0; SECRET_BYTES]);
    if bytes.len() != SECRET_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "secret store key has the wrong length",
        ));
    }
    key.copy_from_slice(&bytes);
    Ok(key)
}

/// The store's binding: an envelope moved from another purpose or record
/// does not open here.
fn store_context() -> Context<'static> {
    Context::new(Label::PortalSecretStore, STORE_RECORD)
}

/// Seal the store as a `tilo-seal` at-rest envelope.
fn seal_store(key: &[u8; SECRET_BYTES], plaintext: &[u8]) -> Option<Vec<u8>> {
    tilo_seal::envelope::seal(&SymmetricKey::from_bytes(*key), store_context(), plaintext).ok()
}

/// Open a store sealed by [`seal_store`].
fn open_store(key: &[u8; SECRET_BYTES], bytes: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    tilo_seal::envelope::open(&SymmetricKey::from_bytes(*key), store_context(), bytes).ok()
}

#[cfg(target_os = "linux")]
#[interface(name = "org.freedesktop.impl.portal.Secret")]
impl SecretBackend {
    #[zbus(property, name = "version")]
    const fn version(&self) -> u32 {
        1
    }
    async fn retrieve_secret(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        _handle: ObjectPath<'_>,
        app_id: String,
        fd: OwnedFd,
        options: std::collections::HashMap<String, OwnedValue>,
    ) -> (u32, std::collections::HashMap<String, OwnedValue>) {
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        let Ok(auth) = authenticate_call(&self.ctx, sender.as_ref(), &app_id, None).await else {
            return (2, Default::default());
        };
        if options
            .get("token")
            .and_then(|value| value.try_clone().ok())
            .and_then(|value| String::try_from(value).ok())
            .is_some_and(|token| valid_identifier(&token, MAX_TOKEN, "token").is_err())
        {
            return (2, Default::default());
        }
        let mut secret = match self.secret_for(&auth.caller.app_id) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!("secret portal request rejected: {error}");
                return (2, Default::default());
            }
        };
        let std_fd: std::os::fd::OwnedFd = fd.into();
        let result = std::fs::File::from(std_fd).write_all(&secret);
        secret.zeroize();
        if result.is_ok() {
            (0, Default::default())
        } else {
            (2, Default::default())
        }
    }
}
#[cfg(target_os = "linux")]
fn default_path() -> PathBuf {
    super::private_state_root()
        .unwrap_or_else(|| PathBuf::from("/dev/null"))
        .join("tilo/portal/secrets.json")
}
fn failed(message: &str) -> zbus::fdo::Error {
    zbus::fdo::Error::Failed(message.into())
}
fn valid_identifier(value: &str, max: usize, label: &str) -> zbus::fdo::Result<()> {
    (!value.is_empty() && value.len() <= max && !value.contains('\0') && !value.contains('/'))
        .then_some(())
        .ok_or_else(|| zbus::fdo::Error::InvalidArgs(format!("invalid {label}")))
}
fn decode(encoded: &str) -> zbus::fdo::Result<[u8; SECRET_BYTES]> {
    let mut out = [0; SECRET_BYTES];
    let bytes = hex::decode(encoded).map_err(|_| failed("secret store is corrupted"))?;
    if bytes.len() != SECRET_BYTES {
        return Err(failed("secret store has an invalid secret"));
    }
    out.copy_from_slice(&bytes);
    Ok(out)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn secret_store_envelopes_open_only_under_the_store_binding() {
        {
            let key = [0x61; SECRET_BYTES];
            let resealed = seal_store(&key, b"kat-portal-store").unwrap();
            assert!(tilo_seal::envelope::is_envelope(&resealed));
            assert_eq!(
                open_store(&key, &resealed).unwrap().as_slice(),
                b"kat-portal-store"
            );
            assert!(open_store(&[0x62; SECRET_BYTES], &resealed).is_none());
            let foreign = tilo_seal::envelope::seal(
                &SymmetricKey::from_bytes(key),
                Context::new(Label::SecretVault, STORE_RECORD),
                b"kat-portal-store",
            )
            .unwrap();
            assert!(open_store(&key, &foreign).is_none());
        }
    }

    #[test]
    fn app_secrets_share_one_key_that_is_never_replaced() {
        // Phase `app_secret_persists_and_is_separate`.
        {
            let dir = tempfile::tempdir().unwrap();
            let p = dir.path().join("secrets.json");
            let a = SecretStore::at_path(&p);
            let one = a.secret_for("org.example.One").unwrap();
            assert_eq!(
                one,
                SecretStore::at_path(&p)
                    .secret_for("org.example.One")
                    .unwrap()
            );
            assert_ne!(one, a.secret_for("org.example.Two").unwrap());
            assert_eq!(
                fs::metadata(&p).unwrap().permissions().mode() & 0o777,
                0o600
            );
            let disk = fs::read(&p).unwrap();
            assert!(tilo_seal::envelope::is_envelope(&disk));
            assert!(!disk.windows(one.len()).any(|window| window == one));
            assert_eq!(
                fs::metadata(key_path(&p)).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        // Phase `existing_key_is_never_replaced`.
        {
            let dir = tempfile::tempdir().unwrap();
            let p = dir.path().join("secrets.json");
            let key = key_path(&p);

            // Public access: refused rather than trusted or rewritten.
            fs::write(&key, [7_u8; SECRET_BYTES]).unwrap();
            fs::set_permissions(&key, fs::Permissions::from_mode(0o644)).unwrap();
            assert!(load_or_create_key(&p).is_none());
            assert_eq!(fs::read(&key).unwrap(), [7_u8; SECRET_BYTES]);
            assert!(
                SecretStore::at_path(&p)
                    .secret_for("org.example.One")
                    .is_err()
            );
            assert_eq!(fs::read(&key).unwrap(), [7_u8; SECRET_BYTES]);

            // Wrong length: refused and left in place.
            fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
            fs::write(&key, [7_u8; 3]).unwrap();
            assert!(load_or_create_key(&p).is_none());
            assert_eq!(fs::read(&key).unwrap(), [7_u8; 3]);
        }
        // Phase `concurrent_first_starts_share_one_key`.
        {
            let dir = tempfile::tempdir().unwrap();
            let p = Arc::new(dir.path().join("secrets.json"));
            let keys: Vec<[u8; SECRET_BYTES]> = (0..8)
                .map(|_| {
                    let p = Arc::clone(&p);
                    std::thread::spawn(move || *load_or_create_key(&p).expect("key"))
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect();
            assert!(keys.iter().all(|key| *key == keys[0]));
            assert_eq!(fs::read(key_path(&p)).unwrap(), keys[0]);
        }
    }
}
