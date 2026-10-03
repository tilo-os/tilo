//! Registries configuration — which registries a host resolves
//! against, in what order, and how much each is trusted.
//!
//! Two layers, merged at load time: **global**
//! (`~/.config/tilo/registries.toml` / `$XDG_CONFIG_HOME`) and
//! **per-workspace** (`<workspace>/.tilo/registries.toml`), which can
//! add registries or override a global one's trust — workspace
//! entries win on a name collision and sort before global entries of
//! equal priority.
//!
//! ```toml
//! [[registry]]
//! name = "central"
//! base-url = "https://apps.tilo.so"
//! trust = "ed25519:2f1c…"     # pin the operator key (fail-closed)
//! priority = 0                # lower sorts first, default 0
//! ```
//!
//! `base-url` is `https://…` (TLS) or `file://…` (self-hosted — see
//! [`crate::registry_dir`]). `trust` is `ed25519:<hex>` to require the
//! served index to verify against that key, or `unsigned` to skip the
//! *index* signature — per-entry publisher signatures are ALWAYS
//! verified regardless of trust level; the resolver never installs
//! unverified bytes.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// How much a registry is trusted at the *index* level.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegistryTrust {
    /// Require the served index to carry a valid signature from this Ed25519 operator key.
    Key([u8; 32]),
    /// Do not require an index signature — only sensible for a
    /// self-hosted `file://` registry; publisher signatures on each entry are still verified.
    Unsigned,
}

impl RegistryTrust {
    /// Parse `ed25519:<hex>` or `unsigned`.
    ///
    /// # Errors
    ///
    /// Returns an error if `s` is neither `unsigned` nor `ed25519:<hex32>`.
    pub fn parse(s: &str) -> Result<Self, ConfigError> {
        if s == "unsigned" {
            return Ok(RegistryTrust::Unsigned);
        }
        let rest = s
            .strip_prefix("ed25519:")
            .ok_or_else(|| ConfigError::BadTrust(s.to_string()))?;
        let bytes = hex::decode(rest).map_err(|e| ConfigError::Hex(e.to_string()))?;
        let arr: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| ConfigError::BadTrust(s.to_string()))?;
        Ok(RegistryTrust::Key(arr))
    }

    fn to_wire(&self) -> String {
        match self {
            RegistryTrust::Unsigned => "unsigned".to_string(),
            RegistryTrust::Key(k) => format!("ed25519:{}", hex::encode(k)),
        }
    }
}

/// One configured registry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistryRef {
    pub name: String,
    pub base_url: String,
    pub trust: RegistryTrust,
    /// Consulted in ascending order; ties broken by scope (workspace before global).
    pub priority: i32,
}

impl RegistryRef {
    /// Whether `base_url` names a local self-hosted registry directory (`file://`).
    pub fn is_file(&self) -> bool {
        self.base_url.starts_with("file://")
    }

    /// The filesystem path for a `file://` registry, else `None`.
    pub fn file_path(&self) -> Option<PathBuf> {
        self.base_url.strip_prefix("file://").map(PathBuf::from)
    }
}

// TOML (de)serialisation shims

#[derive(Serialize, Deserialize)]
struct RegistryFile {
    #[serde(default, rename = "registry")]
    registries: Vec<RegistryRow>,
}

#[derive(Serialize, Deserialize)]
struct RegistryRow {
    name: String,
    #[serde(rename = "base-url")]
    base_url: String,
    trust: String,
    #[serde(default)]
    priority: i32,
}

impl RegistryRow {
    fn into_ref(self) -> Result<RegistryRef, ConfigError> {
        Ok(RegistryRef {
            name: self.name,
            base_url: self.base_url,
            trust: RegistryTrust::parse(&self.trust)?,
            priority: self.priority,
        })
    }

    fn from_ref(r: &RegistryRef) -> Self {
        Self {
            name: r.name.clone(),
            base_url: r.base_url.clone(),
            trust: r.trust.to_wire(),
            priority: r.priority,
        }
    }
}

/// The merged, resolution-ordered set of registries.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RegistriesConfig {
    pub registries: Vec<RegistryRef>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("I/O reading {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("TOML in {path}: {source}")]
    Toml {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("TOML encode: {0}")]
    Encode(String),
    #[error("bad trust {0:?}: expected `ed25519:<hex>` or `unsigned`")]
    BadTrust(String),
    #[error("hex: {0}")]
    Hex(String),
}

impl RegistriesConfig {
    /// The built-in default: the central Tilo registry. Shipped so a
    /// fresh install resolves *something* without config, but the
    /// operator key is a placeholder — a real deployment pins the true key.
    pub fn default_central() -> RegistryRef {
        RegistryRef {
            name: "central".to_string(),
            base_url: "https://apps.tilo.so".to_string(),
            // Placeholder key (all-zero is not a valid ed25519 point,
            // so an unconfigured central registry fails closed until
            // the operator pins the real key).
            trust: RegistryTrust::Key([0u8; 32]),
            priority: 0,
        }
    }

    /// Load + merge the global and per-workspace configs; missing files
    /// aren't an error, and if neither exists returns the built-in [`Self::default_central`].
    ///
    /// # Errors
    ///
    /// Returns an error if a present config file fails to read or parse.
    pub fn load(workspace_root: Option<&Path>) -> Result<Self, ConfigError> {
        Self::load_from(global_path().as_deref(), workspace_root)
    }

