//! What an app outside the Tilo workspace needs to build the code the
//! workspace tests: the SDK's `[patch]` set, its lockfile, toolchain, target
//! flags and profiles. All of it is derived from the SDK's own workspace files,
//! read from the checkout the app builds against or, for a git SDK, embedded in
//! this CLI at the commit the dependency is pinned to. An app on the published
//! SDK needs only the toolchain, target flags and profiles: its crates name
//! their dependencies by version and carry no `[patch]`.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// The public repository a scaffold outside any checkout builds against.
pub const DEFAULT_SDK_GIT: &str = "https://code.tilo.so/tilo/tilo.git";

/// Everything a developer with rustup runs to get this CLI.
pub const INSTALL_COMMAND: &str =
    "cargo install --locked --git https://code.tilo.so/tilo/tilo.git tilo-app-cli";

/// The Tilo commit this CLI was built from, when the build could see it.
pub const BUILT_FROM_REV: Option<&str> = option_env!("TILO_SDK_REV");

const EMBEDDED_MANIFEST: &str = include_str!("../../../../Cargo.toml");
const EMBEDDED_LOCK: &str = include_str!("../../../../Cargo.lock");
const EMBEDDED_TOOLCHAIN: &str = include_str!("../../../../rust-toolchain.toml");
const EMBEDDED_CARGO_CONFIG: &str = include_str!("../../../../.cargo/config.toml");
const EMBEDDED_SDK_MANIFEST: &str = include_str!("../../../sdk/tilo-app/Cargo.toml");

/// The crates an app links: the SDK and the shared toolkit a thin build uses.
const SDK_ROOTS: [&str; 2] = ["tilo-app", "runtime-dylib"];
/// Profiles an app build selects. `thin-app` inherits `release`.
const APP_PROFILES: [&str; 4] = ["dev", "test", "release", "thin-app"];
/// hakari's feature union spans the whole monorepo; an app uses the
/// placeholder crate instead.
const UNPATCHED: [&str; 1] = ["workspace-hack"];

/// Where a standalone app gets the SDK.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SdkSource {
    /// A Tilo checkout, as the path from the app directory to its root.
    Checkout(PathBuf),
    /// A Tilo repository, pinned to `rev` or following its default branch.
    Git { url: String, rev: Option<String> },
    /// The published SDK: `tilo-app` from crates.io, or whatever registry
    /// Cargo's source replacement names, at this version requirement.
    Registry { version: String },
}

impl SdkSource {
    /// The TOML inline-table body that points a dependency at `member`, a
    /// crate directory relative to the Tilo root.
    fn locate(&self, member: &str) -> String {
        match self {
            Self::Checkout(root) => format!("path = {:?}", root.join(member).to_string_lossy()),
            Self::Git { url, rev } => match rev {
                Some(rev) => format!("git = {url:?}, rev = {rev:?}"),
                None => format!("git = {url:?}"),
            },
            Self::Registry { version } => format!("version = {version:?}"),
        }
    }

    /// Whether the app builds on published crates, which bring no `[patch]`,
    /// lockfile or shared toolkit dylib.
    #[must_use]
    pub fn is_registry(&self) -> bool {
        matches!(self, Self::Registry { .. })
    }

    /// One line for humans: where the SDK comes from.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Checkout(root) => format!("checkout {}", root.display()),
            Self::Git {
                url,
                rev: Some(rev),
            } => format!("{url} at {rev}"),
            Self::Git { url, rev: None } => format!("{url} (default branch)"),
            Self::Registry { version } => format!("tilo-app {version} from the registry"),
        }
    }
}

/// The SDK workspace files a standalone app mirrors.
#[derive(Debug, Clone)]
pub struct Baseline {
    manifest: toml::Table,
    lock: String,
    toolchain: toml::Table,
    cargo_config: toml::Table,
    sdk_version: Option<String>,
}

impl Baseline {
    /// The files of the commit this CLI was built from.
    ///
    /// # Errors
    ///
    /// Returns an error if an embedded workspace file does not parse.
    pub fn embedded() -> Result<Self> {
        Self::parse(
            EMBEDDED_MANIFEST,
            EMBEDDED_LOCK.to_owned(),
            EMBEDDED_TOOLCHAIN,
            EMBEDDED_CARGO_CONFIG,
            EMBEDDED_SDK_MANIFEST,
        )
    }

