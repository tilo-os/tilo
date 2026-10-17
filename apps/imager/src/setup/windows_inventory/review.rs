//! Review rows are derived from the granted inventory, never from app metadata.
use super::{Availability, Category, Decision, RootInventory};
use serde::Serialize;

#[derive(Serialize)]
pub struct CategoryReview {
    pub category: Category,
    pub source_id: String,
    pub source_path: String,
    pub entries: usize,
    pub known_bytes: u64,
    pub unknown_sizes: usize,
    pub unavailable: usize,
    pub decision: Decision,
    pub destination: String,
    pub paths: Vec<ReviewPath>,
}

#[derive(Serialize)]
pub struct ReviewPath {
    pub path: String,
    pub bytes: Option<u64>,
    pub availability: Availability,
    pub copy_eligible: bool,
    pub destination: String,
}

pub fn review_categories(root: &RootInventory, index: usize) -> Vec<CategoryReview> {
    let mut groups = std::collections::BTreeMap::new();
    for entry in &root.entries {
        let group = groups
            .entry(entry.category)
            .or_insert_with(|| CategoryReview {
                category: entry.category,
                source_id: root.source_id.clone(),
                source_path: root.path.to_string_lossy().into_owned(),
                entries: 0,
                known_bytes: 0,
                unknown_sizes: 0,
                unavailable: 0,
                decision: root.entry_decision(entry),
                // This is the actual relative layout used by the exporter. The
                // enclosing unique export directory is chosen only at export.
                destination: format!("source-{index}"),
                paths: Vec::new(),
            });
        group.entries += 1;
        group.known_bytes = group.known_bytes.saturating_add(entry.bytes.unwrap_or(0));
        group.unknown_sizes += usize::from(entry.bytes.is_none());
        group.unavailable += usize::from(!entry.copy_eligible);
        group.paths.push(ReviewPath {
            path: root
                .path
                .join(&entry.relative)
                .to_string_lossy()
                .into_owned(),
            bytes: entry.bytes,
            availability: entry.availability,
            copy_eligible: entry.copy_eligible,
            destination: format!("source-{index}/{}", entry.relative.to_string_lossy()),
        });
    }
    groups.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::windows_inventory::{Inventory, RootRole, SelectedRoot};
    use std::{collections::BTreeMap, fs};

    #[test]
    fn category_rows_preserve_actual_files_sizes_decisions_and_export_layout() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().canonicalize().unwrap().join("granted");
        fs::create_dir_all(path.join("userdata/account")).unwrap();
        fs::create_dir_all(path.join("steamapps/common/Game")).unwrap();
        let files: [(&str, &[u8]); 3] = [
            ("userdata/account/save.dat", b"unique save"),
            ("steamapps/common/Game/local-mod.bin", b"local modification"),
            ("unclassified.bin", b"not regenerable"),
        ];
        for (relative, bytes) in files {
            fs::write(path.join(relative), bytes).unwrap();
        }
        let mut inventory = Inventory::discover(&[SelectedRoot {
            path: path.clone(),
            role: RootRole::Steam,
        }])
        .unwrap();
        let id = inventory.roots[0].source_id.clone();
        inventory
            .select_category(&id, Category::SteamUserdata, Decision::Copy, None)
            .unwrap();
        let rows = review_categories(&inventory.roots[0], 2);
        assert_eq!(rows.len(), 3);
        let actual: BTreeMap<_, _> = rows
            .iter()
            .flat_map(|row| {
                assert_eq!(row.source_id, id);
                assert_eq!(row.source_path, path.to_string_lossy());
                assert_eq!(row.destination, "source-2");
                assert_eq!(row.entries, 1);
                assert_eq!(row.unknown_sizes, 0);
                assert_eq!(row.unavailable, 0);
                assert_eq!(row.known_bytes, row.paths[0].bytes.unwrap());
                assert_eq!(
                    row.decision,
                    if row.category == Category::SteamUserdata {
                        Decision::Copy
                    } else {
                        Decision::RetainOriginal
                    }
                );
                row.paths.iter().map(|entry| {
                    assert_eq!(entry.availability, Availability::Local);
                    assert!(entry.copy_eligible);
                    (entry.path.clone(), (entry.bytes, entry.destination.clone()))
                })
            })
            .collect();
        for (relative, bytes) in files {
            assert_eq!(
                actual[&path.join(relative).to_string_lossy().into_owned()],
                (Some(bytes.len() as u64), format!("source-2/{relative}"))
            );
        }
        // These are the existing review wire keys, not native handle state.
        let wire = serde_json::to_value(&inventory).unwrap();
        let root = &wire["roots"][0];
        assert!(root.get("pin").is_none());
        assert!(root.get("examined").is_none());
        assert_eq!(root["role"], "steam");
        assert_eq!(root["decision"], "retain-original");
        assert_eq!(root["category_decisions"]["steam-userdata"], "copy");
        assert!(
            root["entries"]
                .as_array()
                .unwrap()
                .iter()
                .all(|entry| entry["sync_verified"] == false)
        );
    }

    #[test]
    fn unknown_sizes_and_unavailable_entries_remain_visible_in_review() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().canonicalize().unwrap();
        fs::write(path.join("local.bin"), b"potentially unique").unwrap();
        let mut inventory = Inventory::discover(&[SelectedRoot {
            path,
            role: RootRole::Other,
        }])
        .unwrap();
        let entry = &mut inventory.roots[0].entries[0];
        entry.bytes = None;
        entry.availability = Availability::Placeholder;
        entry.copy_eligible = false;
        let rows = review_categories(&inventory.roots[0], 0);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].entries, 1);
        assert_eq!(rows[0].known_bytes, 0);
        assert_eq!(rows[0].unknown_sizes, 1);
        assert_eq!(rows[0].unavailable, 1);
        assert_eq!(rows[0].decision, Decision::RetainOriginal);
        assert_eq!(rows[0].paths.len(), 1);
        assert_eq!(rows[0].paths[0].bytes, None);
        assert_eq!(rows[0].paths[0].availability, Availability::Placeholder);
        assert!(!rows[0].paths[0].copy_eligible);
    }
}
