//! A device's channels as it enters a workspace through its door
//! ([`tilo_relay_protocol::entering`]-style entries): before it holds anything
//! of the workspace's governance, it asks for the keys of the workspace's
//! group.
//!
//! This decides nothing the governance would: the request is one the group's
//! participants judge against the workspace's policy, as every member's, and
//! the group is followed as MLS checks it ([`ChannelPeer::follow`]).

use super::{
    Access, Arc, ChannelEncryption, ChannelId, ChannelPeer, Instant, MlsAccessRequestV1,
    REQUEST_LONGEST, WorkspaceId,
};

/// The entry's access request ([`ChannelEncryption::entry_request`]): signed already, or
/// waiting for the signature of the device's custody.
pub enum EntryRequest {
    Signed(MlsAccessRequestV1),
    Unsigned(
        ChannelId,
        MlsAccessRequestV1,
        collab_types::signer::SignFuture<'static>,
    ),
}

impl ChannelEncryption {
    /// This device's signed request for the keys of `workspace`'s own group
    /// ([`collab_types::workspace_scope`]), made before the workspace's
    /// governance names this device: its entry carries it to the host, which
    /// hands it to the group's participants as it admits the device. The
    /// device then waits for the group's Welcome as if it had published the
    /// request itself, and asks again on the usual backoff if none arrives.
    /// `None` when no secret store keeps this device's channels, when it is
    /// in the group already, or when the request could not be made.
    pub async fn ask_on_entry(
        &mut self,
        workspace: WorkspaceId,
        now: Instant,
        wall: u64,
    ) -> Option<MlsAccessRequestV1> {
        match self.entry_request(workspace, now, wall)? {
            EntryRequest::Signed(request) => Some(request),
            EntryRequest::Unsigned(scope, mut request, signing) => {
                let signature = signing
                    .await
                    .inspect_err(
                        |error| tracing::warn!(%error, "channel access request was not signed"),
                    )
                    .ok()?;
                request.signature = collab_types::Signature::from_bytes(signature);
                Some(self.entry_signed(scope, request, now))
            }
        }
    }

    /// [`Self::ask_on_entry`] up to the signature the device's custody owes: what a caller
    /// that shares this service awaits without holding it, then hands back with
    /// [`Self::entry_signed`].
    pub fn entry_request(
        &mut self,
        workspace: WorkspaceId,
        now: Instant,
        wall: u64,
    ) -> Option<EntryRequest> {
        let scope = Self::workspace_scope(workspace);
        if !self.peers.contains_key(&scope) {
            let opened = match self.open_peer(workspace, scope, now) {
                Ok(opened) => opened,
                Err(error) => {
                    tracing::warn!(%error, %workspace, "channel state could not be opened");
                    return None;
                }
            };
            if !opened {
                let store = self.store.as_ref()?;
                let peer = ChannelPeer::waiting(
                    Some(store.state.peers(workspace)),
                    scope,
                    Box::new(Arc::clone(&store.signer)),
                    &store.vault,
                )
                .inspect_err(|error| {
                    tracing::warn!(%error, %workspace, "channel state could not be created");
                })
                .ok()?;
                self.insert_peer(workspace, scope, peer, now);
            }
        }
        let store = self.store.as_ref()?;
        let entry = self.peers.get_mut(&scope)?;
        if entry.workspace != workspace || !matches!(entry.peer.access(), Access::Waiting) {
            return None;
        }
        entry.wanted = true;
        if let Some(request) = entry.request.clone() {
            return Some(EntryRequest::Signed(self.entry_signed(scope, request, now)));
        }
        let request = entry
            .peer
            .unsigned_access_request(wall, false, &store.vault)
            .inspect_err(|error| {
                tracing::warn!(%error, "channel access request could not be prepared");
            })
            .ok()??;
        let message = request.signing_bytes().ok()?;
        let signer = Arc::clone(&store.signer);
        let signing: collab_types::signer::SignFuture<'static> =
            Box::pin(async move { signer.sign(message).await });
        Some(EntryRequest::Unsigned(scope, request, signing))
    }

    /// Keep `request`, the entry's signed request for `scope`: asked now, by the entry, so the
    /// next request waits out the backoff.
    pub fn entry_signed(
        &mut self,
        scope: ChannelId,
        request: MlsAccessRequestV1,
        now: Instant,
    ) -> MlsAccessRequestV1 {
        if let Some(entry) = self.peers.get_mut(&scope) {
            entry.request = Some(request.clone());
            entry.retry.fired(now, REQUEST_LONGEST);
        }
        request
    }
}

#[cfg(test)]
mod tests {
    include!("entry_tests.rs");
}
