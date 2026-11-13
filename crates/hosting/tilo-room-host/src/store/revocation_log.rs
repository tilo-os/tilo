//! The host's revocation log: every root-signed revocation it is bound to
//! refuse, kept across restarts.
//!
//! The log was one JSON array the host read and wrote whole for each entry it
//! took in, so each revocation cost a write of every one before it, and the
//! host capped how many a root could keep; a revocation past the cap was
//! refused only until the next restart. It is now appended to: a header
//! (`MAGIC`, then the format's version, four bytes little-endian), then
//! frames, each its length (four bytes, little-endian), the postcard
//! [`RevocationEntry`] and the first eight bytes of the entry's BLAKE3 digest.
//! A write appends the entries the log does not hold as early and syncs its
//! data, so the log grows with the revocations themselves and nothing else.
//! What a root may put here is bounded where entries are taken in: only a
//! root the host serves has its entries kept.
//!
//! A frame whose length or digest does not hold was cut short by a crash: a
//! read stops before it, and the next write cuts it off before appending. A
//! log holding more than twice the frames it has distinct entries (a root
//! that revoked a key again, earlier) is written anew, whole.
//!
//! Version 1 was the JSON array (`revocations.json`). Reading or writing a
//! state root that holds one brings it up through [`FORMAT`] into
//! [`REVOCATIONS_FILE`], keeping the old file beside it as the migration's
//! snapshot.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write as _};
use std::path::Path;

use tilo_files::native::migration::Format;
use tilo_lock::MutexRecover as _;
use tilo_user_identity::RevocationEntry;

use super::{StoreError, atomic_write};

/// Root-signed device revocations enforced during relay authentication.
pub const REVOCATIONS_FILE: &str = "revocations.log";
/// The version 1 file, read only to bring it up.
pub(crate) const V1_FILE: &str = "revocations.json";

/// What every revocation log starts with, before its version.
const MAGIC: &[u8; 8] = b"tilorev\0";
const HEADER: usize = MAGIC.len() + 4;
/// The bytes of a frame beside its entry: its length and its digest.
const FRAMING: usize = 4 + 8;

/// Version 1 (one JSON array) and 2 (this log).
pub(crate) const FORMAT: Format = Format {
    name: "relay revocation log",
    version: 2,
    oldest: 1,
    steps: &[v1_to_v2],
};

/// One writer at a time in this process: a write reads where the log's last
/// whole frame ends, and another write landing between that read and its own
/// would be cut off with a torn tail.
static WRITING: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn header() -> Vec<u8> {
    let mut bytes = MAGIC.to_vec();
    bytes.extend_from_slice(&FORMAT.version.to_le_bytes());
    bytes
}

fn frame(entry: &RevocationEntry) -> Result<Vec<u8>, StoreError> {
    let body = postcard::to_stdvec(entry).map_err(|error| StoreError::Json(error.to_string()))?;
    let len = u32::try_from(body.len()).map_err(|error| StoreError::Json(error.to_string()))?;
    let mut bytes = Vec::with_capacity(body.len().saturating_add(FRAMING));
    bytes.extend_from_slice(&len.to_le_bytes());
    bytes.extend_from_slice(&body);
    bytes.extend_from_slice(&blake3::hash(&body).as_bytes()[..8]);
    Ok(bytes)
}

fn whole(entries: &[RevocationEntry]) -> Result<Vec<u8>, StoreError> {
    let mut bytes = header();
    for entry in entries {
        bytes.extend(frame(entry)?);
    }
    Ok(bytes)
}

/// The entries a log holds, as each root revoked each of its keys earliest.
#[derive(Default)]
struct Held {
    entries: Vec<RevocationEntry>,
    /// Where in `entries` each root's entry for each key is.
    at: HashMap<([u8; 32], [u8; 32]), usize>,
    /// Frames read, distinct or not.
    frames: usize,
}

impl Held {
    /// Whether `entry` changes what is held: a key its root had not revoked,
    /// or revoked later than `entry` says.
    fn takes(&self, entry: &RevocationEntry) -> bool {
        self.at
            .get(&(entry.user_public_key, entry.revoked_device_public_key))
            .and_then(|at| self.entries.get(*at))
            .is_none_or(|known| entry.revoked_at_ns < known.revoked_at_ns)
    }

    fn add(&mut self, entry: RevocationEntry) {
        self.frames = self.frames.saturating_add(1);
        let key = (entry.user_public_key, entry.revoked_device_public_key);
        if let Some(at) = self.at.get(&key).copied() {
            if let Some(known) = self.entries.get_mut(at)
                && entry.revoked_at_ns < known.revoked_at_ns
            {
                *known = entry;
            }
        } else {
            self.at.insert(key, self.entries.len());
            self.entries.push(entry);
        }
    }
}

