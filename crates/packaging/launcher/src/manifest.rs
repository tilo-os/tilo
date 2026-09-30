//! In-tree minimal manifest shim.
//!
//! This module decodes the launcher-facing subset of the canonical manifest
//! schema. [`MinimalManifest`] keeps the launcher independent of the full
//! package model while sharing its policy types.
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
//! `[sandbox.fs]` is rejected rather than interpreted: filesystem writes use
//! [`crate::appdir`], system reads use [`crate::profile`], and other access
//! requires an explicit [`crate::capabilities`] grant. This prevents a package
//! from defining the boundary meant to contain it; rejecting the field also
//! avoids silently running an app with less access than its manifest requested.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Minimal manifest needed by the launcher, lives in-tree pending full
/// `tilo-app-package` integration. Accepts both the top-level `package_id =
/// "..."` form and tilo-app-package's canonical `[package] id = "..."` nested
/// table, normalised into `MinimalManifest::package_id` (same for
/// `version`).
#[derive(Debug, Clone, Deserialize)]
#[serde(from = "RawManifest")]
pub struct MinimalManifest {
    pub package_id: String,
    /// `[package].name` — the publisher's human-facing name. `None` falls
    /// back to deriving one from the package id.
    pub name: Option<String>,
    /// `[package.names]` — the name in other languages, by BCP 47 tag.
    pub localized_names: std::collections::BTreeMap<String, String>,
    /// `[package].category` — freedesktop-style menu category.
    pub category: Option<String>,
    /// `[package].icon` — payload-relative icon path, resolved to the
    /// in-sandbox payload mount and exported as `TILO_APP_ICON`. `None`
    /// draws a generated default.
    pub icon: Option<String>,
    pub version: String,
    /// `[package].kind` — what the package *is*: `app` is something a user
    /// opens; `service`/`plugin`/`format-provider`/`runtime` are
    /// system-used. Separate from `Entrypoint::entry_type` since a
    /// package can draw and still not belong in a launcher (the on-screen
    /// keyboard). Defaults to `app`.
    pub kind: String,
    /// Optional `[package].runtime` reference (`tilo-runtime/<version>`);
    /// when present the launcher mounts the bundle so the app doesn't need
    /// `--minimal-libs`.
    pub runtime: Option<String>,
    pub entrypoint: Entrypoint,
    pub sandbox: Sandbox,
    /// `[cli].commands` — PATH-visible command names the package declares.
    pub cli_commands: Vec<String>,
    /// `[providers].ext` — file extensions this package can open, how the
    /// desktop learns which app owns a document type.
    pub opens_ext: Vec<String>,
    /// Normalized, concrete MIME types declared by the package.
    pub opens_mime: Vec<String>,
    /// Normalized URI schemes (without `:`) declared by the package.
    pub opens_schemes: Vec<String>,
    /// Signed minimum handoff-fidelity facets. Legacy manifests default to
    /// framework presentation state.
    pub continuity: ContinuityContract,
}

impl MinimalManifest {
    /// Split `runtime = "tilo-runtime/<v>"` into `(name, version)`; `None`
    /// if unset or shaped wrong.
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
    #[serde(default)]
    continuity: ContinuityContract,
}

pub use tilo_app_package::manifest::{
    ContinuityContract, ContinuityFacet, ContinuitySemanticSchema,
};

/// `[providers]` block, mirrors [`tilo_app_package::manifest::Providers`], decoded
/// here to keep the launcher independent of the full pkg crate.
#[derive(Debug, Clone, Default, Deserialize)]
struct ProvidersBlock {
    #[serde(default)]
    ext: Vec<String>,
    #[serde(default)]
    mime: Vec<String>,
    #[serde(default)]
    schemes: Vec<String>,
}

