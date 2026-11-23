//! What a migration keeps between its steps: each source's review with the person's choices,
//! and the journal of what already reached the workspace.
//!
//! An agent reviews a source, shows the person, chooses, and starts, each a command of its own,
//! so the review is durable. The journal is durable item by item: a run that was stopped, or a
//! computer that slept, resumes where it was, and running a migration again brings only what
//! is not in the workspace yet.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One thing a review lists, which the person chooses or leaves out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    /// What `tilo migrate choose` names it by.
    pub key: String,
    /// account, folder, contacts, calendar, collection, team, mail-account.
    pub kind: String,
    pub title: String,
    /// How many things it holds: messages, contacts, events, documents, issues.
    pub count: usize,
    /// What `count` counts.
    pub counts: String,
    /// The key of the item it belongs to: a folder's account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub within: Option<String>,
    pub chosen: bool,
    /// What the person should know before choosing it: an account whose server did not answer,
    /// and what to choose instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// What a source holds, as the person reviews it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Review {
    /// thunderbird, outline or linear.
    pub source: String,
    /// How the source names itself: the Thunderbird profile, the Outline team.
    pub title: String,
    /// The source's own handle: the Thunderbird profile id, the vault account of a service.
    pub handle: String,
    pub items: Vec<Item>,
    /// What the source holds that could not be read, each naming its path and
    /// why. A review with any of these lists only part of the source.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unreadable: Vec<String>,
}

impl Review {
    /// Choose (or leave out) the items `keys` name, with everything within them. `only` leaves
    /// out everything else first.
    ///
    /// # Errors
    /// A key names nothing in the review; nothing changes then.
    pub fn choose(&mut self, keys: &[String], chosen: bool, only: bool) -> Result<()> {
        let mut named = BTreeSet::new();
        for key in keys {
            let found: Vec<String> = self
                .items
                .iter()
                .filter(|item| item.key == *key || item.key.eq_ignore_ascii_case(key))
                .map(|item| item.key.clone())
                .collect();
            if found.is_empty() {
                let known: Vec<&str> = self.items.iter().map(|item| item.key.as_str()).collect();
                bail!(
                    "the {} review lists no {key:?}; it lists: {}",
                    self.source,
                    known.join(", ")
                );
            }
            named.extend(found);
        }
        if only {
            for item in &mut self.items {
                item.chosen = false;
            }
        }
        let within: BTreeSet<String> = self
            .items
            .iter()
            .filter(|item| {
                item.within
                    .as_ref()
                    .is_some_and(|parent| named.contains(parent))
            })
            .map(|item| item.key.clone())
            .collect();
        for item in &mut self.items {
            if named.contains(&item.key) || within.contains(&item.key) {
                item.chosen = chosen;
            }
        }
        // A folder chosen on its own brings its account (its identity) along.
        let parents: BTreeSet<String> = self
            .items
            .iter()
            .filter(|item| item.chosen)
            .filter_map(|item| item.within.clone())
            .collect();
        for item in &mut self.items {
            if parents.contains(&item.key) {
                item.chosen = true;
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn chosen(&self) -> Vec<&Item> {
        self.items.iter().filter(|item| item.chosen).collect()
    }
}

/// How the last run of a source went.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunState {
    /// Reviewed, never started.
    #[default]
    Reviewed,
    Running,
    /// Everything chosen is in the workspace.
    Complete,
    /// The person or their agent stopped it; `start` resumes.
    Stopped,
    Failed,
}

/// What reached the workspace, item by item, and how the last run went.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Journal {
    /// The keys of everything the workspace has from this source.
    pub landed: BTreeSet<String>,
    pub state: RunState,
    /// What History calls the last run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// The id every action of the last run named: History keeps it with each change the run
    /// made, so `undo` finds them by it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// What the last run brought, by kind: `messages`, `contacts`, …
    #[serde(default)]
    pub brought: std::collections::BTreeMap<String, usize>,
    /// What the last run found already there.
    #[serde(default)]
    pub already_there: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    /// What each run brought, by its id: the keys of what it landed, so `undo` forgets
    /// exactly those once the workspace took the run back.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub runs: std::collections::BTreeMap<String, BTreeSet<String>>,
    /// What the last run could not bring, each by the source's key for it: `start` again copies
    /// these and nothing the workspace already has.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed: Vec<Problem>,
    /// What the last run left out because the source cannot give it: a document it cannot
    /// export, a folder it would not list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub left_out: Vec<Problem>,
    /// The workspace folder the last run's files went into, as a path inside the workspace's
    /// files (`Google Drive/Trip`), which Files opens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
}

/// One thing a run could not bring, and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Problem {
    /// The source's key for it: a drive's item id, a path on this computer.
    pub key: String,
    /// Its name as a person reads it: its path from what was chosen.
    pub name: String,
    pub why: String,
}

/// Where a workspace's migrations keep what they know, field by field for each source: its
/// `review`, its `journal`, and what a source keeps of its own (the account it was connected
/// with, a service's journal, the ids its items were made under, the files a run wrote).
/// Whoever runs the engine says where that is.
pub trait Fields: Send + Sync {
    /// The field `field` of `source`'s migration, when it was kept.
    ///
    /// # Errors
    /// It could not be read.
    fn get(&self, source: &str, field: &str) -> Result<Option<Value>>;

