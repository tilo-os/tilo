//! A workspace of a person's own on this host: one per person, made when
//! they first ask ([`HostOperation::CreateOwnWorkspace`]), governed here like
//! every workspace this relay governs, with the person as its administrator.
//!
//! A host keeps them only when its operator offers them
//! ([`Workspaces::offer_own_workspaces`]), with what each person may keep:
//! the bytes their workspace takes on this host, its history and state
//! ([`OwnWorkspaceOffer::storage_bytes`]). A write to a full workspace is
//! refused ([`Workspaces::storage_refusal`]), typed as the host's account
//! refusals are ([`host_account::Reason::StorageFull`]); the person's account
//! read answers how much they keep and may keep ([`Workspaces::own_account`]).
//! The files members post are kept apart, bounded on their own (oldest go
//! first), and not counted here.
//!
//! Which workspace is whose is kept beside the workspaces' records, in
//! `own-workspaces.json`, so asking again after a restart answers the same
//! workspace.
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
/// The plan every person with a workspace of their own here is on.
const FREE_PLAN: &str = "free";
/// Workspaces of their own a person keeps here.
const MAX_OWN: u32 = 1;

/// What a host gives each person who asks for a workspace of their own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnWorkspaceOffer {
    /// The host's name, as people know it (`tilo.sh`): how their account
    /// read names it.
    pub host: String,
    /// Bytes the person's workspace may take on this host.
    pub storage_bytes: u64,
}

impl OwnWorkspaceOffer {
    /// Ten gigabytes: what a person's free workspace keeps.
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
    /// The person (hex root) whose own each workspace (its UUID) is, and
    /// back.
    by_person: std::sync::Mutex<BTreeMap<String, String>>,
    /// Each own workspace's measured size, and when it was measured.
    measured: std::sync::Mutex<HashMap<String, (u64, Instant)>>,
    /// Held while a person's workspace is made: two of their devices asking
    /// at once get the same one.
    pub(super) making: tokio::sync::Mutex<()>,
}

