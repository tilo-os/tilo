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
//! [sandbox.services]
//! collab        = "required@1"     # or "optional@1"
//! fs            = "optional@1"
//! ```
//!
//! ### `[sandbox.fs]` is not part of the vocabulary
//!
//! A manifest used to name the paths it wanted, and the launcher bound
//! them — so the sandbox enforced a policy written by the thing it was
//! sandboxing. `write = ["~"]`, `write = ["/"]` and
//! `write = ["~/.local/share/dev.qos.totp"]` were all accepted and all
//! faithfully bound read-write. Signing did not help: it proves who
//! wrote the manifest, not that what it asks for is reasonable.
//!
//! So the field is gone, and a manifest that still carries one is
//! refused rather than ignored — see [`enforce_fs_policy`]. Ignoring it
//! would leave the app running with less access than it expected, which
//! surfaces as a mysterious bug instead of a policy.
//!
//! What replaces it:
//!
//! - **Writes** — [`crate::appdir`]. The launcher gives the app one
//!   directory, per room, and points `HOME`/`XDG_*` at it.
//! - **Reads** — [`crate::profile`]. One fixed system list, identical for
//!   every package.
//! - **Anything else** — [`crate::capabilities`], a grant the user
//!   consented to. One route, not two.

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
    /// `[package].name` — the publisher's human-facing name. `None`
    /// when the manifest omits it; callers fall back to deriving one
    /// from the package id.
    pub name: Option<String>,
    /// `[package].category` — freedesktop-style menu category.
    pub category: Option<String>,
    /// `[package].icon` — payload-relative path to the app's icon image
    /// (a raster file: PNG/JPEG/WebP). The launcher resolves it to the
    /// in-sandbox payload mount and exports it as `QOS_APP_ICON`, which
    /// the toolkit loads into the window icon. `None` when the manifest
    /// ships no icon art — the toolkit then draws a generated default.
    pub icon: Option<String>,
    pub version: String,
    /// `[package].kind` — what the package *is*, as distinct from how it
    /// starts. `app` is something a user opens; `service`, `plugin`,
    /// `format-provider` and `runtime` are things the system uses.
    ///
    /// This is separate from [`Entrypoint::entry_type`] on purpose. A
    /// package can draw and still not belong in a launcher: the
    /// on-screen keyboard is a real GUI surface the shell places itself,
    /// and offering "Open On-Screen Keyboard" as a tiled window is not
    /// what anyone meant by it. Defaults to `app` so a manifest that
    /// omits the field keeps its current behaviour.
    pub kind: String,
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
    /// `[providers].ext` — file extensions this package declares it can
    /// open, lowercased and without the leading dot. This is how the
    /// desktop learns which installed app owns a document type: the
    /// format registry knows `.odt` is an editable document, the
    /// manifest is what says *this* package opens it. Empty for a
    /// package that opens no files.
    pub opens_ext: Vec<String>,
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
    #[serde(default)]
    providers: Option<ProvidersBlock>,
}

/// `[providers]` block — the file kinds a package declares it opens.
/// Mirrors [`pkg::manifest::Providers`]; decoded here so the launcher
/// stays independent of the full pkg crate.
#[derive(Debug, Clone, Default, Deserialize)]
struct ProvidersBlock {
    #[serde(default)]
    ext: Vec<String>,
    /// Accepted so a manifest carrying `mime = [...]` still decodes.
    /// Nothing routes on MIME yet — extension is the only resolver.
    #[serde(default)]
    #[allow(dead_code)]
    mime: Vec<String>,
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
    name: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    icon: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    runtime: Option<String>,
}

