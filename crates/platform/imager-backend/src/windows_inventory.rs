//! Windows migration review from explicitly selected directory grants only.
//!
//! Registry/known-folder suggestions are not grants. Cloud membership never
//! establishes a backup; every unclassified byte is potentially unique. This
//! inventory is an estimate, not a snapshot or authority for the copier.

pub(crate) mod metadata;
mod native;
pub use metadata::SteamAppManifest;
#[cfg(test)]
mod tests;

use crate::migration_directory::PinnedDirectory;
use crate::nofollow::{WindowsCopyBlock, metadata_nofollow, windows_copy_block};
use cap_fs_ext::MetadataExt;
use cap_std::fs::Dir;
pub use native::{RootSuggestion, suggestions};
use serde::Serialize;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RootRole {
    KnownFolder,
    OneDrive,
    Outlook,
    Steam,
    SteamLibrary,
    Other,
}

/// The caller must obtain independent consent for each path, including Steam
/// libraries or redirected folders mentioned by metadata in another grant.
#[derive(Clone, Debug)]
pub struct SelectedRoot {
    pub path: PathBuf,
    pub role: RootRole,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Category {
    PersonalFiles,
    SyncData,
    MailArchive,
    MailCacheOrDrafts,
    SteamUserdata,
    GameSave,
    GameMod,
    GameInstallation,
    PotentiallyUnique,
}

/// What the reviewed entry is, so a source tree can group it without a second
/// filesystem scan.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EntryKind {
    Directory,
    File,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Availability {
    Local,
    Placeholder,
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Decision {
    Copy,
    RetainOriginal,
    Exclude,
}

#[derive(Clone, Debug, Serialize)]
pub struct Entry {
    pub relative: PathBuf,
    pub kind: EntryKind,
    pub category: Category,
    /// Logical bytes, not physical allocation. None means unreadable metadata.
    pub bytes: Option<u64>,
    pub availability: Availability,
    pub copy_eligible: bool,
    /// Local bytes do not prove server-side sync or a recoverable cloud copy.
    pub sync_verified: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct CoverageGap {
    pub relative: PathBuf,
    pub reason: String,
}

#[derive(Clone, Serialize)]
pub struct RootInventory {
    pub source_id: String,
    pub path: PathBuf,
    pub role: RootRole,
    pub entries: Vec<Entry>,
    pub gaps: Vec<CoverageGap>,
    /// Suggestions only: the metadata parser never opens these paths. Only
    /// a separate selected root can authorize inventory of a referenced library.
    pub additional_grants: Vec<PathBuf>,
    /// A root already inside another selected root by real directory identity.
    /// Its content is inventoried once, through the containing root: redirected
    /// folders are deduplicated, never merged by matching names.
    pub covered_by: Option<String>,
    pub decision: Decision,
    pub exclusion_reason: Option<String>,
    /// Nested, root-relative subtrees the person explicitly excluded. Paths are
    /// matched as path prefixes, so excluding a folder excludes its contents.
    pub excluded: Vec<PathBuf>,
    pub category_decisions: std::collections::BTreeMap<Category, Decision>,
    pub category_exclusion_reasons: std::collections::BTreeMap<Category, String>,
    /// Public catalog metadata read from bounded Valve manifests.
    pub steam_apps: Vec<SteamAppManifest>,
    #[serde(skip)]
    examined: usize,
    #[serde(skip)]
    pin: Arc<PinnedDirectory>,
}

#[derive(Clone, Default, Serialize)]
pub struct Inventory {
    pub roots: Vec<RootInventory>,
}

#[derive(Default, Debug, Serialize)]
pub struct Budget {
    pub copy_bytes: u64,
    pub retained_bytes: u64,
    pub explicitly_excluded_bytes: u64,
    pub unavailable_bytes: u64,
    pub coverage_complete: bool,
}

impl Inventory {
    /// All roots must be absolute, non-overlapping and admitted before scanning.
    /// No environment-derived home/profile is consulted.
    ///
    /// # Errors
    /// Rejects excessive, overlapping, invalid or changed selected grants.
    pub fn discover(selected: &[SelectedRoot]) -> Result<Self, String> {
        if selected.len() > 64 {
            return Err("too many selected Windows roots".into());
        }
        let mut pins = Vec::new();
        for root in selected {
            pins.push(PinnedDirectory::open_absolute(&root.path).map_err(|e| e.to_string())?);
        }
        // Directory identity, not the spelling of a path, decides whether two
        // grants are the same data. A redirected folder inside a cloud root is
        // covered by that root; two different folders that merely share a name
        // stay separate.
        let mut container: Vec<Option<usize>> = vec![None; pins.len()];
        for (index, pin) in pins.iter().enumerate() {
            for (other_index, other) in pins.iter().enumerate() {
                if index == other_index || !pin.contains_identity(&other.identity) {
                    continue;
                }
                if crate::nofollow::same_directory_identity(&other.identity, &pin.identity) {
                    return Err("select each Windows root once".into());
                }
                if let Some(slot) = container.get_mut(index) {
                    *slot = Some(other_index);
                }
            }
        }
        let mut inventory = Self::default();
        for (index, root) in selected.iter().enumerate() {
            let Some(pin) = pins.get(index).map(Arc::clone) else {
                continue;
            };
            let covered_by = container.get(index).copied().flatten().and_then(|_| {
                // Resolve to the outermost container, which is the root that
                // actually walks and owns the data.
                let mut outer = index;
                for _ in 0..pins.len() {
                    match container.get(outer).copied().flatten() {
                        Some(next) => outer = next,
                        None => break,
                    }
                }
                pins.get(outer)
                    .map(|pin| format!("{}:{}", pin.identity.dev(), pin.identity.ino()))
            });
            let mut result = RootInventory {
                source_id: format!("{}:{}", pin.identity.dev(), pin.identity.ino()),
                path: root.path.clone(),
                role: root.role,
                entries: Vec::new(),
                gaps: Vec::new(),
                additional_grants: Vec::new(),
                covered_by,
                examined: 0,
                decision: Decision::RetainOriginal,
                exclusion_reason: None,
                excluded: Vec::new(),
                category_decisions: BTreeMap::default(),
                category_exclusion_reasons: BTreeMap::default(),
                steam_apps: Vec::new(),
                pin,
            };
            if result.covered_by.is_none() {
                let started = Instant::now();
                result.walk(Arc::clone(&result.pin), Path::new(""), started);
                result.pin.validate().map_err(|e| e.to_string())?;
            }
            inventory.roots.push(result);
        }
        for root in &mut inventory.roots {
            if root.covered_by.is_some() {
                continue;
            }
            for path in root.additional_grants.clone() {
                if !selected.iter().any(|grant| path.starts_with(&grant.path)) {
                    root.gap(Path::new("steamapps/libraryfolders.vdf"),
                        "referenced Steam library is unchecked and requires an independent exact path grant");
                }
            }
        }
        Ok(inventory)
    }

    /// Semantic review operation. Explicit exclusions always require a reason;
    /// nothing is excluded automatically because of an app or cloud label.
    ///
    /// # Errors
    /// Refuses unknown source identities and exclusions without an explicit reason.
    pub fn select(
        &mut self,
        source_id: &str,
        decision: Decision,
        reason: Option<String>,
    ) -> Result<(), String> {
        let root = self
            .roots
            .iter_mut()
            .find(|r| r.source_id == source_id)
            .ok_or("unknown Windows source identity")?;
        if root.covered_by.is_some() {
            return Err("Windows source is covered by another selected root".into());
        }
        if decision == Decision::Exclude && reason.as_ref().is_none_or(|s| s.trim().is_empty()) {
            return Err("excluding Windows data requires an explicit reason".into());
        }
        root.category_decisions.clear();
        root.category_exclusion_reasons.clear();
        root.decision = decision;
        root.exclusion_reason = (decision == Decision::Exclude).then_some(reason).flatten();
        Ok(())
    }

    /// Override one reviewed inventory category, without excluding other data.
    ///
    /// # Errors
    /// Refuses unknown sources/categories and exclusions without an explicit reason.
    pub fn select_category(
        &mut self,
        source_id: &str,
        category: Category,
        decision: Decision,
        reason: Option<String>,
    ) -> Result<(), String> {
        let root = self
            .roots
            .iter_mut()
            .find(|root| root.source_id == source_id)
            .ok_or("unknown Windows source identity")?;
        if !root.entries.iter().any(|entry| entry.category == category) {
            return Err("category is not present in the selected source".into());
        }
        if decision == Decision::Exclude
            && reason
                .as_ref()
                .is_none_or(|reason| reason.trim().is_empty())
        {
            return Err("excluding Windows data requires an explicit reason".into());
        }
        root.category_decisions.insert(category, decision);
        root.category_exclusion_reasons.remove(&category);
        if decision == Decision::Exclude {
            root.category_exclusion_reasons
                .insert(category, reason.unwrap_or_default());
        }
        Ok(())
    }

    /// Exclude reviewed subtrees of one selected root. Each prefix must name an
    /// entry of that root's own inventory, so a selection can never reach data
    /// outside the grant, and excluding a folder excludes everything beneath it.
    ///
    /// # Errors
    /// Refuses unknown or covered sources, non-relative prefixes and prefixes
    /// that do not match the reviewed inventory.
    pub fn select_exclusions(
        &mut self,
        source_id: &str,
        prefixes: &[PathBuf],
    ) -> Result<(), String> {
        let root = self
            .roots
            .iter_mut()
            .find(|root| root.source_id == source_id)
            .ok_or("unknown Windows source identity")?;
        if root.covered_by.is_some() {
            return Err("Windows source is covered by another selected root".into());
        }
        if prefixes.len() > 1024 {
            return Err("too many excluded Windows subtrees".into());
        }
        for prefix in prefixes {
            if prefix.as_os_str().is_empty()
                || prefix.as_os_str().len() > 4096
                || prefix
                    .components()
                    .any(|c| !matches!(c, std::path::Component::Normal(_)))
            {
                return Err("excluded Windows subtree is not a normalized relative path".into());
            }
            if !root
                .entries
                .iter()
                .any(|entry| entry.relative == *prefix || entry.relative.starts_with(prefix))
            {
                return Err("excluded Windows subtree is not in the reviewed inventory".into());
            }
        }
        root.excluded = prefixes.to_vec();
        Ok(())
    }

    pub fn budget(&self) -> Budget {
        let mut budget = Budget {
            coverage_complete: true,
            ..Budget::default()
        };
        for root in &self.roots {
            budget.coverage_complete &= root.gaps.is_empty();
            for entry in &root.entries {
                let bytes = entry.bytes.unwrap_or(0);
                if !entry.copy_eligible {
                    budget.unavailable_bytes = budget.unavailable_bytes.saturating_add(bytes);
                }
                let total = match root.entry_decision(entry) {
                    Decision::Copy if entry.copy_eligible => &mut budget.copy_bytes,
                    Decision::Exclude => &mut budget.explicitly_excluded_bytes,
                    _ => &mut budget.retained_bytes,
                };
                *total = total.saturating_add(bytes);
            }
        }
        budget
    }

    /// Refuse output within any granted source, even if that source is retained.
    ///
    /// # Errors
    /// Refuses an export destination inside any selected source.
    pub fn validate_export_destination(&self, parent: &PinnedDirectory) -> Result<(), String> {
        if self
            .roots
            .iter()
            .any(|root| parent.contains_identity(&root.pin.identity))
        {
            return Err("export destination overlaps a selected source".into());
        }
        Ok(())
    }

    /// Execute only an explicitly reviewed Copy selection with the verified
    /// copier. Skipped placeholders remain at source and are not copied.
    ///
    /// # Errors
    /// Refuses unreviewed selections, changed grants and verified-copy failures.
    pub fn copy_selected(
        &self,
        source_id: &str,
        destination: &Path,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Result<crate::migration::CopyReport, String> {
        let root = self
            .roots
            .iter()
            .find(|r| r.source_id == source_id)
            .ok_or("unknown Windows source identity")?;
        if !root.has_copy_selection() {
            return Err("Windows source was not selected for copying".into());
        }
        let selection = root
            .entries
            .iter()
            .filter(|entry| root.entry_decision(entry) == Decision::Copy)
            .map(|entry| entry.relative.clone())
            .collect();
        root.pin.validate().map_err(|e| e.to_string())?;
        crate::migration::copy_pinned_folder(
            Arc::clone(&root.pin),
            destination,
            cancelled,
            |_| {},
            None,
            Some(&selection),
        )
    }

    /// Validate the held grant before returning an opener target.
    ///
    /// # Errors
    /// Refuses unknown or changed grants and paths without a directory URI.
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

impl RootInventory {
    pub fn entry_decision(&self, entry: &Entry) -> Decision {
        if self
            .excluded
            .iter()
            .any(|prefix| entry.relative.starts_with(prefix))
        {
            return Decision::Exclude;
        }
        self.category_decisions
            .get(&entry.category)
            .copied()
            .unwrap_or(self.decision)
    }

    pub fn has_copy_selection(&self) -> bool {
        self.decision == Decision::Copy
            || self
                .category_decisions
                .values()
                .any(|decision| *decision == Decision::Copy)
    }

    /// Record one installed title from its own bounded Valve manifest. The
    /// manifest names public catalog metadata; it never authorizes a library.
    fn steam_manifest(
        &mut self,
        pin: &Arc<PinnedDirectory>,
        name: &std::ffi::OsStr,
        metadata: &cap_primitives::fs::Metadata,
        path: &Path,
    ) {
        match metadata::steam_app(pin, name, metadata) {
            Ok(Some(app)) => {
                if self.steam_apps.len() < 512 && !self.steam_apps.contains(&app) {
                    self.steam_apps.push(app);
                }
            }
            Ok(None) => self.gap(path, "Steam application manifest has no identity"),
            Err(error) => self.gap(path, error),
        }
    }

    fn gap(&mut self, relative: &Path, reason: impl ToString) {
        if self.gaps.len() < 128 {
            self.gaps.push(CoverageGap {
                relative: relative.into(),
                reason: reason.to_string(),
            });
        }
    }

    fn walk(&mut self, pin: Arc<PinnedDirectory>, relative: &Path, started: Instant) {
        if relative.components().count() > 64 {
            self.gap(relative, "directory depth limit reached");
            return;
        }
        let directory = match pin.file.try_clone() {
            Ok(file) => Dir::from_std_file(file),
            Err(error) => {
                self.gap(relative, error);
                return;
            }
        };
        let entries = match directory.entries() {
            Ok(entries) => entries,
            Err(error) => {
                self.gap(relative, error);
                return;
            }
        };
        for entry in entries {
            self.examined += 1;
            if self.examined > 50_000 || started.elapsed() > Duration::from_secs(30) {
                self.gap(
                    relative,
                    "inventory entry/time limit reached; remaining data is unchecked",
                );
                return;
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    self.gap(relative, error);
                    return;
                }
            };
            let name = entry.file_name();
            let path = relative.join(&name);
            let meta = match metadata_nofollow(&pin.file, Path::new(&name)) {
                Ok(meta) => meta,
                Err(error) => {
                    self.gap(&path, error);
                    continue;
                }
            };
            let block = windows_copy_block(&meta);
            if meta.is_dir() && !meta.is_symlink() && block.is_none() {
                match pin.open_child(&name, &meta) {
                    Ok(child) => self.walk(child, &path, started),
                    Err(error) => self.gap(&path, error),
                }
                continue;
            }
            let availability = match block {
                Some(WindowsCopyBlock::CloudUnavailable) => Availability::Placeholder,
                None if meta.is_file() && !meta.is_symlink() => Availability::Local,
                Some(_) | None => Availability::Unsupported,
            };
            let eligible = availability == Availability::Local;
            if !eligible {
                self.gap(
                    &path,
                    "entry is not copyable locally; it remains at the source",
                );
            }
            let relative_text = path
                .to_string_lossy()
                .replace('\\', "/")
                .to_ascii_lowercase();
            if eligible
                && matches!(self.role, RootRole::Steam | RootRole::SteamLibrary)
                && relative_text.starts_with("steamapps/appmanifest_")
                && path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("acf"))
            {
                self.steam_manifest(&pin, &name, &meta, &path);
            }
            if eligible
                && matches!(self.role, RootRole::Steam | RootRole::SteamLibrary)
                && matches!(
                    relative_text.as_str(),
                    "steamapps/libraryfolders.vdf" | "config/libraryfolders.vdf"
                )
            {
                match metadata::steam_libraries(&pin, &name, &meta) {
                    Ok(paths) => {
                        for path in paths {
                            if !self.additional_grants.contains(&path) {
                                if self.additional_grants.len() >= 64 {
                                    self.gap(relative, "Steam library suggestion limit reached");
                                    break;
                                }
                                self.additional_grants.push(path);
                            }
                        }
                    }
                    Err(error) => self.gap(&path, error),
                }
            }
            self.entries.push(Entry {
                category: category(self.role, &path),
                kind: if meta.is_dir() && !meta.is_symlink() {
                    EntryKind::Directory
                } else if meta.is_file() && !meta.is_symlink() {
                    EntryKind::File
                } else {
                    EntryKind::Other
                },
                relative: path,
                bytes: meta.is_file().then_some(meta.len()),
                availability,
                copy_eligible: eligible,
                sync_verified: false,
            });
        }
        if let Err(error) = pin.validate() {
            self.gap(relative, error);
        }
    }
}

fn category(role: RootRole, path: &Path) -> Category {
    let text = path
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let extension = path.extension();
    if extension.is_some_and(|ext| ext.eq_ignore_ascii_case("ost"))
        || text.split('/').any(|s| s == "drafts")
    {
        Category::MailCacheOrDrafts
    } else if extension.is_some_and(|ext| {
        ["pst", "mbox", "eml"]
            .iter()
            .any(|candidate| ext.eq_ignore_ascii_case(candidate))
    }) {
        Category::MailArchive
    } else if text.split('/').any(|s| s == "userdata") {
        Category::SteamUserdata
    } else if text
        .split('/')
        .any(|s| matches!(s, "save" | "saves" | "saved games" | "remote"))
    {
        Category::GameSave
    } else if text.split('/').any(|s| matches!(s, "mods" | "workshop")) {
        Category::GameMod
    } else if text.starts_with("steamapps/common/") {
        // Installation directories can contain mods/local changes: this label
        // does not make them regenerable or authorize automatic exclusion.
        Category::GameInstallation
    } else {
        match role {
            RootRole::OneDrive => Category::SyncData,
            RootRole::KnownFolder => Category::PersonalFiles,
            _ => Category::PotentiallyUnique,
        }
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
