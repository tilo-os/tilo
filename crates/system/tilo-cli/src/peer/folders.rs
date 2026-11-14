//! Which of the person's workspaces this computer keeps a folder of, where,
//! and whether it syncs: what `tilo folders` changes and the peer applies.
//!
//! Kept beside the device (`folders.json`); the peer reads it again on its
//! next look, so a change takes effect within seconds without a restart.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization as _;

/// The file, in the device's directory.
pub(crate) const FILE: &str = "folders.json";

/// This computer's synced folders.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Folders {
    /// Whether a workspace of the person's own gets a folder as soon as this
    /// computer learns of it.
    #[serde(default = "yes")]
    pub(crate) auto_own: bool,
    /// Whether a workspace the person joined gets one too.
    #[serde(default)]
    pub(crate) auto_joined: bool,
    /// By workspace id.
    #[serde(default)]
    pub(crate) workspaces: BTreeMap<String, Folder>,
}

impl Default for Folders {
    fn default() -> Self {
        Self {
            auto_own: true,
            auto_joined: false,
            workspaces: BTreeMap::new(),
        }
    }
}

const fn yes() -> bool {
    true
}

/// One workspace's folder here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Folder {
    /// The workspace's name when the folder was made.
    pub(crate) name: String,
    /// Where it is.
    pub(crate) path: PathBuf,
    /// Not syncing: changes made here wait, others' changes do not arrive.
    #[serde(default)]
    pub(crate) paused: bool,
    /// Removed by the person: not shown here, and not added back by itself.
    #[serde(default)]
    pub(crate) removed: bool,
}

impl Folders {
    /// The folders kept in `device_dir`; none yet when it has none.
    ///
    /// # Errors
    /// The file exists but cannot be read.
    pub(crate) fn load(device_dir: &Path) -> anyhow::Result<Self> {
        match std::fs::read(device_dir.join(FILE)) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }

    /// Keep these folders in `device_dir`.
    ///
    /// # Errors
    /// They cannot be written.
    pub(crate) fn save(&self, device_dir: &Path) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec_pretty(self)?;
        tilo_files::native::atomic::write_with_mode(&device_dir.join(FILE), &bytes, 0o600)?;
        Ok(())
    }

    /// Give `workspace`, called `name`, a folder when this computer has none
    /// for it and the person did not remove it: of their own when `auto_own`,
    /// one they joined when `auto_joined`. Whether one was added.
    pub(crate) fn offer(&mut self, workspace: &str, name: &str, own: bool, home: &Path) -> bool {
        if self.workspaces.contains_key(workspace)
            || !(if own { self.auto_own } else { self.auto_joined })
        {
            return false;
        }
        let path = self.free_path(home, name);
        self.workspaces.insert(
            workspace.to_owned(),
            Folder {
                name: name.to_owned(),
                path,
                paused: false,
                removed: false,
            },
        );
        true
    }

    /// The folder a workspace called `name` gets below `home`: `~/<name>`,
    /// unless that path is another folder's or holds the person's own files.
    pub(crate) fn free_path(&self, home: &Path, name: &str) -> PathBuf {
        let name = folder_name(name);
        let taken = |path: &Path| {
            self.workspaces
                .values()
                .any(|folder| !folder.removed && folder.path == path)
                || holds_files(path)
        };
        let plain = home.join(&name);
        if !taken(&plain) {
            return plain;
        }
        (1..=1000_u32)
            .map(|n| {
                home.join(if n == 1 {
                    format!("{name} (Tilo)")
                } else {
                    format!("{name} (Tilo {n})")
                })
            })
            .find(|path| !taken(path))
            .unwrap_or(plain)
    }

    /// The workspace whose folder is `at` or holds it, or whose id or name is `at`.
    pub(crate) fn find(&self, at: &str) -> Option<String> {
        let path = std::path::absolute(at).ok();
        self.workspaces
            .iter()
            .find(|(id, folder)| {
                id.as_str() == at
                    || folder.name == at
                    || path
                        .as_ref()
                        .is_some_and(|path| path.starts_with(&folder.path))
            })
            .map(|(id, _)| id.clone())
    }
}

/// A workspace's name as a folder name: one path component, composed as
/// macOS and Linux both read it back (NFC), with what no folder name may hold
/// replaced.
pub(crate) fn folder_name(name: &str) -> String {
    let name: String = name
        .nfc()
        .map(|c| {
            if c == '/' || c == '\0' || c.is_control() {
                '-'
            } else {
                c
            }
        })
        .collect();
    let name = name.trim().trim_start_matches('.').trim();
    if name.is_empty() {
        "Workspace".to_owned()
    } else {
        name.to_owned()
    }
}

/// Whether `path` is a folder with something in it (or a file): the
/// person's, which a workspace is never mounted over.
fn holds_files(path: &Path) -> bool {
    match std::fs::read_dir(path) {
        Ok(mut entries) => entries.next().is_some(),
        Err(error) => error.kind() != std::io::ErrorKind::NotFound,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_workspace_folder_lands_at_its_name_and_never_over_the_persons_files() {
        let home = tempfile::tempdir().unwrap();
        let mut folders = Folders::default();
        assert!(folders.offer("w1", "Acme", true, home.path()));
        assert_eq!(folders.workspaces["w1"].path, home.path().join("Acme"));
        assert!(!folders.offer("w1", "Acme", true, home.path()), "once");
        // Joined: only when the person asked for it.
        assert!(!folders.offer("w2", "Club", false, home.path()));
        folders.auto_joined = true;
        assert!(folders.offer("w2", "Club", false, home.path()));

        // The person's own Documents folder is never mounted over; an empty one is fine.
        std::fs::create_dir_all(home.path().join("Documents/old")).unwrap();
        assert!(folders.offer("w3", "Documents", true, home.path()));
        assert_eq!(
            folders.workspaces["w3"].path,
            home.path().join("Documents (Tilo)")
        );
        std::fs::create_dir_all(home.path().join("Empty")).unwrap();
        assert_eq!(
            folders.free_path(home.path(), "Empty"),
            home.path().join("Empty")
        );
        // Two workspaces of one name.
        assert!(folders.offer("w4", "Acme", true, home.path()));
        assert_eq!(
            folders.workspaces["w4"].path,
            home.path().join("Acme (Tilo)")
        );

        // A removed folder is not offered again.
        folders.workspaces.get_mut("w1").unwrap().removed = true;
        assert!(!folders.offer("w1", "Acme", true, home.path()));

        assert_eq!(folders.find("w2"), Some("w2".into()));
        assert_eq!(folders.find("Club"), Some("w2".into()));
        let inside = home.path().join("Documents (Tilo)/a/b.md");
        assert_eq!(folders.find(inside.to_str().unwrap()), Some("w3".into()));

        folders.save(home.path()).unwrap();
        assert_eq!(Folders::load(home.path()).unwrap(), folders);
        assert_eq!(
            Folders::load(&home.path().join("none")).unwrap(),
            Folders::default()
        );
    }

    #[test]
    fn a_folder_name_is_one_composed_component() {
        assert_eq!(folder_name("a/b"), "a-b");
        assert_eq!(folder_name("..hidden"), "hidden");
        assert_eq!(folder_name("  "), "Workspace");
        // "é" decomposed (as macOS once stored it) comes back composed.
        assert_eq!(folder_name("Caf\u{65}\u{301}"), "Caf\u{e9}");
    }
}
