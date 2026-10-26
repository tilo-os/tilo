//! The worker chrome's peer-sync requests run on, never the frame loop.
//!
//! Minting an invitation can wait for a relay's answer (seconds when the
//! relay is slow or unreachable), redeeming one opens the native owner,
//! and finishing an enrollment installs the room and publishes the state
//! file. The compositor's overlay host hands each such request here
//! ([`ShellNetwork::run_off_loop`], [`ShellNetwork::peer_sync_request`])
//! and applies its outcome once the worker wakes the loop. Shared rooms
//! whose enrollment completed are installed here too, once one lands
//! ([`ShellNetwork::frame_tick`] on the wake it raises), and every sync
//! round, whose
//! requests carry identity material read from the disk
//! ([`ShellNetwork::publish_sync_requests`]), and the governance changes
//! made here, each of which waits for its room's governor
//! ([`ShellNetwork::send_outbox_off_loop`]). One thread runs
//! them all, in the order they came, so enrollments are finished by one
//! writer at a time and a room's changes reach its governor in order.
use super::ShellNetwork;
use crate::shell::{
    peer_sync_overlay::{PeerSyncRequest, PeerSyncResponse, PendingInvitationSummary},
    state::WorkspaceId,
};
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

/// How long an invite minted from the invitation card stays valid.
const INVITE_EXPIRY_SECONDS: u64 = crate::shell::peer_sync_overlay::INVITE_EXPIRES_IN_SECONDS;

/// One unit of work for the worker.
pub(crate) type Job = Box<dyn FnOnce() + Send>;

enum Work {
    Job(Job),
    FinishSharedRooms,
    PlaceWorkspaces,
    FollowList(crate::shell::personal_workspaces::Adoption),
    /// Enter the rooms a configured host lists (each, when `None`).
    HostRooms(Option<iroh::EndpointId>),
    SyncRound,
    /// Reconcile the person's workspace list with the tree
    /// ([`ShellNetwork::reconcile_personal_workspaces`]).
    PersonalWorkspaces,
    Outbox(Vec<crate::shell::state::Outgoing>),
    /// Ask a room's host whether it is public, for what its people see of
    /// this person ([`ShellNetwork::presence_visibility`]).
    Publication(crate::shell::state::RoomId),
    /// Ask a room's governor to register a topology namespace to this
    /// device ([`ShellNetwork::claim_namespace_now`]): it reads this
    /// device's key material, which can wait on the disk.
    ClaimNamespace(crate::shell::state::RoomId, u16),
}

