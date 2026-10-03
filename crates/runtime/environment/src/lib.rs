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
//! [`Environment::ADD_TOOL`]; `perm trust-dir` is [`Environment::GRANT_DIR`].
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

/// One environment.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Environment {
    /// What the person calls it.
    pub name: String,
    /// The `env-runtime` image (`tapp:<hex>`): the base of `/usr` and `/etc`.
    pub runtime: String,
    /// Tool images built for [`Self::runtime`], topmost last.
    pub tools: Vec<ToolRef>,
    /// `FsPath` (and `NetHost`) grants, subject [`RUNTIME_APP_ID`].
    pub grants: Json<Vec<CapOverride>>,
    /// Whether the home survives the session.
    pub home: HomePolicy,
}

/// A tool image in an environment.
#[derive(
    Model, Default, Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, JsonSchema,
)]
pub struct ToolRef {
    /// The tool image (`tapp:<hex>`).
    pub id: String,
    /// What the person asked for, e.g. `["pandoc"]`.
    pub packages: Vec<String>,
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
    /// Appends a tool image built for this environment's runtime. Adding an
    /// image the environment already has changes nothing.
    #[action]
    pub fn add_tool(
        &mut self,
        #[param("Tool image")] id: String,
        #[param("Packages it provides")] packages: Vec<String>,
    ) -> Result<(), String> {
        parse_image(&id)?;
        if packages.iter().all(|p| p.trim().is_empty()) {
            return Err("a tool provides at least one package".into());
        }
        if self.tools.iter().any(|tool| tool.id == id) {
            return Ok(());
        }
        self.tools.push(ToolRef { id, packages });
        Ok(())
    }

    /// Remove tool.
    #[action(destructive)]
    pub fn remove_tool(&mut self, #[param("Tool image")] id: String) -> Result<(), String> {
        let before = self.tools.len();
        self.tools.retain(|tool| tool.id != id);
        if self.tools.len() == before {
            return Err(format!("{id} is not in this environment"));
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
    /// Switches to another `env-runtime` together with every tool rebuilt for
    /// it, in one step: a tool is never mounted on a runtime it was not built
    /// for.
    #[action]
    pub fn set_runtime(
        &mut self,
        #[param("Runtime image")] runtime: String,
        #[param("Tools rebuilt for it")] tools: Vec<ToolRef>,
    ) -> Result<(), String> {
        parse_image(&runtime)?;
        for tool in &tools {
            parse_image(&tool.id)?;
        }
        if tools.len() != self.tools.len() {
            return Err(format!(
                "this environment has {} tools and {} were rebuilt; every tool moves \
                 to the new runtime or the runtime does not change",
                self.tools.len(),
                tools.len()
            ));
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

    /// A new environment on `runtime` with nothing added and nothing granted.
    pub fn new(name: &str, runtime: &PackageId) -> Self {
        Self {
            name: name.to_owned(),
            runtime: runtime.to_string(),
            ..Self::default()
        }
    }

    /// The runtime image.
    ///
    /// # Errors
    /// Returns the reason when the stored id is not a `tapp:<hex>` id.
    pub fn runtime_id(&self) -> Result<PackageId, String> {
        parse_image(&self.runtime)
    }

    /// The tool images, topmost first — the order their trees stack over the
    /// runtime.
    ///
    /// # Errors
    /// Returns the reason when a stored id is not a `tapp:<hex>` id.
    pub fn tool_ids_topmost_first(&self) -> Result<Vec<PackageId>, String> {
        self.tools
            .iter()
            .rev()
            .map(|t| parse_image(&t.id))
            .collect()
    }

    /// The tool that provides `package`, if one does.
    pub fn tool_providing(&self, package: &str) -> Option<&ToolRef> {
        self.tools
            .iter()
            .find(|tool| tool.packages.iter().any(|p| p == package))
    }
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
    },
}

impl Target {
    /// The default environment.
    pub fn default_environment() -> Self {
        Self::Environment { record: None }
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
        Environment::new("Work", &image(1))
            .write_record(&mut doc)
            .unwrap();
        doc.commit();
        Replica::from_doc(doc)
    }

    #[test]
    fn tools_are_added_once_and_removed_by_image() {
        let mut r = replica();
        r.apply(Environment::ADD_TOOL.with((image(2).to_string(), vec!["pandoc".into()])))
            .unwrap();
        r.apply(Environment::ADD_TOOL.with((image(2).to_string(), vec!["pandoc".into()])))
            .unwrap();
        r.apply(Environment::ADD_TOOL.with((image(3).to_string(), vec!["jq".into()])))
            .unwrap();
        let env = r.state().unwrap();
        assert_eq!(
            env.tools.len(),
            2,
            "adding the same image twice adds it once"
        );
        assert_eq!(env.tool_providing("jq").unwrap().id, image(3).to_string());
        assert_eq!(
            env.tool_ids_topmost_first().unwrap(),
            vec![image(3), image(2)],
            "the newest tool stacks on top"
        );

        assert!(
            r.apply(Environment::ADD_TOOL.with(("pandoc".into(), vec!["pandoc".into()])))
                .is_err(),
            "a tool is an image id, not a package name"
        );
        r.apply(Environment::REMOVE_TOOL.with(image(2).to_string()))
            .unwrap();
        assert!(
            r.apply(Environment::REMOVE_TOOL.with(image(2).to_string()))
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
    fn the_runtime_moves_only_with_every_tool_and_names_stay_non_empty() {
        let mut r = replica();
        r.apply(Environment::ADD_TOOL.with((image(2).to_string(), vec!["git".into()])))
            .unwrap();
        assert!(
            r.apply(Environment::SET_RUNTIME.with((image(9).to_string(), Vec::new())))
                .is_err(),
            "switching without the rebuilt tool would mount it on a runtime it was not built for"
        );
        let rebuilt = ToolRef {
            id: image(8).to_string(),
            packages: vec!["git".into()],
        };
        r.apply(Environment::SET_RUNTIME.with((image(9).to_string(), vec![rebuilt.clone()])))
            .unwrap();
        let env = r.state().unwrap();
        assert_eq!(env.runtime_id().unwrap(), image(9));
        assert_eq!(env.tools, vec![rebuilt]);

        assert!(r.apply(Environment::RENAME.with("  ".into())).is_err());
        r.apply(Environment::RENAME.with(" Docs ".into())).unwrap();
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
                record: Some("work".into())
            }
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
