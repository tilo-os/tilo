//! Joining the home workspace of a site as this computer's person: through
//! the site's admission door, then the host's workspace join exchange over
//! HTTPS ([`super::hosted_sites`]).

use super::hosted_sites::{
    self, Arrival, HostHttp, HostedRoom, JoinProgress, SiteError, SiteJoinState, SiteWorkspace,
};
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

/// One redemption of a site workspace's invitation into a slot.
struct SiteEnrollment<'a> {
    room: RoomId,
    /// The workspace slot it installs into.
    local: WorkspaceId,
    /// The invitation, as text.
    invitation: &'a str,
    via: Via,
    /// The slot was made for this join, and goes when it fails.
    placeholder: bool,
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
        // The person's name, never the device's: a room, and a public one
        // above all, learns nothing of this person's devices.
        let name = super::shared_person_name(
            ui::local_identity::load().map(|identity| identity.display_name),
        );
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
            // A join whose exchange or installation failed keeps its proof
            // in its slot, and the host has admitted that proof: it is
            // retried exactly, never by a second admission into a second
            // slot, which the retained proof refuses.
            let retained = self
                .pending_host_room(room)
                .map_err(SiteError::Refused)?
                .filter(|(local, _)| {
                    tree.read()
                        .desktop
                        .workspaces
                        .iter()
                        .any(|workspace| workspace.id == *local)
                });
            if let Some((local, invitation)) = retained {
                let origin = self
                    .hosted_sites
                    .origin(room)
                    .ok_or_else(|| SiteError::Refused("the workspace's host is unknown".into()))?;
                let http = HostHttp::new(&origin).map_err(SiteError::Unreachable)?;
                tracing::info!(%room, name = %site.name, "joining a site's workspace again with its retained proof");
                return self.enroll_site(
                    site,
                    SiteEnrollment {
                        room,
                        local,
                        invitation: &invitation,
                        via: Via::Https { http, door: None },
                        placeholder: false,
                    },
                    background,
                );
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
                    app: site.app.clone(),
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
        tracing::info!(%room, name = %site.name, "joining a site's workspace");
        self.enroll_site(
            site,
            SiteEnrollment {
                room,
                local,
                invitation: &answer.invitation,
                via,
                placeholder: true,
            },
            background,
        )
    }

    /// Redeem `enrollment`'s invitation into its slot, and say that the
    /// site's workspace is joining; its enrollment installs in the
    /// `background`, else by its caller.
    fn enroll_site(
        &self,
        site: &SiteWorkspace,
        enrollment: SiteEnrollment<'_>,
        background: bool,
    ) -> Result<SiteJoin, SiteError> {
        let SiteEnrollment {
            room,
            local,
            invitation,
            via,
            placeholder,
        } = enrollment;
        let joining = |progress| SiteJoinState {
            room,
            site: Some(site.entry.to_string()),
            workspace: Some(local),
            progress,
        };
        // The slot says Joining… from the moment its exchange starts, not
        // once the host answered it.
        self.hosted_sites.set_join(joining(JoinProgress::Joining));
        match self.accept_invitation_via(local, invitation, via) {
            Ok(PeerSyncResponse::EnrollmentPending { id }) => {
                if let Err(error) = self.finish_when_enrolled(room, id, background) {
                    self.hosted_sites.end_join(room);
                    return Err(SiteError::Refused(error));
                }
                Ok(SiteJoin::Enrolling {
                    room,
                    local,
                    enrollment: id,
                })
            }
            Ok(_) => {
                self.hosted_sites.end_join(room);
                Ok(SiteJoin::Held { room })
            }
            Err(error) => {
                tracing::error!(%room, name = %site.name, %error, "a site's workspace was not joined");
                let mut failed = joining(JoinProgress::Failed(error.clone()));
                if placeholder {
                    self.remove_placeholder(local, room);
                    failed.workspace = None;
                }
                self.hosted_sites.set_join(failed);
                Err(SiteError::Refused(error))
            }
        }
    }

    /// Take away `local`, the empty workspace a join into `room` made, unless
    /// the room installed into it after all.
    pub(super) fn remove_placeholder(&self, local: WorkspaceId, room: RoomId) {
        let Ok(replica) = self.replica.lock() else {
            return;
        };
        let mut tree = replica.tree.write();
        let unjoined = tree
            .desktop
            .workspaces
            .iter()
            .any(|workspace| workspace.id == local && workspace.room != room);
        if unjoined {
            tree.delete_workspace(local);
        }
    }

    /// Where the joins of site workspaces this session runs stand: each
    /// joining, or failed and why.
    pub(crate) fn site_joins(&self) -> Vec<SiteJoinState> {
        self.hosted_sites.joins()
    }

    /// The site workspaces whose joins installed since this was last asked:
    /// chrome brings the person into each, in the app its site names.
    pub(crate) fn take_arrivals(&self) -> Vec<Arrival> {
        self.hosted_sites.take_arrivals()
    }

    /// Open the app `arrival`'s site names in the workspace its join
    /// installed: where the person arrives, once. Waits on the launch.
    ///
    /// The workspace's windows are everyone's: when someone already has the
    /// app open there, the person arrives at that window (their own copy of
    /// it runs, as for every window someone else opened) rather than adding
    /// another, so a community with fifty people in it has one Chat, not
    /// fifty.
    ///
    /// # Errors
    ///
    /// When the workspace is gone or the app did not open.
    pub(crate) fn open_arrival(&self, arrival: &Arrival) -> Result<(), String> {
        let Some(app) = arrival.app.as_deref() else {
            return Ok(());
        };
        let tree = self
            .replica
            .lock()
            .map_err(|_| "the workspaces are unavailable")?
            .tree
            .clone();
        let workspace = tree
            .read()
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room == arrival.room)
            .cloned()
            .ok_or("the joined workspace is gone")?;
        if let Some((strip, group, tab)) = window_of(&workspace, app) {
            let mut tree = tree.write();
            tree.desktop.active_strip.insert(workspace.id, strip);
            tree.desktop.set_active_group(strip, group);
            if let Some(group) = tree
                .desktop
                .workspaces
                .iter_mut()
                .find(|candidate| candidate.id == workspace.id)
                .and_then(|workspace| workspace.strips.iter_mut().find(|s| s.id == strip))
                .and_then(|strip| strip.window_groups.iter_mut().find(|g| g.id == group))
            {
                group.activate_tab(tab);
            }
            tree.queue_save();
            return Ok(());
        }
        let launcher = crate::shell::broker_services::NativeLauncher(Some(tree), None);
        tilo_action_broker::services::LauncherServices::launch(
            &launcher,
            app,
            &workspace,
            None,
            &tilo_action_broker::services::LaunchPlace::Focus,
        )
    }

    /// An enrollment into `room`'s slot `local` ended: installed, or failed
    /// with its error, which is logged as the failure it is. A workspace
    /// held through the web that installed is arrived in; a site's that
    /// failed says why, and keeps its slot only while the slot retains the
    /// proof a retry redeems.
    pub(super) fn enrollment_ended(
        &self,
        room: RoomId,
        local: WorkspaceId,
        outcome: Result<(), String>,
    ) {
        match outcome {
            Ok(()) => {
                if self.hosted_sites.hosted(room).is_some() {
                    self.hosted_sites.arrive(room);
                }
            }
            Err(error) => {
                tracing::error!(%room, %error, "a workspace was not enrolled");
                let Some(mut state) = self.hosted_sites.join_of(room) else {
                    return;
                };
                let retained = self
                    .native_owner()
                    .ok()
                    .and_then(|owner| owner.pending_enrollment(room).ok().flatten())
                    .is_some_and(|pending| {
                        pending.target_workspace() == Some(local)
                            && pending.status() == tilo_joined_room::PendingEnrollmentStatus::Active
                    });
                if !retained {
                    self.remove_placeholder(local, room);
                    state.workspace = None;
                }
                state.progress = JoinProgress::Failed(error);
                self.hosted_sites.set_join(state);
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
        self.hosted_sites.end_join(room);
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

/// The first window of `app` in `workspace`, whoever opened it: its strip,
/// group and tab.
fn window_of(
    workspace: &crate::shell::state::Workspace,
    app: &str,
) -> Option<(
    crate::shell::state::StripId,
    crate::shell::state::GroupId,
    crate::shell::state::TabId,
)> {
    use crate::shell::state::TileContent;
    workspace.strips.iter().find_map(|strip| {
        strip.window_groups.iter().find_map(|group| {
            group.tabs.iter().find_map(|tab| {
                let shows = match &tab.tile {
                    TileContent::Foreign { app_id, .. }
                    | TileContent::LocalApp { app_id, .. }
                    | TileContent::PeerWindow { app_id, .. } => app_id == app,
                    _ => false,
                };
                shows.then_some((strip.id, group.id, tab.id))
            })
        })
    })
}
