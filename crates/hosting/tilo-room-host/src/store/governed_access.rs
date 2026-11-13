//! A workspace's access as the host keeps it on disk: its member devices, the
//! person roots its handshake admits, and whether governance owns admission,
//! in one file. It is the host's only answer to who is a member.
//!
//! The file was written whole with every change, so each person a public
//! workspace let in cost a write of everyone let in before (about 7 ms at
//! three hundred members, most of it the rename's directory sync). It is now
//! a log of changes: a header (`MAGIC`, then the format's version, four
//! bytes little-endian), then frames, each its length (four bytes,
//! little-endian), its postcard [`Change`] and the first eight bytes of the
//! change's BLAKE3 digest. A write appends the change from what the file
//! holds and syncs its data; a change that would leave the log more than
//! twice the size of the access it holds writes the file anew, as one whole
//! change. Both sets are in every frame, so a crash still never pairs new
//! roots with old devices: a frame whose length or digest does not hold was
//! cut short by one, and it and anything after it are dropped as the file
//! is read.
//!
//! Version 1 was a JSON object written whole (`governed-access-v1.json`);
//! opening a workspace that holds one brings it up through [`FORMAT`] into
//! [`GOVERNED_ACCESS_FILE`], keeping the old file beside it as the migration's
//! snapshot. Version 2 knew no admission flag: every record it held was the
//! governor's, so version 3 reads all of them as governed.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Write as _};

use serde::{Deserialize, Serialize};
use tilo_files::native::migration::Format;
use tilo_lock::MutexRecover as _;

use super::{GovernedAccessRecord, StoreError, WorkspaceStore, atomic_write};

/// The access log of a governed workspace.
pub const GOVERNED_ACCESS_FILE: &str = "governed-access.log";
/// The version 1 file, read only to bring it up.
pub(crate) const V1_FILE: &str = "governed-access-v1.json";

/// What every access log starts with, before its version.
const MAGIC: &[u8; 8] = b"tiloacc\0";
const HEADER: usize = MAGIC.len() + 4;
/// The bytes of a frame beside its change: its length and its digest.
const FRAMING: usize = 4 + 8;

/// Version 1 (whole JSON), 2 (this log) and 3 (the log, with the admission
/// flag).
pub(crate) const FORMAT: Format = Format {
    name: "governed workspace access",
    version: 3,
    oldest: 1,
    steps: &[v1_to_v2, v2_to_v3],
};

/// One write: the whole access when `whole`, else what it added and took away.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Change {
    whole: bool,
    added_devices: Vec<[u8; 32]>,
    removed_devices: Vec<[u8; 32]>,
    added_roots: Vec<[u8; 32]>,
    removed_roots: Vec<[u8; 32]>,
    /// Whether governance owns the workspace's admission, when this write
    /// changes it.
    governed: Option<bool>,
}

/// What a store knows its access log holds, so a write appends only the
/// change and never reads the file back.
#[derive(Clone)]
pub(super) struct Held {
    access: GovernedAccessRecord,
    /// The log's length.
    len: u64,
    /// The digest of the whole log so far, which the membership watch
    /// compares a file it is told of against.
    digest: blake3::Hasher,
}

fn header_by(version: u32) -> Vec<u8> {
    let mut bytes = MAGIC.to_vec();
    bytes.extend_from_slice(&version.to_le_bytes());
    bytes
}

fn header() -> Vec<u8> {
    header_by(FORMAT.version)
}

fn frame(change: &Change) -> Result<Vec<u8>, StoreError> {
    let body = postcard::to_stdvec(change).map_err(|error| StoreError::Json(error.to_string()))?;
    let len = u32::try_from(body.len()).map_err(|error| StoreError::Json(error.to_string()))?;
    let mut bytes = Vec::with_capacity(body.len().saturating_add(FRAMING));
    bytes.extend_from_slice(&len.to_le_bytes());
    bytes.extend_from_slice(&body);
    bytes.extend_from_slice(&blake3::hash(&body).as_bytes()[..8]);
    Ok(bytes)
}

fn whole(access: &GovernedAccessRecord) -> Change {
    Change {
        whole: true,
        added_devices: access.member_devices.clone(),
        added_roots: access.member_user_roots.clone(),
        governed: Some(access.governed),
        ..Change::default()
    }
}

