//! The workspaces this server lists as public workspaces, and the entry that lets
//! anyone in with the role their owner chose.
//!
//! Making a workspace public is one of its governance settings
//! ([`governance::SETTINGS`]): its owner, or anyone the workspace lets change its
//! settings, settles its entry ([`tilo_action_policy::EntryPolicy`]) — a listing
//! (a description and the default role), whether it takes people by invitation
//! only, and how many requests it keeps — as one change to the workspace's
//! settings. The entry is that record, kept with the workspace's governance and
//! read back from it by every host serving the workspace, so a workspace listed
//! here is listed on a person's own machine too, and a restart settles nothing
//! anew.
//!
//! The server lists its workspaces whose entry carries a listing in its directory
//! ([`Workspaces::directory`]) and answers each such workspace's entry
//! ([`Workspaces::admit_listed`]): a person who asks gets an invitation only they
//! can redeem and the one-time token of a pre-admission at the listing's default
//! role, so their redemption is admitted at once, as the workspace's owner
//! decided when they settled it. The listing is that decision; no one is asked
//! per join. The host signs each such grant with its own device key, and a
//! redemption is admitted only under a grant the owner or this host's entry
//! signed while the workspace is listed.
//!
//! Moderation is the workspace's own: a banned person is refused at the entry, the
//! workspace's moderators remove and ban through its governance, each admission
//! is a [`WorkspaceNotice::PreAdmitted`] the owner sees, and a report of the
//! workspace through its entry is a [`WorkspaceNotice::Reported`].

mod entering;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use sha2::{Digest as _, Sha256};
use tilo_action_policy::{EntryPolicy, ListingPolicy, Role, roles::governance};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::join_log::request_digest;
use tilo_server_protocol::directory::{
    DirectoryEntry, EntryJoin, EntryReport, Face, Listing, MAX_FACES, entry_path,
};
use tilo_server_protocol::workspaces::{
    IssueInvitationRequest, IssuedInvitation, WorkspaceListing,
};
use tilo_server_protocol::workspaces::{PreAdmission, WorkspaceNotice};
use tilo_user_identity::DeviceCert;

use super::authority::WorkspaceOp;
use super::{CollaborationError, GovernedWorkspace, Workspaces};

/// Where a server kept its public workspaces' listings before a workspace's
/// entry was part of its governance ([`Workspaces::adopt_listings_file`]).
const LISTINGS_FILE: &str = "listings.json";
/// How long an entry's invitation and its token stay good: time for the
/// person's device to redeem it now, not to keep it.
const ENTRY_INVITATION_NS: u64 = 10 * 60 * 1_000_000_000;
/// How an entry's admission names where the person came from, in the
/// owner's notices.
pub(crate) const ENTRY_LABEL: &str = "the public directory";

/// Why a public workspace's entry turns a person away.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryRefusal {
    /// The person may not join now; `Display` says why.
    Refused(String),
    /// The workspace's people banned the person.
    Banned,
    /// The person was removed from the workspace within the day.
    Removed,
    /// The entry admits only people invited or pre-admitted, and this person
    /// holds neither: their page asks them to be invited.
    InvitationOnly,
    /// The entry holds as many open grants as the workspace keeps: the person
    /// comes back in `retry_after_secs`.
    Full {
        /// How long until the earliest open grant expires, at least a second.
        retry_after_secs: u64,
    },
}

impl EntryRefusal {
    fn refused(reason: impl Into<String>) -> Self {
        Self::Refused(reason.into())
    }

    /// The refusal as the entry answers it, naming the wait list `entry` keeps
    /// for a workspace taking people by invitation.
    #[must_use]
    pub fn answer_with(
        &self,
        entry: Option<&tilo_server_protocol::directory::EntryWaitList>,
    ) -> tilo_server_protocol::directory::EntryRefused {
        self.answer().with_wait_list(entry)
    }

    /// The refusal as the entry answers it.
    #[must_use]
    pub fn answer(&self) -> tilo_server_protocol::directory::EntryRefused {
        use tilo_server_protocol::directory::EntryRefusalCode;
        tilo_server_protocol::directory::EntryRefused {
            code: match self {
                Self::Refused(_) => EntryRefusalCode::Refused,
                Self::Banned => EntryRefusalCode::Banned,
                Self::Removed => EntryRefusalCode::Removed,
                Self::InvitationOnly => EntryRefusalCode::InvitationOnly,
                Self::Full { .. } => EntryRefusalCode::Full,
            },
            error: self.to_string(),
            ..Default::default()
        }
    }
}

impl std::fmt::Display for EntryRefusal {
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

impl std::error::Error for EntryRefusal {}

/// What this host knows of its workspaces beyond their governance: when each
/// was last written to, and a ping after each entry that changed.
pub(super) struct Directory {
    /// When a member last wrote to each workspace, Unix milliseconds, since this
    /// server started: the directory's activity hint.
    active: std::sync::Mutex<BTreeMap<String, u64>>,
    /// The workspace UUID after each change of its entry.
    changes: tokio::sync::broadcast::Sender<String>,
}

impl Default for Directory {
    fn default() -> Self {
        Self {
            active: std::sync::Mutex::default(),
            changes: tokio::sync::broadcast::channel(16).0,
        }
    }
}

impl Directory {
    /// Someone changed something in `workspace` at `now_ms`.
    pub(super) fn touch(&self, workspace: &str, now_ms: u64) {
        self.active
            .lock_recover()
            .insert(workspace.to_owned(), now_ms);
    }

    fn last_active(&self, workspace: &str) -> Option<u64> {
        self.active.lock_recover().get(workspace).copied()
    }

    /// Someone changed `workspace`'s entry: whoever follows the directory
    /// answers it again.
    pub(super) fn announce(&self, workspace: &str) {
        let _ = self.changes.send(workspace.to_owned());
    }
}

/// The people of `members` in the workspace now, by `online` user roots: their
/// faces (at most [`MAX_FACES`]) and how many there are.
fn faces(members: &[tilo_peer_state::Member], online: &BTreeSet<[u8; 32]>) -> (Vec<Face>, u32) {
    let here: Vec<_> = members
        .iter()
        .filter(|member| {
            !member.removed && member.root().is_some_and(|root| online.contains(&root))
        })
        .collect();
    let count = u32::try_from(here.len()).unwrap_or(u32::MAX);
    let faces = here
        .into_iter()
        .take(MAX_FACES)
        .map(|member| Face {
            user_public_key: member.root().map(hex::encode).unwrap_or_default(),
            name: member.name.clone(),
        })
        .collect();
    (faces, count)
}

/// `entry`'s listing as the directory and the pages read it.
fn listing_of_entry(entry: &EntryPolicy) -> Option<Listing> {
    entry.listing.as_ref().map(|listing| Listing {
        description: listing.description.clone(),
        default_role: listing.default_role,
    })
}

fn person(hex_key: &str) -> Result<[u8; 32], CollaborationError> {
    let mut key = [0; 32];
    hex::decode_to_slice(hex_key, &mut key)
        .map_err(|_| CollaborationError::new("a person is 64 hex characters"))?;
    Ok(key)
}

impl Workspaces {
    /// Have `workspace`'s host send its records to devices when they open
    /// them (`when_opened`), as a public workspace does, or replay them to
    /// every device again. A public workspace is one anyone may walk into:
    /// what a newcomer waits for is its governance and its shared desktop,
    /// not every record its crowd ever wrote.
    /// The entry `governed` holds, as its governance records it.
    pub(super) fn entry_of(governed: &GovernedWorkspace) -> EntryPolicy {
        governed.peer().governance().governed_entry()
    }