/// `[cli]` block, mirrors [`tilo_app_package::manifest::Cli`], decoded independently
/// of the pkg crate.
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
    names: std::collections::BTreeMap<String, String>,
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
/// `[package].kind` for a headless component the desktop starts and
/// supervises. Service packages are never promoted to ordinary app launchers.
pub const KIND_SERVICE: &str = "service";

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
        let providers = raw.providers.unwrap_or_default();
        // Normalised on decode: lowercase, no leading dot, no blanks — a
        // manifest writing `".ODT"` and one writing `"odt"` must resolve the same.
        let opens_ext = providers
            .ext
            .into_iter()
            .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
            .filter(|e| valid_extension(e))
            .collect();
        let opens_mime = providers
            .mime
            .into_iter()
            .map(|mime| mime.trim().to_ascii_lowercase())
            .filter(|mime| valid_mime(mime))
            .collect();
        let opens_schemes = providers
            .schemes
            .into_iter()
            .map(|scheme| scheme.trim().trim_end_matches(':').to_ascii_lowercase())
            .filter(|scheme| valid_scheme(scheme))
            .collect();
        let runtime = raw.package.as_ref().and_then(|p| p.runtime.clone());
        let name = raw.package.as_ref().and_then(|p| p.name.clone());
        let localized_names = raw
            .package
            .as_ref()
            .map(|p| {
                p.names
                    .iter()
                    .map(|(tag, name)| (tag.trim().to_string(), name.trim().to_string()))
                    .filter(|(tag, name)| !tag.is_empty() && !name.is_empty())
                    .collect()
            })
            .unwrap_or_default();
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
            localized_names,
            category,
            icon,
            version,
            kind,
            runtime,
            entrypoint: raw.entrypoint,
            sandbox: raw.sandbox,
            cli_commands,
            opens_ext,
            opens_mime,
            opens_schemes,
            continuity: raw.continuity,
        }
    }
}

fn valid_extension(ext: &str) -> bool {
    !ext.is_empty()
        && ext.len() <= 64
        && ext
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'_'))
}

fn valid_mime(mime: &str) -> bool {
    let Some((top, subtype)) = mime.split_once('/') else {
        return false;
    };
    let token = |part: &str| {
        !part.is_empty()
            && part.len() <= 127
            && part.bytes().all(|b| {
                b.is_ascii_alphanumeric()
                    || matches!(
                        b,
                        b'!' | b'#' | b'$' | b'&' | b'^' | b'_' | b'.' | b'+' | b'-'
                    )
            })
    };
    mime.len() <= 255
        && !subtype.contains('/')
        && top != "*"
        && subtype != "*"
        && token(top)
        && token(subtype)
}

fn valid_scheme(scheme: &str) -> bool {
    let mut chars = scheme.chars();
    !scheme.is_empty()
        && scheme.len() <= 32
        && chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        && !matches!(scheme, "file" | "data" | "javascript")
}

#[derive(Debug, Clone, Deserialize)]
pub struct Entrypoint {
    /// Path of the entrypoint binary, relative to the payload root. Used
    /// verbatim for `"gui"`/`"cli"`; falls back for `"both"`.
    #[serde(default)]
    pub bin: String,
    /// Which flavour of entrypoint. Mirrors [`tilo_app_package::manifest::EntrypointType`].
    #[serde(default, rename = "type")]
    pub r#type: EntrypointType,
    /// GUI-flavour binary path inside the payload. Only meaningful when
    /// `r#type = "both"`.
    #[serde(default, rename = "gui-bin")]
    pub gui_bin: Option<String>,
    /// CLI-flavour binary path inside the payload. Only meaningful when
    /// `r#type = "both"`.
    #[serde(default, rename = "cli-bin")]
    pub cli_bin: Option<String>,
}

/// Which flavour of entrypoint the package exposes: `type = "gui"` /
/// `"cli"` / `"both"`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntrypointType {
    #[default]
    Gui,
    Cli,
    Both,
}

impl Entrypoint {
    /// Binary the *GUI* launcher path should exec. `None` for a pure-CLI
    /// package (the `run` subcommand refuses to launch it directly).
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

