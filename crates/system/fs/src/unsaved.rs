//! Durable staging for file bytes the mount accepted but has not yet
//! committed as a version.
//!
//! Two situations put bytes here:
//!
//! * `fsync` of an open file. Durability is the only promise `fsync` makes,
//!   so it stages the bytes here instead of minting a signed version per call
//!   (a database that syncs every transaction would otherwise exhaust the
//!   record's version budget). The version is committed when the file is
//!   closed.
//! * A failed commit. The error is returned to the writer and the bytes stay
//!   here (and in memory) until a later commit succeeds, so neither a crash
//!   nor an unmount can drop them.
//!
//! Each entry is two files keyed by the record: the exact bytes and a small
//! JSON description naming the bytes' blake3 hash. The bytes are written
//! first, so a crash between the two leaves either the previous complete pair
//! or a description whose hash no longer matches; the loader reports such
//! mismatches instead of trusting them.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const ENTRY_SCHEMA_VERSION: u16 = 1;
const BYTES_EXTENSION: &str = "bytes";
const ENTRY_EXTENSION: &str = "json";

/// What is known about one staged set of bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnsavedEntry {
    pub schema_version: u16,
    /// Stable key: the record id when the node has one, else `ino-<n>`.
    pub key: String,
    pub record_id: Option<fs_protocol::FileRecordId>,
    /// Workspace path when the bytes were staged (diagnostic and fallback
    /// location for recovery when the record is gone).
    pub path: Option<String>,
    /// Committed content the staged bytes were derived from. Recovery only
    /// writes the bytes back into the same record while it still holds this
    /// content; otherwise they are kept as a separate copy.
    pub base_hash: Option<String>,
    pub byte_len: u64,
    pub blake3: String,
    pub staged_at_ms: u64,
    /// Why the bytes are here: `None` for an `fsync`, the commit error otherwise.
    pub error: Option<String>,
}

/// A staged entry read back from disk together with its bytes.
#[derive(Debug)]
pub struct LoadedUnsaved {
    pub entry: UnsavedEntry,
    pub bytes: Vec<u8>,
}

/// The on-disk staging directory for one workspace mount.
#[derive(Debug)]
pub struct UnsavedStore {
    dir: PathBuf,
}

