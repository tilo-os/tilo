//! Content puts: how a session client hands bytes to its room's content
//! store, chunk by chunk, and gets back a reference to them.
//!
//! Actions carry intent and references; bytes travel by session transport
//! into the content store. A client (an AI agent through MCP, first) puts
//! the bytes, receives `tilo.content:<hash>` ([`crate::content_resource`])
//! and passes that reference to an action, which resolves it in the room.
//!
//! A put is invisible to everyone until an action at its own node
//! references it: storing content is never authority over any document.

use super::{ActionHostError, Caller};

/// The most raw bytes one append carries. MCP requests are bounded at
/// 1 MiB; 512 KiB is about 683 KiB of base64, leaving room for the rest of
/// the request.
pub const MAX_CONTENT_PUT_CHUNK: usize = 512 * 1024;

/// Uploads one session keeps open at once.
pub const MAX_OPEN_CONTENT_PUTS: usize = 4;

/// What a host without a content store answers.
pub const CONTENT_PUTS_UNAVAILABLE: &str = "content uploads are not available here";

/// A host's door for content puts.
pub trait ContentPuts: Send + Sync {
    /// Open an upload of exactly `size` bytes into the content store of
    /// `caller`'s room.
    ///
    /// # Errors
    ///
    /// Refuses a caller that may not add content to the room, a size the
    /// store does not take, or a store that cannot be reached.
    fn begin(&self, caller: &Caller, size: u64) -> Result<Box<dyn ContentPut>, ActionHostError>;
}

/// One upload in progress, owned by the session that began it: dropped
/// unfinished (the session ended), it stores nothing.
pub trait ContentPut: Send {
    /// The size the upload began with.
    fn size(&self) -> u64;

    /// Bytes accepted so far.
    fn written(&self) -> u64;

    /// Accept `chunk` at `offset`, answering the bytes accepted so far.
    /// `offset` must be exactly what was accepted so far, so a chunk sent
    /// again is refused, never stored twice; the refusal names the offset
    /// to resume at.
    ///
    /// # Errors
    ///
    /// Refuses a wrong offset, a chunk past the upload's size, or a store
    /// failure (which ends the upload).
    fn append(&mut self, offset: u64, chunk: &[u8]) -> Result<u64, ActionHostError>;

    /// Store the content and answer its reference, `tilo.content:<hash>`.
    ///
    /// # Errors
    ///
    /// Refuses an upload short of its size, a caller no longer allowed to
    /// add content to the room, or a store failure. Nothing is stored then.
    fn finish(self: Box<Self>) -> Result<String, ActionHostError>;
}
