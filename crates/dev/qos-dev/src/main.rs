use std::ffi::OsString;
use std::io::{BufRead as _, BufReader};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, ExitCode, Stdio};
use std::time::Duration;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "qos-dev",
    version,
    about = "Create and work on qOS apps",
    long_about = "The short path from an app idea to a running qOS window.\n\n\
                  Run `qos-dev new` for an interactive setup, or provide a name \
                  and flags for a reproducible, non-interactive scaffold. Then \
                  use `add`, `locale`, `check`, `scenario`, `dev`, `test`, `run`, `shot`, `inspect`, and `package` from anywhere inside the app. Use `ui` to construct validated serialized screens."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a buildable app with packaging and headless-test defaults.
    New(NewArgs),
    /// Add a screen, reusable component, headless scenario, or translation catalog.
    Add(AddArgs),
    /// Keep translation catalogs structurally aligned with the source locale.
    Locale(LocaleArgs),
    /// Run saved headless interaction scenarios against isolated app state.
    Scenario(ScenarioArgs),
    /// Diagnose app source, manifest, lifecycle, and toolkit wiring.
    Doctor(DoctorArgs),
    /// Run structural checks, tests, and accessibility-audited scenarios.
    Check(CheckArgs),
    /// Prove the production root satisfies the live qOS-native contract.
    Certify(CertifyArgs),
    /// Run an app from any path inside it, after a structural preflight.
    Run(AppTaskArgs),
    /// Test an app from any path inside it, after a structural preflight.
    Test(AppTaskArgs),
    /// Run an app and rebuild/restart it after source or asset changes.
    Dev(DevArgs),
    /// Render the production app root to an image without opening a window.
    Shot(ShotArgs),
    /// Write the post-layout widget and accessibility tree without opening a window.
    Inspect(InspectArgs),
    /// Create and edit validated serialized UI object graphs.
    Ui(UiArgs),
    /// Build, assemble, sign, and verify an installable qOS package.
    Package(PackageArgs),
}

#[derive(clap::Args)]
struct AddArgs {
    #[command(subcommand)]
    command: Option<AddCommand>,
}

#[derive(Subcommand)]
enum AddCommand {
    /// Add a validated serialized screen under ui/.
    Screen {
        /// Lowercase kebab-case screen name.
        name: String,
        /// Visible starter heading. Defaults to a title made from the name.
        #[arg(long)]
        title: Option<String>,
        /// Common screen shape to scaffold.
        #[arg(long, value_enum, default_value = "blank")]
        template: ScreenTemplateArg,
        /// Add a button on this existing screen that pushes the new screen.
        #[arg(long)]
        link_from: Option<String>,
        #[command(flatten)]
        options: AddOptions,
    },
    /// Add a headless action sequence under tests/scenarios/.
    Scenario {
        /// Lowercase kebab-case scenario name.
        name: String,
        /// Derive actions from a registered graph screen reachable from main.
        #[arg(long)]
        screen: Option<String>,
        #[command(flatten)]
        options: AddOptions,
    },
    /// Add a Fluent translation catalog under locales/.
    Locale {
        /// BCP-47-style locale, for example de, en-GB, or zh-Hant-TW.
        locale: String,
        #[command(flatten)]
        options: AddOptions,
    },
    /// Add a reusable Rust component under src/components/.
    Component {
        /// Lowercase kebab-case component name.
        name: String,
        /// Common component shape to scaffold.
        #[arg(long, value_enum, default_value = "display")]
        template: ComponentTemplateArg,
        /// Also insert the first display or form instance into this graph screen.
        #[arg(long)]
        mount: Option<String>,
        /// Stable mounted node id. Defaults to a unique component-name id.
        #[arg(long, requires = "mount")]
        id: Option<String>,
        /// Mounted node parent. Defaults to root.
        #[arg(long, requires = "mount")]
        parent: Option<String>,
        /// Mounted child position. Defaults to appending.
        #[arg(long, requires = "mount")]
        index: Option<usize>,
        /// Mounted component props as one JSON object. Defaults to starter copy.
        #[arg(long, requires = "mount")]
        props: Option<String>,
        #[command(flatten)]
        options: AddOptions,
    },
}

#[derive(clap::Args)]
struct AddOptions {
    /// App directory or a file/directory inside it. Defaults to the current directory.
    #[arg(long)]
    app: Option<PathBuf>,
    /// Replace an existing artifact.
    #[arg(long)]
    force: bool,
    /// Print the destination without writing it.
    #[arg(long)]
    dry_run: bool,
}

#[derive(clap::Args)]
struct LocaleArgs {
    #[command(subcommand)]
    command: LocaleCommand,
}

#[derive(clap::Args)]
struct ScenarioArgs {
    #[command(subcommand)]
    command: ScenarioCommand,
}

#[derive(Subcommand)]
enum ScenarioCommand {
    /// Run one named scenario, or every saved scenario when omitted.
    Run {
        /// Scenario name under tests/scenarios, or `default` for headless-actions.json.
        name: Option<String>,
        /// App directory or a file/directory inside it. Defaults to the current directory.
        #[arg(long)]
        app: Option<PathBuf>,
        /// Headless layout size as `WIDTHxHEIGHT`.
        #[arg(long, value_parser = parse_viewport)]
        viewport: Option<(u32, u32)>,
        /// Write each run's semantic tree, targets, and document JSON here.
        /// Relative paths are resolved from the app root.
        #[arg(long)]
        artifacts: Option<PathBuf>,
        /// Fail each case when its final semantic tree violates accessibility contracts.
        #[arg(long)]
        audit: bool,
    },
}