/// `held` with `change` applied, in place: removals first, then additions
/// not held yet, each set keeping its order.
fn apply(held: &mut GovernedAccessRecord, change: Change) {
    fn fold(list: &mut Vec<[u8; 32]>, whole: bool, added: Vec<[u8; 32]>, removed: &[[u8; 32]]) {
        if whole {
            list.clear();
        }
        let removed: HashSet<_> = removed.iter().collect();
        list.retain(|key| !removed.contains(key));
        let mut present: HashSet<[u8; 32]> = list.iter().copied().collect();
        list.extend(added.into_iter().filter(|key| present.insert(*key)));
    }
    fold(
        &mut held.member_devices,
        change.whole,
        change.added_devices,
        &change.removed_devices,
    );
    fold(
        &mut held.member_user_roots,
        change.whole,
        change.added_roots,
        &change.removed_roots,
    );
    if let Some(governed) = change.governed {
        held.governed = governed;
    }
}

/// The access a log's `bytes` hold and the length of their frames that
/// hold together: a frame a crash cut short ends the log.
fn read(bytes: &[u8]) -> Result<(GovernedAccessRecord, usize), StoreError> {
    let refused = |reason: &str| StoreError::Json(format!("governed access log: {reason}"));
    let (magic, rest) = bytes
        .split_first_chunk::<8>()
        .ok_or_else(|| refused("no header"))?;
    let (version, mut rest) = rest
        .split_first_chunk::<4>()
        .ok_or_else(|| refused("no version"))?;
    if magic != MAGIC {
        return Err(refused("not an access log"));
    }
    let version = u32::from_le_bytes(*version);
    if version != FORMAT.version {
        return Err(refused(&format!(
            "version {version}, not {}",
            FORMAT.version
        )));
    }
    let mut access = GovernedAccessRecord::default();
    let mut valid = HEADER;
    while let Some((len, tail)) = rest.split_first_chunk::<4>() {
        let Some(len) = usize::try_from(u32::from_le_bytes(*len)).ok() else {
            break;
        };
        let Some((body, tail)) = tail.split_at_checked(len) else {
            break;
        };
        let Some((digest, tail)) = tail.split_first_chunk::<8>() else {
            break;
        };
        if digest != &blake3::hash(body).as_bytes()[..8] {
            break;
        }
        let Ok(change) = postcard::from_bytes::<Change>(body) else {
            break;
        };
        apply(&mut access, change);
        valid = valid.saturating_add(len).saturating_add(FRAMING);
        rest = tail;
    }
    Ok((access, valid))
}

/// Version 1 to 2: the JSON object's devices and roots as a log of one whole
/// change.
#[expect(
    clippy::needless_pass_by_value,
    reason = "a migration step takes the bytes it brings up"
)]
fn v1_to_v2(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct V1 {
        version: u8,
        member_devices: Vec<String>,
        member_user_roots: Vec<String>,
    }
    let v1: V1 = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if v1.version != 1 {
        return Err(format!("version {} in a version 1 file", v1.version));
    }
    let keys = |values: Vec<String>| {
        values
            .into_iter()
            .map(|value| {
                hex::decode(&value)
                    .map_err(|error| format!("{value:?}: {error}"))?
                    .try_into()
                    .map_err(|_| format!("{value:?} is not 32 bytes"))
            })
            .collect::<Result<Vec<[u8; 32]>, String>>()
    };
    let access = GovernedAccessRecord {
        member_devices: keys(v1.member_devices)?,
        member_user_roots: keys(v1.member_user_roots)?,
        governed: true,
    };
    let mut log = header_by(2);
    log.extend(frame(&whole(&access)).map_err(|error| error.to_string())?);
    Ok(log)
}

/// Version 2 to 3: every record version 2 wrote was the governor's, so each of
/// its changes is one governance still owns, and the flag is written with it.
#[expect(
    clippy::needless_pass_by_value,
    reason = "a migration step takes the bytes it brings up"
)]
fn v2_to_v3(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    /// The change as version 2 framed it.
    #[derive(Deserialize)]
    struct ChangeV2 {
        whole: bool,
        added_devices: Vec<[u8; 32]>,
        removed_devices: Vec<[u8; 32]>,
        added_roots: Vec<[u8; 32]>,
        removed_roots: Vec<[u8; 32]>,
    }
    let refused = |reason: &str| format!("governed access log: {reason}");
    let (magic, rest) = bytes
        .split_first_chunk::<8>()
        .ok_or_else(|| refused("no header"))?;
    let (version, mut rest) = rest
        .split_first_chunk::<4>()
        .ok_or_else(|| refused("no version"))?;
    if magic != MAGIC {
        return Err(refused("not an access log"));
    }
    if u32::from_le_bytes(*version) != 2 {
        return Err(refused("not a version 2 log"));
    }
    let mut log = header_by(3);
    while let Some((len, tail)) = rest.split_first_chunk::<4>() {
        let Some(len) = usize::try_from(u32::from_le_bytes(*len)).ok() else {
            break;
        };
        let Some((body, tail)) = tail.split_at_checked(len) else {
            break;
        };
        let Some((digest, tail)) = tail.split_first_chunk::<8>() else {
            break;
        };
        if digest != &blake3::hash(body).as_bytes()[..8] {
            break;
        }
        let Ok(change) = postcard::from_bytes::<ChangeV2>(body) else {
            break;
        };
        let change = Change {
            whole: change.whole,
            added_devices: change.added_devices,
            removed_devices: change.removed_devices,
            added_roots: change.added_roots,
            removed_roots: change.removed_roots,
            governed: Some(true),
        };
        log.extend(frame(&change).map_err(|error| error.to_string())?);
        rest = tail;
    }
    Ok(log)
}

