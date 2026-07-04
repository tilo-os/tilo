//! `UninstalledQosAppsSource` — surfaces `.qpkg` apps advertised by
//! any of the user-configured registries in
//! `$XDG_CONFIG_HOME/qos-store/registries.toml`.
//!
//! The store and the palette share the *same* file, per the design
//! constraint that the palette must not fork the registry list.
//! Format:
//!
//! ```toml
//! [[registry]]
//! id       = "official"
//! name     = "qOS official"
//! base_url = "https://apps.qos.dev"
//! # Optional pinned owner pubkey; the palette treats this as opaque
//! # and just forwards it to `qos-install` when the user selects an
//! # `Add` action.
//! owner_pubkey_hex = "aa…"
//! ```
//!
//! Refresh performs `GET {base_url}/entries` per configured
//! registry, expecting a JSON array of [`RegistryEntry`] as
//! defined by `crates/packaging/registry-types::RegistryEntry`.
//!
//! Installed apps (per [`InstalledQosAppsSource`](
//!     super::installed_qos_apps::InstalledQosAppsSource)) are
//! *not* filtered out here — the palette's own de-dup pass masks
//! them by `pkg_id` so the two sources stay independent.

use std::path::PathBuf;
use std::time::Instant;

use serde::Deserialize;

use super::{
    read_optional_toml, SearchAction, SearchItem, SearchSource, SourceError, SourceKind,
};

/// A single configured registry the store and the palette both
/// use as a source.
#[derive(Debug, Clone, Deserialize)]
pub struct RegistryConfig {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub base_url: String,
    #[serde(default)]
    pub owner_pubkey_hex: Option<String>,
}

/// Envelope for the shared TOML — mirrors the `registries.toml`
/// shape the store loads.
#[derive(Debug, Deserialize, Default)]
struct RegistriesFile {
    #[serde(default)]
    registry: Vec<RegistryConfig>,
}

/// Type mirroring `registry-types::RegistryEntry`. We keep a local
/// deserialisation view instead of pulling the crate as a
/// dep because the shell already builds heavy and the fields we
/// need are stable.
#[derive(Debug, Deserialize, Clone)]
struct WireEntry {
    package_id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    categories: Vec<String>,
    #[serde(default)]
    versions: Vec<WireVersion>,
}

#[derive(Debug, Deserialize, Clone)]
struct WireVersion {
    #[serde(default)]
    qpkg_url: String,
    #[serde(default)]
    qpkg_root_hash_hex: String,
}

pub struct UninstalledQosAppsSource {
    override_config: Option<PathBuf>,
    /// Injectable fetch: `Fn(base_url) -> Result<Vec<WireEntry>>`.
    /// Production wiring builds this via `reqwest` blocking; tests
    /// swap in a canned list without touching the network.
    fetcher: FetchBox,
    items: Vec<SearchItem>,
    last_refresh: Option<Instant>,
}

type FetchBox = Box<
    dyn Fn(&str) -> Result<Vec<serde_json::Value>, SourceError> + Send + Sync,
>;

impl UninstalledQosAppsSource {
    /// Production constructor — the fetcher performs a blocking
    /// `GET {base_url}/entries` via `reqwest::blocking`. Tests use
    /// [`Self::with_fetcher`] to inject deterministic responses.
    pub fn new() -> Self {
        Self {
            override_config: None,
            fetcher: Box::new(fetch_via_reqwest_blocking),
            items: Vec::new(),
            last_refresh: None,
        }
    }

    pub fn with_config_path(mut self, path: PathBuf) -> Self {
        self.override_config = Some(path);
        self
    }

    pub fn with_fetcher<F>(mut self, fetcher: F) -> Self
    where
        F: Fn(&str) -> Result<Vec<serde_json::Value>, SourceError>
            + Send
            + Sync
            + 'static,
    {
        self.fetcher = Box::new(fetcher);
        self
    }

    fn resolve_config_path(&self) -> Option<PathBuf> {
        if let Some(p) = &self.override_config {
            return Some(p.clone());
        }
        dirs::config_dir()
            .map(|d| d.join("qos-store").join("registries.toml"))
    }

    fn load_registries(&self) -> Result<Vec<RegistryConfig>, SourceError> {
        let Some(path) = self.resolve_config_path() else {
            return Ok(Vec::new());
        };
        let Some(text) = read_optional_toml(&path)? else {
            return Ok(Vec::new());
        };
        let file: RegistriesFile = toml::from_str(&text)
            .map_err(|e| SourceError::Parse(format!("{}: {e}", path.display())))?;
        Ok(file.registry)
    }
}

impl Default for UninstalledQosAppsSource {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchSource for UninstalledQosAppsSource {
    fn kind(&self) -> SourceKind {
        SourceKind::UninstalledQosApp
    }

