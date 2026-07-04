//! `InstalledQosAppsSource` — reads `$XDG_DATA_HOME/qos-apps/registry.toml`
//! (the file `qos-install` writes on every successful install).
//!
//! The on-disk shape is the [`RegistryEntry`] list defined in
//! `crates/packaging/install/src/main.rs`. We deserialise a
//! narrow view of it — just the fields the palette actually
//! renders — so a future addition to the on-disk struct (e.g. a
//! new metadata field) doesn't break search.
//!
//! Refresh is a straight file read; the mtime check would be
//! premature optimisation.

use std::path::PathBuf;
use std::time::Instant;

use serde::Deserialize;

use super::{
    read_optional_toml, SearchAction, SearchItem, SearchSource, SourceError, SourceKind,
};

/// Envelope matching `Registry` in `qos-install`. Only the fields
/// the palette cares about are pulled off; the rest are ignored.
#[derive(Debug, Deserialize)]
struct RegistryFile {
    #[serde(default)]
    entry: Vec<RegistryEntry>,
}

#[derive(Debug, Deserialize)]
struct RegistryEntry {
    id: String,
    version: String,
    #[serde(default)]
    cli_commands: Vec<String>,
}

pub struct InstalledQosAppsSource {
    /// Resolved override — tests set this to a tempdir. Production
    /// leaves it `None` so the source resolves through XDG.
    override_path: Option<PathBuf>,
    items: Vec<SearchItem>,
    last_refresh: Option<Instant>,
}

impl InstalledQosAppsSource {
    pub fn new() -> Self {
        Self {
            override_path: None,
            items: Vec::new(),
            last_refresh: None,
        }
    }

    /// Test hook: read `registry.toml` from an explicit path
    /// instead of resolving via [`dirs::data_dir`].
    pub fn with_registry_path(mut self, path: PathBuf) -> Self {
        self.override_path = Some(path);
        self
    }

    fn resolved_path(&self) -> Option<PathBuf> {
        if let Some(p) = &self.override_path {
            return Some(p.clone());
        }
        // Honour `QOS_APPS_DATA_DIR` so a running qOS user with a
        // relocated install directory (e.g. dev installs pointing
        // at a scratch dir) still finds their apps.
        if let Some(dir) = std::env::var_os("QOS_APPS_DATA_DIR") {
            return Some(PathBuf::from(dir).join("registry.toml"));
        }
        dirs::data_dir().map(|d| d.join("qos-apps").join("registry.toml"))
    }
}

impl Default for InstalledQosAppsSource {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchSource for InstalledQosAppsSource {
    fn kind(&self) -> SourceKind {
        SourceKind::InstalledQosApp
    }

    fn items(&self) -> &[SearchItem] {
        &self.items
    }

    fn refresh(&mut self) -> Result<(), SourceError> {
        let Some(path) = self.resolved_path() else {
            // No XDG_DATA_HOME + no HOME (e.g. a CI runner without
            // env vars). Treat as "no installed apps" so the
            // palette still opens.
            self.items.clear();
            self.last_refresh = Some(Instant::now());
            return Ok(());
        };
        let Some(text) = read_optional_toml(&path)? else {
            self.items.clear();
            self.last_refresh = Some(Instant::now());
            return Ok(());
        };
        let file: RegistryFile = toml::from_str(&text)
            .map_err(|e| SourceError::Parse(format!("{}: {e}", path.display())))?;
        let mut items = Vec::with_capacity(file.entry.len());
        for entry in file.entry {
            // The on-disk record doesn't carry a display name or
            // description separately from the reverse-DNS id, so
            // derive a friendly name from the tail component of the
            // id (`dev.qos.notes` → `notes`) and use the id itself
            // as the description hint. Real UX would pull a name
            // from the `.qpkg` manifest at install time and mirror
            // it into the registry entry.
            let tail = entry
                .id
                .rsplit('.')
                .next()
                .unwrap_or(entry.id.as_str())
                .to_string();
            let name = titlecase(&tail);
            let description = if entry.cli_commands.is_empty() {
                format!("{} · v{}", entry.id, entry.version)
            } else {
                format!(
                    "{} · v{} · CLI: {}",
                    entry.id,
                    entry.version,
                    entry.cli_commands.join(", ")
                )
            };
            items.push(SearchItem {
                id: entry.id.clone(),
                name,
                description,
                icon: None,
                source: SourceKind::InstalledQosApp,
                categories: vec!["qOS".into()],
                action: SearchAction::LaunchInstalled {
                    pkg_id: entry.id,
                    kind: SourceKind::InstalledQosApp,
                },
            });
        }
        self.items = items;
        self.last_refresh = Some(Instant::now());
        Ok(())
    }

    fn last_refreshed_at(&self) -> Option<Instant> {
        self.last_refresh
    }
}

fn titlecase(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut boundary = true;
    for c in s.chars() {
        if c == '-' || c == '_' {
            out.push(' ');
            boundary = true;
            continue;
        }
        if boundary {
            out.extend(c.to_uppercase());
            boundary = false;
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn reads_installed_registry() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("registry.toml");
        fs::write(
            &path,
            r#"
[[entry]]
id = "dev.qos.notes"
version = "1.0.0"
package_id_hex = "aa"
payload_root = "/x"
manifest_path = "/x/manifest.toml"
publisher_key_hex = "bb"
pinned = false
installed_at = 1
cli_commands = ["notes"]

[[entry]]
id = "dev.qos.editor"
version = "2.0.0"
package_id_hex = "aa"
payload_root = "/x"
manifest_path = "/x/manifest.toml"
publisher_key_hex = "bb"
pinned = false
installed_at = 1
"#,
        )
        .unwrap();

        let mut src = InstalledQosAppsSource::new().with_registry_path(path);
        src.refresh().unwrap();
        assert_eq!(src.items().len(), 2);
        let names: Vec<&str> = src.items().iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"Notes"));
        assert!(names.contains(&"Editor"));
        assert!(src.last_refreshed_at().is_some());
    }

    #[test]
    fn missing_registry_is_empty_not_error() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("does-not-exist.toml");
        let mut src = InstalledQosAppsSource::new().with_registry_path(path);
        src.refresh().unwrap();
        assert!(src.items().is_empty());
    }
}
