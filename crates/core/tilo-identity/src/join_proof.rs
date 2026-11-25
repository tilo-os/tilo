//! This device's signed collaboration-join proof for a workspace: its signature over the
//! workspace's join bytes, presented with its certificate.
//!
//! Signatures are deterministic, so a proof changes only when the identity it signs as, the device
//! key, its certificate or the person's devices change — the first three each a file (`stat`ed,
//! settled past the filesystem clock's granularity, never cached while unsettled or on failure;
//! the cache keeps no key), the last a number the host that holds their record moves when it
//! does. A receiver still checks it against current membership, the person's devices and policy
//! time.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

use tilo_device_identity::IdentityPaths;
use tilo_device_identity::persist::DeviceKeyOwnership;
use tilo_user_identity::{DeviceCert, StatementSigner};

use crate::store::IdentityRegistryStore;
use tilo_files::native::FileStamp;

/// This device's certificate and its signed join proof for one workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrentDeviceJoinProof {
    pub certificate: DeviceCert,
    pub proof: [u8; 64],
}

/// Everything a workspace's join proof is derived from, as of one `stat` of each
/// file. While two settled stamps are equal, so are the proofs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CurrentDeviceStamp {
    registry: RegistryStamp,
    /// `None` while the workspace selects no identity.
    device: Option<DeviceStamp>,
}

impl CurrentDeviceStamp {
    /// Every file is old enough that a later write can't share its stamp.
    #[must_use]
    pub fn settled(&self) -> bool {
        self.registry.settled() && self.device.as_ref().is_none_or(DeviceStamp::settled)
    }

