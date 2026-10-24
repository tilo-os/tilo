//! Putting a grant a person gives in force before the call it was given
//! for goes on. A room's grants change only at its governor, its host, and
//! reach this device as the projection of the room's admitted document, so
//! a grant given here is asked of the host and waited for until the room
//! as this device holds it carries it. The wait lasts as long as the
//! room's sessions with its host do and ends with the first of: the
//! host's refusal, the grant arriving, or the last session closing.
//!
//! The receipt that notes the grant in the room's history is the room's
//! too, put in this device's outbox once the grant is in force here and
//! written by the host from there, so it can never land ahead of the grant
//! it records.
use std::sync::Arc;

use super::relay_rooms::HostAskError;
use super::{NetCmd, ShellNetwork, room_topic_id};
use crate::shell::state::{GovernanceRequest, HostAnswer, RoomId};
use tilo_action_policy::{Change, Grant, GrantId};

/// Receipts made here that their room's host refused, since the process
/// started, counted on each warning: the host and this device disagree
/// about the room, which is a fault to find.
static REFUSED_RECEIPTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Why a wait on the room's host ended without its answer.
const SESSIONS_CLOSED: &str = "the connection to the room's host was lost";

impl ShellNetwork {
    /// Have `room`'s governor grant `grant` and wait until it is in force
    /// here, then have the host note `receipt` in the room's history.
    /// Answers the grant's id.
    ///
    /// # Errors
    /// The host refused the grant, or the room's sessions with its host
    /// closed before the grant was in force here.
    pub(crate) async fn put_grant_in_force(
        self: Arc<Self>,
        room: RoomId,
        grant: Grant,
        receipt: tilo_desktop_model::WorkspaceEffectReceipt,
    ) -> Result<GrantId, String> {
        let id = grant.id.clone();
        let asking = Arc::clone(&self);
        let request = GovernanceRequest::Policy(Change::Grant(grant));
        tokio::task::spawn_blocking(move || asking.ask_governor_held(room, request))
            .await
            .map_err(|_| "the request to the room's host stopped")??;

        // Watched once the host answered, on the session it answered on (or
        // the one this device's room replica keeps with its own host): one
        // still being established at boot or after a wake counts as open.
        let (watch, mut closed) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .try_send(NetCmd::WatchHostSessions {
                ws: room_topic_id(room),
                ended: watch,
            })
            .map_err(|_| "the network is not running")?;
        // A write that changes a room's grants announces a policy change
        // (`WorkspaceTreeHandle::write`): armed before each look, so a grant
        // arriving between the look and the wait still wakes it.
        let arrived = Arc::new(tokio::sync::Notify::new());
        let wake = Arc::clone(&arrived);
        let announcements =
            tilo_identity_host::policy_change::Watch::new(move || wake.notify_one());
        loop {
            announcements.arm();
            if self.grant_in_force(room, &id) {
                break;
            }
            tokio::select! {
                () = arrived.notified() => {}
                _ = &mut closed => return Err(SESSIONS_CLOSED.into()),
            }
        }
        drop(announcements);

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

    /// Ask `room`'s governor for `request` and wait for its answer for as
    /// long as the room's sessions last: this device's own host in process,
    /// or the room's host on its session. For a worker.
    fn ask_governor_held(&self, room: RoomId, request: GovernanceRequest) -> Result<(), String> {
        let route = super::governance_requests::route(request, tilo_clock::unix_now_ms())?;
        if let Some(local) = self.local_governor.get()
            && local.governs(room)
        {
            return local.ask(room, route);
        }
        let held = self
            .administered_room(room)
            .ok_or("this device holds no session with the workspace host")?;
        self.govern_held(&held, route.action, route.arguments)
            .map(drop)
            .map_err(String::from)
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