    fn items(&self) -> &[SearchItem] {
        &self.items
    }

    fn refresh(&mut self) -> Result<(), SourceError> {
        let registries = self.load_registries()?;
        let mut items = Vec::new();
        let mut last_err: Option<SourceError> = None;
        for reg in registries {
            match (self.fetcher)(&reg.base_url) {
                Ok(values) => {
                    for v in values {
                        // WireEntry is a narrow view; ignore any
                        // per-entry deserialise failure so a
                        // single malformed row doesn't sink the
                        // whole fetch.
                        let Ok(entry) = serde_json::from_value::<WireEntry>(v) else {
                            continue;
                        };
                        let Some(version) = entry.versions.into_iter().next() else {
                            continue;
                        };
                        let display_name = if entry.name.is_empty() {
                            entry.package_id.clone()
                        } else {
                            entry.name.clone()
                        };
                        items.push(SearchItem {
                            id: entry.package_id.clone(),
                            name: display_name,
                            description: entry.description,
                            icon: None,
                            source: SourceKind::UninstalledQosApp,
                            categories: {
                                let mut c = entry.categories;
                                c.push("qOS".into());
                                c
                            },
                            action: SearchAction::AddQosApp {
                                pkg_id: entry.package_id,
                                registry_id: reg.id.clone(),
                                qpkg_url: version.qpkg_url,
                                root_hash_hex: version.qpkg_root_hash_hex,
                            },
                        });
                    }
                }
                Err(e) => {
                    // Record the last error so we can surface it if
                    // every registry fails; a single reachable
                    // registry still yields useful items.
                    last_err = Some(e);
                }
            }
        }
        if items.is_empty() {
            if let Some(e) = last_err {
                return Err(e);
            }
        }
        self.items = items;
        self.last_refresh = Some(Instant::now());
        Ok(())
    }

    fn last_refreshed_at(&self) -> Option<Instant> {
        self.last_refresh
    }
}

/// The production fetcher. Uses `reqwest`'s blocking client because
/// the palette-open path already runs off a Tokio blocking task —
/// we don't need another executor.
fn fetch_via_reqwest_blocking(
    base_url: &str,
) -> Result<Vec<serde_json::Value>, SourceError> {
    // Bounded timeout — the palette must not hang the shell when a
    // configured registry is unreachable.
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .map_err(|e| SourceError::Fetch(e.to_string()))?;
    let url = format!("{}/entries", base_url.trim_end_matches('/'));
    let resp = client
        .get(&url)
        .send()
        .map_err(|e| SourceError::Fetch(format!("{url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(SourceError::Fetch(format!(
            "{url}: HTTP {}",
            resp.status()
        )));
    }
    let arr: Vec<serde_json::Value> = resp
        .json()
        .map_err(|e| SourceError::Parse(format!("{url}: {e}")))?;
    Ok(arr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn empty_config_yields_no_items() {
        let src = UninstalledQosAppsSource::new().with_fetcher(|_| Ok(vec![]));
        let mut src = src.with_config_path(PathBuf::from(
            "/tmp/does-not-exist/registries.toml",
        ));
        src.refresh().unwrap();
        assert!(src.items().is_empty());
    }

    #[test]
    fn fetched_entries_become_items() {
        let dir = TempDir::new().unwrap();
        let cfg = dir.path().join("registries.toml");
        fs::write(
            &cfg,
            r#"
[[registry]]
id = "official"
name = "qOS official"
base_url = "http://127.0.0.1:0"
"#,
        )
        .unwrap();

        let src = UninstalledQosAppsSource::new()
            .with_config_path(cfg)
            .with_fetcher(|url| {
                assert_eq!(url, "http://127.0.0.1:0");
                Ok(vec![
                    serde_json::json!({
                        "package_id": "dev.qos.example",
                        "name": "Example",
                        "description": "A demo app",
                        "categories": ["Demo"],
                        "versions": [{
                            "version": "1.0",
                            "qpkg_url": "https://apps.qos.dev/example.qpkg",
                            "qpkg_root_hash_hex": "aa",
                            "qpkg_size_bytes": 0,
                            "released_at_unix": 0,
                        }],
                        "updated_at_unix": 0,
                        "publisher_pubkey_hex": "bb",
                        "signature_hex": "cc",
                    }),
                ])
            });
        let mut src = src;
        src.refresh().unwrap();
        assert_eq!(src.items().len(), 1);
        let item = &src.items()[0];
        assert_eq!(item.name, "Example");
        assert!(matches!(
            &item.action,
            SearchAction::AddQosApp { registry_id, .. } if registry_id == "official"
        ));
    }
}
