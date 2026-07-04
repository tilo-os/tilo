//! In-tree minimal manifest shim.
//!
//! The canonical manifest crate (`qos-pkg`) is being built in parallel.
//! This module defines just enough of the shape `qos-launcher` needs
//! today so we can iterate without blocking on the other agent. When
//! `qos-pkg` lands, replace [`MinimalManifest`] with the published type
//! (and pin it as a path dep here).
//!
//! ### Assumed manifest shape (TOML)
//!
//! ```toml
//! package_id    = "com.example.notes"
//! version       = "0.1.0"
//!
//! [entrypoint]
//! bin           = "bin/notes"
//!
//! [sandbox.net]
//! mode          = "off"           # "off" | "on" | "isolated"
//!
//! [sandbox.fs]
//! read          = ["~/Documents", "/usr/share/fonts"]
//! write         = ["<scope=current>/scratch"]
//!
//! [sandbox.services]
//! collab        = "required@1"     # or "optional@1"
//! fs            = "optional@1"
//! ```
//!
//! Path placeholders supported in `sandbox.fs.read` / `.write`:
//! - `~/` — expands to `$HOME`.
//! - `<scope=current>` — expands to the current process working dir.
//!   (When the per-cluster scope is finalised by the compositor, this
//!   placeholder will be resolved against the cluster root instead;
//!   the launcher's CWD is a reasonable interim stand-in for now.)

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Minimal manifest needed by the launcher. Lives in-tree pending
/// full `qos-pkg` integration.
///
/// Accepts both the top-level `package_id = "..."` form (this crate's
/// original shim) and the qos-pkg canonical form (`[package] id = "..."`
/// nested table). Whichever shows up is normalised into
/// `MinimalManifest::package_id`. Same story for `version`.
#[derive(Debug, Clone, Deserialize)]
#[serde(from = "RawManifest")]
pub struct MinimalManifest {
    pub package_id: String,
    pub version: String,
    /// Optional `[package].runtime` reference from the manifest, of
    /// the form `qos-runtime/<version>`. When present, the launcher
    /// looks up the corresponding bundle and mounts it (see
    /// [`crate::run::RunOptions::runtime_root`]) — apps declaring
    /// a runtime don't need `--minimal-libs`.
    pub runtime: Option<String>,
    pub entrypoint: Entrypoint,
    pub sandbox: Sandbox,
    /// `[cli].commands` — PATH-visible command names the package
    /// declares. Empty when the package doesn't ship a CLI.
    pub cli_commands: Vec<String>,
}

impl MinimalManifest {
    /// Split `runtime = "qos-runtime/<v>"` into `(name, version)`.
    /// Returns `None` if `runtime` is unset or doesn't have the
    /// `<name>/<version>` shape.
    pub fn runtime_ref(&self) -> Option<(&str, &str)> {
        let raw = self.runtime.as_deref()?;
        raw.split_once('/')
    }
}

/// Wire-level manifest — accepts both shapes.
#[derive(Debug, Clone, Deserialize)]
struct RawManifest {
    #[serde(default)]
    package_id: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    package: Option<PackageBlock>,
    entrypoint: Entrypoint,
    #[serde(default)]
    sandbox: Sandbox,
    #[serde(default)]
    cli: Option<CliBlock>,
}

