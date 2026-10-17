//! Exact retained results with bounded public pages. These are reporting caches;
//! completed moves remain authorized and verified by the durable v1 journal.
use super::*;
use crate::migration_review::storage::{IndexValue, SortedIndex};
#[derive(Serialize, Deserialize)]
struct Marker;
impl IndexValue for Marker {
    const RECORD_BYTES: usize = 1024 * 1024;
}
#[derive(Clone, Default)]
pub struct Paths(Option<Arc<SortedIndex<Marker>>>);
impl std::fmt::Debug for Paths {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Paths").field("count", &self.len()).finish()
    }
}
impl Paths {
    pub fn len(&self) -> usize {
        self.0.as_ref().map_or(0, |index| index.len())
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    #[cfg(test)]
    pub fn iter(&self) -> impl Iterator<Item = io::Result<PathBuf>> + '_ {
        self.0
            .iter()
            .flat_map(|index| index.iter())
            .map(|item| item.map(|(path, _)| path))
    }
    pub fn page(&self, offset: usize, limit: usize) -> io::Result<serde_json::Value> {
        if limit == 0 || limit > 16 || offset > self.len() {
            return Err(changed("Adoption result page exceeds bounds"));
        }
        let mut items = Vec::new();
        if let Some(index) = &self.0 {
            for item in index.page(offset, limit)? {
                let (path, _) = item?;
                items.push(serde_json::json!({"display":path.to_string_lossy(),"native":NativePath::encode(&path)}));
            }
        }
        let next = offset
            .checked_add(items.len())
            .ok_or_else(|| changed("Adoption result page overflow"))?;
        Ok(
            serde_json::json!({"total":self.len(),"offset":offset,"next":(next<self.len()).then_some(next),"items":items}),
        )
    }
    #[cfg(test)]
    pub(super) fn contains(&self, path: &Path) -> bool {
        self.0
            .as_ref()
            .is_some_and(|index| index.get(path).unwrap().is_some())
    }
}
#[cfg(test)]
impl PartialEq for Paths {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len()
            && self
                .iter()
                .zip(other.iter())
                .all(|(a, b)| a.unwrap() == b.unwrap())
    }
}
#[cfg(test)]
impl PartialEq<Vec<PathBuf>> for Paths {
    fn eq(&self, other: &Vec<PathBuf>) -> bool {
        self.len() == other.len() && self.iter().zip(other.iter()).all(|(a, b)| a.unwrap() == *b)
    }
}
pub(super) struct Reports {
    installed: SortedIndex<Marker>,
    collisions: SortedIndex<Marker>,
    inactive: SortedIndex<Marker>,
    merged: SortedIndex<Marker>,
}
impl Reports {
    pub(super) fn new(plan: &Plan) -> Self {
        let scratch = plan.entries.store().scratch();
        Self {
            installed: SortedIndex::with_scratch(scratch.clone()),
            collisions: SortedIndex::with_scratch(scratch.clone()),
            inactive: SortedIndex::with_scratch(scratch.clone()),
            merged: SortedIndex::with_scratch(scratch),
        }
    }
    pub(super) fn append(&mut self, entry: &Entry) -> io::Result<()> {
        let target = match entry.kind {
            Kind::Move => &mut self.installed,
            Kind::Collision { .. } => &mut self.collisions,
            Kind::InactiveCustody => &mut self.inactive,
            Kind::MergedDirectory { .. } => &mut self.merged,
        };
        for item in entry.items.iter() {
            target.insert(decoded(&item?)?, Marker, &AtomicBool::new(false))?;
        }
        Ok(())
    }
    pub(super) fn finish(mut self, report: &mut PromotionReport) -> io::Result<()> {
        let cancel = AtomicBool::new(false);
        self.installed.finish(&cancel)?;
        self.collisions.finish(&cancel)?;
        self.inactive.finish(&cancel)?;
        self.merged.finish(&cancel)?;
        report.installed = Paths(Some(Arc::new(self.installed)));
        report.collisions = Paths(Some(Arc::new(self.collisions)));
        report.inactive_custody = Paths(Some(Arc::new(self.inactive)));
        report.merged_directories = Paths(Some(Arc::new(self.merged)));
        Ok(())
    }
}

impl PromotionReport {
    pub fn result_page(
        &self,
        category: &str,
        offset: usize,
        limit: usize,
        review: bool,
    ) -> io::Result<serde_json::Value> {
        let paths = match category {
            "installed" if !review => &self.installed,
            "will_install" if review => &self.installed,
            "collisions" => &self.collisions,
            "inactive_custody" => &self.inactive_custody,
            "merged_directories" => &self.merged_directories,
            _ => return Err(changed("Unknown adoption result category")),
        };
        let mut value = paths.page(offset, limit)?;
        value["phase"] = serde_json::json!(if review { "reviewed" } else { "finished" });
        value["verified"] = serde_json::json!(self.verified);
        Ok(value)
    }
    pub fn summary(&self, review: bool) -> io::Result<serde_json::Value> {
        let mut value = serde_json::json!({
            "collisions":self.collisions.page(0,8)?["items"],"collisions_count":self.collisions.len(),
            "inactive_custody":self.inactive_custody.page(0,8)?["items"],"inactive_custody_count":self.inactive_custody.len(),
            "merged_directories":self.merged_directories.page(0,8)?["items"],"merged_directories_count":self.merged_directories.len(),
            "failed":self.failed.iter().map(|(path,error)|serde_json::json!({"path":[{"display":path.to_string_lossy(),"native":NativePath::encode(path)}],"error":error})).collect::<Vec<_>>(),"verified":self.verified
        });
        let key = if review { "will_install" } else { "installed" };
        value[key] = self.installed.page(0, 8)?["items"].clone();
        value[format!("{key}_count")] = serde_json::json!(self.installed.len());
        Ok(value)
    }
}
