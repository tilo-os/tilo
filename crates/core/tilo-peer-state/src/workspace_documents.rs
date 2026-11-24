//! A workspace's documents as a device's own roles read and write them through its service.
//!
//! The service holding a workspace on a device answers its own device's reads without a
//! network hop ([`DeviceDocuments`]), and writes the documents the device's roles write as
//! that device itself ([`DeviceWrites`]): signed and sealed as the device the service
//! serves. Both sides name these traits, so a role reads and writes documents wherever the
//! service lives, the page included.

/// What this device's service holds of one document, as it reads it for the device itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocumentRead {
    /// The document whole, saved, at the heads and revision it stands at.
    Whole {
        /// Names exactly these heads: equal revisions are equal documents.
        revision: [u8; 32],
        /// The document's heads.
        heads: Vec<[u8; 32]>,
        /// The document, saved.
        bytes: Vec<u8>,
        /// The load hint for exactly `bytes` ([`tilo_automerge::load_hint`]), when they are
        /// large enough to need one: what their reader skips rebuilding.
        hint: Option<Vec<u8>>,
    },
    /// Only the changes since the heads the reader holds, which they complete to `heads`.
    Changes {
        /// The revision the completed document stands at.
        revision: [u8; 32],
        /// The heads the changes complete to.
        heads: Vec<[u8; 32]>,
        /// The changes, as raw change chunks.
        bytes: Vec<u8>,
    },
    /// The document stands where the reader last read it.
    Unchanged([u8; 32]),
    /// This device holds nothing of it.
    Absent,
    /// It is not here yet: a checkpoint of it is arriving, or the keys that open it are on their
    /// way, and it reads once they are.
    Pending,
}

/// The documents of the workspaces a device's service holds, read for that device itself: what
/// the session's own roles read on worker threads, without an app or a peer asking.
///
/// Synchronous, as those roles read: the service's tasks must be driven by a runtime with a
/// worker of its own, never by the thread that calls here. `Send + Sync` where the platform has
/// threads, nothing on the web, where a page's holder reaches JavaScript handles bound to its one
/// thread.
pub trait DeviceDocuments: collab_types::signer::MaybeSendSync + 'static {
    /// `app`'s `resource` of `workspace` whole, or, for `since` heads the service holds, only
    /// what changed after them.
    ///
    /// # Errors
    ///
    /// The service is not open, holds no such workspace, the device may not read it, or it cannot
    /// be read now.
    fn read(
        &self,
        workspace: &str,
        app: &str,
        resource: &str,
        since: Option<&[[u8; 32]]>,
    ) -> Result<DocumentRead, String>;

    /// Whether `app`'s `resource` of `workspace` reads now: held here, and nothing it waits for
    /// (its keys, its checkpoint, a write it lacks) still on its way. A service decides it without
    /// opening the resource, which costs a sealed resource its whole history the first time; this
    /// default reads it.
    fn readable(&self, workspace: &str, app: &str, resource: &str) -> bool {
        match self.read(workspace, app, resource, None) {
            Ok(
                DocumentRead::Whole { .. }
                | DocumentRead::Changes { .. }
                | DocumentRead::Unchanged(_),
            ) => true,
            Ok(DocumentRead::Absent | DocumentRead::Pending) => false,
            // A resource that does not read here is still the device's to answer for: what it
            // holds decides, as before it read.
            Err(error) => {
                tracing::warn!(%error, resource, "a resource did not read to decide whether it can");
                self.holds(workspace, app, resource)
            }
        }
    }

    /// Every document `workspace` holds, by its app and route.
    ///
    /// # Errors
    ///
    /// The service is not open or holds no such workspace.
    fn listing(&self, workspace: &str) -> Result<Vec<(String, String)>, String>;

    /// Whether the service holds anything of `app`'s `resource` in `workspace`.
    fn holds(&self, workspace: &str, app: &str, resource: &str) -> bool;

    /// The workspaces the service holds, by their ids.
    fn workspaces(&self) -> Vec<String>;
}