impl WorkspaceStore {
    /// Bring a version 1 access file up to the log, keeping it beside the log
    /// as the migration's snapshot. Nothing to do when there is none.
    fn bring_up_governed_access(&self) -> Result<(), StoreError> {
        let old = self.root.join(V1_FILE);
        let bytes = match fs::read(&old) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let log = FORMAT
            .migrate(bytes, 1)
            .map_err(|error| StoreError::Json(error.to_string()))?;
        let path = self.root.join(GOVERNED_ACCESS_FILE);
        atomic_write(&path, &log)?;
        fs::rename(&old, Format::snapshot_path(&path, 1))?;
        Ok(())
    }

    /// Load the governed access this workspace's log holds; `None` when the
    /// workspace has none. A frame a crash cut short is cut off the file.
    ///
    /// # Errors
    ///
    /// The log cannot be read, or is not an access log of this version.
    pub(crate) fn load_governed_access(&self) -> Result<Option<GovernedAccessRecord>, StoreError> {
        let path = self.root.join(GOVERNED_ACCESS_FILE);
        let mut bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.bring_up_governed_access()?;
                match fs::read(&path) {
                    Ok(bytes) => bytes,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        *self.governed_access.lock_recover() = None;
                        return Ok(None);
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            Err(error) => return Err(error.into()),
        };
        // A log an older build wrote is brought up on disk, its old bytes kept
        // beside it, before it is read: a host keeps the workspaces it governs
        // across every release.
        if let Some(stored) = bytes
            .strip_prefix(MAGIC.as_slice())
            .and_then(|rest| rest.first_chunk::<4>())
            .map(|version| u32::from_le_bytes(*version))
            .filter(|stored| *stored < FORMAT.version)
        {
            bytes = FORMAT
                .bring_up(&path, bytes, stored)
                .map_err(|error| StoreError::Json(error.to_string()))?;
        }
        let (access, valid) = read(&bytes)?;
        if valid < bytes.len() {
            let kept = u64::try_from(valid).unwrap_or(u64::MAX);
            fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .and_then(|file| {
                    file.set_len(kept)?;
                    file.sync_data()
                })?;
            bytes.truncate(valid);
        }
        let mut digest = blake3::Hasher::new();
        digest.update(&bytes);
        *self.governed_access.lock_recover() = Some(Held {
            access: access.clone(),
            len: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            digest,
        });
        Ok(Some(access))
    }

