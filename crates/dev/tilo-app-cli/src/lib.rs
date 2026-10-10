//! App scaffolding used by the `tilo-app` command.
//!
//! Rendering is deliberately pure: tests can assert every generated file,
//! while the command performs one checked write phase only after all inputs
//! have been validated.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::{self, BufRead as _, BufReader, IsTerminal, Write as _};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};

pub mod android;
pub mod capability_usage;
pub mod ci;
pub mod contract;
mod dev_reload;
pub mod examples;
mod font_assets;
mod json;
pub mod native_libs;
pub mod native_sdk;
mod pack;
pub mod portability;
mod resource_kinds;
pub mod size;
pub mod standalone;
pub mod surface;
pub mod targets;
pub mod web;

const APP_AGENT_GUIDE: &str = include_str!("../templates/AGENTS.md");
const STARTER_APP_TEMPLATE: &str = include_str!("../templates/starter.rs");
const DOCUMENT_APP_TEMPLATE: &str = include_str!("../templates/document.rs");
const COLLECTION_APP_TEMPLATE: &str = include_str!("../templates/collection.rs");
const GRAPH_APP_TEMPLATE: &str = include_str!("../templates/graph.rs");
const STARTER_APP_CATALOG: &str = include_str!("../templates/starter.ftl");
const DOCUMENT_APP_CATALOG: &str = include_str!("../templates/document.ftl");
const COLLECTION_APP_CATALOG: &str = include_str!("../templates/collection.ftl");
const GRAPH_APP_CATALOG: &str = include_str!("../templates/graph.ftl");
const STARTER_APP_TESTS: &str = include_str!("../templates/tests/starter.rs");
const DOCUMENT_APP_TESTS: &str = include_str!("../templates/tests/document.rs");
const COLLECTION_APP_TESTS: &str = include_str!("../templates/tests/collection.rs");
const GRAPH_APP_TESTS: &str = include_str!("../templates/tests/graph.rs");
const GRAPH_SCREEN_TESTS: &str = include_str!("../templates/tests/graph_screen.rs");
const NATIVE_APP_TESTS: &str = include_str!("../templates/tests/native.rs");

/// A starting point `tilo-app new` scaffolds and the author then owns.
///
/// Templates are few and small: each is the least code that is a correct Tilo
/// app of one shape. Programs that teach one concept are examples
/// (`tilo-app examples`), not templates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Template {
    Starter,
    Document,
    Collection,
    Graph,
}

impl Template {
    /// Every template, in the order `--list-templates` and the prompt show.
    pub const ALL: [Self; 4] = [Self::Starter, Self::Document, Self::Collection, Self::Graph];

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Starter => "starter",
            Self::Document => "document",
            Self::Collection => "collection",
            Self::Graph => "graph",
        }
    }

    #[must_use]
    pub fn description(self) -> &'static str {
        match self {
            Self::Starter => "One resource, one action, one bound field: the smallest correct app",
            Self::Document => "One collaborative document with a live preview",
            Self::Collection => "A list of items the user adds, edits, checks off and removes",
            Self::Graph => "Serialized, live-editable screens above controls bound to the resource",
        }
    }

    fn catalog(self) -> &'static str {
        match self {
            Self::Starter => STARTER_APP_CATALOG,
            Self::Document => DOCUMENT_APP_CATALOG,
            Self::Collection => COLLECTION_APP_CATALOG,
            Self::Graph => GRAPH_APP_CATALOG,
        }
    }
}

/// The templates, as `tilo-app new --list-templates --json` reports them.
#[must_use]
pub fn templates_json() -> serde_json::Value {
    Template::ALL
        .iter()
        .map(|template| {
            serde_json::json!({
                "name": template.name(),
                "description": template.description(),
                "default": *template == Template::Starter,
            })
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScreenTemplate {
    #[default]
    Blank,
    Form,
    Settings,
    EmptyState,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ComponentTemplate {
    /// A reusable typed-props heading and detail view.
    #[default]
    Display,
    /// A reusable form with explicit cloneable local state.
    Form,
    /// A source-agnostic virtualized tree with typed configuration.
    Tree,
    /// A source-agnostic sparse variable-height list with retained delegates.
    List,
    /// A source-agnostic sparse table with app-defined columns and cells.
    Table,
    /// Advanced text with shared style, polygon flow, and replaceable layout.
    Text,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GraphPropsContract {
    DisplayV1,
    FormV2,
}

impl GraphPropsContract {
    fn for_template(template: ComponentTemplate) -> Option<Self> {
        match template {
            ComponentTemplate::Display => Some(Self::DisplayV1),
            ComponentTemplate::Form => Some(Self::FormV2),
            ComponentTemplate::Tree
            | ComponentTemplate::List
            | ComponentTemplate::Table
            | ComponentTemplate::Text => None,
        }
    }

    fn invalid_props(self, props: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
        match self {
            Self::DisplayV1 => ["title", "detail"].into_iter().find_map(|field| {
                (!props.get(field).is_some_and(serde_json::Value::is_string))
                    .then(|| format!("requires string prop {field:?}"))
            }),
            Self::FormV2 => ["value_path", "enabled_path"]
                .into_iter()
                .find_map(|field| {
                    props
                        .get(field)
                        .and_then(serde_json::Value::as_str)
                        .is_none_or(|path| path.trim().is_empty())
                        .then(|| format!("requires non-empty string prop {field:?}"))
                }),
        }
    }
}

#[derive(Debug)]
pub struct NewAppRequest {
    pub name: Option<String>,
    pub app_id: Option<String>,
    pub title: Option<String>,
    /// `None` prompts in an interactive terminal and defaults to `Starter`
    /// for deterministic non-interactive use.
    pub template: Option<Template>,
    /// Copy this example instead of rendering a template.
    pub example: Option<&'static examples::Example>,
    pub directory: Option<PathBuf>,
    /// Explicit toolkit crate. `None` discovers `TILO_UI_PATH`, a surrounding
    /// checkout, the running executable's checkout, or the build checkout.
    pub ui_path: Option<PathBuf>,
    /// Build a standalone app against this Tilo git repository instead of a
    /// checkout. `None` uses the public repository when no checkout is found.
    pub sdk_git: Option<String>,
    /// Commit of the SDK repository. `None` pins the commit this CLI was
    /// built from.
    pub sdk_rev: Option<String>,
    /// Build a standalone app on the published SDK: `tilo-app` at this
    /// version requirement from the registry, with no `[patch]`. An empty
    /// string means the version this CLI was built with.
    pub sdk_version: Option<String>,
    pub dry_run: bool,
}

enum SdkRequest {
    Discover,
    Checkout(PathBuf),
    Git {
        url: Option<String>,
        rev: Option<String>,
    },
    Registry(Option<String>),
}

impl NewAppRequest {
    fn sdk_request(&self) -> SdkRequest {
        if self.ui_path.is_none()
            && let Some(version) = &self.sdk_version
        {
            let version = version.trim();
            return SdkRequest::Registry((!version.is_empty()).then(|| version.to_owned()));
        }
        match (&self.ui_path, &self.sdk_git, &self.sdk_rev) {
            (Some(ui), _, _) => SdkRequest::Checkout(ui.clone()),
            (None, None, None) => SdkRequest::Discover,
            (None, url, rev) => SdkRequest::Git {
                url: url.clone(),
                rev: rev.clone(),
            },
        }
    }
}

#[derive(Debug)]
pub struct CreateOutcome {
    pub destination: PathBuf,
    pub package_name: String,
    pub app_id: String,
    pub files: Vec<PathBuf>,
    pub dry_run: bool,
    pub workspace_member: bool,
    /// Where a standalone app gets the SDK; `None` for a workspace member.
    pub sdk: Option<standalone::SdkSource>,
    /// Whether the app became its own git repository. Only an app created
    /// outside every git work tree gets one.
    pub git_initialized: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AddArtifact {
    Screen {
        name: String,
        title: Option<String>,
        template: ScreenTemplate,
        /// Existing screen that receives a generated push-navigation button.
        link_from: Option<String>,
    },
    Scenario {
        name: String,
        /// Registered screen used to derive a runnable interaction sequence.
        screen: Option<String>,
    },
    Locale {
        locale: String,
    },
    Component {
        name: String,
        template: ComponentTemplate,
    },
    /// A graph-compatible typed component plus its first instance.
    GraphComponent {
        name: String,
        template: ComponentTemplate,
        mount: ComponentMount,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComponentMount {
    pub screen: String,
    pub id: Option<String>,
    pub parent: String,
    /// `None` appends to the parent.
    pub index: Option<usize>,
    /// One JSON object passed to the generated typed props adapter.
    pub props: String,
}

impl ComponentMount {
    /// Create an append-to-root mount with useful typed-display placeholder
    /// props derived from the component name.
    #[must_use]
    pub fn display(screen: impl Into<String>, component_name: &str) -> Self {
        Self {
            screen: screen.into(),
            id: None,
            parent: "root".into(),
            index: None,
            props: serde_json::json!({
                "title": title_from_name(component_name),
                "detail": "Start building here.",
            })
            .to_string(),
        }
    }

    /// Create an append-to-root form mount bound to component-namespaced root
    /// document paths.
    #[must_use]
    pub fn form(screen: impl Into<String>, component_name: &str) -> Self {
        Self {
            screen: screen.into(),
            id: None,
            parent: "root".into(),
            index: None,
            props: serde_json::json!({
                "value_path": format!("{component_name}-value"),
                "enabled_path": format!("{component_name}-enabled"),
            })
            .to_string(),
        }
    }
}

#[derive(Debug)]
pub struct AddRequest {
    /// App directory or any file/directory below it.
    pub start: PathBuf,
    pub artifact: AddArtifact,
    pub force: bool,
    pub dry_run: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct AddOutcome {
    pub app_dir: PathBuf,
    /// Primary artifact followed by any generated source files that changed.
    pub files: Vec<PathBuf>,
    /// Subset of `files` that existed and were replaced.
    pub updated: Vec<PathBuf>,
    pub dry_run: bool,
    /// A command that exercises the generated artifact immediately.
    pub next_command: Option<String>,
}

#[derive(Debug)]
pub struct LocaleSyncRequest {
    /// App directory or any file/directory below it.
    pub start: PathBuf,
    /// One locale to synchronize, or every non-source catalog when absent.
    pub locale: Option<String>,
    /// Remove entries, attributes, and values no longer present in `en-US`.
    pub prune: bool,
    /// Reset only patterns whose external variable set no longer matches the source.
    pub reset_variables: bool,
    pub dry_run: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct LocaleSyncOutcome {
    pub app_dir: PathBuf,
    pub checked: Vec<String>,
    pub files: Vec<PathBuf>,
    /// Contract differences deliberately preserved by the selected options.
    pub unresolved: Vec<String>,
    pub dry_run: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DoctorStatus {
    Pass,
    Warning,
    Fail,
}

#[derive(Debug, PartialEq, Eq)]
pub struct DoctorCheck {
    pub status: DoctorStatus,
    pub label: &'static str,
    pub detail: String,
}

#[derive(Debug)]
pub struct DoctorReport {
    pub app_dir: PathBuf,
    pub checks: Vec<DoctorCheck>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppTask {
    Run,
    Test,
}

impl AppTask {
    fn cargo_subcommand(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Test => "test",
        }
    }
}

/// A fully resolved Cargo invocation for one app. Keeping planning separate
/// from execution makes command construction deterministic and testable.
#[derive(Debug, PartialEq, Eq)]
pub struct CargoPlan {
    pub app_dir: PathBuf,
    pub program: OsString,
    pub args: Vec<OsString>,
}

/// Package-scoped static checks used by `tilo-app check` before scenarios.
#[derive(Debug, PartialEq, Eq)]
pub struct AppQualityPlan {
    pub format: CargoPlan,
    pub clippy: CargoPlan,
    pub test: CargoPlan,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AppFormatMode {
    /// Reject formatting drift without changing source.
    #[default]
    Check,
    /// Apply rustfmt to the app package before the remaining checks.
    Apply,
}

/// Resolved native UI-builder invocation. The heavy graphical toolkit lives in
/// the builder process, so ordinary `tilo-app` commands stay quick to compile.
#[derive(Debug, PartialEq, Eq)]
pub struct UiBuilderPlan {
    pub program: OsString,
    pub args: Vec<OsString>,
}

#[derive(Debug)]
pub struct ShotRequest {
    pub output: PathBuf,
    pub viewport: Option<(u32, u32)>,
    pub scenario: Option<String>,
    pub forwarded: Vec<OsString>,
}

#[derive(Debug)]
pub struct InspectRequest {
    pub output: PathBuf,
    pub viewport: Option<(u32, u32)>,
    pub actions: Option<PathBuf>,
    pub dump: Option<PathBuf>,
    pub targets: Option<PathBuf>,
    pub audit: bool,
    pub audit_report: Option<PathBuf>,
}

#[derive(Debug)]
pub struct ScenarioRunRequest {
    /// App directory or any file/directory below it.
    pub start: PathBuf,
    /// One named file under `tests/scenarios`; absent runs the complete suite.
    pub name: Option<String>,
    pub viewport: Option<(u32, u32)>,
    /// Optional directory for per-case semantic, target, and document JSON.
    /// Relative paths are resolved from the discovered app root.
    pub artifacts: Option<PathBuf>,
    /// Audit the final semantic tree of every isolated case.
    pub audit: bool,
}

#[derive(Debug)]
pub struct ScenarioSuitePlan {
    pub app_dir: PathBuf,
    /// Builds the app once, in the caller's environment: a Cargo wrapper such
    /// as a remote builder reads its own configuration from the XDG
    /// directories each case replaces.
    build: CargoPlan,
    /// The binary `build` reports, else the only one it builds.
    bin: Option<String>,
    cases: Vec<ScenarioCasePlan>,
    artifact_dir: Option<PathBuf>,
}

#[derive(Debug)]
struct ScenarioCasePlan {
    name: String,
    source: PathBuf,
    args: Vec<OsString>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ScenarioSuiteOutcome {
    pub passed: Vec<String>,
    pub failed: Vec<ScenarioFailure>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ScenarioFailure {
    pub name: String,
    pub source: PathBuf,
    pub detail: String,
}

#[derive(Debug)]
pub struct DevRequest {
    pub forwarded: Vec<OsString>,
    pub watch_ui: bool,
    pub debounce: Duration,
    /// Report where each reload spent its time.
    pub timings: bool,
    /// Cargo profile every build of the app uses; `None` is Cargo's `dev`.
    pub profile: Option<String>,
}

impl DevRequest {
    fn profile(&self) -> Option<&str> {
        self.profile.as_deref()
    }
}

/// The profile `tilo-app dev --release` picks: release optimisation without
/// LTO, the profile thin Tilo builds ship with. It links the toolkit
/// dynamically, so the app still reloads in place.
pub const DEV_RELEASE_PROFILE: &str = "thin-app";

/// Quiet period `tilo-app dev` waits after a save before it builds.
pub const DEV_DEBOUNCE_MS: u64 = 30;

pub use dev_reload::run_as_build_tool_if_requested;

#[derive(Debug)]
pub struct PackageRequest {
    /// Independently selected signed native SDK; never an authored runtime override.
    pub sdk: Option<native_sdk::SdkSelection>,
    pub output: Option<PathBuf>,
    pub identity: Option<PathBuf>,
    /// `None` picks `thin-app` for a thin build and `release` for a static one.
    pub profile: Option<String>,
    /// Link everything into the binary instead of against the toolkit and
    /// `libstd` the OS ships. Static binaries are several times the size limit.
    pub static_link: bool,
    /// Rust target triple; overrides the one `platform` and `arch` pick.
    pub target: Option<String>,
    pub platform: PackagePlatform,
    /// `None` builds for the host architecture.
    pub arch: Option<PackageArch>,
    pub features: Vec<String>,
    pub no_default_features: bool,
    pub force: bool,
}

/// The system a package is built for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PackagePlatform {
    /// A thin Tilo OS app, linked against the toolkit the OS ships.
    #[default]
    Tilo,
    /// One self-contained executable with the toolkit linked in, for Linux
    /// desktops other than Tilo OS.
    Linux,
    /// One self-contained Windows executable with the toolkit linked in.
    Windows,
    /// One self-contained macOS executable with the toolkit linked in.
    Macos,
    /// A WebAssembly side module of the browser's toolkit core.
    Web,
    /// The app's library in an APK whose `NativeActivity` enters it.
    Android,
}

impl PackagePlatform {
    /// Every platform, in the order `check --platform all` visits them.
    pub const ALL: [Self; 6] = [
        Self::Tilo,
        Self::Linux,
        Self::Windows,
        Self::Macos,
        Self::Web,
        Self::Android,
    ];

    /// The platform's name on the command line.
    #[must_use]
    pub const fn cli_name(self) -> &'static str {
        match self {
            Self::Tilo => "tilo",
            Self::Linux => "linux",
            Self::Windows => "windows",
            Self::Macos => "macos",
            Self::Web => "web",
            Self::Android => "android",
        }
    }

    /// Refuse a build this machine cannot execute: one for another operating
    /// system, or for an architecture other than the host's. A web build runs
    /// in a browser anywhere.
    ///
    /// # Errors
    ///
    /// Names what to run instead.
    pub fn ensure_runs_here(self, arch: Option<PackageArch>) -> Result<()> {
        let os = match self {
            Self::Web => return Ok(()),
            Self::Android => bail!(
                "an android build runs on a device or an emulator, not here; \
                 `tilo-app run --platform android` installs it on one"
            ),
            Self::Tilo | Self::Linux => "linux",
            Self::Windows => "windows",
            Self::Macos => "macos",
        };
        let name = self.cli_name();
        if std::env::consts::OS != os {
            bail!(
                "a {name} build does not run on {}; build it with \
                 `tilo-app build --platform {name}` and run it on {os}",
                std::env::consts::OS
            );
        }
        if let Some(arch) = arch
            && Some(arch) != PackageArch::host()
        {
            bail!(
                "this machine is {}, so it cannot run an {} build; build it with \
                 `tilo-app build --platform {name} --arch {}` and run it there",
                std::env::consts::ARCH,
                arch.as_str(),
                arch.as_str()
            );
        }
        Ok(())
    }

    /// The registry's name for the platform family. The browser has no
    /// registry family (its runtime compiles its apps in), so `web` is only
    /// the platform's own name.
    #[must_use]
    pub const fn registry_name(self) -> &'static str {
        match self {
            Self::Tilo => registry_types::TILO_OS_PLATFORM,
            Self::Linux => registry_types::LINUX_PLATFORM,
            Self::Windows => registry_types::WINDOWS_PLATFORM,
            Self::Macos => registry_types::MACOS_PLATFORM,
            Self::Web => "web",
            Self::Android => registry_types::ANDROID_PLATFORM,
        }
    }

    /// The Rust target triple for `arch`.
    #[must_use]
    pub fn triple(self, arch: PackageArch) -> String {
        let arch = arch.as_str();
        match self {
            Self::Tilo | Self::Linux => format!("{arch}-unknown-linux-gnu"),
            Self::Windows => format!("{arch}-pc-windows-msvc"),
            Self::Macos => format!("{arch}-apple-darwin"),
            Self::Web => "wasm32-unknown-unknown".to_owned(),
            Self::Android => format!("{arch}-linux-android"),
        }
    }

    const fn executable_suffix(self) -> &'static str {
        match self {
            Self::Windows => ".exe",
            Self::Tilo | Self::Linux | Self::Macos | Self::Web | Self::Android => "",
        }
    }
}

/// A CPU architecture a package can target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageArch {
    X86_64,
    Aarch64,
}

impl PackageArch {
    pub const ALL: [Self; 2] = [Self::Aarch64, Self::X86_64];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::Aarch64 => "aarch64",
        }
    }

    /// The architecture `tilo-app` itself runs on, when it is one of these.
    #[must_use]
    pub fn host() -> Option<Self> {
        match std::env::consts::ARCH {
            "x86_64" => Some(Self::X86_64),
            "aarch64" => Some(Self::Aarch64),
            _ => None,
        }
    }

    /// The architecture a target triple such as `aarch64-unknown-linux-gnu`
    /// builds for.
    #[must_use]
    pub fn of_triple(triple: &str) -> Option<Self> {
        match triple.split('-').next()? {
            "x86_64" => Some(Self::X86_64),
            "aarch64" => Some(Self::Aarch64),
            _ => None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct PackageOutcome {
    pub output: PathBuf,
    pub entrypoints: Vec<PathBuf>,
    pub size: size::SizeReport,
}

/// One validated edit to a serialized UI object graph.
pub use tilo_ui_graph::UiGraphEdit;

/// High-level request for an app-owned component in a serialized UI graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiComponentInsert {
    pub name: String,
    pub id: Option<String>,
    pub parent: String,
    /// `None` appends to the parent.
    pub index: Option<usize>,
    /// One JSON object passed unchanged to the component factory.
    pub props: String,
}

/// Create a useful, accessible starter graph.
///
/// # Errors
///
/// Returns an error when the destination exists without `force`, its parent
/// cannot be created, or the atomic write fails.
pub fn init_ui_graph(path: &Path, title: &str, force: bool) -> Result<()> {
    if title.trim().is_empty() {
        bail!("screen title must not be empty");
    }
    if path.exists() && !force {
        bail!(
            "UI graph {} already exists; pass --force to replace it",
            path.display()
        );
    }
    let document = starter_ui_document(title.trim());
    write_ui_graph(path, &document)
}

fn starter_ui_document(title: &str) -> tilo_ui_graph::UiDocument {
    tilo_ui_graph::UiDocument::new(tilo_ui_graph::UiNode::Column {
        id: "root".into(),
        children: vec![
            tilo_ui_graph::UiNode::Text {
                id: "title".into(),
                text: tilo_ui_graph::UiTextSource::Static {
                    value: title.into(),
                },
                role: tilo_ui_graph::UiTextRole::Title,
            },
            tilo_ui_graph::UiNode::Text {
                id: "body".into(),
                text: tilo_ui_graph::UiTextSource::Static {
                    value: "Start building here.".into(),
                },
                role: tilo_ui_graph::UiTextRole::Body,
            },
        ],
        gap: Some(8.0),
        no_padding: false,
        intrinsic_height: false,
        fill_remaining_height: false,
    })
}

const SCREEN_REGISTRY_HEADER: &str = "// @generated by tilo-app; screen graph registry.\n";
const SCREEN_ENTRY_PREFIX: &str = "// tilo-app:screen=";
const COMPONENT_REGISTRY_HEADER: &str = "// @generated by tilo-app; Rust component registry.\n";
const COMPONENT_ENTRY_PREFIX: &str = "// tilo-app:component=";
const GRAPH_COMPONENT_ENTRY_PREFIX: &str = "// tilo-app:graph-component=";
const GENERATED_GRAPH_ROUTER_CALL: &str =
    "crate::screens::router_with(\"main\", &Theme::default(), context)";

struct PendingWrite {
    path: PathBuf,
    source: String,
    description: &'static str,
}

#[derive(Clone, Copy)]
enum AddWizardArtifact {
    Screen,
    Scenario,
    Locale,
    Component,
}

/// Interactively choose, review, and add one conventional app artifact.
///
/// # Errors
///
/// Returns an error when input is not interactive, `start` is not inside a Tilo
/// app, input ends, the user cancels, validation fails, or publication fails.
pub fn add_app_artifact_interactive(start: &Path) -> Result<AddOutcome> {
    if !io::stdin().is_terminal() {
        bail!(
            "an artifact subcommand is required when stdin is not interactive; use `tilo-app add screen`, `tilo-app add component`, `tilo-app add scenario`, or `tilo-app add locale`"
        );
    }
    let stdin = io::stdin();
    let stdout = io::stdout();
    add_app_artifact_interactive_with(start, &mut stdin.lock(), &mut stdout.lock(), true)
}

fn add_app_artifact_interactive_with(
    start: &Path,
    input: &mut impl io::BufRead,
    output: &mut impl io::Write,
    interactive: bool,
) -> Result<AddOutcome> {
    if !interactive {
        bail!("an artifact subcommand is required when stdin is not interactive");
    }
    let app_dir = resolve_app_dir(start)?;
    if !app_dir.join("tilo-app.toml").is_file() {
        bail!(
            "{} is not a Tilo app (missing tilo-app.toml)",
            app_dir.display()
        );
    }
    writeln!(output, "Grow {}\n", app_dir.display()).context("showing add wizard heading")?;
    writeln!(
        output,
        "Artifact:\n  1) screen     Accessible serialized screen and routing\n  2) scenario   Headless semantic interaction workflow\n  3) locale     Complete Fluent translation catalog\n  4) component  Reusable Rust view with typed inputs"
    )
    .context("showing artifact choices")?;
    let kind = loop {
        let value = prompt(input, output, "Artifact", Some("1"))?;
        match value.trim().to_ascii_lowercase().as_str() {
            "1" | "screen" | "s" => break AddWizardArtifact::Screen,
            "2" | "scenario" | "test" => break AddWizardArtifact::Scenario,
            "3" | "locale" | "language" | "l" => break AddWizardArtifact::Locale,
            "4" | "component" | "c" => break AddWizardArtifact::Component,
            _ => writeln!(
                output,
                "  ! unknown artifact {value:?}; choose 1-4 or `screen`, `scenario`, `locale`, or `component`"
            )
            .context("showing artifact error")?,
        }
    };

    let artifact = match kind {
        AddWizardArtifact::Screen => {
            let name =
                prompt_validated(input, output, "Screen name", None, validate_artifact_name)?;
            let title = prompt(input, output, "Screen title", Some(&title_from_name(&name)))?;
            let template = prompt_screen_template(input, output)?;
            let default_link = app_dir.join("ui/main.json").is_file().then_some("main");
            let link_from =
                prompt_optional(input, output, "Link from (`-` for none)", default_link)?;
            AddArtifact::Screen {
                name,
                title: Some(title),
                template,
                link_from,
            }
        }
        AddWizardArtifact::Scenario => {
            let name =
                prompt_validated(input, output, "Scenario name", None, validate_artifact_name)?;
            let screen = prompt_optional(input, output, "Derive from screen", None)?;
            AddArtifact::Scenario { name, screen }
        }
        AddWizardArtifact::Locale => AddArtifact::Locale {
            locale: prompt_validated(input, output, "Locale", None, validate_locale)?,
        },
        AddWizardArtifact::Component => prompt_component_artifact(&app_dir, input, output)?,
    };

    writeln!(
        output,
        "\nReview:\n  App:      {}\n  Artifact: {}",
        app_dir.display(),
        artifact_summary(&artifact),
    )
    .context("showing artifact review")?;
    if !prompt_confirm(input, output, "Add this artifact", true)? {
        bail!("artifact creation cancelled");
    }
    add_app_artifact(AddRequest {
        start: app_dir,
        artifact,
        force: false,
        dry_run: false,
    })
}

fn prompt_component_artifact(
    app_dir: &Path,
    input: &mut impl io::BufRead,
    output: &mut impl io::Write,
) -> Result<AddArtifact> {
    let name = prompt_validated(
        input,
        output,
        "Component name",
        None,
        validate_component_name,
    )?;
    let template = prompt_component_template(input, output)?;
    let graph_screen = graph_compatible_component_template(template)
        .then(|| default_graph_screen(app_dir))
        .flatten();
    let Some(default_screen) = graph_screen else {
        return Ok(AddArtifact::Component { name, template });
    };
    let Some(screen) = prompt_optional(
        input,
        output,
        "Mount in screen (`-` for none)",
        Some(&default_screen),
    )?
    else {
        return Ok(AddArtifact::Component { name, template });
    };
    let props = match template {
        ComponentTemplate::Display => {
            let title = prompt(
                input,
                output,
                "Component title",
                Some(&title_from_name(&name)),
            )?;
            let detail = prompt(
                input,
                output,
                "Component detail",
                Some("Start building here."),
            )?;
            serde_json::json!({ "title": title, "detail": detail }).to_string()
        }
        ComponentTemplate::Form => {
            let value_path = prompt(
                input,
                output,
                "Value document path",
                Some(&format!("{name}-value")),
            )?;
            let enabled_path = prompt(
                input,
                output,
                "Enabled document path",
                Some(&format!("{name}-enabled")),
            )?;
            serde_json::json!({
                "value_path": value_path,
                "enabled_path": enabled_path,
            })
            .to_string()
        }
        _ => bail!("this component template requires an app-supplied model and cannot be mounted"),
    };
    Ok(AddArtifact::GraphComponent {
        name,
        template,
        mount: ComponentMount {
            screen,
            id: None,
            parent: "root".into(),
            index: None,
            props,
        },
    })
}

fn artifact_summary(artifact: &AddArtifact) -> String {
    match artifact {
        AddArtifact::Screen {
            name,
            template,
            link_from,
            ..
        } => format!(
            "screen {name:?} ({}){}",
            screen_template_name(*template),
            link_from
                .as_deref()
                .map_or_else(String::new, |source| format!(" linked from {source:?}")),
        ),
        AddArtifact::Scenario { name, screen } => screen.as_deref().map_or_else(
            || format!("empty scenario {name:?}"),
            |screen| format!("scenario {name:?} derived from {screen:?}"),
        ),
        AddArtifact::Locale { locale } => format!("locale {locale:?}"),
        AddArtifact::Component { name, template } => format!(
            "component {name:?} ({})",
            component_template_name(*template)
        ),
        AddArtifact::GraphComponent {
            name,
            template,
            mount,
        } => {
            format!(
                "component {name:?} ({}) mounted in {:?}",
                component_template_name(*template),
                mount.screen
            )
        }
    }
}

fn screen_template_name(template: ScreenTemplate) -> &'static str {
    match template {
        ScreenTemplate::Blank => "blank",
        ScreenTemplate::Form => "form",
        ScreenTemplate::Settings => "settings",
        ScreenTemplate::EmptyState => "empty-state",
    }
}

fn component_template_name(template: ComponentTemplate) -> &'static str {
    match template {
        ComponentTemplate::Display => "display",
        ComponentTemplate::Form => "form",
        ComponentTemplate::Tree => "tree",
        ComponentTemplate::List => "list",
        ComponentTemplate::Table => "table",
        ComponentTemplate::Text => "text",
    }
}

fn graph_compatible_component_template(template: ComponentTemplate) -> bool {
    GraphPropsContract::for_template(template).is_some()
}

/// Add one conventional app artifact and any required generated wiring.
///
/// Screens are validated serialized object graphs, scenarios are directly
/// accepted by `tilo-app inspect --actions`, and locale files are valid Fluent
/// catalogs wired into the generated app locale registry.
///
/// # Errors
///
/// Returns an error when `start` is not inside a Tilo app, the artifact name is
/// unsafe, the destination exists without `force`, or an atomic write fails.
pub fn add_app_artifact(request: AddRequest) -> Result<AddOutcome> {
    let app_dir = resolve_app_dir(&request.start)?;
    if !app_dir.join("tilo-app.toml").is_file() {
        bail!(
            "{} is not a Tilo app (missing tilo-app.toml)",
            app_dir.display()
        );
    }

    let (mut writes, next_command) = match request.artifact {
        AddArtifact::Screen {
            name,
            title,
            template,
            link_from,
        } => screen_artifact_writes(
            &app_dir,
            &name,
            title.as_deref(),
            template,
            link_from.as_deref(),
            request.force,
        )?,
        AddArtifact::Scenario { name, screen } => {
            validate_artifact_name(&name)?;
            let relative = PathBuf::from("tests")
                .join("scenarios")
                .join(format!("{name}.json"));
            let path = app_dir.join(&relative);
            protect_destination(&path, request.force)?;
            let command = format!("tilo-app scenario run {name}");
            let source = screen.as_deref().map_or_else(
                || Ok("[]\n".to_owned()),
                |screen| screen_scenario_source(&app_dir, screen),
            )?;
            (
                vec![PendingWrite {
                    path,
                    source,
                    description: "headless scenario",
                }],
                Some(command),
            )
        }
        AddArtifact::Locale { locale } => (
            locale_artifact_writes(&app_dir, &locale, request.force)?,
            None,
        ),
        AddArtifact::Component { name, template } => {
            component_artifact_writes(&app_dir, &name, template, request.force)?
        }
        AddArtifact::GraphComponent {
            name,
            template,
            mount,
        } => {
            if !graph_compatible_component_template(template) {
                bail!(
                    "--mount supports the display and form templates; generate tree, list, table, and text components without --mount and construct them from Rust"
                );
            }
            if !generated_graph_context_ready(&app_dir)? {
                bail!(
                    "--mount requires an intact generated graph app root; add the component without --mount, register it in the app-owned UiBuildContext, then use `tilo-app ui component`"
                );
            }
            validate_component_mount_props(&name, template, &mount.props)?;
            let (mut writes, _) =
                component_artifact_writes(&app_dir, &name, template, request.force)?;
            writes.push(component_mount_write(&app_dir, &name, &mount)?);
            (writes, Some("tilo-app dev".into()))
        }
    };
    follow_source_catalog(&app_dir, &mut writes)?;
    format_pending_rust(&mut writes)?;
    let updated = writes
        .iter()
        .filter(|write| write.path.exists())
        .map(|write| write.path.clone())
        .collect();
    if !request.dry_run {
        write_batch(&writes)?;
    }
    Ok(AddOutcome {
        app_dir,
        files: writes.into_iter().map(|write| write.path).collect(),
        updated,
        dry_run: request.dry_run,
        next_command,
    })
}

const LOCALE_REGISTRY_HEADER: &str = "// @generated by tilo-app; locale registry.\n";
const LOCALE_ENTRY_PREFIX: &str = "// tilo-app:locale=";
const GENERATED_LOCALE_SETUP: &str =
    "pub mod locales;\n\nfn install_catalog() {\n    locales::install();\n}";
fn locale_artifact_writes(app_dir: &Path, locale: &str, force: bool) -> Result<Vec<PendingWrite>> {
    validate_locale(locale)?;
    if locale.eq_ignore_ascii_case("en-US") {
        bail!("en-US is the source catalog; edit locales/en-US.ftl directly");
    }
    let relative = PathBuf::from("locales").join(format!("{locale}.ftl"));
    let path = app_dir.join(relative);
    protect_destination(&path, force)?;

    let registry_path = app_dir.join("src/locales.rs");
    let registry = match std::fs::read_to_string(&registry_path) {
        Ok(registry) => registry,
        Err(error) if error.kind() == io::ErrorKind::NotFound => bail!(
            "{} is missing, so localization setup is app-owned; register {locale:?} there explicitly",
            registry_path.display()
        ),
        Err(error) => {
            return Err(error).with_context(|| format!("reading {}", registry_path.display()));
        }
    };
    let mut locales = parse_locale_registry(&registry).with_context(|| {
        format!(
            "{} is app-owned; register {locale:?} in that localization setup explicitly",
            registry_path.display()
        )
    })?;
    if let Some(existing) = locales
        .iter()
        .find(|existing| existing.eq_ignore_ascii_case(locale))
    {
        if existing != locale {
            bail!("locale {locale:?} is already registered as {existing:?}");
        }
    } else {
        locales.push(locale.to_owned());
    }
    locales.sort();
    locales.dedup();
    if !locales.iter().any(|locale| locale == "en-US") {
        bail!("generated locale registry is missing its en-US source catalog");
    }
    let source_path = app_dir.join("locales/en-US.ftl");
    let source_catalog = std::fs::read_to_string(&source_path)
        .with_context(|| format!("reading source catalog {}", source_path.display()))?;
    let source_contract = parse_fluent_catalog(&source_catalog)
        .with_context(|| format!("source catalog {} is invalid", source_path.display()))?;
    let registry_source = rustfmt_source(
        &locale_registry_source(&locales, &catalog_message_ids(&source_contract)),
        &registry_path,
    )?;

    let mut writes = vec![PendingWrite {
        path,
        source: translation_catalog_template(locale, &source_catalog),
        description: "locale catalog",
    }];
    if registry_source != registry {
        writes.push(PendingWrite {
            path: registry_path,
            source: registry_source,
            description: "locale registry",
        });
    }
    Ok(writes)
}

fn translation_catalog_template(locale: &str, source: &str) -> String {
    let mut catalog = format!(
        "### {locale} translations.\n### Values initially mirror locales/en-US.ftl; translate them in place.\n\n"
    );
    catalog.push_str(source.trim_start());
    if !catalog.ends_with('\n') {
        catalog.push('\n');
    }
    catalog
}

/// Synchronize translation catalog structure with `locales/en-US.ftl` without
/// replacing translated values. Missing messages, terms, attributes, and
/// values are copied from the source. Stale structure and variable-mismatched
/// patterns change only when their explicit options are enabled.
///
/// # Errors
///
/// Returns an error when the app or requested locale cannot be resolved, any
/// catalog is malformed, or the atomic publication fails.
pub fn sync_locale_catalogs(request: &LocaleSyncRequest) -> Result<LocaleSyncOutcome> {
    let app_dir = resolve_app_dir(&request.start)?;
    if !app_dir.join("tilo-app.toml").is_file() {
        bail!(
            "{} is not a Tilo app (missing tilo-app.toml)",
            app_dir.display()
        );
    }
    if let Some(locale) = request.locale.as_deref() {
        validate_locale(locale)?;
        if locale.eq_ignore_ascii_case("en-US") {
            bail!("en-US is the source catalog and cannot be synchronized to itself");
        }
    }

    let source_path = app_dir.join("locales/en-US.ftl");
    let source_text = std::fs::read_to_string(&source_path)
        .with_context(|| format!("reading source catalog {}", source_path.display()))?;
    let source_resource = parse_editable_fluent(&source_text, &source_path)?;
    let source_contract = parse_fluent_catalog(&source_text)
        .with_context(|| format!("validating source catalog {}", source_path.display()))?;

    let mut targets = if let Some(locale) = request.locale.as_deref() {
        let path = app_dir.join("locales").join(format!("{locale}.ftl"));
        if !path.is_file() {
            bail!(
                "translation catalog {} does not exist; create it with `tilo-app add locale {locale}`",
                path.display()
            );
        }
        vec![(locale.to_owned(), path)]
    } else {
        files_with_extension(&app_dir.join("locales"), "ftl")
            .map_err(anyhow::Error::msg)?
            .into_iter()
            .filter_map(|path| {
                let locale = path
                    .file_stem()
                    .and_then(std::ffi::OsStr::to_str)?
                    .to_owned();
                (locale != "en-US").then_some((locale, path))
            })
            .collect()
    };
    targets.sort_by(|left, right| left.0.cmp(&right.0));

    let mut writes = Vec::new();
    let mut checked = Vec::new();
    let mut unresolved = Vec::new();
    for (locale, path) in targets {
        validate_locale(&locale).with_context(|| format!("catalog filename for {locale:?}"))?;
        let original = std::fs::read_to_string(&path)
            .with_context(|| format!("reading translation catalog {}", path.display()))?;
        let mut translation = parse_editable_fluent(&original, &path)?;
        let changed = synchronize_fluent_resource(
            &source_resource,
            &mut translation,
            request.prune,
            request.reset_variables,
        );
        let synchronized = if changed {
            fluent_syntax::serializer::serialize(&translation)
        } else {
            original
        };
        let synchronized_contract = parse_fluent_catalog(&synchronized)
            .with_context(|| format!("validating synchronized catalog {}", path.display()))?;
        unresolved.extend(
            fluent_contract_drift(&source_contract, &synchronized_contract)
                .into_iter()
                .map(|difference| format!("{locale}: {difference}")),
        );
        checked.push(locale);
        if changed {
            writes.push(PendingWrite {
                path,
                source: synchronized,
                description: "synchronized locale catalog",
            });
        }
    }
    writes.extend(registry_key_write(&app_dir, &source_contract)?);
    if !request.dry_run {
        write_batch(&writes)?;
    }
    Ok(LocaleSyncOutcome {
        app_dir,
        checked,
        files: writes.into_iter().map(|write| write.path).collect(),
        unresolved,
        dry_run: request.dry_run,
    })
}

/// Keep a generated registry's label keys in step with a pending change of
/// `locales/en-US.ftl` (a screen or component adding its messages).
fn follow_source_catalog(app_dir: &Path, writes: &mut Vec<PendingWrite>) -> Result<()> {
    let source_path = app_dir.join("locales/en-US.ftl");
    let registry_path = app_dir.join("src/locales.rs");
    let Some(source) = writes
        .iter()
        .find(|write| write.path == source_path)
        .map(|write| write.source.clone())
    else {
        return Ok(());
    };
    let pending = writes.iter().position(|write| write.path == registry_path);
    let registry = match pending.and_then(|index| writes.get(index)) {
        Some(write) => write.source.clone(),
        None => match std::fs::read_to_string(&registry_path) {
            Ok(registry) => registry,
            Err(_) => return Ok(()),
        },
    };
    if !registry.starts_with(LOCALE_REGISTRY_HEADER) {
        return Ok(());
    }
    let ids = catalog_message_ids(&parse_fluent_catalog(&source)?);
    if label_key_drift(&registry, &ids).is_ok_and(|drift| drift.is_empty()) {
        return Ok(());
    }
    let rewritten = locale_registry_source(&parse_locale_registry(&registry)?, &ids);
    match pending.and_then(|index| writes.get_mut(index)) {
        Some(write) => write.source = rewritten,
        None => writes.push(PendingWrite {
            path: registry_path,
            source: rewritten,
            description: "locale registry",
        }),
    }
    Ok(())
}

/// The rewrite of a generated locale registry whose label keys differ from
/// the source catalog's messages, if any.
fn registry_key_write(
    app_dir: &Path,
    source: &FluentCatalogContract,
) -> Result<Option<PendingWrite>> {
    let registry_path = app_dir.join("src/locales.rs");
    let Ok(registry) = std::fs::read_to_string(&registry_path) else {
        return Ok(None);
    };
    if !registry.starts_with(LOCALE_REGISTRY_HEADER) {
        return Ok(None);
    }
    let ids = catalog_message_ids(source);
    if label_key_drift(&registry, &ids).is_ok_and(|drift| drift.is_empty()) {
        return Ok(None);
    }
    let locales = parse_locale_registry(&registry)?;
    Ok(Some(PendingWrite {
        source: rustfmt_source(&locale_registry_source(&locales, &ids), &registry_path)?,
        path: registry_path,
        description: "locale registry",
    }))
}

fn parse_editable_fluent(
    source: &str,
    path: &Path,
) -> Result<fluent_syntax::ast::Resource<String>> {
    fluent_syntax::parser::parse(source.to_owned()).map_err(|(_, errors)| {
        anyhow::anyhow!("parsing Fluent catalog {}: {errors:?}", path.display())
    })
}

fn synchronize_fluent_resource(
    source: &fluent_syntax::ast::Resource<String>,
    translation: &mut fluent_syntax::ast::Resource<String>,
    prune: bool,
    reset_variables: bool,
) -> bool {
    let mut changed = false;
    for source_entry in &source.body {
        let Some(source_id) = editable_fluent_entry_id(source_entry) else {
            continue;
        };
        if let Some(target_entry) = translation
            .body
            .iter_mut()
            .find(|entry| editable_fluent_entry_id(entry).as_deref() == Some(&source_id))
        {
            changed |= synchronize_fluent_entry(source_entry, target_entry, prune, reset_variables);
        } else {
            translation.body.push(source_entry.clone());
            changed = true;
        }
    }
    if prune {
        let source_ids: BTreeSet<_> = source
            .body
            .iter()
            .filter_map(editable_fluent_entry_id)
            .collect();
        let before = translation.body.len();
        translation.body.retain(|entry| {
            editable_fluent_entry_id(entry).is_none_or(|id| source_ids.contains(&id))
        });
        changed |= translation.body.len() != before;
    }
    changed
}

fn editable_fluent_entry_id(entry: &fluent_syntax::ast::Entry<String>) -> Option<String> {
    use fluent_syntax::ast::Entry;

    match entry {
        Entry::Message(message) => Some(message.id.name.clone()),
        Entry::Term(term) => Some(format!("-{}", term.id.name)),
        Entry::Comment(_)
        | Entry::GroupComment(_)
        | Entry::ResourceComment(_)
        | Entry::Junk { .. } => None,
    }
}

fn synchronize_fluent_entry(
    source: &fluent_syntax::ast::Entry<String>,
    translation: &mut fluent_syntax::ast::Entry<String>,
    prune: bool,
    reset_variables: bool,
) -> bool {
    use fluent_syntax::ast::Entry;

    match (source, translation) {
        (Entry::Message(source), Entry::Message(translation)) => {
            let mut changed = synchronize_fluent_value(
                source.value.as_ref(),
                &mut translation.value,
                prune,
                reset_variables,
            );
            changed |= synchronize_fluent_attributes(
                &source.attributes,
                &mut translation.attributes,
                prune,
                reset_variables,
            );
            changed
        }
        (Entry::Term(source), Entry::Term(translation)) => {
            let mut changed = false;
            if reset_variables
                && fluent_pattern_variables(&source.value)
                    != fluent_pattern_variables(&translation.value)
            {
                translation.value = source.value.clone();
                changed = true;
            }
            changed
                | synchronize_fluent_attributes(
                    &source.attributes,
                    &mut translation.attributes,
                    prune,
                    reset_variables,
                )
        }
        _ => false,
    }
}

fn synchronize_fluent_value(
    source: Option<&fluent_syntax::ast::Pattern<String>>,
    translation: &mut Option<fluent_syntax::ast::Pattern<String>>,
    prune: bool,
    reset_variables: bool,
) -> bool {
    match (source, translation.as_mut()) {
        (Some(source), None) => {
            *translation = Some(source.clone());
            true
        }
        (None, Some(_)) if prune => {
            *translation = None;
            true
        }
        (Some(source), Some(translation))
            if reset_variables
                && fluent_pattern_variables(source) != fluent_pattern_variables(translation) =>
        {
            *translation = source.clone();
            true
        }
        _ => false,
    }
}

fn synchronize_fluent_attributes(
    source: &[fluent_syntax::ast::Attribute<String>],
    translation: &mut Vec<fluent_syntax::ast::Attribute<String>>,
    prune: bool,
    reset_variables: bool,
) -> bool {
    let mut changed = false;
    for source_attribute in source {
        if let Some(target_attribute) = translation
            .iter_mut()
            .find(|attribute| attribute.id.name == source_attribute.id.name)
        {
            if reset_variables
                && fluent_pattern_variables(&source_attribute.value)
                    != fluent_pattern_variables(&target_attribute.value)
            {
                target_attribute.value = source_attribute.value.clone();
                changed = true;
            }
        } else {
            translation.push(source_attribute.clone());
            changed = true;
        }
    }
    if prune {
        let source_ids: BTreeSet<_> = source
            .iter()
            .map(|attribute| attribute.id.name.as_str())
            .collect();
        let before = translation.len();
        translation.retain(|attribute| source_ids.contains(attribute.id.name.as_str()));
        changed |= translation.len() != before;
    }
    changed
}

fn parse_locale_registry(source: &str) -> Result<Vec<String>> {
    if !source.starts_with(LOCALE_REGISTRY_HEADER) {
        bail!("missing generated locale-registry header");
    }
    let mut locales: Vec<String> = Vec::new();
    for line in source.lines() {
        if let Some(locale) = line.strip_prefix(LOCALE_ENTRY_PREFIX) {
            validate_locale(locale)?;
            if let Some(existing) = locales
                .iter()
                .find(|existing| existing.eq_ignore_ascii_case(locale))
            {
                bail!("locale {locale:?} duplicates registered locale {existing:?}");
            }
            locales.push(locale.to_owned());
        }
    }
    Ok(locales)
}

/// The constant `tilo-app` generates for the source catalog's message `id`:
/// `notes-add` is `NOTES_ADD`.
fn label_key_constant(id: &str) -> String {
    let mut name: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    if name.starts_with(|c: char| c.is_ascii_digit()) {
        name.insert(0, '_');
    }
    name
}

/// The messages (not terms) of a source catalog, in catalog order of ids.
fn catalog_message_ids(contract: &FluentCatalogContract) -> Vec<String> {
    contract
        .entries
        .keys()
        .filter(|id| !id.starts_with('-'))
        .cloned()
        .collect()
}

/// The message ids of the app's `locales/en-US.ftl`.
fn source_message_ids(app_dir: &Path) -> Result<Vec<String>> {
    let source_path = app_dir.join("locales/en-US.ftl");
    let source = std::fs::read_to_string(&source_path)
        .with_context(|| format!("reading source catalog {}", source_path.display()))?;
    let contract = parse_fluent_catalog(&source)
        .with_context(|| format!("source catalog {} is invalid", source_path.display()))?;
    Ok(catalog_message_ids(&contract))
}

/// The label keys a generated registry declares, by constant name.
fn registry_label_keys(registry: &str) -> Result<BTreeMap<String, String>> {
    let file = syn::parse_file(registry).context("generated locale registry is not valid Rust")?;
    let Some(keys) = file.items.iter().find_map(|item| match item {
        syn::Item::Mod(module) if module.ident == "keys" => module.content.as_ref(),
        _ => None,
    }) else {
        bail!("the registry has no `keys` module");
    };
    let mut declared = BTreeMap::new();
    for item in &keys.1 {
        let syn::Item::Const(constant) = item else {
            continue;
        };
        let syn::Expr::Call(call) = constant.expr.as_ref() else {
            bail!("key {} is not a LabelKey::new call", constant.ident);
        };
        let Some(syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(id),
            ..
        })) = call.args.first()
        else {
            bail!("key {} does not name its message", constant.ident);
        };
        declared.insert(constant.ident.to_string(), id.value());
    }
    Ok(declared)
}

/// Whether a generated registry's keys are exactly the source catalog's
/// messages; the difference otherwise.
fn label_key_drift(registry: &str, ids: &[String]) -> Result<Vec<String>> {
    let declared = registry_label_keys(registry)?;
    let expected: BTreeMap<String, String> = ids
        .iter()
        .map(|id| (label_key_constant(id), id.clone()))
        .collect();
    let mut drift = Vec::new();
    for (name, id) in &expected {
        match declared.get(name) {
            Some(declared) if declared == id => {}
            Some(declared) => drift.push(format!("{name} names {declared:?}, not {id:?}")),
            None => drift.push(format!("missing {name} for {id:?}")),
        }
    }
    for name in declared.keys() {
        if !expected.contains_key(name) {
            drift.push(format!("stale {name}"));
        }
    }
    Ok(drift)
}

/// Rewrite a generated locale registry whose label keys no longer match
/// `locales/en-US.ftl`: the build step that keeps `locales::keys` current.
/// App-owned registries are left alone. Returns whether it was rewritten.
///
/// # Errors
///
/// Returns an error when the source catalog or registry cannot be read or
/// the rewritten registry cannot be formatted or written.
pub fn refresh_locale_registry(app_dir: &Path) -> Result<bool> {
    let registry_path = app_dir.join("src/locales.rs");
    let Ok(registry) = std::fs::read_to_string(&registry_path) else {
        return Ok(false);
    };
    if !registry.starts_with(LOCALE_REGISTRY_HEADER) || !app_dir.join("locales/en-US.ftl").is_file()
    {
        return Ok(false);
    }
    let ids = source_message_ids(app_dir)?;
    if label_key_drift(&registry, &ids).is_ok_and(|drift| drift.is_empty()) {
        return Ok(false);
    }
    let locales = parse_locale_registry(&registry)?;
    let source = rustfmt_source(&locale_registry_source(&locales, &ids), &registry_path)?;
    write_batch(&[PendingWrite {
        path: registry_path,
        source,
        description: "locale registry",
    }])?;
    Ok(true)
}

fn locale_registry_source(locales: &[String], keys: &[String]) -> String {
    let mut source = String::from(LOCALE_REGISTRY_HEADER);
    source.push_str("// Edit Fluent catalogs under locales/; tilo-app maintains this registry.\n");
    for locale in locales {
        let _ = writeln!(source, "{LOCALE_ENTRY_PREFIX}{locale}");
    }
    source.push('\n');
    for (index, locale) in locales.iter().enumerate() {
        let _ = writeln!(
            source,
            "const CATALOG_{index}: &str = include_str!(\"../locales/{locale}.ftl\");"
        );
    }
    source.push_str("\npub const IDS: &[&str] = &[\n");
    for locale in locales {
        let _ = writeln!(source, "    {locale:?},");
    }
    source.push_str(
        "];\n\npub fn install() {\n    if let Err(error) = ui::i18n::I18nManager::install_system(\n        \"en-US\",\n        &[\n",
    );
    for (index, locale) in locales.iter().enumerate() {
        let _ = writeln!(source, "            ({locale:?}, CATALOG_{index}),");
    }
    source.push_str(
        "        ],\n    ) {\n        eprintln!(\"localization catalog: {error}\");\n    }\n}\n",
    );
    source.push_str(
        "\n/// One constant per message of locales/en-US.ftl, for labels and placeholders.\npub mod keys {\n    use ui::i18n::LabelKey;\n",
    );
    for id in keys {
        let _ = writeln!(
            source,
            "    pub const {}: LabelKey = LabelKey::new({id:?});",
            label_key_constant(id)
        );
    }
    source.push_str("}\n");
    source
}

fn screen_artifact_writes(
    app_dir: &Path,
    name: &str,
    title: Option<&str>,
    template: ScreenTemplate,
    link_from: Option<&str>,
    force: bool,
) -> Result<(Vec<PendingWrite>, Option<String>)> {
    validate_artifact_name(name)?;
    let title = title.map_or_else(|| title_from_name(name), ToOwned::to_owned);
    if title.trim().is_empty() {
        bail!("screen title must not be empty");
    }
    let relative = PathBuf::from("ui").join(format!("{name}.json"));
    let localized = app_dir.join("locales/en-US.ftl").is_file();
    let (mut document, mut messages) =
        screen_template_document(name, title.trim(), template, localized);
    if name != "main" {
        let back = ScreenMessage::new(name, "back", "Back");
        let root = document.root.id().to_owned();
        let index = document.root.children().len();
        document.insert_child(
            &root,
            index,
            tilo_ui_graph::UiNode::Button {
                id: "back".into(),
                label: back.reference(localized),
                accessible_name: None,
                tooltip: None,
                on_click_path: None,
                click_value: None,
                navigate: Some(tilo_ui_graph::UiNavigation::Back),
                enabled: true,
                role: tilo_ui_graph::UiButtonRole::Plain,
            },
        )?;
        messages.push(back);
    }
    let mut source = document.to_json_pretty()?;
    source.push('\n');
    let command = format!("tilo-app ui edit {}", shell_display_path(&relative));
    let mut writes = screen_writes(app_dir, &relative, source, name, force)?;
    if let Some(link_from) = link_from {
        validate_artifact_name(link_from)?;
        if link_from == name {
            bail!("--link-from must name a different screen");
        }
        let link_label = if localized {
            format!("{name}-title")
        } else {
            title.trim().to_owned()
        };
        writes.push(navigation_link_write(
            app_dir,
            link_from,
            name,
            &link_label,
        )?);
    }
    writes.extend(artifact_locale_writes(app_dir, &messages, force)?);
    Ok((writes, Some(command)))
}

fn screen_scenario_source(app_dir: &Path, screen: &str) -> Result<String> {
    validate_artifact_name(screen)?;
    let app_source_path = app_dir.join("src/lib.rs");
    let app_source = std::fs::read_to_string(&app_source_path)
        .with_context(|| format!("reading app root {}", app_source_path.display()))?;
    if !app_source.contains("screens::router(\"main\"")
        && !app_source.contains("screens::router_with(")
    {
        bail!(
            "screen-derived scenarios require the app root to use the generated `screens::router` or `screens::router_with`; create the app with `--template graph` or wire the generated router first"
        );
    }

    let graph_path = app_dir.join("ui").join(format!("{screen}.json"));
    let document = load_ui_graph(&graph_path)
        .with_context(|| format!("loading scenario screen {screen:?}"))?;
    let mut actions = Vec::new();
    if screen != "main" {
        let main_path = app_dir.join("ui/main.json");
        let main = load_ui_graph(&main_path)
            .with_context(|| format!("loading entry screen {}", main_path.display()))?;
        let opener = main.nodes().into_iter().find_map(|node| match node {
            tilo_ui_graph::UiNode::Button {
                id,
                navigate: Some(tilo_ui_graph::UiNavigation::Push { route }),
                enabled: true,
                ..
            } if route == screen => Some(id.clone()),
            _ => None,
        });
        let opener = opener.with_context(|| {
            format!(
                "screen {screen:?} is not directly reachable from main; add it with `tilo-app add screen {screen} --link-from main` or add a push-navigation button"
            )
        })?;
        actions.push(semantic_action(&opener, "activate", None));
    }

    for node in document.nodes() {
        match node {
            tilo_ui_graph::UiNode::TextInput {
                id,
                multiline,
                disabled: false,
                ..
            } => actions.push(semantic_action(
                id,
                "set_value",
                Some(if *multiline {
                    "Example details"
                } else {
                    "Example"
                }),
            )),
            tilo_ui_graph::UiNode::Checkbox {
                id, enabled: true, ..
            }
            | tilo_ui_graph::UiNode::Toggle {
                id, enabled: true, ..
            } => actions.push(semantic_action(id, "toggle_check", None)),
            tilo_ui_graph::UiNode::Button {
                id,
                on_click_path: Some(_),
                enabled: true,
                ..
            } => actions.push(semantic_action(id, "activate", None)),
            _ => {}
        }
    }
    if actions.is_empty() {
        bail!("screen {screen:?} has no enabled interactive controls to exercise");
    }
    let mut source = serde_json::to_string_pretty(&actions)
        .context("serializing generated headless scenario")?;
    tilo_headless_schema::parse_actions(&source)
        .map_err(|error| anyhow::anyhow!("validating generated headless scenario: {error}"))?;
    source.push('\n');
    Ok(source)
}

fn semantic_action(target: &str, action: &str, value: Option<&str>) -> serde_json::Value {
    let mut perform = serde_json::Map::from_iter([
        (
            "target".into(),
            serde_json::Value::String(target.to_owned()),
        ),
        (
            "action".into(),
            serde_json::Value::String(action.to_owned()),
        ),
    ]);
    if let Some(value) = value {
        perform.insert("value".into(), serde_json::Value::String(value.to_owned()));
    }
    serde_json::json!({ "perform": perform })
}

#[derive(Clone, Debug)]
struct ScreenMessage {
    id: String,
    value: String,
}

impl ScreenMessage {
    fn new(screen: &str, suffix: &str, value: impl Into<String>) -> Self {
        Self {
            id: format!("{screen}-{suffix}"),
            value: value.into(),
        }
    }

    fn reference(&self, localized: bool) -> String {
        if localized {
            self.id.clone()
        } else {
            self.value.clone()
        }
    }
}

fn screen_template_document(
    name: &str,
    title: &str,
    template: ScreenTemplate,
    localized: bool,
) -> (tilo_ui_graph::UiDocument, Vec<ScreenMessage>) {
    let title_message = ScreenMessage::new(name, "title", title);
    let mut messages = vec![title_message.clone()];
    let mut children = vec![screen_text(
        "title",
        title_message.reference(localized),
        tilo_ui_graph::UiTextRole::Title,
    )];
    let (content, content_messages) = match template {
        ScreenTemplate::Blank => blank_screen_content(name, localized),
        ScreenTemplate::Form => form_screen_content(name, localized),
        ScreenTemplate::Settings => settings_screen_content(name, localized),
        ScreenTemplate::EmptyState => empty_screen_content(name, localized),
    };
    children.extend(content);
    messages.extend(content_messages);
    (
        tilo_ui_graph::UiDocument::new(tilo_ui_graph::UiNode::Column {
            id: "root".into(),
            children,
            gap: Some(8.0),
            no_padding: false,
            intrinsic_height: false,
            fill_remaining_height: false,
        }),
        messages,
    )
}

fn blank_screen_content(
    name: &str,
    localized: bool,
) -> (Vec<tilo_ui_graph::UiNode>, Vec<ScreenMessage>) {
    let body = ScreenMessage::new(name, "body", "Start building here.");
    (
        vec![screen_text(
            "body",
            body.reference(localized),
            tilo_ui_graph::UiTextRole::Body,
        )],
        vec![body],
    )
}

fn form_screen_content(
    name: &str,
    localized: bool,
) -> (Vec<tilo_ui_graph::UiNode>, Vec<ScreenMessage>) {
    let description = ScreenMessage::new(name, "description", "Enter the details below.");
    let name_label = ScreenMessage::new(name, "name-label", "Name");
    let name_placeholder = ScreenMessage::new(name, "name-placeholder", "Enter a name");
    let details_label = ScreenMessage::new(name, "details-label", "Details");
    let details_placeholder =
        ScreenMessage::new(name, "details-placeholder", "Add more information");
    let enabled = ScreenMessage::new(name, "enabled", "Enabled");
    let save = ScreenMessage::new(name, "save", "Save");
    let nodes = vec![
        screen_text(
            "description",
            description.reference(localized),
            tilo_ui_graph::UiTextRole::Body,
        ),
        tilo_ui_graph::UiNode::TextInput {
            id: "name-input".into(),
            label: Some(name_label.reference(localized)),
            accessible_name: String::new(),
            placeholder: name_placeholder.reference(localized),
            value_path: Some(format!("{name}-draft-name")),
            initial_value: String::new(),
            multiline: false,
            disabled: false,
        },
        tilo_ui_graph::UiNode::TextInput {
            id: "details-input".into(),
            label: Some(details_label.reference(localized)),
            accessible_name: String::new(),
            placeholder: details_placeholder.reference(localized),
            value_path: Some(format!("{name}-draft-details")),
            initial_value: String::new(),
            multiline: true,
            disabled: false,
        },
        labeled_check_node(
            "enabled",
            enabled.reference(localized),
            format!("{name}-draft-enabled"),
            false,
        ),
        primary_action_button(name, "save", save.reference(localized), "save-requested"),
    ];
    (
        nodes,
        vec![
            description,
            name_label,
            name_placeholder,
            details_label,
            details_placeholder,
            enabled,
            save,
        ],
    )
}

fn settings_screen_content(
    name: &str,
    localized: bool,
) -> (Vec<tilo_ui_graph::UiNode>, Vec<ScreenMessage>) {
    let description = ScreenMessage::new(name, "description", "Choose how the app behaves.");
    let notifications = ScreenMessage::new(name, "notifications", "Notifications");
    let compact = ScreenMessage::new(name, "compact", "Compact layout");
    let analytics = ScreenMessage::new(name, "analytics", "Share anonymous diagnostics");
    let save = ScreenMessage::new(name, "save", "Save settings");
    let nodes = vec![
        screen_text(
            "description",
            description.reference(localized),
            tilo_ui_graph::UiTextRole::Body,
        ),
        labeled_check_node(
            "notifications",
            notifications.reference(localized),
            format!("{name}-notifications"),
            true,
        ),
        labeled_check_node(
            "compact",
            compact.reference(localized),
            format!("{name}-compact"),
            true,
        ),
        labeled_check_node(
            "analytics",
            analytics.reference(localized),
            format!("{name}-analytics"),
            false,
        ),
        primary_action_button(name, "save", save.reference(localized), "save-requested"),
    ];
    (
        nodes,
        vec![description, notifications, compact, analytics, save],
    )
}

fn empty_screen_content(
    name: &str,
    localized: bool,
) -> (Vec<tilo_ui_graph::UiNode>, Vec<ScreenMessage>) {
    let body = ScreenMessage::new(name, "body", "There is nothing here yet.");
    let action = ScreenMessage::new(name, "action", "Create the first item");
    (
        vec![
            screen_text(
                "body",
                body.reference(localized),
                tilo_ui_graph::UiTextRole::Reading,
            ),
            primary_action_button(
                name,
                "primary-action",
                action.reference(localized),
                "primary-requested",
            ),
        ],
        vec![body, action],
    )
}

fn primary_action_button(
    screen: &str,
    id: &str,
    label: String,
    action: &str,
) -> tilo_ui_graph::UiNode {
    tilo_ui_graph::UiNode::Button {
        id: id.into(),
        label,
        accessible_name: None,
        tooltip: None,
        on_click_path: Some(format!("{screen}-{action}")),
        click_value: Some("yes".into()),
        navigate: None,
        enabled: true,
        role: tilo_ui_graph::UiButtonRole::Primary,
    }
}

fn screen_text(id: &str, value: String, role: tilo_ui_graph::UiTextRole) -> tilo_ui_graph::UiNode {
    tilo_ui_graph::UiNode::Text {
        id: id.into(),
        text: tilo_ui_graph::UiTextSource::Static { value },
        role,
    }
}

fn labeled_check_node(
    control_id: &str,
    label: String,
    value_path: String,
    toggle: bool,
) -> tilo_ui_graph::UiNode {
    if toggle {
        tilo_ui_graph::UiNode::Toggle {
            id: control_id.into(),
            label: Some(label),
            accessible_name: String::new(),
            value_path,
            enabled: true,
        }
    } else {
        tilo_ui_graph::UiNode::Checkbox {
            id: control_id.into(),
            label: Some(label),
            accessible_name: String::new(),
            value_path,
            enabled: true,
        }
    }
}

fn artifact_locale_writes(
    app_dir: &Path,
    messages: &[ScreenMessage],
    force: bool,
) -> Result<Vec<PendingWrite>> {
    let source_path = app_dir.join("locales/en-US.ftl");
    if !source_path.is_file() {
        return Ok(Vec::new());
    }
    let original_source = std::fs::read_to_string(&source_path)
        .with_context(|| format!("reading source catalog {}", source_path.display()))?;
    parse_fluent_catalog(&original_source)
        .with_context(|| format!("validating source catalog {}", source_path.display()))?;
    let mut source = parse_editable_fluent(&original_source, &source_path)?;
    for message in messages {
        let generated = fluent_message_entry(message)?;
        if let Some(existing) = source
            .body
            .iter_mut()
            .find(|entry| editable_fluent_entry_id(entry).as_deref() == Some(message.id.as_str()))
        {
            if !force {
                bail!(
                    "source locale already defines message {:?}; choose another artifact name or pass --force with the artifact replacement",
                    message.id
                );
            }
            let (
                fluent_syntax::ast::Entry::Message(existing),
                fluent_syntax::ast::Entry::Message(generated),
            ) = (existing, generated)
            else {
                bail!("source locale entry {:?} is not a message", message.id);
            };
            existing.value = generated.value;
        } else {
            source.body.push(generated);
        }
    }
    let synchronized_source = fluent_syntax::serializer::serialize(&source);
    parse_fluent_catalog(&synchronized_source).with_context(|| {
        format!(
            "validating updated source catalog {}",
            source_path.display()
        )
    })?;
    let mut writes = Vec::new();
    if synchronized_source != original_source {
        writes.push(PendingWrite {
            path: source_path,
            source: synchronized_source,
            description: "artifact source messages",
        });
    }

    for path in files_with_extension(&app_dir.join("locales"), "ftl").map_err(anyhow::Error::msg)? {
        if path.file_stem().and_then(std::ffi::OsStr::to_str) == Some("en-US") {
            continue;
        }
        let original = std::fs::read_to_string(&path)
            .with_context(|| format!("reading translation catalog {}", path.display()))?;
        parse_fluent_catalog(&original)
            .with_context(|| format!("validating translation catalog {}", path.display()))?;
        let mut translation = parse_editable_fluent(&original, &path)?;
        if synchronize_fluent_resource(&source, &mut translation, false, false) {
            let synchronized = fluent_syntax::serializer::serialize(&translation);
            parse_fluent_catalog(&synchronized).with_context(|| {
                format!("validating synchronized translation {}", path.display())
            })?;
            writes.push(PendingWrite {
                path,
                source: synchronized,
                description: "artifact translation messages",
            });
        }
    }
    Ok(writes)
}

fn fluent_message_entry(message: &ScreenMessage) -> Result<fluent_syntax::ast::Entry<String>> {
    let source = format!(
        "{} = {{ \"{}\" }}\n",
        message.id,
        escape_fluent_string(&message.value)
    );
    let mut resource = fluent_syntax::parser::parse(source)
        .map_err(|(_, errors)| anyhow::anyhow!("encoding message {:?}: {errors:?}", message.id))?;
    if resource.body.len() != 1 {
        bail!("encoding message {:?} produced no single entry", message.id);
    }
    resource
        .body
        .pop()
        .ok_or_else(|| anyhow::anyhow!("encoding message {:?} produced no entry", message.id))
}

fn escape_fluent_string(value: &str) -> String {
    let mut escaped = String::new();
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            character if character.is_control() => {
                let value = u32::from(character);
                if value <= 0xFFFF {
                    let _ = write!(escaped, "\\u{value:04X}");
                } else {
                    let _ = write!(escaped, "\\U{value:06X}");
                }
            }
            character => escaped.push(character),
        }
    }
    escaped
}

fn navigation_link_write(
    app_dir: &Path,
    from: &str,
    to: &str,
    label: &str,
) -> Result<PendingWrite> {
    let path = app_dir.join("ui").join(format!("{from}.json"));
    let mut document = load_ui_graph(&path)
        .with_context(|| format!("--link-from screen {from:?} must already exist and be valid"))?;
    let root = document.root.id().to_owned();
    let index = document.root.children().len();
    document.insert_child(
        &root,
        index,
        tilo_ui_graph::UiNode::Button {
            id: format!("open-{to}"),
            label: label.to_owned(),
            accessible_name: None,
            tooltip: None,
            on_click_path: None,
            click_value: None,
            navigate: Some(tilo_ui_graph::UiNavigation::Push {
                route: to.to_owned(),
            }),
            enabled: true,
            role: tilo_ui_graph::UiButtonRole::Secondary,
        },
    )?;
    let mut source = document.to_json_pretty()?;
    source.push('\n');
    Ok(PendingWrite {
        path,
        source,
        description: "source screen navigation",
    })
}

fn write_batch(writes: &[PendingWrite]) -> Result<()> {
    write_batch_with(writes, |write| {
        write_atomic(&write.path, write.source.as_bytes(), write.description)
    })
}

fn format_pending_rust(writes: &mut [PendingWrite]) -> Result<()> {
    for write in writes {
        if write
            .path
            .extension()
            .is_some_and(|extension| extension == "rs")
        {
            write.source = rustfmt_source(&write.source, &write.path)?;
        }
    }
    Ok(())
}

fn rustfmt_source(source: &str, path: &Path) -> Result<String> {
    let rustfmt = std::env::var_os("RUSTFMT").unwrap_or_else(|| OsString::from("rustfmt"));
    let mut child = ProcessCommand::new(&rustfmt)
        .args(["--emit", "stdout", "--edition", "2024"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| {
            format!(
                "starting rustfmt for generated Rust {}; install the rustfmt component or set RUSTFMT",
                path.display()
            )
        })?;
    child
        .stdin
        .take()
        .context("opening rustfmt input")?
        .write_all(source.as_bytes())
        .with_context(|| format!("sending generated Rust {} to rustfmt", path.display()))?;
    let output = child
        .wait_with_output()
        .with_context(|| format!("waiting for rustfmt on {}", path.display()))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        bail!(
            "rustfmt rejected generated Rust {}: {}",
            path.display(),
            detail.trim()
        );
    }
    String::from_utf8(output.stdout)
        .with_context(|| format!("rustfmt returned non-UTF-8 output for {}", path.display()))
}

fn write_batch_with(
    writes: &[PendingWrite],
    mut publish: impl FnMut(&PendingWrite) -> Result<()>,
) -> Result<()> {
    let originals = writes
        .iter()
        .map(|write| {
            std::fs::read(&write.path)
                .map(Some)
                .or_else(|error| {
                    (error.kind() == io::ErrorKind::NotFound)
                        .then_some(None)
                        .ok_or(error)
                })
                .with_context(|| format!("reading {} before generation", write.path.display()))
        })
        .collect::<Result<Vec<_>>>()?;

    for (index, write) in writes.iter().enumerate() {
        if let Err(error) = publish(write) {
            let mut rollback_failures = Vec::new();
            for (restore, original) in writes
                .iter()
                .zip(&originals)
                .take(index.saturating_add(1))
                .rev()
            {
                let result = match original {
                    Some(source) => write_atomic(&restore.path, source, "generator rollback"),
                    None if restore.path.exists() => std::fs::remove_file(&restore.path)
                        .with_context(|| format!("removing new {}", restore.path.display())),
                    None => Ok(()),
                };
                if let Err(rollback) = result {
                    rollback_failures.push(format!("{}: {rollback:#}", restore.path.display()));
                }
            }
            if rollback_failures.is_empty() {
                return Err(error)
                    .context("publishing generated files; earlier writes rolled back");
            }
            bail!(
                "{error:#}; rollback was incomplete: {}",
                rollback_failures.join("; ")
            );
        }
    }
    Ok(())
}

fn protect_destination(path: &Path, force: bool) -> Result<()> {
    if path.exists() && !force {
        bail!(
            "{} already exists; pass --force to replace it",
            path.display()
        );
    }
    Ok(())
}

fn screen_writes(
    app_dir: &Path,
    relative: &Path,
    graph_source: String,
    name: &str,
    force: bool,
) -> Result<Vec<PendingWrite>> {
    let graph_path = app_dir.join(relative);
    protect_destination(&graph_path, force)?;

    let registry_path = app_dir.join("src/screens.rs");
    let mut names = if registry_path.is_file() {
        let registry = std::fs::read_to_string(&registry_path)
            .with_context(|| format!("reading {}", registry_path.display()))?;
        parse_screen_registry(&registry).with_context(|| {
            format!(
                "{} is app-owned; move it or add the tilo-app generated header before generating screens",
                registry_path.display()
            )
        })?
    } else {
        Vec::new()
    };
    if !names.iter().any(|existing| existing == name) {
        names.push(name.to_owned());
    }
    names.sort();
    names.dedup();
    let registry_source = screen_registry_source(&names);

    let library_path = app_dir.join("src/lib.rs");
    let library_source = std::fs::read_to_string(&library_path)
        .with_context(|| format!("reading {}", library_path.display()))?;
    let syntax = syn::parse_file(&library_source)
        .with_context(|| format!("parsing {} before screen wiring", library_path.display()))?;
    let screens_modules: Vec<_> = syntax
        .items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Mod(module) if module.ident == "screens" => Some(module),
            _ => None,
        })
        .collect();
    let updated_library = match screens_modules.as_slice() {
        [] => {
            let separator = if library_source.ends_with('\n') {
                "\n"
            } else {
                "\n\n"
            };
            format!("{library_source}{separator}pub mod screens;\n")
        }
        [module]
            if module.content.is_none()
                && matches!(module.vis, syn::Visibility::Public(_))
                && !module.attrs.iter().any(|attr| attr.path().is_ident("path")) =>
        {
            library_source.clone()
        }
        [_] => bail!(
            "{} already defines a custom `screens` module; generated screens require a plain `pub mod screens;` declaration",
            library_path.display()
        ),
        _ => bail!(
            "{} defines `screens` more than once",
            library_path.display()
        ),
    };
    syn::parse_file(&updated_library)
        .with_context(|| format!("validating screen wiring for {}", library_path.display()))?;

    let mut writes = vec![PendingWrite {
        path: graph_path,
        source: graph_source,
        description: "screen graph",
    }];
    if std::fs::read_to_string(&registry_path).ok().as_deref() != Some(&registry_source) {
        writes.push(PendingWrite {
            path: registry_path,
            source: registry_source,
            description: "screen registry",
        });
    }
    if updated_library != library_source {
        writes.push(PendingWrite {
            path: library_path,
            source: updated_library,
            description: "app library",
        });
    }
    Ok(writes)
}

fn component_artifact_writes(
    app_dir: &Path,
    name: &str,
    template: ComponentTemplate,
    force: bool,
) -> Result<(Vec<PendingWrite>, Option<String>)> {
    let (module_name, type_name) = component_rust_names(name)?;
    let component_path = app_dir
        .join("src/components")
        .join(format!("{module_name}.rs"));
    protect_destination(&component_path, force)?;
    validate_component_graph_references(app_dir, name, template)?;

    let registry_path = app_dir.join("src/components/mod.rs");
    let registry = std::fs::read_to_string(&registry_path).unwrap_or_default();
    let mut names = if registry.is_empty() {
        Vec::new()
    } else {
        parse_component_registry(&registry).with_context(|| {
            format!(
                "{} is app-owned; move it or add the tilo-app generated header before generating components",
                registry_path.display()
            )
        })?
    };
    if !names.iter().any(|existing| existing == name) {
        names.push(name.to_owned());
    }
    names.sort();
    names.dedup();
    let graph_names = graph_component_names(app_dir, &registry, &names, name, template)?;
    let registry_source = component_registry_source(&names, &graph_names)?;

    let library_path = app_dir.join("src/lib.rs");
    let library_source = std::fs::read_to_string(&library_path)
        .with_context(|| format!("reading {}", library_path.display()))?;
    let syntax = syn::parse_file(&library_source)
        .with_context(|| format!("parsing {} before component wiring", library_path.display()))?;
    let component_modules: Vec<_> = syntax
        .items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Mod(module) if module.ident == "components" => Some(module),
            _ => None,
        })
        .collect();
    let updated_library = match component_modules.as_slice() {
        [] => {
            let separator = if library_source.ends_with('\n') {
                "\n"
            } else {
                "\n\n"
            };
            format!("{library_source}{separator}pub mod components;\n")
        }
        [module]
            if module.content.is_none()
                && matches!(module.vis, syn::Visibility::Public(_))
                && !module.attrs.iter().any(|attr| attr.path().is_ident("path")) =>
        {
            library_source.clone()
        }
        [_] => bail!(
            "{} already defines a custom `components` module; generated components require a plain `pub mod components;` declaration",
            library_path.display()
        ),
        _ => bail!(
            "{} defines `components` more than once",
            library_path.display()
        ),
    };
    syn::parse_file(&updated_library)
        .with_context(|| format!("validating component wiring for {}", library_path.display()))?;

    let (component_source, messages) = component_source(name, &module_name, &type_name, template);
    syn::parse_file(&component_source)
        .with_context(|| format!("validating generated component {name:?}"))?;
    let mut writes = vec![PendingWrite {
        path: component_path,
        source: component_source,
        description: "Rust component",
    }];
    if registry_source != registry {
        writes.push(PendingWrite {
            path: registry_path,
            source: registry_source,
            description: "component registry",
        });
    }
    if updated_library != library_source {
        writes.push(PendingWrite {
            path: library_path,
            source: updated_library,
            description: "app library",
        });
    }
    writes.extend(artifact_locale_writes(app_dir, &messages, force)?);
    Ok((writes, Some("tilo-app test".into())))
}

fn validate_component_graph_references(
    app_dir: &Path,
    name: &str,
    template: ComponentTemplate,
) -> Result<()> {
    let contract = GraphPropsContract::for_template(template);
    let files = files_with_extension(&app_dir.join("ui"), "json").map_err(anyhow::Error::msg)?;
    for path in files {
        let document = load_ui_graph(&path)?;
        for node in document.nodes() {
            let tilo_ui_graph::UiNode::Component {
                id,
                name: used_name,
                props,
            } = node
            else {
                continue;
            };
            if used_name != name {
                continue;
            }
            let location = format!("{} node {id:?}", relative_artifact(app_dir, &path));
            let Some(contract) = contract else {
                bail!(
                    "{location} already uses component {name:?}; the {} template has no serialized-graph factory, so remove that node or choose display/form",
                    component_template_name(template)
                );
            };
            if let Some(invalid) = contract.invalid_props(props) {
                bail!(
                    "{location} is incompatible with the {} component template: {invalid}; update the node props before replacing the component",
                    component_template_name(template)
                );
            }
        }
    }
    Ok(())
}

fn default_graph_screen(app_dir: &Path) -> Option<String> {
    if !generated_graph_context_ready(app_dir).ok()? {
        return None;
    }
    if app_dir.join("ui/main.json").is_file() {
        return Some("main".into());
    }
    let source = std::fs::read_to_string(app_dir.join("src/screens.rs")).ok()?;
    parse_screen_registry(&source).ok()?.into_iter().next()
}

fn component_mount_write(
    app_dir: &Path,
    component: &str,
    mount: &ComponentMount,
) -> Result<PendingWrite> {
    validate_artifact_name(&mount.screen)?;
    let path = app_dir.join("ui").join(format!("{}.json", mount.screen));
    let mut document = load_ui_graph(&path)
        .with_context(|| format!("mounting component in screen {:?}", mount.screen))?;
    apply_ui_component(
        &mut document,
        UiComponentInsert {
            name: component.to_owned(),
            id: mount.id.clone(),
            parent: mount.parent.clone(),
            index: mount.index,
            props: mount.props.clone(),
        },
    )?;
    let mut source = document.to_json_pretty()?;
    source.push('\n');
    Ok(PendingWrite {
        path,
        source,
        description: "component screen mount",
    })
}

fn validate_component_mount_props(
    name: &str,
    template: ComponentTemplate,
    source: &str,
) -> Result<()> {
    let value: serde_json::Value =
        serde_json::from_str(source).context("parsing component props as JSON")?;
    let props = value
        .as_object()
        .context("component props must be a JSON object")?;
    let contract = GraphPropsContract::for_template(template)
        .context("mounted component template has no serialized-graph props contract")?;
    if let Some(invalid) = contract.invalid_props(props) {
        bail!("component {name:?} {invalid}");
    }
    Ok(())
}

fn component_rust_names(name: &str) -> Result<(String, String)> {
    validate_component_name(name)?;
    let module_name = name.replace('-', "_");
    syn::parse_str::<syn::Ident>(&module_name)
        .with_context(|| format!("component name {name:?} is not a usable Rust module name"))?;
    let type_name = title_from_name(name).replace(' ', "");
    syn::parse_str::<syn::Ident>(&type_name)
        .with_context(|| format!("component name {name:?} is not a usable Rust type name"))?;
    Ok((module_name, type_name))
}

fn parse_component_registry(source: &str) -> Result<Vec<String>> {
    if !source.starts_with(COMPONENT_REGISTRY_HEADER) {
        bail!("missing generated component-registry header");
    }
    let mut names = Vec::new();
    for line in source.lines() {
        if let Some(name) = line.strip_prefix(COMPONENT_ENTRY_PREFIX) {
            validate_component_name(name)?;
            if names.iter().any(|existing| existing == name) {
                bail!("component {name:?} is registered more than once");
            }
            names.push(name.to_owned());
        }
    }
    Ok(names)
}

fn parse_graph_component_registry(source: &str, components: &[String]) -> Result<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    for line in source.lines() {
        if let Some(name) = line.strip_prefix(GRAPH_COMPONENT_ENTRY_PREFIX) {
            validate_component_name(name)?;
            if !components.iter().any(|component| component == name) {
                bail!("graph component {name:?} is not in the component registry");
            }
            if !names.insert(name.to_owned()) {
                bail!("graph component {name:?} is registered more than once");
            }
        }
    }
    Ok(names)
}

fn graph_component_names(
    app_dir: &Path,
    registry: &str,
    components: &[String],
    replaced: &str,
    template: ComponentTemplate,
) -> Result<BTreeSet<String>> {
    let mut graph_names = if registry.is_empty() {
        BTreeSet::new()
    } else {
        parse_graph_component_registry(registry, components)?
    };
    for name in components {
        if name == replaced {
            continue;
        }
        let (module, _) = component_rust_names(name)?;
        let source =
            std::fs::read_to_string(app_dir.join("src/components").join(format!("{module}.rs")))
                .unwrap_or_default();
        if source.contains("pub fn register_ui_graph(") {
            graph_names.insert(name.clone());
        } else {
            graph_names.remove(name);
        }
    }
    graph_names.remove(replaced);
    if graph_compatible_component_template(template) {
        graph_names.insert(replaced.to_owned());
    }
    Ok(graph_names)
}

fn component_registry_source(names: &[String], graph_names: &BTreeSet<String>) -> Result<String> {
    let mut source = String::from(COMPONENT_REGISTRY_HEADER);
    source.push_str("// Edit component implementations beside this file; tilo-app maintains declarations and graph registration.\n");
    for name in names {
        let _ = writeln!(source, "{COMPONENT_ENTRY_PREFIX}{name}");
    }
    for name in graph_names {
        if !names.iter().any(|component| component == name) {
            bail!("graph component {name:?} is not in the component registry");
        }
        let _ = writeln!(source, "{GRAPH_COMPONENT_ENTRY_PREFIX}{name}");
    }
    source.push('\n');
    for name in names {
        let (module_name, _) = component_rust_names(name)?;
        let _ = writeln!(source, "pub mod {module_name};");
    }
    source.push_str(
        "\nuse ui::UiBuildContext;\n\n/// Build the app-owned component factory registry for serialized screens.\n#[must_use]\npub fn ui_graph_context() -> UiBuildContext {\n",
    );
    // The last registration is the tail expression: binding it and returning
    // the binding is what `clippy::let_and_return` rejects.
    let mut modules = graph_names
        .iter()
        .map(|name| component_rust_names(name).map(|(module_name, _)| module_name))
        .collect::<Result<Vec<_>>>()?;
    match modules.pop() {
        None => source.push_str("    UiBuildContext::default()\n"),
        Some(last) => {
            source.push_str("    let context = UiBuildContext::default();\n");
            for module_name in modules {
                let _ = writeln!(
                    source,
                    "    let context = {module_name}::register_ui_graph(context);"
                );
            }
            let _ = writeln!(source, "    {last}::register_ui_graph(context)");
        }
    }
    source.push_str("}\n");
    syn::parse_file(&source).context("validating generated component registry")?;
    Ok(source)
}

fn is_generated_graph_app(app_dir: &Path) -> Result<bool> {
    let screens = app_dir.join("src/screens.rs");
    if !screens.is_file() || !app_dir.join("ui/main.json").is_file() {
        return Ok(false);
    }
    let source = std::fs::read_to_string(&screens)
        .with_context(|| format!("reading {}", screens.display()))?;
    Ok(source.starts_with(SCREEN_REGISTRY_HEADER))
}

fn generated_graph_context_ready(app_dir: &Path) -> Result<bool> {
    if !is_generated_graph_app(app_dir)? {
        return Ok(false);
    }
    let path = app_dir.join("src/lib.rs");
    let source =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    Ok(source.contains(GENERATED_GRAPH_ROUTER_CALL))
}

fn component_source(
    name: &str,
    module_name: &str,
    type_name: &str,
    template: ComponentTemplate,
) -> (String, Vec<ScreenMessage>) {
    match template {
        ComponentTemplate::Display => (
            display_component_source(name, module_name, type_name),
            Vec::new(),
        ),
        ComponentTemplate::Form => form_component_source(name, module_name, type_name),
        ComponentTemplate::Tree => tree_component_source(name, module_name, type_name),
        ComponentTemplate::List => list_component_source(name, module_name, type_name),
        ComponentTemplate::Table => table_component_source(name, module_name, type_name),
        ComponentTemplate::Text => text_component_source(name, module_name, type_name),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "the single format template is the reviewable source of one complete generated component and its tests"
)]
fn display_component_source(name: &str, module_name: &str, type_name: &str) -> String {
    format!(
        r##"// tilo-app:graph-props=display-v1
use ui::tokens::TextRole;
use ui::widget::{{Element, Text}};
use ui::{{UiBuildContext, UiComponentRequest}};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct {type_name}Props {{
    pub title: String,
    pub detail: String,
}}

impl {type_name}Props {{
    #[must_use]
    pub fn new(title: impl Into<String>, detail: impl Into<String>) -> Self {{
        Self {{ title: title.into(), detail: detail.into() }}
    }}
}}

/// Build one reusable `{name}` instance. `id` keeps semantic identities unique.
pub fn {module_name}(id: &str, props: &{type_name}Props) -> ui::widget::Column {{
    ui::column![
        Text::new(&props.title)
            .with_role(TextRole::Heading)
            .with_id(format!("{{id}}-title")),
        Text::new(&props.detail)
            .with_role(TextRole::Body)
            .with_id(format!("{{id}}-detail")),
    ]
    .with_no_padding()
}}

/// Build this component from a serialized UI graph node.
///
/// # Errors
///
/// Returns a field-specific error when `title` or `detail` is not a string.
pub fn from_ui_graph(request: UiComponentRequest<'_>) -> Result<Element, String> {{
    let string = |key| {{
        request
            .props
            .get(key)
            .and_then(|value| value.as_str())
            .map(str::to_owned)
            .ok_or_else(|| format!("{{key}} must be a string"))
    }};
    let props = {type_name}Props::new(string("title")?, string("detail")?);
    Ok(Box::new({module_name}(request.id, &props)))
}}

/// Register this component's stable graph factory name.
#[must_use]
pub fn register_ui_graph(context: UiBuildContext) -> UiBuildContext {{
    context.with_component("{name}", from_ui_graph)
}}

#[cfg(test)]
mod tests {{
    use super::*;
    use ui::a11y::Role;
    use ui::headless::SemanticExpectation;

    #[test]
    fn component_exposes_typed_props_and_stable_semantics() {{
        let props = {type_name}Props::new("Example title", "Example detail");
        let mut app = ui::headless::Driver::new(
            ui::State::new(),
            Box::new({module_name}("example", &props)),
            (480, 240),
        );
        app.expect(
            &SemanticExpectation::named("example-title")
                .role(Role::Heading {{ level: 2 }})
                .name("Example title"),
        )
        .expect("component heading semantics");
        app.expect(
            &SemanticExpectation::named("example-detail")
                .role(Role::StaticText)
                .name("Example detail"),
        )
        .expect("component detail semantics");
    }}

    #[test]
    fn graph_factory_validates_props_and_builds_the_same_component() {{
        let document = ui::UiDocument::from_json(
            r#"{{"version":1,"root":{{"type":"component","id":"example","name":"{name}","props":{{"title":"Graph title","detail":"Graph detail"}}}}}}"#,
        )
        .expect("valid component graph");
        let context = register_ui_graph(UiBuildContext::default());
        let widget = ui::UiDocumentExt::build_with(
            &document,
            &ui::Theme::default(),
            &context,
        )
        .expect("registered component");
        let mut app = ui::headless::Driver::new(ui::State::new(), widget, (480, 240));
        app.expect(
            &SemanticExpectation::named("example-title")
                .role(Role::Heading {{ level: 2 }})
                .name("Graph title"),
        )
        .expect("graph component heading");

        let invalid = ui::UiDocument::from_json(
            r#"{{"version":1,"root":{{"type":"component","id":"invalid","name":"{name}","props":{{"title":"Missing detail"}}}}}}"#,
        )
        .expect("structurally valid component graph");
        assert!(matches!(
            ui::UiDocumentExt::build_with(&invalid, &ui::Theme::default(), &context),
            Err(ui::UiGraphError::ComponentBuild {{ message, .. }})
                if message == "detail must be a string"
        ));
    }}
}}
"##
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "the single format template is the reviewable source of one complete generated component and its tests"
)]
fn form_component_source(
    name: &str,
    module_name: &str,
    type_name: &str,
) -> (String, Vec<ScreenMessage>) {
    let value_label = ScreenMessage::new(name, "value-label", "Value");
    let value_placeholder = ScreenMessage::new(name, "value-placeholder", "Enter a value");
    let enabled_label = ScreenMessage::new(name, "enabled-label", "Enabled");
    let source = format!(
        r##"// tilo-app:graph-props=form-v2
use ui::widget::{{Checkbox, Element, TextInput, labeled_check, labeled_field}};
use ui::{{LocalState, UiBuildContext, UiComponentRequest}};

#[derive(Clone)]
pub struct {type_name}State {{
    pub value: LocalState<String>,
    pub enabled: LocalState<bool>,
}}

impl Default for {type_name}State {{
    fn default() -> Self {{
        Self {{
            value: LocalState::new(String::new()),
            enabled: LocalState::new(true),
        }}
    }}
}}

/// Build one reusable `{name}` form. Clone its state into app actions or workers.
pub fn {module_name}(id: &str, state: &{type_name}State) -> ui::widget::Column {{
    let value_id = format!("{{id}}-value");
    let enabled_id = format!("{{id}}-enabled");
    let value = TextInput::from_state(&state.value)
        .with_id(&value_id)
        .with_placeholder("{name}-value-placeholder");
    let enabled = Checkbox::from_state(&state.enabled)
        .with_id(&enabled_id);
    ui::column![
        labeled_field("{name}-value-label", value),
        labeled_check("{name}-enabled-label", enabled),
    ]
    .with_no_padding()
}}

/// Build one `{name}` form for a serialized graph. The value is the graph's
/// typed value at `value_path`, which the app reads when it acts on it; the
/// checkbox shares the graph's checked value at `enabled_path`. The app routes
/// that path to a field of its resource with `ViewCx::route_graph_bool` when
/// the value belongs in the document.
///
/// Reusing either path in another component or ordinary graph control creates
/// another synchronized view of the same value.
pub fn {module_name}_in_graph(
    id: &str,
    values: &ui::UiValues,
    value_path: &str,
    enabled_path: &str,
) -> ui::widget::Column {{
    let value_id = format!("{{id}}-value");
    let enabled_id = format!("{{id}}-enabled");
    let value = TextInput::from_state(&values.text(value_path))
        .with_id(&value_id)
        .with_placeholder("{name}-value-placeholder");
    let enabled = values.checkbox(enabled_path).with_id(&enabled_id);
    ui::column![
        labeled_field("{name}-value-label", value),
        labeled_check("{name}-enabled-label", enabled),
    ]
    .with_no_padding()
}}

/// Build this component from a serialized UI graph node.
///
/// Each graph instance receives explicit `value_path` and `enabled_path` props.
/// Paths may be unique per instance or deliberately shared across views.
///
/// # Errors
///
/// Returns a field-specific error when either binding path is missing or empty.
pub fn from_ui_graph(request: UiComponentRequest<'_>) -> Result<Element, String> {{
    let path = |key| {{
        request
            .props
            .get(key)
            .and_then(|value| value.as_str())
            .filter(|path| !path.trim().is_empty())
            .ok_or_else(|| format!("{{key}} must be a non-empty string"))
    }};
    Ok(Box::new({module_name}_in_graph(
        request.id,
        request.values,
        path("value_path")?,
        path("enabled_path")?,
    )))
}}

/// Register this component's stable graph factory name.
#[must_use]
pub fn register_ui_graph(context: UiBuildContext) -> UiBuildContext {{
    context.with_component("{name}", from_ui_graph)
}}

#[cfg(test)]
mod tests {{
    use super::*;
    use ui::a11y::{{Action, Role}};
    use ui::headless::SemanticExpectation;
    use ui::prelude::*;

    #[test]
    fn component_binds_typed_state_and_stable_semantics() {{
        let state = {type_name}State::default();
        let mut app = ui::headless::Driver::new(
            ui::State::new(),
            Box::new({module_name}("example", &state)),
            (480, 240),
        );
        app.perform_named("example-value", &Action::SetValue("Ada".into()))
            .expect("component input remains addressable");
        app.perform_named("example-enabled", &Action::ToggleCheck)
            .expect("component checkbox remains addressable");
        app.expect(
            &SemanticExpectation::named("example-value")
                .role(Role::TextInput)
                .value("Ada"),
        )
        .expect("component input semantics");
        assert_eq!(state.value.with(Clone::clone), "Ada");
        assert!(!state.enabled.with(|enabled| *enabled));
    }}

    #[derive(ui::prelude::Model, Default, Debug, Clone, PartialEq)]
    pub struct GraphFlags {{
        enabled: bool,
    }}

    #[ui::prelude::resource(kind = "template.form-flags", label = "Form flags")]
    impl GraphFlags {{}}

    struct GraphForm {{
        root: Element,
        values: ui::UiValues,
    }}

    impl ui::widget::Widget for GraphForm {{
        fn name(&self) -> &'static str {{ "GraphForm" }}
        fn children(&self) -> Vec<&dyn ui::widget::Widget> {{ vec![self.root.as_ref()] }}
        fn children_mut(&mut self) -> Vec<&mut (dyn ui::widget::Widget + '_)> {{ vec![self.root.as_mut()] }}
        fn build_layout(&mut self, tree: &mut ui::taffy::TaffyTree<()>, theme: &ui::Theme, nodes: &mut std::collections::HashMap<ui::taffy::NodeId, *mut dyn ui::widget::Widget>) -> ui::taffy::NodeId {{
            self.root.build_layout(tree, theme, nodes)
        }}
        fn apply_layout(&mut self, tree: &ui::taffy::TaffyTree<()>, node: ui::taffy::NodeId, origin: ui::kurbo::Point) {{ self.root.apply_layout(tree, node, origin); }}
        fn draw(&mut self, scene: &mut ui::vello::Scene, transform: ui::kurbo::Affine, text: &mut ui::TextRenderer, theme: &ui::Theme, presence: &ui::presence::PresenceCtx) {{ self.root.draw(scene, transform, text, theme, presence); }}
        fn handle_event(&mut self, event: &ui::winit::event::WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {{ self.root.handle_event(event, ctx); }}
        fn get_rect(&self) -> ui::kurbo::Rect {{ self.root.get_rect() }}
    }}

    impl ui::prelude::ResourceSurface<GraphFlags> for GraphForm {{
        fn present(&mut self, _: &std::rc::Rc<GraphFlags>) {{}}
    }}

    struct GraphApp;

    impl ui::prelude::ResourceApp for GraphApp {{
        type Resource = GraphFlags;
        const ID: &'static str = "template.form-graph";
        const TITLE: &'static str = "Form graph";

        fn view(cx: &mut ui::prelude::ViewCx<GraphFlags>) -> Element {{
            let context = register_ui_graph(UiBuildContext::default());
            cx.route_graph_bool(context.values(), "shared-enabled", GraphFlags::paths().enabled);
            let document = graph_document();
            let root = ui::UiDocumentExt::build_with(&document, &ui::Theme::default(), &context).expect("registered form component");
            cx.surface(GraphForm {{ root, values: context.values().clone() }})
        }}
    }}

    fn graph_document() -> ui::UiDocument {{
        ui::UiDocument::from_json(
            r#"{{"version":1,"root":{{"type":"column","id":"root","children":[{{"type":"component","id":"first","name":"{name}","props":{{"value_path":"shared-value","enabled_path":"shared-enabled"}}}},{{"type":"component","id":"second","name":"{name}","props":{{"value_path":"shared-value","enabled_path":"shared-enabled"}}}},{{"type":"toggle","id":"ordinary-enabled","value_path":"shared-enabled","accessible_name":"Enabled"}}]}}}}"#,
        ).expect("valid component graph")
    }}

    #[test]
    fn graph_checked_controls_use_resource_actions_and_permissions() {{
        let mut app = TestApp::<GraphApp>::new().expect("the graph app opens");
        app.harness().perform_named("ordinary-enabled", &Action::ToggleCheck)
            .expect("the ordinary graph toggle invokes the resource action");
        assert!(app.state().enabled);
        app.harness().expect(&SemanticExpectation::named("second-enabled").checked(true))
            .expect("the component follows the ordinary graph toggle");
        app.set_role(ui::Role::Viewer);
        let off = Invoke::edit(&GraphFlags::paths().enabled.set(false)).expect("an edit");
        assert!(app.invoke(off).is_err(), "a viewer cannot make the routed edit");
        let _ = app.harness().perform_named("ordinary-enabled", &Action::ToggleCheck);
        assert!(app.state().enabled);
        app.harness().expect(&SemanticExpectation::named("second-enabled").checked(true))
            .expect("refusal preserves the displayed resource value");
    }}

    #[test]
    fn graph_factory_binds_shared_graph_values_and_document_state() {{
        let mut app = ui::prelude::TestApp::<GraphApp>::new().expect("the graph app opens");
        let values = app.surface_mut::<GraphForm>().values.clone();
        app.harness().perform_named("first-value", &Action::SetValue("Ada".into()))
            .expect("first graph form edits the shared value");
        app.harness().expect(
            &SemanticExpectation::named("second-value")
                .role(Role::TextInput)
                .value("Ada"),
        )
        .expect("second graph form observes the shared value");
        app.harness().perform_named("first-enabled", &Action::ToggleCheck)
            .expect("first graph form edits shared boolean");
        app.harness().expect(
            &SemanticExpectation::named("second-enabled")
                .role(Role::Checkbox)
                .checked(true),
        )
        .expect("second graph form observes shared document boolean");
        assert!(app.undo(), "document-bound toggle is undoable");
        app.harness().expect(
            &SemanticExpectation::named("second-enabled")
                .role(Role::Checkbox)
                .checked(false),
        )
        .expect("undo updates every bound view");
        assert!(app.redo(), "document-bound toggle is redoable");
        assert_eq!(
            values.get("shared-value").as_deref(),
            Some("Ada"),
            "the app reads what the forms typed"
        );
        let state = app.harness().dump_json();
        assert!(!state.contains("shared-value"), "typing never writes the document");
        assert!(app.state().enabled, "the routed toggle writes the document");

        let context = register_ui_graph(UiBuildContext::default());
        let invalid = ui::UiDocument::from_json(
            r#"{{"version":1,"root":{{"type":"component","id":"invalid","name":"{name}","props":{{"value_path":"","enabled_path":"enabled"}}}}}}"#,
        )
        .expect("structurally valid component graph");
        assert!(matches!(
            ui::UiDocumentExt::build_with(&invalid, &ui::Theme::default(), &context),
            Err(ui::UiGraphError::ComponentBuild {{ message, .. }})
                if message == "value_path must be a non-empty string"
        ));
    }}
}}
"##
    );
    (source, vec![value_label, value_placeholder, enabled_label])
}

fn tree_component_source(
    name: &str,
    module_name: &str,
    type_name: &str,
) -> (String, Vec<ScreenMessage>) {
    let accessible_name = ScreenMessage::new(name, "accessible-name", title_from_name(name));
    let loading = ScreenMessage::new(name, "loading", "Loading…");
    let load_error = ScreenMessage::new(name, "load-error", "Could not load. Activate to retry");
    let mut source = format!(
        r#"use ui::selection::KeySelection;
use ui::widget::Text;
use ui::widget::tree_view::{{NodeId, TreeModel, TreeSlotState, TreeView}};

#[derive(Clone, Debug, PartialEq)]
pub struct {type_name}Config {{
    pub accessible_name: String,
    pub expanded: Vec<NodeId>,
    pub selection: KeySelection<NodeId>,
    pub multi_select: bool,
    pub viewport_height: f32,
    pub estimated_row_height: f32,
}}

impl Default for {type_name}Config {{
    fn default() -> Self {{
        Self {{
            accessible_name: "{name}-accessible-name".into(),
            expanded: Vec::new(),
            selection: KeySelection::new(),
            multi_select: true,
            viewport_height: 360.0,
            estimated_row_height: 36.0,
        }}
    }}
}}

/// Build one reusable `{name}` over any app, database, CRDT, or paged tree model.
///
/// The concrete return type deliberately preserves `TreeView`'s row-delegate,
/// model-notification, loading, selection, and reorder configuration methods.
pub fn {module_name}<M: TreeModel>(
    id: &str,
    model: M,
    config: &{type_name}Config,
) -> TreeView<(), M> {{
    TreeView::from_model(model)
        .with_id(id)
        .with_expanded(config.expanded.iter().copied())
        .with_selection(config.selection.clone())
        .multi_select(config.multi_select)
        .with_viewport(config.viewport_height, config.estimated_row_height)
        .with_accessible_name(&config.accessible_name)
        .with_slot_delegate(|_, _, _, state| {{
            let message = match state {{
                TreeSlotState::Loading => "{name}-loading",
                TreeSlotState::Error {{ .. }} => "{name}-load-error",
            }};
            Box::new(Text::new(message))
        }})
}}
"#
    );
    source.push_str(&tree_component_test_source(module_name, type_name));
    (source, vec![accessible_name, loading, load_error])
}

fn tree_component_test_source(module_name: &str, type_name: &str) -> String {
    format!(
        r#"
#[cfg(test)]
mod tests {{
    use super::*;
    use ui::a11y::{{Action, Role}};
    use ui::headless::SemanticExpectation;
    use ui::widget::{{Button, Text, TreeNode}};

    #[test]
    fn component_keeps_tree_behavior_and_delegated_controls() {{
        let model = TreeNode::branch(
            1,
            "Workspace",
            (),
            vec![TreeNode::branch(
                2,
                "Projects",
                (),
                vec![TreeNode::leaf(3, "Desktop app", ())],
            )],
        );
        let config = {type_name}Config {{
            expanded: vec![1, 2],
            ..{type_name}Config::default()
        }};
        let activated = ui::LocalState::new(None::<String>);
        let rows_activated = activated.clone();
        let tree = {module_name}("example-tree", model, &config)
            .with_row_delegate(move |model, id, _depth| {{
                let label = model.label(id).unwrap_or_default();
                let activated = rows_activated.clone();
                Box::new(ui::row![
                    Text::new(label),
                    Button::new(&format!("more-{{id}}"), "More")
                        .on_view(move || activated.set(Some(id.to_string()))),
                ])
            }})
            .reorderable();
        let mut app = ui::headless::Driver::new(
            ui::State::new(),
            Box::new(tree),
            (560, 360),
        );

        app.perform_semantic_named("example-tree", Some("node:2"), &Action::Collapse)
            .expect("branch remains collapsible");
        app.perform_semantic_named("example-tree", Some("node:2"), &Action::Expand)
            .expect("branch remains expandable");
        app.perform_semantic_named("example-tree", Some("node:3"), &Action::Activate)
            .expect("stable model node remains selectable");
        app.expect(
            &SemanticExpectation::named("example-tree")
                .semantic_target("node:3")
                .role(Role::TreeItem)
                .selected(true),
        )
        .expect("selected row semantics");
        app.perform_named("more-3", &Action::Activate)
            .expect("delegated control remains independently interactive");
        assert_eq!(activated.get().as_deref(), Some("3"));
    }}
}}
"#
    )
}

fn list_component_source(
    name: &str,
    module_name: &str,
    type_name: &str,
) -> (String, Vec<ScreenMessage>) {
    let accessible_name = ScreenMessage::new(name, "accessible-name", title_from_name(name));
    let mut source = format!(
        r#"use std::hash::Hash;
use ui::selection::KeySelection;
use ui::widget::virtual_list::{{ListModel, ListRowState, VirtualList}};
use ui::widget::Element;

#[derive(Clone, Debug, PartialEq)]
pub struct {type_name}Config<K: Clone + Eq + Hash> {{
    pub accessible_name: String,
    pub selection: KeySelection<K>,
    pub multi_select: bool,
    pub viewport_height: f32,
    pub estimated_row_height: f32,
}}

impl<K: Clone + Eq + Hash> Default for {type_name}Config<K> {{
    fn default() -> Self {{
        Self {{
            accessible_name: "{name}-accessible-name".into(),
            selection: KeySelection::new(),
            multi_select: true,
            viewport_height: 360.0,
            estimated_row_height: 48.0,
        }}
    }}
}}

/// Build one reusable `{name}` over any resident, database, or paged list model.
///
/// The row delegate receives ready, loading, and error states. The concrete
/// return type preserves `VirtualList` activation, notification, measurement,
/// scrolling, selection, and reorder methods for app-specific configuration.
pub fn {module_name}<M: ListModel>(
    id: &str,
    model: M,
    config: &{type_name}Config<M::Key>,
    row_delegate: impl for<'a> FnMut(&M, usize, &M::Key, ListRowState<'a>) -> Element + 'static,
    encode_key: impl Fn(&M::Key) -> String + 'static,
) -> VirtualList<M> {{
    VirtualList::new_async(model, config.estimated_row_height, row_delegate)
        .with_id(id)
        .with_accessible_name(&config.accessible_name)
        .with_semantic_key(encode_key)
        .with_viewport_height(config.viewport_height)
        .multi_select(config.multi_select)
        .with_selection(config.selection.clone())
}}
"#
    );
    source.push_str(&list_component_test_source(module_name, type_name));
    (source, vec![accessible_name])
}

fn list_component_test_source(module_name: &str, type_name: &str) -> String {
    format!(
        r#"
#[cfg(test)]
mod tests {{
    use super::*;
    use ui::a11y::{{Action, Role}};
    use ui::headless::SemanticExpectation;
    use ui::widget::{{Button, Container, Text}};

    struct SparseItems {{
        count: usize,
    }}

    impl ListModel for SparseItems {{
        type Key = u64;

        fn len(&self) -> usize {{
            self.count
        }}

        fn key_at(&self, index: usize) -> Option<Self::Key> {{
            (index < self.count)
                .then_some(index)
                .and_then(|index| u64::try_from(index).ok())
        }}

        fn index_of(&self, key: &Self::Key) -> Option<usize> {{
            usize::try_from(*key).ok().filter(|index| *index < self.count)
        }}
    }}

    #[test]
    fn component_keeps_sparse_variable_interactive_rows() {{
        const ITEM_COUNT: usize = 100_000_000;
        let mut selection = KeySelection::new();
        selection.select_only(3);
        let config = {type_name}Config {{
            selection,
            ..{type_name}Config::default()
        }};
        let activated = ui::LocalState::new(None::<String>);
        let rows_activated = activated.clone();
        let list = {module_name}(
            "example-list",
            SparseItems {{ count: ITEM_COUNT }},
            &config,
            move |_model, index, key, _state| {{
                let height = if index % 5 == 0 {{ 72.0 }} else {{ 44.0 }};
                let activated = rows_activated.clone();
                let key = key.to_string();
                Box::new(Container::new().with_height(height).with_child(Box::new(
                    ui::row![
                        Text::new(&format!("Item {{index}}")),
                        Button::new(&format!("open-{{key}}"), "Open")
                            .on_view(move || activated.set(Some(key.clone()))),
                    ],
                )))
            }},
            u64::to_string,
        );
        let mut app = ui::headless::Driver::new(
            ui::State::new(),
            Box::new(list),
            (560, 360),
        );

        app.expect(
            &SemanticExpectation::named("example-list")
                .semantic_target("key:3")
                .role(Role::ListItem)
                .selected(true),
        )
        .expect("restored stable-key selection");
        app.perform_semantic_named("example-list", Some("key:4"), &Action::Activate)
            .expect("visible stable key remains selectable");
        app.perform_named("open-0", &Action::Activate)
            .expect("delegated control remains independently interactive");

        let semantics = app.inspection().root.semantics.expect("list semantics");
        assert_eq!(semantics.role, Role::List);
        assert!(semantics.children.len() < 30, "only the viewport is mounted");
        assert_eq!(semantics.children[0].set_size, Some(ITEM_COUNT));
        let first = semantics.children[0].bounds.expect("first row bounds").to_rect().height();
        let second = semantics.children[1].bounds.expect("second row bounds").to_rect().height();
        assert!((first - second).abs() > 1.0, "delegate heights drive geometry");
        assert_eq!(activated.get().as_deref(), Some("0"));
    }}
}}
"#
    )
}

fn table_component_source(
    name: &str,
    module_name: &str,
    type_name: &str,
) -> (String, Vec<ScreenMessage>) {
    let accessible_name = ScreenMessage::new(name, "accessible-name", title_from_name(name));
    let mut source = format!(
        r#"use ui::selection::KeySelection;
use ui::widget::data_table::{{ColumnDef, DataTable, TableModel}};

#[derive(Clone, Debug, PartialEq)]
pub struct {type_name}Config {{
    pub accessible_name: String,
    pub selection: KeySelection<u64>,
    pub selectable: bool,
    pub viewport_height: f32,
    pub estimated_row_height: f32,
}}

impl Default for {type_name}Config {{
    fn default() -> Self {{
        Self {{
            accessible_name: "{name}-accessible-name".into(),
            selection: KeySelection::new(),
            selectable: true,
            viewport_height: 480.0,
            estimated_row_height: 32.0,
        }}
    }}
}}

/// Build one reusable `{name}` over any resident, database, or paged table model.
///
/// App-defined columns retain text, custom paint, or real child-widget cells.
/// The concrete return type preserves sorting, resizing, activation, targeted
/// model notifications, measurement, scrolling, and selection methods.
pub fn {module_name}<Row, M>(
    id: &str,
    columns: Vec<ColumnDef<Row>>,
    model: M,
    config: &{type_name}Config,
) -> DataTable<Row>
where
    M: TableModel<Row = Row> + 'static,
{{
    DataTable::from_model(columns, model)
        .with_id(id)
        .with_accessible_name(&config.accessible_name)
        .selectable(config.selectable)
        .with_model_selection(config.selection.clone())
        .with_viewport(config.viewport_height, config.estimated_row_height)
}}
"#
    );
    source.push_str(&table_component_test_source(module_name, type_name));
    (source, vec![accessible_name])
}

fn table_component_test_source(module_name: &str, type_name: &str) -> String {
    format!(
        r#"
#[cfg(test)]
mod tests {{
    use super::*;
    use ui::a11y::{{Action, Role}};
    use ui::headless::SemanticExpectation;
    use ui::widget::{{Button, ControlSize}};
    use ui::widget::data_table::{{ColumnWidth, TableRowState}};

    struct Row {{
        name: String,
    }}

    struct SparseRows {{
        count: usize,
        resident: Row,
    }}

    impl TableModel for SparseRows {{
        type Row = Row;

        fn len(&self) -> usize {{
            self.count
        }}

        fn key_at(&self, index: usize) -> Option<u64> {{
            (index < self.count)
                .then_some(index)
                .and_then(|index| u64::try_from(index).ok())
        }}

        fn index_of(&self, key: u64) -> Option<usize> {{
            usize::try_from(key).ok().filter(|index| *index < self.count)
        }}

        fn row_state(&self, index: usize) -> TableRowState<'_, Row> {{
            if index < self.count {{
                TableRowState::Ready(&self.resident)
            }} else {{
                TableRowState::Error("Row outside the model")
            }}
        }}
    }}

    #[test]
    fn component_keeps_sparse_variable_interactive_table_rows() {{
        const ROW_COUNT: usize = 100_000_000;
        let activated = ui::LocalState::new(None::<String>);
        let cells_activated = activated.clone();
        let columns = vec![
            ColumnDef::<Row>::new("name", "Name", ColumnWidth::Flex(1.0))
                .with_text(|row| row.name.clone()),
            ColumnDef::<Row>::new("actions", "Actions", ColumnWidth::Fixed(112.0))
                .with_widget(move |_row, index| {{
                    let activated = cells_activated.clone();
                    Box::new(
                        Button::new(&format!("open-{{index}}"), "Open")
                            .on_view(move || activated.set(Some(index.to_string())))
                            .control_size(if index == 0 {{ ControlSize::Large }} else {{ ControlSize::Small }}),
                    )
                }}),
        ];
        let mut selection = KeySelection::new();
        selection.select_only(3);
        let config = {type_name}Config {{
            selection,
            ..{type_name}Config::default()
        }};
        let table = {module_name}(
            "example-table",
            columns,
            SparseRows {{
                count: ROW_COUNT,
                resident: Row {{ name: "Resident row".into() }},
            }},
            &config,
        );
        let mut app = ui::headless::Driver::new(
            ui::State::new(),
            Box::new(table),
            (720, 420),
        );

        app.expect(
            &SemanticExpectation::named("example-table")
                .semantic_target("row:3:0")
                .role(Role::Row)
                .selected(true),
        )
        .expect("restored stable-key row selection");
        app.perform_semantic_named("example-table", Some("row:4:0"), &Action::Activate)
            .expect("visible stable row remains selectable");
        app.perform_named("open-3", &Action::Activate)
            .expect("retained cell control remains independently interactive");

        let semantics = app.inspection().root.semantics.expect("table semantics");
        assert_eq!(semantics.role, Role::Table);
        assert!(semantics.children.len() < 40, "only the viewport is mounted");
        assert_eq!(semantics.children[0].set_size, Some(ROW_COUNT));
        let first = semantics.children[0].bounds.expect("first row bounds").to_rect().height();
        let second = semantics.children[1].bounds.expect("second row bounds").to_rect().height();
        assert!((first - second).abs() > 1.0, "measured rows drive geometry");
        assert_eq!(activated.get().as_deref(), Some("3"));
    }}
}}
"#
    )
}

fn text_component_source(
    name: &str,
    module_name: &str,
    type_name: &str,
) -> (String, Vec<ScreenMessage>) {
    let body = ScreenMessage::new(name, "body", "Replace this copy with application content.");
    let mut source = format!(
        r#"use ui::widget::Text;
use ui::{{PolygonTextLayout, TextFlowRegion, TextLayoutEngine, TextRole}};

#[derive(Clone, Debug, PartialEq)]
pub struct {type_name}Props {{
    pub message: String,
    /// What the text is; the theme decides how it looks.
    pub role: TextRole,
}}

impl {type_name}Props {{
    #[must_use]
    pub fn new(message: impl Into<String>, role: TextRole) -> Self {{
        Self {{ message: message.into(), role }}
    }}
}}

impl Default for {type_name}Props {{
    fn default() -> Self {{
        Self::new("{name}-body", TextRole::Reading)
    }}
}}

/// Build ordinary reusable `{name}` text in its role, shared by layout and paint.
pub fn {module_name}(id: &str, props: &{type_name}Props) -> Text {{
    Text::new(&props.message)
        .with_id(id)
        .with_role(props.role)
        .with_kerning(true)
        .with_auto_hyphenation("en-US")
}}

/// Keep the standard Text lifecycle while replacing measurement and painting.
pub fn {module_name}_with_engine<E: TextLayoutEngine + 'static>(
    id: &str,
    props: &{type_name}Props,
    engine: E,
) -> Text {{
    {module_name}(id, props).with_layout_engine(engine)
}}

/// Flow and exactly clip the text to a concave or convex local-coordinate polygon.
pub fn {module_name}_in_region(
    id: &str,
    props: &{type_name}Props,
    region: TextFlowRegion,
) -> Text {{
    {module_name}_with_engine(id, props, PolygonTextLayout::new(region))
}}
"#
    );
    source.push_str(&text_component_test_source(module_name, type_name));
    (source, vec![body])
}

fn text_component_test_source(module_name: &str, type_name: &str) -> String {
    format!(
        r#"
#[cfg(test)]
mod tests {{
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;
    use ui::a11y::Role;
    use ui::headless::SemanticExpectation;
    use ui::kurbo::Point;
    use ui::{{TextLayoutConstraints, TextRenderer, TextStyle}};

    struct RecordingLayout(Rc<Cell<u32>>);

    impl TextLayoutEngine for RecordingLayout {{
        fn measure(
            &mut self,
            _renderer: &mut TextRenderer,
            _text: &str,
            _constraints: TextLayoutConstraints,
            _style: &TextStyle,
        ) -> (f32, f32) {{
            self.0.set(self.0.get().saturating_add(1));
            (320.0, 96.0)
        }}

        fn draw(
            &mut self,
            _renderer: &mut TextRenderer,
            _scene: &mut ui::vello::Scene,
            _text: &str,
            _constraints: TextLayoutConstraints,
            _style: &TextStyle,
            _color: ui::peniko::Color,
            _transform: ui::kurbo::Affine,
        ) {{}}
    }}

    #[test]
    fn component_keeps_text_semantics_with_a_custom_engine() {{
        let measures = Rc::new(Cell::new(0));
        let props = {type_name}Props::new("Advanced typography", TextRole::Reading);
        let text = {module_name}_with_engine(
            "example-text",
            &props,
            RecordingLayout(Rc::clone(&measures)),
        );
        let mut app = ui::headless::Driver::new(
            ui::State::new(),
            Box::new(text),
            (480, 240),
        );

        app.expect(
            &SemanticExpectation::named("example-text")
                .role(Role::StaticText)
                .name("Advanced typography"),
        )
        .expect("custom engine retains Text accessibility");
        assert!(measures.get() > 0, "custom engine participates in layout");
    }}

    #[test]
    fn component_accepts_non_rectangular_text_flow() {{
        let props = {type_name}Props::new("Polygon text", TextRole::Body);
        let region = TextFlowRegion::polygon([
            Point::new(0.0, 0.0),
            Point::new(360.0, 0.0),
            Point::new(360.0, 180.0),
            Point::new(220.0, 180.0),
            Point::new(220.0, 72.0),
            Point::new(140.0, 72.0),
            Point::new(140.0, 180.0),
            Point::new(0.0, 180.0),
        ])
        .expect("valid concave region");
        let text = {module_name}_in_region("polygon-text", &props, region);
        let mut app = ui::headless::Driver::new(
            ui::State::new(),
            Box::new(text),
            (400, 220),
        );

        app.expect(
            &SemanticExpectation::named("polygon-text")
                .role(Role::StaticText)
                .name("Polygon text"),
        )
        .expect("polygon layout retains Text accessibility");
    }}
}}
"#
    )
}

/// Render one reusable Rust component without writing it.
///
/// This is the same validated source used by `tilo-app add component`; build
/// fixtures can type-check the generator output directly instead of carrying a
/// copy that may drift.
///
/// # Errors
///
/// Returns an error when `name` cannot safely become Rust module and type
/// identifiers or when the generated source does not parse.
pub fn render_component_source(name: &str, template: ComponentTemplate) -> Result<String> {
    let (module_name, type_name) = component_rust_names(name)?;
    let (source, _) = component_source(name, &module_name, &type_name, template);
    syn::parse_file(&source).with_context(|| format!("validating generated component {name:?}"))?;
    Ok(source)
}

/// Render the `src/components/mod.rs` registry `tilo-app add component`
/// maintains for `components`, registering the graph-compatible ones with
/// serialized screens as `--mount` does.
///
/// # Errors
///
/// Returns an error when a component name is not a usable Rust module or type
/// name, or appears more than once.
pub fn render_component_registry_source(
    components: &[(&str, ComponentTemplate)],
) -> Result<String> {
    let mut names = Vec::with_capacity(components.len());
    let mut graph_names = BTreeSet::new();
    for &(name, template) in components {
        validate_component_name(name)?;
        if names.iter().any(|existing| existing == name) {
            bail!("component {name:?} is listed more than once");
        }
        names.push(name.to_owned());
        if graph_compatible_component_template(template) {
            graph_names.insert(name.to_owned());
        }
    }
    names.sort();
    component_registry_source(&names, &graph_names)
}

fn parse_screen_registry(source: &str) -> Result<Vec<String>> {
    if !source.starts_with(SCREEN_REGISTRY_HEADER) {
        bail!("missing generated screen-registry header");
    }
    let mut names = Vec::new();
    for line in source.lines() {
        if let Some(name) = line.strip_prefix(SCREEN_ENTRY_PREFIX) {
            validate_artifact_name(name)?;
            names.push(name.to_owned());
        }
    }
    Ok(names)
}

fn screen_registry_source(names: &[String]) -> String {
    let mut source = String::from(SCREEN_REGISTRY_HEADER);
    source.push_str("// Edit screen graphs under ui/; tilo-app maintains this registry.\n");
    for name in names {
        let _ = writeln!(source, "{SCREEN_ENTRY_PREFIX}{name}");
    }
    source.push_str(
        "\nuse ui::widget::Widget;\nuse ui::{RouteError, RouteHandle, Router, Theme, UiBuildContext, UiGraphError, UiGraphHandle};\n\n",
    );
    source.push_str("pub const NAMES: &[&str] = &[\n");
    for name in names {
        let _ = writeln!(source, "    {name:?},");
    }
    source.push_str("];");
    for name in names {
        let function = name.replace('-', "_");
        let _ = writeln!(
            source,
            "\n\n/// Build the `{name}` screen.\n///\n/// # Errors\n///\n/// Returns a graph parse, validation, component, or construction error.\npub fn {function}(theme: &Theme) -> Result<Box<dyn Widget>, UiGraphError> {{\n    {function}_with(theme, UiBuildContext::default())\n}}\n\n/// Build `{name}` with app-owned component factories and other services.\n///\n/// # Errors\n///\n/// Returns a graph parse, validation, component, or construction error.\npub fn {function}_with(\n    theme: &Theme,\n    context: UiBuildContext,\n) -> Result<Box<dyn Widget>, UiGraphError> {{\n    build_with({name:?}, include_str!(\"../ui/{name}.json\"), theme, context)\n}}"
        );
    }
    source.push_str(
        "

/// Build a screen selected at run time.
pub fn get(name: &str, theme: &Theme) -> Option<Result<Box<dyn Widget>, UiGraphError>> {
    match name {
",
    );
    for name in names {
        let function = name.replace('-', "_");
        let _ = writeln!(source, "        {name:?} => Some({function}(theme)),");
    }
    source.push_str(
        "        _ => None,\n    }\n}\n\nfn get_routed(\n    name: &str,\n    theme: &Theme,\n    navigation: &RouteHandle,\n    context: &UiBuildContext,\n) -> Option<Result<Box<dyn Widget>, UiGraphError>> {\n    let source = match name {\n",
    );
    for name in names {
        let _ = writeln!(
            source,
            "        {name:?} => include_str!(\"../ui/{name}.json\"),"
        );
    }
    source.push_str(
        "        _ => return None,\n    };\n    let context = context.clone().with_router(navigation.clone());\n    Some(build_with(name, source, theme, context))\n}\n\n/// Build a history-aware host for all registered screens.\n///\n/// # Errors\n///\n/// Returns an error when the initial route or a screen is invalid.\npub fn router(initial: &str, theme: &Theme) -> Result<Router, RouteError> {\n    router_with(initial, theme, UiBuildContext::default())\n}\n\n/// Build the screen router with app-owned component factories and services.\n///\n/// # Errors\n///\n/// Returns an error when the initial route or a screen is invalid.\npub fn router_with(\n    initial: &str,\n    theme: &Theme,\n    context: UiBuildContext,\n) -> Result<Router, RouteError> {\n    Router::new(initial, NAMES.iter().copied(), theme, move |route, theme, navigation| {\n        get_routed(route, theme, navigation, &context)\n            .ok_or_else(|| format!(\"screen {route:?} is not registered\"))?\n            .map_err(|error| error.to_string())\n    })\n}\n\nfn build_with(\n    screen: &str,\n    source: &str,\n    theme: &Theme,\n    context: UiBuildContext,\n) -> Result<Box<dyn Widget>, UiGraphError> {\n    let graph = UiGraphHandle::from_embedded_or_live(screen, source)?;\n    Ok(Box::new(graph.widget_with(theme, context)?))\n}\n",
    );
    source
}

fn validate_artifact_name(name: &str) -> Result<()> {
    validate_package_name(name).map_err(|_| {
        anyhow::anyhow!(
            "invalid artifact name {name:?}; use lowercase ASCII letters, digits, and single hyphens"
        )
    })
}

fn validate_component_name(name: &str) -> Result<()> {
    validate_artifact_name(name)?;
    let module_name = name.replace('-', "_");
    syn::parse_str::<syn::Ident>(&module_name).map_err(|_| {
        anyhow::anyhow!(
            "invalid component name {name:?}; choose a kebab-case name that is also a Rust identifier"
        )
    })?;
    Ok(())
}

fn validate_locale(locale: &str) -> Result<()> {
    let mut parts = locale.split('-');
    let language = parts.next().unwrap_or_default();
    let rest: Vec<_> = parts.collect();
    let valid_language =
        (2..=8).contains(&language.len()) && language.bytes().all(|byte| byte.is_ascii_lowercase());
    let valid_rest = rest.iter().all(|part| {
        !part.is_empty() && part.len() <= 8 && part.bytes().all(|byte| byte.is_ascii_alphanumeric())
    });
    if !valid_language || !valid_rest || locale.ends_with('-') {
        bail!(
            "invalid locale {locale:?}; use a BCP-47-style tag such as `de`, `en-GB`, or `zh-Hant-TW`"
        );
    }
    Ok(())
}

fn shell_display_path(path: &Path) -> String {
    let rendered = path.to_string_lossy();
    if rendered
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"/_-.".contains(&byte))
    {
        rendered.into_owned()
    } else {
        format!("'{}'", rendered.replace('\'', "'\\''"))
    }
}

/// Parse and validate a serialized UI graph with the toolkit's own schema.
///
/// # Errors
///
/// Returns a filesystem, JSON, or graph-validation error.
pub fn load_ui_graph(path: &Path) -> Result<tilo_ui_graph::UiDocument> {
    let source = std::fs::read_to_string(path)
        .with_context(|| format!("reading UI graph {}", path.display()))?;
    tilo_ui_graph::UiDocument::from_json(&source)
        .with_context(|| format!("validating UI graph {}", path.display()))
}

/// Parse one node object accepted by the current UI graph schema.
///
/// # Errors
///
/// Returns a JSON decoding error. Complete graph invariants are checked when
/// the node is inserted or used as a replacement.
pub fn parse_ui_node(source: &str) -> Result<tilo_ui_graph::UiNode> {
    serde_json::from_str(source).context("parsing UI node JSON")
}

/// Apply one edit and atomically replace the graph file only after
/// the complete candidate validates.
///
/// # Errors
///
/// Returns a load, structural-validation, or atomic-write error. The source
/// file remains unchanged on failure.
pub fn edit_ui_graph(path: &Path, edit: UiGraphEdit) -> Result<()> {
    edit_ui_graph_batch(path, vec![edit])
}

/// Insert an app-owned component with a unique default id and append-by-default
/// placement, then atomically publish the validated graph.
///
/// # Errors
///
/// Returns an error when props are not a JSON object, the parent cannot accept
/// children, an explicit id is invalid or duplicated, or publication fails.
pub fn insert_ui_component(path: &Path, request: UiComponentInsert) -> Result<String> {
    let mut document = load_ui_graph(path)?;
    let id = apply_ui_component(&mut document, request)?;
    write_ui_graph(path, &document)?;
    Ok(id)
}

fn apply_ui_component(
    document: &mut tilo_ui_graph::UiDocument,
    request: UiComponentInsert,
) -> Result<String> {
    let value: serde_json::Value =
        serde_json::from_str(&request.props).context("parsing component props as JSON")?;
    let props = value
        .as_object()
        .cloned()
        .context("component props must be a JSON object")?;
    let id = match request.id {
        Some(id) => id,
        None => unique_ui_node_id(document, &request.name)?,
    };
    let index = if let Some(index) = request.index {
        index
    } else {
        match document.node(&request.parent) {
            Some(
                tilo_ui_graph::UiNode::Column { children, .. }
                | tilo_ui_graph::UiNode::Row { children, .. },
            ) => children.len(),
            Some(_) => bail!(
                "UI graph node {:?} is not a row or column and cannot accept children",
                request.parent
            ),
            None => bail!("UI graph has no node {:?}", request.parent),
        }
    };
    document.insert_child(
        &request.parent,
        index,
        tilo_ui_graph::UiNode::Component {
            id: id.clone(),
            name: request.name,
            props,
        },
    )?;
    Ok(id)
}

fn unique_ui_node_id(document: &tilo_ui_graph::UiDocument, base: &str) -> Result<String> {
    if document.node(base).is_none() {
        return Ok(base.to_owned());
    }
    let mut suffix = 2_u64;
    loop {
        let candidate = format!("{base}-{suffix}");
        if document.node(&candidate).is_none() {
            return Ok(candidate);
        }
        suffix = suffix
            .checked_add(1)
            .context("exhausted component id suffixes")?;
    }
}

/// Parse a JSON array of tagged graph edits suitable for
/// [`edit_ui_graph_batch`].
///
/// # Errors
///
/// Returns an error when the source is not a non-empty array of supported,
/// structurally valid edit objects.
pub fn parse_ui_graph_edits(source: &str) -> Result<Vec<UiGraphEdit>> {
    tilo_ui_graph::parse_ui_graph_edits(source).context("parsing UI graph edit batch")
}

/// Apply a non-empty edit batch and publish the graph exactly once after every
/// operation and the resulting complete graph validate.
///
/// # Errors
///
/// Returns a load, edit, validation, or atomic-write error. The source file
/// remains byte-for-byte unchanged when any operation fails.
pub fn edit_ui_graph_batch(path: &Path, edits: Vec<UiGraphEdit>) -> Result<()> {
    let mut document = load_ui_graph(path)?;
    document.apply_edits(edits).map_err(|error| match error {
        tilo_ui_graph::UiGraphError::EditFailed { index, message } => {
            anyhow::anyhow!("applying UI graph edit {index}: {message}")
        }
        tilo_ui_graph::UiGraphError::EmptyEditBatch => {
            anyhow::anyhow!("UI graph edit batch must contain at least one edit")
        }
        other => anyhow::Error::new(other).context("applying UI graph edit batch"),
    })?;
    write_ui_graph(path, &document)
}

/// Rewrite a valid graph in the canonical pretty-printed form.
///
/// # Errors
///
/// Returns a load, validation, or atomic-write error.
pub fn format_ui_graph(path: &Path) -> Result<()> {
    let document = load_ui_graph(path)?;
    write_ui_graph(path, &document)
}

/// A compact hierarchy suitable for a terminal builder or code review.
///
/// # Errors
///
/// Returns a load or validation error.
pub fn ui_graph_tree(path: &Path) -> Result<String> {
    let document = load_ui_graph(path)?;
    let mut output = String::new();
    write_ui_node_tree(&document.root, 0, &mut output);
    Ok(output)
}

/// Resolve the native editor used by `tilo-app ui edit`.
///
/// An explicit program wins, followed by `TILO_UI_BUILDER`, a
/// `tilo-ui-builder` executable beside `tilo-app`, the source workspace's
/// builder package, and finally `PATH`.
///
/// # Errors
///
/// Returns an error if the current executable cannot be inspected while
/// looking for a sibling builder.
pub fn ui_builder_plan(path: &Path, explicit: Option<OsString>) -> Result<UiBuilderPlan> {
    let graph_arg = path.as_os_str().to_owned();
    let (program, args) = if let Some(program) = explicit {
        (program, vec![graph_arg])
    } else if let Some(program) = std::env::var_os("TILO_UI_BUILDER") {
        (program, vec![graph_arg])
    } else {
        let current = std::env::current_exe().context("locating tilo-app executable")?;
        let sibling =
            current.with_file_name(format!("tilo-ui-builder{}", std::env::consts::EXE_SUFFIX));
        if sibling.is_file() {
            (sibling.into_os_string(), vec![graph_arg])
        } else if let Some(manifest) = find_ui_builder_manifest(path, &current)? {
            (
                std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo")),
                vec![
                    OsString::from("run"),
                    OsString::from("--manifest-path"),
                    manifest.into_os_string(),
                    OsString::from("--"),
                    graph_arg,
                ],
            )
        } else {
            (OsString::from("tilo-ui-builder"), vec![graph_arg])
        }
    };
    Ok(UiBuilderPlan { program, args })
}

fn find_ui_builder_manifest(path: &Path, current_exe: &Path) -> Result<Option<PathBuf>> {
    let graph = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("locating current directory")?
            .join(path)
    };
    for start in [graph.as_path(), current_exe] {
        let directory = if start.is_dir() {
            start
        } else {
            start.parent().unwrap_or(start)
        };
        for ancestor in directory.ancestors() {
            let manifest = ancestor.join("crates/dev/ui-builder/Cargo.toml");
            if manifest.is_file() {
                return Ok(Some(manifest));
            }
        }
    }
    Ok(None)
}

impl UiBuilderPlan {
    /// Open the graph in the native builder and wait until its window closes.
    ///
    /// # Errors
    ///
    /// Returns an error if the builder cannot start or exits unsuccessfully.
    pub fn execute(&self) -> Result<()> {
        let status = ProcessCommand::new(&self.program)
            .args(&self.args)
            .status()
            .with_context(|| {
                format!(
                    "starting {}; build tilo-ui-builder or set TILO_UI_BUILDER",
                    self.program.to_string_lossy()
                )
            })?;
        if !status.success() {
            bail!("{} exited with {status}", self.program.to_string_lossy());
        }
        Ok(())
    }
}

fn write_ui_node_tree(node: &tilo_ui_graph::UiNode, depth: usize, output: &mut String) {
    let kind = match node {
        tilo_ui_graph::UiNode::Column { .. } => "column",
        tilo_ui_graph::UiNode::Row { .. } => "row",
        tilo_ui_graph::UiNode::Container { .. } => "container",
        tilo_ui_graph::UiNode::Text { .. } => "text",
        tilo_ui_graph::UiNode::Button { .. } => "button",
        tilo_ui_graph::UiNode::TextInput { .. } => "text-input",
        tilo_ui_graph::UiNode::Checkbox { .. } => "checkbox",
        tilo_ui_graph::UiNode::Toggle { .. } => "toggle",
        tilo_ui_graph::UiNode::Component { .. } => "component",
        tilo_ui_graph::UiNode::Spacer { .. } => "spacer",
    };
    let _ = writeln!(output, "{}{} ({kind})", "  ".repeat(depth), node.id());
    match node {
        tilo_ui_graph::UiNode::Column { children, .. }
        | tilo_ui_graph::UiNode::Row { children, .. } => {
            for child in children {
                write_ui_node_tree(child, depth.saturating_add(1), output);
            }
        }
        tilo_ui_graph::UiNode::Container { child, .. } => {
            write_ui_node_tree(child, depth.saturating_add(1), output);
        }
        _ => {}
    }
}

fn write_ui_graph(path: &Path, document: &tilo_ui_graph::UiDocument) -> Result<()> {
    let mut source = document.to_json_pretty()?;
    source.push('\n');
    write_atomic(path, source.as_bytes(), "UI graph")
}

fn write_atomic(path: &Path, source: &[u8], description: &str) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .with_context(|| format!("creating {description} directory {}", parent.display()))?;
    // Keep a regenerated file's existing permissions; new files are 0644.
    #[cfg(unix)]
    let written = {
        let mode = std::fs::metadata(path)
            .map_or(0o644, |metadata| metadata.permissions().mode() & 0o7777);
        tilo_files::native::atomic::write_with_mode(path, source, mode)
    };
    #[cfg(not(unix))]
    let written = tilo_files::native::atomic::write(path, source);
    written.with_context(|| format!("publishing {description} {}", path.display()))
}

impl CargoPlan {
    /// Run with inherited stdin/stdout/stderr so window startup, compiler
    /// diagnostics, and test output behave exactly like invoking Cargo.
    ///
    /// # Errors
    ///
    /// Returns an error when Cargo cannot start or exits unsuccessfully.
    pub fn execute(&self) -> Result<()> {
        let status = ProcessCommand::new(&self.program)
            .args(&self.args)
            .current_dir(&self.app_dir)
            .status()
            .with_context(|| format!("starting {}", self.program.to_string_lossy()))?;
        if !status.success() {
            bail!(
                "{} {} failed with {status}",
                self.program.to_string_lossy(),
                self.args
                    .first()
                    .map_or_else(|| "command".into(), |arg| arg.to_string_lossy())
            );
        }
        Ok(())
    }
}

impl DoctorReport {
    #[must_use]
    pub fn is_ready(&self) -> bool {
        !self
            .checks
            .iter()
            .any(|check| check.status == DoctorStatus::Fail)
    }

    #[must_use]
    pub fn summary(&self) -> String {
        let mut output = format!("Tilo app: {}\n", self.app_dir.display());
        for check in &self.checks {
            let mark = match check.status {
                DoctorStatus::Pass => "✓",
                DoctorStatus::Warning => "!",
                DoctorStatus::Fail => "✗",
            };
            let _ = writeln!(output, "  {mark} {}: {}", check.label, check.detail);
        }
        output.push_str(if self.is_ready() {
            "\nReady for `tilo-app run`."
        } else {
            "\nNot ready; fix the failed checks above."
        });
        output
    }
}

/// Run the `portability` lint on the app containing `start`.
///
/// # Errors
///
/// Returns an error when the app directory cannot be resolved or its
/// manifests or reachable sources cannot be read or parsed.
pub fn lint_portability(start: &Path) -> Result<portability::PortabilityReport> {
    portability::lint_app(&resolve_app_dir(start)?)
}

/// Inspect an app's source and package wiring without compiling it.
///
/// `start` may be the app directory or any descendant. All expected app
/// problems are accumulated in the report so one run can fix more than one
/// issue; only filesystem access failures are returned as `Err`.
///
/// # Errors
///
/// Returns an error when the selected app directory cannot be resolved.
pub fn doctor_app(start: &Path) -> Result<DoctorReport> {
    Ok(doctor_resolved_app(resolve_app_dir(start)?))
}

fn doctor_resolved_app(app_dir: PathBuf) -> DoctorReport {
    let mut checks = Vec::new();
    let package_name = check_cargo_manifest(&app_dir, &mut checks);
    let manifest = check_package_manifest(&app_dir, &mut checks);
    let package_id = manifest
        .as_ref()
        .map(|manifest| manifest.package.id.clone());

    check_source_file(
        &app_dir,
        "src/lib.rs",
        "app root",
        &["impl ResourceApp for", "pub fn app_root"],
        &mut checks,
    );
    check_source_file(
        &app_dir,
        "src/main.rs",
        "launch lifecycle",
        &[
            "ui::main!",
            "framework::run::<",
            "framework::launch_spec::<",
        ],
        &mut checks,
    );
    let draws_nothing = manifest
        .as_ref()
        .and_then(|manifest| manifest.entrypoint.as_ref())
        .is_some_and(|entrypoint| entrypoint.r#type == tilo_app_package::EntrypointType::Cli);
    if draws_nothing {
        // A CLI-only app is never handed a surface, so it has no controls to
        // publish; it may still consume other apps' actions.
        checks.push(DoctorCheck {
            status: DoctorStatus::Pass,
            label: "resource/action surface",
            detail: "CLI entrypoint draws no controls, so there is no surface to publish"
                .to_owned(),
        });
    } else {
        check_resource_action_surface(&app_dir, &mut checks);
    }
    check_ui_graph_artifacts(&app_dir, &mut checks);
    check_locale_artifacts(&app_dir, &mut checks);
    check_headless_scenarios(&app_dir, &mut checks);

    if let (Some(name), Some(id)) = (package_name, package_id) {
        checks.push(DoctorCheck {
            status: DoctorStatus::Pass,
            label: "identity",
            detail: format!("Cargo package {name} → app id {id}"),
        });
    }

    DoctorReport { app_dir, checks }
}

fn check_resource_action_surface(app_dir: &Path, checks: &mut Vec<DoctorCheck>) {
    fn append_rust_sources(dir: &Path, source: &mut String) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_dir() {
                append_rust_sources(&path, source)?;
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                source.push_str(&std::fs::read_to_string(path)?);
                source.push('\n');
            }
        }
        Ok(())
    }

    let mut source = String::new();
    if append_rust_sources(&app_dir.join("src"), &mut source).is_err() {
        return;
    }
    let code = rust_code_only(&source);
    let toolkit_host = code.contains("ui::widget::resource_actions::immediate(")
        || code.contains("ui::widget::resource_actions::form(")
        || code.contains("resource_actions::immediate(")
        || code.contains("resource_actions::form(");
    let toolkit_descriptor = code.contains("ui::widget::resource_actions::resource(")
        || code.contains("ui::widget::resource_actions::typed_resource(")
        || code.contains("resource_actions::resource(")
        || code.contains("resource_actions::typed_resource(")
        || code.contains("ActionForm::new(")
        || code.contains("ui::widget::resource_actions::ActionForm::new(");
    let custom_catalogue = code.contains("fn resources(") && code.contains("ActionSpec::");
    let custom_invocation = code.contains("fn start_operation(");
    let custom_operation =
        code.contains("OperationKind::Effect") || code.contains("OperationKind::View");
    let toolkit_surface = toolkit_host && toolkit_descriptor;
    let custom_surface = custom_catalogue && (!custom_operation || custom_invocation);
    // `#[resource]` publishes the model's field edits and turns `#[action]`
    // methods into verbs, the replayable catalogue `ResourceApp` hosts; a
    // hand-written `impl Resource` (a custom storage layout) is the same
    // contract.
    let framework_surface = code.contains("#[resource(") || code.contains("impl Resource for");
    let framework_host = code.contains("impl ResourceApp for");
    let framework = framework_surface && framework_host;
    let (status, detail) = if framework || toolkit_surface || custom_surface {
        (
            DoctorStatus::Pass,
            if framework {
                "the app framework publishes the resource's field edits and verbs"
            } else if toolkit_surface {
                "native SDK host publishes a typed resource/action catalogue"
            } else if custom_operation {
                "custom widget publishes and invokes a typed resource/action catalogue"
            } else {
                "custom widget publishes registered replayable resource actions"
            }
            .to_owned(),
        )
    } else {
        let mut missing = Vec::new();
        if !toolkit_descriptor && !custom_catalogue {
            missing.push("typed Resource/ActionSpec descriptor");
        }
        if !toolkit_host && !custom_invocation {
            missing.push("native host or Widget invocation implementation");
        }
        if missing.is_empty() {
            missing.push("matching host/descriptor or custom catalogue/invocation pair");
        }
        (
            DoctorStatus::Fail,
            format!(
                "missing {}; Tilo apps must expose meaningful controls through resources/actions",
                missing.join(", ")
            ),
        )
    };
    checks.push(DoctorCheck {
        status,
        label: "resource/action surface",
        detail,
    });
}

/// Strip comments and string contents before checking generated Rust structure.
/// This is intentionally a small lexical pass rather than a Rust parser: the
/// doctor needs to reject prose that merely names the native SDK contract.
#[allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "the byte cursor is bounded by the loop and advances by inspected token widths"
)]
fn rust_code_only(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut output = String::with_capacity(source.len());
    let mut index = 0;
    let mut block_depth = 0_u32;
    let mut string = false;
    while index < bytes.len() {
        if block_depth > 0 {
            if bytes.get(index..index + 2) == Some(b"/*") {
                block_depth = block_depth.saturating_add(1);
                output.push_str("  ");
                index += 2;
            } else if bytes.get(index..index + 2) == Some(b"*/") {
                block_depth = block_depth.saturating_sub(1);
                output.push_str("  ");
                index += 2;
            } else {
                output.push(if bytes[index] == b'\n' { '\n' } else { ' ' });
                index += 1;
            }
        } else if string {
            let byte = bytes[index];
            output.push(if byte == b'\n' { '\n' } else { ' ' });
            index += 1;
            if byte == b'\\' && index < bytes.len() {
                output.push(' ');
                index += 1;
            } else if byte == b'"' {
                string = false;
            }
        } else if bytes.get(index..index + 2) == Some(b"//") {
            while index < bytes.len() && bytes[index] != b'\n' {
                output.push(' ');
                index += 1;
            }
        } else if bytes.get(index..index + 2) == Some(b"/*") {
            block_depth = 1;
            output.push_str("  ");
            index += 2;
        } else if let Some(end) = raw_string_end(bytes, index) {
            // A raw string's quotes are not delimiters: `r#"a="b""#`.
            for &byte in &bytes[index..end] {
                output.push(if byte == b'\n' { '\n' } else { ' ' });
            }
            index = end;
        } else if let Some(length) = char_literal_length(bytes, index) {
            // `'"'` is a character, not the start of a string.
            output.push_str(&" ".repeat(length));
            index += length;
        } else if bytes[index] == b'"' {
            string = true;
            output.push(' ');
            index += 1;
        } else {
            output.push(char::from(bytes[index]));
            index += 1;
        }
    }
    output
}

/// Where the raw string literal (`r"…"`, `r#"…"#`, `br#"…"#`) starting at
/// `index` ends, if one starts there.
#[allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "the byte cursor is bounded by the slice and advances by inspected token widths"
)]
fn raw_string_end(bytes: &[u8], index: usize) -> Option<usize> {
    let starts_word = index == 0
        || !bytes
            .get(index - 1)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_');
    let mut at = index;
    if bytes.get(at) == Some(&b'b') {
        at += 1;
    }
    if !starts_word || bytes.get(at) != Some(&b'r') {
        return None;
    }
    at += 1;
    let hashes = bytes[at..].iter().take_while(|byte| **byte == b'#').count();
    at += hashes;
    if bytes.get(at) != Some(&b'"') {
        return None;
    }
    at += 1;
    while at < bytes.len() {
        if bytes[at] == b'"'
            && bytes.len() >= at + 1 + hashes
            && bytes[at + 1..at + 1 + hashes]
                .iter()
                .all(|byte| *byte == b'#')
        {
            return Some(at + 1 + hashes);
        }
        at += 1;
    }
    Some(bytes.len())
}

/// The length of the character literal (`'"'`, `'\''`, `'\u{22}'`) starting
/// at `index`, if one starts there; a lifetime (`'a`) is not one.
#[allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "the byte cursor is bounded by the slice and advances by inspected token widths"
)]
fn char_literal_length(bytes: &[u8], index: usize) -> Option<usize> {
    if bytes.get(index) != Some(&b'\'') {
        return None;
    }
    if bytes.get(index + 1) == Some(&b'\\') {
        // The escaped character itself may be a quote: `'\''`.
        let close = bytes
            .get(index + 3..)?
            .iter()
            .take(10)
            .position(|byte| *byte == b'\'')?;
        return Some(close + 4);
    }
    let width = match bytes.get(index + 1)? {
        byte if *byte < 0x80 => 1,
        byte if *byte >= 0xf0 => 4,
        byte if *byte >= 0xe0 => 3,
        _ => 2,
    };
    (bytes.get(index + 1 + width) == Some(&b'\'')).then_some(width + 2)
}

pub(crate) fn resolve_app_dir(start: &Path) -> Result<PathBuf> {
    let start = if start.is_file() {
        start.parent().unwrap_or(start)
    } else {
        start
    };
    let app_dir = start
        .ancestors()
        .find(|dir| dir.join("Cargo.toml").is_file() || dir.join("tilo-app.toml").is_file())
        .unwrap_or(start)
        .canonicalize()
        .with_context(|| format!("resolving app directory {}", start.display()))?;
    Ok(app_dir)
}

/// Resolve an app, require a clean doctor preflight, and construct the Cargo
/// command used by [`AppTask::Run`] or [`AppTask::Test`]. Arguments after the
/// Tilo command's `--` are forwarded after Cargo's own `--`.
///
/// # Errors
///
/// Returns an error when no app can be resolved or its doctor checks fail.
pub fn plan_app_task(start: &Path, task: AppTask, forwarded: &[OsString]) -> Result<CargoPlan> {
    let app_dir = resolve_app_dir(start)?;
    refresh_locale_registry(&app_dir)?;
    let report = doctor_resolved_app(app_dir.clone());
    if !report.is_ready() {
        bail!("app preflight failed\n\n{}", report.summary());
    }
    let manifest = app_dir.join("Cargo.toml");
    let mut args = vec![
        OsString::from(task.cargo_subcommand()),
        OsString::from("--manifest-path"),
        manifest.into_os_string(),
    ];
    if !forwarded.is_empty() {
        args.push(OsString::from("--"));
        args.extend_from_slice(forwarded);
    }
    Ok(CargoPlan {
        app_dir,
        program: std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo")),
        args,
    })
}

/// The Clippy run `check --platform` adds for a native platform: the app's
/// code compiled for that platform's target triple, warnings denied. `None`
/// when that is the host build the rest of `check` already lints.
///
/// # Errors
///
/// Returns an error for the web platform, which `check` proves with a web
/// build instead, when the architecture cannot be told, or when the app
/// cannot be resolved.
pub fn plan_platform_clippy(
    start: &Path,
    platform: PackagePlatform,
    arch: Option<PackageArch>,
) -> Result<Option<CargoPlan>> {
    if platform == PackagePlatform::Web {
        bail!("the web platform is checked by building it");
    }
    if platform == PackagePlatform::Android {
        bail!("the android platform is checked by `android::clippy`");
    }
    let Some(arch) = arch.or_else(PackageArch::host) else {
        bail!(
            "cannot tell which architecture to check on {}; pass --arch",
            std::env::consts::ARCH
        );
    };
    if platform.ensure_runs_here(Some(arch)).is_ok() {
        return Ok(None);
    }
    let app_dir = resolve_app_dir(start)?;
    let manifest = app_dir.join("Cargo.toml");
    let package = cargo_package_name(&manifest)?;
    Ok(Some(CargoPlan {
        app_dir,
        program: std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo")),
        args: vec![
            OsString::from("clippy"),
            OsString::from("--manifest-path"),
            manifest.into_os_string(),
            OsString::from("--package"),
            OsString::from(package),
            OsString::from("--target"),
            OsString::from(platform.triple(arch)),
            OsString::from("--no-deps"),
            OsString::from("--"),
            OsString::from("-D"),
            OsString::from("warnings"),
        ],
    }))
}

/// Resolve an app and construct its package-scoped format, lint, and test
/// commands. `format_mode` controls whether formatting drift is rejected or
/// repaired. Clippy checks every app target with warnings denied while
/// excluding dependency lint debt.
///
/// # Errors
///
/// Returns an error when no ready app can be resolved or its Cargo package
/// name cannot be read.
pub fn plan_app_quality(
    start: &Path,
    forwarded: &[OsString],
    format_mode: AppFormatMode,
) -> Result<AppQualityPlan> {
    let app_dir = resolve_app_dir(start)?;
    let report = doctor_resolved_app(app_dir.clone());
    if !report.is_ready() {
        bail!("app preflight failed\n\n{}", report.summary());
    }
    let manifest = app_dir.join("Cargo.toml");
    let package = cargo_package_name(&manifest)?;
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let base = || CargoPlan {
        app_dir: app_dir.clone(),
        program: cargo.clone(),
        args: Vec::new(),
    };

    let mut format = base();
    format.args = vec![
        OsString::from("fmt"),
        OsString::from("--manifest-path"),
        manifest.clone().into_os_string(),
        OsString::from("--package"),
        OsString::from(&package),
    ];
    if format_mode == AppFormatMode::Check {
        format.args.push(OsString::from("--check"));
    }

    let mut clippy = base();
    clippy.args = vec![
        OsString::from("clippy"),
        OsString::from("--manifest-path"),
        manifest.clone().into_os_string(),
        OsString::from("--package"),
        OsString::from(&package),
        OsString::from("--all-targets"),
        OsString::from("--no-deps"),
        OsString::from("--"),
        OsString::from("-D"),
        OsString::from("warnings"),
    ];

    let mut test = base();
    test.args = vec![
        OsString::from("test"),
        OsString::from("--manifest-path"),
        manifest.into_os_string(),
        OsString::from("--package"),
        OsString::from(package),
    ];
    if !forwarded.is_empty() {
        test.args.push(OsString::from("--"));
        test.args.extend_from_slice(forwarded);
    }

    Ok(AppQualityPlan {
        format,
        clippy,
        test,
    })
}

/// Construct a preflighted screenshot invocation using the app's standard
/// launch lifecycle.
///
/// # Errors
///
/// Returns an error when the app cannot be resolved or its preflight fails.
pub fn plan_app_shot(start: &Path, request: ShotRequest) -> Result<CargoPlan> {
    let mut app_args = vec![OsString::from("--shots"), request.output.into_os_string()];
    if let Some((width, height)) = request.viewport {
        app_args.extend([
            OsString::from("--viewport"),
            OsString::from(format!("{width}x{height}")),
        ]);
    }
    if let Some(scenario) = request.scenario {
        app_args.extend([OsString::from("--scenario"), OsString::from(scenario)]);
    }
    app_args.extend(request.forwarded);
    plan_app_task(start, AppTask::Run, &app_args)
}

/// Construct a preflighted, windowless semantic-inspection invocation.
///
/// The optional actions file contains a JSON array of normal headless-driver
/// actions. Output paths are passed to the app unchanged and are therefore
/// relative to the discovered app root when they are not absolute.
///
/// # Errors
///
/// Returns an error when the app cannot be resolved, preflight fails, an
/// output path is not valid UTF-8, or the actions file is unreadable or is not
/// a JSON array.
pub fn plan_app_inspection(start: &Path, request: InspectRequest) -> Result<CargoPlan> {
    let app_dir = resolve_app_dir(start)?;
    let mut payload = serde_json::Map::new();
    payload.insert("inspect".into(), json_path(&request.output)?);
    payload.insert(
        "actions".into(),
        request.actions.as_deref().map_or_else(
            || Ok(serde_json::Value::Array(Vec::new())),
            |path| {
                let path = if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    app_dir.join(path)
                };
                read_actions(&path)
            },
        )?,
    );
    if let Some((width, height)) = request.viewport {
        payload.insert("viewport".into(), serde_json::json!([width, height]));
    }
    if let Some(path) = request.dump {
        payload.insert("dump".into(), json_path(&path)?);
    }
    if let Some(path) = request.targets {
        payload.insert("targets".into(), json_path(&path)?);
    }
    if request.audit {
        payload.insert("audit".into(), serde_json::Value::Bool(true));
    }
    if let Some(path) = request.audit_report {
        payload.insert("audit_report".into(), json_path(&path)?);
    }
    let app_args = [
        OsString::from("--headless-drive"),
        OsString::from(serde_json::Value::Object(payload).to_string()),
    ];
    plan_app_task(&app_dir, AppTask::Run, &app_args)
}

fn json_path(path: &Path) -> Result<serde_json::Value> {
    path.to_str()
        .map(|path| serde_json::Value::String(path.to_owned()))
        .with_context(|| format!("path is not valid UTF-8: {}", path.display()))
}

fn read_actions(path: &Path) -> Result<serde_json::Value> {
    let source = std::fs::read_to_string(path)
        .with_context(|| format!("reading inspection actions {}", path.display()))?;
    tilo_headless_schema::parse_actions(&source).map_err(|error| {
        anyhow::anyhow!("validating inspection actions {}: {error}", path.display())
    })?;
    let actions: serde_json::Value = serde_json::from_str(&source)
        .with_context(|| format!("parsing inspection actions {}", path.display()))?;
    Ok(actions)
}

/// Discover and preflight one or every saved headless scenario.
///
/// Every action file is parsed before the returned plan can start the app, so
/// an invalid later scenario never leaves a partially executed suite.
///
/// # Errors
///
/// Returns an error when the app cannot be resolved, its structural preflight
/// fails, a requested scenario is absent, or any action file is malformed.
pub fn plan_scenario_suite(request: &ScenarioRunRequest) -> Result<ScenarioSuitePlan> {
    let app_dir = resolve_app_dir(&request.start)?;
    let artifact_dir = request.artifacts.as_ref().map(|path| {
        if path.is_absolute() {
            path.clone()
        } else {
            app_dir.join(path)
        }
    });
    let scenarios = discover_scenarios(&app_dir, request.name.as_deref())?;
    if scenarios.is_empty() {
        bail!("no headless scenarios found; add one with `tilo-app add scenario smoke`");
    }
    let mut cases = Vec::with_capacity(scenarios.len());
    for (name, source) in scenarios {
        let actions = read_actions(&source)?;
        let mut payload = serde_json::Map::new();
        payload.insert("actions".into(), actions);
        if let Some((width, height)) = request.viewport {
            payload.insert("viewport".into(), serde_json::json!([width, height]));
        }
        if request.audit {
            payload.insert("audit".into(), serde_json::Value::Bool(true));
        }
        if let Some(directory) = &artifact_dir {
            payload.insert(
                "inspect".into(),
                json_path(&directory.join(format!("{name}.inspect.json")))?,
            );
            payload.insert(
                "targets".into(),
                json_path(&directory.join(format!("{name}.targets.json")))?,
            );
            payload.insert(
                "dump".into(),
                json_path(&directory.join(format!("{name}.document.json")))?,
            );
            if request.audit {
                payload.insert(
                    "audit_report".into(),
                    json_path(&directory.join(format!("{name}.audit.json")))?,
                );
            }
        }
        cases.push(ScenarioCasePlan {
            name,
            source,
            args: vec![
                OsString::from("--headless-drive"),
                OsString::from(serde_json::Value::Object(payload).to_string()),
            ],
        });
    }
    let report = doctor_resolved_app(app_dir.clone());
    if !report.is_ready() {
        bail!("app preflight failed\n\n{}", report.summary());
    }
    let bin = load_app_package_spec(app_dir.clone())
        .ok()
        .and_then(|spec| spec.gui_bin);
    let mut build_args = vec![
        OsString::from("build"),
        OsString::from("--manifest-path"),
        app_dir.join("Cargo.toml").into_os_string(),
        OsString::from("--message-format=json-render-diagnostics"),
    ];
    match &bin {
        Some(bin) => build_args.extend([OsString::from("--bin"), OsString::from(bin)]),
        None => build_args.push(OsString::from("--bins")),
    }
    Ok(ScenarioSuitePlan {
        build: CargoPlan {
            app_dir: app_dir.clone(),
            program: std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo")),
            args: build_args,
        },
        bin,
        app_dir,
        cases,
        artifact_dir,
    })
}

fn discover_scenarios(app_dir: &Path, requested: Option<&str>) -> Result<Vec<(String, PathBuf)>> {
    if let Some(name) = requested {
        if name == "default" {
            let path = app_dir.join("tests/headless-actions.json");
            if !path.is_file() {
                bail!("default scenario tests/headless-actions.json does not exist");
            }
            return Ok(vec![(name.to_owned(), path)]);
        }
        validate_artifact_name(name)?;
        let path = app_dir.join("tests/scenarios").join(format!("{name}.json"));
        if !path.is_file() {
            bail!("scenario {name:?} does not exist at {}", path.display());
        }
        return Ok(vec![(name.to_owned(), path)]);
    }

    let mut scenarios = Vec::new();
    let conventional = app_dir.join("tests/headless-actions.json");
    if conventional.is_file() {
        scenarios.push(("default".to_owned(), conventional));
    }
    let named_start = scenarios.len();
    for path in files_with_extension(&app_dir.join("tests/scenarios"), "json")
        .map_err(anyhow::Error::msg)?
    {
        let name = path
            .file_stem()
            .and_then(std::ffi::OsStr::to_str)
            .ok_or_else(|| anyhow::anyhow!("scenario filename is not valid UTF-8"))?;
        validate_artifact_name(name)?;
        if name == "default" {
            bail!("tests/scenarios/default.json conflicts with the reserved default scenario");
        }
        scenarios.push((name.to_owned(), path));
    }
    scenarios
        .get_mut(named_start..)
        .unwrap_or_default()
        .sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    Ok(scenarios)
}

impl ScenarioSuitePlan {
    #[must_use]
    pub fn len(&self) -> usize {
        self.cases.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cases.is_empty()
    }

    /// Directory receiving requested per-scenario inspection artifacts.
    #[must_use]
    pub fn artifact_dir(&self) -> Option<&Path> {
        self.artifact_dir.as_deref()
    }

    /// Build the app, then run every prevalidated case against the binary in
    /// deterministic order. Each case receives private XDG directories and
    /// cannot read or overwrite normal app state. Failures are accumulated so
    /// one run reports the complete suite.
    ///
    /// # Errors
    ///
    /// Returns an error when the build fails or reports no app binary, or an
    /// isolated state directory or child process cannot be created. Non-zero
    /// scenario exits are returned in `failed`.
    pub fn execute(&self) -> Result<ScenarioSuiteOutcome> {
        let program = self.build_app()?;
        self.run_cases(&program)
    }

    fn build_app(&self) -> Result<PathBuf> {
        let mut child = ProcessCommand::new(&self.build.program)
            .args(&self.build.args)
            .current_dir(&self.build.app_dir)
            .stdout(Stdio::piped())
            .spawn()
            .with_context(|| format!("starting {} build", self.build.program.to_string_lossy()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Cargo build has no artifact stream"))?;
        let manifest = self.app_dir.join("Cargo.toml");
        let manifest = manifest.canonicalize().unwrap_or(manifest);
        let mut artifacts = HashMap::new();
        for line in BufReader::new(stdout).lines() {
            let line = line.context("reading Cargo artifact stream")?;
            collect_cargo_message(&line, &manifest, &mut artifacts);
        }
        let status = child.wait().context("waiting for Cargo build")?;
        if !status.success() {
            bail!("building the app for its scenarios failed with {status}");
        }
        if let Some(bin) = &self.bin {
            return artifacts.remove(bin).ok_or_else(|| {
                anyhow::anyhow!("Cargo build completed without reporting binary {bin}")
            });
        }
        let mut executables = artifacts
            .into_values()
            .filter(|path| path.extension().is_none_or(|extension| extension != "so"));
        match (executables.next(), executables.next()) {
            (Some(executable), None) => Ok(executable),
            (None, _) => bail!("Cargo build reported no app binary"),
            (Some(_), Some(_)) => bail!(
                "the app builds several binaries; name the GUI one in tilo-app.toml's entrypoint"
            ),
        }
    }

    fn run_cases(&self, program: &Path) -> Result<ScenarioSuiteOutcome> {
        if let Some(directory) = &self.artifact_dir {
            std::fs::create_dir_all(directory).with_context(|| {
                format!(
                    "creating scenario artifact directory {}",
                    directory.display()
                )
            })?;
        }
        let mut passed = Vec::new();
        let mut failed = Vec::new();
        for case in &self.cases {
            eprintln!("[scenario] {} ({})", case.name, case.source.display());
            let state = tempfile::Builder::new()
                .prefix("tilo-scenario-")
                .tempdir()
                .with_context(|| format!("creating isolated state for scenario {:?}", case.name))?;
            let status = ProcessCommand::new(program)
                .args(&case.args)
                .current_dir(&self.app_dir)
                .env("XDG_DATA_HOME", state.path().join("data"))
                .env("XDG_STATE_HOME", state.path().join("state"))
                .env("XDG_CONFIG_HOME", state.path().join("config"))
                .env("XDG_CACHE_HOME", state.path().join("cache"))
                .env_remove("TILO_PACKAGE_ID")
                .env_remove("TILO_WORKSPACE_ID")
                .env_remove("QUICK_CLUSTER_ID")
                .status()
                .with_context(|| format!("starting scenario {:?}", case.name))?;
            if status.success() {
                passed.push(case.name.clone());
            } else {
                failed.push(ScenarioFailure {
                    name: case.name.clone(),
                    source: case.source.clone(),
                    detail: status.to_string(),
                });
            }
        }
        Ok(ScenarioSuiteOutcome { passed, failed })
    }
}

#[derive(Debug)]
struct AppPackageSpec {
    app_dir: PathBuf,
    cargo_package: String,
    manifest_path: PathBuf,
    entrypoints: Vec<(String, PathBuf)>,
    /// The Cargo binary of the GUI entrypoint, the one a desktop build ships.
    gui_bin: Option<String>,
    thin_feature: bool,
}

/// Build an app's declared entrypoints, assemble its payload, sign it through
/// `tilo-app-package`, verify the result, and publish the completed `.tapp` atomically.
///
/// An app-local `payload/` directory is copied into the package before the
/// freshly built binaries are placed at their manifest paths. Relative output
/// and identity paths are resolved from the discovered app root.
///
/// # Errors
///
/// Returns an error when preflight, compilation, staging, packing, or
/// verification fails, or when the destination exists without `force`.
pub fn package_app(start: &Path, request: &PackageRequest) -> Result<PackageOutcome> {
    native_sdk::validate_request(request)?;
    if request.platform == PackagePlatform::Web {
        return web::package_web(start, request);
    }
    if request.platform == PackagePlatform::Android {
        if request.arch.is_none() {
            bail!("an Android package is one architecture here; pass --arch aarch64 or x86_64");
        }
        return android::package(start, request, &android::PackageOptions::default())?
            .pop()
            .ok_or_else(|| anyhow::anyhow!("no Android package was built"));
    }
    let NativeTarget {
        spec,
        desktop,
        desktop_arch,
        profile,
        shipped,
        triple,
    } = NativeTarget::resolve(start, request)?;
    let manifest_source = std::fs::read_to_string(&spec.manifest_path)
        .with_context(|| format!("reading {}", spec.manifest_path.display()))?;
    // `[budget]` bounds the thin Tilo OS build; a desktop build carries the
    // whole toolkit and answers to the registry's desktop limit instead.
    let budget = if desktop {
        size::Budget::default()
    } else {
        size::Budget::from_manifest(&manifest_source)
            .with_context(|| format!("reading [budget] from {}", spec.manifest_path.display()))?
    };
    let output = package_output(&spec, request, desktop_arch)?;
    let output_parent = output
        .parent()
        .ok_or_else(|| anyhow::anyhow!("package output has no parent: {}", output.display()))?;
    let built = build_entrypoints(
        &spec,
        &BuildPlan {
            bins: &shipped,
            triple: triple.as_deref(),
            thin: !desktop && !request.static_link,
            profile: &profile,
        },
        request,
    )?;
    let staging = tempfile::Builder::new()
        .prefix(".tilo-app-package-")
        .tempdir_in(output_parent)
        .with_context(|| {
            format!(
                "creating package staging area in {}",
                output_parent.display()
            )
        })?;
    let payload = staging.path().join("payload");
    stage_payload(&spec, &shipped, &built, &payload)?;

    let staged_package = staging.path().join("package.tapp");
    let identity = signing_identity(&spec.app_dir, request.identity.as_deref())?;
    let resource_kinds = package_resources(&spec, request, &built, triple.as_deref())?;
    let mut manifest = signed_manifest(
        &spec,
        request,
        &manifest_source,
        desktop_arch,
        triple.as_deref(),
        &shipped,
    )?;
    if let Some(selection) = &request.sdk {
        manifest.package.runtime = Some(native_sdk::bind_staged(
            selection,
            built,
            (&payload, &shipped),
            (&spec.app_dir, triple.as_deref()),
        )?);
    }
    let packed = pack::pack_manifest(
        manifest,
        resource_kinds,
        &payload,
        &staged_package,
        &identity,
    )?;
    let entrypoints: Vec<PathBuf> = shipped.iter().map(|(_, path)| path.clone()).collect();
    let size = within_budget(
        &payload,
        &entrypoints,
        packed.bytes,
        (budget, request.platform),
        &output,
    )?;
    std::fs::rename(&staged_package, &output)
        .with_context(|| format!("publishing package to {}", output.display()))?;
    eprintln!(
        "[tilo-app] packaged {} ({}, {} bytes)",
        output.display(),
        packed.id,
        packed.bytes
    );
    Ok(PackageOutcome {
        output,
        entrypoints,
        size,
    })
}

/// What a native `package` or `build` compiles, for which triple, and how.
struct NativeTarget {
    spec: AppPackageSpec,
    /// A self-contained executable for a system other than Tilo OS.
    desktop: bool,
    /// The architecture a desktop build is for; `None` for Tilo OS.
    desktop_arch: Option<PackageArch>,
    profile: String,
    /// The Cargo binaries to build and their payload paths.
    shipped: Vec<(String, PathBuf)>,
    /// `None` builds for the host.
    triple: Option<String>,
}

impl NativeTarget {
    fn resolve(start: &Path, request: &PackageRequest) -> Result<Self> {
        let app_dir = resolve_app_dir(start)?;
        let report = doctor_resolved_app(app_dir.clone());
        if !report.is_ready() {
            bail!("app preflight failed\n\n{}", report.summary());
        }
        let spec = load_app_package_spec(app_dir)?;
        let desktop = request.platform != PackagePlatform::Tilo;
        if !desktop && !request.static_link && !spec.thin_feature {
            bail!(
                "{} has no `thin` feature, so it cannot link against the toolkit the OS ships; \
                 add `thin = [\"dep:runtime-dylib\"]` to [features] (as `tilo-app new` does) or pass --static",
                spec.app_dir.join("Cargo.toml").display()
            );
        }
        let profile = package_profile(request)?;
        let desktop_arch = match (desktop, request.arch.or_else(PackageArch::host)) {
            (false, _) => None,
            (true, Some(arch)) => Some(arch),
            (true, None) => bail!(
                "cannot tell which architecture to build for on {}; pass --arch",
                std::env::consts::ARCH
            ),
        };
        let shipped = shipped_binaries(&spec, request, desktop)?;
        let triple = request.target.clone().or_else(|| {
            desktop_arch
                .or(request.arch)
                .map(|arch| request.platform.triple(arch))
        });
        Ok(Self {
            spec,
            desktop,
            desktop_arch,
            profile,
            shipped,
            triple,
        })
    }
}

/// A native build `tilo-app build` left in Cargo's target directory.
#[derive(Debug)]
pub struct NativeBuild {
    pub app_dir: PathBuf,
    pub platform: PackagePlatform,
    /// `None` built for the host.
    pub triple: Option<String>,
    pub profile: String,
    /// Each Cargo binary built and where Cargo left it.
    pub executables: Vec<(String, PathBuf)>,
    /// Exact toolkit path emitted by Cargo when using the canonical SDK graph.
    pub sdk_toolkit: Option<PathBuf>,
    pub elapsed: Duration,
}

impl NativeBuild {
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "app_dir": self.app_dir,
            "platform": self.platform.cli_name(),
            "target": self.triple,
            "profile": self.profile,
            "sdk_toolkit": self.sdk_toolkit,
            "executables": self
                .executables
                .iter()
                .map(|(bin, path)| serde_json::json!({"bin": bin, "path": path}))
                .collect::<Vec<_>>(),
            "elapsed_ms": u64::try_from(self.elapsed.as_millis()).unwrap_or(u64::MAX),
        })
    }
}

/// Compile an app's entrypoints for a native platform exactly as
/// [`package_app`] does, without assembling or signing a package: thin for
/// Tilo OS unless `static_link`, one self-contained executable otherwise.
///
/// # Errors
///
/// Returns an error for the web platform (see [`web::build_web`]), when
/// preflight fails, or when Cargo does.
pub fn build_native(start: &Path, request: &PackageRequest) -> Result<NativeBuild> {
    native_sdk::validate_request(request)?;
    if request.platform == PackagePlatform::Web {
        bail!("a web build is `tilo-app build --platform web`, not a native build");
    }
    if request.platform == PackagePlatform::Android {
        bail!("an Android build is `android::build`: an APK, not executables");
    }
    let started = Instant::now();
    let target = NativeTarget::resolve(start, request)?;
    let built = build_entrypoints(
        &target.spec,
        &BuildPlan {
            bins: &target.shipped,
            triple: target.triple.as_deref(),
            thin: !target.desktop && !request.static_link,
            profile: &target.profile,
        },
        request,
    )?;
    let executables = target
        .shipped
        .iter()
        .map(|(bin, _)| {
            built
                .get(bin)
                .map(|path| (bin.clone(), path.clone()))
                .ok_or_else(|| anyhow::anyhow!("Cargo did not report binary {bin}"))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(NativeBuild {
        app_dir: target.spec.app_dir,
        platform: request.platform,
        triple: target.triple,
        profile: target.profile,
        executables,
        sdk_toolkit: built.get("sdk:runtime_dylib").cloned(),
        elapsed: started.elapsed(),
    })
}

/// Where the package goes: `--output`, or `dist/<package>.tapp` (with the
/// platform and architecture for a desktop build). Refuses to replace an
/// existing file without `force`, and creates the directory.
fn package_resources(
    spec: &AppPackageSpec,
    request: &PackageRequest,
    built: &HashMap<String, PathBuf>,
    triple: Option<&str>,
) -> Result<Vec<String>> {
    match request.sdk.as_ref().filter(|sdk| sdk.compiler.is_some()) {
        Some(sdk) => native_sdk::compiler::declared_resources(sdk, built, &spec.app_dir, triple),
        None => resource_kinds::declared_by_app(&spec.app_dir),
    }
}

fn package_output(
    spec: &AppPackageSpec,
    request: &PackageRequest,
    desktop_arch: Option<PackageArch>,
) -> Result<PathBuf> {
    let output = request.output.as_ref().map_or_else(
        || {
            let name = match desktop_arch {
                Some(arch) => format!(
                    "{}-{}-{}.tapp",
                    spec.cargo_package,
                    request.platform.registry_name(),
                    arch.as_str()
                ),
                None => format!("{}.tapp", spec.cargo_package),
            };
            spec.app_dir.join("dist").join(name)
        },
        |path| absolute_from(&spec.app_dir, path),
    );
    if output.exists() && !request.force {
        bail!(
            "package output {} already exists; pass --force to replace it",
            output.display()
        );
    }
    let parent = output
        .parent()
        .ok_or_else(|| anyhow::anyhow!("package output has no parent: {}", output.display()))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("creating package output directory {}", parent.display()))?;
    Ok(output)
}

/// The Cargo binaries a package ships and their payload paths: every declared
/// entrypoint for Tilo OS, the GUI executable alone for a desktop build.
fn shipped_binaries(
    spec: &AppPackageSpec,
    request: &PackageRequest,
    desktop: bool,
) -> Result<Vec<(String, PathBuf)>> {
    if !desktop {
        return Ok(spec.entrypoints.clone());
    }
    let bin = spec.gui_bin.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "{} has no GUI entrypoint; a {} build ships one executable",
            spec.manifest_path.display(),
            request.platform.registry_name()
        )
    })?;
    let path = PathBuf::from(format!("bin/{bin}{}", request.platform.executable_suffix()));
    Ok(vec![(bin, path)])
}

/// Fill `payload` with the app's `payload/` tree, its committed surface and
/// the built binaries.
fn stage_payload(
    spec: &AppPackageSpec,
    shipped: &[(String, PathBuf)],
    built: &HashMap<String, PathBuf>,
    payload: &Path,
) -> Result<()> {
    std::fs::create_dir(payload).context("creating staged payload")?;
    let app_payload = spec.app_dir.join("payload");
    if app_payload.is_dir() {
        copy_payload_tree(&app_payload, payload)?;
    }
    // The committed public surface ships at the package root: what the OS
    // offers of the app while it is not running.
    let surface = spec.app_dir.join(surface::FILE);
    if surface.is_file() {
        std::fs::copy(&surface, payload.join(surface::FILE))
            .with_context(|| format!("copying {}", surface.display()))?;
    }
    for (cargo_bin, payload_path) in shipped {
        let source = built
            .get(cargo_bin)
            .ok_or_else(|| anyhow::anyhow!("Cargo did not report binary {cargo_bin}"))?;
        let destination = payload.join(payload_path);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating payload directory {}", parent.display()))?;
        }
        std::fs::copy(source, &destination).with_context(|| {
            format!("copying {} to {}", source.display(), destination.display())
        })?;
        let permissions = std::fs::metadata(source)?.permissions();
        std::fs::set_permissions(&destination, permissions)?;
    }
    Ok(())
}

/// The authored manifest around a desktop build's executable at `module`.
fn desktop_manifest(
    spec: &AppPackageSpec,
    platform: PackagePlatform,
    arch: PackageArch,
    manifest_source: &str,
    module: &Path,
) -> Result<tilo_app_package::Manifest> {
    let authored = tilo_app_package::Manifest::from_toml_str(manifest_source)
        .with_context(|| format!("parsing {}", spec.manifest_path.display()))?;
    tilo_app_package::desktop::desktop_manifest(
        &authored,
        platform.registry_name(),
        arch.as_str(),
        arch.as_str(),
        &module.to_string_lossy(),
    )
    .map_err(|error| {
        anyhow::anyhow!(
            "{}: {error}; a {} build declares the toolkit release it links as [package].runtime",
            spec.manifest_path.display(),
            platform.registry_name()
        )
    })
}

/// The manifest `tilo-app package` signs for this build.
fn signed_manifest(
    spec: &AppPackageSpec,
    request: &PackageRequest,
    manifest_source: &str,
    desktop_arch: Option<PackageArch>,
    triple: Option<&str>,
    shipped: &[(String, PathBuf)],
) -> Result<tilo_app_package::Manifest> {
    if let (Some(arch), Some((_, module))) = (desktop_arch, shipped.first()) {
        return desktop_manifest(spec, request.platform, arch, manifest_source, module);
    }
    let arch = request
        .arch
        .or_else(|| triple.and_then(PackageArch::of_triple))
        .or_else(PackageArch::host)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "cannot tell which architecture to build for on {}; pass --arch",
                std::env::consts::ARCH
            )
        })?;
    tilo_manifest(spec, manifest_source, arch)
}

/// The authored manifest, declaring exactly the architecture this build is
/// for: the level it names for that architecture, else the floor the
/// scaffolded `.cargo/config.toml` builds at (`x86-64-v3`, generic ARMv8-A).
/// A package never claims a build it does not carry.
fn tilo_manifest(
    spec: &AppPackageSpec,
    manifest_source: &str,
    arch: PackageArch,
) -> Result<tilo_app_package::Manifest> {
    use tilo_app_package::manifest::{AArch64Level, IsaRequirements, X86_64Level};
    let mut manifest = tilo_app_package::Manifest::from_toml_str(manifest_source)
        .with_context(|| format!("parsing {}", spec.manifest_path.display()))?;
    let declared = manifest.package.isa;
    let undeclared = declared.x86_64.is_none() && declared.aarch64.is_none();
    manifest.package.isa = match arch {
        PackageArch::X86_64 => IsaRequirements {
            x86_64: declared
                .x86_64
                .or_else(|| undeclared.then_some(X86_64Level::V3)),
            aarch64: None,
        },
        PackageArch::Aarch64 => IsaRequirements {
            x86_64: None,
            aarch64: declared
                .aarch64
                .or_else(|| undeclared.then_some(AArch64Level::Armv8A)),
        },
    };
    if manifest.package.isa.x86_64.is_none() && manifest.package.isa.aarch64.is_none() {
        bail!(
            "{}: [package.isa] names other architectures, not {}, which this build is for",
            spec.manifest_path.display(),
            arch.as_str()
        );
    }
    Ok(manifest)
}

fn package_profile(request: &PackageRequest) -> Result<String> {
    let profile = request.profile.clone().unwrap_or_else(|| {
        if request.static_link || request.platform != PackagePlatform::Tilo {
            "release"
        } else {
            "thin-app"
        }
        .to_owned()
    });
    validate_profile_name(&profile)?;
    Ok(profile)
}

/// Prints where the staged package's bytes go and refuses it when it exceeds
/// the app's `[budget]`, before anything replaces `output`.
fn within_budget(
    payload: &Path,
    entrypoints: &[PathBuf],
    archive: u64,
    (budget, platform): (size::Budget, PackagePlatform),
    output: &Path,
) -> Result<size::SizeReport> {
    let limit = tilo_app_package::platform_size_limit(platform.registry_name());
    let size = size::SizeReport::measure(payload, entrypoints, archive, budget, limit)?;
    eprintln!("{}", size.render());
    let overruns = size.overruns();
    if !overruns.is_empty() {
        let kept = if output.exists() {
            "the previous package was kept"
        } else {
            "nothing was written"
        };
        bail!(
            "over budget, so {kept} at {}:\n  {}",
            output.display(),
            overruns.join("\n  ")
        );
    }
    Ok(size)
}

/// The publisher identity at `path`, relative to the app, or an ephemeral
/// one when none is given.
fn signing_identity(
    app_dir: &Path,
    path: Option<&Path>,
) -> Result<tilo_app_package::SigningIdentity> {
    let Some(path) = path else {
        if let Some(key) = registry_client::key::discover(&registry_client::key::machine_store())? {
            eprintln!(
                "[tilo-app] signing with publisher key {}",
                registry_client::key::public_hex(&key)
            );
            return Ok(tilo_app_package::SigningIdentity::from_seed(
                &key.to_bytes(),
            ));
        }
        eprintln!(
            "[tilo-app] no --identity or publisher key; signing with an ephemeral key the registry will not accept"
        );
        return Ok(tilo_app_package::SigningIdentity::generate());
    };
    pack::load_identity(&absolute_from(app_dir, path))
}

fn load_app_package_spec(app_dir: PathBuf) -> Result<AppPackageSpec> {
    let cargo_path = app_dir.join("Cargo.toml");
    let cargo_source = std::fs::read_to_string(&cargo_path)
        .with_context(|| format!("reading {}", cargo_path.display()))?;
    let cargo: toml::Table = cargo_source
        .parse()
        .with_context(|| format!("parsing {}", cargo_path.display()))?;
    let cargo_package = cargo
        .get("package")
        .and_then(|package| package.get("name"))
        .and_then(toml::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("Cargo.toml is missing [package].name"))?
        .to_owned();
    let thin_feature = cargo
        .get("features")
        .and_then(|features| features.get("thin"))
        .is_some();

    let manifest_path = app_dir.join("tilo-app.toml");
    let manifest_source = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    let manifest = tilo_app_package::Manifest::from_toml_str(&manifest_source)
        .with_context(|| format!("parsing {}", manifest_path.display()))?;
    let entrypoint = manifest
        .entrypoint
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("tilo-app.toml declares no package entrypoint"))?;
    let gui_bin = entrypoint
        .gui_bin_path()
        .and_then(|path| path.file_name())
        .and_then(|name| name.to_str())
        .map(str::to_owned);
    let mut payload_paths = Vec::new();
    if let Some(path) = entrypoint.gui_bin_path() {
        payload_paths.push(path.clone());
    }
    if let Some(path) = entrypoint.cli_bin_path()
        && !payload_paths.contains(path)
    {
        payload_paths.push(path.clone());
    }
    if payload_paths.is_empty() {
        bail!("tilo-app.toml declares no package entrypoint");
    }
    let entrypoints = payload_paths
        .into_iter()
        .map(|payload_path| {
            let cargo_bin = payload_path
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "entrypoint {} has no UTF-8 binary name",
                        payload_path.display()
                    )
                })?
                .to_owned();
            Ok((cargo_bin, payload_path))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(AppPackageSpec {
        app_dir,
        cargo_package,
        manifest_path,
        entrypoints,
        gui_bin,
        thin_feature,
    })
}

/// What [`build_entrypoints`] compiles: `bins` for `triple` (the host when
/// `None`), linked against the OS toolkit when `thin`.
struct BuildPlan<'a> {
    bins: &'a [(String, PathBuf)],
    triple: Option<&'a str>,
    thin: bool,
    profile: &'a str,
}

fn build_entrypoints(
    spec: &AppPackageSpec,
    plan: &BuildPlan<'_>,
    request: &PackageRequest,
) -> Result<HashMap<String, PathBuf>> {
    if request
        .sdk
        .as_ref()
        .is_some_and(|sdk| sdk.compiler.is_some())
    {
        return native_sdk::compiler::build(spec, plan, request);
    }
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let mut command = ProcessCommand::new(&cargo);
    command
        .arg("build")
        .arg("--manifest-path")
        .arg(spec.app_dir.join("Cargo.toml"))
        .arg("--profile")
        .arg(plan.profile)
        .arg("--message-format=json-render-diagnostics")
        .current_dir(&spec.app_dir)
        .stdout(Stdio::piped());
    for (cargo_bin, _) in plan.bins {
        command.arg("--bin").arg(cargo_bin);
    }
    let mut features = request.features.clone();
    if request.sdk.is_some() {
        let root = find_repo_root(&spec.app_dir)
            .filter(|_| app_is_workspace_member(&spec.app_dir))
            .context("native SDK packaging requires a checkout workspace app; scaffold with tilo-app new in the SDK checkout")?;
        features = native_sdk::configure_graph(
            &mut command,
            &root,
            plan.profile,
            &spec.cargo_package,
            &features,
        )?;
        if let Some(target) = plan.triple {
            command.arg("--target").arg(target);
        }
    } else if plan.thin {
        let triple = if let Some(target) = plan.triple {
            target.to_owned()
        } else {
            host_triple(&spec.app_dir)?
        };
        let (key, flags) = prefer_dynamic_rustflags(&triple);
        command.arg("--target").arg(&triple).env(key, flags);
        if !features.iter().any(|feature| feature == "thin") {
            features.push("thin".to_owned());
        }
    } else if let Some(target) = plan.triple {
        command.arg("--target").arg(target);
    }
    if request.no_default_features {
        command.arg("--no-default-features");
    }
    if !features.is_empty() {
        command.arg("--features").arg(features.join(","));
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("starting {} build", cargo.to_string_lossy()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("Cargo build has no artifact stream"))?;
    let expected_manifest = spec
        .app_dir
        .join("Cargo.toml")
        .canonicalize()
        .unwrap_or_else(|_| spec.app_dir.join("Cargo.toml"));
    let mut artifacts = HashMap::new();
    for line in BufReader::new(stdout).lines() {
        let line = line.context("reading Cargo artifact stream")?;
        collect_cargo_message(&line, &expected_manifest, &mut artifacts);
        if request.sdk.is_some() {
            native_sdk::collect_toolkit(&line, &spec.app_dir, &mut artifacts)?;
        }
    }
    let status = child.wait().context("waiting for Cargo build")?;
    if !status.success() {
        // Asked only now: Cargo may have compiled on another machine, which
        // needs none of this one's development files.
        if plan.thin
            && plan.triple.is_none()
            && let Some(hint) = native_libs::thin_build_failure_hint()
        {
            bail!("Cargo package build failed with {status}\n{hint}");
        }
        bail!("Cargo package build failed with {status}");
    }
    for (cargo_bin, _) in plan.bins {
        if !artifacts.contains_key(cargo_bin) {
            bail!("Cargo build completed without reporting binary {cargo_bin}");
        }
    }
    Ok(artifacts)
}

fn collect_cargo_message(
    line: &str,
    expected_manifest: &Path,
    artifacts: &mut HashMap<String, PathBuf>,
) {
    let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else {
        eprintln!("{line}");
        return;
    };
    if message.get("reason").and_then(serde_json::Value::as_str) == Some("compiler-message") {
        if let Some(rendered) = message
            .get("message")
            .and_then(|message| message.get("rendered"))
            .and_then(serde_json::Value::as_str)
        {
            eprint!("{rendered}");
        }
        return;
    }
    if message.get("reason").and_then(serde_json::Value::as_str) != Some("compiler-artifact") {
        return;
    }
    let manifest_matches = message
        .get("manifest_path")
        .and_then(serde_json::Value::as_str)
        .map(Path::new)
        .is_some_and(|path| path == expected_manifest);
    if !manifest_matches {
        return;
    }
    let Some(name) = message
        .get("target")
        .and_then(|target| target.get("name"))
        .and_then(serde_json::Value::as_str)
    else {
        return;
    };
    if let Some(executable) = message
        .get("executable")
        .and_then(serde_json::Value::as_str)
    {
        artifacts.insert(name.to_owned(), PathBuf::from(executable));
    }
    let Some(files) = message
        .get("filenames")
        .and_then(serde_json::Value::as_array)
    else {
        return;
    };
    for file in files {
        let Some(path) = file.as_str() else {
            continue;
        };
        if Path::new(path)
            .extension()
            .is_some_and(|extension| extension == "so")
        {
            artifacts.insert(format!("dylib:{name}"), PathBuf::from(path));
        }
    }
}

fn copy_payload_tree(source: &Path, destination: &Path) -> Result<()> {
    for entry in std::fs::read_dir(source)
        .with_context(|| format!("reading payload overlay {}", source.display()))?
    {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = destination.join(entry.file_name());
        if file_type.is_symlink() {
            bail!(
                "payload overlay contains unsupported symlink {}",
                entry.path().display()
            );
        }
        if file_type.is_dir() {
            std::fs::create_dir_all(&target)?;
            copy_payload_tree(&entry.path(), &target)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), &target)?;
            std::fs::set_permissions(&target, entry.metadata()?.permissions())?;
        }
    }
    Ok(())
}

fn absolute_from(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

fn validate_profile_name(profile: &str) -> Result<()> {
    if profile.is_empty()
        || !profile
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("invalid Cargo profile {profile:?}");
    }
    Ok(())
}

static DEV_STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Run an app and restart it after relevant source or asset changes.
///
/// The app and Cargo children share a dedicated process group, so every
/// restart and Ctrl-C stops the previous application before starting another.
/// After the initial launch, changes are compiled while the last good app
/// remains alive. An app that links the shared toolkit reloads each open
/// window in that same process. Any other app is replaced by the new binary.
/// A failed build leaves the running app and the watcher alive for the next edit.
///
/// # Errors
///
/// Returns an error when the app cannot be resolved, its initial preflight
/// fails, signal handlers cannot be installed, or filesystem watching cannot
/// be started. On success, returns one timing record per reload when
/// `request.timings` is set.
pub fn dev_app(start: &Path, request: &DevRequest) -> Result<Vec<serde_json::Value>> {
    let app_dir = resolve_app_dir(start)?;
    let initial = dev_run_plan(&app_dir, request)?;
    let roots = dev_watch_roots(&app_dir, request.watch_ui)?;
    let (event_tx, event_rx) = mpsc::channel();
    let source_revision = Arc::new(AtomicU64::new(0));
    let callback_revision = Arc::clone(&source_revision);
    let callback_roots = roots.clone();
    let watcher: RecommendedWatcher =
        notify::recommended_watcher(move |event: notify::Result<Event>| {
            if event
                .as_ref()
                .is_ok_and(|event| event_requires_restart(event, &callback_roots))
            {
                callback_revision.fetch_add(1, Ordering::Release);
            }
            let _ = event_tx.send(event);
        })
        .context("creating app file watcher")?;
    let mut watcher = DevWatcher::new(watcher);
    for root in &roots {
        watcher.watch_tree(root)?;
    }
    let _signals = DevSignalGuard::install()?;
    provide_runtime_host(&app_dir);
    let _runtime = DevRuntimeGuard;
    eprintln!(
        "[tilo-app] watching {}",
        roots
            .iter()
            .map(|root| root.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    // inotify does not cross every bind mount, and a busy editor keeps the
    // event channel from ever going idle. Stamp polls run on a timer either way.
    let mut sources = snapshot_restart_sources(&roots).unwrap_or_default();
    let mut next_source_poll = Instant::now();
    let mut dev = dev_reload::DevLoop::new(
        dev_session_dir(&app_dir),
        request.timings,
        Arc::clone(&source_revision),
        request.profile.clone(),
    );
    // The first process has to be the prefer-dynamic host. A static binary
    // links its own `ui`, so the library built on the next save cannot run
    // inside those windows.
    let mut child = if reloads_in_place(&app_dir, request.profile()) {
        None
    } else {
        spawn_dev_child_reported(&initial)
    };
    if child.is_none()
        && reloads_in_place(&app_dir, request.profile())
        && rebuild_running_dev_app(
            &initial,
            request,
            &mut child,
            &event_rx,
            &roots,
            &mut watcher,
            &mut sources,
            &mut dev,
        )
    {
        stop_dev_child(&mut child);
        eprintln!("[tilo-app] stopped");
        return Ok(Vec::new());
    }
    'watching: while !DEV_STOP_REQUESTED.load(Ordering::Relaxed) {
        match watch_tick(
            &event_rx,
            &roots,
            &mut watcher,
            &mut sources,
            &mut next_source_poll,
        ) {
            WatchTick::Stop => bail!("app file watcher stopped"),
            WatchTick::Rebuild { bump_revision } => {
                dev.changed_at = Some(SystemTime::now());
                if bump_revision {
                    source_revision.fetch_add(1, Ordering::Release);
                }
                if rebuild_running_dev_app(
                    &initial,
                    request,
                    &mut child,
                    &event_rx,
                    &roots,
                    &mut watcher,
                    &mut sources,
                    &mut dev,
                ) {
                    break 'watching;
                }
            }
            WatchTick::Idle => {}
        }
        dev.poll();
        report_child_exit(&mut child);
    }
    dev.finish(Duration::from_secs(2));
    stop_dev_child(&mut child);
    eprintln!("[tilo-app] stopped");
    Ok(std::mem::take(&mut dev.reports))
}

/// Rebuild until the candidate matches the newest source, or the user stops.
/// Returns whether the watcher itself should stop.
fn rebuild_running_dev_app(
    plan: &CargoPlan,
    request: &DevRequest,
    child: &mut Option<Child>,
    events: &mpsc::Receiver<notify::Result<Event>>,
    roots: &[PathBuf],
    watcher: &mut DevWatcher,
    sources: &mut HashMap<PathBuf, (SystemTime, u64)>,
    dev: &mut dev_reload::DevLoop,
) -> bool {
    drain_until_quiet(events, roots, request.debounce, watcher);
    if reload_catalogs_without_build(plan, request.profile(), child.as_ref(), roots, sources) {
        dev.changed_at = None;
        return false;
    }
    loop {
        if let Some(next) = snapshot_restart_sources(roots) {
            *sources = next;
        }
        let mut timings = dev.start();
        if let Some(timings) = timings.as_mut() {
            let waited = SystemTime::now()
                .duration_since(timings.changed)
                .unwrap_or_default();
            timings.set("debounce", waited.as_secs_f64() * 1000.0);
        }
        report_child_exit(child);
        eprintln!("[tilo-app] change detected; rebuilding");
        let building_revision = dev.source_revision.load(Ordering::Acquire);
        // The startup plan already passed preflight. A newer edit cancels this
        // compile instead of waiting for a binary that will not be launched.
        let in_place = reloads_in_place(&plan.app_dir, request.profile());
        let candidate = if in_place {
            build_in_place_dev_app_for_loop(
                plan,
                events,
                roots,
                watcher,
                child.is_none(),
                sources,
                dev,
                &mut timings,
            )
        } else {
            build_dev_candidate_interruptible(
                plan,
                request.profile(),
                Some(events),
                roots,
                |event| {
                    watcher.observe(event);
                },
            )
        };
        let queued_restart = drain_pending_restart_events(events, roots, |event| {
            watcher.observe(event);
        });
        let superseded =
            queued_restart || dev.source_revision.load(Ordering::Acquire) != building_revision;
        if DEV_STOP_REQUESTED.load(Ordering::Relaxed) {
            return true;
        }
        if superseded {
            eprintln!(
                "[tilo-app] newer change arrived during the build; skipping the stale candidate"
            );
            dev.changed_at = Some(SystemTime::now());
            drain_until_quiet(events, roots, request.debounce, watcher);
            continue;
        }
        match candidate {
            Ok(DevCandidate::Built(executable)) => {
                if in_place
                    && let Some(running) = child.as_ref()
                    && reload_running_dev_app(running)
                {
                    eprintln!("[tilo-app] build succeeded; reloading the open windows");
                    if let Some(timings) = timings {
                        dev.signalled(timings);
                    }
                } else if in_place {
                    eprintln!("[tilo-app] build succeeded; opening the app");
                    *child = spawn_thin_dev_binary_reported(
                        &plan.app_dir,
                        &executable,
                        &request.forwarded,
                    );
                } else {
                    restart_dev_app(child, &plan.app_dir, &executable, &request.forwarded);
                }
            }
            Ok(DevCandidate::Superseded) => {
                eprintln!(
                    "[tilo-app] newer change arrived during the build; skipping the stale candidate"
                );
                dev.changed_at = Some(SystemTime::now());
                drain_until_quiet(events, roots, request.debounce, watcher);
                continue;
            }
            Err(error) => {
                let state = if child.is_some() {
                    "keeping the last good app running"
                } else {
                    "waiting for another change"
                };
                eprintln!("[tilo-app] build failed; {state}\n{error:#}");
            }
        }
        return false;
    }
}

/// A running in-place app reads `locales/*.ftl` from disk on every reload, so
/// an edit to catalogs alone needs a signal, not a build. Returns whether it
/// was handled that way.
fn reload_catalogs_without_build(
    plan: &CargoPlan,
    profile: Option<&str>,
    child: Option<&Child>,
    roots: &[PathBuf],
    sources: &mut HashMap<PathBuf, (SystemTime, u64)>,
) -> bool {
    let Some(running) = child else {
        return false;
    };
    if !reloads_in_place(&plan.app_dir, profile) {
        return false;
    }
    let Some(next) = snapshot_restart_sources(roots) else {
        return false;
    };
    let app_dir = plan
        .app_dir
        .canonicalize()
        .unwrap_or_else(|_| plan.app_dir.clone());
    let changed = changed_sources(sources, &next);
    if changed.is_empty()
        || !changed.iter().all(|path| is_locale_catalog(&app_dir, path))
        || !reload_running_dev_app(running)
    {
        return false;
    }
    *sources = next;
    eprintln!("[tilo-app] catalogs changed; reloading strings without a build");
    true
}

/// Paths added, removed or restamped between two source snapshots.
fn changed_sources<'a>(
    before: &'a HashMap<PathBuf, (SystemTime, u64)>,
    after: &'a HashMap<PathBuf, (SystemTime, u64)>,
) -> Vec<&'a Path> {
    let mut changed = after
        .iter()
        .filter(|(path, stamp)| before.get(*path) != Some(stamp))
        .map(|(path, _)| path.as_path())
        .collect::<Vec<_>>();
    changed.extend(
        before
            .keys()
            .filter(|path| !after.contains_key(*path))
            .map(PathBuf::as_path),
    );
    changed
}

/// A Fluent catalog directly under the app's `locales/`.
fn is_locale_catalog(app_dir: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(app_dir) else {
        return false;
    };
    let mut components = relative.components();
    components
        .next()
        .is_some_and(|first| first.as_os_str() == "locales")
        && components.next().is_some()
        && components.next().is_none()
        && relative
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("ftl"))
}

/// Drain edits queued while Cargo was compiling. Returning `true` makes the
/// caller discard that candidate and rebuild the newest source instead of
/// briefly launching an already-stale app.
fn drain_pending_restart_events(
    events: &mpsc::Receiver<notify::Result<Event>>,
    roots: &[PathBuf],
    mut observe: impl FnMut(&Event),
) -> bool {
    let mut restart = false;
    for _ in 0..DEV_EVENT_DRAIN_LIMIT {
        match events.try_recv() {
            Ok(Ok(event)) => {
                observe(&event);
                restart |= event_requires_restart(&event, roots);
            }
            Ok(Err(error)) => eprintln!("[tilo-app] watch error: {error}"),
            Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => return restart,
        }
    }
    restart
}

enum DevCandidate {
    Built(PathBuf),
    /// A source edit arrived while Cargo was running. That process was stopped.
    Superseded,
}

struct DevCargo {
    args: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
    pick: DevPick,
}

enum DevPick {
    Executable(String),
    DynamicLibrary,
}

#[cfg(test)]
fn build_dev_candidate(plan: &CargoPlan) -> Result<PathBuf> {
    match build_dev_candidate_interruptible(plan, None, None, &[], |_| {})? {
        DevCandidate::Built(executable) => Ok(executable),
        DevCandidate::Superseded => bail!("development build was cancelled without a watcher"),
    }
}

/// Build the app binary. When `events` is set, a restart-worthy edit cancels
/// Cargo so the next build compiles the newest source instead of waiting out
/// a binary that will be discarded.
fn build_dev_candidate_interruptible(
    plan: &CargoPlan,
    profile: Option<&str>,
    events: Option<&mpsc::Receiver<notify::Result<Event>>>,
    roots: &[PathBuf],
    observe: impl FnMut(&Event),
) -> Result<DevCandidate> {
    let manifest = plan.app_dir.join("Cargo.toml");
    let expected_binary = dev_binary_name(&manifest)?;
    let mut args = vec![
        OsString::from("build"),
        OsString::from("--manifest-path"),
        manifest.as_os_str().to_os_string(),
        OsString::from("--bin"),
        OsString::from(&expected_binary),
    ];
    args.extend(cargo_profile_args(profile));
    args.push(OsString::from("--message-format=json-render-diagnostics"));
    run_dev_cargo(
        plan,
        events,
        roots,
        observe,
        &DevCargo {
            args,
            env: Vec::new(),
            pick: DevPick::Executable(expected_binary),
        },
        None,
    )
}

fn run_dev_cargo(
    plan: &CargoPlan,
    events: Option<&mpsc::Receiver<notify::Result<Event>>>,
    roots: &[PathBuf],
    mut observe: impl FnMut(&Event),
    cargo: &DevCargo,
    mut span: Option<&mut RustcSpan>,
) -> Result<DevCandidate> {
    let manifest = plan.app_dir.join("Cargo.toml");
    let expected_manifest = manifest.canonicalize().unwrap_or_else(|_| manifest.clone());
    let mut command = ProcessCommand::new(&plan.program);
    command
        .args(&cargo.args)
        .envs(cargo.env.iter().map(|(key, value)| (key, value)))
        .current_dir(&plan.app_dir)
        .stdin(Stdio::inherit())
        .stderr(Stdio::inherit())
        .stdout(Stdio::piped());
    #[cfg(unix)]
    command.process_group(0);
    let mut build = command
        .spawn()
        .with_context(|| format!("starting {} build", plan.program.to_string_lossy()))?;
    let stdout = build
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("Cargo build has no artifact stream"))?;
    let (line_tx, line_rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if line_tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut artifacts = HashMap::new();
    loop {
        if DEV_STOP_REQUESTED.load(Ordering::Relaxed) {
            terminate_build_child(&mut build);
            bail!("development stopped");
        }
        if events.is_some_and(|events| newer_source_during_build(events, roots, &mut observe)) {
            terminate_build_child(&mut build);
            return Ok(DevCandidate::Superseded);
        }
        while let Ok(line) = line_rx.try_recv() {
            let line = line.context("reading Cargo development artifact stream")?;
            collect_cargo_message(&line, &expected_manifest, &mut artifacts);
        }
        match build.try_wait() {
            Ok(Some(status)) => {
                // The reader thread can still be forwarding the last lines
                // after the process has exited. Wait until it drops the sender.
                while let Ok(line) = line_rx.recv() {
                    let line = line.context("reading Cargo development artifact stream")?;
                    collect_cargo_message(&line, &expected_manifest, &mut artifacts);
                }
                if !status.success() {
                    bail!("Cargo development build failed with {status}");
                }
                let artifact = match &cargo.pick {
                    DevPick::Executable(name) => artifacts.remove(name).with_context(|| {
                        format!(
                            "Cargo build completed without reporting executable {name:?}; set package.default-run when the app has multiple binaries"
                        )
                    })?,
                    DevPick::DynamicLibrary => artifacts
                        .into_iter()
                        .find(|(name, _)| name.starts_with("dylib:"))
                        .map(|(_, path)| path)
                        .context("Cargo build completed without reporting a shared library")?,
                };
                return Ok(DevCandidate::Built(artifact));
            }
            Ok(None) => {}
            Err(error) => return Err(error).context("waiting for Cargo development build"),
        }
        #[cfg(unix)]
        if let Some(span) = span.as_mut() {
            span.observe(build.id());
        }
        std::thread::sleep(Duration::from_millis(if span.is_some() { 5 } else { 20 }));
    }
}

fn newer_source_during_build(
    events: &mpsc::Receiver<notify::Result<Event>>,
    roots: &[PathBuf],
    observe: &mut impl FnMut(&Event),
) -> bool {
    // A bound keeps Cargo reapable when the editor opens files faster than
    // this loop can drain them. The next build tick sees whatever remains.
    let mut restart = false;
    for _ in 0..DEV_EVENT_DRAIN_LIMIT {
        match events.try_recv() {
            Ok(Ok(event)) => {
                restart |= event_requires_restart(&event, roots);
                observe(&event);
            }
            Ok(Err(error)) => eprintln!("[tilo-app] watch error: {error}"),
            Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => break,
        }
    }
    restart
}

fn terminate_build_child(child: &mut Child) {
    #[cfg(unix)]
    {
        let root = child.id();
        // Snapshot descendants before signalling. A shell with job control
        // puts `sleep`/`rustc` in a new group, and killing only the leader's
        // group would leave that child holding the target lock.
        let pids = descendant_pids(root);
        if let Ok(pid) = i32::try_from(root) {
            // SAFETY: the build was spawned as its own process group. Negating
            // that pid signals Cargo and the rustc children that inherited it.
            unsafe {
                libc::kill(pid.saturating_neg(), libc::SIGKILL);
            }
        }
        for pid in pids {
            if let Ok(pid) = i32::try_from(pid) {
                // SAFETY: `pid` is the build process or a descendant recorded
                // from /proc before the signal. SIGKILL is the cancel.
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// The process and every descendant, found from `/proc/<pid>/status` `PPid`.
///
/// `/proc/<pid>/task/<pid>/children` is empty on kernels built without
/// `CONFIG_PROC_CHILDREN`, which is how a cancelled `sleep` survived the
/// process-group signal in CI.
#[cfg(unix)]
fn descendant_pids(root: u32) -> Vec<u32> {
    let mut by_parent: HashMap<u32, Vec<u32>> = HashMap::new();
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
                continue;
            };
            let Some(parent) = parent_pid(pid) else {
                continue;
            };
            by_parent.entry(parent).or_default().push(pid);
        }
    }
    let mut pids = vec![root];
    let mut cursor = 0usize;
    while let Some(pid) = pids.get(cursor).copied() {
        cursor = cursor.saturating_add(1);
        if let Some(children) = by_parent.get(&pid) {
            for child in children {
                if !pids.contains(child) {
                    pids.push(*child);
                }
            }
        }
    }
    pids
}

#[cfg(unix)]
fn parent_pid(pid: u32) -> Option<u32> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("PPid:") {
            return rest.trim().parse().ok();
        }
    }
    None
}

fn dev_binary_name(manifest: &Path) -> Result<String> {
    let source = std::fs::read_to_string(manifest)
        .with_context(|| format!("reading {}", manifest.display()))?;
    let value = source
        .parse::<toml::Table>()
        .with_context(|| format!("parsing {}", manifest.display()))?;
    let package = value
        .get("package")
        .and_then(toml::Value::as_table)
        .with_context(|| format!("{} has no [package] table", manifest.display()))?;
    package
        .get("default-run")
        .or_else(|| package.get("name"))
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
        .with_context(|| format!("{} has no package name", manifest.display()))
}

fn cargo_package_name(manifest: &Path) -> Result<String> {
    let source = std::fs::read_to_string(manifest)
        .with_context(|| format!("reading {}", manifest.display()))?;
    let value = source
        .parse::<toml::Table>()
        .with_context(|| format!("parsing {}", manifest.display()))?;
    value
        .get("package")
        .and_then(|package| package.get("name"))
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
        .with_context(|| format!("{} has no package name", manifest.display()))
}

fn dev_watch_roots(app_dir: &Path, watch_ui: bool) -> Result<Vec<PathBuf>> {
    let mut roots = vec![
        app_dir
            .canonicalize()
            .with_context(|| format!("resolving app watch root {}", app_dir.display()))?,
    ];
    if watch_ui && let Some(sdk_dir) = local_ui_dependency(app_dir)? {
        if !roots.iter().any(|root| sdk_dir.starts_with(root)) {
            roots.push(sdk_dir.clone());
        }
        if let Some(toolkit_dir) = local_dependency(&sdk_dir, "ui-core")?
            && !roots.iter().any(|root| toolkit_dir.starts_with(root))
        {
            roots.push(toolkit_dir);
        }
    }
    Ok(roots)
}

fn local_ui_dependency(app_dir: &Path) -> Result<Option<PathBuf>> {
    local_dependency(app_dir, "ui")
}

fn local_dependency(app_dir: &Path, dependency_name: &str) -> Result<Option<PathBuf>> {
    let manifest_path = app_dir.join("Cargo.toml");
    let source = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    let manifest = source
        .parse::<toml::Table>()
        .with_context(|| format!("parsing {}", manifest_path.display()))?;
    let targets = manifest
        .get("target")
        .and_then(toml::Value::as_table)
        .into_iter()
        .flat_map(toml::Table::values);
    let Some(path) = manifest
        .get("dependencies")
        .into_iter()
        .chain(targets.filter_map(|target| target.get("dependencies")))
        .filter_map(|dependencies| dependencies.get(dependency_name))
        .find_map(|dependency| dependency.get("path").and_then(toml::Value::as_str))
    else {
        return Ok(None);
    };
    let resolved = app_dir.join(path).canonicalize().with_context(|| {
        format!(
            "resolving local {dependency_name} dependency {}",
            app_dir.join(path).display()
        )
    })?;
    Ok(Some(resolved))
}

struct DevWatcher {
    inner: RecommendedWatcher,
    watched: HashSet<PathBuf>,
    /// The OS ran out of watches; the source stamp poll carries on alone.
    exhausted: bool,
}

impl DevWatcher {
    fn new(inner: RecommendedWatcher) -> Self {
        Self {
            inner,
            watched: HashSet::new(),
            exhausted: false,
        }
    }

    fn watch_tree(&mut self, root: &Path) -> Result<()> {
        if self.exhausted {
            return Ok(());
        }
        for directory in source_watch_directories(root)? {
            if !self.watched.insert(directory.clone()) {
                continue;
            }
            if let Err(error) = self.inner.watch(&directory, RecursiveMode::NonRecursive) {
                self.watched.remove(&directory);
                if matches!(error.kind, notify::ErrorKind::MaxFilesWatch) {
                    self.exhausted = true;
                    eprintln!(
                        "[tilo-app] the OS file watch limit is reached (another program holds \
                         most of fs.inotify.max_user_watches); checking for changes every {} ms instead",
                        SOURCE_POLL_INTERVAL.as_millis()
                    );
                    return Ok(());
                }
                return Err(error).with_context(|| format!("watching {}", directory.display()));
            }
        }
        Ok(())
    }

    fn observe(&mut self, event: &Event) {
        if matches!(event.kind, EventKind::Remove(_)) {
            for path in &event.paths {
                let removed = self
                    .watched
                    .iter()
                    .filter(|directory| directory.starts_with(path))
                    .cloned()
                    .collect::<Vec<_>>();
                for directory in removed {
                    let _ = self.inner.unwatch(&directory);
                    self.watched.remove(&directory);
                }
            }
        }
        // Opening a directory we already watch must not walk it again.
        // `read_dir` emits another open event, and following those never ends.
        if !event_introduces_directory(event) {
            return;
        }
        for path in &event.paths {
            if self.watched.contains(path) || !path.is_dir() {
                continue;
            }
            if ignored_watch_component(path.file_name().and_then(|name| name.to_str())) {
                continue;
            }
            if let Err(error) = self.watch_tree(path) {
                eprintln!("[tilo-app] cannot watch new directory: {error:#}");
            }
        }
    }
}

/// How often to compare file stamps. inotify delivery is not required.
const SOURCE_POLL_INTERVAL: Duration = Duration::from_millis(200);
/// Events inspected per build tick. The rest stay queued for the next tick
/// so a flood cannot postpone reaping Cargo.
const DEV_EVENT_DRAIN_LIMIT: usize = 128;

#[derive(Debug)]
enum WatchTick {
    Idle,
    /// `bump_revision` is set for a stamp poll. inotify already bumped it.
    Rebuild {
        bump_revision: bool,
    },
    Stop,
}

/// One wait on the watcher. Access traffic does not postpone the stamp poll.
fn watch_tick(
    events: &mpsc::Receiver<notify::Result<Event>>,
    roots: &[PathBuf],
    watcher: &mut DevWatcher,
    sources: &mut HashMap<PathBuf, (SystemTime, u64)>,
    next_source_poll: &mut Instant,
) -> WatchTick {
    let timeout = next_source_poll
        .saturating_duration_since(Instant::now())
        .min(Duration::from_millis(100));
    let event_restart = match events.recv_timeout(timeout) {
        Ok(Ok(event)) => {
            watcher.observe(&event);
            event_requires_restart(&event, roots)
        }
        Err(RecvTimeoutError::Timeout) => false,
        Ok(Err(error)) => {
            eprintln!("[tilo-app] watch error: {error}");
            false
        }
        Err(RecvTimeoutError::Disconnected) => return WatchTick::Stop,
    };
    if event_restart {
        return WatchTick::Rebuild {
            bump_revision: false,
        };
    }
    if take_source_stamp_change(roots, sources, next_source_poll, Instant::now()) {
        return WatchTick::Rebuild {
            bump_revision: true,
        };
    }
    WatchTick::Idle
}

fn take_source_stamp_change(
    roots: &[PathBuf],
    sources: &mut HashMap<PathBuf, (SystemTime, u64)>,
    next_source_poll: &mut Instant,
    now: Instant,
) -> bool {
    if now < *next_source_poll {
        return false;
    }
    *next_source_poll = now.checked_add(SOURCE_POLL_INTERVAL).unwrap_or(now);
    let Some(next) = snapshot_restart_sources(roots) else {
        return false;
    };
    let changed = next != *sources;
    if !changed {
        *sources = next;
    }
    changed
}

fn event_introduces_directory(event: &Event) -> bool {
    matches!(
        event.kind,
        EventKind::Create(_)
            | EventKind::Modify(notify::event::ModifyKind::Name(
                notify::event::RenameMode::To
                    | notify::event::RenameMode::Both
                    | notify::event::RenameMode::Any,
            ))
    )
}

fn source_watch_directories(root: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        if ignored_watch_path(&directory, root) {
            continue;
        }
        let entries = std::fs::read_dir(&directory)
            .with_context(|| format!("reading watch directory {}", directory.display()))?;
        found.push(directory);
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    Ok(found)
}

fn ignored_watch_path(path: &Path, root: &Path) -> bool {
    path.strip_prefix(root).is_ok_and(|relative| {
        relative
            .components()
            .any(|component| ignored_watch_component(component.as_os_str().to_str()))
    })
}

fn ignored_watch_component(component: Option<&str>) -> bool {
    matches!(component, Some("target" | ".git" | ".idea" | ".vscode"))
}

/// Modification time and length of every file that would restart the app.
/// `None` when every watch root is unreadable, so a failed scan is not
/// mistaken for a deletion. One unreadable directory is skipped.
fn snapshot_restart_sources(roots: &[PathBuf]) -> Option<HashMap<PathBuf, (SystemTime, u64)>> {
    let mut stamps = HashMap::new();
    let mut saw_root = false;
    for root in roots {
        // One unreadable directory must not hide an edit under another root.
        let Ok(directories) = source_watch_directories(root) else {
            continue;
        };
        saw_root = true;
        for directory in directories {
            let Ok(entries) = std::fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !restart_path(&path, roots) {
                    continue;
                }
                let Ok(metadata) = entry.metadata() else {
                    continue;
                };
                if metadata.is_dir() {
                    continue;
                }
                let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                stamps.insert(path, (modified, metadata.len()));
            }
        }
    }
    saw_root.then_some(stamps)
}

fn event_requires_restart(event: &Event, roots: &[PathBuf]) -> bool {
    if matches!(event.kind, EventKind::Access(_)) {
        return false;
    }
    event.paths.iter().any(|path| restart_path(path, roots))
}

fn restart_path(path: &Path, roots: &[PathBuf]) -> bool {
    let relative = roots.iter().find_map(|root| path.strip_prefix(root).ok());
    let Some(relative) = relative else {
        return false;
    };
    if relative.as_os_str().is_empty() {
        return false;
    }
    if relative
        .components()
        .any(|component| ignored_watch_component(component.as_os_str().to_str()))
    {
        return false;
    }
    // LiveUi hot-reloads `ui/*.json` when TILO_UI_GRAPH_DIR is set; restarting
    // would wipe process-local state the graph edit is meant to preserve.
    if relative
        .components()
        .next()
        .is_some_and(|component| component.as_os_str() == "ui")
        && relative
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
    {
        return false;
    }
    let Some(name) = relative.file_name().and_then(|name| name.to_str()) else {
        return true;
    };
    let temporary_extension = relative.extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("swp") || extension.eq_ignore_ascii_case("tmp")
    });
    name != "Cargo.lock" && !name.ends_with('~') && !name.starts_with(".#") && !temporary_extension
}

fn drain_until_quiet(
    events: &mpsc::Receiver<notify::Result<Event>>,
    roots: &[PathBuf],
    quiet: Duration,
    watcher: &mut DevWatcher,
) {
    let mut deadline = Instant::now().checked_add(quiet);
    loop {
        if DEV_STOP_REQUESTED.load(Ordering::Relaxed) {
            return;
        }
        let Some(until) = deadline else {
            return;
        };
        let remaining = until.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return;
        }
        let poll = remaining.min(Duration::from_millis(100));
        match events.recv_timeout(poll) {
            Ok(Ok(event)) => {
                watcher.observe(&event);
                if event_requires_restart(&event, roots) {
                    deadline = Instant::now().checked_add(quiet);
                }
            }
            Ok(Err(error)) => eprintln!("[tilo-app] watch error: {error}"),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn attach_live_graph_env(command: &mut ProcessCommand, app_dir: &Path) {
    let ui_dir = app_dir.join("ui");
    if ui_dir.is_dir() {
        command.env("TILO_UI_GRAPH_DIR", ui_dir);
    }
}

/// Directory outside the watched app tree. The running app writes its document,
/// route, and caret here on SIGTERM; the next process reads them back.
fn dev_session_dir(app_dir: &Path) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let identity = app_dir
        .canonicalize()
        .unwrap_or_else(|_| app_dir.to_path_buf());
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    identity.hash(&mut hasher);
    std::env::temp_dir().join(format!("tilo-dev-{:016x}", hasher.finish()))
}

/// The environment of an app `dev` starts: its dev session, its runtime host,
/// and inside a Tilo session the shell's file chooser. Returns this process's
/// end of the chooser connection, to keep until the app has started.
fn attach_dev_session_env(command: &mut ProcessCommand, app_dir: &Path) -> Option<DevChooser> {
    let dir = dev_session_dir(app_dir);
    if let Err(error) = std::fs::create_dir_all(&dir) {
        eprintln!(
            "[tilo-app] cannot keep the dev session at {}: {error}",
            dir.display()
        );
        return None;
    }
    command.env("TILO_DEV_SESSION", dir);
    if let Some(runtime) = DEV_RUNTIME.get().and_then(Option::as_ref) {
        command
            .env("TILO_RUNTIME_BINARY", &runtime.host)
            .env("TILO_RUNTIME_PROFILE", &runtime.profile);
    }
    #[cfg(target_os = "linux")]
    {
        let mut kept: Vec<std::os::fd::OwnedFd> =
            attach_model_gateway(command, app_dir).into_iter().collect();
        kept.extend(attach_session_chooser(command, app_dir));
        Some(kept)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// `dev`'s ends of the connections it made for an app it started: the file
/// chooser, and the services it hands over as the launcher does.
#[cfg(target_os = "linux")]
type DevChooser = Vec<std::os::fd::OwnedFd>;
#[cfg(not(target_os = "linux"))]
type DevChooser = ();

/// The room of the workspace in front of the Tilo session this command runs
/// in, as its shell publishes it; `None` outside a session.
#[cfg(target_os = "linux")]
fn session_room() -> Option<String> {
    let runtime = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?);
    if !runtime.is_absolute() {
        return None;
    }
    let room = std::fs::read_to_string(runtime.join("tilo-session").join("workspace-id")).ok()?;
    let room = room.trim();
    (!room.is_empty()).then(|| room.to_owned())
}

/// Hand the app the session's model gateway when its manifest declares it,
/// connected and authenticated for the app the way the launcher does
/// (`launcher::services`), so features served by the gateway (Canvas's
/// select subject, Photos' analysis) work in a dev session as they do in a
/// login. Nothing is attached when no gateway runs in the session.
#[cfg(target_os = "linux")]
fn attach_model_gateway(
    command: &mut ProcessCommand,
    app_dir: &Path,
) -> Option<std::os::fd::OwnedFd> {
    use std::os::fd::AsRawFd as _;
    let source = std::fs::read_to_string(app_dir.join("tilo-app.toml")).ok()?;
    let manifest = tilo_app_package::Manifest::from_toml_str(&source).ok()?;
    let name = launcher::services::MODEL_GATEWAY_SERVICE;
    let requirement = manifest.sandbox.services.get(name)?;
    // A dev session may run without one: what the app requires, it asks
    // for here as optional, and says what is missing.
    let wanted = std::collections::BTreeMap::from([(
        name.to_owned(),
        format!("optional@{}", requirement.version),
    )]);
    let context = launcher::services::ServiceLaunchContext {
        app_id: &manifest.package.id,
        workspace_id: None,
    };
    let resolved =
        match launcher::services::resolve_service_binds_for_launch(&wanted, false, context) {
            Ok(resolved) => resolved,
            Err(error) => {
                eprintln!("[tilo-app] the session's model gateway is unavailable: {error}");
                return None;
            }
        };
    let Some(service) = resolved.into_iter().next() else {
        eprintln!(
            "[tilo-app] no model gateway answers at {}; the app runs without it",
            launcher::services::socket_for(name).display()
        );
        return None;
    };
    let fd = service.host;
    // SAFETY: `fd` is an open descriptor this process owns; F_GETFD and
    // F_SETFD only read and change its close-on-exec flag.
    let inheritable = unsafe {
        let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFD);
        flags >= 0 && libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, flags & !libc::FD_CLOEXEC) == 0
    };
    if !inheritable {
        return None;
    }
    command.env(
        launcher::sandbox::svc_env_var(name),
        fd.as_raw_fd().to_string(),
    );
    Some(fd)
}

/// Hand the app the session shell's file chooser, the way the launcher hands
/// one to an app the shell starts: connected on the app's behalf and named
/// for the app and the room it runs in before the app holds it. Inside a
/// Tilo session that runs no desktop portal this is how an app chooses
/// files; elsewhere nothing is attached. The returned descriptor is this
/// process's end, kept until the app has started.
#[cfg(target_os = "linux")]
fn attach_session_chooser(
    command: &mut ProcessCommand,
    app_dir: &Path,
) -> Option<std::os::fd::OwnedFd> {
    use std::os::fd::AsRawFd as _;
    let room = session_room()?;
    let source = std::fs::read_to_string(app_dir.join("tilo-app.toml")).ok()?;
    let manifest = tilo_app_package::Manifest::from_toml_str(&source).ok()?;
    let chooser =
        match launcher::services::resolve_chooser_bind(&manifest.package.id, Some(&room), false) {
            Ok(chooser) => chooser?,
            Err(error) => {
                eprintln!("[tilo-app] the session's file chooser is unavailable: {error}");
                return None;
            }
        };
    let fd = chooser.host;
    // The app inherits this descriptor; the launcher hands it at a fixed
    // slot inside its sandbox, an unsandboxed app takes it where it is.
    // SAFETY: `fd` is an open descriptor this process owns; F_GETFD and
    // F_SETFD only read and change its close-on-exec flag.
    let inheritable = unsafe {
        let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFD);
        flags >= 0 && libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, flags & !libc::FD_CLOEXEC) == 0
    };
    if !inheritable {
        return None;
    }
    command.env(
        launcher::sandbox::svc_env_var(&chooser.name),
        fd.as_raw_fd().to_string(),
    );
    Some(fd)
}

/// The runtime [`provide_runtime_host`] built for this dev session.
static DEV_RUNTIME: std::sync::OnceLock<Option<DevRuntime>> = std::sync::OnceLock::new();

/// A runtime host and the private profile its daemon serves. The profile is
/// the session's own: the user's runtime daemon may be any other version, and
/// an app cannot attach to a daemon that speaks another protocol.
struct DevRuntime {
    host: PathBuf,
    profile: PathBuf,
}

/// Stops the session's runtime daemon when the dev loop ends, so the next
/// session's daemon is started from the host that session builds.
struct DevRuntimeGuard;

impl Drop for DevRuntimeGuard {
    fn drop(&mut self) {
        if let Some(runtime) = DEV_RUNTIME.get().and_then(Option::as_ref) {
            stop_runtime_daemon(&runtime.profile);
        }
    }
}

/// The pid the runtime daemon serving `profile` reports, if one answers.
fn runtime_daemon_pid(profile: &Path) -> Option<i32> {
    use std::io::Read as _;
    let mut stream = std::os::unix::net::UnixStream::connect(profile.join("native.sock")).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .ok()?;
    stream.write_all(b"{\"native\":\"status\"}\n").ok()?;
    let mut line = String::new();
    BufReader::new(stream)
        .take(16_385)
        .read_line(&mut line)
        .ok()?;
    let status: serde_json::Value = serde_json::from_str(&line).ok()?;
    i32::try_from(status.get("pid")?.as_u64()?)
        .ok()
        .filter(|pid| *pid > 0)
}

fn stop_runtime_daemon(profile: &Path) {
    let Some(pid) = runtime_daemon_pid(profile) else {
        return;
    };
    // SAFETY: `pid` is the daemon serving this dev session's private profile,
    // which only runtime hosts this loop started can have created.
    if unsafe { libc::kill(pid, libc::SIGTERM) } == 0 {
        eprintln!("[tilo-app] stopped the dev runtime (pid {pid})");
    }
}

/// Whether the dev loop has to supply a runtime host. A workspace member finds
/// the one the workspace builds beside it, a Tilo session serves its own, and
/// `TILO_RUNTIME_BINARY` or `TILO_RUNTIME_STANDALONE=1` is the user's choice.
fn wants_runtime_host(var: impl Fn(&str) -> Option<OsString>, workspace_member: bool) -> bool {
    !workspace_member
        && var("TILO_RUNTIME_BINARY").is_none()
        && var("TILO_RUNTIME_STANDALONE").as_deref() != Some("1".as_ref())
        && var("XDG_CURRENT_DESKTOP").as_deref() != Some("tilo".as_ref())
}

/// The checkout holding the `tilo-app` crate an app resolves to, from
/// `cargo metadata`. Its workspace also builds the standalone runtime.
fn sdk_root_from_metadata(metadata: &serde_json::Value) -> Option<PathBuf> {
    let manifest = metadata
        .get("packages")?
        .as_array()?
        .iter()
        .find(|package| {
            package.get("name").and_then(serde_json::Value::as_str) == Some("tilo-app")
        })?
        .get("manifest_path")?
        .as_str()?;
    Path::new(manifest)
        .ancestors()
        .find(|root| {
            root.join("crates/runtime/tilo-runtime/Cargo.toml")
                .is_file()
        })
        .map(Path::to_path_buf)
}

/// Builds `tilo-runtime` from the SDK an outside app uses, so the app can
/// attach to a runtime that speaks its own protocol version.
fn provide_runtime_host(app_dir: &Path) {
    let wanted = wants_runtime_host(
        |name| std::env::var_os(name),
        app_is_workspace_member(app_dir),
    );
    let host = wanted.then(|| build_runtime_host(app_dir)).and_then(|built| {
        built
            .inspect_err(|error| {
                eprintln!(
                    "[tilo-app] no runtime host for the app: {error:#}\n\
                     [tilo-app] set TILO_RUNTIME_BINARY to one, or TILO_RUNTIME_STANDALONE=1 to run without"
                );
            })
            .ok()
    });
    let runtime = host.map(|host| {
        let profile = dev_session_dir(app_dir).join("runtime");
        // A daemon left by a session that did not stop cleanly runs an older host.
        stop_runtime_daemon(&profile);
        DevRuntime { host, profile }
    });
    let _ = DEV_RUNTIME.set(runtime);
}

/// `cargo metadata` for the app at `app_dir`, with its dependencies resolved.
fn cargo_metadata(app_dir: &Path) -> Result<serde_json::Value> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let output = ProcessCommand::new(&cargo)
        .args(["metadata", "--format-version", "1", "--manifest-path"])
        .arg(app_dir.join("Cargo.toml"))
        .current_dir(app_dir)
        .stderr(Stdio::inherit())
        .output()
        .context("running cargo metadata")?;
    if !output.status.success() {
        bail!("cargo metadata failed with {}", output.status);
    }
    serde_json::from_slice(&output.stdout).context("parsing cargo metadata")
}

fn build_runtime_host(app_dir: &Path) -> Result<PathBuf> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let metadata = cargo_metadata(app_dir)?;
    let root = sdk_root_from_metadata(&metadata).ok_or_else(|| {
        anyhow::anyhow!("the app's tilo-app dependency is not in a Tilo checkout")
    })?;
    let manifest = root.join("crates/runtime/tilo-runtime/Cargo.toml");
    eprintln!(
        "[tilo-app] building the runtime host from {}",
        root.display()
    );
    let mut child = ProcessCommand::new(&cargo)
        .args([
            "build",
            "--locked",
            "-p",
            "tilo-runtime",
            "--bin",
            "tilo-runtime",
        ])
        .arg("--message-format=json-render-diagnostics")
        .current_dir(&root)
        .stdout(Stdio::piped())
        .spawn()
        .context("starting the runtime host build")?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("Cargo build has no artifact stream"))?;
    let expected = manifest.canonicalize().unwrap_or(manifest);
    let mut artifacts = HashMap::new();
    for line in BufReader::new(stdout).lines() {
        collect_cargo_message(
            &line.context("reading Cargo artifact stream")?,
            &expected,
            &mut artifacts,
        );
    }
    let status = child.wait().context("waiting for the runtime host build")?;
    if !status.success() {
        bail!("the runtime host build failed with {status}");
    }
    let host = artifacts
        .remove("tilo-runtime")
        .ok_or_else(|| anyhow::anyhow!("Cargo did not report the tilo-runtime binary"))?;
    eprintln!("[tilo-app] runtime host {}", host.display());
    Ok(host)
}

fn spawn_dev_child(plan: &CargoPlan) -> Result<Child> {
    let mut command = ProcessCommand::new(&plan.program);
    command.args(&plan.args).current_dir(&plan.app_dir);
    attach_live_graph_env(&mut command, &plan.app_dir);
    let _chooser = attach_dev_session_env(&mut command, &plan.app_dir);
    #[cfg(unix)]
    command.process_group(0);
    let child = command
        .spawn()
        .with_context(|| format!("starting {}", plan.program.to_string_lossy()))?;
    eprintln!("[tilo-app] started (pid {})", child.id());
    Ok(child)
}

fn spawn_dev_child_reported(plan: &CargoPlan) -> Option<Child> {
    match spawn_dev_child(plan) {
        Ok(child) => Some(child),
        Err(error) => {
            eprintln!("[tilo-app] {error:#}; waiting for a change");
            None
        }
    }
}

fn spawn_dev_binary(app_dir: &Path, executable: &Path, forwarded: &[OsString]) -> Result<Child> {
    let mut command = ProcessCommand::new(executable);
    command.args(forwarded).current_dir(app_dir);
    attach_live_graph_env(&mut command, app_dir);
    let _chooser = attach_dev_session_env(&mut command, app_dir);
    #[cfg(unix)]
    command.process_group(0);
    let child = command
        .spawn()
        .with_context(|| format!("starting rebuilt app {}", executable.display()))?;
    eprintln!("[tilo-app] started (pid {})", child.id());
    Ok(child)
}

fn spawn_dev_binary_reported(
    app_dir: &Path,
    executable: &Path,
    forwarded: &[OsString],
) -> Option<Child> {
    match spawn_dev_binary(app_dir, executable, forwarded) {
        Ok(child) => Some(child),
        Err(error) => {
            eprintln!("[tilo-app] {error:#}; waiting for a change");
            None
        }
    }
}

fn report_child_exit(child: &mut Option<Child>) {
    let Some(running) = child else {
        return;
    };
    match running.try_wait() {
        Ok(Some(status)) => {
            eprintln!("[tilo-app] app exited ({status}); waiting for a change");
            *child = None;
        }
        Ok(None) => {}
        Err(error) => {
            eprintln!("[tilo-app] cannot inspect app process: {error}");
            *child = None;
        }
    }
}

/// Whether `dev` reloads the app inside its windows: it supports that, and
/// this machine can build the shared toolkit it links.
fn reloads_in_place(app_dir: &Path, profile: Option<&str>) -> bool {
    profile_reloads_in_place(profile)
        && app_supports_in_place_reload(app_dir)
        && native_libs::thin_runtime_available()
}

/// Reloading in place links the toolkit and `libstd` dynamically, which an
/// LTO profile such as `release` refuses. `dev` and `thin-app` have no LTO;
/// any other profile restarts the app instead, whatever its settings.
fn profile_reloads_in_place(profile: Option<&str>) -> bool {
    matches!(profile, None | Some("dev" | DEV_RELEASE_PROFILE))
}

/// `--profile <name>` for Cargo, or nothing for its default `dev` profile.
fn cargo_profile_args(profile: Option<&str>) -> Vec<OsString> {
    profile
        .map(|profile| vec![OsString::from("--profile"), OsString::from(profile)])
        .unwrap_or_default()
}

/// The first `cargo run` of a restarting dev loop, in the requested profile.
/// Says so when that profile keeps an app that could reload in place from it.
fn dev_run_plan(app_dir: &Path, request: &DevRequest) -> Result<CargoPlan> {
    if let Some(profile) = request.profile() {
        validate_profile_name(profile)?;
    }
    if !profile_reloads_in_place(request.profile()) && app_supports_in_place_reload(app_dir) {
        eprintln!(
            "[tilo-app] the {:?} profile cannot link the toolkit dynamically; restarting the app on each change instead of reloading it in place",
            request.profile().unwrap_or("dev")
        );
    }
    let mut plan = plan_app_task(app_dir, AppTask::Run, &request.forwarded)?;
    // `run --manifest-path <manifest>`, then the forwarded arguments.
    plan.args
        .splice(3..3, cargo_profile_args(request.profile()));
    Ok(plan)
}

/// An app can reload Rust inside the windows it already has when it links the
/// shared toolkit (`thin`) and calls `ui::dev_reload_entry!` from its library.
fn app_supports_in_place_reload(app_dir: &Path) -> bool {
    let Ok(manifest) = std::fs::read_to_string(app_dir.join("Cargo.toml")) else {
        return false;
    };
    let has_thin = manifest.lines().any(|line| {
        let line = line.trim();
        line.starts_with("thin ") || line.starts_with("thin=")
    });
    if !has_thin {
        return false;
    }
    std::fs::read_to_string(app_dir.join("src/lib.rs"))
        .unwrap_or_default()
        .contains("dev_reload_entry!")
}

/// Build the prefer-dynamic host and the app library
/// that the host will `dlopen`. Stages the library before returning the host
/// executable. `need_host` is false when that executable is already running.
#[cfg(test)]
fn build_in_place_dev_app(
    plan: &CargoPlan,
    events: Option<&mpsc::Receiver<notify::Result<Event>>>,
    roots: &[PathBuf],
    observe: impl FnMut(&Event),
    need_host: bool,
) -> Result<DevCandidate> {
    build_in_place_dev_app_with_cargo(plan, None, events, roots, observe, need_host, &mut None)
}

/// The watch loop's in-place build. Once the app is running and every edit
/// since Cargo last compiled the reload library is a file that library's
/// rustc read, rustc alone rebuilds it; anything else goes through Cargo.
fn build_in_place_dev_app_for_loop(
    plan: &CargoPlan,
    events: &mpsc::Receiver<notify::Result<Event>>,
    roots: &[PathBuf],
    watcher: &mut DevWatcher,
    need_host: bool,
    sources: &HashMap<PathBuf, (SystemTime, u64)>,
    dev: &mut dev_reload::DevLoop,
    timings: &mut Option<dev_reload::ReloadTimings>,
) -> Result<DevCandidate> {
    let fast = (!need_host)
        .then(|| dev_reload::FastRebuild::load(dev.session()))
        .flatten()
        .filter(|fast| fast.covers(&changed_sources(&dev.baseline, sources)));
    if let Some(fast) = fast {
        eprintln!("[tilo-app] building the reload library with rustc");
        match dev_reload::rebuild_with_rustc(
            &fast,
            events,
            roots,
            |event| watcher.observe(event),
            timings,
        ) {
            dev_reload::FastOutcome::Done(DevCandidate::Built(library)) => {
                let staging = SystemTime::now();
                let staged = stage_dev_reload_library(&plan.app_dir, &library)?;
                if let Some(timings) = timings.as_mut() {
                    timings.set("stage", elapsed_ms(staging));
                    timings.staged(staged.clone());
                }
                eprintln!("[tilo-app] reload library {}", staged.display());
                return Ok(DevCandidate::Built(PathBuf::new()));
            }
            dev_reload::FastOutcome::Done(DevCandidate::Superseded) => {
                return Ok(DevCandidate::Superseded);
            }
            dev_reload::FastOutcome::Failed(error) => return Err(error),
            dev_reload::FastOutcome::Fallback(reason) => {
                eprintln!("[tilo-app] {reason}; rebuilding with Cargo");
            }
        }
    }
    // What Cargo is about to compile. Later edits are compared against it.
    dev.baseline.clone_from(sources);
    let session = dev.session().to_path_buf();
    let profile = dev.profile.clone();
    build_in_place_dev_app_with_cargo(
        plan,
        profile.as_deref(),
        Some(events),
        roots,
        |event| watcher.observe(event),
        need_host,
        &mut Some((session.as_path(), timings)),
    )
}

fn elapsed_ms(since: SystemTime) -> f64 {
    SystemTime::now()
        .duration_since(since)
        .unwrap_or_default()
        .as_secs_f64()
        * 1000.0
}

/// Build the steps with Cargo. With a session, the reload library's rustc
/// command is recorded there for the next edit, and each step is timed.
fn build_in_place_dev_app_with_cargo(
    plan: &CargoPlan,
    profile: Option<&str>,
    events: Option<&mpsc::Receiver<notify::Result<Event>>>,
    roots: &[PathBuf],
    mut observe: impl FnMut(&Event),
    need_host: bool,
    loop_state: &mut Option<(&Path, &mut Option<dev_reload::ReloadTimings>)>,
) -> Result<DevCandidate> {
    let manifest = plan.app_dir.join("Cargo.toml");
    let binary = dev_binary_name(&manifest)?;
    let linkage = if app_is_workspace_member(&plan.app_dir) {
        ThinDevLinkage::WorkspaceToolkit
    } else {
        ThinDevLinkage::Target(host_triple(&plan.app_dir)?)
    };
    let library_crate = cargo_library_name(&manifest);
    let mut executable = PathBuf::new();
    for mut step in thin_dev_steps(&manifest, &binary, &linkage, profile, need_host) {
        eprintln!("[tilo-app] building {}", step.label);
        if let (DevPick::DynamicLibrary, Some((session, _)), Some(name)) = (
            &step.cargo.pick,
            loop_state.as_ref(),
            library_crate.as_deref(),
        ) {
            step.cargo
                .env
                .extend(dev_reload::capture_env(session, name));
        }
        let timing = loop_state
            .as_mut()
            .and_then(|(_, timings)| timings.as_mut());
        let mut span = timing.is_some().then(RustcSpan::default);
        let started = SystemTime::now();
        let built = run_dev_cargo(
            plan,
            events,
            roots,
            &mut observe,
            &step.cargo,
            span.as_mut(),
        )?;
        if let Some(timings) = loop_state
            .as_mut()
            .and_then(|(_, timings)| timings.as_mut())
        {
            let wall = elapsed_ms(started);
            let compiled = span.as_ref().map_or(0.0, |span| span.record(timings));
            timings.add("cargo", (wall - compiled).max(0.0));
        }
        match built {
            DevCandidate::Superseded => return Ok(DevCandidate::Superseded),
            DevCandidate::Built(artifact) => match step.cargo.pick {
                DevPick::Executable(_) => executable = artifact,
                DevPick::DynamicLibrary => {
                    let staging = SystemTime::now();
                    let staged = stage_dev_reload_library(&plan.app_dir, &artifact)?;
                    if let Some(timings) = loop_state
                        .as_mut()
                        .and_then(|(_, timings)| timings.as_mut())
                    {
                        timings.set("stage", elapsed_ms(staging));
                        timings.staged(staged.clone());
                    }
                    eprintln!("[tilo-app] reload library {}", staged.display());
                }
            },
        }
    }
    Ok(DevCandidate::Built(executable))
}

/// The library target's crate name: `[lib] name`, else the package name with
/// dashes as underscores.
fn cargo_library_name(manifest: &Path) -> Option<String> {
    let value = std::fs::read_to_string(manifest)
        .ok()?
        .parse::<toml::Table>()
        .ok()?;
    let explicit = value
        .get("lib")
        .and_then(|lib| lib.get("name"))
        .and_then(toml::Value::as_str);
    let package = value
        .get("package")
        .and_then(|package| package.get("name"))
        .and_then(toml::Value::as_str);
    explicit.or(package).map(|name| name.replace('-', "_"))
}

/// When rustc ran inside a Cargo step, seen by polling Cargo's descendants.
#[derive(Debug, Default)]
struct RustcSpan {
    first: Option<SystemTime>,
    linking: Option<SystemTime>,
    last: Option<SystemTime>,
}

impl RustcSpan {
    #[cfg(unix)]
    fn observe(&mut self, cargo: u32) {
        let now = SystemTime::now();
        let descendants = descendant_pids(cargo);
        let rustc = descendants
            .iter()
            .copied()
            .filter(|pid| process_name(*pid).is_some_and(|name| name == "rustc"))
            .collect::<Vec<_>>();
        if rustc.is_empty() {
            return;
        }
        self.first.get_or_insert(now);
        self.last = Some(now);
        let linking = descendants
            .iter()
            .any(|pid| parent_pid(*pid).is_some_and(|parent| rustc.contains(&parent)));
        if linking {
            self.linking.get_or_insert(now);
        }
    }

    /// Add compile and link time; returns the milliseconds rustc ran.
    fn record(&self, timings: &mut dev_reload::ReloadTimings) -> f64 {
        let (Some(first), Some(last)) = (self.first, self.last) else {
            return 0.0;
        };
        let linking = self.linking.unwrap_or(last);
        let ms = |from: SystemTime, to: SystemTime| {
            to.duration_since(from).unwrap_or_default().as_secs_f64() * 1000.0
        };
        timings.add("compile", ms(first, linking));
        timings.add("link", ms(linking, last));
        ms(first, last)
    }
}

#[cfg(unix)]
fn process_name(pid: u32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .ok()
        .map(|name| name.trim().to_owned())
}

/// How a thin development build links the toolkit and `libstd` dynamically.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ThinDevLinkage {
    /// `runtime-dylib` is a member of the app's workspace. It comes in as a
    /// dependency of the host and the library, with the flags both share: a
    /// step of its own with other flags made each of them rebuild it.
    WorkspaceToolkit,
    /// Outside the Tilo workspace Cargo cannot select `runtime-dylib` or set
    /// its features. Every artifact for this target triple is built with
    /// `-C prefer-dynamic` instead, so the toolkit is a dependency of the host.
    /// Build scripts and proc macros are host artifacts and keep their flags.
    Target(String),
}

struct ThinDevStep {
    label: &'static str,
    cargo: DevCargo,
}

fn thin_dev_steps(
    manifest: &Path,
    binary: &str,
    linkage: &ThinDevLinkage,
    profile: Option<&str>,
    need_host: bool,
) -> Vec<ThinDevStep> {
    let (target_args, env) = match linkage {
        ThinDevLinkage::WorkspaceToolkit => (Vec::new(), Vec::new()),
        ThinDevLinkage::Target(triple) => (
            vec![OsString::from("--target"), OsString::from(triple)],
            vec![prefer_dynamic_rustflags(triple)],
        ),
    };
    let app_step = |label, target: &[&str], pick| {
        let mut args = vec![
            OsString::from("rustc"),
            OsString::from("--manifest-path"),
            manifest.as_os_str().to_os_string(),
        ];
        args.extend(target.iter().map(OsString::from));
        args.extend(target_args.iter().cloned());
        args.extend(cargo_profile_args(profile));
        args.extend(
            [
                "--features",
                "thin",
                "--message-format=json-render-diagnostics",
                "--",
                "-C",
                "prefer-dynamic",
            ]
            .map(OsString::from),
        );
        ThinDevStep {
            label,
            cargo: DevCargo {
                args,
                env: env.clone(),
                pick,
            },
        }
    };
    let mut steps = Vec::new();
    if need_host {
        steps.push(app_step(
            "the reload host",
            &["--bin", binary],
            DevPick::Executable(binary.to_owned()),
        ));
    }
    steps.push(app_step(
        "the reload library",
        &["--lib", "--crate-type", "cdylib"],
        DevPick::DynamicLibrary,
    ));
    steps
}

/// Whether `app_dir` is a member of the Tilo workspace around it.
fn app_is_workspace_member(app_dir: &Path) -> bool {
    let app_dir = app_dir
        .canonicalize()
        .unwrap_or_else(|_| app_dir.to_path_buf());
    find_repo_root(&app_dir).is_some_and(|root| {
        std::fs::read_to_string(root.join("Cargo.toml")).is_ok_and(|manifest| {
            app_dir
                .strip_prefix(&root)
                .is_ok_and(|member| manifest.contains(&format!("{:?}", member.to_string_lossy())))
        })
    })
}

/// The triple the app's own compiler builds for by default.
/// `CARGO_TARGET_<triple>_RUSTFLAGS` with `-C prefer-dynamic` appended, so
/// every artifact for `triple` links `libstd` and the toolkit dynamically while
/// build scripts and proc macros, which are host artifacts, keep their flags.
fn prefer_dynamic_rustflags(triple: &str) -> (OsString, OsString) {
    let key = format!(
        "CARGO_TARGET_{}_RUSTFLAGS",
        triple.to_ascii_uppercase().replace(['-', '.'], "_")
    );
    let mut flags = std::env::var_os(&key).unwrap_or_default();
    if !flags.is_empty() {
        flags.push(" ");
    }
    flags.push("-C prefer-dynamic");
    (OsString::from(key), flags)
}

fn host_triple(app_dir: &Path) -> Result<String> {
    let output = ProcessCommand::new("rustc")
        .arg("-vV")
        .current_dir(app_dir)
        .output()
        .context("asking rustc for the host target")?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("rustc -vV did not name a host target"))
}

/// Copy `library` to a new file in the dev session and point `reload.path` at
/// it. A new path makes `dlopen` read this build rather than one already mapped.
fn stage_dev_reload_library(app_dir: &Path, library: &Path) -> Result<PathBuf> {
    let dir = dev_session_dir(app_dir);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let seq_path = dir.join("reload.seq");
    let seq = std::fs::read_to_string(&seq_path)
        .ok()
        .and_then(|text| text.trim().parse::<u64>().ok())
        .unwrap_or(0)
        .saturating_add(1);
    std::fs::write(&seq_path, seq.to_string())
        .with_context(|| format!("writing {}", seq_path.display()))?;
    let destination = dir.join(format!("reload-{seq}.so"));
    std::fs::copy(library, &destination)
        .with_context(|| format!("copying {} to {}", library.display(), destination.display()))?;
    let pointer = dir.join("reload.path");
    std::fs::write(&pointer, format!("{}\n", destination.display()))
        .with_context(|| format!("writing {}", pointer.display()))?;
    remove_superseded_reload_libraries(&dir, seq);
    Ok(destination)
}

/// Drops every staged library older than the one before `current`. An app
/// that already mapped one keeps it after the file goes; it only ever opens
/// the library `reload.path` names.
fn remove_superseded_reload_libraries(dir: &Path, current: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let superseded = name
            .to_str()
            .and_then(|name| {
                let name = name.strip_prefix("reload-")?;
                name.strip_suffix(dev_reload::APP_TIMINGS_SUFFIX)
                    .unwrap_or(name)
                    .strip_suffix(".so")
            })
            .and_then(|seq| seq.parse::<u64>().ok())
            .is_some_and(|seq| seq.saturating_add(1) < current);
        if superseded {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn spawn_thin_dev_binary_reported(
    app_dir: &Path,
    executable: &Path,
    forwarded: &[OsString],
) -> Option<Child> {
    match spawn_thin_dev_binary(app_dir, executable, forwarded) {
        Ok(child) => Some(child),
        Err(error) => {
            eprintln!("[tilo-app] {error:#}; waiting for a change");
            None
        }
    }
}

fn spawn_thin_dev_binary(
    app_dir: &Path,
    executable: &Path,
    forwarded: &[OsString],
) -> Result<Child> {
    let mut command = ProcessCommand::new(executable);
    command.args(forwarded).current_dir(app_dir);
    attach_live_graph_env(&mut command, app_dir);
    let _chooser = attach_dev_session_env(&mut command, app_dir);
    attach_dynamic_loader_env(&mut command, app_dir, executable);
    #[cfg(unix)]
    command.process_group(0);
    let child = command
        .spawn()
        .with_context(|| format!("starting reload host {}", executable.display()))?;
    eprintln!("[tilo-app] started (pid {})", child.id());
    Ok(child)
}

/// The host and the library it loads share `libstd` and `libruntime_dylib`.
/// A toolkit built as a dependency stays in `deps/`, beside the host.
fn attach_dynamic_loader_env(command: &mut ProcessCommand, app_dir: &Path, executable: &Path) {
    let mut dirs = Vec::new();
    if let Some(parent) = executable.parent() {
        dirs.push(parent.to_path_buf());
        dirs.push(parent.join("deps"));
    }
    if let Some(libdir) = rustc_target_libdir(app_dir) {
        dirs.push(libdir);
    }
    if let Some(existing) = std::env::var_os("LD_LIBRARY_PATH") {
        dirs.push(PathBuf::from(existing));
    }
    let mut value = OsString::new();
    for (index, dir) in dirs.iter().enumerate() {
        if index > 0 {
            value.push(":");
        }
        value.push(dir);
    }
    if !value.is_empty() {
        command.env("LD_LIBRARY_PATH", value);
    }
}

/// The `libstd` of the compiler the app's `rust-toolchain.toml` selects.
fn rustc_target_libdir(app_dir: &Path) -> Option<PathBuf> {
    let output = ProcessCommand::new("rustc")
        .arg("--print")
        .arg("target-libdir")
        .current_dir(app_dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let path = PathBuf::from(text.trim());
    path.is_dir().then_some(path)
}

/// Ask the running app to swap the contents of the windows it already has.
///
/// The process stays. A new process would be a new window.
/// Without in-place reload the running app is the old build and cannot load
/// the new one, so it is replaced by `executable`.
fn restart_dev_app(
    child: &mut Option<Child>,
    app_dir: &Path,
    executable: &Path,
    forwarded: &[OsString],
) {
    if child
        .as_ref()
        .is_some_and(|running| !child_has_exited(running))
    {
        eprintln!("[tilo-app] build succeeded; restarting the app");
    } else {
        eprintln!("[tilo-app] build succeeded; opening the app");
    }
    stop_dev_child(child);
    *child = spawn_dev_binary_reported(app_dir, executable, forwarded);
}

fn reload_running_dev_app(child: &Child) -> bool {
    let Ok(pid) = i32::try_from(child.id()) else {
        return false;
    };
    if child_has_exited(child) {
        return false;
    }
    // SAFETY: `pid` is the dev app this loop spawned. SIGUSR1 asks it to
    // reload in place; the app installs a handler for that signal.
    unsafe { libc::kill(pid, libc::SIGUSR1) == 0 }
}

fn child_has_exited(child: &Child) -> bool {
    let Ok(pid) = i32::try_from(child.id()) else {
        return true;
    };
    // SAFETY: a signal of 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) != 0 }
}

fn stop_dev_child(child: &mut Option<Child>) {
    let Some(mut running) = child.take() else {
        return;
    };
    if running.try_wait().ok().flatten().is_some() {
        return;
    }
    stop_dev_process_tree(&mut running);
}

#[cfg(unix)]
fn stop_dev_process_tree(running: &mut Child) {
    let Ok(pid) = i32::try_from(running.id()) else {
        let _ = running.kill();
        let _ = running.wait();
        return;
    };
    // SAFETY: both development spawn paths create a process group whose ID is
    // the child PID. Negating that validated PID confines SIGTERM to it.
    let _ = unsafe { libc::kill(pid.saturating_neg(), libc::SIGTERM) };
    let Some(deadline) = Instant::now().checked_add(Duration::from_secs(1)) else {
        let _ = running.kill();
        let _ = running.wait();
        return;
    };
    while Instant::now() < deadline {
        if running.try_wait().ok().flatten().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // SAFETY: the same dedicated process-group invariant applies to SIGKILL.
    let _ = unsafe { libc::kill(pid.saturating_neg(), libc::SIGKILL) };
    let _ = running.wait();
}

#[cfg(windows)]
fn stop_dev_process_tree(running: &mut Child) {
    let pid = running.id().to_string();
    let stopped_tree = ProcessCommand::new("taskkill")
        .args(["/PID", &pid, "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if !stopped_tree {
        let _ = running.kill();
    }
    let _ = running.wait();
}

#[cfg(not(any(unix, windows)))]
fn stop_dev_process_tree(running: &mut Child) {
    let _ = running.kill();
    let _ = running.wait();
}

extern "C" fn request_dev_stop(_signal: libc::c_int) {
    DEV_STOP_REQUESTED.store(true, Ordering::Relaxed);
}

struct DevSignalGuard {
    previous_interrupt: libc::sighandler_t,
    previous_terminate: libc::sighandler_t,
}

impl DevSignalGuard {
    fn install() -> Result<Self> {
        DEV_STOP_REQUESTED.store(false, Ordering::Relaxed);
        // SAFETY: the handler only stores to a lock-free atomic and has the C
        // signal-handler ABI. The previous disposition is retained for Drop.
        let previous_interrupt = unsafe {
            libc::signal(
                libc::SIGINT,
                request_dev_stop as *const () as libc::sighandler_t,
            )
        };
        if previous_interrupt == libc::sighandler_t::MAX {
            bail!("installing Ctrl-C handler failed");
        }
        // SAFETY: identical invariant to the SIGINT registration above.
        let previous_terminate = unsafe {
            libc::signal(
                libc::SIGTERM,
                request_dev_stop as *const () as libc::sighandler_t,
            )
        };
        if previous_terminate == libc::sighandler_t::MAX {
            // SAFETY: restore the valid disposition returned by `signal`.
            unsafe { libc::signal(libc::SIGINT, previous_interrupt) };
            bail!("installing termination handler failed");
        }
        Ok(Self {
            previous_interrupt,
            previous_terminate,
        })
    }
}

impl Drop for DevSignalGuard {
    fn drop(&mut self) {
        // SAFETY: both values were returned by successful `signal` calls.
        unsafe {
            libc::signal(libc::SIGINT, self.previous_interrupt);
            libc::signal(libc::SIGTERM, self.previous_terminate);
        }
    }
}

fn check_cargo_manifest(app_dir: &Path, checks: &mut Vec<DoctorCheck>) -> Option<String> {
    let path = app_dir.join("Cargo.toml");
    let source = match std::fs::read_to_string(&path) {
        Ok(source) => source,
        Err(error) => {
            checks.push(DoctorCheck {
                status: DoctorStatus::Fail,
                label: "Cargo manifest",
                detail: format!("{}: {error}", path.display()),
            });
            return None;
        }
    };
    let manifest = match source.parse::<toml::Table>() {
        Ok(manifest) => manifest,
        Err(error) => {
            checks.push(DoctorCheck {
                status: DoctorStatus::Fail,
                label: "Cargo manifest",
                detail: error.to_string(),
            });
            return None;
        }
    };
    let name = manifest
        .get("package")
        .and_then(|package| package.get("name"))
        .and_then(toml::Value::as_str)
        .map(str::to_owned);
    checks.push(DoctorCheck {
        status: if name.is_some() {
            DoctorStatus::Pass
        } else {
            DoctorStatus::Fail
        },
        label: "Cargo manifest",
        detail: name.as_ref().map_or_else(
            || "missing [package].name".to_string(),
            |name| format!("package {name}"),
        ),
    });
    check_ui_dependency(app_dir, &manifest, checks);
    name
}

fn check_package_manifest(
    app_dir: &Path,
    checks: &mut Vec<DoctorCheck>,
) -> Option<tilo_app_package::Manifest> {
    let path = app_dir.join("tilo-app.toml");
    let source = match std::fs::read_to_string(&path) {
        Ok(source) => source,
        Err(error) => {
            checks.push(DoctorCheck {
                status: DoctorStatus::Fail,
                label: "Tilo manifest",
                detail: format!("{}: {error}", path.display()),
            });
            return None;
        }
    };
    let manifest = match tilo_app_package::Manifest::from_toml_str(&source) {
        Ok(manifest) => manifest,
        Err(error) => {
            checks.push(DoctorCheck {
                status: DoctorStatus::Fail,
                label: "Tilo manifest",
                detail: error.to_string(),
            });
            return None;
        }
    };
    if let Err(error) = tilo_app_package::validate_manifest(&manifest) {
        checks.push(DoctorCheck {
            status: DoctorStatus::Fail,
            label: "Tilo manifest",
            detail: error.to_string(),
        });
        return None;
    }
    checks.push(DoctorCheck {
        status: DoctorStatus::Pass,
        label: "Tilo manifest",
        detail: format!("{} is schema-valid", manifest.package.id),
    });
    Some(manifest)
}

fn check_ui_dependency(app_dir: &Path, manifest: &toml::Table, checks: &mut Vec<DoctorCheck>) {
    let direct_dependency = manifest
        .get("dependencies")
        .and_then(|table| table.get("ui"))
        .into_iter();
    let target_dependencies = manifest
        .get("target")
        .and_then(toml::Value::as_table)
        .into_iter()
        .flat_map(toml::Table::values)
        .filter_map(|target| target.get("dependencies").and_then(|table| table.get("ui")));
    let evaluate = |ui: &toml::Value| {
        let package = ui.get("package").and_then(toml::Value::as_str);
        let path = ui.get("path").and_then(toml::Value::as_str);
        match (package, path) {
            (None, Some(_)) => (
                DoctorStatus::Fail,
                "dependency `ui` must declare `package = \"tilo-app\"`; depending on rendering core directly bypasses native app contracts".to_owned(),
            ),
            (Some("tilo-app"), Some(path)) => {
                let resolved = app_dir.join(path);
                let sdk_manifest = std::fs::read_to_string(resolved.join("Cargo.toml"))
                    .ok()
                    .and_then(|source| source.parse::<toml::Table>().ok());
                let is_sdk = sdk_manifest.as_ref().is_some_and(|manifest| {
                    manifest
                        .get("package")
                        .and_then(|package| package.get("name"))
                        .and_then(toml::Value::as_str)
                        == Some("tilo-app")
                });
                if is_sdk {
                    (DoctorStatus::Pass, format!("{}", resolved.display()))
                } else {
                    (
                        DoctorStatus::Fail,
                        format!("path {} is not a tilo-app SDK crate", resolved.display()),
                    )
                }
            }
            (Some("tilo-app"), None) => match ui.get("git").and_then(toml::Value::as_str) {
                Some(url) => {
                    let pin = ["rev", "tag", "branch"].iter().find_map(|key| {
                        ui.get(key)
                            .and_then(toml::Value::as_str)
                            .map(|value| format!(" {key} {value}"))
                    });
                    (
                        DoctorStatus::Pass,
                        format!("{url}{}", pin.as_deref().unwrap_or(" (default branch)")),
                    )
                }
                None => match ui.get("version").and_then(toml::Value::as_str) {
                    Some(version) => (DoctorStatus::Pass, format!("tilo-app {version} from the registry")),
                    None => (
                        DoctorStatus::Warning,
                        "non-path tilo-app dependency; SDK source could not be verified".into(),
                    ),
                },
            },
            (Some(package), _) => (
                DoctorStatus::Fail,
                format!("dependency `ui` renames `{package}`, expected the `tilo-app` SDK"),
            ),
            (None, None) => (
                DoctorStatus::Fail,
                "dependency `ui` must rename the `tilo-app` SDK".to_string(),
            ),
        }
    };
    let results = direct_dependency
        .chain(target_dependencies)
        .map(evaluate)
        .collect::<Vec<_>>();
    let (status, detail) = if results.is_empty() {
        (DoctorStatus::Fail, "missing dependency `ui`".to_string())
    } else if let Some(failed) = results
        .iter()
        .find(|(status, _)| *status == DoctorStatus::Fail)
    {
        failed.clone()
    } else if let Some(warning) = results
        .iter()
        .find(|(status, _)| *status == DoctorStatus::Warning)
    {
        warning.clone()
    } else {
        (
            DoctorStatus::Pass,
            results
                .into_iter()
                .map(|(_, detail)| detail)
                .collect::<Vec<_>>()
                .join(", "),
        )
    };
    checks.push(DoctorCheck {
        status,
        label: "Tilo app SDK",
        detail,
    });
}

/// Check that `relative` uses one of the `expected` generated contracts: the
/// framework's, listed first, or an older generated app's.
fn check_source_file(
    app_dir: &Path,
    relative: &'static str,
    label: &'static str,
    expected: &[&str],
    checks: &mut Vec<DoctorCheck>,
) {
    let path = app_dir.join(relative);
    let (status, detail) = match std::fs::read_to_string(&path) {
        Ok(source) if expected.iter().any(|needle| source.contains(needle)) => {
            (DoctorStatus::Pass, relative.to_string())
        }
        Ok(_) => (
            DoctorStatus::Warning,
            format!(
                "{relative} does not use the generated {} contract",
                expected.first().copied().unwrap_or_default()
            ),
        ),
        Err(error) => (DoctorStatus::Fail, format!("{relative}: {error}")),
    };
    checks.push(DoctorCheck {
        status,
        label,
        detail,
    });
}

#[derive(Clone, Debug)]
struct GraphComponentUse {
    screen: String,
    node: String,
    name: String,
    props: serde_json::Map<String, serde_json::Value>,
}

const GRAPH_PROPS_MARKER: &str = "// tilo-app:graph-props=";

#[derive(Debug)]
struct ComponentRegistryInspection {
    components: Vec<String>,
    graph_contracts: BTreeMap<String, GraphPropsContract>,
}

fn check_ui_graph_artifacts(app_dir: &Path, checks: &mut Vec<DoctorCheck>) {
    let files = match files_with_extension(&app_dir.join("ui"), "json") {
        Ok(files) => files,
        Err(detail) => {
            checks.push(DoctorCheck {
                status: DoctorStatus::Fail,
                label: "UI graphs",
                detail,
            });
            check_graph_component_contract(app_dir, &[], checks);
            return;
        }
    };
    if files.is_empty() {
        checks.push(DoctorCheck {
            status: DoctorStatus::Pass,
            label: "UI graphs",
            detail: "no serialized screens".to_owned(),
        });
        check_graph_component_contract(app_dir, &[], checks);
        return;
    }
    let mut component_uses = Vec::new();
    for path in files {
        let detail = relative_artifact(app_dir, &path);
        let result = std::fs::read_to_string(&path)
            .map_err(anyhow::Error::from)
            .and_then(|source| tilo_ui_graph::UiDocument::from_json(&source).map_err(Into::into));
        let (status, detail) = match result {
            Ok(document) => {
                component_uses.extend(document.nodes().into_iter().filter_map(|node| match node {
                    tilo_ui_graph::UiNode::Component {
                        id, name, props, ..
                    } => Some(GraphComponentUse {
                        screen: detail.clone(),
                        node: id.clone(),
                        name: name.clone(),
                        props: props.clone(),
                    }),
                    _ => None,
                }));
                (DoctorStatus::Pass, detail)
            }
            Err(error) => (DoctorStatus::Fail, format!("{detail}: {error}")),
        };
        checks.push(DoctorCheck {
            status,
            label: "UI graph",
            detail,
        });
    }
    check_graph_component_contract(app_dir, &component_uses, checks);
}

fn check_graph_component_contract(
    app_dir: &Path,
    uses: &[GraphComponentUse],
    checks: &mut Vec<DoctorCheck>,
) {
    let source = std::fs::read_to_string(app_dir.join("src/lib.rs")).unwrap_or_default();
    let app_owned_context = !source.contains("components::ui_graph_context()");
    let registry = match inspect_component_registry(app_dir) {
        Ok(Some(inspection)) => {
            checks.push(DoctorCheck {
                status: DoctorStatus::Pass,
                label: "component registry",
                detail: format!(
                    "{} component module(s), {} graph factory/factories",
                    inspection.components.len(),
                    inspection.graph_contracts.len()
                ),
            });
            Some(inspection)
        }
        Ok(None) if uses.is_empty() || app_owned_context => None,
        Ok(None) => {
            checks.push(DoctorCheck {
                status: DoctorStatus::Fail,
                label: "component registry",
                detail: "serialized component nodes require generated src/components/mod.rs"
                    .to_owned(),
            });
            None
        }
        Err(error) => {
            checks.push(DoctorCheck {
                status: DoctorStatus::Fail,
                label: "component registry",
                detail: error.to_string(),
            });
            return;
        }
    };
    if uses.is_empty() {
        return;
    }
    if app_owned_context {
        checks.push(DoctorCheck {
            status: DoctorStatus::Warning,
            label: "UI components",
            detail: format!(
                "{} serialized component node(s) use an app-owned build context; factory registration cannot be checked structurally",
                uses.len()
            ),
        });
        return;
    }
    let Some(inspection) = registry else {
        return;
    };
    for usage in uses {
        check_graph_component_use(
            usage,
            &inspection.components,
            &inspection.graph_contracts,
            checks,
        );
    }
}

fn inspect_component_registry(app_dir: &Path) -> Result<Option<ComponentRegistryInspection>> {
    let path = app_dir.join("src/components/mod.rs");
    let source = match std::fs::read_to_string(&path) {
        Ok(source) => source,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    syn::parse_file(&source).with_context(|| format!("parsing {}", path.display()))?;
    let components = parse_component_registry(&source)?;
    let graph_components = parse_graph_component_registry(&source, &components)?;
    let mut graph_contracts = BTreeMap::new();
    for name in &components {
        let (module, _) = component_rust_names(name)?;
        let module_path = app_dir.join("src/components").join(format!("{module}.rs"));
        let module_source = std::fs::read_to_string(&module_path)
            .with_context(|| format!("reading registered component {name:?}"))?;
        syn::parse_file(&module_source)
            .with_context(|| format!("parsing registered component {name:?}"))?;
        if graph_components.contains(name) {
            if !module_source.contains("pub fn register_ui_graph(") {
                bail!("graph component {name:?} has no register_ui_graph factory");
            }
            graph_contracts.insert(name.clone(), graph_props_contract(name, &module_source)?);
        }
    }
    Ok(Some(ComponentRegistryInspection {
        components,
        graph_contracts,
    }))
}

fn graph_props_contract(name: &str, source: &str) -> Result<GraphPropsContract> {
    let markers: Vec<_> = source
        .lines()
        .filter_map(|line| line.strip_prefix(GRAPH_PROPS_MARKER))
        .collect();
    match markers.as_slice() {
        [] | ["display-v1"] => Ok(GraphPropsContract::DisplayV1),
        ["form-v2"] => Ok(GraphPropsContract::FormV2),
        [unknown] => bail!("graph component {name:?} has unknown props contract {unknown:?}"),
        _ => bail!("graph component {name:?} declares more than one props contract"),
    }
}

fn check_graph_component_use(
    usage: &GraphComponentUse,
    components: &[String],
    graph_components: &BTreeMap<String, GraphPropsContract>,
    checks: &mut Vec<DoctorCheck>,
) {
    let location = format!("{} node {:?}", usage.screen, usage.node);
    let Some(contract) = graph_components.get(&usage.name) else {
        let detail = if components.iter().any(|name| name == &usage.name) {
            format!(
                "{location} uses component {:?}, but that generated component has no graph factory",
                usage.name
            )
        } else {
            format!(
                "{location} uses unregistered component {:?}; generate it with `tilo-app add component {} --mount {}`",
                usage.name,
                usage.name,
                screen_name_from_detail(&usage.screen)
            )
        };
        checks.push(DoctorCheck {
            status: DoctorStatus::Fail,
            label: "UI component",
            detail,
        });
        return;
    };
    let invalid = contract.invalid_props(&usage.props);
    if let Some(invalid) = invalid {
        checks.push(DoctorCheck {
            status: DoctorStatus::Fail,
            label: "UI component",
            detail: format!("{location} component {:?} {invalid}", usage.name),
        });
        return;
    }
    checks.push(DoctorCheck {
        status: DoctorStatus::Pass,
        label: "UI component",
        detail: format!("{location} → {:?}", usage.name),
    });
}

fn screen_name_from_detail(detail: &str) -> &str {
    detail
        .strip_prefix("ui/")
        .and_then(|value| value.strip_suffix(".json"))
        .unwrap_or("main")
}

#[derive(Debug, PartialEq, Eq)]
struct FluentEntryContract {
    has_value: bool,
    attributes: BTreeSet<String>,
    value_variables: BTreeSet<String>,
    attribute_variables: BTreeMap<String, BTreeSet<String>>,
}

#[derive(Debug, PartialEq, Eq)]
struct FluentCatalogContract {
    entries: BTreeMap<String, FluentEntryContract>,
}

fn parse_fluent_catalog(source: &str) -> Result<FluentCatalogContract> {
    use fluent_syntax::ast::Entry;

    let resource = fluent::FluentResource::try_new(source.to_owned())
        .map_err(|(_, errors)| anyhow::anyhow!("{errors:?}"))?;
    let mut entries = BTreeMap::new();
    for entry in resource.entries() {
        let (id, contract) = match entry {
            Entry::Message(message) => (
                message.id.name.to_owned(),
                fluent_entry_contract(message.value.as_ref(), &message.attributes),
            ),
            Entry::Term(term) => (
                format!("-{}", term.id.name),
                fluent_entry_contract(Some(&term.value), &term.attributes),
            ),
            Entry::Comment(_)
            | Entry::GroupComment(_)
            | Entry::ResourceComment(_)
            | Entry::Junk { .. } => continue,
        };
        if entries.insert(id.clone(), contract).is_some() {
            bail!("duplicate Fluent entry {id:?}");
        }
    }
    Ok(FluentCatalogContract { entries })
}

fn fluent_entry_contract(
    value: Option<&fluent_syntax::ast::Pattern<&str>>,
    attributes: &[fluent_syntax::ast::Attribute<&str>],
) -> FluentEntryContract {
    FluentEntryContract {
        has_value: value.is_some(),
        attributes: attributes
            .iter()
            .map(|attribute| attribute.id.name.to_owned())
            .collect(),
        value_variables: value.map_or_else(BTreeSet::new, fluent_pattern_variables),
        attribute_variables: attributes
            .iter()
            .map(|attribute| {
                (
                    attribute.id.name.to_owned(),
                    fluent_pattern_variables(&attribute.value),
                )
            })
            .collect(),
    }
}

fn fluent_pattern_variables<S: AsRef<str>>(
    pattern: &fluent_syntax::ast::Pattern<S>,
) -> BTreeSet<String> {
    use fluent_syntax::ast::PatternElement;

    let mut variables = BTreeSet::new();
    for element in &pattern.elements {
        if let PatternElement::Placeable { expression } = element {
            collect_fluent_expression_variables(expression, &mut variables);
        }
    }
    variables
}

fn collect_fluent_expression_variables<S: AsRef<str>>(
    expression: &fluent_syntax::ast::Expression<S>,
    variables: &mut BTreeSet<String>,
) {
    use fluent_syntax::ast::Expression;

    match expression {
        Expression::Inline(inline) => collect_fluent_inline_variables(inline, variables),
        Expression::Select { selector, variants } => {
            collect_fluent_inline_variables(selector, variables);
            for variant in variants {
                variables.extend(fluent_pattern_variables(&variant.value));
            }
        }
    }
}

fn collect_fluent_inline_variables<S: AsRef<str>>(
    expression: &fluent_syntax::ast::InlineExpression<S>,
    variables: &mut BTreeSet<String>,
) {
    use fluent_syntax::ast::InlineExpression;

    match expression {
        InlineExpression::VariableReference { id } => {
            variables.insert(id.name.as_ref().to_owned());
        }
        InlineExpression::FunctionReference { arguments, .. } => {
            collect_fluent_argument_variables(arguments, variables);
        }
        InlineExpression::TermReference {
            arguments: Some(arguments),
            ..
        } => collect_fluent_argument_variables(arguments, variables),
        InlineExpression::Placeable { expression } => {
            collect_fluent_expression_variables(expression, variables);
        }
        InlineExpression::StringLiteral { .. }
        | InlineExpression::NumberLiteral { .. }
        | InlineExpression::MessageReference { .. }
        | InlineExpression::TermReference {
            arguments: None, ..
        } => {}
    }
}

fn collect_fluent_argument_variables<S: AsRef<str>>(
    arguments: &fluent_syntax::ast::CallArguments<S>,
    variables: &mut BTreeSet<String>,
) {
    for argument in &arguments.positional {
        collect_fluent_inline_variables(argument, variables);
    }
    for argument in &arguments.named {
        collect_fluent_inline_variables(&argument.value, variables);
    }
}

fn fluent_contract_drift(
    source: &FluentCatalogContract,
    translation: &FluentCatalogContract,
) -> Vec<String> {
    let mut drift = Vec::new();
    for (id, expected) in &source.entries {
        let Some(actual) = translation.entries.get(id) else {
            drift.push(format!("missing {id}"));
            continue;
        };
        if expected.has_value && !actual.has_value {
            drift.push(format!("{id} is missing its value"));
        } else if !expected.has_value && actual.has_value {
            drift.push(format!("{id} has an unexpected value"));
        } else if expected.value_variables != actual.value_variables {
            drift.push(format!(
                "{id} variables are {} instead of {}",
                fluent_variable_list(&actual.value_variables),
                fluent_variable_list(&expected.value_variables)
            ));
        }
        for attribute in expected.attributes.difference(&actual.attributes) {
            drift.push(format!("missing {id}.{attribute}"));
        }
        for attribute in actual.attributes.difference(&expected.attributes) {
            drift.push(format!("stale {id}.{attribute}"));
        }
        for attribute in expected.attributes.intersection(&actual.attributes) {
            let Some(expected_variables) = expected.attribute_variables.get(attribute) else {
                continue;
            };
            let Some(actual_variables) = actual.attribute_variables.get(attribute) else {
                continue;
            };
            if expected_variables != actual_variables {
                drift.push(format!(
                    "{id}.{attribute} variables are {} instead of {}",
                    fluent_variable_list(actual_variables),
                    fluent_variable_list(expected_variables)
                ));
            }
        }
    }
    for id in translation.entries.keys() {
        if !source.entries.contains_key(id) {
            drift.push(format!("stale {id}"));
        }
    }
    drift
}

fn fluent_variable_list(variables: &BTreeSet<String>) -> String {
    if variables.is_empty() {
        "none".to_owned()
    } else {
        variables
            .iter()
            .map(|variable| format!("${variable}"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn check_locale_artifacts(app_dir: &Path, checks: &mut Vec<DoctorCheck>) {
    let registry_path = app_dir.join("src/locales.rs");
    let files = match files_with_extension(&app_dir.join("locales"), "ftl") {
        Ok(files) => files,
        Err(detail) => {
            checks.push(DoctorCheck {
                status: DoctorStatus::Fail,
                label: "locales",
                detail,
            });
            return;
        }
    };
    if files.is_empty() && !registry_path.exists() {
        checks.push(DoctorCheck {
            status: DoctorStatus::Pass,
            label: "locales",
            detail: "no embedded catalogs".to_owned(),
        });
        return;
    }
    let mut catalog_ids = Vec::new();
    let mut catalogs = BTreeMap::new();
    for path in &files {
        let relative = relative_artifact(app_dir, path);
        let locale = path.file_stem().and_then(std::ffi::OsStr::to_str);
        let result = locale
            .ok_or_else(|| anyhow::anyhow!("filename is not valid UTF-8"))
            .and_then(|locale| {
                validate_locale(locale)?;
                catalog_ids.push(locale.to_owned());
                let source = std::fs::read_to_string(path)?;
                let contract = parse_fluent_catalog(&source)?;
                catalogs.insert(locale.to_owned(), contract);
                Ok(())
            });
        let (status, detail) = match result {
            Ok(()) => (DoctorStatus::Pass, relative),
            Err(error) => (DoctorStatus::Fail, format!("{relative}: {error}")),
        };
        checks.push(DoctorCheck {
            status,
            label: "locale catalog",
            detail,
        });
    }
    if !catalog_ids.iter().any(|locale| locale == "en-US") {
        checks.push(DoctorCheck {
            status: DoctorStatus::Fail,
            label: "source locale",
            detail: "missing locales/en-US.ftl".to_owned(),
        });
    } else if let Some(source) = catalogs.get("en-US") {
        for (locale, catalog) in &catalogs {
            if locale == "en-US" {
                continue;
            }
            let drift = fluent_contract_drift(source, catalog);
            checks.push(DoctorCheck {
                status: if drift.is_empty() {
                    DoctorStatus::Pass
                } else {
                    DoctorStatus::Warning
                },
                label: "locale contract",
                detail: if drift.is_empty() {
                    format!("locales/{locale}.ftl matches en-US")
                } else {
                    format!("locales/{locale}.ftl: {}", drift.join(", "))
                },
            });
        }
    }
    let source_ids = catalogs.get("en-US").map(catalog_message_ids);
    check_locale_registry(&registry_path, &catalog_ids, source_ids.as_deref(), checks);
}

fn check_locale_registry(
    registry_path: &Path,
    catalog_ids: &[String],
    source_ids: Option<&[String]>,
    checks: &mut Vec<DoctorCheck>,
) {
    let Ok(registry) = std::fs::read_to_string(registry_path) else {
        checks.push(DoctorCheck {
            status: DoctorStatus::Warning,
            label: "locale registry",
            detail: "src/locales.rs is absent or unreadable; app-owned setup was not inspected"
                .to_owned(),
        });
        return;
    };
    if !registry.starts_with(LOCALE_REGISTRY_HEADER) {
        checks.push(DoctorCheck {
            status: DoctorStatus::Warning,
            label: "locale registry",
            detail: "src/locales.rs is app-owned and was not inspected".to_owned(),
        });
        return;
    }
    let registered = parse_locale_registry(&registry);
    let result = registered.and_then(|registered| {
        syn::parse_file(&registry).context("generated locale registry is not valid Rust")?;
        for (index, locale) in registered.iter().enumerate() {
            let catalog =
                format!("const CATALOG_{index}: &str = include_str!(\"../locales/{locale}.ftl\");");
            if !registry.contains(&catalog) {
                bail!("registered locale {locale:?} is not embedded as CATALOG_{index}");
            }
            let mapping = format!("({locale:?}, CATALOG_{index})");
            if !registry.contains(&mapping) {
                bail!("registered locale {locale:?} is absent from the install mapping");
            }
        }
        if !registry.contains("pub const IDS:") {
            bail!("locale id registry is absent");
        }
        if !registry.contains("I18nManager::install_system") {
            bail!("locale installation is absent");
        }
        if let Some(source) = source_ids {
            let drift = label_key_drift(&registry, source)?;
            if !drift.is_empty() {
                bail!(
                    "label keys differ from locales/en-US.ftl ({}); any `tilo-app` build or `tilo-app locale sync` regenerates them",
                    drift.join(", ")
                );
            }
        }
        for locale in &registered {
            if !catalog_ids.iter().any(|catalog| catalog == locale) {
                bail!("registered locale {locale:?} has no locales/{locale}.ftl");
            }
        }
        let unregistered: Vec<_> = catalog_ids
            .iter()
            .filter(|locale| !registered.iter().any(|entry| entry == *locale))
            .cloned()
            .collect();
        if !unregistered.is_empty() {
            bail!("catalogs are not registered: {}", unregistered.join(", "));
        }
        Ok(registered.len())
    });
    checks.push(DoctorCheck {
        status: if result.is_ok() {
            DoctorStatus::Pass
        } else {
            DoctorStatus::Fail
        },
        label: "locale registry",
        detail: result.map_or_else(
            |error| format!("src/locales.rs: {error}"),
            |count| format!("{count} embedded catalog(s)"),
        ),
    });
}

fn check_headless_scenarios(app_dir: &Path, checks: &mut Vec<DoctorCheck>) {
    let mut files = Vec::new();
    let conventional = app_dir.join("tests/headless-actions.json");
    if conventional.is_file() {
        files.push(conventional);
    }
    match files_with_extension(&app_dir.join("tests/scenarios"), "json") {
        Ok(mut scenarios) => files.append(&mut scenarios),
        Err(detail) => {
            checks.push(DoctorCheck {
                status: DoctorStatus::Warning,
                label: "headless scenarios",
                detail,
            });
            return;
        }
    }
    if files.is_empty() {
        checks.push(DoctorCheck {
            status: DoctorStatus::Pass,
            label: "headless scenarios",
            detail: "no saved action sequences".to_owned(),
        });
        return;
    }
    for path in files {
        let relative = relative_artifact(app_dir, &path);
        let result = std::fs::read_to_string(&path)
            .map_err(|error| error.to_string())
            .and_then(|source| tilo_headless_schema::parse_actions(&source).map(|_| ()));
        let (status, detail) = match result {
            Ok(()) => (DoctorStatus::Pass, relative),
            Err(error) => (DoctorStatus::Warning, format!("{relative}: {error}")),
        };
        checks.push(DoctorCheck {
            status,
            label: "headless scenario",
            detail,
        });
    }
}

fn files_with_extension(directory: &Path, extension: &str) -> Result<Vec<PathBuf>, String> {
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let entries = std::fs::read_dir(directory)
        .map_err(|error| format!("{}: {error}", directory.display()))?;
    let mut files = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|error| format!("{}: {error}", directory.display()))?
            .path();
        if path.is_file()
            && path
                .extension()
                .is_some_and(|candidate| candidate.eq_ignore_ascii_case(extension))
        {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn relative_artifact(app_dir: &Path, path: &Path) -> String {
    path.strip_prefix(app_dir)
        .unwrap_or(path)
        .display()
        .to_string()
}

impl CreateOutcome {
    /// One-line result suitable when another workflow starts immediately.
    #[must_use]
    pub fn headline(&self) -> String {
        let action = if self.dry_run {
            "Would create"
        } else {
            "Created"
        };
        format!(
            "{action} {} ({}) in {}",
            self.package_name,
            self.app_id,
            self.destination.display()
        )
    }

    #[must_use]
    pub fn summary(&self) -> String {
        let destination = shell_display_path(&self.destination);
        if self.dry_run {
            let mut summary = self.headline();
            for file in &self.files {
                let shown = file.strip_prefix(&self.destination).unwrap_or(file);
                let _ = write!(summary, "\n  {}", shown.display());
            }
            return summary;
        }
        let develop = format!("tilo-app dev {destination}");
        let verify = format!("tilo-app check {destination}");
        let mut notes = String::new();
        if let Some(sdk) = &self.sdk {
            let _ = write!(notes, "\n\nSDK: {}", sdk.describe());
        }
        if self.git_initialized {
            notes.push_str("\n\nInitialised a git repository for the app.");
        }
        if self.app_id.starts_with("local.") {
            let _ = write!(
                notes,
                "\n\nThe app id {} is for local development. Change it in tilo-app.toml and src/lib.rs to a namespace you control before publishing.",
                self.app_id
            );
        }
        format!(
            "{}{notes}\n\nStart developing:\n  {develop}\n\nVerify everything:\n  {verify}\n\nRun `tilo-app --help` for testing, scenarios, screenshots, inspection, localization, and packaging.",
            self.headline()
        )
    }
}

#[derive(Debug)]
struct AppSpec {
    package_name: String,
    crate_name: String,
    title: String,
    app_id: String,
    sdk: AppSdk,
    template: Template,
}

impl AppSpec {
    /// Whether the app offers the `thin` build against the toolkit dylib.
    fn links_toolkit_dylib(&self) -> bool {
        match &self.sdk {
            AppSdk::Workspace { .. } => true,
            AppSdk::Standalone(standalone) => !standalone.source.is_registry(),
        }
    }
}

#[derive(Debug)]
enum AppSdk {
    /// A member of the Tilo workspace, with the path from the app to `ui-core`.
    Workspace {
        ui_path: PathBuf,
    },
    Standalone(standalone::Standalone),
}

/// Create an app, prompting for missing identity, title, and template fields
/// when attached to a terminal.
///
/// # Errors
///
/// Returns an error for invalid input, a non-interactive missing name, an
/// occupied destination, or any failed filesystem operation.
pub fn create_app_interactive(request: NewAppRequest) -> Result<CreateOutcome> {
    let interactive = io::stdin().is_terminal();
    let stdin = io::stdin();
    let stdout = io::stdout();
    create_app_interactive_with(request, &mut stdin.lock(), &mut stdout.lock(), interactive)
}

/// The wizard's answer that selects the Tilo repository over a checkout.
const SDK_FROM_GIT: &str = "git";

fn create_app_interactive_with(
    request: NewAppRequest,
    input: &mut impl io::BufRead,
    output: &mut impl io::Write,
    interactive: bool,
) -> Result<CreateOutcome> {
    let current_dir = std::env::current_dir().context("reading current directory")?;
    create_app_interactive_from(&current_dir, request, input, output, interactive)
}

/// Ask for the checkout to build against, defaulting to the one discovered, or
/// to the Tilo repository when there is none.
fn prompt_sdk(
    current_dir: &Path,
    request: &mut NewAppRequest,
    input: &mut impl io::BufRead,
    output: &mut impl io::Write,
) -> Result<()> {
    let discovered = discover_ui_dir(current_dir, None).map(|path| path.display().to_string());
    let value = prompt_validated(
        input,
        output,
        "Toolkit ui path, or `git` for the Tilo repository",
        Some(discovered.as_deref().unwrap_or(SDK_FROM_GIT)),
        |value| {
            if value == SDK_FROM_GIT {
                return Ok(());
            }
            resolve_ui_dir(current_dir, None, Some(Path::new(value)))
                .map(|_| ())
                .context("invalid toolkit path")
        },
    )?;
    if value == SDK_FROM_GIT {
        request.sdk_git = Some(standalone::DEFAULT_SDK_GIT.to_owned());
    } else {
        request.ui_path = Some(PathBuf::from(value));
    }
    Ok(())
}

fn create_app_interactive_from(
    current_dir: &Path,
    mut request: NewAppRequest,
    input: &mut impl io::BufRead,
    output: &mut impl io::Write,
    interactive: bool,
) -> Result<CreateOutcome> {
    let full_wizard = request.name.is_none();
    if full_wizard {
        if !interactive {
            bail!("an app name is required when stdin is not interactive");
        }
        writeln!(output, "Create a Tilo app\n").context("showing wizard heading")?;
        request.name = Some(prompt_validated(
            input,
            output,
            "Package name",
            None,
            validate_package_name,
        )?);
    }
    let name = request.name.as_deref().unwrap_or_default();
    let default_title = title_from_name(name);
    if request.title.is_none() && interactive {
        request.title = Some(prompt(input, output, "App title", Some(&default_title))?);
    }
    let repo = find_repo_root(current_dir);
    if request.app_id.is_none() && full_wizard {
        let workspace_member = repo.is_some()
            && request.directory.as_deref().is_none_or(|dir| {
                repo.as_deref()
                    .is_some_and(|root| current_dir.join(dir).starts_with(root))
            });
        let default_id = default_app_id(name, workspace_member);
        request.app_id = Some(prompt_validated(
            input,
            output,
            "App id",
            Some(&default_id),
            validate_app_id,
        )?);
    }
    if request.template.is_none() && request.example.is_none() {
        request.template = Some(if interactive {
            prompt_template(input, output)?
        } else {
            Template::Starter
        });
    }
    let choose_sdk = request.ui_path.is_none()
        && request.sdk_git.is_none()
        && request.sdk_rev.is_none()
        && request.sdk_version.is_none()
        && repo.is_none();
    if choose_sdk && interactive {
        prompt_sdk(current_dir, &mut request, input, output)?;
    }
    let toolkit = match request.sdk_request() {
        SdkRequest::Git { url, rev } => git_sdk_source(url, rev).describe(),
        SdkRequest::Discover if discover_ui_dir(current_dir, repo.as_deref()).is_none() => {
            git_sdk_source(None, None).describe()
        }
        SdkRequest::Registry(version) => standalone::SdkSource::Registry {
            version: version
                .or_else(|| {
                    standalone::Baseline::embedded()
                        .ok()?
                        .sdk_version()
                        .map(str::to_owned)
                })
                .unwrap_or_default(),
        }
        .describe(),
        SdkRequest::Discover | SdkRequest::Checkout(_) => {
            resolve_ui_dir(current_dir, repo.as_deref(), request.ui_path.as_deref())?
                .display()
                .to_string()
        }
    };
    if full_wizard {
        let destination = new_app_destination(
            current_dir,
            repo.as_deref(),
            request.name.as_deref().unwrap_or_default(),
            request.directory.as_deref(),
        );
        let template = request.template.unwrap_or(Template::Starter);
        writeln!(
            output,
            "\nReview:\n  App:         {}\n  Package:     {}\n  App id:      {}\n  Template:    {} — {}\n  Toolkit:     {}\n  Destination: {}",
            request.title.as_deref().unwrap_or_default(),
            request.name.as_deref().unwrap_or_default(),
            request.app_id.as_deref().unwrap_or_default(),
            template.name(),
            template.description(),
            toolkit,
            destination.display(),
        )
        .context("showing app review")?;
        if !prompt_confirm(input, output, "Create this app", true)? {
            bail!("app creation cancelled");
        }
    }
    create_app_from(current_dir, request)
}

/// Validate, render, and write a new app without prompting.
///
/// # Errors
///
/// Returns an error for invalid input, an occupied destination, or any failed
/// filesystem/workspace-manifest operation.
pub fn create_app(request: NewAppRequest) -> Result<CreateOutcome> {
    let current_dir = std::env::current_dir().context("reading current directory")?;
    create_app_from(&current_dir, request)
}

fn create_app_from(current_dir: &Path, request: NewAppRequest) -> Result<CreateOutcome> {
    let package_name = request
        .name
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("an app name is required"))?;
    validate_package_name(package_name)?;
    let sdk_request = request.sdk_request();

    let repo = find_repo_root(current_dir);
    let destination = new_app_destination(
        current_dir,
        repo.as_deref(),
        package_name,
        request.directory.as_deref(),
    );
    validate_destination(&destination)?;

    let workspace_path = repo
        .as_deref()
        .and_then(|root| destination.strip_prefix(root).ok())
        .filter(|path| path.components().next().is_some())
        .map(Path::to_path_buf);
    let workspace_member = workspace_path.is_some();
    let app_id = request
        .app_id
        .unwrap_or_else(|| default_app_id(package_name, workspace_member));
    validate_app_id(&app_id)?;
    let title = request
        .title
        .unwrap_or_else(|| title_from_name(package_name));
    if title.trim().is_empty() {
        bail!("app title must not be empty");
    }

    let sdk = if workspace_member {
        let ui_dir = resolve_ui_dir(current_dir, repo.as_deref(), request.ui_path.as_deref())?;
        AppSdk::Workspace {
            ui_path: relative_path(&canonical_destination(&destination), &ui_dir),
        }
    } else {
        AppSdk::Standalone(resolve_standalone_sdk(
            current_dir,
            &destination,
            repo.as_deref(),
            &sdk_request,
        )?)
    };
    let sdk_source = match &sdk {
        AppSdk::Workspace { .. } => None,
        AppSdk::Standalone(standalone) => Some(standalone.source.clone()),
    };
    let spec = AppSpec {
        package_name: package_name.to_string(),
        crate_name: package_name.replace('-', "_"),
        title,
        app_id: app_id.clone(),
        sdk,
        template: request.template.unwrap_or(Template::Starter),
    };
    let mut rendered = render(&spec)?;
    if let Some(example) = request.example {
        examples::overlay(
            &mut rendered,
            example,
            &spec.crate_name,
            &spec.app_id,
            &spec.title,
        )?;
    }
    let files = rendered.iter().map(|(path, _)| path.clone()).collect();

    if !request.dry_run {
        let workspace_update = repo
            .as_deref()
            .zip(workspace_path.as_deref())
            .map(|(root, member)| prepare_workspace_member(root, member))
            .transpose()?
            .flatten();
        publish_new_app(&destination, &rendered, workspace_update.as_ref())?;
    }
    let git_initialized =
        !request.dry_run && !workspace_member && init_git_repository(&destination);

    Ok(CreateOutcome {
        destination,
        package_name: spec.package_name,
        app_id,
        files,
        dry_run: request.dry_run,
        workspace_member,
        sdk: sdk_source,
        git_initialized,
    })
}

/// Runs `git init` in a new app that no git work tree contains. Git is a
/// convenience here: without it, or when it fails, the app is still complete.
fn init_git_repository(destination: &Path) -> bool {
    let inside = ProcessCommand::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(destination)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match inside {
        Ok(status) if status.success() => return false,
        Ok(_) => {}
        Err(_) => {
            eprintln!("[tilo-app] git is not installed; the app has no repository");
            return false;
        }
    }
    match ProcessCommand::new("git")
        .args(["init", "--quiet"])
        .current_dir(destination)
        .status()
    {
        Ok(status) if status.success() => true,
        outcome => {
            eprintln!(
                "[tilo-app] git init in {} failed: {outcome:?}",
                destination.display()
            );
            false
        }
    }
}

/// Apps in the Tilo workspace belong to Tilo. Anyone else's app starts in a
/// namespace nobody can publish to until they choose one they have verified.
fn default_app_id(package_name: &str, workspace_member: bool) -> String {
    let prefix = if workspace_member {
        "so.tilo"
    } else {
        "local.dev"
    };
    format!("{prefix}.{}", package_name.replace('-', "."))
}

/// A toolkit named with `--ui-path` or `TILO_UI_PATH`, or a checkout around
/// the app, this CLI or its build, is used in place. Otherwise the app builds
/// against the Tilo repository at the commit this CLI came from.
fn resolve_standalone_sdk(
    current_dir: &Path,
    destination: &Path,
    repo: Option<&Path>,
    request: &SdkRequest,
) -> Result<standalone::Standalone> {
    let checkout_ui = match request {
        SdkRequest::Discover => discover_ui_dir(current_dir, repo)
            .map(|_| resolve_ui_dir(current_dir, repo, None))
            .transpose()?,
        SdkRequest::Checkout(ui) => Some(resolve_ui_dir(current_dir, repo, Some(ui))?),
        SdkRequest::Git { .. } | SdkRequest::Registry(_) => None,
    };
    if let Some(ui_dir) = checkout_ui {
        let root = checkout_root_for_ui(&ui_dir).to_path_buf();
        let relative_ui = relative_path(&canonical_destination(destination), &ui_dir);
        return Ok(standalone::Standalone {
            source: standalone::SdkSource::Checkout(
                checkout_root_for_ui(&relative_ui).to_path_buf(),
            ),
            baseline: standalone::Baseline::from_checkout(&root)?,
        });
    }
    if let SdkRequest::Registry(version) = request {
        let baseline = standalone::Baseline::embedded()?;
        let version = match version {
            Some(version) => version.clone(),
            None => baseline
                .sdk_version()
                .context("this tilo-app does not know its SDK version; pass --sdk-version")?
                .to_owned(),
        };
        return Ok(standalone::Standalone {
            source: standalone::SdkSource::Registry { version },
            baseline,
        });
    }
    let (url, rev) = match request {
        SdkRequest::Git { url, rev } => (url.clone(), rev.clone()),
        SdkRequest::Discover | SdkRequest::Checkout(_) | SdkRequest::Registry(_) => (None, None),
    };
    Ok(standalone::Standalone {
        source: git_sdk_source(url, rev),
        baseline: standalone::Baseline::embedded()?,
    })
}

/// The Tilo repository, at the commit this CLI came from unless named.
fn git_sdk_source(url: Option<String>, rev: Option<String>) -> standalone::SdkSource {
    standalone::SdkSource::Git {
        url: url.unwrap_or_else(|| standalone::DEFAULT_SDK_GIT.to_owned()),
        rev: rev.or_else(|| standalone::BUILT_FROM_REV.map(str::to_owned)),
    }
}

fn new_app_destination(
    current_dir: &Path,
    repo: Option<&Path>,
    package_name: &str,
    directory: Option<&Path>,
) -> PathBuf {
    directory.map_or_else(
        || {
            repo.map_or_else(
                || PathBuf::from(package_name),
                |root| root.join("apps").join(package_name),
            )
        },
        |path| {
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                current_dir.join(path)
            }
        },
    )
}

#[derive(Debug)]
struct WorkspaceUpdate {
    path: PathBuf,
    original: String,
    updated: String,
}

fn prepare_workspace_member(root: &Path, workspace_path: &Path) -> Result<Option<WorkspaceUpdate>> {
    let manifest_path = root.join("Cargo.toml");
    let manifest = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    let member_path = workspace_path.to_string_lossy();
    let member = format!("    {member_path:?},\n");
    if manifest.contains(member.trim()) {
        return Ok(None);
    }
    let Some(insert_at) = workspace_members_insert_at(&manifest) else {
        bail!(
            "{} has no [workspace] members list",
            manifest_path.display()
        );
    };
    let mut updated = String::with_capacity(manifest.len().saturating_add(member.len()));
    updated.push_str(manifest.get(..insert_at).unwrap_or_default());
    updated.push_str(&member);
    updated.push_str(manifest.get(insert_at..).unwrap_or_default());
    Ok(Some(WorkspaceUpdate {
        path: manifest_path,
        original: manifest,
        updated,
    }))
}

fn workspace_members_insert_at(manifest: &str) -> Option<usize> {
    let mut offset = 0_usize;
    let mut in_workspace = false;
    for line in manifest.split_inclusive('\n') {
        let syntax = line.split('#').next().unwrap_or_default().trim();
        if syntax.starts_with('[') {
            in_workspace = syntax == "[workspace]";
        } else if in_workspace
            && syntax
                .split_once('=')
                .is_some_and(|(key, value)| key.trim() == "members" && value.trim() == "[")
        {
            return Some(offset.saturating_add(line.len()));
        }
        offset = offset.saturating_add(line.len());
    }
    None
}

fn publish_new_app(
    destination: &Path,
    rendered: &[(PathBuf, String)],
    workspace: Option<&WorkspaceUpdate>,
) -> Result<()> {
    validate_destination(destination)?;
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .with_context(|| format!("creating app parent {}", parent.display()))?;
    let destination_permissions = std::fs::metadata(destination)
        .ok()
        .map(|metadata| metadata.permissions());
    let staging = tempfile::Builder::new()
        .prefix(".tilo-app-new-")
        .tempdir_in(parent)
        .with_context(|| format!("staging new app beside {}", destination.display()))?;
    for (relative, content) in rendered {
        let path = staging.path().join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating staged directory {}", parent.display()))?;
        }
        let mut file = std::fs::File::create(&path)
            .with_context(|| format!("creating staged app file {}", path.display()))?;
        file.write_all(content.as_bytes())
            .with_context(|| format!("writing staged app file {}", path.display()))?;
        file.sync_all()
            .with_context(|| format!("syncing staged app file {}", path.display()))?;
    }
    if let Some(permissions) = destination_permissions.as_ref() {
        std::fs::set_permissions(staging.path(), permissions.clone()).with_context(|| {
            format!(
                "preserving destination permissions for {}",
                destination.display()
            )
        })?;
    }
    publish_staged_app_with(
        staging.path(),
        destination,
        destination_permissions.as_ref(),
        workspace,
        |staged, destination| {
            std::fs::rename(staged, destination)
                .with_context(|| format!("publishing new app {}", destination.display()))?;
            std::fs::File::open(parent)
                .and_then(|directory| directory.sync_all())
                .with_context(|| format!("syncing app parent {}", parent.display()))
        },
    )
}

fn publish_staged_app_with(
    staged: &Path,
    destination: &Path,
    original_permissions: Option<&std::fs::Permissions>,
    workspace: Option<&WorkspaceUpdate>,
    publish: impl FnOnce(&Path, &Path) -> Result<()>,
) -> Result<()> {
    validate_destination(destination)?;
    if let Some(update) = workspace
        && let Err(error) = write_atomic(
            &update.path,
            update.updated.as_bytes(),
            "workspace manifest",
        )
    {
        let restore = write_atomic(
            &update.path,
            update.original.as_bytes(),
            "workspace rollback",
        );
        return match restore {
            Ok(()) => Err(error).context("registering app in workspace; manifest rolled back"),
            Err(rollback) => bail!("{error:#}; workspace rollback was incomplete: {rollback:#}"),
        };
    }

    let destination_existed = destination.exists();
    let publication = (|| {
        if destination_existed {
            std::fs::remove_dir(destination).with_context(|| {
                format!("preparing empty destination {}", destination.display())
            })?;
        }
        publish(staged, destination)
    })();
    if let Err(error) = publication {
        let mut rollback_failures = Vec::new();
        if !staged.exists()
            && destination.exists()
            && let Err(rollback) = std::fs::remove_dir_all(destination)
                .with_context(|| format!("removing generated app {}", destination.display()))
        {
            rollback_failures.push(rollback.to_string());
        }
        if destination_existed && !destination.exists() {
            let restore = std::fs::create_dir(destination)
                .with_context(|| format!("restoring empty {}", destination.display()))
                .and_then(|()| {
                    original_permissions.map_or(Ok(()), |permissions| {
                        std::fs::set_permissions(destination, permissions.clone()).with_context(
                            || format!("restoring permissions on {}", destination.display()),
                        )
                    })
                });
            if let Err(rollback) = restore {
                rollback_failures.push(rollback.to_string());
            }
        }
        if let Some(update) = workspace
            && let Err(rollback) = write_atomic(
                &update.path,
                update.original.as_bytes(),
                "workspace rollback",
            )
        {
            rollback_failures.push(rollback.to_string());
        }
        if rollback_failures.is_empty() {
            return Err(error).context("publishing new app; all external changes rolled back");
        }
        bail!(
            "{error:#}; new-app rollback was incomplete: {}",
            rollback_failures.join("; ")
        );
    }
    Ok(())
}

fn prompt(
    input: &mut impl io::BufRead,
    output: &mut impl io::Write,
    label: &str,
    default: Option<&str>,
) -> Result<String> {
    match default {
        Some(value) => write!(output, "{label} [{value}]: "),
        None => write!(output, "{label}: "),
    }
    .context("showing prompt")?;
    output.flush().context("showing prompt")?;
    let mut value = String::new();
    if input.read_line(&mut value).context("reading prompt")? == 0 {
        bail!("input ended while reading {label}");
    }
    let value = value.trim();
    if value.is_empty() {
        if let Some(default) = default {
            return Ok(default.to_string());
        }
        bail!("{label} must not be empty");
    }
    Ok(value.to_string())
}

fn prompt_validated(
    input: &mut impl io::BufRead,
    output: &mut impl io::Write,
    label: &str,
    default: Option<&str>,
    validate: impl Fn(&str) -> Result<()>,
) -> Result<String> {
    loop {
        let value = prompt(input, output, label, default)?;
        match validate(&value) {
            Ok(()) => return Ok(value),
            Err(error) => writeln!(output, "  ! {error}").context("showing validation error")?,
        }
    }
}

fn prompt_confirm(
    input: &mut impl io::BufRead,
    output: &mut impl io::Write,
    label: &str,
    default: bool,
) -> Result<bool> {
    let default_label = if default { "Y/n" } else { "y/N" };
    loop {
        let value = prompt(input, output, label, Some(default_label))?;
        match value.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            "y/n" => return Ok(default),
            _ => writeln!(output, "  ! answer `yes` or `no`")
                .context("showing confirmation error")?,
        }
    }
}

fn prompt_optional(
    input: &mut impl io::BufRead,
    output: &mut impl io::Write,
    label: &str,
    default: Option<&str>,
) -> Result<Option<String>> {
    let shown = default.unwrap_or("none");
    write!(output, "{label} [{shown}]: ").context("showing optional prompt")?;
    output.flush().context("showing optional prompt")?;
    let mut value = String::new();
    if input
        .read_line(&mut value)
        .context("reading optional prompt")?
        == 0
    {
        bail!("input ended while reading {label}");
    }
    let value = value.trim();
    if value.is_empty() {
        return Ok(default.map(ToOwned::to_owned));
    }
    if value == "-" || value.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    validate_artifact_name(value)?;
    Ok(Some(value.to_owned()))
}

fn prompt_screen_template(
    input: &mut impl io::BufRead,
    output: &mut impl io::Write,
) -> Result<ScreenTemplate> {
    writeln!(
        output,
        "\nScreen template:\n  1) blank        Title and starter body\n  2) form         Labeled inputs and save action\n  3) settings     Preferences and save action\n  4) empty-state  Explanation and recovery action"
    )
    .context("showing screen template choices")?;
    loop {
        let value = prompt(input, output, "Template", Some("1"))?;
        match value.trim().to_ascii_lowercase().as_str() {
            "1" | "blank" | "b" => return Ok(ScreenTemplate::Blank),
            "2" | "form" | "f" => return Ok(ScreenTemplate::Form),
            "3" | "settings" | "s" => return Ok(ScreenTemplate::Settings),
            "4" | "empty-state" | "empty" | "e" => return Ok(ScreenTemplate::EmptyState),
            _ => writeln!(
                output,
                "  ! unknown screen template {value:?}; choose 1-4 or `blank`, `form`, `settings`, or `empty-state`"
            )
            .context("showing screen template error")?,
        }
    }
}

fn prompt_component_template(
    input: &mut impl io::BufRead,
    output: &mut impl io::Write,
) -> Result<ComponentTemplate> {
    writeln!(
        output,
        "\nComponent template:\n  1) display  Typed props rendered as heading and detail\n  2) form     Typed local state bound to labeled controls\n  3) tree     Any TreeModel with professional virtualized behavior\n  4) list     Any ListModel with sparse variable-height rows\n  5) table    Any TableModel with app-defined columns and cells\n  6) text     Styled text with polygon flow or a custom layout engine"
    )
    .context("showing component template choices")?;
    loop {
        let value = prompt(input, output, "Template", Some("1"))?;
        match value.trim().to_ascii_lowercase().as_str() {
            "1" | "display" | "d" => return Ok(ComponentTemplate::Display),
            "2" | "form" | "f" => return Ok(ComponentTemplate::Form),
            "3" | "tree" | "t" => return Ok(ComponentTemplate::Tree),
            "4" | "list" | "l" => return Ok(ComponentTemplate::List),
            "5" | "table" | "data" => return Ok(ComponentTemplate::Table),
            "6" | "text" | "typography" => return Ok(ComponentTemplate::Text),
            _ => writeln!(
                output,
                "  ! unknown component template {value:?}; choose 1-6 or `display`, `form`, `tree`, `list`, `table`, or `text`"
            )
            .context("showing component template error")?,
        }
    }
}

fn prompt_template(input: &mut impl io::BufRead, output: &mut impl io::Write) -> Result<Template> {
    let mut choices = String::from("\nTemplate:");
    for (number, template) in (1..).zip(Template::ALL) {
        let _ = write!(
            choices,
            "\n  {number}) {:<11}{}",
            template.name(),
            template.description()
        );
    }
    choices.push_str(
        "\n\nTo start from a complete example instead, run `tilo-app examples` and then\n`tilo-app new NAME --from-example EXAMPLE`.",
    );
    writeln!(output, "{choices}").context("showing template choices")?;
    loop {
        let value = prompt(input, output, "Template", Some("1"))?;
        match parse_template(&value) {
            Ok(template) => return Ok(template),
            Err(error) => writeln!(output, "  ! {error}").context("showing template error")?,
        }
    }
}

/// The example that replaced a template earlier versions of `tilo-app` had.
#[must_use]
pub fn retired_template_example(name: &str) -> Option<&'static str> {
    match name {
        "data" => Some("data-table"),
        "list" => Some("virtual-list"),
        "tree" | "workspace" => Some("tree-browser"),
        "async" => Some("background-task"),
        "qr" => Some("qr-code"),
        _ => None,
    }
}

fn parse_template(value: &str) -> Result<Template> {
    let value = value.trim().to_ascii_lowercase();
    if let Ok(index) = value.parse::<usize>()
        && let Some(template) = index
            .checked_sub(1)
            .and_then(|index| Template::ALL.get(index))
    {
        return Ok(*template);
    }
    if let Some(template) = Template::ALL
        .into_iter()
        .find(|template| template.name() == value)
    {
        return Ok(template);
    }
    if let Some(example) = retired_template_example(&value) {
        bail!(
            "the {value} template is now the `{example}` example; run `tilo-app new NAME --from-example {example}`"
        );
    }
    bail!(
        "unknown template {value:?}; choose 1-{} or one of: {}",
        Template::ALL.len(),
        Template::ALL.map(Template::name).join(", ")
    )
}

fn validate_package_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && name.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && !name.ends_with('-')
        && !name.contains("--");
    if !valid {
        bail!(
            "invalid package name {name:?}; use lowercase ASCII letters, digits, and single hyphens"
        );
    }
    Ok(())
}

fn validate_app_id(id: &str) -> Result<()> {
    let parts: Vec<&str> = id.split('.').collect();
    let valid = parts.len() >= 3
        && parts.iter().all(|part| {
            !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        });
    if !valid {
        bail!("invalid app id {id:?}; use at least three dot-separated DNS-style components");
    }
    Ok(())
}

fn validate_destination(path: &Path) -> Result<()> {
    if path
        .components()
        .any(|component| component == std::path::Component::ParentDir)
    {
        bail!("destination must not contain `..`: {}", path.display());
    }
    if !path.exists() {
        return Ok(());
    }
    if !path.is_dir() {
        bail!(
            "destination {} already exists and is not a directory",
            path.display()
        );
    }
    let mut entries = std::fs::read_dir(path)
        .with_context(|| format!("reading destination {}", path.display()))?;
    if entries.next().transpose()?.is_some() {
        bail!("destination {} is not empty", path.display());
    }
    Ok(())
}

fn title_from_name(name: &str) -> String {
    name.split('-')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(chars).collect()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn find_repo_root(start: &Path) -> Option<PathBuf> {
    start.ancestors().find_map(|path| {
        (path.join("crates/sdk/ui-core/Cargo.toml").is_file() && path.join("apps").is_dir())
            .then(|| path.to_path_buf())
    })
}

fn ui_dir_from_ancestor(start: &Path) -> Option<PathBuf> {
    start.ancestors().find_map(|root| {
        let ui = root.join("crates/sdk/ui-core");
        ui.join("Cargo.toml").is_file().then_some(ui)
    })
}

/// Locate the toolkit for a standalone scaffold without requiring users to
/// know the source checkout layout.
fn discover_ui_dir(current_dir: &Path, repo: Option<&Path>) -> Option<PathBuf> {
    if let Some(configured) = std::env::var_os("TILO_UI_PATH") {
        let configured = PathBuf::from(configured);
        let candidate = if configured.is_absolute() {
            configured
        } else {
            current_dir.join(configured)
        };
        return Some(candidate);
    }
    if let Some(root) = repo {
        return Some(root.join("crates/sdk/ui-core"));
    }
    if let Ok(executable) = std::env::current_exe()
        && let Some(ui) = ui_dir_from_ancestor(&executable)
        && !standalone::in_cargo_cache(&ui)
    {
        return Some(ui);
    }
    ui_dir_from_ancestor(Path::new(env!("CARGO_MANIFEST_DIR")))
        .filter(|ui| !standalone::in_cargo_cache(ui))
}

fn resolve_ui_dir(
    current_dir: &Path,
    repo: Option<&Path>,
    requested: Option<&Path>,
) -> Result<PathBuf> {
    let candidate = requested.map_or_else(
        || discover_ui_dir(current_dir, repo),
        |path| {
            Some(if path.is_absolute() {
                path.to_path_buf()
            } else {
                current_dir.join(path)
            })
        },
    );
    let Some(candidate) = candidate else {
        bail!(
            "could not locate the Tilo UI toolkit; run inside a Tilo checkout, set TILO_UI_PATH, or pass `--ui-path /path/to/tilo/crates/sdk/ui-core`"
        );
    };
    // A checkout root also has a Cargo.toml; name its toolkit so the paths
    // derived from it do not treat the root as a crate inside the checkout.
    let toolkit = candidate.join("crates/sdk/ui-core");
    let candidate = if !is_sdk_crate_path(&candidate) && toolkit.join("Cargo.toml").is_file() {
        toolkit
    } else {
        candidate
    };
    if !candidate.join("Cargo.toml").is_file() {
        bail!(
            "UI path {} does not contain Cargo.toml",
            candidate.display()
        );
    }
    candidate
        .canonicalize()
        .with_context(|| format!("resolving UI path {}", candidate.display()))
}

/// `destination` with its deepest existing ancestor canonicalised, so that it
/// shares a prefix with the canonical toolkit path even through a symlink.
fn canonical_destination(destination: &Path) -> PathBuf {
    let absolute = std::path::absolute(destination).unwrap_or_else(|_| destination.to_path_buf());
    absolute
        .ancestors()
        .find_map(|ancestor| {
            let canonical = ancestor.canonicalize().ok()?;
            Some(canonical.join(absolute.strip_prefix(ancestor).ok()?))
        })
        .unwrap_or(absolute)
}

fn relative_path(from_dir: &Path, target: &Path) -> PathBuf {
    let from: Vec<_> = from_dir.components().collect();
    let to: Vec<_> = target.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();
    for _ in common..from.len() {
        out.push("..");
    }
    for component in to.iter().skip(common) {
        out.push(component.as_os_str());
    }
    out
}

fn render(spec: &AppSpec) -> Result<Vec<(PathBuf, String)>> {
    let mut files = vec![
        (PathBuf::from("Cargo.toml"), cargo_toml(spec)?),
        (PathBuf::from("src/lib.rs"), app_lib(spec)),
        (
            PathBuf::from("src/locales.rs"),
            locale_registry_source(
                &["en-US".to_owned()],
                &catalog_message_ids(&parse_fluent_catalog(&app_catalog(spec))?),
            ),
        ),
        (PathBuf::from("src/main.rs"), app_main(spec)),
        (PathBuf::from("tests/app.rs"), app_test(spec)),
        (
            PathBuf::from("tests/headless-actions.json"),
            headless_actions(spec),
        ),
        (PathBuf::from("tilo-app.toml"), app_manifest(spec)),
        (PathBuf::from("README.md"), app_readme(spec)),
        (PathBuf::from("AGENTS.md"), APP_AGENT_GUIDE.to_owned()),
        (PathBuf::from("locales/en-US.ftl"), app_catalog(spec)),
    ];
    if spec.template == Template::Graph {
        files.push((PathBuf::from("ui/main.json"), graph_document()?));
        files.push((
            PathBuf::from("src/screens.rs"),
            screen_registry_source(&["main".into()]),
        ));
        files.push((
            PathBuf::from("src/components/mod.rs"),
            component_registry_source(&[], &BTreeSet::new())?,
        ));
    }
    if let AppSdk::Standalone(standalone) = &spec.sdk {
        files.extend(standalone.files()?);
    }
    for (path, source) in &mut files {
        if path.extension().is_some_and(|extension| extension == "rs") {
            *source = rustfmt_source(source, path)?;
        }
    }
    Ok(files)
}

fn cargo_toml(spec: &AppSpec) -> Result<String> {
    let license = match spec.sdk {
        AppSdk::Workspace { .. } => "license.workspace = true\n",
        AppSdk::Standalone(_) => "license = \"AGPL-3.0-only\"\n",
    };
    // The shared toolkit dylib ships with the OS and is built from a Tilo
    // tree, so an app on the published SDK always links statically.
    let features = if spec.links_toolkit_dylib() {
        "[features]\nthin = [\"dep:runtime-dylib\"]\n\n"
    } else {
        ""
    };
    let mut manifest = format!(
        "[package]\nname = {name:?}\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n{license}\n[lib]\nname = {crate_name:?}\npath = \"src/lib.rs\"\n\n[[bin]]\nname = {name:?}\npath = \"src/main.rs\"\n\n{features}[dependencies]\n",
        name = spec.package_name,
        crate_name = spec.crate_name,
    );
    match &spec.sdk {
        AppSdk::Workspace { ui_path } => {
            let sdk_path = sdk_path_for_ui(ui_path);
            let runtime_path = sdk_path.parent().and_then(Path::parent).map_or_else(
                || PathBuf::from("../../crates/runtime/runtime-dylib"),
                |crates| crates.join("runtime/runtime-dylib"),
            );
            // CI rejects a workspace member that cargo-hakari has not been
            // told about. The hack's feature set is native, so it stays off
            // the web build (`build --platform web`); the SDK's default
            // features select nothing there.
            let _ = write!(
                manifest,
                "runtime-dylib = {{ path = {runtime:?}, optional = true }}\n\
                 ui = {{ package = \"tilo-app\", path = {sdk:?} }}\n\n\
                 [target.'cfg(not(target_arch = \"wasm32\"))'.dependencies]\n\
                 # The workspace-hack's native feature set does not build for wasm32.\n\
                 workspace-hack = {{ version = \"0.1\" }}\n\n\
                 [lints]\nworkspace = true\n",
                runtime = runtime_path.to_string_lossy(),
                sdk = sdk_path.to_string_lossy(),
            );
        }
        AppSdk::Standalone(standalone) => {
            manifest.push_str(&standalone.dependencies());
            if !spec.links_toolkit_dylib() {
                manifest.push_str(
                    "\n# `ui::dev_reload_entry!` expands code for a `thin` build, which this app has not.\n[lints.rust]\nunexpected_cfgs = { level = \"warn\", check-cfg = ['cfg(feature, values(\"thin\"))'] }\n",
                );
            }
            manifest.push_str(&standalone.manifest_tail()?);
        }
    }
    Ok(manifest)
}

fn is_sdk_crate_path(ui_path: &Path) -> bool {
    ui_path.ends_with("crates/sdk/ui-core") || ui_path.ends_with("crates/sdk/tilo-app")
}

fn checkout_root_for_ui(ui_path: &Path) -> &Path {
    if is_sdk_crate_path(ui_path) {
        ui_path
            .ancestors()
            .nth(3)
            .unwrap_or_else(|| Path::new(".."))
    } else {
        ui_path.parent().unwrap_or_else(|| Path::new(".."))
    }
}

fn sdk_path_for_ui(ui_path: &Path) -> PathBuf {
    if is_sdk_crate_path(ui_path) {
        ui_path
            .parent()
            .unwrap_or_else(|| Path::new(".."))
            .join("tilo-app")
    } else {
        checkout_root_for_ui(ui_path).join("crates/sdk/tilo-app")
    }
}

fn app_lib(spec: &AppSpec) -> String {
    let application = match spec.template {
        Template::Starter => STARTER_APP_TEMPLATE.to_owned(),
        Template::Document => DOCUMENT_APP_TEMPLATE.to_owned(),
        Template::Collection => COLLECTION_APP_TEMPLATE.to_owned(),
        Template::Graph => {
            format!("pub mod components;\npub mod screens;\n\n{GRAPH_APP_TEMPLATE}")
        }
    };
    // The templates name their resource `generated.<what>`; an app's kind
    // lives under its own id, so two generated apps never share actions,
    // permissions or search under one kind.
    let application =
        application.replace("kind = \"generated.", &format!("kind = \"{}.", spec.app_id));
    format!(
        "//! {title} application.\n\n/// The application id.\npub const APP_ID: &str = {app_id:?};\n/// The window and launcher title.\npub const APP_TITLE: &str = {title:?};\n\n{GENERATED_LOCALE_SETUP}\n\n{application}\nui::dev_reload_entry!(App);\n",
        title = spec.title,
        app_id = spec.app_id,
    )
}

fn app_main(spec: &AppSpec) -> String {
    let thin = if spec.links_toolkit_dylib() {
        "#[cfg(feature = \"thin\")]\nextern crate runtime_dylib;\n\n"
    } else {
        ""
    };
    format!(
        "//! The app's entry point on every target: the live window and the standard\n//! `--shots`, `--headless-drive` and `--native-audit` modes on Linux and macOS,\n//! registration with the page runtime on the web.\n\n{thin}ui::main!({crate_name}::App);\n",
        crate_name = spec.crate_name,
    )
}

fn app_test(spec: &AppSpec) -> String {
    let tests = match spec.template {
        Template::Starter => STARTER_APP_TESTS.to_owned(),
        Template::Document => DOCUMENT_APP_TESTS.to_owned(),
        Template::Collection => COLLECTION_APP_TESTS.to_owned(),
        Template::Graph => format!("{GRAPH_APP_TESTS}\n{GRAPH_SCREEN_TESTS}"),
    };
    // The test sources name the app crate `generated_app`: the template smoke
    // crate runs these same tests against each template under that name.
    format!("{tests}{NATIVE_APP_TESTS}").replace("generated_app", &spec.crate_name)
}

fn app_manifest(spec: &AppSpec) -> String {
    format!(
        "[package]\nid = {app_id:?}\nname = {title:?}\ncategory = \"Utility\"\nversion = \"0.1.0\"\nkind = \"app\"\nruntime = \"tilo-runtime/2026.07\"\n\n[package.isa]\nx86_64 = \"v3\"\naarch64 = \"aarch64\"\n\n[publisher]\nidentity = \"ed25519:0000000000000000000000000000000000000000000000000000000000000000\"\nsignature = \"\"\n\n[sandbox]\nnamespaces = [\"user\", \"pid\", \"uts\", \"ipc\", \"mount\"]\nseccomp = \"tilo-default\"\nlandlock = \"strict\"\n\n[sandbox.net]\nmode = \"off\"\n\n[sandbox.portals]\n\n[sandbox.services]\ncollab = \"optional@1\"\ntilo-fs = \"optional@1\"\n\n[entrypoint]\nbin = \"bin/{package}\"\ntype = \"gui\"\nsplash-budget = 250\n\n[providers]\nmime = []\next = []\n",
        app_id = spec.app_id,
        title = spec.title,
        package = spec.package_name,
    )
}

fn headless_actions(spec: &AppSpec) -> String {
    match spec.template {
        Template::Starter => String::from(
            "[\n  {\"perform\": {\"target\": \"name-input\", \"action\": \"set_value\", \"value\": \"Ada\"}},\n  {\"perform\": {\"target\": \"name-input\", \"action\": \"focus\"}},\n  {\"expect\": {\"target\": \"name-input\", \"role\": \"text_input\", \"value\": \"Ada\", \"focused\": true}},\n  {\"expect\": {\"target\": \"greeting\", \"role\": \"static_text\", \"name_contains\": \"Ada\"}}\n]\n",
        ),
        Template::Document => String::from(
            "[\n  {\"perform\": {\"target\": \"document-title-input\", \"action\": \"set_value\", \"value\": \"Project Atlas\"}},\n  {\"perform\": {\"target\": \"document-body-input\", \"action\": \"set_value\", \"value\": \"A durable first draft.\"}},\n  {\"expect\": {\"target\": \"document-title-preview\", \"role\": \"heading\", \"name\": \"Project Atlas\"}},\n  {\"expect\": {\"target\": \"document-body-preview\", \"name\": \"A durable first draft.\"}}\n]\n",
        ),
        Template::Collection => String::from(
            "[\n  {\"perform\": {\"target\": \"draft-input\", \"action\": \"set_value\", \"value\": \"Buy milk\"}},\n  {\"expect\": {\"target\": \"draft-input\", \"role\": \"text_input\", \"value\": \"Buy milk\"}},\n  {\"perform\": {\"target\": \"add\", \"action\": \"activate\"}},\n  {\"expect\": {\"target\": \"draft-input\", \"role\": \"text_input\", \"value\": \"\"}}\n]\n",
        ),
        Template::Graph => String::from(
            "[\n  {\"perform\": {\"target\": \"name-input\", \"action\": \"set_value\", \"value\": \"Atlas\"}},\n  {\"expect\": {\"target\": \"name-preview\", \"role\": \"static_text\", \"name\": \"Atlas\"}},\n  {\"perform\": {\"target\": \"create\", \"action\": \"activate\"}}\n]\n",
        ),
    }
}

/// The app's source catalog. A graph screen's static text is a message id
/// too, so the graph template's catalog also names the screen it generates.
fn app_catalog(spec: &AppSpec) -> String {
    let catalog = spec.template.catalog();
    if spec.template != Template::Graph {
        return catalog.to_owned();
    }
    format!(
        "graph-title = {{ \"{}\" }}\ngraph-introduction = This screen is a serialized graph under ui/. Edit it while the app runs with tilo-app ui edit ui/main.json.\n{catalog}",
        escape_fluent_string(&spec.title)
    )
}

fn graph_document() -> Result<String> {
    let document = tilo_ui_graph::UiDocument::new(tilo_ui_graph::UiNode::Column {
        id: "root".into(),
        children: vec![
            tilo_ui_graph::UiNode::Text {
                id: "title".into(),
                text: tilo_ui_graph::UiTextSource::Static {
                    value: "graph-title".into(),
                },
                role: tilo_ui_graph::UiTextRole::Title,
            },
            tilo_ui_graph::UiNode::Text {
                id: "graph-introduction".into(),
                text: tilo_ui_graph::UiTextSource::Static {
                    value: "graph-introduction".into(),
                },
                role: tilo_ui_graph::UiTextRole::Body,
            },
        ],
        gap: Some(8.0),
        no_padding: false,
        intrinsic_height: false,
        fill_remaining_height: false,
    });
    let mut json = document
        .to_json_pretty()
        .context("serializing generated UI graph")?;
    json.push('\n');
    Ok(json)
}

#[allow(
    clippy::too_many_lines,
    reason = "one linear generated README keeps the scaffolded workflow reviewable"
)]
fn app_readme(spec: &AppSpec) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# {}", spec.title);
    let _ = writeln!(out, "\nGenerated by `tilo-app new`.\n");
    let _ = writeln!(
        out,
        "The commands below are the `tilo-app` CLI. It needs rustup and no system packages:\n\n```bash\n{}\n```\n",
        standalone::INSTALL_COMMAND
    );
    let _ = writeln!(
        out,
        "The app is written on the Tilo app framework: its state is a `#[derive(Model)]` resource, every edit is an `#[action]` method, and the view binds controls to those actions (`cx.input`, `cx.checkbox`, `cx.button`, `cx.control`, `cx.each`). Storage, sync, collaboration, history, per-author undo, permissions, the action palette, agents, scripts and search come from the framework; `Replica` tests the resource and `TestApp` tests the running app. `tilo-app doctor` rejects apps that remove this surface.\n"
    );
    let (resource, action, arguments) = readme_action_example(spec.template);
    let resource = format!("{}.{resource}", spec.app_id);
    let _ = writeln!(
        out,
        "Agents, scripts, and search invoke the same address as the visible control. Discover the live catalogue, then submit an invocation shaped like:\n"
    );
    let _ = writeln!(out, "```json");
    let _ = writeln!(
        out,
        "{{\"resource\":{resource:?},\"action\":{action:?},\"arguments\":{arguments}}}"
    );
    let _ = writeln!(out, "```");
    let sections: [(&str, &[&str]); 5] = [
        (
            "Develop: run the app, rebuilding and reloading it as you edit",
            &["tilo-app dev", "tilo-app run", "tilo-app test"],
        ),
        (
            "Verify: `check` runs lints, structure, formatting, Clippy, tests, certification and audited scenarios",
            &[
                "tilo-app check --fix",
                "tilo-app scenario run",
                "tilo-app scenario run --artifacts target/scenario-artifacts",
                "tilo-app scenario run --audit --artifacts target/scenario-artifacts",
                "tilo-app shot",
                "tilo-app inspect --actions tests/headless-actions.json",
                "tilo-app inspect --audit --audit-report target/audit/report.json --actions tests/headless-actions.json",
            ],
        ),
        (
            "Grow: add screens, components, scenarios and translations",
            &[
                "tilo-app add component profile-card --template form",
                "tilo-app add component project-tree --template tree",
                "tilo-app add component activity-list --template list",
                "tilo-app add component results-table --template table",
                "tilo-app add component article-body --template text",
                "tilo-app add screen settings --template settings",
                "tilo-app add scenario empty-state",
                "tilo-app add locale de-DE",
                "tilo-app locale sync",
            ],
        ),
        (
            "Ship: build, sign and verify a package for each platform (`--platform tilo|linux|windows|macos|web`, `--arch x86_64|aarch64`; `build`, `run`, `dev` and `check` take the same flags)",
            &[
                "tilo-app package",
                "tilo-app package --platform web",
                "tilo-app check --platform all",
            ],
        ),
        (
            "Publish: create a publisher key, prove a namespace, and upload what `dist/` holds",
            &[
                "tilo-app keys new",
                "tilo-app apply --domain example.com",
                "tilo-app publish",
                "tilo-app add ci --forge github",
            ],
        ),
    ];
    for (heading, commands) in sections {
        let _ = writeln!(out, "\n{heading}:\n\n```bash");
        for command in commands {
            let _ = writeln!(out, "{command}");
        }
        let _ = writeln!(out, "```");
    }
    let _ = writeln!(
        out,
        "\nEvery command accepts `--json` and then writes one JSON document to stdout, for scripts and agents."
    );
    let _ = writeln!(
        out,
        "\nThe same source runs on Linux, macOS and the web; `--target` picks the platform (each builds on its own host OS, the web on any):\n"
    );
    let _ = writeln!(out, "```bash");
    let _ = writeln!(out, "tilo-app run --target linux");
    let _ = writeln!(out, "tilo-app run --target macos");
    let _ = writeln!(out, "tilo-app run --target web");
    let _ = writeln!(out, "tilo-app build --target macos");
    let _ = writeln!(out, "tilo-app package --target linux");
    let _ = writeln!(out, "tilo-app package --target macos");
    let _ = writeln!(out, "tilo-app package --target web");
    let _ = writeln!(out, "```");
    let _ = writeln!(
        out,
        "\nCargo remains available directly with `cargo test` and `cargo run`."
    );
    let _ = writeln!(
        out,
        "\n`tilo-app add screen account-settings` creates `ui/account-settings.json` and a callable `screens::account_settings(&theme)` constructor."
    );
    if spec.template == Template::Graph {
        let _ = writeln!(
            out,
            "\nOpen the serialized screen with `tilo-app ui edit ui/main.json`. Script or review it with `tilo-app ui tree`, `tilo-app ui set`, `tilo-app ui apply`, `tilo-app ui insert`, `tilo-app ui move`, and `tilo-app ui validate`. Generate, register, and place an app-owned retained component atomically with `tilo-app add component profile-card --mount main --props '{{\"title\":\"Ada\",\"detail\":\"Owner\"}}'`, or mount a document-bound form with `tilo-app add component profile-form --template form --mount main --props '{{\"value_path\":\"profile-name\",\"enabled_path\":\"profile-enabled\"}}'`. The generated component registry makes graph-compatible components available to every generated screen automatically."
        );
    }
    if spec.template == Template::Document {
        let _ = writeln!(
            out,
            "\nThe title and body inputs invoke the document's actions; the preview reads the same fields and follows every edit (local, a collaborator's, an undo) with no notification code. Text fields are collaborative text, so concurrent typing merges character by character. `clear` is declared destructive, so it asks for confirmation wherever it is invoked."
        );
    }
    if spec.template == Template::Collection {
        let _ = writeln!(
            out,
            "\nItems live in a `Collection`, so each has a stable id. Actions that take an item's `&Id` are item actions: the palette and agents see every item as a resource of its own, and a row's context menu offers them. `cx.each` builds one retained row per item, so focus and the caret survive other items changing."
        );
    }
    let _ = writeln!(
        out,
        "\nUser-facing strings start in `locales/en-US.ftl`. `tilo-app add locale de-DE` creates and registers a complete editable copy of that message contract. After changing the source, `tilo-app locale sync` adds missing units without replacing translations; `tilo-app doctor` reports differences deliberately left for a translator while the toolkit falls back to the source catalog."
    );
    let _ = writeln!(out, "\nApp id: `{}`", spec.app_id);
    if spec.app_id.starts_with("local.dev.") {
        let _ = writeln!(
            out,
            "\nThis id is for local development. Before publishing, change it in `tilo-app.toml` and `src/lib.rs` to one inside a namespace you prove with `tilo-app apply`: `example.com` proves `com.example`."
        );
    }
    out
}

/// The template's resource kind under the app's id, one of its actions and
/// arguments for it.
fn readme_action_example(template: Template) -> (&'static str, &'static str, &'static str) {
    match template {
        Template::Starter => ("greeting", "clear", "{}"),
        Template::Document => (
            "document",
            "set-content",
            r#"{"title":"Plan","body":"Ship it"}"#,
        ),
        Template::Collection => ("items", "add", r#"{"title":"Buy milk"}"#),
        Template::Graph => ("graph-project", "create", "{}"),
    }
}

#[cfg(test)]
mod tests;
