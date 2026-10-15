//! Review rows are derived from the granted inventory, never from app metadata.
use super::{Availability, Category, Decision, Inventory, RootInventory};
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

impl RootInventory {
    pub fn review_categories(&self, index: usize) -> Vec<CategoryReview> {
        let mut groups = std::collections::BTreeMap::new();
        for entry in &self.entries {
            let group = groups
                .entry(entry.category)
                .or_insert_with(|| CategoryReview {
                    category: entry.category,
                    source_id: self.source_id.clone(),
                    source_path: self.path.to_string_lossy().into_owned(),
                    entries: 0,
                    known_bytes: 0,
                    unknown_sizes: 0,
                    unavailable: 0,
                    decision: self.entry_decision(entry),
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
                path: self
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
}

impl Inventory {
    /// Open only the exact granted folder. Neither a caller-supplied path nor
    /// an unchecked library reference can become an opener target. Opening the
    /// root rather than a file also avoids launching an executable from it.
    pub fn source_folder_uri(&self, source_id: &str) -> Result<String, String> {
        let root = self
            .roots
            .iter()
            .find(|root| root.source_id == source_id)
            .ok_or("unknown Windows source identity")?;
        root.pin.validate().map_err(|error| error.to_string())?;
        url::Url::from_directory_path(&root.path)
            .map(|uri| uri.to_string())
            .map_err(|()| "source cannot be represented as a file URI".into())
    }
}
