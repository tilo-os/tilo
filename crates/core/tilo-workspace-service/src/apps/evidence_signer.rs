//! Host-only signing for semantic admission receipts. Never exposed as a
//! signing service: callers supply the receipt's domain-separated bytes.
//!
//! Keep what signs as the device only for an unchanged, settled trust stamp.
//! The stamp covers the identity, key, certificate, enrollment and revocation;
//! certificate time validity is checked separately on every signature.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};

use collab_types::signer::DeviceSigner;
use parking_lot::Mutex;
use tilo_identity::join_proof::{CurrentDeviceStamp, current_device_stamp};
use tilo_identity::store::IdentityRegistryStore;
use tilo_user_identity::DeviceCert;

/// One-shot fault injection. Admission signs on a blocking thread, so a test
/// installs the hook from any thread.
#[cfg(test)]
pub(crate) static BEFORE_SIGN: Mutex<Option<Box<dyn FnOnce() + Send>>> = Mutex::new(None);

static CACHE: LazyLock<Mutex<SignerCache>> = LazyLock::new(Mutex::default);
const CACHE_LIMIT: usize = 256;

#[derive(Hash, PartialEq, Eq)]
struct CacheKey {
    workspace: String,
    identity_dir: PathBuf,
    device_key: PathBuf,
    device_cert: PathBuf,
}

struct Entry {
    stamp: CurrentDeviceStamp,
    /// What signs as the device: the key this daemon took, or the daemon asked.
    key: Arc<dyn DeviceSigner>,
    certificate: DeviceCert,
}

#[derive(Default)]
struct SignerCache {
    entries: HashMap<CacheKey, Entry>,
    #[cfg(test)]
    loads: usize,
}

/// Sign already domain-separated semantic receipt bytes with this host's
/// currently trusted device. This never calls a signing RPC or uses a user
/// root key. The returned certificate identifies the device signing the bytes.
pub(crate) fn sign(workspace: &str, bytes: &[u8]) -> Result<(DeviceCert, [u8; 64]), String> {
    #[cfg(test)]
    {
        let hook = BEFORE_SIGN.lock().take();
        if let Some(hook) = hook {
            hook();
        }
    }
    let mut cache = CACHE.lock();
    let now = tilo_clock::try_unix_now_ns().ok_or("receipt signing clock is unavailable")?;
    cache.sign_at(workspace, bytes, now)
}

impl SignerCache {
    fn sign_at(
        &mut self,
        workspace: &str,
        bytes: &[u8],
        now: u64,
    ) -> Result<(DeviceCert, [u8; 64]), String> {
        let registry = IdentityRegistryStore::from_env().map_err(|error| error.to_string())?;
        let paths = tilo_identity::provisioning::collaboration_identity_paths();
        let cache_key = CacheKey {
            workspace: workspace.to_owned(),
            identity_dir: registry.identity_dir().to_path_buf(),
            device_key: paths.device_privkey,
            device_cert: paths.device_cert,
        };
        let before = current_device_stamp(workspace);
        let settled = before.filter(CurrentDeviceStamp::settled);
        if let Some(entry) = self.entries.get(&cache_key)
            && settled == Some(entry.stamp)
        {
            let result = sign_entry(entry.key.as_ref(), &entry.certificate, bytes, now);
            if result.is_ok() && current_device_stamp(workspace) == before {
                return result;
            }
            // An expired certificate must evict the private key too. Never
            // serve a result if trust changed while the signature was made.
            self.entries.remove(&cache_key);
            return result.and_then(|_| Err("device trust changed while signing receipt".into()));
        }
        // Drop stale ownership before attempting a reload, including on error.
        self.entries.remove(&cache_key);
        #[cfg(test)]
        {
            self.loads = self.loads.saturating_add(1);
        }
        let certificate = tilo_identity::trusted_device::certificate(workspace)
            .map_err(|error| error.to_string())?;
        let key = tilo_identity::trusted_device::device_signer(
            &tilo_identity::provisioning::collaboration_identity_paths(),
        )
        .map_err(|error| error.to_string())?;
        if key.device_public_key() != certificate.device_public_key {
            return Err("the device signing receipts is not the one its certificate names".into());
        }
        let signed = sign_entry(key.as_ref(), &certificate, bytes, now)?;
        let after = current_device_stamp(workspace);
        if before != after {
            return Err("device trust changed while signing receipt".into());
        }
        // Unobservable/keychain or recently written files are never cached.
        // Serialize loading and signing under the host cache lock so concurrent
        // requests share one key load per settled workspace trust epoch.
        if let Some(stamp) = settled {
            if self.entries.len() >= CACHE_LIMIT {
                self.entries.clear();
            }
            self.entries.insert(
                cache_key,
                Entry {
                    stamp,
                    key,
                    certificate,
                },
            );
        }
        Ok(signed)
    }
}

