//! Workspaces of a person's own on this host: made whenever they ask
//! ([`ServiceOperation::CreateOwnWorkspace`]), any number of them, governed here
//! like every workspace this server governs, with the person as each one's
//! administrator.
//!
//! Every host that serves persistently keeps them, with what each person may
//! keep: the bytes all of them take on this host together, their history and
//! state ([`OwnWorkspaceOffer::storage_bytes`], or what their plan sets when
//! the host knows their plan, [`Workspaces::set_own_plan`]). There is no bound
//! on how many they have. A write to a full workspace is refused
//! ([`Workspaces::storage_refusal`]), typed as the host's account refusals
//! are ([`server_account::Reason::StorageFull`]); the person's account read
//! answers how much they keep and may keep ([`Workspaces::own_account`]).
//!
//! The files members post are kept apart, bounded on their own (oldest go
//! first), and not counted here.
//!
//! Which workspaces are whose is kept beside the workspaces' records, in
//! `own-workspaces.json`, so asking again after a restart answers the same
//! ones, and a person's listing is found by their ids rather than by reading
//! every workspace this host governs. What each owner labelled theirs with
//! (`customer_id` → `cus_8121`, [`WorkspaceLabels`]), and whether they let it
//! run commands unattended ([`Workspaces::runs_unattended`]), is their
//! membership of it ([`Membership`]): a record of the owner's own workspace,
//! so nobody invited in reads the labels, and no key working in the workspace
//! turns running unattended on for itself.
//!
//! [`Membership`]: super::person_workspace::membership::Membership
//!
//! [`ServiceOperation::CreateOwnWorkspace`]: crate::service_authority::ServiceOperation::CreateOwnWorkspace

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tilo_actions::remote::server_account::{
    Limits, NextStep, NextStepKind, PlanOffer, Reason, Refusal, ServerAccount, ServerKind,
    Subscription, Usage, WorkspaceUsage,
};
use tilo_lock::MutexRecover as _;

use super::Workspaces;
use super::person_workspace::{
    self,
    membership::{self, Membership},
};
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::workspaces::WorkspaceLabels;

/// The file beside the workspaces' records that says whose each own
/// workspace is.
pub(super) const INDEX: &str = "own-workspaces.json";
/// How long a workspace's measured size is trusted before it is measured
/// again: walking its files on every write would cost more than the write.
const MEASURED_FOR: Duration = Duration::from_secs(30);
/// The plan every person with a workspace of their own here is on until the
/// host's account puts them on another.
const FREE_PLAN: &str = "free";

/// What a host gives each person who asks for workspaces of their own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnWorkspaceOffer {
    /// The host's name, as people know it (`tilo.sh`): how their account
    /// read names it.
    pub host: String,
    /// Bytes all the person's workspaces of their own may take on this host
    /// together.
    pub storage_bytes: u64,
}

impl OwnWorkspaceOffer {
    /// Ten gigabytes: what a person's own workspaces keep here.
    pub const DEFAULT_STORAGE_BYTES: u64 = 10_000_000_000;

    /// The host `host` giving each person [`Self::DEFAULT_STORAGE_BYTES`].
    #[must_use]
    pub fn free(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            storage_bytes: Self::DEFAULT_STORAGE_BYTES,
        }
    }
}

/// The own workspaces of a host that offers them.
pub(super) struct OwnWorkspaces {
    offer: OwnWorkspaceOffer,
    path: PathBuf,
    /// The person (hex root) whose own each workspace (its UUID) is: the
    /// workspaces they made, in that order.
    by_person: std::sync::Mutex<BTreeMap<String, Vec<String>>>,
    /// Whose own each workspace is, by its UUID: the same index read the
    /// other way, so a change to a workspace finds its person without
    /// looking through every person's workspaces.
    owners: std::sync::Mutex<HashMap<String, String>>,
    /// The plan each person is on here, by their hex root, when the host's
    /// account knows theirs: what they may keep rather than what the offer
    /// gives everyone.
    plans: std::sync::Mutex<BTreeMap<String, PlanOffer>>,
    /// Each own workspace's measured size, and when it was measured.
    measured: std::sync::Mutex<HashMap<String, (u64, Instant)>>,
    /// Held while a workspace is made under one name ([`Self::making`]),
    /// by that name.
    making: std::sync::Mutex<HashMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>>,
    /// The site's home workspace: the one whose entry, or an invitation into
    /// it, lets a person in to keep workspaces of their own here
    /// ([`Workspaces::admit_own_through`]). Until it is named, only the
    /// host's person makes one.
    home: std::sync::OnceLock<String>,
    /// How many times the records directory was listed: a test's measure of
    /// what a create or a read costs, with no clock to read.
    #[cfg(test)]
    listed: std::sync::atomic::AtomicUsize,
}