    /// Binary a CLI command should exec. `None` for a pure-GUI package.
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
    /// `[sandbox] seccomp = "…"` — the named syscall profile requested.
    /// `tilo-default` enforces write-xor-execute; `tilo-jit` is the same
    /// policy without that rule, for code genuinely compiled at run time.
    ///
    /// A **request**, not a setting: [`Sandbox::seccomp_profile`] treats an
    /// unrecognised name as strict, and checks the weaker name against the
    /// image's entitlement list — a package writing a string about itself
    /// is not authority to lift a protection off itself.
    #[serde(default = "default_seccomp_profile")]
    pub seccomp: String,
    #[serde(default)]
    pub net: Net,
    #[serde(default)]
    pub fs: Fs,
    /// Map of service-name -> requirement string ("required@1",
    /// "optional@2", …), parsed in [`ServiceReq::parse`].
    #[serde(default)]
    pub services: BTreeMap<String, String>,
    /// XDG portal policy per portal name. When at least one is `ViaHost`
    /// the launcher bind-mounts the host session D-Bus socket and sets
    /// `DBUS_SESSION_BUS_ADDRESS`; absent/empty means the bus is NOT exposed.
    #[serde(default)]
    pub portals: BTreeMap<String, PortalPolicy>,
}

fn default_seccomp_profile() -> String {
    crate::seccomp::SeccompProfile::TiloDefault
        .as_str()
        .to_string()
}

/// Hand-written rather than derived so the default profile is the
/// named strict one, not an empty string.
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
    /// The syscall profile this launch gets, resolved fail-closed against
    /// the image's entitlement policy — takes the package id since the
    /// manifest's own answer isn't sufficient: whether a package may run
    /// without write-xor-execute is the image's decision, not the
    /// packager's.
    pub fn seccomp_profile(&self, package_id: &str) -> crate::seccomp::SeccompProfile {
        crate::seccomp::resolve_profile(&self.seccomp, package_id).applied
    }
}

/// How the launcher should route a portal request. `pkg`'s type,
/// re-exported not copied, so the launcher and packager can't disagree
/// about what the field means.
pub use tilo_app_package::manifest::PortalPolicy;

#[derive(Debug, Clone, Deserialize)]
pub struct Net {
    /// - `"off"` → no network (`--unshare-all` with NO `--share-net`).
    /// - `"allowlist"` → only [`Net::hosts`], brokered through the
    ///   egress gate.
    /// - anything else → the host network is shared in.
    #[serde(default = "default_net_mode")]
    pub mode: String,
    /// Endpoints declared for `mode = "allowlist"`. Previously not
    /// decoded, so an enumerating manifest was read as "not off" and got
    /// unrestricted access. [`crate::netgate::EgressDecision::resolve`]
    /// now makes it the ceiling.
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

/// `[sandbox.fs]` — **retired**. Decoded only so a manifest still
/// carrying it can be named in a refusal by [`enforce_fs_policy`].
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Fs {
    #[serde(default)]
    pub read: Vec<String>,
    #[serde(default)]
    pub write: Vec<String>,
}

/// A manifest asking for filesystem access it is not allowed to ask
/// for. One variant per field so the message can say what to do
/// instead — the two cases have different answers.
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
    Write {
        package: String,
        entry: String,
        app_dir: &'static str,
    },
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
/// Called at launch and at install: an install-time check alone would
/// be bypassed by `tilo-launcher run --manifest` or a package installed
/// before this rule existed.
///
/// **No first-party exception.** Every manifest in the tree asked for
/// something plausible — that is how the original hole stayed
/// invisible. A bypass for OS-shipped packages would make this a rule
/// about strangers, not a boundary.
///
/// # Errors
///
/// Returns [`FsPolicyViolation::Write`] or [`FsPolicyViolation::Read`] if
/// `manifest`'s `[sandbox.fs]` names any path at all — the vocabulary was
/// removed; naming one is refused, not silently dropped.
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

/// Required/optional service version requirement. The version integer
/// is opaque here; the launcher only needs to know if absence is fatal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceReq {
    Required(u32),
    Optional(u32),
}

