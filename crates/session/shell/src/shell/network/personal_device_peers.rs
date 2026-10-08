//! This account's person-scoped enrollment and its certified recipients.
//!
//! The identity host writes that file from another process, so the shell
//! cannot be told when it changes. Every outgoing personal-fabric message —
//! each 20 ms remote-audio frame, each forwarded input event — and the
//! per-frame owner-view pump consult it, so reading and parsing it per call
//! put disk I/O on the compositor thread. Instead it is re-validated at most
//! once per [`RECHECK`]: a `stat`, and a re-read only when the file's identity
//! (mtime, length, inode) changed. Removing a device therefore stops sends to
//! it within [`RECHECK`]. Root-signed revocations are checked independently
//! on every query, including while an enrollment check remains fresh.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crate::shell::personal_continuity::DeviceId;
use tilo_relay_protocol::personal_devices::PersonPeers;
use tilo_user_identity::RevocationList;

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
    owner: u32,
    mode: u32,
    changed: (i64, i64),
}

impl FileStamp {
    fn of(path: &Path) -> Option<Self> {
        use std::os::unix::fs::MetadataExt as _;
        let metadata = std::fs::symlink_metadata(path).ok()?;
        Some(Self {
            modified: metadata.modified().ok(),
            len: metadata.len(),
            inode: metadata.ino(),
            owner: metadata.uid(),
            mode: metadata.mode(),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        })
    }
}

/// The certified recipient set, valid for one local device and one version
/// of the enrolment file and of the registered home-device ids.
struct Recipients {
    local: Option<DeviceId>,
    set: Arc<BTreeSet<DeviceId>>,
}

/// Bounded-staleness cache of `personal-device-peers.json` and of the
/// certified secret recipients derived from it. Lives on `ShellNetwork`.
#[derive(Default)]
pub(super) struct PersonalDevicePeers {
    scope: Option<[u8; 32]>,
    person: Option<PersonPeers>,
    stamp: Option<FileStamp>,
    checked_at: Option<Instant>,
    /// Every valid endpoint key in the file, this device included; callers
    /// filter their own key, since they name it differently.
    devices: Arc<[[u8; 32]]>,
    enrolled: Arc<[[u8; 32]]>,
    revoked: BTreeSet<[u8; 32]>,
    recipients: Option<Recipients>,
    /// How many times the file was read. Observed by tests only.
    reads: u64,
}

impl PersonalDevicePeers {
    /// Live device keys for `scope`. Resolve its directory only when a
    /// recheck is due or the person changes; revocation is checked every time.
    pub(super) fn devices_at(
        &mut self,
        scope: Option<[u8; 32]>,
        person: impl FnOnce() -> Option<PersonPeers>,
        now: Instant,
        known: &RevocationList,
    ) -> Arc<[[u8; 32]]> {
        self.refresh(scope, person, now, known);
        Arc::clone(&self.devices)
    }

    /// The certified recipient set for `local`, rebuilt by `build` from the
    /// enrolled keys only when the file, the local device or the registered
    /// home-device ids (see [`Self::invalidate_recipients`]) changed.
    pub(super) fn recipients_at(
        &mut self,
        scope: Option<[u8; 32]>,
        person: impl FnOnce() -> Option<PersonPeers>,
        now: Instant,
        known: &RevocationList,
        local: Option<&DeviceId>,
        build: impl FnOnce(&[[u8; 32]]) -> BTreeSet<DeviceId>,
    ) -> Arc<BTreeSet<DeviceId>> {
        self.refresh(scope, person, now, known);
        if let Some(recipients) = &self.recipients
            && recipients.local.as_ref() == local
        {
            return Arc::clone(&recipients.set);
        }
        let mut set = build(&self.devices);
        set.retain(|device| {
            super::decode_lower_hex_32(device.as_str())
                .is_none_or(|key| !self.revoked.contains(&key))
        });
        let set = Arc::new(set);
        self.recipients = Some(Recipients {
            local: local.cloned(),
            set: Arc::clone(&set),
        });
        set
    }

    /// Forget the derived recipient set; the next query rebuilds it. Called
    /// whenever the registered home-device ids change.
    pub(super) fn invalidate_recipients(&mut self) {
        self.recipients = None;
    }

    /// Make the next query re-validate the file regardless of [`RECHECK`].
    #[cfg(test)]
    pub(super) fn expire(&mut self) {
        self.checked_at = None;
    }

    #[cfg(test)]
    pub(super) fn reads(&self) -> u64 {
        self.reads
    }