impl OwnWorkspaces {
    /// Those recorded beside the workspaces' records in `dir`.
    pub(super) fn open(dir: &Path, offer: OwnWorkspaceOffer) -> Result<Self, String> {
        let path = dir.join(INDEX);
        let by_person = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| error.to_string())?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => return Err(error.to_string()),
        };
        Ok(Self {
            offer,
            path,
            by_person: std::sync::Mutex::new(by_person),
            measured: std::sync::Mutex::default(),
            making: tokio::sync::Mutex::new(()),
        })
    }

    pub(super) const fn offer(&self) -> &OwnWorkspaceOffer {
        &self.offer
    }

    /// The workspace of `person`'s own, if they have one.
    pub(super) fn of(&self, person: &[u8; 32]) -> Option<String> {
        self.by_person
            .lock_recover()
            .get(&hex::encode(person))
            .cloned()
    }

    /// Whose own `workspace` is, if anyone's.
    pub(super) fn owner_of(&self, workspace: &str) -> Option<[u8; 32]> {
        let person = self
            .by_person
            .lock_recover()
            .iter()
            .find(|(_, own)| *own == workspace)
            .map(|(person, _)| person.clone())?;
        let mut key = [0; 32];
        hex::decode_to_slice(person, &mut key).ok()?;
        Some(key)
    }

    /// Record `workspace` as `person`'s own, durably.
    pub(super) fn record(&self, person: &[u8; 32], workspace: &str) -> Result<(), String> {
        let mut index = self.by_person.lock_recover();
        let mut next = index.clone();
        next.insert(hex::encode(person), workspace.to_owned());
        let bytes = serde_json::to_vec_pretty(&next).map_err(|error| error.to_string())?;
        // Written before it is answered, under the lock, so two records
        // never race to the file.
        tilo_files::native::atomic::write(&self.path, &bytes).map_err(|error| error.to_string())?;
        *index = next;
        drop(index);
        Ok(())
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
    /// Give each person who asks a workspace of their own on this host, with
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

    /// Whether `device` is shown `workspace` when it lists this host's
    /// workspaces: the host's person every one, anyone else only the one of
    /// their own, and an agent's grant only what its scope reaches.
    pub(super) fn lists_to(
        &self,
        device: &tilo_user_identity::DeviceCert,
        workspace: &str,
    ) -> bool {
        let theirs = device.user_public_key == self.inner.host.cert.user_public_key
            || self.own_workspace_of(&device.user_public_key).as_deref() == Some(workspace);
        theirs
            && device
                .capabilities
                .agent
                .is_none_or(|scope| scope.reaches(workspace))
    }

    /// The workspace UUID of `person`'s own here, if they have one.
    #[must_use]
    pub fn own_workspace_of(&self, person: &[u8; 32]) -> Option<String> {
        self.own()?.of(person)
    }

    /// Why a write to `workspace` (carried as `workspace_id`) is not kept:
    /// it is someone's own and keeps as much as they may. `None` for any
    /// other workspace, and one with room.
    #[must_use]
    pub fn storage_refusal(&self, workspace: &str, workspace_id: u64) -> Option<Refusal> {
        let own = self.own()?;
        own.owner_of(workspace)?;
        let limit = own.offer().storage_bytes;
        let used = own.stored(&self.inner.dir, workspace, workspace_id, false);
        // Measured last a while ago: a person who freed room is not refused
        // on a stale size.
        let used = if used >= limit {
            own.stored(&self.inner.dir, workspace, workspace_id, true)
        } else {
            used
        };
        (used >= limit).then(|| Refusal {
            reason: Reason::StorageFull {
                limit_bytes: limit,
                used_bytes: used,
            },
            message: format!(
                "Your workspace is full: it keeps {} of the {} this host keeps for you, so this change was not kept.",
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
    /// people's own: the free plan, what their workspace keeps and may keep.
    #[must_use]
    pub fn own_account(&self, person: &[u8; 32]) -> Option<HostAccount> {
        let own = self.own()?;
        let limits = Limits {
            storage_bytes: Some(own.offer().storage_bytes),
            max_workspaces: Some(MAX_OWN),
            ..Limits::default()
        };
        let workspaces = own
            .of(person)
            .and_then(|workspace| {
                let workspace_id = tilo_relay_protocol::room_workspace_id(&workspace).ok()?;
                let stored_bytes = own.stored(&self.inner.dir, &workspace, workspace_id, false);
                Some(WorkspaceUsage {
                    workspace,
                    stored_bytes,
                    ..WorkspaceUsage::default()
                })
            })
            .into_iter()
            .collect();
        Some(HostAccount {
            host: own.offer().host.clone(),
            kind: HostKind::Provider,
            plans: vec![PlanOffer {
                id: FREE_PLAN.to_owned(),
                name: "Free".to_owned(),
                price: None,
                limits: limits.clone(),
            }],
            standing: Some(Standing {
                account: hex::encode(person),
                plan: Some(FREE_PLAN.to_owned()),
                limits: Some(limits),
                usage: Usage {
                    workspaces,
                    ..Usage::default()
                },
                ..Standing::default()
            }),
            manage_billing: None,
        })
    }

    /// The refusal of a second workspace of `person`'s own, who has
    /// `existing`.
    pub(super) fn second_own_refusal(existing: &str) -> Refusal {
        Refusal {
            reason: host_account::Reason::TooManyWorkspaces {
                limit: MAX_OWN,
                existing: vec![existing.to_owned()],
            },
            message: "You already have a workspace of your own on this host; use that one."
                .to_owned(),
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
        let again = OwnWorkspaces::open(dir.path(), offer).unwrap();
        assert_eq!(again.of(&[3; 32]).as_deref(), Some("w-3"));
        assert_eq!(again.owner_of("w-3"), Some([3; 32]));
        assert_eq!(again.of(&[4; 32]), None);
        assert_eq!(again.owner_of("w-4"), None);
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
        };
        let own = OwnWorkspaces::open(&dir, offer).unwrap();
        assert_eq!(own.stored(&dir, "w", 7, false), 1001);
        std::fs::remove_file(history.join("b")).unwrap();
        // Trusted until measured afresh.
        assert_eq!(own.stored(&dir, "w", 7, false), 1001);
        assert_eq!(own.stored(&dir, "w", 7, true), 115);
    }
}
