//! Exact admitted document data borrowed from a restored peer.
use super::{BoundedPeerError, PeerRead, Route, SystemTime};
use automerge::{AutoCommit, ChangeHash};

/// Current access failures are preserved separately from route absence.
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

/// Borrowed data only: the admitted document, read-only, and its heads. No
/// caller proof, owner frame, Clone or constructor is exposed.
///
/// ```
/// use tilo_peer_state::{PeerRead, PeerDocumentRead, PeerDocumentReadError};
/// fn view<'peer>(
///     peer: &PeerRead<'peer>, device: [u8; 32], now: web_time::SystemTime,
/// ) -> Result<PeerDocumentRead<'peer>, PeerDocumentReadError> {
///     peer.document_read(device, "test.app", "tab:1", now)
/// }
/// fn inspect(view: &PeerDocumentRead<'_>) {
///     let _ = (view.app(), view.record(), view.heads(), view.document());
/// }
/// ```
pub struct PeerDocumentRead<'peer> {
    route: &'peer Route,
    document: &'peer AutoCommit,
    heads: &'peer [ChangeHash],
}
impl<'peer> PeerDocumentRead<'peer> {
    pub fn document(&self) -> &'peer AutoCommit {
        self.document
    }
    pub fn heads(&self) -> &'peer [ChangeHash] {
        self.heads
    }
    pub fn app(&self) -> &str {
        &self.route.app
    }
    pub fn record(&self) -> &str {
        &self.route.record
    }
}
impl<'peer> PeerRead<'peer> {
    fn admitted_document(
        &self,
        app: &str,
        record: &str,
    ) -> Result<PeerDocumentRead<'peer>, PeerDocumentReadError> {
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
        if document.doc.has_missing_dependencies() {
            return Err(PeerDocumentReadError::Incomplete);
        }
        Ok(PeerDocumentRead {
            route,
            document: document.doc.document(),
            heads: document.doc.heads(),
        })
    }
    /// Rechecks membership and document access before looking up the exact
    /// admitted plaintext, causally complete route. The embedding owner still
    /// binds the authenticated caller, actual lock/generation and fresh clock.
    ///
    /// # Errors
    ///
    /// Returns a [`PeerDocumentReadError`] if access is denied or the route is not an exact,
    /// plaintext, causally complete admitted document.
    pub fn document_read(
        &self,
        device: [u8; 32],
        app: &str,
        record: &str,
        now: SystemTime,
    ) -> Result<PeerDocumentRead<'peer>, PeerDocumentReadError> {
        if !self
            .document_read_allowed(device, app, record, now)
            .map_err(PeerDocumentReadError::Peer)?
        {
            return Err(PeerDocumentReadError::Denied);
        }
        self.admitted_document(app, record)
    }

    /// Rechecks the named Agent's own governed permission before exposing the
    /// exact admitted plaintext route. No device or person role is borrowed.
    ///
    /// # Errors
    ///
    /// Returns a [`PeerDocumentReadError`] when the Agent is denied or the
    /// exact admitted document is missing, encrypted or causally incomplete.
    pub fn agent_document_read(
        &self,
        agent: &str,
        app: &str,
        record: &str,
        now: SystemTime,
    ) -> Result<PeerDocumentRead<'peer>, PeerDocumentReadError> {
        if !self.agent_document_read_allowed(agent, app, record, now) {
            return Err(PeerDocumentReadError::Denied);
        }
        self.admitted_document(app, record)
    }
}

#[cfg(test)]
mod tests;
