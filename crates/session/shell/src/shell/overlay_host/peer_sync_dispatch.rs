//! Chrome's peer-sync requests, carried out off the frame loop.
//!
//! Minting an invitation can wait seconds for a relay's answer, redeeming
//! one opens the native owner, and finishing an enrollment publishes the
//! state file. The session's network therefore carries each request out on
//! a worker of its own ([`PeerSyncWorker`]) and the frame pump applies the
//! outcome once it is back: to the invitation card, which says it is
//! working meanwhile, or to the action that asked. A bridge that answers at
//! once ([`super::PeerSyncBridge`], tests and embedders) is applied in the
//! same call.
use super::{PeerSyncBridge, PeerSyncRequest, PeerSyncResponse};
use std::{collections::BTreeMap, sync::Arc, sync::mpsc};

/// Carries out one request for the workspace that was active when it was
/// made, on whatever thread runs it.
pub type PeerSyncCall = Arc<
    dyn Fn(crate::shell::state::WorkspaceId, PeerSyncRequest) -> Result<PeerSyncResponse, String>
        + Send
        + Sync,
>;

/// A unit of work for the worker.
pub type PeerSyncJob = Box<dyn FnOnce() + Send>;

/// Runs a job off the frame loop, handing it back when it cannot.
pub type PeerSyncRunner = std::rc::Rc<dyn Fn(PeerSyncJob) -> Result<(), PeerSyncJob>>;

/// The rooms this computer holds as a site's workspace, which the person
/// can leave.
pub type SiteRooms = Arc<dyn Fn() -> Vec<crate::shell::network::SiteRoom> + Send + Sync>;

/// A network that carries requests out on a worker, never on the loop.
#[derive(Clone)]
pub struct PeerSyncWorker {
    pub call: PeerSyncCall,
    pub run: PeerSyncRunner,
    pub site_rooms: SiteRooms,
}

impl PeerSyncWorker {
    /// The session network's: each request carried out on its peer-sync
    /// worker, which wakes the loop with the outcome.
    #[must_use]
    pub fn for_network(network: &Arc<crate::shell::network::ShellNetwork>) -> Self {
        let carrier = Arc::clone(network);
        let runner = Arc::clone(network);
        let sites = Arc::clone(network);
        Self {
            call: Arc::new(move |workspace, request| carrier.peer_sync_request(workspace, request)),
            run: std::rc::Rc::new(move |job| runner.run_off_loop(job)),
            site_rooms: Arc::new(move || sites.site_room_labels()),
        }
    }
}

/// What to do with a request's outcome once it is back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum FollowUp {
    /// The invitation card's request: its outcome lands on the card.
    Card {
        request: PeerSyncRequest,
        copy_result: bool,
    },
    /// An invitation action's: its outcome is the action's answer.
    Action {
        request: PeerSyncRequest,
        request_id: Option<u64>,
    },
    /// Pairing a new device: its link, the devices asking and the answers,
    /// which the sheet and the approval notifications show.
    Pairing { request: PeerSyncRequest },
    /// The directory of public workspaces and a workspace's publication:
    /// the sheet, the switch a join leads to, and workspace settings.
    Directory { request: PeerSyncRequest },
    /// Nothing comes back that anyone reads.
    Ignore,
}

/// One request and what it needs checked before it runs.
pub(super) struct Job {
    pub(super) request: PeerSyncRequest,
    /// Resend and cancel act only on an invitation the active workspace
    /// still has pending.
    pub(super) require_pending: bool,
    /// The workspace active when the request was made, which it is for.
    pub(super) workspace: Option<crate::shell::state::WorkspaceId>,
}

type Reply = (u64, Result<PeerSyncResponse, String>);

pub(super) struct PeerSyncDispatch {
    inline: Option<PeerSyncBridge>,
    worker: Option<PeerSyncWorker>,
    next: u64,
    in_flight: BTreeMap<u64, FollowUp>,
    replies: mpsc::Sender<Reply>,
    answered: mpsc::Receiver<Reply>,
}