    /// When it is [`Self::settled`], the moment its last file is old
    /// enough: a reader that found it unsettled reads again then. `None`
    /// when it holds no file.
    #[must_use]
    pub fn settles_at(&self) -> Option<std::time::SystemTime> {
        let device = self.device.as_ref().map(DeviceStamp::settles_at);
        [self.registry.settles_at(), device.flatten()]
            .into_iter()
            .flatten()
            .max()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct RegistryStamp {
    /// `None` while there is no identity directory at all.
    dir: Option<FileStamp>,
}

impl RegistryStamp {
    fn settled(&self) -> bool {
        self.dir.is_none_or(FileStamp::settled)
    }

    fn settles_at(&self) -> Option<std::time::SystemTime> {
        self.dir.map(FileStamp::settles_at)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct DeviceStamp {
    key: Option<FileStamp>,
    cert: Option<FileStamp>,
    /// What the host that holds the person's devices answered when the proof was minted
    /// ([`crate::sources::DeviceTrust::generation`]): it moves when their devices do.
    trust: u64,
}

impl DeviceStamp {
    fn settled(&self) -> bool {
        [self.key, self.cert]
            .into_iter()
            .flatten()
            .all(FileStamp::settled)
    }

    fn settles_at(&self) -> Option<std::time::SystemTime> {
        [self.key, self.cert]
            .into_iter()
            .flatten()
            .map(FileStamp::settles_at)
            .max()
    }
}

/// The identity a workspace selected when its registry had one stamp.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Selected {
    id: String,
    user_public_key: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct CacheKey {
    workspace: String,
    identity_dir: PathBuf,
    device_key: PathBuf,
    device_cert: PathBuf,
}

struct Entry {
    registry: RegistryStamp,
    selected: Option<Selected>,
    minted: Option<(CurrentDeviceStamp, CurrentDeviceJoinProof)>,
}

static CACHE: LazyLock<Mutex<HashMap<CacheKey, Entry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// A desktop holds a handful of workspaces; the bound only stops a caller
/// that names ever new ones or roots from growing the cache without limit.
const CACHE_LIMIT: usize = 256;

#[cfg(test)]
thread_local! {
    static MINTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many proofs this thread minted from disk and the device key.
#[cfg(test)]
pub(crate) fn mints_on_this_thread() -> usize {
    MINTS.with(std::cell::Cell::get)
}

/// The identity files of this environment.
struct Files {
    registry: IdentityRegistryStore,
    paths: IdentityPaths,
}

impl Files {
    fn from_env() -> io::Result<Self> {
        let registry = IdentityRegistryStore::from_env()?;
        let paths = crate::provisioning::collaboration_identity_paths();
        Ok(Self { registry, paths })
    }

    fn key(&self, workspace: &str) -> CacheKey {
        CacheKey {
            workspace: workspace.to_owned(),
            identity_dir: self.registry.identity_dir().to_path_buf(),
            device_key: self.paths.device_privkey.clone(),
            device_cert: self.paths.device_cert.clone(),
        }
    }

    fn registry_stamp(&self) -> Option<RegistryStamp> {
        Some(RegistryStamp {
            dir: FileStamp::of(self.registry.identity_dir()).ok()?,
        })
    }

    fn stamp(
        &self,
        registry: RegistryStamp,
        selected: Option<&Selected>,
    ) -> Option<CurrentDeviceStamp> {
        let device = match selected {
            None => None,
            Some(selected) => Some(DeviceStamp {
                key: match self.paths.device_key_ownership {
                    DeviceKeyOwnership::File => FileStamp::of(&self.paths.device_privkey).ok()?,
                    // A key with no file has no stamp to follow.
                    #[cfg(any(target_os = "macos", target_os = "android"))]
                    DeviceKeyOwnership::PlatformStore => return None,
                },
                cert: FileStamp::of(&self.paths.device_cert_for_user(&selected.user_public_key))
                    .ok()?,
                trust: crate::sources::device_trust().map_or(0, |trust| trust.generation()),
            }),
        };
        Some(CurrentDeviceStamp { registry, device })
    }

    /// The identity `workspace` selects now, read from the registry.
    fn selected(workspace: &str) -> io::Result<Option<Selected>> {
        Ok(
            crate::store::identity_for_workspace(workspace)?.map(|profile| Selected {
                id: profile.id,
                user_public_key: profile.user_public_key,
            }),
        )
    }
}

fn remember(key: CacheKey, entry: Entry) {
    if let Ok(mut cache) = CACHE.lock() {
        if cache.len() >= CACHE_LIMIT && !cache.contains_key(&key) {
            cache.clear();
        }
        cache.insert(key, entry);
    }
}

/// The stamp of everything `workspace`'s join proof is derived from: equal
/// settled stamps mean an unchanged proof, including whether there is one.
/// One `stat` per file once its identity is known; `None` when a
/// file can't be examined or the device key has no file, so the caller must
/// treat the proof as changed.
#[must_use]
pub fn current_device_stamp(workspace: &str) -> Option<CurrentDeviceStamp> {
    // An environment that names no identity directory names none on every
    // call: no identity, and nothing to change.
    let Ok(files) = Files::from_env() else {
        return Some(CurrentDeviceStamp {
            registry: RegistryStamp { dir: None },
            device: None,
        });
    };
    let registry = files.registry_stamp()?;
    let key = files.key(workspace);
    let known = CACHE.lock().ok().and_then(|cache| {
        cache
            .get(&key)
            .filter(|entry| entry.registry == registry)
            .map(|entry| entry.selected.clone())
    });
    let selected = if let Some(selected) = known {
        selected
    } else {
        let selected = match registry.dir {
            None => None,
            Some(_) => Files::selected(workspace).ok()?,
        };
        if registry.settled() && files.registry_stamp() == Some(registry) {
            remember(
                key,
                Entry {
                    registry,
                    selected: selected.clone(),
                    minted: None,
                },
            );
        }
        selected
    };
    files.stamp(registry, selected.as_ref())
}

/// This device's certificate and its signed join proof for the identity
/// selected in `workspace`, only while that certificate is valid, enrolled
/// and not revoked. Served from memory while every file it is derived from is
/// unchanged (see the module documentation); otherwise derived and signed
/// again through [`crate::trusted_device::current`].
///
/// # Errors
///
/// As [`crate::trusted_device::current`].
pub fn trusted_current_join_proof(workspace: &str) -> io::Result<CurrentDeviceJoinProof> {
    let files = Files::from_env()?;
    let key = files.key(workspace);
    let registry = files.registry_stamp();
    if let Some(registry) = registry {
        let cached = CACHE.lock().ok().and_then(|cache| {
            let entry = cache.get(&key).filter(|entry| entry.registry == registry)?;
            let (stamp, proof) = entry.minted.as_ref()?;
            Some((entry.selected.clone(), *stamp, proof.clone()))
        });
        if let Some((selected, stamp, proof)) = cached
            && files.stamp(registry, selected.as_ref()) == Some(stamp)
        {
            return Ok(proof);
        }
    }
    let selected = Files::selected(workspace)?;
    let before = registry.and_then(|registry| files.stamp(registry, selected.as_ref()));
    let (device, certificate) = crate::trusted_device::current(workspace)?;
    #[cfg(test)]
    MINTS.with(|mints| mints.set(mints.get().saturating_add(1)));
    let digest = tilo_identity_client::collaboration_join_digest(workspace)
        .map_err(|error| io::Error::other(error.to_string()))?;
    let proof = CurrentDeviceJoinProof {
        proof: device
            .sign_digest(&digest)
            .map_err(|error| io::Error::other(error.to_string()))?,
        certificate,
    };
    drop(device);
    if let Some(before) = before
        && before.settled()
        && selected
            .as_ref()
            .is_some_and(|selected| selected.user_public_key == proof.certificate.user_public_key)
        && files
            .registry_stamp()
            .and_then(|registry| files.stamp(registry, selected.as_ref()))
            == Some(before)
    {
        remember(
            key,
            Entry {
                registry: before.registry,
                selected,
                minted: Some((before, proof.clone())),
            },
        );
    }
    Ok(proof)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime};

    use tilo_device_identity::IdentityPaths;

    use super::*;

    /// A provisioned identity the environment points at, every file of it
    /// dated back as it is a few seconds after its last write.
    pub(crate) struct Fixture {
        identity_dir: PathBuf,
        paths: IdentityPaths,
        pub(crate) user_public_key: [u8; 32],
        pub(crate) workspace: String,
        /// The device's key, taken as its daemon takes it: the proofs are signed with it.
        _taken: tilo_device_identity::persist::DeviceKeyLock,
        _env: tilo_test_env::ScopedEnv,
        _root: tempfile::TempDir,
    }

    impl Fixture {
        pub(crate) fn new() -> Self {
            let temp = tempfile::tempdir().expect("temp root");
            let xdg = temp.path().join("config");
            let system_root = temp.path().join("system");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("XDG_CONFIG_HOME", &xdg)
                .set("TILO_IDENTITY_ROOT", &system_root)
                .set(crate::provisioning::FILE_SECRETS_ENV, "1");
            let identity_dir = crate::store::account_identity_dir(&xdg.join("tilo/identity"));
            let paths = IdentityPaths::under(&system_root);
            tilo_device_identity::persist::load_or_create_device_key(&paths).expect("device key");
            let taken =
                tilo_device_identity::persist::take_device_key(&paths).expect("the key taken");
            crate::provisioning::ensure_collaboration_identity(
                &identity_dir,
                &paths,
                crate::provisioning::IdentityStorage::Files,
            )
            .expect("provision");
            let workspace = uuid_like_workspace();
            let profile = crate::store::identity_for_workspace(&workspace)
                .expect("registry")
                .expect("selected identity");
            let fixture = Self {
                identity_dir,
                paths,
                user_public_key: profile.user_public_key,
                workspace,
                _taken: taken,
                _env: env,
                _root: temp,
            };
            fixture.settle();
            // The person's devices, as the host that holds their record answers: their root
            // certified this device as it made them.
            fixture.trust();
            fixture
        }

        /// Answer the person's devices from the fake a test drives, with this device enrolled.
        pub(crate) fn trust(&self) {
            crate::sources::test_trust::install();
            let device = tilo_device_identity::persist::read_device_cert(&self.cert_file())
                .expect("this device's certificate")
                .device_public_key;
            crate::sources::test_trust::enrol(self.user_public_key, device);
        }

        fn cert_file(&self) -> PathBuf {
            self.paths.device_cert_for_user(&self.user_public_key)
        }

        pub(crate) fn settle(&self) {
            for path in [
                self.identity_dir.clone(),
                self.paths.device_privkey.clone(),
                self.cert_file(),
            ] {
                settle(&path);
            }
        }
    }

    /// Proofs minted from disk on this thread by `run`.
    pub(crate) fn mints(run: impl FnOnce()) -> usize {
        let before = mints_on_this_thread();
        run();
        mints_on_this_thread().saturating_sub(before)
    }

    fn uuid_like_workspace() -> String {
        tilo_desktop_model::WorkspaceId::new().to_string()
    }

    /// Date `path` back past the filesystem clock's granularity.
    fn settle(path: &Path) {
        std::fs::File::open(path)
            .expect("open identity file")
            .set_modified(
                SystemTime::now()
                    .checked_sub(Duration::from_secs(10))
                    .expect("a time ten seconds ago"),
            )
            .expect("date identity file back");
    }

    tilo_test_env::scenario! {
        fn the_join_proof_cache;

        phase proof_is_minted_once_while_identity_files_are_unchanged;
        {
            let fixture = Fixture::new();
            let mut proofs = Vec::new();
            let mints = mints(|| {
                for _ in 0..3 {
                    proofs
                        .push(trusted_current_join_proof(&fixture.workspace).expect("trusted device"));
                }
            });
            assert_eq!(mints, 1, "one mint serves every later call");
            assert!(proofs.windows(2).all(|pair| pair[0] == pair[1]));
            let proof = &proofs[0];
            ed25519_dalek::VerifyingKey::from_bytes(&proof.certificate.device_public_key)
                .expect("device key")
                .verify_strict(
                    &tilo_identity_client::collaboration_join_digest(&fixture.workspace).unwrap(),
                    &ed25519_dalek::Signature::from_bytes(&proof.proof),
                )
                .expect("the proof is this device's signature for this workspace");
            let other = uuid_like_workspace();
            let other_proof = trusted_current_join_proof(&other).expect("another workspace");
            assert_ne!(
                other_proof.proof, proof.proof,
                "a proof binds its workspace"
            );
        }

        phase revocation_fails_the_very_next_call;
        {
            let fixture = Fixture::new();
            trusted_current_join_proof(&fixture.workspace).expect("trusted device");
            let stamp = current_device_stamp(&fixture.workspace).expect("stamp");
            assert_eq!(
                mints(|| {
                    trusted_current_join_proof(&fixture.workspace).expect("cached");
                }),
                0
            );
            let device = trusted_current_join_proof(&fixture.workspace)
                .expect("trusted device")
                .certificate
                .device_public_key;
            crate::sources::test_trust::revoke(fixture.user_public_key, device);
            let refused = trusted_current_join_proof(&fixture.workspace)
                .expect_err("a revoked device has no proof");
            assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
            assert_ne!(current_device_stamp(&fixture.workspace), Some(stamp));
            // Even once the identity files settle, nothing was cached.
            fixture.settle();
            assert!(trusted_current_join_proof(&fixture.workspace).is_err());
        }

        phase rewritten_certificate_or_key_mints_again;
        {
            let fixture = Fixture::new();
            trusted_current_join_proof(&fixture.workspace).expect("trusted device");
            for path in [fixture.cert_file(), fixture.paths.device_privkey.clone()] {
                let bytes = std::fs::read(&path).expect("read");
                std::fs::write(&path, &bytes).expect("rewrite");
                assert_eq!(
                    mints(|| {
                        trusted_current_join_proof(&fixture.workspace).expect("still trusted");
                    }),
                    1,
                    "{} changed",
                    path.display()
                );
                fixture.settle();
                trusted_current_join_proof(&fixture.workspace).expect("re-cached");
            }
            std::fs::write(fixture.cert_file(), b"malformed").expect("break certificate");
            assert!(trusted_current_join_proof(&fixture.workspace).is_err());
        }

        phase join_proof_cost;
        // Wall time of re-deriving a workspace's proof from disk as every action
        // call did before (registry, key, certificate, an uncached fleet load,
        // a signature) against serving it from its stamps. Prints the numbers
        // (`--nocapture`); the mint counts above are the regression guard.
        {
            const ROUNDS: u32 = 200;
            let fixture = Fixture::new();
            trusted_current_join_proof(&fixture.workspace).expect("warm");
            let started = std::time::Instant::now();
            for _ in 0..ROUNDS {
                let (key, _) =
                    crate::trusted_device::current(&fixture.workspace).expect("trusted device");
                let proof = key
                    .sign_digest(
                        &tilo_identity_client::collaboration_join_digest(&fixture.workspace).unwrap(),
                    )
                    .unwrap();
                assert_eq!(proof.len(), 64);
            }
            let minted = started.elapsed();
            let mut cached = Duration::ZERO;
            let mints = mints(|| {
                let started = std::time::Instant::now();
                for _ in 0..ROUNDS {
                    trusted_current_join_proof(&fixture.workspace).expect("cached proof");
                }
                cached = started.elapsed();
            });
            assert_eq!(mints, 0);
            eprintln!(
                "#3911 join proof: re-derived from disk {:?}/call, served from stamps {:?}/call",
                minted.checked_div(ROUNDS).unwrap_or_default(),
                cached.checked_div(ROUNDS).unwrap_or_default(),
            );
        }

        phase a_desktop_without_identity_has_a_stable_stamp;
        {
            let temp = tempfile::tempdir().expect("temp root");
            let config = temp.path().join("config");
            let system = temp.path().join("system");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("XDG_CONFIG_HOME", &config)
                .set("TILO_IDENTITY_ROOT", &system)
                .set(crate::provisioning::FILE_SECRETS_ENV, "1");
            let workspace = uuid_like_workspace();
            let stamp = current_device_stamp(&workspace).expect("no identity is a known state");
            assert!(stamp.settled());
            assert_eq!(current_device_stamp(&workspace), Some(stamp));
            assert!(trusted_current_join_proof(&workspace).is_err());
        }

        phase unsettled_files_are_never_cached;
        {
            let fixture = Fixture::new();
            std::fs::write(
                fixture.cert_file(),
                std::fs::read(fixture.cert_file()).expect("certificate"),
            )
            .expect("touch certificate");
            let mints = mints(|| {
                trusted_current_join_proof(&fixture.workspace).expect("trusted");
                trusted_current_join_proof(&fixture.workspace).expect("trusted");
            });
            assert_eq!(mints, 2, "a certificate written just now is read on every call");
            assert!(
                !current_device_stamp(&fixture.workspace)
                    .expect("stamp")
                    .settled()
            );
        }
    }
}