impl OwnWorkspaces {
    /// Those recorded beside the workspaces' records in `dir`.
    pub(super) fn open(dir: &Path, offer: OwnWorkspaceOffer) -> Result<Self, String> {
        let path = dir.join(INDEX);
        let by_person = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                // A host that kept one workspace each, before a person could
                // keep more, wrote a workspace where a list of them is read
                // now. Its file does not read: the host serves with none
                // recorded, and each person asking again is given a new one.
                tracing::warn!(%error, "no own-workspaces.json this host reads; serving with none recorded");
                BTreeMap::new()
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => return Err(error.to_string()),
        };
        let owners = owners_of(&by_person);
        Ok(Self {
            offer,
            path,
            by_person: std::sync::Mutex::new(by_person),
            owners: std::sync::Mutex::new(owners),
            plans: std::sync::Mutex::default(),
            measured: std::sync::Mutex::default(),
            making: std::sync::Mutex::default(),
            home: std::sync::OnceLock::new(),
            #[cfg(test)]
            listed: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    /// Wait until nothing else is making a workspace under `name`, and hold
    /// it while this one is made: the workspace's id when the ask names one
    /// (a retried create waits for the first and answers the workspace it
    /// made), else, for an ask that names no workspace and no name, the
    /// person (two of their devices asking for their workspace at once are
    /// answered in the order they were recorded). A named new workspace takes
    /// no lock: a backend making hundreds at once waits for none of them.
    pub(super) async fn making(&self, name: String) -> tokio::sync::OwnedMutexGuard<()> {
        let held = {
            let mut making = self.making.lock_recover();
            making.retain(|_, held| held.strong_count() > 0);
            if let Some(held) = making.get(&name).and_then(std::sync::Weak::upgrade) {
                held
            } else {
                let held = std::sync::Arc::new(tokio::sync::Mutex::new(()));
                making.insert(name, std::sync::Arc::downgrade(&held));
                held
            }
        };
        held.lock_owned().await
    }

    pub(super) const fn offer(&self) -> &OwnWorkspaceOffer {
        &self.offer
    }

    /// The workspaces of `person`'s own, in the order they made them.
    pub(super) fn of(&self, person: &[u8; 32]) -> Vec<String> {
        self.by_person
            .lock_recover()
            .get(&hex::encode(person))
            .cloned()
            .unwrap_or_default()
    }

    /// Whether `person` keeps any workspace of their own here.
    pub(super) fn any_of(&self, person: &[u8; 32]) -> bool {
        self.by_person
            .lock_recover()
            .get(&hex::encode(person))
            .is_some_and(|mine| !mine.is_empty())
    }

    /// The first workspace of `person`'s own here, in the order they made them.
    pub(super) fn first_of(&self, person: &[u8; 32]) -> Option<String> {
        self.by_person
            .lock_recover()
            .get(&hex::encode(person))
            .and_then(|mine| mine.first().cloned())
    }

    /// Whether `workspace` is one of `person`'s own.
    pub(super) fn is_of(&self, person: &[u8; 32], workspace: &str) -> bool {
        self.by_person
            .lock_recover()
            .get(&hex::encode(person))
            .is_some_and(|mine| mine.iter().any(|own| own == workspace))
    }

    /// Whose own `workspace` is, if anyone's.
    pub(super) fn owner_of(&self, workspace: &str) -> Option<[u8; 32]> {
        let person = self.owners.lock_recover().get(workspace)?.clone();
        let mut key = [0; 32];
        hex::decode_to_slice(person, &mut key).ok()?;
        Some(key)
    }

    /// Record `workspace` as another of `person`'s own, durably.
    pub(super) fn record(&self, person: &[u8; 32], workspace: &str) -> Result<(), String> {
        let mut index = self.by_person.lock_recover();
        let mut next = index.clone();
        let mine = next.entry(hex::encode(person)).or_default();
        if mine.iter().any(|own| own == workspace) {
            return Ok(());
        }
        mine.push(workspace.to_owned());
        let bytes = serde_json::to_vec(&next).map_err(|error| error.to_string())?;
        // Written before it is answered, under the lock, so two records
        // never race to the file.
        tilo_files::native::atomic::write(&self.path, &bytes).map_err(|error| error.to_string())?;
        *index = next;
        self.owners
            .lock_recover()
            .insert(workspace.to_owned(), hex::encode(person));
        drop(index);
        Ok(())
    }

    /// Forget `workspace`, deleted: it is nobody's any more, and what it
    /// took no longer counts against its person. Durably.
    pub(super) fn forget(&self, workspace: &str) -> Result<(), String> {
        let mut index = self.by_person.lock_recover();
        let mut next = index.clone();
        for mine in next.values_mut() {
            mine.retain(|own| own != workspace);
        }
        next.retain(|_, mine| !mine.is_empty());
        if next != *index {
            let bytes = serde_json::to_vec(&next).map_err(|error| error.to_string())?;
            tilo_files::native::atomic::write(&self.path, &bytes)
                .map_err(|error| error.to_string())?;
            *index = next;
        }
        drop(index);
        self.owners.lock_recover().remove(workspace);
        self.measured.lock_recover().remove(workspace);
        Ok(())
    }

    /// Put `person` on `plan` here: what their workspaces of their own may
    /// keep rather than what the offer gives everyone.
    pub(super) fn plan(&self, person: &[u8; 32], plan: PlanOffer) {
        self.plans.lock_recover().insert(hex::encode(person), plan);
    }

    /// The plan `person` is on here: theirs when the host's account knows it,
    /// else the free plan with what the offer gives everyone. What this host
    /// keeps of it is how much their own workspaces may keep: how many they
    /// have is not bounded here, so the count it may name is not answered.
    fn plan_of(&self, person: &[u8; 32]) -> PlanOffer {
        let held = self.plans.lock_recover().get(&hex::encode(person)).cloned();
        let mut plan = held.unwrap_or_else(|| PlanOffer {
            id: FREE_PLAN.to_owned(),
            name: "Free".to_owned(),
            price: None,
            limits: Limits::default(),
        });
        plan.limits = Limits {
            storage_bytes: Some(
                plan.limits
                    .storage_bytes
                    .unwrap_or(self.offer.storage_bytes),
            ),
            ..Limits::default()
        };
        plan
    }

    /// The bytes all the workspaces of `person`'s own may take here together.
    fn storage_bytes(&self, person: &[u8; 32]) -> u64 {
        self.plan_of(person)
            .limits
            .storage_bytes
            .unwrap_or(self.offer.storage_bytes)
    }

    /// The bytes each of `workspaces` (their UUIDs) takes on the host whose
    /// workspaces' records are in `dir`, in the same order: measured again
    /// for the ones last measured longer ago than [`MEASURED_FOR`], or for
    /// all of them when `fresh`. The records are read once for all of them,
    /// so a person's hundredth workspace costs no more than their first.
    fn sizes(&self, dir: &Path, workspaces: &[String], fresh: bool) -> Vec<u64> {
        let now = tilo_clock::now();
        let cached: Vec<Option<u64>> = {
            let measured = self.measured.lock_recover();
            workspaces
                .iter()
                .map(|workspace| {
                    measured
                        .get(workspace)
                        .filter(|_| !fresh)
                        .filter(|(_, at)| now.saturating_duration_since(*at) < MEASURED_FOR)
                        .map(|(bytes, _)| *bytes)
                })
                .collect()
        };
        let stale: Vec<&String> = workspaces
            .iter()
            .zip(&cached)
            .filter_map(|(workspace, cached)| cached.is_none().then_some(workspace))
            .collect();
        if !stale.is_empty() {
            #[cfg(test)]
            {
                self.listed
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            let beside = beside_bytes(
                dir,
                &stale.iter().map(|workspace| workspace.as_str()).collect(),
            );
            let host_state = dir.parent();
            let measured_now: Vec<u64> = stale
                .iter()
                .map(|workspace| {
                    beside
                        .get(workspace.as_str())
                        .copied()
                        .unwrap_or_default()
                        .saturating_add(
                            host_state.map_or(0, |state| stored_state(state, workspace)),
                        )
                })
                .collect();
            let mut measured = self.measured.lock_recover();
            for (workspace, bytes) in stale.iter().zip(&measured_now) {
                measured.insert((*workspace).clone(), (*bytes, now));
            }
        }
        let measured = self.measured.lock_recover();
        workspaces
            .iter()
            .map(|workspace| measured.get(workspace).map_or(0, |(bytes, _)| *bytes))
            .collect()
    }

    /// The bytes every workspace of `person`'s own takes on the host whose
    /// workspaces' records are in `dir` together: what they share.
    fn used(&self, dir: &Path, person: &[u8; 32], fresh: bool) -> u64 {
        self.sizes(dir, &self.of(person), fresh)
            .iter()
            .copied()
            .fold(0_u64, u64::saturating_add)
    }
}

/// Whose own each workspace in `by_person` is.
fn owners_of(by_person: &BTreeMap<String, Vec<String>>) -> HashMap<String, String> {
    by_person
        .iter()
        .flat_map(|(person, mine)| {
            mine.iter()
                .map(move |workspace| (workspace.clone(), person.clone()))
        })
        .collect()
}

/// Every file beside the workspaces' records in `dir` that belongs to one of
/// `wanted`, by the workspace whose record it belongs to
/// (`<workspace>.<what>`): the directory is listed once, however many
/// workspaces are measured, and only theirs are walked, however many other
/// workspaces the host keeps. The directory named for a workspace is its
/// retained state, which [`stored_state`] counts.
fn beside_bytes(dir: &Path, wanted: &HashSet<&str>) -> HashMap<String, u64> {
    let mut beside: HashMap<String, u64> = HashMap::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some((workspace, _)) = name.split_once('.') else {
            continue;
        };
        if !wanted.contains(workspace) {
            continue;
        }
        let bytes = tree_bytes(&entry.path());
        let total = beside.entry(workspace.to_owned()).or_default();
        *total = total.saturating_add(bytes);
    }
    beside
}

/// What `workspace` keeps in the host's retained state and sealed history,
/// under its `state` directory.
fn stored_state(state: &Path, workspace: &str) -> u64 {
    let Some(workspace_id) = WorkspaceId::parse(workspace) else {
        return 0;
    };
    [
        state.join("workspaces").join(workspace_id.to_string()),
        state
            .join("store")
            .join("workspaces")
            .join(workspace_id.to_string()),
    ]
    .iter()
    .map(|path| tree_bytes(path))
    .fold(0_u64, u64::saturating_add)
}

/// The bytes of the file at `path`, or of every file under it.
fn tree_bytes(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| tree_bytes(&entry.path()))
        .fold(0_u64, u64::saturating_add)
}

/// Gigabytes, for the words a person reads.
fn gigabytes(bytes: u64) -> String {
    #[expect(
        clippy::cast_precision_loss,
        reason = "a size shown to a person to one decimal"
    )]
    let gb = bytes as f64 / 1_000_000_000.0;
    format!("{gb:.1} GB")
}

