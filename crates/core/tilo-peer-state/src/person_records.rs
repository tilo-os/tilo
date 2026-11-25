//! The documents of the person's own workspace, as the parts of a session that keep a
//! person's own records read and write them.
//!
//! What is the person's alone — their devices, the answers they gave about them, the
//! machines asking to join, the disk recovery keys their devices kept, the shell they
//! arranged — is an ordinary document of their own workspace: written through the
//! workspace's typed actions and records, carried to their other devices as any
//! workspace resource is, and read and written here under the workspace's roles.
//!
//! A reader keeps what it last read until [`PersonDocuments::changes`] moves, so a part of
//! the session that reads a document on its own thread sees the person's other devices'
//! writes without holding the workspace's lock.

use automerge::AutoCommit;
use tilo_user_identity::DeviceCert;

/// The person's own workspace, as one of the session's parts reads and writes its documents.
pub trait PersonDocuments: Send + Sync {
    /// This device's own key.
    fn device(&self) -> [u8; 32];

    /// The person's root, which their own workspace's id follows from.
    fn person(&self) -> [u8; 32];

    /// The person's devices whose certificates hold now, with what each says of itself.
    fn devices(&self) -> Vec<DeviceCert>;

    /// Moves whenever a document here changed, here or on another of their devices.
    fn changes(&self) -> tokio::sync::watch::Receiver<u64>;

    /// The document `id` of `app` in their own workspace, as this device last read it:
    /// `None` while this device holds none of it.
    fn document(&self, app: &str, id: &str) -> Option<Vec<u8>>;

    /// Edit the document `id` of `app`: `edit` runs on the document as this device holds
    /// it, and what it writes reaches the person's other devices as any workspace's change
    /// does. A typed record's `kind` is the kind its store writes it as; `None` for one that is
    /// no typed record. An edit that cannot be made yet waits for the workspace.
    fn edit(&self, app: &str, id: &str, kind: Option<&'static str>, edit: DocumentEdit);
}

/// An edit of one of the person's documents, run on it as this device holds it.
pub type DocumentEdit = Box<dyn Fn(&mut AutoCommit) -> Result<(), String> + Send + Sync>;
