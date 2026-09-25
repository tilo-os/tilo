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
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};
use serde::{Deserialize, Serialize};

const STARTER_APP_TEMPLATE: &str = include_str!("../templates/starter.rs");
const DATA_APP_TEMPLATE: &str = include_str!("../templates/data.rs");
const LIST_APP_TEMPLATE: &str = include_str!("../templates/list.rs");
const TREE_APP_TEMPLATE: &str = include_str!("../templates/tree.rs");
const GRAPH_APP_TEMPLATE: &str = include_str!("../templates/graph.rs");
const ASYNC_APP_TEMPLATE: &str = include_str!("../templates/async.rs");
const DOCUMENT_APP_TEMPLATE: &str = include_str!("../templates/document.rs");
const QR_APP_TEMPLATE: &str = include_str!("../templates/qr.rs");
const WORKSPACE_APP_TEMPLATE: &str = include_str!("../templates/workspace.rs");
/// The graph template as generated before apps started on the framework.
/// Kept only to recognise, and migrate, apps generated from it.
const LEGACY_GRAPH_APP_TEMPLATE: &str = include_str!("../templates/legacy/graph.rs");
const STARTER_APP_TESTS: &str = include_str!("../templates/tests/starter.rs");
const DATA_APP_TESTS: &str = include_str!("../templates/tests/data.rs");
const LIST_APP_TESTS: &str = include_str!("../templates/tests/list.rs");
const TREE_APP_TESTS: &str = include_str!("../templates/tests/tree.rs");
const GRAPH_APP_TESTS: &str = include_str!("../templates/tests/graph.rs");
const GRAPH_SCREEN_TESTS: &str = include_str!("../templates/tests/graph_screen.rs");
const ASYNC_APP_TESTS: &str = include_str!("../templates/tests/async.rs");
const DOCUMENT_APP_TESTS: &str = include_str!("../templates/tests/document.rs");
const QR_APP_TESTS: &str = include_str!("../templates/tests/qr.rs");
const WORKSPACE_APP_TESTS: &str = include_str!("../templates/tests/workspace.rs");
const NATIVE_APP_TESTS: &str = include_str!("../templates/tests/native.rs");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Template {
    Starter,
    Data,
    List,
    Tree,
    Graph,
    Async,
    Document,
    Qr,
    Workspace,
}

impl Template {
    fn name(self) -> &'static str {
        match self {
            Self::Starter => "starter",
            Self::Data => "data",
            Self::List => "list",
            Self::Tree => "tree",
            Self::Graph => "graph",
            Self::Async => "async",
            Self::Document => "document",
            Self::Qr => "qr",
            Self::Workspace => "workspace",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Starter => "Profile resource edited through bound form controls",
            Self::Data => "Sparse table with retained cell controls",
            Self::List => "Sparse variable-height retained list",
            Self::Tree => "Professional hierarchy browser",
            Self::Graph => "Serialized UI graph and router",
            Self::Async => "Restartable background task with cancellation",
            Self::Document => "Durable synchronized editor with undo",
            Self::Qr => "IME input to accessible generated QR output",
            Self::Workspace => "Responsive master-detail workspace with durable item editing",
        }
    }
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
    FormV1,
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
            Self::FormV1 => {
                if !props.get("value").is_some_and(serde_json::Value::is_string) {
                    Some("requires string prop \"value\"".to_owned())
                } else if !props
                    .get("enabled")
                    .is_some_and(serde_json::Value::is_boolean)
                {
                    Some("requires boolean prop \"enabled\"".to_owned())
                } else {
                    None
                }
            }
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
    pub directory: Option<PathBuf>,
    /// Explicit toolkit crate. `None` discovers `TILO_UI_PATH`, a surrounding
    /// checkout, the running executable's checkout, or the build checkout.
    pub ui_path: Option<PathBuf>,
    pub dry_run: bool,
}

#[derive(Debug)]
pub struct CreateOutcome {
    pub destination: PathBuf,
    pub package_name: String,
    pub app_id: String,
    pub files: Vec<PathBuf>,
    pub dry_run: bool,
    pub workspace_member: bool,
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
    cases: Vec<ScenarioCasePlan>,
    artifact_dir: Option<PathBuf>,
}

#[derive(Debug)]
struct ScenarioCasePlan {
    name: String,
    source: PathBuf,
    cargo: CargoPlan,
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
}