/// `[package].kind` for something a user opens. Everything else is a
/// system component, however it starts.
pub const KIND_APP: &str = "app";

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
        let kind = raw
            .package
            .as_ref()
            .and_then(|p| p.kind.clone())
            .map(|k| k.trim().to_ascii_lowercase())
            .filter(|k| !k.is_empty())
            .unwrap_or_else(|| KIND_APP.to_string());
        let cli_commands = raw.cli.map(|c| c.commands).unwrap_or_default();
        // Normalised on decode so every consumer compares the same
        // shape: lowercase, no leading dot, no blanks. A manifest that
        // writes `".ODT"` and one that writes `"odt"` mean the same
        // thing and must not resolve differently.
        let opens_ext = raw
            .providers
            .map(|p| p.ext)
            .unwrap_or_default()
            .into_iter()
            .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
            .filter(|e| !e.is_empty())
            .collect();
        let runtime = raw.package.as_ref().and_then(|p| p.runtime.clone());
        let name = raw.package.as_ref().and_then(|p| p.name.clone());
        let category = raw.package.as_ref().and_then(|p| p.category.clone());
        let icon = raw
            .package
            .as_ref()
            .and_then(|p| p.icon.clone())
            .map(|i| i.trim().to_string())
            .filter(|i| !i.is_empty());
        MinimalManifest {
            package_id,
            name,
            category,
            icon,
            version,
            kind,
            runtime,
            entrypoint: raw.entrypoint,
            sandbox: raw.sandbox,
            cli_commands,
            opens_ext,
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

#[derive(Debug, Clone, Deserialize)]
pub struct Sandbox {
    /// `[sandbox] seccomp = "…"` — the named syscall profile the package
    /// declares. `qos-default` is the baseline and enforces write-xor-execute;
    /// `qos-jit` is the same policy without that rule, for a program that
    /// genuinely compiles code at run time.
    ///
    /// A **request**, not a setting. Resolved through
    /// [`Sandbox::seccomp_profile`], which treats every unrecognised name as
    /// the strict one — a manifest typo must not be a route to a weaker
    /// sandbox — and checks the weaker name against the image's own
    /// entitlement list, because a package writing a string about itself is
    /// not authority to lift a protection off itself.
    #[serde(default = "default_seccomp_profile")]
    pub seccomp: String,
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
}

fn default_seccomp_profile() -> String {
    crate::seccomp::SeccompProfile::QosDefault
        .as_str()
        .to_string()
}

/// Hand-written rather than derived so the default profile is the named strict
/// one and not an empty string — a `Sandbox` built in code confines the same
/// way as one decoded from a manifest that omitted the key.
impl Default for Sandbox {
    fn default() -> Self {
        Self {
            seccomp: default_seccomp_profile(),
            net: Net::default(),
            fs: Fs::default(),
            services: BTreeMap::new(),
            portals: BTreeMap::new(),
        }
    }
}

impl Sandbox {
    /// The syscall profile this launch gets, resolved fail-closed against the
    /// image's entitlement policy.
    ///
    /// Takes the package id because the manifest's own answer is not
    /// sufficient: whether a package may run without write-xor-execute is the
    /// image's decision, not the packager's. See
    /// [`crate::seccomp::resolve_profile`]. There is deliberately no id-free
    /// variant to reach for.
    pub fn seccomp_profile(&self, package_id: &str) -> crate::seccomp::SeccompProfile {
        crate::seccomp::resolve_profile(&self.seccomp, package_id).applied
    }
}

/// How the launcher should route a portal request. Kebab-case wire
/// form: `"via-host"` / `"deny"`.
///
/// This is `pkg`'s type, re-exported — not a copy. The launcher and
/// the packager decide the same question from the same manifest
/// field, so they must not be able to disagree about what the string
/// means.
pub use pkg::manifest::PortalPolicy;

#[derive(Debug, Clone, Deserialize)]
pub struct Net {
    /// - `"off"` → no network (`--unshare-all` with NO `--share-net`).
    /// - `"allowlist"` → only [`Net::hosts`], brokered through the
    ///   egress gate.
    /// - anything else → the host network is shared in.
    #[serde(default = "default_net_mode")]
    pub mode: String,
    /// Endpoints the package declared for `mode = "allowlist"`.
    ///
    /// This field was previously not decoded at all, so a manifest that
    /// enumerated its endpoints was read as "not off" and got
    /// unrestricted access — the packager's allowlist reached nothing.
    /// [`crate::netgate::EgressDecision::resolve`] now makes it the
    /// ceiling for the app, and the gate enforces it.
    #[serde(default)]
    pub hosts: Vec<String>,
}

impl Default for Net {
    fn default() -> Self {
        Self {
            mode: default_net_mode(),
            hosts: Vec::new(),
        }
    }
}

fn default_net_mode() -> String {
    "off".to_string()
}

/// `[sandbox.fs]` — **retired**. Decoded only so that a manifest still
/// carrying it can be named in a refusal.
///
/// Nothing reads these fields to build a sandbox. They exist so
/// [`enforce_fs_policy`] can tell the user *which* entry in *which*
/// package is the problem, which a serde "unknown field" error cannot.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Fs {
    #[serde(default)]
    pub read: Vec<String>,
    #[serde(default)]
    pub write: Vec<String>,
}

