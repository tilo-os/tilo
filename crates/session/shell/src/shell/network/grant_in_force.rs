//! Putting a grant a person gives in force before the call it was given
//! for goes on. This device writes the grant into its replica of the room
//! itself, signed with its own key, and publishes it as any room change;
//! the room's admission decides it, so once the write is committed the
//! grant is in force here.
//!
//! The receipt that notes the grant in the room's history is the room's
//! governance, put in this device's outbox once the grant is in force here
//! and written by the room's host from there, so it can never land ahead of
//! the grant it records.
use std::sync::Arc;

use super::ShellNetwork;
use super::relay_rooms::HostAskError;
use crate::shell::state::{HostAnswer, RoomId};
use tilo_action_policy::{Change, Grant, GrantId};

/// Receipts made here that their room's host refused, since the process
/// started, counted on each warning: the host and this device disagree
/// about the room, which is a fault to find.
static REFUSED_RECEIPTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl ShellNetwork {
    /// Write `grant` in `room` as this device, then have the room's host
    /// note `receipt` in the room's history. Answers the grant's id.
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
        // becomes of its receipt, which the room's host is asked to record
        // from this device's outbox, after the grant.
        self.tree.write().record_in_room(room, receipt);
        Ok(id)
    }

    /// Have `room`'s host record `receipt`, and say how it answered: for
    /// the outbox ([`ShellNetwork::send_outbox`]), on a worker. A refusal
    /// means the host and this device disagree about the room, which is a
    /// fault to find, so it is counted as well as said.
    pub(super) fn record_receipt(
        &self,
        room: RoomId,
        receipt: &tilo_desktop_model::WorkspaceEffectReceipt,
    ) -> HostAnswer {
        match self.record_receipt_at_host(room, receipt) {
            Ok(()) => HostAnswer::Accepted,
            Err(HostAskError::Unreachable(error)) => {
                tracing::info!(%room, %error, "a receipt waits for the room's host");
                HostAnswer::Unreachable
            }
            Err(HostAskError::Refused(error)) => {
                let refused = REFUSED_RECEIPTS
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    .saturating_add(1);
                tracing::warn!(%room, %error, refused, "the room's host refused a receipt");
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

    /// Have `room`'s host record `receipt`, this device's own: in process
    /// when the host is this device's, on the room's session otherwise.
    /// For a worker.
    fn record_receipt_at_host(
        &self,
        room: RoomId,
        receipt: &tilo_desktop_model::WorkspaceEffectReceipt,
    ) -> Result<(), HostAskError> {
        if let Some(local) = self.local_governor.get()
            && local.governs(room)
        {
            return local
                .record_receipt(room, receipt)
                .map_err(HostAskError::Refused);
        }
        self.record_receipt_held(room, receipt)
    }
}
