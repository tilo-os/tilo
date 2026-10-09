//! The environment record: where a shell runs.
//!
//! An environment is a synced record (`#[resource(kind = "environment")]`)
//! declared by the Terminal app. It names the read-only images that make up
//! the guest's `/usr` and `/etc` — one `env-runtime` and the `tool` images
//! built for it — the directories the guest was granted, and whether its home
//! outlives a session. `ush` builds the guest from it; nothing else about a
//! guest is configured anywhere. See `docs/app-model/environments.md`.
//!
//! The record is the only package manifest there is: `pkg install` builds a
//! tool image through a [`ToolBuilder`] and appends it with
//! [`Environment::ADD_TOOL`]; `perm trust-dir` is [`Environment::GRANT_DIR`]
//! and `perm allow-host` is [`Environment::ALLOW_HOST`].
//! History and undo come from the record layer.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::collections::BTreeMap;
use std::path::{Component, Path};
use std::str::FromStr;

use collab_types::caps::{CapOverride, Effect, Principal, Scope, Subject, Verb};
pub use tilo_app_package::PackageId;
use tilo_resource::{Json, JsonSchema, Model, ModelError, resource};

/// The app id every environment grant is written against — an app in the
/// capability vocabulary, because that is what the permissions panel renders
/// and revokes.
pub const RUNTIME_APP_ID: &str = "so.tilo.ush";

/// The app whose records environments are.
pub const OWNER_APP_ID: &str = "so.tilo.terminal";

/// The record key of the environment a session gets when it names none.
pub const DEFAULT_RECORD: &str = "default";

/// The name a freshly created default environment carries.
pub const DEFAULT_NAME: &str = "Default";

/// One environment. Architecture-neutral: a node uses the images for its own
/// architecture, so an environment made on an `x86_64` laptop opens on an
/// `aarch64` relay.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Environment {
    /// What the person calls it.
    pub name: String,
    /// The `env-runtime` release: the base of `/usr` and `/etc`.
    pub runtime: RuntimeRef,
    /// Tools built for [`Self::runtime`], topmost last.
    pub tools: Vec<ToolRef>,
    /// `FsPath` and `NetHost` grants, subject [`RUNTIME_APP_ID`].
    pub grants: Json<Vec<CapOverride>>,
    /// Whether the home survives the session.
    pub home: HomePolicy,
}

/// An `env-runtime` release and its image per architecture.
#[derive(
    Model, Default, Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, JsonSchema,
)]
pub struct RuntimeRef {
    /// The release, e.g. `2026.10`: the same userland on every architecture.
    pub release: String,
    /// The `env-runtime` image (`tapp:<hex>`) per architecture
    /// (`x86_64`, `aarch64`).
    pub images: BTreeMap<String, String>,
}

/// A tool in an environment and its image per architecture.
#[derive(
    Model, Default, Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, JsonSchema,
)]
pub struct ToolRef {
    /// What the person asked for, e.g. `["pandoc"]`.
    pub packages: Vec<String>,
    /// The tool image (`tapp:<hex>`) per architecture, built for the
    /// runtime's image of that architecture.
    pub images: BTreeMap<String, String>,
}

/// What happens to an environment's home when a session ends.
#[derive(Model, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomePolicy {
    /// One home per environment, kept in the `ush` store.
    #[default]
    Persistent,
    /// A fresh home per session, removed when it ends.
    Ephemeral,
}

#[resource(kind = "environment", label = "Environment")]
impl Environment {
    /// Add tool.
    ///
    /// Records the image built for `packages` on `arch`: a new tool, or the
    /// image an existing tool was missing for that architecture.
    #[action]
    pub fn add_tool(
        &mut self,
        #[param("Packages it provides")] packages: Vec<String>,
        #[param("Architecture")] arch: String,
        #[param("Tool image")] image: String,
    ) -> Result<(), String> {
        parse_image(&image)?;
        check_arch(&arch)?;
        let packages: Vec<String> = packages
            .into_iter()
            .map(|p| p.trim().to_owned())
            .filter(|p| !p.is_empty())
            .collect();
        if packages.is_empty() {
            return Err("a tool provides at least one package".into());
        }
        if let Some(tool) = self
            .tools
            .iter_mut()
            .find(|tool| same_packages(&tool.packages, &packages))
        {
            tool.images.insert(arch, image);
            return Ok(());
        }
        self.tools.push(ToolRef {
            packages,
            images: BTreeMap::from([(arch, image)]),
        });
        Ok(())
    }

