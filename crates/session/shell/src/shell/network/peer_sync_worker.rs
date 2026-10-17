//! The worker chrome's peer-sync requests run on, never the frame loop.
//!
//! Minting an invitation can wait for a relay's answer (seconds when the
//! relay is slow or unreachable), redeeming one opens the native owner,
//! and finishing an enrollment installs the room and publishes the state
//! file. The compositor's overlay host hands each such request here
//! ([`ShellNetwork::run_off_loop`], [`ShellNetwork::peer_sync_request`])
//! and applies its outcome once the worker wakes the loop. Shared rooms
//! whose enrollment completed are installed here too, at the frame loop's
//! prompting ([`ShellNetwork::frame_tick`]). One thread runs them all, in
//! the order they came, so enrollments are finished by one writer at a
//! time.
use super::ShellNetwork;
use crate::shell::{
    peer_sync_overlay::{PeerSyncRequest, PeerSyncResponse, PendingInvitationSummary},
    state::WorkspaceId,
};
use std::sync::{
    Arc, Mutex, OnceLock, PoisonError,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

/// How often the frame loop asks for completed shared-room enrollments to
/// be installed, while any is pending.
const SHARED_ROOMS_EVERY: Duration = Duration::from_millis(200);

/// How long an invite minted from the invitation card stays valid.
const INVITE_EXPIRY_SECONDS: u64 = 3600;

/// One unit of work for the worker.
pub(crate) type Job = Box<dyn FnOnce() + Send>;

enum Work {
    Job(Job),
    FinishSharedRooms,
}

#[derive(Default)]
pub(super) struct State {
    worker: OnceLock<mpsc::Sender<Work>>,
    /// A shared-room pass is queued and has not run yet.
    shared_rooms_queued: Arc<AtomicBool>,
    shared_rooms_asked: Mutex<Option<Instant>>,
}

impl ShellNetwork {
    /// Start the worker peer-sync requests run on, once. It holds the
    /// network weakly, and ends with it.
    pub(crate) fn start_peer_sync_worker(self: &Arc<Self>) {
        if self.peer_sync_worker.worker.get().is_some() {
            return;
        }
        let (sender, work) = mpsc::channel::<Work>();
        let network = Arc::downgrade(self);
        let queued = Arc::clone(&self.peer_sync_worker.shared_rooms_queued);
        let wake = Arc::clone(&self.event_wake);
        let spawned = std::thread::Builder::new()
            .name("peer-sync-worker".into())
            .spawn(move || {
                // Ends once the network, which holds the only sender, is gone.
                while let Ok(item) = work.recv() {
                    let Some(network) = network.upgrade() else {
                        break;
                    };
                    // A panicking request must not take later ones with it.
                    let ran =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match item {
                            Work::Job(job) => job(),
                            Work::FinishSharedRooms => {
                                queued.store(false, Ordering::Release);
                                network.finish_shared_rooms();
                            }
                        }));
                    if ran.is_err() {
                        tracing::error!("a peer-sync request panicked on its worker");
                    }
                    drop(network);
                    // The loop applies what the request answered.
                    if let Some(wake) = wake.get() {
                        wake();
                    }
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "cannot start the peer-sync worker");
            return;
        }
        // Only the loop starts it, so nothing races this.
        let _ = self.peer_sync_worker.worker.set(sender);
    }

    /// Carry out one of chrome's peer-sync requests for `workspace`: on the
    /// peer-sync worker, as it waits on relays and the disk.
    pub(crate) fn peer_sync_request(
        &self,
        workspace: WorkspaceId,
        request: PeerSyncRequest,
    ) -> Result<PeerSyncResponse, String> {
        match request {
            PeerSyncRequest::PrepareInvitation => Ok(self.prepare_invitation()),
            PeerSyncRequest::ExportIdentity => self
                .person_identity_card(workspace)
                .map(PeerSyncResponse::Message),
            PeerSyncRequest::IssueInviteFor {
                name,
                role,
                recipient_user_public_key,
            } => self
                .issue_invite_bound_with_role(
                    workspace,
                    INVITE_EXPIRY_SECONDS,
                    &name,
                    recipient_user_public_key,
                    role,
                )
                .map(PeerSyncResponse::Message),
            PeerSyncRequest::IssueInvitation(terms) => self
                .issue_invitation(workspace, &terms)
                .map(PeerSyncResponse::Message),
            PeerSyncRequest::AcceptInvite(text) => self.join_from_field(workspace, &text),
            PeerSyncRequest::LeaveSite(room) => self
                .leave_site(room)
                .map(|()| PeerSyncResponse::Message(room.to_string())),
            PeerSyncRequest::InviteDevice => {
                self.invite_device()
                    .map(|invite| PeerSyncResponse::PairingInvite {
                        link: invite.link,
                        expires_at_ms: invite.expires_at_ms,
                    })
            }
            PeerSyncRequest::DevicesAsking => self.devices_asking().map(|asking| {
                PeerSyncResponse::DevicesAsking(
                    asking
                        .into_iter()
                        .map(
                            |device| crate::shell::peer_sync_overlay::DeviceAskingSummary {
                                digest: device.digest,
                                label: device.label,
                                code: device.code,
                                requested_at_ms: device.requested_at_ms,
                                country: device.country,
                            },
                        )
                        .collect(),
                )
            }),
            PeerSyncRequest::AnswerDevice { digest, approve } => self
                .answer_device(digest, approve)
                .map(|()| PeerSyncResponse::Message(String::new())),
            PeerSyncRequest::PollEnrollment(id) => self.poll_enrollment(id),
            PeerSyncRequest::CancelEnrollment(id) => self.cancel_enrollment(id),
            PeerSyncRequest::ListPendingInvitations => {
                let room = self
                    .replica
                    .lock()
                    .map_err(|_| "peer-sync replica lock poisoned".to_string())?
                    .tree
                    .read()
                    .room_of(workspace)
                    .ok_or_else(|| "no active workspace".to_string())?;
                Ok(PeerSyncResponse::PendingInvitations(
                    self.pending_invitations(room)
                        .into_iter()
                        .map(|invite| PendingInvitationSummary {
                            id: invite.id,
                            name: invite.name,
                            role: invite.role,
                            expiry_unix_seconds: invite.expiry_unix_seconds,
                        })
                        .collect(),
                ))
            }
            PeerSyncRequest::ResendInvitation(id) => {
                self.resend_invitation(&id).map(PeerSyncResponse::Message)
            }
            PeerSyncRequest::CancelInvitation(id) => {
                if self.cancel_invitation(&id) {
                    Ok(PeerSyncResponse::Message("canceled".into()))
                } else {
                    Err("invitation is no longer pending".into())
                }
            }
        }
    }

    /// Run `job` on the peer-sync worker; handed back when there is none.
    pub(crate) fn run_off_loop(&self, job: Job) -> Result<(), Job> {
        let Some(worker) = self.peer_sync_worker.worker.get() else {
            return Err(job);
        };
        match worker.send(Work::Job(job)) {
            Err(mpsc::SendError(Work::Job(job))) => Err(job),
            Ok(()) | Err(mpsc::SendError(Work::FinishSharedRooms)) => Ok(()),
        }
    }

    /// Install the shared rooms whose enrollment completed, for the frame
    /// loop: installing one publishes the state file, so it runs on the
    /// peer-sync worker, asked at most every [`SHARED_ROOMS_EVERY`]. A
    /// network the session started no worker for installs them in place.
    pub(super) fn finish_shared_rooms_off_loop(&self) {
        let Some(worker) = self.peer_sync_worker.worker.get() else {
            self.finish_shared_rooms();
            return;
        };
        if !self.shared_rooms_pending()
            || self
                .peer_sync_worker
                .shared_rooms_queued
                .load(Ordering::Acquire)
        {
            return;
        }
        let now = Instant::now();
        {
            let mut asked = self
                .peer_sync_worker
                .shared_rooms_asked
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if asked.is_some_and(|last| now.saturating_duration_since(last) < SHARED_ROOMS_EVERY) {
                return;
            }
            *asked = Some(now);
        }
        self.peer_sync_worker
            .shared_rooms_queued
            .store(true, Ordering::Release);
        if worker.send(Work::FinishSharedRooms).is_err() {
            self.peer_sync_worker
                .shared_rooms_queued
                .store(false, Ordering::Release);
        }
    }
}
