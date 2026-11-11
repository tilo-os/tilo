//! The workspaces this relay lists as public workspaces, and the door that lets
//! anyone in with the role their owner chose.
//!
//! Making a workspace public is one of its governance settings
//! ([`governance::SETTINGS`]): its owner, or anyone the workspace lets change its
//! settings, publishes a [`Listing`] (a description and the default role)
//! through the workspace's `publish` action and takes it back with `unpublish`.
//! The relay keeps the listings beside its workspaces (`listings.json`), lists
//! them in its directory ([`Workspaces::directory`]) and answers each workspace's door
//! ([`Workspaces::admit_listed`]): a person who asks gets an invitation only they
//! can redeem and the one-time token of a pre-admission at the default role,
//! so their redemption is admitted at once, as the workspace's owner decided when
//! they published it. The listing is that decision; no one is asked per
//! join. The host signs each such grant with its own device key, and a
//! redemption is admitted only under a grant the owner or this host's door
//! signed while the workspace is public.
//!
//! Moderation is the workspace's own: a banned person is refused at the door, the
//! workspace's moderators remove and ban through its governance, each admission
//! is a [`WorkspaceNotice::PreAdmitted`] the owner sees, and a report of the
//! workspace through its door is a [`WorkspaceNotice::Reported`].

mod entering;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tilo_action_policy::roles::governance;
use tilo_lock::MutexRecover as _;
use tilo_relay_protocol::directory::{
    DirectoryEntry, DoorJoin, DoorReport, Face, Listing, MAX_FACES, door_path,
};
use tilo_relay_protocol::workspaces::{PreAdmission, WorkspaceNotice, WorkspaceRole};
use tilo_relay_session::workspaces::{IssueInvitationRequest, IssuedInvitation};
use tilo_user_identity::DeviceCert;
use web_time::SystemTime;

use super::authority::WorkspaceOp;
use super::{CollaborationError, Workspaces};

/// Where the listings are kept, under the workspaces' directory.
const LISTINGS_FILE: &str = "listings.json";
/// How long a door's invitation and its token stay good: time for the
/// person's device to redeem it now, not to keep it.
const DOOR_INVITATION_NS: u64 = 10 * 60 * 1_000_000_000;
/// How a door's admission names where the person came from, in the
/// owner's notices.
pub(crate) const DOOR_LABEL: &str = "the public directory";

/// Why a public workspace's door turns a person away.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DoorRefusal {
    /// The person may not join now; `Display` says why.
    Refused(String),
    /// The workspace's people banned the person.
    Banned,
    /// The person was removed from the workspace within the day.
    Removed,
    /// The door admits only people invited or pre-admitted, and this person
    /// holds neither: their page asks them to be invited.
    InvitationOnly,
    /// The door holds as many open grants as the workspace keeps: the person
    /// comes back in `retry_after_secs`.
    Full {
        /// How long until the earliest open grant expires, at least a second.
        retry_after_secs: u64,
    },
}

impl DoorRefusal {
    fn refused(reason: impl Into<String>) -> Self {
        Self::Refused(reason.into())
    }

    /// The refusal as the door answers it.
    #[must_use]
    pub fn answer(&self) -> tilo_relay_protocol::directory::DoorRefused {
        use tilo_relay_protocol::directory::DoorRefusalCode;
        tilo_relay_protocol::directory::DoorRefused {
            code: match self {
                Self::Refused(_) => DoorRefusalCode::Refused,
                Self::Banned => DoorRefusalCode::Banned,
                Self::Removed => DoorRefusalCode::Removed,
                Self::InvitationOnly => DoorRefusalCode::InvitationOnly,
                Self::Full { .. } => DoorRefusalCode::Full,
            },
            error: self.to_string(),
        }
    }
}

impl std::fmt::Display for DoorRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(reason) => f.write_str(reason),
            Self::Banned => f.write_str("you were banned from this workspace"),
            Self::Removed => f.write_str("you were removed from this workspace within the day"),
            Self::InvitationOnly => f.write_str("this workspace is joined by invitation"),
            Self::Full { retry_after_secs } => write!(
                f,
                "the workspace is taking nobody else just now; come back in {retry_after_secs} seconds"
            ),
        }
    }
}

impl std::error::Error for DoorRefusal {}

#[derive(Default, Serialize, Deserialize)]
struct Stored {
    /// Each public workspace's listing, by workspace UUID.
    workspaces: BTreeMap<String, Listing>,
}

/// The public workspaces, and when someone last changed something in each.
pub(super) struct Listings {
    path: PathBuf,
    stored: std::sync::Mutex<Stored>,
    /// The public workspaces whose door admits only people invited or
    /// pre-admitted: a visitor without an invitation is told so, instead of
    /// being handed one. The host's operator sets it; it is not a listing's
    /// field, so what a stored listing holds never changes.
    invitation_only: std::sync::Mutex<BTreeSet<String>>,
    /// When a member last wrote to each workspace, Unix milliseconds, since this
    /// relay started: the directory's activity hint.
    active: std::sync::Mutex<BTreeMap<String, u64>>,
    /// The workspace UUID after each publish or unpublish.
    changes: tokio::sync::broadcast::Sender<String>,
}

