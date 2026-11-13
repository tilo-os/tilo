//! What the window selected, kept by identity so a refresh or a rebuild holds
//! it.

use super::FilesState;
use crate::resource::MAX_MOVE_ENTRIES;
use host_fs::{ObservedObject, RetainedObject};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Default)]
pub(super) struct ShownSelection {
    pub(super) changed: HashSet<PathBuf>,
    pub(super) objects: HashMap<PathBuf, (ObservedObject, Option<Arc<RetainedObject>>)>,
}

pub(super) fn folder(path: &Path) -> Option<Arc<RetainedObject>> {
    if crate::recent::is_location(path)
        || crate::applications::is_location(path)
        || crate::archive::location(path).is_some()
    {
        return None;
    }
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.is_dir() {
        return None;
    }
    let observed = ObservedObject::from_metadata(&metadata)?;
    RetainedObject::retain_shown(path, observed)
        .ok()
        .map(Arc::new)
}

impl FilesState {
    pub(super) fn retain_shown_selection(&mut self) {
        let selected = self.selection.selected();
        if selected.len() > MAX_MOVE_ENTRIES {
            self.shown_selection.objects.clear();
            self.shown_selection.changed.clear();
            return;
        }
        let entries = &self.entries;
        let previous = &mut self.shown_selection.objects;
        let changed = std::mem::take(&mut self.shown_selection.changed);
        let mut next_changed = HashSet::new();
        let mut next = HashMap::with_capacity(selected.len());
        for index in selected {
            let Some(entry) = entries.get(index) else {
                continue;
            };
            let Some(observed) = entry.observed else {
                continue;
            };
            let retained = match previous.remove(&entry.path) {
                Some((before, retained))
                    if before == observed
                        || retained.as_ref().is_some_and(|file| file.retains(observed)) =>
                {
                    if changed.contains(&entry.path) {
                        next_changed.insert(entry.path.clone());
                    }
                    retained
                }
                Some(_) => {
                    next_changed.insert(entry.path.clone());
                    None
                }
                _ => RetainedObject::retain_shown(&entry.path, observed)
                    .ok()
                    .map(Arc::new),
            };
            next.insert(entry.path.clone(), (observed, retained));
        }
        *previous = next;
        self.shown_selection.changed = next_changed;
    }
}
