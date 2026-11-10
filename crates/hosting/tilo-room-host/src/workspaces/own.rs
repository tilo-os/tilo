//! Workspaces of a person's own on this host: made when they first ask
//! ([`HostOperation::CreateOwnWorkspace`]) and as many as their plan allows,
//! governed here like every workspace this relay governs, with the person as
//! each one's administrator.
//!
//! A host keeps them only when its operator offers them
//! ([`Workspaces::offer_own_workspaces`]), with what each person may keep:
//! how many they have ([`OwnWorkspaceOffer::max_workspaces`], or the count
//! their plan sets when the host knows their plan,
//! [`Workspaces::set_own_plan`]) and the bytes all of them take on this host
//! together, their history and state ([`OwnWorkspaceOffer::storage_bytes`]).
//! A write to a full workspace is refused ([`Workspaces::storage_refusal`]),
//! typed as the host's account refusals are
//! ([`host_account::Reason::StorageFull`]); the person's account read answers
//! how many they have, how much they keep and how much they may keep
//! ([`Workspaces::own_account`]).
//!
//! The files members post are kept apart, bounded on their own (oldest go
//! first), and not counted here.
//!
//! Which workspaces are whose is kept beside the workspaces' records, in
//! `own-workspaces.json`, so asking again after a restart answers the same
//! ones.
//!
//! [`HostOperation::CreateOwnWorkspace`]: crate::host_authority::HostOperation::CreateOwnWorkspace

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tilo_actions::remote::host_account::{
    self, HostAccount, HostKind, Limits, NextStep, NextStepKind, PlanOffer, Reason, Refusal,
    Standing, Usage, WorkspaceUsage,
};
use tilo_lock::MutexRecover as _;

use super::Workspaces;

/// The file beside the workspaces' records that says whose each own
/// workspace is.
const INDEX: &str = "own-workspaces.json";
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
    /// Workspaces of their own each person may keep here, while their plan
    /// sets no count.
    pub max_workspaces: u32,
}

impl OwnWorkspaceOffer {
    /// Ten gigabytes: what a person's own workspaces keep here.
    pub const DEFAULT_STORAGE_BYTES: u64 = 10_000_000_000;
    /// Ten workspaces of their own: what a host gives each person while its
    /// operator names no count.
    pub const DEFAULT_MAX_WORKSPACES: u32 = 10;