    fn refresh(
        &mut self,
        scope: Option<[u8; 32]>,
        resolve: impl FnOnce() -> Option<PersonPeers>,
        now: Instant,
        known: &RevocationList,
    ) {
        if self.scope != scope {
            // Identity switches cannot borrow a still-fresh cache from
            // another person, including its registered recipient set.
            self.scope = scope;
            self.person = None;
            self.stamp = None;
            self.checked_at = None;
            self.enrolled = Arc::from([]);
            self.recipients = None;
        }
        if self
            .checked_at
            .is_none_or(|checked| now.saturating_duration_since(checked) >= RECHECK)
        {
            self.checked_at = Some(now);
            let person = resolve().filter(|person| Some(*person.identity()) == scope);
            let stamp = person
                .as_ref()
                .and_then(|person| FileStamp::of(&person.direct_peers_path()));
            if self.person != person || self.stamp != stamp {
                self.enrolled = if let Some(person) = &person
                    && stamp.is_some()
                {
                    self.reads = self.reads.saturating_add(1);
                    person
                        .read_direct_peers()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|peer| peer.device)
                        .filter(|device| iroh::EndpointId::from_bytes(device).is_ok())
                        .collect::<Vec<_>>()
                        .into()
                } else {
                    Arc::from([])
                };
                self.person = person;
                self.stamp = stamp;
                self.recipients = None;
            }
        }
        let recorded = self.person.as_ref().map(PersonPeers::read_revocations);
        let trusted = matches!(&recorded, Some(Ok(_)));
        let revoked: BTreeSet<_> = known
            .iter()
            .chain(
                recorded
                    .as_ref()
                    .and_then(|list| list.as_ref().ok())
                    .into_iter()
                    .flat_map(RevocationList::iter),
            )
            .filter(|entry| Some(entry.user_public_key) == scope)
            .map(|entry| entry.revoked_device_public_key)
            .collect();
        let devices: Vec<_> = self
            .enrolled
            .iter()
            .copied()
            .filter(|device| trusted && !revoked.contains(device))
            .collect();
        if *self.devices != devices || self.revoked != revoked {
            self.devices = devices.into();
            self.revoked = revoked;
            self.recipients = None;
        }
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
        let peers = devices
            .iter()
            .map(
                |&device| tilo_relay_protocol::personal_devices::DirectPeer {
                    identity: [0x44; 32],
                    device,
                },
            )
            .collect::<Vec<_>>();
        // Through a fresh file, as the identity host's atomic write does.
        let staged = path.with_extension("staged");
        std::fs::write(
            &staged,
            tilo_relay_protocol::personal_devices::encode_direct_peers(&peers).unwrap(),
        )
        .unwrap();
        std::fs::rename(&staged, path).unwrap();
    }

    #[test]
    fn devices_are_reread_only_when_the_file_changes() {
        let dir = tempfile::tempdir().expect("temp");
        let person = PersonPeers::open_or_create_own(dir.path(), &[0x44; 32]).unwrap();
        let path = person.direct_peers_path();
        let none = RevocationList::new();
        let (a, b) = (endpoint(), endpoint());
        write(&path, &[a, b]);
        let mut cache = PersonalDevicePeers::default();
        let start = Instant::now();
        for _ in 0..1_000 {
            assert_eq!(
                &*cache.devices_at(Some([0x44; 32]), || Some(person.clone()), start, &none),
                &[a, b]
            );
        }
        assert_eq!(cache.reads(), 1);
        // A due recheck of an unchanged file stats it but does not read it.
        assert_eq!(
            &*cache.devices_at(
                Some([0x44; 32]),
                || Some(person.clone()),
                start + RECHECK,
                &none
            ),
            &[a, b]
        );
        assert_eq!(cache.reads(), 1);

        write(&path, &[a]);
        assert_eq!(
            &*cache.devices_at(
                Some([0x44; 32]),
                || Some(person.clone()),
                start + RECHECK,
                &none
            ),
            &[a, b],
            "within the recheck window the last check stands"
        );
        assert_eq!(
            &*cache.devices_at(
                Some([0x44; 32]),
                || Some(person.clone()),
                start + RECHECK * 2,
                &none
            ),
            &[a]
        );
        assert_eq!(cache.reads(), 2);

        std::fs::remove_file(&path).unwrap();
        assert!(
            cache
                .devices_at(
                    Some([0x44; 32]),
                    || Some(person.clone()),
                    start + RECHECK * 3,
                    &none
                )
                .is_empty()
        );
        assert_eq!(cache.reads(), 2, "a missing file is not read");
    }

    #[test]
    fn the_path_is_resolved_only_when_a_recheck_is_due() {
        let dir = tempfile::tempdir().expect("temp");
        let first_person =
            PersonPeers::open_or_create_own(&dir.path().join("first"), &[0x44; 32]).unwrap();
        let second_person =
            PersonPeers::open_or_create_own(&dir.path().join("second"), &[0x44; 32]).unwrap();
        let first = first_person.direct_peers_path();
        let second = second_person.direct_peers_path();
        let none = RevocationList::new();
        let (a, b) = (endpoint(), endpoint());
        write(&first, &[a]);
        write(&second, &[b]);
        let mut cache = PersonalDevicePeers::default();
        let now = Instant::now();
        assert_eq!(
            &*cache.devices_at(Some([0x44; 32]), || Some(first_person.clone()), now, &none),
            &[a]
        );
        assert_eq!(
            &*cache.devices_at(Some([0x44; 32]), || unreachable!("fresh check"), now, &none),
            &[a]
        );
        assert_eq!(
            &*cache.devices_at(
                Some([0x44; 32]),
                || Some(second_person.clone()),
                now + RECHECK,
                &none
            ),
            &[b],
            "a moved file is followed at the next recheck"
        );
    }

    #[test]
    fn recipients_follow_the_file_the_local_device_and_invalidation() {
        let dir = tempfile::tempdir().expect("temp");
        let person = PersonPeers::open_or_create_own(dir.path(), &[0x44; 32]).unwrap();
        let path = person.direct_peers_path();
        let none = RevocationList::new();
        let (a, b) = (endpoint(), endpoint());
        write(&path, &[a, b]);
        let id = |key: &[u8; 32]| DeviceId::new(hex::encode(key)).unwrap();
        let local = DeviceId::new("local").unwrap();
        let mut builds = 0;
        let mut cache = PersonalDevicePeers::default();
        let start = Instant::now();
        let mut query = |cache: &mut PersonalDevicePeers, now, local: Option<&DeviceId>| {
            cache.recipients_at(
                Some([0x44; 32]),
                || Some(person.clone()),
                now,
                &none,
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

    #[test]
    fn a_fresh_cache_cannot_cross_persons_or_trust_an_invalid_directory() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let alice = PersonPeers::open_or_create_own(dir.path(), &[0x44; 32]).unwrap();
        let bob = PersonPeers::open_or_create_own(dir.path(), &[0x55; 32]).unwrap();
        let (a, b) = (endpoint(), endpoint());
        write(&alice.direct_peers_path(), &[a]);
        bob.remember_direct_peers(&[tilo_relay_protocol::personal_devices::DirectPeer {
            identity: *bob.identity(),
            device: b,
        }])
        .unwrap();
        let mut cache = PersonalDevicePeers::default();
        let now = Instant::now();
        let none = RevocationList::new();
        let id = |key: &[u8; 32]| DeviceId::new(hex::encode(key)).unwrap();
        let recipients = |devices: &[[u8; 32]]| devices.iter().map(id).collect();
        let alice_set = cache.recipients_at(
            Some(*alice.identity()),
            || Some(alice.clone()),
            now,
            &none,
            None,
            recipients,
        );
        assert!(alice_set.contains(&id(&a)) && !alice_set.contains(&id(&b)));
        assert_eq!(
            &*cache.devices_at(Some(*alice.identity()), || Some(alice.clone()), now, &none),
            &[a]
        );
        assert_eq!(
            &*cache.devices_at(Some(*bob.identity()), || Some(bob.clone()), now, &none),
            &[b]
        );
        let bob_set = cache.recipients_at(
            Some(*bob.identity()),
            || unreachable!("fresh enrollment"),
            now,
            &none,
            None,
            recipients,
        );
        assert!(bob_set.contains(&id(&b)) && !bob_set.contains(&id(&a)));
        assert!(cache.devices_at(None, || None, now, &none).is_empty());
        // The resolver cannot substitute another person's valid directory.
        assert!(
            cache
                .devices_at(Some(*alice.identity()), || Some(bob.clone()), now, &none)
                .is_empty()
        );
        cache.expire();
        assert_eq!(
            &*cache.devices_at(Some(*alice.identity()), || Some(alice.clone()), now, &none),
            &[a]
        );
        std::fs::set_permissions(alice.dir(), std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(
            cache
                .devices_at(
                    Some(*alice.identity()),
                    || unreachable!("fresh enrollment"),
                    now,
                    &none
                )
                .is_empty(),
            "directory trust is checked even while enrollment is cached"
        );
        assert!(
            cache
                .recipients_at(
                    Some(*alice.identity()),
                    || unreachable!("fresh enrollment"),
                    now,
                    &none,
                    None,
                    recipients,
                )
                .is_empty()
        );
        std::fs::set_permissions(alice.dir(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}