    /// Remove tool.
    #[action(destructive)]
    pub fn remove_tool(
        &mut self,
        #[param("Packages it provides")] packages: Vec<String>,
    ) -> Result<(), String> {
        let before = self.tools.len();
        self.tools
            .retain(|tool| !same_packages(&tool.packages, &packages));
        if self.tools.len() == before {
            return Err(format!(
                "no tool provides exactly {} in this environment",
                packages.join(", ")
            ));
        }
        Ok(())
    }

    /// Grant directory.
    ///
    /// Binds a host directory into the guest at the same path, read-write or
    /// read-only. Granting a directory again replaces the earlier grant.
    #[action]
    pub fn grant_dir(
        &mut self,
        #[param("Directory")] path: String,
        #[param("Writable")] write: bool,
    ) -> Result<(), String> {
        let grant = dir_grant(Path::new(&path), write)?;
        self.grants.0.retain(|c| c.id != grant.id);
        self.grants.0.push(grant);
        Ok(())
    }

    /// Allow host.
    ///
    /// Lets the guest resolve and connect to a host: `example.com`, a
    /// domain and everything under it (`*.example.com`), or `*` for any
    /// host. Allowing a host again replaces the earlier grant.
    #[action]
    pub fn allow_host(&mut self, #[param("Host")] host: String) -> Result<(), String> {
        let grant = host_grant(&host)?;
        self.grants.0.retain(|c| c.id != grant.id);
        self.grants.0.push(grant);
        Ok(())
    }

    /// Revoke grant.
    #[action(destructive)]
    pub fn revoke_grant(&mut self, #[param("Grant id")] id: String) -> Result<(), String> {
        let before = self.grants.0.len();
        self.grants.0.retain(|c| c.id != id);
        if self.grants.0.len() == before {
            return Err(format!("no grant with id {id}"));
        }
        Ok(())
    }

    /// Set runtime.
    ///
    /// Switches to another `env-runtime` release together with every tool
    /// rebuilt for it, in one step: a tool is never mounted on a runtime it
    /// was not built for.
    #[action]
    pub fn set_runtime(
        &mut self,
        #[param("Runtime release")] runtime: RuntimeRef,
        #[param("Tools rebuilt for it")] tools: Vec<ToolRef>,
    ) -> Result<(), String> {
        check_images(&runtime.images)?;
        if runtime.release.trim().is_empty() {
            return Err("a runtime names its release".into());
        }
        for tool in &tools {
            check_images(&tool.images)?;
        }
        let rebuilt_all = tools.len() == self.tools.len()
            && self.tools.iter().all(|old| {
                tools
                    .iter()
                    .any(|new| same_packages(&old.packages, &new.packages))
            });
        if !rebuilt_all {
            return Err(
                "every tool moves to the new runtime, or the runtime does not change".into(),
            );
        }
        self.runtime = runtime;
        self.tools = tools;
        Ok(())
    }

    /// Rename.
    #[action]
    pub fn rename(&mut self, #[param("Name")] name: String) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("an environment needs a name".into());
        }
        name.clone_into(&mut self.name);
        Ok(())
    }
}

impl Environment {
    /// Write this environment as the whole of a fresh record document — how
    /// a record is created; every later change is an action.
    ///
    /// # Errors
    /// Returns the document's write error.
    pub fn write_record(&self, doc: &mut tilo_resource::Doc) -> Result<(), ModelError> {
        tilo_resource::Fields::write_fields(self, doc, &automerge::ROOT)
    }