impl Workspaces {
    /// Give each person who asks workspaces of their own on this host, with
    /// what `offer` lets them keep. Called once, before the host serves;
    /// `false` when called before.
    ///
    /// # Errors
    ///
    /// Which workspaces are whose could not be read.
    pub fn offer_own_workspaces(&self, offer: OwnWorkspaceOffer) -> Result<bool, String> {
        let own = OwnWorkspaces::open(&self.inner.dir, offer)?;
        Ok(self.inner.own.set(own).is_ok())
    }

    /// Whether this host keeps workspaces for the people it lets in
    /// ([`Self::offer_own_workspaces`]): theirs, and the ones another host
    /// governs that their owner has it hold as well.
    #[must_use]
    pub fn keeps_workspaces_for_people(&self) -> bool {
        self.own().is_some()
    }

    pub(super) fn own(&self) -> Option<&OwnWorkspaces> {
        self.inner.own.get()
    }

    /// Keep workspaces of their own here only for the people the site let in
    /// through `home`, its home workspace: those its entry or an invitation
    /// admitted. Called once, after [`Self::offer_own_workspaces`] and before
    /// the host serves; `false` when this host keeps no workspaces of
    /// people's own or the home was named before.
    pub fn admit_own_through(&self, home: &str) -> bool {
        self.own()
            .is_some_and(|own| own.home.set(home.to_owned()).is_ok())
    }

