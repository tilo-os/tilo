//! This person's own devices: those enrolled in `personal-device-peers.json`,
//! and every personal device their root certified that this device's fleet
//! keeps, less the ones the fleet revoked.
//!
//! Enrolment lists the devices known when this device joined; the fleet also
//! keeps each device of the person met since ([`PersonalDevicePeers::remember`]),
//! so the person's devices trust and dial each other whichever joined first.
//!
//! Other processes write both files. Every outgoing personal-fabric message
//! (each 20 ms remote-audio frame, each forwarded input event) and the
//! per-frame owner-view pump consult the set, many of them on the
//! compositor's loop, which must never wait on the disk: not to read the
//! files, and not to `stat` them to see whether they changed. So they are
//! kept as a [`Watched`] value: read when the network starts (off the loop),
//! and again on a worker whenever their directories change. Removing a
//! device stops sends to it as soon as the worker has read the change; a
//! revocation this session learns ([`PersonalDevicePeers::revoke`]) applies
//! at once, and route revocation stays independent and immediate.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tilo_files::native::Watched;

use crate::device_fleet::{DeviceFleet, DeviceFleetStore};
use crate::shell::personal_continuity::DeviceId;

/// Where this person's own devices are recorded.
pub(super) struct Sources {
    /// The enrolment file.
    pub(super) peers: PathBuf,
    /// The person and the store keeping their fleet; `None` on a device
    /// without a person.
    pub(super) fleet: Option<([u8; 32], DeviceFleetStore)>,
}

impl Sources {
    /// Just the enrolment file at `peers`.
    #[cfg(test)]
    pub(super) fn peers(peers: PathBuf) -> Self {
        Self { peers, fleet: None }
    }

    /// The directories the files live in.
    fn directories(&self) -> Vec<PathBuf> {
        let mut files = vec![self.peers.clone()];
        if let Some((person, store)) = &self.fleet {
            files.extend(store.backing_files(&hex::encode(person)));
        }
        let mut directories: Vec<PathBuf> = Vec::new();
        for directory in files.iter().map(|file| {
            file.parent()
                .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
        }) {
            if !directories.contains(&directory) {
                directories.push(directory);
            }
        }
        directories
    }

    /// Read both, on the worker. `reads` counts the reads of the enrolment
    /// file, for tests.
    fn read(&self, reads: &AtomicU64) -> Read {
        let enrolled = if self.peers.exists() {
            reads.fetch_add(1, Ordering::AcqRel);
            super::read_personal_device_peers(&self.peers)
        } else {
            Vec::new()
        };
        let fleet = self.fleet.as_ref().and_then(|(person, store)| {
            store
                .load_cached(&hex::encode(person))
                .ok()
                .map(|fleet| (*person, fleet))
        });
        Read { enrolled, fleet }
    }
}

struct Read {
    /// The valid endpoint keys in the enrolment file.
    enrolled: Vec<[u8; 32]>,
    /// The person and their fleet.
    fleet: Option<([u8; 32], Arc<DeviceFleet>)>,
}

impl Read {
    fn fleet(&self) -> Option<&DeviceFleet> {
        self.fleet.as_ref().map(|(_, fleet)| &**fleet)
    }
}

/// The certified recipient set, valid for one local device and one version
/// of the device set and of the registered home-device ids.
struct Recipients {
    local: Option<DeviceId>,
    set: Arc<BTreeSet<DeviceId>>,
}

/// This person's own devices, kept current off the loop, and the certified
/// secret recipients derived from them. Lives on `ShellNetwork`.
#[derive(Default)]
pub(super) struct PersonalDevicePeers {
    watched: Option<Watched<Read>>,
    /// The reading the set was last derived from.
    derived_from: Option<Arc<Read>>,
    /// Devices of the person met this session, until the fleet keeps them.
    met: BTreeSet<[u8; 32]>,
    /// Devices revoked by a revocation this session learned.
    revoked: BTreeSet<[u8; 32]>,
    /// Every own device, this device included; callers filter their own key,
    /// since they name it differently.
    devices: Arc<[[u8; 32]]>,
    recipients: Option<Recipients>,
    /// How many times the enrolment file was read. Observed by tests only.
    reads: Arc<AtomicU64>,
}

