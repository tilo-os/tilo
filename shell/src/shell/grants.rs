//! Per-workspace portal-grant persistence.
//!
//! When a running app calls `runtime_cli::request_grant` (or the
//! moral equivalent inside its manifest), the compositor decides
//! whether to prompt the user, record the answer, and — for
//! approved requests — persist the grant so a subsequent launch of
//! the same package in the same workspace runs with the granted
//! paths mounted without a fresh prompt.
//!
//! Storage layout (on disk):
//!
//! ```text
//! $XDG_STATE_HOME/qos-shell/grants/
//!   <workspace-id>.toml
//! ```
//!
//! Each workspace file is a `Vec<WorkspaceGrantEntry>` — one row
//! per `(pkg_id, grant)` pair. Absence of an entry == no grant,
//! not "denied" (a denial is not persisted; the user is asked
//! again on the next launch).
//!
//! The launcher reads the same file when the manifest carries
//! `[sandbox] workspace_grants = "auto"` and adds any read/write
//! paths to its bwrap bind list before spawning bwrap. See
//! `crates/packaging/launcher/src/manifest.rs`.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One grant. Wire form is TOML `{ path = "...", mode = "read"|"write" }`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct Grant {
    /// Absolute path being granted access. Relative paths are
    /// rejected at write time — apps must ask for real paths.
    pub path: PathBuf,
    #[serde(default)]
    pub mode: GrantMode,
}

/// Read vs. read+write. Wire form is lowercase.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum GrantMode {
    #[default]
    Read,
    Write,
}

/// One `(pkg_id, grant)` row inside a workspace grant file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceGrantEntry {
    /// Reverse-DNS package id the grant applies to.
    pub pkg_id: String,
    /// The path + mode granted. One row per grant so multiple
    /// grants to the same package produce multiple entries.
    pub grant: Grant,
}

/// On-disk shape of `<workspace-id>.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspaceGrantFile {
    #[serde(default, rename = "entry")]
    pub entries: Vec<WorkspaceGrantEntry>,
}

impl WorkspaceGrantFile {
    /// Grants scoped to a single package id, deduped by (path, mode).
    pub fn for_pkg(&self, pkg_id: &str) -> Vec<Grant> {
        let mut out: Vec<Grant> = Vec::new();
        for e in &self.entries {
            if e.pkg_id != pkg_id {
                continue;
            }
            if !out.contains(&e.grant) {
                out.push(e.grant.clone());
            }
        }
        out
    }
}

/// Filesystem-backed grant store. All operations are single-file
/// TOML round-trips; concurrent writers to the same workspace race
/// on `rename(2)` and last-writer-wins — good enough for a single
/// desktop session.
#[derive(Debug, Clone)]
pub struct GrantRegistry {
    root: PathBuf,
}

impl GrantRegistry {
    /// Open the default store at `$XDG_STATE_HOME/qos-shell/grants/`
    /// (creating the dir if absent).
    pub fn open_default() -> Result<Self, GrantError> {
        let root = default_grants_dir()?;
        Self::open_at(&root)
    }