impl UnsavedStore {
    /// Open (creating, owner-only) the staging directory.
    ///
    /// # Errors
    ///
    /// Returns an error when the directory cannot be created or restricted.
    pub fn open(dir: impl Into<PathBuf>) -> io::Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self { dir })
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn paths(&self, key: &str) -> io::Result<(PathBuf, PathBuf)> {
        if key.is_empty()
            || !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsaved entry key must be a plain identifier",
            ));
        }
        Ok((
            self.dir.join(format!("{key}.{BYTES_EXTENSION}")),
            self.dir.join(format!("{key}.{ENTRY_EXTENSION}")),
        ))
    }

    /// Durably stage `bytes` under `entry.key`, replacing any earlier entry.
    /// Returns only after both files and the directory are synced.
    ///
    /// # Errors
    ///
    /// Returns any I/O error; on error the previous entry, if any, may have
    /// been replaced by the new bytes but never by a torn mix of the two.
    pub fn stage(&self, entry: &UnsavedEntry, bytes: &[u8]) -> io::Result<()> {
        let (bytes_path, entry_path) = self.paths(&entry.key)?;
        let json = serde_json::to_vec_pretty(entry).map_err(io::Error::other)?;
        tilo_files::native::atomic::write_private(&bytes_path, bytes)?;
        tilo_files::native::atomic::write_private(&entry_path, &json)?;
        Ok(())
    }

    /// [`Self::stage`] of the bytes of the file at `source`, which must not
    /// change while this runs. The file is copied file to file (the kernel
    /// shares extents where it can), never read into memory whole.
    ///
    /// # Errors
    ///
    /// As [`Self::stage`], and an error when the copy does not match
    /// `entry`'s length.
    pub fn stage_file(&self, entry: &UnsavedEntry, source: &Path) -> io::Result<()> {
        let (bytes_path, entry_path) = self.paths(&entry.key)?;
        let json = serde_json::to_vec_pretty(entry).map_err(io::Error::other)?;
        let mut temporary = tempfile::Builder::new()
            .prefix(".staging-")
            .tempfile_in(&self.dir)?;
        let copied = io::copy(&mut std::fs::File::open(source)?, temporary.as_file_mut())?;
        if copied != entry.byte_len {
            return Err(io::Error::other(format!(
                "{} changed while it was staged",
                source.display()
            )));
        }
        temporary.as_file().sync_all()?;
        temporary
            .persist(&bytes_path)
            .map_err(|error| error.error)?;
        tilo_files::native::atomic::sync_directory(&self.dir)?;
        tilo_files::native::atomic::write_private(&entry_path, &json)?;
        Ok(())
    }

    /// Remove the entry for `key`, if present.
    ///
    /// # Errors
    ///
    /// Returns an error when an existing file cannot be removed.
    pub fn remove(&self, key: &str) -> io::Result<()> {
        let (bytes_path, entry_path) = self.paths(key)?;
        for path in [entry_path, bytes_path] {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        tilo_files::native::atomic::sync_directory(&self.dir)
    }

    /// Whether an entry exists for `key`.
    #[must_use]
    pub fn contains(&self, key: &str) -> bool {
        self.paths(key)
            .is_ok_and(|(_, entry_path)| entry_path.exists())
    }

    /// Every complete entry. Entries whose bytes do not match their recorded
    /// hash are returned in the error list with their paths, never as data.
    #[must_use]
    pub fn load_all(&self) -> (Vec<LoadedUnsaved>, Vec<(PathBuf, String)>) {
        let mut loaded = Vec::new();
        let mut damaged = Vec::new();
        let Ok(read_dir) = std::fs::read_dir(&self.dir) else {
            return (loaded, damaged);
        };
        let mut names: Vec<PathBuf> = read_dir
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some(ENTRY_EXTENSION))
            .collect();
        names.sort();
        for entry_path in names {
            match Self::load_one(&entry_path) {
                Ok(item) => loaded.push(item),
                Err(error) => damaged.push((entry_path, error)),
            }
        }
        (loaded, damaged)
    }

    fn load_one(entry_path: &Path) -> Result<LoadedUnsaved, String> {
        let json = std::fs::read(entry_path).map_err(|e| e.to_string())?;
        let entry: UnsavedEntry = serde_json::from_slice(&json).map_err(|e| e.to_string())?;
        if entry.schema_version != ENTRY_SCHEMA_VERSION {
            return Err(format!(
                "unsupported unsaved entry schema {}",
                entry.schema_version
            ));
        }
        let bytes_path = entry_path.with_extension(BYTES_EXTENSION);
        let bytes = std::fs::read(&bytes_path).map_err(|e| e.to_string())?;
        if blake3::hash(&bytes).to_hex().as_str() != entry.blake3
            || bytes.len() as u64 != entry.byte_len
        {
            return Err(format!(
                "staged bytes at {} do not match their description",
                bytes_path.display()
            ));
        }
        Ok(LoadedUnsaved { entry, bytes })
    }
}

impl UnsavedEntry {
    /// Describe `bytes` for staging.
    #[must_use]
    pub fn describe(
        key: String,
        record_id: Option<fs_protocol::FileRecordId>,
        path: Option<String>,
        base_hash: Option<iroh_blobs::Hash>,
        bytes: &[u8],
        staged_at_ms: u64,
        error: Option<String>,
    ) -> Self {
        Self {
            schema_version: ENTRY_SCHEMA_VERSION,
            key,
            record_id,
            path,
            base_hash: base_hash.map(|hash| hash.to_hex()),
            byte_len: bytes.len() as u64,
            blake3: blake3::hash(bytes).to_hex().to_string(),
            staged_at_ms,
            error,
        }
    }

