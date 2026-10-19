//! Joining the home workspace of a site as this computer's person: through
//! the site's admission door, then the host's workspace join exchange over
//! HTTPS ([`super::hosted_sites`]).

use super::hosted_sites::{self, HostHttp, HostedRoom, SiteError, SiteWorkspace};
use super::{RoomId, ShellNetwork};
use crate::network::invite::enrollment_driver::Via;
use crate::shell::peer_sync_overlay::PeerSyncResponse;
use crate::shell::state::WorkspaceId;

/// A room held through the web, as the switcher shows it: which site it is
/// the workspace of, and whether that site is this computer's community.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SiteRoom {
    pub room: RoomId,
    /// The site's host name (`tilo.coffee`); none for a room an invitation
    /// link named.
    pub site: Option<String>,
    pub community: bool,
    /// Joined from a directory of public workspaces: its door takes
    /// reports.
    pub public: bool,
}

/// Where joining a site's workspace stands once it was asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SiteJoin {
    /// The workspace is held here already.
    Held {
        /// Its room.
        room: RoomId,
    },
    /// The host admitted this device; the workspace appears once its
    /// enrollment is installed.
    Enrolling {
        /// Its room.
        room: RoomId,
        /// The workspace slot it is installed into.
        local: WorkspaceId,
        /// The enrollment, which [`ShellNetwork::poll_enrollment`] installs.
        enrollment: u64,
    },
}