    /// A new environment on the runtime `release`, whose image for `arch` is
    /// `image`, with nothing added and nothing granted.
    pub fn new(name: &str, release: &str, arch: &str, image: &PackageId) -> Self {
        Self {
            name: name.to_owned(),
            runtime: RuntimeRef {
                release: release.to_owned(),
                images: BTreeMap::from([(arch.to_owned(), image.to_string())]),
            },
            ..Self::default()
        }
    }

    /// The runtime image for `arch`.
    ///
    /// # Errors
    /// Returns the reason when the release has no image for `arch` or the
    /// stored id is not a `tapp:<hex>` id.
    pub fn runtime_image(&self, arch: &str) -> Result<PackageId, String> {
        let id = self.runtime.images.get(arch).ok_or_else(|| {
            format!(
                "runtime release {} has no image for {arch}",
                self.runtime.release
            )
        })?;
        parse_image(id)
    }

    /// The tool that provides `package`, if one does.
    pub fn tool_providing(&self, package: &str) -> Option<&ToolRef> {
        self.tools
            .iter()
            .find(|tool| tool.packages.iter().any(|p| p == package))
    }
}

impl ToolRef {
    /// The tool's image for `arch`: `None` when it has not been built there
    /// yet.
    ///
    /// # Errors
    /// Returns the reason when the stored id is not a `tapp:<hex>` id.
    pub fn image(&self, arch: &str) -> Result<Option<PackageId>, String> {
        self.images.get(arch).map(|id| parse_image(id)).transpose()
    }
}

/// The architecture this node runs, as the record names it.
pub fn host_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    }
}

/// The architectures an image may be recorded for — the spelling of
/// `tilo_app_package::manifest::Arch`.
pub const ARCHES: &[&str] = &["x86_64", "aarch64"];

fn check_arch(arch: &str) -> Result<(), String> {
    if ARCHES.contains(&arch) {
        Ok(())
    } else {
        Err(format!(
            "{arch:?} is not an architecture images are built for"
        ))
    }
}

fn check_images(images: &BTreeMap<String, String>) -> Result<(), String> {
    if images.is_empty() {
        return Err("an image is recorded for at least one architecture".into());
    }
    for (arch, id) in images {
        check_arch(arch)?;
        parse_image(id)?;
    }
    Ok(())
}

/// Whether two package lists name the same packages, in any order.
pub fn same_packages(a: &[String], b: &[String]) -> bool {
    let set = |list: &[String]| -> std::collections::BTreeSet<String> {
        list.iter().map(|p| p.trim().to_owned()).collect()
    };
    set(a) == set(b)
}

/// Parse an image id as the record stores it.
///
/// # Errors
/// Returns the reason when `id` is not a `tapp:<hex>` id.
pub fn parse_image(id: &str) -> Result<PackageId, String> {
    PackageId::from_str(id).map_err(|e| format!("{id:?} is not an image id: {e}"))
}

/// Guest trees a grant may not reach into: the images' read-only `/usr` and
/// `/etc`, the guest's pseudo-filesystems and the runtime's commands.
/// Binding there would replace part of the guest rather than add a
/// directory to it.
const GUEST_OWNED_TREES: &[&str] = &[
    "/usr", "/etc", "/proc", "/dev", "/bin", "/sbin", "/lib", "/lib64", "/run/ush",
];

/// Guest directories a grant may sit below but not replace: the fresh
/// scratch mounts, the environment's own `/var` state and the parent of its
/// home. `/run/media/…`, `/tmp/build` or `/var/lib/data` are ordinary grants.
const GUEST_OWNED_DIRS: &[&str] = &[
    "/tmp",
    "/run",
    "/var",
    "/var/tmp",
    "/var/lib",
    "/var/cache",
    "/home",
];

/// Whether a host directory may be granted: absolute, without `..`, not the
/// root, and not a path the guest builds itself (see [`GUEST_OWNED_TREES`]
/// and [`GUEST_OWNED_DIRS`]).
pub fn is_grantable(path: &Path) -> bool {
    let Some(raw) = path.to_str() else {
        return false;
    };
    if raw.trim().is_empty() || !path.is_absolute() || raw == "/" {
        return false;
    }
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return false;
    }
    let path = Path::new(raw.trim_end_matches('/'));
    !GUEST_OWNED_TREES
        .iter()
        .any(|owned| path.starts_with(owned))
        && !GUEST_OWNED_DIRS
            .iter()
            .any(|owned| path == Path::new(owned))
}

