//! Reader for `os/components.toml`, the component manifest.
//!
//! The manifest's full schema belongs to the image build; dev tooling needs
//! only a few fields, so this reader is deliberately tolerant and isolated.
//! Accepted shapes:
//!
//! ```toml
//! [components.shell]          # or [[component]] / [[components]] with name = "shell"
//! scope = "session"           # "session" | "vm" | "app"; anything else reads as session
//! units = ["fs.service"]      # system units a vm-scope swap restarts
//! packages = ["shell"]        # or package = "shell"
//! bins = ["shell"]            # or bin = "shell"
//! features = []               # cargo features for this component's dev build
//! dev-features = ["dev-overrides"]
//!
//! [dev]
//! installer = "tools/install-component"   # called with --component NAME --dest DIR
//! ```
//!
//! Unknown keys are ignored so the schema can grow without breaking dev tools.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use toml::Value;

/// What a swap of this component restarts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Shell, launcher, user services, runtime: restart the slot's session.
    Session,
    /// `fs`, local model, greeter: merge into the VM's `/usr`.
    Vm,
    /// Apps alone: live at next launch, no restart.
    App,
}

impl Scope {
    fn parse(text: &str) -> Self {
        match text {
            "vm" => Self::Vm,
            "app" | "apps" => Self::App,
            // Restarting the session is the safe default for anything unknown.
            _ => Self::Session,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Vm => "vm",
            Self::App => "app",
        }
    }
}

/// The part of a component the daemon needs to apply a swap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentInfo {
    pub scope: Scope,
    #[serde(default)]
    pub units: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Component {
    pub name: String,
    pub info: ComponentInfo,
    pub packages: Vec<String>,
    pub bins: Vec<String>,
    pub features: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Manifest {
    pub components: BTreeMap<String, Component>,
    /// Repository-relative installer that lays out a component under `--dest`.
    pub installer: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("{path} is not valid TOML: {detail}")]
    Parse { path: String, detail: String },
}

impl Manifest {
    /// Load the manifest, or `Ok(None)` when the file does not exist.
    ///
    /// # Errors
    /// Unreadable or syntactically invalid files.
    pub fn load(path: &Path) -> Result<Option<Self>, ManifestError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text)
                .map(Some)
                .map_err(|detail| ManifestError::Parse {
                    path: path.display().to_string(),
                    detail,
                }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(ManifestError::Read {
                path: path.display().to_string(),
                source,
            }),
        }
    }

    /// Parse manifest text.
    ///
    /// # Errors
    /// Invalid TOML. Entries lacking a name are skipped, not rejected.
    pub fn parse(text: &str) -> Result<Self, String> {
        let root: toml::Table = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
        let mut components = BTreeMap::new();
        for key in ["components", "component"] {
            match root.get(key) {
                Some(Value::Table(table)) => {
                    for (name, entry) in table {
                        if let Value::Table(entry) = entry {
                            components.insert(name.clone(), component(name, entry));
                        }
                    }
                }
                Some(Value::Array(entries)) => {
                    for entry in entries {
                        if let Value::Table(entry) = entry
                            && let Some(Value::String(name)) = entry.get("name")
                        {
                            components.insert(name.clone(), component(name, entry));
                        }
                    }
                }
                _ => {}
            }
        }
        let installer = root
            .get("dev")
            .and_then(|dev| dev.get("installer"))
            .or_else(|| root.get("installer"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        Ok(Self {
            components,
            installer,
        })
    }

    pub fn get(&self, name: &str) -> Option<&Component> {
        self.components.get(name)
    }
}

fn component(name: &str, entry: &toml::Table) -> Component {
    let scope = entry
        .get("scope")
        .and_then(Value::as_str)
        .map_or(Scope::Session, Scope::parse);
    let mut features = strings(entry, &["features"]);
    for feature in strings(entry, &["dev-features", "dev_features"]) {
        if !features.contains(&feature) {
            features.push(feature);
        }
    }
    Component {
        name: name.to_owned(),
        info: ComponentInfo {
            scope,
            units: strings(entry, &["units", "unit"]),
        },
        packages: strings(entry, &["packages", "package"]),
        bins: strings(entry, &["bins", "bin"]),
        features,
    }
}

/// A string or array of strings under any of `keys`; non-strings are dropped.
fn strings(entry: &toml::Table, keys: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for key in keys {
        match entry.get(*key) {
            Some(Value::String(s)) => out.push(s.clone()),
            Some(Value::Array(items)) => {
                out.extend(items.iter().filter_map(Value::as_str).map(str::to_owned));
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_form() {
        let m = Manifest::parse(
            r#"
            [dev]
            installer = "tools/install-component"
            [components.fs]
            scope = "vm"
            units = ["fs.service"]
            package = "fs"
            bins = ["fs"]
            [components.shell]
            scope = "session"
            packages = ["shell"]
            features = ["a"]
            dev-features = ["dev-overrides", "a"]
            extra = { anything = 1 }
            "#,
        )
        .unwrap();
        let fs = m.get("fs").unwrap();
        assert_eq!(fs.info.scope, Scope::Vm);
        assert_eq!(fs.info.units, ["fs.service"]);
        assert_eq!(fs.packages, ["fs"]);
        assert_eq!(m.get("shell").unwrap().features, ["a", "dev-overrides"]);
        assert_eq!(m.installer.as_deref(), Some("tools/install-component"));
    }

    #[test]
    fn array_form_and_unknown_scope() {
        let m = Manifest::parse(
            r#"
            [[component]]
            name = "apps+runtime"
            scope = "app"
            [[component]]
            name = "greeter"
            scope = "something-new"
            [[component]]
            scope = "vm"
            "#,
        )
        .unwrap();
        assert_eq!(m.components.len(), 2);
        assert_eq!(m.get("apps+runtime").unwrap().info.scope, Scope::App);
        assert_eq!(m.get("greeter").unwrap().info.scope, Scope::Session);
    }

    #[test]
    fn missing_file_is_none() {
        assert!(
            Manifest::load(Path::new("/nonexistent/components.toml"))
                .unwrap()
                .is_none()
        );
    }
}