    /// Open a store rooted at `root`. Creates it if absent.
    pub fn open_at(root: &Path) -> Result<Self, GrantError> {
        fs::create_dir_all(root).map_err(|source| GrantError::Io {
            path: root.to_path_buf(),
            source,
        })?;
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    /// Root directory the store manages — one TOML per workspace.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Filename for a given workspace id. Sanitises `/` and `..`
    /// out of the id so a caller-controlled string can't escape
    /// the store root.
    pub fn path_for(&self, workspace_id: &str) -> PathBuf {
        self.root.join(format!("{}.toml", sanitise(workspace_id)))
    }

    /// Read a workspace's grant file. Returns an empty file when it
    /// doesn't exist (a fresh workspace has no grants).
    pub fn load(&self, workspace_id: &str) -> Result<WorkspaceGrantFile, GrantError> {
        let path = self.path_for(workspace_id);
        if !path.exists() {
            return Ok(WorkspaceGrantFile::default());
        }
        let text = fs::read_to_string(&path).map_err(|source| GrantError::Io {
            path: path.clone(),
            source,
        })?;
        toml::from_str(&text).map_err(|e| GrantError::Parse {
            path,
            reason: e.to_string(),
        })
    }

    /// Overwrite the workspace grant file. Atomic rename so a
    /// crashed writer can't leave a truncated file behind.
    pub fn save(
        &self,
        workspace_id: &str,
        file: &WorkspaceGrantFile,
    ) -> Result<(), GrantError> {
        let path = self.path_for(workspace_id);
        let text = toml::to_string(file).map_err(|e| GrantError::Serialise(e.to_string()))?;
        let tmp = path.with_extension("toml.tmp");
        fs::write(&tmp, text).map_err(|source| GrantError::Io {
            path: tmp.clone(),
            source,
        })?;
        fs::rename(&tmp, &path).map_err(|source| GrantError::Io {
            path: path.clone(),
            source,
        })?;
        Ok(())
    }

    /// Append a grant, deduping by (pkg_id, path, mode). Returns
    /// `true` when the grant was new, `false` when it already
    /// existed (no rewrite in that case).
    pub fn add_grant(
        &self,
        workspace_id: &str,
        pkg_id: &str,
        grant: Grant,
    ) -> Result<bool, GrantError> {
        if !grant.path.is_absolute() {
            return Err(GrantError::RelativePath(grant.path.clone()));
        }
        let mut file = self.load(workspace_id)?;
        let exists = file
            .entries
            .iter()
            .any(|e| e.pkg_id == pkg_id && e.grant == grant);
        if exists {
            return Ok(false);
        }
        file.entries.push(WorkspaceGrantEntry {
            pkg_id: pkg_id.to_string(),
            grant,
        });
        self.save(workspace_id, &file)?;
        Ok(true)
    }

    /// Grants persisted for `(workspace_id, pkg_id)`. Empty vec
    /// means "no persisted grants" — the launcher then falls back
    /// to the manifest's declared paths (which may prompt on
    /// first access).
    pub fn grants_for(
        &self,
        workspace_id: &str,
        pkg_id: &str,
    ) -> Result<Vec<Grant>, GrantError> {
        Ok(self.load(workspace_id)?.for_pkg(pkg_id))
    }

    /// Enumerate the workspace ids the store has any file for.
    /// Order is directory-scan order (not stable across filesystems).
    pub fn known_workspaces(&self) -> Result<Vec<String>, GrantError> {
        let mut out = Vec::new();
        if !self.root.exists() {
            return Ok(out);
        }
        for e in fs::read_dir(&self.root).map_err(|source| GrantError::Io {
            path: self.root.clone(),
            source,
        })? {
            let e = e.map_err(|source| GrantError::Io {
                path: self.root.clone(),
                source,
            })?;
            let name = e.file_name();
            let s = name.to_string_lossy();
            if let Some(stem) = s.strip_suffix(".toml") {
                out.push(stem.to_string());
            }
        }
        out.sort();
        Ok(out)
    }
}

/// Errors the grant store surfaces.
#[derive(Debug, thiserror::Error)]
pub enum GrantError {
    #[error("HOME / XDG_STATE_HOME not set — cannot locate grants dir")]
    NoHome,
    #[error("io on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("parse {path}: {reason}")]
    Parse { path: PathBuf, reason: String },
    #[error("serialise: {0}")]
    Serialise(String),
    #[error("grant path must be absolute: {0}")]
    RelativePath(PathBuf),
}

/// Sanitise a workspace id into a filesystem-safe basename. Any
/// character outside `[A-Za-z0-9._-]` is rewritten to `_`.
fn sanitise(id: &str) -> String {
    id.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '.' | '_' | '-' => c,
            _ => '_',
        })
        .collect()
}

/// Default store root: `$XDG_STATE_HOME/qos-shell/grants` else
/// `$HOME/.local/state/qos-shell/grants`. `QOS_SHELL_STATE_DIR` (a
/// dev/test hook) wins when set.
pub fn default_grants_dir() -> Result<PathBuf, GrantError> {
    if let Some(dir) = std::env::var_os("QOS_SHELL_STATE_DIR") {
        return Ok(PathBuf::from(dir).join("grants"));
    }
    if let Some(xdg) = std::env::var_os("XDG_STATE_HOME") {
        if !xdg.is_empty() {
            return Ok(PathBuf::from(xdg).join("qos-shell").join("grants"));
        }
    }
    let home = std::env::var_os("HOME").ok_or(GrantError::NoHome)?;
    Ok(Path::new(&home)
        .join(".local/state")
        .join("qos-shell")
        .join("grants"))
}

