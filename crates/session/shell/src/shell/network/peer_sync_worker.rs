//! The worker chrome's peer-sync requests run on, never the frame loop.
//!
//! Minting an invitation can wait for a relay's answer (seconds when the
//! relay is slow or unreachable), redeeming one opens the native owner,
//! and finishing an enrollment installs the room and publishes the state
//! file. The compositor's overlay host hands each such request here
//! ([`ShellNetwork::run_off_loop`], [`ShellNetwork::peer_sync_request`])
//! and applies its outcome once the worker wakes the loop. Shared rooms
//! whose enrollment completed are installed here too, at the frame loop's
//! prompting ([`ShellNetwork::frame_tick`]), and every sync round, whose
//! requests carry identity material read from the disk
//! ([`ShellNetwork::publish_sync_requests`]), and peers' batches for the
//! rooms, whose admission decodes them in a worker process and copies the
//! room's document ([`ShellNetwork::merge_room_batch_off_loop`]), and the
//! reconcile of the person's workspace list with the tree, which waits for
//! the tree ([`ShellNetwork::reconcile_personal_workspaces`]). One thread
//! runs them all, in the order they came, so enrollments are finished by
//! one writer at a time and a room's batches merge in the order they came.
use super::ShellNetwork;
use crate::shell::{
    peer_sync_overlay::{PeerSyncRequest, PeerSyncResponse, PendingInvitationSummary},
    state::{RoomId, WorkspaceId},
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
const INVITE_EXPIRY_SECONDS: u64 = crate::shell::peer_sync_overlay::INVITE_EXPIRES_IN_SECONDS;

/// One unit of work for the worker.
pub(crate) type Job = Box<dyn FnOnce() + Send>;

enum Work {
    Job(Job),
    FinishSharedRooms,
    SyncRound,
    PersonalWorkspaces,
    RoomBatch {
        peer: iroh::EndpointId,
        room: RoomId,
        changes: Vec<Vec<u8>>,
    },
}

impl Work {
    /// What it is, for the log; a slow request also logs its own name.
    const fn label(&self) -> &'static str {
        match self {
            Self::Job(_) => "job",
            Self::FinishSharedRooms => "shared rooms",
            Self::SyncRound => "sync round",
            Self::PersonalWorkspaces => "personal workspaces",
            Self::RoomBatch { .. } => "room batch",
        }
    }
}

/// How long one item may hold the serial worker before it is logged.
const SLOW_WORK: Duration = Duration::from_secs(5);

