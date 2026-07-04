//! `PathCommandSource` — every executable file on `PATH` plus the
//! per-user `~/.local/share/qos-apps/bin` shim directory the qOS
//! installer writes CLI shims into.
//!
//! The palette surfaces these as `RunCommand` actions; Enter routes
//! the command into the active workspace's terminal.
//!
//! Refresh reads each directory and filters to files marked
//! executable by any of the user/group/other bits. Symlinks are
//! resolved via `metadata()`; broken symlinks are dropped. Duplicate
//! names (a binary earlier on `PATH` shadowing a later one) are
//! deduped, keeping the earliest — matching what the shell would
//! actually run.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;

use super::{SearchAction, SearchItem, SearchSource, SourceError, SourceKind};

pub struct PathCommandSource {
    /// Override for `$PATH` (colon-separated). Tests set this to a
    /// tempdir; production leaves it `None` so `std::env::var("PATH")`
    /// wins.
    override_path: Option<String>,
    /// Also-scanned per-user shim directory. Real wiring resolves
    /// this via [`dirs::data_dir`]; tests can pin it explicitly.
    extra_dirs: Vec<PathBuf>,
    items: Vec<SearchItem>,
    last_refresh: Option<Instant>,
}

impl PathCommandSource {
    pub fn new() -> Self {
        let mut extra_dirs = Vec::new();
        if let Some(base) = dirs::data_dir() {
            extra_dirs.push(base.join("qos-apps").join("bin"));
        }
        Self {
            override_path: None,
            extra_dirs,
            items: Vec::new(),
            last_refresh: None,
        }
    }

    pub fn with_path(mut self, path: String) -> Self {
        self.override_path = Some(path);
        self
    }

    pub fn with_extra_dirs(mut self, dirs: Vec<PathBuf>) -> Self {
        self.extra_dirs = dirs;
        self
    }
}

impl Default for PathCommandSource {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchSource for PathCommandSource {
    fn kind(&self) -> SourceKind {
        SourceKind::PathCommand
    }

    fn items(&self) -> &[SearchItem] {
        &self.items
    }

    fn refresh(&mut self) -> Result<(), SourceError> {
        let path_var = match &self.override_path {
            Some(p) => p.clone(),
            None => std::env::var("PATH").unwrap_or_default(),
        };
        let mut seen: HashSet<String> = HashSet::new();
        let mut items: Vec<SearchItem> = Vec::new();

        let dirs: Vec<PathBuf> = path_var
            .split(':')
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .chain(self.extra_dirs.iter().cloned())
            .collect();

        for dir in dirs {
            let Ok(read) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in read.flatten() {
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                if file_type.is_dir() {
                    continue;
                }
                // Symlinks: resolve via metadata() so we can check
                // the mode of the *target*. Broken symlinks fail
                // here and get skipped.
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                if !is_executable(&meta) {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                if !seen.insert(name.clone()) {
                    continue;
                }
                let path = entry.path();
                items.push(SearchItem {
                    id: name.clone(),
                    name: name.clone(),
                    description: format!("Run {} from {}", name, dir.display()),
                    icon: None,
                    source: SourceKind::PathCommand,
                    categories: vec!["Commands".into()],
                    action: SearchAction::RunCommand { cmd: path },
                });
            }
        }
        // Stable-ish ordering so tests aren't at the mercy of
        // `read_dir` order across filesystems.
        items.sort_by(|a, b| a.name.cmp(&b.name));
        self.items = items;
        self.last_refresh = Some(Instant::now());
        Ok(())
    }

    fn last_refreshed_at(&self) -> Option<Instant> {
        self.last_refresh
    }
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let mode = meta.permissions().mode();
    // Any of user/group/other execute bit set. Matches what the
    // shell would consider runnable.
    (mode & 0o111) != 0
}

#[cfg(not(unix))]
fn is_executable(_meta: &std::fs::Metadata) -> bool {
    // qOS is Linux-only, but keep a stub so shell builds on
    // non-Unix (e.g. developer's macOS `cargo check`).
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    #[cfg(unix)]
    fn make_exec(dir: &std::path::Path, name: &str) {
        let path = dir.join(name);
        fs::write(&path, "#!/bin/sh\ntrue\n").unwrap();
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn finds_executables_and_dedupes_earlier_first() {
        let a = TempDir::new().unwrap();
        let b = TempDir::new().unwrap();
        make_exec(a.path(), "foo");
        make_exec(a.path(), "bar");
        make_exec(b.path(), "foo");
        // Non-executable file that must NOT appear.
        fs::write(a.path().join("readme.txt"), "not exec").unwrap();

        let path = format!("{}:{}", a.path().display(), b.path().display());
        let mut src = PathCommandSource::new()
            .with_path(path)
            .with_extra_dirs(vec![]);
        src.refresh().unwrap();
        let names: Vec<&str> = src.items().iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"foo"));
        assert!(names.contains(&"bar"));
        assert!(!names.contains(&"readme.txt"));
        // De-dup: only one "foo" — from the earlier dir.
        let foo_hits: Vec<_> = src
            .items()
            .iter()
            .filter(|i| i.name == "foo")
            .collect();
        assert_eq!(foo_hits.len(), 1);
        let SearchAction::RunCommand { cmd } = &foo_hits[0].action else {
            panic!("wrong action");
        };
        assert!(cmd.starts_with(a.path()));
    }

    #[test]
    fn empty_path_yields_no_items() {
        let mut src = PathCommandSource::new()
            .with_path(String::new())
            .with_extra_dirs(vec![]);
        src.refresh().unwrap();
        assert!(src.items().is_empty());
    }
}
