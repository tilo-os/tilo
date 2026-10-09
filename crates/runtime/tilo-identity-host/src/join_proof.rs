//! This device's signed collaboration-join proof for a room, minted once
//! while nothing that decides it changed.
//!
//! A join proof is this device's signature over the room's join bytes,
//! presented with its certificate. Ed25519 signatures are deterministic, so
//! the proof for a room changes only when the identity the room selects, the
//! device key, the device certificate or the device fleet changes, and each
//! of those lives in a file. A proof is served from memory while one `stat`
//! of every such file matches the stamp taken when it was minted:
//!
//! * the identity directory and its registry file: which identity the room
//!   selects (a selection change, an imported or removed identity);
//! * the device key file: key rotation. A key in the macOS Keychain has no
//!   file to stamp, so its proofs are never cached;
//! * the selected identity's device certificate: certificate rotation;
//! * the fleet file and the home-peer registry: revocation and enrollment.
//!
//! A file changed within the last two seconds is never trusted to its stamp,
//! because a coarse filesystem clock can give the next write the same time:
//! until every file has settled the proof is minted again on each call.
//! Failures are never cached, and a stamp that can't be taken (a file that
//! can't be examined) never matches. The cache keeps the public certificate
//! and the signature, never the private key.
//!
//! This only spares re-deriving the proof. Whoever receives it still checks
//! it against current membership, fleet and policy time on every call.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use ed25519_dalek::Signer as _;
use tilo_user_identity::persist::DeviceKeyCustody;
use tilo_user_identity::{DeviceCert, IdentityPaths};

use crate::device_fleet::{DeviceFleetStore, FileStamp, FleetStamps};
use crate::registry::{DirStamp, IdentityRegistryStore};

/// This device's certificate and its signed join proof for one room.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrentDeviceJoinProof {
    pub certificate: DeviceCert,
    pub proof: [u8; 64],
}

/// Everything a room's join proof is derived from, as of one `stat` of each
/// file. While two settled stamps are equal, so are the proofs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CurrentDeviceStamp {
    registry: RegistryStamp,
    /// `None` while the room selects no identity.
    device: Option<DeviceStamp>,
}

