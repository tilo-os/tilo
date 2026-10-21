//! A workspace joined through its host is its room from the first moment.
//!
//! A join (a site's door, a link, an invitation, a host's room) knows the
//! room it joins before it talks to the host: the door's or the invitation's
//! grant names it. The workspace is made for that room at once
//! ([`WorkspaceTree::join_native_room`]) and says Joining… until the room's
//! replica installs into it; nothing is written into it meanwhile. A join
//! that fails leaves that workspace, saying why, and the person tries again
//! from it ([`ShellNetwork::retry_join`]): with the proof the host already
//! admitted when one is kept, through the site's door when the room is a
//! site's. A room whose host stopped admitting this device's person says it
//! needs joining again, and is joined again only when the person asks.
//!
//! [`WorkspaceTree::join_native_room`]: crate::shell::state::WorkspaceTree::join_native_room

use super::hosted_sites::{HostHttp, JoinProgress, SiteJoinState};
use super::{RoomId, ShellNetwork};
use crate::network::invite::enrollment_driver::Via;
use crate::shell::peer_sync_overlay::PeerSyncResponse;
use crate::shell::state::WorkspaceId;

impl ShellNetwork {
    /// The workspace and invitation of `room`'s retained enrollment: one
    /// whose exchange or installation failed after the host admitted its
    /// proof. Trying again redeems exactly that invitation into that
    /// workspace; a fresh one never matches the proof it keeps, which the
    /// host resolves idempotently (or answers that this device is a member
    /// already, [`tilo_peer_state::join::JOIN_COMPLETED_REFRESH`]).
    pub(super) fn retained_join(
        &self,
        room: RoomId,
    ) -> Result<Option<(WorkspaceId, String)>, String> {
        let Some(pending) = self.native_owner()?.pending_enrollment(room)? else {
            return Ok(None);
        };
        let Some(local) = pending.target_workspace() else {
            return Ok(None);
        };
        if pending.status() != tilo_joined_room::PendingEnrollmentStatus::Active {
            return Ok(None);
        }
        let intent =
            tilo_joined_room::VerifiedNativeEnrollmentIntent::restore(pending.intent_bytes())?;
        Ok(Some((
            local,
            tilo_peer_state::join::encode_invitation_text(
                intent.verified_join().signed_invitation(),
            ),
        )))
    }

    /// The way to `room`'s host: its HTTPS routes when it is held through
    /// the web, else the iroh address its invitation names.
    fn route_to(&self, room: RoomId) -> Result<Via, String> {
        match self.hosted_sites.origin(room) {
            Some(origin) => Ok(Via::Https {
                http: HostHttp::new(&origin)?,
                door: None,
            }),
            None => Ok(Via::Iroh),
        }
    }

    /// Try again to join `room`, whose workspace says it was not joined or
    /// needs joining again: the one way back the person has, from that
    /// workspace. Blocks on the host: call it off the frame loop.
    ///
    /// # Errors
    ///
    /// Why it was not joined, in the person's words.
    pub(crate) fn retry_join(&self, room: RoomId) -> Result<PeerSyncResponse, String> {
        let tree = self
            .replica
            .lock()
            .map_err(|_| "the workspaces are unavailable")?
            .tree
            .clone();
        let needs_join_again = self
            .hosted_sites
            .join_of(room)
            .is_some_and(|join| join.progress == JoinProgress::NeedsJoinAgain);
        if tree.read().holds_room(room) && !needs_join_again {
            return Ok(PeerSyncResponse::Message(room.to_string()));
        }
        if let Some(enrollment) = self.enrolling_into(room) {
            return Ok(PeerSyncResponse::EnrollmentPending { id: enrollment });
        }
        if needs_join_again {
            return self
                .join_site_again(room)
                .map(|()| PeerSyncResponse::Message(room.to_string()));
        }
        if let Some((local, invitation)) = self.retained_join(room)? {
            let via = self.route_to(room)?;
            self.hosted_sites.set_join(SiteJoinState {
                room,
                site: self
                    .hosted_sites
                    .hosted(room)
                    .and_then(|hosted| hosted.site),
                workspace: Some(local),
                progress: JoinProgress::Joining,
            });
            tracing::info!(%room, "joining a workspace again with its retained proof");
            let response = self.accept_invitation_via(local, &invitation, via);
            return match response {
                Ok(PeerSyncResponse::EnrollmentPending { id }) => {
                    self.finish_when_enrolled(room, id, true)?;
                    Ok(PeerSyncResponse::EnrollmentPending { id })
                }
                Ok(response) => Ok(response),
                Err(error) => {
                    self.enrollment_ended(room, local, Err(error.clone()));
                    Err(error)
                }
            };
        }
        if let Some(site) = self.site_of_room(room) {
            return self
                .rejoin_site(&site)
                .map(|joined| match joined {
                    super::site_join::SiteJoin::Held { room } => {
                        PeerSyncResponse::Message(room.to_string())
                    }
                    super::site_join::SiteJoin::Enrolling { enrollment, .. } => {
                        PeerSyncResponse::EnrollmentPending { id: enrollment }
                    }
                })
                .map_err(|error| error.to_string());
        }
        Err(crate::shell::i18n::t(
            "workspace-join-again-needs-invitation",
        ))
    }

    /// Stop holding `room` here: its workspace and this device's replica of
    /// it go together, with any join into it still under way
    /// ([`crate::shell::native_workspace_owner::release_workspace_room`]).
    /// The room itself lives on with its governor and its other members.
    ///
    /// # Errors
    ///
    /// It is the last workspace, or the room's record cannot be changed.
    pub(crate) fn release_room(&self, room: RoomId) -> Result<(), String> {
        if let Some(enrollment) = self.enrolling_into(room)
            && let Err(error) = self.cancel_enrollment(enrollment)
        {
            tracing::debug!(%room, %error, "a join into a room let go was over already");
        }
        let tree = self
            .replica
            .lock()
            .map_err(|_| "the workspaces are unavailable")?
            .tree
            .clone();
        crate::shell::native_workspace_owner::release_workspace_room(&tree, room).map(drop)
    }

    /// The host of `room`, a room this device holds, stopped admitting this
    /// device's person (a public room prunes the guests it has not seen for
    /// long). The workspace says it needs joining again; nothing is asked of
    /// the host until the person does ([`Self::retry_join`]).
    pub(super) fn room_not_admitted(&self, room: RoomId) {
        let workspace = self.replica.lock().ok().and_then(|replica| {
            replica
                .tree
                .read()
                .desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.room() == room)
                .map(|workspace| workspace.id)
        });
        if workspace.is_none() {
            return;
        }
        let already = self
            .hosted_sites
            .join_of(room)
            .is_some_and(|join| join.progress == JoinProgress::NeedsJoinAgain);
        if already {
            return;
        }
        tracing::warn!(%room, "the workspace's host no longer admits this person; it needs joining again");
        self.hosted_sites.set_join(SiteJoinState {
            room,
            site: self
                .hosted_sites
                .hosted(room)
                .and_then(|hosted| hosted.site),
            workspace,
            progress: JoinProgress::NeedsJoinAgain,
        });
    }
}

#[cfg(test)]
#[path = "joining_tests.rs"]
mod tests;