    /// Whether `person` was let in to keep workspaces of their own here: the
    /// host's person; a member of the site's home workspace, whom its entry or
    /// an invitation admitted, or of any other workspace here, whom an
    /// invitation into it admitted; a person an invitation into it still names,
    /// who enters by it; or a person who keeps workspaces of their own here
    /// already, which only someone let in was given. That last keeps a person
    /// in whose home membership lapsed while they worked in their own
    /// workspaces. Every key and agent of the person is the person here; what
    /// each may make is its scope's.
    pub(super) async fn let_in(&self, person: &[u8; 32], now: u64) -> bool {
        if *person == self.inner.host.cert.user_public_key {
            return true;
        }
        let Some(own) = self.own() else {
            return false;
        };
        if !own.of(person).is_empty() {
            return true;
        }
        let Some(home) = own.home.get() else {
            return false;
        };
        self.inner.memberships.holds_any(person) || self.invited(home, *person, now).await
    }

    /// Whether this host keeps for `person`, an owner of it, a workspace
    /// another host governs: as it keeps workspaces of their own here, only
    /// for someone it let in ([`Self::let_in`]) whose workspaces here have
    /// capacity left. Why not, in plain words, when it does not.
    ///
    /// # Errors
    ///
    /// The host does not keep it for them.
    pub(crate) async fn keeps_for_owner(&self, person: &[u8; 32], now: u64) -> Result<(), String> {
        if !self.let_in(person, now).await {
            return Err(self
                .not_let_in_then("It holds your workspaces once you are in.")
                .await
                .error);
        }
        if let Some(own) = self.own() {
            let limit = own.storage_bytes(person);
            let used = own.used(&self.inner.dir, person, true);
            if used >= limit {
                return Err(format!(
                    "your workspaces on {} are full: they keep {} of the {} it keeps for you",
                    own.offer().host,
                    gigabytes(used),
                    gigabytes(limit)
                ));
            }
        }
        Ok(())
    }