/// Small helper: group grants for a whole workspace by pkg id.
/// Not used by the compositor today, but the app-store UI wants
/// this shape to show "what has app X been granted where?".
pub fn grants_by_pkg(file: &WorkspaceGrantFile) -> BTreeMap<String, Vec<Grant>> {
    let mut out: BTreeMap<String, Vec<Grant>> = BTreeMap::new();
    for e in &file.entries {
        out.entry(e.pkg_id.clone()).or_default().push(e.grant.clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_grant_dedupes() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GrantRegistry::open_at(tmp.path()).unwrap();
        let g = Grant {
            path: PathBuf::from("/home/u/Documents"),
            mode: GrantMode::Read,
        };
        assert!(store
            .add_grant("ws-1", "dev.qos.notes", g.clone())
            .unwrap(),);
        assert!(!store
            .add_grant("ws-1", "dev.qos.notes", g.clone())
            .unwrap());
        let grants = store.grants_for("ws-1", "dev.qos.notes").unwrap();
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].path, PathBuf::from("/home/u/Documents"));
    }

    #[test]
    fn per_workspace_isolation() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GrantRegistry::open_at(tmp.path()).unwrap();
        let g = Grant {
            path: PathBuf::from("/home/u/Documents"),
            mode: GrantMode::Read,
        };
        store
            .add_grant("ws-x", "dev.qos.notes", g.clone())
            .unwrap();
        assert_eq!(
            store.grants_for("ws-x", "dev.qos.notes").unwrap().len(),
            1
        );
        // Same pkg, different workspace — no grant.
        assert!(store
            .grants_for("ws-y", "dev.qos.notes")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn relative_paths_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GrantRegistry::open_at(tmp.path()).unwrap();
        let g = Grant {
            path: PathBuf::from("relative/dir"),
            mode: GrantMode::Read,
        };
        let err = store
            .add_grant("ws-1", "dev.qos.notes", g)
            .expect_err("relative rejected");
        assert!(matches!(err, GrantError::RelativePath(_)));
    }

    #[test]
    fn sanitise_strips_slashes() {
        assert_eq!(sanitise("ws/1"), "ws_1");
        assert_eq!(sanitise("ws-1.0"), "ws-1.0");
        assert_eq!(sanitise("../etc"), ".._etc");
    }

    #[test]
    fn known_workspaces_lists_toml_stems() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GrantRegistry::open_at(tmp.path()).unwrap();
        store
            .add_grant(
                "ws-1",
                "a",
                Grant {
                    path: PathBuf::from("/tmp/a"),
                    mode: GrantMode::Read,
                },
            )
            .unwrap();
        store
            .add_grant(
                "ws-2",
                "b",
                Grant {
                    path: PathBuf::from("/tmp/b"),
                    mode: GrantMode::Write,
                },
            )
            .unwrap();
        let mut ws = store.known_workspaces().unwrap();
        ws.sort();
        assert_eq!(ws, vec!["ws-1", "ws-2"]);
    }

    #[test]
    fn grants_by_pkg_groups() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GrantRegistry::open_at(tmp.path()).unwrap();
        store
            .add_grant(
                "ws-1",
                "app.a",
                Grant {
                    path: PathBuf::from("/tmp/a"),
                    mode: GrantMode::Read,
                },
            )
            .unwrap();
        store
            .add_grant(
                "ws-1",
                "app.b",
                Grant {
                    path: PathBuf::from("/tmp/b"),
                    mode: GrantMode::Write,
                },
            )
            .unwrap();
        let file = store.load("ws-1").unwrap();
        let grouped = grants_by_pkg(&file);
        assert_eq!(grouped.len(), 2);
        assert!(grouped.contains_key("app.a"));
        assert!(grouped.contains_key("app.b"));
    }
}
