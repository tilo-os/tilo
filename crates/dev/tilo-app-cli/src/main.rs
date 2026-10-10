use std::ffi::OsString;
use std::io::{BufRead as _, BufReader};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, ExitCode, Stdio};
use std::time::Duration;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::{Value, json};

mod guide;
mod json_stdout;
mod publishing;

#[derive(Parser)]
#[command(
    name = "tilo-app",
    version,
    about = "Create and work on Tilo apps",
    long_about = "The short path from an app idea to a running Tilo window.\n\n\
                  Start with `tilo-app guide` for short offline workflows.\n\n\
                  Run `tilo-app new` for an interactive setup, or provide a name \
                  and flags for a reproducible, non-interactive scaffold. Then \
                  use `add`, `locale`, `check`, `scenario`, `dev`, `test`, `run`, `build`, `shot`, `inspect`, and `package` from anywhere inside the app; `--target linux|macos|web` picks the platform. Use `ui` to construct validated serialized screens."
)]
struct Cli {
    /// Write one JSON document describing the result to stdout. Progress and
    /// tool output move to stderr, and nothing prompts.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Read one short offline workflow, or list the available topics.
    Guide {
        #[arg(value_enum)]
        topic: Option<guide::Topic>,
    },
    /// Create a buildable app with packaging and headless-test defaults.
    New(NewArgs),
    /// List examples, inspect one, or read its embedded source without a checkout.
    Examples {
        /// One example from `tilo-app examples`.
        name: Option<String>,
        /// Read an embedded file, such as README.md, src/lib.rs or tests/app.rs.
        #[arg(long, requires = "name")]
        file: Option<String>,
    },
    /// Add a screen, reusable component, headless scenario, or translation catalog.
    Add(AddArgs),
    /// Keep translation catalogs structurally aligned with the source locale.
    Locale(LocaleArgs),
    /// Run saved headless interaction scenarios against isolated app state.
    Scenario(ScenarioArgs),
    /// Diagnose app source, manifest, lifecycle, and toolkit wiring
    /// (`--platform android`: the Android toolchain).
    Doctor(DoctorArgs),
    /// Install the toolchain a platform's builds need (`setup android`).
    Setup(SetupArgs),
    /// Run lints, structural checks, tests, and accessibility-audited scenarios.
    #[command(after_long_help = CHECK_EXAMPLES)]
    Check(CheckArgs),
    /// Prove the production root satisfies the live Tilo-native contract.
    Certify(CertifyArgs),
    /// Write the app's public surface (its verbs and stored fields) to
    /// `surface.toml`, which `check` and CI compare with the code.
    Surface(SurfaceArgs),
    /// Run an app from any path inside it: natively after a structural
    /// preflight, or with `--target` for Linux, macOS or the web.
    #[command(after_long_help = TARGET_EXAMPLES)]
    Run(RunArgs),
    /// Build the runnable artifact for a target: a binary, a dev `.app`, or a web bundle.
    #[command(after_long_help = TARGET_EXAMPLES)]
    Build(BuildArgs),
    /// Assemble an unsigned macOS bundle from existing binaries, without building or signing.
    BundleMacos(BundleMacosArgs),
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
    /// Build, assemble, sign, and verify an installable Tilo package.
    Package(PackageArgs),
    /// Capture and seal a reusable compiler interface for an exact native SDK.
    Sdk(SdkArgs),
    /// Create or show the publisher key that signs packages and entries.
    Keys(publishing::KeysArgs),
    /// Ask a registry to let your key publish under namespaces you prove.
    Apply(publishing::ApplyArgs),
    /// Upload packages and ask the registry to list them.
    Publish(publishing::PublishArgs),
}

#[derive(clap::Args)]
struct SdkArgs {
    #[command(subcommand)]
    command: SdkCommand,
}

