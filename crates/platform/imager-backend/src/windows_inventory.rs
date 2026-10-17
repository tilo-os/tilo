//! Windows migration review from explicitly selected directory grants only.
//!
//! Registry/known-folder suggestions are not grants. Cloud membership never
//! establishes a backup; every unclassified byte is potentially unique. This
//! inventory is an estimate, not a snapshot or authority for the copier.

mod metadata;
mod native;
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
    pub decision: Decision,
    pub exclusion_reason: Option<String>,
    pub category_decisions: std::collections::BTreeMap<Category, Decision>,
    pub category_exclusion_reasons: std::collections::BTreeMap<Category, String>,
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
        for (index, root) in selected.iter().enumerate() {
            for other in &selected[..index] {
                if root.path.starts_with(&other.path) || other.path.starts_with(&root.path) {
                    return Err("select non-overlapping Windows roots".into());
                }
            }
            let pin = PinnedDirectory::open_absolute(&root.path).map_err(|e| e.to_string())?;
            if pins.iter().any(|other: &Arc<PinnedDirectory>| {
                other.contains_identity(&pin.identity) || pin.contains_identity(&other.identity)
            }) {
                return Err("selected Windows roots overlap by identity".into());
            }
            pins.push(pin);
        }
        let mut inventory = Self::default();
        for (root, pin) in selected.iter().zip(pins) {
            let mut result = RootInventory {
                source_id: format!("{}:{}", pin.identity.dev(), pin.identity.ino()),
                path: root.path.clone(),
                role: root.role,
                entries: Vec::new(),
                gaps: Vec::new(),
                additional_grants: Vec::new(),
                examined: 0,
                decision: Decision::RetainOriginal,
                exclusion_reason: None,
                category_decisions: BTreeMap::default(),
                category_exclusion_reasons: BTreeMap::default(),
                pin,
            };
            let started = Instant::now();
            result.walk(Arc::clone(&result.pin), Path::new(""), started);
            result.pin.validate().map_err(|e| e.to_string())?;
            inventory.roots.push(result);
        }
        for root in &mut inventory.roots {
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