fn sign_entry(
    key: &dyn DeviceSigner,
    certificate: &DeviceCert,
    bytes: &[u8],
    now: u64,
) -> Result<(DeviceCert, [u8; 64]), String> {
    tilo_user_identity::verify_device_cert_at(certificate, now)
        .map_err(|error| error.to_string())?;
    let signature =
        collab_types::signer::sign_now(key, bytes.to_vec()).map_err(|error| error.to_string())?;
    Ok((certificate.clone(), signature))
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use tilo_device_identity::IdentityPaths;
    use tilo_identity::provisioning::{self, IdentityStorage};

    use super::*;

    struct Fixture {
        identity_dir: PathBuf,
        paths: IdentityPaths,
        profile: tilo_identity::store::IdentityProfile,
        workspace: String,
        /// The device's key, taken as its daemon takes it, for as long as this lives.
        _taken_key: tilo_device_identity::persist::DeviceKeyLock,
        _env: tilo_test_env::ScopedEnv,
        _root: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().expect("root");
            let config = directory.path().join("config");
            let system = directory.path().join("system");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("XDG_CONFIG_HOME", &config)
                .set("TILO_IDENTITY_ROOT", &system)
                .set(provisioning::FILE_SECRETS_ENV, "1");
            let identity_dir =
                tilo_identity::store::account_identity_dir(&config.join("tilo/identity"));
            let paths = IdentityPaths::under(&system);
            tilo_identity::sources::test_trust::install();
            let taken_key = provisioning::provisioned_as_its_daemon(
                &identity_dir,
                &paths,
                IdentityStorage::Files,
            )
            .expect("provision");
            let workspace = uuid::Uuid::new_v4().to_string();
            let profile = tilo_identity::store::identity_for_workspace(&workspace)
                .expect("registry")
                .expect("selected identity");
            // The person's devices, as the host that holds their record answers: their root
            // certified this device as it made them.
            tilo_identity::sources::test_trust::enrol(
                profile.user_public_key,
                tilo_device_identity::persist::read_device_cert(
                    &paths.device_cert_for_user(&profile.user_public_key),
                )
                .expect("this device's certificate")
                .device_public_key,
            );
            let fixture = Self {
                identity_dir,
                paths,
                profile,
                workspace,
                _taken_key: taken_key,
                _env: env,
                _root: directory,
            };
            fixture.settle();
            fixture
        }

        fn cert_file(&self) -> PathBuf {
            self.paths
                .device_cert_for_user(&self.profile.user_public_key)
        }

        fn settle(&self) {
            for path in [
                self.identity_dir.clone(),
                self.paths.device_privkey.clone(),
                self.cert_file(),
            ] {
                settle_file(&path);
            }
            assert!(
                current_device_stamp(&self.workspace)
                    .expect("stamp")
                    .settled()
            );
        }

        fn now() -> u64 {
            tilo_clock::unix_now_ns()
        }
    }

    fn settle_file(path: &Path) {
        std::fs::File::open(path)
            .expect("open")
            .set_modified(
                tilo_clock::wall()
                    .checked_sub(Duration::from_secs(10))
                    .expect("file timestamp"),
            )
            .expect("settle");
    }

    tilo_test_env::scenario! {
        fn the_signer_keeps_a_device_key_only_while_its_trust_stands;

        phase unchanged_trust_loads_one_key_and_signs_each_payload;
        {
            let fixture = Fixture::new();
            let mut cache = SignerCache::default();
            // Deterministic overhead budget: a bounded burst performs one private
            // key load, not one load per invocation. Every payload still gets its
            // own signature; no wall-clock performance threshold can flake here.
            for position in 0..128 {
                let payload = format!("semantic receipt {position}");
                let (cert, signature) = cache
                    .sign_at(&fixture.workspace, payload.as_bytes(), Fixture::now())
                    .expect("sign");
                ed25519_dalek::VerifyingKey::from_bytes(&cert.device_public_key)
                    .expect("key")
                    .verify_strict(
                        payload.as_bytes(),
                        &ed25519_dalek::Signature::from_bytes(&signature),
                    )
                    .expect("signature binds the actual receipt bytes");
            }
            assert_eq!(cache.loads, 1);
            assert_eq!(cache.entries.len(), 1);
            assert!(sign(&fixture.workspace, b"host receipt").is_ok());
        }

        phase revocation_evicts_ownership_and_fails_even_after_files_settle;
        {
            let fixture = Fixture::new();
            let mut cache = SignerCache::default();
            let (cert, _) = cache
                .sign_at(&fixture.workspace, b"before", Fixture::now())
                .expect("sign");
            tilo_identity::sources::test_trust::revoke(
                fixture.profile.user_public_key,
                cert.device_public_key,
            );
            assert!(
                cache
                    .sign_at(&fixture.workspace, b"after", Fixture::now())
                    .is_err()
            );
            assert!(cache.entries.is_empty());
            fixture.settle();
            assert!(
                cache
                    .sign_at(&fixture.workspace, b"settled", Fixture::now())
                    .is_err()
            );
        }

        phase certificate_and_key_rewrites_reload_and_malformed_cert_fails;
        {
            let fixture = Fixture::new();
            let mut cache = SignerCache::default();
            cache
                .sign_at(&fixture.workspace, b"before", Fixture::now())
                .expect("sign");
            for path in [fixture.cert_file(), fixture.paths.device_privkey.clone()] {
                let bytes = std::fs::read(&path).expect("read");
                std::fs::write(&path, bytes).expect("rewrite");
                let loads = cache.loads;
                cache
                    .sign_at(&fixture.workspace, b"unsettled", Fixture::now())
                    .expect("reload");
                cache
                    .sign_at(&fixture.workspace, b"still unsettled", Fixture::now())
                    .expect("reload");
                assert_eq!(cache.loads, loads + 2, "recent writes are not cached");
                fixture.settle();
                cache
                    .sign_at(&fixture.workspace, b"settled", Fixture::now())
                    .expect("cache");
            }
            std::fs::write(fixture.cert_file(), b"malformed").expect("break certificate");
            assert!(
                cache
                    .sign_at(&fixture.workspace, b"refused", Fixture::now())
                    .is_err()
            );
            assert!(cache.entries.is_empty());
        }

        phase removing_the_identity_record_invalidates_cached_key;
        {
            let fixture = Fixture::new();
            let mut cache = SignerCache::default();
            cache
                .sign_at(&fixture.workspace, b"before", Fixture::now())
                .expect("sign");
            // The record is the identity: removing it removes the person.
            std::fs::remove_file(&fixture.profile.record_path).expect("remove identity record");
            assert!(
                tilo_identity::store::identity_for_workspace(&fixture.workspace)
                    .expect("a read")
                    .is_none()
            );
            assert!(
                cache
                    .sign_at(&fixture.workspace, b"after", Fixture::now())
                    .is_err()
            );
            assert!(cache.entries.is_empty());
        }

        phase clock_before_issuance_invalidates_cached_key;
        {
            let fixture = Fixture::new();
            let mut cache = SignerCache::default();
            let (certificate, _) = cache
                .sign_at(&fixture.workspace, b"valid", Fixture::now())
                .expect("sign");
            let before_issuance = certificate
                .issued_at_ns
                .checked_sub(1)
                .expect("issued time");
            assert!(
                cache
                    .sign_at(
                        &fixture.workspace,
                        b"clock moved backwards",
                        before_issuance
                    )
                    .is_err()
            );
            assert!(cache.entries.is_empty());
        }

        phase time_expiry_without_file_changes_evicts_the_key;
        {
            let fixture = Fixture::new();
            let (key, mut cert) =
                tilo_identity::trusted_device::current(&fixture.workspace).expect("device");
            drop(key);
            let expiry = cert.issued_at_ns.saturating_add(60_000_000_000);
            cert.expires_at_ns = Some(expiry);
            let root = tilo_identity::store::load_from_path(fixture.profile.record_path.clone())
                .expect("file-backed test identity");
            cert.signature = root
                .sign(&tilo_user_identity::device_cert_signing_digest(&cert).expect("digest"))
                .expect("certify");
            tilo_device_identity::persist::write_device_cert(&fixture.cert_file(), &cert)
                .expect("write certificate");
            fixture.settle();
            let mut cache = SignerCache::default();
            cache
                .sign_at(&fixture.workspace, b"valid", expiry - 1)
                .expect("before expiry");
            let stamp = current_device_stamp(&fixture.workspace);
            assert!(
                cache
                    .sign_at(&fixture.workspace, b"expired", expiry)
                    .is_err()
            );
            assert_eq!(current_device_stamp(&fixture.workspace), stamp);
            assert!(cache.entries.is_empty());
            assert!(
                cache
                    .sign_at(&fixture.workspace, b"still expired", expiry + 1)
                    .is_err()
            );
        }
    }
}
