//! Collaborative, persistent recent-directory projection.

use std::path::{Path, PathBuf};
use ui::doc::{read_bool, read_i64_coerced, read_str};

use ui::automerge::transaction::Transactable;
use ui::automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};

const RECENTS: &str = "recent_directories";
const LIMIT: usize = 5;

pub struct DiskUsageDoc;

impl DiskUsageDoc {
    pub fn recent(doc: &AutoCommit) -> Vec<PathBuf> {
        let Some(rows) = map_read(doc, &ROOT, RECENTS) else {
            return Vec::new();
        };
        let mut recent = doc
            .keys(&rows)
            .filter_map(|id| {
                let row = map_read(doc, &rows, &id)?;
                if read_bool(doc, &row, "deleted").unwrap_or(false) {
                    return None;
                }
                Some((
                    read_i64_coerced(doc, &row, "rank").unwrap_or_default(),
                    id,
                    PathBuf::from(read_str(doc, &row, "path")?),
                ))
            })
            .collect::<Vec<_>>();
        recent.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
        recent
            .into_iter()
            .take(LIMIT)
            .map(|(_, _, path)| path)
            .collect()
    }

    pub fn remember(doc: &mut AutoCommit, path: &Path) {
        let Some(rows) = map_make(doc, &ROOT, RECENTS) else {
            return;
        };
        let id = stable_id(path);
        let Some(row) = map_make(doc, &rows, &id) else {
            return;
        };
        let next_rank = doc
            .keys(&rows)
            .filter_map(|key| map_read(doc, &rows, &key))
            .filter(|candidate| !read_bool(doc, candidate, "deleted").unwrap_or(false))
            .map(|candidate| read_i64_coerced(doc, &candidate, "rank").unwrap_or_default())
            .min()
            .unwrap_or(0)
            .saturating_sub(1);
        ui::doc::put_if_changed(doc, &row, "path", path.to_string_lossy().as_ref());
        ui::doc::put_if_changed(doc, &row, "rank", next_rank);
        ui::doc::put_if_changed(doc, &row, "deleted", false);

        let keep = Self::recent(doc);
        let keep_ids = keep
            .iter()
            .map(|candidate| stable_id(candidate))
            .collect::<std::collections::HashSet<_>>();
        let keys = doc.keys(&rows).collect::<Vec<_>>();
        for key in keys {
            if !keep_ids.contains(&key)
                && let Some(candidate) = map_read(doc, &rows, &key)
            {
                ui::doc::put_if_changed(doc, &candidate, "deleted", true);
            }
        }
    }
}

fn map_read(doc: &AutoCommit, parent: &ObjId, key: &str) -> Option<ObjId> {
    match doc.get(parent, key).ok().flatten() {
        Some((Value::Object(ObjType::Map), id)) => Some(id),
        _ => None,
    }
}

fn map_make(doc: &mut AutoCommit, parent: &ObjId, key: &str) -> Option<ObjId> {
    map_read(doc, parent, key).or_else(|| doc.put_object(parent, key, ObjType::Map).ok())
}

fn stable_id(path: &Path) -> String {
    let mut hasher = fnv::FnvHasher::default();
    std::hash::Hasher::write(&mut hasher, path.to_string_lossy().as_bytes());
    let hash = std::hash::Hasher::finish(&hasher);
    format!("path-{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recents_are_deduplicated_and_bounded() {
        let mut doc = AutoCommit::new();
        for index in 0..8 {
            DiskUsageDoc::remember(&mut doc, Path::new(&format!("/tmp/{index}")));
        }
        DiskUsageDoc::remember(&mut doc, Path::new("/tmp/4"));
        let recent = DiskUsageDoc::recent(&doc);
        assert_eq!(recent.len(), LIMIT);
        assert_eq!(recent.first(), Some(&PathBuf::from("/tmp/4")));
    }
}