/// `[cli]` block — mirrors [`pkg::manifest::Cli`] but decoded
/// inside this crate to keep the launcher independent of the
/// full pkg crate.
#[derive(Debug, Clone, Default, Deserialize)]
struct CliBlock {
    #[serde(default)]
    commands: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct PackageBlock {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    runtime: Option<String>,
}

impl From<RawManifest> for MinimalManifest {
    fn from(raw: RawManifest) -> Self {
        let package_id = raw
            .package_id
            .or_else(|| raw.package.as_ref().and_then(|p| p.id.clone()))
            .unwrap_or_default();
        let version = raw
            .version
            .or_else(|| raw.package.as_ref().and_then(|p| p.version.clone()))
            .unwrap_or_default();
        let cli_commands = raw.cli.map(|c| c.commands).unwrap_or_default();
        let runtime = raw.package.as_ref().and_then(|p| p.runtime.clone());
        MinimalManifest {
            package_id,
            version,
            runtime,
            entrypoint: raw.entrypoint,
            sandbox: raw.sandbox,
            cli_commands,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Entrypoint {
    /// Path of the entrypoint binary, relative to the payload root.
    /// Joined with the in-sandbox payload mount to form the absolute
    /// path used for `exec`. Used verbatim when `r#type = "gui"`
    /// or `"cli"`; falls back for `"both"` when neither split
    /// path is set.
    #[serde(default)]
    pub bin: String,
    /// Which flavour of entrypoint this is. Mirrors
    /// [`pkg::manifest::EntrypointType`]; kept here so the launcher
    /// stays independent of the full pkg crate.
    #[serde(default, rename = "type")]
    pub r#type: EntrypointType,
    /// GUI-flavour binary path inside the payload. Only meaningful
    /// when `r#type = "both"`.
    #[serde(default, rename = "gui-bin")]
    pub gui_bin: Option<String>,
    /// CLI-flavour binary path inside the payload. Only meaningful
    /// when `r#type = "both"`.
    #[serde(default, rename = "cli-bin")]
    pub cli_bin: Option<String>,
}

/// Which flavour of entrypoint the package exposes. Wire form is
/// lowercase — `type = "gui"` / `"cli"` / `"both"`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntrypointType {
    #[default]
    Gui,
    Cli,
    Both,
}

impl Entrypoint {
    /// Binary the *GUI* launcher path should exec. `None` for a
    /// pure-CLI package (the launcher's `run` subcommand refuses
    /// to launch it directly).
    pub fn gui_bin(&self) -> Option<&str> {
        match self.r#type {
            EntrypointType::Gui => Some(self.bin.as_str()),
            EntrypointType::Cli => None,
            EntrypointType::Both => self
                .gui_bin
                .as_deref()
                .or_else(|| (!self.bin.is_empty()).then_some(self.bin.as_str())),
        }
    }

    /// Binary a CLI command should exec. `None` for a pure-GUI
    /// package.
    pub fn cli_bin(&self) -> Option<&str> {
        match self.r#type {
            EntrypointType::Gui => None,
            EntrypointType::Cli => Some(self.bin.as_str()),
            EntrypointType::Both => self
                .cli_bin
                .as_deref()
                .or_else(|| (!self.bin.is_empty()).then_some(self.bin.as_str())),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Sandbox {
    #[serde(default)]
    pub net: Net,
    #[serde(default)]
    pub fs: Fs,
    /// Map of service-name -> requirement string ("required@1",
    /// "optional@2", …). Parsed in [`ServiceReq::parse`].
    #[serde(default)]
    pub services: BTreeMap<String, String>,
    /// XDG portal policy per portal name — `file-chooser`,
    /// `screencast`, `notifications`, `secret-service`, `open-uri`.
    /// When at least one portal is `ViaHost` the launcher bind-mounts
    /// the host session D-Bus socket into the sandbox and sets
    /// `DBUS_SESSION_BUS_ADDRESS`. Absent / empty == the bus is NOT
    /// exposed (so apps that didn't declare a portal can't reach it).
    #[serde(default)]
    pub portals: BTreeMap<String, PortalPolicy>,
    /// Per-workspace portal grants. When set to `"auto"`, the
    /// launcher inspects the compositor's grant store at
    /// `$XDG_STATE_HOME/qos-shell/grants/<QOS_WORKSPACE_ID>.toml`
    /// and adds every `(pkg_id, path)` row it finds for this
    /// package to the sandbox's read/write bind list. Absent /
    /// any other value == the grant store is ignored (equivalent
    /// to the pre-integration behaviour). This is how the shell's
    /// portal prompt persists across app launches: the user
    /// approves once, and subsequent launches in the same
    /// workspace never prompt again.
    #[serde(default)]
    pub workspace_grants: Option<String>,
}

/// How the launcher should route a portal request. Kebab-case wire
/// form: `"via-host"` / `"deny"`. Matches the enum on the pkg side
/// verbatim — the shim carries its own copy so this crate doesn't
/// depend on `pkg`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PortalPolicy {
    /// Route through the freedesktop portal on the host session bus.
    ViaHost,
    /// Portal explicitly denied.
    Deny,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Net {
    /// "off" → no network (`--unshare-all` with NO `--share-net`).
    /// Anything else gets the host network shared in.
    #[serde(default = "default_net_mode")]
    pub mode: String,
}

impl Default for Net {
    fn default() -> Self {
        Self {
            mode: default_net_mode(),
        }
    }
}

fn default_net_mode() -> String {
    "off".to_string()
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Fs {
    #[serde(default)]
    pub read: Vec<String>,
    #[serde(default)]
    pub write: Vec<String>,
}

/// Required/optional service version requirement. We treat the version
/// integer as opaque here — strict version negotiation is the broker's
/// job. The launcher only needs to know whether absence is fatal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceReq {
    Required(u32),
    Optional(u32),
}

impl ServiceReq {
    pub fn parse(raw: &str) -> Result<ServiceReq, ServiceReqParseError> {
        let (kind, rest) = raw
            .split_once('@')
            .ok_or_else(|| ServiceReqParseError::MissingAt(raw.to_string()))?;
        let version: u32 = rest
            .trim()
            .parse()
            .map_err(|_| ServiceReqParseError::BadVersion(rest.to_string()))?;
        match kind.trim() {
            "required" => Ok(ServiceReq::Required(version)),
            "optional" => Ok(ServiceReq::Optional(version)),
            other => Err(ServiceReqParseError::BadKind(other.to_string())),
        }
    }

    pub fn version(&self) -> u32 {
        match self {
            ServiceReq::Required(v) | ServiceReq::Optional(v) => *v,
        }
    }

    pub fn is_required(&self) -> bool {
        matches!(self, ServiceReq::Required(_))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ServiceReqParseError {
    #[error("service requirement `{0}` is missing `@<version>`")]
    MissingAt(String),
    #[error("service requirement version `{0}` is not a positive integer")]
    BadVersion(String),
    #[error("service requirement kind `{0}` must be `required` or `optional`")]
    BadKind(String),
}

/// Read + parse a manifest TOML file.
pub fn read_manifest(path: &Path) -> Result<MinimalManifest, ManifestError> {
    let raw = std::fs::read_to_string(path).map_err(|e| ManifestError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let parsed: MinimalManifest =
        toml::from_str(&raw).map_err(|e| ManifestError::Parse(e.to_string()))?;
    Ok(parsed)
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("reading manifest at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("manifest parse error: {0}")]
    Parse(String),
}

/// Expand `~/` and `<scope=current>` placeholders against the process's
/// environment. Pure-ish (reads `$HOME` and CWD) — kept here so tests
/// can run it directly. Unknown placeholders pass through unchanged.
pub fn expand_path(raw: &str) -> PathBuf {
    let mut s = raw.to_string();
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            let mut buf = PathBuf::from(home);
            buf.push(rest);
            return buf;
        }
    }
    if let Some(rest) = s.strip_prefix("<scope=current>") {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let mut buf = cwd;
        // Allow either "<scope=current>/foo" or "<scope=current>foo".
        let rel = rest.trim_start_matches('/');
        buf.push(rel);
        return buf;
    }
    // GUI apps typically need `$XDG_RUNTIME_DIR/wayland-*` bound in.
    // Expand it (and a small allowlist of similar env vars) so
    // manifests stay portable across users.
    for (var, prefix) in [
        ("XDG_RUNTIME_DIR", "$XDG_RUNTIME_DIR"),
        ("XDG_CONFIG_HOME", "$XDG_CONFIG_HOME"),
        ("XDG_DATA_HOME", "$XDG_DATA_HOME"),
        ("XDG_CACHE_HOME", "$XDG_CACHE_HOME"),
    ] {
        if let Some(rest) = s.strip_prefix(prefix) {
            if let Some(value) = std::env::var_os(var) {
                let mut buf = PathBuf::from(value);
                let rel = rest.trim_start_matches('/');
                if !rel.is_empty() {
                    buf.push(rel);
                }
                return buf;
            }
        }
    }
    // Last-ditch: replace HOME-style `~` mid-string is not supported.
    if s.starts_with('~') {
        // bare "~" with no slash — return $HOME if set.
        if s.len() == 1 {
            if let Some(home) = std::env::var_os("HOME") {
                return PathBuf::from(home);
            }
        }
    }
    s.shrink_to_fit();
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_minimal_manifest() {
        let toml_text = r#"
package_id = "com.example.notes"
version = "0.1.0"

[entrypoint]
bin = "bin/notes"

[sandbox.net]
mode = "off"

[sandbox.fs]
read = ["/usr/share/fonts"]
write = []

[sandbox.services]
collab = "required@1"
"#;
        let m: MinimalManifest = toml::from_str(toml_text).expect("parse");
        assert_eq!(m.package_id, "com.example.notes");
        assert_eq!(m.entrypoint.bin, "bin/notes");
        assert_eq!(m.entrypoint.r#type, EntrypointType::Gui);
        assert_eq!(m.sandbox.net.mode, "off");
        assert_eq!(
            m.sandbox.fs.read,
            vec!["/usr/share/fonts".to_string()]
        );
        assert!(m.runtime.is_none());
        let req = ServiceReq::parse(m.sandbox.services.get("collab").unwrap()).unwrap();
        assert!(req.is_required());
        assert_eq!(req.version(), 1);
        assert!(m.cli_commands.is_empty(), "no [cli] block → empty commands");
    }

    #[test]
    fn parses_a_cli_manifest() {
        let toml_text = r#"
package_id = "dev.qos.notes-cli"
version    = "0.1.0"

[entrypoint]
bin  = "bin/notes"
type = "cli"

[sandbox.net]
mode = "off"

[cli]
commands = ["notes", "notes-import"]
"#;
        let m: MinimalManifest = toml::from_str(toml_text).expect("parse");
        assert_eq!(m.entrypoint.r#type, EntrypointType::Cli);
        assert_eq!(m.entrypoint.cli_bin(), Some("bin/notes"));
        assert_eq!(m.entrypoint.gui_bin(), None);
        assert_eq!(m.cli_commands, vec!["notes".to_string(), "notes-import".to_string()]);
    }

    #[test]
    fn parses_a_both_manifest_with_split_bins() {
        let toml_text = r#"
package_id = "dev.qos.notes"
version    = "0.2.0"

[entrypoint]
type    = "both"
gui-bin = "bin/notes-gui"
cli-bin = "bin/notes"

[sandbox.net]
mode = "off"

[cli]
commands = ["notes"]
"#;
        let m: MinimalManifest = toml::from_str(toml_text).expect("parse");
        assert_eq!(m.entrypoint.r#type, EntrypointType::Both);
        assert_eq!(m.entrypoint.gui_bin(), Some("bin/notes-gui"));
        assert_eq!(m.entrypoint.cli_bin(), Some("bin/notes"));
        assert_eq!(m.cli_commands, vec!["notes".to_string()]);
    }

    #[test]
    fn parses_runtime_field_from_package_block() {
        let toml_text = r#"
[package]
id       = "dev.qos.hello"
version  = "0.1.0"
runtime  = "qos-runtime/2026.07"

[entrypoint]
bin = "bin/qos-hello"

[sandbox.net]
mode = "off"
"#;
        let m: MinimalManifest = toml::from_str(toml_text).expect("parse");
        assert_eq!(m.package_id, "dev.qos.hello");
        assert_eq!(m.runtime.as_deref(), Some("qos-runtime/2026.07"));
        let (name, ver) = m.runtime_ref().expect("runtime ref");
        assert_eq!(name, "qos-runtime");
        assert_eq!(ver, "2026.07");
    }

    #[test]
    fn service_req_parses_required_and_optional() {
        assert_eq!(
            ServiceReq::parse("required@2").unwrap(),
            ServiceReq::Required(2)
        );
        assert_eq!(
            ServiceReq::parse("optional@7").unwrap(),
            ServiceReq::Optional(7)
        );
    }

    #[test]
    fn service_req_rejects_malformed() {
        assert!(ServiceReq::parse("required").is_err());
        assert!(ServiceReq::parse("maybe@1").is_err());
        assert!(ServiceReq::parse("required@abc").is_err());
    }

    #[test]
    fn portals_table_parses_via_host_and_deny() {
        let toml_text = r#"
package_id = "com.example.notes"
version = "0.1.0"

[entrypoint]
bin = "bin/notes"

[sandbox.portals]
file-chooser = "via-host"
notifications = "deny"
"#;
        let m: MinimalManifest = toml::from_str(toml_text).expect("parse");
        assert_eq!(
            m.sandbox.portals.get("file-chooser").copied(),
            Some(PortalPolicy::ViaHost)
        );
        assert_eq!(
            m.sandbox.portals.get("notifications").copied(),
            Some(PortalPolicy::Deny)
        );
    }

    #[test]
    fn portals_table_empty_by_default() {
        let toml_text = r#"
package_id = "com.example.notes"
version = "0.1.0"

[entrypoint]
bin = "bin/notes"
"#;
        let m: MinimalManifest = toml::from_str(toml_text).expect("parse");
        assert!(m.sandbox.portals.is_empty());
    }

    #[test]
    fn expand_path_replaces_home_and_scope() {
        // SAFETY: tests run in a single thread per-process by default for
        // env mutation here; the test asserts only on the structure.
        // Use a value unlikely to collide with the real $HOME.
        unsafe { std::env::set_var("HOME", "/tmp/qos-launcher-test-home") };
        let p = expand_path("~/Documents");
        assert_eq!(p, PathBuf::from("/tmp/qos-launcher-test-home/Documents"));

        let scope = expand_path("<scope=current>/scratch");
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(scope, cwd.join("scratch"));
    }
}
