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
use super::{CollaborationError, HostedWorkspace, Workspaces, wait_list};

/// How long an invitation the entry hands a person the workspace declared
/// stands: time for their device to redeem it now, not to keep it.
const ENTRY_INVITATION_NS: u64 = 10 * 60 * 1_000_000_000;

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

/// Whether `hosted` lists a device of `person`'s own now: one of the person's
/// devices, never a key acting for them nor a device that serves the workspace.
fn has_device_of(hosted: &HostedWorkspace, person: [u8; 32]) -> bool {
    // One read of the workspace, held across the row it reads: its peer is not locked twice.
    let peer = hosted.peer();
    let keeping: BTreeSet<[u8; 32]> = peer
        .governance()
        .keeping_devices()
        .into_iter()
        .map(|hint| hint.host.device_public_key)
        .collect();
    let their_devices: BTreeSet<[u8; 32]> = peer
        .certificates()
        .into_iter()
        .filter(|cert| {
            cert.user_public_key == person && crate::workspace_access::person_device(cert)
        })
        // A device the workspace took off its member list is no device of theirs here: the
        // person brings it back the way the workspace's rules say, not by asking the entry.
        .filter(|cert| !peer.governance().removed_from_member_list(cert))
        .map(|cert| cert.device_public_key)
        .collect();
    peer.governance()
        .governed_members(tilo_clock::wall())
        .is_ok_and(|members| {
            members
                .devices
                .get(&tilo_action_policy::MemberId::person(hex::encode(person)))
                .is_some_and(|devices| {
                    devices
                        .iter()
                        .any(|device| their_devices.contains(device) && !keeping.contains(device))
                })
        })
}

/// What a workspace's entry answers a device that asks to come in.
#[derive(Debug)]
pub enum Listed {
    /// The entry takes people who ask: the device signs its own admission
    /// under the entry the workspace records, and no host signs it in.
    Open,
    /// The workspace holds an invitation for the person, made by a member:
    /// the person redeems it as any invitation.
    Invited(IssuedInvitation),
}

impl Workspaces {
    /// Have `workspace`'s host send its records to devices when they open
    /// them (`when_opened`), as a public workspace does, or replay them to
    /// every device again. A public workspace is one anyone may walk into:
    /// what a newcomer waits for is its governance and its shared desktop,
    /// not every record its crowd ever wrote.
    /// The entry `hosted` keeps, as its governance records it.
    pub(super) fn entry_of(hosted: &HostedWorkspace) -> EntryPolicy {
        hosted.peer().governance().workspace_entry()
    }

    /// The entry `workspace`'s governance records here now: whether a directory
    /// lists it, whom it lets in, and how many requests it keeps. `None` when
    /// this device does not host the workspace.
    pub async fn entry_policy(&self, workspace: &str) -> Option<EntryPolicy> {
        let hosted = self.workspace(workspace).await.ok()?;
        let hosted = hosted.lock().await;
        Some(Self::entry_of(&hosted))
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
        let mut hosted = state.lock().await;
        self.authorize(
            device,
            Some(&hosted),
            WorkspaceOp::Govern(governance::SETTINGS),
            now,
        )?;
        let mut entry = Self::entry_of(&hosted);
        change(&mut entry);
        let entry = entry
            .checked()
            .map_err(|error| CollaborationError::new(error.to_string()))?;
        if entry == Self::entry_of(&hosted) {
            return Ok(entry);
        }
        let digest = request_digest(&("the workspace's entry", &entry))
            .map_err(|error| CollaborationError::new(error.to_string()))?;
        self.administer(
            &mut hosted,
            rand::random(),
            digest,
            |peer| {
                peer.governance_mut()
                    .put_workspace_entry(&entry, &key, super::system_time(now))
            },
            now,
        )
        .await?;
        let listed = entry.is_listed();
        drop(hosted);
        self.inner.directory.announce(workspace);
        tracing::info!(%workspace, listed, "the workspace's entry settled");
        Ok(entry)
    }