impl CurrentDeviceStamp {
    /// Every file is old enough that a later write can't share its stamp.
    #[must_use]
    pub fn settled(&self) -> bool {
        self.registry.settled() && self.device.as_ref().is_none_or(DeviceStamp::settled)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct RegistryStamp {
    /// `None` while there is no identity directory at all.
    dir: Option<DirStamp>,
    file: Option<FileStamp>,
}

impl RegistryStamp {
    fn settled(&self) -> bool {
        self.dir.is_none_or(DirStamp::settled) && self.file.is_none_or(FileStamp::settled)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct DeviceStamp {
    key: Option<FileStamp>,
    cert: Option<FileStamp>,
    fleet: FleetStamps,
}

impl DeviceStamp {
    fn settled(&self) -> bool {
        [self.key, self.cert]
            .into_iter()
            .flatten()
            .all(FileStamp::settled)
            && self.fleet.settled()
    }
}

/// The identity a room selected when its registry had one stamp.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Selected {
    id: String,
    user_public_key: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct CacheKey {
    room: String,
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

/// A desktop holds a handful of rooms; the bound only stops a caller that
/// names ever new rooms or roots from growing the cache without limit.
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
    fleets: DeviceFleetStore,
}

impl Files {
    fn from_env() -> io::Result<Self> {
        let registry = IdentityRegistryStore::from_env()?;
        let paths = crate::provisioning::collaboration_identity_paths();
        let fleets = DeviceFleetStore::at(registry.identity_dir(), paths.clone());
        Ok(Self {
            registry,
            paths,
            fleets,
        })
    }

    fn key(&self, room: &str) -> CacheKey {
        CacheKey {
            room: room.to_owned(),
            identity_dir: self.registry.identity_dir().to_path_buf(),
            device_key: self.paths.device_privkey.clone(),
            device_cert: self.paths.device_cert.clone(),
        }
    }

    fn registry_stamp(&self) -> Option<RegistryStamp> {
        Some(RegistryStamp {
            dir: dir_stamp(self.registry.identity_dir()).ok()?,
            file: FileStamp::of(self.registry.registry_path()).ok()?,
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
                key: match self.paths.device_key_custody {
                    DeviceKeyCustody::File => FileStamp::of(&self.paths.device_privkey).ok()?,
                    // A key with no file has no stamp to follow.
                    #[cfg(target_os = "macos")]
                    DeviceKeyCustody::Keychain => return None,
                },
                cert: FileStamp::of(&self.paths.device_cert_for_user(&selected.user_public_key))
                    .ok()?,
                fleet: self.fleets.stamps(&selected.id)?,
            }),
        };
        Some(CurrentDeviceStamp { registry, device })
    }

    /// The identity `room` selects now, read from the registry.
    fn selected(room: &str) -> io::Result<Option<Selected>> {
        Ok(
            crate::registry::selected_identity_profile(room)?.map(|profile| Selected {
                id: profile.id,
                user_public_key: profile.user_public_key,
            }),
        )
    }
}

/// `Ok(None)` for a missing directory, an error when it can't be examined.
fn dir_stamp(path: &Path) -> io::Result<Option<DirStamp>> {
    match std::fs::metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
        Ok(_) => DirStamp::of(path)
            .map(Some)
            .ok_or_else(|| io::Error::other("identity directory has no stamp")),
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

/// The stamp of everything `room`'s join proof is derived from: equal
/// settled stamps mean an unchanged proof, including whether there is one.
/// One `stat` per file once the room's selection is known; `None` when a
/// file can't be examined or the device key has no file, so the caller must
/// treat the proof as changed.
#[must_use]
pub fn current_device_stamp(room: &str) -> Option<CurrentDeviceStamp> {
    // An environment that names no identity directory names none on every
    // call: no identity, and nothing to change.
    let Ok(files) = Files::from_env() else {
        return Some(CurrentDeviceStamp {
            registry: RegistryStamp {
                dir: None,
                file: None,
            },
            device: None,
        });
    };
    let registry = files.registry_stamp()?;
    let key = files.key(room);
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
            Some(_) => Files::selected(room).ok()?,
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
/// selected in `room`, only while that certificate is valid, enrolled and not
/// revoked. Served from memory while every file it is derived from is
/// unchanged (see the module documentation); otherwise derived and signed
/// again through [`crate::broker::trusted_current_device`].
///
/// # Errors
///
/// As [`crate::broker::trusted_current_device`].
pub fn trusted_current_join_proof(room: &str) -> io::Result<CurrentDeviceJoinProof> {
    let files = Files::from_env()?;
    let key = files.key(room);
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
    let selected = Files::selected(room)?;
    let before = registry.and_then(|registry| files.stamp(registry, selected.as_ref()));
    let (device, certificate) = crate::broker::trusted_current_device(room)?;
    #[cfg(test)]
    MINTS.with(|mints| mints.set(mints.get().saturating_add(1)));
    let proof = CurrentDeviceJoinProof {
        proof: device
            .sign(&runtime_identity::collaboration_join_signing_bytes(room))
            .to_bytes(),
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
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime};

    use ed25519_dalek::Signer as _;
    use tilo_user_identity::{IdentityPaths, RevocationReason};

    use super::*;

    /// A provisioned identity the environment points at, every file of it
    /// dated back as it is a few seconds after its last write.
    struct Fixture {
        identity_dir: PathBuf,
        paths: IdentityPaths,
        identity_id: String,
        user_public_key: [u8; 32],
        room: String,
        _env: tilo_test_env::ScopedEnv,
        _root: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().expect("temp root");
            let xdg = temp.path().join("config");
            let system_root = temp.path().join("system");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("XDG_CONFIG_HOME", &xdg)
                .set("TILO_IDENTITY_ROOT", &system_root)
                .set(crate::provisioning::FILE_SECRETS_ENV, "1");
            let identity_dir = xdg.join("tilo/identity");
            let paths = IdentityPaths::under(&system_root);
            crate::provisioning::ensure_collaboration_identity(
                &identity_dir,
                &paths,
                crate::provisioning::IdentityStorage::Files,
            )
            .expect("provision");
            let room = uuid_like_room();
            let profile = crate::registry::selected_identity_profile(&room)
                .expect("registry")
                .expect("selected identity");
            let fixture = Self {
                identity_dir,
                paths,
                identity_id: profile.id,
                user_public_key: profile.user_public_key,
                room,
                _env: env,
                _root: temp,
            };
            fixture.settle();
            fixture
        }

        fn fleet_file(&self) -> PathBuf {
            self.identity_dir
                .join("fleets")
                .join(format!("{}.json", self.identity_id))
        }

        fn cert_file(&self) -> PathBuf {
            self.paths.device_cert_for_user(&self.user_public_key)
        }

        fn settle(&self) {
            for path in [
                self.identity_dir.clone(),
                IdentityRegistryStore::at(&self.identity_dir)
                    .registry_path()
                    .to_path_buf(),
                self.paths.device_privkey.clone(),
                self.cert_file(),
                self.fleet_file(),
            ] {
                settle(&path);
            }
        }

        fn store(&self) -> DeviceFleetStore {
            DeviceFleetStore::at(&self.identity_dir, self.paths.clone())
        }
    }

    /// Proofs minted from disk on this thread by `run`.
    fn mints(run: impl FnOnce()) -> usize {
        let before = mints_on_this_thread();
        run();
        mints_on_this_thread().saturating_sub(before)
    }

    fn uuid_like_room() -> String {
        tilo_desktop_model::RoomId::new().to_string()
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

    fn proof_issued_at(fixture: &Fixture) -> u64 {
        trusted_current_join_proof(&fixture.room)
            .expect("trusted device")
            .certificate
            .issued_at_ns
    }

    #[test]
    fn proof_is_minted_once_while_identity_files_are_unchanged() {
        let fixture = Fixture::new();
        let mut proofs = Vec::new();
        let fleet_loads = crate::device_fleet::loads_on_this_thread();
        let mints = mints(|| {
            for _ in 0..3 {
                proofs.push(trusted_current_join_proof(&fixture.room).expect("trusted device"));
            }
        });
        assert_eq!(mints, 1, "one mint serves every later call");
        assert!(
            crate::device_fleet::loads_on_this_thread().saturating_sub(fleet_loads) <= 1,
            "the fleet is read at most once"
        );
        assert!(proofs.windows(2).all(|pair| pair[0] == pair[1]));
        let proof = &proofs[0];
        ed25519_dalek::VerifyingKey::from_bytes(&proof.certificate.device_public_key)
            .expect("device key")
            .verify_strict(
                &runtime_identity::collaboration_join_signing_bytes(&fixture.room),
                &ed25519_dalek::Signature::from_bytes(&proof.proof),
            )
            .expect("the proof is this device's signature for this room");
        let other = uuid_like_room();
        let other_proof = trusted_current_join_proof(&other).expect("another room");
        assert_ne!(other_proof.proof, proof.proof, "a proof binds its room");
    }

    #[test]
    fn revocation_fails_the_very_next_call() {
        let fixture = Fixture::new();
        trusted_current_join_proof(&fixture.room).expect("trusted device");
        let stamp = current_device_stamp(&fixture.room).expect("stamp");
        assert_eq!(
            mints(|| {
                trusted_current_join_proof(&fixture.room).expect("cached");
            }),
            0
        );
        let device = trusted_current_join_proof(&fixture.room)
            .expect("trusted device")
            .certificate
            .device_public_key;
        fixture
            .store()
            .revoke(
                &fixture.identity_id,
                device,
                RevocationReason::Lost,
                proof_issued_at(&fixture).saturating_add(1),
            )
            .expect("revoke");
        let refused =
            trusted_current_join_proof(&fixture.room).expect_err("a revoked device has no proof");
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
        assert_ne!(current_device_stamp(&fixture.room), Some(stamp));
        // Even once the revoked fleet settles, nothing was cached.
        fixture.settle();
        assert!(trusted_current_join_proof(&fixture.room).is_err());
    }

    #[test]
    fn rewritten_certificate_or_key_mints_again() {
        let fixture = Fixture::new();
        trusted_current_join_proof(&fixture.room).expect("trusted device");
        // An in-place rewrite of the same bytes still changes the stamp.
        for path in [fixture.cert_file(), fixture.paths.device_privkey.clone()] {
            let bytes = std::fs::read(&path).expect("read");
            std::fs::write(&path, &bytes).expect("rewrite");
            assert_eq!(
                mints(|| {
                    trusted_current_join_proof(&fixture.room).expect("still trusted");
                }),
                1,
                "{} changed",
                path.display()
            );
            fixture.settle();
            trusted_current_join_proof(&fixture.room).expect("re-cached");
        }
        std::fs::write(fixture.cert_file(), b"malformed").expect("break certificate");
        assert!(trusted_current_join_proof(&fixture.room).is_err());
    }

    /// Wall time of re-deriving a room's proof from disk as every action
    /// call did before (registry, key, certificate, an uncached fleet load,
    /// a signature) against serving it from its stamps. Prints the numbers
    /// (`--nocapture`); the mint counts above are the regression guard.
    #[test]
    fn join_proof_cost() {
        const ROUNDS: u32 = 200;
        let fixture = Fixture::new();
        trusted_current_join_proof(&fixture.room).expect("warm");
        let started = std::time::Instant::now();
        for _ in 0..ROUNDS {
            let (key, _) =
                crate::broker::trusted_current_device(&fixture.room).expect("trusted device");
            fixture.store().load(&fixture.identity_id).expect("fleet");
            let proof = key.sign(&runtime_identity::collaboration_join_signing_bytes(
                &fixture.room,
            ));
            assert_eq!(proof.to_bytes().len(), 64);
        }
        let minted = started.elapsed();
        let mut cached = Duration::ZERO;
        let mints = mints(|| {
            let started = std::time::Instant::now();
            for _ in 0..ROUNDS {
                trusted_current_join_proof(&fixture.room).expect("cached proof");
            }
            cached = started.elapsed();
        });
        assert_eq!(mints, 0);
        eprintln!(
            "TIL-593 join proof: re-derived from disk {:?}/call, served from stamps {:?}/call",
            minted.checked_div(ROUNDS).unwrap_or_default(),
            cached.checked_div(ROUNDS).unwrap_or_default(),
        );
    }

    #[test]
    fn a_desktop_without_identity_has_a_stable_stamp() {
        let temp = tempfile::tempdir().expect("temp root");
        let config = temp.path().join("config");
        let system = temp.path().join("system");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_CONFIG_HOME", &config)
            .set("TILO_IDENTITY_ROOT", &system)
            .set(crate::provisioning::FILE_SECRETS_ENV, "1");
        let room = uuid_like_room();
        let stamp = current_device_stamp(&room).expect("no identity is a known state");
        assert!(stamp.settled());
        assert_eq!(current_device_stamp(&room), Some(stamp));
        assert!(trusted_current_join_proof(&room).is_err());
    }

    #[test]
    fn unsettled_files_are_never_cached() {
        let fixture = Fixture::new();
        std::fs::write(
            fixture.fleet_file(),
            std::fs::read(fixture.fleet_file()).expect("fleet"),
        )
        .expect("touch fleet");
        let mints = mints(|| {
            trusted_current_join_proof(&fixture.room).expect("trusted");
            trusted_current_join_proof(&fixture.room).expect("trusted");
        });
        assert_eq!(mints, 2, "a fleet written just now is read on every call");
        assert!(
            !current_device_stamp(&fixture.room)
                .expect("stamp")
                .settled()
        );
    }
}