    /// The files of the checkout at `root`. A file the checkout lacks falls
    /// back to this CLI's copy.
    ///
    /// # Errors
    ///
    /// Returns an error if a workspace file does not parse.
    pub fn from_checkout(root: &Path) -> Result<Self> {
        let read = |relative: &str, embedded: &'static str| {
            std::fs::read_to_string(root.join(relative)).unwrap_or_else(|_| embedded.to_owned())
        };
        Self::parse(
            &read("Cargo.toml", EMBEDDED_MANIFEST),
            read("Cargo.lock", EMBEDDED_LOCK),
            &read("rust-toolchain.toml", EMBEDDED_TOOLCHAIN),
            &read(".cargo/config.toml", EMBEDDED_CARGO_CONFIG),
            &read("crates/sdk/tilo-app/Cargo.toml", EMBEDDED_SDK_MANIFEST),
        )
    }

    fn parse(
        manifest: &str,
        lock: String,
        toolchain: &str,
        cargo_config: &str,
        sdk_manifest: &str,
    ) -> Result<Self> {
        let manifest = toml::from_str(manifest).context("parsing the SDK workspace Cargo.toml")?;
        let toolchain = toml::from_str(toolchain).context("parsing the SDK rust-toolchain.toml")?;
        let cargo_config =
            toml::from_str(cargo_config).context("parsing the SDK .cargo/config.toml")?;
        let sdk_manifest: toml::Table =
            toml::from_str(sdk_manifest).context("parsing the tilo-app Cargo.toml")?;
        let sdk_version = sdk_manifest
            .get("package")
            .and_then(|package| package.get("version"))
            .and_then(toml::Value::as_str)
            .map(str::to_owned);
        Ok(Self {
            manifest,
            lock,
            toolchain,
            cargo_config,
            sdk_version,
        })
    }

    /// The version of `tilo-app` these files belong to.
    #[must_use]
    pub fn sdk_version(&self) -> Option<&str> {
        self.sdk_version.as_deref()
    }

    fn locked_packages(&self) -> Vec<toml::Value> {
        toml::from_str::<toml::Table>(&self.lock)
            .ok()
            .and_then(|mut lock| lock.remove("package"))
            .and_then(|packages| match packages {
                toml::Value::Array(packages) => Some(packages),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// Every package name reachable from the SDK in the lockfile. Versions are
    /// ignored, so this can only over-approximate.
    fn sdk_closure(packages: &[toml::Value]) -> BTreeSet<String> {
        let mut reached = BTreeSet::new();
        let mut pending: Vec<String> = SDK_ROOTS.iter().map(|root| (*root).to_owned()).collect();
        while let Some(name) = pending.pop() {
            if !reached.insert(name.clone()) {
                continue;
            }
            let dependencies = packages
                .iter()
                .filter(|package| package.get("name").and_then(toml::Value::as_str) == Some(&name))
                .filter_map(|package| package.get("dependencies").and_then(toml::Value::as_array))
                .flatten()
                .filter_map(toml::Value::as_str)
                .filter_map(|dependency| dependency.split(' ').next());
            pending.extend(dependencies.map(str::to_owned));
        }
        reached
    }
}

/// A scaffold outside the Tilo workspace.
#[derive(Debug, Clone)]
pub struct Standalone {
    pub source: SdkSource,
    pub baseline: Baseline,
}

impl Standalone {
    /// The `[dependencies]` entries for the SDK and, from a checkout or git,
    /// the optional shared toolkit.
    #[must_use]
    pub fn dependencies(&self) -> String {
        if self.source.is_registry() {
            return format!(
                "ui = {{ package = \"tilo-app\", {} }}\n",
                self.source.locate("crates/sdk/tilo-app")
            );
        }
        format!(
            "ui = {{ package = \"tilo-app\", {} }}\nruntime-dylib = {{ {}, optional = true }}\n",
            self.source.locate("crates/sdk/tilo-app"),
            self.source.locate("crates/runtime/runtime-dylib"),
        )
    }

    /// The `[patch.crates-io]` and `[profile.*]` tables that follow the
    /// dependencies. `[patch]` in the SDK workspace does not reach an app that
    /// depends on it, so each patch the SDK's graph uses is repeated here.
    ///
    /// # Errors
    ///
    /// Returns an error if the tables cannot be serialised.
    pub fn manifest_tail(&self) -> Result<String> {
        let packages = self.baseline.locked_packages();
        let closure = Baseline::sdk_closure(&packages);
        let mut patches = Vec::new();
        let registry = self.source.is_registry();
        let workspace_patches = self
            .baseline
            .manifest
            .get("patch")
            .and_then(|patch| patch.get("crates-io"))
            .and_then(toml::Value::as_table);
        for (name, patch) in workspace_patches.into_iter().flatten() {
            if registry || UNPATCHED.contains(&name.as_str()) || !closure.contains(name) {
                continue;
            }
            let location = match patch.get("path").and_then(toml::Value::as_str) {
                Some(path) => self.source.locate(path),
                None => match git_patch(patch) {
                    Some(location) => location,
                    None => continue,
                },
            };
            patches.push(format!("{name} = {{ {location} }}\n"));
        }
        let mut tail = String::new();
        if !patches.is_empty() {
            tail.push_str(
                "\n# The forks the SDK is built and tested with; see Tilo's vendor/. Cargo warns\n# about one your features do not reach yet. Keep it for when they do.\n[patch.crates-io]\n",
            );
            tail.extend(patches);
        }
        let profiles = self.profiles(&closure);
        if !profiles.is_empty() {
            let mut table = toml::Table::new();
            table.insert("profile".to_owned(), toml::Value::Table(profiles));
            tail.push_str(
                "\n# Tilo's own profiles. `thin-app` builds against the toolkit the OS ships.\n",
            );
            tail.push_str(&toml::to_string(&table).context("rendering app profiles")?);
        }
        Ok(tail)
    }

    fn profiles(&self, closure: &BTreeSet<String>) -> toml::Table {
        let Some(workspace) = self
            .baseline
            .manifest
            .get("profile")
            .and_then(toml::Value::as_table)
        else {
            return toml::Table::new();
        };
        let mut profiles = toml::Table::new();
        for name in APP_PROFILES {
            let Some(mut profile) = workspace.get(name).and_then(toml::Value::as_table).cloned()
            else {
                continue;
            };
            if let Some(toml::Value::Table(packages)) = profile.get_mut("package") {
                packages.retain(|package, _| package == "*" || closure.contains(package));
                if packages.is_empty() {
                    profile.remove("package");
                }
            }
            profiles.insert(name.to_owned(), toml::Value::Table(profile));
        }
        profiles
    }

    /// The files a standalone app carries besides its sources.
    ///
    /// # Errors
    ///
    /// Returns an error if a generated file cannot be serialised.
    pub fn files(&self) -> Result<Vec<(PathBuf, String)>> {
        let mut files = vec![
            (PathBuf::from("rust-toolchain.toml"), self.toolchain()?),
            (PathBuf::from(".cargo/config.toml"), self.cargo_config()?),
            (PathBuf::from(".gitignore"), "/target\n".to_owned()),
        ];
        // The workspace's lockfile names its own path crates; published crates
        // resolve from the registry on the first build.
        if !self.source.is_registry() {
            files.insert(0, (PathBuf::from("Cargo.lock"), self.baseline.lock.clone()));
        }
        Ok(files)
    }

    /// The SDK's compiler. A thin app links the toolkit dylib the OS ships,
    /// whose symbols are only stable for the compiler that built it.
    fn toolchain(&self) -> Result<String> {
        let mut toolchain = toml::Table::new();
        if let Some(channel) = self
            .baseline
            .toolchain
            .get("toolchain")
            .and_then(|table| table.get("channel"))
        {
            toolchain.insert("channel".to_owned(), channel.clone());
        }
        toolchain.insert(
            "components".to_owned(),
            toml::Value::Array(vec!["rustfmt".into(), "clippy".into()]),
        );
        toolchain.insert("profile".to_owned(), "minimal".into());
        let mut document = toml::Table::new();
        document.insert("toolchain".to_owned(), toml::Value::Table(toolchain));
        Ok(format!(
            "# The compiler Tilo is built with; thin apps must match it.\n{}",
            toml::to_string(&document).context("rendering rust-toolchain.toml")?
        ))
    }

    /// The SDK workspace's target flags, without its repository-local runner
    /// and job settings, and its C compiler hardening. Apps then compile for the
    /// CPU floor and hardening the OS image uses.
    fn cargo_config(&self) -> Result<String> {
        let mut targets = toml::Table::new();
        let workspace_targets = self
            .baseline
            .cargo_config
            .get("target")
            .and_then(toml::Value::as_table);
        for (target, settings) in workspace_targets.into_iter().flatten() {
            let Some(rustflags) = settings.get("rustflags") else {
                continue;
            };
            let mut kept = toml::Table::new();
            kept.insert("rustflags".to_owned(), rustflags.clone());
            targets.insert(target.clone(), toml::Value::Table(kept));
        }
        let mut document = toml::Table::new();
        document.insert("target".to_owned(), toml::Value::Table(targets));
        if let Some(env) = self.baseline.cargo_config.get("env") {
            document.insert("env".to_owned(), env.clone());
        }
        Ok(format!(
            "# Tilo's target flags: the CPU floor and hardening the OS image is built with.\n{}",
            toml::to_string(&document).context("rendering .cargo/config.toml")?
        ))
    }
}

/// A `[patch]` fork pinned in git, repeated as the workspace pins it.
fn git_patch(patch: &toml::Value) -> Option<String> {
    let url = patch.get("git").and_then(toml::Value::as_str)?;
    let mut location = format!("git = {url:?}");
    for key in ["rev", "tag", "branch"] {
        if let Some(value) = patch.get(key).and_then(toml::Value::as_str) {
            let _ = write!(location, ", {key} = {value:?}");
        }
    }
    Some(location)
}

/// Whether `path` lies in Cargo's own git or registry cache, which Cargo may
/// delete. `cargo install --git` builds this CLI from there.
#[must_use]
pub fn in_cargo_cache(path: &Path) -> bool {
    let components: Vec<_> = path
        .components()
        .map(std::path::Component::as_os_str)
        .collect();
    components.windows(2).any(|pair| {
        matches!(pair, [first, second]
            if (*first == "git" && *second == "checkouts") || (*first == "registry" && *second == "src"))
    })
}
