//! Who a workspace's public door keeps out: a person moderation removed stays
//! out for a day, however often they ask again. An idle member the host prunes
//! is not kept out ([`Door::clear`]): they come back through the door as the
//! person they are.
//!
//! The day is the workspace service's own: a person removed from a community
//! workspace is also turned away by the address they joined from, and the two
//! doors agree on how long. Kept beside the workspace's record, in
//! `<workspace>.door`, so a restart lets nobody back early.
//!
//! Before the door, the persistent room kept the same removals in its own
//! `removals.json`. A room takes them over once
//! ([`Workspaces::adopt_legacy_removals`]) and deletes the old file: the door
//! is from then on the one record of whom moderation removed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tilo_files::native::migration::Format;

/// How long a person moderation removed is refused at the door.
pub(super) const REMOVED_FOR_NS: u64 = 24 * 3600 * 1_000_000_000;

/// The door's on-disk format: a `u32` version, then when each removed person
/// was removed. The removals a room kept before the door are written in this
/// same version 1 by [`Door::adopt`].
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

    /// Take over the removals a room kept before the door: each `person` at
    /// the time moderation removed them. Written in the door's own version,
    /// once, and the room's old file goes.
    pub(super) fn adopt(&mut self, removed: BTreeMap<[u8; 32], u64>) -> Result<(), String> {
        for (person, at) in removed {
            self.removed
                .entry(person)
                .and_modify(|kept| *kept = (*kept).max(at))
                .or_insert(at);
        }
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

impl super::Workspaces {
    /// Take over the removals a persistent room kept in `file` before the
    /// door (`removals.json`: hex person to Unix milliseconds) and delete it:
    /// each person the day has not passed for stays out until it does.
    /// Returns how many the door keeps. A file that is not there is nothing
    /// to take over.
    ///
    /// # Errors
    ///
    /// The file cannot be read or parsed, or the door cannot be written.
    pub async fn adopt_legacy_removals(
        &self,
        workspace: &str,
        file: &Path,
    ) -> Result<usize, super::CollaborationError> {
        let bytes = match std::fs::read(file) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => {
                return Err(super::CollaborationError::new(format!(
                    "{}: {error}",
                    file.display()
                )));
            }
        };
        let stored: BTreeMap<String, u64> = serde_json::from_slice(&bytes).map_err(|error| {
            super::CollaborationError::new(format!("{}: {error}", file.display()))
        })?;
        let now = tilo_clock::unix_now_ns();
        let kept: BTreeMap<[u8; 32], u64> = stored
            .iter()
            .filter_map(|(key, at_ms)| {
                let mut person = [0; 32];
                hex::decode_to_slice(key, &mut person).ok()?;
                let at = at_ms.saturating_mul(1_000_000);
                (now.saturating_sub(at) < REMOVED_FOR_NS).then_some((person, at))
            })
            .collect();
        if !kept.is_empty() {
            let state = self.workspace(workspace).await?;
            state
                .lock()
                .await
                .door
                .adopt(kept.clone())
                .map_err(super::CollaborationError::new)?;
        }
        std::fs::remove_file(file).map_err(|error| {
            super::CollaborationError::new(format!("{}: {error}", file.display()))
        })?;
        Ok(kept.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_files::native::migration::assert_golden_bytes;

    /// The door file this build writes: one person kept out, when they were
    /// removed. Kept so a workspace's door goes on reading the same way; a
    /// field added, removed or reordered changes these bytes.
    const KEPT: &str =
        "010107070707070707070707070707070707070707070707070707070707070707078080a8b1e39fe7cb17";

    #[test]
    fn the_door_file_kept_here_reads_and_writes_as_it_is() {
        assert_golden_bytes(
            "a workspace's door",
            &hex::decode(KEPT).unwrap(),
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