/// A manifest asking for filesystem access it is not allowed to ask for.
///
/// One variant per field so the message can say what to do instead —
/// the two cases have different answers, and "declare fewer paths" is
/// not one of them.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FsPolicyViolation {
    #[error(
        "package `{package}` declares `sandbox.fs.write = [{entry:?}]`, and an app does not \
         choose where it writes.\n  \
         The launcher gives every app one directory — per room, per package — binds it at \
         {app_dir} and points HOME, XDG_DATA_HOME, XDG_CONFIG_HOME, XDG_STATE_HOME and \
         XDG_CACHE_HOME inside it. Remove the `[sandbox.fs]` block; anything the app writes \
         under those paths already lands there.\n  \
         Access to a path outside it is a capability the user grants at runtime, not a line \
         in a manifest."
    )]
    Write { package: String, entry: String, app_dir: &'static str },
    #[error(
        "package `{package}` declares `sandbox.fs.read = [{entry:?}]`, and an app does not \
         choose what it reads.\n  \
         Every app gets the same system read profile (loader and C library, fonts, GPU loader \
         manifests, CA bundle, timezone data). Remove the `[sandbox.fs]` block.\n  \
         Anything the profile does not cover — a user's documents, /usr/bin, /var/workspace, a \
         device node — is a capability the user grants at runtime, not a line in a manifest."
    )]
    Read { package: String, entry: String },
}

