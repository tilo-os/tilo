//! This person's own devices: those enrolled in `personal-device-peers.json`,
//! and every personal device their root certified that this device's fleet
//! keeps, less the ones the fleet revoked.
//!
//! Enrolment lists the devices known when this device joined; the fleet also
//! keeps each device of the person met since ([`PersonalDevicePeers::remember`]),
//! so the person's devices trust and dial each other whichever joined first.
//!
//! Other processes write both files, so the shell cannot be told when they
//! change. Every outgoing personal-fabric message — each 20 ms remote-audio
//! frame, each forwarded input event — and the per-frame owner-view pump
//! consult the set, so reading and parsing the files per call put disk I/O on
//! the compositor thread. Instead the set is re-validated at most once per
//! [`RECHECK`] against the enrolment file's `stat`, re-read only when it
//! changed, and against the fleet the caller last loaded: the compositor's
//! fleet is loaded by the home-peer reader's thread, never on its loop.
//! Removing a device therefore stops sends to it within [`RECHECK`] (the
//! fleet within one home-peer refresh more); a
//! revocation this session learns ([`PersonalDevicePeers::revoke`]) applies at
//! once, and route revocation stays independent and immediate.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crate::device_fleet::{DeviceFleet, DeviceFleetStore};
use crate::shell::personal_continuity::DeviceId;

/// How long a check of the enrolment file stays fresh. Mirrors the
/// continuity-policy store poll, so revocation lands within about a second.
pub(super) const RECHECK: Duration = Duration::from_millis(750);

/// What identifies one version of the file without reading it. The identity
/// host writes atomically (a fresh inode), and length and mtime catch an
/// in-place rewrite.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileStamp {
    modified: Option<SystemTime>,
    len: u64,
    inode: u64,
}

impl FileStamp {
    fn of(path: &Path) -> Option<Self> {
        use std::os::unix::fs::MetadataExt as _;
        let metadata = std::fs::metadata(path).ok()?;
        Some(Self {
            modified: metadata.modified().ok(),
            len: metadata.len(),
            inode: metadata.ino(),
        })
    }
}

/// This person's own devices as recorded: the enrolment file, and the fleet
/// already loaded. The fleet is loaded by whoever builds this, off the
/// compositor's loop (see `ShellNetwork::own_device_sources`).
pub(super) struct Sources {
    /// The enrolment file.
    pub(super) peers: PathBuf,
    /// The person; `None` on a device without one.
    pub(super) person: Option<[u8; 32]>,
    /// The person's fleet as last loaded, if it was.
    pub(super) fleet: Option<Arc<DeviceFleet>>,
}

impl Sources {
    /// Just the enrolment file at `peers`.
    #[cfg(test)]
    pub(super) fn peers(peers: PathBuf) -> Self {
        Self {
            peers,
            person: None,
            fleet: None,
        }
    }
}

/// `person`'s fleet kept by `store`, read now: waits for the disk.
pub(super) fn load_fleet(person: &[u8; 32], store: &DeviceFleetStore) -> Option<Arc<DeviceFleet>> {
    store.load_cached(&hex::encode(person)).ok()
}

/// The certified recipient set, valid for one local device and one version
/// of the device set and of the registered home-device ids.
struct Recipients {
    local: Option<DeviceId>,
    set: Arc<BTreeSet<DeviceId>>,
}

/// Bounded-staleness cache of this person's own devices and of the certified
/// secret recipients derived from them. Lives on `ShellNetwork`.
#[derive(Default)]
pub(super) struct PersonalDevicePeers {
    path: Option<PathBuf>,
    stamp: Option<FileStamp>,
    checked_at: Option<Instant>,
    /// The valid endpoint keys in the enrolment file.
    enrolled: Vec<[u8; 32]>,
    /// The fleet the set was last derived from.
    fleet: Option<Arc<DeviceFleet>>,
    /// The person the fleet belongs to.
    person: Option<[u8; 32]>,
    /// Devices of the person met this session, until the fleet keeps them.
    met: BTreeSet<[u8; 32]>,
    /// Devices revoked by a revocation this session learned.
    revoked: BTreeSet<[u8; 32]>,
    /// Every own device, this device included; callers filter their own key,
    /// since they name it differently.
    devices: Arc<[[u8; 32]]>,
    recipients: Option<Recipients>,
    /// How many times the enrolment file was read. Observed by tests only.
    reads: u64,
}