#[derive(Subcommand)]
enum SdkCommand {
    /// Read the canonical source SDK resource declarations without running app code.
    Declarations,
    /// Capture metadata from the actual canonical Cargo artifact stream (on its builder).
    Capture {
        #[arg(long)]
        units: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    /// Sign a captured compiler interface bound to an independently verified runtime.
    Export {
        #[arg(long)]
        capture: PathBuf,
        #[arg(long)]
        sdk_bundle: PathBuf,
        #[arg(long)]
        sdk_publisher: String,
        #[arg(long)]
        sdk_root: String,
        #[arg(long)]
        identity: PathBuf,
        #[arg(long)]
        bundle_tool: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
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
    /// Declare a host capability in tilo-app.toml (optional unless --required).
    Capability {
        /// Capability name, for example `network.http` or `files.save`.
        #[arg(required_unless_present = "list")]
        name: Option<String>,
        /// The app cannot work without it; every platform must provide it.
        #[arg(long)]
        required: bool,
        /// Print every capability and where it is available.
        #[arg(long, conflicts_with_all = ["name", "required"])]
        list: bool,
        /// App directory or a file/directory inside it. Defaults to the current directory.
        #[arg(long)]
        app: Option<PathBuf>,
    },
    /// Add a workflow that checks, packages and publishes the app from CI.
    Ci {
        /// The forge the repository is hosted on; Codeberg is forgejo.
        #[arg(long, value_enum)]
        forge: ForgeArg,
        /// Linux runner label. Defaults to `ubuntu-latest`, or `docker` on
        /// Forgejo (`codeberg-small` on Codeberg).
        #[arg(long)]
        runs_on: Option<String>,
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
        /// Override the tilo-ui-builder executable.
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

    /// Check the toolchain this platform's builds need instead of an app.
    #[arg(long, value_enum)]
    platform: Option<ToolchainArg>,
}

/// A platform with a toolchain of its own to check and install.
#[derive(Clone, Copy, clap::ValueEnum)]
enum ToolchainArg {
    Android,
}

#[derive(clap::Args)]
struct SetupArgs {
    /// The platform whose toolchain to install.
    #[arg(value_enum)]
    platform: ToolchainArg,

    /// Accept the Android SDK licences without showing each one.
    #[arg(long)]
    accept_licenses: bool,
}

const CHECK_EXAMPLES: &str = "\
Lints:
  portability  Apps call portable APIs; platform code lives in platform crates
               (crates/platform). The rules are in
               tools/policy/app_portability_rules.toml: target cfg (R1), std::os,
               extern blocks and native binding crates (R2), platform paths and
               environment (R3), OS tools (R4), platform and internal dependencies
               (D1, D2), SDK backend features (F1), and the calls that fail on the
               web: threads, std clocks, std fs/net (W1-W3, warnings for now).
               tests/ and #[cfg(test)] are exempt. An inherently
               platform-specific app narrows its platforms once in tilo-app.toml:

                 [package]
                 platforms = [\"linux\", \"macos\"]
                 platform-reason = \"Writes OS images to block devices.\"

Examples:
  tilo-app check                     Everything, for the app in the current directory
  tilo-app check --lint apps/notes   Only the lints, without building
  tilo-app check --lint --all        The lints for every Tilo app in the workspace (CI)";

#[derive(clap::Args)]
struct LintScope {
    /// Run only the lints (`portability` and the app contract); nothing is
    /// built or run.
    #[arg(long)]
    lint: bool,

    /// Lint every Tilo app in the workspace containing PATH (or the current directory).
    #[arg(long, requires = "lint")]
    all: bool,
}

#[derive(clap::Args)]
struct CheckArgs {
    /// App directory or a file/directory inside it. Defaults to the current directory.
    path: Option<PathBuf>,

    #[command(flatten)]
    lints: LintScope,

    /// Only check that the app builds for these targets: declared platforms,
    /// required capabilities, then `cargo check` where this host can compile.
    #[arg(long, value_enum, value_delimiter = ',', conflicts_with = "all")]
    target: Vec<TargetArg>,

    /// With --target: accept required capabilities whose backend is only planned.
    #[arg(long, requires = "target")]
    allow_planned: bool,

    /// Also lint the app for another platform's target triple, or prove the
    /// web build; repeat it or pass `all`. The host build is always checked.
    #[arg(long = "platform", value_enum)]
    platforms: Vec<CheckPlatformArg>,

    /// CPU architecture for `--platform`. Defaults to the host's.
    #[arg(long, value_enum)]
    arch: Option<ArchArg>,

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
struct SurfaceArgs {
    /// App directory or a file/directory inside it. Defaults to the current directory.
    path: Option<PathBuf>,

    /// Write the surface from a report the app's binary printed for
    /// `--surface` (on another machine, a CI artifact) instead of building it.
    #[arg(long)]
    report: Option<PathBuf>,
}

#[derive(clap::Args)]
struct CertifyArgs {
    /// App directory or a file/directory inside it. Defaults to the current directory.
    path: Option<PathBuf>,

    /// Certify every graphical application in this Tilo workspace.
    #[arg(long)]
    workspace: bool,

    /// After workspace certification, render every graphical app at the
    /// supported phone, tablet, laptop, and desktop sizes in default,
    /// expanded-copy, 200%-text, and high-contrast modes.
    #[arg(long, requires = "workspace", value_name = "DIR")]
    responsive_artifacts: Option<PathBuf>,

    /// With `--workspace`, certify only the applications of these Cargo
    /// packages; packages that are no workspace application are ignored.
    #[arg(
        long = "package",
        short = 'p',
        requires = "workspace",
        value_name = "SPEC"
    )]
    packages: Vec<String>,
}

#[derive(clap::Args)]
struct AppTaskArgs {
    /// App directory or a file/directory inside it. Defaults to the current directory.
    path: Option<PathBuf>,

    /// Arguments forwarded to the app or test harness. Write them after `--`.
    #[arg(last = true, allow_hyphen_values = true)]
    forwarded: Vec<OsString>,
}

const TARGET_EXAMPLES: &str = "\
Targets:
  linux   a native binary; `package` makes a signed .tapp
  macos   an ad-hoc-signed .app; `package` signs (TILO_MAC_SIGNING_IDENTITY),
          notarizes (TILO_NOTARY_KEYCHAIN_PROFILE) and zips it, with the
          signing descriptor `tilo-app-package entry --platform macos` reads
  web     a bundle for the browser runtime; `run` serves it on 127.0.0.1:7878
          and reloads the page after each rebuild; `package` writes the
          immutable versions/<digest>/ layout Cloud serves and its registry
          zip (`tilo-app-package entry --platform web`)
  android what `--platform android` builds: a debug APK; `run` and `dev`
          install it on a device (--device); `package` signs an APK and bundle

A target must be in the manifest's `package.platforms`, and every `required`
capability must be available there (`tilo-app add capability --list`).

Examples:
  tilo-app run --target web            Serve, rebuild on change, reload the page
  tilo-app build --target macos        target/tilo-app/macos/<app>/<Name>.app
  tilo-app package --target web        dist/web/<app-id>/versions/<digest>/";

#[derive(Clone, Copy, Debug, ValueEnum)]
enum TargetArg {
    Linux,
    Macos,
    Web,
    /// An APK for a device or emulator, as `--platform android` builds it.
    Android,
}

impl TargetArg {
    /// The SDK target; `None` for Android, which builds through the
    /// `--platform android` flow.
    fn sdk(self) -> Option<tilo_app_cli::targets::Target> {
        match self {
            Self::Linux => Some(tilo_app_cli::targets::Target::Linux),
            Self::Macos => Some(tilo_app_cli::targets::Target::Macos),
            Self::Web => Some(tilo_app_cli::targets::Target::Web),
            Self::Android => None,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum BundleArg {
    /// The desktop bundle for Tilo's own web apps, otherwise the app alone.
    Auto,
    /// The page runtime with only this app.
    App,
    /// Every web-capable Tilo app (inside a Tilo checkout).
    Desktop,
}

#[derive(clap::Args)]
struct TargetOptions {
    /// Optimized build.
    #[arg(long)]
    release: bool,
    /// Web only: which bundle to build.
    #[arg(long, value_enum, default_value = "auto")]
    bundle: BundleArg,
    /// Build even when a required capability's backend is only planned.
    #[arg(long)]
    allow_planned: bool,
    /// Comma-separated Cargo features enabled for the app.
    #[arg(long, value_delimiter = ',')]
    features: Vec<String>,
    /// Disable the app's default Cargo features.
    #[arg(long)]
    no_default_features: bool,
}

impl TargetOptions {
    fn request(
        &self,
        target: tilo_app_cli::targets::Target,
    ) -> tilo_app_cli::targets::TargetRequest {
        let mut request = tilo_app_cli::targets::TargetRequest::new(target);
        request.release = self.release;
        request.allow_planned = self.allow_planned;
        request.bundle = match self.bundle {
            BundleArg::Auto => tilo_app_cli::targets::WebBundle::Auto,
            BundleArg::App => tilo_app_cli::targets::WebBundle::App,
            BundleArg::Desktop => tilo_app_cli::targets::WebBundle::Desktop,
        };
        request.features.clone_from(&self.features);
        request.no_default_features = self.no_default_features;
        request
    }
}

/// The package platform and architecture a command builds for, the other
/// way to name what `--target` names.
#[derive(Clone, Copy, clap::Args)]
struct PlatformArgs {
    /// The system to build for: Tilo OS (the default), another Linux
    /// desktop, Windows, macOS, the web, or Android.
    #[arg(long, value_enum)]
    platform: Option<PlatformArg>,

    /// CPU architecture to build for. Defaults to the host's; `wasm32` is the
    /// web's only one. On Android, the device's for `run` and `dev`, else
    /// `aarch64`.
    #[arg(long, value_enum)]
    arch: Option<ArchArg>,
}

impl PlatformArgs {
    fn resolve(
        self,
    ) -> Result<(
        tilo_app_cli::PackagePlatform,
        Option<tilo_app_cli::PackageArch>,
    )> {
        platform_and_arch(self.platform.unwrap_or(PlatformArg::Tilo), self.arch)
    }

    /// `--platform android` for `--target android`, which names the same build.
    fn or_target(mut self, target: Option<TargetArg>) -> Self {
        if matches!(target, Some(TargetArg::Android)) {
            self.platform = Some(PlatformArg::Android);
        }
        self
    }
}

#[derive(clap::Args)]
struct RunArgs {
    /// App directory or a file/directory inside it. Defaults to the current directory.
    path: Option<PathBuf>,

    /// Build for a target first; without it, `cargo run` for this host.
    #[arg(long, value_enum, conflicts_with_all = ["platform", "arch"])]
    target: Option<TargetArg>,

    #[command(flatten)]
    options: TargetOptions,

    /// A native platform runs only on the system and architecture it is
    /// built for; `web` builds once and serves the page.
    #[command(flatten)]
    platform: PlatformArgs,

    /// Address the page is served on (`--platform web`).
    #[arg(long, default_value = "127.0.0.1:4321")]
    bind: String,

    /// The `adb` serial of the device or emulator to install on
    /// (`--platform android`). Defaults to `$ANDROID_SERIAL`, else the one
    /// connected.
    #[arg(long)]
    device: Option<String>,

    /// Web only: loopback port to serve on.
    #[arg(long, default_value = "7878")]
    port: u16,

    /// Web only: serve once, without rebuilding on change.
    #[arg(long)]
    no_watch: bool,

    /// Arguments forwarded to the app. Write them after `--`.
    #[arg(last = true, allow_hyphen_values = true)]
    forwarded: Vec<OsString>,
}

#[derive(clap::Args)]
struct BuildArgs {
    /// App directory or a file/directory inside it. Defaults to the current directory.
    path: Option<PathBuf>,

    /// Target to build for.
    #[arg(long, value_enum, conflicts_with_all = ["platform", "arch"])]
    target: Option<TargetArg>,

    #[command(flatten)]
    options: TargetOptions,

    /// Without `--target`: `tilo` builds thin against the toolkit the OS
    /// ships; `linux`, `windows` and `macos` one self-contained executable;
    /// `web` a page with the toolkit core and the app, ready to serve.
    #[command(flatten)]
    platform: PlatformArgs,

    /// For the web, the page directory [default: target/web]; for a native
    /// platform, a directory to copy the executables into. Relative to the
    /// app root unless absolute.
    #[arg(short, long, conflicts_with = "target")]
    output: Option<PathBuf>,

    /// Relink only the app against the toolkit core already built
    /// (`--platform web`).
    #[arg(long, conflicts_with = "target")]
    app_only: bool,

    /// Cargo profile for a native `--platform` build. Defaults to `thin-app`,
    /// or `release` with `--static` and for the other desktops.
    #[arg(long, conflicts_with = "target")]
    profile: Option<String>,

    /// Link the toolkit and `libstd` into a Tilo OS build instead of against
    /// the copies the OS ships.
    #[arg(long = "static", conflicts_with = "target")]
    static_link: bool,
}

#[derive(clap::Args)]
struct BundleMacosArgs {
    /// App project whose manifest supplies identity, capabilities, icon and version.
    path: PathBuf,
    /// Previously compiled app executable. Architecture/signature validation is separate.
    #[arg(long)]
    binary: PathBuf,
    /// Previously compiled tilo-runtime-mcp helper.
    #[arg(long)]
    runtime_host: PathBuf,
    /// New output directory; an existing destination is never overwritten.
    #[arg(long)]
    output: PathBuf,
}

#[derive(clap::Args)]
struct DevArgs {
    /// App directory or a file/directory inside it. Defaults to the current directory.
    path: Option<PathBuf>,

    /// Quiet period used to coalesce editor save events.
    #[arg(long, default_value_t = NonZeroU64::new(tilo_app_cli::DEV_DEBOUNCE_MS).expect("the default debounce is not zero"))]
    debounce_ms: NonZeroU64,

    /// Watch only the app, not its local `ui` dependency.
    #[arg(long)]
    app_only: bool,

    /// Report where each reload spent its time, from the change to the
    /// first presented frame. With `--json`, the result lists every reload.
    #[arg(long)]
    timings: bool,

    /// Build the app optimised, with `--profile thin-app`: release
    /// optimisation without LTO, as thin Tilo builds ship. Use it when the
    /// debug build is too slow to judge (painting, video, large documents);
    /// each rebuild takes longer. The app still reloads in place.
    #[arg(long, conflicts_with = "profile")]
    release: bool,

    /// Cargo profile for every build of the app [default: Cargo's `dev`].
    /// Only `dev` and `thin-app` reload in place; any other profile (such
    /// as the fat-LTO `release`) restarts the app on each change instead.
    /// The runtime host stays a debug build.
    #[arg(long)]
    profile: Option<String>,

    /// `web` serves the app's web build and reloads the page into the same
    /// document after each rebuild; a native platform runs on this machine
    /// only when it is that system and architecture.
    #[command(flatten)]
    platform: PlatformArgs,

    /// Address the web page is served on (`--platform web`).
    #[arg(long, default_value = "127.0.0.1:4321")]
    bind: String,

    /// The `adb` serial of the device or emulator to install on
    /// (`--platform android`). Defaults to `$ANDROID_SERIAL`, else the one
    /// connected.
    #[arg(long)]
    device: Option<String>,

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
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent command-line switches, not a state machine"
)]
struct PackageArgs {
    /// App directory or a file/directory inside it. Defaults to the current directory.
    path: Option<PathBuf>,

    /// Package path, relative to the app root unless absolute.
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Persistent tilo-app-package signing identity, relative to the app root unless absolute.
    #[arg(long)]
    identity: Option<PathBuf>,

    /// Signed native SDK directory. Its publisher and root must be selected independently.
    #[arg(long, requires_all = ["sdk_publisher", "sdk_root"])]
    sdk_bundle: Option<PathBuf>,

    /// Expected SDK publisher public key, from the authenticated SDK receipt.
    #[arg(long, requires = "sdk_bundle")]
    sdk_publisher: Option<String>,

    /// Expected signed SDK payload root, from the authenticated SDK receipt.
    #[arg(long, requires = "sdk_bundle")]
    sdk_root: Option<String>,

    /// Separately signed compiler interface bound to the selected SDK.
    #[arg(long, requires_all = ["sdk_bundle", "sdk_compiler_root"])]
    sdk_compiler_bundle: Option<PathBuf>,

    /// Independently selected compiler-interface content root.
    #[arg(long, requires = "sdk_compiler_bundle")]
    sdk_compiler_root: Option<String>,

    /// Cargo profile used to build the declared entrypoints. Defaults to
    /// `thin-app`, or `release` with `--static`.
    #[arg(long)]
    profile: Option<String>,

    /// Link the toolkit and `libstd` into the binary instead of against the
    /// copies the OS ships. The result is far over the application size limit;
    /// use it for local experiments only.
    #[arg(long = "static")]
    static_link: bool,

    /// The system to build for: a thin Tilo OS app, one self-contained
    /// Linux, Windows or macOS executable with the toolkit linked in, a
    /// WebAssembly side module of the browser's toolkit core, or a signed
    /// Android APK (and bundle).
    #[arg(long, value_enum, default_value_t = PlatformArg::Tilo)]
    platform: PlatformArg,

    /// CPU architecture to build for. Defaults to the host's.
    #[arg(long, value_enum)]
    arch: Option<ArchArg>,

    /// `linux`, `macos` or `web` packages the app for that SDK target, and
    /// `android` as `--platform android` does; any other value is a Rust
    /// target triple passed to Cargo, overriding the
    /// one --platform and --arch pick (for example `x86_64-pc-windows-gnu`).
    #[arg(long)]
    target: Option<String>,

    /// Package even when a required capability's backend is only planned.
    #[arg(long)]
    allow_planned: bool,

    /// Comma-separated Cargo features enabled for the package build.
    #[arg(long, value_delimiter = ',')]
    features: Vec<String>,

    /// Disable the app's default Cargo features.
    #[arg(long)]
    no_default_features: bool,

    /// Replace an existing output package after the new package verifies successfully.
    #[arg(long)]
    force: bool,

    #[command(flatten)]
    android: AndroidPackageArgs,
}

/// `package --platform android`'s own options.
#[derive(clap::Args)]
struct AndroidPackageArgs {
    /// The keystore that signs an Android release (`--platform android`).
    /// Defaults to `$TILO_ANDROID_KEYSTORE`, else a release key kept beside
    /// the publisher key.
    #[arg(long)]
    keystore: Option<PathBuf>,

    /// Skip the Android App Bundle (`.aab`) beside the APK.
    #[arg(long)]
    no_bundle: bool,
}

#[derive(clap::Args)]
struct NewArgs {
    /// Cargo package name, for example `reading-list`.
    name: Option<String>,

    /// Reverse-DNS application id. Defaults to `so.tilo.<name>` in the Tilo
    /// workspace and `local.dev.<name>` elsewhere; publishing needs a
    /// namespace you have verified.
    #[arg(long)]
    id: Option<String>,

    /// Human-readable application name.
    #[arg(long)]
    title: Option<String>,

    /// Starter shape to generate; `--list-templates` describes each.
    #[arg(long, value_enum, conflicts_with = "from_example")]
    template: Option<TemplateArg>,

    /// Start from a complete example app instead of a template, renamed to
    /// this app; `tilo-app examples` lists them.
    #[arg(long, value_name = "EXAMPLE")]
    from_example: Option<String>,

    /// List the templates `--template` accepts, then exit.
    #[arg(long, conflicts_with_all = [
        "name", "id", "title", "template", "from_example", "directory", "ui_path", "sdk_git",
        "sdk_rev", "dry_run", "dev",
    ])]
    list_templates: bool,

    /// Destination directory. Defaults to `apps/<name>` in a Tilo checkout.
    #[arg(long)]
    directory: Option<PathBuf>,

    /// Build against the toolkit `ui` crate of this Tilo checkout (or set
    /// `TILO_UI_PATH`).
    #[arg(long, conflicts_with_all = ["sdk_git", "sdk_rev", "sdk_version"])]
    ui_path: Option<PathBuf>,

    /// Build a standalone app against this Tilo git repository. Without a
    /// checkout nearby, the default is <https://code.tilo.so/tilo/tilo.git>.
    #[arg(long, value_name = "URL")]
    sdk_git: Option<String>,

    /// Pin the SDK repository to this commit. Defaults to the commit this
    /// `tilo-app` was built from.
    #[arg(long, value_name = "COMMIT")]
    sdk_rev: Option<String>,

    /// Build a standalone app on the published SDK: `tilo-app` from the
    /// registry at this version requirement, with no `[patch]` or lockfile.
    /// Without a value, the version this `tilo-app` was built with.
    #[arg(
        long,
        value_name = "VERSION",
        num_args = 0..=1,
        default_missing_value = "",
        conflicts_with_all = ["sdk_git", "sdk_rev"]
    )]
    sdk_version: Option<String>,

    /// Print the files that would be created without writing them.
    #[arg(long, conflicts_with = "dev")]
    dry_run: bool,

    /// Enter the live rebuild/restart loop after creating the app.
    #[arg(long, conflicts_with = "dry_run")]
    dev: bool,
}

/// Every template starts on the app framework: a `#[derive(Model)]` resource,
/// `#[action]` edits and a view bound to them.
#[derive(Clone, Copy, Debug, ValueEnum)]
enum TemplateArg {
    /// One resource, one action and one bound field: the smallest correct app.
    Starter,
    /// One collaborative document with a live preview, undo and a destructive clear.
    Document,
    /// A list of items the user adds, edits, checks off and removes.
    Collection,
    /// Serialized, live-editable screens above controls bound to the resource.
    Graph,
    // Templates earlier versions had, now examples; naming one says which.
    #[value(hide = true)]
    Data,
    #[value(hide = true)]
    List,
    #[value(hide = true)]
    Tree,
    #[value(hide = true)]
    Async,
    #[value(hide = true)]
    Qr,
    #[value(hide = true)]
    Workspace,
}