    /// The content policy `workspace`'s governance records here now: the
    /// largest file a host keeps, and what one member's files may take, now
    /// and in any hour. The defaults when its owners set none. `None` when this
    /// device does not host the workspace.
    pub async fn content_policy(
        &self,
        workspace: &str,
    ) -> Option<tilo_action_policy::ContentPolicy> {
        let hosted = self.workspace(workspace).await.ok()?;
        let hosted = hosted.lock().await;
        Some(hosted.peer().governance().content_policy())
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
        let mut hosted = state.lock().await;
        self.authorize(
            device,
            Some(&hosted),
            WorkspaceOp::Govern(governance::SETTINGS),
            now,
        )?;
        if hosted.peer().governance().content_policy() == policy {
            return Ok(policy);
        }
        let digest = request_digest(&("the workspace's content policy", &policy))
            .map_err(|error| CollaborationError::new(error.to_string()))?;
        self.administer(
            &mut hosted,
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

    /// Found `workspace`'s entry as `entry`, as `device`: where a directory lists
    /// it and what it says, whom the entry lets in, and how many requests it keeps
    /// — one change, which the workspace's governance records. What a deployment
    /// names when its founder makes the workspace; an owner changes it later
    /// through the same setting ([`Self::publish`], [`Self::unpublish`],
    /// [`Self::list_invitation_only`]).
    ///
    /// # Errors
    ///
    /// Returns why `device` may not change the workspace's settings, or why the
    /// entry cannot stand.
    pub async fn found_entry(
        &self,
        workspace: &str,
        device: &DeviceCert,
        entry: EntryPolicy,
    ) -> Result<EntryPolicy, CollaborationError> {
        self.settle_entry(workspace, device, |current| *current = entry)
            .await
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
            // The wait list's settings stay on the entry, dormant: only an
            // entry that takes people by invitation offers or serves one, so
            // opening the entry and closing it again leaves them in force.
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
    /// certified as, whose one key administers the workspaces it keeps and acts in the ones it
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

    /// `workspace`'s policy where `person` answers something for their agent `agent` now:
    /// `None` while they gave it no answer, so nothing narrows it
    /// ([`tilo_action_policy::WorkspacePolicy::answers_narrow`]).
    pub async fn agent_policy(
        &self,
        workspace: &str,
        agent: &str,
        person: &str,
    ) -> Option<tilo_action_policy::WorkspacePolicy> {
        let policy = self.policy_of(workspace).await?;
        let now = tilo_action_policy::Context::at_time(tilo_clock::wall());
        policy.answers_narrow(agent, person, &now).then_some(policy)
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
            .member_sandbox()
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

    /// The workspace UUID after each change of a workspace's entry.
    #[must_use]
    pub fn listing_changes(&self) -> tokio::sync::broadcast::Receiver<String> {
        self.inner.directory.changes.subscribe()
    }

    /// Keep the directory's activity hint: each member write marks its workspace
    /// active, for as long as the workspaces are hosted here.
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
                // The wait list an entry here names in its refusal.
                wait_list: wait_list::offered(&entry),
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
    ) -> Result<Listed, EntryRefusal> {
        let refused = |error: CollaborationError| EntryRefusal::Refused(error.to_string());
        let user = person(&join.user_public_key).map_err(refused)?;
        let now = tilo_clock::unix_now_ns();
        let address = self.invitation_addr().await;
        let mut minted = false;
        let listed = {
            let state = self.workspace(workspace).await.map_err(refused)?;
            let mut hosted = state.lock().await;
            let policy = hosted
                .peer()
                .permissions(self.inner.host.cert.device_public_key, tilo_clock::wall())
                .map_err(|error| refused(CollaborationError::new(error.to_string())))?
                .policy;
            let entry = Self::entry_of(&hosted);
            if entry.listing.is_none() {
                return Err(EntryRefusal::refused("that workspace is not public"));
            }
            if policy.is_blocked(&hex::encode(user)) {
                return Err(EntryRefusal::Banned);
            }
            if hosted.removals.refuses(user, now) {
                return Err(EntryRefusal::Removed);
            }
            let part = policy.active_role(&hex::encode(user));
            // An open entry admits strangers, so a person whose device is
            // already in the workspace brings another device in as theirs, on
            // the device that is in. An invitation-only entry takes only whom
            // the workspace declares or invites, and a declared person's
            // devices come in at the part the workspace gave them.
            if !entry.invitation_only && part.is_some() && has_device_of(&hosted, user) {
                return Err(EntryRefusal::refused(
                    tilo_peer_state::workspace::membership::ENTRY_NAMES_ANOTHER_DEVICE,
                ));
            }
            // An invitation-only entry refuses a stranger with no invitation of
            // their own, and says so: the person's page offers them the way to
            // be invited (the site's wait list) instead of a link in as a
            // guest. The invitation a member made is the decision when nobody
            // declared them here.
            if entry.invitation_only && part.is_none() {
                let Some(issued) = hosted.invitation_for(user, now) else {
                    return Err(EntryRefusal::InvitationOnly);
                };
                return Ok(Listed::Invited(issued));
            }
                    return Err(EntryRefusal::InvitationOnly);
                };
                return Ok(Listed::Invited(issued));
            }
            if let Some(part) = part {
                // Someone the operator declared here comes in at the part
                // they were given, not at the role the entry hands a
                // stranger: the workspace invites them at it, as a member
                // would, rather than the entry changing their role.
                let invited = hosted.invitation_for(user, now);
                let issued = if let Some(issued) = invited {
                    issued
                } else {
                    let role = match part {
                        Role::Viewer | Role::Commenter => part,
                        _ => Role::Editor,
                    };
                    let request = IssueInvitationRequest {
                        request_id: rand::random(),
                        role,
                        recipient_user_public_key: Some(user),
                        invitee_name: tilo_user_identity::display_name(&join.display_name)
                            .ok()
                            .map(str::to_owned),
                        expires_at_ns: now.saturating_add(ENTRY_INVITATION_NS),
                        inviter_name: self
                            .member_name_by_root(&hosted, self.inner.host.cert.user_public_key),
                    };
                    let issued = self
                        .stage_issue(
                            &mut hosted,
                            &self.inner.host.cert,
                            request,
                            false,
                            &address,
                            now,
                        )
                        .map_err(refused)?;
                    minted = true;
                    issued
                };
                Listed::Invited(issued)
            } else {
                Listed::Open
            }
        };
        if minted {
            self.written(workspace, true).await.map_err(refused)?;
        }
        Ok(listed)
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
    pub(super) fn pre_admission_signer(&self, grant: &PreAdmission) -> Result<(), String> {
        grant.verify()?;
        let host = &self.inner.host.cert;
        if grant.owner_user_public_key == host.user_public_key {
            return Ok(());
        }
        Err("the pre-admission is not signed by the workspace's owner".into())
    }

    /// `workspace`'s governance as the entry hands it to a device that signs
    /// its own admission: what a person's device writes its row and its entry
    /// admission on. The bytes grant nothing; they are what every entry answer
    /// carries to the device it lets sign, and what this host's own surfaces
    /// (its pages, its tests) hand the same way.
    ///
    /// # Errors
    ///
    /// This host holds no such workspace, or its governance does not encode.
    pub async fn entry_signed_changes(&self, workspace: &str) -> Result<Vec<Vec<u8>>, String> {
        use tilo_peer_state::join_log::GovernanceMessages as _;
        self.workspace(workspace)
            .await
            .map_err(|error| error.to_string())?
            .lock()
            .await
            .peer()
            .governance_messages()
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
        let hosted = self.workspace(workspace).await?;
        let mut hosted = hosted.lock().await;
        tracing::warn!(%workspace, reporter = %hex::encode(reporter), %reason, "a public workspace was reported");
        hosted
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
    use ed25519_dalek::SigningKey;
    use tilo_action_policy::{EntryPolicy, ListingPolicy};
    use tilo_actions::remote::{
        ActionProvider as _, Caller, InvokeArgument, InvokeRequest, Pending, ReceiptStatus,
        RemoteActionTarget,
    };
    use tilo_peer_state::join::Role;
    use tilo_server_protocol::WorkspaceId;
    use tilo_server_protocol::directory::{EntryJoin, Listing};
    use tilo_server_protocol::workspaces::RemoveMemberRequest;
    use tilo_server_protocol::workspaces::{JoinReply, WorkspacesReply, WorkspacesRequest};
    use tilo_user_identity::{DeviceCapabilities, DeviceCert};

    use super::super::actions::{PUBLISH, UNPUBLISH};
    use super::Listed;
    use crate::workspaces::EntryRefusal;
    use crate::workspaces::Workspaces;
    use crate::workspaces::fixtures::signed_pre_admission;
    use crate::workspaces::tests::{device_cert, guest_device, hosting_device, test_host};
    use crate::workspaces::{AddMemberRequest, MemberPlace};

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
            let host = crate::workspaces::actions::tests::host_dispatch(workspaces);
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
        let (key, cert) = guest_device();
        let join = EntryJoin {
            user_public_key: hex::encode(cert.user_public_key),
            display_name: "Cy".into(),
        };
        let reply = walk_in_as(workspaces, workspace, &join, (&key, &cert)).await;
        (reply, cert)
    }

    pub(crate) async fn walk_in_as(
        workspaces: &Workspaces,
        workspace: &str,
        join: &EntryJoin,
        (key, cert): (&SigningKey, &DeviceCert),
    ) -> Result<JoinReply, String> {
        let listed = workspaces
            .admit_listed(workspace, join)
            .await
            .map_err(|error| error.to_string())?;
        let request_id = rand::random();
        match listed {
            Listed::Invited(issued) => Ok(workspaces
                .admit_proven(
                    issued.signed_invitation,
                    (cert.clone(), key),
                    request_id,
                    "Cy",
                    None,
                )
                .await),
            Listed::Open => {
                let signed = workspaces.entry_signed_changes(workspace).await?;
                let admission = tilo_peer_state::own_admission::sign_entry_admission(
                    workspace,
                    &signed,
                    cert,
                    request_id,
                    Some("Cy"),
                    key,
                    tilo_clock::wall(),
                )
                .await?;
                Ok(workspaces
                    .admit_entered(cert, workspace, request_id, &admission)
                    .await)
            }
        }
    }

    /// The entry an owner published open lets a guest in at its listed role,
    /// with its own device's signed admission and nothing the host signs.
    #[tokio::test]
    async fn an_open_entry_admits_a_guest_at_the_role_it_lists() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = hosting_device();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();
        let workspace = workspaces.create("Lounge").await.unwrap().workspace;
        workspaces
            .publish(
                &workspace,
                &owner,
                Listing {
                    description: "Everyone".into(),
                    default_role: Role::Commenter,
                },
            )
            .await
            .unwrap();
        let (guest_key, guest) = guest_device();
        let join = EntryJoin {
            user_public_key: hex::encode(guest.user_public_key),
            display_name: "Ada".into(),
        };
        let reply = walk_in_as(workspaces, &workspace, &join, (&guest_key, &guest))
            .await
            .unwrap();
        assert!(
            matches!(reply, JoinReply::Admitted { role, .. } if role == Role::Commenter),
            "the guest comes in at the entry's role: {reply:?}"
        );
        let joined = workspaces
            .summary(&workspace)
            .await
            .unwrap()
            .members
            .into_iter()
            .find(|member| member.root() == Some(guest.user_public_key))
            .expect("the workspace holds the guest now");
        assert_eq!(joined.role, Some(Role::Commenter));
        server.shutdown().await;
    }

    /// A person the workspace already has in it asks the entry from a second
    /// device: they are told, in words they can act on, to add it from the
    /// device that is already in, because the entry admits strangers and not
    /// a person who is here already.
    #[tokio::test]
    async fn a_second_device_is_told_to_be_added_from_the_one_already_in() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = hosting_device();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();
        let workspace = workspaces.create("Lounge").await.unwrap().workspace;
        workspaces
            .publish(
                &workspace,
                &owner,
                Listing {
                    description: "Everyone".into(),
                    default_role: Role::Commenter,
                },
            )
            .await
            .unwrap();
        // The person's first device walks in through the open entry.
        let root = SigningKey::from_bytes(&rand::random());
        let (first_key, first) = device_cert(&root, "first", DeviceCapabilities::member());
        let first_join = EntryJoin {
            user_public_key: hex::encode(first.user_public_key),
            display_name: "Ada".into(),
        };
        assert!(
            matches!(
                walk_in_as(workspaces, &workspace, &first_join, (&first_key, &first)).await,
                Ok(JoinReply::Admitted { .. })
            ),
            "the person's first device comes in"
        );
        // A second device of the same person asks the entry: it is told to be
        // added from the device that is in.
        let (second_key, second) = device_cert(&root, "second", DeviceCapabilities::member());
        let join = EntryJoin {
            user_public_key: hex::encode(second.user_public_key),
            display_name: "Ada".into(),
        };
        let refused = workspaces.admit_listed(&workspace, &join).await;
        let Err(EntryRefusal::Refused(said)) = refused else {
            panic!("a member's second device asks the entry: {refused:?}");
        };
        assert!(
            said.contains("as one of yours") && said.contains("already in"),
            "the person reads what to do instead: {said}"
        );
        let _ = (second_key, second);
        server.shutdown().await;
    }