impl PersonalDevicePeers {
    /// The own device keys, re-validated against `sources` if the last check
    /// is older than [`RECHECK`]. `sources` is resolved only then.
    pub(super) fn devices_at(
        &mut self,
        sources: impl FnOnce() -> Sources,
        now: Instant,
    ) -> Arc<[[u8; 32]]> {
        self.refresh(sources, now);
        Arc::clone(&self.devices)
    }

    /// The certified recipient set for `local`, rebuilt by `build` from the
    /// own device keys only when they, the local device or the registered
    /// home-device ids (see [`Self::invalidate_recipients`]) changed.
    pub(super) fn recipients_at(
        &mut self,
        sources: impl FnOnce() -> Sources,
        now: Instant,
        local: Option<&DeviceId>,
        build: impl FnOnce(&[[u8; 32]]) -> BTreeSet<DeviceId>,
    ) -> Arc<BTreeSet<DeviceId>> {
        self.refresh(sources, now);
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

    /// Whether the fleet the set was derived from already keeps `cert`.
    pub(super) fn keeps(&self, cert: &tilo_user_identity::DeviceCert) -> bool {
        self.fleet
            .as_ref()
            .is_some_and(|fleet| fleet.certificates.contains(cert))
    }

    /// Whether the fleet the set was derived from already revoked `device`.
    pub(super) fn fleet_revoked(&self, device: &[u8; 32]) -> bool {
        self.fleet
            .as_ref()
            .is_some_and(|fleet| fleet.is_revoked(device))
    }

    /// Whether `device` is revoked, by the fleet or a revocation this
    /// session learned.
    pub(super) fn is_revoked(&self, device: &[u8; 32]) -> bool {
        self.revoked.contains(device) || self.fleet_revoked(device)
    }

    /// Count `device`, a device of the person that proved its certificate,
    /// as one of theirs from now on: the caller has it kept in the fleet,
    /// which the set reads from the next write on.
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

    /// Make the next query re-validate the files regardless of [`RECHECK`].
    #[cfg(test)]
    pub(super) fn expire(&mut self) {
        self.checked_at = None;
    }

    #[cfg(test)]
    pub(super) fn reads(&self) -> u64 {
        self.reads
    }

    fn refresh(&mut self, sources: impl FnOnce() -> Sources, now: Instant) {
        if self
            .checked_at
            .is_some_and(|checked| now.saturating_duration_since(checked) < RECHECK)
        {
            return;
        }
        self.checked_at = Some(now);
        let sources = sources();
        let mut changed = false;
        let stamp = FileStamp::of(&sources.peers);
        if self.path.as_ref() != Some(&sources.peers) || stamp != self.stamp {
            self.enrolled = if stamp.is_some() {
                self.reads = self.reads.saturating_add(1);
                super::read_personal_device_peers(&sources.peers)
            } else {
                Vec::new()
            };
            self.path = Some(sources.peers.clone());
            self.stamp = stamp;
            changed = true;
        }
        let Sources { person, fleet, .. } = sources;
        let same_fleet = match (&fleet, &self.fleet) {
            (Some(new), Some(old)) => Arc::ptr_eq(new, old),
            (None, None) => true,
            _ => false,
        };
        if !same_fleet || person != self.person {
            self.fleet = fleet;
            self.person = person;
            changed = true;
        }
        if changed {
            self.derive();
        }
    }

    /// Rebuild the device set: enrolled, certified by the fleet or met, less
    /// what the fleet or this session revoked.
    fn derive(&mut self) {
        let now_ns = tilo_clock::unix_now_ns();
        let certified = match (&self.fleet, &self.person) {
            (Some(fleet), Some(person)) => fleet.personal_devices(person, now_ns),
            _ => Vec::new(),
        };
        let mut devices: Vec<[u8; 32]> = Vec::new();
        for device in self
            .enrolled
            .iter()
            .chain(&certified)
            .chain(&self.met)
            .copied()
        {
            if !devices.contains(&device) && !self.is_revoked(&device) {
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
        // Through a fresh file, as the identity host's atomic write does.
        let staged = path.with_extension("staged");
        std::fs::write(&staged, serde_json::to_vec(&ids).unwrap()).unwrap();
        std::fs::rename(&staged, path).unwrap();
    }

    #[test]
    fn devices_are_reread_only_when_the_file_changes() {
        let dir = tempfile::tempdir().expect("temp");
        let path = dir.path().join("personal-device-peers.json");
        let (a, b) = (endpoint(), endpoint());
        write(&path, &[a, b]);
        let mut cache = PersonalDevicePeers::default();
        let start = Instant::now();
        for _ in 0..1_000 {
            assert_eq!(
                &*cache.devices_at(|| Sources::peers(path.clone()), start),
                &[a, b]
            );
        }
        assert_eq!(cache.reads(), 1);
        // A due recheck of an unchanged file stats it but does not read it.
        assert_eq!(
            &*cache.devices_at(|| Sources::peers(path.clone()), start + RECHECK),
            &[a, b]
        );
        assert_eq!(cache.reads(), 1);

        write(&path, &[a]);
        assert_eq!(
            &*cache.devices_at(|| Sources::peers(path.clone()), start + RECHECK),
            &[a, b],
            "within the recheck window the last check stands"
        );
        assert_eq!(
            &*cache.devices_at(|| Sources::peers(path.clone()), start + RECHECK * 2),
            &[a]
        );
        assert_eq!(cache.reads(), 2);

        std::fs::remove_file(&path).unwrap();
        assert!(
            cache
                .devices_at(|| Sources::peers(path.clone()), start + RECHECK * 3)
                .is_empty()
        );
        assert_eq!(cache.reads(), 2, "a missing file is not read");
    }

    #[test]
    fn the_path_is_resolved_only_when_a_recheck_is_due() {
        let dir = tempfile::tempdir().expect("temp");
        let first = dir.path().join("first.json");
        let second = dir.path().join("second.json");
        let (a, b) = (endpoint(), endpoint());
        write(&first, &[a]);
        write(&second, &[b]);
        let mut cache = PersonalDevicePeers::default();
        let now = Instant::now();
        assert_eq!(
            &*cache.devices_at(|| Sources::peers(first.clone()), now),
            &[a]
        );
        assert_eq!(
            &*cache.devices_at(|| unreachable!("fresh check"), now),
            &[a]
        );
        assert_eq!(
            &*cache.devices_at(|| Sources::peers(second.clone()), now + RECHECK),
            &[b],
            "a moved file is followed at the next recheck"
        );
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
        let mut cache = PersonalDevicePeers::default();
        let start = Instant::now();
        let mut query = |cache: &mut PersonalDevicePeers, now, local: Option<&DeviceId>| {
            cache.recipients_at(
                || Sources::peers(path.clone()),
                now,
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
            assert!(query(&mut cache, start, Some(&local)).contains(&id(&b)));
        }
        assert!(query(&mut cache, start, None).contains(&id(&a)));
        cache.invalidate_recipients();
        let set = query(&mut cache, start, None);
        write(&path, &[a]);
        let after = query(&mut cache, start + RECHECK, None);
        assert!(set.contains(&id(&b)) && !after.contains(&id(&b)));
        assert_eq!(builds, 4, "built once per change, not per query");
    }
}
