//! What a migration keeps on this computer between commands: each source's review with the
//! person's choices, and the journal of what already reached the workspace.
//!
//! An agent reviews a source, shows the person, chooses, and starts, each a command of its own,
//! so the review is durable. The journal is durable item by item: a run that was stopped, or a
//! computer that slept, resumes where it was, and running a migration again brings only what
//! is not in the workspace yet.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};

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
    /// The process running it, while it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// What History calls the last run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// The id every action of the last run named: what takes it back as one step
    /// (`#undo` with this `run`, or the run's entry in History).
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
}

/// Where one workspace's migrations are kept on this computer.
#[derive(Clone, Debug)]
pub struct Place {
    root: PathBuf,
}

impl Place {
    /// The migrations into `workspace` of this computer's account.
    ///
    /// # Errors
    /// The account has no state directory.
    pub fn of(workspace: &str) -> Result<Self> {
        let root = match std::env::var_os("TILO_MIGRATE_DIR") {
            Some(dir) => PathBuf::from(dir),
            None => tilo_paths::state_dir()
                .context("this account has no state directory (no HOME)")?
                .join("tilo/migrate"),
        };
        Ok(Self::at(&root.join(safe(workspace))))
    }

    #[must_use]
    pub fn at(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    /// The file `name` of `source`'s migration.
    #[must_use]
    pub fn path(&self, source: &str, name: &str) -> PathBuf {
        self.root.join(safe(source)).join(name)
    }

    /// The review of `source`, when it was reviewed.
    ///
    /// # Errors
    /// The kept review could not be read.
    pub fn review(&self, source: &str) -> Result<Option<Review>> {
        read(&self.path(source, "review.json"))
    }

    /// Keep `review`.
    ///
    /// # Errors
    /// It could not be written.
    pub fn keep_review(&self, review: &Review) -> Result<()> {
        write(&self.path(&review.source, "review.json"), review)
    }

    /// The journal of `source`; an empty one before its first run.
    ///
    /// # Errors
    /// The kept journal could not be read.
    pub fn journal(&self, source: &str) -> Result<Journal> {
        Ok(read(&self.path(source, "journal.json"))?.unwrap_or_default())
    }

    /// Keep `journal`, durably.
    ///
    /// # Errors
    /// It could not be written.
    pub fn keep_journal(&self, source: &str, journal: &Journal) -> Result<()> {
        write(&self.path(source, "journal.json"), journal)
    }

    /// Ask the run of `source` to stop at its next item.
    ///
    /// # Errors
    /// The request could not be written.
    pub fn ask_stop(&self, source: &str) -> Result<()> {
        let path = self.path(source, "stop");
        std::fs::create_dir_all(path.parent().unwrap_or(&self.root))?;
        std::fs::write(path, b"")?;
        Ok(())
    }

    /// Whether the run of `source` was asked to stop; `clear` forgets the ask.
    #[must_use]
    pub fn stop_asked(&self, source: &str, clear: bool) -> bool {
        let path = self.path(source, "stop");
        let asked = path.exists();
        if asked && clear {
            let _ = std::fs::remove_file(path);
        }
        asked
    }

    /// What `path` keeps, when it was written.
    ///
    /// # Errors
    /// It could not be read.
    pub fn read<T: serde::de::DeserializeOwned>(&self, path: &Path) -> Result<Option<T>> {
        self.within(path)?;
        read(path)
    }

    /// Keep `value` at `path`, durably.
    ///
    /// # Errors
    /// It could not be written.
    pub fn keep<T: Serialize>(&self, path: &Path, value: &T) -> Result<()> {
        self.within(path)?;
        write(path, value)
    }

    /// Refuse a path outside this workspace's migrations.
    fn within(&self, path: &Path) -> Result<()> {
        if !path.starts_with(&self.root) {
            bail!("{} is not a migration's file", path.display());
        }
        Ok(())
    }

    /// The sources with a review here.
    #[must_use]
    pub fn sources(&self) -> Vec<String> {
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

fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match std::fs::read(path) {
        Ok(bytes) => {
            Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
                format!("{} is damaged", path.display())
            })?))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("{} is unreadable", path.display())),
    }
}

/// Replace `path` with `value`, durably, readable only by this account.
fn write<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let dir = path.parent().context("a kept file has a directory")?;
    std::fs::create_dir_all(dir)?;
    let temporary = path.with_extension("tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    std::fs::rename(&temporary, path)?;
    std::fs::File::open(dir)?.sync_all()?;
    Ok(())
}
