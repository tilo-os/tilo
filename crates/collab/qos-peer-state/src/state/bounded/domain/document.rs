//! Exact admitted document data borrowed from one current native work turn.
use super::*;
use automerge::ChangeHash;
use automerge::resource_limits::{NativeObject, NativeReadSession};

/// Current access/native failures are preserved separately from route absence.
/// These are observations; the embedding owner authenticates caller and time.
#[derive(Debug, PartialEq, Eq)]
pub enum PeerDocumentReadError {
    Peer(BoundedPeerError),
    Denied,
    Missing,
    Encrypted,
    Incomplete,
}
impl std::fmt::Display for PeerDocumentReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "document read refusal: {self:?}")
    }
}
impl std::error::Error for PeerDocumentReadError {}

/// Borrowed data only. No raw document, caller proof, owner frame, Clone or
/// constructor is exposed. The native session remains tied to peer and work.
/// Complete heads are copied into a fixed inline frontier; no unbounded output
/// collection is allocated and no caller-supplied heads are accepted.
///
/// The public types resolve and preserve the actual peer/work lifetimes:
/// ```
/// use qos_peer_state::{PeerRead, PeerDocumentRead, PeerDocumentReadError};
/// fn view<'peer, 'work>(
///     peer: &PeerRead<'peer, 'work>, device: [u8; 32], now: web_time::SystemTime,
/// ) -> Result<PeerDocumentRead<'peer, 'work>, PeerDocumentReadError> {
///     peer.document_read(device, "test.app", "tab:1", now)
/// }
/// fn inspect(view: &PeerDocumentRead<'_, '_>) {
///     let _ = (view.app(), view.record(), view.heads(), view.root());
///     let _ = view.read().pending_ops();
/// }
/// ```
/// The current work borrow cannot be promoted to a lasting session:
/// ```compile_fail
/// use qos_peer_state::{PeerRead, PeerDocumentRead};
/// fn escape<'peer, 'work>(
///     peer: &PeerRead<'peer, 'work>, device: [u8; 32], now: web_time::SystemTime,
/// ) -> PeerDocumentRead<'peer, 'static> {
///     peer.document_read(device, "test.app", "tab:1", now).unwrap()
/// }
/// ```
/// Nor can the actual peer lifetime be extended:
/// ```compile_fail
/// use qos_peer_state::{PeerRead, PeerDocumentRead};
/// fn escape<'peer, 'work>(
///     peer: &PeerRead<'peer, 'work>, device: [u8; 32], now: web_time::SystemTime,
/// ) -> PeerDocumentRead<'static, 'work> {
///     peer.document_read(device, "test.app", "tab:1", now).unwrap()
/// }
/// ```
/// A native visitor session never exposes a raw document:
/// ```compile_fail
/// use qos_peer_state::PeerDocumentRead;
/// fn raw<'a, 'peer, 'work>(view: &'a PeerDocumentRead<'peer, 'work>)
///     -> &'a automerge::AutoCommit {
///     view.read()
/// }
/// ```
pub struct PeerDocumentRead<'peer, 'work> {
    route: &'peer Route,
    read: NativeReadSession<'peer, 'work>,
    heads: [ChangeHash; 64],
    heads_len: usize,
}
impl<'peer, 'work> PeerDocumentRead<'peer, 'work> {
    pub fn read(&self) -> &NativeReadSession<'peer, 'work> {
        &self.read
    }
    pub fn root(&self) -> NativeObject<'peer> {
        self.read.root()
    }
    pub fn heads(&self) -> &[ChangeHash] {
        &self.heads[..self.heads_len]
    }
    pub fn app(&self) -> &str {
        &self.route.app
    }
    pub fn record(&self) -> &str {
        &self.route.record
    }
}
impl<'peer, 'work> PeerRead<'peer, 'work> {
    /// Rechecks membership and document access before looking up the exact
    /// admitted plaintext, causally complete route. The embedding owner still
    /// binds the authenticated caller, actual lock/generation and fresh clock.
    pub fn document_read(
        &self,
        device: [u8; 32],
        app: &str,
        record: &str,
        now: SystemTime,
    ) -> Result<PeerDocumentRead<'peer, 'work>, PeerDocumentReadError> {
        if !self
            .document_read_allowed(device, app, record, now)
            .map_err(PeerDocumentReadError::Peer)?
        {
            return Err(PeerDocumentReadError::Denied);
        }
        let (route, document) = self
            .peer
            .core
            .documents
            .iter()
            .find(|(route, _)| route.app == app && route.record == record)
            .ok_or(PeerDocumentReadError::Missing)?;
        if document.encryption_required {
            return Err(PeerDocumentReadError::Encrypted);
        }
        let read = document
            .doc
            .read(self._work)
            .map_err(|error| PeerDocumentReadError::Peer(BoundedPeerError::Resource(error)))?;
        if document.doc.has_missing_dependencies() || read.pending_ops() != 0 {
            return Err(PeerDocumentReadError::Incomplete);
        }
        let mut heads = [ChangeHash([0; 32]); 64];
        let mut heads_len = 0;
        // The native visitor counts the WHOLE frontier before any callback.
        // The document is immutable throughout this check and copy.
        read.visit_heads(heads.len(), |hash| {
            heads[heads_len] = hash;
            heads_len += 1;
        })
        .map_err(|error| PeerDocumentReadError::Peer(BoundedPeerError::Resource(error)))?;
        Ok(PeerDocumentRead {
            route,
            read,
            heads,
            heads_len,
        })
    }
}

#[cfg(test)]
mod tests;