/// Refuse a manifest that names filesystem paths.
///
/// Called at launch (`crate::run`) and at install (`pkg::validate`), on
/// purpose: an install-time check alone would be bypassed by anything
/// that hands the launcher a manifest path directly — which is exactly
/// what `qos-launcher run --manifest` does, and what a package that was
/// installed before this rule existed still has on disk.
///
/// **No first-party exception.** Every manifest in the tree is ours and
/// every one of them asked for something plausible; that is how the
/// original hole stayed invisible. A rule with a bypass for the packages
/// that ship with the OS is a rule about strangers, not a boundary.
pub fn enforce_fs_policy(manifest: &MinimalManifest) -> Result<(), FsPolicyViolation> {
    // Writes first: a package asking for `/` is the more alarming of the
    // two and should be the line the user reads.
    if let Some(entry) = manifest.sandbox.fs.write.first() {
        return Err(FsPolicyViolation::Write {
            package: manifest.package_id.clone(),
            entry: entry.clone(),
            app_dir: crate::appdir::SANDBOX_APP_DIR,
        });
    }
    if let Some(entry) = manifest.sandbox.fs.read.first() {
        return Err(FsPolicyViolation::Read {
            package: manifest.package_id.clone(),
            entry: entry.clone(),
        });
    }
    Ok(())
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

// `expand_path` used to live here. It turned a manifest string into a
// host path — `~/` against `$HOME`, `$XDG_*` against the environment,
// `<scope=current>` against the launcher's CWD — with no `..` handling,
// no canonicalisation and no allowlist, and unknown placeholders passed
// through unchanged. Whatever came out was bound, source and
// destination, read-write for every `fs.write` entry.
//
// It is deleted rather than hardened because there is nothing left to
// call it: no manifest field feeds a path into the sandbox any more. A
// hardened expander would still be a mechanism for turning package text
// into a mount, waiting for the next field that wants one.

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped manifests are the source of truth for which app opens
    /// which file type, so the parse has to survive a *real* one — the
    /// full `qos-app.toml` shape with `[publisher]`, `[[trust.*]]`,
    /// `[sandbox.*]` and the rest — not just a hand-written minimum.
    #[test]
    fn a_shipped_manifest_yields_its_declared_extensions() {
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .expect("workspace root above crates/packaging/launcher");
        let path = repo.join("apps/word-processor/qos-app.toml");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        let m: MinimalManifest = toml::from_str(&text)
            .unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()));
        assert_eq!(m.package_id, "dev.qos.word-processor");
        for ext in ["odt", "docx", "md", "rtf"] {
            assert!(
                m.opens_ext.iter().any(|e| e == ext),
                "the word processor declares .{ext}; parsed {:?}",
                m.opens_ext
            );
        }
        assert!(
            m.opens_ext.iter().all(|e| !e.starts_with('.')
                && e.chars().all(|c| !c.is_ascii_uppercase())),
            "extensions are normalised on decode: {:?}",
            m.opens_ext
        );
    }

    /// A package that opens nothing must say so as an empty list, not as
    /// a parse failure — most manifests have no `[providers]` block at
    /// all, and every one of them still has to load.
    #[test]
    fn a_manifest_without_a_providers_block_opens_nothing() {
        let m: MinimalManifest = toml::from_str(
            "package_id = \"com.example.x\"\nversion = \"1\"\n\
             [entrypoint]\nbin = \"bin/x\"\n",
        )
        .expect("a manifest with no [providers] block still parses");
        assert!(m.opens_ext.is_empty());
    }

    /// Case and a leading dot are packaging noise, not meaning.
    #[test]
    fn declared_extensions_are_normalised() {
        let m: MinimalManifest = toml::from_str(
            "package_id = \"com.example.x\"\nversion = \"1\"\n\
             [entrypoint]\nbin = \"bin/x\"\n\
             [providers]\next = [\".ODT\", \" Md \", \"\"]\n",
        )
        .expect("parses");
        assert_eq!(m.opens_ext, vec!["odt".to_string(), "md".to_string()]);
    }

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
    fn package_kind_decodes_and_defaults_to_app() {
        // A GUI entrypoint on a `service` is legal and meaningful: the
        // on-screen keyboard draws, but the shell decides when to show
        // it. Callers distinguish the two by `kind`, so it has to
        // survive decoding rather than being inferred from the
        // entrypoint.
        let service = r#"
[package]
id      = "dev.qos.osk"
version = "0.1.0"
kind    = "service"

[entrypoint]
bin  = "bin/osk"
type = "gui"
"#;
        let m: MinimalManifest = toml::from_str(service).expect("parse");
        assert_eq!(m.kind, "service");
        assert_eq!(
            m.entrypoint.gui_bin(),
            Some("bin/osk"),
            "a service can still have a GUI entrypoint",
        );

        // Omitted, blank, and oddly-cased all mean the ordinary case.
        for (src, why) in [
            ("", "a manifest with no kind is an app"),
            ("kind = \"\"\n", "a blank kind is an app"),
            ("kind = \"App\"\n", "kind is compared case-insensitively"),
            ("kind = \" app \"\n", "surrounding space is not part of the kind"),
        ] {
            let toml_text = format!(
                "[package]\nid = \"dev.qos.x\"\nversion = \"0.1.0\"\n{src}\n\
                 [entrypoint]\nbin = \"bin/x\"\n"
            );
            let m: MinimalManifest = toml::from_str(&toml_text).expect("parse");
            assert_eq!(m.kind, KIND_APP, "{why}");
        }
    }

    #[test]
    fn every_shipped_manifest_declares_a_kind_the_launcher_understands() {
        // A typo'd kind is not a parse error — it just silently stops
        // being an app, which shows up as an icon missing from the
        // launcher and nowhere else.
        let known = ["app", "service", "plugin", "format-provider", "runtime"];
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../apps");
        let mut checked = 0;
        for entry in std::fs::read_dir(&root).expect("apps/ readable") {
            let path = entry.expect("dir entry").path().join("qos-app.toml");
            if !path.exists() {
                continue;
            }
            let raw = std::fs::read_to_string(&path).expect("read manifest");
            let m: MinimalManifest = toml::from_str(&raw)
                .unwrap_or_else(|e| panic!("{} does not parse: {e}", path.display()));
            assert!(
                known.contains(&m.kind.as_str()),
                "{} declares kind = {:?}, which is not one of {known:?}",
                path.display(),
                m.kind,
            );
            checked += 1;
        }
        assert!(checked > 0, "found no manifests under {}", root.display());
    }

    #[test]
    fn parses_name_and_icon_from_package_block() {
        // The human name and the icon path both come off `[package]`;
        // the launcher forwards them to the app as QOS_APP_NAME /
        // QOS_APP_ICON.
        let toml_text = r#"
[package]
id      = "dev.qos.notes"
name    = "Notes"
icon    = "assets/notes.png"
version = "0.1.0"

[entrypoint]
bin = "bin/notes"
"#;
        let m: MinimalManifest = toml::from_str(toml_text).expect("parse");
        assert_eq!(m.name.as_deref(), Some("Notes"));
        assert_eq!(m.icon.as_deref(), Some("assets/notes.png"));
    }

    #[test]
    fn icon_is_none_when_absent_or_blank() {
        // Omitted or whitespace-only means "no icon" — the toolkit then
        // draws its generated default rather than pointing at nothing.
        let without = r#"
[package]
id      = "dev.qos.notes"
version = "0.1.0"

[entrypoint]
bin = "bin/notes"
"#;
        let m: MinimalManifest = toml::from_str(without).expect("parse");
        assert_eq!(m.icon, None);

        let blank = r#"
[package]
id      = "dev.qos.notes"
icon    = "   "
version = "0.1.0"

[entrypoint]
bin = "bin/notes"
"#;
        let m: MinimalManifest = toml::from_str(blank).expect("parse");
        assert_eq!(m.icon, None, "a blank icon path is treated as absent");
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

    fn manifest_with(block: &str) -> MinimalManifest {
        let src = format!(
            "package_id = \"com.example.hostile\"\nversion = \"1\"\n\
             [entrypoint]\nbin = \"bin/x\"\n{block}"
        );
        toml::from_str(&src).expect("a manifest declaring fs paths still decodes")
    }

    /// The six shapes the original hole made possible. Each is refused,
    /// and the refusal names the package and the entry — a message that
    /// says only "invalid manifest" leaves a packager guessing.
    #[test]
    fn every_path_a_package_could_name_is_refused() {
        for entry in [
            "~",
            "/",
            "~/.ssh",
            "~/../../etc",
            "~/.local/share/dev.qos.totp",
            "$XDG_DATA_HOME/dev.qos.totp",
        ] {
            let m = manifest_with(&format!("[sandbox.fs]\nwrite = [{entry:?}]\n"));
            let Err(err) = enforce_fs_policy(&m) else {
                panic!("write = [{entry:?}] must be refused");
            };
            let text = err.to_string();
            assert!(
                text.contains("com.example.hostile") && text.contains(entry),
                "the refusal must name the package and the entry: {text}"
            );
        }
    }

    /// A read is refused on the same terms. The read profile is not a
    /// filter a manifest gets to add to — an entry the profile already
    /// covers is refused too, because the point is that the field is
    /// gone, not that its contents are checked.
    #[test]
    fn a_declared_read_is_refused_whether_or_not_the_profile_covers_it() {
        for entry in ["/etc/shadow", "/var/workspace", "/usr/share/fonts"] {
            let m = manifest_with(&format!("[sandbox.fs]\nread = [{entry:?}]\n"));
            let Err(err) = enforce_fs_policy(&m) else {
                panic!("read = [{entry:?}] must be refused");
            };
            assert!(
                err.to_string().contains(entry),
                "the refusal must name the entry: {err}"
            );
        }
    }

    /// A manifest with no `[sandbox.fs]` block is the normal case and
    /// must stay silent — an empty list is not a declaration.
    #[test]
    fn a_manifest_that_names_no_path_passes() {
        let m = manifest_with("[sandbox.net]\nmode = \"off\"\n");
        assert!(enforce_fs_policy(&m).is_ok());
        let empty = manifest_with("[sandbox.fs]\nread = []\nwrite = []\n");
        assert!(
            enforce_fs_policy(&empty).is_ok(),
            "an explicitly empty block declares nothing"
        );
    }

    /// Every manifest the OS ships goes through the same check as a
    /// stranger's. The moment one of ours is exempt, the rule is about
    /// strangers rather than about apps.
    #[test]
    fn no_shipped_manifest_names_a_path() {
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .expect("workspace root above crates/packaging/launcher");
        let apps = repo.join("apps");
        let mut checked = 0usize;
        for entry in std::fs::read_dir(&apps).expect("apps/ is readable") {
            let path = entry.expect("dir entry").path().join("qos-app.toml");
            if !path.exists() {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("manifest readable");
            let m: MinimalManifest = toml::from_str(&text)
                .unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()));
            enforce_fs_policy(&m)
                .unwrap_or_else(|e| panic!("{} is refused by its own OS: {e}", path.display()));
            checked += 1;
        }
        assert!(
            checked >= 40,
            "expected the shipped app fleet, found {checked} manifests"
        );
    }
}
