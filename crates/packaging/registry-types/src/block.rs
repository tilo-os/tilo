//! Apps blocked from the catalogue: by the person, by a workspace's policy
//! or by the device's administrator. A blocked app appears in neither search
//! nor the Store's lists; its Store page says who blocked it, and only that
//! blocker can unblock it.
//!
//! The person's list is a record of the person's own, kept by the Store in
//! its person directory: `$TILO_PERSON_DIR/blocked.json` inside the Store,
//! `<data home>/tilo-person/so.tilo.store/blocked.json` from outside it
//! (`{"blocked": ["com.example.App"]}`). The device's policy is
//! [`DEVICE_POLICY_PATH`] (`{"admin": "IT", "blocked": [...]}`), written by
//! whoever administers the device and only readable here. A workspace's
//! policy comes from the workspace itself, through
//! [`BlockList::with_workspace_policy`].

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Where the device administrator's catalogue policy lives.
pub const DEVICE_POLICY_PATH: &str = "/etc/tilo/store-policy.json";

/// Who blocked an app.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Blocker {
    /// The person using this device.
    Person,
    /// A workspace's policy, named as the workspace is.
    Workspace { workspace: String },
    /// The device's administrator, named as the policy names them; empty
    /// when it names nobody, which a page reads as "your administrator".
    Device { admin: String },
}

/// One app one blocker blocked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    /// The app's catalogue id, as `app_catalogue::CatalogueEntry::id`.
    pub app_id: String,
    pub by: Blocker,
}

/// Why an unblock was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnblockError {
    /// Someone else blocked it; only they can unblock it.
    NotTheBlocker(Vec<Blocker>),
    /// It was not blocked.
    NotBlocked,
}

impl std::fmt::Display for UnblockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotTheBlocker(_) => f.write_str("only whoever blocked this app can unblock it"),
            Self::NotBlocked => f.write_str("this app is not blocked"),
        }
    }
}

/// The person's list on disk.
#[derive(Default, Deserialize, Serialize)]
struct PersonFile {
    #[serde(default)]
    blocked: Vec<String>,
}

/// The device policy on disk.
#[derive(Default, Deserialize)]
struct PolicyFile {
    #[serde(default)]
    admin: String,
    #[serde(default)]
    blocked: Vec<String>,
}

/// Every block in force.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockList {
    blocks: Vec<Block>,
}

/// The Store, whose person directory keeps the person's list.
pub const STORE_PACKAGE_ID: &str = "so.tilo.store";

/// The person's list's file name in the Store's person directory.
pub const PERSON_LIST_FILE: &str = "blocked.json";

/// The person's list: in the Store's person directory, which the launcher
/// names `TILO_PERSON_DIR` inside the Store and keeps at
/// `<data home>/tilo-person/so.tilo.store` for everyone else (the shell).
#[must_use]
pub fn person_list_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("TILO_PERSON_DIR").filter(|dir| !dir.is_empty()) {
        return Some(PathBuf::from(dir).join(PERSON_LIST_FILE));
    }
    tilo_paths::data_dir().map(|dir| {
        dir.join("tilo-person")
            .join(STORE_PACKAGE_ID)
            .join(PERSON_LIST_FILE)
    })
}

impl BlockList {
    /// Nothing blocked.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// The person's list and the device's policy, from their usual places.
    /// A missing or unreadable file blocks nothing.
    #[must_use]
    pub fn load() -> Self {
        Self::load_from(
            person_list_path().as_deref(),
            Some(Path::new(DEVICE_POLICY_PATH)),
        )
    }

    /// [`Self::load`] from the given files.
    #[must_use]
    pub fn load_from(person: Option<&Path>, device: Option<&Path>) -> Self {
        let mut list = Self::empty();
        if let Some(file) = person.and_then(read_json::<PersonFile>) {
            for app in &file.blocked {
                list.add(app, Blocker::Person);
            }
        }
        if let Some(file) = device.and_then(read_json::<PolicyFile>) {
            let admin = file.admin.trim().to_owned();
            for app in &file.blocked {
                list.add(
                    app,
                    Blocker::Device {
                        admin: admin.clone(),
                    },
                );
            }
        }
        list
    }

    /// This list with `workspace`'s policy blocking `apps` too.
    #[must_use]
    pub fn with_workspace_policy<I, S>(mut self, workspace: &str, apps: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for app in apps {
            self.add(
                &app.into(),
                Blocker::Workspace {
                    workspace: workspace.to_owned(),
                },
            );
        }
        self
    }

    fn add(&mut self, app_id: &str, by: Blocker) {
        let app_id = app_id.trim().to_owned();
        if app_id.is_empty()
            || self
                .blocks
                .iter()
                .any(|block| block.by == by && block.app_id.eq_ignore_ascii_case(&app_id))
        {
            return;
        }
        self.blocks.push(Block { app_id, by });
    }

