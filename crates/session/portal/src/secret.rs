//! Per-application master-secret portal.
//!
//! The secrets are kept by the person's Secret Service
//! (`tilo-secret-service`), the same store libsecret clients use, sealed
//! under the device identity. This backend only vouches for who is asking:
//! the portal frontend authenticates the app, the session names it, and
//! the store records and may revoke that application like any other. The value never enters workspace state,
//! diagnostics, or a D-Bus reply.
//!
//! Secrets an earlier build kept in a private file beside the compositor's
//! state are adopted by the store the first time their app asks, then
//! removed from that file, so what apps sealed under them stays readable.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::future::Future;
use std::io::Write;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tilo_seal::{Context, Label, SymmetricKey};
use zbus::interface;
use zbus::zvariant::{ObjectPath, OwnedFd, OwnedValue};
use zeroize::{Zeroize, Zeroizing};

use crate::Portal;

const SECRET_BYTES: usize = 32;
const MAX_TOKEN: usize = 512;
const STORE_RECORD: &[u8] = b"portal-secrets";
/// Where `tilo-secret-service` serves, and its management object.
const SECRET_SERVICE: &str = "org.freedesktop.secrets";
const SECRET_SERVICE_MANAGEMENT: &str = "/so/tilo/Secrets1";

type Answer<'a> =
    Pin<Box<dyn Future<Output = Result<Zeroizing<[u8; SECRET_BYTES]>, String>> + Send + 'a>>;

/// Where application master secrets come from.
pub trait SandboxSecrets: Send + Sync {
    /// The master secret of the sandboxed app `app`; `legacy` is the one an
    /// earlier store kept for it, if any.
    fn secret_for<'a>(&'a self, app: &'a str, legacy: Option<&'a [u8]>) -> Answer<'a>;
}

/// The person's Secret Service, asked over the session bus. Only the
/// session may ask it for these; the service checks.
pub struct ServiceSecrets {
    connection: zbus::Connection,
}
impl SandboxSecrets for ServiceSecrets {
    fn secret_for<'a>(&'a self, app: &'a str, legacy: Option<&'a [u8]>) -> Answer<'a> {
        Box::pin(async move {
            let service = zbus::Proxy::new(
                &self.connection,
                SECRET_SERVICE,
                SECRET_SERVICE_MANAGEMENT,
                "so.tilo.Secrets1",
            )
            .await
            .map_err(|error| error.to_string())?;
            let request = (app, legacy.unwrap_or_default());
            let ask = || service.call::<_, _, Vec<u8>>("SandboxSecret", &request);
            // The compositor holds the device key and so is non-dumpable:
            // the store cannot read its executable and refuses it until this
            // connection proves the device identity (once per connection,
            // and again after the store restarts).
            let bytes = match ask().await {
                Err(zbus::Error::MethodError(name, _, _))
                    if name.as_str() == "org.freedesktop.Secret.Error.AccessDenied"
                        && tilo_secrets::prove_device_caller(&self.connection)
                            .await
                            .unwrap_or(false) =>
                {
                    ask().await
                }
                answer => answer,
            }
            .map_err(|error| error.to_string())?;
            let bytes = Zeroizing::new(bytes);
            let mut secret = Zeroizing::new([0_u8; SECRET_BYTES]);
            if bytes.len() != secret.len() {
                return Err("the secret store returned a secret of the wrong length".into());
            }
            secret.copy_from_slice(&bytes);
            Ok(secret)
        })
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct SecretFile {
    values: BTreeMap<String, String>,
}

/// The file an earlier build kept these secrets in, read once per app and
/// emptied as the store adopts them.
pub struct LegacyFile {
    path: PathBuf,
    state: Mutex<SecretFile>,
    key: Option<Zeroizing<[u8; SECRET_BYTES]>>,
}

impl LegacyFile {
    pub fn at_path(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        // Never mints: with no earlier key there is nothing to adopt.
        let key = read_key(&key_path(&path)).ok();
        let state = key
            .as_deref()
            .and_then(|key| fs::read(&path).ok().and_then(|data| open_store(key, &data)))
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default();
        Self {
            path,
            state: Mutex::new(state),
            key,
        }
    }

    fn find(&self, app: &str) -> Option<Zeroizing<Vec<u8>>> {
        let encoded = Zeroizing::new(self.state.lock().ok()?.values.get(app)?.clone());
        let bytes = hex::decode(encoded.as_str()).ok()?;
        (bytes.len() == SECRET_BYTES).then(|| Zeroizing::new(bytes))
    }

