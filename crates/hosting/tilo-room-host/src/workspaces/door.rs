//! Who a workspace's public door keeps out: a person moderation removed stays
//! out for a day, however often they ask again. An idle member the host prunes
//! is not kept out ([`Door::clear`]): they come back through the door as the
//! person they are.
//!
//! The day is the workspace service's own: a person removed from a community
//! workspace is also turned away by the address they joined from, and the two
//! doors agree on how long. Kept beside the workspace's record, in
//! `<workspace>.door`, so a restart lets nobody back early.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tilo_files::native::migration::Format;

/// How long a person moderation removed is refused at the door.
pub(super) const REMOVED_FOR_NS: u64 = 24 * 3600 * 1_000_000_000;

/// The door's on-disk format: a `u32` version, then when each removed person
/// was removed.
pub(super) const FORMAT: Format = Format {
    name: "room door",
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

/// One workspace's door state.
#[derive(Debug, Default)]
pub(super) struct Door {
    removed: BTreeMap<[u8; 32], u64>,
    path: PathBuf,
}

impl Door {
    /// The door state of the workspace recorded at `room_path` (its `.room`
    /// file).
    pub(super) fn open(room_path: &Path) -> Result<Self, String> {
        let path = room_path.with_extension("door");
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

    /// Let `person` in again at once: the workspace pruned them as idle, or
    /// lifted their ban.
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

    #[test]
    fn a_removed_person_is_refused_for_the_day_and_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("room.workspace");
        let mut door = Door::open(&workspace).unwrap();
        let person = [7; 32];
        assert!(!door.refuses(person, 1_000));
        door.remove(person, 1_000).unwrap();
        assert!(door.refuses(person, 1_000));
        assert!(door.refuses(person, 1_000 + REMOVED_FOR_NS - 1));
        assert!(!door.refuses(person, 1_000 + REMOVED_FOR_NS));

        let restored = Door::open(&workspace).unwrap();
        assert!(restored.refuses(person, 1_000 + REMOVED_FOR_NS - 1));

        door.clear(person).unwrap();
        assert!(!door.refuses(person, 1_000));
        assert!(!Door::open(&workspace).unwrap().refuses(person, 1_000));
    }

    #[test]
    fn an_expired_removal_is_forgotten_when_the_next_one_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("room.workspace");
        let mut door = Door::open(&workspace).unwrap();
        door.remove([1; 32], 1).unwrap();
        door.remove([2; 32], 1 + REMOVED_FOR_NS).unwrap();
        assert_eq!(door.removed.len(), 1, "{:?}", door.removed.keys());
        assert!(door.refuses([2; 32], 1 + REMOVED_FOR_NS));
    }
}
