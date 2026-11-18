//! A member device entering the workspaces the server it serves places on it.
//!
//! A server that hosts a workspace runs the workspace's shell on the member
//! device holding its VM ([`crate::standalone::take_work`]). The member holds the
//! workspace as any member does, so its device action service admits the server's runs;
//! the first time the server asks about a workspace the member does not hold
//! yet, the member asks the server to enter it:
//!
//! 1. it asks the server's own `workspace.enter`
//!    ([`ENTER_WORKSPACE`](tilo_server_session::workspaces::ENTER_WORKSPACE)) over a
//!    session of its own, as the device its ownership certifies;
//! 2. it redeems the invitation the server answers with
//!    ([`super::Joined::join`]), which makes the workspace one it holds and
//!    syncs from then on.
//!
//! What admits the member is the server's own decision, never the ask. The
//! server answers `workspace.enter` for the workspaces it governs, to a device of
//! its own person, with an invitation that names that person only. So a
//! device of another person asking the member about a workspace it does not
//! hold gets nothing from it: the invitation is not for its person, and once
//! the member holds the workspace its action service refuses that device as it refuses
//! any stranger. A workspace no server of this member's governs is refused by
//! the server, and the member remembers the refusal for a while rather than
//! asking again for every request that names it.

use super::Joined;
use crate::standalone::take_work::Server;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tilo_desktop_model::WorkspaceId;
use tilo_peer_state::StatePeer;
use tilo_peer_state::actions::{ReadFuture, Workspaces};
use tilo_server_session::ServerClient;
use tilo_server_session::workspaces::enter_invitation;

/// How long the server is given to admit this member to a workspace it placed
/// here: the session, the ask and the invitation redeemed.
const ENTER_WITHIN: std::time::Duration = std::time::Duration::from_secs(30);
/// How long the invitation this member redeems stays redeemable.
const ENTER_INVITATION_SECONDS: u64 = 600;
/// How long a refusal keeps this member from asking the server about the same
/// workspace again.
const REFUSED_FOR: std::time::Duration = std::time::Duration::from_secs(30);
/// How many refusals are remembered before the oldest is forgotten, so nothing
/// that names many workspaces can grow what this member keeps.
const KEPT_REFUSALS: usize = 64;

/// The workspaces a member answers for, as its device action service reads them: those
/// it holds, and on a miss the one the server it serves enters it into.
pub(in crate::standalone) struct Entering {
    joined: Arc<Joined>,
    /// The server that places this member's work: the one host this member
    /// enters a workspace for.
    server: Server,
    /// One enter at a time, so two requests for a workspace the member does
    /// not hold yet make one enter.
    turn: tokio::sync::Mutex<()>,
    /// The workspaces the server did not admit this member to, and why, while
    /// it is not worth asking again.
    refused: Refusals,
}

impl Entering {
    pub(in crate::standalone) fn new(joined: Arc<Joined>, server: Server) -> Self {
        Self {
            joined,
            server,
            turn: tokio::sync::Mutex::new(()),
            refused: Refusals::default(),
        }
    }

    /// Enter `workspace`, unless the member holds it already or the server
    /// refused it a moment ago.
    async fn enter(&self, workspace: &str) -> Result<(), String> {
        let Some(workspace) = WorkspaceId::parse(workspace) else {
            return Err("the action service named no workspace".into());
        };
        if self.joined.owner.contains_workspace(workspace) {
            return Ok(());
        }
        if let Some(reason) = self.refused.reason(workspace) {
            return Err(reason);
        }
        let _turn = self.turn.lock().await;
        if self.joined.owner.contains_workspace(workspace) {
            // What another request waited for this turn entered.
            return Ok(());
        }
        let entered = match tokio::time::timeout(ENTER_WITHIN, self.ask_the_server(workspace)).await
        {
            Ok(entered) => entered,
            Err(_) => Err("the server did not admit this member in time".into()),
        };
        if let Err(reason) = &entered {
            // A member that cannot enter the workspaces its server places here
            // can do none of their work: every run for one is refused until
            // it can, so the reason belongs in the host's log and not behind
            // a debug flag.
            tracing::warn!(%workspace, %reason, "a workspace is not entered on this member");
            self.refused.remember(workspace, reason.clone());
        }
        entered
    }