    /// Drop `app`'s secret from the file once the store holds it. A file
    /// left with nothing is deleted, key included.
    fn adopted(&self, app: &str) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.values.remove(app).is_none() {
            return;
        }
        if state.values.is_empty() {
            let _ = fs::remove_file(&self.path);
            let _ = fs::remove_file(key_path(&self.path));
            return;
        }
        let Some(key) = self.key.as_deref() else {
            return;
        };
        let plaintext = Zeroizing::new(serde_json::to_vec(&*state).unwrap_or_default());
        if let Some(sealed) = seal_store(key, &plaintext) {
            let _ = tilo_files::native::private::write_atomic(&self.path, &sealed);
        }
    }
}

/// Asks the store, handing it what the earlier file kept for the app.
pub struct PortalSecrets {
    store: Arc<dyn SandboxSecrets>,
    legacy: LegacyFile,
}

impl PortalSecrets {
    pub fn new(store: Arc<dyn SandboxSecrets>, legacy: LegacyFile) -> Self {
        Self { store, legacy }
    }

    async fn secret_for(&self, app: &str) -> Result<Zeroizing<[u8; SECRET_BYTES]>, String> {
        let earlier = self.legacy.find(app);
        let secret = self
            .store
            .secret_for(app, earlier.as_deref().map(Vec::as_slice))
            .await?;
        if earlier.is_some() {
            self.legacy.adopted(app);
        }
        Ok(secret)
    }
}

fn key_path(path: &std::path::Path) -> PathBuf {
    path.with_extension("key")
}

fn read_key(path: &std::path::Path) -> std::io::Result<Zeroizing<[u8; SECRET_BYTES]>> {
    use std::io::Read;
    let mut bytes = Zeroizing::new(Vec::with_capacity(SECRET_BYTES));
    tilo_files::native::private::read(path)?.read_to_end(&mut bytes)?;
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

/// The earlier store's binding.
fn store_context() -> Context<'static> {
    Context::new(Label::PortalSecretStore, STORE_RECORD)
}

fn seal_store(key: &[u8; SECRET_BYTES], plaintext: &[u8]) -> Option<Vec<u8>> {
    tilo_seal::envelope::seal(&SymmetricKey::from_bytes(*key), store_context(), plaintext).ok()
}

fn open_store(key: &[u8; SECRET_BYTES], bytes: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    tilo_seal::envelope::open(&SymmetricKey::from_bytes(*key), store_context(), bytes).ok()
}

/// Answer one request: the secret goes to `fd` and nowhere else.
/// Write `app`'s secret into `fd`: the response code.
async fn deliver(secrets: &PortalSecrets, app: &str, fd: std::os::fd::OwnedFd) -> u32 {
    let mut secret = match secrets.secret_for(app).await {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(%error, app, "the secret store refused an app's secret");
            return 2;
        }
    };
    let result = std::fs::File::from(fd).write_all(secret.as_ref());
    secret.zeroize();
    if result.is_ok() { 0 } else { 2 }
}

/// The `org.freedesktop.impl.portal.Secret` interface.
#[derive(Clone)]
pub struct Secret {
    portal: Portal,
    secrets: Arc<PortalSecrets>,
}

impl Secret {
    /// Secrets from the Secret Service on `connection`, adopting what the
    /// earlier store at `legacy` kept.
    #[must_use]
    pub fn new(portal: Portal, store: Arc<dyn SandboxSecrets>, legacy: PathBuf) -> Self {
        Self {
            portal,
            secrets: Arc::new(PortalSecrets::new(store, LegacyFile::at_path(legacy))),
        }
    }

    /// Secrets of the person's Secret Service on `connection`.
    #[must_use]
    pub fn on_session(portal: Portal, connection: zbus::Connection) -> Self {
        let legacy = crate::private_state_root()
            .unwrap_or_else(|| PathBuf::from("/dev/null"))
            .join("tilo/portal/secrets.json");
        Self::new(portal, Arc::new(ServiceSecrets { connection }), legacy)
    }
}

#[interface(name = "org.freedesktop.impl.portal.Secret")]
impl Secret {
    #[zbus(property, name = "version")]
    const fn version(&self) -> u32 {
        1
    }

    async fn retrieve_secret(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        handle: ObjectPath<'_>,
        app_id: String,
        fd: OwnedFd,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        let Ok(auth) = self
            .portal
            .authenticate(&header, handle.as_str(), &app_id, None)
            .await
        else {
            return (2, HashMap::new());
        };
        if crate::option::<String>(&options, "token")
            .is_some_and(|token| !valid_identifier(&token, MAX_TOKEN))
        {
            return (2, HashMap::new());
        }
        let fd: std::os::fd::OwnedFd = fd.into();
        (
            deliver(&self.secrets, &auth.caller.app_id, fd).await,
            HashMap::new(),
        )
    }
}