    /// Whether `home`'s entry takes people by invitation only, as its governance
    /// records it.
    async fn takes_people_by_invitation(&self, home: &str) -> bool {
        self.entry_policy(home)
            .await
            .is_some_and(|entry| entry.invitation_only)
    }

    /// What a person [`Self::let_in`] does not admit hears: the refusal of
    /// the site's entry, offering its wait list when it takes people by
    /// invitation, carried as the action's refusal reason
    /// ([`tilo_server_protocol::directory::EntryRefused::to_reason`]).
    pub(super) async fn not_let_in(&self) -> tilo_server_protocol::directory::EntryRefused {
        self.not_let_in_then("Your workspaces are made once you are in.")
            .await
    }

    /// [`Self::not_let_in`], ending with `then`: what being let in brings.
    async fn not_let_in_then(&self, then: &str) -> tilo_server_protocol::directory::EntryRefused {
        use tilo_server_protocol::directory::{EntryRefusalCode, EntryRefused};
        let host = self
            .own()
            .map_or_else(String::new, |own| own.offer().host.clone());
        let home = self.own().and_then(|own| own.home.get());
        let (code, mut error) = match home {
            Some(home) if self.takes_people_by_invitation(home).await => (
                EntryRefusalCode::InvitationOnly,
                format!("{host} takes people by invitation and has not let you in yet"),
            ),
            Some(_) => (
                EntryRefusalCode::Refused,
                format!("come into {host} through its entry first"),
            ),
            None => {
                return EntryRefused {
                    code: EntryRefusalCode::Refused,
                    error: format!("{host} keeps workspaces only for the person it runs for"),
                    ..EntryRefused::default()
                };
            }
        };
        let entry = self
            .wait_list_page()
            .filter(|_| code == EntryRefusalCode::InvitationOnly);
        if entry.is_some() {
            error.push_str("; leave your address on its wait list");
        }
        error.push_str(". ");
        error.push_str(then);
        EntryRefused {
            code,
            error,
            ..EntryRefused::default()
        }
        .with_wait_list(entry)
    }

    /// Put `person` on `plan` here: how much the workspaces of their own may
    /// keep, as the host's account for them says rather than what
    /// [`OwnWorkspaceOffer`] gives everyone. How many they have stays
    /// unbounded. `false` when this host keeps no workspaces of people's own.
    ///
    /// A plan is not kept across a restart: whoever knows a person's plan
    /// sets it again, as it sets the offer.
    pub fn set_own_plan(&self, person: &[u8; 32], plan: PlanOffer) -> bool {
        let Some(own) = self.own() else {
            return false;
        };
        own.plan(person, plan);
        true
    }