impl TemplateArg {
    fn template(self) -> Result<tilo_app_cli::Template> {
        let retired = match self {
            Self::Starter => return Ok(tilo_app_cli::Template::Starter),
            Self::Document => return Ok(tilo_app_cli::Template::Document),
            Self::Collection => return Ok(tilo_app_cli::Template::Collection),
            Self::Graph => return Ok(tilo_app_cli::Template::Graph),
            Self::Data => "data",
            Self::List => "list",
            Self::Tree => "tree",
            Self::Async => "async",
            Self::Qr => "qr",
            Self::Workspace => "workspace",
        };
        let example = tilo_app_cli::retired_template_example(retired).unwrap_or("counter");
        anyhow::bail!(
            "the {retired} template is now the `{example}` example; run `tilo-app new NAME --from-example {example}`"
        )
    }
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
    tilo_app_cli::run_as_build_tool_if_requested();
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => return report_usage_error(&error),
    };
    let json = if cli.json {
        match json_stdout::JsonStdout::claim() {
            Ok(stdout) => Some(stdout),
            Err(error) => {
                eprintln!("tilo-app: reserving stdout for --json: {error}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        None
    };
    let interactive = json.is_none();
    let name = cli.command.name();
    let result = match cli.command {
        Command::Guide { topic } => Ok(guide::run(topic, interactive)),
        Command::New(args) => run_new(args, interactive),
        Command::Examples { name, file } => {
            run_examples(name.as_deref(), file.as_deref(), interactive)
        }
        Command::Add(args) => run_add(args, interactive),
        Command::Locale(args) => run_locale(args),
        Command::Scenario(args) => run_scenario(args),
        Command::Doctor(args) => run_doctor(args),
        Command::Setup(args) => run_setup(&args),
        Command::Check(args) => run_check(args),
        Command::Certify(args) => run_certify(args),
        Command::Surface(args) => run_surface(args),
        Command::Run(args) => run_run(args),
        Command::Build(args) => run_build(args),
        Command::BundleMacos(args) => run_bundle_macos(&args),
        Command::Test(args) => run_app_task(args, tilo_app_cli::AppTask::Test),
        Command::Dev(args) => run_dev(args),
        Command::Shot(args) => run_shot(args),
        Command::Inspect(args) => run_inspect(args),
        Command::Ui(args) => run_ui(args),
        Command::Package(args) => run_package(args),
        Command::Sdk(args) => run_sdk(args, interactive),
        Command::Keys(args) => publishing::run_keys(args),
        Command::Apply(args) => publishing::run_apply(args),
        Command::Publish(args) => publishing::run_publish(args),
    };
    let code = match &result {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("tilo-app: {error:#}");
            ExitCode::FAILURE
        }
    };
    if let Some(stdout) = json {
        let document = match result {
            Ok(report) => serde_json::json!({"ok": true, "command": name, "result": report}),
            Err(error) => serde_json::json!({
                "ok": false,
                "command": name,
                "error": format!("{error:#}"),
                "causes": error.chain().map(ToString::to_string).collect::<Vec<_>>(),
                "result": error
                    .downcast_ref::<Failed>()
                    .or_else(|| error.chain().find_map(|cause| cause.downcast_ref::<Failed>()))
                    .map(|failed| &failed.report),
            }),
        };
        if let Err(error) = stdout.emit(&document) {
            eprintln!("tilo-app: writing --json result: {error}");
            return ExitCode::FAILURE;
        }
    }
    code
}

/// Help and version stay human text; any other usage error is reported as a
/// JSON document when `--json` was requested, so a caller never has to parse
/// clap's prose to learn that its invocation was wrong.
fn report_usage_error(error: &clap::Error) -> ExitCode {
    use clap::error::ErrorKind;

    let informational = matches!(
        error.kind(),
        ErrorKind::DisplayHelp
            | ErrorKind::DisplayVersion
            | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
    );
    let wants_json = std::env::args_os().skip(1).any(|arg| arg == "--json");
    if informational || !wants_json {
        error.exit();
    }
    let document = json!({
        "ok": false,
        "command": Value::Null,
        "error": error.kind().to_string(),
        "usage": error.render().to_string(),
    });
    println!("{document}");
    ExitCode::from(2)
}

/// A command that ran to completion but whose verdict is failure. The report
/// still describes what happened, so `--json` includes it.
#[derive(Debug)]
struct Failed {
    message: String,
    report: Value,
}

impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Failed {}

fn failed(message: impl Into<String>, report: Value) -> anyhow::Error {
    Failed {
        message: message.into(),
        report,
    }
    .into()
}

impl Command {
    fn name(&self) -> &'static str {
        match self {
            Self::Guide { .. } => "guide",
            Self::New(_) => "new",
            Self::Examples { .. } => "examples",
            Self::Add(_) => "add",
            Self::Locale(_) => "locale",
            Self::Scenario(_) => "scenario",
            Self::Doctor(_) => "doctor",
            Self::Setup(_) => "setup",
            Self::Check(_) => "check",
            Self::Certify(_) => "certify",
            Self::Surface(_) => "surface",
            Self::Run(_) => "run",
            Self::Build(_) => "build",
            Self::BundleMacos(_) => "bundle-macos",
            Self::Test(_) => "test",
            Self::Dev(_) => "dev",
            Self::Shot(_) => "shot",
            Self::Inspect(_) => "inspect",
            Self::Ui(_) => "ui",
            Self::Package(_) => "package",
            Self::Sdk(_) => "sdk",
            Self::Keys(_) => "keys",
            Self::Apply(_) => "apply",
            Self::Publish(_) => "publish",
        }
    }
}

