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
/// worker of its own, never by the thread that calls here.
pub trait DeviceDocuments: Send + Sync + 'static {
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

/// The documents of the workspaces a device's service holds, written as that device itself: what
/// the session's own roles write on work of their own, without an app or a peer asking.
///
/// A write is signed and sealed as the device the service serves, so the workspace keeps it as
/// any of that device's writes and carries it to the workspace's other devices.
pub trait DeviceWrites: Send + Sync + 'static {
    /// Write `changes` to `app`'s `document` of `workspace`: the document's founding write when
    /// `founding` (the whole document, which the workspace must not hold yet), a change of it
    /// otherwise, sealed under `sealed` (the keys of the document's group) where the workspace
    /// seals its documents.
    fn write(
        &self,
        workspace: &str,
        app: &str,
        document: &str,
        changes: &[u8],
        founding: bool,
        sealed: Option<&key_group::ContentKeys>,
    ) -> DocumentWrite;
}