    /// The entry `workspace`'s governance records here now: whether a directory
    /// lists it, whom it lets in, and how many requests it keeps. `None` when
    /// this host does not govern the workspace.
    pub async fn entry_policy(&self, workspace: &str) -> Option<EntryPolicy> {
        let governed = self.workspace(workspace).await.ok()?;
        let governed = governed.lock().await;
        Some(Self::entry_of(&governed))
    }

    /// Whether a directory lists `workspace` here now.
    pub async fn is_listed(&self, workspace: &str) -> bool {
        self.entry_policy(workspace)
            .await
            .is_some_and(|entry| entry.is_listed())
    }

    /// Settle `workspace`'s entry as `change` asks, as `device`: the workspace's
    /// governance must let its person change its settings
    /// ([`governance::SETTINGS`]). Answers the entry the workspace now records.
    async fn settle_entry(
        &self,
        workspace: &str,
        device: &DeviceCert,
        change: impl FnOnce(&mut EntryPolicy),
    ) -> Result<EntryPolicy, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        let key = self.inner.host.key.clone();
        let state = self.workspace(workspace).await?;
        let mut governed = state.lock().await;
        self.authorize(
            device,
            Some(&governed),
            WorkspaceOp::Govern(governance::SETTINGS),
            now,
        )?;
        let mut entry = Self::entry_of(&governed);
        change(&mut entry);
        let entry = entry
            .checked()
            .map_err(|error| CollaborationError::new(error.to_string()))?;
        if entry == Self::entry_of(&governed) {
            return Ok(entry);
        }
        let digest = request_digest(&("the workspace's entry", &entry))
            .map_err(|error| CollaborationError::new(error.to_string()))?;
        self.administer(
            &mut governed,
            rand::random(),
            digest,
            |peer| {
                peer.governance_mut()
                    .put_governed_entry(&entry, &key, super::system_time(now))
            },
            now,
        )
        .await?;
        let listed = entry.is_listed();
        drop(governed);
        self.inner.directory.announce(workspace);
        tracing::info!(%workspace, listed, "the workspace's entry settled");
        Ok(entry)
    }

    /// The content policy `workspace`'s governance records here now: the
    /// largest file a host keeps, and what one member's files may take, now
    /// and in any hour. The defaults when its owners set none. `None` when this
    /// host does not govern the workspace.
    pub async fn content_policy(
        &self,
        workspace: &str,
    ) -> Option<tilo_action_policy::ContentPolicy> {
        let governed = self.workspace(workspace).await.ok()?;
        let governed = governed.lock().await;
        Some(governed.peer().governance().content_policy())
    }

