//! Who a workspace keeps out: a person moderation removed stays out for a day,
//! however often they ask again, until the workspace admits them again — an
//! administrator holding them, or an invitation redeemed — which lifts it at
//! once. A ban is not lifted so; [`Removals::clear`] on an unban is the only
//! thing that lifts one. An idle member the host prunes is not kept out
//! ([`Removals::clear`]): they come back as the person they are.
//!
//! The day is the workspace's own: a person removed from a community workspace
//! is also turned away by the address they joined from, and the two agree on
//! how long. Kept beside the workspace's record, in `<workspace>.removals`, so
//! a restart lets nobody back early.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tilo_files::native::migration::Format;

/// How long a person moderation removed is refused.
pub(super) const REMOVED_FOR_NS: u64 = 24 * 3600 * 1_000_000_000;

/// The removals' on-disk format: a `u32` version, then when each removed person
/// was removed.
pub(super) const FORMAT: Format = Format {
    name: "workspace removals",
    version: 1,
    oldest: 1,
    steps: &[],
};

#[derive(Serialize, Deserialize)]
struct Stored {
    version: u32,
    /// When moderation removed each person, in Unix nanoseconds.
    removed: BTreeMap<[u8; 32], u64>,
}

/// One workspace's removals.
#[derive(Debug, Default)]
pub(super) struct Removals {
    removed: BTreeMap<[u8; 32], u64>,
    path: PathBuf,
}

impl Removals {
    /// The removals of the workspace recorded at `workspace_path` (its
    /// `.workspace` file).
    pub(super) fn open(workspace_path: &Path) -> Result<Self, String> {
        let path = workspace_path.with_extension("removals");
        let stored = match std::fs::read(&path) {
            Ok(bytes) => {
                let (stored, _) =
                    postcard::take_from_bytes::<u32>(&bytes).map_err(|error| error.to_string())?;
                let bytes = FORMAT
                    .bring_up(&path, bytes, stored)
                    .map_err(|error| error.to_string())?;
                postcard::from_bytes::<Stored>(&bytes).map_err(|error| error.to_string())?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Stored {
                version: FORMAT.version,
                removed: BTreeMap::new(),
            },
            Err(error) => return Err(error.to_string()),
        };
        Ok(Self {
            removed: stored.removed,
            path,
        })
    }

    fn save(&self) -> Result<(), String> {
        let bytes = postcard::to_stdvec(&Stored {
            version: FORMAT.version,
            removed: self.removed.clone(),
        })
        .map_err(|error| error.to_string())?;
        tilo_files::native::atomic::write(&self.path, &bytes).map_err(|error| error.to_string())
    }

    /// Whether moderation removed `person` less than a day before `now_ns`.
    pub(super) fn refuses(&self, person: [u8; 32], now_ns: u64) -> bool {
        self.removed
            .get(&person)
            .is_some_and(|at| now_ns.saturating_sub(*at) < REMOVED_FOR_NS)
    }

    /// Keep `person` out from `now_ns` for the day: moderation removed them.
    pub(super) fn remove(&mut self, person: [u8; 32], now_ns: u64) -> Result<(), String> {
        self.removed
            .retain(|_, at| now_ns.saturating_sub(*at) < REMOVED_FOR_NS);
        self.removed.insert(person, now_ns);
        self.save()
    }

    /// Let `person` in again at once: the workspace pruned them as idle, lifted
    /// their ban, or admitted them again.
    pub(super) fn clear(&mut self, person: [u8; 32]) -> Result<(), String> {
        if self.removed.remove(&person).is_some() {
            self.save()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_files::native::migration::assert_golden_bytes;

    /// The removals file this build writes: one person kept out, when they were
    /// removed. Kept so a workspace goes on reading the same way; a field
    /// added, removed or reordered changes these bytes.
    const GOLDEN: &str = include_str!("../../golden/workspace-removals/v1.hex");

    fn golden() -> Vec<u8> {
        hex::decode(GOLDEN.split_whitespace().collect::<String>()).unwrap()
    }

    #[test]
    fn the_removals_file_kept_here_reads_and_writes_as_it_is() {
        assert_golden_bytes(
            FORMAT.name,
            &golden(),
            |bytes| {
                let stored: Stored = postcard::from_bytes(bytes).unwrap();
                assert_eq!(stored.version, FORMAT.version);
                assert_eq!(stored.removed, BTreeMap::from([([7; 32], KEPT_REMOVED_AT)]));
            },
            || postcard::to_stdvec(&kept()).unwrap(),
        );
    }

    const KEPT_REMOVED_AT: u64 = 1_700_000_000_000_000_000;

    fn kept() -> Stored {
        Stored {
            version: FORMAT.version,
            removed: BTreeMap::from([([7; 32], KEPT_REMOVED_AT)]),
        }
    }

    #[test]
    fn a_removed_person_is_refused_for_the_day_and_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace.workspace");
        let mut removals = Removals::open(&workspace).unwrap();
        let person = [7; 32];
        assert!(!removals.refuses(person, 1_000));
        removals.remove(person, 1_000).unwrap();
        assert!(removals.refuses(person, 1_000));
        assert!(removals.refuses(person, 1_000 + REMOVED_FOR_NS - 1));
        assert!(!removals.refuses(person, 1_000 + REMOVED_FOR_NS));

        let restored = Removals::open(&workspace).unwrap();
        assert!(restored.refuses(person, 1_000 + REMOVED_FOR_NS - 1));

        removals.clear(person).unwrap();
        assert!(!removals.refuses(person, 1_000));
        assert!(!Removals::open(&workspace).unwrap().refuses(person, 1_000));
    }

    #[test]
    fn an_expired_removal_is_forgotten_when_the_next_one_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace.workspace");
        let mut removals = Removals::open(&workspace).unwrap();
        removals.remove([1; 32], 1).unwrap();
        removals.remove([2; 32], 1 + REMOVED_FOR_NS).unwrap();
        assert_eq!(removals.removed.len(), 1, "{:?}", removals.removed.keys());
        assert!(removals.refuses([2; 32], 1 + REMOVED_FOR_NS));
    }
}
