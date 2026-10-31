//! Putting a grant a person gives in force before the call it was given
//! for goes on. This device writes the grant into its replica of the room
//! itself, signed with its own key, and publishes it as any room change;
//! the room's admission decides it, so once the write is committed the
//! grant is in force here.
//!
//! The receipt that notes the grant in the room's history is this device's
//! own, put in its outbox once the grant is in force here and written from
//! there into its person's slice of the room, so it can never land ahead of
//! the grant it records.
use std::sync::Arc;

use super::ShellNetwork;
use super::relay_rooms::HostAskError;
use crate::shell::state::{HostAnswer, RoomId};
use tilo_action_policy::{Change, Grant, GrantId};

/// Receipts made here that the room refused, since the process started,
/// counted on each warning: this device and its own replica of the room
/// disagree, which is a fault to find.
static REFUSED_RECEIPTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl ShellNetwork {
    /// Write `grant` in `room` as this device, then note `receipt` in the
    /// room's history. Answers the grant's id.
    ///
    /// # Errors
    /// The room's admission refused the grant, or this device holds no
    /// replica of the room to write it to.
    pub(crate) async fn put_grant_in_force(
        self: Arc<Self>,
        room: RoomId,
        grant: Grant,
        receipt: tilo_desktop_model::WorkspaceEffectReceipt,
    ) -> Result<GrantId, String> {
        let id = grant.id.clone();
        let writing = Arc::clone(&self);
        tokio::task::spawn_blocking(move || {
            writing.write_governance_change(
                room,
                crate::shell::state::GovernanceRequest::Policy(Change::Grant(grant)),
            )
        })
        .await
        .map_err(|_| "writing the grant stopped")?
        .map_err(String::from)?;
        // The write projects the room into its workspace as it commits.
        if !self.grant_in_force(room, &id) {
            return Err("the grant was written but is not in force here".into());
        }
        // The grant is the call's authority, so the call goes on whatever
        // becomes of its receipt, which this device records from its
        // outbox, after the grant.
        self.tree.write().record_in_room(room, receipt);
        Ok(id)
    }

    /// Record `receipt`, this device's own, in `room`, and say how the
    /// room took it: for the outbox ([`ShellNetwork::send_outbox`]), on a
    /// worker. A refusal means this device and its replica of the room
    /// disagree, which is a fault to find, so it is counted as well as said.
    pub(super) fn record_receipt(
        &self,
        room: RoomId,
        receipt: &tilo_desktop_model::WorkspaceEffectReceipt,
    ) -> HostAnswer {
        match self.record_receipt_here(room, receipt) {
            Ok(()) => HostAnswer::Accepted,
            Err(HostAskError::Unreachable(error)) => {
                tracing::info!(%room, %error, "a receipt waits for this device's replica of the room");
                HostAnswer::Unreachable
            }
            Err(HostAskError::Refused(error)) => {
                let refused = REFUSED_RECEIPTS
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    .saturating_add(1);
                tracing::warn!(%room, %error, refused, "the room refused a receipt made here");
                HostAnswer::Refused(error)
            }
        }
    }

    /// Whether `room`'s workspace here holds the grant `id`.
    fn grant_in_force(&self, room: RoomId, id: &GrantId) -> bool {
        self.tree
            .read()
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room() == room)
            .is_some_and(|workspace| workspace.policy.grants.iter().any(|held| &held.id == id))
    }

    /// Write `receipt` into this device's person's slice of `room`, signed
    /// with its own key, and publish it as any room change. Unreachable
    /// while this device holds no replica of the room to write to yet.
    fn record_receipt_here(
        &self,
        room: RoomId,
        receipt: &tilo_desktop_model::WorkspaceEffectReceipt,
    ) -> Result<(), HostAskError> {
        let owner = self.native_owner().map_err(HostAskError::Unreachable)?;
        owner
            .with_current(room, |_| Ok(()))
            .map_err(HostAskError::Unreachable)?;
        let request = tilo_peer_state::owner_commit::OwnerCommitRequest {
            room: room.to_string(),
            operation: tilo_peer_state::owner_commit::CommitOperation::EffectReceipt {
                invocation: receipt.invocation.0,
                redacted: serde_json::to_vec(receipt)
                    .map_err(|error| HostAskError::Refused(error.to_string()))?,
            },
        };
        let key = self
            .native_signing_key_of(&owner, room)
            .map_err(HostAskError::Refused)?;
        owner
            .commit_effect_receipt(&request, &key, std::time::SystemTime::now())
            .map_err(HostAskError::Refused)?;
        self.schedule_native_owner_publication(room);
        Ok(())
    }
}