/// The grant id for a directory: stable, so granting it again replaces one
/// record instead of adding another.
pub fn grant_id(path: &Path) -> String {
    // FNV-1a: only has to be deterministic and short enough to read.
    let mut hasher = fnv::FnvHasher::default();
    std::hash::Hasher::write(&mut hasher, path.as_os_str().as_encoded_bytes());
    format!("ush-grant-{:016x}", std::hash::Hasher::finish(&hasher))
}

/// The capability that grants `path` — exactly what the compositor writes
/// for "Always allow" on a filesystem prompt, against [`RUNTIME_APP_ID`].
///
/// # Errors
/// Returns the reason when `path` is not [`is_grantable`].
pub fn dir_grant(path: &Path, write: bool) -> Result<CapOverride, String> {
    if !is_grantable(path) {
        return Err(format!(
            "{} cannot be granted: it is the root, relative, or a path the guest \
             builds itself (/usr, /etc, /proc, /dev, /run/ush, or /tmp, /run, /var, \
             /var/lib, /var/cache, /home themselves)",
            path.display()
        ));
    }
    Ok(CapOverride::grant(
        Subject::Principal(Principal::App(RUNTIME_APP_ID.to_owned())),
        Scope::FsPath(path.to_string_lossy().into_owned()),
        if write { Verb::Write } else { Verb::Read },
    )
    .with_id(grant_id(path))
    .granted_by(Principal::Person("local".to_owned())))
}

/// A host as a `NetHost` grant spells it: `*`, `example.com`, or
/// `.example.com` for the domain and everything under it (`*.example.com`
/// is accepted and written that way). Lower-case, no trailing dot. The
/// grammar is [`collab_types::caps::NetHostPattern`]'s, the one apps' grants
/// are read with too.
///
/// # Errors
/// Returns the reason when `host` is not a host name: empty, an address
/// (egress is decided by name, so a literal would bypass the resolver that
/// decides), or characters DNS names do not carry.
pub fn normalize_host(host: &str) -> Result<String, String> {
    collab_types::caps::NetHostPattern::parse(host)
        .map(|pattern| pattern.to_string())
        .map_err(|reason| reason.to_string())
}

/// The grant id for a host: stable, so allowing it again replaces one
/// record instead of adding another.
pub fn host_grant_id(host: &str) -> String {
    let mut hasher = fnv::FnvHasher::default();
    std::hash::Hasher::write(&mut hasher, host.as_bytes());
    format!("ush-host-{:016x}", std::hash::Hasher::finish(&hasher))
}

/// The capability that lets the guest reach `host` — a `NetHost` grant
/// against [`RUNTIME_APP_ID`], what the permissions panel shows and revokes.
///
/// # Errors
/// Returns the reason when `host` is not a host (see [`normalize_host`]).
pub fn host_grant(host: &str) -> Result<CapOverride, String> {
    let host = normalize_host(host)?;
    Ok(CapOverride::grant(
        Subject::Principal(Principal::App(RUNTIME_APP_ID.to_owned())),
        Scope::NetHost(host.clone()),
        Verb::Read,
    )
    .with_id(host_grant_id(&host))
    .granted_by(Principal::Person("local".to_owned())))
}

/// Whether `cap` is a grant this record should carry: a `Grant` for
/// [`RUNTIME_APP_ID`]. A `Restrict` written elsewhere is honoured by the
/// evaluator, but never introduces a path.
pub fn is_runtime_grant(cap: &CapOverride) -> bool {
    cap.effect == Effect::Grant
        && cap.subject == Subject::Principal(Principal::App(RUNTIME_APP_ID.to_owned()))
}