/// What a log's `bytes` hold, and the length of their frames that hold
/// together: a frame a crash cut short ends the log.
fn read(bytes: &[u8]) -> Result<(Held, usize), StoreError> {
    let refused = |reason: &str| StoreError::Json(format!("revocation log: {reason}"));
    let (magic, rest) = bytes
        .split_first_chunk::<8>()
        .ok_or_else(|| refused("no header"))?;
    let (version, mut rest) = rest
        .split_first_chunk::<4>()
        .ok_or_else(|| refused("no version"))?;
    if magic != MAGIC {
        return Err(refused("not a revocation log"));
    }
    let version = u32::from_le_bytes(*version);
    if version != FORMAT.version {
        return Err(refused(&format!(
            "version {version}, not {}",
            FORMAT.version
        )));
    }
    let mut held = Held::default();
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
        let Ok(entry) = postcard::from_bytes::<RevocationEntry>(body) else {
            break;
        };
        held.add(entry);
        valid = valid.saturating_add(len).saturating_add(FRAMING);
        rest = tail;
    }
    Ok((held, valid))
}

/// Version 1 to 2: the JSON array's entries as frames of the log.
#[expect(
    clippy::needless_pass_by_value,
    reason = "a migration step takes the bytes it brings up"
)]
fn v1_to_v2(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    let entries: Vec<RevocationEntry> =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    whole(&entries).map_err(|error| error.to_string())
}

/// Bring a version 1 file in `state_root` up to the log, keeping it beside
/// the log as the migration's snapshot. Nothing to do when there is none.
fn bring_up(state_root: &Path) -> Result<(), StoreError> {
    let old = state_root.join(V1_FILE);
    let bytes = match fs::read(&old) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let path = state_root.join(REVOCATIONS_FILE);
    let log = FORMAT
        .migrate(bytes, 1)
        .map_err(|error| StoreError::Json(error.to_string()))?;
    atomic_write(&path, &log)?;
    match fs::rename(&old, Format::snapshot_path(&path, 1)) {
        // Another reader brought it up first.
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result.map_err(Into::into),
    }
}

/// The log's bytes, `None` when the state root holds none.
fn log_bytes(state_root: &Path) -> Result<Option<Vec<u8>>, StoreError> {
    let path = state_root.join(REVOCATIONS_FILE);
    match fs::read(&path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            bring_up(state_root)?;
            match fs::read(&path) {
                Ok(bytes) => Ok(Some(bytes)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error.into()),
            }
        }
        Err(error) => Err(error.into()),
    }
}

/// Load the root-signed revocation entries the relay's log holds: for each
/// root and key it revoked, the earliest entry.
///
/// # Errors
///
/// Returns an error if the log exists but can't be read, or is not a
/// revocation log of this version.
pub fn load_revocations(state_root: &Path) -> Result<Vec<RevocationEntry>, StoreError> {
    let Some(bytes) = log_bytes(state_root)? else {
        return Ok(Vec::new());
    };
    Ok(read(&bytes)?.0.entries)
}

/// Atomically replace the relay's root-signed revocation entries.
///
/// # Errors
///
/// Returns an error if the state root can't be created, an entry can't be
/// encoded, or the atomic write fails.
pub fn save_revocations(state_root: &Path, entries: &[RevocationEntry]) -> Result<(), StoreError> {
    let _writing = WRITING.lock_recover();
    fs::create_dir_all(state_root)?;
    bring_up(state_root)?;
    atomic_write(&state_root.join(REVOCATIONS_FILE), &whole(entries)?)?;
    Ok(())
}

/// Add `entries` to the relay's revocation log, each unless the log already
/// holds its root's revocation of its device as early. How many it added.
/// Each root's entries are its own: another root's entry naming the same key
/// neither replaces nor is replaced by it.
///
/// # Errors
///
/// Returns an error if the log can't be read or written.
pub fn append_revocations(
    state_root: &Path,
    entries: &[RevocationEntry],
) -> Result<usize, StoreError> {
    let _writing = WRITING.lock_recover();
    fs::create_dir_all(state_root)?;
    let path = state_root.join(REVOCATIONS_FILE);
    let (mut held, valid, len) = match log_bytes(state_root)? {
        Some(bytes) => {
            let (held, valid) = read(&bytes)?;
            (held, valid, bytes.len())
        }
        None => (Held::default(), 0, 0),
    };
    let mut appended = Vec::new();
    let mut added = 0_usize;
    for entry in entries {
        if held.takes(entry) {
            appended.extend(frame(entry)?);
            held.add(entry.clone());
            added = added.saturating_add(1);
        }
    }
    if added == 0 {
        return Ok(0);
    }
    if len == 0 || held.frames > held.entries.len().saturating_mul(2) {
        atomic_write(&path, &whole(&held.entries)?)?;
        return Ok(added);
    }
    let mut file = fs::OpenOptions::new().append(true).open(&path)?;
    if valid < len {
        // A frame a crash cut short: what follows it would never be read.
        file.set_len(u64::try_from(valid).unwrap_or(u64::MAX))?;
    }
    file.write_all(&appended)?;
    file.sync_data()?;
    Ok(added)
}