#[derive(Debug)]
pub struct PackageRequest {
    pub output: Option<PathBuf>,
    pub identity: Option<PathBuf>,
    pub profile: String,
    pub target: Option<String>,
    pub features: Vec<String>,
    pub no_default_features: bool,
    pub force: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct PackageOutcome {
    pub output: PathBuf,
    pub entrypoints: Vec<PathBuf>,
}

/// One validated edit to a serialized UI object graph.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case", deny_unknown_fields)]
pub enum UiGraphEdit {
    Insert {
        parent: String,
        index: usize,
        node: tilo_ui_graph::UiNode,
    },
    Remove {
        id: String,
    },
    Replace {
        id: String,
        node: tilo_ui_graph::UiNode,
    },
    Move {
        id: String,
        parent: String,
        index: usize,
    },
    /// Set one serialized node property. A dotted path addresses nested
    /// objects such as `text.value` or `navigate.route`.
    SetProperty {
        id: String,
        property: String,
        value: serde_json::Value,
    },
    /// Remove one serialized node property so its schema default applies.
    UnsetProperty {
        id: String,
        property: String,
    },
}

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
                size: None,
            },
            tilo_ui_graph::UiNode::Text {
                id: "body".into(),
                text: tilo_ui_graph::UiTextSource::Static {
                    value: "Start building here.".into(),
                },
                role: tilo_ui_graph::UiTextRole::Body,
                size: None,
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
const LEGACY_GRAPH_ROUTER_CALL: &str = "crate::screens::router(\"main\", &Theme::default())";
const GENERATED_GRAPH_ROUTER_CALL: &str = "crate::screens::router_with(\n        \"main\",\n        &Theme::default(),\n        crate::components::ui_graph_context(),\n    )";

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
const LEGACY_GENERATED_LOCALE_SETUP: &str = r#"const SOURCE_CATALOG: &str = include_str!("../locales/en-US.ftl");

fn install_catalog() {
    if let Err(error) = ui::i18n::I18nManager::install_system(
        "en-US",
        &[("en-US", SOURCE_CATALOG)],
    ) {
        eprintln!("localization catalog: {error}");
    }
}"#;

fn locale_artifact_writes(app_dir: &Path, locale: &str, force: bool) -> Result<Vec<PendingWrite>> {
    validate_locale(locale)?;
    if locale.eq_ignore_ascii_case("en-US") {
        bail!("en-US is the source catalog; edit locales/en-US.ftl directly");
    }
    let relative = PathBuf::from("locales").join(format!("{locale}.ftl"));
    let path = app_dir.join(relative);
    protect_destination(&path, force)?;

    let registry_path = app_dir.join("src/locales.rs");
    let (registry, mut locales, migrated_library) = match std::fs::read_to_string(&registry_path) {
        Ok(registry) => {
            let locales = parse_locale_registry(&registry).with_context(|| {
                format!(
                    "{} is app-owned; register {locale:?} in that localization setup explicitly",
                    registry_path.display()
                )
            })?;
            (registry, locales, None)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let library = migrate_legacy_locale_setup(app_dir, locale)?;
            (String::new(), vec!["en-US".to_owned()], Some(library))
        }
        Err(error) => {
            return Err(error).with_context(|| format!("reading {}", registry_path.display()));
        }
    };
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
    let registry_source = rustfmt_source(&locale_registry_source(&locales), &registry_path)?;
    let source_path = app_dir.join("locales/en-US.ftl");
    let source_catalog = std::fs::read_to_string(&source_path)
        .with_context(|| format!("reading source catalog {}", source_path.display()))?;
    parse_fluent_catalog(&source_catalog)
        .with_context(|| format!("source catalog {} is invalid", source_path.display()))?;

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
    if let Some(library) = migrated_library {
        writes.push(library);
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

fn migrate_legacy_locale_setup(app_dir: &Path, locale: &str) -> Result<PendingWrite> {
    let path = app_dir.join("src/lib.rs");
    let source = std::fs::read_to_string(&path)
        .with_context(|| format!("reading localization entrypoint {}", path.display()))?;
    if source.matches(LEGACY_GENERATED_LOCALE_SETUP).count() != 1 {
        bail!(
            "{} has app-owned localization setup; register {locale:?} explicitly or add a tilo-app generated src/locales.rs registry",
            path.display()
        );
    }
    let updated = source.replacen(LEGACY_GENERATED_LOCALE_SETUP, GENERATED_LOCALE_SETUP, 1);
    syn::parse_file(&updated)
        .with_context(|| format!("validating migrated locale setup in {}", path.display()))?;
    Ok(PendingWrite {
        path,
        source: updated,
        description: "locale module wiring",
    })
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

fn locale_registry_source(locales: &[String]) -> String {
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
                style: tilo_ui_graph::UiButtonStyle::Ghost,
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
        style: tilo_ui_graph::UiButtonStyle::Primary,
    }
}

fn screen_text(id: &str, value: String, role: tilo_ui_graph::UiTextRole) -> tilo_ui_graph::UiNode {
    tilo_ui_graph::UiNode::Text {
        id: id.into(),
        text: tilo_ui_graph::UiTextSource::Static { value },
        role,
        size: None,
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
            style: tilo_ui_graph::UiButtonStyle::Neutral,
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
    let mut updated_library = match component_modules.as_slice() {
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
    if is_generated_graph_app(app_dir)? {
        updated_library = migrate_generated_graph_context(&updated_library);
    }
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
        "\nuse ui::UiBuildContext;\n\n/// Build the app-owned component factory registry for serialized screens.\n#[must_use]\npub fn ui_graph_context() -> UiBuildContext {\n    let context = UiBuildContext::default();\n",
    );
    for name in graph_names {
        let (module_name, _) = component_rust_names(name)?;
        let _ = writeln!(
            source,
            "    let context = {module_name}::register_ui_graph(context);"
        );
    }
    source.push_str("    context\n}\n");
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

fn migrate_generated_graph_context(source: &str) -> String {
    let legacy_template = legacy_graph_app_template();
    if source.contains(&legacy_template) {
        source.replacen(&legacy_template, LEGACY_GRAPH_APP_TEMPLATE, 1)
    } else {
        source.to_owned()
    }
}

/// The widget-root graph template from before `screens::router_with`: apps
/// generated from it get the component context wired on their next
/// component, in the widget-root form they already use.
fn legacy_graph_app_template() -> String {
    LEGACY_GRAPH_APP_TEMPLATE.replace(GENERATED_GRAPH_ROUTER_CALL, LEGACY_GRAPH_ROUTER_CALL)
}

fn generated_graph_context_ready(app_dir: &Path) -> Result<bool> {
    if !is_generated_graph_app(app_dir)? {
        return Ok(false);
    }
    let path = app_dir.join("src/lib.rs");
    let source =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    Ok(source.contains(GENERATED_GRAPH_ROUTER_CALL)
        || source.contains(&legacy_graph_app_template()))
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

/// Build one `{name}` form whose controls bind directly to the app document.
///
/// Reusing either path in another component or ordinary graph control creates
/// another synchronized view of the same persistent, undoable value.
pub fn {module_name}_from_document(
    id: &str,
    value_path: &str,
    enabled_path: &str,
) -> ui::widget::Column {{
    let value_id = format!("{{id}}-value");
    let enabled_id = format!("{{id}}-enabled");
    let value = TextInput::from_document_string(value_path)
        .with_id(&value_id)
        .with_placeholder("{name}-value-placeholder");
    let enabled = Checkbox::new(&enabled_id, enabled_path).with_id(&enabled_id);
    ui::column![
        labeled_field("{name}-value-label", value),
        labeled_check("{name}-enabled-label", enabled),
    ]
    .with_no_padding()
}}

/// Build this document-bound component from a serialized UI graph node.
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
    Ok(Box::new({module_name}_from_document(
        request.id,
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

    #[test]
    fn graph_factory_binds_shared_document_state() {{
        let document = ui::UiDocument::from_json(
            r#"{{"version":1,"root":{{"type":"column","id":"root","children":[{{"type":"component","id":"first","name":"{name}","props":{{"value_path":"shared-value","enabled_path":"shared-enabled"}}}},{{"type":"component","id":"second","name":"{name}","props":{{"value_path":"shared-value","enabled_path":"shared-enabled"}}}}]}}}}"#,
        )
        .expect("valid component graph");
        let context = register_ui_graph(UiBuildContext::default());
        let widget = ui::UiDocumentExt::build_with(
            &document,
            &ui::Theme::default(),
            &context,
        )
        .expect("registered form component");
        let mut app = ui::headless::Driver::new(ui::State::new(), widget, (480, 240));
        app.perform_named("first-value", &Action::SetValue("Ada".into()))
            .expect("first graph form edits shared text");
        app.expect(
            &SemanticExpectation::named("second-value")
                .role(Role::TextInput)
                .value("Ada"),
        )
        .expect("second graph form observes shared document text");
        app.perform_named("first-enabled", &Action::ToggleCheck)
            .expect("first graph form edits shared boolean");
        app.expect(
            &SemanticExpectation::named("second-enabled")
                .role(Role::Checkbox)
                .checked(true),
        )
        .expect("second graph form observes shared document boolean");
        assert!(app.perform_undo(), "document-bound toggle is undoable");
        app.expect(
            &SemanticExpectation::named("second-enabled")
                .role(Role::Checkbox)
                .checked(false),
        )
        .expect("undo updates every bound view");
        assert!(app.perform_redo(), "document-bound toggle is redoable");
        let state = app.dump_json();
        assert!(state.contains("shared-value"));
        assert!(state.contains("Ada"));
        assert!(state.contains("shared-enabled"));

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
        let tree = {module_name}("example-tree", model, &config)
            .with_row_delegate(|model, id, _depth| {{
                let label = model.label(id).unwrap_or_default();
                Box::new(ui::row![
                    Text::new(label),
                    Button::new(
                        &format!("more-{{id}}"),
                        "More",
                        Some("row-action-requested"),
                    )
                    .with_click_value(id.to_string()),
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
        assert!(app.dump_json().contains("row-action-requested"));
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
        let list = {module_name}(
            "example-list",
            SparseItems {{ count: ITEM_COUNT }},
            &config,
            |_model, index, key, _state| {{
                let height = if index % 5 == 0 {{ 72.0 }} else {{ 44.0 }};
                Box::new(Container::new().with_height(height).with_child(Box::new(
                    ui::row![
                        Text::new(&format!("Item {{index}}")),
                        Button::new(
                            &format!("open-{{key}}"),
                            "Open",
                            Some("open-requested"),
                        )
                        .with_click_value(key.to_string()),
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
        assert!(app.dump_json().contains("open-requested"));
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
    use ui::widget::Button;
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
        let columns = vec![
            ColumnDef::<Row>::new("name", "Name", ColumnWidth::Flex(1.0))
                .with_text(|row| row.name.clone()),
            ColumnDef::<Row>::new("actions", "Actions", ColumnWidth::Fixed(112.0))
                .with_widget(|_row, index| {{
                    Box::new(
                        Button::new(
                            &format!("open-{{index}}"),
                            "Open",
                            Some("open-requested"),
                        )
                        .with_click_value(index.to_string())
                        .fit_content(if index == 0 {{ 56.0 }} else {{ 28.0 }}),
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
        assert!(app.dump_json().contains("open-requested"));
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
use ui::{{PolygonTextLayout, TextFlowRegion, TextLayoutEngine, TextStyle}};

#[derive(Clone, Debug, PartialEq)]
pub struct {type_name}Props {{
    pub message: String,
    pub style: TextStyle,
}}

impl {type_name}Props {{
    #[must_use]
    pub fn new(message: impl Into<String>, style: TextStyle) -> Self {{
        Self {{ message: message.into(), style }}
    }}
}}

impl Default for {type_name}Props {{
    fn default() -> Self {{
        Self::new(
            "{name}-body",
            TextStyle::new(18.0)
                .with_line_height(1.5)
                .with_kerning(true)
                .with_letter_spacing(0.15)
                .with_auto_hyphenation("en-US"),
        )
    }}
}}

/// Build ordinary reusable `{name}` text with one style shared by layout and paint.
pub fn {module_name}(id: &str, props: &{type_name}Props) -> Text {{
    Text::new(&props.message)
        .with_id(id)
        .with_style(props.style.clone())
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
    use ui::{{TextLayoutConstraints, TextRenderer}};

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
        let props = {type_name}Props::new(
            "Advanced typography",
            TextStyle::new(18.0).with_kerning(true),
        );
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
        let props = {type_name}Props::new("Polygon text", TextStyle::new(18.0));
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
            "\n\n/// Build the `{name}` screen.\n///\n/// # Errors\n///\n/// Returns a graph parse, validation, component, or construction error.\npub fn {function}(theme: &Theme) -> Result<Box<dyn Widget>, UiGraphError> {{\n    {function}_with(theme, UiBuildContext::default())\n}}\n\n/// Build `{name}` with app-owned component factories and other services.\n///\n/// # Errors\n///\n/// Returns a graph parse, validation, component, or construction error.\npub fn {function}_with(\n    theme: &Theme,\n    context: UiBuildContext,\n) -> Result<Box<dyn Widget>, UiGraphError> {{\n    build_with(include_str!(\"../ui/{name}.json\"), theme, context)\n}}"
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
        "        _ => return None,\n    };\n    let context = context.clone().with_router(navigation.clone());\n    Some(build_with(source, theme, context))\n}\n\n/// Build a history-aware host for all registered screens.\n///\n/// # Errors\n///\n/// Returns an error when the initial route or a screen is invalid.\npub fn router(initial: &str, theme: &Theme) -> Result<Router, RouteError> {\n    router_with(initial, theme, UiBuildContext::default())\n}\n\n/// Build the screen router with app-owned component factories and services.\n///\n/// # Errors\n///\n/// Returns an error when the initial route or a screen is invalid.\npub fn router_with(\n    initial: &str,\n    theme: &Theme,\n    context: UiBuildContext,\n) -> Result<Router, RouteError> {\n    Router::new(initial, NAMES.iter().copied(), theme, move |route, theme, navigation| {\n        get_routed(route, theme, navigation, &context)\n            .ok_or_else(|| format!(\"screen {route:?} is not registered\"))?\n            .map_err(|error| error.to_string())\n    })\n}\n\nfn build_with(\n    source: &str,\n    theme: &Theme,\n    context: UiBuildContext,\n) -> Result<Box<dyn Widget>, UiGraphError> {\n    let graph = UiGraphHandle::from_json(source)?;\n    Ok(Box::new(graph.widget_with(theme, context)?))\n}\n",
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
    let edits: Vec<UiGraphEdit> =
        serde_json::from_str(source).context("parsing UI graph edit batch")?;
    if edits.is_empty() {
        bail!("UI graph edit batch must contain at least one edit");
    }
    Ok(edits)
}

/// Apply a non-empty edit batch and publish the graph exactly once after every
/// operation and the resulting complete graph validate.
///
/// # Errors
///
/// Returns a load, edit, validation, or atomic-write error. The source file
/// remains byte-for-byte unchanged when any operation fails.
pub fn edit_ui_graph_batch(path: &Path, edits: Vec<UiGraphEdit>) -> Result<()> {
    if edits.is_empty() {
        bail!("UI graph edit batch must contain at least one edit");
    }
    let mut document = load_ui_graph(path)?;
    for (index, edit) in edits.into_iter().enumerate() {
        apply_ui_graph_edit(&mut document, edit)
            .with_context(|| format!("applying UI graph edit {}", index.saturating_add(1)))?;
    }
    write_ui_graph(path, &document)
}

fn apply_ui_graph_edit(document: &mut tilo_ui_graph::UiDocument, edit: UiGraphEdit) -> Result<()> {
    match edit {
        UiGraphEdit::Insert {
            parent,
            index,
            node,
        } => document.insert_child(&parent, index, node)?,
        UiGraphEdit::Remove { id } => {
            let _ = document.remove_node(&id)?;
        }
        UiGraphEdit::Replace { id, node } => document.replace_node(&id, node)?,
        UiGraphEdit::Move { id, parent, index } => {
            document.move_node(&id, &parent, index)?;
        }
        UiGraphEdit::SetProperty {
            id,
            property,
            value,
        } => {
            set_ui_node_property(document, &id, &property, Some(value))?;
        }
        UiGraphEdit::UnsetProperty { id, property } => {
            set_ui_node_property(document, &id, &property, None)?;
        }
    }
    Ok(())
}

fn set_ui_node_property(
    document: &mut tilo_ui_graph::UiDocument,
    id: &str,
    property: &str,
    value: Option<serde_json::Value>,
) -> Result<()> {
    let node = document
        .node(id)
        .cloned()
        .with_context(|| format!("UI graph has no node {id:?}"))?;
    let segments: Vec<_> = property.split('.').collect();
    if segments.iter().any(|segment| segment.is_empty()) {
        bail!("property path must contain non-empty dot-separated names");
    }
    let mut serialized = serde_json::to_value(node).context("serializing UI node")?;
    let Some((leaf, parents)) = segments.split_last() else {
        bail!("property path must contain a name");
    };
    let mut current = &mut serialized;
    for segment in parents {
        current = current
            .as_object_mut()
            .and_then(|object| object.get_mut(*segment))
            .with_context(|| format!("node {id:?} has no object property path {property:?}"))?;
    }
    let object = current
        .as_object_mut()
        .with_context(|| format!("node {id:?} property parent is not an object"))?;
    if let Some(value) = value {
        object.insert((*leaf).to_owned(), value);
    } else if object.remove(*leaf).is_none() {
        bail!("node {id:?} has no property {property:?}");
    }
    let replacement: tilo_ui_graph::UiNode = serde_json::from_value(serialized)
        .with_context(|| format!("property {property:?} is not valid for node {id:?}"))?;
    document
        .replace_node(id, replacement)
        .with_context(|| format!("validating property {property:?} on node {id:?}"))
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
        atomic_file::write_with_mode(path, source, mode)
    };
    #[cfg(not(unix))]
    let written = atomic_file::write(path, source);
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
    doctor_app_with_capnp_probe(start, probe_capnp_compiler)
}

fn doctor_app_with_capnp_probe(
    start: &Path,
    probe_capnp: impl FnOnce() -> std::result::Result<String, String>,
) -> Result<DoctorReport> {
    let app_dir = resolve_app_dir(start)?;
    Ok(doctor_resolved_app_with_capnp_probe(app_dir, probe_capnp))
}

fn doctor_resolved_app(app_dir: PathBuf) -> DoctorReport {
    #[cfg(not(test))]
    let probe = probe_capnp_compiler;
    #[cfg(test)]
    let probe = || Ok("Cap'n Proto version 1.0.2".to_owned());
    doctor_resolved_app_with_capnp_probe(app_dir, probe)
}

fn doctor_resolved_app_with_capnp_probe(
    app_dir: PathBuf,
    probe_capnp: impl FnOnce() -> std::result::Result<String, String>,
) -> DoctorReport {
    let mut checks = Vec::new();
    let package_name = check_cargo_manifest(&app_dir, &mut checks);
    let package_id = check_package_manifest(&app_dir, &mut checks);
    check_capnp_compiler(&mut checks, probe_capnp);

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
        &["framework::run::<", "LaunchSpec::new"],
        &mut checks,
    );
    check_resource_action_surface(&app_dir, &mut checks);
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

fn probe_capnp_compiler() -> std::result::Result<String, String> {
    let output = ProcessCommand::new("capnp")
        .arg("--version")
        .output()
        .map_err(|error| format!("could not run `capnp --version`: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let error_detail = if stderr.trim().is_empty() {
            String::new()
        } else {
            format!(": {}", stderr.trim())
        };
        return Err(format!(
            "`capnp --version` exited with {}{}",
            output.status, error_detail
        ));
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    Ok(if version.is_empty() {
        "`capnp --version` succeeded".to_owned()
    } else {
        version
    })
}

fn check_capnp_compiler(
    checks: &mut Vec<DoctorCheck>,
    probe_capnp: impl FnOnce() -> std::result::Result<String, String>,
) {
    let (status, detail) = match probe_capnp() {
        Ok(version) => (DoctorStatus::Pass, version),
        Err(error) => (
            DoctorStatus::Fail,
            format!(
                "{error}; install the Cap'n Proto compiler (`capnp`) or run tilo-app inside the Tilo development container"
            ),
        ),
    };
    checks.push(DoctorCheck {
        status,
        label: "Cap'n Proto compiler",
        detail,
    });
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
        || code.contains("ui::widget::resource_actions::background(")
        || code.contains("ui::widget::resource_actions::form(")
        || code.contains("resource_actions::immediate(")
        || code.contains("resource_actions::background(")
        || code.contains("resource_actions::form(");
    let toolkit_descriptor = code.contains("ui::widget::resource_actions::resource(")
        || code.contains("ui::widget::resource_actions::typed_resource(")
        || code.contains("resource_actions::resource(")
        || code.contains("resource_actions::typed_resource(")
        || code.contains("ActionForm::new(")
        || code.contains("ui::widget::resource_actions::ActionForm::new(");
    let custom_catalogue = code.contains("fn resources(") && code.contains("ActionSpec::");
    let custom_invocation =
        code.contains("invoke_resource_operation") || code.contains("start_resource_operation");
    let custom_operation =
        code.contains("OperationKind::Effect") || code.contains("OperationKind::View");
    let toolkit_surface = toolkit_host && toolkit_descriptor;
    let custom_surface = custom_catalogue && (!custom_operation || custom_invocation);
    // `#[resource]` turns `#[action]` methods into the published, replayable
    // catalogue, and `ResourceApp` hosts it; a hand-written `impl Resource`
    // (a custom storage layout) is the same contract.
    let framework_surface = (code.contains("#[resource(") && code.contains("#[action"))
        || code.contains("impl Resource for");
    let framework_host = code.contains("impl ResourceApp for");
    let framework = framework_surface && framework_host;
    let (status, detail) = if framework || toolkit_surface || custom_surface {
        (
            DoctorStatus::Pass,
            if framework {
                "the app framework publishes the resource's typed actions"
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

fn resolve_app_dir(start: &Path) -> Result<PathBuf> {
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
        let app_args = [
            OsString::from("--headless-drive"),
            OsString::from(serde_json::Value::Object(payload).to_string()),
        ];
        cases.push(ScenarioCasePlan {
            name,
            source,
            cargo: plan_app_task(&app_dir, AppTask::Run, &app_args)?,
        });
    }
    Ok(ScenarioSuitePlan {
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

    /// Run every prevalidated case in deterministic order. Each case receives
    /// private XDG directories and cannot read or overwrite normal app state.
    /// Failures are accumulated so one run reports the complete suite.
    ///
    /// # Errors
    ///
    /// Returns an error only when an isolated state directory or child process
    /// cannot be created. Non-zero scenario exits are returned in `failed`.
    pub fn execute(&self) -> Result<ScenarioSuiteOutcome> {
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
            let status = ProcessCommand::new(&case.cargo.program)
                .args(&case.cargo.args)
                .current_dir(&case.cargo.app_dir)
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
    let app_dir = resolve_app_dir(start)?;
    let report = doctor_resolved_app(app_dir.clone());
    if !report.is_ready() {
        bail!("app preflight failed\n\n{}", report.summary());
    }
    validate_profile_name(&request.profile)?;
    let spec = load_app_package_spec(app_dir)?;
    let output = request.output.as_ref().map_or_else(
        || {
            spec.app_dir
                .join("dist")
                .join(format!("{}.tapp", spec.cargo_package))
        },
        |path| absolute_from(&spec.app_dir, path),
    );
    if output.exists() && !request.force {
        bail!(
            "package output {} already exists; pass --force to replace it",
            output.display()
        );
    }
    let output_parent = output
        .parent()
        .ok_or_else(|| anyhow::anyhow!("package output has no parent: {}", output.display()))?;
    std::fs::create_dir_all(output_parent).with_context(|| {
        format!(
            "creating package output directory {}",
            output_parent.display()
        )
    })?;

    let built = build_entrypoints(&spec, request)?;
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
    std::fs::create_dir(&payload).context("creating staged payload")?;
    let app_payload = spec.app_dir.join("payload");
    if app_payload.is_dir() {
        copy_payload_tree(&app_payload, &payload)?;
    }
    for (cargo_bin, payload_path) in &spec.entrypoints {
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

    let staged_package = staging.path().join("package.tapp");
    let identity = request
        .identity
        .as_ref()
        .map(|path| absolute_from(&spec.app_dir, path));
    run_tilo_pkg_pack(&spec, &payload, &staged_package, identity.as_deref())?;
    run_tilo_pkg_verify(&spec.app_dir, &staged_package)?;
    std::fs::rename(&staged_package, &output)
        .with_context(|| format!("publishing package to {}", output.display()))?;
    eprintln!("[tilo-app] packaged {}", output.display());
    Ok(PackageOutcome {
        output,
        entrypoints: spec.entrypoints.into_iter().map(|(_, path)| path).collect(),
    })
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

    let manifest_path = app_dir.join("tilo-app.toml");
    let manifest_source = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    let manifest = tilo_app_package::Manifest::from_toml_str(&manifest_source)
        .with_context(|| format!("parsing {}", manifest_path.display()))?;
    let entrypoint = manifest
        .entrypoint
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("tilo-app.toml declares no package entrypoint"))?;
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
    })
}

fn build_entrypoints(
    spec: &AppPackageSpec,
    request: &PackageRequest,
) -> Result<HashMap<String, PathBuf>> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let mut command = ProcessCommand::new(&cargo);
    command
        .arg("build")
        .arg("--manifest-path")
        .arg(spec.app_dir.join("Cargo.toml"))
        .arg("--profile")
        .arg(&request.profile)
        .arg("--message-format=json-render-diagnostics")
        .current_dir(&spec.app_dir)
        .stdout(Stdio::piped());
    for (cargo_bin, _) in &spec.entrypoints {
        command.arg("--bin").arg(cargo_bin);
    }
    if let Some(target) = &request.target {
        command.arg("--target").arg(target);
    }
    if request.no_default_features {
        command.arg("--no-default-features");
    }
    if !request.features.is_empty() {
        command.arg("--features").arg(request.features.join(","));
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
    }
    let status = child.wait().context("waiting for Cargo build")?;
    if !status.success() {
        bail!("Cargo package build failed with {status}");
    }
    for (cargo_bin, _) in &spec.entrypoints {
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
    let Some(executable) = message
        .get("executable")
        .and_then(serde_json::Value::as_str)
    else {
        return;
    };
    artifacts.insert(name.to_owned(), PathBuf::from(executable));
}

fn run_tilo_pkg_pack(
    spec: &AppPackageSpec,
    payload: &Path,
    output: &Path,
    identity: Option<&Path>,
) -> Result<()> {
    let program =
        std::env::var_os("TILO_PKG").unwrap_or_else(|| OsString::from("tilo-app-package"));
    let mut command = ProcessCommand::new(&program);
    command
        .arg("pack")
        .arg("--manifest")
        .arg(&spec.manifest_path)
        .arg("--payload")
        .arg(payload)
        .arg("--output")
        .arg(output)
        .current_dir(&spec.app_dir);
    if let Some(identity) = identity {
        command.arg("--identity").arg(identity);
    } else {
        eprintln!(
            "[tilo-app] no --identity supplied; tilo-app-package will use an ephemeral self-signing key"
        );
    }
    run_package_command(&mut command, &program, "pack")
}

fn run_tilo_pkg_verify(app_dir: &Path, package: &Path) -> Result<()> {
    let program =
        std::env::var_os("TILO_PKG").unwrap_or_else(|| OsString::from("tilo-app-package"));
    let mut command = ProcessCommand::new(&program);
    command.arg("verify").arg(package).current_dir(app_dir);
    run_package_command(&mut command, &program, "verify")
}

fn run_package_command(
    command: &mut ProcessCommand,
    program: &OsString,
    operation: &str,
) -> Result<()> {
    let status = command.status().with_context(|| {
        format!(
            "starting {} {operation}; install/build tilo-app-package or set TILO_PKG",
            program.to_string_lossy()
        )
    })?;
    if !status.success() {
        bail!(
            "{} {operation} failed with {status}",
            program.to_string_lossy()
        );
    }
    Ok(())
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
/// remains alive; only a successful candidate replaces its process group.
/// A failed build or structural preflight leaves both it and the watcher alive
/// for the next edit.
///
/// # Errors
///
/// Returns an error when the app cannot be resolved, its initial preflight
/// fails, signal handlers cannot be installed, or filesystem watching cannot
/// be started.
pub fn dev_app(start: &Path, request: &DevRequest) -> Result<()> {
    let app_dir = resolve_app_dir(start)?;
    let initial = plan_app_task(&app_dir, AppTask::Run, &request.forwarded)?;
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
    eprintln!(
        "[tilo-app] watching {}",
        roots
            .iter()
            .map(|root| root.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut child = spawn_dev_child_reported(&initial);
    'watching: while !DEV_STOP_REQUESTED.load(Ordering::Relaxed) {
        match event_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(event)) => {
                watcher.observe(&event);
                if event_requires_restart(&event, &roots) {
                    drain_until_quiet(&event_rx, &roots, request.debounce, &mut watcher);
                    loop {
                        report_child_exit(&mut child);
                        eprintln!("[tilo-app] change detected; rebuilding");
                        let building_revision = source_revision.load(Ordering::Acquire);
                        let candidate = plan_app_task(&app_dir, AppTask::Run, &request.forwarded)
                            .and_then(|plan| build_dev_candidate(&plan));

                        let queued_restart =
                            drain_pending_restart_events(&event_rx, &roots, |event| {
                                watcher.observe(event);
                            });
                        let superseded = queued_restart
                            || source_revision.load(Ordering::Acquire) != building_revision;
                        if DEV_STOP_REQUESTED.load(Ordering::Relaxed) {
                            break 'watching;
                        }
                        if superseded {
                            eprintln!(
                                "[tilo-app] newer change arrived during the build; skipping the stale candidate"
                            );
                            drain_until_quiet(&event_rx, &roots, request.debounce, &mut watcher);
                            continue;
                        }
                        match candidate {
                            Ok(executable) => {
                                eprintln!("[tilo-app] build succeeded; restarting");
                                stop_dev_child(&mut child);
                                child = spawn_dev_binary_reported(
                                    &app_dir,
                                    &executable,
                                    &request.forwarded,
                                );
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
                        break;
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Ok(Err(error)) => eprintln!("[tilo-app] watch error: {error}"),
            Err(RecvTimeoutError::Disconnected) => bail!("app file watcher stopped"),
        }
        report_child_exit(&mut child);
    }
    stop_dev_child(&mut child);
    eprintln!("[tilo-app] stopped");
    Ok(())
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
    loop {
        match events.try_recv() {
            Ok(Ok(event)) => {
                observe(&event);
                restart |= event_requires_restart(&event, roots);
            }
            Ok(Err(error)) => eprintln!("[tilo-app] watch error: {error}"),
            Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => return restart,
        }
    }
}

fn build_dev_candidate(plan: &CargoPlan) -> Result<PathBuf> {
    let manifest = plan.app_dir.join("Cargo.toml");
    let expected_manifest = manifest.canonicalize().unwrap_or_else(|_| manifest.clone());
    let expected_binary = dev_binary_name(&manifest)?;
    let mut command = ProcessCommand::new(&plan.program);
    command
        .arg("build")
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("--bin")
        .arg(&expected_binary)
        .arg("--message-format=json-render-diagnostics")
        .current_dir(&plan.app_dir)
        .stdin(Stdio::inherit())
        .stderr(Stdio::inherit())
        .stdout(Stdio::piped());
    let mut build = command
        .spawn()
        .with_context(|| format!("starting {} build", plan.program.to_string_lossy()))?;
    let stdout = build
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("Cargo build has no artifact stream"))?;
    let mut artifacts = HashMap::new();
    for line in BufReader::new(stdout).lines() {
        let line = line.context("reading Cargo development artifact stream")?;
        collect_cargo_message(&line, &expected_manifest, &mut artifacts);
    }
    let status = build
        .wait()
        .context("waiting for Cargo development build")?;
    if !status.success() {
        bail!("Cargo development build failed with {status}");
    }
    artifacts.remove(&expected_binary).with_context(|| {
        format!(
            "Cargo build completed without reporting executable {expected_binary:?}; set package.default-run when the app has multiple binaries"
        )
    })
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
    let Some(path) = manifest
        .get("dependencies")
        .and_then(|dependencies| dependencies.get(dependency_name))
        .and_then(|dependency| dependency.get("path"))
        .and_then(toml::Value::as_str)
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
}

impl DevWatcher {
    fn new(inner: RecommendedWatcher) -> Self {
        Self {
            inner,
            watched: HashSet::new(),
        }
    }

    fn watch_tree(&mut self, root: &Path) -> Result<()> {
        for directory in source_watch_directories(root)? {
            if !self.watched.insert(directory.clone()) {
                continue;
            }
            if let Err(error) = self.inner.watch(&directory, RecursiveMode::NonRecursive) {
                self.watched.remove(&directory);
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
        for path in event.paths.iter().filter(|path| {
            path.is_dir()
                && !ignored_watch_component(path.file_name().and_then(|name| name.to_str()))
        }) {
            if let Err(error) = self.watch_tree(path) {
                eprintln!("[tilo-app] cannot watch new directory: {error:#}");
            }
        }
    }
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

fn spawn_dev_child(plan: &CargoPlan) -> Result<Child> {
    let mut command = ProcessCommand::new(&plan.program);
    command.args(&plan.args).current_dir(&plan.app_dir);
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

fn check_package_manifest(app_dir: &Path, checks: &mut Vec<DoctorCheck>) -> Option<String> {
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
    Some(manifest.package.id)
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
            (Some("tilo-app"), None) => (
                DoctorStatus::Warning,
                "non-path tilo-app dependency; SDK source could not be verified".to_string(),
            ),
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
    let legacy_context = source.contains(&legacy_graph_app_template());
    let generated_context = source.contains("components::ui_graph_context()");
    let app_owned_context = !legacy_context && !generated_context;
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
    if legacy_context {
        checks.push(DoctorCheck {
            status: DoctorStatus::Fail,
            label: "UI components",
            detail: "src/lib.rs uses the legacy default router context; run `tilo-app add component <name> --mount <screen>` to migrate it"
                .to_owned(),
        });
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
        ["form-v1"] => Ok(GraphPropsContract::FormV1),
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
    check_locale_registry(&registry_path, &catalog_ids, checks);
}

fn check_locale_registry(
    registry_path: &Path,
    catalog_ids: &[String],
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
        let develop = format!("tilo-app dev {destination}");
        let verify = format!("tilo-app check {destination}");
        format!(
            "{}\n\nStart developing:\n  {develop}\n\nVerify everything:\n  {verify}\n\nRun `tilo-app --help` for testing, scenarios, screenshots, inspection, localization, and packaging.",
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
    ui_path: PathBuf,
    workspace_member: bool,
    template: Template,
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

fn create_app_interactive_with(
    request: NewAppRequest,
    input: &mut impl io::BufRead,
    output: &mut impl io::Write,
    interactive: bool,
) -> Result<CreateOutcome> {
    let current_dir = std::env::current_dir().context("reading current directory")?;
    create_app_interactive_from(&current_dir, request, input, output, interactive)
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
    if request.app_id.is_none() && full_wizard {
        let default_id = format!("so.tilo.{}", name.replace('-', "."));
        request.app_id = Some(prompt_validated(
            input,
            output,
            "App id",
            Some(&default_id),
            validate_app_id,
        )?);
    }
    if request.template.is_none() {
        request.template = Some(if interactive {
            prompt_template(input, output)?
        } else {
            Template::Starter
        });
    }
    let repo = find_repo_root(current_dir);
    if request.ui_path.is_none() && repo.is_none() && interactive {
        let discovered = discover_ui_dir(current_dir, None).map(|path| path.display().to_string());
        let value = prompt_validated(
            input,
            output,
            "Toolkit ui path",
            discovered.as_deref(),
            |value| {
                resolve_ui_dir(current_dir, None, Some(Path::new(value)))
                    .map(|_| ())
                    .context("invalid toolkit path")
            },
        )?;
        request.ui_path = Some(PathBuf::from(value));
    }
    let ui_dir = resolve_ui_dir(current_dir, repo.as_deref(), request.ui_path.as_deref())?;
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
            ui_dir.display(),
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

    let repo = find_repo_root(current_dir);
    let destination = new_app_destination(
        current_dir,
        repo.as_deref(),
        package_name,
        request.directory.as_deref(),
    );
    validate_destination(&destination)?;

    let app_id = request
        .app_id
        .unwrap_or_else(|| format!("so.tilo.{}", package_name.replace('-', ".")));
    validate_app_id(&app_id)?;
    let title = request
        .title
        .unwrap_or_else(|| title_from_name(package_name));
    if title.trim().is_empty() {
        bail!("app title must not be empty");
    }

    let workspace_path = repo
        .as_deref()
        .and_then(|root| destination.strip_prefix(root).ok())
        .filter(|path| path.components().next().is_some())
        .map(Path::to_path_buf);
    let workspace_member = workspace_path.is_some();
    let ui_dir = resolve_ui_dir(current_dir, repo.as_deref(), request.ui_path.as_deref())?;
    let ui_path = relative_path(&destination, &ui_dir);
    let spec = AppSpec {
        package_name: package_name.to_string(),
        crate_name: package_name.replace('-', "_"),
        title,
        app_id: app_id.clone(),
        ui_path,
        workspace_member,
        template: request.template.unwrap_or(Template::Starter),
    };
    let rendered = render(&spec)?;
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

    Ok(CreateOutcome {
        destination,
        package_name: spec.package_name,
        app_id,
        files,
        dry_run: request.dry_run,
        workspace_member,
    })
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
    writeln!(
        output,
        "\nStarter template:\n  1) starter    Profile resource edited through bound form controls\n  2) data       Sparse table with retained cell controls\n  3) list       Sparse variable-height retained list\n  4) tree       Professional hierarchy browser\n  5) graph      Serialized UI graph and router\n  6) async      Restartable background task with cancellation\n  7) document   Durable synchronized editor with undo\n  8) qr         IME input to accessible generated QR output\n  9) workspace  Responsive master-detail app with durable item editing"
    )
    .context("showing template choices")?;
    loop {
        let value = prompt(input, output, "Template", Some("1"))?;
        match parse_template(&value) {
            Ok(template) => return Ok(template),
            Err(error) => writeln!(output, "  ! {error}").context("showing template error")?,
        }
    }
}

fn parse_template(value: &str) -> Result<Template> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "starter" | "s" => Ok(Template::Starter),
        "2" | "data" | "d" => Ok(Template::Data),
        "3" | "list" | "l" => Ok(Template::List),
        "4" | "tree" | "t" => Ok(Template::Tree),
        "5" | "graph" | "g" | "builder" => Ok(Template::Graph),
        "6" | "async" | "a" | "task" => Ok(Template::Async),
        "7" | "document" | "doc" | "editor" => Ok(Template::Document),
        "8" | "qr" | "qrcode" | "showcase" => Ok(Template::Qr),
        "9" | "workspace" | "master-detail" | "professional" => Ok(Template::Workspace),
        _ => bail!(
            "unknown template {value:?}; choose 1-9 or `starter`, `data`, `list`, `tree`, `graph`, `async`, `document`, `qr`, or `workspace`"
        ),
    }
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
    {
        return Some(ui);
    }
    ui_dir_from_ancestor(Path::new(env!("CARGO_MANIFEST_DIR")))
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
        (PathBuf::from("Cargo.toml"), cargo_toml(spec)),
        (PathBuf::from("src/lib.rs"), app_lib(spec)),
        (
            PathBuf::from("src/locales.rs"),
            locale_registry_source(&["en-US".to_owned()]),
        ),
        (PathBuf::from("src/main.rs"), app_main(spec)),
        (PathBuf::from("tests/app.rs"), app_test(spec)),
        (
            PathBuf::from("tests/headless-actions.json"),
            headless_actions(spec),
        ),
        (PathBuf::from("tilo-app.toml"), app_manifest(spec)),
        (PathBuf::from("README.md"), app_readme(spec)),
        (PathBuf::from("locales/en-US.ftl"), source_catalog()),
    ];
    if spec.template == Template::Graph {
        files.push((PathBuf::from("ui/main.json"), graph_document(spec)?));
        files.push((
            PathBuf::from("src/screens.rs"),
            screen_registry_source(&["main".into()]),
        ));
        files.push((
            PathBuf::from("src/components/mod.rs"),
            component_registry_source(&[], &BTreeSet::new())?,
        ));
    }
    for (path, source) in &mut files {
        if path.extension().is_some_and(|extension| extension == "rs") {
            *source = rustfmt_source(source, path)?;
        }
    }
    Ok(files)
}

fn cargo_toml(spec: &AppSpec) -> String {
    let lints = if spec.workspace_member {
        "\n[lints]\nworkspace = true\n"
    } else {
        ""
    };
    let patches = if spec.workspace_member {
        String::new()
    } else {
        let repo = checkout_root_for_ui(&spec.ui_path);
        format!(
            "\n[patch.crates-io]\nwinit = {{ path = {:?} }}\nvello_encoding = {{ path = {:?} }}\nvello = {{ path = {:?} }}\nvello_shaders = {{ path = {:?} }}\n",
            repo.join("vendor/winit").to_string_lossy(),
            repo.join("vendor/vello_encoding").to_string_lossy(),
            repo.join("vendor/vello").to_string_lossy(),
            repo.join("vendor/vello_shaders").to_string_lossy(),
        )
    };
    let extras = format!("{lints}{patches}");
    let sdk_path = sdk_path_for_ui(&spec.ui_path);
    format!(
        "[package]\nname = {name:?}\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n[lib]\nname = {crate_name:?}\npath = \"src/lib.rs\"\n\n[[bin]]\nname = {name:?}\npath = \"src/main.rs\"\n\n[dependencies]\nui = {{ package = \"tilo-app\", path = {sdk_path:?} }}\nserde = {{ version = \"1\", features = [\"derive\"] }}\nserde_json = \"1\"\n{lints}",
        name = spec.package_name,
        crate_name = spec.crate_name,
        sdk_path = sdk_path.to_string_lossy(),
        lints = extras,
    )
}

fn checkout_root_for_ui(ui_path: &Path) -> &Path {
    if ui_path.ends_with("crates/sdk/ui-core") {
        ui_path
            .ancestors()
            .nth(3)
            .unwrap_or_else(|| Path::new(".."))
    } else {
        ui_path.parent().unwrap_or_else(|| Path::new(".."))
    }
}

fn sdk_path_for_ui(ui_path: &Path) -> PathBuf {
    if ui_path.ends_with("crates/sdk/ui-core") {
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
        Template::Data => DATA_APP_TEMPLATE.to_owned(),
        Template::List => LIST_APP_TEMPLATE.to_owned(),
        Template::Tree => TREE_APP_TEMPLATE.to_owned(),
        Template::Graph => {
            format!("pub mod components;\npub mod screens;\n\n{GRAPH_APP_TEMPLATE}")
        }
        Template::Async => ASYNC_APP_TEMPLATE.to_owned(),
        Template::Document => DOCUMENT_APP_TEMPLATE.to_owned(),
        Template::Qr => QR_APP_TEMPLATE.to_owned(),
        Template::Workspace => WORKSPACE_APP_TEMPLATE.to_owned(),
    };
    format!(
        "//! {title} application.\n\n/// The application id.\npub const APP_ID: &str = {app_id:?};\n/// The window and launcher title.\npub const APP_TITLE: &str = {title:?};\n\n{GENERATED_LOCALE_SETUP}\n\n{application}",
        title = spec.title,
        app_id = spec.app_id,
    )
}

fn source_catalog() -> String {
    String::from(
        "starter-title = Your first Tilo app\n\
starter-profile-title = Profile\n\
starter-name-label = Your name\n\
starter-name-placeholder = Type your name\n\
starter-updates-label = Receive updates\n\
starter-refresh-label = Refresh interval\n\
starter-volume-label = Volume\n\
starter-region-label = Region\n\
starter-greeting = Hello, { $name }!\n\
starter-reset = Reset profile\n\
data-name-column = Name\n\
data-actions-column = Actions\n\
data-accessible-name = Results\n\
data-open = Open\n\
data-recent = Recently opened: { $rows }\n\
data-none-opened = No rows opened yet\n\
list-accessible-name = Activity\n\
list-open = Open\n\
list-loading = Loading activity…\n\
list-last-opened = Last opened: { $activity }\n\
list-none-opened = Nothing opened yet\n\
tree-accessible-name = Workspace items\n\
tree-row-action = Pin\n\
tree-pinned = Pinned: { $count }\n\
graph-project-name = Project name\n\
graph-create = Create\n\
graph-create-tooltip = Create the project\n\
task-title = Background work\n\
task-run = Run task\n\
task-cancel = Cancel\n\
task-idle = Start a background task without polling\n\
task-loading = Loading…\n\
task-ready = Found { $count } entries\n\
task-folder-label = Folder\n\
document-heading = Durable document\n\
document-title-label = Title\n\
document-title-placeholder = Untitled\n\
document-body-label = Body\n\
document-body-placeholder = Start writing…\n\
document-preview-heading = Live preview\n\
qr-title = QR code generator\n\
qr-input-label = Content\n\
qr-input-placeholder = Type text or a link\n\
qr-output-alt = QR code for { $payload }\n\
qr-reset = Reset QR code\n\
workspace-root = Workspace\n\
workspace-projects = Projects\n\
workspace-archive = Archive\n\
workspace-desktop-app = Desktop app\n\
workspace-design-system = Design system\n\
workspace-first-prototype = First prototype\n\
workspace-unknown-item = Selected item\n\
workspace-tree-name = Workspace items\n\
workspace-navigation-heading = Projects\n\
workspace-title-label = Title\n\
workspace-title-placeholder = Untitled item\n\
workspace-notes-label = Notes\n\
workspace-notes-placeholder = Add project notes…\n\
workspace-preview-heading = Live preview\n\
workspace-back = Back\n\
workspace-share = Share\n",
    )
}

fn app_main(spec: &AppSpec) -> String {
    format!(
        "//! The app's entry point: the live window, and the standard `--shots`,\n//! `--headless-drive` and `--native-audit` modes, from one declaration.\n\nfn main() {{\n    ui::framework::run::<{crate_name}::App>();\n}}\n",
        crate_name = spec.crate_name,
    )
}

fn app_test(spec: &AppSpec) -> String {
    let tests = match spec.template {
        Template::Starter => STARTER_APP_TESTS.to_owned(),
        Template::Data => DATA_APP_TESTS.to_owned(),
        Template::List => LIST_APP_TESTS.to_owned(),
        Template::Tree => TREE_APP_TESTS.to_owned(),
        Template::Graph => format!("{GRAPH_APP_TESTS}\n{GRAPH_SCREEN_TESTS}"),
        Template::Async => ASYNC_APP_TESTS.to_owned(),
        Template::Document => DOCUMENT_APP_TESTS.to_owned(),
        Template::Qr => QR_APP_TESTS.to_owned(),
        Template::Workspace => WORKSPACE_APP_TESTS.to_owned(),
    };
    // The test sources name the app crate `generated_app`: the template smoke
    // crate runs these same tests against each template under that name.
    format!("{tests}{NATIVE_APP_TESTS}").replace("generated_app", &spec.crate_name)
}

fn app_manifest(spec: &AppSpec) -> String {
    format!(
        "[package]\nid = {app_id:?}\nname = {title:?}\ncategory = \"Utility\"\nversion = \"0.1.0\"\nkind = \"app\"\nruntime = \"tilo-runtime/2026.07\"\n\n[publisher]\nidentity = \"ed25519:0000000000000000000000000000000000000000000000000000000000000000\"\nsignature = \"\"\n\n[sandbox]\nnamespaces = [\"user\", \"pid\", \"uts\", \"ipc\", \"mount\"]\nseccomp = \"tilo-default\"\nlandlock = \"strict\"\n\n[sandbox.net]\nmode = \"off\"\n\n[sandbox.portals]\n\n[sandbox.services]\ncollab = \"optional@1\"\ntilo-fs = \"optional@1\"\n\n[entrypoint]\nbin = \"bin/{package}\"\ntype = \"gui\"\nsplash-budget = 250\n\n[providers]\nmime = []\next = []\n",
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
        Template::Data => String::from(
            "[\n  {\"perform\": {\"target\": \"rows-table\", \"action\": \"focus\"}},\n  {\"perform\": {\"target\": \"rows-table\", \"semantic_target\": \"row:3:0\", \"action\": \"activate\"}},\n  {\"expect\": {\"target\": \"rows-table\", \"semantic_target\": \"row:3:0\", \"role\": \"row\", \"selected\": true}}\n]\n",
        ),
        Template::List => String::from(
            "[\n  {\"perform\": {\"target\": \"activity-list\", \"action\": \"focus\"}},\n  {\"wait_for_named\": {\"target\": \"open-0\", \"timeout_ms\": 1000}},\n  {\"perform\": {\"target\": \"activity-list\", \"semantic_target\": \"key:3\", \"action\": \"activate\"}},\n  {\"expect\": {\"target\": \"activity-list\", \"semantic_target\": \"key:3\", \"role\": \"list_item\", \"selected\": true}},\n  {\"perform\": {\"target\": \"open-0\", \"action\": \"activate\"}}\n]\n",
        ),
        Template::Tree => String::from(
            "[\n  {\"perform\": {\"target\": \"workspace-tree\", \"action\": \"focus\"}},\n  {\"perform\": {\"target\": \"workspace-tree\", \"semantic_target\": \"node:2\", \"action\": \"collapse\"}},\n  {\"perform\": {\"target\": \"workspace-tree\", \"semantic_target\": \"node:2\", \"action\": \"expand\"}},\n  {\"perform\": {\"target\": \"workspace-tree\", \"semantic_target\": \"node:3\", \"action\": \"activate\"}},\n  {\"expect\": {\"target\": \"workspace-tree\", \"semantic_target\": \"node:3\", \"role\": \"tree_item\", \"selected\": true}},\n  {\"perform\": {\"target\": \"more-4\", \"action\": \"activate\"}}\n]\n",
        ),
        Template::Graph => String::from(
            "[\n  {\"perform\": {\"target\": \"name-input\", \"action\": \"set_value\", \"value\": \"Atlas\"}},\n  {\"expect\": {\"target\": \"name-preview\", \"role\": \"static_text\", \"name\": \"Atlas\"}},\n  {\"perform\": {\"target\": \"create\", \"action\": \"activate\"}}\n]\n",
        ),
        Template::Async => String::from(
            "[\n  {\"perform\": {\"target\": \"run-task\", \"action\": \"activate\"}},\n  {\"expect\": {\"target\": \"task-result\", \"role\": \"static_text\", \"name_contains\": \"Found\", \"timeout_ms\": 1000}}\n]\n",
        ),
        Template::Document => String::from(
            "[\n  {\"perform\": {\"target\": \"document-title-input\", \"action\": \"set_value\", \"value\": \"Project Atlas\"}},\n  {\"perform\": {\"target\": \"document-body-input\", \"action\": \"set_value\", \"value\": \"A durable first draft.\"}},\n  {\"expect\": {\"target\": \"document-title-preview\", \"role\": \"heading\", \"name\": \"Project Atlas\"}},\n  {\"expect\": {\"target\": \"document-body-preview\", \"name\": \"A durable first draft.\"}}\n]\n",
        ),
        Template::Qr => String::from(
            "[\n  {\"perform\": {\"target\": \"qr-input\", \"action\": \"set_value\", \"value\": \"你好 Rust GUI\"}},\n  {\"expect\": {\"target\": \"qr-input\", \"role\": \"text_input\", \"value\": \"你好 Rust GUI\"}},\n  {\"expect\": {\"target\": \"qr-output\", \"role\": \"image\", \"name_contains\": \"你好 Rust GUI\"}}\n]\n",
        ),
        Template::Workspace => String::from(
            "[\n  {\"perform\": {\"target\": \"workspace-tree\", \"semantic_target\": \"node:4\", \"action\": \"activate\"}},\n  {\"expect\": {\"target\": \"workspace-tree\", \"semantic_target\": \"node:4\", \"role\": \"tree_item\", \"selected\": true}},\n  {\"expect\": {\"target\": \"workspace-selected-item\", \"role\": \"heading\", \"name\": \"Design system\"}},\n  {\"perform\": {\"target\": \"workspace-title-input\", \"action\": \"set_value\", \"value\": \"Design tokens\"}},\n  {\"expect\": {\"target\": \"workspace-title-preview\", \"role\": \"heading\", \"name\": \"Design tokens\"}}\n]\n",
        ),
    }
}

fn graph_document(spec: &AppSpec) -> Result<String> {
    let document = tilo_ui_graph::UiDocument::new(tilo_ui_graph::UiNode::Column {
        id: "root".into(),
        children: vec![
            tilo_ui_graph::UiNode::Text {
                id: "title".into(),
                text: tilo_ui_graph::UiTextSource::Static {
                    value: spec.title.clone(),
                },
                role: tilo_ui_graph::UiTextRole::Title,
                size: None,
            },
            tilo_ui_graph::UiNode::Text {
                id: "graph-introduction".into(),
                text: tilo_ui_graph::UiTextSource::Static {
                    value: "This read-only screen remains live-editable; native controls below publish actions."
                        .into(),
                },
                role: tilo_ui_graph::UiTextRole::Body,
                size: None,
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
        "The app is written on the Tilo app framework: its state is a `#[derive(Model)]` resource, every edit is an `#[action]` method, and the view binds controls to those actions (`cx.input`, `cx.checkbox`, `cx.button`, `cx.control`, `cx.each`). Storage, sync, collaboration, history, per-author undo, permissions, the action palette, agents, scripts and search come from the framework; `Replica` tests the resource and `TestApp` tests the running app. `tilo-app doctor` rejects apps that remove this surface.\n"
    );
    let _ = writeln!(
        out,
        "From the parent directory, scaffold and launch this template in one command:"
    );
    let _ = writeln!(out, "\n```bash");
    let _ = writeln!(
        out,
        "tilo-app new {} --template {} --dev",
        spec.package_name,
        spec.template.name()
    );
    let _ = writeln!(out, "```");
    let (resource, action, arguments) = readme_action_example(spec.template);
    let _ = writeln!(
        out,
        "\nAgents, scripts, and search invoke the same address as the visible control. Discover the live catalogue, then submit an invocation shaped like:\n"
    );
    let _ = writeln!(out, "```json");
    let _ = writeln!(
        out,
        "{{\"resource\":{resource:?},\"action\":{action:?},\"arguments\":{arguments}}}"
    );
    let _ = writeln!(out, "```");
    let _ = writeln!(
        out,
        "\nValidate the native SDK, action surface, scenarios, and package metadata with one command:\n"
    );
    let _ = writeln!(out, "```bash");
    let _ = writeln!(out, "tilo-app check --fix");
    let _ = writeln!(out, "tilo-app test");
    let _ = writeln!(out, "tilo-app dev");
    let _ = writeln!(out, "tilo-app run");
    let _ = writeln!(out, "tilo-app add component profile-card --template form");
    let _ = writeln!(out, "tilo-app add component project-tree --template tree");
    let _ = writeln!(out, "tilo-app add component activity-list --template list");
    let _ = writeln!(out, "tilo-app add component results-table --template table");
    let _ = writeln!(out, "tilo-app add component article-body --template text");
    let _ = writeln!(out, "tilo-app add screen settings --template settings");
    let _ = writeln!(out, "tilo-app add scenario empty-state");
    let _ = writeln!(out, "tilo-app scenario run");
    let _ = writeln!(
        out,
        "tilo-app scenario run --artifacts target/scenario-artifacts"
    );
    let _ = writeln!(
        out,
        "tilo-app scenario run --audit --artifacts target/scenario-artifacts"
    );
    let _ = writeln!(out, "tilo-app add locale de-DE");
    let _ = writeln!(out, "tilo-app locale sync");
    let _ = writeln!(out, "tilo-app shot");
    let _ = writeln!(
        out,
        "tilo-app inspect --actions tests/headless-actions.json"
    );
    let _ = writeln!(
        out,
        "tilo-app inspect --audit --audit-report target/audit/report.json --actions tests/headless-actions.json"
    );
    let _ = writeln!(out, "tilo-app package");
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
    if spec.template == Template::Async {
        let _ = writeln!(
            out,
            "\nThe folder to scan is resource state: durable, synced and undoable. The task itself is an effect action, so the palette, agents and scripts can start and cancel it too. The `run-task` button creates a `LatestTask` completion capability for its worker, which reads its input from the resource when it starts. Starting again or using `cancel-task` makes older completions stale, so late work cannot overwrite newer UI state. An accepted completion wakes the app automatically, and `ModelView::from_state_by` swaps the loading/result/error subtree by a small structural key."
        );
    }
    if spec.template == Template::Document {
        let _ = writeln!(
            out,
            "\nThe title and body inputs invoke the document's actions; their preview reads the same fields and follows every edit (local, a collaborator's, an undo) with no notification code. Text fields are collaborative text, so concurrent typing merges character by character. `clear` is declared destructive, so it asks for confirmation wherever it is invoked."
        );
    }
    if spec.template == Template::Qr {
        let _ = writeln!(
            out,
            "\nThe input invokes `set-payload` on the resource. `cx.value` follows the payload into a `LocalState<String>`, `ModelView::from_state` rebuilds only the generated QR output when it changes, and the replacement image carries payload-specific alternative text for inspection, tests, and screen readers."
        );
    }
    if spec.template == Template::Workspace {
        let _ = writeln!(
            out,
            "\nThe workspace starter composes a responsive master-detail shell from ordinary retained widgets. Wide windows show navigation and detail together; compact windows expose an accessible Back action and switch between full-width panes through typed `LocalState<NarrowScreen>`. The selection is view state (`cx.local`); `cx.switch` rebuilds only the detail pane when it changes, and the pane's inputs bind to the selected project's fields in the resource. Switching projects therefore needs no synchronization callbacks and preserves independent persisted, collaborative, undoable edits."
        );
    }
    let _ = writeln!(
        out,
        "\nUser-facing strings start in `locales/en-US.ftl`. `tilo-app add locale de-DE` creates and registers a complete editable copy of that message contract. After changing the source, `tilo-app locale sync` adds missing units without replacing translations; `tilo-app doctor` reports differences deliberately left for a translator while the toolkit falls back to the source catalog."
    );
    let _ = writeln!(out, "\nApp id: `{}`", spec.app_id);
    out
}

fn readme_action_example(template: Template) -> (&'static str, &'static str, &'static str) {
    match template {
        Template::Starter => ("generated.profile", "set-name", r#"{"name":"Ada"}"#),
        Template::Data => ("generated.table", "open", r#"{"id":42}"#),
        Template::List => ("generated.activity-list", "open", r#"{"id":42}"#),
        Template::Tree => ("generated.tree", "toggle-pin", r#"{"id":4}"#),
        Template::Graph => (
            "generated.graph-project",
            "set-draft",
            r#"{"draft":"Launch plan"}"#,
        ),
        Template::Async => ("generated.background-task:root", "run", "{}"),
        Template::Document => (
            "generated.document",
            "set-content",
            r#"{"title":"Plan","body":"Ship it"}"#,
        ),
        Template::Qr => (
            "generated.qr-code",
            "set-payload",
            r#"{"payload":"https://example.com"}"#,
        ),
        Template::Workspace => ("generated.workspace-item", "share", r#"{"id":4}"#),
    }
}

#[cfg(test)]
mod tests;