/// Whether `key` may name an environment record: a short single path
/// component of `[A-Za-z0-9._-]`, never `.` or `..`. Keys arrive from apps, so
/// this is the whole of what an app can say about where a record lives.
pub fn is_record_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && !key.chars().all(|c| c == '.')
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// Where a session runs. Carried by `ush`'s `open-session` request.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Target {
    /// The machine's own login shell.
    Host,
    /// A guest built from an environment record; `None` is the default
    /// environment, created on first use.
    Environment {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        record: Option<String>,
        /// Where the guest runs: this device unless it names a relay.
        #[serde(default, skip_serializing_if = "Location::is_this_device")]
        location: Location,
    },
}

/// Where an environment's guest runs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Location {
    /// On the device that asks.
    #[default]
    ThisDevice,
    /// On a relay the person is a member of, named by its endpoint id
    /// (hex). The device's supervisor carries the pty there.
    Relay { relay: String },
}

impl Location {
    pub fn is_this_device(&self) -> bool {
        matches!(self, Self::ThisDevice)
    }

    /// Whether a relay's name is one a request may carry: a 32-byte endpoint
    /// id in lowercase hex.
    pub fn is_valid(&self) -> bool {
        match self {
            Self::ThisDevice => true,
            Self::Relay { relay } => {
                relay.len() == 64
                    && relay
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            }
        }
    }
}

impl Target {
    /// The default environment, on this device.
    pub fn default_environment() -> Self {
        Self::Environment {
            record: None,
            location: Location::ThisDevice,
        }
    }
}

impl Default for Target {
    fn default() -> Self {
        Self::default_environment()
    }
}

/// Why a tool could not be built.
#[derive(Debug, thiserror::Error)]
pub enum ToolBuildError {
    /// This device cannot build tools (no builder is installed).
    #[error("building tools is not available here: {0}")]
    Unavailable(String),
    /// A package name did not resolve against the runtime.
    #[error("no package named {0} for this runtime")]
    UnknownPackage(String),
    /// The build ran and failed.
    #[error("the tool build failed: {0}")]
    Failed(String),
}

/// Builds `tool` images: the files a set of packages adds on top of one
/// `env-runtime`.
///
/// The cache key is `(runtime id, sorted package NEVRAs)`; a hit returns an
/// existing image id. A miss builds in a sandbox whose `/usr` is the runtime
/// with a writable upper, seals the upper into a `tool` tapp naming
/// `runtime`, signs and stores it. The returned id must be materialisable by
/// the package store on this device.
pub trait ToolBuilder: Send + Sync {
    /// Build (or find) the tool image adding `packages` to `runtime`.
    /// Progress — the package manager's output — goes to `progress`, which
    /// reaches the shell that asked.
    ///
    /// # Errors
    /// Returns [`ToolBuildError`] when the packages do not resolve or the
    /// build fails.
    fn build(
        &self,
        runtime: &PackageId,
        packages: &[String],
        progress: &mut dyn std::io::Write,
    ) -> Result<PackageId, ToolBuildError>;
}

/// The [`ToolBuilder`] for a device without one.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoToolBuilder;