    /// Whether `device` is shown `workspace` when it lists this host's
    /// workspaces: the host's person every one, anyone else only the ones of
    /// their own and the ones their person is a member of, and an agent's
    /// grant only what its scope reaches.
    pub(super) fn lists_to(
        &self,
        device: &tilo_user_identity::DeviceCert,
        workspace: &str,
    ) -> bool {
        let person = &device.user_public_key;
        let theirs = *person == self.inner.host.cert.user_public_key
            || self.own().is_some_and(|own| own.is_of(person, workspace))
            || self.inner.memberships.holds(person, workspace);
        theirs
            && device
                .capabilities
                .agent
                .is_none_or(|scope| scope.reaches(workspace))
    }

    /// Whether this host keeps workspaces of people's own: one that does
    /// says whose each of them is ([`Self::owns`]); one that does not
    /// governs its workspaces for its own person.
    #[must_use]
    pub fn keeps_own_workspaces(&self) -> bool {
        self.own().is_some()
    }

    /// What `workspace`'s owner labelled it with, as their membership of it says: none for one
    /// they did not label.
    #[must_use]
    pub fn labels_of(&self, workspace: &str) -> WorkspaceLabels {
        self.membership(workspace)
            .map(|membership| membership.labels)
            .unwrap_or_default()
    }

    /// Whether `workspace`'s owner let keys reaching it alone run commands in its machine
    /// without asking them each time, as their membership of it says.
    #[must_use]
    pub fn runs_unattended(&self, workspace: &str) -> bool {
        self.membership_unattended(workspace)
    }

    /// [`Self::runs_unattended`].
    pub(super) fn membership_unattended(&self, workspace: &str) -> bool {
        self.membership(workspace)
            .is_some_and(|membership| membership.unattended)
    }

    /// Where the owner of `workspace`, an own workspace here, keeps their membership of it: their
    /// own workspace, and the record there.
    fn membership_at(&self, workspace: &str) -> Option<(String, String)> {
        let person = self.own()?.owner_of(workspace)?;
        Some((
            tilo_server_protocol::servers::person_workspace_id(&person),
            membership::record(workspace),
        ))
    }

    /// The owner's membership of `workspace`, when it is an own workspace here and this host
    /// holds the owner's workspace.
    fn membership(&self, workspace: &str) -> Option<Membership> {
        let (person, record) = self.membership_at(workspace)?;
        let workspaces = self.clone();
        crate::device_documents::on_its_own(async move {
            person_workspace::read_on::<Membership>(
                &workspaces,
                &person,
                (membership::APP, &record),
            )
            .await
        })
        .ok()
    }

    /// Apply `action` to the owner's membership of `workspace`, an own workspace here.
    fn change_membership(
        &self,
        workspace: &str,
        action: tilo_resource::Invoke<Membership>,
    ) -> Result<(), String> {
        let (person, record) = self
            .membership_at(workspace)
            .ok_or("the workspace is nobody's own here")?;
        let workspaces = self.clone();
        crate::device_documents::on_its_own(async move {
            person_workspace::apply_on(&workspaces, &person, (membership::APP, &record), action)
                .await
        })
    }

    /// Keep that `workspace`'s owner holds `role` in it, made now.
    pub(super) fn keep_membership(&self, workspace: &str, role: &str) -> Result<(), String> {
        let now = i64::try_from(tilo_clock::unix_now_ms()).unwrap_or(i64::MAX);
        self.change_membership(
            workspace,
            Membership::KEEP_ROLE.with((role.to_owned(), now)),
        )
    }

    /// Label `workspace` with `labels`, in place of what it was labelled with before (none takes
    /// them all off).
    pub(super) fn label(&self, workspace: &str, labels: WorkspaceLabels) -> Result<(), String> {
        let labels = serde_json::to_string(&labels).map_err(|error| error.to_string())?;
        self.change_membership(workspace, Membership::RELABEL.with(labels))
    }

    /// Turn [`Self::runs_unattended`] for `workspace` on or off.
    pub(super) fn set_unattended(&self, workspace: &str, on: bool) -> Result<(), String> {
        self.change_membership(workspace, Membership::RUN_UNATTENDED.with(on))
    }

    /// `workspace` was deleted: its owner's membership of it keeps nothing.
    pub(super) fn forget_membership(&self, workspace: &str) -> Result<(), String> {
        self.change_membership(workspace, Membership::FORGET.with(()))
    }