impl PersonalDevicePeers {
    /// Watch `sources` from now on: read them on this thread (the network's
    /// start, off the loop), then on a worker after each change.
    pub(super) fn watching(sources: Sources) -> Self {
        let mut peers = Self::default();
        peers.watch(sources);
        peers
    }

    fn watch(&mut self, sources: Sources) {
        let reads = Arc::clone(&self.reads);
        let directories = sources.directories();
        match Watched::start("own-devices", directories, move || sources.read(&reads)) {
            Ok(watched) => self.watched = Some(watched),
            Err(error) => {
                // Unwatched, only devices met this session count: personal
                // secrets and fabric actions fail closed to them.
                tracing::warn!(%error, "cannot watch this person's devices");
            }
        }
    }

    /// The reading the worker last made, the set derived again if it is a
    /// new one. `sources` is asked for only by a network that did not watch
    /// them from its start (a test's).
    fn current(&mut self, sources: impl FnOnce() -> Sources) {
        if self.watched.is_none() {
            self.watch(sources());
        }
        let Some(read) = self.watched.as_ref().map(Watched::get) else {
            return;
        };
        if self
            .derived_from
            .as_ref()
            .is_none_or(|derived| !Arc::ptr_eq(derived, &read))
        {
            self.derived_from = Some(read);
            self.derive();
        }
    }

    /// The own device keys, as the worker last read their sources.
    pub(super) fn devices_at(&mut self, sources: impl FnOnce() -> Sources) -> Arc<[[u8; 32]]> {
        self.current(sources);
        Arc::clone(&self.devices)
    }

    /// The certified recipient set for `local`, rebuilt by `build` from the
    /// own device keys only when they, the local device or the registered
    /// home-device ids (see [`Self::invalidate_recipients`]) changed.
    pub(super) fn recipients_at(
        &mut self,
        sources: impl FnOnce() -> Sources,
        local: Option<&DeviceId>,
        build: impl FnOnce(&[[u8; 32]]) -> BTreeSet<DeviceId>,
    ) -> Arc<BTreeSet<DeviceId>> {
        self.current(sources);
        if let Some(recipients) = &self.recipients
            && recipients.local.as_ref() == local
        {
            return Arc::clone(&recipients.set);
        }
        let set = Arc::new(build(&self.devices));
        self.recipients = Some(Recipients {
            local: local.cloned(),
            set: Arc::clone(&set),
        });
        set
    }

    /// The fleet as the worker last read it.
    fn fleet(&self) -> Option<Arc<Read>> {
        self.watched.as_ref().map(Watched::get)
    }

    /// Whether the fleet already keeps `cert`.
    pub(super) fn keeps(&self, cert: &tilo_user_identity::DeviceCert) -> bool {
        self.fleet().is_some_and(|read| {
            read.fleet()
                .is_some_and(|fleet| fleet.certificates.contains(cert))
        })
    }

    pub(super) fn fleet_revoked(&self, device: &[u8; 32]) -> bool {
        self.fleet()
            .is_some_and(|read| read.fleet().is_some_and(|fleet| fleet.is_revoked(device)))
    }

    /// Whether `device` is revoked, by the fleet or a revocation this
    /// session learned.
    pub(super) fn is_revoked(&self, device: &[u8; 32]) -> bool {
        self.revoked.contains(device) || self.fleet_revoked(device)
    }

    /// Count `device`, a device of the person that proved its certificate,
    /// as one of theirs from now on: the caller has it kept in the fleet,
    /// which the worker reads once it is written.
    pub(super) fn remember(&mut self, device: [u8; 32]) {
        if !self.revoked.contains(&device) && self.met.insert(device) {
            self.derive();
        }
    }

    /// Stop counting `devices`, revoked by the person's root, at once.
    pub(super) fn revoke(&mut self, devices: impl IntoIterator<Item = [u8; 32]>) {
        let before = self.revoked.len();
        self.revoked.extend(devices);
        if self.revoked.len() != before {
            self.derive();
        }
    }

    /// Forget the derived recipient set; the next query rebuilds it. Called
    /// whenever the registered home-device ids change.
    pub(super) fn invalidate_recipients(&mut self) {
        self.recipients = None;
    }