#[derive(Subcommand)]
enum LocaleCommand {
    /// Add missing source units while preserving translated values.
    Sync {
        /// One locale to synchronize. Omit it to synchronize every translation.
        locale: Option<String>,
        /// Remove messages, terms, attributes, and values absent from en-US.
        #[arg(long)]
        prune: bool,
        /// Replace only patterns whose external variable set differs from en-US.
        #[arg(long)]
        reset_variables: bool,
        /// App directory or a file/directory inside it. Defaults to the current directory.
        #[arg(long)]
        app: Option<PathBuf>,
        /// Report changes without writing them.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(clap::Args)]
struct UiArgs {
    #[command(subcommand)]
    command: UiCommand,
}

#[derive(Subcommand)]
enum UiCommand {
    /// Open the graph in the native hierarchy, preview, and property editor.
    Edit {
        file: PathBuf,
        /// Override the qos-ui-builder executable.
        #[arg(long)]
        builder: Option<OsString>,
    },
    /// Create an accessible starter screen.
    Init {
        file: PathBuf,
        #[arg(long, default_value = "New screen")]
        title: String,
        #[arg(long)]
        force: bool,
    },
    /// Print the node hierarchy.
    Tree { file: PathBuf },
    /// Validate a graph against the toolkit schema.
    Validate { file: PathBuf },
    /// Rewrite a graph in canonical formatting.
    Format { file: PathBuf },
    /// Insert a node into a row or column.
    Insert {
        file: PathBuf,
        #[arg(long)]
        parent: String,
        #[arg(long)]
        index: usize,
        #[command(flatten)]
        source: NodeSourceArgs,
    },
    /// Insert an app-owned component without hand-writing a complete node.
    Component {
        file: PathBuf,
        /// Registered kebab-case component factory name.
        name: String,
        /// Stable semantic node id. Defaults to the component name, made unique.
        #[arg(long)]
        id: Option<String>,
        /// Row or column that receives the component.
        #[arg(long, default_value = "root")]
        parent: String,
        /// Child position. Defaults to appending to the parent.
        #[arg(long)]
        index: Option<usize>,
        /// Component props as one JSON object.
        #[arg(long, default_value = "{}")]
        props: String,
    },
    /// Remove a node and its subtree.
    Remove {
        file: PathBuf,
        #[arg(long)]
        id: String,
    },
    /// Replace a node in place, including the root.
    Replace {
        file: PathBuf,
        #[arg(long)]
        id: String,
        #[command(flatten)]
        source: NodeSourceArgs,
    },
    /// Reorder or reparent a subtree.
    Move {
        file: PathBuf,
        #[arg(long)]
        id: String,
        #[arg(long)]
        parent: String,
        #[arg(long)]
        index: usize,
    },
    /// Change one node property without replacing the complete node.
    Set {
        file: PathBuf,
        #[arg(long)]
        id: String,
        /// Dot-separated serialized property, for example `text.value`.
        #[arg(long)]
        property: String,
        #[command(flatten)]
        edit: PropertyEditArgs,
    },
    /// Apply a JSON array of graph edits as one validated publication.
    Apply {
        file: PathBuf,
        /// JSON file containing tagged insert/remove/replace/move/set operations.
        edits: PathBuf,
    },
}

#[derive(clap::Args)]
#[group(id = "node-source", required = true, multiple = false)]
struct NodeSourceArgs {
    /// Inline JSON object for one node.
    #[arg(long, group = "node-source")]
    node: Option<String>,
    /// File containing one node JSON object.
    #[arg(long, group = "node-source")]
    node_file: Option<PathBuf>,
}

#[derive(clap::Args)]
#[group(id = "property-edit", required = true, multiple = false)]
struct PropertyEditArgs {
    /// JSON value, or an unquoted string when JSON parsing does not apply.
    #[arg(long, group = "property-edit")]
    value: Option<String>,
    /// Remove the property so its schema default applies.
    #[arg(long, group = "property-edit")]
    unset: bool,
}

#[derive(clap::Args)]
struct DoctorArgs {
    /// App directory or a file/directory inside it. Defaults to the current directory.
    path: Option<PathBuf>,
}

#[derive(clap::Args)]
struct CheckArgs {
    /// App directory or a file/directory inside it. Defaults to the current directory.
    path: Option<PathBuf>,

    /// Write each audited scenario's semantic, target, document, and audit JSON here.
    /// Relative paths are resolved from the app root.
    #[arg(long)]
    artifacts: Option<PathBuf>,

    /// Headless scenario layout size as `WIDTHxHEIGHT`.
    #[arg(long, value_parser = parse_viewport)]
    viewport: Option<(u32, u32)>,

    /// Run structural checks and Rust tests without saved scenarios.
    #[arg(long)]
    no_scenarios: bool,

    /// Apply rustfmt to the app package before running the remaining strict checks.
    #[arg(long)]
    fix: bool,

    /// Arguments forwarded to the Cargo test harness. Write them after `--`.
    #[arg(last = true, allow_hyphen_values = true)]
    forwarded: Vec<OsString>,
}

#[derive(clap::Args)]
struct CertifyArgs {
    /// App directory or a file/directory inside it. Defaults to the current directory.
    path: Option<PathBuf>,

    /// Certify every graphical application in this qOS workspace.
    #[arg(long)]
    workspace: bool,
}

#[derive(clap::Args)]
struct AppTaskArgs {
    /// App directory or a file/directory inside it. Defaults to the current directory.
    path: Option<PathBuf>,

    /// Arguments forwarded to the app or test harness. Write them after `--`.
    #[arg(last = true, allow_hyphen_values = true)]
    forwarded: Vec<OsString>,
}

#[derive(clap::Args)]
struct DevArgs {
    /// App directory or a file/directory inside it. Defaults to the current directory.
    path: Option<PathBuf>,

    /// Quiet period used to coalesce editor save events.
    #[arg(long, default_value = "150")]
    debounce_ms: NonZeroU64,

    /// Watch only the app, not its local `ui` dependency.
    #[arg(long)]
    app_only: bool,

    /// Arguments forwarded to the app. Write them after `--`.
    #[arg(last = true, allow_hyphen_values = true)]
    forwarded: Vec<OsString>,
}

#[derive(clap::Args)]
struct ShotArgs {
    /// App directory or a file/directory inside it. Defaults to the current directory.
    path: Option<PathBuf>,

    /// Image path, relative to the app root unless absolute.
    #[arg(short, long, default_value = "preview.png")]
    output: PathBuf,

    /// Screenshot size as `WIDTHxHEIGHT`.
    #[arg(long, value_parser = parse_viewport)]
    viewport: Option<(u32, u32)>,

    /// App-defined screenshot scenario.
    #[arg(long)]
    scenario: Option<String>,

    /// Additional app-defined screenshot arguments. Write them after `--`.
    #[arg(last = true, allow_hyphen_values = true)]
    forwarded: Vec<OsString>,
}

#[derive(clap::Args)]
struct InspectArgs {
    /// App directory or a file/directory inside it. Defaults to the current directory.
    path: Option<PathBuf>,

    /// Inspection JSON path, relative to the app root unless absolute.
    #[arg(short, long, default_value = "inspect.json")]
    output: PathBuf,

    /// Headless layout size as `WIDTHxHEIGHT`.
    #[arg(long, value_parser = parse_viewport)]
    viewport: Option<(u32, u32)>,

    /// JSON file containing an array of headless actions to run before inspection.
    #[arg(long)]
    actions: Option<PathBuf>,

    /// Also write the resulting Automerge document as JSON.
    #[arg(long)]
    dump: Option<PathBuf>,

    /// Also write the addressable-widget target list as JSON.
    #[arg(long)]
    targets: Option<PathBuf>,

    /// Fail when the final semantic tree violates accessibility contracts.
    #[arg(long)]
    audit: bool,

    /// Also write the structured accessibility audit as JSON.
    #[arg(long)]
    audit_report: Option<PathBuf>,
}

#[derive(clap::Args)]
struct PackageArgs {
    /// App directory or a file/directory inside it. Defaults to the current directory.
    path: Option<PathBuf>,

    /// Package path, relative to the app root unless absolute.
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Persistent qos-pkg signing identity, relative to the app root unless absolute.
    #[arg(long)]
    identity: Option<PathBuf>,

    /// Cargo profile used to build the declared entrypoints.
    #[arg(long, default_value = "release")]
    profile: String,

    /// Rust target triple passed to Cargo.
    #[arg(long)]
    target: Option<String>,

    /// Comma-separated Cargo features enabled for the package build.
    #[arg(long, value_delimiter = ',')]
    features: Vec<String>,

    /// Disable the app's default Cargo features.
    #[arg(long)]
    no_default_features: bool,

    /// Replace an existing output package after the new package verifies successfully.
    #[arg(long)]
    force: bool,
}

#[derive(clap::Args)]
struct NewArgs {
    /// Cargo package name, for example `reading-list`.
    name: Option<String>,

    /// Reverse-DNS application id. Defaults to `dev.qos.<name>`.
    #[arg(long)]
    id: Option<String>,

    /// Human-readable application name.
    #[arg(long)]
    title: Option<String>,

    /// Starter shape to generate.
    #[arg(long, value_enum)]
    template: Option<TemplateArg>,

    /// Destination directory. Defaults to `apps/<name>` in a qOS checkout.
    #[arg(long)]
    directory: Option<PathBuf>,

    /// Override the discovered toolkit `ui` crate (or set `QOS_UI_PATH`).
    #[arg(long)]
    ui_path: Option<PathBuf>,

    /// Print the files that would be created without writing them.
    #[arg(long, conflicts_with = "dev")]
    dry_run: bool,

    /// Enter the live rebuild/restart loop after creating the app.
    #[arg(long, conflicts_with = "dry_run")]
    dev: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum TemplateArg {
    /// A small, accessible window using the standard app lifecycle.
    Starter,
    /// A virtualized, selectable data table suitable for large models.
    Data,
    /// A sparse, variable-height list with interactive retained row widgets.
    List,
    /// A stable-id, accessible tree suitable for professional hierarchy UIs.
    Tree,
    /// A live-editable serialized UI graph with model-bound controls.
    Graph,
    /// Typed background work with automatic wakeup and keyed result views.
    Async,
    /// Durable synchronized fields with persistence and built-in undo.
    Document,
    /// IME-capable input driving an accessible generated QR image.
    Qr,
    /// A responsive master-detail app with durable per-item editing.
    Workspace,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum ScreenTemplateArg {
    /// A localized title and starter body.
    #[default]
    Blank,
    /// Labeled inputs, a checkbox, draft bindings, and a save action.
    Form,
    /// Labeled preferences, stable bindings, and a save action.
    Settings,
    /// Explanatory copy and a primary recovery action.
    EmptyState,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum ComponentTemplateArg {
    /// Typed props rendered as an accessible heading and detail.
    #[default]
    Display,
    /// Typed local state bound to labeled text and boolean controls.
    Form,
    /// Source-agnostic virtualized tree with typed configuration.
    Tree,
    /// Source-agnostic sparse variable-height list with retained delegates.
    List,
    /// Source-agnostic sparse table with app-defined columns and cells.
    Table,
    /// Styled text with polygon flow or a custom layout engine.
    Text,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::New(args) => run_new(args),
        Command::Add(args) => run_add(args),
        Command::Locale(args) => run_locale(args),
        Command::Scenario(args) => run_scenario(args),
        Command::Doctor(args) => run_doctor(args),
        Command::Check(args) => run_check(args),
        Command::Certify(args) => run_certify(args),
        Command::Run(args) => run_app_task(args, qos_dev::AppTask::Run),
        Command::Test(args) => run_app_task(args, qos_dev::AppTask::Test),
        Command::Dev(args) => run_dev(args),
        Command::Shot(args) => run_shot(args),
        Command::Inspect(args) => run_inspect(args),
        Command::Ui(args) => run_ui(args),
        Command::Package(args) => run_package(args),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qos-dev: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run_scenario(args: ScenarioArgs) -> Result<()> {
    match args.command {
        ScenarioCommand::Run {
            name,
            app,
            viewport,
            artifacts,
            audit,
        } => {
            let start = app.map_or_else(std::env::current_dir, Ok)?;
            let plan = qos_dev::plan_scenario_suite(&qos_dev::ScenarioRunRequest {
                start,
                name,
                viewport,
                artifacts,
                audit,
            })?;
            execute_scenario_plan(&plan)
        }
    }
}

fn execute_scenario_plan(plan: &qos_dev::ScenarioSuitePlan) -> Result<()> {
    let outcome = plan.execute()?;
    for name in &outcome.passed {
        println!("✓ {name}");
    }
    for failure in &outcome.failed {
        println!("✗ {}: {}", failure.name, failure.detail);
    }
    if let Some(path) = plan.artifact_dir() {
        println!("Artifacts: {}", path.display());
    }
    if outcome.failed.is_empty() {
        println!("{} scenario(s) passed", outcome.passed.len());
        Ok(())
    } else {
        let total = outcome.passed.len().saturating_add(outcome.failed.len());
        anyhow::bail!("{} of {} scenario(s) failed", outcome.failed.len(), total)
    }
}

fn run_locale(args: LocaleArgs) -> Result<()> {
    match args.command {
        LocaleCommand::Sync {
            locale,
            prune,
            reset_variables,
            app,
            dry_run,
        } => {
            let start = app.map_or_else(std::env::current_dir, Ok)?;
            let outcome = qos_dev::sync_locale_catalogs(&qos_dev::LocaleSyncRequest {
                start,
                locale,
                prune,
                reset_variables,
                dry_run,
            })?;
            for path in &outcome.files {
                let shown = path.strip_prefix(&outcome.app_dir).unwrap_or(path);
                println!(
                    "{} {}",
                    if outcome.dry_run {
                        "Would synchronize"
                    } else {
                        "Synchronized"
                    },
                    shown.display()
                );
            }
            if outcome.checked.is_empty() {
                println!("No translation catalogs to synchronize");
            } else if outcome.files.is_empty() && outcome.unresolved.is_empty() {
                println!("Locale contracts already synchronized");
            } else if outcome.files.is_empty() {
                println!("No preservation-safe changes to apply");
            }
            for difference in &outcome.unresolved {
                eprintln!("Preserved: {difference}");
            }
            if !outcome.unresolved.is_empty() {
                eprintln!(
                    "Use --prune for stale units and --reset-variables for mismatched patterns"
                );
            }
            Ok(())
        }
    }
}

fn run_add(args: AddArgs) -> Result<()> {
    let Some(command) = args.command else {
        let start = std::env::current_dir()?;
        let outcome = qos_dev::add_app_artifact_interactive(&start)?;
        print_add_outcome(&outcome);
        return Ok(());
    };
    let (artifact, options) = match command {
        AddCommand::Screen {
            name,
            title,
            template,
            link_from,
            options,
        } => (
            qos_dev::AddArtifact::Screen {
                name,
                title,
                template: match template {
                    ScreenTemplateArg::Blank => qos_dev::ScreenTemplate::Blank,
                    ScreenTemplateArg::Form => qos_dev::ScreenTemplate::Form,
                    ScreenTemplateArg::Settings => qos_dev::ScreenTemplate::Settings,
                    ScreenTemplateArg::EmptyState => qos_dev::ScreenTemplate::EmptyState,
                },
                link_from,
            },
            options,
        ),
        AddCommand::Scenario {
            name,
            screen,
            options,
        } => (qos_dev::AddArtifact::Scenario { name, screen }, options),
        AddCommand::Locale { locale, options } => {
            (qos_dev::AddArtifact::Locale { locale }, options)
        }
        AddCommand::Component {
            name,
            template,
            mount,
            id,
            parent,
            index,
            props,
            options,
        } => {
            let template = match template {
                ComponentTemplateArg::Display => qos_dev::ComponentTemplate::Display,
                ComponentTemplateArg::Form => qos_dev::ComponentTemplate::Form,
                ComponentTemplateArg::Tree => qos_dev::ComponentTemplate::Tree,
                ComponentTemplateArg::List => qos_dev::ComponentTemplate::List,
                ComponentTemplateArg::Table => qos_dev::ComponentTemplate::Table,
                ComponentTemplateArg::Text => qos_dev::ComponentTemplate::Text,
            };
            let artifact = if let Some(screen) = mount {
                let mut mount = match template {
                    qos_dev::ComponentTemplate::Display => {
                        qos_dev::ComponentMount::display(screen, &name)
                    }
                    qos_dev::ComponentTemplate::Form => {
                        qos_dev::ComponentMount::form(screen, &name)
                    }
                    _ => anyhow::bail!(
                        "--mount supports --template display and --template form; tree, list, table, and text templates must be constructed from Rust"
                    ),
                };
                mount.id = id;
                mount.parent = parent.unwrap_or_else(|| "root".into());
                mount.index = index;
                if let Some(props) = props {
                    mount.props = props;
                }
                qos_dev::AddArtifact::GraphComponent {
                    name,
                    template,
                    mount,
                }
            } else {
                qos_dev::AddArtifact::Component { name, template }
            };
            (artifact, options)
        }
    };
    let start = options.app.map_or_else(std::env::current_dir, Ok)?;
    let outcome = qos_dev::add_app_artifact(qos_dev::AddRequest {
        start,
        artifact,
        force: options.force,
        dry_run: options.dry_run,
    })?;
    print_add_outcome(&outcome);
    Ok(())
}

fn print_add_outcome(outcome: &qos_dev::AddOutcome) {
    for path in &outcome.files {
        let updating = outcome.updated.contains(path);
        let verb = match (outcome.dry_run, updating) {
            (true, true) => "Would update",
            (true, false) => "Would create",
            (false, true) => "Updated",
            (false, false) => "Created",
        };
        let shown = path.strip_prefix(&outcome.app_dir).unwrap_or(path);
        println!("{verb} {}", shown.display());
    }
    if let Some(command) = &outcome.next_command {
        println!("Next: {command}");
    }
}

fn run_ui(args: UiArgs) -> Result<()> {
    match args.command {
        UiCommand::Edit { file, builder } => {
            let _ = qos_dev::load_ui_graph(&file)?;
            qos_dev::ui_builder_plan(&file, builder)?.execute()?;
        }
        UiCommand::Init { file, title, force } => {
            qos_dev::init_ui_graph(&file, &title, force)?;
            println!("Created {}", file.display());
        }
        UiCommand::Tree { file } => print!("{}", qos_dev::ui_graph_tree(&file)?),
        UiCommand::Validate { file } => {
            let document = qos_dev::load_ui_graph(&file)?;
            println!(
                "Valid UI graph: {} nodes (version {})",
                document.nodes().len(),
                document.version
            );
        }
        UiCommand::Format { file } => {
            qos_dev::format_ui_graph(&file)?;
            println!("Formatted {}", file.display());
        }
        UiCommand::Insert {
            file,
            parent,
            index,
            source,
        } => {
            let node = read_ui_node(source)?;
            qos_dev::edit_ui_graph(
                &file,
                qos_dev::UiGraphEdit::Insert {
                    parent,
                    index,
                    node,
                },
            )?;
            println!("Updated {}", file.display());
        }
        UiCommand::Component {
            file,
            name,
            id,
            parent,
            index,
            props,
        } => run_ui_component(
            &file,
            qos_dev::UiComponentInsert {
                name,
                id,
                parent,
                index,
                props,
            },
        )?,
        UiCommand::Remove { file, id } => {
            qos_dev::edit_ui_graph(&file, qos_dev::UiGraphEdit::Remove { id })?;
            println!("Updated {}", file.display());
        }
        UiCommand::Replace { file, id, source } => {
            let node = read_ui_node(source)?;
            qos_dev::edit_ui_graph(&file, qos_dev::UiGraphEdit::Replace { id, node })?;
            println!("Updated {}", file.display());
        }
        UiCommand::Move {
            file,
            id,
            parent,
            index,
        } => {
            qos_dev::edit_ui_graph(&file, qos_dev::UiGraphEdit::Move { id, parent, index })?;
            println!("Updated {}", file.display());
        }
        UiCommand::Set {
            file,
            id,
            property,
            edit,
        } => {
            let graph_edit = match (edit.value, edit.unset) {
                (Some(source), false) => qos_dev::UiGraphEdit::SetProperty {
                    id,
                    property,
                    value: parse_ui_property_value(source),
                },
                (None, true) => qos_dev::UiGraphEdit::UnsetProperty { id, property },
                _ => anyhow::bail!("choose exactly one of --value or --unset"),
            };
            qos_dev::edit_ui_graph(&file, graph_edit)?;
            println!("Updated {}", file.display());
        }
        UiCommand::Apply { file, edits } => run_ui_apply(&file, &edits)?,
    }
    Ok(())
}

fn run_ui_component(file: &Path, request: qos_dev::UiComponentInsert) -> Result<()> {
    let id = qos_dev::insert_ui_component(file, request)?;
    println!("Inserted {id} in {}", file.display());
    Ok(())
}

fn run_ui_apply(file: &Path, edits: &Path) -> Result<()> {
    let source = std::fs::read_to_string(edits)
        .with_context(|| format!("reading UI graph edits {}", edits.display()))?;
    let edits = qos_dev::parse_ui_graph_edits(&source)?;
    let count = edits.len();
    qos_dev::edit_ui_graph_batch(file, edits)?;
    println!("Applied {count} edits to {}", file.display());
    Ok(())
}

fn parse_ui_property_value(source: String) -> serde_json::Value {
    serde_json::from_str(&source).unwrap_or(serde_json::Value::String(source))
}

fn read_ui_node(source: NodeSourceArgs) -> Result<qos_ui_graph::UiNode> {
    let source = match (source.node, source.node_file) {
        (Some(source), None) => source,
        (None, Some(path)) => std::fs::read_to_string(&path)
            .map_err(anyhow::Error::from)
            .with_context(|| format!("reading UI node {}", path.display()))?,
        _ => anyhow::bail!("provide exactly one of --node or --node-file"),
    };
    qos_dev::parse_ui_node(&source)
}

fn run_package(args: PackageArgs) -> Result<()> {
    let path = args.path.map_or_else(std::env::current_dir, Ok)?;
    qos_dev::package_app(
        &path,
        &qos_dev::PackageRequest {
            output: args.output,
            identity: args.identity,
            profile: args.profile,
            target: args.target,
            features: args.features,
            no_default_features: args.no_default_features,
            force: args.force,
        },
    )?;
    Ok(())
}

fn run_dev(args: DevArgs) -> Result<()> {
    let path = args.path.map_or_else(std::env::current_dir, Ok)?;
    qos_dev::dev_app(
        &path,
        &qos_dev::DevRequest {
            forwarded: args.forwarded,
            watch_ui: !args.app_only,
            debounce: Duration::from_millis(args.debounce_ms.get()),
        },
    )
}

fn run_shot(args: ShotArgs) -> Result<()> {
    let path = args.path.map_or_else(std::env::current_dir, Ok)?;
    qos_dev::plan_app_shot(
        &path,
        qos_dev::ShotRequest {
            output: args.output,
            viewport: args.viewport,
            scenario: args.scenario,
            forwarded: args.forwarded,
        },
    )?
    .execute()
}

fn run_inspect(args: InspectArgs) -> Result<()> {
    let path = args.path.map_or_else(std::env::current_dir, Ok)?;
    qos_dev::plan_app_inspection(
        &path,
        qos_dev::InspectRequest {
            output: args.output,
            viewport: args.viewport,
            actions: args.actions,
            dump: args.dump,
            targets: args.targets,
            audit: args.audit,
            audit_report: args.audit_report,
        },
    )?
    .execute()
}

fn parse_viewport(raw: &str) -> std::result::Result<(u32, u32), String> {
    let (width, height) = raw
        .split_once('x')
        .or_else(|| raw.split_once('X'))
        .ok_or_else(|| "expected WIDTHxHEIGHT".to_owned())?;
    let width = width
        .parse::<u32>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| "width must be a non-zero u32".to_owned())?;
    let height = height
        .parse::<u32>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| "height must be a non-zero u32".to_owned())?;
    Ok((width, height))
}

fn run_app_task(args: AppTaskArgs, task: qos_dev::AppTask) -> Result<()> {
    let path = args.path.map_or_else(std::env::current_dir, Ok)?;
    qos_dev::plan_app_task(&path, task, &args.forwarded)?.execute()
}

fn run_doctor(args: DoctorArgs) -> Result<()> {
    let path = args.path.map_or_else(std::env::current_dir, Ok)?;
    let report = qos_dev::doctor_app(&path)?;
    println!("{}", report.summary());
    if !report.is_ready() {
        anyhow::bail!("app checks failed");
    }
    Ok(())
}

fn run_check(args: CheckArgs) -> Result<()> {
    let path = args.path.map_or_else(std::env::current_dir, Ok)?;
    println!("[check] app structure");
    let report = qos_dev::doctor_app(&path)?;
    println!("{}", report.summary());
    if !report.is_ready() {
        anyhow::bail!("app checks failed");
    }

    let format_mode = if args.fix {
        qos_dev::AppFormatMode::Apply
    } else {
        qos_dev::AppFormatMode::Check
    };
    let quality = qos_dev::plan_app_quality(&path, &args.forwarded, format_mode)?;
    if args.fix {
        println!("[check] Rust formatting (applying fixes)");
    } else {
        println!("[check] Rust formatting");
    }
    quality.format.execute()?;
    println!("[check] app-local Clippy");
    quality.clippy.execute()?;
    println!("[check] Rust tests and generated accessibility contract");
    quality.test.execute()?;

    println!("[check] qOS-native runtime certification");
    certify_one(&path)?;

    if !args.no_scenarios {
        println!("[check] isolated accessibility-audited scenarios");
        let plan = qos_dev::plan_scenario_suite(&qos_dev::ScenarioRunRequest {
            start: path,
            name: None,
            viewport: args.viewport,
            artifacts: args.artifacts,
            audit: true,
        })?;
        execute_scenario_plan(&plan)?;
    }
    println!("All app checks passed");
    Ok(())
}

fn certify_one(path: &Path) -> Result<()> {
    qos_dev::plan_app_task(
        path,
        qos_dev::AppTask::Run,
        &[OsString::from("--native-audit")],
    )?
    .execute()
    .context("production app root failed qOS-native certification")
}

#[allow(
    clippy::too_many_lines,
    reason = "workspace certification deliberately keeps discovery, one unified build, and execution in one auditable flow"
)]
fn run_certify(args: CertifyArgs) -> Result<()> {
    let path = args.path.map_or_else(std::env::current_dir, Ok)?;
    if !args.workspace {
        certify_one(&path)?;
        println!("qOS-native certification passed");
        return Ok(());
    }

    let apps = path.join("apps");
    anyhow::ensure!(
        apps.is_dir(),
        "{} is not a qOS workspace root",
        path.display()
    );
    let mut app_dirs = std::fs::read_dir(&apps)?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|dir| dir.join("Cargo.toml").is_file())
        // The agent is the headless consumer being used to drive apps; its
        // broker protocol is certified by the structural contract and tests.
        .filter(|dir| dir.file_name().is_none_or(|name| name != "agent"))
        .collect::<Vec<_>>();
    app_dirs.sort();
    let mut packages = Vec::with_capacity(app_dirs.len());
    for app in &app_dirs {
        let manifest_path = app.join("Cargo.toml");
        let manifest = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("reading {}", manifest_path.display()))?
            .parse::<toml::Value>()
            .with_context(|| format!("parsing {}", manifest_path.display()))?;
        let package = manifest
            .get("package")
            .and_then(toml::Value::as_table)
            .with_context(|| format!("{} has no [package] table", manifest_path.display()))?;
        let package_name = package
            .get("name")
            .and_then(toml::Value::as_str)
            .with_context(|| format!("{} has no package name", manifest_path.display()))?;
        let explicit_bins = manifest
            .get("bin")
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|binary| binary.get("name").and_then(toml::Value::as_str))
            .collect::<Vec<_>>();
        let binary_name = package
            .get("default-run")
            .and_then(toml::Value::as_str)
            .or(match explicit_bins.as_slice() {
                [binary] => Some(*binary),
                _ => None,
            })
            .unwrap_or(package_name);
        packages.push((app.clone(), package_name.to_owned(), binary_name.to_owned()));
    }

    // Build the complete application set in one Cargo graph. Apart from being
    // substantially faster, this keeps the SDK's feature set unified instead
    // of recompiling ui-core for every application's individual `cargo run`.
    println!(
        "[certify] building {} graphical applications",
        packages.len()
    );
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let mut build = ProcessCommand::new(cargo);
    build
        .current_dir(&path)
        .arg("build")
        .arg("--manifest-path")
        .arg(path.join("Cargo.toml"))
        .arg("--message-format=json-render-diagnostics");
    for (_, package, _) in &packages {
        build.arg("--package").arg(package);
    }
    let mut child = build
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("starting the workspace certification build")?;
    let stdout = child
        .stdout
        .take()
        .context("workspace certification build has no artifact stream")?;
    let mut executables = std::collections::HashMap::new();
    for line in BufReader::new(stdout).lines() {
        let line = line.context("reading the workspace certification build")?;
        let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if message.get("reason").and_then(serde_json::Value::as_str) != Some("compiler-artifact") {
            continue;
        }
        let Some(executable) = message
            .get("executable")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        let Some(name) = message
            .get("target")
            .and_then(|target| target.get("name"))
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        executables.insert(name.to_owned(), PathBuf::from(executable));
    }
    let status = child
        .wait()
        .context("waiting for the workspace certification build")?;
    anyhow::ensure!(status.success(), "workspace certification build failed");

    for (app, _, binary) in &packages {
        println!(
            "[certify] {}",
            app.file_name().unwrap_or_default().to_string_lossy()
        );
        let executable = executables.get(binary).with_context(|| {
            format!(
                "Cargo built no executable named {binary:?} for {}",
                app.display()
            )
        })?;
        let status = ProcessCommand::new(executable)
            .arg("--native-audit")
            .current_dir(app)
            .status()
            .with_context(|| format!("running {} --native-audit", executable.display()))?;
        anyhow::ensure!(
            status.success(),
            "{} production root failed qOS-native certification",
            app.display()
        );
    }
    println!("Certified {} graphical qOS applications", app_dirs.len());
    Ok(())
}

