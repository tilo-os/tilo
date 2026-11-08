//! Content by address: the store a host lends an app (`ViewCx::content`),
//! and in a native process the device store the launcher handed it, so an
//! app or a headless agent reaches content through the toolkit rather than
//! through the store's own crates.

pub use tilo_content::{ContentStore, opened_as, valid_hash};
pub use tilo_content_device::{PublicCopy, PublicState};

/// The device's one copy of the public file `name` (a catalogue the
/// filesystem daemon mirrors for every app and workspace), when this host
/// keeps such copies: `None` on one that keeps none (the web).
#[must_use]
pub fn public_copy(name: &'static str) -> Option<PublicCopy> {
    tilo_content_device::public_copy(name)
}

/// The device content store the launcher handed this process, or `None`
/// when it was handed none (started outside a Tilo session).
#[cfg(feature = "native")]
#[must_use]
pub fn device() -> Option<ContentStore> {
    tilo_content_device::device()
}

/// The content `workspace` holds, read through the document media the session this process runs
/// in serves its apps (`TILO_FS_BLOB_SOCKET`, `fs serve-media`), or `None` when none is served.
///
/// A session's own process draws the same pictures its apps do — the shell composes views of
/// records whose stills an app's connector kept — and the launcher handed it no capability of its
/// own, so this is where it reads them from. Which workspace a picture is in is the caller's to
/// say: the session serves a place's pictures from the workspace that place stands in.
#[cfg(feature = "native")]
#[must_use]
pub fn session(workspace: &str) -> Option<ContentStore> {
    tilo_content_device::session(workspace)
}

/// Tell every store a process drawing a session's content opens from here on which workspaces
/// it draws, the workspace in front first: a picture of any of them is read through the one
/// that references it ([`session`]).
#[cfg(feature = "native")]
pub fn set_session_workspaces(workspaces: Vec<String>) {
    tilo_content_device::set_session_workspaces(workspaces);
}