    /// An admission signed while the entry was open is refused once its owner
    /// closes it: the entry the workspace records now is the grant.
    #[tokio::test]
    async fn an_admission_signed_before_the_entry_closed_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = hosting_device();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();
        let workspace = workspaces.create("Lounge").await.unwrap().workspace;
        workspaces
            .publish(
                &workspace,
                &owner,
                Listing {
                    description: "Everyone".into(),
                    default_role: Role::Commenter,
                },
            )
            .await
            .unwrap();
        let (guest_key, guest) = guest_device();
        let request_id = rand::random();
        let signed = workspaces.entry_signed_changes(&workspace).await.unwrap();
        let admission = tilo_peer_state::own_admission::sign_entry_admission(
            &workspace,
            &signed,
            &guest,
            request_id,
            Some("Ada"),
            &guest_key,
            tilo_clock::wall(),
        )
        .await
        .unwrap();
        workspaces
            .list_invitation_only(&workspace, &owner, true)
            .await
            .unwrap();
        let reply = workspaces
            .admit_entered(&guest, &workspace, request_id, &admission)
            .await;
        let JoinReply::Refused(said) = reply else {
            panic!("a closed entry admits nobody: {reply:?}");
        };
        assert!(
            said.contains("does not take people who ask"),
            "the refusal names the entry: {said}"
        );
        server.shutdown().await;
    }

    /// A guest admitted while the entry was open is a member still after its
    /// owner closes the entry and a fresh host reads the workspace from its
    /// files: replay judges the admission by the entry its own place held,
    /// not by the entry at the head.
    #[tokio::test]
    async fn a_guest_admitted_while_open_stays_a_member_after_the_owner_closes_the_entry() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = hosting_device();
        let workspace;
        let guest_person;
        {
            let server = test_host(dir.path(), &key, &owner).await;
            let workspaces = server.workspaces();
            workspace = workspaces.create("Lounge").await.unwrap().workspace;
            workspaces
                .publish(
                    &workspace,
                    &owner,
                    Listing {
                        description: "Everyone".into(),
                        default_role: Role::Commenter,
                    },
                )
                .await
                .unwrap();
            let (guest_key, guest) = guest_device();
            guest_person = guest.user_public_key;
            let join = EntryJoin {
                user_public_key: hex::encode(guest_person),
                display_name: "Ada".into(),
            };
            let reply = walk_in_as(workspaces, &workspace, &join, (&guest_key, &guest))
                .await
                .unwrap();
            assert!(matches!(reply, JoinReply::Admitted { .. }), "{reply:?}");
            workspaces
                .list_invitation_only(&workspace, &owner, true)
                .await
                .unwrap();
            server.shutdown().await;
        }
        let again = test_host(dir.path(), &key, &owner).await;
        let members = again
            .workspaces()
            .summary(&workspace)
            .await
            .unwrap()
            .members;
        assert!(
            members
                .iter()
                .any(|member| member.root() == Some(guest_person) && !member.removed),
            "the guest admitted while the entry was open is a member still: {members:?}"
        );
        again.shutdown().await;
    }

    #[tokio::test]
    async fn whoever_joins_a_public_workspace_gets_the_default_role_its_owner_set() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = hosting_device();
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
        let (key, owner) = hosting_device();
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
        let (key, owner) = hosting_device();
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
        let (key, owner) = hosting_device();
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
        let (guest_key, guest) = guest_device();
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
            walk_in_as(workspaces, &workspace, &join, (&guest_key, &guest)).await,
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
                walk_in_as(workspaces, &workspace, &join, (&guest_key, &guest)).await,
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
    /// for the day; an administrator holding them again lifts a plain removal
    /// at once, and so does the person redeeming an invitation.
    #[tokio::test]
    async fn a_person_moderation_removed_is_refused_at_the_entry_for_the_day() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = hosting_device();
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
        let (guest_key, guest) = guest_device();
        let join = EntryJoin {
            user_public_key: hex::encode(guest.user_public_key),
            display_name: "Troll".into(),
        };
        assert!(matches!(
            walk_in_as(workspaces, &workspace, &join, (&guest_key, &guest)).await,
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

        // An administrator holds them again: a deliberate admission, which lifts a plain
        // removal at once, with no ban involved.
        workspaces
            .add_member(
                &workspace,
                &owner,
                AddMemberRequest {
                    request_id: [8; 32],
                    user_public_key: guest.user_public_key,
                    place: MemberPlace::Role(Role::Editor),
                    name: None,
                },
            )
            .await
            .unwrap();
        assert!(
            !workspaces
                .removed_within_the_day(&workspace, guest.user_public_key)
                .await
        );
        let reply = walk_in_as(workspaces, &workspace, &join, (&guest_key, &guest)).await;
        assert!(
            matches!(reply, Ok(JoinReply::Admitted { .. })),
            "an administrator holding them again lets their device in: {reply:?}"
        );

        an_invitation_is_let_in(workspaces, &workspace, &owner, &guest, &guest_key, &join).await;
        server.shutdown().await;
    }

    /// The owner removes the person again, and the invitation the owner answers it with lifts
    /// the removal: the person is admitted when they redeem it.
    async fn an_invitation_is_let_in(
        workspaces: &Workspaces,
        workspace: &str,
        owner: &DeviceCert,
        guest: &DeviceCert,
        guest_key: &SigningKey,
        join: &EntryJoin,
    ) {
        // A removal the person answers with an invitation is lifted by it too.
        workspaces
            .remove_member(
                workspace,
                owner,
                RemoveMemberRequest {
                    request_id: [9; 32],
                    user_public_key: guest.user_public_key,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            workspaces.admit_listed(workspace, join).await.unwrap_err(),
            EntryRefusal::Removed
        );
        let WorkspacesReply::Invitation {
            signed_invitation, ..
        } = workspaces
            .page_workspaces(
                owner,
                WorkspacesRequest::Invite {
                    workspace: workspace.to_owned(),
                    request_id: [10; 32],
                    role: Role::Editor,
                    expires_at_ns: tilo_clock::unix_now_ns().saturating_add(600_000_000_000),
                    inviter_name: None,
                    wait_list_entry: None,
                    recipient: Some(guest.user_public_key),
                    administrator: false,
                },
            )
            .await
        else {
            panic!("the owner invites the person");
        };
        assert!(matches!(
            workspaces
                .admit_proven(
                    signed_invitation,
                    (guest.clone(), guest_key),
                    rand::random(),
                    "Troll",
                    None
                )
                .await,
            JoinReply::Admitted { .. }
        ));
        assert!(
            !workspaces
                .removed_within_the_day(workspace, guest.user_public_key)
                .await
        );
    }

    /// A ban is not a removal: it is lifted only through `unban`, and neither
    /// holding the person again nor an invitation to them admits them while it
    /// stands.
    #[tokio::test]
    async fn a_banned_person_is_not_let_in_again_by_being_added_or_invited() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = hosting_device();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();
        let workspace = workspaces.create("Hearth").await.unwrap().workspace;
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
        let (guest_key, guest) = guest_device();
        let join = EntryJoin {
            user_public_key: hex::encode(guest.user_public_key),
            display_name: "Troll".into(),
        };
        assert!(matches!(
            walk_in_as(workspaces, &workspace, &join, (&guest_key, &guest)).await,
            Ok(JoinReply::Admitted { .. })
        ));
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

        let held = workspaces
            .add_member(
                &workspace,
                &owner,
                AddMemberRequest {
                    request_id: [8; 32],
                    user_public_key: guest.user_public_key,
                    place: MemberPlace::Role(Role::Editor),
                    name: None,
                },
            )
            .await;
        assert!(held.is_err(), "a banned person is not held again: {held:?}");

        let WorkspacesReply::Invitation {
            signed_invitation, ..
        } = workspaces
            .page_workspaces(
                &owner,
                WorkspacesRequest::Invite {
                    workspace: workspace.clone(),
                    request_id: [9; 32],
                    role: Role::Editor,
                    expires_at_ns: tilo_clock::unix_now_ns().saturating_add(600_000_000_000),
                    inviter_name: None,
                    wait_list_entry: None,
                    recipient: Some(guest.user_public_key),
                    administrator: false,
                },
            )
            .await
        else {
            panic!("the owner invites the person");
        };
        let reply = workspaces
            .admit_proven(
                signed_invitation,
                (guest.clone(), &guest_key),
                rand::random(),
                "Troll",
                None,
            )
            .await;
        assert!(
            matches!(reply, JoinReply::Refused(_)),
            "an invitation admits no banned person: {reply:?}"
        );
        assert_eq!(
            workspaces
                .admit_listed(&workspace, &join)
                .await
                .unwrap_err(),
            EntryRefusal::Banned
        );
        let members = workspaces.summary(&workspace).await.unwrap().members;
        assert!(
            !members
                .iter()
                .any(|member| member.root() == Some(guest.user_public_key) && !member.removed),
            "still no member"
        );
        server.shutdown().await;
    }

    #[tokio::test]
    async fn a_private_workspace_has_no_entry_and_is_not_listed() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = hosting_device();
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
        let (key, owner) = hosting_device();
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
        let (guest_key, guest) = guest_device();
        let join = EntryJoin {
            user_public_key: hex::encode(guest.user_public_key),
            display_name: "Troll".into(),
        };
        assert!(matches!(
            walk_in_as(workspaces, &workspace, &join, (&guest_key, &guest)).await,
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
        let (key, owner) = hosting_device();
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

        the_directory_names_an_invitation_only_entry(workspaces, &workspace, &owner).await;
        a_stranger_is_refused_and_an_invited_person_walks_in(workspaces, &workspace, &owner).await;
        opening_the_entry_again_lets_a_stranger_in(workspaces, &workspace, &owner).await;

        server.shutdown().await;
    }

    /// Still a listed workspace: the site's directory names it, says it is invitation-only, and
    /// names no wait list while its entry keeps none. Once its entry keeps one, the directory
    /// names it, as the entry's refusal does: a page offers it to someone with no invitation
    /// before anyone asks the entry.
    async fn the_directory_names_an_invitation_only_entry(
        workspaces: &Workspaces,
        workspace: &str,
        owner: &DeviceCert,
    ) {
        let listed = workspaces
            .directory(&std::collections::BTreeSet::new())
            .await;
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert_eq!(listed[0].name, "Lounge");
        assert!(listed[0].invitation_only);
        assert_eq!(listed[0].wait_list, None);
        workspaces
            .found_entry(
                workspace,
                owner,
                EntryPolicy {
                    listing: Some(ListingPolicy {
                        description: "Coffee and tea".into(),
                        default_role: Role::Editor,
                    }),
                    invitation_only: true,
                    wait_list_max: 4,
                    contact: Some("hello@lounge.example".into()),
                    mail: Some(tilo_action_policy::EntryMail {
                        from: "lounge@example.com".into(),
                        templates: None,
                    }),
                },
            )
            .await
            .unwrap();
        let offered = tilo_server_protocol::directory::EntryWaitList {
            contact: Some("hello@lounge.example".into()),
            mail: true,
        };
        let listed = workspaces
            .directory(&std::collections::BTreeSet::new())
            .await;
        assert_eq!(listed[0].wait_list, Some(offered));
    }

    /// A stranger who holds no invitation is refused, and told why; a person a member invited is
    /// let in.
    async fn a_stranger_is_refused_and_an_invited_person_walks_in(
        workspaces: &Workspaces,
        workspace: &str,
        owner: &DeviceCert,
    ) {
        let (_stranger_key, stranger) = guest_device();
        let stranger_join = EntryJoin {
            user_public_key: hex::encode(stranger.user_public_key),
            display_name: "Cy".into(),
        };
        assert_eq!(
            workspaces
                .admit_listed(workspace, &stranger_join)
                .await
                .map(|_| ()),
            Err(EntryRefusal::InvitationOnly)
        );

        let (invited_key, invited) = guest_device();
        workspaces
            .pre_admit(
                owner,
                crate::workspaces::PreAdmitRequest {
                    request_id: [3; 32],
                    grant: signed_pre_admission(workspace, b"invited-by-the-owner"),
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
        let reply = walk_in_as(
            workspaces,
            workspace,
            &invited_join,
            (&invited_key, &invited),
        )
        .await
        .unwrap();
        assert!(matches!(reply, JoinReply::Admitted { .. }), "{reply:?}");
    }

    /// Open again, the same workspace hands a stranger in.
    async fn opening_the_entry_again_lets_a_stranger_in(
        workspaces: &Workspaces,
        workspace: &str,
        owner: &DeviceCert,
    ) {
        workspaces
            .list_invitation_only(workspace, owner, false)
            .await
            .unwrap();
        let (open_key, open) = guest_device();
        let open_join = EntryJoin {
            user_public_key: hex::encode(open.user_public_key),
            display_name: "Di".into(),
        };
        let reply = walk_in_as(workspaces, workspace, &open_join, (&open_key, &open))
            .await
            .unwrap();
        assert!(matches!(reply, JoinReply::Admitted { .. }), "{reply:?}");
    }

    /// An entry that takes people by invitation keeps its wait list while it
    /// is open, dormant, and offers it again once it goes back: the owner
    /// keeps the contact, sender and templates they configured. Only an entry
    /// that takes people by invitation offers a wait list, and a settings
    /// change does change the entry's digest.
    #[tokio::test]
    async fn opening_an_entry_leaves_its_owner_contact_and_mail_in_force() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = hosting_device();
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
        let settled = workspaces
            .found_entry(
                &workspace,
                &owner,
                EntryPolicy {
                    listing: Some(ListingPolicy {
                        description: "Coffee and tea".into(),
                        default_role: Role::Editor,
                    }),
                    invitation_only: true,
                    wait_list_max: 4,
                    contact: Some("hello@lounge.example".into()),
                    mail: Some(tilo_action_policy::EntryMail {
                        from: "lounge@example.com".into(),
                        templates: Some("/srv/mail".into()),
                    }),
                },
            )
            .await
            .unwrap();
        let offered = tilo_server_protocol::directory::EntryWaitList {
            contact: Some("hello@lounge.example".into()),
            mail: true,
        };
        let listed = workspaces
            .directory(&std::collections::BTreeSet::new())
            .await;
        assert_eq!(listed[0].wait_list, Some(offered.clone()));
        let settled_digest = tilo_peer_state::entry_digest(&settled).unwrap();

        // A settings change is a different entry: what a guest admission
        // named is the entry it was let in under.
        workspaces
            .list_wait_list_max(&workspace, &owner, 5)
            .await
            .unwrap();
        let changed = workspaces.entry_policy(&workspace).await.unwrap();
        assert_ne!(
            tilo_peer_state::entry_digest(&changed).unwrap(),
            settled_digest
        );
        workspaces
            .list_wait_list_max(&workspace, &owner, 4)
            .await
            .unwrap();

        // Open: the settings stay, and the entry offers no wait list.
        workspaces
            .list_invitation_only(&workspace, &owner, false)
            .await
            .unwrap();
        let open = workspaces.entry_policy(&workspace).await.unwrap();
        assert!(!open.invitation_only);
        assert!(!open.keeps_requests());
        assert_eq!(open.wait_list_max, 4);
        assert_eq!(open.contact.as_deref(), Some("hello@lounge.example"));
        assert_eq!(
            open.mail
                .as_ref()
                .map(|mail| (mail.from.as_str(), mail.templates.as_deref())),
            Some(("lounge@example.com", Some("/srv/mail")))
        );
        assert_eq!(workspaces.waiting_offer(&workspace).await, None);
        let listed = workspaces
            .directory(&std::collections::BTreeSet::new())
            .await;
        assert_eq!(listed[0].wait_list, None);

        // Back to invitation-only: the same settings are in force and the
        // wait list is offered again, as the entry every admission names.
        workspaces
            .list_invitation_only(&workspace, &owner, true)
            .await
            .unwrap();
        let again = workspaces.entry_policy(&workspace).await.unwrap();
        assert_eq!(again, settled);
        assert_eq!(
            tilo_peer_state::entry_digest(&again).unwrap(),
            settled_digest
        );
        assert_eq!(
            workspaces.waiting_offer(&workspace).await,
            Some(offered.clone())
        );
        let listed = workspaces
            .directory(&std::collections::BTreeSet::new())
            .await;
        assert_eq!(listed[0].wait_list, Some(offered));
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
        let (key, owner) = hosting_device();
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
        let (declared_key, declared) = guest_device();
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
        let reply = walk_in_as(
            workspaces,
            &workspace,
            &declared_join,
            (&declared_key, &declared),
        )
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
            (&declared_key, &declared),
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

    /// The entry a deployment names founds the workspace's: `open` lists it and
    /// lets anyone who asks in, at the role the founder chose.
    #[tokio::test]
    async fn a_kept_workspace_founded_open_admits_a_guest_at_the_default_role() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = hosting_device();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();
        let workspace = workspaces.create("Lounge").await.unwrap().workspace;
        let settled = workspaces
            .found_entry(
                &workspace,
                &owner,
                EntryPolicy {
                    listing: Some(ListingPolicy {
                        description: "Coffee and tea".into(),
                        default_role: Role::Commenter,
                    }),
                    invitation_only: false,
                    wait_list_max: 0,
                    contact: None,
                    mail: None,
                },
            )
            .await
            .expect("the founder settles the entry");
        assert!(
            settled
                .listing
                .as_ref()
                .is_some_and(|listing| !listing.description.is_empty()),
            "{settled:?}"
        );

        let (guest_key, guest) = guest_device();
        let reply = walk_in_as(
            workspaces,
            &workspace,
            &EntryJoin {
                user_public_key: hex::encode(guest.user_public_key),
                display_name: "Ada".into(),
            },
            (&guest_key, &guest),
        )
        .await;
        assert!(
            matches!(&reply, Ok(JoinReply::Admitted { .. })),
            "a guest who asks walks into an open entry: {reply:?}"
        );
        assert_eq!(
            workspaces
                .member_role(&workspace, guest.user_public_key)
                .await,
            Some(Role::Commenter),
            "the guest holds the role the entry hands out"
        );
        server.shutdown().await;
    }

    /// Founded `invite`, the same workspace lists itself and refuses a stranger
    /// while a person its owner invited walks in.
    #[tokio::test]
    async fn a_kept_workspace_founded_invite_refuses_a_guest_and_admits_an_invited_person() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = hosting_device();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();
        let workspace = workspaces.create("Lounge").await.unwrap().workspace;
        workspaces
            .found_entry(
                &workspace,
                &owner,
                EntryPolicy {
                    listing: Some(ListingPolicy {
                        description: "Coffee and tea".into(),
                        default_role: Role::Editor,
                    }),
                    invitation_only: true,
                    wait_list_max: 0,
                    contact: None,
                    mail: None,
                },
            )
            .await
            .expect("the founder settles the entry");

        let listed = workspaces
            .directory(&std::collections::BTreeSet::new())
            .await;
        assert_eq!(listed.len(), 1, "the workspace is listed: {listed:?}");
        assert!(listed[0].invitation_only);

        let (_, guest) = guest_device();
        let refused = workspaces
            .admit_listed(
                &workspace,
                &EntryJoin {
                    user_public_key: hex::encode(guest.user_public_key),
                    display_name: "Cy".into(),
                },
            )
            .await;
        assert_eq!(
            refused.map(|_| ()),
            Err(EntryRefusal::InvitationOnly),
            "a stranger is refused an entry that takes people by invitation"
        );

        let (invited_key, invited) = guest_device();
        workspaces
            .pre_admit(
                &owner,
                crate::workspaces::PreAdmitRequest {
                    request_id: [4; 32],
                    grant: signed_pre_admission(&workspace, b"invited-by-the-owner"),
                    recipient_user_public_key: Some(invited.user_public_key),
                    inviter_name: Some("Ky".into()),
                },
            )
            .await
            .unwrap();
        let reply = walk_in_as(
            workspaces,
            &workspace,
            &EntryJoin {
                user_public_key: hex::encode(invited.user_public_key),
                display_name: "Ada".into(),
            },
            (&invited_key, &invited),
        )
        .await;
        assert!(
            matches!(&reply, Ok(JoinReply::Admitted { .. })),
            "a person its owner invited walks in: {reply:?}"
        );
        server.shutdown().await;
    }

    /// The entry a workspace is founded with is its first setting, not its last:
    /// a change its owner makes later is what every host reads, and a host that
    /// starts again reads the workspace's entry rather than founding it anew.
    #[tokio::test]
    async fn a_kept_workspace_keeps_its_owners_later_entry_change_across_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = hosting_device();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();
        let workspace = workspaces.create("Lounge").await.unwrap().workspace;
        workspaces
            .found_entry(
                &workspace,
                &owner,
                EntryPolicy {
                    listing: Some(ListingPolicy {
                        description: "As it was founded".into(),
                        default_role: Role::Editor,
                    }),
                    invitation_only: false,
                    wait_list_max: 0,
                    contact: None,
                    mail: None,
                },
            )
            .await
            .expect("the founder settles the entry");
        // Its owner changes the entry afterwards.
        workspaces
            .publish(
                &workspace,
                &owner,
                Listing {
                    description: "As its owner left it".into(),
                    default_role: Role::Viewer,
                },
            )
            .await
            .unwrap();
        workspaces
            .list_invitation_only(&workspace, &owner, true)
            .await
            .unwrap();
        server.shutdown().await;

        let server = test_host(dir.path(), &key, &owner).await;
        let entry = server
            .workspaces()
            .entry_policy(&workspace)
            .await
            .expect("the host reads the workspace's entry");
        let listing = entry.listing.expect("the workspace is listed");
        assert_eq!(listing.description, "As its owner left it");
        assert_eq!(listing.default_role, Role::Viewer);
        assert!(entry.invitation_only, "the owner's change is what stands");
        server.shutdown().await;
    }

    /// A kept workspace's entry is settled by a device the workspace has in it: a
    /// server's own device key, which the workspace does not have as a member,
    /// settles nothing of its governance.
    #[tokio::test]
    async fn a_servers_device_key_settles_no_entry_of_a_workspace_it_only_keeps() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = hosting_device();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();
        let workspace = workspaces.create("Lounge").await.unwrap().workspace;
        let (_, server_device) = guest_device();
        let refused = workspaces
            .found_entry(
                &workspace,
                &server_device,
                EntryPolicy {
                    listing: Some(ListingPolicy {
                        description: "Ours now".into(),
                        default_role: Role::Editor,
                    }),
                    invitation_only: false,
                    wait_list_max: 0,
                    contact: None,
                    mail: None,
                },
            )
            .await;
        assert!(
            refused.is_err(),
            "a server keeping the workspace does not settle its entry: {refused:?}"
        );
        assert!(
            workspaces
                .entry_policy(&workspace)
                .await
                .is_some_and(|entry| entry.listing.is_none()),
            "the entry is as its owner left it"
        );
        server.shutdown().await;
    }
}

impl Workspaces {
    /// The listing of each of `wanted`'s workspaces this server hosts, taken in one pass over
    /// the registry: a person's whole list is built without a registry lock per workspace and
    /// without reading any document but the ones named.
    pub async fn listings_of(
        &self,
        wanted: &std::collections::HashSet<String>,
    ) -> std::collections::HashMap<String, WorkspaceListing> {
        let held = self.inner.workspaces.lock().await;
        let named: Vec<(String, Arc<tokio::sync::Mutex<HostedWorkspace>>)> = held
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