    #[must_use]
    pub fn is_blocked(&self, app_id: &str) -> bool {
        self.blocks
            .iter()
            .any(|block| block.app_id.eq_ignore_ascii_case(app_id))
    }

    /// Everyone who blocked `app_id`, for its Store page.
    #[must_use]
    pub fn blockers(&self, app_id: &str) -> Vec<&Blocker> {
        self.blocks
            .iter()
            .filter(|block| block.app_id.eq_ignore_ascii_case(app_id))
            .map(|block| &block.by)
            .collect()
    }

    /// Every block in force.
    #[must_use]
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    /// The person blocks `app_id`. Save with [`Self::save_person`].
    pub fn block(&mut self, app_id: &str) {
        self.add(app_id, Blocker::Person);
    }

    /// `as_blocker` lifts its own block of `app_id`.
    ///
    /// # Errors
    ///
    /// [`UnblockError::NotTheBlocker`] when only others blocked it, naming
    /// them; [`UnblockError::NotBlocked`] when nobody did.
    pub fn unblock(&mut self, app_id: &str, as_blocker: &Blocker) -> Result<(), UnblockError> {
        let before = self.blocks.len();
        self.blocks.retain(|block| {
            !(block.by == *as_blocker && block.app_id.eq_ignore_ascii_case(app_id))
        });
        if self.blocks.len() != before {
            return Ok(());
        }
        let others: Vec<Blocker> = self.blockers(app_id).into_iter().cloned().collect();
        if others.is_empty() {
            Err(UnblockError::NotBlocked)
        } else {
            Err(UnblockError::NotTheBlocker(others))
        }
    }

    /// Write the person's own blocks to `path` (see [`person_list_path`]).
    ///
    /// # Errors
    ///
    /// The file or its directory cannot be written.
    pub fn save_person(&self, path: &Path) -> std::io::Result<()> {
        let file = PersonFile {
            blocked: self
                .blocks
                .iter()
                .filter(|block| block.by == Blocker::Person)
                .map(|block| block.app_id.clone())
                .collect(),
        };
        let encoded = serde_json::to_vec_pretty(&file).map_err(std::io::Error::other)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let partial = path.with_extension("json.partial");
        std::fs::write(&partial, encoded)?;
        std::fs::rename(&partial, path)
    }
}

/// A value that changes whenever either block file changes. Only `stat`s.
#[must_use]
pub fn stamp(person: Option<&Path>, device: Option<&Path>) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for path in [person, device].into_iter().flatten() {
        path.hash(&mut hasher);
        if let Ok(meta) = std::fs::metadata(path) {
            meta.len().hash(&mut hasher);
            if let Ok(modified) = meta.modified() {
                modified.hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

/// The usual files' [`stamp`].
#[must_use]
pub fn current_stamp() -> u64 {
    stamp(
        person_list_path().as_deref(),
        Some(Path::new(DEVICE_POLICY_PATH)),
    )
}

/// Block files are a few kilobytes; a larger one is not one.
const MAX_BLOCK_FILE_BYTES: u64 = 1024 * 1024;

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > MAX_BLOCK_FILE_BYTES {
        return None;
    }
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_block_holds_until_its_own_blocker_lifts_it() {
        let dir = tempfile::tempdir().unwrap();
        let person = dir.path().join("blocked.json");
        let device = dir.path().join("store-policy.json");
        std::fs::write(&person, r#"{"blocked": ["com.example.Game"]}"#).unwrap();
        std::fs::write(
            &device,
            r#"{"admin": "Acme IT", "blocked": ["com.spotify.Client"]}"#,
        )
        .unwrap();
        let mut list = BlockList::load_from(Some(&person), Some(&device))
            .with_workspace_policy("School", ["so.tilo.tube"]);
        assert!(list.is_blocked("COM.example.game"), "ids match in any case");
        assert!(list.is_blocked("so.tilo.tube"));
        assert_eq!(
            list.blockers("com.spotify.Client"),
            [&Blocker::Device {
                admin: "Acme IT".into()
            }]
        );
        assert_eq!(
            list.unblock("com.spotify.Client", &Blocker::Person),
            Err(UnblockError::NotTheBlocker(vec![Blocker::Device {
                admin: "Acme IT".into()
            }])),
            "the person cannot lift the administrator's block"
        );
        assert_eq!(list.unblock("com.example.Game", &Blocker::Person), Ok(()));
        assert!(!list.is_blocked("com.example.Game"));

        list.block("org.example.Other");
        list.save_person(&person).unwrap();
        let reread = BlockList::load_from(Some(&person), None);
        assert!(reread.is_blocked("org.example.Other"));
        assert!(
            !reread.is_blocked("com.spotify.Client"),
            "the device's blocks are never written into the person's list"
        );
    }
}