impl Default for PeerSyncDispatch {
    fn default() -> Self {
        let (replies, answered) = mpsc::channel();
        Self {
            inline: None,
            worker: None,
            next: 0,
            in_flight: BTreeMap::new(),
            replies,
            answered,
        }
    }
}

/// Run `job` through `call`: what both the worker and an inline bridge do.
fn execute(
    call: &dyn Fn(PeerSyncRequest) -> Result<PeerSyncResponse, String>,
    job: Job,
) -> Result<PeerSyncResponse, String> {
    if job.require_pending
        && let PeerSyncRequest::ResendInvitation(id) | PeerSyncRequest::CancelInvitation(id) =
            &job.request
    {
        let PeerSyncResponse::PendingInvitations(pending) =
            call(PeerSyncRequest::ListPendingInvitations)?
        else {
            return Err("peer-sync answered an unexpected response".into());
        };
        if !pending.iter().any(|invite| &invite.id == id) {
            return Err("invitation is no longer pending".into());
        }
    }
    call(job.request)
}

impl PeerSyncDispatch {
    pub(super) fn set_bridge(&mut self, bridge: PeerSyncBridge) {
        self.inline = Some(bridge);
        self.worker = None;
    }

    pub(super) fn set_worker(&mut self, worker: PeerSyncWorker) {
        self.worker = Some(worker);
        self.inline = None;
    }

    /// The rooms held as a site's workspace; none without a network.
    pub(super) fn site_rooms(&self) -> Vec<crate::shell::network::SiteRoom> {
        self.worker
            .as_ref()
            .map(|worker| (worker.site_rooms)())
            .unwrap_or_default()
    }

    /// Whether a request like `request` is still being carried out: the
    /// card does not send a second one meanwhile.
    pub(super) fn in_flight(&self, request: &PeerSyncRequest) -> bool {
        let kind = std::mem::discriminant(request);
        self.in_flight.values().any(|follow| match follow {
            FollowUp::Card { request, .. }
            | FollowUp::Action { request, .. }
            | FollowUp::Pairing { request }
            | FollowUp::Directory { request } => std::mem::discriminant(request) == kind,
            FollowUp::Ignore => false,
        })
    }

    /// Carry `job` out. Answers its outcome with `follow` when it is known
    /// now (an inline bridge, or no bridge at all); `None` when the worker
    /// has it, whose outcome [`Self::answered`] hands back later.
    pub(super) fn submit(
        &mut self,
        job: Job,
        follow: FollowUp,
    ) -> Option<(FollowUp, Result<PeerSyncResponse, String>)> {
        if let Some(worker) = self.worker.as_ref() {
            let ticket = self.next;
            self.next = self.next.wrapping_add(1);
            let Some(workspace) = job.workspace else {
                return Some((follow, Err("no active workspace".into())));
            };
            let call = Arc::clone(&worker.call);
            let replies = self.replies.clone();
            let work: PeerSyncJob = Box::new(move || {
                // A request that panics still answers, so nothing waits on it.
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    execute(&|request| call(workspace, request), job)
                }))
                .unwrap_or_else(|_| Err("the peer-sync request failed".into()));
                let _ = replies.send((ticket, outcome));
            });
            return match (worker.run)(work) {
                Ok(()) => {
                    self.in_flight.insert(ticket, follow);
                    None
                }
                Err(_) => Some((
                    follow,
                    Err("peer-sync is not running in this session".into()),
                )),
            };
        }
        let outcome = match self.inline.as_ref() {
            Some(bridge) => execute(&**bridge, job),
            None => Err("peer-sync is not running in this session".to_string()),
        };
        Some((follow, outcome))
    }

    /// The outcomes the worker finished since the last call, in the order
    /// it finished them.
    pub(super) fn answered(&mut self) -> Vec<(FollowUp, Result<PeerSyncResponse, String>)> {
        let mut done = Vec::new();
        while let Ok((ticket, outcome)) = self.answered.try_recv() {
            if let Some(follow) = self.in_flight.remove(&ticket) {
                done.push((follow, outcome));
            }
        }
        done
    }
}