    /// Record `workspace`, which this host keeps for its own person, as one of `person`'s own: a
    /// workspace founded here before the host kept an index of its person's workspaces. Whether it
    /// is theirs afterwards; `false` when it belongs to another person or this host does not keep
    /// it.
    #[must_use]
    pub async fn claim_own(&self, person: &[u8; 32], workspace: &str) -> bool {
        let Some(own) = self.own() else {
            return false;
        };
        if *person != self.inner.host.cert.user_public_key {
            return false;
        }
        if own.is_of(person, workspace) {
            return true;
        }
        if !self.inner.workspaces.lock().await.contains_key(workspace) {
            return false;
        }
        let claimed = own.record(person, workspace).is_ok();
        if claimed && let Err(error) = self.keep_membership(workspace, "owner") {
            tracing::warn!(%workspace, %error, "a claimed workspace's owner's membership of it is not kept");
        }
        claimed
    }

    /// Whether `workspace` is one of `person`'s own here.
    #[must_use]
    pub fn owns(&self, person: &[u8; 32], workspace: &str) -> bool {
        self.own().is_some_and(|own| own.is_of(person, workspace))
    }

    /// The first workspace of `person`'s own here, if they have one: the one
    /// a plain ask for "my workspace" answers.
    #[must_use]
    pub fn own_workspace_of(&self, person: &[u8; 32]) -> Option<String> {
        self.own()?.of(person).into_iter().next()
    }

    /// Every workspace of `person`'s own here, in the order they made them.
    #[must_use]
    pub fn own_workspaces_of(&self, person: &[u8; 32]) -> Vec<String> {
        self.own().map(|own| own.of(person)).unwrap_or_default()
    }

    /// Whether this host keeps anything of `person`: their own workspaces, a
    /// membership of theirs, or the host's own person. A host that keeps
    /// nothing of them forgot them (its state was reset, or it pruned them),
    /// and a session of theirs is turned away as not admitted.
    #[must_use]
    pub fn knows(&self, person: &[u8; 32]) -> bool {
        *person == self.inner.host.cert.user_public_key
            || self.inner.memberships.holds_any(person)
            || self.own().is_some_and(|own| !own.of(person).is_empty())
    }

    /// Why a write to `workspace` is not kept: it is someone's own and their
    /// workspaces of their own keep as much as they may between them. `None`
    /// for any other workspace, and ones with room.
    #[must_use]
    pub fn storage_refusal(&self, workspace: &str) -> Option<Refusal> {
        let own = self.own()?;
        let person = own.owner_of(workspace)?;
        let limit = own.storage_bytes(&person);
        let used = own.used(&self.inner.dir, &person, false);
        // Measured last a while ago: a person who freed room is not refused
        // on a stale size.
        let used = if used >= limit {
            own.used(&self.inner.dir, &person, true)
        } else {
            used
        };
        (used >= limit).then(|| Refusal {
            reason: Reason::StorageFull {
                limit_bytes: limit,
                used_bytes: used,
            },
            message: format!(
                "Your workspaces are full: they keep {} of the {} this host keeps for you, so this change was not kept.",
                gigabytes(used),
                gigabytes(limit)
            ),
            next: vec![NextStep {
                kind: NextStepKind::RaiseLimit,
                url: None,
            }],
        })
    }

    /// How many times this host listed the workspaces' records directory:
    /// what a create or a read costs, in operations rather than in seconds.
    #[cfg(test)]
    pub(crate) fn own_directory_listings(&self) -> usize {
        self.own().map_or(0, |own| {
            own.listed.load(std::sync::atomic::Ordering::Relaxed)
        })
    }