impl ToolBuilder for NoToolBuilder {
    fn build(
        &self,
        _runtime: &PackageId,
        _packages: &[String],
        _progress: &mut dyn std::io::Write,
    ) -> Result<PackageId, ToolBuildError> {
        Err(ToolBuildError::Unavailable(
            "this build of Tilo has no tool builder yet, so `pkg install` cannot add \
             packages; the tools the environment already has keep working"
                .into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_resource::Replica;

    fn image(byte: u8) -> PackageId {
        PackageId::from_bytes([byte; 32])
    }

    fn replica() -> Replica<Environment> {
        let mut doc = tilo_resource::Doc::new();
        Environment::new("Work", "2026.10", "x86_64", &image(1))
            .write_record(&mut doc)
            .unwrap();
        doc.commit();
        Replica::from_doc(doc)
    }

    fn add(packages: &[&str], arch: &str, id: PackageId) -> tilo_resource::Invoke<Environment> {
        Environment::ADD_TOOL.with((
            packages.iter().map(|p| (*p).to_owned()).collect(),
            arch.to_owned(),
            id.to_string(),
        ))
    }

    #[test]
    fn tools_carry_one_image_per_architecture() {
        let mut r = replica();
        r.apply(add(&["pandoc"], "x86_64", image(2))).unwrap();
        r.apply(add(&["pandoc"], "aarch64", image(3))).unwrap();
        r.apply(add(&["jq"], "x86_64", image(4))).unwrap();
        let env = r.state().unwrap();
        assert_eq!(
            env.tools.len(),
            2,
            "a second architecture's build is the same tool"
        );
        let pandoc = env.tool_providing("pandoc").unwrap();
        assert_eq!(pandoc.image("x86_64").unwrap(), Some(image(2)));
        assert_eq!(pandoc.image("aarch64").unwrap(), Some(image(3)));
        let jq = env.tool_providing("jq").unwrap();
        assert_eq!(jq.image("aarch64").unwrap(), None, "not built there yet");
        assert_eq!(env.runtime_image("x86_64").unwrap(), image(1));
        assert!(env.runtime_image("aarch64").is_err());

        assert!(r.apply(add(&["pandoc"], "x86_64", image(2))).is_ok());
        assert!(
            r.apply(Environment::ADD_TOOL.with((
                vec!["pandoc".into()],
                "x86_64".into(),
                "pandoc".into()
            )))
            .is_err(),
            "a tool image is an image id, not a package name"
        );
        assert!(r.apply(add(&["pandoc"], "riscv", image(5))).is_err());

        r.apply(Environment::REMOVE_TOOL.with(vec!["pandoc".into()]))
            .unwrap();
        assert!(
            r.apply(Environment::REMOVE_TOOL.with(vec!["pandoc".into()]))
                .is_err()
        );
        assert_eq!(r.state().unwrap().tools.len(), 1);
    }

    #[test]
    fn directory_grants_are_capabilities_and_refuse_guest_paths() {
        let mut r = replica();
        r.apply(Environment::GRANT_DIR.with(("/var/home/you/src".into(), true)))
            .unwrap();
        r.apply(Environment::GRANT_DIR.with(("/var/home/you/src".into(), false)))
            .unwrap();
        let env = r.state().unwrap();
        assert_eq!(env.grants.0.len(), 1, "granting again replaces the grant");
        let cap = &env.grants.0[0];
        assert!(is_runtime_grant(cap));
        assert_eq!(cap.verb, Verb::Read);
        assert_eq!(cap.scope, Scope::FsPath("/var/home/you/src".into()));

        for bad in [
            "/",
            "/usr/bin",
            "/etc",
            "/var/lib",
            "/run/ush",
            "/tmp",
            "/tmp/",
            "/home",
            "relative",
            "/a/../etc",
        ] {
            assert!(
                r.apply(Environment::GRANT_DIR.with((bad.into(), true)))
                    .is_err(),
                "{bad} must not be grantable"
            );
        }
        for good in [
            "/tmp/build",
            "/run/media/you/usb",
            "/home/you/src",
            "/var/lib/data",
            "/srv/x",
        ] {
            assert!(is_grantable(Path::new(good)), "{good} should be grantable");
        }
        r.apply(Environment::REVOKE_GRANT.with(cap.id.clone()))
            .unwrap();
        assert!(r.state().unwrap().grants.0.is_empty());
        assert!(
            r.apply(Environment::REVOKE_GRANT.with(cap.id.clone()))
                .is_err()
        );
    }

    #[test]
    fn host_grants_are_capabilities_in_the_shared_spelling() {
        let mut r = replica();
        r.apply(Environment::ALLOW_HOST.with("GitHub.com."))
            .unwrap();
        r.apply(Environment::ALLOW_HOST.with("*.crates.io"))
            .unwrap();
        r.apply(Environment::ALLOW_HOST.with("github.com")).unwrap();
        let env = r.state().unwrap();
        assert_eq!(env.grants.0.len(), 2, "allowing again replaces the grant");
        let scopes: Vec<&Scope> = env.grants.0.iter().map(|c| &c.scope).collect();
        assert_eq!(
            scopes,
            [
                &Scope::NetHost(".crates.io".into()),
                &Scope::NetHost("github.com".into())
            ]
        );
        assert!(env.grants.0.iter().all(is_runtime_grant));
        // The record's spelling is the evaluator's: a leading dot covers
        // the domain and everything under it.
        assert!(collab_types::caps::host_covers(
            ".crates.io",
            "static.crates.io"
        ));

        assert_eq!(normalize_host(" * ").unwrap(), "*");
        for bad in ["", "1.1.1.1", "::1", "[::1]", "a b", "-x.com", "x..com"] {
            assert!(
                r.apply(Environment::ALLOW_HOST.with(bad)).is_err(),
                "{bad:?} must not be allowed"
            );
        }
        let id = host_grant_id("github.com");
        r.apply(Environment::REVOKE_GRANT.with(id)).unwrap();
        assert_eq!(r.state().unwrap().grants.0.len(), 1);
    }

    #[test]
    fn the_runtime_moves_only_with_every_tool_and_names_stay_non_empty() {
        let mut r = replica();
        r.apply(add(&["git"], "x86_64", image(2))).unwrap();
        let release = RuntimeRef {
            release: "2027.04".into(),
            images: BTreeMap::from([("x86_64".into(), image(9).to_string())]),
        };
        assert!(
            r.apply(Environment::SET_RUNTIME.with((release.clone(), Vec::new())))
                .is_err(),
            "switching without the rebuilt tool would mount it on a runtime it was not built for"
        );
        let rebuilt = ToolRef {
            packages: vec!["git".into()],
            images: BTreeMap::from([("x86_64".into(), image(8).to_string())]),
        };
        r.apply(Environment::SET_RUNTIME.with((release, vec![rebuilt.clone()])))
            .unwrap();
        let env = r.state().unwrap();
        assert_eq!(env.runtime.release, "2027.04");
        assert_eq!(env.runtime_image("x86_64").unwrap(), image(9));
        assert_eq!(env.tools, vec![rebuilt]);

        assert!(r.apply(Environment::RENAME.with("  ")).is_err());
        r.apply(Environment::RENAME.with(" Docs ")).unwrap();
        assert_eq!(r.state().unwrap().name, "Docs");
        assert_eq!(r.state().unwrap().home, HomePolicy::Persistent);
    }

    #[test]
    fn targets_and_record_keys_are_what_an_app_can_say() {
        let json = serde_json::to_string(&Target::default_environment()).unwrap();
        assert_eq!(json, r#"{"kind":"environment"}"#);
        let named: Target =
            serde_json::from_str(r#"{"kind":"environment","record":"work"}"#).unwrap();
        assert_eq!(
            named,
            Target::Environment {
                record: Some("work".into()),
                location: Location::ThisDevice,
            }
        );
        let relay = "ab".repeat(32);
        let on_relay: Target = serde_json::from_str(&format!(
            r#"{{"kind":"environment","record":"work","location":{{"kind":"relay","relay":"{relay}"}}}}"#
        ))
        .unwrap();
        assert!(matches!(
            &on_relay,
            Target::Environment { location: Location::Relay { relay: r }, .. } if *r == relay
        ));
        assert!(
            serde_json::to_string(&Target::default_environment())
                .unwrap()
                .find("location")
                .is_none(),
            "this device is the default and is not spelled out"
        );
        assert!(Location::Relay { relay }.is_valid());
        assert!(
            !Location::Relay {
                relay: "home".into()
            }
            .is_valid()
        );
        assert_eq!(
            serde_json::from_str::<Target>(r#"{"kind":"host"}"#).unwrap(),
            Target::Host
        );
        for good in ["default", "a1b2", "work-env_2"] {
            assert!(is_record_key(good), "{good}");
        }
        for bad in ["", ".", "..", "a/b", "../x", &"x".repeat(65)] {
            assert!(!is_record_key(bad), "{bad}");
        }
    }

    #[test]
    fn without_a_builder_installing_says_why() {
        let err = NoToolBuilder
            .build(&image(1), &["pandoc".into()], &mut std::io::sink())
            .unwrap_err();
        assert!(matches!(err, ToolBuildError::Unavailable(_)), "{err}");
    }
}
