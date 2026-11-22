//! A workspace's access as the host keeps it on disk: its member devices, the
//! person roots its handshake admits, and whether governance owns admission,
//! in one file. It is the host's only answer to who is a member.
//!
//! The file is a log of changes: a header (`MAGIC`, then the format's
//! version, four bytes little-endian), then frames, each its length (four
//! bytes, little-endian), its postcard [`Change`] and the first eight bytes of
//! the change's BLAKE3 digest. A write appends the change from what the file
//! holds and syncs its data; a change that would leave the log more than twice
//! the size of the access it holds writes the file anew, as one whole change.
//! Both sets are in every frame, so a crash never pairs new roots with old
//! devices: a frame whose length or digest does not hold was cut short by one,
//! and it and anything after it are dropped as the file is read. A log of
//! another version is refused, naming the version it holds.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Write as _};

use serde::{Deserialize, Serialize};
use tilo_files::native::migration::Format;
use tilo_lock::MutexRecover as _;

use super::{FilesError, GovernedAccessRecord, WorkspaceFiles, atomic_write};

/// The access log of a governed workspace.
pub const GOVERNED_ACCESS_FILE: &str = "governed-access.log";
/// What every access log starts with, before its version.
const MAGIC: &[u8; 8] = b"tiloacc\0";
const HEADER: usize = MAGIC.len() + 4;
/// The bytes of a frame beside its change: its length and its digest.
const FRAMING: usize = 4 + 8;

/// The log this build writes and reads.
pub(crate) const FORMAT: Format = Format {
    name: "governed workspace access",
    version: 3,
    oldest: 3,
    steps: &[],
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

fn header() -> Vec<u8> {
    let mut bytes = MAGIC.to_vec();
    bytes.extend_from_slice(&FORMAT.version.to_le_bytes());
    bytes
}

fn frame(change: &Change) -> Result<Vec<u8>, FilesError> {
    let body = postcard::to_stdvec(change).map_err(|error| FilesError::Json(error.to_string()))?;
    let len = u32::try_from(body.len()).map_err(|error| FilesError::Json(error.to_string()))?;
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

/// The version a log's header names.
fn stored_version(bytes: &[u8]) -> Result<u32, FilesError> {
    let refused = |reason: &str| FilesError::Json(format!("governed access log: {reason}"));
    let (magic, rest) = bytes
        .split_first_chunk::<8>()
        .ok_or_else(|| refused("no header"))?;
    let (version, _) = rest
        .split_first_chunk::<4>()
        .ok_or_else(|| refused("no version"))?;
    if magic != MAGIC {
        return Err(refused("not an access log"));
    }
    Ok(u32::from_le_bytes(*version))
}

/// The access a log's `bytes` hold and the length of their frames that
/// hold together: a frame a crash cut short ends the log.
fn read(bytes: &[u8]) -> Result<(GovernedAccessRecord, usize), FilesError> {
    let version = stored_version(bytes)?;
    if version != FORMAT.version {
        return Err(FilesError::Json(format!(
            "governed access log: version {version}, not {}",
            FORMAT.version
        )));
    }
    let mut rest = bytes.get(HEADER..).unwrap_or_default();
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

impl WorkspaceFiles {
    /// Load the governed access this workspace's log holds; `None` when the
    /// workspace has none. A frame a crash cut short is cut off the file.
    ///
    /// # Errors
    ///
    /// The log cannot be read, or is not an access log of this version.
    pub(crate) fn load_governed_access(&self) -> Result<Option<GovernedAccessRecord>, FilesError> {
        let path = self.root.join(GOVERNED_ACCESS_FILE);
        let mut bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                *self.governed_access.lock_recover() = None;
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        let stored = stored_version(&bytes)?;
        bytes = FORMAT
            .bring_up(&path, bytes, stored)
            .map_err(|error| FilesError::Json(error.to_string()))?;
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
    ) -> Result<([u8; 32], u64), FilesError> {
        #[cfg(test)]
        super::test_barrier::pause(&self.root, "access-write");
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
    use tilo_server_protocol::WorkspaceId;

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
        let store = WorkspaceFiles::open(dir.path(), WorkspaceId::from_u128(7)).unwrap();
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
        let reopened = WorkspaceFiles::open(dir.path(), WorkspaceId::from_u128(7)).unwrap();
        assert_eq!(
            reopened.load_governed_access().unwrap(),
            Some(access(&devices, &[200]))
        );
        devices.retain(|n| n % 2 == 0);
        reopened
            .save_governed_access(&access(&devices, &[201]))
            .unwrap();
        assert_eq!(
            WorkspaceFiles::open(dir.path(), WorkspaceId::from_u128(7))
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
        let store = WorkspaceFiles::open(dir.path(), WorkspaceId::from_u128(7)).unwrap();
        store.save_governed_access(&access(&[1, 2], &[9])).unwrap();
        let path = dir
            .path()
            .join(format!("workspaces/{}", WorkspaceId::from_u128(7)))
            .join(GOVERNED_ACCESS_FILE);
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
        let reopened = WorkspaceFiles::open(dir.path(), WorkspaceId::from_u128(7)).unwrap();
        assert_eq!(
            reopened.load_governed_access().unwrap(),
            Some(access(&[1, 2], &[9]))
        );
        assert_eq!(fs::read(&path).unwrap(), held, "the torn frame is cut off");
        reopened
            .save_governed_access(&access(&[1, 2, 4], &[9]))
            .unwrap();
        assert_eq!(
            WorkspaceFiles::open(dir.path(), WorkspaceId::from_u128(7))
                .unwrap()
                .load_governed_access()
                .unwrap(),
            Some(access(&[1, 2, 4], &[9]))
        );
    }

    /// The bytes this build writes for a workspace's first access.
    const GOLDEN: &str = include_str!("../../golden/governed-workspace-access/v3.hex");

    fn golden() -> Vec<u8> {
        hex::decode(GOLDEN.split_whitespace().collect::<String>()).unwrap()
    }

    #[test]
    fn the_log_this_build_writes() {
        tilo_files::native::migration::assert_golden_bytes(
            FORMAT.name,
            &golden(),
            |bytes| assert_eq!(read(bytes).unwrap().0, access(&[1], &[2])),
            || {
                let mut log = header();
                log.extend(frame(&whole(&access(&[1], &[2]))).unwrap());
                log
            },
        );
    }
}
