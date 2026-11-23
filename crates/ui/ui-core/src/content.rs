//! Content by address: the store a host lends an app (`ViewCx::content`), and in a native process the
//! device store the launcher handed it, so an app or a headless agent reaches content through the
//! toolkit rather than through the store crates.
//!
//! A workspace file is the manifest of its chunks, and its first bytes name the app that opens
//! it: both are content questions, answered here too.

pub use tilo_content::{
    ContentStore, NoContent, content_address, is_sealed, opened_as, valid_hash, valid_reference,
};
pub use tilo_content_device::{PublicCopy, PublicState};
pub use tilo_content_types::{HEAD_BYTES, Landing, Takes, landing, opened_extension};

/// A workspace file whose content is a manifest: the chunks it lists, each
/// read by its own hash.
pub mod manifest {
    pub use fs_content::{is_manifest, whole};

    /// Keep `bytes` in `store` as a workspace file's content: each chunk under its own hash,
    /// then the manifest that lists them, so a device that reads the file by its manifest's
    /// hash (the workspace's service, a computer that mounts the folder) asks for the chunks
    /// it lacks. The manifest's hash (hex), which the file's node names.
    ///
    /// # Errors
    /// The store refused a chunk or the manifest (full, or an item past its bound).
    pub fn keep(store: &super::ContentStore, bytes: &[u8]) -> Result<String, String> {
        let (manifest, _) = fs_content::build(bytes, None);
        for index in 0..manifest.chunks().len() {
            let Some(range) = manifest.chunk_range(index) else {
                continue;
            };
            let start = usize::try_from(range.start).unwrap_or(usize::MAX);
            let end = usize::try_from(range.end).unwrap_or(usize::MAX);
            let chunk = bytes.get(start..end).unwrap_or_default();
            store.put_public(chunk).map_err(|error| error.to_string())?;
        }
        store
            .put_public(&manifest.encode())
            .map_err(|error| error.to_string())
    }
}

/// The device's one copy of the public file `name` (a catalogue the filesystem daemon mirrors for every
/// app and workspace), when this host keeps such copies: `None` on one that keeps none (the web).
#[must_use]
pub fn public_copy(name: &'static str) -> Option<PublicCopy> {
    tilo_content_device::public_copy(name)
}

/// The device content store the launcher handed this process, or `None` when it was handed none
/// (started outside a Tilo session).
#[cfg(feature = "native")]
#[must_use]
pub fn device() -> Option<ContentStore> {
    tilo_content_device::device()
}

/// The content `workspace` has, read through the document media the session this process runs
/// in serves its apps (`TILO_FS_BLOB_SOCKET`, `fs serve-media`), or `None` when none is served.
///
/// A session process draws the same pictures its apps do — the shell composes views of
/// records whose stills an app's connector kept — and the launcher handed it no capability of its
/// own, so this is where it reads them from. Which workspace a picture is in is the caller's to
/// say: the session serves a place's pictures from the workspace that place is in.
#[cfg(feature = "native")]
#[must_use]
pub fn session(workspace: &str) -> Option<ContentStore> {
    tilo_content_device::session(workspace)
}

/// Tell every store a process drawing a session's content opens from here on which workspaces it draws,
/// the workspace in front first: a picture of any of them is read through the one that references it
/// ([`session`]).
#[cfg(feature = "native")]
pub fn set_session_workspaces(workspaces: Vec<String>) {
    tilo_content_device::set_session_workspaces(workspaces);
}