impl Listings {
    /// The listings kept under `dir`; none yet, or none readable, is none.
    pub(super) fn open(dir: &Path) -> Self {
        let path = dir.join(LISTINGS_FILE);
        let stored = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                tracing::warn!(path = %path.display(), %error, "the public listings do not parse; listing nothing");
                Stored::default()
            }),
            Err(_) => Stored::default(),
        };
        Self {
            path,
            stored: std::sync::Mutex::new(stored),
            invitation_only: std::sync::Mutex::default(),
            active: std::sync::Mutex::default(),
            changes: tokio::sync::broadcast::channel(16).0,
        }
    }

    /// Whether `workspace`'s door admits only people invited or
    /// pre-admitted.
    fn invites_only(&self, workspace: &str) -> bool {
        self.invitation_only.lock_recover().contains(workspace)
    }

    /// Set whether `workspace`'s door admits only people invited or
    /// pre-admitted. Settled at every start.
    fn set_invites_only(&self, workspace: &str, invites_only: bool) {
        let mut closed = self.invitation_only.lock_recover();
        if invites_only {
            closed.insert(workspace.to_owned());
        } else {
            closed.remove(workspace);
        }
    }

    fn get(&self, workspace: &str) -> Option<Listing> {
        self.stored
            .lock_recover()
            .workspaces
            .get(workspace)
            .cloned()
    }

    fn listed(&self) -> Vec<(String, Listing)> {
        self.stored
            .lock_recover()
            .workspaces
            .iter()
            .map(|(workspace, listing)| (workspace.clone(), listing.clone()))
            .collect()
    }

    /// List `workspace` with `listing`, or take it out of the directory.
    pub(super) fn set(&self, workspace: &str, listing: Option<Listing>) -> Result<(), String> {
        let mut stored = self.stored.lock_recover();
        let changed = match listing {
            Some(listing) => {
                stored
                    .workspaces
                    .insert(workspace.to_owned(), listing.clone())
                    != Some(listing)
            }
            None => stored.workspaces.remove(workspace).is_some(),
        };
        if !changed {
            return Ok(());
        }
        let bytes = serde_json::to_vec_pretty(&*stored).map_err(|error| error.to_string())?;
        tilo_files::native::atomic::write(&self.path, &bytes).map_err(|error| error.to_string())?;
        drop(stored);
        let _ = self.changes.send(workspace.to_owned());
        Ok(())
    }

    /// Someone changed something in `workspace` at `now_ms`.
    pub(super) fn touch(&self, workspace: &str, now_ms: u64) {
        self.active
            .lock_recover()
            .insert(workspace.to_owned(), now_ms);
    }

    fn last_active(&self, workspace: &str) -> Option<u64> {
        self.active.lock_recover().get(workspace).copied()
    }
}

/// The people of `members` in the workspace now, by `online` user roots: their
/// faces (at most [`MAX_FACES`]) and how many there are.
fn faces(
    members: &[tilo_peer_state::GovernedMember],
    online: &BTreeSet<[u8; 32]>,
) -> (Vec<Face>, u32) {
    let here: Vec<_> = members
        .iter()
        .filter(|member| !member.removed && online.contains(&member.user_public_key))
        .collect();
    let count = u32::try_from(here.len()).unwrap_or(u32::MAX);
    let faces = here
        .into_iter()
        .take(MAX_FACES)
        .map(|member| Face {
            user_public_key: hex::encode(member.user_public_key),
            name: member.display_name.clone().unwrap_or_default(),
        })
        .collect();
    (faces, count)
}

fn person(hex_key: &str) -> Result<[u8; 32], CollaborationError> {
    let mut key = [0; 32];
    hex::decode_to_slice(hex_key, &mut key)
        .map_err(|_| CollaborationError::new("a person is 64 hex characters"))?;
    Ok(key)
}

impl Workspaces {
    /// Make `workspace` public with `listing`, as `device` asks: the workspace's
    /// governance must let its person change the workspace's settings.
    pub async fn publish(
        &self,
        workspace: &str,
        device: &DeviceCert,
        listing: Listing,
    ) -> Result<Listing, CollaborationError> {
        let listing = listing
            .checked()
            .map_err(|error| CollaborationError::new(error.to_string()))?;
        {
            let governed = self.workspace(workspace).await?;
            let governed = governed.lock().await;
            self.authorize(
                device,
                Some(&governed),
                WorkspaceOp::Govern(governance::SETTINGS),
                tilo_clock::unix_now_ns(),
            )?;
        }
        self.inner
            .listings
            .set(workspace, Some(listing.clone()))
            .map_err(CollaborationError::new)?;
        tracing::info!(%workspace, role = ?listing.default_role, "workspace made public");
        Ok(listing)
    }

    /// Take `workspace` out of the directory, as `device` asks. Its members stay;
    /// nobody new gets in through its door.
    ///
    /// # Errors
    ///
    /// As [`Self::publish`].
    pub async fn unpublish(
        &self,
        workspace: &str,
        device: &DeviceCert,
    ) -> Result<(), CollaborationError> {
        {
            let governed = self.workspace(workspace).await?;
            let governed = governed.lock().await;
            self.authorize(
                device,
                Some(&governed),
                WorkspaceOp::Govern(governance::SETTINGS),
                tilo_clock::unix_now_ns(),
            )?;
        }
        self.inner
            .listings
            .set(workspace, None)
            .map_err(CollaborationError::new)?;
        tracing::info!(%workspace, "workspace no longer public");
        Ok(())
    }

    /// Settle whether `workspace`'s door admits only people invited or
    /// pre-admitted, as the host's operator asked. Its directory listing is
    /// untouched: the workspace stays listed either way.
    ///
    /// # Errors
    ///
    /// Returns why `device` may not change the workspace's settings.
    pub async fn list_invitation_only(
        &self,
        workspace: &str,
        device: &DeviceCert,
        invites_only: bool,
    ) -> Result<(), CollaborationError> {
        {
            let governed = self.workspace(workspace).await?;
            let governed = governed.lock().await;
            self.authorize(
                device,
                Some(&governed),
                WorkspaceOp::Govern(governance::SETTINGS),
                tilo_clock::unix_now_ns(),
            )?;
        }
        self.inner
            .listings
            .set_invites_only(workspace, invites_only);
        Ok(())
    }

    /// Whether `user` holds an invitation this workspace issued and has not
    /// spent: one from a member, or the one its door hands a person it lets
    /// in. Either is bound to them, so an invitation-only door still knows
    /// whom it let in when they come back to redeem it.
    async fn invited(&self, workspace: &str, user: [u8; 32], now: u64) -> bool {
        let Ok(state) = self.workspace(workspace).await else {
            return false;
        };
        state
            .lock()
            .await
            .collaboration()
            .pending(now)
            .iter()
            .any(|invitation| invitation.recipient_user_public_key == Some(user))
    }

    pub fn listing(&self, workspace: &str) -> Option<Listing> {
        self.inner.listings.get(workspace)
    }

    /// The workspaces public here now.
    #[must_use]
    pub fn published_rooms(&self) -> Vec<String> {
        self.inner
            .listings
            .listed()
            .into_iter()
            .map(|(workspace, _)| workspace)
            .collect()
    }

    /// The workspace UUID after each publish or unpublish.
    #[must_use]
    pub fn listing_changes(&self) -> tokio::sync::broadcast::Receiver<String> {
        self.inner.listings.changes.subscribe()
    }