#[derive(Default)]
pub(super) struct State {
    worker: OnceLock<mpsc::Sender<Work>>,
    /// A shared-room pass is queued and has not run yet.
    shared_rooms_queued: Arc<AtomicBool>,
    shared_rooms_asked: Mutex<Option<Instant>>,
    /// A sync round is queued and has not run yet: one covers every ask
    /// made before it runs.
    sync_round_queued: Arc<AtomicBool>,
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
        let sync_queued = Arc::clone(&self.peer_sync_worker.sync_round_queued);
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
                    // Each says whether the loop has anything to take from it.
                    let what = item.label();
                    let started = Instant::now();
                    let ran =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match item {
                            Work::Job(job) => {
                                job();
                                true
                            }
                            Work::FinishSharedRooms => {
                                queued.store(false, Ordering::Release);
                                network.finish_shared_rooms();
                                true
                            }
                            Work::SyncRound => {
                                sync_queued.store(false, Ordering::Release);
                                network.publish_sync_requests_now();
                                true
                            }
                            // Only a reconcile that wrote the tree has
                            // anything to draw: an idle one wakes nothing.
                            Work::PersonalWorkspaces => {
                                network
                                    .personal_reconcile
                                    .queued
                                    .store(false, Ordering::Release);
                                network.reconcile_personal_workspaces()
                            }
                            Work::RoomBatch {
                                peer,
                                room,
                                changes,
                            } => {
                                network.merge_room_batch(peer, room, &changes);
                                true
                            }
                        }));
                    if ran.is_err() {
                        tracing::error!("a peer-sync request panicked on its worker");
                    }
                    // The worker is serial: whatever runs long here holds up
                    // every join queued behind it.
                    let took = started.elapsed();
                    if took >= SLOW_WORK {
                        tracing::warn!(
                            work = what,
                            took_ms = took.as_millis(),
                            "the peer-sync worker was held; joins queued behind it waited"
                        );
                    }
                    drop(network);
                    // The loop applies what the request answered.
                    if ran.unwrap_or(true)
                        && let Some(wake) = wake.get()
                    {
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
            PeerSyncRequest::AcceptInvite(text) => self.join_from_field(&text),
            PeerSyncRequest::LeaveSite(room) => self
                .leave_site(room)
                .map(|()| PeerSyncResponse::Message(room.to_string())),
            PeerSyncRequest::RetryJoin(room) => self.retry_join(room),
            PeerSyncRequest::InviteDevice => {
                self.invite_device()
                    .map(|invite| PeerSyncResponse::PairingInvite {
                        link: invite.link,
                        expires_at_ms: invite.expires_at_ms,
                    })
            }
            PeerSyncRequest::DevicesAsking => self.devices_asking_response(),
            PeerSyncRequest::AnswerDevice { digest, approve } => {
                // A device another of the person's devices found, or one
                // asking at a host under this computer's pairing link.
                match crate::shell::device_certification::answer(self, digest, approve)? {
                    Some(_) => Ok(PeerSyncResponse::Message(String::new())),
                    None => self
                        .answer_device(digest, approve)
                        .map(|()| PeerSyncResponse::Message(String::new())),
                }
            }
            PeerSyncRequest::PollEnrollment(id) => self.poll_enrollment(id),
            PeerSyncRequest::CancelEnrollment(id) => self.cancel_enrollment(id),
            PeerSyncRequest::ListPendingInvitations => self.pending_invitations_response(workspace),
            PeerSyncRequest::Directory { site } => self
                .directory(site.as_deref())
                .map(PeerSyncResponse::Directory),
            PeerSyncRequest::JoinListed(listing) => {
                self.join_listed(&listing).map(|joined| match joined {
                    super::site_join::SiteJoin::Held { room } => {
                        PeerSyncResponse::JoinedWorkspace {
                            room,
                            workspace: None,
                        }
                    }
                    super::site_join::SiteJoin::Enrolling { room, local, .. } => {
                        PeerSyncResponse::JoinedWorkspace {
                            room,
                            workspace: Some(local),
                        }
                    }
                })
            }
            PeerSyncRequest::ReportWorkspace { room, reason } => self
                .report_workspace(room, &reason)
                .map(|()| PeerSyncResponse::Message(String::new())),
            PeerSyncRequest::Publication(room) => {
                self.publication(room).map(PeerSyncResponse::Publication)
            }
            PeerSyncRequest::Publish { room, listing } => self
                .publish(room, listing)
                .map(PeerSyncResponse::Publication),
            PeerSyncRequest::AskForBot { room, ask } => self
                .ask_for_bot(room, &ask)
                .map(|()| PeerSyncResponse::Message(String::new())),
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

    /// The devices asking to join this person, as the sheet lists them:
    /// those another of the person's devices found, and those asking at a
    /// host under this computer's pairing links.
    fn devices_asking_response(&self) -> Result<PeerSyncResponse, String> {
        use crate::shell::peer_sync_overlay::DeviceAskingSummary;
        let found: Vec<DeviceAskingSummary> = crate::shell::device_certification::asking(self)
            .into_iter()
            .map(|device| DeviceAskingSummary {
                digest: device.digest,
                label: device.label,
                code: device.code,
                requested_at_ms: device.asked_at_ms,
                country: None,
                via: Some(device.via),
            })
            .collect();
        let hosted = match self.devices_asking() {
            Ok(hosted) => hosted,
            Err(error) if !found.is_empty() => {
                tracing::debug!(%error, "the hosts' devices asking could not be read");
                Vec::new()
            }
            Err(error) => return Err(error),
        };
        Ok(PeerSyncResponse::DevicesAsking(
            found
                .into_iter()
                .chain(hosted.into_iter().map(|device| DeviceAskingSummary {
                    digest: device.digest,
                    label: device.label,
                    code: device.code,
                    requested_at_ms: device.requested_at_ms,
                    country: device.country,
                    via: None,
                }))
                .collect(),
        ))
    }

    /// The invitations of `workspace`'s room still waiting for an answer.
    fn pending_invitations_response(
        &self,
        workspace: WorkspaceId,
    ) -> Result<PeerSyncResponse, String> {
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

    /// Run `job` on the peer-sync worker; handed back when there is none.
    pub(crate) fn run_off_loop(&self, job: Job) -> Result<(), Job> {
        let Some(worker) = self.peer_sync_worker.worker.get() else {
            return Err(job);
        };
        match worker.send(Work::Job(job)) {
            Err(mpsc::SendError(Work::Job(job))) => Err(job),
            Ok(())
            | Err(mpsc::SendError(
                Work::FinishSharedRooms
                | Work::SyncRound
                | Work::PersonalWorkspaces
                | Work::RoomBatch { .. },
            )) => Ok(()),
        }
    }

    /// Merge a peer's batch for `room` on the worker, for the frame loop,
    /// which wakes once it landed. A network the session started no worker
    /// for merges it in place.
    pub(super) fn merge_room_batch_off_loop(
        &self,
        peer: iroh::EndpointId,
        room: RoomId,
        changes: Vec<Vec<u8>>,
    ) {
        let work = Work::RoomBatch {
            peer,
            room,
            changes,
        };
        let unsent = match self.peer_sync_worker.worker.get() {
            Some(worker) => match worker.send(work) {
                Ok(()) => return,
                Err(mpsc::SendError(work)) => work,
            },
            None => work,
        };
        if let Work::RoomBatch {
            peer,
            room,
            changes,
        } = unsent
        {
            self.merge_room_batch(peer, room, &changes);
        }
    }

    /// Merge a peer's batch for `room` into the tree, holding neither the
    /// tree nor the replica while it is admitted
    /// ([`crate::shell::state::WorkspaceTree::merge_room_changes`]).
    pub(super) fn merge_room_batch(
        &self,
        peer: iroh::EndpointId,
        room: RoomId,
        changes: &[Vec<u8>],
    ) {
        let origin = crate::shell::state::PeerOrigin::peer(&peer);
        let Some(tree) = self.replica.lock().ok().map(|rep| rep.tree.clone()) else {
            return;
        };
        if tree.read().requires_native_owner(room) {
            return;
        }
        let result =
            crate::shell::state::WorkspaceTree::merge_room_changes(&tree, room, changes, &origin);
        let Ok(mut rep) = self.replica.lock() else {
            return;
        };
        rep.room_batch_merged(room, &origin, changes.len(), result);
        self.finish_replica_turn(rep, Some(room));
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

    /// Ask every room this machine is in for whatever it is missing, and
    /// publish the requests. Each carries the identity the room is
    /// presented with, read from the identity directory and this device's
    /// certificates, which can wait on the disk: with the peer-sync worker
    /// running the round runs there, never on the compositor's loop.
    pub fn publish_sync_requests(&self) {
        if !self.publish_sync_requests_off_loop() {
            self.publish_sync_requests_now();
        }
    }

    /// Queue a sync round on the peer-sync worker: whether there is one to
    /// run it. A round already queued covers this ask.
    fn publish_sync_requests_off_loop(&self) -> bool {
        let Some(worker) = self.peer_sync_worker.worker.get() else {
            return false;
        };
        let queued = &self.peer_sync_worker.sync_round_queued;
        if queued.swap(true, Ordering::AcqRel) {
            return true;
        }
        if worker.send(Work::SyncRound).is_err() {
            queued.store(false, Ordering::Release);
            return false;
        }
        true
    }

    /// Queue a reconcile of the person's workspace list on the peer-sync
    /// worker: whether there is one to run it. A reconcile already queued
    /// covers this ask.
    pub(super) fn reconcile_personal_workspaces_off_loop(&self) -> bool {
        let Some(worker) = self.peer_sync_worker.worker.get() else {
            return false;
        };
        let queued = &self.personal_reconcile.queued;
        if queued.swap(true, Ordering::AcqRel) {
            return true;
        }
        if worker.send(Work::PersonalWorkspaces).is_err() {
            queued.store(false, Ordering::Release);
            return false;
        }
        true
    }
}