    /// Keep `value` as the field `field` of `source`'s migration, durably; `None` forgets it.
    ///
    /// # Errors
    /// It could not be kept.
    fn put(&self, source: &str, field: &str, value: Option<Value>) -> Result<()>;

    /// The fields `source`'s migration keeps.
    fn fields(&self, source: &str) -> Vec<String>;

    /// The sources with a review.
    fn sources(&self) -> Vec<String>;
}

/// One workspace's migrations, as the engine reads and keeps them.
#[derive(Clone)]
pub struct Place {
    fields: Arc<dyn Fields>,
}

impl std::fmt::Debug for Place {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Place").finish_non_exhaustive()
    }
}

const REVIEW: &str = "review";
const JOURNAL: &str = "journal";

impl Place {
    /// The migrations `fields` keeps.
    #[must_use]
    pub fn new(fields: Arc<dyn Fields>) -> Self {
        Self { fields }
    }

    /// The migrations kept in files under `root`, readable only by this account.
    #[must_use]
    pub fn at(root: &Path) -> Self {
        Self::new(Arc::new(OnDisk {
            root: root.to_path_buf(),
        }))
    }

    /// The field `name` of `source`'s migration, when it was kept.
    ///
    /// # Errors
    /// It could not be read, or it is damaged.
    pub fn field<T: serde::de::DeserializeOwned>(
        &self,
        source: &str,
        name: &str,
    ) -> Result<Option<T>> {
        self.fields
            .get(source, name)?
            .map(|value| {
                serde_json::from_value(value)
                    .with_context(|| format!("the {source} migration's {name} is damaged"))
            })
            .transpose()
    }

    /// Keep `value` as the field `name` of `source`'s migration, durably.
    ///
    /// # Errors
    /// It could not be kept.
    pub fn keep_field<T: Serialize>(&self, source: &str, name: &str, value: &T) -> Result<()> {
        self.fields
            .put(source, name, Some(serde_json::to_value(value)?))
    }

    /// Forget the field `name` of `source`'s migration.
    ///
    /// # Errors
    /// It could not be forgotten.
    pub fn forget_field(&self, source: &str, name: &str) -> Result<()> {
        self.fields.put(source, name, None)
    }

    /// The fields `source`'s migration keeps.
    #[must_use]
    pub fn fields(&self, source: &str) -> Vec<String> {
        self.fields.fields(source)
    }

    /// The review of `source`, when it was reviewed.
    ///
    /// # Errors
    /// The kept review could not be read.
    pub fn review(&self, source: &str) -> Result<Option<Review>> {
        self.field(source, REVIEW)
    }

    /// Keep `review`.
    ///
    /// # Errors
    /// It could not be kept.
    pub fn keep_review(&self, review: &Review) -> Result<()> {
        self.keep_field(&review.source, REVIEW, review)
    }

    /// The journal of `source`; an empty one before its first run.
    ///
    /// # Errors
    /// The kept journal could not be read.
    pub fn journal(&self, source: &str) -> Result<Journal> {
        Ok(self.field(source, JOURNAL)?.unwrap_or_default())
    }

    /// Keep `journal`, durably.
    ///
    /// # Errors
    /// It could not be kept.
    pub fn keep_journal(&self, source: &str, journal: &Journal) -> Result<()> {
        self.keep_field(source, JOURNAL, journal)
    }

    /// The sources with a review.
    #[must_use]
    pub fn sources(&self) -> Vec<String> {
        self.fields.sources()
    }
}

/// Migrations kept in files under a folder of this account, one folder for each source and one
/// file for each field.
struct OnDisk {
    root: PathBuf,
}

impl OnDisk {
    fn path(&self, source: &str, field: &str) -> PathBuf {
        self.root
            .join(safe(source))
            .join(format!("{}.json", safe(field)))
    }
}

impl Fields for OnDisk {
    fn get(&self, source: &str, field: &str) -> Result<Option<Value>> {
        let path = self.path(source, field);
        match std::fs::read(&path) {
            Ok(bytes) => {
                Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
                    format!("{} is damaged", path.display())
                })?))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).with_context(|| format!("{} is unreadable", path.display())),
        }
    }

    fn put(&self, source: &str, field: &str, value: Option<Value>) -> Result<()> {
        let path = self.path(source, field);
        match value {
            Some(value) => tilo_files::native::atomic::write_creating_parent_with_mode(
                &path,
                &serde_json::to_vec_pretty(&value)?,
                0o600,
            )
            .with_context(|| format!("keeping {}", path.display())),
            None => match std::fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error).with_context(|| format!("forgetting {}", path.display())),
            },
        }
    }

    fn fields(&self, source: &str) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(self.root.join(safe(source))) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                entry
                    .file_name()
                    .to_str()?
                    .strip_suffix(".json")
                    .map(str::to_owned)
            })
            .collect()
    }

    fn sources(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut sources: Vec<String> = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.path().join("review.json").is_file())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect();
        sources.sort();
        sources
    }
}

/// `name` as one path component.
fn safe(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@') {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_start_matches('.')
        .to_owned()
}
