//! Recent files: what you opened or changed through Files, newest first.
//!
//! The list is personal and local: it lives beside the favourites in the
//! Files configuration directory, holds paths only, and forgets entries whose
//! file has gone. The Recent place in the sidebar lists it in recency order
//! rather than by the folder sort.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The pseudo-location the Recent place navigates to. It is relative and
/// scheme-like so it can never name a real directory Files would list or
/// write into.
pub const RECENT_LOCATION: &str = "recent://";

/// How many entries are kept.
pub const MAX_RECENT: usize = 200;

const FILE_NAME: &str = "recent-files.json";

pub fn location() -> PathBuf {
    PathBuf::from(RECENT_LOCATION)
}

pub fn is_location(path: &Path) -> bool {
    path.as_os_str() == RECENT_LOCATION
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecentUse {
    Opened,
    Changed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentItem {
    pub path: PathBuf,
    /// Unix milliseconds of the latest use.
    pub at_ms: u64,
    #[serde(rename = "use")]
    pub last_use: RecentUse,
}

/// The recent list, newest first, one entry per path.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentFiles {
    #[serde(default)]
    items: Vec<RecentItem>,
}

impl RecentFiles {
    pub fn items(&self) -> &[RecentItem] {
        &self.items
    }

    /// Record a use of `path` at `at_ms`, moving it to the front. Relative
    /// paths and the Recent place itself are ignored.
    pub fn note(&mut self, path: &Path, last_use: RecentUse, at_ms: u64) {
        if !path.is_absolute() {
            return;
        }
        self.items.retain(|item| item.path != path);
        self.items.insert(
            0,
            RecentItem {
                path: path.to_path_buf(),
                at_ms,
                last_use,
            },
        );
        // Keep newest first even if the clock stepped backwards.
        self.items.sort_by_key(|item| std::cmp::Reverse(item.at_ms));
        self.items.truncate(MAX_RECENT);
    }

    /// Drop `path` from the list. Returns whether it was there.
    pub fn forget(&mut self, path: &Path) -> bool {
        let before = self.items.len();
        self.items.retain(|item| item.path != path);
        self.items.len() != before
    }

    /// Follow a rename or move so the entry keeps its place.
    pub fn moved(&mut self, from: &Path, to: &Path) {
        for item in &mut self.items {
            if item.path == from {
                item.path = to.to_path_buf();
            } else if let Ok(rest) = item.path.strip_prefix(from) {
                item.path = to.join(rest);
            }
        }
    }

    pub fn load(dir: &Path) -> Self {
        std::fs::read(dir.join(FILE_NAME))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// # Errors
    ///
    /// Returns the I/O error if the list cannot be written.
    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        let body = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        atomic_file::write_creating_parent(&dir.join(FILE_NAME), &body)
    }
}

pub fn now_ms() -> u64 {
    ui::time::wall()
        .duration_since(ui::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_use_leads_and_a_path_appears_once() {
        let mut recent = RecentFiles::default();
        recent.note(Path::new("/docs/a.txt"), RecentUse::Opened, 10);
        recent.note(Path::new("/docs/b.txt"), RecentUse::Changed, 20);
        recent.note(Path::new("/docs/a.txt"), RecentUse::Opened, 30);
        let paths: Vec<_> = recent
            .items()
            .iter()
            .map(|item| item.path.clone())
            .collect();
        assert_eq!(
            paths,
            vec![PathBuf::from("/docs/a.txt"), PathBuf::from("/docs/b.txt")]
        );
        assert_eq!(recent.items()[0].at_ms, 30);
    }

    #[test]
    fn a_backwards_clock_still_orders_by_time() {
        let mut recent = RecentFiles::default();
        recent.note(Path::new("/a"), RecentUse::Opened, 50);
        recent.note(Path::new("/b"), RecentUse::Opened, 40);
        assert_eq!(recent.items()[0].path, PathBuf::from("/a"));
    }

    #[test]
    fn relative_paths_and_the_recent_place_are_not_recorded() {
        let mut recent = RecentFiles::default();
        recent.note(&location(), RecentUse::Opened, 1);
        recent.note(Path::new("notes.txt"), RecentUse::Opened, 2);
        assert!(recent.items().is_empty());
    }

    #[test]
    fn the_list_is_bounded() {
        let mut recent = RecentFiles::default();
        for i in 0..(MAX_RECENT as u64 + 5) {
            recent.note(&PathBuf::from(format!("/f/{i}")), RecentUse::Opened, i);
        }
        assert_eq!(recent.items().len(), MAX_RECENT);
        assert_eq!(
            recent.items()[0].path,
            PathBuf::from(format!("/f/{}", MAX_RECENT as u64 + 4))
        );
    }

    #[test]
    fn renames_follow_files_and_folders() {
        let mut recent = RecentFiles::default();
        recent.note(Path::new("/w/old/a.txt"), RecentUse::Opened, 1);
        recent.note(Path::new("/w/b.txt"), RecentUse::Opened, 2);
        recent.moved(Path::new("/w/old"), Path::new("/w/new"));
        recent.moved(Path::new("/w/b.txt"), Path::new("/w/c.txt"));
        let paths: Vec<_> = recent
            .items()
            .iter()
            .map(|item| item.path.clone())
            .collect();
        assert_eq!(
            paths,
            vec![PathBuf::from("/w/c.txt"), PathBuf::from("/w/new/a.txt")]
        );
    }

    #[test]
    fn round_trips_through_the_config_dir() {
        let dir = tempfile::tempdir().unwrap();
        let mut recent = RecentFiles::default();
        recent.note(Path::new("/docs/a.txt"), RecentUse::Changed, 7);
        recent.save(dir.path()).unwrap();
        assert_eq!(RecentFiles::load(dir.path()), recent);
        assert!(recent.forget(Path::new("/docs/a.txt")));
        assert!(!recent.forget(Path::new("/docs/a.txt")));
    }
}
