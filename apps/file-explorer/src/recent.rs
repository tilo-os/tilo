//! Recent files: what you opened or changed through Files, newest first.
//!
//! The lists are a record beside the folder ([`RecentRecord`]), one per
//! device: a device's list holds its own paths only, and the Recent listing
//! skips entries whose file has gone. The Recent place in the sidebar lists
//! it in recency order rather than by the folder sort.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ui::prelude::{Model, resource, tilo_resource};
use ui::tilo_resource::{AbsolutePath, JsonSchema};

/// The pseudo-location the Recent place navigates to. It is relative and
/// scheme-like so it can never name a real directory Files would list or
/// write into.
pub const RECENT_LOCATION: &str = "recent://";

pub fn location() -> PathBuf {
    PathBuf::from(RECENT_LOCATION)
}

pub fn is_location(path: &Path) -> bool {
    path.as_os_str() == RECENT_LOCATION
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
    items: ui::tilo_resource::Recents<RecentItem, 200>,
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
        let item = RecentItem {
            path: path.to_path_buf(),
            at_ms,
            last_use,
        };
        self.items
            .remember_by(item, |kept, item| kept.path == item.path);
        // Keep newest first even if the clock stepped backwards.
        self.items.sort_by_key(|item| std::cmp::Reverse(item.at_ms));
    }

    /// Drop `path` from the list. Returns whether it was there.
    pub fn forget(&mut self, path: &Path) -> bool {
        self.items.retain(|item| item.path != path)
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
}

tilo_resource::json_model!(RecentFiles);

/// Every device's Recent list, by device id: a record Files keeps beside its
/// own in the workspace, synced with it. Paths name files on one device, so
/// each device reads and writes only its own list.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct RecentRecord {
    pub devices: BTreeMap<String, RecentFiles>,
}

#[resource(kind = "files.recent", label = "Recent files")]
impl RecentRecord {
    /// `path` on `device` was opened or changed at `at_ms`: it moves to the
    /// front.
    #[action(housekeeping)]
    pub fn note(&mut self, device: String, path: AbsolutePath, last_use: RecentUse, at_ms: u64) {
        self.devices
            .entry(device)
            .or_default()
            .note(path.as_path(), last_use, at_ms);
    }

    /// `from` on `device` was renamed or moved to `to`: its entries follow.
    #[action(housekeeping)]
    pub fn moved(&mut self, device: String, from: AbsolutePath, to: AbsolutePath) {
        if let Some(recent) = self.devices.get_mut(&device) {
            recent.moved(from.as_path(), to.as_path());
        }
    }

    /// Forget `path` on `device`; the file is untouched.
    #[action]
    pub fn forget(&mut self, device: String, path: AbsolutePath) {
        if let Some(recent) = self.devices.get_mut(&device) {
            recent.forget(path.as_path());
        }
    }
}

impl RecentRecord {
    /// `device`'s Recent list.
    #[must_use]
    pub fn on(&self, device: &str) -> RecentFiles {
        self.devices.get(device).cloned().unwrap_or_default()
    }
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
    fn forgetting_reports_whether_the_path_was_listed() {
        let mut recent = RecentFiles::default();
        recent.note(Path::new("/docs/a.txt"), RecentUse::Changed, 7);
        assert!(recent.forget(Path::new("/docs/a.txt")));
        assert!(!recent.forget(Path::new("/docs/a.txt")));
    }
}