    /// Read the sources again now, on this thread: for tests that rewrite
    /// them mid-session and must not wait for the worker.
    #[cfg(test)]
    pub(super) fn reread(&self) {
        if let Some(watched) = &self.watched {
            watched.refresh();
        }
    }

    #[cfg(test)]
    pub(super) fn reads(&self) -> u64 {
        self.reads.load(Ordering::Acquire)
    }

    /// Rebuild the device set: enrolled, certified by the fleet or met, less
    /// what the fleet or this session revoked.
    fn derive(&mut self) {
        let now_ns = tilo_clock::unix_now_ns();
        let read = self.derived_from.clone();
        let enrolled = read.as_ref().map_or(&[][..], |read| &read.enrolled[..]);
        let certified = read
            .as_ref()
            .and_then(|read| read.fleet.as_ref())
            .map(|(person, fleet)| fleet.personal_devices(person, now_ns))
            .unwrap_or_default();
        let fleet = read.as_ref().and_then(|read| read.fleet());
        let mut devices: Vec<[u8; 32]> = Vec::new();
        for device in enrolled.iter().chain(&certified).chain(&self.met).copied() {
            let revoked = self.revoked.contains(&device)
                || fleet.is_some_and(|fleet| fleet.is_revoked(&device));
            if !devices.contains(&device) && !revoked {
                devices.push(device);
            }
        }
        self.devices = devices.into();
        self.recipients = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A valid endpoint key: the file keeps only those.
    fn endpoint() -> [u8; 32] {
        *iroh::SecretKey::generate().public().as_bytes()
    }

    fn write(path: &Path, devices: &[[u8; 32]]) {
        let ids = devices.iter().map(hex::encode).collect::<Vec<_>>();
        let staged = path.with_extension("staged");
        std::fs::write(&staged, serde_json::to_vec(&ids).unwrap()).unwrap();
        std::fs::rename(&staged, path).unwrap();
    }

    #[test]
    fn devices_are_read_off_the_asking_thread_when_the_file_changes() {
        let dir = tempfile::tempdir().expect("temp");
        let path = dir.path().join("personal-device-peers.json");
        let (a, b) = (endpoint(), endpoint());
        write(&path, &[a, b]);
        let mut cache = PersonalDevicePeers::watching(Sources::peers(path.clone()));
        for _ in 0..1_000 {
            assert_eq!(&*cache.devices_at(|| unreachable!("watched")), &[a, b]);
        }
        assert_eq!(cache.reads(), 1, "asking never reads the file");

        write(&path, &[a]);
        tilo_test_env::wait_until("the worker to read the change", || {
            *cache.devices_at(|| unreachable!("watched")) == [a]
        });
        std::fs::remove_file(&path).unwrap();
        tilo_test_env::wait_until("the worker to see the file go", || {
            cache.devices_at(|| unreachable!("watched")).is_empty()
        });
    }

    #[test]
    fn recipients_follow_the_file_the_local_device_and_invalidation() {
        let dir = tempfile::tempdir().expect("temp");
        let path = dir.path().join("personal-device-peers.json");
        let (a, b) = (endpoint(), endpoint());
        write(&path, &[a, b]);
        let id = |key: &[u8; 32]| DeviceId::new(hex::encode(key)).unwrap();
        let local = DeviceId::new("local").unwrap();
        let mut builds = 0;
        let mut cache = PersonalDevicePeers::watching(Sources::peers(path.clone()));
        let mut query = |cache: &mut PersonalDevicePeers, local: Option<&DeviceId>| {
            cache.recipients_at(
                || unreachable!("watched"),
                local,
                |devices| {
                    builds += 1;
                    local
                        .cloned()
                        .into_iter()
                        .chain(devices.iter().map(id))
                        .collect()
                },
            )
        };
        for _ in 0..100 {
            assert!(query(&mut cache, Some(&local)).contains(&id(&b)));
        }
        assert!(query(&mut cache, None).contains(&id(&a)));
        cache.invalidate_recipients();
        let set = query(&mut cache, None);
        write(&path, &[a]);
        cache.reread();
        let after = query(&mut cache, None);
        assert!(set.contains(&id(&b)) && !after.contains(&id(&b)));
        assert_eq!(builds, 4, "built once per change, not per query");
    }
}
