//! This person's enrolled devices, as recorded in `personal-device-peers.json`.
//!
//! The identity host writes that file from another process, so the shell
//! cannot be told when it changes. Every outgoing personal-fabric message —
//! each 20 ms remote-audio frame, each forwarded input event — and the
//! per-frame owner-view pump consult it, so reading and parsing it per call
//! put disk I/O on the compositor thread. Instead it is re-validated at most
//! once per [`RECHECK`]: a `stat`, and a re-read only when the file's identity
//! (mtime, length, inode) changed. Removing a device therefore stops sends to
//! it within [`RECHECK`]; route revocation stays independent and immediate.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

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
    path: Option<PathBuf>,
    stamp: Option<FileStamp>,
    checked_at: Option<Instant>,
    /// Every valid endpoint key in the file, this device included; callers
    /// filter their own key, since they name it differently.
    devices: Arc<[[u8; 32]]>,
    recipients: Option<Recipients>,
    /// How many times the file was read. Observed by tests only.
    reads: u64,
}

impl PersonalDevicePeers {
    /// The enrolled device keys, re-validated against the file at `path` if
    /// the last check is older than [`RECHECK`]. `path` is resolved only then.
    pub(super) fn devices_at(
        &mut self,
        path: impl FnOnce() -> PathBuf,
        now: Instant,
    ) -> Arc<[[u8; 32]]> {
        self.refresh(path, now);
        Arc::clone(&self.devices)
    }

    /// The certified recipient set for `local`, rebuilt by `build` from the
    /// enrolled keys only when the file, the local device or the registered
    /// home-device ids (see [`Self::invalidate_recipients`]) changed.
    pub(super) fn recipients_at(
        &mut self,
        path: impl FnOnce() -> PathBuf,
        now: Instant,
        local: Option<&DeviceId>,
        build: impl FnOnce(&[[u8; 32]]) -> BTreeSet<DeviceId>,
    ) -> Arc<BTreeSet<DeviceId>> {
        self.refresh(path, now);
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

    fn refresh(&mut self, path: impl FnOnce() -> PathBuf, now: Instant) {
        if self
            .checked_at
            .is_some_and(|checked| now.saturating_duration_since(checked) < RECHECK)
        {
            return;
        }
        self.checked_at = Some(now);
        let path = path();
        let stamp = FileStamp::of(&path);
        if self.path.as_ref() == Some(&path) && stamp == self.stamp {
            return;
        }
        self.devices = if stamp.is_some() {
            self.reads = self.reads.saturating_add(1);
            super::read_personal_device_peers(&path).into()
        } else {
            Arc::from([])
        };
        self.path = Some(path);
        self.stamp = stamp;
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
            assert_eq!(&*cache.devices_at(|| path.clone(), start), &[a, b]);
        }
        assert_eq!(cache.reads(), 1);
        // A due recheck of an unchanged file stats it but does not read it.
        assert_eq!(
            &*cache.devices_at(|| path.clone(), start + RECHECK),
            &[a, b]
        );
        assert_eq!(cache.reads(), 1);

        write(&path, &[a]);
        assert_eq!(
            &*cache.devices_at(|| path.clone(), start + RECHECK),
            &[a, b],
            "within the recheck window the last check stands"
        );
        assert_eq!(
            &*cache.devices_at(|| path.clone(), start + RECHECK * 2),
            &[a]
        );
        assert_eq!(cache.reads(), 2);

        std::fs::remove_file(&path).unwrap();
        assert!(
            cache
                .devices_at(|| path.clone(), start + RECHECK * 3)
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
        assert_eq!(&*cache.devices_at(|| first.clone(), now), &[a]);
        assert_eq!(
            &*cache.devices_at(|| unreachable!("fresh check"), now),
            &[a]
        );
        assert_eq!(
            &*cache.devices_at(|| second.clone(), now + RECHECK),
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
                || path.clone(),
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