    /// Keep the directory's activity hint: each member write marks its workspace
    /// active, for as long as the workspaces are governed here.
    pub(super) fn follow_activity(&self) {
        let mut writes = self.record_writes();
        let workspaces = std::sync::Arc::downgrade(&self.inner);
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        runtime.spawn(async move {
            loop {
                match writes.recv().await {
                    Ok(write) => match workspaces.upgrade() {
                        Some(inner) => inner
                            .listings
                            .touch(&write.workspace, tilo_clock::unix_now_ms()),
                        None => return,
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        });
    }

    /// Who of `workspace`'s members is in it now by `online` (user roots with an
    /// open session here): some of their faces, how many they are, and how
    /// many people belong to the workspace.
    pub async fn presence(
        &self,
        workspace: &str,
        online: &BTreeSet<[u8; 32]>,
    ) -> Result<(Vec<Face>, u32, u32), CollaborationError> {
        let summary = self.summary(workspace).await?;
        let (here, here_count) = faces(&summary.members, online);
        let members = summary
            .members
            .iter()
            .filter(|member| !member.removed)
            .count();
        Ok((here, here_count, u32::try_from(members).unwrap_or(u32::MAX)))
    }

    /// The public workspaces as the directory lists them, the people of each in
    /// it now by `online` (user roots with an open session here).
    pub async fn directory(&self, online: &BTreeSet<[u8; 32]>) -> Vec<DirectoryEntry> {
        let mut entries = Vec::new();
        for (workspace, listing) in self.inner.listings.listed() {
            let Ok(summary) = self.summary(&workspace).await else {
                continue;
            };
            let (here, here_count) = faces(&summary.members, online);
            let members = summary
                .members
                .iter()
                .filter(|member| !member.removed)
                .count();
            entries.push(DirectoryEntry {
                door: door_path(&workspace),
                workspace,
                name: summary.name,
                description: listing.description,
                default_role: listing.default_role,
                here,
                here_count,
                members: u32::try_from(members).unwrap_or(u32::MAX),
                last_active_ms: self.inner.listings.last_active(&summary.workspace),
                community: false,
            });
        }
        entries
    }

    /// Let the person `join` names into the public workspace `workspace`: an
    /// invitation only they can redeem, and the token of a pre-admission at
    /// the workspace's default role, under the owner's listing. The door answers
    /// them as a [`tilo_relay_protocol::directory::DoorAnswer`].
    ///
    /// A workspace that takes people by invitation answers this door only to a
    /// person it has invited: someone the workspace already has in it (joining
    /// again from another device of theirs) or one an invitation still names
    /// [`PendingInvitation::recipient_user_public_key`]. Everyone else is
    /// turned away with the door's own words.
    ///
    /// For a person an invitation still names, the door answers with that
    /// invitation, and records a grant for it at once so the page that made
    /// the join redeems it the way every page redeems a door's answer: its
    /// token is the one the door hands back. The invitation itself is the
    /// decision — its role and lifetime are the inviter's — and the door
    /// mints nothing.
    pub async fn admit_listed(
        &self,
        workspace: &str,
        join: &DoorJoin,
    ) -> Result<(IssuedInvitation, String), DoorRefusal> {
        let refused = |error: CollaborationError| DoorRefusal::Refused(error.to_string());
        let listing = self
            .listing(workspace)
            .ok_or_else(|| DoorRefusal::refused("that workspace is not public"))?;
        let user = person(&join.user_public_key).map_err(refused)?;
        let now = tilo_clock::unix_now_ns();
        // An invitation-only door refuses a stranger who holds no
        // invitation of their own, and says so, rather than letting them in
        // as a guest.
        if self.inner.listings.invites_only(workspace) && !self.invited(workspace, user, now).await
        {
            return Err(DoorRefusal::InvitationOnly);
        }
        let token = hex::encode(rand::random::<[u8; 32]>());
        let grant = self.door_grant(workspace, listing.default_role, now, &token)?;
        let issued = self
            .admit_governed(workspace, join, listing.default_role, user, now, grant)
            .await?;
        self.written(workspace, true).await.map_err(refused)?;
        Ok((issued, token))
    }

    /// The grant the door records for the token it hands out at `workspace`'s
    /// door: signed by this host's device key, which the redemption checks
    /// against the host and the workspace's listing
    /// ([`Workspaces::pre_admission_signer`]).
    fn door_grant(
        &self,
        workspace: &str,
        role: WorkspaceRole,
        now: u64,
        token: &str,
    ) -> Result<PreAdmission, DoorRefusal> {
        let mut grant = PreAdmission {
            workspace: workspace.to_owned(),
            role,
            token_sha256: Sha256::digest(token.as_bytes()).into(),
            label: DOOR_LABEL.to_owned(),
            expires_at_ns: now.saturating_add(DOOR_INVITATION_NS),
            owner_user_public_key: [0; 32],
            signature: [0; 64],
        };
        grant.sign(&self.inner.host.key).map_err(|error| {
            DoorRefusal::refused(CollaborationError::new(error.to_string()).to_string())
        })?;
        Ok(grant)
    }

    /// The invitation `workspace`'s door answers `join` with, its grant
    /// recorded for the token the door hands out.
    async fn admit_governed(
        &self,
        workspace: &str,
        join: &DoorJoin,
        default_role: WorkspaceRole,
        user: [u8; 32],
        now: u64,
        grant: PreAdmission,
    ) -> Result<IssuedInvitation, DoorRefusal> {
        let refused = |error: CollaborationError| DoorRefusal::Refused(error.to_string());
        let issued = {
            let governed = self.workspace(workspace).await.map_err(refused)?;
            let mut governed = governed.lock().await;
            let policy = governed
                .peer()
                .permissions(self.inner.host.cert.device_public_key, SystemTime::now())
                .map_err(|error| refused(CollaborationError::new(error.to_string())))?
                .policy;
            if policy.is_blocked(&hex::encode(user)) {
                return Err(DoorRefusal::Banned);
            }
            if governed.door.refuses(user, now) {
                return Err(DoorRefusal::Removed);
            }
            // A person an invitation of the workspace's own names is handed
            // that invitation, not a new one: the inviter decided its role and
            // how long it stands, and its consent is the decision. The page
            // that joined redeems it with a token as it redeems every door's
            // answer, so the door records a grant for this invitation at once.
            // An invitation a member already pre-admitted carries that grant,
            // and the person redeems it with the member's own token; a page
            // that asks the door without it is handed a fresh invitation as
            // any guest is.
            let their_own = self
                .inner
                .listings
                .invites_only(workspace)
                .then(|| governed.invitation_for(user, now))
                .flatten()
                .filter(|issued| !governed.admissions.carries(issued.invite_id));
            if let Some(issued) = &their_own {
                governed
                    .admissions
                    .record(
                        issued.invite_id,
                        grant,
                        now,
                        self.inner.host.max_pre_admissions,
                    )
                    .map_err(|error| refused(CollaborationError::new(error)))?;
                // The grant is written before the door answers: the page
                // redeems with it at once.
                return {
                    drop(governed);
                    self.written(workspace, true).await.map_err(refused)?;
                    Ok(issued.clone())
                };
            }
            if let Some(retry_after_secs) = governed
                .admissions
                .full_for(now, self.inner.host.max_pre_admissions)
            {
                return Err(DoorRefusal::Full { retry_after_secs });
            }
            let host = self.inner.host.cert.clone();
            let issued = self
                .stage_issue(
                    &mut governed,
                    &host,
                    IssueInvitationRequest {
                        request_id: rand::random(),
                        role: super::pages::member_role(default_role),
                        recipient_user_public_key: Some(user),
                        invitee_name: tilo_user_identity::display_name(&join.display_name)
                            .ok()
                            .map(str::to_owned),
                        expires_at_ns: grant.expires_at_ns,
                        inviter_name: None,
                    },
                    now,
                )
                .map_err(refused)?;
            governed
                .admissions
                .record(
                    issued.invite_id,
                    grant,
                    now,
                    self.inner.host.max_pre_admissions,
                )
                .map_err(|error| refused(CollaborationError::new(error)))?;
            issued
        };
        Ok(issued)
    }

    /// Whether `workspace`'s door keeps `person` out because moderation removed
    /// them within the day.
    pub async fn removed_within_the_day(&self, workspace: &str, person: [u8; 32]) -> bool {
        let now = tilo_clock::unix_now_ns();
        match self.workspace(workspace).await {
            Ok(state) => state.lock().await.door.refuses(person, now),
            Err(_) => false,
        }
    }

    /// Whether `grant`, recorded for an invitation to `workspace`, was signed by
    /// someone who may admit people there: the workspace's owner (this relay's
    /// person), or this host's own door while the workspace is public.
    ///
    /// # Errors
    ///
    /// The signature does not verify, or its signer may not admit anyone.
    pub(super) fn pre_admission_signer(
        &self,
        workspace: &str,
        grant: &PreAdmission,
    ) -> Result<(), String> {
        grant.verify()?;
        let host = &self.inner.host.cert;
        if grant.owner_user_public_key == host.user_public_key {
            return Ok(());
        }
        if grant.owner_user_public_key == host.device_public_key
            && grant.label == DOOR_LABEL
            && self.listing(workspace).is_some()
        {
            return Ok(());
        }
        Err("the pre-admission is signed by neither the room's owner nor its host's door".into())
    }

    /// Tell the people who run the public workspace `workspace` that someone reported
    /// it: a notice its owner and moderators read beside the workspace's
    /// admissions.
    pub async fn report_listed(
        &self,
        workspace: &str,
        report: &DoorReport,
    ) -> Result<(), CollaborationError> {
        if self.listing(workspace).is_none() {
            return Err(CollaborationError::new("that workspace is not public"));
        }
        let reporter = person(&report.reporter_user_public_key)?;
        let reason = report.reason().map_err(CollaborationError::new)?.to_owned();
        let governed = self.workspace(workspace).await?;
        let mut governed = governed.lock().await;
        tracing::warn!(%workspace, reporter = %hex::encode(reporter), %reason, "a public workspace was reported");
        governed
            .admissions
            .notice(WorkspaceNotice::Reported {
                user_public_key: reporter,
                reason,
                at_ns: tilo_clock::unix_now_ns(),
            })
            .map_err(CollaborationError::new)
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use tilo_relay_protocol::workspaces::WorkspaceRole;

    fn member(key: u8, name: &str, removed: bool) -> tilo_peer_state::GovernedMember {
        tilo_peer_state::GovernedMember {
            user_public_key: [key; 32],
            role: tilo_peer_state::join::MemberRole::Commenter,
            display_name: Some(name.into()),
            active_device_keys: Vec::new(),
            removed,
            administrator: false,
        }
    }

    #[test]
    fn only_members_in_the_room_now_are_its_faces() {
        let members = vec![
            member(1, "Ana", false),
            member(2, "Ben", false),
            member(3, "Gone", true),
        ];
        let online: BTreeSet<_> = [[1; 32], [3; 32], [9; 32]].into_iter().collect();
        let (shown, count) = faces(&members, &online);
        assert_eq!(
            count, 1,
            "a removed member online is not here, nor a stranger"
        );
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].name, "Ana");
        assert_eq!(shown[0].user_public_key, hex::encode([1; 32]));
    }

    #[test]
    fn listings_are_kept_and_each_change_is_announced_once() {
        let dir = tempfile::tempdir().unwrap();
        let listings = Listings::open(dir.path());
        let mut changes = listings.changes.subscribe();
        let listing = Listing {
            description: "Coffee and tea".into(),
            default_role: WorkspaceRole::Commenter,
        };
        listings.set("room-a", Some(listing.clone())).unwrap();
        listings.set("room-a", Some(listing.clone())).unwrap();
        assert_eq!(changes.try_recv().ok().as_deref(), Some("room-a"));
        assert!(
            changes.try_recv().is_err(),
            "the same listing again changes nothing"
        );
        let reopened = Listings::open(dir.path());
        assert_eq!(reopened.get("room-a"), Some(listing));
        listings.set("room-a", None).unwrap();
        assert_eq!(Listings::open(dir.path()).get("room-a"), None);
        assert_eq!(changes.try_recv().ok().as_deref(), Some("room-a"));
    }
}

#[cfg(test)]
mod door_tests {
    #![expect(
        clippy::significant_drop_tightening,
        reason = "the relay and workspace handles a test binds are shared handles it keeps until the test ends; dropping one earlier closes the host it drives and releases no lock"
    )]
    use ed25519_dalek::Signer as _;
    use tilo_actions::remote::{
        ActionHost as _, Caller, InvokeArgument, InvokeRequest, Pending, ReceiptStatus,
        RemoteActionTarget,
    };
    use tilo_peer_state::join::MemberRole;
    use tilo_relay_protocol::directory::{DoorJoin, Listing};
    use tilo_relay_protocol::workspaces::{
        JoinReply, JoinStep, PreAdmission, WorkspaceRole, join_proof_digest,
    };
    use tilo_relay_session::workspaces::RemoveMemberRequest;
    use tilo_user_identity::DeviceCert;

