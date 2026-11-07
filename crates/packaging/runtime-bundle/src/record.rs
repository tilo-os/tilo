//! What a full verification leaves behind, so the next launch can trust the
//! same bytes without reading them.
//!
//! Verifying a bundle a person installed into their own store reads its whole
//! payload: hundreds of megabytes on a launch's critical path, on every
//! launch. Install-time verification proved the bytes once; this record lets
//! every later launch prove that the files are *still the same files* without
//! reading them. It holds, per payload file, the identity the file had when
//! it was verified: device, inode, size, modification time and change time.
//! Rewriting a file, replacing it, or restoring its modification time all move
//! one of those, and no unprivileged process can set a change time back.
//!
//! The record is evidence, not authority: a launch still checks the signed
//! manifest, its root hash and the publisher, and it reads any file the
//! record does not vouch for. It lives beside the other per-user state the
//! launcher keeps (never inside the bundle, whose file set is signed), where a
//! sandboxed app cannot write it.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tilo_files::native::checked::Identity;

/// Directory under the user's data home that holds the records: beside the
/// other per-user state the launcher keeps, and outside every directory a
/// launch makes writable for a sandboxed app.
pub const DIRECTORY_NAME: &str = "tilo-runtime-verified";

/// Answer [`DIRECTORY_NAME`], creating it if needed, or `None` where the data
/// home is unknown or the directory cannot be made — a launch then verifies
/// in full, as it did before records, and an install leaves none.
#[must_use]
pub fn directory() -> Option<PathBuf> {
    let dir = tilo_paths::xdg::data_home()?.join(DIRECTORY_NAME);
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Bumped whenever what a record holds, or what a hit relies on, changes. A
/// record of another version is ignored and the bundle verified in full.
const VERSION: u32 = 1;

/// A record larger than this is not one this build wrote.
const MAX_RECORD_BYTES: u64 = 8 << 20;

/// The identity of every payload file of one bundle at the moment its bytes
/// were proven.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationRecord {
    version: u32,
    /// The root hash the identities were proven to: the record answers only
    /// for the bundle whose manifest names it.
    root_hash: String,
    /// One entry per payload file, by payload-relative path.
    files: Vec<RecordedFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RecordedFile {
    path: String,
    identity: Identity,
}

impl VerificationRecord {
    /// A record of `files`, the identities a verification just proved, for the
    /// bundle whose payload hashes to `root_hash`.
    #[must_use]
    pub fn new(root_hash: &str, files: Vec<(String, Identity)>) -> Self {
        Self {
            version: VERSION,
            root_hash: root_hash.to_string(),
            files: files
                .into_iter()
                .map(|(path, identity)| RecordedFile { path, identity })
                .collect(),
        }
    }

    /// Whether this record vouches that the payload file `path` holds, right
    /// now, the bytes it was verified with.
    #[must_use]
    pub fn holds(&self, path: &str, now: Identity) -> bool {
        self.files
            .iter()
            .any(|file| file.path == path && file.identity == now)
    }

    /// The record `dir` holds for the bundle whose payload hashes to
    /// `root_hash`, when it holds one this build wrote and can read. A
    /// missing, unreadable, malformed or foreign record is no record: every
    /// file it does not vouch for is verified the way it always was.
    #[must_use]
    pub fn read(dir: &Path, root_hash: &str) -> Option<Self> {
        let path = dir.join(format!("{root_hash}.json"));
        let bytes = tilo_files::native::bounded::read(&path, MAX_RECORD_BYTES)
            .ok()
            .flatten()?;
        let record: Self = serde_json::from_slice(&bytes).ok()?;
        (record.version == VERSION && record.root_hash == root_hash).then_some(record)
    }

    /// Record these identities for `root_hash` in `dir`, replacing what was
    /// there.
    ///
    /// # Errors
    ///
    /// A directory that cannot be written fails this and nothing else: the
    /// launch that proved the bundle has its answer already, and the cost is
    /// a later launch skipping the verification this one could not leave.
    pub fn write(&self, dir: &Path) -> std::io::Result<()> {
        let bytes = serde_json::to_vec(self).map_err(std::io::Error::other)?;
        let path = dir.join(format!("{}.json", self.root_hash));
        tilo_files::native::atomic::write_creating_parent(&path, &bytes)
    }
}