impl Work {
    /// What it is, for the log; a slow request also logs its own name.
    const fn label(&self) -> &'static str {
        match self {
            Self::Job(_) => "job",
            Self::FinishSharedRooms => "shared rooms",
            Self::PlaceWorkspaces => "placement pass",
            Self::FollowList(_) => "following the person's list",
            Self::HostRooms(_) => "host rooms",
            Self::SyncRound => "sync round",
            Self::PersonalWorkspaces => "personal workspaces",
            Self::Outbox(_) => "outbox",
            Self::Publication(_) => "publication",
            Self::ClaimNamespace(..) => "namespace claim",
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
                    // What each request changes that the frame loop shows,
                    // and so whether the loop is woken after it:
                    // - a job: answers through a reply the loop polls; always.
                    // - finishing shared rooms: installs rooms (the tree) and
                    //   ends site joins and enrollments (network state); when
                    //   one finished.
                    // - a placement pass: the tree; its own gates only make
                    //   the loop ask for less, and an enrollment it starts
                    //   wakes the loop when it lands.
                    // - following the person's list, entering a host's rooms:
                    //   the tree; enrollments wake on landing.
                    // - a sync round: sends only, via the actor.
                    // - reconciling the person's workspace list: the tree,
                    //   when it wrote it.
                    // - governance and receipts: the tree when one is refused
                    //   or waits again; their gates only make the loop ask
                    //   for less, and what a governor applies arrives as an
                    //   event of its own.
                    // Any change to the tree wakes it too, and a panic does.
                    let before = network.tree.generation();
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
                                network.finish_shared_rooms()
                            }
                            Work::FollowList(adoption) => {
                                network.follow_list(adoption);
                                false
                            }
                            Work::HostRooms(host) => {
                                network.enter_configured_host_rooms(host);
                                false
                            }
                            Work::PlaceWorkspaces => {
                                network.placement_pass_started();
                                network.place_pending_workspaces();
                                false
                            }
                            Work::SyncRound => {
                                sync_queued.store(false, Ordering::Release);
                                network.publish_sync_requests_now();
                                false
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
                            Work::Outbox(entries) => {
                                network.send_outbox(&entries);
                                false
                            }
                            // Held by the network; nothing on screen waits on it.
                            Work::Publication(room) => {
                                let _ = network.publication(room);
                                false
                            }
                            Work::ClaimNamespace(room, salt) => {
                                network.claim_namespace_now(room, salt);
                                false
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
                    let changed = network.tree.generation() != before;
                    drop(network);
                    // The loop applies what the request answered or changed.
                    if (ran.unwrap_or(true) || changed)
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
                | Work::PlaceWorkspaces
                | Work::FollowList(_)
                | Work::HostRooms(_)
                | Work::SyncRound
                | Work::PersonalWorkspaces
                | Work::Outbox(_)
                | Work::Publication(_)
                | Work::ClaimNamespace(..),
            )) => Ok(()),
        }
    }

    /// Hand `entries` of the outbox to the worker, which asks their rooms'
    /// hosts; answered with them when the session started no worker.
    pub(super) fn outbox_off_loop(
        &self,
        entries: Vec<crate::shell::state::Outgoing>,
    ) -> Result<(), Vec<crate::shell::state::Outgoing>> {
        let Some(worker) = self.peer_sync_worker.worker.get() else {
            return Err(entries);
        };
        match worker.send(Work::Outbox(entries)) {
            Err(mpsc::SendError(Work::Outbox(entries))) => Err(entries),
            Ok(()) | Err(mpsc::SendError(_)) => Ok(()),
        }
    }

    /// Install the shared rooms whose enrollment completed, for the frame
    /// loop, once one landed: installing one publishes the state file, so
    /// it runs on the peer-sync worker. A network the session started no
    /// worker for installs them in place.
    pub(super) fn finish_shared_rooms_off_loop(&self) {
        if self
            .peer_sync_worker
            .shared_rooms_queued
            .load(Ordering::Acquire)
            || !self.take_shared_rooms_due()
        {
            return;
        }
        let Some(worker) = self.peer_sync_worker.worker.get() else {
            self.finish_shared_rooms();
            return;
        };
        self.peer_sync_worker
            .shared_rooms_queued
            .store(true, Ordering::Release);
        // The channel is unbounded: a send fails only once the worker, the
        // one receiver, is gone with the network, and no pass is due then.
        if worker.send(Work::FinishSharedRooms).is_err() {
            self.peer_sync_worker
                .shared_rooms_queued
                .store(false, Ordering::Release);
        }
    }

    /// Enter the rooms the person's list brought and release those it let
    /// go, on the worker, as they wait on hosts and the disk. A network the
    /// session started no worker for does it in place.
    pub(super) fn follow_list_off_loop(
        &self,
        adoption: crate::shell::personal_workspaces::Adoption,
    ) {
        if adoption.enter.is_empty() && adoption.release.is_empty() {
            return;
        }
        let unsent = match self.peer_sync_worker.worker.get() {
            Some(worker) => match worker.send(Work::FollowList(adoption)) {
                Ok(()) => return,
                Err(mpsc::SendError(work)) => work,
            },
            None => Work::FollowList(adoption),
        };
        if let Work::FollowList(adoption) = unsent {
            self.follow_list(adoption);
        }
    }

    /// Enter, on the peer-sync worker, the rooms the configured workspace
    /// host `host` (each, when `None`) lists: nothing for a session
    /// configured with none.
    pub(super) fn enter_configured_host_rooms_off_loop(&self, host: Option<iroh::EndpointId>) {
        let configured = super::configured_host_addrs();
        if configured.is_empty()
            || host.is_some_and(|host| !configured.iter().any(|addr| addr.id == host))
        {
            return;
        }
        let unsent = match self.peer_sync_worker.worker.get() {
            Some(worker) => match worker.send(Work::HostRooms(host)) {
                Ok(()) => return,
                Err(mpsc::SendError(work)) => work,
            },
            None => Work::HostRooms(host),
        };
        if let Work::HostRooms(host) = unsent {
            self.enter_configured_host_rooms(host);
        }
    }

    /// Queue a pass placing the workspaces that wait for their rooms on this
    /// device's host: whether there is a worker to run it.
    pub(super) fn queue_placements(&self) -> bool {
        self.peer_sync_worker
            .worker
            .get()
            .is_some_and(|worker| worker.send(Work::PlaceWorkspaces).is_ok())
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

    /// Ask `room`'s host whether it is public on the peer-sync worker:
    /// whether there is one to ask it.
    pub(super) fn ask_publication_off_loop(&self, room: crate::shell::state::RoomId) -> bool {
        self.peer_sync_worker
            .worker
            .get()
            .is_some_and(|worker| worker.send(Work::Publication(room)).is_ok())
    }

    /// Claim topology namespace `salt` in `room` on the peer-sync worker:
    /// whether there is one to do it.
    pub(super) fn claim_namespace_off_loop(
        &self,
        room: crate::shell::state::RoomId,
        salt: u16,
    ) -> bool {
        self.peer_sync_worker
            .worker
            .get()
            .is_some_and(|worker| worker.send(Work::ClaimNamespace(room, salt)).is_ok())
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
