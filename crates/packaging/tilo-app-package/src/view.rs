//! What a reader of an installed package takes from its manifest.
//!
//! The launcher, the shell, Settings, the install CLI and the relay all ask
//! the same few questions of a package — its id and name, what it opens,
//! how it starts, what its sandbox declares. [`ManifestView`] answers them,
//! and it is built only from the canonical [`Manifest`]: there is one
//! decoder of `manifest.toml`, so a field means the same thing to the
//! registry, the installer and the sandbox.
//!
//! In particular the package's identity is `[package].id`, the field the
//! signed root covers. A key the canonical schema does not define, such as
//! a top-level `package_id`, is never read: an archive may describe itself,
//! but it may not name itself something other than what was signed.
//!
//! The view keeps the launcher's shapes (strings for the network mode and
//! the service requirements, payload paths as strings) and normalises what
//! the desktop matches on: extensions, MIME types and schemes are trimmed,
//! lowercased and dropped when unsafe.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::manifest::Manifest;
pub use crate::manifest::{
    ContinuityContract, ContinuityFacet, ContinuitySemanticSchema, EntrypointType, PortalPolicy,
    Requirement, ServiceReq,
};
use crate::validate::{valid_extension, valid_mime, valid_scheme};

/// `[package].kind` for something a user opens. Everything else is a
/// system component, however it starts.
pub const KIND_APP: &str = "app";
/// `[package].kind` for a headless component the desktop starts and
/// supervises. Service packages are never promoted to ordinary app launchers.
pub const KIND_SERVICE: &str = "service";
/// The strict syscall profile, the one a sandbox gets unless it names
/// another.
pub const DEFAULT_SECCOMP_PROFILE: &str = "tilo-default";

/// A package's manifest as its readers use it, derived from the canonical
/// [`Manifest`]. Decoding (`toml::from_str`, [`ManifestView::from_toml_str`]
/// or [`read_manifest`]) goes through the canonical schema first.
#[derive(Debug, Clone, Deserialize)]
#[serde(try_from = "Manifest")]
pub struct ManifestView {
    /// `[package].id`, the signed identity.
    pub package_id: String,
    /// `[package].name` — the publisher's human-facing name. `None` falls
    /// back to deriving one from the package id.
    pub name: Option<String>,
    /// `[package.names]` — the name in other languages, by BCP 47 tag.
    pub localized_names: BTreeMap<String, String>,
    /// `[package.keywords]` — the other words the app is known by, every
    /// language's together: trimmed, de-duplicated and bounded.
    pub keywords: Vec<String>,
    /// `[package].category` — freedesktop-style menu category.
    pub category: Option<String>,
    /// Payload-relative icon path, exported to the app as `TILO_APP_ICON`.
    /// The signed schema has no icon field, so a decoded manifest never
    /// carries one and the app draws its generated default.
    pub icon: Option<String>,
    pub version: String,
    /// `[package].kind` — what the package *is*: `app` is something a user
    /// opens; `service`/`plugin`/`format-provider`/`runtime` are
    /// system-used. Separate from `Entrypoint::entry_type` since a
    /// package can draw and still not belong in a launcher (the on-screen
    /// keyboard).
    pub kind: String,
    /// Optional `[package].runtime` reference (`tilo-runtime/<version>`);
    /// when present the launcher mounts the bundle so the app doesn't need
    /// `--minimal-libs`. For a desktop build (`[target]`), the toolkit
    /// release linked into the module.
    pub runtime: Option<String>,
    /// `[package].optional-runtimes` — further runtime bundles
    /// (`<name>/<version>`) mounted read-only beside the runtime when they
    /// are installed and verify. A missing one never stops the launch.
    pub optional_runtimes: Vec<String>,
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
    /// `[providers].secondary`: another app claiming the same resource is
    /// its default.
    pub opens_secondary: bool,
    /// Signed minimum handoff-fidelity facets. Legacy manifests default to
    /// framework presentation state.
    pub continuity: ContinuityContract,
}

impl ManifestView {
    /// Decode manifest TOML through the canonical schema.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestReadError::Parse`] if `source` is not a canonical
    /// manifest, has no `[entrypoint]`, or is a desktop build for another
    /// host.
    pub fn from_toml_str(source: &str) -> Result<Self, ManifestReadError> {
        let manifest =
            Manifest::from_toml_str(source).map_err(|e| ManifestReadError::Parse(e.to_string()))?;
        Self::try_from(manifest)
    }