fn run_new(args: NewArgs) -> Result<()> {
    let start_dev = args.dev;
    let request = qos_dev::NewAppRequest {
        name: args.name,
        app_id: args.id,
        title: args.title,
        template: args.template.map(|template| match template {
            TemplateArg::Starter => qos_dev::Template::Starter,
            TemplateArg::Data => qos_dev::Template::Data,
            TemplateArg::List => qos_dev::Template::List,
            TemplateArg::Tree => qos_dev::Template::Tree,
            TemplateArg::Graph => qos_dev::Template::Graph,
            TemplateArg::Async => qos_dev::Template::Async,
            TemplateArg::Document => qos_dev::Template::Document,
            TemplateArg::Qr => qos_dev::Template::Qr,
            TemplateArg::Workspace => qos_dev::Template::Workspace,
        }),
        directory: args.directory,
        ui_path: args.ui_path,
        dry_run: args.dry_run,
    };
    let outcome = qos_dev::create_app_interactive(request)?;
    if start_dev {
        println!("{}", outcome.headline());
        println!("\nStarting live development; press Ctrl-C to stop.");
        qos_dev::dev_app(
            &outcome.destination,
            &qos_dev::DevRequest {
                forwarded: Vec::new(),
                watch_ui: true,
                debounce: Duration::from_millis(150),
            },
        )
    } else {
        println!("{}", outcome.summary());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_exposes_the_sparse_list_template() {
        let cli = Cli::try_parse_from(["qos-dev", "new", "activity-feed", "--template", "list"])
            .expect("list template command");
        let Command::New(args) = cli.command else {
            panic!("expected new command");
        };
        assert!(matches!(args.template, Some(TemplateArg::List)));
    }

    #[test]
    fn new_exposes_the_typed_async_template() {
        let cli = Cli::try_parse_from(["qos-dev", "new", "repo-scan", "--template", "async"])
            .expect("async template command");
        let Command::New(args) = cli.command else {
            panic!("expected new command");
        };
        assert!(matches!(args.template, Some(TemplateArg::Async)));
    }

    #[test]
    fn new_exposes_the_durable_document_template() {
        let cli = Cli::try_parse_from(["qos-dev", "new", "notes", "--template", "document"])
            .expect("document template command");
        let Command::New(args) = cli.command else {
            panic!("expected new command");
        };
        assert!(matches!(args.template, Some(TemplateArg::Document)));
    }

    #[test]
    fn new_exposes_the_accessible_qr_template() {
        let cli = Cli::try_parse_from(["qos-dev", "new", "qr-code", "--template", "qr"])
            .expect("QR template command");
        let Command::New(args) = cli.command else {
            panic!("expected new command");
        };
        assert!(matches!(args.template, Some(TemplateArg::Qr)));
    }

    #[test]
    fn new_exposes_the_professional_workspace_template() {
        let cli = Cli::try_parse_from([
            "qos-dev",
            "new",
            "project-studio",
            "--template",
            "workspace",
        ])
        .expect("workspace template command");
        let Command::New(args) = cli.command else {
            panic!("expected new command");
        };
        assert!(matches!(args.template, Some(TemplateArg::Workspace)));
    }

    #[test]
    fn new_can_enter_dev_but_cannot_dev_a_dry_run() {
        let cli = Cli::try_parse_from(["qos-dev", "new", "notes", "--dev"])
            .expect("create-and-develop command");
        let Command::New(args) = cli.command else {
            panic!("expected new command");
        };
        assert!(args.dev);
        assert!(!args.dry_run);

        assert!(Cli::try_parse_from(["qos-dev", "new", "notes", "--dev", "--dry-run"]).is_err());
    }

    #[test]
    fn add_commands_expose_safe_repeatable_generation_options() {
        let screen = Cli::try_parse_from([
            "qos-dev",
            "add",
            "screen",
            "account-settings",
            "--title",
            "Account",
            "--template",
            "settings",
            "--link-from",
            "main",
            "--app",
            "apps/demo/src",
            "--dry-run",
        ])
        .expect("screen command");
        let Command::Add(AddArgs {
            command:
                Some(AddCommand::Screen {
                    name,
                    title,
                    template,
                    link_from,
                    options,
                }),
        }) = screen.command
        else {
            panic!("expected add screen command");
        };
        assert_eq!(name, "account-settings");
        assert_eq!(title.as_deref(), Some("Account"));
        assert!(matches!(template, ScreenTemplateArg::Settings));
        assert_eq!(link_from.as_deref(), Some("main"));
        assert_eq!(options.app, Some(PathBuf::from("apps/demo/src")));
        assert!(options.dry_run);
        assert!(!options.force);

        let component = Cli::try_parse_from([
            "qos-dev",
            "add",
            "component",
            "profile-card",
            "--template",
            "form",
            "--app",
            "apps/demo",
            "--dry-run",
        ])
        .expect("component command");
        let Command::Add(AddArgs {
            command:
                Some(AddCommand::Component {
                    name,
                    template,
                    options,
                    ..
                }),
        }) = component.command
        else {
            panic!("expected add component command");
        };
        assert_eq!(name, "profile-card");
        assert!(matches!(template, ComponentTemplateArg::Form));
        assert_eq!(options.app, Some(PathBuf::from("apps/demo")));
        assert!(options.dry_run);

        let tree_component = Cli::try_parse_from([
            "qos-dev",
            "add",
            "component",
            "project-tree",
            "--template",
            "tree",
        ])
        .expect("tree component command");
        assert!(matches!(
            tree_component.command,
            Command::Add(AddArgs {
                command: Some(AddCommand::Component {
                    template: ComponentTemplateArg::Tree,
                    ..
                })
            })
        ));

        let locale = Cli::try_parse_from(["qos-dev", "add", "locale", "de-DE", "--force"])
            .expect("locale command");
        let Command::Add(AddArgs {
            command: Some(AddCommand::Locale { locale, options }),
        }) = locale.command
        else {
            panic!("expected add locale command");
        };
        assert_eq!(locale, "de-DE");
        assert!(options.force);

        let wizard = Cli::try_parse_from(["qos-dev", "add"]).expect("add wizard command");
        let Command::Add(AddArgs { command: None }) = wizard.command else {
            panic!("expected interactive add command");
        };
    }

    #[test]
    fn add_component_mount_options_are_explicit_and_scoped() {
        let mounted = Cli::try_parse_from([
            "qos-dev",
            "add",
            "component",
            "activity-card",
            "--mount",
            "main",
            "--id",
            "summary",
            "--parent",
            "content",
            "--index",
            "1",
            "--props",
            r#"{"title":"Recent","detail":"Ready"}"#,
        ])
        .expect("mounted component command");
        assert!(matches!(
            mounted.command,
            Command::Add(AddArgs {
                command: Some(AddCommand::Component {
                    mount: Some(screen),
                    id: Some(id),
                    parent: Some(parent),
                    index: Some(1),
                    props: Some(_),
                    ..
                })
            }) if screen == "main" && id == "summary" && parent == "content"
        ));
        assert!(
            Cli::try_parse_from([
                "qos-dev",
                "add",
                "component",
                "activity-card",
                "--parent",
                "content",
            ])
            .is_err()
        );
    }

    #[test]
    fn add_command_exposes_the_reusable_list_template() {
        let cli = Cli::try_parse_from([
            "qos-dev",
            "add",
            "component",
            "activity-list",
            "--template",
            "list",
        ])
        .expect("list component command");

        assert!(matches!(
            cli.command,
            Command::Add(AddArgs {
                command: Some(AddCommand::Component {
                    template: ComponentTemplateArg::List,
                    ..
                })
            })
        ));
    }

    #[test]
    fn add_command_exposes_the_reusable_table_template() {
        let cli = Cli::try_parse_from([
            "qos-dev",
            "add",
            "component",
            "results-table",
            "--template",
            "table",
        ])
        .expect("table component command");

        assert!(matches!(
            cli.command,
            Command::Add(AddArgs {
                command: Some(AddCommand::Component {
                    template: ComponentTemplateArg::Table,
                    ..
                })
            })
        ));
    }

    #[test]
    fn add_command_exposes_the_advanced_text_template() {
        let cli = Cli::try_parse_from([
            "qos-dev",
            "add",
            "component",
            "article-body",
            "--template",
            "text",
        ])
        .expect("text component command");

        assert!(matches!(
            cli.command,
            Command::Add(AddArgs {
                command: Some(AddCommand::Component {
                    template: ComponentTemplateArg::Text,
                    ..
                })
            })
        ));
    }

    #[test]
    fn locale_sync_exposes_preservation_and_explicit_repair_options() {
        let cli = Cli::try_parse_from([
            "qos-dev",
            "locale",
            "sync",
            "de-DE",
            "--prune",
            "--reset-variables",
            "--app",
            "apps/demo/src",
            "--dry-run",
        ])
        .expect("locale sync command");
        let Command::Locale(LocaleArgs {
            command:
                LocaleCommand::Sync {
                    locale,
                    prune,
                    reset_variables,
                    app,
                    dry_run,
                },
        }) = cli.command
        else {
            panic!("expected locale sync command");
        };
        assert_eq!(locale.as_deref(), Some("de-DE"));
        assert!(prune);
        assert!(reset_variables);
        assert_eq!(app, Some(PathBuf::from("apps/demo/src")));
        assert!(dry_run);
    }

    #[test]
    fn run_and_test_accept_a_descendant_path_and_forwarded_arguments() {
        let run = Cli::try_parse_from([
            "qos-dev",
            "run",
            "apps/demo/src",
            "--",
            "--shots",
            "preview.png",
        ])
        .expect("run CLI");
        let Command::Run(run) = run.command else {
            panic!("expected run command");
        };
        assert_eq!(run.path, Some(PathBuf::from("apps/demo/src")));
        assert_eq!(
            run.forwarded,
            [OsString::from("--shots"), OsString::from("preview.png")]
        );

        let test = Cli::try_parse_from(["qos-dev", "test", "--", "--nocapture"]).expect("test CLI");
        let Command::Test(test) = test.command else {
            panic!("expected test command");
        };
        assert_eq!(test.path, None);
        assert_eq!(test.forwarded, [OsString::from("--nocapture")]);
    }

    #[test]
    fn certify_accepts_one_app_or_a_workspace_root() {
        let app = Cli::try_parse_from(["qos-dev", "certify", "apps/demo/src"])
            .expect("single-app certification CLI");
        let Command::Certify(app) = app.command else {
            panic!("expected certify command");
        };
        assert_eq!(app.path, Some(PathBuf::from("apps/demo/src")));
        assert!(!app.workspace);

        let workspace = Cli::try_parse_from(["qos-dev", "certify", ".", "--workspace"])
            .expect("workspace certification CLI");
        let Command::Certify(workspace) = workspace.command else {
            panic!("expected certify command");
        };
        assert_eq!(workspace.path, Some(PathBuf::from(".")));
        assert!(workspace.workspace);
    }

    #[test]
    fn shot_and_inspect_expose_direct_artifact_workflows() {
        let shot = Cli::try_parse_from([
            "qos-dev",
            "shot",
            "apps/demo/src",
            "--output",
            "dark.png",
            "--viewport",
            "1440x900",
            "--scenario",
            "populated",
            "--",
            "--theme",
            "dark",
        ])
        .expect("shot CLI");
        let Command::Shot(shot) = shot.command else {
            panic!("expected shot command");
        };
        assert_eq!(shot.path, Some(PathBuf::from("apps/demo/src")));
        assert_eq!(shot.output, PathBuf::from("dark.png"));
        assert_eq!(shot.viewport, Some((1440, 900)));
        assert_eq!(shot.scenario.as_deref(), Some("populated"));
        assert_eq!(
            shot.forwarded,
            [OsString::from("--theme"), OsString::from("dark")]
        );

        let inspect = Cli::try_parse_from([
            "qos-dev",
            "inspect",
            "apps/demo/src/lib.rs",
            "--viewport",
            "1280X800",
            "--actions",
            "actions.json",
            "--dump",
            "state.json",
            "--targets",
            "targets.json",
            "--audit",
            "--audit-report",
            "audit.json",
        ])
        .expect("inspect CLI");
        let Command::Inspect(inspect) = inspect.command else {
            panic!("expected inspect command");
        };
        assert_eq!(inspect.viewport, Some((1280, 800)));
        assert_eq!(inspect.output, PathBuf::from("inspect.json"));
        assert_eq!(inspect.actions, Some(PathBuf::from("actions.json")));
        assert_eq!(inspect.dump, Some(PathBuf::from("state.json")));
        assert_eq!(inspect.targets, Some(PathBuf::from("targets.json")));
        assert!(inspect.audit);
        assert_eq!(inspect.audit_report, Some(PathBuf::from("audit.json")));

        assert!(Cli::try_parse_from(["qos-dev", "inspect", "--viewport", "1280x0"]).is_err());
    }

    #[test]
    fn scenario_run_selects_a_case_and_viewport() {
        let cli = Cli::try_parse_from([
            "qos-dev",
            "scenario",
            "run",
            "empty-state",
            "--viewport",
            "1024x768",
            "--app",
            "apps/demo/src",
            "--artifacts",
            "target/scenarios",
            "--audit",
        ])
        .expect("scenario CLI");
        let Command::Scenario(ScenarioArgs {
            command:
                ScenarioCommand::Run {
                    name,
                    app,
                    viewport,
                    artifacts,
                    audit,
                },
        }) = cli.command
        else {
            panic!("expected scenario run command");
        };
        assert_eq!(name.as_deref(), Some("empty-state"));
        assert_eq!(app, Some(PathBuf::from("apps/demo/src")));
        assert_eq!(viewport, Some((1024, 768)));
        assert_eq!(artifacts, Some(PathBuf::from("target/scenarios")));
        assert!(audit);
    }

    #[test]
    fn check_composes_tests_audited_scenarios_and_artifacts() {
        let cli = Cli::try_parse_from([
            "qos-dev",
            "check",
            "apps/demo/src",
            "--viewport",
            "1280x720",
            "--artifacts",
            "target/check",
            "--fix",
            "--",
            "--nocapture",
        ])
        .expect("check CLI");
        let Command::Check(args) = cli.command else {
            panic!("expected check command");
        };
        assert_eq!(args.path, Some(PathBuf::from("apps/demo/src")));
        assert_eq!(args.viewport, Some((1280, 720)));
        assert_eq!(args.artifacts, Some(PathBuf::from("target/check")));
        assert!(!args.no_scenarios);
        assert!(args.fix);
        assert_eq!(args.forwarded, [OsString::from("--nocapture")]);
    }

    #[test]
    fn dev_exposes_a_bounded_restart_loop() {
        let cli = Cli::try_parse_from([
            "qos-dev",
            "dev",
            "apps/demo/src",
            "--debounce-ms",
            "75",
            "--app-only",
            "--",
            "--document",
            "sample.qdoc",
        ])
        .expect("dev CLI");
        let Command::Dev(dev) = cli.command else {
            panic!("expected dev command");
        };
        assert_eq!(dev.path, Some(PathBuf::from("apps/demo/src")));
        assert_eq!(dev.debounce_ms.get(), 75);
        assert!(dev.app_only);
        assert_eq!(
            dev.forwarded,
            [OsString::from("--document"), OsString::from("sample.qdoc")]
        );
        assert!(Cli::try_parse_from(["qos-dev", "dev", "--debounce-ms", "0"]).is_err());
    }

    #[test]
    fn package_exposes_reproducible_build_and_signing_options() {
        let cli = Cli::try_parse_from([
            "qos-dev",
            "package",
            "apps/demo/src",
            "--output",
            "dist/demo-dev.qpkg",
            "--identity",
            "publisher.json",
            "--profile",
            "distribution",
            "--target",
            "x86_64-unknown-linux-gnu",
            "--features",
            "sqlite,telemetry",
            "--no-default-features",
            "--force",
        ])
        .expect("package CLI");
        let Command::Package(package) = cli.command else {
            panic!("expected package command");
        };
        assert_eq!(package.path, Some(PathBuf::from("apps/demo/src")));
        assert_eq!(package.output, Some(PathBuf::from("dist/demo-dev.qpkg")));
        assert_eq!(package.identity, Some(PathBuf::from("publisher.json")));
        assert_eq!(package.profile, "distribution");
        assert_eq!(package.target.as_deref(), Some("x86_64-unknown-linux-gnu"));
        assert_eq!(package.features, ["sqlite", "telemetry"]);
        assert!(package.no_default_features);
        assert!(package.force);

        let defaults = Cli::try_parse_from(["qos-dev", "package"]).expect("package defaults");
        let Command::Package(defaults) = defaults.command else {
            panic!("expected package command");
        };
        assert_eq!(defaults.profile, "release");
        assert!(!defaults.force);
    }

    #[test]
    fn ui_builder_commands_require_unambiguous_node_sources() {
        let edit = Cli::try_parse_from([
            "qos-dev",
            "ui",
            "edit",
            "ui/main.json",
            "--builder",
            "/opt/qos-ui-builder",
        ])
        .expect("UI edit command");
        let Command::Ui(UiArgs {
            command: UiCommand::Edit { file, builder },
        }) = edit.command
        else {
            panic!("expected UI edit command");
        };
        assert_eq!(file, PathBuf::from("ui/main.json"));
        assert_eq!(builder, Some(OsString::from("/opt/qos-ui-builder")));

        let insert = Cli::try_parse_from([
            "qos-dev",
            "ui",
            "insert",
            "ui/main.json",
            "--parent",
            "root",
            "--index",
            "2",
            "--node",
            r#"{"type":"spacer","id":"gap"}"#,
        ])
        .expect("UI insert command");
        let Command::Ui(UiArgs {
            command:
                UiCommand::Insert {
                    parent,
                    index,
                    source,
                    ..
                },
        }) = insert.command
        else {
            panic!("expected UI insert command");
        };
        assert_eq!(parent, "root");
        assert_eq!(index, 2);
        assert!(source.node.is_some());
        assert!(source.node_file.is_none());

        let component = Cli::try_parse_from([
            "qos-dev",
            "ui",
            "component",
            "ui/main.json",
            "profile-card",
            "--props",
            r#"{"title":"Ada","detail":"Owner"}"#,
        ])
        .expect("UI component command");
        let Command::Ui(UiArgs {
            command:
                UiCommand::Component {
                    file,
                    name,
                    id,
                    parent,
                    index,
                    props,
                },
        }) = component.command
        else {
            panic!("expected UI component command");
        };
        assert_eq!(file, PathBuf::from("ui/main.json"));
        assert_eq!(name, "profile-card");
        assert_eq!(id, None);
        assert_eq!(parent, "root");
        assert_eq!(index, None);
        assert!(props.contains("Ada"));

        assert!(
            Cli::try_parse_from(["qos-dev", "ui", "replace", "ui/main.json", "--id", "body",])
                .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "qos-dev",
                "ui",
                "insert",
                "ui/main.json",
                "--parent",
                "root",
                "--index",
                "0",
                "--node",
                r#"{"type":"spacer","id":"a"}"#,
                "--node-file",
                "node.json",
            ])
            .is_err()
        );
    }

    #[test]
    fn ui_set_accepts_one_typed_property_edit() {
        let set = Cli::try_parse_from([
            "qos-dev",
            "ui",
            "set",
            "ui/main.json",
            "--id",
            "title",
            "--property",
            "text.value",
            "--value",
            "Recent projects",
        ])
        .expect("UI property command");
        assert!(matches!(
            set.command,
            Command::Ui(UiArgs {
                command: UiCommand::Set {
                    edit: PropertyEditArgs {
                        value: Some(_),
                        unset: false,
                    },
                    ..
                }
            })
        ));

        assert!(
            Cli::try_parse_from([
                "qos-dev",
                "ui",
                "set",
                "ui/main.json",
                "--id",
                "title",
                "--property",
                "size",
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "qos-dev",
                "ui",
                "set",
                "ui/main.json",
                "--id",
                "title",
                "--property",
                "size",
                "--value",
                "24",
                "--unset",
            ])
            .is_err()
        );
        assert_eq!(
            parse_ui_property_value("true".into()),
            serde_json::json!(true)
        );
        assert_eq!(parse_ui_property_value("24".into()), serde_json::json!(24));
        assert_eq!(
            parse_ui_property_value("Recent projects".into()),
            serde_json::json!("Recent projects")
        );

        let apply =
            Cli::try_parse_from(["qos-dev", "ui", "apply", "ui/main.json", "ui/edits.json"])
                .expect("UI batch command");
        assert!(matches!(
            apply.command,
            Command::Ui(UiArgs {
                command: UiCommand::Apply { file, edits }
            }) if file.as_path() == std::path::Path::new("ui/main.json")
                && edits.as_path() == std::path::Path::new("ui/edits.json")
        ));
    }

    #[test]
    fn ui_apply_reads_and_publishes_one_complete_batch() {
        let temp = tempfile::tempdir().expect("temp dir");
        let graph = temp.path().join("main.json");
        let edits = temp.path().join("edits.json");
        qos_dev::init_ui_graph(&graph, "Projects", false).unwrap();
        std::fs::write(
            &edits,
            r#"[
              {"op":"set-property","id":"title","property":"text.value","value":"Recent projects"},
              {"op":"insert","parent":"root","index":2,"node":{"type":"button","id":"open","label":"Open"}}
            ]"#,
        )
        .unwrap();

        run_ui(UiArgs {
            command: UiCommand::Apply {
                file: graph.clone(),
                edits,
            },
        })
        .expect("apply batch through CLI handler");

        let document = qos_dev::load_ui_graph(&graph).unwrap();
        assert!(document.node("open").is_some());
        assert!(
            std::fs::read_to_string(graph)
                .unwrap()
                .contains("Recent projects")
        );
    }
}