    /// Make the workspace's log hold `access`: the change from what it holds
    /// is appended and synced, or, when the log has grown to more than twice
    /// the access it holds, the log is written anew. Answers the digest of
    /// the whole log, for the membership watch
    /// ([`crate::membership_watch::wrote_own`]), and how many bytes it wrote.
    ///
    /// # Errors
    ///
    /// The log cannot be read or written.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "what the store knows the log holds stays locked until the log holds it"
    )]
    pub(crate) fn save_governed_access(
        &self,
        access: &GovernedAccessRecord,
    ) -> Result<([u8; 32], u64), StoreError> {
        let path = self.root.join(GOVERNED_ACCESS_FILE);
        if self.governed_access.lock_recover().is_none() {
            self.load_governed_access()?;
        }
        let mut held = self.governed_access.lock_recover();
        let change = match held.as_ref() {
            Some(held) => {
                let missing = |from: &[[u8; 32]], within: &[[u8; 32]]| {
                    let within: HashSet<_> = within.iter().collect();
                    from.iter()
                        .filter(|key| !within.contains(key))
                        .copied()
                        .collect::<Vec<_>>()
                };
                let change = Change {
                    whole: false,
                    added_devices: missing(&access.member_devices, &held.access.member_devices),
                    removed_devices: missing(&held.access.member_devices, &access.member_devices),
                    added_roots: missing(&access.member_user_roots, &held.access.member_user_roots),
                    removed_roots: missing(
                        &held.access.member_user_roots,
                        &access.member_user_roots,
                    ),
                    governed: (held.access.governed != access.governed).then_some(access.governed),
                };
                if change == Change::default() {
                    return Ok((*held.digest.finalize().as_bytes(), 0));
                }
                Some(change)
            }
            None => None,
        };
        let anew = whole(access);
        let whole_len =
            u64::try_from(frame(&anew)?.len().saturating_add(HEADER)).unwrap_or(u64::MAX);
        let appended = match (change, held.as_mut()) {
            (Some(change), Some(current)) => {
                let bytes = frame(&change)?;
                let grown = current
                    .len
                    .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
                if grown > whole_len.saturating_mul(2) {
                    None
                } else {
                    let mut file = fs::OpenOptions::new().append(true).open(&path)?;
                    file.write_all(&bytes)?;
                    file.sync_data()?;
                    super::credit_fsyncs(1);
                    let mut next = current.access.clone();
                    apply(&mut next, change);
                    current.access = next;
                    current.len = grown;
                    current.digest.update(&bytes);
                    Some((
                        *current.digest.finalize().as_bytes(),
                        u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                    ))
                }
            }
            _ => None,
        };
        if let Some(digest) = appended {
            return Ok(digest);
        }
        let mut log = header();
        log.extend(frame(&anew)?);
        atomic_write(&path, &log)?;
        let mut digest = blake3::Hasher::new();
        digest.update(&log);
        let answer = *digest.finalize().as_bytes();
        let len = u64::try_from(log.len()).unwrap_or(u64::MAX);
        *held = Some(Held {
            access: access.clone(),
            len,
            digest,
        });
        Ok((answer, len))
    }

    /// The length of the workspace's access log, for tests of what a write
    /// costs.
    #[cfg(test)]
    pub(crate) fn governed_access_len(&self) -> u64 {
        fs::metadata(self.root.join(GOVERNED_ACCESS_FILE)).map_or(0, |file| file.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> [u8; 32] {
        [n; 32]
    }

    fn access(devices: &[u8], roots: &[u8]) -> GovernedAccessRecord {
        GovernedAccessRecord {
            member_devices: devices.iter().copied().map(key).collect(),
            member_user_roots: roots.iter().copied().map(key).collect(),
            governed: true,
        }
    }

    /// One more member costs the log one small frame, however many it holds;
    /// reopening reads what was written, and a log grown past twice what it
    /// holds is written anew.
    #[test]
    fn an_admission_appends_its_change_and_the_log_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(dir.path(), 7, false).unwrap();
        let mut devices = Vec::new();
        let mut appended = Vec::new();
        for n in 0..60_u8 {
            devices.push(n);
            let before = store.governed_access_len();
            store
                .save_governed_access(&access(&devices, &[200]))
                .unwrap();
            appended.push(store.governed_access_len().saturating_sub(before));
        }
        let small = appended[10];
        assert!(
            appended[50] <= small,
            "the 51st member's write ({} B) is no larger than the 11th's ({small} B)",
            appended[50]
        );
        let reopened = WorkspaceStore::open(dir.path(), 7, false).unwrap();
        assert_eq!(
            reopened.load_governed_access().unwrap(),
            Some(access(&devices, &[200]))
        );
        devices.retain(|n| n % 2 == 0);
        reopened
            .save_governed_access(&access(&devices, &[201]))
            .unwrap();
        assert_eq!(
            WorkspaceStore::open(dir.path(), 7, false)
                .unwrap()
                .load_governed_access()
                .unwrap(),
            Some(access(&devices, &[201]))
        );
        let whole = u64::try_from(HEADER + frame(&whole(&access(&devices, &[201]))).unwrap().len())
            .unwrap();
        assert!(reopened.governed_access_len() <= whole * 2);
    }

    /// A change a crash cut short is no change: the log reads as it was
    /// before it, both sets together, and the next write appends after what
    /// holds.
    #[test]
    fn a_change_cut_short_is_dropped_whole() {
        let dir = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(dir.path(), 7, false).unwrap();
        store.save_governed_access(&access(&[1, 2], &[9])).unwrap();
        let path = dir.path().join("workspaces/7").join(GOVERNED_ACCESS_FILE);
        let held = fs::read(&path).unwrap();
        let mut torn = held.clone();
        torn.extend_from_slice(
            &frame(&Change {
                added_devices: vec![key(3)],
                added_roots: vec![key(8)],
                ..Change::default()
            })
            .unwrap()[..20],
        );
        fs::write(&path, &torn).unwrap();
        let reopened = WorkspaceStore::open(dir.path(), 7, false).unwrap();
        assert_eq!(
            reopened.load_governed_access().unwrap(),
            Some(access(&[1, 2], &[9]))
        );
        assert_eq!(fs::read(&path).unwrap(), held, "the torn frame is cut off");
        reopened
            .save_governed_access(&access(&[1, 2, 4], &[9]))
            .unwrap();
        assert_eq!(
            WorkspaceStore::open(dir.path(), 7, false)
                .unwrap()
                .load_governed_access()
                .unwrap(),
            Some(access(&[1, 2, 4], &[9]))
        );
    }

    /// A version 1 file, as a build before the log wrote it, opens as the
    /// same access, and is kept beside the log.
    #[test]
    fn a_version_1_file_is_brought_up() {
        let v1 = br#"{
  "version": 1,
  "member_devices": [
    "0101010101010101010101010101010101010101010101010101010101010101"
  ],
  "member_user_roots": [
    "0202020202020202020202020202020202020202020202020202020202020202"
  ]
}"#;
        let brought_up = FORMAT.migrate(v1.to_vec(), 1).unwrap();
        assert_eq!(read(&brought_up).unwrap().0, access(&[1], &[2]));
        let dir = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(dir.path(), 7, false).unwrap();
        let root = dir.path().join("workspaces/7");
        fs::write(root.join(V1_FILE), v1).unwrap();
        assert_eq!(
            store.load_governed_access().unwrap(),
            Some(access(&[1], &[2]))
        );
        assert!(!root.join(V1_FILE).exists());
        assert_eq!(
            fs::read(Format::snapshot_path(&root.join(GOVERNED_ACCESS_FILE), 1)).unwrap(),
            v1
        );
    }

    /// A version 2 log, as a build before the admission flag wrote it, opens
    /// as the same access, governed by the workspace's own policy.
    #[test]
    fn a_version_2_log_is_brought_up_as_governed() {
        /// The change as version 2 framed it.
        #[derive(Serialize)]
        struct ChangeV2 {
            whole: bool,
            added_devices: Vec<[u8; 32]>,
            removed_devices: Vec<[u8; 32]>,
            added_roots: Vec<[u8; 32]>,
            removed_roots: Vec<[u8; 32]>,
        }
        let body = postcard::to_stdvec(&ChangeV2 {
            whole: true,
            added_devices: vec![key(1)],
            removed_devices: Vec::new(),
            added_roots: vec![key(2)],
            removed_roots: Vec::new(),
        })
        .unwrap();
        let mut v2 = header_by(2);
        v2.extend_from_slice(&u32::try_from(body.len()).unwrap().to_le_bytes());
        v2.extend_from_slice(&body);
        v2.extend_from_slice(&blake3::hash(&body).as_bytes()[..8]);
        let brought_up = FORMAT.migrate(v2.clone(), 2).unwrap();
        assert_eq!(read(&brought_up).unwrap().0, access(&[1], &[2]));

        // A workspace whose log a version 2 build wrote opens, and keeps the
        // log as it was beside it.
        let dir = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(dir.path(), 7, false).unwrap();
        let path = dir.path().join("workspaces/7").join(GOVERNED_ACCESS_FILE);
        fs::write(&path, &v2).unwrap();
        assert_eq!(
            store.load_governed_access().unwrap(),
            Some(access(&[1], &[2]))
        );
        assert_eq!(fs::read(Format::snapshot_path(&path, 2)).unwrap(), v2);
        assert_eq!(fs::read(&path).unwrap(), brought_up);
    }

    /// The bytes this build writes for a workspace's first access.
    #[test]
    fn the_log_this_build_writes() {
        const KEPT: &str = "74696c6f61636300030000004700000001010101010101010101010101010101010101010101010101010101010101010101000102020202020202020202020202020202020202020202020202020202020202020001017059f4fed357fdc7";
        tilo_files::native::migration::assert_golden_bytes(
            "governed workspace access",
            &hex::decode(KEPT).unwrap(),
            |bytes| assert_eq!(read(bytes).unwrap().0, access(&[1], &[2])),
            || {
                let mut log = header();
                log.extend(frame(&whole(&access(&[1], &[2]))).unwrap());
                log
            },
        );
    }
}