    /// Split `runtime = "tilo-runtime/<v>"` into `(name, version)`; `None`
    /// if unset or shaped wrong.
    pub fn runtime_ref(&self) -> Option<(&str, &str)> {
        let raw = self.runtime.as_deref()?;
        raw.split_once('/')
    }
}

impl TryFrom<Manifest> for ManifestView {
    type Error = ManifestReadError;

    fn try_from(manifest: Manifest) -> Result<Self, Self::Error> {
        let manifest = manifest.normalized();
        let runtime = if manifest.target.is_some() {
            // A desktop build names the host it runs on and the toolkit it
            // links; refuse one for another host rather than start it.
            crate::desktop::validate_native_host(&manifest)
                .map_err(|e| ManifestReadError::Parse(e.to_string()))?;
            crate::desktop::runtime_dependency(&manifest).map(str::to_owned)
        } else {
            manifest.package.runtime.clone()
        };
        let Manifest {
            package,
            continuity,
            sandbox,
            entrypoint,
            providers,
            cli,
            ..
        } = manifest;
        let entrypoint = entrypoint
            .ok_or_else(|| ManifestReadError::Parse("missing `[entrypoint]`".to_string()))?;
        let localized_names = package
            .names
            .into_iter()
            .map(|(tag, name)| (tag.trim().to_string(), name.trim().to_string()))
            .filter(|(tag, name)| !tag.is_empty() && !name.is_empty())
            .collect();
        let keywords = search_keywords(package.keywords);
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
        let opens_secondary = providers.secondary;
        let opens_schemes = providers
            .schemes
            .into_iter()
            .map(|scheme| scheme.trim().trim_end_matches(':').to_ascii_lowercase())
            .filter(|scheme| valid_scheme(scheme))
            .collect();
        Ok(ManifestView {
            package_id: package.id,
            name: package.name,
            localized_names,
            keywords,
            category: package.category,
            icon: None,
            version: package.version,
            kind: package.kind.as_str().to_string(),
            runtime,
            optional_runtimes: package.optional_runtimes,
            entrypoint: Entrypoint::from(entrypoint),
            sandbox: Sandbox::from(sandbox),
            cli_commands: cli.map(|c| c.commands).unwrap_or_default(),
            opens_ext,
            opens_mime,
            opens_schemes,
            opens_secondary,
            continuity,
        })
    }
}

/// Most keywords an app may declare, and the longest, so a manifest cannot
/// make the desktop's search index unbounded.
const MAX_KEYWORDS: usize = 96;
const MAX_KEYWORD_SCALARS: usize = 48;

/// Every language's keywords as one list, trimmed, without blanks or
/// repeats, in tag order.
fn search_keywords(by_language: BTreeMap<String, Vec<String>>) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    by_language
        .into_values()
        .flatten()
        .map(|keyword| keyword.trim().to_owned())
        .filter(|keyword| {
            !keyword.is_empty()
                && keyword.chars().count() <= MAX_KEYWORD_SCALARS
                && seen.insert(keyword.to_lowercase())
        })
        .take(MAX_KEYWORDS)
        .collect()
}

fn path_string(path: &Path) -> String {
    // TOML strings are UTF-8, so a decoded path always is.
    path.to_string_lossy().into_owned()
}

/// `[entrypoint]` with payload paths as strings.
#[derive(Debug, Clone)]
pub struct Entrypoint {
    /// Path of the entrypoint binary, relative to the payload root. Used
    /// verbatim for `"gui"`/`"cli"`; falls back for `"both"`.
    pub bin: String,
    /// Which flavour of entrypoint.
    pub r#type: EntrypointType,
    /// GUI-flavour binary path inside the payload. Only meaningful when
    /// `r#type = "both"`.
    pub gui_bin: Option<String>,
    /// CLI-flavour binary path inside the payload. Only meaningful when
    /// `r#type = "both"`.
    pub cli_bin: Option<String>,
}