impl ShellNetwork {
    /// The person this computer acts as, and the name they give a room.
    pub(super) fn site_person(&self) -> Result<([u8; 32], String), SiteError> {
        let identity = self
            .host_identity
            .as_ref()
            .ok_or_else(|| SiteError::Refused("this computer has no identity yet".into()))?;
        let (certificate, _) = identity
            .current_native_material(0)
            .map_err(SiteError::Refused)?;
        let name = ui::local_identity::load()
            .map(|identity| identity.display_name)
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| certificate.device_label.clone());
        let name = tilo_user_identity::display_name(&name)
            .map(str::to_owned)
            .unwrap_or_default();
        Ok((certificate.user_public_key, name))
    }

    /// Join the home workspace of the site at `entry` as this computer's
    /// person. Blocks on the site and its door: call it off the frame loop.
    ///
    /// # Errors
    ///
    /// Why the workspace was not joined: the site names none or lets people
    /// in by invitation only, its door refused, or it was not reached.
    pub(crate) fn join_site(&self, entry: &url::Url) -> Result<SiteJoin, SiteError> {
        self.join_site_with(entry, true)
    }

    /// [`Self::join_site`] for a caller that installs the enrollment itself
    /// ([`Self::poll_enrollment`]), as the join field does to show it.
    pub(crate) fn join_site_now(&self, entry: &url::Url) -> Result<SiteJoin, SiteError> {
        self.join_site_with(entry, false)
    }

    fn join_site_with(&self, entry: &url::Url, background: bool) -> Result<SiteJoin, SiteError> {
        let runtime = self.runtime_handle().map_err(SiteError::Unreachable)?;
        let site = runtime.block_on(hosted_sites::resolve_site(entry))?;
        self.join_site_workspace(&site, background)
    }

    /// Join the workspace a site's entry named; its enrollment installs in
    /// the `background`, else by its caller.
    pub(super) fn join_site_workspace(
        &self,
        site: &SiteWorkspace,
        background: bool,
    ) -> Result<SiteJoin, SiteError> {
        let runtime = self.runtime_handle().map_err(SiteError::Unreachable)?;
        let tree = self
            .replica
            .lock()
            .map_err(|_| SiteError::Refused("the workspaces are unavailable".into()))?
            .tree
            .clone();
        let holds = |room: RoomId| {
            tree.read()
                .desktop
                .workspaces
                .iter()
                .any(|workspace| workspace.room == room)
        };
        // The site's workspace held already needs no door; one this device
        // still holds a membership of (the person left it here and came
        // back) gets its place back.
        if let Some(room) = self.hosted_sites.room_of_site(&site.entry) {
            if holds(room) {
                return Ok(SiteJoin::Held { room });
            }
            if let Ok(owner) = self.native_owner()
                && owner.with_current(room, |_| Ok(())).is_ok()
            {
                tree.write().append_workspace_for_room(&site.name, room);
                self.connect_native_room(owner, room)
                    .map_err(SiteError::Refused)?;
                tracing::info!(%room, "back in a site's workspace this device left");
                return Ok(SiteJoin::Held { room });
            }
            if let Some((local, enrollment)) = self.enrolling_site_room(room) {
                return Ok(SiteJoin::Enrolling {
                    room,
                    local,
                    enrollment,
                });
            }
        }
        let (user, name) = self.site_person()?;
        let answer = runtime.block_on(hosted_sites::admit(site, user, &name))?;
        let invitation = tilo_peer_state::join::decode_and_verify_invitation_text(
            &answer.invitation,
            Some(user),
            &tilo_user_identity::RevocationList::new(),
            tilo_clock::unix_now_ns(),
        )
        .map_err(SiteError::Refused)?;
        let room = RoomId::parse(&invitation.grant().room)
            .ok_or_else(|| SiteError::Refused("the door named an invalid room".into()))?;
        if holds(room) {
            return Ok(SiteJoin::Held { room });
        }
        if let Some((local, enrollment)) = self.enrolling_site_room(room) {
            return Ok(SiteJoin::Enrolling {
                room,
                local,
                enrollment,
            });
        }
        let http = HostHttp::new(&answer.origin).map_err(SiteError::Unreachable)?;
        let host = runtime
            .block_on(tilo_relay_client::https::host_info(http.as_ref()))
            .map_err(|error| SiteError::Unreachable(error.to_string()))?
            .host_cert
            .device_public_key;
        self.hosted_sites
            .remember(
                room,
                HostedRoom {
                    origin: answer.origin.clone(),
                    site: Some(site.entry.to_string()),
                    host: Some(hex::encode(host)),
                },
            )
            .map_err(SiteError::Refused)?;
        let via = Via::Https {
            http,
            door: Some(tilo_relay_session::workspace_join::DoorAdmission {
                token: answer.admission_token.into_bytes(),
                name,
            }),
        };
        let local = tree.write().append_empty_workspace(&site.name);
        match self.accept_invitation_via(local, &answer.invitation, via) {
            Ok(PeerSyncResponse::EnrollmentPending { id }) => {
                self.finish_when_enrolled(room, id, background)
                    .map_err(SiteError::Refused)?;
                tracing::info!(%room, name = %site.name, "joining a site's workspace");
                Ok(SiteJoin::Enrolling {
                    room,
                    local,
                    enrollment: id,
                })
            }
            Ok(_) => Ok(SiteJoin::Held { room }),
            Err(error) => {
                tree.write().delete_workspace(local);
                Err(SiteError::Refused(error))
            }
        }
    }

    /// The site whose home workspace `room` is, when it was joined as one.
    pub(crate) fn site_of_room(&self, room: RoomId) -> Option<url::Url> {
        self.hosted_sites
            .hosted(room)?
            .site
            .and_then(|site| url::Url::parse(&site).ok())
    }

    /// The rooms held here through the web (a site's workspace, or one an
    /// invitation link named): the ones the person can leave.
    pub(crate) fn site_rooms(&self) -> Vec<RoomId> {
        self.hosted_sites.rooms()
    }

    /// The rooms held through the web, each with the site it came from, as
    /// the switcher names them.
    pub(crate) fn site_room_labels(&self) -> Vec<SiteRoom> {
        self.site_rooms()
            .into_iter()
            .map(|room| {
                let site = self
                    .hosted_sites
                    .hosted(room)
                    .and_then(|hosted| hosted.site);
                SiteRoom {
                    room,
                    community: site
                        .as_deref()
                        .is_some_and(|site| self.hosted_sites.is_community(site)),
                    public: site
                        .as_deref()
                        .and_then(|site| url::Url::parse(site).ok())
                        .is_some_and(|site| site.path().contains("/directory/")),
                    site: site
                        .and_then(|site| url::Url::parse(&site).ok())
                        .and_then(|site| site.host_str().map(str::to_owned)),
                }
            })
            .collect()
    }

    /// Leave `room`, a workspace held through the web: it goes from this
    /// computer's workspaces, and a site's is remembered as left, so it is
    /// never joined again unasked.
    ///
    /// # Errors
    ///
    /// When `room` is not held through the web, or what was left is not
    /// kept.
    pub(crate) fn leave_site(&self, room: RoomId) -> Result<(), String> {
        if self.hosted_sites.hosted(room).is_none() {
            return Err("that workspace is not held through the web".into());
        }
        match self.site_of_room(room) {
            // A site's workspace is remembered as left, and with it the
            // membership this device keeps, for Join again.
            Some(site) => {
                self.hosted_sites.set_left(&site, true)?;
                self.remove_workspace_of(room);
            }
            None => self.drop_hosted_room(room),
        }
        tracing::info!(%room, "left a workspace held through the web");
        Ok(())
    }

    /// Whether this computer holds the workspace of `site` now.
    pub(crate) fn holds_site(&self, site: &url::Url) -> bool {
        let Some(room) = self.hosted_sites.room_of_site(site) else {
            return false;
        };
        self.replica.lock().is_ok_and(|replica| {
            replica
                .tree
                .read()
                .desktop
                .workspaces
                .iter()
                .any(|workspace| workspace.room == room)
        })
    }

    /// Leave the workspace of `site`, held or not: it is not joined again
    /// unasked.
    ///
    /// # Errors
    ///
    /// When what was left is not kept.
    #[cfg(test)]
    pub(crate) fn leave_community(&self, site: &url::Url) -> Result<(), String> {
        match self.hosted_sites.room_of_site(site) {
            Some(room) => self.leave_site(room),
            None => self.hosted_sites.set_left(site, true),
        }
    }

    /// Whether the person left the workspace of `site`.
    #[cfg(test)]
    pub(crate) fn left_site(&self, site: &url::Url) -> bool {
        self.hosted_sites.left(site)
    }

    /// Join the workspace of `site` again after the person left it.
    /// Blocks on the site: call it off the frame loop.
    ///
    /// # Errors
    ///
    /// As [`Self::join_site`].
    pub(crate) fn rejoin_site(&self, site: &url::Url) -> Result<SiteJoin, SiteError> {
        self.hosted_sites
            .set_left(site, false)
            .map_err(SiteError::Refused)?;
        self.join_site(site)
    }

    /// The slot and enrollment an enrollment into `room` still runs for.
    fn enrolling_site_room(&self, room: RoomId) -> Option<(WorkspaceId, u64)> {
        let id = self.enrolling_into(room)?;
        let attempts = self.native_enrollment.attempts.lock().ok()?;
        attempts.get(&id).map(|attempt| (attempt.local(), id))
    }
}