    /// Settle `workspace`'s content policy as `policy`, as `device`: the
    /// workspace's governance must let its person change its settings
    /// ([`governance::SETTINGS`]). Answers the policy the workspace now
    /// records, which every host of it obeys.
    ///
    /// # Errors
    ///
    /// Returns why `device` may not change the workspace's settings, or why
    /// the change could not be kept.
    pub async fn settle_content_policy(
        &self,
        workspace: &str,
        device: &DeviceCert,
        policy: tilo_action_policy::ContentPolicy,
    ) -> Result<tilo_action_policy::ContentPolicy, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        let key = self.inner.host.key.clone();
        let state = self.workspace(workspace).await?;
        let mut governed = state.lock().await;
        self.authorize(
            device,
            Some(&governed),
            WorkspaceOp::Govern(governance::SETTINGS),
            now,
        )?;
        if governed.peer().governance().content_policy() == policy {
            return Ok(policy);
        }
        let digest = request_digest(&("the workspace's content policy", &policy))
            .map_err(|error| CollaborationError::new(error.to_string()))?;
        self.administer(
            &mut governed,
            rand::random(),
            digest,
            |peer| {
                peer.governance_mut()
                    .put_content_policy(&policy, &key, super::system_time(now))
            },
            now,
        )
        .await?;
        Ok(policy)
    }

    /// Make `workspace` public with `listing`, as `device` asks: the workspace's
    /// governance must let its person change the workspace's settings.
    pub async fn publish(
        &self,
        workspace: &str,
        device: &DeviceCert,
        listing: Listing,
    ) -> Result<Listing, CollaborationError> {
        let settling = ListingPolicy {
            description: listing.description,
            default_role: listing.default_role,
        };
        let entry = self
            .settle_entry(workspace, device, |entry| {
                entry.listing = Some(settling);
            })
            .await?;
        listing_of_entry(&entry)
            .ok_or_else(|| CollaborationError::new("the workspace records no listing"))
    }

    /// Take `workspace` out of the directory, as `device` asks. Its members stay;
    /// nobody new gets in through its entry.
    ///
    /// # Errors
    ///
    /// As [`Self::publish`].
    pub async fn unpublish(
        &self,
        workspace: &str,
        device: &DeviceCert,
    ) -> Result<(), CollaborationError> {
        self.settle_entry(workspace, device, |entry| entry.listing = None)
            .await
            .map(drop)
    }

    /// Settle whether `workspace`'s entry admits only people invited or
    /// pre-admitted, as the host's operator asked. Its listing is untouched:
    /// the workspace stays listed either way. Asked once, it is governance: a
    /// restart reads the workspace's entry rather than settling it again.
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
        self.settle_entry(workspace, device, |entry| {
            entry.invitation_only = invites_only;
        })
        .await
        .map(drop)
    }

    /// Settle how many requests `workspace`'s entry keeps for whoever may
    /// invite, as the host's operator asked.
    ///
    /// # Errors
    ///
    /// Returns why `device` may not change the workspace's settings.
    pub async fn list_wait_list_max(
        &self,
        workspace: &str,
        device: &DeviceCert,
        keep: usize,
    ) -> Result<(), CollaborationError> {
        self.settle_entry(workspace, device, |entry| entry.wait_list_max = keep)
            .await
            .map(drop)
    }

    /// Whether `user` holds an invitation this workspace issued and has not
    /// spent: one from a member, or the one its entry hands a person it lets
    /// in. Either is bound to them, so an invitation-only entry still knows
    /// whom it let in when they come back to redeem it.
    pub(super) async fn invited(&self, workspace: &str, user: [u8; 32], now: u64) -> bool {
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

    /// The authority `workspace` records, as this service reads it: as the device it is
    /// certified as, whose one key governs the workspaces it holds and acts in the ones it
    /// joins. The one policy lookup the entry, the directory and a member's role answer from.
    pub async fn policy_of(&self, workspace: &str) -> Option<tilo_action_policy::WorkspacePolicy> {
        let reader = self.own_reader();
        let state = self.workspace(workspace).await.ok()?;
        let state = state.lock().await;
        state
            .peer()
            .permissions(reader, tilo_clock::wall())
            .ok()
            .map(|permissions| (*permissions.policy).clone())
    }

    /// `workspace`'s policy where it answers something for `agent` now: `None` while its
    /// person gave no answer for that agent, so nothing narrows it. What answers it are the
    /// grants the person set as its answers ([`tilo_action_policy::agent_answers::is_answer`]);
    /// a grant kept for one use of theirs (the "Always" of a vault item's leave) narrows
    /// nothing.
    pub async fn agent_policy(
        &self,
        workspace: &str,
        agent: &str,
    ) -> Option<tilo_action_policy::WorkspacePolicy> {
        let policy = self.policy_of(workspace).await?;
        let named = tilo_action_policy::Grantee::agent(agent.to_owned());
        let now = tilo_action_policy::Context::at_time(tilo_clock::wall());
        policy
            .grants
            .iter()
            .any(|grant| {
                grant.grantee == named
                    && grant.is_live(&now)
                    && tilo_action_policy::agent_answers::is_answer(grant)
            })
            .then_some(policy)
    }

    /// The sandbox capabilities `workspace`'s governance grants its apps: the
    /// folders they reach in the Files mounts, as every other reader of them
    /// takes them. Empty for a workspace this service does not hold.
    pub async fn sandbox_of(&self, workspace: &str) -> Vec<collab_types::caps::CapOverride> {
        let Ok(state) = self.workspace(workspace).await else {
            return Vec::new();
        };
        let state = state.lock().await;
        state
            .peer()
            .governance()
            .governed_sandbox()
            .unwrap_or_default()
    }

    /// The role `user` holds in `workspace` as one of its members now: `None`
    /// for someone it does not have in it, a person it removed, and one it
    /// banned.
    pub(super) async fn member_role(&self, workspace: &str, user: [u8; 32]) -> Option<Role> {
        self.policy_of(workspace)
            .await?
            .active_role(&hex::encode(user))
    }

    /// `workspace`'s listing, as its entry records it.
    pub async fn listing(&self, workspace: &str) -> Option<Listing> {
        listing_of_entry(&self.entry_policy(workspace).await?)
    }

    /// The workspaces a directory lists here now, by the entry each records.
    /// Unlisted workspaces cost their entry's read and nothing else: a host that
    /// keeps many of them answers the directory without reading each one's
    /// people.
    pub async fn published_workspaces(&self) -> Vec<String> {
        // Each workspace is waited for with the others let go: holding them
        // all kept every read of any workspace waiting for the busiest one.
        let held: Vec<_> = self
            .inner
            .workspaces
            .lock()
            .await
            .values()
            .cloned()
            .collect();
        let mut listed = Vec::new();
        for workspace in held {
            let workspace = workspace.lock().await;
            if Self::entry_of(&workspace).is_listed() {
                listed.push(workspace.workspace().to_owned());
            }
        }
        listed
    }

    /// Take over the listings a server kept in [`LISTINGS_FILE`] before a
    /// workspace's entry was part of its governance: each workspace that file
    /// lists, whose entry lists nothing yet, is listed again as its owner listed
    /// it, settled by this host as the workspace's governor. The file then
    /// moves beside the records as `listings.json.v1`, so going back to the
    /// build that wrote it still finds it. Nothing to do when there is none.
    ///
    /// A listing whose workspace is gone, or whose entry this host may not
    /// settle, is left out and said; the rest are still taken over.
    ///
    /// # Errors
    ///
    /// The file cannot be read, does not parse, or cannot be moved.
    pub(super) async fn adopt_listings_file(&self) -> Result<(), String> {
        #[derive(serde::Deserialize)]
        struct Kept {
            /// Each public workspace's listing, by workspace UUID.
            workspaces: BTreeMap<String, KeptListing>,
        }
        /// A listing as the file holds it.
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct KeptListing {
            description: String,
            default_role: KeptRole,
        }
        /// A role as the builds that wrote the file named it: each by its
        /// variant's own name ("Editor"), where this build's role is in snake
        /// case.
        #[derive(Clone, Copy, serde::Deserialize)]
        enum KeptRole {
            Viewer,
            Commenter,
            Editor,
            Moderator,
        }
        impl From<KeptListing> for Listing {
            fn from(kept: KeptListing) -> Self {
                Self {
                    description: kept.description,
                    default_role: match kept.default_role {
                        KeptRole::Viewer => Role::Viewer,
                        KeptRole::Commenter => Role::Commenter,
                        KeptRole::Editor => Role::Editor,
                        KeptRole::Moderator => Role::Moderator,
                    },
                }
            }
        }
        let path = self.inner.dir.join(LISTINGS_FILE);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        let kept: Kept = serde_json::from_slice(&bytes)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let host = self.inner.host.cert.clone();
        for (workspace, listing) in kept.workspaces {
            if self.entry_policy(&workspace).await.is_none() {
                tracing::warn!(%workspace, "a listing kept before the entry names a workspace this host no longer keeps");
                continue;
            }
            if self.is_listed(&workspace).await {
                continue;
            }
            match self.publish(&workspace, &host, listing.into()).await {
                Ok(_) => tracing::info!(%workspace, "listed again as it was before the entry"),
                Err(error) => {
                    tracing::error!(%workspace, %error, "a listing kept before the entry was not taken over");
                }
            }
        }
        std::fs::rename(&path, path.with_file_name(format!("{LISTINGS_FILE}.v1")))
            .map_err(|error| format!("{}: {error}", path.display()))
    }

    /// The workspace UUID after each change of a workspace's entry.
    #[must_use]
    pub fn listing_changes(&self) -> tokio::sync::broadcast::Receiver<String> {
        self.inner.directory.changes.subscribe()
    }

    /// Keep the directory's activity hint: each member write marks its workspace
    /// active, for as long as the workspaces are governed here.
    pub(super) fn follow_activity(&self) {
        let mut writes = self.document_writes();
        let workspaces = std::sync::Arc::downgrade(&self.inner);
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        runtime.spawn(async move {
            loop {
                match writes.recv().await {
                    Ok(write) => match workspaces.upgrade() {
                        Some(inner) => inner
                            .directory
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
        for workspace in self.published_workspaces().await {
            let (Ok(summary), Some(entry)) = (
                self.summary(&workspace).await,
                self.entry_policy(&workspace).await,
            ) else {
                continue;
            };
            let Some(listing) = listing_of_entry(&entry) else {
                continue;
            };
            let (here, here_count) = faces(&summary.members, online);
            let members = summary
                .members
                .iter()
                .filter(|member| !member.removed)
                .count();
            entries.push(DirectoryEntry {
                entry: entry_path(&summary.workspace),
                workspace: summary.workspace.clone(),
                name: summary.name,
                description: listing.description,
                default_role: listing.default_role,
                here,
                here_count,
                members: u32::try_from(members).unwrap_or(u32::MAX),
                last_active_ms: self.inner.directory.last_active(&summary.workspace),
                community: false,
                invitation_only: entry.invitation_only,
                // The wait list every invitation-only entry here names in its refusal.
                wait_list: self
                    .wait_list_page()
                    .filter(|_| entry.invitation_only)
                    .cloned(),
            });
        }
        entries
    }

    /// Let the person `join` names into the public workspace `workspace`: an
    /// invitation only they can redeem, and the token of a pre-admission at
    /// the listing's default role, under the owner's entry. The entry answers
    /// them with an invitation, or with a [`tilo_server_protocol::directory::EntryRefused`].
    ///
    /// A workspace whose entry takes people by invitation answers its entry only
    /// to a person it has in it (joining again from another device of theirs),
    /// one an invitation still names
    /// [`PendingInvitation::recipient_user_public_key`], or one it declared a
    /// member before their device arrived. Everyone else is turned away with
    /// the entry's own words.
    ///
    /// Someone the workspace has in it is let in at the part they hold there,
    /// never the listing's default: their device's redemption writes the
    /// invitation's part as theirs, and an entry that handed them another would
    /// change it. A device of theirs that enters on its session is admitted as
    /// theirs before it would ask this entry at all.
    ///
    /// For a person an invitation still names, the entry answers with that
    /// invitation, and records a grant for it at once so the page that made
    /// the join redeems it the way every page redeems an entry's answer: its
    /// token is the one the entry hands back. The invitation itself is the
    /// decision — its role and lifetime are the inviter's — and the entry
    /// mints nothing.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace is held while the invitation is written: a change between the two would hand out a role the workspace no longer has"
    )]
    pub async fn admit_listed(
        &self,
        workspace: &str,
        join: &EntryJoin,
    ) -> Result<(IssuedInvitation, String), EntryRefusal> {
        let refused = |error: CollaborationError| EntryRefusal::Refused(error.to_string());
        let user = person(&join.user_public_key).map_err(refused)?;
        let now = tilo_clock::unix_now_ns();
        let address = self.invitation_addr().await;
        let issued = {
            let state = self.workspace(workspace).await.map_err(refused)?;
            let mut governed = state.lock().await;
            let policy = governed
                .peer()
                .permissions(self.inner.host.cert.device_public_key, tilo_clock::wall())
                .map_err(|error| refused(CollaborationError::new(error.to_string())))?
                .policy;
            let entry = Self::entry_of(&governed);
            let Some(listing) = entry.listing.as_ref() else {
                return Err(EntryRefusal::refused("that workspace is not public"));
            };
            let member = policy.active_role(&hex::encode(user));
            // An invitation-only entry refuses a stranger who holds no
            // invitation of their own and whom the workspace does not have in
            // it, and says so: the person's page offers them the way to be
            // invited (the site's wait list) instead of a link in as a guest.
            // Someone invited into any workspace on this host is no stranger:
            // whoever invited them let them in.
            let invited = governed
                .collaboration()
                .pending(now)
                .iter()
                .any(|invitation| invitation.recipient_user_public_key == Some(user))
                || self.inner.memberships.holds_any(&user);
            if entry.invitation_only && member.is_none() && !invited {
                return Err(EntryRefusal::InvitationOnly);
            }
            if policy.is_blocked(&hex::encode(user)) {
                return Err(EntryRefusal::Banned);
            }
            if governed.removals.refuses(user, now) {
                return Err(EntryRefusal::Removed);
            }
            // A member is handed the part they hold, so that redeeming it leaves them as they
            // are; the owner holds the workspace whatever an invitation names.
            let role = member
                .filter(|role| role.assignable())
                .unwrap_or(listing.default_role);
            let token = hex::encode(rand::random::<[u8; 32]>());
            let grant = self.entry_grant(workspace, role, now, &token)?;
            // A person an invitation of the workspace's own names is handed
            // that invitation, not a new one: the inviter decided its role and
            // how long it stands, and its consent is the decision. The page
            // that joined redeems it with a token as it redeems every entry's
            // answer, so the entry records a grant for this invitation at once.
            // An invitation a member already pre-admitted carries that grant,
            // and the person redeems it with the member's own token; a page
            // that asks the entry without it is handed a fresh invitation as
            // any guest is.
            let their_own = Self::entry_of(&governed)
                .invitation_only
                .then(|| governed.invitation_for(user, now))
                .flatten()
                .filter(|issued| !governed.admissions.carries(issued.invite_id));
            let issued = if let Some(issued) = &their_own {
                governed
                    .admissions
                    .record(
                        issued.invite_id,
                        grant,
                        now,
                        self.inner.host.max_pre_admissions,
                    )
                    .map_err(|error| refused(CollaborationError::new(error)))?;
                issued.clone()
            } else {
                if let Some(retry_after_secs) = governed
                    .admissions
                    .full_for(now, self.inner.host.max_pre_admissions)
                {
                    return Err(EntryRefusal::Full { retry_after_secs });
                }
                let host = self.inner.host.cert.clone();
                let issued = self
                    .stage_issue(
                        &mut governed,
                        &host,
                        IssueInvitationRequest {
                            request_id: rand::random(),
                            role,
                            recipient_user_public_key: Some(user),
                            invitee_name: tilo_user_identity::display_name(&join.display_name)
                                .ok()
                                .map(str::to_owned),
                            expires_at_ns: grant.expires_at_ns,
                            inviter_name: None,
                        },
                        false,
                        &address,
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
            (issued, token)
        };
        self.written(workspace, true).await.map_err(refused)?;
        Ok(issued)
    }

    /// The grant the entry records for the token it hands out at `workspace`'s
    /// entry: signed by this host's device key, which the redemption checks
    /// against the host and the workspace's listing
    /// ([`Workspaces::pre_admission_signer`]).
    fn entry_grant(
        &self,
        workspace: &str,
        role: Role,
        now: u64,
        token: &str,
    ) -> Result<PreAdmission, EntryRefusal> {
        let mut grant = PreAdmission {
            workspace: workspace.to_owned(),
            role,
            token_sha256: Sha256::digest(token.as_bytes()).into(),
            label: ENTRY_LABEL.to_owned(),
            expires_at_ns: now.saturating_add(ENTRY_INVITATION_NS),
            owner_user_public_key: [0; 32],
            signature: [0; 64],
        };
        grant.sign(&self.inner.host.key).map_err(|error| {
            EntryRefusal::refused(CollaborationError::new(error.to_string()).to_string())
        })?;
        Ok(grant)
    }

    /// Whether `workspace`'s entry keeps `person` out because moderation removed
    /// them within the day.
    pub async fn removed_within_the_day(&self, workspace: &str, person: [u8; 32]) -> bool {
        let now = tilo_clock::unix_now_ns();
        match self.workspace(workspace).await {
            Ok(state) => state.lock().await.removals.refuses(person, now),
            Err(_) => false,
        }
    }

    /// Whether `grant`, recorded for an invitation to `workspace`, was signed by
    /// someone who may admit people there: the workspace's owner (this server's
    /// person), or this host's own entry while the workspace is listed.
    ///
    /// # Errors
    ///
    /// The signature does not verify, or its signer may not admit anyone.
    pub(super) fn pre_admission_signer(
        &self,
        workspace: &GovernedWorkspace,
        grant: &PreAdmission,
    ) -> Result<(), String> {
        grant.verify()?;
        let host = &self.inner.host.cert;
        if grant.owner_user_public_key == host.user_public_key {
            return Ok(());
        }
        if grant.owner_user_public_key == host.device_public_key
            && grant.label == ENTRY_LABEL
            && Self::entry_of(workspace).is_listed()
        {
            return Ok(());
        }
        Err(
            "the pre-admission is signed by neither the workspace's owner nor its host's entry"
                .into(),
        )
    }

    /// Tell the people who run the public workspace `workspace` that someone reported
    /// it: a notice its owner and moderators read beside the workspace's
    /// admissions.
    pub async fn report_listed(
        &self,
        workspace: &str,
        report: &EntryReport,
    ) -> Result<(), CollaborationError> {
        if !self.is_listed(workspace).await {
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

    fn member(key: u8, name: &str, removed: bool) -> tilo_peer_state::Member {
        let mut row = tilo_peer_state::Member::of_person(
            &tilo_peer_state::join::Person::new(hex::encode([key; 32]), name),
            Some(tilo_peer_state::join::Role::Commenter),
        );
        row.removed = removed;
        row
    }

    #[test]
    fn only_members_in_the_workspace_now_are_its_faces() {
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
}

#[cfg(test)]
pub(crate) mod entry_tests {
    use tilo_actions::remote::{
        ActionProvider as _, Caller, InvokeArgument, InvokeRequest, Pending, ReceiptStatus,
        RemoteActionTarget,
    };
    use tilo_peer_state::join::Role;
    use tilo_server_protocol::WorkspaceId;
    use tilo_server_protocol::directory::{EntryJoin, Listing};
    use tilo_server_protocol::workspaces::RemoveMemberRequest;
    use tilo_server_protocol::workspaces::{JoinReply, PreAdmission};
    use tilo_user_identity::DeviceCert;

    use super::super::actions::{PUBLISH, UNPUBLISH};
    use crate::workspaces::EntryRefusal;
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
            signed: None,
        }
    }

    /// The owner invokes the workspace's governance `verb` with `argument`.
    async fn govern(
        workspaces: &Workspaces,
        workspace: WorkspaceId,
        owner: &DeviceCert,
        request: InvokeRequest,
    ) -> serde_json::Value {
        let caller = Caller::authenticated(owner.user_public_key, owner.device_public_key);
        let receipt = {
            let host = crate::workspaces::actions::tests::host_dispatch(&workspaces);
            settled(host.in_workspace(workspace).invoke(&caller, request))
                .await
                .unwrap()
        };
        match receipt.status {
            ReceiptStatus::Completed { output } => output.value.unwrap_or_default(),
            other => panic!("governance completes at once: {other:?}"),
        }
    }

    /// A stranger asks the entry of `workspace` and redeems what it answered, as
    /// a Tilo computer does: the reply, and their certificate.
    async fn walk_in(
        workspaces: &Workspaces,
        workspace: &str,
    ) -> (Result<JoinReply, String>, DeviceCert) {
        let (_, cert) = guest_device();
        let join = EntryJoin {
            user_public_key: hex::encode(cert.user_public_key),
            display_name: "Cy".into(),
        };
        let reply = walk_in_as(workspaces, workspace, &join, &cert).await;
        (reply, cert)
    }

    pub(crate) async fn walk_in_as(
        workspaces: &Workspaces,
        workspace: &str,
        join: &EntryJoin,
        cert: &DeviceCert,
    ) -> Result<JoinReply, String> {
        let (issued, token) = workspaces
            .admit_listed(workspace, join)
            .await
            .map_err(|error| error.to_string())?;
        Ok(redeem(workspaces, issued.signed_invitation, token, cert).await)
    }

    /// Redeem `invitation` with the entry's `token`, as the person's device.
    pub(crate) async fn redeem(
        workspaces: &Workspaces,
        invitation: Vec<u8>,
        token: String,
        cert: &DeviceCert,
    ) -> JoinReply {
        workspaces
            .admit_proven(
                invitation,
                cert.clone(),
                rand::random(),
                "Cy",
                Some(token.as_bytes()),
            )
            .await
    }

    /// What a test does to an entry's grant before it is redeemed.
    type Forgery = Box<dyn FnOnce(&mut PreAdmission) + Send>;

    #[tokio::test]
    async fn a_entry_grant_with_a_forged_or_zero_signature_admits_nobody() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();
        let workspace = workspaces.create("Plaza").await.unwrap().workspace;
        workspaces
            .publish(
                &workspace,
                &owner,
                Listing {
                    description: String::new(),
                    default_role: Role::Editor,
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
            let (_, guest) = guest_device();
            let join = EntryJoin {
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
            let reply = redeem(workspaces, issued.signed_invitation, token, &guest).await;
            assert!(
                !matches!(reply, JoinReply::Admitted { .. }),
                "{what} admitted someone: {reply:?}"
            );
            let members = workspaces.summary(&workspace).await.unwrap().members;
            assert!(
                members
                    .iter()
                    .all(|member| member.root() != Some(guest.user_public_key)),
                "{what}: not a member"
            );
        }

        let (_, guest) = guest_device();
        let join = EntryJoin {
            user_public_key: hex::encode(guest.user_public_key),
            display_name: "Cy".into(),
        };
        assert!(matches!(
            walk_in_as(workspaces, &workspace, &join, &guest).await,
            Ok(JoinReply::Admitted { .. })
        ));
        server.shutdown().await;
    }

    #[tokio::test]
    async fn whoever_joins_a_public_workspace_gets_the_default_role_its_owner_set() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();
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
        assert_eq!(listed[0].default_role, Role::Commenter);

        for _ in 0..2 {
            let (reply, cert) = walk_in(workspaces, &workspace).await;
            match reply.unwrap() {
                JoinReply::Admitted { role, .. } => assert_eq!(role, Role::Commenter),
                other => panic!("the entry admits at once: {other:?}"),
            }
            let members = workspaces.summary(&workspace).await.unwrap().members;
            let joined = members
                .iter()
                .find(|member| member.root() == Some(cert.user_public_key))
                .expect("a member now");
            assert_eq!(joined.role, Some(Role::Commenter));
        }

        // Another default from now on: the next person gets it.
        workspaces
            .publish(
                &workspace,
                &owner,
                Listing {
                    description: "Talk about coffee".into(),
                    default_role: Role::Viewer,
                },
            )
            .await
            .unwrap();
        let (reply, _) = walk_in(workspaces, &workspace).await;
        assert!(
            matches!(
                reply.unwrap(),
                JoinReply::Admitted {
                    role: Role::Viewer,
                    ..
                }
            ),
            "the default is read at the entry, not when the listing was first made"
        );
        server.shutdown().await;
    }

    /// The workspace's owners set what members may post through its action,
    /// every member reads it there, and every host of the workspace obeys it.
    #[tokio::test]
    async fn the_owners_set_what_members_may_post_and_the_host_keeps_to_it() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();
        let summary = workspaces.create("Lobby").await.unwrap();
        let workspace = summary.workspace.clone();

        let policy = govern(
            workspaces,
            summary.workspace_id,
            &owner,
            invocation(
                &workspace,
                super::super::actions::SET_CONTENT_POLICY,
                serde_json::json!({
                    "file_bytes": 4_096,
                    "member_bytes": 8_192,
                    "hourly_bytes": 16_384,
                }),
            ),
        )
        .await;
        let wanted = tilo_action_policy::ContentPolicy {
            file_bytes: Some(4_096),
            member_bytes: Some(8_192),
            hourly_bytes: Some(16_384),
        };
        assert_eq!(policy["file_bytes"], 4_096);
        assert_eq!(policy["member_bytes"], 8_192);
        assert_eq!(policy["hourly_bytes"], 16_384);

        // A read is the policy the workspace records, and what a host keeping
        // its content answers from.
        let read = govern(
            workspaces,
            summary.workspace_id,
            &owner,
            invocation(
                &workspace,
                super::super::actions::CONTENT_POLICY,
                serde_json::json!({}),
            ),
        )
        .await;
        assert_eq!(read, policy);
        assert_eq!(workspaces.content_policy(&workspace).await, Some(wanted));

        // A value left out stays as it is: the owners raise one limit only.
        let raised = govern(
            workspaces,
            summary.workspace_id,
            &owner,
            invocation(
                &workspace,
                super::super::actions::SET_CONTENT_POLICY,
                serde_json::json!({ "member_bytes": 65_536 }),
            ),
        )
        .await;
        assert_eq!(raised["file_bytes"], 4_096);
        assert_eq!(raised["member_bytes"], 65_536);
        assert_eq!(raised["hourly_bytes"], 16_384);

        // A limit set to 0 is lifted: the file size is no limit again, and
        // the limits the owners did not name stand.
        let cleared = govern(
            workspaces,
            summary.workspace_id,
            &owner,
            invocation(
                &workspace,
                super::super::actions::SET_CONTENT_POLICY,
                serde_json::json!({ "file_bytes": 0 }),
            ),
        )
        .await;
        assert_eq!(cleared["file_bytes"], 0);
        assert_eq!(cleared["member_bytes"], 65_536);
        assert_eq!(cleared["hourly_bytes"], 16_384);
        assert_eq!(
            workspaces.content_policy(&workspace).await,
            Some(tilo_action_policy::ContentPolicy {
                file_bytes: None,
                member_bytes: Some(65_536),
                hourly_bytes: Some(16_384),
            }),
            "a limit lifted is recorded as none, not as a zero"
        );
        server.shutdown().await;
    }

    /// Whether `device` may write in `workspace`'s Chat now: its role lets it
    /// change Chat's content, the thing a Tilo computer checks before it opens
    /// Chat writable rather than read-only.
    async fn writes_in_chat(workspaces: &Workspaces, workspace: &str, device: &DeviceCert) -> bool {
        const CHAT: &str = tilo_app_id::apps::CHAT;
        let state = workspaces.workspace(workspace).await.unwrap();
        let now = tilo_clock::wall();
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
        )
    }

    #[tokio::test]
    async fn a_person_who_joined_a_public_workspace_writes_in_its_chat_as_their_role_allows() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();
        let workspace = workspaces.create("Lounge").await.unwrap().workspace;
        workspaces
            .publish(
                &workspace,
                &owner,
                Listing {
                    description: String::new(),
                    default_role: Role::Editor,
                },
            )
            .await
            .unwrap();
        let (reply, guest) = walk_in(workspaces, &workspace).await;
        assert!(matches!(
            reply.unwrap(),
            JoinReply::Admitted {
                role: Role::Editor,
                ..
            }
        ));
        // An Editor writes in Chat, as in everything else in the workspace.
        assert!(writes_in_chat(workspaces, &workspace, &guest).await);

        // Whether a person writes stays their role's.
        workspaces
            .publish(
                &workspace,
                &owner,
                Listing {
                    description: String::new(),
                    default_role: Role::Viewer,
                },
            )
            .await
            .unwrap();
        let (_, viewer) = walk_in(workspaces, &workspace).await;
        assert!(!writes_in_chat(workspaces, &workspace, &viewer).await);
        server.shutdown().await;
    }

    /// A public workspace prunes the guests it has not seen for long; a Tilo
    /// computer that holds the workspace comes back through the entry as the same
    /// person, with the same device, and is a member again (the shell's
    /// `readmit_site_workspace`).
    #[tokio::test]
    async fn a_pruned_guest_comes_back_through_the_entry_with_the_same_device() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();
        let workspace = workspaces.create("Lounge").await.unwrap().workspace;
        workspaces
            .publish(
                &workspace,
                &owner,
                Listing {
                    description: String::new(),
                    default_role: Role::Editor,
                },
            )
            .await
            .unwrap();
        let (_, guest) = guest_device();
        let join = EntryJoin {
            user_public_key: hex::encode(guest.user_public_key),
            display_name: "Cy".into(),
        };
        let member = |members: &[tilo_peer_state::Member]| {
            members
                .iter()
                .any(|member| member.root() == Some(guest.user_public_key) && !member.removed)
        };
        assert!(matches!(
            walk_in_as(workspaces, &workspace, &join, &guest).await,
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
                walk_in_as(workspaces, &workspace, &join, &guest).await,
                Ok(JoinReply::Admitted {
                    role: Role::Editor,
                    ..
                })
            ),
            "the same person and device are let in again"
        );
        assert!(member(
            &workspaces.summary(&workspace).await.unwrap().members
        ));
        server.shutdown().await;
    }

    /// Moderation takes a person's membership away and the entry keeps them out
    /// for the day; a ban keeps them out until a moderator lifts it, and lifting
    /// it lets them in again at once.
    #[tokio::test]
    async fn a_person_moderation_removed_is_refused_at_the_entry_for_the_day() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();
        let workspace = workspaces.create("Plaza").await.unwrap().workspace;
        workspaces
            .publish(
                &workspace,
                &owner,
                Listing {
                    description: String::new(),
                    default_role: Role::Editor,
                },
            )
            .await
            .unwrap();
        let (_, guest) = guest_device();
        let join = EntryJoin {
            user_public_key: hex::encode(guest.user_public_key),
            display_name: "Troll".into(),
        };
        assert!(matches!(
            walk_in_as(workspaces, &workspace, &join, &guest).await,
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
        assert_eq!(refused, EntryRefusal::Removed);
        assert!(
            workspaces
                .removed_within_the_day(&workspace, guest.user_public_key)
                .await
        );

        // A ban keeps them out past the day; a person only removed was never banned, so lifting
        // a ban is what lets them in again, and the entry opens with it.
        workspaces
            .ban_member(
                &workspace,
                &owner,
                RemoveMemberRequest {
                    request_id: [8; 32],
                    user_public_key: guest.user_public_key,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            workspaces
                .admit_listed(&workspace, &join)
                .await
                .unwrap_err(),
            EntryRefusal::Banned
        );
        workspaces
            .unban_member(
                &workspace,
                &owner,
                RemoveMemberRequest {
                    request_id: [9; 32],
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
        server.shutdown().await;
    }

    /// A server that kept its listings in a file of their own, before a
    /// workspace's entry was part of its governance, lists the same workspaces
    /// after the upgrade: each is listed again as its owner listed it, and the
    /// old file stays beside the records.
    #[tokio::test]
    async fn a_listing_kept_before_the_entry_lists_its_workspace_after_the_upgrade() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspace = server
            .workspaces()
            .create("Sketches")
            .await
            .unwrap()
            .workspace;
        server.shutdown().await;
        let file = dir.path().join("workspaces").join(super::LISTINGS_FILE);
        let kept = serde_json::json!({"workspaces": {
            workspace.clone(): {"description": "Open to look at", "defaultRole": "Viewer"},
            "00000000-0000-4000-8000-000000000000": {"description": "Gone", "defaultRole": "Editor"},
        }})
        .to_string();
        std::fs::write(&file, &kept).unwrap();

        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces().clone();
        assert_eq!(
            workspaces.listing(&workspace).await,
            Some(Listing {
                description: "Open to look at".into(),
                default_role: Role::Viewer,
            })
        );
        assert_eq!(
            workspaces.published_workspaces().await,
            vec![workspace.clone()]
        );
        assert!(!file.exists(), "taken over once");
        assert_eq!(
            std::fs::read_to_string(file.with_file_name("listings.json.v1")).unwrap(),
            kept,
            "the old file stays beside the records"
        );
        server.shutdown().await;

        // The workspace's entry is its governance from then on: unlisted, it
        // stays unlisted across a restart.
        let server = test_host(dir.path(), &key, &owner).await;
        server
            .workspaces()
            .unpublish(&workspace, &owner)
            .await
            .unwrap();
        server.shutdown().await;
        let server = test_host(dir.path(), &key, &owner).await;
        assert!(server.workspaces().published_workspaces().await.is_empty());
        server.shutdown().await;
    }

    /// An entry that holds as many open grants as the workspace keeps says when
    /// it has room again instead of taking another invitation.
    #[tokio::test]
    async fn a_full_entry_answers_when_it_has_room_again() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let server = test_host_with_grants(dir.path(), &key, &owner, Some(1)).await;
        let workspaces = server.workspaces();
        let workspace = workspaces.create("Plaza").await.unwrap().workspace;
        workspaces
            .publish(
                &workspace,
                &owner,
                Listing {
                    description: String::new(),
                    default_role: Role::Editor,
                },
            )
            .await
            .unwrap();
        let join = || {
            let (_, guest) = guest_device();
            EntryJoin {
                user_public_key: hex::encode(guest.user_public_key),
                display_name: "Cy".into(),
            }
        };
        workspaces
            .admit_listed(&workspace, &join())
            .await
            .expect("the entry takes the first grant");
        let refused = workspaces
            .admit_listed(&workspace, &join())
            .await
            .unwrap_err();
        let EntryRefusal::Full { retry_after_secs } = refused else {
            panic!("the full entry refused another way: {refused:?}");
        };
        assert!(
            (1..=600).contains(&retry_after_secs),
            "the earliest open grant expires within its ten minutes: {retry_after_secs}"
        );
        server.shutdown().await;
    }

    #[tokio::test]
    async fn a_private_workspace_has_no_entry_and_is_not_listed() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();
        let summary = workspaces.create("Team").await.unwrap();
        let workspace = summary.workspace.clone();
        let (reply, _) = walk_in(workspaces, &workspace).await;
        assert!(reply.is_err(), "never published: no entry");

        workspaces
            .publish(
                &workspace,
                &owner,
                Listing {
                    description: String::new(),
                    default_role: Role::Editor,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            workspaces.published_workspaces().await,
            vec![workspace.clone()]
        );
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
        assert!(reply.is_err(), "unpublished: the entry is shut again");

        assert!(
            workspaces
                .publish(
                    &workspace,
                    &owner,
                    Listing {
                        description: String::new(),
                        default_role: Role::Moderator,
                    },
                )
                .await
                .is_err()
        );
        server.shutdown().await;
    }

    #[tokio::test]
    async fn a_banned_person_is_turned_away_at_the_entry_and_others_cannot_publish() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();
        let workspace = workspaces.create("Plaza").await.unwrap().workspace;
        workspaces
            .publish(
                &workspace,
                &owner,
                Listing {
                    description: String::new(),
                    default_role: Role::Editor,
                },
            )
            .await
            .unwrap();
        let (_, guest) = guest_device();
        let join = EntryJoin {
            user_public_key: hex::encode(guest.user_public_key),
            display_name: "Troll".into(),
        };
        assert!(matches!(
            walk_in_as(workspaces, &workspace, &join, &guest).await,
            Ok(JoinReply::Admitted { .. })
        ));
        assert!(
            workspaces.unpublish(&workspace, &guest).await.is_err(),
            "an Editor who joined does not change the workspace's settings"
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
        assert!(again.is_err(), "banned: the entry refuses them");
        let reported = workspaces
            .report_listed(
                &workspace,
                &tilo_server_protocol::directory::EntryReport {
                    reporter_user_public_key: hex::encode([5; 32]),
                    reason: "spam in general".into(),
                },
            )
            .await;
        assert!(reported.is_ok(), "{reported:?}");
        server.shutdown().await;
    }

    /// A listed workspace whose entry takes people by invitation only stays
    /// listed — the site's directory names it — while a stranger who holds no
    /// invitation is told so, and a person a member invited walks in. Opening
    /// the entry again lets a stranger in as before.
    #[tokio::test]
    async fn an_invitation_only_entry_refuses_a_stranger_and_admits_an_invited_person() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();
        let workspace = workspaces.create("Lounge").await.unwrap().workspace;
        workspaces
            .publish(
                &workspace,
                &owner,
                Listing {
                    description: "Coffee and tea".into(),
                    default_role: Role::Editor,
                },
            )
            .await
            .unwrap();
        workspaces
            .list_invitation_only(&workspace, &owner, true)
            .await
            .unwrap();

        // Still a listed workspace: the site's directory names it, says it is
        // invitation-only, and names no wait list while the server offers none.
        let listed = workspaces
            .directory(&std::collections::BTreeSet::new())
            .await;
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert_eq!(listed[0].name, "Lounge");
        assert!(listed[0].invitation_only);
        assert_eq!(listed[0].wait_list, None);
        // Once it offers one, the directory names it, as the entry's refusal does: a
        // page offers it to someone with no invitation before anyone asks the entry.
        let offered = tilo_server_protocol::directory::EntryWaitList {
            contact: Some("hello@lounge.example".into()),
            mail: true,
        };
        assert!(workspaces.offer_wait_list(offered.clone()));
        let listed = workspaces
            .directory(&std::collections::BTreeSet::new())
            .await;
        assert_eq!(listed[0].wait_list, Some(offered));

        // A stranger who holds no invitation is refused, and told why.
        let (_stranger_key, stranger) = guest_device();
        let stranger_join = EntryJoin {
            user_public_key: hex::encode(stranger.user_public_key),
            display_name: "Cy".into(),
        };
        assert_eq!(
            workspaces
                .admit_listed(&workspace, &stranger_join)
                .await
                .map(|_| ()),
            Err(EntryRefusal::InvitationOnly)
        );

        // A person a member invited is let in.
        let (_, invited) = guest_device();
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
        let invited_join = EntryJoin {
            user_public_key: hex::encode(invited.user_public_key),
            display_name: "Ada".into(),
        };
        let reply = walk_in_as(workspaces, &workspace, &invited_join, &invited)
            .await
            .unwrap();
        assert!(matches!(reply, JoinReply::Admitted { .. }), "{reply:?}");

        // Open again, the same workspace hands a stranger in.
        workspaces
            .list_invitation_only(&workspace, &owner, false)
            .await
            .unwrap();
        let (_, open) = guest_device();
        let open_join = EntryJoin {
            user_public_key: hex::encode(open.user_public_key),
            display_name: "Di".into(),
        };
        let reply = walk_in_as(workspaces, &workspace, &open_join, &open)
            .await
            .unwrap();
        assert!(matches!(reply, JoinReply::Admitted { .. }), "{reply:?}");
        server.shutdown().await;
    }

    /// Someone the host's operator declares an owner comes in through an
    /// invitation-only entry with no invitation: the first owner of a workspace
    /// nobody is in yet has no one to invite them. The declaration is the
    /// workspace's governance, so it is still there after the host restarts,
    /// and the entry still lets them in.
    #[tokio::test]
    async fn an_invitation_only_entry_admits_whom_the_operator_declares() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();
        let workspace = workspaces.create("Lounge").await.unwrap().workspace;
        workspaces
            .publish(
                &workspace,
                &owner,
                Listing {
                    description: "Coffee and tea".into(),
                    default_role: Role::Editor,
                },
            )
            .await
            .unwrap();
        workspaces
            .list_invitation_only(&workspace, &owner, true)
            .await
            .unwrap();

        // Someone the operator declares an owner holds governance before any
        // device of theirs arrives, so the entry does not see a stranger.
        let (_, declared) = guest_device();
        workspaces
            .set_member_administrator(
                &workspace,
                &owner,
                rand::random(),
                declared.user_public_key,
                None,
            )
            .await
            .unwrap();
        let declared_join = EntryJoin {
            user_public_key: hex::encode(declared.user_public_key),
            display_name: "Ed".into(),
        };
        let reply = walk_in_as(workspaces, &workspace, &declared_join, &declared)
            .await
            .unwrap();
        assert!(matches!(reply, JoinReply::Admitted { .. }), "{reply:?}");
        server.shutdown().await;

        // The host starts again on the same workspace: the declaration and the
        // invitation-only entry are the workspace's own, so another person
        // still cannot walk in and the declared owner still can.
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();
        let (_, stranger) = guest_device();
        assert_eq!(
            workspaces
                .admit_listed(
                    &workspace,
                    &EntryJoin {
                        user_public_key: hex::encode(stranger.user_public_key),
                        display_name: "Fa".into(),
                    }
                )
                .await
                .map(|_| ()),
            Err(EntryRefusal::InvitationOnly),
            "the entry takes people by invitation after the restart too"
        );
        let declared_again = walk_in_as(
            workspaces,
            &workspace,
            &EntryJoin {
                user_public_key: hex::encode(declared.user_public_key),
                display_name: "Ed".into(),
            },
            &declared,
        )
        .await;
        assert!(
            matches!(&declared_again, Ok(JoinReply::Admitted { .. })),
            "the declared owner comes in after the restart: {declared_again:?}"
        );
        assert!(
            workspaces
                .listing(&workspace)
                .await
                .is_some_and(|listing| listing.description == "Coffee and tea"),
            "the listing is the workspace's too"
        );
        server.shutdown().await;
    }
}

impl Workspaces {
    /// The listing of each of `wanted`'s workspaces this server governs, taken in one pass over
    /// the registry: a person's whole list is built without a registry lock per workspace and
    /// without reading any document but the ones named.
    pub async fn listings_of(
        &self,
        wanted: &std::collections::HashSet<String>,
    ) -> std::collections::HashMap<String, WorkspaceListing> {
        let held = self.inner.workspaces.lock().await;
        let named: Vec<(String, Arc<tokio::sync::Mutex<GovernedWorkspace>>)> = held
            .iter()
            .filter(|(workspace, _)| wanted.contains(*workspace))
            .map(|(workspace, held)| (workspace.clone(), Arc::clone(held)))
            .collect();
        drop(held);
        let mut listings = std::collections::HashMap::with_capacity(named.len());
        for (workspace, held) in named {
            let listing = held.lock().await.listing();
            listings.insert(workspace, listing);
        }
        listings
    }
}
