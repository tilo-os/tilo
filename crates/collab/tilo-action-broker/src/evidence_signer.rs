//! Host-only signing for semantic admission receipts. Never exposed as a
//! signing service: callers supply the receipt's domain-separated bytes.
//!
//! Keep the device key in memory only for an unchanged, settled trust stamp.
//! The stamp covers selection, key, certificate, enrollment and revocation;
//! certificate time validity is checked separately on every signature.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::LazyLock;

use ed25519_dalek::{Signer as _, SigningKey};
use parking_lot::Mutex;
use tilo_identity_host::join_proof::{CurrentDeviceStamp, current_device_stamp};
use tilo_identity_host::registry::IdentityRegistryStore;
use tilo_user_identity::DeviceCert;

static CACHE: LazyLock<Mutex<SignerCache>> = LazyLock::new(Mutex::default);
const CACHE_LIMIT: usize = 256;

#[derive(Hash, PartialEq, Eq)]
struct CacheKey {
    room: String,
    identity_dir: PathBuf,
    device_key: PathBuf,
    device_cert: PathBuf,
}

struct Entry {
    stamp: CurrentDeviceStamp,
    key: SigningKey,
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
pub(crate) fn sign(room: &str, bytes: &[u8]) -> Result<(DeviceCert, [u8; 64]), String> {
    let mut cache = CACHE.lock();
    let now = tilo_clock::try_unix_now_ns().ok_or("receipt signing clock is unavailable")?;
    cache.sign_at(room, bytes, now)
}

impl SignerCache {
    fn sign_at(
        &mut self,
        room: &str,
        bytes: &[u8],
        now: u64,
    ) -> Result<(DeviceCert, [u8; 64]), String> {
        let registry = IdentityRegistryStore::from_env().map_err(|error| error.to_string())?;
        let paths = tilo_identity_host::provisioning::collaboration_identity_paths();
        let cache_key = CacheKey {
            room: room.to_owned(),
            identity_dir: registry.identity_dir().to_path_buf(),
            device_key: paths.device_privkey,
            device_cert: paths.device_cert,
        };
        let before = current_device_stamp(room);
        let settled = before.filter(CurrentDeviceStamp::settled);
        if let Some(entry) = self.entries.get(&cache_key)
            && settled == Some(entry.stamp)
        {
            let result = sign_entry(&entry.key, &entry.certificate, bytes, now);
            if result.is_ok() && current_device_stamp(room) == before {
                return result;
            }
            // An expired certificate must evict the private key too. Never
            // serve a result if trust changed while the signature was made.
            self.entries.remove(&cache_key);
            return result.and_then(|_| Err("device trust changed while signing receipt".into()));
        }
        // Drop stale custody before attempting a reload, including on error.
        self.entries.remove(&cache_key);
        #[cfg(test)]
        {
            self.loads = self.loads.saturating_add(1);
        }
        let (key, certificate) = tilo_identity_host::broker::trusted_current_device(room)
            .map_err(|error| error.to_string())?;
        let signed = sign_entry(&key, &certificate, bytes, now)?;
        let after = current_device_stamp(room);
        if before != after {
            return Err("device trust changed while signing receipt".into());
        }
        // Unobservable/keychain or recently written files are never cached.
        // Serialize loading and signing under the host cache lock so concurrent
        // requests share one key load per settled room trust epoch.
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
    key: &SigningKey,
    certificate: &DeviceCert,
    bytes: &[u8],
    now: u64,
) -> Result<(DeviceCert, [u8; 64]), String> {
    tilo_user_identity::verify_device_cert_at(certificate, now)
        .map_err(|error| error.to_string())?;
    Ok((certificate.clone(), key.sign(bytes).to_bytes()))
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::{Duration, SystemTime};

    use tilo_identity_host::device_fleet::DeviceFleetStore;
    use tilo_identity_host::provisioning::{self, IdentityStorage};
    use tilo_user_identity::{IdentityPaths, RevocationReason};

    use super::*;

    struct Fixture {
        identity_dir: PathBuf,
        paths: IdentityPaths,
        profile: tilo_identity_host::registry::IdentityProfile,
        room: String,
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
            let identity_dir = config.join("tilo/identity");
            let paths = IdentityPaths::under(&system);
            provisioning::ensure_collaboration_identity(
                &identity_dir,
                &paths,
                IdentityStorage::Files,
            )
            .expect("provision");
            let room = uuid::Uuid::new_v4().to_string();
            let profile = tilo_identity_host::registry::selected_identity_profile(&room)
                .expect("registry")
                .expect("selected identity");
            let fixture = Self {
                identity_dir,
                paths,
                profile,
                room,
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
                self.identity_dir.join("registry.json"),
                self.paths.device_privkey.clone(),
                self.cert_file(),
                self.identity_dir
                    .join("fleets")
                    .join(format!("{}.json", self.profile.id)),
            ] {
                settle_file(&path);
            }
            assert!(current_device_stamp(&self.room).expect("stamp").settled());
        }

        fn now() -> u64 {
            tilo_clock::unix_now_ns()
        }
    }

    fn settle_file(path: &Path) {
        std::fs::File::open(path)
            .expect("open")
            .set_modified(
                SystemTime::now()
                    .checked_sub(Duration::from_secs(10))
                    .expect("file timestamp"),
            )
            .expect("settle");
    }

