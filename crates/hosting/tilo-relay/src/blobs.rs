//! The host's one blob store: bytes it keeps for the workspaces it governs,
//! each under its own BLAKE3 hash in one directory.
//!
//! What a host keeps there is content a workspace's records reference but do
//! not carry: what its bots fetched from outside (a bridged person's avatar),
//! the files members posted in a workspace's Chat and the pictures and
//! recordings of their reports ([`crate::room_bots::attachments`]), and the
//! bytes of the files a governed workspace's members wrote
//! ([`crate::files_content`]). Members read any of it by asking their
//! workspace for the hash their record names
//! ([`crate::workspaces::Workspaces::content_answer`]).
//!
//! Content is addressed by hash and read only through a workspace whose
//! record references it, so one directory serves every workspace: possessing
//! a hash grants nothing on its own, and the same bytes kept for one
//! workspace are the same bytes for another.

use std::path::{Path, PathBuf};

use crate::workspaces::WorkspaceContent;

/// The directory, under a host's state root, its blob store keeps its blobs
/// in.
pub const DIRECTORY: &str = "blobs";

/// Bytes kept by their BLAKE3 hash; a published blob is an avatar served at
/// the host's public avatar URL.
pub struct FsBlobs {
    dir: PathBuf,
    public: Option<crate::public_avatars::PublicAvatars>,
}

impl FsBlobs {
    /// Blobs under `dir`, published through `public` when the host serves
    /// avatars.
    ///
    /// # Errors
    /// `dir` cannot be created.
    pub fn new(
        dir: PathBuf,
        public: Option<crate::public_avatars::PublicAvatars>,
    ) -> std::io::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir, public })
    }

    fn path(&self, blake3: &str) -> Option<PathBuf> {
        (!blake3.is_empty() && blake3.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then(|| self.dir.join(blake3))
    }

    /// Whether the blob `blake3` is kept here.
    #[must_use]
    pub fn contains(&self, blake3: &str) -> bool {
        self.path(blake3).is_some_and(|path| path.is_file())
    }

    /// How many bytes the blob `blake3` takes here, when it is kept.
    #[must_use]
    pub fn kept_bytes(&self, blake3: &str) -> Option<u64> {
        self.path(blake3)
            .and_then(|path| std::fs::metadata(path).ok())
            .map(|kept| kept.len())
    }

    /// The bytes kept as `blake3`; nothing when this host keeps none.
    #[must_use]
    pub fn read(&self, blake3: &str) -> Option<Vec<u8>> {
        std::fs::read(self.path(blake3)?).ok()
    }

    /// Keep `bytes` under their BLAKE3 hash, which is returned; bytes already
    /// kept as that hash are left as they are.
    ///
    /// # Errors
    /// The bytes cannot be written, or `dir` is not one.
    pub fn keep(&self, bytes: &[u8]) -> Result<String, String> {
        let hash = blake3::hash(bytes).to_hex().to_string();
        let path = self.path(&hash).ok_or("not a hash")?;
        if !path.is_file() {
            tilo_files::native::atomic::write(&path, bytes).map_err(|error| error.to_string())?;
        }
        Ok(hash)
    }

    /// Let go of the blob `blake3`; nothing when it is not kept.
    ///
    /// # Errors
    /// The file is there and cannot be removed.
    pub fn remove(&self, blake3: &str) -> std::io::Result<()> {
        let Some(path) = self.path(blake3) else {
            return Ok(());
        };
        match std::fs::remove_file(path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    /// Publish the bytes kept as `blake3` where this host serves blobs
    /// publicly (a bridged person's avatar).
    ///
    /// # Errors
    /// This host publishes no blobs, keeps none as `blake3`, or the public
    /// side refused them.
    pub fn publish(&self, blake3: &str) -> Result<(), String> {
        let public = self.public.as_ref().ok_or("this host publishes no blobs")?;
        let bytes = self.read(blake3).ok_or("no such blob")?;
        public
            .publish(&bytes)
            .map(drop)
            .map_err(|error| error.to_string())
    }

    /// Where the store keeps its blobs.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.dir
    }
}

/// Members of the workspaces this host serves get whatever it keeps for them:
/// what its bots fetched, what their own records reference. The app and the
/// workspace a request names decide who may ask
/// ([`crate::workspaces::Workspaces::content_answer`]), not what the store
/// answers with: the bytes are the same whatever record names them.
impl WorkspaceContent for FsBlobs {
    fn read(&self, _workspace: &str, _app: &str, hash: &str) -> Option<Vec<u8>> {
        FsBlobs::read(self, hash)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test this is the assertion")]

    use super::*;

    #[test]
    fn bytes_are_kept_under_their_hash_and_read_back_from_it() {
        let scratch = tempfile::tempdir().unwrap();
        let blobs = FsBlobs::new(scratch.path().join("blobs"), None).unwrap();
        let bytes = b"a picture someone posted".to_vec();
        let hash = blobs.keep(&bytes).unwrap();
        assert_eq!(hash, blake3::hash(&bytes).to_hex().to_string());
        assert!(blobs.contains(&hash));
        assert_eq!(blobs.read(&hash).as_deref(), Some(bytes.as_slice()));
        assert_eq!(
            blobs.kept_bytes(&hash),
            Some(u64::try_from(bytes.len()).unwrap())
        );
        assert_eq!(blobs.keep(&bytes).unwrap(), hash, "kept once");

        blobs.remove(&hash).unwrap();
        assert!(!blobs.contains(&hash));
        assert_eq!(blobs.read(&hash), None);
    }

    #[test]
    fn a_name_that_is_no_hash_names_no_blob() {
        let scratch = tempfile::tempdir().unwrap();
        let blobs = FsBlobs::new(scratch.path().join("blobs"), None).unwrap();
        assert!(!blobs.contains("../elsewhere"));
        assert_eq!(blobs.read(""), None);
        assert_eq!(blobs.kept_bytes("not a hash"), None);
    }
}
