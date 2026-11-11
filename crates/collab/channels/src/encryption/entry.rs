//! A device's channels as it enters a workspace through its door
//! ([`tilo_relay_protocol::entering`]-style entries): before it holds anything
//! of the workspace's governance, it asks for the keys of the workspace's own
//! group, and learns which group seals a record from its host's answer.
//!
//! Neither decides anything the governance would: the request is one the
//! group's participants judge against the workspace's policy, as every
//! member's, and a group this device follows before it can name the channel's
//! readers is followed as MLS checks it, as any whose record it cannot name
//! ([`ChannelPeer::follow`]). The scope the host named for a record holds only
//! until the governance here names one itself.

use super::{
    Access, Arc, ChannelEncryption, ChannelId, ChannelPeer, ChannelRoute, Instant,
    MlsAccessRequestV1, REQUEST_LONGEST, WorkspaceId,
};

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
        let request = if let Some(request) = entry.request.clone() {
            request
        } else {
            let mut request = entry
                .peer
                .unsigned_access_request(wall, false, &store.vault)
                .inspect_err(|error| {
                    tracing::warn!(%error, "channel access request could not be prepared");
                })
                .ok()??;
            let message = request.signing_bytes().ok()?;
            let signature = store
                .signer
                .sign(message)
                .await
                .inspect_err(
                    |error| tracing::warn!(%error, "channel access request was not signed"),
                )
                .ok()?;
            request.signature = collab_types::Signature::from_bytes(signature);
            request
        };
        // Asked now, by the entry: the next request waits out the backoff.
        if let Some(entry) = self.peers.get_mut(&scope) {
            entry.request = Some(request.clone());
            entry.retry.fired(now, REQUEST_LONGEST);
        }
        Some(request)
    }

    /// Take `scope` as the group sealing `route`'s record in `workspace`,
    /// as the workspace's host named it, until the governance here names one
    /// ([`super::ChannelDirectory::unit_scope`]). Only the workspace's own
    /// group or the record's own channel can be named.
    pub fn scope_told(&mut self, workspace: WorkspaceId, route: &ChannelRoute, scope: ChannelId) {
        let unit = route.channel(&workspace.to_string());
        if scope != unit && scope != Self::workspace_scope(workspace) {
            return;
        }
        self.told.insert(unit, scope);
        self.bump(unit);
    }
}

#[cfg(test)]
mod tests {
    include!("entry_tests.rs");
}