    #[test]
    fn unchanged_trust_loads_one_key_and_signs_each_payload() {
        let fixture = Fixture::new();
        let mut cache = SignerCache::default();
        // Deterministic overhead budget: a bounded burst performs one private
        // key load, not one load per invocation. Every payload still gets its
        // own signature; no wall-clock performance threshold can flake here.
        for position in 0..128 {
            let payload = format!("semantic receipt {position}");
            let (cert, signature) = cache
                .sign_at(&fixture.room, payload.as_bytes(), Fixture::now())
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
        assert!(sign(&fixture.room, b"host receipt").is_ok());
    }

    #[test]
    fn revocation_evicts_custody_and_fails_even_after_files_settle() {
        let fixture = Fixture::new();
        let mut cache = SignerCache::default();
        let (cert, _) = cache
            .sign_at(&fixture.room, b"before", Fixture::now())
            .expect("sign");
        DeviceFleetStore::at(&fixture.identity_dir, fixture.paths.clone())
            .revoke(
                &fixture.profile.id,
                cert.device_public_key,
                RevocationReason::Lost,
                Fixture::now(),
            )
            .expect("revoke");
        assert!(
            cache
                .sign_at(&fixture.room, b"after", Fixture::now())
                .is_err()
        );
        assert!(cache.entries.is_empty());
        fixture.settle();
        assert!(
            cache
                .sign_at(&fixture.room, b"settled", Fixture::now())
                .is_err()
        );
    }

    #[test]
    fn certificate_and_key_rewrites_reload_and_malformed_cert_fails() {
        let fixture = Fixture::new();
        let mut cache = SignerCache::default();
        cache
            .sign_at(&fixture.room, b"before", Fixture::now())
            .expect("sign");
        for path in [fixture.cert_file(), fixture.paths.device_privkey.clone()] {
            let bytes = std::fs::read(&path).expect("read");
            std::fs::write(&path, bytes).expect("rewrite");
            let loads = cache.loads;
            cache
                .sign_at(&fixture.room, b"unsettled", Fixture::now())
                .expect("reload");
            cache
                .sign_at(&fixture.room, b"still unsettled", Fixture::now())
                .expect("reload");
            assert_eq!(cache.loads, loads + 2, "recent writes are not cached");
            fixture.settle();
            cache
                .sign_at(&fixture.room, b"settled", Fixture::now())
                .expect("cache");
        }
        std::fs::write(fixture.cert_file(), b"malformed").expect("break certificate");
        assert!(
            cache
                .sign_at(&fixture.room, b"refused", Fixture::now())
                .is_err()
        );
        assert!(cache.entries.is_empty());
    }

    #[test]
    fn removing_identity_selection_invalidates_cached_key() {
        let fixture = Fixture::new();
        let mut cache = SignerCache::default();
        cache
            .sign_at(&fixture.room, b"before", Fixture::now())
            .expect("sign");
        let store = IdentityRegistryStore::at(&fixture.identity_dir);
        let mut registry = store.load().expect("registry");
        registry.default_identity = None;
        registry.workspace_identities.clear();
        registry.identities.clear();
        // Reconciliation restores selection from records still on disk, so
        // removing only the explicit selection leaves a legitimate fallback.
        std::fs::remove_file(&fixture.profile.record_path).expect("remove identity record");
        store.save(&registry).expect("remove identity");
        assert!(
            tilo_identity_host::registry::selected_identity_profile(&fixture.room)
                .expect("selection")
                .is_none()
        );
        assert!(
            cache
                .sign_at(&fixture.room, b"after", Fixture::now())
                .is_err()
        );
        assert!(cache.entries.is_empty());
    }

    #[test]
    fn clock_before_issuance_invalidates_cached_key() {
        let fixture = Fixture::new();
        let mut cache = SignerCache::default();
        let (certificate, _) = cache
            .sign_at(&fixture.room, b"valid", Fixture::now())
            .expect("sign");
        let before_issuance = certificate
            .issued_at_ns
            .checked_sub(1)
            .expect("issued time");
        assert!(
            cache
                .sign_at(&fixture.room, b"clock moved backwards", before_issuance)
                .is_err()
        );
        assert!(cache.entries.is_empty());
    }

    #[test]
    fn time_expiry_without_file_changes_evicts_the_key() {
        let fixture = Fixture::new();
        let (key, mut cert) =
            tilo_identity_host::broker::trusted_current_device(&fixture.room).expect("device");
        drop(key);
        let expiry = Fixture::now().saturating_add(60_000_000_000);
        cert.expires_at_ns = Some(expiry);
        let root = runtime_identity::load_from_path(fixture.profile.record_path.clone())
            .expect("file-backed test identity");
        cert.signature = root
            .sign(&tilo_user_identity::device_cert_signing_digest(&cert).expect("digest"))
            .expect("certify");
        tilo_user_identity::persist::write_device_cert(&fixture.cert_file(), &cert)
            .expect("write certificate");
        fixture.settle();
        let mut cache = SignerCache::default();
        cache
            .sign_at(&fixture.room, b"valid", expiry - 1)
            .expect("before expiry");
        let stamp = current_device_stamp(&fixture.room);
        assert!(cache.sign_at(&fixture.room, b"expired", expiry).is_err());
        assert_eq!(current_device_stamp(&fixture.room), stamp);
        assert!(cache.entries.is_empty());
        assert!(
            cache
                .sign_at(&fixture.room, b"still expired", expiry + 1)
                .is_err()
        );
    }
}