    /// Ask the server to enter this member into `workspace`, and hold what it
    /// answers.
    async fn ask_the_server(&self, workspace: WorkspaceId) -> Result<(), String> {
        let client = ServerClient::connect_v2_to_own_device(
            self.joined.endpoint.clone(),
            self.server.endpoint_addr()?,
            self.joined.operator(),
            self.server.device(),
            self.joined.device_cert().clone(),
            self.joined.signer(),
            tilo_user_identity::RevocationList::new(),
        )
        .await
        .map_err(|error| format!("this member could not reach its server: {error}"))?;
        let invitation =
            enter_invitation(&client, &workspace.to_string(), ENTER_INVITATION_SECONDS).await?;
        if self.joined.join(&invitation).await? != workspace {
            return Err("the server admitted this member to another workspace".into());
        }
        Ok(())
    }
}

/// The workspaces the server refused this member, and why, while asking again
/// about them is not worth it: one answer from the server per workspace is
/// kept, so a request that names a workspace this member may not have costs
/// no ask at all.
#[derive(Default)]
struct Refusals(Mutex<BTreeMap<WorkspaceId, (Instant, String)>>);

impl Refusals {
    /// Why the server refused this member `workspace` a moment ago, if it did.
    fn reason(&self, workspace: WorkspaceId) -> Option<String> {
        self.reason_at(workspace, Instant::now())
    }

    fn reason_at(&self, workspace: WorkspaceId, now: Instant) -> Option<String> {
        let refused = self.0.lock().ok()?;
        let fresh = refused
            .get(&workspace)
            .filter(|(at, _)| now.saturating_duration_since(*at) < REFUSED_FOR)
            .map(|(_, reason)| reason.clone());
        drop(refused);
        fresh
    }

    fn remember(&self, workspace: WorkspaceId, reason: String) {
        self.remember_at(workspace, Instant::now(), reason);
    }

    fn remember_at(&self, workspace: WorkspaceId, at: Instant, reason: String) {
        let Ok(mut refused) = self.0.lock() else {
            return;
        };
        refused
            .retain(|_, (remembered, _)| at.saturating_duration_since(*remembered) < REFUSED_FOR);
        while refused.len() >= KEPT_REFUSALS {
            let Some(oldest) = refused
                .iter()
                .min_by_key(|(_, (remembered, _))| *remembered)
                .map(|(workspace, _)| *workspace)
            else {
                break;
            };
            refused.remove(&oldest);
        }
        refused.insert(workspace, (at, reason));
    }
}

impl Workspaces for Entering {
    /// What the member holds. Reading never enters a workspace: only a
    /// request the server itself signed does ([`Self::read_for`]).
    fn read<'a>(
        &'a self,
        workspace: &'a str,
        read: &'a mut (dyn FnMut(&StatePeer) + Send),
    ) -> ReadFuture<'a> {
        Box::pin(held(&self.joined, workspace, read))
    }

    /// What the member holds, and on a miss for a request the server this
    /// member serves signed, the workspace the server enters it into. A
    /// request any other device signs only reads, so no device but the
    /// server makes this member hold a workspace.
    fn read_for<'a>(
        &'a self,
        asker: [u8; 32],
        workspace: &'a str,
        read: &'a mut (dyn FnMut(&StatePeer) + Send),
    ) -> ReadFuture<'a> {
        if asker != self.server.device() {
            return self.read(workspace, read);
        }
        Box::pin(async move {
            let read: &mut (dyn FnMut(&StatePeer) + Send) = read;
            if held(&self.joined, workspace, &mut *read).await {
                return true;
            }
            if let Err(reason) = self.enter(workspace).await {
                tracing::debug!(%workspace, %reason, "a workspace's request is refused here");
                return false;
            }
            held(&self.joined, workspace, &mut *read).await
        })
    }
}

/// Whether this member holds `workspace`, reading what it holds out of the
/// store it keeps it in.
async fn held(joined: &Joined, workspace: &str, read: &mut (dyn FnMut(&StatePeer) + Send)) -> bool {
    Workspaces::read(&joined.owner, workspace, read).await
}

#[cfg(test)]
#[path = "joined_enter_tests.rs"]
mod tests;
