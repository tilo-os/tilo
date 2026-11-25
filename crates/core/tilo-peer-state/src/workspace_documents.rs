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

    /// Every document `workspace` holds, by its app and route: a record of the workspace's store
    /// by its id, with its kind.
    ///
    /// # Errors
    ///
    /// The service is not open or holds no such workspace.
    fn listing(&self, workspace: &str)
    -> Result<Vec<tilo_actions::remote::ListedResource>, String>;

    /// Whether the service holds anything of `app`'s `resource` in `workspace`.
    fn holds(&self, workspace: &str, app: &str, resource: &str) -> bool;

    /// The workspaces the service holds, by their ids.
    fn workspaces(&self) -> Vec<String>;

    /// Every change of `workspace`'s members its governance names, oldest first
    /// ([`crate::workspace::base::named_changes`]): what a feed of the workspace's changes lists
    /// as changes of its members' resource. A reader holding no workspace's governance names none.
    ///
    /// # Errors
    ///
    /// The service is not open or holds no such workspace.
    fn member_changes(&self, workspace: &str) -> Result<Vec<MemberChange>, String> {
        let _ = workspace;
        Ok(Vec::new())
    }
}

/// One change of a workspace's members as its governance names it: the change's hash, the
/// message naming the action it carried out and who decided it, and the time its commit carries
/// (Unix seconds).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MemberChange {
    pub change: [u8; 32],
    pub message: String,
    pub time: i64,
}

impl From<crate::workspace::base::KeptChange> for MemberChange {
    fn from(kept: crate::workspace::base::KeptChange) -> Self {
        Self {
            change: kept.change.0,
            message: kept.message,
            time: kept.time,
        }
    }
}

/// A write of one document under way in this device's service: done, or why the workspace took
/// nothing of it.
#[cfg(not(target_arch = "wasm32"))]
pub type DocumentWrite = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<(), crate::owner::WriteRefusal>> + Send + 'static>,
>;
/// A write of one document under way in this device's service: done, or why the workspace took
/// nothing of it.
#[cfg(target_arch = "wasm32")]
pub type DocumentWrite = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<(), crate::owner::WriteRefusal>> + 'static>,
>;

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

/// What a write names besides the workspace and the changes themselves: the record it is of.
#[derive(Clone, Copy)]
pub struct DocumentWriting<'a> {
    /// The app whose record this is.
    pub app: &'a str,
    /// The record's kind, where the writer names it: what founds a record the workspace holds
    /// none of yet. `None` reads it from what the workspace holds.
    pub kind: Option<&'a str>,
    /// The record itself.
    pub record: &'a str,
}

/// The documents of the workspaces a device's service holds, written as that device itself: what
/// the session's own roles write on work of their own, without an app or a peer asking.
///
/// A write is signed and sealed as the device the service serves, so the workspace keeps it as
/// any of that device's writes and carries it to the workspace's other devices.
pub trait DeviceWrites: collab_types::signer::MaybeSendSync + 'static {
    /// Write `changes` to `workspace`'s record of [`DocumentWriting`]: founding the record from
    /// them when the workspace holds none of it, a change of it otherwise. A record a device
    /// starts is founded by changes that hold its canonical founding first, as every copy of
    /// it derives it.
    ///
    /// Answers once the workspace holds the write, or why it took nothing of it.
    fn write(&self, workspace: &str, changes: &[u8], writing: DocumentWriting<'_>)
    -> DocumentWrite;

    /// The wait for the device serving `workspace` to answer every write this device made to
    /// `documents` — each an app and a document of it — before this call: the wait answers `Ok`
    /// once the serving device holds each of them, and `Err` with why once it refused one.
    ///
    /// `None` when this holder has no serving device to ask: a workspace it hosts and keeps
    /// itself, and a copy whose writes the device's own link carries and follows
    /// ([`crate::owner::AppWrite`]). Whoever waits then waits on that link instead.
    fn served(&self, workspace: &str, documents: &[(&str, &str)]) -> Option<DocumentServing> {
        let _ = (workspace, documents);
        None
    }
}