/// Add `entry` to the relay's revocation log unless the log already holds
/// its root's revocation of its device as early. Whether the log changed.
///
/// # Errors
///
/// Returns an error if the log can't be read or written.
pub fn append_revocation(state_root: &Path, entry: &RevocationEntry) -> Result<bool, StoreError> {
    Ok(append_revocations(state_root, std::slice::from_ref(entry))? > 0)
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::SigningKey;
    use tilo_user_identity::{RevocationEntry, RevocationReason, sign_revocation_entry};

    use super::*;

    fn revocation(root: &SigningKey, device: u8, at: u64) -> RevocationEntry {
        let mut entry = RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: [device; 32],
            revoked_at_ns: at,
            reason: RevocationReason::Lost,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut entry, root).unwrap();
        entry
    }

    #[test]
    fn the_json_array_comes_up_as_the_log() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let entries = vec![revocation(&root, 2, 10), revocation(&root, 3, 20)];
        let v1 = serde_json::to_vec_pretty(&entries).unwrap();
        tilo_files::native::migration::assert_step_reads_previous(&FORMAT, &v1, |bytes| {
            assert_eq!(read(bytes).unwrap().0.entries, entries);
        });

        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(V1_FILE), &v1).unwrap();
        assert_eq!(load_revocations(dir.path()).unwrap(), entries);
        assert!(!dir.path().join(V1_FILE).exists());
        assert_eq!(
            fs::read(Format::snapshot_path(&dir.path().join(REVOCATIONS_FILE), 1)).unwrap(),
            v1,
            "the version 1 file is kept as the migration's snapshot"
        );
    }

    /// An entry appends only what the log does not hold as early, and an
    /// earlier one of the same root and key takes its place.
    #[test]
    fn an_append_keeps_each_roots_earliest_entry() {
        let dir = tempfile::tempdir().unwrap();
        let ada = SigningKey::from_bytes(&[1; 32]);
        let bob = SigningKey::from_bytes(&[2; 32]);
        assert!(append_revocation(dir.path(), &revocation(&ada, 7, 20)).unwrap());
        assert!(!append_revocation(dir.path(), &revocation(&ada, 7, 30)).unwrap());
        assert!(append_revocation(dir.path(), &revocation(&bob, 7, 30)).unwrap());
        assert!(append_revocation(dir.path(), &revocation(&ada, 7, 10)).unwrap());
        assert_eq!(
            load_revocations(dir.path()).unwrap(),
            vec![revocation(&ada, 7, 10), revocation(&bob, 7, 30)]
        );
    }

    /// A frame a crash cut short is dropped as the log is read, and the next
    /// append lands after the last whole frame.
    #[test]
    fn a_torn_frame_is_cut_off_before_the_next_append() {
        let dir = tempfile::tempdir().unwrap();
        let root = SigningKey::from_bytes(&[1; 32]);
        append_revocation(dir.path(), &revocation(&root, 1, 10)).unwrap();
        let path = dir.path().join(REVOCATIONS_FILE);
        let whole_len = fs::metadata(&path).unwrap().len();
        let torn = frame(&revocation(&root, 2, 10)).unwrap();
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(&torn[..torn.len() / 2]).unwrap();
        drop(file);
        assert_eq!(
            load_revocations(dir.path()).unwrap(),
            vec![revocation(&root, 1, 10)]
        );
        assert_eq!(
            fs::metadata(&path).unwrap().len(),
            whole_len + u64::try_from(torn.len() / 2).unwrap(),
            "a read leaves the file alone"
        );

        append_revocation(dir.path(), &revocation(&root, 3, 10)).unwrap();
        assert_eq!(
            load_revocations(dir.path()).unwrap(),
            vec![revocation(&root, 1, 10), revocation(&root, 3, 10)]
        );
    }

    /// One more revocation appends one frame: the log grows with what it
    /// holds, not with what it held before.
    #[test]
    fn an_append_writes_one_frame() {
        let dir = tempfile::tempdir().unwrap();
        let root = SigningKey::from_bytes(&[1; 32]);
        let path = dir.path().join(REVOCATIONS_FILE);
        append_revocation(dir.path(), &revocation(&root, 1, 10)).unwrap();
        let before = fs::metadata(&path).unwrap().len();
        let next = revocation(&root, 2, 10);
        append_revocation(dir.path(), &next).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().len(),
            before + u64::try_from(frame(&next).unwrap().len()).unwrap()
        );
    }
}