fn run_scenario(args: ScenarioArgs) -> Result<Value> {
    match args.command {
        ScenarioCommand::Run {
            name,
            app,
            viewport,
            artifacts,
            audit,
        } => {
            let start = app.map_or_else(std::env::current_dir, Ok)?;
            let plan = tilo_app_cli::plan_scenario_suite(&tilo_app_cli::ScenarioRunRequest {
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

fn execute_scenario_plan(plan: &tilo_app_cli::ScenarioSuitePlan) -> Result<Value> {
    let outcome = plan.execute()?;
    let mut report = outcome.to_json();
    if let Some(fields) = report.as_object_mut() {
        fields.insert(
            "artifacts".into(),
            json!(plan.artifact_dir().map(|path| path.display().to_string())),
        );
    }
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
        Ok(report)
    } else {
        let total = outcome.passed.len().saturating_add(outcome.failed.len());
        Err(failed(
            format!("{} of {} scenario(s) failed", outcome.failed.len(), total),
            report,
        ))
    }
}

fn run_locale(args: LocaleArgs) -> Result<Value> {
    match args.command {
        LocaleCommand::Sync {
            locale,
            prune,
            reset_variables,
            app,
            dry_run,
        } => {
            let start = app.map_or_else(std::env::current_dir, Ok)?;
            let outcome = tilo_app_cli::sync_locale_catalogs(&tilo_app_cli::LocaleSyncRequest {
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
            Ok(outcome.to_json())
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one dispatch over every artifact the command adds"
)]
fn run_add(args: AddArgs, interactive: bool) -> Result<Value> {
    let Some(command) = args.command else {
        anyhow::ensure!(
            interactive,
            "an artifact subcommand is required with --json; use `tilo-app add screen`, `tilo-app add component`, `tilo-app add scenario`, or `tilo-app add locale`"
        );
        let start = std::env::current_dir()?;
        let outcome = tilo_app_cli::add_app_artifact_interactive(&start)?;
        print_add_outcome(&outcome);
        return Ok(outcome.to_json());
    };
    let (artifact, options) = match command {
        AddCommand::Capability {
            name,
            required,
            list,
            app,
        } => return run_add_capability(name, required, list, app),
        AddCommand::Ci {
            forge,
            runs_on,
            options,
        } => {
            return run_add_ci(forge, runs_on.as_deref(), &options);
        }
        AddCommand::Screen {
            name,
            title,
            template,
            link_from,
            options,
        } => (
            tilo_app_cli::AddArtifact::Screen {
                name,
                title,
                template: match template {
                    ScreenTemplateArg::Blank => tilo_app_cli::ScreenTemplate::Blank,
                    ScreenTemplateArg::Form => tilo_app_cli::ScreenTemplate::Form,
                    ScreenTemplateArg::Settings => tilo_app_cli::ScreenTemplate::Settings,
                    ScreenTemplateArg::EmptyState => tilo_app_cli::ScreenTemplate::EmptyState,
                },
                link_from,
            },
            options,
        ),
        AddCommand::Scenario {
            name,
            screen,
            options,
        } => (
            tilo_app_cli::AddArtifact::Scenario { name, screen },
            options,
        ),
        AddCommand::Locale { locale, options } => {
            (tilo_app_cli::AddArtifact::Locale { locale }, options)
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
            let template = component_template(template);
            let artifact = if let Some(screen) = mount {
                let mut mount = match template {
                    tilo_app_cli::ComponentTemplate::Display => {
                        tilo_app_cli::ComponentMount::display(screen, &name)
                    }
                    tilo_app_cli::ComponentTemplate::Form => {
                        tilo_app_cli::ComponentMount::form(screen, &name)
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
                tilo_app_cli::AddArtifact::GraphComponent {
                    name,
                    template,
                    mount,
                }
            } else {
                tilo_app_cli::AddArtifact::Component { name, template }
            };
            (artifact, options)
        }
    };
    let start = options.app.map_or_else(std::env::current_dir, Ok)?;
    let outcome = tilo_app_cli::add_app_artifact(tilo_app_cli::AddRequest {
        start,
        artifact,
        force: options.force,
        dry_run: options.dry_run,
    })?;
    print_add_outcome(&outcome);
    Ok(outcome.to_json())
}

fn component_template(template: ComponentTemplateArg) -> tilo_app_cli::ComponentTemplate {
    match template {
        ComponentTemplateArg::Display => tilo_app_cli::ComponentTemplate::Display,
        ComponentTemplateArg::Form => tilo_app_cli::ComponentTemplate::Form,
        ComponentTemplateArg::Tree => tilo_app_cli::ComponentTemplate::Tree,
        ComponentTemplateArg::List => tilo_app_cli::ComponentTemplate::List,
        ComponentTemplateArg::Table => tilo_app_cli::ComponentTemplate::Table,
        ComponentTemplateArg::Text => tilo_app_cli::ComponentTemplate::Text,
    }
}

fn run_add_ci(forge: ForgeArg, runs_on: Option<&str>, options: &AddOptions) -> Result<Value> {
    let start = options.app.clone().map_or_else(std::env::current_dir, Ok)?;
    let forge = match forge {
        ForgeArg::Github => tilo_app_cli::ci::Forge::Github,
        ForgeArg::Forgejo => tilo_app_cli::ci::Forge::Forgejo,
        ForgeArg::Gitea => tilo_app_cli::ci::Forge::Gitea,
    };
    let outcome = tilo_app_cli::ci::add_ci(&start, forge, runs_on, options.force, options.dry_run)?;
    let verb = if outcome.written {
        "Created"
    } else {
        "Would create"
    };
    println!("{verb} {}", outcome.path.display());
    if let Some(path) = &outcome.reusable_path {
        println!("{verb} {} (reusable GitHub workflow)", path.display());
    }
    println!(
        "Next: add the TILO_PUBLISHER_KEY secret (what `tilo-app keys export` prints), then push a v* tag to publish"
    );
    Ok(outcome.to_json())
}

fn print_add_outcome(outcome: &tilo_app_cli::AddOutcome) {
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

fn run_ui(args: UiArgs) -> Result<Value> {
    let report = match args.command {
        UiCommand::Edit { file, builder } => {
            let _ = tilo_app_cli::load_ui_graph(&file)?;
            tilo_app_cli::ui_builder_plan(&file, builder)?.execute()?;
            json!({"file": file, "action": "edit"})
        }
        UiCommand::Init { file, title, force } => {
            tilo_app_cli::init_ui_graph(&file, &title, force)?;
            println!("Created {}", file.display());
            json!({"file": file, "action": "create"})
        }
        UiCommand::Tree { file } => {
            let tree = tilo_app_cli::ui_graph_tree(&file)?;
            print!("{tree}");
            json!({"file": file, "tree": tree})
        }
        UiCommand::Validate { file } => {
            let document = tilo_app_cli::load_ui_graph(&file)?;
            println!(
                "Valid UI graph: {} nodes (version {})",
                document.nodes().len(),
                document.version
            );
            json!({"file": file, "valid": true, "nodes": document.nodes().len(), "version": document.version})
        }
        UiCommand::Format { file } => {
            tilo_app_cli::format_ui_graph(&file)?;
            println!("Formatted {}", file.display());
            json!({"file": file, "action": "format"})
        }
        UiCommand::Insert {
            file,
            parent,
            index,
            source,
        } => {
            let node = read_ui_node(source)?;
            ui_graph_edit(
                &file,
                tilo_app_cli::UiGraphEdit::Insert {
                    parent,
                    index,
                    node,
                },
            )?
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
            tilo_app_cli::UiComponentInsert {
                name,
                id,
                parent,
                index,
                props,
            },
        )?,
        UiCommand::Remove { file, id } => {
            ui_graph_edit(&file, tilo_app_cli::UiGraphEdit::Remove { id })?
        }
        UiCommand::Replace { file, id, source } => {
            let node = read_ui_node(source)?;
            ui_graph_edit(&file, tilo_app_cli::UiGraphEdit::Replace { id, node })?
        }
        UiCommand::Move {
            file,
            id,
            parent,
            index,
        } => ui_graph_edit(&file, tilo_app_cli::UiGraphEdit::Move { id, parent, index })?,
        UiCommand::Set {
            file,
            id,
            property,
            edit,
        } => {
            let graph_edit = match (edit.value, edit.unset) {
                (Some(source), false) => tilo_app_cli::UiGraphEdit::SetProperty {
                    id,
                    property,
                    value: parse_ui_property_value(source),
                },
                (None, true) => tilo_app_cli::UiGraphEdit::UnsetProperty { id, property },
                _ => anyhow::bail!("choose exactly one of --value or --unset"),
            };
            ui_graph_edit(&file, graph_edit)?
        }
        UiCommand::Apply { file, edits } => run_ui_apply(&file, &edits)?,
    };
    Ok(report)
}

fn ui_graph_edit(file: &Path, edit: tilo_app_cli::UiGraphEdit) -> Result<Value> {
    tilo_app_cli::edit_ui_graph(file, edit)?;
    println!("Updated {}", file.display());
    Ok(json!({"file": file, "action": "update"}))
}

fn run_ui_component(file: &Path, request: tilo_app_cli::UiComponentInsert) -> Result<Value> {
    let id = tilo_app_cli::insert_ui_component(file, request)?;
    println!("Inserted {id} in {}", file.display());
    Ok(json!({"file": file, "action": "insert", "id": id}))
}

fn run_ui_apply(file: &Path, edits: &Path) -> Result<Value> {
    let source = std::fs::read_to_string(edits)
        .with_context(|| format!("reading UI graph edits {}", edits.display()))?;
    let edits = tilo_app_cli::parse_ui_graph_edits(&source)?;
    let count = edits.len();
    tilo_app_cli::edit_ui_graph_batch(file, edits)?;
    println!("Applied {count} edits to {}", file.display());
    Ok(json!({"file": file, "action": "apply", "edits": count}))
}

fn parse_ui_property_value(source: String) -> serde_json::Value {
    serde_json::from_str(&source).unwrap_or(serde_json::Value::String(source))
}

fn read_ui_node(source: NodeSourceArgs) -> Result<tilo_ui_graph::UiNode> {
    let source = match (source.node, source.node_file) {
        (Some(source), None) => source,
        (None, Some(path)) => std::fs::read_to_string(&path)
            .map_err(anyhow::Error::from)
            .with_context(|| format!("reading UI node {}", path.display()))?,
        _ => anyhow::bail!("provide exactly one of --node or --node-file"),
    };
    tilo_app_cli::parse_ui_node(&source)
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum PlatformArg {
    Tilo,
    Linux,
    Windows,
    Macos,
    Web,
    Android,
}

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum CheckPlatformArg {
    Tilo,
    Linux,
    Windows,
    Macos,
    Web,
    Android,
    /// Every platform above.
    All,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum ForgeArg {
    Github,
    Forgejo,
    Gitea,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum ArchArg {
    #[value(name = "x86_64")]
    X86_64,
    Aarch64,
    /// The one architecture of `--platform web`.
    Wasm32,
}

/// The platform and, for a native one, the architecture `--platform` and
/// `--arch` name; `None` is the host's.
fn platform_and_arch(
    platform: PlatformArg,
    arch: Option<ArchArg>,
) -> Result<(
    tilo_app_cli::PackagePlatform,
    Option<tilo_app_cli::PackageArch>,
)> {
    let web = matches!(platform, PlatformArg::Web);
    match arch {
        Some(ArchArg::Wasm32) if !web => anyhow::bail!("--arch wasm32 is for --platform web"),
        Some(ArchArg::X86_64 | ArchArg::Aarch64) if web => {
            anyhow::bail!("--platform web builds for wasm32")
        }
        _ => {}
    }
    Ok((
        match platform {
            PlatformArg::Tilo => tilo_app_cli::PackagePlatform::Tilo,
            PlatformArg::Linux => tilo_app_cli::PackagePlatform::Linux,
            PlatformArg::Windows => tilo_app_cli::PackagePlatform::Windows,
            PlatformArg::Macos => tilo_app_cli::PackagePlatform::Macos,
            PlatformArg::Web => tilo_app_cli::PackagePlatform::Web,
            PlatformArg::Android => tilo_app_cli::PackagePlatform::Android,
        },
        arch.and_then(|arch| match arch {
            ArchArg::X86_64 => Some(tilo_app_cli::PackageArch::X86_64),
            ArchArg::Aarch64 => Some(tilo_app_cli::PackageArch::Aarch64),
            ArchArg::Wasm32 => None,
        }),
    ))
}

fn run_sdk(args: SdkArgs, interactive: bool) -> Result<Value> {
    let cwd = std::env::current_dir()?;
    let repo = cwd
        .ancestors()
        .find(|path| path.join("os/dev/export-sdk-compiler.py").is_file())
        .context("SDK export requires its canonical checkout")?;
    let mut command = std::process::Command::new("python3");
    command
        .arg(repo.join("os/dev/export-sdk-compiler.py"))
        .current_dir(repo);
    let mut resource_file = None;
    match args.command {
        SdkCommand::Declarations => {
            let report =
                json!({"resources": tilo_app_cli::native_sdk::compiler::capture_resources(repo)?});
            if interactive {
                println!("{}", serde_json::to_string_pretty(&report)?);
            }
            return Ok(report);
        }
        SdkCommand::Capture { units, output } => {
            let resources = tilo_app_cli::native_sdk::compiler::capture_resources(repo)?;
            let temporary = tempfile::NamedTempFile::new()?;
            std::fs::write(temporary.path(), serde_json::to_vec(&resources)?)?;
            command.arg("--resources").arg(temporary.path());
            resource_file = Some(temporary);
            command
                .args(["--capture-only", "--repo"])
                .arg(repo)
                .arg("--units")
                .arg(units)
                .arg("--capture")
                .arg(output);
        }
        SdkCommand::Export {
            capture,
            sdk_bundle,
            sdk_publisher,
            sdk_root,
            identity,
            bundle_tool,
            output,
        } => {
            command
                .arg("--capture")
                .arg(capture)
                .arg("--sdk")
                .arg(sdk_bundle)
                .arg("--sdk-publisher")
                .arg(sdk_publisher)
                .arg("--sdk-root")
                .arg(sdk_root)
                .arg("--identity")
                .arg(identity)
                .arg("--bundle-tool")
                .arg(bundle_tool)
                .arg("--out-root")
                .arg(output);
        }
    }
    let output = command.output()?;
    drop(resource_file);
    std::io::Write::write_all(&mut std::io::stderr(), &output.stderr)?;
    if !output.status.success() {
        anyhow::bail!("SDK compiler capture/export failed");
    }
    let report: Value = serde_json::from_slice(&output.stdout)?;
    if interactive {
        println!("{}", serde_json::to_string_pretty(&report)?);
    }
    Ok(report)
}

fn run_package(args: PackageArgs) -> Result<Value> {
    let path = args.path.map_or_else(std::env::current_dir, Ok)?;
    let sdk_target = match args.target.as_deref() {
        Some("linux") => Some(tilo_app_cli::targets::Target::Linux),
        Some("macos") => Some(tilo_app_cli::targets::Target::Macos),
        Some("web") => Some(tilo_app_cli::targets::Target::Web),
        _ => None,
    };
    if let Some(target) = sdk_target {
        if args.sdk_bundle.is_some() {
            anyhow::bail!(
                "--sdk-bundle requires --platform tilo and a Rust target triple, not an SDK target alias"
            );
        }
        let mut build = tilo_app_cli::targets::TargetRequest::new(target);
        build.allow_planned = args.allow_planned;
        build.features = args.features;
        build.no_default_features = args.no_default_features;
        let output = tilo_app_cli::targets::package(
            &path,
            &tilo_app_cli::targets::TargetPackageRequest {
                build,
                output: args.output,
                identity: args.identity,
                force: args.force,
            },
        )?;
        println!("{}", output.display());
        return Ok(json!({"artifact": output}));
    }
    let android = args.target.as_deref() == Some("android");
    let (platform, arch) = platform_and_arch(
        if android {
            PlatformArg::Android
        } else {
            args.platform
        },
        args.arch,
    )?;
    if platform != tilo_app_cli::PackagePlatform::Android
        && (args.android.keystore.is_some() || args.android.no_bundle)
    {
        anyhow::bail!("--keystore and --no-bundle are for --platform android");
    }
    let sdk = match (args.sdk_bundle, args.sdk_publisher, args.sdk_root) {
        (Some(bundle), Some(publisher), Some(root)) => {
            Some(tilo_app_cli::native_sdk::SdkSelection {
                compiler: match (args.sdk_compiler_bundle, args.sdk_compiler_root) {
                    (Some(bundle), Some(root)) => {
                        Some(tilo_app_cli::native_sdk::compiler::CompilerSelection { bundle, root })
                    }
                    (None, None) => None,
                    _ => anyhow::bail!("compiler bundle and root must be selected together"),
                },
                bundle,
                publisher,
                root,
            })
        }
        (None, None, None) => None,
        _ => anyhow::bail!("native SDK selection requires bundle, publisher and root"),
    };
    let request = tilo_app_cli::PackageRequest {
        sdk,
        output: args.output,
        identity: args.identity,
        profile: args.profile,
        static_link: args.static_link,
        target: args.target.filter(|_| !android),
        platform,
        arch,
        features: args.features,
        no_default_features: args.no_default_features,
        force: args.force,
    };
    if platform == tilo_app_cli::PackagePlatform::Android {
        let outcomes = tilo_app_cli::android::package(
            &path,
            &request,
            &tilo_app_cli::android::PackageOptions {
                keystore: args.android.keystore,
                bundle: args.android.no_bundle.then_some(false),
            },
        )?;
        let mut packages: Vec<Value> = outcomes
            .iter()
            .map(tilo_app_cli::PackageOutcome::to_json)
            .collect();
        return Ok(if packages.len() == 1 {
            packages.remove(0)
        } else {
            json!({ "packages": packages })
        });
    }
    Ok(tilo_app_cli::package_app(&path, &request)?.to_json())
}

fn run_run(args: RunArgs) -> Result<Value> {
    let path = args.path.map_or_else(std::env::current_dir, Ok)?;
    let Some(target) = args.target.and_then(TargetArg::sdk) else {
        let (platform, arch) = args.platform.or_target(args.target).resolve()?;
        if platform == tilo_app_cli::PackagePlatform::Web {
            tilo_app_cli::web::run_web(
                &path,
                &tilo_app_cli::web::WebDevOptions { bind: args.bind },
            )?;
            return Ok(json!({"stopped": true}));
        }
        if platform == tilo_app_cli::PackagePlatform::Android {
            if !args.forwarded.is_empty() {
                anyhow::bail!("an Android app takes no command-line arguments");
            }
            let build = tilo_app_cli::android::run_on_device(
                &path,
                &tilo_app_cli::android::DeviceRequest {
                    device: args.device,
                    arch,
                },
            )?;
            return Ok(build.to_json());
        }
        platform.ensure_runs_here(arch)?;
        let plan = tilo_app_cli::plan_app_task(&path, tilo_app_cli::AppTask::Run, &args.forwarded)?;
        plan.execute()?;
        return Ok(json!({"app_dir": plan.app_dir, "platform": platform.cli_name()}));
    };
    let serve = tilo_app_cli::targets::ServeOptions {
        address: std::net::SocketAddr::from(([127, 0, 0, 1], args.port)),
        watch: !args.no_watch,
        ..tilo_app_cli::targets::ServeOptions::default()
    };
    tilo_app_cli::targets::run(
        &path,
        &args.options.request(target),
        &args.forwarded,
        &serve,
    )?;
    Ok(json!({"app_dir": path}))
}

fn run_bundle_macos(args: &BundleMacosArgs) -> Result<Value> {
    let artifact = tilo_app_cli::targets::assemble_macos(
        &args.path,
        &args.binary,
        &args.runtime_host,
        &args.output,
    )?;
    println!("{}", artifact.display());
    Ok(json!({"artifact": artifact, "signed": false, "notarized": false}))
}

fn run_build(args: BuildArgs) -> Result<Value> {
    let path = args.path.clone().map_or_else(std::env::current_dir, Ok)?;
    let Some(target) = args.target.and_then(TargetArg::sdk) else {
        return run_platform_build(&path, args);
    };
    let outcome = tilo_app_cli::targets::build(&path, &args.options.request(target))?;
    println!("{}", outcome.artifact.display());
    Ok(json!({"artifact": outcome.artifact}))
}

fn run_add_capability(
    name: Option<String>,
    required: bool,
    list: bool,
    app: Option<PathBuf>,
) -> Result<Value> {
    if list {
        print!("{}", tilo_app_cli::targets::capability_table());
        return Ok(json!({"listed": true}));
    }
    let name = name.context("name a capability, or pass --list")?;
    let start = app.map_or_else(std::env::current_dir, Ok)?;
    let added = tilo_app_cli::targets::add_capability(&start, &name, required)?;
    println!(
        "Declared `{}` as {} in {}",
        added.capability,
        if added.required {
            "required"
        } else {
            "optional"
        },
        added.manifest.display()
    );
    for (target, reason) in &added.unavailable {
        println!("  note: not available on {target} ({reason}); the app must degrade there");
    }
    Ok(json!({
        "capability": added.capability.to_string(),
        "required": added.required,
        "manifest": added.manifest,
    }))
}

fn run_dev(args: DevArgs) -> Result<Value> {
    let path = args.path.map_or_else(std::env::current_dir, Ok)?;
    let request = tilo_app_cli::DevRequest {
        forwarded: args.forwarded,
        watch_ui: !args.app_only,
        debounce: Duration::from_millis(args.debounce_ms.get()),
        timings: args.timings,
        profile: dev_profile(args.release, args.profile),
    };
    let (platform, arch) = args.platform.resolve()?;
    if platform == tilo_app_cli::PackagePlatform::Web && request.profile.is_some() {
        anyhow::bail!("--release and --profile are for a native or Android dev loop");
    }
    if platform == tilo_app_cli::PackagePlatform::Web {
        tilo_app_cli::web::dev_web(
            &path,
            &request,
            &tilo_app_cli::web::WebDevOptions { bind: args.bind },
        )?;
    } else if platform == tilo_app_cli::PackagePlatform::Android {
        tilo_app_cli::android::dev_on_device(
            &path,
            &request,
            &tilo_app_cli::android::DeviceRequest {
                device: args.device,
                arch,
            },
        )?;
    } else {
        platform.ensure_runs_here(arch)?;
        let reloads = tilo_app_cli::dev_app(&path, &request)?;
        if request.timings {
            return Ok(json!({"stopped": true, "reloads": reloads}));
        }
    }
    Ok(json!({"stopped": true}))
}

/// The Cargo profile `dev` builds the app with; `None` is Cargo's `dev`.
fn dev_profile(release: bool, profile: Option<String>) -> Option<String> {
    if release {
        Some(tilo_app_cli::DEV_RELEASE_PROFILE.to_owned())
    } else {
        profile
    }
}

fn run_platform_build(path: &Path, args: BuildArgs) -> Result<Value> {
    let (platform, arch) = args.platform.or_target(args.target).resolve()?;
    if platform == tilo_app_cli::PackagePlatform::Android {
        return build_android(path, &args, arch);
    }
    if platform != tilo_app_cli::PackagePlatform::Web {
        return build_native(path, &args, platform, arch);
    }
    if args.profile.is_some() || args.static_link {
        anyhow::bail!("--profile and --static are for a native build");
    }
    let build = tilo_app_cli::web::build_web(
        path,
        &tilo_app_cli::web::WebBuildRequest {
            output: args.output,
            app_only: args.app_only,
        },
    )?;
    eprintln!(
        "[tilo-app] built {} for the web in {:.1} s: {} ({}, module {} bytes)",
        build.app_id,
        build.elapsed.as_secs_f64(),
        build.output.display(),
        build.entry,
        build.module_bytes
    );
    Ok(build.to_json())
}

/// A debug-signed APK under `target/android/`, copied to `--output`.
fn build_android(
    path: &Path,
    args: &BuildArgs,
    arch: Option<tilo_app_cli::PackageArch>,
) -> Result<Value> {
    if args.app_only || args.static_link {
        anyhow::bail!("--app-only and --static are not for --platform android");
    }
    let build = tilo_app_cli::android::build(
        path,
        &tilo_app_cli::android::BuildRequest {
            arch: arch.unwrap_or(tilo_app_cli::PackageArch::Aarch64),
            profile: args.profile.clone().unwrap_or_else(|| "dev".to_owned()),
            signing: tilo_app_cli::android::Signing::Debug,
            bundle: false,
            features: Vec::new(),
            no_default_features: false,
        },
    )?;
    let mut json = build.to_json();
    if let Some(output) = &args.output {
        let output = if output.is_absolute() {
            output.clone()
        } else {
            build.app_dir.join(output)
        };
        std::fs::create_dir_all(&output)
            .with_context(|| format!("creating {}", output.display()))?;
        let name = build
            .apk
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("{} has no file name", build.apk.display()))?;
        let destination = output.join(name);
        std::fs::copy(&build.apk, &destination).with_context(|| {
            format!(
                "copying {} to {}",
                build.apk.display(),
                destination.display()
            )
        })?;
        if let Some(fields) = json.as_object_mut() {
            fields.insert("copied".to_owned(), json!([destination]));
        }
    }
    eprintln!(
        "[tilo-app] built {} for android ({}) in {:.1} s: {}",
        build.metadata.application_id,
        build.arch.android_abi(),
        build.elapsed.as_secs_f64(),
        build.apk.display()
    );
    Ok(json)
}

fn build_native(
    path: &Path,
    args: &BuildArgs,
    platform: tilo_app_cli::PackagePlatform,
    arch: Option<tilo_app_cli::PackageArch>,
) -> Result<Value> {
    if args.app_only {
        anyhow::bail!("--app-only is for --platform web");
    }
    let build = tilo_app_cli::build_native(
        path,
        &tilo_app_cli::PackageRequest {
            sdk: None,
            output: None,
            identity: None,
            profile: args.profile.clone(),
            static_link: args.static_link,
            target: None,
            platform,
            arch,
            features: Vec::new(),
            no_default_features: false,
            force: false,
        },
    )?;
    let mut json = build.to_json();
    if let Some(output) = &args.output {
        let output = if output.is_absolute() {
            output.clone()
        } else {
            build.app_dir.join(output)
        };
        std::fs::create_dir_all(&output)
            .with_context(|| format!("creating {}", output.display()))?;
        let mut copied = Vec::new();
        for (_, executable) in &build.executables {
            let name = executable
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("{} has no file name", executable.display()))?;
            let destination = output.join(name);
            std::fs::copy(executable, &destination).with_context(|| {
                format!(
                    "copying {} to {}",
                    executable.display(),
                    destination.display()
                )
            })?;
            copied.push(destination);
        }
        if let Some(fields) = json.as_object_mut() {
            fields.insert("copied".to_owned(), json!(copied));
        }
    }
    for (bin, executable) in &build.executables {
        eprintln!(
            "[tilo-app] built {bin} for {} ({}) in {:.1} s: {}",
            platform.cli_name(),
            build.triple.as_deref().unwrap_or("host"),
            build.elapsed.as_secs_f64(),
            executable.display()
        );
    }
    Ok(json)
}

fn run_shot(args: ShotArgs) -> Result<Value> {
    let path = args.path.map_or_else(std::env::current_dir, Ok)?;
    let output = args.output.clone();
    let plan = tilo_app_cli::plan_app_shot(
        &path,
        tilo_app_cli::ShotRequest {
            output: args.output,
            viewport: args.viewport,
            scenario: args.scenario,
            forwarded: args.forwarded,
        },
    )?;
    plan.execute()?;
    Ok(json!({"app_dir": plan.app_dir, "output": plan.app_dir.join(output)}))
}

fn run_inspect(args: InspectArgs) -> Result<Value> {
    let path = args.path.map_or_else(std::env::current_dir, Ok)?;
    let artifacts = json!({
        "output": args.output,
        "dump": args.dump,
        "targets": args.targets,
        "audit_report": args.audit_report,
    });
    let plan = tilo_app_cli::plan_app_inspection(
        &path,
        tilo_app_cli::InspectRequest {
            output: args.output,
            viewport: args.viewport,
            actions: args.actions,
            dump: args.dump,
            targets: args.targets,
            audit: args.audit,
            audit_report: args.audit_report,
        },
    )?;
    plan.execute()?;
    Ok(json!({"app_dir": plan.app_dir, "relative_to_app_dir": artifacts}))
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

fn run_app_task(args: AppTaskArgs, task: tilo_app_cli::AppTask) -> Result<Value> {
    let path = args.path.map_or_else(std::env::current_dir, Ok)?;
    let plan = tilo_app_cli::plan_app_task(&path, task, &args.forwarded)?;
    plan.execute()?;
    Ok(json!({"app_dir": plan.app_dir}))
}

fn run_doctor(args: DoctorArgs) -> Result<Value> {
    if let Some(ToolchainArg::Android) = args.platform {
        let report = tilo_app_cli::android::doctor();
        println!("{}", report.summary());
        if !report.is_ready() {
            return Err(failed(
                "the Android toolchain is incomplete",
                report.to_json(),
            ));
        }
        return Ok(report.to_json());
    }
    let path = args.path.map_or_else(std::env::current_dir, Ok)?;
    let report = tilo_app_cli::doctor_app(&path)?;
    println!("{}", report.summary());
    if !report.is_ready() {
        return Err(failed("app checks failed", report.to_json()));
    }
    Ok(report.to_json())
}

fn run_setup(args: &SetupArgs) -> Result<Value> {
    let ToolchainArg::Android = args.platform;
    let report = tilo_app_cli::android::setup(tilo_app_cli::android::SetupOptions {
        accept_licenses: args.accept_licenses,
    })?;
    println!("{}", report.summary());
    if !report.is_ready() {
        return Err(failed(
            "the Android toolchain is still incomplete",
            report.to_json(),
        ));
    }
    Ok(report.to_json())
}

fn run_check(args: CheckArgs) -> Result<Value> {
    let path = args.path.clone().map_or_else(std::env::current_dir, Ok)?;
    if args.lints.all {
        return lint_workspace(&path);
    }
    if !args.target.is_empty() {
        return check_targets(&path, &args);
    }
    let mut stages = Vec::new();
    println!("[check] lint: {}", tilo_app_cli::portability::LINT);
    let lint = tilo_app_cli::lint_portability(&path)?;
    print!("{}", lint.render());
    stages.push(json!({"stage": "lint", "passed": lint.passed(), "report": lint.to_json()}));
    if !lint.passed() {
        return Err(failed(
            format!("lint `{}` failed", tilo_app_cli::portability::LINT),
            json!({"stages": stages}),
        ));
    }
    check_contract(&path, &mut stages)?;
    if args.lints.lint {
        return Ok(json!({"stages": stages}));
    }

    println!("[check] app structure");
    let report = tilo_app_cli::doctor_app(&path)?;
    println!("{}", report.summary());
    stages
        .push(json!({"stage": "doctor", "passed": report.is_ready(), "report": report.to_json()}));
    if !report.is_ready() {
        return Err(failed("app checks failed", json!({"stages": stages})));
    }

    check_runtime_fonts(&mut stages)?;

    let format_mode = if args.fix {
        tilo_app_cli::AppFormatMode::Apply
    } else {
        tilo_app_cli::AppFormatMode::Check
    };
    let quality = tilo_app_cli::plan_app_quality(&path, &args.forwarded, format_mode)?;
    if args.fix {
        println!("[check] Rust formatting (applying fixes)");
    } else {
        println!("[check] Rust formatting");
    }
    let tool_stages: [CheckStage<'_>; 5] = [
        ("format", "Rust formatting", &|| quality.format.execute()),
        ("clippy", "app-local Clippy", &|| quality.clippy.execute()),
        (
            "test",
            "Rust tests and generated accessibility contract",
            &|| quality.test.execute(),
        ),
        ("surface", "public surface", &|| check_surface(&path)),
        ("certify", "Tilo-native runtime certification", &|| {
            certify_one(&path)
        }),
    ];
    for (index, (stage, label, run)) in tool_stages.iter().enumerate() {
        if index > 0 {
            println!("[check] {label}");
        }
        if let Err(error) = run() {
            stages.push(json!({"stage": stage, "passed": false}));
            return Err(error.context(failed(format!("{label} failed"), json!({"stages": stages}))));
        }
        stages.push(json!({"stage": stage, "passed": true}));
    }

    check_other_platforms(&path, &args.platforms, args.arch, &mut stages)?;

    if !args.no_scenarios {
        println!("[check] isolated accessibility-audited scenarios");
        let plan = tilo_app_cli::plan_scenario_suite(&tilo_app_cli::ScenarioRunRequest {
            start: path,
            name: None,
            viewport: args.viewport,
            artifacts: args.artifacts,
            audit: true,
        })?;
        match execute_scenario_plan(&plan) {
            Ok(report) => {
                stages.push(json!({"stage": "scenarios", "passed": true, "report": report}));
            }
            Err(error) => {
                let report = error
                    .downcast_ref::<Failed>()
                    .map(|failed| failed.report.clone());
                stages.push(json!({"stage": "scenarios", "passed": false, "report": report}));
                return Err(failed(format!("{error:#}"), json!({"stages": stages})));
            }
        }
    }
    println!("All app checks passed");
    Ok(json!({"stages": stages}))
}

/// The surface stage of `check`: the app's binary reports its surface and
/// the controls its view binds, compared with the committed `surface.toml`.
fn check_surface(path: &Path) -> Result<()> {
    match tilo_app_cli::surface::check_app(path, None, tilo_app_cli::contract::Level::Error)? {
        None => {
            println!("the app runs no framework resource, so it reports no surface");
            Ok(())
        }
        Some(report) => {
            print!("{}", report.render());
            anyhow::ensure!(report.passed(), "the public surface check failed");
            Ok(())
        }
    }
}

/// `tilo-app surface`: commit the app's public surface.
fn run_surface(args: SurfaceArgs) -> Result<Value> {
    let path = args.path.map_or_else(std::env::current_dir, Ok)?;
    let (file, report) = tilo_app_cli::surface::write_app(&path, args.report.as_deref())?;
    println!("Wrote {}", file.display());
    for unbound in &report.unbound {
        println!("warning: {unbound}");
    }
    Ok(json!({"file": file, "resources": report.resources.len(), "unbound": report.unbound}))
}

/// The app contract stage of `check`: verb determinism, label keys and
/// complete translations.
fn check_contract(path: &Path, stages: &mut Vec<Value>) -> Result<()> {
    println!("[check] {}", tilo_app_cli::contract::CHECK);
    let contract = tilo_app_cli::contract::check_app(path)?;
    print!("{}", contract.render());
    stages.push(
        json!({"stage": "contract", "passed": contract.passed(), "report": contract.to_json()}),
    );
    if !contract.passed() {
        return Err(failed(
            format!("{} failed", tilo_app_cli::contract::CHECK),
            json!({"stages": stages}),
        ));
    }
    Ok(())
}

fn check_runtime_fonts(stages: &mut Vec<Value>) -> Result<()> {
    println!("[check] runtime fonts");
    if let Err(error) = tilo_app_cli::native_libs::require_fonts() {
        stages.push(json!({"stage": "fonts", "passed": false}));
        return Err(error.context(failed(
            "runtime fonts preflight failed",
            json!({"stages": stages}),
        )));
    }
    stages.push(json!({"stage": "fonts", "passed": true}));
    Ok(())
}

/// `check --target`: the SDK targets' declared platforms, capabilities and
/// `cargo check`, and Clippy for `android`.
fn check_targets(path: &Path, args: &CheckArgs) -> Result<Value> {
    let targets: Vec<_> = args
        .target
        .iter()
        .filter_map(|target| target.sdk())
        .collect();
    if !targets.is_empty() {
        tilo_app_cli::targets::check_targets(path, &targets, args.allow_planned)?;
    }
    if args
        .target
        .iter()
        .any(|target| matches!(target, TargetArg::Android))
    {
        let (_, arch) = platform_and_arch(PlatformArg::Android, args.arch)?;
        tilo_app_cli::android::clippy(path, arch.unwrap_or(tilo_app_cli::PackageArch::Aarch64))?;
    }
    Ok(
        json!({"targets": args.target.iter().map(|target| format!("{target:?}")).collect::<Vec<_>>()}),
    )
}

/// The `check --platform` stages, appended to `stages`: Clippy for each other
/// native target, and a web build for `web`.
fn check_other_platforms(
    path: &Path,
    requested: &[CheckPlatformArg],
    arch: Option<ArchArg>,
    stages: &mut Vec<Value>,
) -> Result<()> {
    for platform in check_platforms(requested) {
        let (label, result) = if platform == tilo_app_cli::PackagePlatform::Web {
            println!("[check] web build");
            (
                "web build".to_owned(),
                tilo_app_cli::web::build_web(path, &tilo_app_cli::web::WebBuildRequest::default())
                    .map(|_| ()),
            )
        } else if platform == tilo_app_cli::PackagePlatform::Android {
            let (_, arch) = platform_and_arch(PlatformArg::Android, arch)?;
            let arch = arch.unwrap_or(tilo_app_cli::PackageArch::Aarch64);
            let label = format!("Clippy for android ({})", arch.android_abi());
            println!("[check] {label}");
            (label, tilo_app_cli::android::clippy(path, arch))
        } else {
            let (_, arch) = platform_and_arch(PlatformArg::Tilo, arch)?;
            let Some(plan) = tilo_app_cli::plan_platform_clippy(path, platform, arch)? else {
                continue;
            };
            let label = format!("Clippy for {}", platform.cli_name());
            println!("[check] {label}");
            (label, plan.execute())
        };
        let stage = format!("platform:{}", platform.cli_name());
        if let Err(error) = result {
            stages.push(json!({"stage": stage, "passed": false}));
            return Err(error.context(failed(format!("{label} failed"), json!({"stages": stages}))));
        }
        stages.push(json!({"stage": stage, "passed": true}));
    }
    Ok(())
}

/// The platforms `check --platform` names, each once, in `ALL` order.
fn check_platforms(requested: &[CheckPlatformArg]) -> Vec<tilo_app_cli::PackagePlatform> {
    use tilo_app_cli::PackagePlatform;
    PackagePlatform::ALL
        .into_iter()
        .filter(|platform| {
            requested.iter().any(|arg| {
                *arg == CheckPlatformArg::All
                    || matches!(
                        (arg, platform),
                        (CheckPlatformArg::Tilo, PackagePlatform::Tilo)
                            | (CheckPlatformArg::Linux, PackagePlatform::Linux)
                            | (CheckPlatformArg::Windows, PackagePlatform::Windows)
                            | (CheckPlatformArg::Macos, PackagePlatform::Macos)
                            | (CheckPlatformArg::Web, PackagePlatform::Web)
                            | (CheckPlatformArg::Android, PackagePlatform::Android)
                    )
            })
        })
        .collect()
}

/// A `check` stage: its JSON name, its progress label, and the work.
type CheckStage<'a> = (&'static str, &'static str, &'a dyn Fn() -> Result<()>);

fn lint_workspace(start: &Path) -> Result<Value> {
    let apps = tilo_app_cli::portability::workspace_apps(start)?;
    let mut failures = Vec::new();
    let mut reports = Vec::new();
    for app in &apps {
        let report = tilo_app_cli::portability::lint_app(app)?;
        reports.push(report.to_json());
        if report.passed() {
            continue;
        }
        println!(
            "[check] lint: {} in {}",
            tilo_app_cli::portability::LINT,
            app.display()
        );
        println!("{}", report.render());
        failures.push(app.display().to_string());
    }
    let mut contracts = Vec::new();
    for app in &apps {
        let report = tilo_app_cli::contract::check_app(app)?;
        contracts.push(json!({"app": app, "findings": report.to_json()}));
        if report.passed() {
            continue;
        }
        println!(
            "[check] {} in {}",
            tilo_app_cli::contract::CHECK,
            app.display()
        );
        print!("{}", report.render());
        failures.push(app.display().to_string());
    }
    let summary = json!({"apps": reports, "contracts": contracts});
    if !failures.is_empty() {
        return Err(failed(
            format!(
                "lint `{}` or the {} failed for {} of {} app(s): {}",
                tilo_app_cli::portability::LINT,
                tilo_app_cli::contract::CHECK,
                failures.len(),
                apps.len(),
                failures.join(", ")
            ),
            summary,
        ));
    }
    println!(
        "lint `{}` and the {} passed for {} app(s)",
        tilo_app_cli::portability::LINT,
        tilo_app_cli::contract::CHECK,
        apps.len()
    );
    Ok(summary)
}

/// The surface contract of an app certified with the workspace: what its
/// built binary reports against its committed `surface.toml`. An app that
/// has not committed one yet is told so without failing.
fn certify_surface(app: &Path, executable: &Path) -> Result<()> {
    let Some(findings) = tilo_app_cli::surface::check_app(
        app,
        Some(executable),
        tilo_app_cli::contract::Level::Warning,
    )?
    else {
        return Ok(());
    };
    print!("{}", findings.render());
    anyhow::ensure!(
        findings.passed(),
        "{} failed its public surface check",
        app.display()
    );
    Ok(())
}

fn certify_one(path: &Path) -> Result<()> {
    tilo_app_cli::plan_app_task(
        path,
        tilo_app_cli::AppTask::Run,
        &[OsString::from("--native-audit")],
    )?
    .execute()
    .context("production app root failed Tilo-native certification")
}

#[allow(
    clippy::too_many_lines,
    reason = "workspace certification deliberately keeps discovery, one unified build, and execution in one auditable flow"
)]
fn run_certify(args: CertifyArgs) -> Result<Value> {
    let path = args.path.map_or_else(std::env::current_dir, Ok)?;
    if !args.workspace {
        certify_one(&path)?;
        println!("Tilo-native certification passed");
        return Ok(json!({"certified": [path]}));
    }

    let apps = path.join("apps");
    anyhow::ensure!(
        apps.is_dir(),
        "{} is not a Tilo workspace root",
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
            .parse::<toml::Table>()
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
    if !args.packages.is_empty() {
        packages.retain(|(_, package, _)| args.packages.contains(package));
        app_dirs = packages.iter().map(|(app, _, _)| app.clone()).collect();
        if packages.is_empty() {
            println!("Certified 0 graphical Tilo applications");
            return Ok(json!({"certified": app_dirs}));
        }
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

    let responsive_root = args.responsive_artifacts.map(|artifacts| {
        if artifacts.is_absolute() {
            artifacts
        } else {
            path.join(artifacts)
        }
    });
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
            "{} production root failed Tilo-native certification",
            app.display()
        );
        certify_surface(app, executable)?;
        if let Some(root) = responsive_root.as_deref() {
            let app_name = app.file_name().unwrap_or_default();
            for run in responsive_acceptance_runs(&root.join(app_name)) {
                println!("[responsive] {} / {}", app_name.to_string_lossy(), run.name);
                let mut command = ProcessCommand::new(executable);
                command.current_dir(app).args(&run.args);
                if let Some(theme) = run.shot_theme {
                    command.env("TILO_SHOTS_THEME", theme);
                }
                let status = command.status().with_context(|| {
                    format!(
                        "running {} responsive acceptance mode {}",
                        executable.display(),
                        run.name
                    )
                })?;
                anyhow::ensure!(
                    status.success(),
                    "{} failed responsive acceptance mode {}",
                    app.display(),
                    run.name
                );
                validate_responsive_pngs(&root.join(app_name).join(run.name)).with_context(
                    || {
                        format!(
                            "{} produced unusable responsive images in mode {}",
                            app.display(),
                            run.name
                        )
                    },
                )?;
                for (viewport_name, (width, height)) in RESPONSIVE_AUDIT_VIEWPORTS {
                    let audit_path = root
                        .join(app_name)
                        .join(run.name)
                        .join(format!("{viewport_name}.audit.json"));
                    let mut audit = ProcessCommand::new(executable);
                    audit.current_dir(app).args([
                        OsString::from("--native-audit"),
                        OsString::from("--viewport"),
                        OsString::from(format!("{width}x{height}")),
                    ]);
                    audit.args(&run.audit_extra);
                    if let Some(theme) = run.shot_theme {
                        audit.env("TILO_SHOTS_THEME", theme);
                    }
                    let output = audit.output().with_context(|| {
                        format!(
                            "auditing {} responsive mode {} at {viewport_name}",
                            executable.display(),
                            run.name
                        )
                    })?;
                    anyhow::ensure!(
                        output.status.success(),
                        "{} failed semantic audit mode {} at {viewport_name}: {}",
                        app.display(),
                        run.name,
                        String::from_utf8_lossy(&output.stderr).trim()
                    );
                    if let Some(parent) = audit_path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(&audit_path, output.stdout).with_context(|| {
                        format!("writing responsive audit {}", audit_path.display())
                    })?;
                }
            }
        }
    }
    println!("Certified {} graphical Tilo applications", app_dirs.len());
    Ok(json!({"certified": app_dirs}))
}

struct ResponsiveAcceptanceRun {
    name: &'static str,
    args: Vec<OsString>,
    audit_extra: Vec<OsString>,
    shot_theme: Option<&'static str>,
}

const RESPONSIVE_AUDIT_VIEWPORTS: [(&str, (u32, u32)); 4] = [
    ("phone", (390, 844)),
    ("tablet", (768, 1024)),
    ("laptop", (1366, 768)),
    ("desktop", (1920, 1080)),
];

/// Reject empty or stale responsive captures while allowing sparse empty states.
/// A frame fails when one exact colour occupies more than 99.9% of its pixels;
/// dimensions are checked against the expected viewport as well.
fn validate_responsive_pngs(mode_root: &Path) -> Result<()> {
    for (viewport_name, expected) in RESPONSIVE_AUDIT_VIEWPORTS {
        let suffix = format!("-{viewport_name}.png");
        let paths = std::fs::read_dir(mode_root)
            .with_context(|| format!("reading responsive images in {}", mode_root.display()))?
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(&suffix))
            })
            .collect::<Vec<_>>();
        anyhow::ensure!(
            paths.len() == 1,
            "expected one {viewport_name} PNG in {}, found {}",
            mode_root.display(),
            paths.len()
        );
        let [path] = paths.as_slice() else {
            anyhow::bail!("responsive PNG count changed after validation")
        };
        validate_responsive_png(path, expected)?;
    }
    Ok(())
}

fn validate_responsive_png(path: &Path, expected: (u32, u32)) -> Result<()> {
    use image::GenericImageView as _;

    let image = image::open(path).with_context(|| format!("decoding {}", path.display()))?;
    anyhow::ensure!(
        image.dimensions() == expected,
        "{} has dimensions {:?}, expected {:?}",
        path.display(),
        image.dimensions(),
        expected
    );
    let pixels = image.to_rgba8();
    let total = u64::from(expected.0)
        .checked_mul(u64::from(expected.1))
        .context("responsive viewport area overflowed")?;
    anyhow::ensure!(total > 0, "responsive viewport must not be empty");
    let mut colours = std::collections::HashMap::<[u8; 4], u64>::new();
    for pixel in pixels.pixels() {
        let count = colours.entry(pixel.0).or_default();
        *count = count.saturating_add(1);
    }
    let dominant = colours.values().copied().max().unwrap_or(total);
    let dominant_hundredths = dominant
        .saturating_mul(10_000)
        .checked_div(total)
        .unwrap_or(10_000);
    anyhow::ensure!(
        dominant.saturating_mul(10_000) <= total.saturating_mul(9_990),
        "{} is effectively blank ({}.{:02}% one colour)",
        path.display(),
        dominant_hundredths.div_euclid(100),
        dominant_hundredths.rem_euclid(100),
    );
    Ok(())
}

fn responsive_acceptance_runs(app_root: &Path) -> Vec<ResponsiveAcceptanceRun> {
    let run = |name: &'static str,
               extra: &[&str],
               shot_theme: Option<&'static str>|
     -> ResponsiveAcceptanceRun {
        let mut args = vec![
            OsString::from("--responsive-matrix"),
            app_root.join(name).into_os_string(),
        ];
        let audit_extra = extra.iter().map(OsString::from).collect::<Vec<_>>();
        args.extend(audit_extra.iter().cloned());
        ResponsiveAcceptanceRun {
            name,
            args,
            audit_extra,
            shot_theme,
        }
    };
    vec![
        run("default", &[], None),
        run("expanded-copy", &["--pseudo-locale"], None),
        run("large-type", &["--text-scale", "2"], None),
        run("high-contrast", &[], Some("high-contrast")),
    ]
}

fn run_new(args: NewArgs, interactive: bool) -> Result<Value> {
    if args.list_templates {
        return Ok(list_templates());
    }
    let start_dev = args.dev;
    let request = tilo_app_cli::NewAppRequest {
        name: args.name,
        app_id: args.id,
        title: args.title,
        template: args.template.map(TemplateArg::template).transpose()?,
        example: args
            .from_example
            .as_deref()
            .map(tilo_app_cli::examples::find)
            .transpose()?,
        directory: args.directory,
        ui_path: args.ui_path,
        sdk_git: args.sdk_git,
        sdk_rev: args.sdk_rev,
        sdk_version: args.sdk_version,
        dry_run: args.dry_run,
    };
    let outcome = if interactive {
        tilo_app_cli::create_app_interactive(request)?
    } else {
        tilo_app_cli::create_app(request)?
    };
    let report = outcome.to_json();
    if start_dev {
        println!("{}", outcome.headline());
        println!("\nStarting live development; press Ctrl-C to stop.");
        tilo_app_cli::dev_app(
            &outcome.destination,
            &tilo_app_cli::DevRequest {
                forwarded: Vec::new(),
                watch_ui: true,
                debounce: Duration::from_millis(tilo_app_cli::DEV_DEBOUNCE_MS),
                timings: false,
                profile: None,
            },
        )?;
        Ok(report)
    } else {
        println!("{}", outcome.summary());
        Ok(report)
    }
}

fn list_templates() -> Value {
    let templates = tilo_app_cli::templates_json();
    for template in templates.as_array().into_iter().flatten() {
        let name = template["name"].as_str().unwrap_or_default();
        let default = if template["default"] == true {
            " (default)"
        } else {
            ""
        };
        println!(
            "{name:<11} {}{default}",
            template["description"].as_str().unwrap_or_default()
        );
    }
    println!("\nCreate one with `tilo-app new NAME --template NAME`.");
    json!({ "templates": templates })
}

fn run_examples(name: Option<&str>, file: Option<&str>, human: bool) -> Result<Value> {
    let Some(name) = name else {
        return Ok(list_examples(human));
    };
    let details = tilo_app_cli::examples::details(name, file)?;
    if human {
        if let Some(content) = details.get("content").and_then(Value::as_str) {
            print!("{content}");
        } else {
            let example = tilo_app_cli::examples::find(name)?;
            println!(
                "{} — {}\n{}",
                example.name, example.concept, example.summary
            );
            println!("\nStart with `tilo-app new NAME --from-example {name}`.");
            println!("Read a file with `tilo-app examples {name} --file PATH`:\n");
            for (path, _) in example.files {
                println!("  {path}");
            }
        }
    }
    Ok(details)
}

fn list_examples(human: bool) -> Value {
    if human {
        for example in tilo_app_cli::examples::ALL {
            let web = if example.web { "" } else { " (native only)" };
            println!("{:<17} {}{web}", example.name, example.concept);
            println!("{:<17} {}", "", example.summary);
        }
        println!(
            "\nStart an app from one with `tilo-app new NAME --from-example EXAMPLE`; read one in examples/apps/."
        );
    }
    json!({ "examples": tilo_app_cli::examples::examples_json() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_macos_requires_explicit_existing_inputs_and_output() {
        let cli = Cli::try_parse_from([
            "tilo-app",
            "bundle-macos",
            "app with spaces",
            "--binary",
            "app binary",
            "--runtime-host",
            "helper binary",
            "--output",
            "new output",
        ])
        .unwrap();
        assert_eq!(cli.command.name(), "bundle-macos");
        let Command::BundleMacos(args) = cli.command else {
            panic!("expected bundle command")
        };
        assert_eq!(args.binary, PathBuf::from("app binary"));
        assert_eq!(args.runtime_host, PathBuf::from("helper binary"));
        assert!(Cli::try_parse_from(["tilo-app", "bundle-macos", "."]).is_err());
    }

    /// new starts from an example instead of a template, never both, and examples are listed.
    #[test]
    fn new_starts_from_an_example_instead_of_a_template() {
        let cli = Cli::try_parse_from(["tilo-app", "new", "notes", "--from-example", "counter"])
            .expect("from-example command");
        let Command::New(args) = cli.command else {
            panic!("expected new command");
        };
        assert_eq!(args.from_example.as_deref(), Some("counter"));
        for conflicting in [
            [
                "tilo-app",
                "new",
                "notes",
                "--from-example",
                "counter",
                "--template",
                "starter",
            ]
            .as_slice(),
            [
                "tilo-app",
                "new",
                "--list-templates",
                "--from-example",
                "counter",
            ]
            .as_slice(),
        ] {
            assert!(Cli::try_parse_from(conflicting).is_err(), "{conflicting:?}");
        }
        let listed = list_examples(false);
        assert_eq!(
            listed["examples"].as_array().map(Vec::len),
            Some(tilo_app_cli::examples::ALL.len())
        );
    }

    /// new offers each template by name, points a retired template at the example that replaced
    /// it, lists templates only on their own, and can enter dev but cannot dev a dry run.
    #[test]
    fn new_exposes_every_app_template_and_refuses_to_dev_a_dry_run() {
        // Phase: new_accepts_every_template_by_name.
        {
            for template in tilo_app_cli::Template::ALL {
                let cli = Cli::try_parse_from([
                    "tilo-app",
                    "new",
                    "notes",
                    "--template",
                    template.name(),
                ])
                .expect("template command");
                let Command::New(args) = cli.command else {
                    panic!("expected new command");
                };
                let parsed = args.template.expect("a template").template().unwrap();
                assert_eq!(parsed, template);
            }
        }

        // Phase: a_retired_template_names_its_example.
        {
            for (retired, example) in [
                ("list", "virtual-list"),
                ("data", "data-table"),
                ("tree", "tree-browser"),
                ("workspace", "tree-browser"),
                ("async", "background-task"),
                ("qr", "qr-code"),
            ] {
                let cli = Cli::try_parse_from(["tilo-app", "new", "notes", "--template", retired])
                    .expect("retired template command still parses");
                let Command::New(args) = cli.command else {
                    panic!("expected new command");
                };
                let error = args.template.expect("a template").template().unwrap_err();
                assert!(
                    error
                        .to_string()
                        .contains(&format!("--from-example {example}")),
                    "{error}"
                );
            }
        }

        // Phase: listing_templates_stands_alone.
        {
            let cli = Cli::try_parse_from(["tilo-app", "new", "--list-templates"])
                .expect("list templates command");
            let Command::New(args) = cli.command else {
                panic!("expected new command");
            };
            assert!(args.list_templates);
            assert!(Cli::try_parse_from(["tilo-app", "new", "notes", "--list-templates"]).is_err());
            let listed = list_templates();
            let names: Vec<_> = listed["templates"]
                .as_array()
                .expect("a template list")
                .iter()
                .map(|template| template["name"].as_str().expect("a name"))
                .collect();
            assert_eq!(names, ["starter", "document", "collection", "graph"]);
        }

        // Phase: new_can_enter_dev_but_cannot_dev_a_dry_run.
        {
            let cli = Cli::try_parse_from(["tilo-app", "new", "notes", "--dev"])
                .expect("create-and-develop command");
            let Command::New(args) = cli.command else {
                panic!("expected new command");
            };
            assert!(args.dev);
            assert!(!args.dry_run);

            assert!(
                Cli::try_parse_from(["tilo-app", "new", "notes", "--dev", "--dry-run"]).is_err()
            );
        }
    }

    /// add commands expose safe repeatable generation options and explicit, scoped component mount
    /// options, and the reusable list, table and advanced text templates.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn add_commands_expose_safe_explicit_generation_options_and_templates() {
        // Phase: add_commands_expose_safe_repeatable_generation_options.
        {
            let screen = Cli::try_parse_from([
                "tilo-app",
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
                "tilo-app",
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
                "tilo-app",
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

            let locale = Cli::try_parse_from(["tilo-app", "add", "locale", "de-DE", "--force"])
                .expect("locale command");
            let Command::Add(AddArgs {
                command: Some(AddCommand::Locale { locale, options }),
            }) = locale.command
            else {
                panic!("expected add locale command");
            };
            assert_eq!(locale, "de-DE");
            assert!(options.force);

            let wizard = Cli::try_parse_from(["tilo-app", "add"]).expect("add wizard command");
            let Command::Add(AddArgs { command: None }) = wizard.command else {
                panic!("expected interactive add command");
            };
        }

        // Phase: add_component_mount_options_are_explicit_and_scoped.
        {
            let mounted = Cli::try_parse_from([
                "tilo-app",
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
                    "tilo-app",
                    "add",
                    "component",
                    "activity-card",
                    "--parent",
                    "content",
                ])
                .is_err()
            );
        }

        // Phase: add_command_exposes_the_reusable_list_template.
        {
            let cli = Cli::try_parse_from([
                "tilo-app",
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

        // Phase: add_command_exposes_the_reusable_table_template.
        {
            let cli = Cli::try_parse_from([
                "tilo-app",
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

        // Phase: add_command_exposes_the_advanced_text_template.
        {
            let cli = Cli::try_parse_from([
                "tilo-app",
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
    }

    /// locale sync exposes preservation and explicit repair options; run and test accept a descendant
    /// path and forwarded arguments; certify accepts one app or a workspace root.
    #[test]
    fn maintenance_commands_accept_explicit_repairs_and_app_paths() {
        // Phase: locale_sync_exposes_preservation_and_explicit_repair_options.
        {
            let cli = Cli::try_parse_from([
                "tilo-app",
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

        // Phase: run_and_test_accept_a_descendant_path_and_forwarded_arguments.
        {
            let run = Cli::try_parse_from([
                "tilo-app",
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

            let test =
                Cli::try_parse_from(["tilo-app", "test", "--", "--nocapture"]).expect("test CLI");
            let Command::Test(test) = test.command else {
                panic!("expected test command");
            };
            assert_eq!(test.path, None);
            assert_eq!(test.forwarded, [OsString::from("--nocapture")]);
        }

        // Phase: certify_accepts_one_app_or_a_workspace_root.
        {
            let app = Cli::try_parse_from(["tilo-app", "certify", "apps/demo/src"])
                .expect("single-app certification CLI");
            let Command::Certify(app) = app.command else {
                panic!("expected certify command");
            };
            assert_eq!(app.path, Some(PathBuf::from("apps/demo/src")));
            assert!(!app.workspace);
            assert!(app.responsive_artifacts.is_none());

            let workspace = Cli::try_parse_from([
                "tilo-app",
                "certify",
                ".",
                "--workspace",
                "--responsive-artifacts",
                "target/acceptance",
            ])
            .expect("workspace certification CLI");
            let Command::Certify(workspace) = workspace.command else {
                panic!("expected certify command");
            };
            assert_eq!(workspace.path, Some(PathBuf::from(".")));
            assert!(workspace.workspace);
            assert_eq!(
                workspace.responsive_artifacts,
                Some(PathBuf::from("target/acceptance"))
            );

            assert!(
                Cli::try_parse_from([
                    "tilo-app",
                    "certify",
                    "apps/demo",
                    "--responsive-artifacts",
                    "target/acceptance",
                ])
                .is_err(),
                "a fleet artifact request without --workspace is ambiguous"
            );
        }
    }

    /// Responsive acceptance covers shape, copy, type and contrast, and the PNG gate rejects blank or
    /// wrong-sized frames.
    #[test]
    fn responsive_acceptance_covers_the_layout_and_rejects_bad_frames() {
        // Phase: responsive_acceptance_covers_shape_copy_type_and_contrast.
        {
            let runs = responsive_acceptance_runs(Path::new("/tmp/acceptance/browser"));
            assert_eq!(
                runs.iter().map(|run| run.name).collect::<Vec<_>>(),
                ["default", "expanded-copy", "large-type", "high-contrast"]
            );
            assert!(runs.iter().all(|run| {
                run.args.first() == Some(&OsString::from("--responsive-matrix"))
                    && run
                        .args
                        .get(1)
                        .is_some_and(|path| Path::new(path).is_absolute())
            }));
            assert!(runs[1].args.contains(&OsString::from("--pseudo-locale")));
            assert_eq!(runs[1].audit_extra, [OsString::from("--pseudo-locale")]);
            assert!(
                runs[2]
                    .args
                    .windows(2)
                    .any(|pair| { pair == [OsString::from("--text-scale"), OsString::from("2")] })
            );
            assert_eq!(RESPONSIVE_AUDIT_VIEWPORTS[0], ("phone", (390, 844)));
            assert_eq!(
                RESPONSIVE_AUDIT_VIEWPORTS.last(),
                Some(&("desktop", (1920, 1080)))
            );
            assert_eq!(runs[3].shot_theme, Some("high-contrast"));
        }

        // Phase: responsive_png_gate_rejects_blank_or_wrong_sized_frames.
        {
            let dir = tempfile::tempdir().expect("temporary screenshots");
            let blank_path = dir.path().join("blank-phone.png");
            image::RgbaImage::from_pixel(100, 80, image::Rgba([12, 13, 15, 255]))
                .save(&blank_path)
                .expect("write blank PNG");
            assert!(validate_responsive_png(&blank_path, (100, 80)).is_err());

            let mut painted = image::RgbaImage::from_pixel(100, 80, image::Rgba([12, 13, 15, 255]));
            for x in 0..10 {
                for y in 0..80 {
                    painted.put_pixel(x, y, image::Rgba([240, 240, 240, 255]));
                }
            }
            let painted_path = dir.path().join("painted-phone.png");
            painted.save(&painted_path).expect("write painted PNG");
            validate_responsive_png(&painted_path, (100, 80)).expect("painted frame is useful");
            assert!(validate_responsive_png(&painted_path, (80, 100)).is_err());

            // A sparse but explicit empty state is useful UI. Even a compact
            // centred title/action treatment occupies little of a desktop frame.
            let mut sparse = image::RgbaImage::from_pixel(100, 100, image::Rgba([12, 13, 15, 255]));
            for x in 47..53 {
                for y in 47..53 {
                    sparse.put_pixel(x, y, image::Rgba([240, 240, 240, 255]));
                }
            }
            let sparse_path = dir.path().join("sparse-empty-state.png");
            sparse.save(&sparse_path).expect("write sparse PNG");
            validate_responsive_png(&sparse_path, (100, 100))
                .expect("explicit sparse empty state is useful");

            let mut near_blank =
                image::RgbaImage::from_pixel(100, 100, image::Rgba([12, 13, 15, 255]));
            near_blank.put_pixel(50, 50, image::Rgba([240, 240, 240, 255]));
            let near_blank_path = dir.path().join("near-blank.png");
            near_blank
                .save(&near_blank_path)
                .expect("write near-blank PNG");
            assert!(validate_responsive_png(&near_blank_path, (100, 100)).is_err());
        }
    }

    /// shot and inspect expose direct artifact workflows; scenario run selects a case and viewport;
    /// check composes tests, audited scenarios and artifacts; dev exposes a bounded restart loop; package
    /// exposes reproducible build and signing options.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn workflow_commands_expose_bounded_reproducible_options() {
        // Phase: shot_and_inspect_expose_direct_artifact_workflows.
        {
            let shot = Cli::try_parse_from([
                "tilo-app",
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
                "tilo-app",
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

            assert!(Cli::try_parse_from(["tilo-app", "inspect", "--viewport", "1280x0"]).is_err());
        }

        // Phase: scenario_run_selects_a_case_and_viewport.
        {
            let cli = Cli::try_parse_from([
                "tilo-app",
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

        // Phase: check_composes_tests_audited_scenarios_and_artifacts.
        {
            let cli = Cli::try_parse_from([
                "tilo-app",
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

        // Phase: dev_exposes_a_bounded_restart_loop.
        {
            let cli = Cli::try_parse_from([
                "tilo-app",
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
            assert!(Cli::try_parse_from(["tilo-app", "dev", "--debounce-ms", "0"]).is_err());
            assert_eq!(dev.profile, None);
            assert_eq!(dev_profile(dev.release, dev.profile), None);
        }

        // Phase: dev_builds_the_app_in_a_chosen_profile.
        {
            let dev = |args: &[&str]| {
                let cli = Cli::try_parse_from(["tilo-app", "dev"].iter().chain(args))?;
                let Command::Dev(dev) = cli.command else {
                    panic!("expected dev command");
                };
                Ok::<_, clap::Error>(dev_profile(dev.release, dev.profile))
            };
            assert_eq!(dev(&["--release"]).unwrap().as_deref(), Some("thin-app"));
            assert_eq!(
                dev(&["--profile", "release"]).unwrap().as_deref(),
                Some("release")
            );
            assert!(dev(&["--release", "--profile", "dev"]).is_err());
        }

        // Phase: every_build_command_takes_platform_and_arch.
        {
            let build = Cli::try_parse_from(["tilo-app", "build"]).expect("build CLI");
            let Command::Build(build) = build.command else {
                panic!("expected build command");
            };
            assert!(build.target.is_none());
            assert!(build.platform.platform.is_none());
            assert!(build.platform.arch.is_none());
            for command in ["run", "dev", "build"] {
                let cli = Cli::try_parse_from([
                    "tilo-app",
                    command,
                    "--platform",
                    "linux",
                    "--arch",
                    "aarch64",
                ])
                .unwrap_or_else(|error| panic!("{command}: {error}"));
                let platform = match cli.command {
                    Command::Run(args) => args.platform,
                    Command::Dev(args) => args.platform,
                    Command::Build(args) => args.platform,
                    _ => panic!("expected {command}"),
                };
                assert!(matches!(platform.platform, Some(PlatformArg::Linux)));
                assert!(matches!(platform.arch, Some(ArchArg::Aarch64)));
            }
            for command in ["run", "build"] {
                assert!(
                    Cli::try_parse_from([
                        "tilo-app",
                        command,
                        "--target",
                        "linux",
                        "--platform",
                        "linux"
                    ])
                    .is_err(),
                    "{command}: --target and --platform name the same thing"
                );
            }
            let check = Cli::try_parse_from([
                "tilo-app",
                "check",
                "--platform",
                "windows",
                "--platform",
                "web",
                "--arch",
                "x86_64",
            ])
            .expect("check CLI");
            let Command::Check(check) = check.command else {
                panic!("expected check command");
            };
            assert_eq!(
                check_platforms(&check.platforms),
                [
                    tilo_app_cli::PackagePlatform::Windows,
                    tilo_app_cli::PackagePlatform::Web
                ]
            );
            assert_eq!(
                check_platforms(&[CheckPlatformArg::Web, CheckPlatformArg::All]),
                tilo_app_cli::PackagePlatform::ALL
            );
            assert!(check_platforms(&[]).is_empty());
        }

        // Phase: package_exposes_reproducible_build_and_signing_options.
        {
            let cli = Cli::try_parse_from([
                "tilo-app",
                "package",
                "apps/demo/src",
                "--output",
                "dist/demo-dev.tapp",
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
            assert_eq!(package.output, Some(PathBuf::from("dist/demo-dev.tapp")));
            assert_eq!(package.identity, Some(PathBuf::from("publisher.json")));
            assert_eq!(package.profile, Some("distribution".into()));
            assert_eq!(package.target.as_deref(), Some("x86_64-unknown-linux-gnu"));
            assert_eq!(package.features, ["sqlite", "telemetry"]);
            assert!(package.no_default_features);
            assert!(package.force);

            let defaults = Cli::try_parse_from(["tilo-app", "package"]).expect("package defaults");
            let Command::Package(defaults) = defaults.command else {
                panic!("expected package command");
            };
            assert_eq!(defaults.profile, None);
            assert!(!defaults.static_link);
            assert!(!defaults.force);
        }
    }

    /// UI builder commands require unambiguous node sources; ui set accepts one typed property edit;
    /// ui apply reads and publishes one complete batch.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn ui_builder_commands_take_unambiguous_sources_and_whole_edits() {
        // Phase: ui_builder_commands_require_unambiguous_node_sources.
        {
            let edit = Cli::try_parse_from([
                "tilo-app",
                "ui",
                "edit",
                "ui/main.json",
                "--builder",
                "/opt/tilo-ui-builder",
            ])
            .expect("UI edit command");
            let Command::Ui(UiArgs {
                command: UiCommand::Edit { file, builder },
            }) = edit.command
            else {
                panic!("expected UI edit command");
            };
            assert_eq!(file, PathBuf::from("ui/main.json"));
            assert_eq!(builder, Some(OsString::from("/opt/tilo-ui-builder")));

            let insert = Cli::try_parse_from([
                "tilo-app",
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
                "tilo-app",
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
                Cli::try_parse_from(["tilo-app", "ui", "replace", "ui/main.json", "--id", "body",])
                    .is_err()
            );
            assert!(
                Cli::try_parse_from([
                    "tilo-app",
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

        // Phase: ui_set_accepts_one_typed_property_edit.
        {
            let set = Cli::try_parse_from([
                "tilo-app",
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
                    "tilo-app",
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
                    "tilo-app",
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
                Cli::try_parse_from(["tilo-app", "ui", "apply", "ui/main.json", "ui/edits.json"])
                    .expect("UI batch command");
            assert!(matches!(
                apply.command,
                Command::Ui(UiArgs {
                    command: UiCommand::Apply { file, edits }
                }) if file.as_path() == std::path::Path::new("ui/main.json")
                    && edits.as_path() == std::path::Path::new("ui/edits.json")
            ));
        }

        // Phase: ui_apply_reads_and_publishes_one_complete_batch.
        {
            let temp = tempfile::tempdir().expect("temp dir");
            let graph = temp.path().join("main.json");
            let edits = temp.path().join("edits.json");
            tilo_app_cli::init_ui_graph(&graph, "Projects", false).unwrap();
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

            let document = tilo_app_cli::load_ui_graph(&graph).unwrap();
            assert!(document.node("open").is_some());
            assert!(
                std::fs::read_to_string(graph)
                    .unwrap()
                    .contains("Recent projects")
            );
        }
    }

    /// `--target android` is the `--platform android` build, beside the SDK's
    /// linux, macos and web targets, and names one build with `--platform`.
    #[test]
    fn android_is_a_target_that_names_the_platform_build() {
        use tilo_app_cli::PackagePlatform;

        let build = Cli::try_parse_from(["tilo-app", "build", "--target", "android"])
            .expect("build --target android");
        let Command::Build(build) = build.command else {
            panic!("expected build command");
        };
        assert!(build.target.and_then(TargetArg::sdk).is_none());
        assert_eq!(
            build.platform.or_target(build.target).resolve().unwrap(),
            (PackagePlatform::Android, None)
        );
        assert!(
            Cli::try_parse_from([
                "tilo-app",
                "run",
                "--target",
                "android",
                "--platform",
                "web"
            ])
            .is_err(),
            "--target and --platform name one build"
        );
    }

    /// Android is one more `--platform`: build, run, dev, package and check
    /// take it, with a device for run and dev, signing and bundle options for
    /// package, and doctor and setup for its toolchain.
    #[test]
    fn android_is_a_platform_of_every_command_and_of_the_toolchain_commands() {
        use tilo_app_cli::{PackageArch, PackagePlatform};

        let run = Cli::try_parse_from([
            "tilo-app",
            "run",
            "--platform",
            "android",
            "--device",
            "emulator-5554",
        ])
        .expect("run --platform android");
        let Command::Run(run) = run.command else {
            panic!("expected run command");
        };
        assert_eq!(
            run.platform.resolve().unwrap(),
            (PackagePlatform::Android, None)
        );
        assert_eq!(run.device.as_deref(), Some("emulator-5554"));

        let dev = Cli::try_parse_from([
            "tilo-app",
            "dev",
            "--platform",
            "android",
            "--arch",
            "x86_64",
            "--device",
            "R5CT",
        ])
        .expect("dev --platform android");
        let Command::Dev(dev) = dev.command else {
            panic!("expected dev command");
        };
        assert_eq!(
            dev.platform.resolve().unwrap(),
            (PackagePlatform::Android, Some(PackageArch::X86_64))
        );
        assert_eq!(dev.device.as_deref(), Some("R5CT"));

        let build = Cli::try_parse_from(["tilo-app", "build", "--platform", "android"])
            .expect("build --platform android");
        let Command::Build(build) = build.command else {
            panic!("expected build command");
        };
        assert_eq!(
            build.platform.resolve().unwrap(),
            (PackagePlatform::Android, None)
        );
        assert!(
            platform_and_arch(PlatformArg::Android, Some(ArchArg::Wasm32)).is_err(),
            "wasm32 is the web's architecture, not Android's"
        );

        let package = Cli::try_parse_from([
            "tilo-app",
            "package",
            "--platform",
            "android",
            "--keystore",
            "release.keystore",
            "--no-bundle",
        ])
        .expect("package --platform android");
        let Command::Package(package) = package.command else {
            panic!("expected package command");
        };
        assert!(matches!(package.platform, PlatformArg::Android));
        assert_eq!(
            package.android.keystore,
            Some(PathBuf::from("release.keystore"))
        );
        assert!(package.android.no_bundle);

        let doctor = Cli::try_parse_from(["tilo-app", "doctor", "--platform", "android"])
            .expect("doctor --platform android");
        let Command::Doctor(doctor) = doctor.command else {
            panic!("expected doctor command");
        };
        assert!(matches!(doctor.platform, Some(ToolchainArg::Android)));

        let setup = Cli::try_parse_from(["tilo-app", "setup", "android", "--accept-licenses"])
            .expect("setup android");
        let Command::Setup(setup) = setup.command else {
            panic!("expected setup command");
        };
        assert!(matches!(setup.platform, ToolchainArg::Android));
        assert!(setup.accept_licenses);

        assert_eq!(
            check_platforms(&[CheckPlatformArg::Android]),
            [PackagePlatform::Android]
        );
        assert!(check_platforms(&[CheckPlatformArg::All]).contains(&PackagePlatform::Android));
    }
    #[test]
    fn compiler_interface_selection_requires_runtime_and_independent_root() {
        for flags in [
            vec!["--sdk-compiler-bundle", "compiler"],
            vec!["--sdk-compiler-root", "root"],
            vec![
                "--sdk-compiler-bundle",
                "compiler",
                "--sdk-compiler-root",
                "root",
            ],
        ] {
            let mut args = vec!["tilo-app", "package"];
            args.extend(flags);
            assert!(Cli::try_parse_from(args).is_err());
        }
        let parsed = Cli::try_parse_from([
            "tilo-app",
            "package",
            "--sdk-bundle",
            "runtime",
            "--sdk-root",
            "runtime-root",
            "--sdk-publisher",
            "publisher",
            "--sdk-compiler-bundle",
            "compiler",
            "--sdk-compiler-root",
            "compiler-root",
        ])
        .expect("complete independent selections");
        let Command::Package(args) = parsed.command else {
            panic!("package command")
        };
        assert_eq!(args.sdk_compiler_bundle, Some(PathBuf::from("compiler")));
        assert_eq!(args.sdk_compiler_root.as_deref(), Some("compiler-root"));
    }

    #[test]
    fn native_sdk_selection_requires_independent_publisher_and_root() {
        assert!(Cli::try_parse_from(["tilo-app", "package", "--sdk-bundle", "sdk"]).is_err());
        assert!(
            Cli::try_parse_from(["tilo-app", "package", "--sdk-publisher", "publisher"]).is_err()
        );
        let parsed = Cli::try_parse_from([
            "tilo-app",
            "package",
            "--sdk-bundle",
            "sdk",
            "--sdk-publisher",
            "publisher",
            "--sdk-root",
            "root",
        ])
        .expect("complete SDK selection");
        let Command::Package(args) = parsed.command else {
            panic!("package command");
        };
        assert_eq!(args.sdk_bundle, Some(PathBuf::from("sdk")));
        assert_eq!(args.sdk_publisher.as_deref(), Some("publisher"));
        assert_eq!(args.sdk_root.as_deref(), Some("root"));
        for alias in ["linux", "macos", "web"] {
            let parsed = Cli::try_parse_from([
                "tilo-app",
                "package",
                "--sdk-bundle",
                "sdk",
                "--sdk-publisher",
                "publisher",
                "--sdk-root",
                "root",
                "--target",
                alias,
            ])
            .expect("SDK target alias is parsed before production admission");
            let Command::Package(args) = parsed.command else {
                panic!("package command");
            };
            assert!(
                run_package(args)
                    .unwrap_err()
                    .to_string()
                    .contains("not an SDK target alias")
            );
        }
    }
}