    /// The host `host` giving each person [`Self::DEFAULT_STORAGE_BYTES`] and
    /// [`Self::DEFAULT_MAX_WORKSPACES`].
    #[must_use]
    pub fn free(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            storage_bytes: Self::DEFAULT_STORAGE_BYTES,
            max_workspaces: Self::DEFAULT_MAX_WORKSPACES,
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
    /// The plan each person is on here, by their hex root, when the host's
    /// account knows theirs: what they may keep rather than what the offer
    /// gives everyone.
    plans: std::sync::Mutex<BTreeMap<String, PlanOffer>>,
    /// Each own workspace's measured size, and when it was measured.
    measured: std::sync::Mutex<HashMap<String, (u64, Instant)>>,
    /// Held while a person's workspace is made: two of their devices asking
    /// at once get the same one, and never one more than they may keep.
    pub(super) making: tokio::sync::Mutex<()>,
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
        Ok(Self {
            offer,
            path,
            by_person: std::sync::Mutex::new(by_person),
            plans: std::sync::Mutex::default(),
            measured: std::sync::Mutex::default(),
            making: tokio::sync::Mutex::new(()),
        })
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

    /// Whether `workspace` is one of `person`'s own.
    pub(super) fn is_of(&self, person: &[u8; 32], workspace: &str) -> bool {
        self.by_person
            .lock_recover()
            .get(&hex::encode(person))
            .is_some_and(|mine| mine.iter().any(|own| own == workspace))
    }

    /// Whose own `workspace` is, if anyone's.
    pub(super) fn owner_of(&self, workspace: &str) -> Option<[u8; 32]> {
        let person = self
            .by_person
            .lock_recover()
            .iter()
            .find(|(_, mine)| mine.iter().any(|own| own == workspace))
            .map(|(person, _)| person.clone())?;
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
        let bytes = serde_json::to_vec_pretty(&next).map_err(|error| error.to_string())?;
        // Written before it is answered, under the lock, so two records
        // never race to the file.
        tilo_files::native::atomic::write(&self.path, &bytes).map_err(|error| error.to_string())?;
        *index = next;
        drop(index);
        Ok(())
    }

    /// Put `person` on `plan` here: what their workspaces of their own may
    /// keep rather than what the offer gives everyone.
    pub(super) fn plan(&self, person: &[u8; 32], plan: PlanOffer) {
        self.plans.lock_recover().insert(hex::encode(person), plan);
    }

    /// The plan `person` is on here: theirs when the host's account knows it,
    /// else the free plan with what the offer gives everyone.
    fn plan_of(&self, person: &[u8; 32]) -> PlanOffer {
        self.plans
            .lock_recover()
            .get(&hex::encode(person))
            .cloned()
            .unwrap_or_else(|| PlanOffer {
                id: FREE_PLAN.to_owned(),
                name: "Free".to_owned(),
                price: None,
                limits: Limits {
                    storage_bytes: Some(self.offer.storage_bytes),
                    max_workspaces: Some(self.offer.max_workspaces),
                    ..Limits::default()
                },
            })
    }

    /// How many workspaces of their own `person` may keep here.
    pub(super) fn max_workspaces(&self, person: &[u8; 32]) -> u32 {
        self.plan_of(person)
            .limits
            .max_workspaces
            .unwrap_or(self.offer.max_workspaces)
    }

    /// The bytes all the workspaces of `person`'s own may take here together.
    fn storage_bytes(&self, person: &[u8; 32]) -> u64 {
        self.plan_of(person)
            .limits
            .storage_bytes
            .unwrap_or(self.offer.storage_bytes)
    }

    /// The bytes `workspace`, carried as `workspace_id`, takes on the host
    /// whose workspaces' records are in `dir`: measured again when the last
    /// measure is older than [`MEASURED_FOR`], or `fresh`.
    pub(super) fn stored(
        &self,
        dir: &Path,
        workspace: &str,
        workspace_id: u64,
        fresh: bool,
    ) -> u64 {
        let now = Instant::now();
        if !fresh
            && let Some((bytes, at)) = self.measured.lock_recover().get(workspace)
            && now.saturating_duration_since(*at) < MEASURED_FOR
        {
            return *bytes;
        }
        let bytes = stored_bytes(dir, workspace, workspace_id);
        self.measured
            .lock_recover()
            .insert(workspace.to_owned(), (bytes, now));
        bytes
    }

    /// The bytes every workspace of `person`'s own takes on the host whose
    /// workspaces' records are in `dir` together: what they share.
    fn used(&self, dir: &Path, person: &[u8; 32], fresh: bool) -> u64 {
        self.of(person)
            .iter()
            .filter_map(|workspace| {
                let workspace_id = tilo_relay_protocol::room_workspace_id(workspace).ok()?;
                Some(self.stored(dir, workspace, workspace_id, fresh))
            })
            .fold(0_u64, u64::saturating_add)
    }
}

/// What `workspace` (carried as `workspace_id`) keeps on a host whose
/// workspaces' records are in `dir`: its record and the files beside it, the
/// relay's retained state of it, and its sealed history.
fn stored_bytes(dir: &Path, workspace: &str, workspace_id: u64) -> u64 {
    let beside = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(workspace))
        })
        .map(|entry| tree_bytes(&entry.path()))
        .fold(0_u64, u64::saturating_add);
    let Some(state) = dir.parent() else {
        return beside;
    };
    [
        state.join("workspaces").join(workspace_id.to_string()),
        state
            .join("store")
            .join("workspaces")
            .join(format!("{workspace_id:016x}")),
    ]
    .iter()
    .map(|path| tree_bytes(path))
    .fold(beside, u64::saturating_add)
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

    pub(super) fn own(&self) -> Option<&OwnWorkspaces> {
        self.inner.own.get()
    }

    /// Put `person` on `plan` here: how many workspaces of their own they may
    /// keep and how much those take, as the host's account for them says
    /// rather than what [`OwnWorkspaceOffer`] gives everyone. `false` when
    /// this host keeps no workspaces of people's own.
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
    /// their own, and an agent's grant only what its scope reaches.
    pub(super) fn lists_to(
        &self,
        device: &tilo_user_identity::DeviceCert,
        workspace: &str,
    ) -> bool {
        let theirs = device.user_public_key == self.inner.host.cert.user_public_key
            || self
                .own()
                .is_some_and(|own| own.is_of(&device.user_public_key, workspace));
        theirs
            && device
                .capabilities
                .agent
                .is_none_or(|scope| scope.reaches(workspace))
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

    /// `person`'s account with this host, when it keeps workspaces of
    /// people's own: the plan they are on, how many workspaces of their own
    /// they have, what those keep and may keep.
    #[must_use]
    pub fn own_account(&self, person: &[u8; 32]) -> Option<HostAccount> {
        let own = self.own()?;
        let plan = own.plan_of(person);
        let workspaces: Vec<WorkspaceUsage> = own
            .of(person)
            .iter()
            .filter_map(|workspace| {
                let workspace_id = tilo_relay_protocol::room_workspace_id(workspace).ok()?;
                Some(WorkspaceUsage {
                    workspace: workspace.clone(),
                    stored_bytes: own.stored(&self.inner.dir, workspace, workspace_id, false),
                    ..WorkspaceUsage::default()
                })
            })
            .collect();
        Some(HostAccount {
            host: own.offer().host.clone(),
            kind: HostKind::Provider,
            plans: vec![plan.clone()],
            standing: Some(Standing {
                account: hex::encode(person),
                plan: Some(plan.id),
                limits: Some(plan.limits),
                usage: Usage {
                    workspaces,
                    ..Usage::default()
                },
                ..Standing::default()
            }),
            manage_billing: None,
        })
    }

    /// The refusal of another workspace of `person`'s own, who has `existing`
    /// and may keep `limit` of them.
    pub(super) fn too_many_own_refusal(limit: u32, existing: &[String]) -> Refusal {
        let keeps = if limit == 1 {
            "one workspace of your own".to_owned()
        } else {
            format!("{limit} workspaces of your own")
        };
        Refusal {
            reason: host_account::Reason::TooManyWorkspaces {
                limit,
                existing: existing.to_vec(),
            },
            message: format!("You have all the {keeps} this host keeps for you; use one of those."),
            next: vec![NextStep {
                kind: NextStepKind::UseExisting,
                url: None,
            }],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_person_on_a_plan_keeps_what_it_says_rather_than_what_the_offer_gives() {
        let dir = tempfile::tempdir().unwrap();
        let own = OwnWorkspaces::open(dir.path(), OwnWorkspaceOffer::free("tilo.test")).unwrap();
        let person = [3; 32];
        assert_eq!(
            own.max_workspaces(&person),
            OwnWorkspaceOffer::DEFAULT_MAX_WORKSPACES
        );
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
        assert_eq!(own.max_workspaces(&person), 3);
        assert_eq!(own.storage_bytes(&person), 50);
        assert_eq!(
            own.max_workspaces(&[4; 32]),
            OwnWorkspaceOffer::DEFAULT_MAX_WORKSPACES,
            "a plan is one person's"
        );
    }

    #[test]
    fn a_workspace_counts_its_record_its_retained_state_and_its_history() {
        let state = tempfile::tempdir().unwrap();
        let dir = state.path().join("rooms");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("w.room"), [0; 10]).unwrap();
        std::fs::write(dir.join("w.admissions"), [0; 5]).unwrap();
        std::fs::write(dir.join("other.room"), [0; 1000]).unwrap();
        let retained = state.path().join("workspaces/7/retained");
        std::fs::create_dir_all(&retained).unwrap();
        std::fs::write(retained.join("a"), [0; 100]).unwrap();
        let history = state.path().join("store/workspaces/0000000000000007/x");
        std::fs::create_dir_all(&history).unwrap();
        std::fs::write(history.join("b"), [0; 1000 - 115 + 1]).unwrap();
        std::fs::create_dir_all(state.path().join("workspaces/8")).unwrap();
        std::fs::write(state.path().join("workspaces/8/c"), [0; 50]).unwrap();
        assert_eq!(stored_bytes(&dir, "w", 7), 1001);
        let offer = OwnWorkspaceOffer {
            host: "tilo.test".into(),
            storage_bytes: 1000,
            max_workspaces: 1,
        };
        let own = OwnWorkspaces::open(&dir, offer).unwrap();
        assert_eq!(own.stored(&dir, "w", 7, false), 1001);
        std::fs::remove_file(history.join("b")).unwrap();
        // Trusted until measured afresh.
        assert_eq!(own.stored(&dir, "w", 7, false), 1001);
        assert_eq!(own.stored(&dir, "w", 7, true), 115);
    }
}
