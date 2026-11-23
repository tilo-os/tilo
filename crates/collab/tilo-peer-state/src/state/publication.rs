//! Signing a device's own change to a record and the messages a peer starts from.

use super::{DocumentMutation, NeighbourMsg, PeerPublicationMut, StatePeer, SystemTime};
use collab_types::signer::DeviceSigner;

impl PeerPublicationMut<'_> {
    /// Sign and admit a snapshot of `record`, returning the message to publish.
    pub fn publish_document(
        self,
        app: &str,
        record: &str,
        bytes: Vec<u8>,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<NeighbourMsg, String> {
        self.signed_mutation(app, record, DocumentMutation::Snapshot(bytes), key, now)
    }

    /// Sign and admit incremental changes to `record`, returning the message to publish.
    pub fn publish_incremental(
        self,
        app: &str,
        record: &str,
        bytes: Vec<u8>,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<NeighbourMsg, String> {
        self.signed_mutation(app, record, DocumentMutation::Incremental(bytes), key, now)
    }

    /// Sign and admit `mutation` of `record`, returning the message to publish.
    pub fn signed_mutation(
        self,
        app: &str,
        record: &str,
        mutation: DocumentMutation,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<NeighbourMsg, String> {
        let mut candidate = self.peer.clone();
        let message = candidate.signed_mutation_in_place(app, record, mutation, key, now)?;
        *self.peer = candidate;
        Ok(message)
    }

    /// Signed snapshots of every admitted document.
    pub fn signed_snapshots(
        self,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<Vec<NeighbourMsg>, String> {
        let mut candidate = self.peer.clone();
        let messages = candidate.signed_snapshots_in_place(key, now)?;
        *self.peer = candidate;
        Ok(messages)
    }

    /// Every message a peer starting from nothing needs from this device: the workspace as it stands and a checkpoint of each record, each signed by `key`.
    ///
    /// # Errors
    ///
    /// A checkpoint cannot be produced.
    pub fn bootstrap_messages(
        self,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<Vec<NeighbourMsg>, String> {
        let mut candidate = self.peer.clone();
        let snapshots = candidate.signed_snapshots_in_place(key, now)?;
        let mut messages = vec![candidate.workspace_message()];
        messages.extend(snapshots);
        messages.extend(candidate.own_sealed_publications(key.device_public_key(), None));
        *self.peer = candidate;
        Ok(messages)
    }
}