impl From<crate::manifest::Entrypoint> for Entrypoint {
    fn from(entrypoint: crate::manifest::Entrypoint) -> Self {
        Self {
            bin: path_string(&entrypoint.bin),
            r#type: entrypoint.r#type,
            gui_bin: entrypoint.gui_bin.as_deref().map(path_string),
            cli_bin: entrypoint.cli_bin.as_deref().map(path_string),
        }
    }
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

/// `[sandbox]` as the launcher reads it.
#[derive(Debug, Clone)]
pub struct Sandbox {
    /// `[sandbox] seccomp = "…"` — the named syscall profile requested.
    /// `tilo-default` enforces write-xor-execute; `tilo-jit` is the same
    /// policy without that rule, for code genuinely compiled at run time.
    ///
    /// A **request**, not a setting: the launcher treats an unrecognised
    /// name as strict, and checks the weaker name against the image's
    /// entitlement list — a package writing a string about itself is not
    /// authority to lift a protection off itself.
    pub seccomp: String,
    pub net: Net,
    pub fs: Fs,
    /// Map of service-name -> requirement.
    pub services: BTreeMap<String, ServiceReq>,
    /// XDG portal policy per portal name. When at least one is `ViaHost`
    /// the launcher bind-mounts the host session D-Bus socket and sets
    /// `DBUS_SESSION_BUS_ADDRESS`; absent/empty means the bus is NOT exposed.
    pub portals: BTreeMap<String, PortalPolicy>,
}

/// Hand-written rather than derived so the default profile is the
/// named strict one, not an empty string.
impl Default for Sandbox {
    fn default() -> Self {
        Self {
            seccomp: DEFAULT_SECCOMP_PROFILE.to_string(),
            net: Net::default(),
            fs: Fs::default(),
            services: BTreeMap::new(),
            portals: BTreeMap::new(),
        }
    }
}

impl From<crate::manifest::Sandbox> for Sandbox {
    fn from(sandbox: crate::manifest::Sandbox) -> Self {
        Self {
            seccomp: sandbox.seccomp,
            net: Net {
                mode: sandbox.net.mode.as_str().to_string(),
                hosts: sandbox.net.hosts,
            },
            fs: Fs {
                read: sandbox.fs.read,
                write: sandbox.fs.write,
            },
            services: sandbox.services,
            portals: sandbox.portals,
        }
    }
}

/// `[sandbox.net]`.
#[derive(Debug, Clone)]
pub struct Net {
    /// - `"off"` → no network (`--unshare-all` with NO `--share-net`).
    /// - `"allowlist"` → only [`Net::hosts`], brokered through the
    ///   egress gate.
    /// - anything else → the host network is shared in.
    pub mode: String,
    /// Endpoints declared for `mode = "allowlist"`; the launcher's egress
    /// gate makes them the ceiling.
    pub hosts: Vec<String>,
}

impl Default for Net {
    fn default() -> Self {
        Self {
            mode: "off".to_string(),
            hosts: Vec::new(),
        }
    }
}

/// `[sandbox.fs]` — **retired**. Carried only so a manifest still
/// declaring it can be named in the launcher's refusal.
#[derive(Debug, Clone, Default)]
pub struct Fs {
    pub read: Vec<String>,
    pub write: Vec<String>,
}

/// Read and decode a manifest file.
///
/// # Errors
///
/// Returns [`ManifestReadError::Io`] if `path` cannot be read, or
/// [`ManifestReadError::Parse`] if its contents are not a valid manifest.
pub fn read_manifest(path: &Path) -> Result<ManifestView, ManifestReadError> {
    let source = std::fs::read_to_string(path).map_err(|e| ManifestReadError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    ManifestView::from_toml_str(&source)
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestReadError {
    #[error("reading manifest at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("manifest parse error: {0}")]
    Parse(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A canonical manifest: `package` is the body of `[package]`, `tail`
    /// any further tables. `[sandbox.net]` is `off` unless `tail` sets it.
    fn source(package: &str, tail: &str) -> String {
        let net = if tail.contains("[sandbox.net]") {
            ""
        } else {
            "[sandbox.net]\nmode = \"off\"\n"
        };
        format!(
            "[package]\n{package}\n\n[publisher]\nidentity = \"ed25519:{}\"\n\n\
             [sandbox]\nnamespaces = []\nseccomp = \"tilo-default\"\nlandlock = \"strict\"\n\
             {net}\n{tail}\n",
            "00".repeat(32)
        )
    }

    fn app(id: &str, tail: &str) -> ManifestView {
        let package = format!("id = \"{id}\"\nversion = \"0.1.0\"\nkind = \"app\"");
        ManifestView::from_toml_str(&source(&package, tail))
            .unwrap_or_else(|e| panic!("{id} decodes: {e}"))
    }

    fn repo() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .expect("workspace root above crates/packaging/tilo-app-package")
            .to_path_buf()
    }

    #[test]
    fn the_identity_is_the_signed_package_id_never_a_top_level_alias() {
        // A publisher owning `com.evil` signs `[package] id = "com.evil.app"`
        // and adds a top-level `package_id` naming the Store. The signature
        // covers only the canonical fields, the registry and the installer
        // see `com.evil.app`, and so must every reader of the installed
        // manifest: the sandbox grants the Store's install door by id.
        let confused = format!(
            "package_id = \"so.tilo.store\"\nversion = \"9.9.9\"\n{}",
            source(
                "id = \"com.evil.app\"\nversion = \"0.1.0\"\nkind = \"app\"",
                "[entrypoint]\nbin = \"bin/x\"\n[sandbox.services]\napp-host = \"required@1\"",
            )
        );
        let view = ManifestView::from_toml_str(&confused).expect("decodes");
        assert_eq!(view.package_id, "com.evil.app");
        assert_eq!(view.version, "0.1.0");
        let parsed: ManifestView = toml::from_str(&confused).expect("serde decodes");
        assert_eq!(parsed.package_id, "com.evil.app");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("manifest.toml");
        std::fs::write(&path, &confused).unwrap();
        assert_eq!(read_manifest(&path).unwrap().package_id, "com.evil.app");

        // The top-level form alone is not a manifest at all.
        let legacy = "package_id = \"so.tilo.store\"\nversion = \"1\"\n\
                      [entrypoint]\nbin = \"bin/x\"\n";
        assert!(ManifestView::from_toml_str(legacy).is_err());
        assert!(toml::from_str::<ManifestView>(legacy).is_err());
    }

    #[test]
    fn a_package_without_an_entrypoint_is_not_a_launchable_view() {
        let service = source(
            "id = \"so.tilo.daemon\"\nversion = \"0.1.0\"\nkind = \"service\"",
            "",
        );
        assert!(Manifest::from_toml_str(&service).is_ok());
        assert!(ManifestView::from_toml_str(&service).is_err());
    }

    #[test]
    fn shipped_system_apps_carry_their_german_names() {
        for (app, german) in [
            ("file-explorer", "Dateien"),
            ("settings", "Persönliche Einstellungen"),
            ("notes", "Notizen"),
        ] {
            let manifest = read_manifest(&repo().join("apps").join(app).join("tilo-app.toml"))
                .unwrap_or_else(|e| panic!("{app}: {e}"));
            assert_eq!(
                manifest.localized_names.get("de").map(String::as_str),
                Some(german),
                "{app}"
            );
        }
    }

    #[test]
    fn every_shipped_manifest_decodes_with_a_kind_readers_understand() {
        // A typo'd kind must not silently stop a package being an app.
        let known = ["app", "service", "plugin", "format-provider", "runtime"];
        let mut checked = 0;
        for entry in std::fs::read_dir(repo().join("apps")).expect("apps/ readable") {
            let path = entry.expect("dir entry").path().join("tilo-app.toml");
            if !path.exists() {
                continue;
            }
            let m = read_manifest(&path)
                .unwrap_or_else(|e| panic!("{} does not decode: {e}", path.display()));
            assert!(
                known.contains(&m.kind.as_str()),
                "{} declares kind = {:?}, which is not one of {known:?}",
                path.display(),
                m.kind,
            );
            checked += 1;
        }
        assert!(
            checked >= 40,
            "expected the shipped app fleet, found {checked}"
        );
    }

    #[test]
    fn opener_claims_are_read_and_normalised() {
        // The shipped manifests are the source of truth for which app opens
        // which file type, so the decode must survive a *real* one.
        let m = read_manifest(&repo().join("apps/word-processor/tilo-app.toml"))
            .expect("the word processor's manifest decodes");
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

        // A package that opens nothing says so as an empty list, not a
        // decode failure — most manifests have no `[providers]` block.
        let m = app("com.example.x", "[entrypoint]\nbin = \"bin/x\"");
        assert!(m.opens_ext.is_empty());
        assert_eq!(
            m.continuity.required,
            vec![ContinuityFacet::Presentation],
            "manifests without [continuity] retain the framework presentation baseline"
        );

        // Case and a leading dot are packaging noise, not meaning.
        let m = app(
            "com.example.x",
            "[entrypoint]\nbin = \"bin/x\"\n[providers]\next = [\".ODT\", \" Md \", \"\"]",
        );
        assert_eq!(m.opens_ext, vec!["odt".to_string(), "md".to_string()]);

        let m = app(
            "com.example.x",
            "[entrypoint]\nbin = \"bin/x\"\n\
             [providers]\nmime = [\" Text/HTML \", \"text/html; charset=x\"]\n\
             schemes = [\"HTTPS:\", \"javascript\", \"bad scheme\"]",
        );
        assert_eq!(m.opens_mime, vec!["text/html"]);
        assert_eq!(m.opens_schemes, vec!["https"]);
    }

    #[test]
    fn entrypoint_sandbox_and_package_fields_decode() {
        let m = app(
            "com.example.notes",
            "[entrypoint]\nbin = \"bin/notes\"\n\
             [sandbox.fs]\nread = [\"/usr/share/fonts\"]\nwrite = []\n\
             [sandbox.services]\ncollab = \"required@1\"\nfs = \"optional@2\"\nmodels = \"required\"",
        );
        assert_eq!(m.package_id, "com.example.notes");
        assert_eq!(m.kind, KIND_APP);
        assert_eq!(m.entrypoint.bin, "bin/notes");
        assert_eq!(m.entrypoint.r#type, EntrypointType::Gui);
        assert_eq!(m.sandbox.net.mode, "off");
        assert_eq!(m.sandbox.seccomp, DEFAULT_SECCOMP_PROFILE);
        assert_eq!(m.sandbox.fs.read, vec!["/usr/share/fonts".to_string()]);
        assert_eq!(
            m.sandbox
                .services
                .get("collab")
                .map(|r| (r.requirement, r.version)),
            Some((Requirement::Required, 1))
        );
        assert_eq!(
            m.sandbox
                .services
                .get("fs")
                .map(|r| (r.requirement, r.version)),
            Some((Requirement::Optional, 2))
        );
        assert_eq!(
            m.sandbox
                .services
                .get("models")
                .map(|r| (r.requirement, r.version)),
            Some((Requirement::Required, 1)),
            "the canonical schema pins an unversioned requirement to 1"
        );
        assert!(m.runtime.is_none());
        assert!(m.cli_commands.is_empty(), "no [cli] block → empty commands");
        assert!(m.icon.is_none());

        let m = app(
            "so.tilo.notes-cli",
            "[entrypoint]\nbin = \"bin/notes\"\ntype = \"cli\"\n\
             [cli]\ncommands = [\"notes\", \"notes-import\"]",
        );
        assert_eq!(m.entrypoint.r#type, EntrypointType::Cli);
        assert_eq!(m.entrypoint.cli_bin(), Some("bin/notes"));
        assert_eq!(m.entrypoint.gui_bin(), None);
        assert_eq!(m.cli_commands, vec!["notes", "notes-import"]);

        let m = app(
            "so.tilo.notes",
            "[entrypoint]\ntype = \"both\"\ngui-bin = \"bin/notes-gui\"\ncli-bin = \"bin/notes\"\n\
             [cli]\ncommands = [\"notes\"]",
        );
        assert_eq!(m.entrypoint.r#type, EntrypointType::Both);
        assert_eq!(m.entrypoint.gui_bin(), Some("bin/notes-gui"));
        assert_eq!(m.entrypoint.cli_bin(), Some("bin/notes"));

        // A GUI entrypoint on a `service` is legal (the on-screen keyboard
        // draws, but the shell decides when to show it).
        let osk = ManifestView::from_toml_str(&source(
            "id = \"so.tilo.osk\"\nversion = \"0.1.0\"\nkind = \"service\"",
            "[entrypoint]\nbin = \"bin/osk\"\ntype = \"gui\"",
        ))
        .unwrap();
        assert_eq!(osk.kind, KIND_SERVICE);
        assert_eq!(osk.entrypoint.gui_bin(), Some("bin/osk"));

        let m = ManifestView::from_toml_str(&source(
            "id = \"so.tilo.browser\"\nname = \"Browser\"\nversion = \"0.1.0\"\nkind = \"app\"\n\
             runtime = \"tilo-runtime/2026.07\"\n\
             optional-runtimes = [\"tilo-runtime/2026.10.0-chromium\"]",
            "[entrypoint]\nbin = \"bin/browser\"\n[sandbox.net]\nmode = \"allowlist\"\n\
             hosts = [\"example.org\"]\n\
             [sandbox.portals]\nfile-chooser = \"via-host\"\nnotifications = \"deny\"",
        ))
        .unwrap();
        assert_eq!(m.name.as_deref(), Some("Browser"));
        assert_eq!(m.runtime_ref(), Some(("tilo-runtime", "2026.07")));
        assert_eq!(m.optional_runtimes, ["tilo-runtime/2026.10.0-chromium"]);
        assert_eq!(m.sandbox.net.mode, "allowlist");
        assert_eq!(m.sandbox.net.hosts, ["example.org"]);
        assert_eq!(
            m.sandbox.portals.get("file-chooser").copied(),
            Some(PortalPolicy::ViaHost)
        );
        assert_eq!(
            m.sandbox.portals.get("notifications").copied(),
            Some(PortalPolicy::Deny)
        );
        assert!(
            app("so.tilo.x", "[entrypoint]\nbin = \"bin/x\"")
                .sandbox
                .portals
                .is_empty()
        );
    }

    #[test]
    fn the_kind_is_the_canonical_one_and_is_never_guessed() {
        // The kind decides whether a package is offered to a person at all,
        // so a manifest that omits it or misspells it is refused instead of
        // being read as an app.
        for kind in ["", "kind = \"\"", "kind = \"App\"", "kind = \" app \""] {
            let text = source(
                &format!("id = \"so.tilo.x\"\nversion = \"0.1.0\"\n{kind}"),
                "[entrypoint]\nbin = \"bin/x\"",
            );
            assert!(
                ManifestView::from_toml_str(&text).is_err(),
                "{kind:?} is not a kind"
            );
        }
    }
    #[test]
    fn keywords_of_every_language_decode_trimmed_unique_and_bounded() {
        let package = "id = \"so.example.tube\"\nversion = \"0.1.0\"\nkind = \"app\"";
        let long = "x".repeat(MAX_KEYWORD_SCALARS + 1);
        let tail = format!(
            "[entrypoint]\nbin = \"bin/x\"\n[package.keywords]\nen = [\" YouTube \", \"video\", \"\", \"{long}\"]\n\
             de = [\"Video\", \"filme\"]\n"
        );
        // `[package.keywords]` belongs to `[package]`; the helper puts `tail`
        // after the other tables, which TOML allows.
        let view = ManifestView::from_toml_str(&source(package, &tail)).expect("decodes");
        assert_eq!(
            view.keywords,
            ["Video", "filme", "YouTube"],
            "in tag order, a repeat dropped"
        );
        let none = ManifestView::from_toml_str(&source(package, "[entrypoint]\nbin = \"bin/x\"\n"))
            .expect("decodes");
        assert!(none.keywords.is_empty());
    }

    #[test]
    fn every_app_has_english_and_german_keywords_and_no_copy_in_its_manifest() {
        let apps = repo().join("apps");
        let mut checked = 0;
        for entry in std::fs::read_dir(&apps).expect("apps/") {
            let dir = entry.expect("entry").path();
            let Ok(text) = std::fs::read_to_string(dir.join("tilo-app.toml")) else {
                continue;
            };
            let keywords = crate::search_keywords::read(&dir)
                .unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
            for language in ["en", "de"] {
                assert!(
                    keywords
                        .get(language)
                        .is_some_and(|words| !words.is_empty()),
                    "{} lacks search/{language}.ftl",
                    dir.display()
                );
            }
            // One source: the words are joined at pack time, never committed.
            let view = ManifestView::from_toml_str(&text)
                .unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
            assert!(
                view.keywords.is_empty(),
                "{} declares [package.keywords]; the words live in search/*.ftl",
                dir.display()
            );
            checked += 1;
        }
        assert!(checked >= 40, "only {checked} apps checked");
    }
}