impl ServiceReq {
    /// # Errors
    ///
    /// Returns [`ServiceReqParseError::MissingAt`] if `raw` has no `@`,
    /// [`ServiceReqParseError::BadVersion`] if the part after `@` is not a
    /// `u32`, or [`ServiceReqParseError::BadKind`] if the part before `@` is
    /// not `required`/`optional`.
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
///
/// # Errors
///
/// Returns [`ManifestError::Io`] if `path` cannot be read, or
/// [`ManifestError::Parse`] if its contents are not a valid manifest.
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

// `expand_path` (host path expansion for manifest fs paths) is deleted,
// not hardened, because no manifest field feeds a path into the
// sandbox any more — a hardened expander would just be a mechanism
// waiting for the next field that wants one.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_system_apps_carry_their_german_names() {
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .expect("workspace root above crates/packaging/launcher");
        for (app, german) in [
            ("file-explorer", "Dateien"),
            ("settings", "Persönliche Einstellungen"),
            ("notes", "Notizen"),
        ] {
            let manifest = read_manifest(&repo.join("apps").join(app).join("tilo-app.toml"))
                .unwrap_or_else(|e| panic!("{app}: {e}"));
            assert_eq!(
                manifest.localized_names.get("de").map(String::as_str),
                Some(german),
                "{app}"
            );
        }
    }

    #[test]
    fn opener_claims_are_read_and_normalised() {
        // Phase `a_shipped_manifest_yields_its_declared_extensions`.
        // The shipped manifests are the source of truth for which app opens
        // which file type, so the parse must survive a *real* one.
        {
            let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(3)
                .expect("workspace root above crates/packaging/launcher");
            let path = repo.join("apps/word-processor/tilo-app.toml");
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
            let m: MinimalManifest =
                toml::from_str(&text).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()));
            assert_eq!(m.package_id, "so.tilo.word-processor");
            for ext in ["odt", "docx", "md", "rtf"] {
                assert!(
                    m.opens_ext.iter().any(|e| e == ext),
                    "the word processor declares .{ext}; parsed {:?}",
                    m.opens_ext
                );
            }
            assert!(
                m.opens_ext
                    .iter()
                    .all(|e| !e.starts_with('.') && e.chars().all(|c| !c.is_ascii_uppercase())),
                "extensions are normalised on decode: {:?}",
                m.opens_ext
            );
        }
        // Phase `a_manifest_without_a_providers_block_opens_nothing`.
        // A package that opens nothing must say so as an empty list, not a
        // parse failure — most manifests have no `[providers]` block at all.
        {
            let m: MinimalManifest = toml::from_str(
                "package_id = \"com.example.x\"\nversion = \"1\"\n\
                 [entrypoint]\nbin = \"bin/x\"\n",
            )
            .expect("a manifest with no [providers] block still parses");
            assert!(m.opens_ext.is_empty());
            assert_eq!(
                m.continuity.required,
                vec![ContinuityFacet::Presentation],
                "legacy manifests retain the framework presentation baseline"
            );
        }
        // Phase `declared_extensions_are_normalised`.
        // Case and a leading dot are packaging noise, not meaning.
        {
            let m: MinimalManifest = toml::from_str(
                "package_id = \"com.example.x\"\nversion = \"1\"\n\
                 [entrypoint]\nbin = \"bin/x\"\n\
                 [providers]\next = [\".ODT\", \" Md \", \"\"]\n",
            )
            .expect("parses");
            assert_eq!(m.opens_ext, vec!["odt".to_string(), "md".to_string()]);
        }
        // Phase `mime_and_scheme_claims_are_normalized_and_unsafe_values_dropped`.
        {
            let m: MinimalManifest = toml::from_str(
                "package_id = \"com.example.x\"\nversion = \"1\"\n\
                 [entrypoint]\nbin = \"bin/x\"\n\
                 [providers]\nmime = [\" Text/HTML \", \"text/html; charset=x\"]\n\
                 schemes = [\"HTTPS:\", \"javascript\", \"bad scheme\"]\n",
            )
            .expect("parses");
            assert_eq!(m.opens_mime, vec!["text/html"]);
            assert_eq!(m.opens_schemes, vec!["https"]);
        }
    }

    #[test]
    fn shipped_manifests_declare_what_they_need_to_open() {
        // Phase `shipped_browser_requires_semantic_continuity`.
        {
            let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(3)
                .expect("workspace root above crates/packaging/launcher");
            let manifest = read_manifest(&repo.join("apps/browser/tilo-app.toml"))
                .expect("shipped browser manifest parses through installed-app schema");
            assert_eq!(
                manifest.continuity.required,
                vec![ContinuityFacet::Presentation, ContinuityFacet::Semantic]
            );
        }
        // Phase `shipped_settings_does_not_require_model_services_to_open`.
        // Settings is the recovery surface for networking, accounts, displays,
        // and the rest of the OS.  A device without the optional local-model
        // stack must still be able to open it; the Agent Models pane handles an
        // unavailable backend in-app.
        {
            let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(3)
                .expect("workspace root above crates/packaging/launcher");
            let manifest = read_manifest(&repo.join("apps/settings/tilo-app.toml"))
                .expect("shipped Settings manifest parses through installed-app schema");

            for service in ["models", "model-gateway"] {
                let requirement = manifest
                    .sandbox
                    .services
                    .get(service)
                    .unwrap_or_else(|| panic!("Settings declares the {service} integration"));
                assert_eq!(
                    ServiceReq::parse(requirement).expect("valid service requirement"),
                    ServiceReq::Optional(1),
                    "Settings must open when {service} is not running"
                );
            }
        }
        // Phase `every_shipped_manifest_declares_a_kind_the_launcher_understands`.
        {
            // A typo'd kind is not a parse error — it just silently stops being an
            // app, invisible except as a missing icon.
            let known = ["app", "service", "plugin", "format-provider", "runtime"];
            let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps");
            let mut checked = 0;
            for entry in std::fs::read_dir(&root).expect("apps/ readable") {
                let path = entry.expect("dir entry").path().join("tilo-app.toml");
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
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn entrypoint_and_package_block_fields_parse() {
        // Phase `parses_a_minimal_manifest`.
        {
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
            assert_eq!(m.sandbox.fs.read, vec!["/usr/share/fonts".to_string()]);
            assert!(m.runtime.is_none());
            let req = ServiceReq::parse(m.sandbox.services.get("collab").unwrap()).unwrap();
            assert!(req.is_required());
            assert_eq!(req.version(), 1);
            assert!(m.cli_commands.is_empty(), "no [cli] block → empty commands");
        }
        // Phase `parses_a_cli_manifest`.
        {
            let toml_text = r#"
    package_id = "so.tilo.notes-cli"
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
            assert_eq!(
                m.cli_commands,
                vec!["notes".to_string(), "notes-import".to_string()]
            );
        }
        // Phase `parses_a_both_manifest_with_split_bins`.
        {
            let toml_text = r#"
    package_id = "so.tilo.notes"
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
        // Phase `package_kind_decodes_and_defaults_to_app`.
        {
            // A GUI entrypoint on a `service` is legal (the on-screen keyboard
            // draws, but the shell decides when to show it), so it survives decoding.
            let service = r#"
    [package]
    id      = "so.tilo.osk"
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

            // Omitted, blank, or oddly-cased all mean the ordinary case.
            for (src, why) in [
                ("", "a manifest with no kind is an app"),
                ("kind = \"\"\n", "a blank kind is an app"),
                ("kind = \"App\"\n", "kind is compared case-insensitively"),
                (
                    "kind = \" app \"\n",
                    "surrounding space is not part of the kind",
                ),
            ] {
                let toml_text = format!(
                    "[package]\nid = \"so.tilo.x\"\nversion = \"0.1.0\"\n{src}\n\
                     [entrypoint]\nbin = \"bin/x\"\n"
                );
                let m: MinimalManifest = toml::from_str(&toml_text).expect("parse");
                assert_eq!(m.kind, KIND_APP, "{why}");
            }
        }
        // Phase `parses_name_and_icon_from_package_block`.
        {
            // The human name and icon path both come off `[package]`, forwarded as
            // TILO_APP_NAME / TILO_APP_ICON.
            let toml_text = r#"
    [package]
    id      = "so.tilo.notes"
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
        // Phase `icon_is_none_when_absent_or_blank`.
        {
            // Omitted or whitespace-only means "no icon" — the toolkit draws its
            // generated default.
            let without = r#"
    [package]
    id      = "so.tilo.notes"
    version = "0.1.0"

    [entrypoint]
    bin = "bin/notes"
    "#;
            let m: MinimalManifest = toml::from_str(without).expect("parse");
            assert_eq!(m.icon, None);

            let blank = r#"
    [package]
    id      = "so.tilo.notes"
    icon    = "   "
    version = "0.1.0"

    [entrypoint]
    bin = "bin/notes"
    "#;
            let m: MinimalManifest = toml::from_str(blank).expect("parse");
            assert_eq!(m.icon, None, "a blank icon path is treated as absent");
        }
        // Phase `parses_runtime_field_from_package_block`.
        {
            let toml_text = r#"
    [package]
    id       = "so.tilo.hello"
    version  = "0.1.0"
    runtime  = "tilo-runtime/2026.07"

    [entrypoint]
    bin = "bin/tilo-hello"

    [sandbox.net]
    mode = "off"
    "#;
            let m: MinimalManifest = toml::from_str(toml_text).expect("parse");
            assert_eq!(m.package_id, "so.tilo.hello");
            assert_eq!(m.runtime.as_deref(), Some("tilo-runtime/2026.07"));
            let (name, ver) = m.runtime_ref().expect("runtime ref");
            assert_eq!(name, "tilo-runtime");
            assert_eq!(ver, "2026.07");
        }
    }

    #[test]
    fn service_and_portal_declarations_parse() {
        // Phase `service_req_parses_required_and_optional`.
        {
            assert_eq!(
                ServiceReq::parse("required@2").unwrap(),
                ServiceReq::Required(2)
            );
            assert_eq!(
                ServiceReq::parse("optional@7").unwrap(),
                ServiceReq::Optional(7)
            );
        }
        // Phase `service_req_rejects_malformed`.
        {
            assert!(ServiceReq::parse("required").is_err());
            assert!(ServiceReq::parse("maybe@1").is_err());
            assert!(ServiceReq::parse("required@abc").is_err());
        }
        // Phase `portals_table_parses_via_host_and_deny`.
        {
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
        // Phase `portals_table_empty_by_default`.
        {
            let toml_text = r#"
    package_id = "com.example.notes"
    version = "0.1.0"

    [entrypoint]
    bin = "bin/notes"
    "#;
            let m: MinimalManifest = toml::from_str(toml_text).expect("parse");
            assert!(m.sandbox.portals.is_empty());
        }
    }

    fn manifest_with(block: &str) -> MinimalManifest {
        let src = format!(
            "package_id = \"com.example.hostile\"\nversion = \"1\"\n\
             [entrypoint]\nbin = \"bin/x\"\n{block}"
        );
        toml::from_str(&src).expect("a manifest declaring fs paths still decodes")
    }

    #[test]
    fn manifests_may_name_no_filesystem_path() {
        // Phase `every_path_a_package_could_name_is_refused`.
        // The six shapes the original hole made possible, each refused by
        // naming the package and entry.
        {
            for entry in [
                "~",
                "/",
                "~/.ssh",
                "~/../../etc",
                "~/.local/share/so.tilo.totp",
                "$XDG_DATA_HOME/so.tilo.totp",
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
        // Phase `a_declared_read_is_refused_whether_or_not_the_profile_covers_it`.
        // A read is refused on the same terms — the read profile isn't a
        // filter to add to, an entry it already covers is refused too.
        {
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
        // Phase `a_manifest_that_names_no_path_passes`.
        // A manifest with no `[sandbox.fs]` block is the normal case and must
        // stay silent — an empty list is not a declaration.
        {
            let m = manifest_with("[sandbox.net]\nmode = \"off\"\n");
            assert!(enforce_fs_policy(&m).is_ok());
            let empty = manifest_with("[sandbox.fs]\nread = []\nwrite = []\n");
            assert!(
                enforce_fs_policy(&empty).is_ok(),
                "an explicitly empty block declares nothing"
            );
        }
        // Phase `no_shipped_manifest_names_a_path`.
        // Every manifest the OS ships goes through the same check as a
        // stranger's — no exemption.
        {
            let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(3)
                .expect("workspace root above crates/packaging/launcher");
            let apps = repo.join("apps");
            let mut checked = 0usize;
            for entry in std::fs::read_dir(&apps).expect("apps/ is readable") {
                let path = entry.expect("dir entry").path().join("tilo-app.toml");
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
}