fn valid_identifier(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.contains('\0') && !value.contains('/')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    /// A store that mints per app and remembers what it was handed.
    #[derive(Default)]
    struct Fake {
        seen: Mutex<Vec<(String, Option<Vec<u8>>)>>,
        refuse: bool,
    }

    impl SandboxSecrets for Fake {
        fn secret_for<'a>(&'a self, app: &'a str, legacy: Option<&'a [u8]>) -> Answer<'a> {
            Box::pin(async move {
                self.seen
                    .lock()
                    .unwrap()
                    .push((app.to_owned(), legacy.map(<[u8]>::to_vec)));
                if self.refuse {
                    return Err("revoked".into());
                }
                let mut out = Zeroizing::new([0_u8; SECRET_BYTES]);
                match legacy {
                    Some(bytes) => out.copy_from_slice(bytes),
                    None => out.fill(app.len().try_into().unwrap()),
                }
                Ok(out)
            })
        }
    }

    fn pipe() -> (std::io::PipeReader, std::os::fd::OwnedFd) {
        let (read, write) = std::io::pipe().unwrap();
        (read, write.into())
    }

    fn write_legacy(dir: &std::path::Path, values: &[(&str, [u8; 32])]) -> PathBuf {
        let path = dir.join("secrets.json");
        let key = [0x61; SECRET_BYTES];
        tilo_files::native::private::directory(dir).unwrap();
        tilo_files::native::private::write_atomic(&key_path(&path), &key).unwrap();
        let file = SecretFile {
            values: values
                .iter()
                .map(|(app, bytes)| ((*app).to_owned(), hex::encode(bytes)))
                .collect(),
        };
        let sealed = seal_store(&key, &serde_json::to_vec(&file).unwrap()).unwrap();
        tilo_files::native::private::write_atomic(&path, &sealed).unwrap();
        path
    }

    #[tokio::test]
    async fn the_secret_reaches_only_the_descriptor_the_app_passed() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = PortalSecrets::new(
            Arc::new(Fake::default()),
            LegacyFile::at_path(dir.path().join("none.json")),
        );
        let (mut read, write) = pipe();
        assert_eq!(deliver(&secrets, "so.tilo.notes", write).await, 0);
        let mut got = Vec::new();
        read.read_to_end(&mut got).unwrap();
        assert_eq!(got, vec![13_u8; SECRET_BYTES]);
    }

    #[tokio::test]
    async fn a_revoked_app_gets_no_secret() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = PortalSecrets::new(
            Arc::new(Fake {
                refuse: true,
                ..Fake::default()
            }),
            LegacyFile::at_path(dir.path().join("none.json")),
        );
        let (mut read, write) = pipe();
        assert_eq!(deliver(&secrets, "so.tilo.notes", write).await, 2);
        let mut got = Vec::new();
        read.read_to_end(&mut got).unwrap();
        assert!(got.is_empty());
    }

    #[tokio::test]
    async fn earlier_secrets_are_adopted_once_and_then_removed_from_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_legacy(
            dir.path(),
            &[("org.example.One", [1; 32]), ("org.example.Two", [2; 32])],
        );
        let fake = Arc::new(Fake::default());
        let secrets = PortalSecrets::new(fake.clone(), LegacyFile::at_path(&path));

        let one = secrets.secret_for("org.example.One").await.unwrap();
        assert_eq!(*one, [1; 32]);
        assert_eq!(
            fake.seen.lock().unwrap()[0].1.as_deref(),
            Some(&[1_u8; 32][..])
        );
        // A restart reads the file again: One is gone, Two remains.
        let again = LegacyFile::at_path(&path);
        assert!(again.find("org.example.One").is_none());
        assert_eq!(
            again.find("org.example.Two").unwrap().as_slice(),
            &[2_u8; 32]
        );
        // An app the file never knew is handed nothing to adopt.
        secrets.secret_for("org.example.New").await.unwrap();
        assert_eq!(fake.seen.lock().unwrap()[1].1, None);
        secrets.secret_for("org.example.Two").await.unwrap();
        assert!(!path.exists());
        assert!(!key_path(&path).exists());
    }

    #[test]
    fn a_missing_or_foreign_file_is_simply_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            LegacyFile::at_path(dir.path().join("secrets.json"))
                .find("a.b")
                .is_none()
        );
        let path = write_legacy(dir.path(), &[("a.b", [3; 32])]);
        let key = key_path(&path);
        tilo_files::native::private::write_atomic(&key, &[0x62; SECRET_BYTES]).unwrap();
        assert!(LegacyFile::at_path(&path).find("a.b").is_none());
    }
}
