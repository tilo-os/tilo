//! Per-application master-secret portal.
//!
//! Secrets are kept in a user-private, mode-0600 local store.  Deployments
//! should place this state directory on qOS's encrypted user volume (or replace
//! the backend with the Secret Service provider); the portal never places the
//! value in workspace state, diagnostics, or a D-Bus reply.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use zbus::interface;
use zbus::names::OwnedUniqueName;
use zbus::zvariant::{ObjectPath, OwnedFd, OwnedValue};
use zeroize::{Zeroize, Zeroizing};

use chacha20poly1305::aead::Aead;
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};

use super::{PortalContext, auth::authenticate_call};

const SECRET_BYTES: usize = 32;
const MAX_APP_ID: usize = 255;
const MAX_TOKEN: usize = 512;
const NONCE_BYTES: usize = 24;
const STORE_MAGIC: &[u8; 5] = b"QPSV1";

#[derive(Clone, Default, Serialize, Deserialize)]
struct SecretFile {
    values: BTreeMap<String, String>,
}

#[derive(Clone)]
pub struct SecretBackend {
    ctx: PortalContext,
    state: Arc<Mutex<SecretFile>>,
    path: Arc<PathBuf>,
    key: Arc<Option<Zeroizing<[u8; SECRET_BYTES]>>>,
}
impl SecretBackend {
    pub fn new(ctx: PortalContext) -> Self {
        Self::at_path(ctx, default_path())
    }
    pub fn at_path(ctx: PortalContext, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let key = load_or_create_key(&path);
        let state = key
            .as_deref()
            .and_then(|key| fs::read(&path).ok().and_then(|data| open_store(key, &data)))
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default();
        Self {
            ctx,
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
        OsRng.fill_bytes(&mut secret);
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
        fs::create_dir_all(parent)
            .map_err(|e| failed(&format!("cannot create secret directory: {e}")))?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .map_err(|e| failed(&format!("cannot protect secret directory: {e}")))?;
        let temporary = self.path.with_extension("tmp");
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
        let mut file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|e| failed(&format!("cannot create secret store: {e}")))?;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|e| failed(&format!("cannot protect secret store: {e}")))?;
        file.write_all(&sealed)
            .and_then(|()| file.sync_all())
            .map_err(|e| failed(&format!("cannot write secret store: {e}")))?;
        fs::rename(&temporary, self.path.as_ref())
            .map_err(|e| failed(&format!("cannot commit secret store: {e}")))?;
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|e| failed(&format!("cannot sync secret store: {e}")))
    }
}

fn key_path(path: &std::path::Path) -> PathBuf {
    path.with_extension("key")
}

fn load_or_create_key(path: &std::path::Path) -> Option<Zeroizing<[u8; SECRET_BYTES]>> {
    let key_path = key_path(path);
    if let Ok(bytes) = fs::read(&key_path) {
        if bytes.len() != SECRET_BYTES {
            return None;
        }
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600)).ok()?;
        let mut key = Zeroizing::new([0; SECRET_BYTES]);
        key.copy_from_slice(&bytes);
        return Some(key);
    }
    let parent = key_path.parent()?;
    fs::create_dir_all(parent).ok()?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).ok()?;
    let mut key = Zeroizing::new([0; SECRET_BYTES]);
    OsRng.fill_bytes(key.as_mut());
    let temporary = key_path.with_extension("key.tmp");
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .ok()?;
    file.write_all(key.as_ref()).ok()?;
    file.sync_all().ok()?;
    fs::rename(temporary, &key_path).ok()?;
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .ok()?;
    Some(key)
}

fn seal_store(key: &[u8; SECRET_BYTES], plaintext: &[u8]) -> Option<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new_from_slice(key).ok()?;
    let mut nonce = [0; NONCE_BYTES];
    OsRng.fill_bytes(&mut nonce);
    let encrypted = cipher.encrypt(XNonce::from_slice(&nonce), plaintext).ok()?;
    let mut out = Vec::with_capacity(STORE_MAGIC.len() + nonce.len() + encrypted.len());
    out.extend_from_slice(STORE_MAGIC);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&encrypted);
    Some(out)
}

fn open_store(key: &[u8; SECRET_BYTES], bytes: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    let rest = bytes.strip_prefix(STORE_MAGIC)?;
    let (nonce, ciphertext) = rest.split_at_checked(NONCE_BYTES)?;
    XChaCha20Poly1305::new_from_slice(key)
        .ok()?
        .decrypt(XNonce::from_slice(nonce), ciphertext)
        .ok()
        .map(Zeroizing::new)
}

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
fn default_path() -> PathBuf {
    super::private_state_root()
        .unwrap_or_else(|| PathBuf::from("/dev/null"))
        .join("qos/portal/secrets.json")
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
    #[test]
    fn app_secret_persists_and_is_separate() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("secrets.json");
        let (grants, _) = crate::portal::GrantStoreHandle::test_with_workspace(
            crate::shell::state::Workspace::test_default(
                crate::shell::state::WorkspaceId(1),
                "test",
            ),
        );
        let ctx = PortalContext::new_with_null_spawners(
            grants,
            crate::portal::PromptQueueHandle::new(crate::portal::PromptResolver::AutoDeny),
            crate::portal::CallerMap::new(),
        );
        let a = SecretBackend::at_path(ctx.clone(), &p);
        let one = a.secret_for("org.example.One").unwrap();
        assert_eq!(
            one,
            SecretBackend::at_path(ctx, &p)
                .secret_for("org.example.One")
                .unwrap()
        );
        assert_ne!(one, a.secret_for("org.example.Two").unwrap());
        assert_eq!(
            fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let disk = fs::read(&p).unwrap();
        assert!(disk.starts_with(STORE_MAGIC));
        assert!(!disk.windows(one.len()).any(|window| window == one));
        assert_eq!(
            fs::metadata(key_path(&p)).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
