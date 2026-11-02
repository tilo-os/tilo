//! The content an offloaded job's worker already keeps: the workspace
//! versions an always-on device's filesystem replica holds.
//!
//! A compute job names its inputs and outputs by blake3 of the *bytes*
//! themselves, while the workspace filesystem names a file version by its
//! *manifest* hash: blake3 of the chunk list that describes the bytes. A
//! device that sent a job can prove it holds an input, and name the version
//! as the proof's locator ([`ComputeContentStore::locator`]); a worker whose
//! replica holds that version whole then runs the job on it in place,
//! without the bytes moving ([`ReplicaContent`]).
//!
//! Nothing here decides anything: a version found only by the requester's
//! own locator is returned only when its bytes hash to the input digest.

use fs::store::ContentStore;
use thiserror::Error;

use crate::compute::HeldContent;

/// Why a workspace file could not be bound as a job input.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum BindError {
    /// This device holds no manifest under that hash.
    #[error("no workspace content under that manifest")]
    UnknownManifest,
    /// The manifest is known but not every byte is on this device yet.
    #[error("the file's content is not on this device yet")]
    NotLocal,
    /// The local bytes do not match the manifest.
    #[error("the file's local content does not match its manifest")]
    Integrity,
    /// Where the rooms' media is kept did not answer.
    #[error("the room's media: {0}")]
    Media(String),
}

/// The workspace content an always-on device's filesystem replica keeps
/// (`fs replica`, one store per home-peer workspace), as [`HeldContent`]
/// for the compute worker running beside it.
///
/// The replica process holds its stores open, so this reads their
/// file-backed versions without opening them
/// ([`ContentStore::read_file_version_unlocked`]), and asks the replica for
/// smaller ones, which are loose blobs inside its database, over the
/// owner-only socket each workspace's replica serves in its own cache
/// directory ([`fs::replica_content`]). A version is found only by the
/// requester's locator — the manifest hash it proved its copy under — and
/// returned only when its bytes hash to the input digest: a partly hydrated,
/// replaced, foreign or misreported version is simply not held, and the
/// input is fetched instead.
#[derive(Clone, Debug)]
pub struct ReplicaContent {
    cache_dirs: Vec<std::path::PathBuf>,
}

impl ReplicaContent {
    /// The replica stores at `cache_dirs`.
    #[must_use]
    pub fn new(cache_dirs: Vec<std::path::PathBuf>) -> Self {
        Self { cache_dirs }
    }
}

impl HeldContent for ReplicaContent {
    fn held(
        &self,
        digest: [u8; 32],
        locator: Option<[u8; 32]>,
        max_bytes: u64,
    ) -> Result<Option<Vec<u8>>, String> {
        // Without the requester's file version there is nothing to look up:
        // a digest alone names no file here.
        let Some(manifest) = locator else {
            return Ok(None);
        };
        let mut failure = None;
        for dir in &self.cache_dirs {
            let read = match ContentStore::read_file_version_unlocked(dir, manifest, max_bytes) {
                Ok(None) => fs::replica_content::read(dir, manifest, max_bytes),
                file => file,
            };
            match read {
                Ok(Some(bytes)) if blake3::hash(&bytes).as_bytes() == &digest => {
                    return Ok(Some(bytes));
                }
                Ok(_) => {}
                Err(error) => failure = Some(error.to_string()),
            }
        }
        failure.map_or(Ok(None), Err)
    }

    fn locates(&self) -> bool {
        true
    }
}