    /// `person`'s account with this host, when it keeps workspaces of
    /// people's own: the plan they are on, the workspaces of their own they
    /// have, what those keep and may keep, and no bound on how many there
    /// are.
    #[must_use]
    pub fn own_account(&self, person: &[u8; 32]) -> Option<ServerAccount> {
        let own = self.own()?;
        let plan = own.plan_of(person);
        let mine = own.of(person);
        let sizes = own.sizes(&self.inner.dir, &mine, false);
        let workspaces: Vec<WorkspaceUsage> = mine
            .into_iter()
            .zip(sizes)
            .map(|(workspace, stored_bytes)| WorkspaceUsage {
                workspace,
                stored_bytes,
                ..WorkspaceUsage::default()
            })
            .collect();
        Some(ServerAccount {
            server: own.offer().host.clone(),
            kind: ServerKind::Provider,
            plans: vec![plan.clone()],
            subscription: Some(Subscription {
                account: hex::encode(person),
                plan: Some(plan.id),
                limits: Some(plan.limits),
                usage: Usage {
                    workspaces,
                    ..Usage::default()
                },
                ..Subscription::default()
            }),
            manage_billing: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A workspace of a person's own to measure.
    const MINE: &str = "0f0e0d0c-0b0a-4908-8706-050403020100";

    #[test]
    fn whose_workspace_is_whose_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let offer = OwnWorkspaceOffer::free("tilo.test");
        let own = OwnWorkspaces::open(dir.path(), offer.clone()).unwrap();
        own.record(&[3; 32], "w-3").unwrap();
        own.record(&[3; 32], "w-4").unwrap();
        let again = OwnWorkspaces::open(dir.path(), offer).unwrap();
        assert_eq!(again.of(&[3; 32]), ["w-3", "w-4"], "in the order made");
        assert_eq!(again.owner_of("w-4"), Some([3; 32]));
        assert!(again.is_of(&[3; 32], "w-3"));
        assert!(!again.is_of(&[4; 32], "w-3"));
        assert_eq!(again.of(&[4; 32]), Vec::<String>::new());
        assert_eq!(again.owner_of("w-5"), None);
    }

    #[test]
    fn a_deleted_workspace_is_nobodys_and_takes_nothing_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let offer = OwnWorkspaceOffer::free("tilo.test");
        let own = OwnWorkspaces::open(dir.path(), offer.clone()).unwrap();
        own.record(&[3; 32], "w-3").unwrap();
        own.record(&[3; 32], "w-4").unwrap();
        own.forget("w-3").unwrap();
        assert_eq!(own.of(&[3; 32]), ["w-4"]);
        assert_eq!(own.owner_of("w-3"), None);
        own.forget("w-4").unwrap();
        let again = OwnWorkspaces::open(dir.path(), offer).unwrap();
        assert_eq!(again.of(&[3; 32]), Vec::<String>::new());
        assert_eq!(again.owner_of("w-4"), None);
    }

    #[test]
    fn a_plan_sets_what_a_persons_workspaces_keep_and_never_how_many_they_have() {
        let dir = tempfile::tempdir().unwrap();
        let own = OwnWorkspaces::open(dir.path(), OwnWorkspaceOffer::free("tilo.test")).unwrap();
        let person = [3; 32];
        assert_eq!(
            own.storage_bytes(&person),
            OwnWorkspaceOffer::DEFAULT_STORAGE_BYTES
        );
        own.plan(
            &person,
            PlanOffer {
                id: "pro".into(),
                name: "Pro".into(),
                price: None,
                limits: Limits {
                    max_workspaces: Some(3),
                    storage_bytes: Some(50),
                    ..Limits::default()
                },
            },
        );
        assert_eq!(own.storage_bytes(&person), 50);
        assert_eq!(
            own.plan_of(&person).limits.max_workspaces,
            None,
            "how many workspaces a person has is not bounded here"
        );
        assert_eq!(
            own.storage_bytes(&[4; 32]),
            OwnWorkspaceOffer::DEFAULT_STORAGE_BYTES,
            "a plan is one person's"
        );
    }

    #[test]
    fn a_workspace_counts_its_record_its_retained_state_and_its_history() {
        let state = tempfile::tempdir().unwrap();
        let dir = state.path().join("workspaces");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{MINE}.workspace")), [0; 10]).unwrap();
        std::fs::write(dir.join(format!("{MINE}.admissions")), [0; 5]).unwrap();
        std::fs::write(dir.join("other.workspace"), [0; 1000]).unwrap();
        let workspace_id = WorkspaceId::parse(MINE).unwrap();
        let retained = state
            .path()
            .join("workspaces")
            .join(workspace_id.to_string());
        std::fs::create_dir_all(&retained).unwrap();
        std::fs::write(retained.join("retained"), [0; 100]).unwrap();
        let history = state
            .path()
            .join("store/workspaces")
            .join(workspace_id.to_string());
        std::fs::create_dir_all(&history).unwrap();
        std::fs::write(history.join("x"), [0; 1000]).unwrap();
        let own = OwnWorkspaces::open(&dir, OwnWorkspaceOffer::free("tilo.test")).unwrap();
        let both = [MINE.to_owned(), "other".to_owned()];
        assert_eq!(own.sizes(&dir, &both, false), [1115, 1000]);
        std::fs::remove_file(history.join("x")).unwrap();
        // Trusted until measured afresh.
        assert_eq!(own.sizes(&dir, &both, false), [1115, 1000]);
        assert_eq!(own.sizes(&dir, &both, true), [115, 1000]);
    }
}
