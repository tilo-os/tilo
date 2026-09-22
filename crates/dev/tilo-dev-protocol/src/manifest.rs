//! Reader for `os/components.toml`, the component manifest.
//!
//! The manifest belongs to the image build, and `os/scripts/install-components.py`
//! owns every path decision in it; dev tooling reads only what it needs to
//! build a component and to decide what a swap restarts:
//!
//! ```toml
//! [[component]]
//! name = "fs"
//! kind = "userspace"          # or "boot", with `scenario` naming the os/dev driver
//! scope = "vm"                # "session" | "vm"; anything else reads as session
//! profile = "release"         # cargo profile, default release
//! packages = ["fs"]
//! features = ["fs/foo"]       # package-qualified
//! units = ["fs.service"]      # system units a vm-scope swap restarts
//! user_units = ["tilo-fs-recipient.service"]
//! ```
//!
//! Everything else is ignored, so the schema can grow without breaking dev
//! tools, and malformed entries are skipped rather than fatal.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use toml::Value;

/// What a swap of this component restarts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Compositor, user services, CLIs a user runs: restart the slot's session.
    Session,
    /// System services, greeter: merge into the VM's `/usr`.
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
    /// `kind = "boot"`: only a cold boot exercises it, so it is never swapped.
    pub boot: bool,
    /// For boot components: the os/dev driver that cold-boots them.
    pub scenario: Option<String>,
    pub info: ComponentInfo,
    pub user_units: Vec<String>,
    pub packages: Vec<String>,
    /// Package-qualified cargo features (`pkg/feature`).
    pub features: Vec<String>,
    pub profile: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Manifest {
    pub components: BTreeMap<String, Component>,
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

    /// Parse manifest text: `[[component]]` entries (a `[components.<name>]`
    /// table is accepted too).
    ///
    /// # Errors
    /// Invalid TOML. Entries lacking a name are skipped, not rejected.
    pub fn parse(text: &str) -> Result<Self, String> {
        let root: toml::Table = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
        let mut components = BTreeMap::new();
        for key in ["component", "components"] {
            match root.get(key) {
                Some(Value::Array(entries)) => {
                    for entry in entries {
                        if let Value::Table(entry) = entry
                            && let Some(Value::String(name)) = entry.get("name")
                        {
                            components.insert(name.clone(), component(name, entry));
                        }
                    }
                }
                Some(Value::Table(table)) => {
                    for (name, entry) in table {
                        if let Value::Table(entry) = entry {
                            components.insert(name.clone(), component(name, entry));
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(Self { components })
    }

    pub fn get(&self, name: &str) -> Option<&Component> {
        self.components.get(name)
    }
}

fn component(name: &str, entry: &toml::Table) -> Component {
    let text = |key: &str| entry.get(key).and_then(Value::as_str).map(str::to_owned);
    Component {
        name: name.to_owned(),
        boot: text("kind").as_deref() == Some("boot"),
        scenario: text("scenario"),
        info: ComponentInfo {
            scope: text("scope")
                .as_deref()
                .map_or(Scope::Session, Scope::parse),
            units: strings(entry, "units"),
        },
        user_units: strings(entry, "user_units"),
        packages: strings(entry, "packages"),
        features: strings(entry, "features"),
        profile: text("profile").unwrap_or_else(|| "release".into()),
    }
}

/// A string or array of strings; non-strings are dropped.
fn strings(entry: &toml::Table, key: &str) -> Vec<String> {
    match entry.get(key) {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn component_array_form() {
        let m = Manifest::parse(
            r#"
            external_units = ["greetd.service"]
            [[component]]
            name = "fs"
            kind = "userspace"
            scope = "vm"
            packages = ["fs"]
            units = ["fs.service", "tilo-fs-replica.service"]
            user_units = ["tilo-fs-recipient.service"]
            files = [{ from = "fs", to = "/usr/bin/fs", mode = "0755" }]
            [[component]]
            name = "browser"
            scope = "session"
            packages = ["browser"]
            features = ["browser/wpe"]
            [[component]]
            name = "apps+runtime"
            scope = "session"
            profile = "thin-app"
            [[component]]
            name = "kernel"
            kind = "boot"
            scope = "vm"
            scenario = "os/dev/vm.sh"
            [[component]]
            scope = "vm"
            "#,
        )
        .unwrap();
        assert_eq!(m.components.len(), 4);
        let fs = m.get("fs").unwrap();
        assert_eq!(fs.info.scope, Scope::Vm);
        assert_eq!(fs.info.units, ["fs.service", "tilo-fs-replica.service"]);
        assert_eq!(fs.user_units, ["tilo-fs-recipient.service"]);
        assert!(!fs.boot);
        assert_eq!(fs.profile, "release");
        assert_eq!(m.get("browser").unwrap().features, ["browser/wpe"]);
        assert_eq!(m.get("apps+runtime").unwrap().profile, "thin-app");
        let kernel = m.get("kernel").unwrap();
        assert!(kernel.boot);
        assert_eq!(kernel.scenario.as_deref(), Some("os/dev/vm.sh"));
    }

    #[test]
    fn unknown_scope_is_session() {
        let m =
            Manifest::parse("[[component]]\nname = \"x\"\nscope = \"something-new\"\n").unwrap();
        assert_eq!(m.get("x").unwrap().info.scope, Scope::Session);
    }

    #[test]
    fn the_repository_manifest_parses() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../os/components.toml");
        let m = Manifest::load(&path)
            .unwrap()
            .expect("os/components.toml exists");
        assert_eq!(m.get("shell").unwrap().info.scope, Scope::Session);
        assert_eq!(m.get("fs").unwrap().info.scope, Scope::Vm);
        assert!(m.get("kernel").unwrap().boot);
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