    /// [`Self::describe`] of the bytes of the file at `file`, read in
    /// bounded memory.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read.
    pub fn describe_file(
        key: String,
        record_id: Option<fs_protocol::FileRecordId>,
        path: Option<String>,
        base_hash: Option<iroh_blobs::Hash>,
        file: &Path,
        staged_at_ms: u64,
        error: Option<String>,
    ) -> io::Result<Self> {
        let (digest, byte_len) = file_digest(file)?;
        Ok(Self {
            schema_version: ENTRY_SCHEMA_VERSION,
            key,
            record_id,
            path,
            base_hash: base_hash.map(|hash| hash.to_hex()),
            byte_len,
            blake3: digest.to_hex().to_string(),
            staged_at_ms,
            error,
        })
    }

    /// The base hash, parsed.
    #[must_use]
    pub fn base(&self) -> Option<iroh_blobs::Hash> {
        self.base_hash.as_deref().and_then(|hex| hex.parse().ok())
    }
}

/// The blake3 hash and length of the file at `path`, read in bounded memory.
///
/// # Errors
///
/// Returns the read error.
pub(crate) fn file_digest(path: &Path) -> io::Result<(blake3::Hash, u64)> {
    use std::io::Read as _;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0_u8; 1 << 20];
    let mut len = 0_u64;
    loop {
        let n = match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        hasher.update(buf.get(..n).unwrap_or_default());
        len = len.saturating_add(n as u64);
    }
    Ok((hasher.finalize(), len))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(key: &str, bytes: &[u8]) -> UnsavedEntry {
        UnsavedEntry::describe(key.into(), None, Some("/a".into()), None, bytes, 7, None)
    }

    #[test]
    fn unsaved_bytes_stage_whole_and_torn_or_escaping_keys_are_refused() {
        // Phase `staged_bytes_round_trip_and_are_replaced_whole`.
        {
            let dir = tempfile::tempdir().unwrap();
            let store = UnsavedStore::open(dir.path().join("unsaved")).unwrap();
            store.stage(&entry("k1", b"first"), b"first").unwrap();
            store.stage(&entry("k1", b"second!"), b"second!").unwrap();
            let (loaded, damaged) = store.load_all();
            assert!(damaged.is_empty());
            assert_eq!(loaded.len(), 1);
            assert_eq!(loaded[0].bytes, b"second!");
            assert!(store.contains("k1"));
            store.remove("k1").unwrap();
            assert!(!store.contains("k1"));
            assert!(store.load_all().0.is_empty());
        }
        // Phase `a_torn_pair_is_reported_not_trusted`.
        {
            let dir = tempfile::tempdir().unwrap();
            let store = UnsavedStore::open(dir.path()).unwrap();
            store
                .stage(&entry("k2", b"described"), b"described")
                .unwrap();
            std::fs::write(dir.path().join("k2.bytes"), b"other bytes").unwrap();
            let (loaded, damaged) = store.load_all();
            assert!(loaded.is_empty());
            assert_eq!(damaged.len(), 1);
        }
        // Phase `keys_cannot_escape_the_directory`.
        {
            let dir = tempfile::tempdir().unwrap();
            let store = UnsavedStore::open(dir.path()).unwrap();
            assert!(store.stage(&entry("../x", b""), b"").is_err());
            assert!(store.remove("a/b").is_err());
        }
    }

    #[test]
    fn a_staged_file_round_trips_like_staged_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let store = UnsavedStore::open(dir.path().join("unsaved")).unwrap();
        let source = dir.path().join("stage-file");
        let bytes: Vec<u8> = (0..3_000_000_u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&source, &bytes).unwrap();
        let described = UnsavedEntry::describe_file(
            "k3".into(),
            None,
            Some("/big".into()),
            None,
            &source,
            7,
            None,
        )
        .unwrap();
        assert_eq!(
            described,
            UnsavedEntry::describe(
                "k3".into(),
                None,
                Some("/big".into()),
                None,
                &bytes,
                7,
                None
            ),
            "a file is described exactly as its bytes are"
        );
        store.stage_file(&described, &source).unwrap();
        let (loaded, damaged) = store.load_all();
        assert!(damaged.is_empty());
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].bytes, bytes);
        // A source that changed since it was described is refused.
        std::fs::write(&source, b"shorter").unwrap();
        assert!(store.stage_file(&described, &source).is_err());
    }
}
