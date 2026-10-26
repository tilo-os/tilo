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
use std::sync::Arc;

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

/// One redemption of a site workspace's invitation into the workspace that
/// is its room.
struct SiteEnrollment<'a> {
    room: RoomId,
    /// The workspace it installs into: the room's own, made for the join.
    local: WorkspaceId,
    /// The invitation, as text.
    invitation: &'a str,
    via: Via,
}

/// Where joining a site's workspace stands once it was asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SiteJoin {
    /// The workspace is held here already.
    Held {
        /// Its room.
        room: RoomId,
    },
    /// The host admitted this device; its workspace says Joining… until the
    /// enrollment is installed.
    Enrolling {
        room: RoomId,
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
        let name = super::own_person_name(
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
        let entry = entry.clone();
        let site = self
            .wait_on(async move { hosted_sites::resolve_site(&entry).await })
            .map_err(SiteError::Unreachable)??;
        self.join_site_workspace(&site, background)
    }

    /// Come back to the site workspace `room` this device knows: held still,
    /// an enrollment under way, or one whose exchange or installation failed
    /// and keeps its proof. `None` when there is nothing to come back to.
    fn rejoin_site_room(
        &self,
        site: &SiteWorkspace,
        room: RoomId,
        tree: &crate::shell::state::WorkspaceTreeHandle,
        background: bool,
    ) -> Result<Option<SiteJoin>, SiteError> {
        if tree.read().holds_room(room) {
            return Ok(Some(SiteJoin::Held { room }));
        }
        if let Some((local, enrollment)) = self.enrolling_site_room(room) {
            return Ok(Some(SiteJoin::Enrolling {
                room,
                local,
                enrollment,
            }));
        }
        // A join whose exchange or installation failed keeps its proof in
        // the room's workspace, and the host has admitted that proof: it is
        // retried exactly, never by a second admission, which the retained
        // proof refuses.
        let Some((local, invitation)) = self.retained_join(room).map_err(SiteError::Refused)?
        else {
            return Ok(None);
        };
        let origin = self
            .hosted_sites
            .origin(room)
            .ok_or_else(|| SiteError::Refused("the workspace's host is unknown".into()))?;
        let http = HostHttp::new(&origin).map_err(SiteError::Unreachable)?;
        tracing::info!(%room, name = %site.name, "joining a site's workspace again with its retained proof");
        self.enroll_site(
            site,
            SiteEnrollment {
                room,
                local,
                invitation: &invitation,
                via: Via::Https { http, door: None },
            },
            background,
        )
        .map(Some)
    }

    /// Join the workspace a site's entry named; its enrollment installs in
    /// the `background`, else by its caller.
    pub(super) fn join_site_workspace(
        &self,
        site: &SiteWorkspace,
        background: bool,
    ) -> Result<SiteJoin, SiteError> {
        let tree = self.tree.clone();
        // The site's workspace held already needs no door; one this device
        // still holds a membership of (the person left it here and came
        // back) gets its place back.
        if let Some(room) = self.hosted_sites.room_of_site(&site.entry)
            && let Some(join) = self.rejoin_site_room(site, room, &tree, background)?
        {
            return Ok(join);
        }
        let (user, name) = self.site_person()?;
        let answer = {
            let (site, name) = (site.clone(), name.clone());
            self.wait_on(async move { hosted_sites::admit(&site, user, &name).await })
                .map_err(SiteError::Unreachable)??
        };
        let invitation = tilo_peer_state::join::decode_and_verify_invitation_text(
            &answer.invitation,
            Some(user),
            &tilo_user_identity::RevocationList::new(),
            tilo_clock::unix_now_ns(),
        )
        .map_err(SiteError::Refused)?;
        let room = RoomId::parse(&invitation.grant().room)
            .ok_or_else(|| SiteError::Refused("the door named an invalid room".into()))?;
        // The door named the room: a room this device holds or is joining
        // already needs nothing more, and a site set up again names another
        // room, joined as a workspace of its own.
        if let Some(join) = self.rejoin_site_room(site, room, &tree, background)? {
            return Ok(join);
        }
        let http = HostHttp::new(&answer.origin).map_err(SiteError::Unreachable)?;
        let host = self
            .wait_on({
                let http = Arc::clone(&http);
                async move { tilo_relay_client::https::host_info(http.as_ref()).await }
            })
            .map_err(SiteError::Unreachable)?
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
        // The workspace is the room from the start, saying Joining… until its
        // enrollment installs: never a placeholder of another room.
        let local = tree
            .write()
            .join_native_room(&site.name, room)
            .ok_or_else(|| SiteError::Refused("the door named a room that is not joined".into()))?;
        tracing::info!(%room, name = %site.name, "joining a site's workspace");
        self.enroll_site(
            site,
            SiteEnrollment {
                room,
                local,
                invitation: &answer.invitation,
                via,
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
                // The workspace stays, saying why it was not joined, for
                // the person to try again.
                tracing::error!(%room, name = %site.name, %error, "a site's workspace was not joined");
                self.hosted_sites
                    .set_join(joining(JoinProgress::Failed(error.clone())));
                Err(SiteError::Refused(error))
            }
        }
    }

    /// Why `room` will not be joined on this device until the person acts:
    /// its join failed, or its host admits them no more.
    pub(crate) fn join_refusal(&self, room: RoomId) -> Option<String> {
        match self.hosted_sites.join_of(room)?.progress {
            JoinProgress::Joining => None,
            JoinProgress::Failed(error) => Some(crate::shell::i18n::t_args(
                "directory-join-failed-line",
                &[("error", error.into())],
            )),
            JoinProgress::NeedsJoinAgain => {
                Some(crate::shell::i18n::t("workspace-needs-join-again"))
            }
        }
    }

    /// Where the joins of site workspaces this session runs stand: each
    /// joining, or failed and why.
    pub(crate) fn site_joins(&self) -> Vec<SiteJoinState> {
        self.hosted_sites.joins()
    }

    /// The site workspaces whose joins installed since this was last asked:
    /// chrome brings the person into each, in the app its site names.
    ///
    /// An arrival waits until this device shows the room's own desktop: its
    /// windows decide whether the site's app is already open there. Decided
    /// before they arrived, the arrival opened the app a second time beside
    /// the room's own window of it.
    pub(crate) fn take_arrivals(&self) -> Vec<Arrival> {
        let tree = self.tree.clone();
        self.hosted_sites
            .take_arrivals(|room| tree.read().room_docs.contains_key(&room))
    }

    /// Open the app `arrival`'s site names in the workspace its join
    /// installed: where the person arrives, once. Waits on the launch.
    ///
    /// The workspace's windows are everyone's: when someone already has the
    /// app open there, the person arrives at that window (their own copy of
    /// it runs, as for every window someone else opened) rather than adding
    /// another, so a community with fifty people in it has one Chat, not
    /// fifty.
    pub(crate) fn open_arrival(&self, arrival: &Arrival) -> Result<(), String> {
        let Some(app) = arrival.app.as_deref() else {
            return Ok(());
        };
        let tree = self.tree.clone();
        let workspace = tree
            .read()
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room() == arrival.room)
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
            drop(tree);
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

    /// An enrollment into `room`'s workspace `local` ended: installed, or
    /// failed with its error, which is logged as the failure it is. A
    /// workspace held through the web that installed is arrived in; one
    /// that failed stays, saying why, for the person to try again.
    pub(super) fn enrollment_ended(
        &self,
        room: RoomId,
        local: WorkspaceId,
        outcome: Result<(), String>,
    ) {
        match outcome {
            Ok(()) => {
                self.hosted_sites.end_join(room);
                if self.hosted_sites.hosted(room).is_some() {
                    self.hosted_sites.arrive(room);
                }
            }
            Err(error) => {
                tracing::error!(%room, %error, "a workspace was not enrolled");
                let mut state = self
                    .hosted_sites
                    .join_of(room)
                    .unwrap_or_else(|| SiteJoinState {
                        room,
                        site: self
                            .hosted_sites
                            .hosted(room)
                            .and_then(|hosted| hosted.site),
                        workspace: Some(local),
                        progress: JoinProgress::Joining,
                    });
                state.workspace = Some(local);
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

    /// Leave `room`, a workspace held through the web: the workspace and
    /// this device's replica of the room go together, and a site's is
    /// remembered as left, so it is never joined again unasked; its door
    /// joins it again when the person asks.
    pub(crate) fn leave_site(&self, room: RoomId) -> Result<(), String> {
        if self.hosted_sites.hosted(room).is_none() {
            return Err("that workspace is not held through the web".into());
        }
        if let Some(site) = self.site_of_room(room) {
            self.hosted_sites.set_left(&site, true)?;
        }
        self.drop_hosted_room(room)?;
        self.hosted_sites.end_join(room);
        tracing::info!(%room, "left a workspace held through the web");
        Ok(())
    }

    /// Whether this computer holds the workspace of `site` now.
    pub(crate) fn holds_site(&self, site: &url::Url) -> bool {
        let Some(room) = self.hosted_sites.room_of_site(site) else {
            return false;
        };
        self.tree
            .read()
            .desktop
            .workspaces
            .iter()
            .any(|workspace| workspace.room() == room)
    }

    /// Leave the workspace of `site`, held or not: it is not joined again
    /// unasked.
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

    /// Join `room`, a site's workspace this computer holds whose host admits
    /// this device's person no longer, again, as the person asked
    /// ([`Self::retry_join`]): ask the site's door to admit the person again
    /// and redeem that admission with this device's own certificate, which
    /// the room's enrollment here already holds, then dial the room again.
    /// Nothing else changes: the workspace, its enrollment and what it holds
    /// stay as they are; the host adds the person back at its door's role.
    /// Blocks on the site: call it off the frame loop.
    ///
    /// # Errors
    ///
    /// Why the person was not admitted again, in their words.
    pub(super) fn join_site_again(&self, room: RoomId) -> Result<(), String> {
        let entry = self
            .site_of_room(room)
            .ok_or("that workspace is not a site's")?;
        let (user, name) = self.site_person().map_err(|error| error.to_string())?;
        let identity = self
            .host_identity
            .as_ref()
            .ok_or("this computer has no identity yet")?;
        let owner = self.native_owner()?;
        let device = owner.with_current(room, |view| Ok(view.device().clone()))?;
        let key = identity.device_signing_key.clone();
        if key.verifying_key().to_bytes() != device.device_public_key {
            return Err("the room's enrollment names another device".into());
        }
        let connect = crate::network::actor::native_ingress::Connect::new(owner, room)?;
        let origin = self
            .hosted_sites
            .origin(room)
            .ok_or("the workspace's host is unknown")?;
        let https = crate::network::actor::HttpsRoute(HostHttp::new(&origin)?);
        self.wait_on(async move { readmit(&entry, room, (user, name), device, &key).await })?
            .map_err(|error| error.to_string())?;
        tracing::info!(%room, "joined a site's workspace again after its host let the person go");
        self.hosted_sites.end_join(room);
        self.cmd_tx
            .try_send(crate::network::actor::NetCmd::NativeConnect(
                connect,
                Some(https),
            ))
            .map_err(|_| "the workspace's connection could not be started")?;
        Ok(())
    }

    /// The slot and enrollment an enrollment into `room` still runs for.
    fn enrolling_site_room(&self, room: RoomId) -> Option<(WorkspaceId, u64)> {
        let id = self.enrolling_into(room)?;
        let attempts = self.native_enrollment.attempts.lock().ok()?;
        attempts.get(&id).map(|attempt| (attempt.local(), id))
    }
}

/// Ask the door of the site at `entry` to admit `person` (their key and
/// name) to `room` again, and redeem that admission with `device`, the
/// certificate the room's enrollment here holds, signing with `key`.
async fn readmit(
    entry: &url::Url,
    room: RoomId,
    (user, name): ([u8; 32], String),
    device: tilo_user_identity::DeviceCert,
    key: &ed25519_dalek::SigningKey,
) -> Result<(), SiteError> {
    use tilo_delivery_protocol::workspace_join::IrohWorkspaceHostHintV1;
    let site = hosted_sites::resolve_site(entry).await?;
    let answer = hosted_sites::admit(&site, user, &name).await?;
    let invitation = tilo_peer_state::join::decode_and_verify_invitation_text(
        &answer.invitation,
        Some(user),
        &tilo_user_identity::RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .map_err(SiteError::Refused)?;
    if RoomId::parse(&invitation.grant().room) != Some(room) {
        return Err(SiteError::Refused(
            "the site's door admits to another workspace now".into(),
        ));
    }
    let host = invitation
        .grant()
        .transport
        .iter()
        .find_map(|hint| {
            IrohWorkspaceHostHintV1::authenticate_transport_hint(hint)
                .ok()
                .map(|(host, _)| host.endpoint_addr)
        })
        .ok_or_else(|| SiteError::Refused("the invitation names no host".into()))?;
    let client = tilo_relay_session::workspace_join::WorkspaceJoinClient::https(
        HostHttp::new(&answer.origin).map_err(SiteError::Unreachable)?,
        Some(tilo_relay_session::workspace_join::DoorAdmission {
            token: answer.admission_token.into_bytes(),
            name,
        }),
    );
    let mut request_id = [0; 32];
    rand_core::Rng::fill_bytes(
        &mut getrandom::rand_core::UnwrapErr(getrandom::SysRng),
        &mut request_id,
    );
    client
        .redeem(
            host,
            invitation.signed_invitation().to_vec(),
            device,
            key,
            request_id,
        )
        .await
        .map_err(|error| SiteError::Refused(error.to_string()))?;
    Ok(())
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