/// What a write of a document came to: the write as the workspace took it, or why it took none.
pub type WriteAnswer = Result<crate::owner::AppWrite, crate::owner::WriteRefusal>;

/// A write of one document under way in this device's service.
#[cfg(not(target_arch = "wasm32"))]
pub type DocumentWrite =
    std::pin::Pin<Box<dyn std::future::Future<Output = WriteAnswer> + Send + 'static>>;
/// A write of one document under way in this device's service.
#[cfg(target_arch = "wasm32")]
pub type DocumentWrite =
    std::pin::Pin<Box<dyn std::future::Future<Output = WriteAnswer> + 'static>>;

/// Whether the device serving a workspace answered this device's last writes to it: `Ok` once it
/// holds each of them, or why it refused one.
pub type DocumentServed = Result<(), String>;

/// The wait for a workspace's serving device to answer this device's writes there.
#[cfg(not(target_arch = "wasm32"))]
pub type DocumentServing =
    std::pin::Pin<Box<dyn std::future::Future<Output = DocumentServed> + Send + 'static>>;
/// The wait for a workspace's serving device to answer this device's writes there.
#[cfg(target_arch = "wasm32")]
pub type DocumentServing =
    std::pin::Pin<Box<dyn std::future::Future<Output = DocumentServed> + 'static>>;

/// What a write names besides the workspace and the changes themselves: the record it is of, who
/// reads it, and how it carries its changes there.
#[derive(Clone, Copy)]
pub struct DocumentWriting<'a> {
    /// The app whose record this is.
    pub app: &'a str,
    /// The document of that app.
    pub document: &'a str,
    /// The members who read it, where only some do.
    pub readers: Option<&'a str>,
    /// Whether this is the document's founding write: the whole document, which the workspace
    /// must not hold yet.
    pub founding: bool,
    /// Whether what is written is the whole document: a founding, or a checkpoint of one the
    /// workspace holds already.
    pub snapshot: bool,
    /// The keys of the document's group, where the workspace seals its documents.
    pub sealed: Option<&'a key_group::ContentKeys>,
}

/// The documents of the workspaces a device's service holds, written as that device itself: what
/// the session's own roles write on work of their own, without an app or a peer asking.
///
/// A write is signed and sealed as the device the service serves, so the workspace keeps it as
/// any of that device's writes and carries it to the workspace's other devices.
pub trait DeviceWrites: collab_types::signer::MaybeSendSync + 'static {
    /// Write `changes` to `workspace`'s document of [`DocumentWriting`]: its founding write when
    /// `writing.founding` (the whole document, which the workspace must not hold yet), a change
    /// of it otherwise; the whole document when `writing.snapshot` (a founding, or a checkpoint
    /// of one the workspace holds already), its changes otherwise; kept for `writing.readers`
    /// where only some members read it, sealed under `writing.sealed` (the keys of the document's
    /// group) where the workspace seals its documents.
    ///
    /// Answers the write as this device's apps take a member's: the signed op its holder admitted,
    /// and the author's access in the workspace.
    fn write(&self, workspace: &str, changes: &[u8], writing: DocumentWriting<'_>)
    -> DocumentWrite;

    /// The wait for the device serving `workspace` to answer every write this device made to
    /// `documents` — each an app and a document of it — before this call: the wait answers `Ok`
    /// once the serving device holds each of them, and `Err` with why once it refused one.
    ///
    /// `None` when this holder has no serving device to ask: a workspace it governs and keeps
    /// itself, and a copy whose writes the device's own link carries and follows
    /// ([`crate::owner::AppWrite`]). Whoever waits then waits on that link instead.
    fn served(&self, workspace: &str, documents: &[(&str, &str)]) -> Option<DocumentServing> {
        let _ = (workspace, documents);
        None
    }
}