    /// Merge an explicit global-config path with a workspace root.
    /// [`Self::load`] calls this with the real global path; tests pass a
    /// temp path so they never read the host's ~/.config/tilo.
    ///
    /// # Errors
    ///
    /// Returns an error if a present config file fails to read or parse.
    pub fn load_from(
        global: Option<&Path>,
        workspace_root: Option<&Path>,
    ) -> Result<Self, ConfigError> {
        let mut merged: Vec<RegistryRef> = Vec::new();

        if let Some(p) = global {
            for r in load_file(p)? {
                merged.push(r);
            }
        }
        // Workspace entries are pushed AFTER global ones; the merge below
        // lets a workspace override a global entry by name and marks
        // workspace scope so it wins on equal priority.
        let mut ws_names = Vec::new();
        if let Some(root) = workspace_root {
            let p = workspace_path(root);
            for r in load_file(&p)? {
                ws_names.push(r.name.clone());
                // Override global-by-name.
                merged.retain(|g| g.name != r.name);
                merged.push(r);
            }
        }

        if merged.is_empty() {
            merged.push(Self::default_central());
        }

        // Deterministic resolution order: priority asc, then workspace-scope before global, then name.
        merged.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| {
                    let aw = ws_names.contains(&a.name);
                    let bw = ws_names.contains(&b.name);
                    bw.cmp(&aw) // workspace (true) sorts first
                })
                .then_with(|| a.name.cmp(&b.name))
        });

        Ok(Self { registries: merged })
    }

    /// Build directly from an explicit list (tests, embedded use).
    pub fn from_registries(registries: Vec<RegistryRef>) -> Self {
        Self { registries }
    }

    /// Serialise to the `registries.toml` schema.
    ///
    /// # Errors
    ///
    /// Returns an error if TOML encoding fails.
    pub fn to_toml(&self) -> Result<String, ConfigError> {
        let file = RegistryFile {
            registries: self.registries.iter().map(RegistryRow::from_ref).collect(),
        };
        toml::to_string_pretty(&file).map_err(|e| ConfigError::Encode(e.to_string()))
    }
}

fn load_file(path: &Path) -> Result<Vec<RegistryRef>, ConfigError> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(ConfigError::Io {
                path: path.display().to_string(),
                source: e,
            });
        }
    };
    let file: RegistryFile = toml::from_str(&text).map_err(|e| ConfigError::Toml {
        path: path.display().to_string(),
        source: e,
    })?;
    file.registries
        .into_iter()
        .map(RegistryRow::into_ref)
        .collect()
}

/// `~/.config/tilo/registries.toml` (honours `$XDG_CONFIG_HOME`).
pub fn global_path() -> Option<PathBuf> {
    xdg_paths::config_dir().map(|d| d.join("tilo").join("registries.toml"))
}

/// `<workspace>/.tilo/registries.toml`.
pub fn workspace_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".tilo").join("registries.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_sources_round_trip_merge_workspace_overrides_and_default_to_central() {
        // Phase `trust_roundtrip`.
        {
            let k = RegistryTrust::Key([3u8; 32]);
            assert_eq!(RegistryTrust::parse(&k.to_wire()).unwrap(), k);
            assert_eq!(
                RegistryTrust::parse("unsigned").unwrap(),
                RegistryTrust::Unsigned
            );
            assert!(RegistryTrust::parse("nonsense").is_err());
        }
        // Phase `toml_roundtrip`.
        {
            let cfg = RegistriesConfig::from_registries(vec![
                RegistryRef {
                    name: "central".into(),
                    base_url: "https://apps.tilo.so".into(),
                    trust: RegistryTrust::Key([9u8; 32]),
                    priority: 0,
                },
                RegistryRef {
                    name: "team".into(),
                    base_url: "file:///srv/reg".into(),
                    trust: RegistryTrust::Unsigned,
                    priority: 10,
                },
            ]);
            let text = cfg.to_toml().unwrap();
            let file: RegistryFile = toml::from_str(&text).unwrap();
            let back: Vec<RegistryRef> = file
                .registries
                .into_iter()
                .map(RegistryRow::into_ref)
                .map(Result::unwrap)
                .collect();
            assert_eq!(back, cfg.registries);
        }
        // Phase `workspace_overrides_global_by_name_and_sorts_first`.
        {
            let dir = tempfile::tempdir().unwrap();
            let global = dir.path().join("global.toml");
            std::fs::write(
                &global,
                r#"
    [[registry]]
    name = "central"
    base-url = "https://apps.tilo.so"
    trust = "ed25519:0303030303030303030303030303030303030303030303030303030303030303"
    priority = 0
    "#,
            )
            .unwrap();

            let ws_root = dir.path().join("ws");
            std::fs::create_dir_all(ws_root.join(".tilo")).unwrap();
            std::fs::write(
                workspace_path(&ws_root),
                r#"
    [[registry]]
    name = "central"
    base-url = "file:///ws/central"
    trust = "unsigned"
    priority = 0

    [[registry]]
    name = "team"
    base-url = "file:///ws/team"
    trust = "unsigned"
    priority = 5
    "#,
            )
            .unwrap();

            let cfg = RegistriesConfig::load_from(Some(&global), Some(&ws_root)).unwrap();
            // The workspace "central" (file://) must have replaced the global central by name.
            let central = cfg.registries.iter().find(|r| r.name == "central").unwrap();
            assert_eq!(central.base_url, "file:///ws/central");
            // priority 0 comes before priority 5.
            assert_eq!(cfg.registries.first().unwrap().name, "central");
            assert_eq!(cfg.registries.len(), 2);
        }
        // Phase `missing_files_yield_default_central`.
        {
            let dir = tempfile::tempdir().unwrap();
            let cfg =
                RegistriesConfig::load_from(Some(&dir.path().join("nope.toml")), Some(dir.path()))
                    .unwrap();
            assert_eq!(cfg.registries.len(), 1);
            assert_eq!(cfg.registries[0].name, "central");
        }
    }
}