    use super::super::actions::{HostWorkspaceActions, PUBLISH, UNPUBLISH};
    use crate::workspaces::DoorRefusal;
    use crate::workspaces::Workspaces;
    use crate::workspaces::fixtures::signed_pre_admission;
    use crate::workspaces::tests::{governor, guest_device, test_host, test_host_with_grants};

    async fn settled<T: Send + 'static>(pending: Pending<T>) -> T {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        pending.then(move |value| {
            let _ = sender.send(value);
        });
        tilo_test_env::within("the governance answer", receiver)
            .await
            .unwrap()
            .expect("answered")
    }

    fn invocation(workspace: &str, verb: &str, argument: serde_json::Value) -> InvokeRequest {
        let serde_json::Value::Object(argument) = argument else {
            panic!("arguments are an object");
        };
        InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: RemoteActionTarget::workspace_governance(workspace, verb),
            argument: if argument.is_empty() {
                InvokeArgument::None
            } else {
                InvokeArgument::Json(argument)
            },
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
        }
    }

    /// The owner invokes the workspace's governance `verb` with `argument`.
    async fn govern(
        workspaces: &Workspaces,
        workspace: u64,
        owner: &DeviceCert,
        request: InvokeRequest,
    ) -> serde_json::Value {
        let caller = Caller::authenticated(owner.user_public_key, owner.device_public_key);
        let receipt = {
            let host = HostWorkspaceActions::new();
            host.attach(workspaces.clone());
            settled(host.in_workspace(workspace).invoke(&caller, request))
                .await
                .unwrap()
        };
        match receipt.status {
            ReceiptStatus::Completed { output } => output.value.unwrap_or_default(),
            other => panic!("governance completes at once: {other:?}"),
        }
    }

    /// A stranger asks the door of `workspace` and redeems what it answered, as
    /// a Tilo computer does: the reply, and their certificate.
    async fn walk_in(
        workspaces: &Workspaces,
        workspace: &str,
    ) -> (Result<JoinReply, String>, DeviceCert) {
        let (key, cert) = guest_device();
        let join = DoorJoin {
            user_public_key: hex::encode(cert.user_public_key),
            display_name: "Cy".into(),
        };
        let reply = walk_in_as(workspaces, workspace, &join, &key, &cert).await;
        (reply, cert)
    }

    pub(super) async fn walk_in_as(
        workspaces: &Workspaces,
        workspace: &str,
        join: &DoorJoin,
        key: &ed25519_dalek::SigningKey,
        cert: &DeviceCert,
    ) -> Result<JoinReply, String> {
        let (issued, token) = workspaces
            .admit_listed(workspace, join)
            .await
            .map_err(|error| error.to_string())?;
        Ok(redeem(workspaces, issued.signed_invitation, token, key, cert).await)
    }

    /// Redeem `invitation` with the door's `token`, as the person's device.
    pub(super) async fn redeem(
        workspaces: &Workspaces,
        invitation: Vec<u8>,
        token: String,
        key: &ed25519_dalek::SigningKey,
        cert: &DeviceCert,
    ) -> JoinReply {
        let JoinReply::Challenge { challenge } = workspaces
            .page_join(JoinStep::Ask {
                invitation,
                device_cert: cert.clone(),
                request_id: rand::random(),
            })
            .await
        else {
            panic!("no challenge");
        };
        let signature = key.sign(&join_proof_digest(&challenge)).to_bytes();
        workspaces
            .page_join(JoinStep::ProvePreAdmitted {
                challenge,
                signature,
                name: "Cy".into(),
                token: token.into_bytes(),
            })
            .await
    }

    /// What a test does to a door's grant before it is redeemed.
    type Forgery = Box<dyn FnOnce(&mut PreAdmission) + Send>;

    #[tokio::test]
    async fn a_door_grant_with_a_forged_or_zero_signature_admits_nobody() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let relay = test_host(dir.path(), &key, &owner).await;
        let workspaces = relay.workspaces();
        let workspace = workspaces.create("Plaza").await.unwrap().workspace;
        workspaces
            .publish(
                &workspace,
                &owner,
                Listing {
                    description: String::new(),
                    default_role: WorkspaceRole::Editor,
                },
            )
            .await
            .unwrap();
        let host_device = owner.device_public_key;
        let forgeries: [(&str, Forgery); 3] = [
            (
                "a zero signature",
                Box::new(|grant: &mut PreAdmission| grant.signature = [0; 64]),
            ),
            (
                "a stranger's key",
                Box::new(|grant: &mut PreAdmission| {
                    grant
                        .sign(&ed25519_dalek::SigningKey::from_bytes(&[77; 32]))
                        .unwrap();
                }),
            ),
            (
                "a stranger's signature in the host's name",
                Box::new(move |grant: &mut PreAdmission| {
                    grant
                        .sign(&ed25519_dalek::SigningKey::from_bytes(&[78; 32]))
                        .unwrap();
                    grant.owner_user_public_key = host_device;
                }),
            ),
        ];
        for (what, forge) in forgeries {
            let (guest_key, guest) = guest_device();
            let join = DoorJoin {
                user_public_key: hex::encode(guest.user_public_key),
                display_name: "Mallory".into(),
            };
            let (issued, token) = workspaces.admit_listed(&workspace, &join).await.unwrap();
            workspaces
                .workspace(&workspace)
                .await
                .unwrap()
                .lock()
                .await
                .admissions
                .tamper(issued.invite_id, forge);
            let reply = redeem(
                workspaces,
                issued.signed_invitation,
                token,
                &guest_key,
                &guest,
            )
            .await;
            assert!(
                !matches!(reply, JoinReply::Admitted { .. }),
                "{what} admitted someone: {reply:?}"
            );
            let members = workspaces.summary(&workspace).await.unwrap().members;
            assert!(
                members
                    .iter()
                    .all(|member| member.user_public_key != guest.user_public_key),
                "{what}: not a member"
            );
        }

        let (guest_key, guest) = guest_device();
        let join = DoorJoin {
            user_public_key: hex::encode(guest.user_public_key),
            display_name: "Cy".into(),
        };
        assert!(matches!(
            walk_in_as(workspaces, &workspace, &join, &guest_key, &guest).await,
            Ok(JoinReply::Admitted { .. })
        ));
        relay.shutdown().await;
    }

    #[tokio::test]
    async fn whoever_joins_a_public_workspace_gets_the_default_role_its_owner_set() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let relay = test_host(dir.path(), &key, &owner).await;
        let workspaces = relay.workspaces();
        let summary = workspaces.create("Coffee").await.unwrap();
        let workspace = summary.workspace.clone();

        let answer = govern(
            workspaces,
            summary.workspace_id,
            &owner,
            invocation(
                &workspace,
                PUBLISH,
                serde_json::json!({ "default_role": "commenter", "description": "Talk about coffee" }),
            ),
        )
        .await;
        assert_eq!(answer["public"], true);
        assert_eq!(answer["default_role"], "commenter");

        let listed = workspaces
            .directory(&std::collections::BTreeSet::new())
            .await;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "Coffee");
        assert_eq!(listed[0].description, "Talk about coffee");
        assert_eq!(listed[0].default_role, WorkspaceRole::Commenter);

        for _ in 0..2 {
            let (reply, cert) = walk_in(workspaces, &workspace).await;
            match reply.unwrap() {
                JoinReply::Admitted { role, .. } => assert_eq!(role, WorkspaceRole::Commenter),
                other => panic!("the door admits at once: {other:?}"),
            }
            let members = workspaces.summary(&workspace).await.unwrap().members;
            let joined = members
                .iter()
                .find(|member| member.user_public_key == cert.user_public_key)
                .expect("a member now");
            assert_eq!(joined.role, MemberRole::Commenter);
        }

        // Another default from now on: the next person gets it.
        workspaces
            .publish(
                &workspace,
                &owner,
                Listing {
                    description: "Talk about coffee".into(),
                    default_role: WorkspaceRole::Viewer,
                },
            )
            .await
            .unwrap();
        let (reply, _) = walk_in(workspaces, &workspace).await;
        assert!(
            matches!(
                reply.unwrap(),
                JoinReply::Admitted {
                    role: WorkspaceRole::Viewer,
                    ..
                }
            ),
            "the default is read at the door, not when the listing was first made"
        );
        relay.shutdown().await;
    }

    /// Whether `device` may write in `workspace`'s Chat now: its role lets it
    /// change Chat's content and Chat authors the workspace, the two things a
    /// Tilo computer checks before it opens Chat writable rather than
    /// read-only.
    async fn writes_in_chat(workspaces: &Workspaces, workspace: &str, device: &DeviceCert) -> bool {
        const CHAT: &str = "so.tilo.chat";
        let state = workspaces.workspace(workspace).await.unwrap();
        let now = std::time::SystemTime::now();
        let member = state
            .lock()
            .await
            .peer()
            .permissions(device.device_public_key, now)
            .unwrap();
        member.allows(
            &member.policy.node().app(CHAT),
            tilo_action_policy::ActionKind::Undoable,
            now,
        ) && member
            .policy
            .app_authors(CHAT, &tilo_action_policy::Context::at_time(now))
    }

    #[tokio::test]
    async fn a_person_who_joined_a_public_room_can_talk_in_its_chat_unless_the_host_withdraws_it() {
        use std::collections::BTreeSet;
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let relay = test_host(dir.path(), &key, &owner).await;
        let workspaces = relay.workspaces();
        let workspace = workspaces.create("Lounge").await.unwrap().workspace;
        workspaces
            .publish(
                &workspace,
                &owner,
                Listing {
                    description: String::new(),
                    default_role: WorkspaceRole::Editor,
                },
            )
            .await
            .unwrap();
        let (reply, guest) = walk_in(workspaces, &workspace).await;
        assert!(matches!(
            reply.unwrap(),
            JoinReply::Admitted {
                role: WorkspaceRole::Editor,
                ..
            }
        ));
        // An Editor of a workspace whose Chat authors nothing reads it only: what
        // the Lounge showed ("You can read this conversation but not write
        // in it").
        assert!(!writes_in_chat(workspaces, &workspace, &guest).await);

        let chat = BTreeSet::from(["so.tilo.chat".to_owned()]);
        let none = BTreeSet::new();
        assert!(
            workspaces
                .settle_app_authorship(&workspace, &owner, &chat, &none)
                .await
                .unwrap()
        );
        assert!(writes_in_chat(workspaces, &workspace, &guest).await);
        // Every start settles it again; a workspace already so is left alone.
        assert!(
            !workspaces
                .settle_app_authorship(&workspace, &owner, &chat, &none)
                .await
                .unwrap()
        );
        let (_, later) = walk_in(workspaces, &workspace).await;
        assert!(writes_in_chat(workspaces, &workspace, &later).await);

        // Authorship is the app's; whether a person writes stays their
        // role's, and moderation stays the host's.
        workspaces
            .publish(
                &workspace,
                &owner,
                Listing {
                    description: String::new(),
                    default_role: WorkspaceRole::Viewer,
                },
            )
            .await
            .unwrap();
        let (_, viewer) = walk_in(workspaces, &workspace).await;
        assert!(!writes_in_chat(workspaces, &workspace, &viewer).await);
        // Only the host settles it, not a member.
        assert!(
            workspaces
                .settle_app_authorship(&workspace, &guest, &none, &chat)
                .await
                .is_err()
        );
        assert!(writes_in_chat(workspaces, &workspace, &guest).await);

        // The host configured otherwise: Chat is read-only to members again.
        assert!(
            workspaces
                .settle_app_authorship(&workspace, &owner, &none, &chat)
                .await
                .unwrap()
        );
        assert!(!writes_in_chat(workspaces, &workspace, &guest).await);
        relay.shutdown().await;
    }

    /// A public workspace prunes the guests it has not seen for long; a Tilo
    /// computer that holds the workspace comes back through the door as the same
    /// person, with the same device, and is a member again (the shell's
    /// `readmit_site_room`).
    #[tokio::test]
    async fn a_pruned_guest_comes_back_through_the_door_with_the_same_device() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let relay = test_host(dir.path(), &key, &owner).await;
        let workspaces = relay.workspaces();
        let workspace = workspaces.create("Lounge").await.unwrap().workspace;
        workspaces
            .publish(
                &workspace,
                &owner,
                Listing {
                    description: String::new(),
                    default_role: WorkspaceRole::Editor,
                },
            )
            .await
            .unwrap();
        let (guest_key, guest) = guest_device();
        let join = DoorJoin {
            user_public_key: hex::encode(guest.user_public_key),
            display_name: "Cy".into(),
        };
        let member = |members: &[tilo_peer_state::GovernedMember]| {
            members
                .iter()
                .any(|member| member.user_public_key == guest.user_public_key && !member.removed)
        };
        assert!(matches!(
            walk_in_as(workspaces, &workspace, &join, &guest_key, &guest).await,
            Ok(JoinReply::Admitted { .. })
        ));
        workspaces
            .prune_members(
                &workspace,
                &owner,
                [9; 32],
                &std::collections::BTreeSet::from([guest.user_public_key]),
            )
            .await
            .unwrap();
        assert!(!member(
            &workspaces.summary(&workspace).await.unwrap().members
        ));

        assert!(
            matches!(
                walk_in_as(workspaces, &workspace, &join, &guest_key, &guest).await,
                Ok(JoinReply::Admitted {
                    role: WorkspaceRole::Editor,
                    ..
                })
            ),
            "the same person and device are let in again"
        );
        assert!(member(
            &workspaces.summary(&workspace).await.unwrap().members
        ));
        relay.shutdown().await;
    }

    /// Moderation takes a person's membership away and the door keeps them out
    /// for the day; an idle member the host prunes is let in again at once.
    #[tokio::test]
    async fn a_person_moderation_removed_is_refused_at_the_door_for_the_day() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let relay = test_host(dir.path(), &key, &owner).await;
        let workspaces = relay.workspaces();
        let workspace = workspaces.create("Plaza").await.unwrap().workspace;
        workspaces
            .publish(
                &workspace,
                &owner,
                Listing {
                    description: String::new(),
                    default_role: WorkspaceRole::Editor,
                },
            )
            .await
            .unwrap();
        let (guest_key, guest) = guest_device();
        let join = DoorJoin {
            user_public_key: hex::encode(guest.user_public_key),
            display_name: "Troll".into(),
        };
        assert!(matches!(
            walk_in_as(workspaces, &workspace, &join, &guest_key, &guest).await,
            Ok(JoinReply::Admitted { .. })
        ));
        workspaces
            .remove_member(
                &workspace,
                &owner,
                RemoveMemberRequest {
                    request_id: [7; 32],
                    user_public_key: guest.user_public_key,
                },
            )
            .await
            .unwrap();
        let refused = workspaces
            .admit_listed(&workspace, &join)
            .await
            .unwrap_err();
        assert_eq!(refused, DoorRefusal::Removed);
        assert!(
            workspaces
                .removed_within_the_day(&workspace, guest.user_public_key)
                .await
        );

        // A moderator lets them in again: the door opens with the ban.
        workspaces
            .unban_member(
                &workspace,
                &owner,
                RemoveMemberRequest {
                    request_id: [8; 32],
                    user_public_key: guest.user_public_key,
                },
            )
            .await
            .unwrap();
        assert!(
            !workspaces
                .removed_within_the_day(&workspace, guest.user_public_key)
                .await
        );
        assert!(workspaces.admit_listed(&workspace, &join).await.is_ok());
        relay.shutdown().await;
    }

    /// A removal moderation made before the door had a record of its own (the
    /// room's `removals.json`) keeps the person out after a restart, while a
    /// removal past the day and a malformed key keep nobody out; the old file
    /// is gone once it has been taken over.
    #[tokio::test]
    async fn a_removal_the_room_kept_before_the_door_keeps_its_person_out_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let relay = test_host(dir.path(), &key, &owner).await;
        let workspaces = relay.workspaces().clone();
        let workspace = workspaces.create("Lounge").await.unwrap().workspace;
        workspaces
            .publish(
                &workspace,
                &owner,
                Listing {
                    description: String::new(),
                    default_role: WorkspaceRole::Editor,
                },
            )
            .await
            .unwrap();
        let (_, guest) = guest_device();
        let join = DoorJoin {
            user_public_key: hex::encode(guest.user_public_key),
            display_name: "Troll".into(),
        };
        let now_ms = tilo_clock::unix_now_ms();
        let stale = [9; 32];
        let file = dir.path().join("removals.json");
        std::fs::write(
            &file,
            serde_json::json!({
                hex::encode(guest.user_public_key): now_ms,
                hex::encode(stale): now_ms.saturating_sub(48 * 3600 * 1000),
                "not a person": now_ms,
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            workspaces
                .adopt_legacy_removals(&workspace, &file)
                .await
                .unwrap(),
            1,
            "the day has passed for one, and a malformed key is nobody"
        );
        assert!(!file.exists(), "the old file is gone");
        drop(workspaces);
        relay.shutdown().await;

        let relay = test_host(dir.path(), &key, &owner).await;
        let workspaces = relay.workspaces().clone();
        assert!(
            workspaces
                .removed_within_the_day(&workspace, guest.user_public_key)
                .await
        );
        assert!(!workspaces.removed_within_the_day(&workspace, stale).await);
        assert_eq!(
            workspaces
                .admit_listed(&workspace, &join)
                .await
                .unwrap_err(),
            DoorRefusal::Removed
        );
        relay.shutdown().await;
    }

    /// A door that holds as many open grants as the workspace keeps says when
    /// it has room again instead of taking another invitation.
    #[tokio::test]
    async fn a_full_door_answers_when_it_has_room_again() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let relay = test_host_with_grants(dir.path(), &key, &owner, Some(1)).await;
        let workspaces = relay.workspaces();
        let workspace = workspaces.create("Plaza").await.unwrap().workspace;
        workspaces
            .publish(
                &workspace,
                &owner,
                Listing {
                    description: String::new(),
                    default_role: WorkspaceRole::Editor,
                },
            )
            .await
            .unwrap();
        let join = || {
            let (_, guest) = guest_device();
            DoorJoin {
                user_public_key: hex::encode(guest.user_public_key),
                display_name: "Cy".into(),
            }
        };
        workspaces
            .admit_listed(&workspace, &join())
            .await
            .expect("the door takes the first grant");
        let refused = workspaces
            .admit_listed(&workspace, &join())
            .await
            .unwrap_err();
        let DoorRefusal::Full { retry_after_secs } = refused else {
            panic!("the full door refused another way: {refused:?}");
        };
        assert!(
            (1..=600).contains(&retry_after_secs),
            "the earliest open grant expires within its ten minutes: {retry_after_secs}"
        );
        relay.shutdown().await;
    }

    #[tokio::test]
    async fn a_private_workspace_has_no_door_and_is_not_listed() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let relay = test_host(dir.path(), &key, &owner).await;
        let workspaces = relay.workspaces();
        let summary = workspaces.create("Team").await.unwrap();
        let workspace = summary.workspace.clone();
        let (reply, _) = walk_in(workspaces, &workspace).await;
        assert!(reply.is_err(), "never published: no door");

        workspaces
            .publish(
                &workspace,
                &owner,
                Listing {
                    description: String::new(),
                    default_role: WorkspaceRole::Editor,
                },
            )
            .await
            .unwrap();
        assert_eq!(workspaces.published_rooms(), vec![workspace.clone()]);
        let answer = govern(
            workspaces,
            summary.workspace_id,
            &owner,
            invocation(&workspace, UNPUBLISH, serde_json::json!({})),
        )
        .await;
        assert_eq!(answer["public"], false);
        assert!(
            workspaces
                .directory(&std::collections::BTreeSet::new())
                .await
                .is_empty()
        );
        let (reply, _) = walk_in(workspaces, &workspace).await;
        assert!(reply.is_err(), "unpublished: the door is shut again");

        assert!(
            workspaces
                .publish(
                    &workspace,
                    &owner,
                    Listing {
                        description: String::new(),
                        default_role: WorkspaceRole::Moderator,
                    },
                )
                .await
                .is_err()
        );
        relay.shutdown().await;
    }

    #[tokio::test]
    async fn a_banned_person_is_turned_away_at_the_door_and_others_cannot_publish() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let relay = test_host(dir.path(), &key, &owner).await;
        let workspaces = relay.workspaces();
        let workspace = workspaces.create("Plaza").await.unwrap().workspace;
        workspaces
            .publish(
                &workspace,
                &owner,
                Listing {
                    description: String::new(),
                    default_role: WorkspaceRole::Editor,
                },
            )
            .await
            .unwrap();
        let (guest_key, guest) = guest_device();
        let join = DoorJoin {
            user_public_key: hex::encode(guest.user_public_key),
            display_name: "Troll".into(),
        };
        assert!(matches!(
            walk_in_as(workspaces, &workspace, &join, &guest_key, &guest).await,
            Ok(JoinReply::Admitted { .. })
        ));
        assert!(
            workspaces.unpublish(&workspace, &guest).await.is_err(),
            "an Editor who joined does not change the room's settings"
        );
        workspaces
            .ban_member(
                &workspace,
                &owner,
                RemoveMemberRequest {
                    request_id: [7; 32],
                    user_public_key: guest.user_public_key,
                },
            )
            .await
            .unwrap();
        let again = workspaces.admit_listed(&workspace, &join).await;
        assert!(again.is_err(), "banned: the door refuses them");
        let reported = workspaces
            .report_listed(
                &workspace,
                &tilo_relay_protocol::directory::DoorReport {
                    reporter_user_public_key: hex::encode([5; 32]),
                    reason: "spam in general".into(),
                },
            )
            .await;
        assert!(reported.is_ok(), "{reported:?}");
        relay.shutdown().await;
    }

    /// A listed workspace whose door takes people by invitation only stays
    /// listed — the site's directory names it — while a stranger who holds no
    /// invitation is told so, and a person a member invited walks in. Opening
    /// the door again lets a stranger in as before.
    #[tokio::test]
    async fn an_invitation_only_door_refuses_a_stranger_and_admits_an_invited_person() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let relay = test_host(dir.path(), &key, &owner).await;
        let workspaces = relay.workspaces();
        let workspace = workspaces.create("Lounge").await.unwrap().workspace;
        workspaces
            .publish(
                &workspace,
                &owner,
                Listing {
                    description: "Coffee and tea".into(),
                    default_role: WorkspaceRole::Editor,
                },
            )
            .await
            .unwrap();
        workspaces
            .list_invitation_only(&workspace, &owner, true)
            .await
            .unwrap();

        // Still a listed workspace: the site's directory names it.
        let listed = workspaces
            .directory(&std::collections::BTreeSet::new())
            .await;
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert_eq!(listed[0].name, "Lounge");

        // A stranger who holds no invitation is refused, and told why.
        let (_stranger_key, stranger) = guest_device();
        let stranger_join = DoorJoin {
            user_public_key: hex::encode(stranger.user_public_key),
            display_name: "Cy".into(),
        };
        assert_eq!(
            workspaces
                .admit_listed(&workspace, &stranger_join)
                .await
                .map(|_| ()),
            Err(DoorRefusal::InvitationOnly)
        );

        // A person a member invited is let in.
        let (invited_key, invited) = guest_device();
        workspaces
            .pre_admit(
                &owner,
                crate::workspaces::PreAdmitRequest {
                    request_id: [3; 32],
                    grant: signed_pre_admission(&workspace, b"invited-by-the-owner"),
                    recipient_user_public_key: Some(invited.user_public_key),
                    inviter_name: Some("Ky".into()),
                },
            )
            .await
            .unwrap();
        let invited_join = DoorJoin {
            user_public_key: hex::encode(invited.user_public_key),
            display_name: "Ada".into(),
        };
        let reply = walk_in_as(
            workspaces,
            &workspace,
            &invited_join,
            &invited_key,
            &invited,
        )
        .await
        .unwrap();
        assert!(matches!(reply, JoinReply::Admitted { .. }), "{reply:?}");

        // Open again, the same workspace hands a stranger in.
        workspaces
            .list_invitation_only(&workspace, &owner, false)
            .await
            .unwrap();
        let (open_key, open) = guest_device();
        let open_join = DoorJoin {
            user_public_key: hex::encode(open.user_public_key),
            display_name: "Di".into(),
        };
        let reply = walk_in_as(workspaces, &workspace, &open_join, &open_key, &open)
            .await
            .unwrap();
        assert!(matches!(reply, JoinReply::Admitted { .. }), "{reply:?}");
        relay.shutdown().await;
    }
}
