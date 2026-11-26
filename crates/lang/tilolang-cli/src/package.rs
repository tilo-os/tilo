//! `tilolang build`: each package of a build becomes a signed Tilo app, a `.tapp`.
//!
//! The package's payload is its [`Bundle`] (module with metadata, ids lock, the metadata of the
//! packages it uses, the generated surface), its icon and, when the package carries one, the
//! program its `app.toml` declares (`main.js` for bun, `main.py` for Python). Its manifest is
//! generated from the `app` declaration: the app id is the package id, the name and category are
//! the declaration's, and the entrypoint is the tilolang runtime bundle's binary
//! (`[entrypoint] runtime`), which the launcher points at the payload — or the runtime and script
//! the package's `app.toml` declares, while the module still runs in the app's tilolang host.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use tilo_app_package::manifest::{AtspiAcceptance, Manifest};
use tilo_app_package::pack::SigningIdentity;
use tilolang::source::write_ids;
use tilolang::{OsEntropy, PackageMeta};
use tilolang_tilo::bundle::{Bundle, ICON_FILE};

pub const RUNTIME_BIN: &str = "bin/tilolang-process";
/// The tilolang runtime a build names its apps run on: the build this tool's own tilolang side
/// is ([`tilolang_tilo::runtime`]), keyed by the standard library it compiles with, so what it
/// builds and what a host carries are one id.
#[must_use]
pub fn default_tilolang_runtime() -> String {
    tilolang_tilo::runtime()
}

/// Write the standard library this tool compiles with, compiled, into `dir` as a tilolang runtime
/// bundle includes it: `tilo.std.wasm`, which the bundle keeps as `lib/tilo.std.wasm`
/// (`runtime_portal::process_link::STD_FILE`). Answers the file written.
///
/// # Errors
/// The file cannot be written.
pub fn write_std(dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(io(dir))?;
    let file = dir.join("tilo.std.wasm");
    std::fs::write(&file, &tilolang::stdlib::compiled().wasm).map_err(io(&file))?;
    Ok(file)
}
pub const DEFAULT_TILO_RUNTIME: &str = "tilo-runtime/2026.07";

/// Where a tilolang app runs: where its runtime runs (`crates/lang/tilolang-process/tilo-runtime.toml`), as a
/// Linux binary baked into the OS image, as a macOS binary shipped in Tilo.app, and on a web page
/// (`tilolang-web`). A package names it, so a runtime that narrows narrows its apps too.
pub const PLATFORMS: &str = "[\"linux\", \"macos\", \"web\"]";

/// What a package's own `tilo-app.toml`, beside its `app.tl`, adds to the manifest `tilolang
/// build` generates. A core app declares its external AT-SPI acceptance scenario here, in the
/// same shape the Rust apps' manifests carry, so one manifest and one validation cover both.
#[derive(serde::Deserialize)]
struct SourceManifest {
    #[serde(default)]
    accessibility: Option<tilo_app_package::manifest::Accessibility>,
}

/// The acceptance scenario `dir`'s `tilo-app.toml` declares, when it declares one.
fn source_accessibility(dir: &Path) -> Result<Option<AtspiAcceptance>, String> {
    let path = dir.join("tilo-app.toml");
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path).map_err(io(&path))?;
    let source: SourceManifest =
        toml::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(source.accessibility.and_then(|it| it.atspi_acceptance))
}

/// What a package's `app.toml`, beside its `app.tl`, declares beyond the language's own sources:
/// the program the package carries (a JavaScript or Python program a runtime's binary runs), the
/// runtimes that program needs, and the sandbox and capabilities it acts within. The package's
/// module is still built and shipped, and an installed device runs it in the app's tilolang host
/// as it runs every tilolang app's: the program is the app's process, not its code.
#[derive(Debug, Default)]
struct SourceProgram {
    /// The declared entrypoint, when the package carries a program.
    entrypoint: Option<SourceEntrypoint>,
    /// The runtimes the declared `[package]` needs beside the language's own.
    runtimes: Vec<String>,
    /// The capabilities the declared `[capabilities]` requires and may use.
    required: Vec<String>,
    optional: Vec<String>,
    /// The network the declared program reaches.
    net: Option<SourceNet>,
    /// The device services the declared program needs, as its `[sandbox.services]` names them.
    services: BTreeMap<String, String>,
}

/// The entrypoint a package's `app.toml` declares: a program of the package that a runtime
/// bundle's binary runs.
#[derive(Debug)]
struct SourceEntrypoint {
    /// `cli`: a program draws no window.
    kind: &'static str,
    /// The runtime whose `bin` runs the script (`bun/1.4.5+bun`).
    runtime: String,
    /// The interpreter, as the runtime's payload names it (`bin/bun`).
    bin: PathBuf,
    /// The program, a file of the package.
    script: PathBuf,
    /// Whether the device runs it as the app's held connection.
    connection: bool,
}

/// What a declared `[sandbox.net]` reaches.
#[derive(Debug)]
struct SourceNet {
    /// `off`, `host` or `allowlist`.
    mode: &'static str,
    hosts: Vec<String>,
}

#[derive(serde::Deserialize, Default)]
struct DeclaredManifest {
    #[serde(default)]
    package: DeclaredPackage,
    #[serde(default)]
    entrypoint: Option<DeclaredEntrypoint>,
    #[serde(default)]
    capabilities: Option<DeclaredCapabilities>,
    #[serde(default)]
    sandbox: Option<DeclaredSandbox>,
}

#[derive(serde::Deserialize, Default)]
struct DeclaredPackage {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default, rename = "optional-runtimes")]
    optional_runtimes: Vec<String>,
}

#[derive(serde::Deserialize, Default)]
struct DeclaredEntrypoint {
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    runtime: Option<String>,
    #[serde(default)]
    bin: Option<String>,
    #[serde(default)]
    script: Option<String>,
    #[serde(default)]
    connection: bool,
}

#[derive(serde::Deserialize, Default)]
struct DeclaredCapabilities {
    #[serde(default)]
    required: Vec<String>,
    #[serde(default)]
    optional: Vec<String>,
}

#[derive(serde::Deserialize, Default)]
struct DeclaredSandbox {
    #[serde(default)]
    net: Option<DeclaredNet>,
    #[serde(default)]
    services: BTreeMap<String, String>,
}

#[derive(serde::Deserialize)]
struct DeclaredNet {
    mode: String,
    #[serde(default)]
    hosts: Vec<String>,
}

/// The program `dir`'s `app.toml` declares, `None` when the package declares none. A declared
/// entrypoint names a script, the runtime whose binary runs it and the interpreter inside that
/// runtime; a declared package id or kind must be the app's own.
fn source_program(dir: &Path, meta: &PackageMeta) -> Result<Option<SourceProgram>, String> {
    let path = dir.join("app.toml");
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path).map_err(io(&path))?;
    let declared: DeclaredManifest =
        toml::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))?;
    let fail = |why: String| format!("{}: {why}", path.display());
    if let Some(id) = &declared.package.id
        && id != &meta.app
    {
        return Err(fail(format!(
            "its package id is {id:?}, not the app's {:?}",
            meta.app
        )));
    }
    if let Some(kind) = &declared.package.kind
        && kind != "app"
    {
        return Err(fail(format!(
            "its package kind is {kind:?}; a tilolang package is an `app`"
        )));
    }
    let entrypoint = declared
        .entrypoint
        .map(|entry| source_entrypoint(dir, &fail, &declared.package, entry))
        .transpose()?;
    let net = declared
        .sandbox
        .as_ref()
        .and_then(|sandbox| sandbox.net.as_ref())
        .map(source_net)
        .transpose()?;
    let (required, optional) = declared
        .capabilities
        .map_or_else(Default::default, |capabilities| {
            (capabilities.required, capabilities.optional)
        });
    Ok(Some(SourceProgram {
        entrypoint,
        runtimes: declared.package.optional_runtimes,
        required,
        optional,
        net,
        services: declared
            .sandbox
            .map(|sandbox| sandbox.services)
            .unwrap_or_default(),
    }))
}

/// [`SourceProgram::entrypoint`] for a declared `[entrypoint]`: a program of the package, run by
/// the interpreter its runtime provides.
fn source_entrypoint(
    dir: &Path,
    fail: &impl Fn(String) -> String,
    package: &DeclaredPackage,
    entry: DeclaredEntrypoint,
) -> Result<SourceEntrypoint, String> {
    let script = entry.script.as_deref().ok_or_else(|| {
        fail("a declared entrypoint runs a script; this one names none".to_owned())
    })?;
    let runtime = entry.runtime.ok_or_else(|| {
        fail(format!(
            "the script {script:?} needs the runtime that runs it"
        ))
    })?;
    let bin = entry.bin.ok_or_else(|| {
        fail(format!(
            "the script {script:?} needs its runtime's interpreter, `bin`"
        ))
    })?;
    match entry.kind.as_deref() {
        None | Some("cli") => {}
        Some(other) => {
            return Err(fail(format!(
                "a script entrypoint is `cli`, not {other:?}: a program draws no window"
            )));
        }
    }
    for path in [&bin, script] {
        let path = Path::new(path);
        if path.is_absolute() || path.components().any(|c| c.as_os_str() == "..") {
            return Err(fail(format!(
                "{} is not a path inside the package or its runtime",
                path.display()
            )));
        }
    }
    if !dir.join(script).is_file() {
        return Err(fail(format!(
            "the script {script:?} is not a file of the package"
        )));
    }
    if !package
        .optional_runtimes
        .iter()
        .any(|declared| declared == &runtime)
    {
        return Err(fail(format!(
            "the entrypoint runs on {runtime}, which `[package] optional-runtimes` does not list"
        )));
    }
    Ok(SourceEntrypoint {
        kind: "cli",
        runtime,
        bin: PathBuf::from(bin),
        script: PathBuf::from(script),
        connection: entry.connection,
    })
}

/// [`SourceProgram::net`] for a declared `[sandbox.net]`.
fn source_net(net: &DeclaredNet) -> Result<SourceNet, String> {
    let mode = match net.mode.as_str() {
        "off" => "off",
        "host" => "host",
        "allowlist" => "allowlist",
        other => {
            return Err(format!(
                "`[sandbox.net] mode = {other:?}` is not off, host or allowlist"
            ));
        }
    };
    if mode == "allowlist" && net.hosts.is_empty() {
        return Err("`[sandbox.net] mode = \"allowlist\"` names no host".to_owned());
    }
    Ok(SourceNet {
        mode,
        hosts: net.hosts.clone(),
    })
}

/// What a build makes of the package in a directory and the packages it uses.
#[derive(Clone, Debug, Default)]
pub struct BuildOptions {
    /// The package directory; the packages it uses are found beside it and in `search`.
    pub dir: PathBuf,
    pub search: Vec<PathBuf>,
    /// Where the payloads and `.tapp`s go.
    pub out: PathBuf,
    /// The Tilo runtime the apps name (`tilo-runtime/<release>`, or the exact build).
    pub tilo_runtime: String,
    /// The tilolang runtime bundle the apps execute (`tilolang/<version>`).
    pub tilolang_runtime: String,
    pub version: String,
    /// Refuse a package whose ids lock or source catalogue is not what its sources write, rather
    /// than rewriting it: a check builds what is committed, and a build that rewrote the tree
    /// would install ids no commit has.
    pub locked: bool,
}

#[derive(Clone, Debug)]
pub struct BuiltApp {
    pub app: String,
    pub title: String,
    /// The compiled package's metadata: what decides whether a build takes over a running one in
    /// place ([`tilolang_tilo::upgrade`]).
    pub meta: PackageMeta,
    /// SHA-256 (hex) of what the app is: its manifest and its payload.
    pub digest: String,
    pub payload: PathBuf,
    pub manifest: PathBuf,
    pub tapp: PathBuf,
}

/// The menu category of a declared `category:`.
#[must_use]
pub fn menu_category(declared: &str) -> &'static str {
    match declared {
        "office" => "Office",
        "development" => "Development",
        "graphics" => "Graphics",
        "multimedia" => "AudioVideo",
        "network" => "Network",
        "education" => "Education",
        "science" => "Science",
        "game" => "Game",
        "system" => "System",
        _ => "Utility",
    }
}

/// The icon artwork of a declared `icon:` (`.file_text`, the platform glyph `file-text`).
pub fn icon_artwork(declared: &str) -> Result<String, String> {
    let name = declared.replace('_', "-");
    tilo_ui_base::icons::Icon::from_name(&name)
        .map(tilo_ui_base::icons::icon_svg)
        .ok_or_else(|| format!("`.{declared}` is not an icon the platform has"))
}

fn toml_string(value: &str) -> String {
    toml::Value::String(value.to_owned()).to_string()
}

/// The kinds of the app's connection process, each once: the outside origins its types hold
/// (`caldav`, `imap`, `music`, `youtube`), sorted, then the held connections it keeps (`connections:
/// ["lsp"]`). The process the system runs for the app with no window runs each one's code.
#[must_use]
pub fn connection_kinds(meta: &PackageMeta) -> Vec<&'static str> {
    let mut kinds = origins_of(meta);
    for kind in &meta.connections {
        if let Some(kind) = tilolang::connections::kind(kind)
            && !kinds.contains(&kind.name)
        {
            kinds.push(kind.name);
        }
    }
    kinds
}

/// The held connections the package keeps whose code releases a vault item into the app's
/// connection process.
fn releases_secrets(meta: &PackageMeta) -> bool {
    meta.connections
        .iter()
        .filter_map(|kind| tilolang::connections::kind(kind))
        .any(|kind| kind.releases)
}

/// The outside origins a package's types hold, each once, in a fixed order: `caldav`, `imap`,
/// `music`, `youtube`.
#[must_use]
pub fn origins_of(meta: &PackageMeta) -> Vec<&'static str> {
    let mut origins: Vec<&'static str> = Vec::new();
    for ty in &meta.types {
        let Some(origin) = ty.origin.as_deref().and_then(|kind| kind.split('.').next()) else {
            continue;
        };
        // Only the origins the platform runs a connector for: the session's own kinds
        // (`session.layout`) are written by the session, never by a process.
        let connector = ["caldav", "imap", "music", "youtube"]
            .into_iter()
            .find(|name| *name == origin);
        if let Some(name) = connector
            && !origins.contains(&name)
        {
            origins.push(name);
        }
    }
    origins.sort_unstable();
    origins
}

/// A package's sandbox as its manifest declares it, before a declared program's own declarations
/// add to it.
struct SandboxPlan {
    required: Vec<String>,
    optional: Vec<String>,
    namespaces: Vec<&'static str>,
    net: String,
    seccomp: &'static str,
}

impl SandboxPlan {
    /// The capabilities a declared program requires and may use, each once: one name stands in one
    /// list, and what the language's own sources declare already is not repeated.
    fn declare_capabilities(&mut self, required: &[String], optional: &[String]) {
        for name in required {
            if !self.required.contains(name) && !self.optional.contains(name) {
                self.required.push(name.clone());
            }
        }
        for name in optional {
            if !self.required.contains(name) && !self.optional.contains(name) {
                self.optional.push(name.clone());
            }
        }
    }

    /// The network a declared program reaches, and the namespace that lets it: a program that
    /// reaches any host opens a socket of its own.
    fn declare_net(&mut self, net: &SourceNet) {
        self.net = match net.mode {
            "host" => "mode = \"host\"".to_owned(),
            "allowlist" => format!(
                "mode = \"allowlist\"\nhosts = [{}]",
                net.hosts
                    .iter()
                    .map(|host| toml_string(host))
                    .collect::<Vec<String>>()
                    .join(", ")
            ),
            _ => "mode = \"off\"".to_owned(),
        };
        if net.mode != "off" && !self.namespaces.contains(&"net") {
            self.namespaces.push("net");
        }
    }

    fn capabilities(&self) -> String {
        let list = |names: &[String]| {
            names
                .iter()
                .map(|name| format!("\"{name}\""))
                .collect::<Vec<String>>()
                .join(", ")
        };
        format!(
            "required = [{}]\noptional = [{}]",
            list(&self.required),
            list(&self.optional)
        )
    }

    fn namespaces(&self) -> String {
        format!(
            "[{}]",
            self.namespaces
                .iter()
                .map(|name| format!("\"{name}\""))
                .collect::<Vec<String>>()
                .join(", ")
        )
    }
}

/// A package's `[capabilities]`: the hosts it reaches, and the device's files when one of its
/// actions reads them (`uses files`). An app that reads a folder also opens the files it lists in
/// the app that opens their kind, which is `files.open`; it is a `required` capability since the
/// app's own page offers the open, and the person saw it declared when they installed the app.
fn capabilities(
    origins: &[&str],
    uses_files: bool,
    engine: bool,
    web: &[String],
    releases: bool,
) -> (Vec<String>, Vec<String>) {
    let mut required: Vec<String> = Vec::new();
    let mut optional: Vec<String> = Vec::new();
    if !origins.is_empty() || engine || !web.is_empty() {
        required.push("network.http".to_owned());
    }
    // The mail and calendar origins sign in to the workspace's accounts (`ACCOUNT_SIGN_INS`), and
    // a held connection whose code needs a vault item has it released through them; the others
    // don't.
    if signs_in(origins) || releases {
        optional.push("account-sign-ins".to_owned());
    }
    // The page runs in this device's engine, which keeps its own processes and speaks to the web.
    if engine {
        required.push("webview".to_owned());
    }
    if uses_files {
        required.push(tilo_app_package::manifest::FILES_OPEN.to_owned());
    }
    (required, optional)
}

/// A package's `[sandbox]`: the capabilities, namespaces and
/// network mode of a package holding the outside `origins` and reading the device's files when
/// `uses_files`. A package whose connection process keeps only held connections reaches the
/// hosts its `web:` names and no other, as every other launch of the app does: a held
/// connection's network is the package's grant.
fn sandbox(
    origins: &[&str],
    uses_files: bool,
    engine: Option<&str>,
    web: &[String],
    releases: bool,
) -> SandboxPlan {
    let (required, optional) = capabilities(origins, uses_files, engine.is_some(), web, releases);
    let plan =
        |required, optional, namespaces: &[&'static str], net: String, seccomp| SandboxPlan {
            required,
            optional,
            namespaces: namespaces.to_vec(),
            net,
            seccomp,
        };
    if engine.is_some() {
        // A page runs in this device's engine, which keeps its own processes and speaks to the
        // web: the launcher grants it the web engine's confinement, which is the JIT a renderer
        // needs and nothing more than the engine's own sandbox around it.
        return plan(
            required,
            optional,
            &["user", "pid", "uts", "ipc", "mount", "net"],
            "# The engine reaches the web itself, in the sandbox the launcher runs the app in.\n\
             mode = \"host\""
                .to_owned(),
            "tilo-web-engine",
        );
    }
    if !origins.is_empty() {
        return plan(
            required,
            optional,
            &["user", "pid", "uts", "ipc", "mount", "net"],
            "# Each origin's connector speaks to the hosts its service names, in the sandbox\n\
             # the launcher runs it in.\nmode = \"host\""
                .to_owned(),
            "tilo-default",
        );
    }
    if !web.is_empty() {
        // The hosts the app's own requests reach, as its declaration names them: `*` asks for
        // any host a person subscribes to, and a list is the only hosts it may ask.
        let net = if web.iter().any(|host| host == "*") {
            "# The app reaches any host it is asked to, in the sandbox the launcher runs it\n\
             # in.\nmode = \"host\""
                .to_owned()
        } else {
            let hosts: Vec<String> = web.iter().map(|host| toml_string(host)).collect();
            format!(
                "# The hosts the app's own requests reach, in the sandbox the launcher runs it\n\
                 # in.\nmode = \"allowlist\"\nhosts = [{}]",
                hosts.join(", ")
            )
        };
        return plan(
            required,
            optional,
            &["user", "pid", "uts", "ipc", "mount", "net"],
            net,
            "tilo-default",
        );
    }
    plan(
        required,
        optional,
        &["user", "pid", "uts", "ipc", "mount"],
        "mode = \"off\"".to_owned(),
        "tilo-default",
    )
}

/// Whether any action of the package reads the device's own files (`uses files`): what declares
/// the `files.open` capability, so the app opens a file in the app that opens its kind.
fn uses_files(meta: &PackageMeta) -> bool {
    meta.actions
        .iter()
        .any(|action| action.uses.iter().any(|effect| effect == "files"))
}

/// The kinds of file the package's actions open (`opens:`), each once: the media types first, as
/// the manifest's `[providers] mime`, then the extensions, as its `ext`. An app that opens no files
/// offers none, and Files opens none of them with it.
#[must_use]
fn opens_of(meta: &PackageMeta) -> (Vec<String>, Vec<String>) {
    let mut mime: Vec<String> = Vec::new();
    let mut ext: Vec<String> = Vec::new();
    for action in &meta.actions {
        for kind in &action.opens {
            let list = if kind.contains('/') {
                &mut mime
            } else {
                &mut ext
            };
            if !list.contains(kind) {
                list.push(kind.clone());
            }
        }
    }
    (mime, ext)
}

/// The manifest's `[entrypoint]` line naming each of the outside `origins`' connectors: one launch
/// per origin the package's types hold, and nothing when it holds none.
fn connector_origins(origins: &[&str]) -> String {
    if origins.is_empty() {
        return String::new();
    }
    let list: Vec<String> = origins.iter().map(|origin| toml_string(origin)).collect();
    format!("origins = [{}]\n", list.join(", "))
}

/// The sandbox of a package, with what a declared program adds to it: its capabilities and the
/// network it reaches.
fn sandbox_plan(meta: &PackageMeta, program: Option<&SourceProgram>) -> SandboxPlan {
    let origins = origins_of(meta);
    let mut plan = sandbox(
        &origins,
        uses_files(meta),
        meta.engine.as_deref(),
        &meta.web,
        releases_secrets(meta),
    );
    if let Some(program) = program {
        plan.declare_capabilities(&program.required, &program.optional);
        if let Some(net) = &program.net {
            plan.declare_net(net);
        }
    }
    plan
}

/// The `[sandbox.services]` a package declares: the two every app asks the runtime for, and what
/// a declared program needs of the device.
fn services_block(program: Option<&SourceProgram>) -> String {
    let mut services = format!(
        "# Collaboration and the filesystem, at the version the runtime serves its session services.\n\
         collab = \"optional@{version}\"\ntilo-fs = \"optional@{version}\"",
        version = runtime_interface::RUNTIME_INTERFACE_VERSION
    );
    let Some(program) = program else {
        return services;
    };
    let declared: Vec<(&String, &String)> = program
        .services
        .iter()
        .filter(|(name, _)| !matches!(name.as_str(), "collab" | "tilo-fs"))
        .collect();
    if declared.is_empty() {
        return services;
    }
    services.push_str("\n# What the package's own program needs of this device's services.");
    for (name, requirement) in declared {
        let _ = write!(
            services,
            "\n{} = {}",
            toml_string(name),
            toml_string(requirement)
        );
    }
    services
}

/// The `[package] optional-runtimes` a package needs: the language's own, the app's page engine
/// (installing the app installs it in the same step or not at all), and the runtimes a declared
/// program names.
fn runtime_list(
    meta: &PackageMeta,
    options: &BuildOptions,
    program: Option<&SourceProgram>,
) -> String {
    let mut runtimes = vec![toml_string(&options.tilolang_runtime)];
    if let Some(engine) = &meta.engine {
        runtimes.push(toml_string(engine));
    }
    if let Some(program) = program {
        for runtime in &program.runtimes {
            let quoted = toml_string(runtime);
            if !runtimes.contains(&quoted) {
                runtimes.push(quoted);
            }
        }
    }
    runtimes.join(", ")
}

/// A manifest's `[entrypoint]` lines: the tilolang runtime bundle's binary the launcher points at
/// the payload, or a declared program's runtime, interpreter, script and connection.
struct EntrypointLines {
    runtime: String,
    bin: String,
    kind: String,
    program: String,
}

fn entrypoint_lines(
    program: Option<&SourceProgram>,
    options: &BuildOptions,
    signs_in: bool,
) -> EntrypointLines {
    let Some(entry) = program.and_then(|program| program.entrypoint.as_ref()) else {
        return EntrypointLines {
            runtime: toml_string(&options.tilolang_runtime),
            bin: toml_string(RUNTIME_BIN),
            kind: toml_string("gui"),
            // An app whose origin signs in to the workspace's accounts keeps its connection:
            // the device runs its connector as that connection.
            program: if signs_in {
                "connection = true
"
                .to_owned()
            } else {
                String::new()
            },
        };
    };
    let mut lines = format!(
        "script = {}\n",
        toml_string(&entry.script.to_string_lossy())
    );
    if entry.connection {
        lines.push_str("connection = true\n");
    }
    EntrypointLines {
        runtime: toml_string(&entry.runtime),
        bin: toml_string(&entry.bin.to_string_lossy()),
        kind: toml_string(entry.kind),
        program: lines,
    }
}

/// The scenario a package declared for the external AT-SPI check, as a signed manifest carries
/// it.
fn accessibility_text(acceptance: Option<&AtspiAcceptance>) -> String {
    acceptance.map_or_else(String::new, |acceptance| {
        let list = |names: &[String]| {
            names
                .iter()
                .map(|name| toml_string(name))
                .collect::<Vec<String>>()
                .join(", ")
        };
        format!(
            "\n[accessibility.atspi_acceptance]\nroot = {}\nrequired = [{}]\naction = {}\nafter_action = [{}]\n",
            toml_string(&acceptance.root),
            list(&acceptance.required),
            toml_string(&acceptance.action),
            list(&acceptance.after_action)
        )
    })
}

/// The manifest of a package of no program of its own, translated into no other language: what
/// the launcher reads of the app `meta`, signed by `publisher`.
#[must_use]
pub fn plain_manifest(meta: &PackageMeta, options: &BuildOptions, publisher: &str) -> String {
    manifest_text(meta, &meta.app, None, None, &[], options, Some(publisher))
}

/// The manifest of the app `meta` is, carrying `id` as the package's id, signed by `publisher`
/// (hex Ed25519 key), with the name the app gives itself in every language `catalogues`
/// translates it in, and what the package's `app.toml` declares when it carries a program of its
/// own (`program`).
///
/// `id` is the app's own id, which is `meta.app` but for a development build: its module is the
/// device's own app ([`BuildOptions::app`]), while the manifest names the app it builds, from
/// which the device derives the id it installs the build under.
///
/// `publisher` is `None` for a build that is not to be signed: a development build, whose device
/// fills in its own key as it packs it, and the desktop's, which signs nothing.
#[must_use]
#[expect(clippy::too_many_lines, reason = "the manifest is one format template")]
fn manifest_text(
    meta: &PackageMeta,
    id: &str,
    accessibility: Option<&AtspiAcceptance>,
    program: Option<&SourceProgram>,
    catalogues: &[(String, String)],
    options: &BuildOptions,
    publisher: Option<&str>,
) -> String {
    let category = meta.category.as_deref().map_or("Utility", menu_category);
    let (providers_mime, providers_ext) = opens_of(meta);
    let providers_mime: Vec<String> = providers_mime
        .iter()
        .map(|mime| toml_string(mime))
        .collect();
    let providers_ext: Vec<String> = providers_ext.iter().map(|ext| toml_string(ext)).collect();
    let origins = connection_kinds(meta);
    let plan = sandbox_plan(meta, program);
    let capabilities = plan.capabilities();
    let namespaces = plan.namespaces();
    let services = services_block(program);
    let runtimes = runtime_list(meta, options, program);
    let entry = entrypoint_lines(program, options, signs_in(&origins));
    let EntrypointLines {
        runtime: entry_runtime,
        bin: entry_bin,
        kind: entry_type,
        program: entry_program,
    } = entry;
    let seccomp = plan.seccomp;
    let net = plan.net;
    // A package that shows a page shows it with this device's engine, so its window runs only
    // where that engine is: the manifest says so, and a session does not move the window.
    let window = if meta.engine.is_some() {
        "window = \"device\"\n"
    } else {
        ""
    };
    let mut extra = if meta.icon.is_some() {
        format!("icon = {}\n", toml_string(ICON_FILE))
    } else {
        String::new()
    };
    // The ids the app had before, signed with the rest of the manifest.
    if !meta.previously.is_empty() {
        let ids: Vec<String> = meta.previously.iter().map(|id| toml_string(id)).collect();
        extra.push_str("previously = [");
        extra.push_str(&ids.join(", "));
        extra.push_str("]\n");
    }
    // What the app may do on the session's action API beyond its own
    // actions: an action or a view of it that reaches another app's
    // published action asks it at that app's action API, under its own
    // identity.
    let invokes = if calls_another_app(meta) {
        "[actions]\ninvoke = true\n"
    } else {
        ""
    };
    // The name the app gives itself, in each language it ships: what a person reads in the
    // launcher, in search and where a change it made is named ("Heute"), never the words its id
    // makes.
    let names = names_table(meta, catalogues);
    let offers = offers_of(meta);
    // The other words people know the app by, in each language it ships: what search matches,
    // translated where a catalogue says it in its own words.
    let keywords = keywords_table(meta, catalogues);
    let accessibility = accessibility_text(accessibility);
    let publisher = publisher.map_or_else(String::new, |key| format!("ed25519:{key}"));
    format!(
        r#"# Generated by `tilolang build` from the package's `app` declaration.

[package]
id = {id}
name = {name}
category = {category}
version = {version}
kind = "app"
platforms = {platforms}
runtime = {runtime}
{extra}optional-runtimes = [{runtimes}]
{names}
{keywords}

[package.isa]
x86_64 = "baseline"
aarch64 = "aarch64"

[capabilities]
{capabilities}

[resources]
kinds = [{resource_types}]

{invokes}[publisher]
identity = {publisher_identity}
signature = ""

[sandbox]
# Without a declared network no `net` namespace: the app opens no socket of its own. Collaboration reaches the
# broker over a service descriptor.
namespaces = {namespaces}
seccomp = {seccomp}
landlock = "strict"

[sandbox.net]
{net}

[sandbox.portals]

[sandbox.services]
{services}

[entrypoint]
runtime = {entry_runtime}
bin = {entry_bin}
type = {entry_type}
{splash}{window}{origins}{entry_program}
{accessibility}[providers]
mime = [{providers_mime}]
ext = [{providers_ext}]
schemes = [{providers_schemes}]
{offers}"#,
        id = toml_string(id),
        name = toml_string(&meta.title),
        category = toml_string(category),
        version = toml_string(&options.version),
        // Where the app runs: where its runtime runs.
        platforms = PLATFORMS,
        runtime = toml_string(&options.tilo_runtime),
        entry_runtime = entry_runtime,
        entry_bin = entry_bin,
        entry_type = entry_type,
        entry_program = entry_program,
        runtimes = runtimes,
        seccomp = toml_string(seccomp),
        // The app's workspace record, and the workspace's record of each person's own state of it,
        // which every app of the language opens.
        resource_types = tilolang_tilo::meta::MetaExt::declared_kinds(meta)
            .iter()
            .map(|kind| toml_string(kind))
            .collect::<Vec<String>>()
            .join(", "),
        window = window,
        // The promise the app's header made (`splash_budget:`), or nothing where it made none,
        // so the manifest's own default stands rather than an invented budget.
        splash = meta
            .splash_budget
            .map_or_else(String::new, |ms| format!("splash-budget = {ms}\n")),
        invokes = invokes,
        services = services,
        capabilities = capabilities,
        namespaces = namespaces,
        net = net,
        // The system runs the app's connection process once per
        // kind it keeps, in this sandbox, window or not.
        origins = connector_origins(&origins),
        providers_mime = providers_mime.join(", "),
        providers_ext = providers_ext.join(", "),
        providers_schemes = scheme_list(meta),
        offers = offers,
        accessibility = accessibility,
        publisher_identity = toml_string(&publisher),
    )
}

/// Where an app's link-opening action is offered: the root's published resource, the root's kind,
/// and the system action and parameter a shell names. `None` when the package declares no
/// `open_link` on a root with the link as its one text argument.
pub(crate) struct LinkOffer {
    pub resource: String,
    pub kind: String,
}

/// [`LinkOffer`] for `meta`, if it declares one.
pub(crate) fn link_offer(meta: &PackageMeta) -> Option<LinkOffer> {
    use tilolang_tilo::meta::MetaExt as _;
    let root = tilolang_tilo::meta::open_link_root(meta)?;
    Some(LinkOffer {
        resource: format!("tl:{}:root-{}", meta.app, root.id),
        kind: meta.kind_of(&root.name),
    })
}

/// The `[[offers]]` a package's manifest carries: the app's `open-link`, when it declares one on
/// its root that takes the link as its one text argument, so a shell hands the app a link of a
/// scheme it claims while no window of it is open. An app that opens no links offers none.
fn offers_of(meta: &PackageMeta) -> String {
    use ui::tilo_actions::remote::{APPLICATION_OPEN_LINK_ACTION, OPEN_LINK_PARAM};
    let Some(offer) = link_offer(meta) else {
        return String::new();
    };
    format!(
        "\n# What the app offers while no window of it is open: the link it opens, on its root.\n\
         [[offers]]\nresource = {resource}\nresource-kind = {kind}\naction = {action}\n\
         label = \"Open link\"\nkind = \"read\"\noperation = \"view\"\nasks = {asks}\n",
        resource = toml_string(&offer.resource),
        kind = toml_string(&offer.kind),
        action = toml_string(APPLICATION_OPEN_LINK_ACTION),
        asks = toml_string(OPEN_LINK_PARAM),
    )
}

/// The manifest's `[providers] schemes` list: the URI schemes the app opens (`schemes: ["tube"]`),
/// claimed as a native app claims them, so a link of one of them opens in it.
fn scheme_list(meta: &PackageMeta) -> String {
    let schemes: Vec<String> = meta
        .schemes
        .iter()
        .map(|scheme| toml_string(scheme))
        .collect();
    schemes.join(", ")
}

/// The `[package.names]` table of an app's manifest: the name its own declarations are called by,
/// in each language its catalogues translate it into (`d-so-tilo-lang-today = Heute`).
fn names_table(meta: &PackageMeta, catalogues: &[(String, String)]) -> String {
    let key = tilolang::messages::key_of(&meta.app);
    let mut table = String::new();
    for (locale, catalogue) in catalogues {
        let Some(name) = tilolang::messages::word(catalogue, &key) else {
            continue;
        };
        if name == meta.title {
            continue;
        }
        if table.is_empty() {
            table.push_str("[package.names]\n");
        }
        table.push_str(&toml_string(locale));
        table.push_str(" = ");
        table.push_str(&toml_string(&name));
        table.push('\n');
    }
    table
}

/// The `[package.keywords]` table of an app's manifest: the other words search matches it by, in
/// each language its catalogues translate them in (`d-so-tilo-tasks-keywords = Aufgaben, ...`).
fn keywords_table(meta: &PackageMeta, catalogues: &[(String, String)]) -> String {
    let mut table = String::new();
    for (locale, catalogue) in catalogues {
        if locale == tilolang::messages::SOURCE_LOCALE {
            continue;
        }
        // A translation names the words its language knows the app by; one that only repeats
        // the source's adds nothing.
        let Some(words) = tilolang::messages::keywords_word(catalogue, &meta.app) else {
            continue;
        };
        if words == meta.keywords {
            continue;
        }
        if table.is_empty() {
            table.push_str("[package.keywords]\n");
        }
        table.push_str(&toml_string(locale));
        table.push_str(" = ");
        let words = words.into_iter().map(toml::Value::String).collect();
        table.push_str(&toml::Value::Array(words).to_string());
        table.push('\n');
    }
    table
}

/// Whether an action of the app, or a view of it, reaches an action another app publishes: what the
/// manifest declares as `[actions] invoke`.
/// Whether one of `origins` signs in to the workspace's accounts: the mail and calendar
/// origins do, and the others don't.
fn signs_in(origins: &[&str]) -> bool {
    origins
        .iter()
        .any(|origin| matches!(*origin, "imap" | "caldav"))
}

fn calls_another_app(meta: &PackageMeta) -> bool {
    meta.actions
        .iter()
        .flat_map(|action| &action.reaches)
        .chain(meta.views.iter().flat_map(|view| &view.reaches))
        .any(|reached| !reached.own)
}

/// The directory name of the app `app` in a build's output: its last id segment.
fn short_name(app: &str) -> &str {
    app.rsplit('.').next().unwrap_or(app)
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> String + '_ {
    move |error| format!("{}: {error}", path.display())
}

/// Compile the package in `options.dir` and those it uses, and stage and sign each as an app. #
/// Errors A package does not compile (the message lists the errors by file), or a file cannot be
/// written, or a generated manifest does not validate.
pub fn build(options: &BuildOptions, identity: &SigningIdentity) -> Result<Vec<BuiltApp>, String> {
    let publisher = hex::encode(identity.public_key_bytes());
    let apps = assembled(options, Some(publisher.as_str()), &[])?;
    deliver_all(apps, options, identity)
}

/// Find the deepest existing directory at or above `out` so the staging area
/// lives on the same filesystem without creating `out` before the build passes.
fn staging_parent(out: &Path) -> Result<PathBuf, String> {
    match std::fs::metadata(out) {
        Ok(meta) if meta.is_dir() => return Ok(out.to_path_buf()),
        Ok(_) => return Err(format!("{}: not a directory", out.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io(out)(error)),
    }
    let mut cursor = out.parent();
    while let Some(dir) = cursor {
        if dir.as_os_str().is_empty() {
            return Ok(PathBuf::from("."));
        }
        match std::fs::metadata(dir) {
            Ok(meta) if meta.is_dir() => return Ok(dir.to_path_buf()),
            Ok(_) => return Err(format!("{}: not a directory", dir.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                cursor = dir.parent();
            }
            Err(error) => return Err(io(dir)(error)),
        }
    }
    Ok(PathBuf::from("."))
}

/// Create `out` and any missing parent directories, returning the directories
/// created (outermost first) so a failed publish can remove them.
fn ensure_output_dirs(out: &Path) -> Result<Vec<PathBuf>, String> {
    let mut missing = Vec::new();
    let mut cursor = Some(out);
    while let Some(dir) = cursor {
        if dir.as_os_str().is_empty() || dir.exists() {
            break;
        }
        missing.push(dir.to_path_buf());
        cursor = dir.parent();
    }
    missing.reverse();
    let mut created = Vec::new();
    for dir in missing {
        match std::fs::create_dir(&dir) {
            Ok(()) => created.push(dir),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && dir.is_dir() => {}
            Err(error) => {
                for made in created.into_iter().rev() {
                    let _ = std::fs::remove_dir(&made);
                }
                return Err(io(&dir)(error));
            }
        }
    }
    Ok(created)
}

struct StagedApp {
    app: Assembled,
    name: String,
    stage_dir: PathBuf,
    stage_tapp: PathBuf,
}

/// Stage, pack, sign and verify every app in `apps` inside a temporary
/// directory, then publish all of them into `options.out`. Any failure at any
/// step leaves `options.out` untouched (and absent when it did not exist).
fn deliver_all(
    apps: Vec<Assembled>,
    options: &BuildOptions,
    identity: &SigningIdentity,
) -> Result<Vec<BuiltApp>, String> {
    let parent = staging_parent(&options.out)?;
    let staging = tempfile::Builder::new()
        .prefix(".tilolang-build-")
        .tempdir_in(&parent)
        .map_err(io(&parent))?;

    let mut staged = Vec::with_capacity(apps.len());
    for app in apps {
        let name = short_name(&app.app).to_owned();
        let stage_dir = staging.path().join(&name);
        let stage_payload = stage_dir.join("payload");
        app.bundle
            .write_installed(&stage_payload)
            .map_err(|error| error.to_string())?;
        if let Some(icon) = &app.meta.icon {
            let path = stage_payload.join(ICON_FILE);
            std::fs::write(&path, icon_artwork(icon)?).map_err(io(&path))?;
        }
        // The program the package carries, beside its module: what its runtime's binary runs, and
        // the files it reads.
        for (relative, source) in &app.files {
            let path = stage_payload.join(relative);
            if path.exists() {
                return Err(format!(
                    "{}: the program's file {} would replace what the build wrote",
                    app.app,
                    relative.display()
                ));
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(io(parent))?;
            }
            std::fs::copy(source, &path).map_err(io(source))?;
        }
        let stage_manifest = stage_dir.join("manifest.toml");
        std::fs::write(&stage_manifest, &app.manifest).map_err(io(&stage_manifest))?;
        let final_manifest = options.out.join(&name).join("manifest.toml");
        let manifest = Manifest::from_toml_str(&app.manifest)
            .map_err(|error| format!("{}: {error}", final_manifest.display()))?;
        let packed = tilo_app_package::pack::pack(&stage_payload, &manifest, identity)
            .map_err(|error| format!("{}: {error}", app.app))?;
        let stage_tapp = staging.path().join(format!("{name}.tapp"));
        let final_tapp = options.out.join(format!("{name}.tapp"));
        tilo_app_package::tapp::write_file(
            &stage_tapp,
            &packed.meta,
            Some(&packed as &dyn tilo_app_package::BlobSource),
            tilo_app_package::tapp::WriteOptions::replace(),
        )
        .map_err(|error| format!("{}: {error}", final_tapp.display()))?;
        staged.push(StagedApp {
            app,
            name,
            stage_dir,
            stage_tapp,
        });
    }

    publish_staged(staged, &options.out, staging.path())
}

fn publish_staged(
    staged: Vec<StagedApp>,
    out: &Path,
    staging_root: &Path,
) -> Result<Vec<BuiltApp>, String> {
    let created_dirs = ensure_output_dirs(out)?;
    let mut published_dirs: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    let mut published_tapps: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    let mut built = Vec::with_capacity(staged.len());

    let result: Result<(), String> = (|| {
        for item in staged {
            let final_dir = out.join(&item.name);
            let backup_dir = if final_dir.symlink_metadata().is_ok() {
                let backup = staging_root.join(format!(".backup-dir-{}", item.name));
                std::fs::rename(&final_dir, &backup).map_err(io(&final_dir))?;
                Some(backup)
            } else {
                None
            };
            if let Err(error) = std::fs::rename(&item.stage_dir, &final_dir) {
                if let Some(backup) = backup_dir {
                    let _ = std::fs::rename(&backup, &final_dir);
                }
                return Err(io(&final_dir)(error));
            }
            published_dirs.push((final_dir.clone(), backup_dir));

            let final_tapp = out.join(format!("{}.tapp", item.name));
            let backup_tapp = match final_tapp.symlink_metadata() {
                Ok(meta) if meta.is_file() => {
                    let backup = staging_root.join(format!(".backup-tapp-{}", item.name));
                    std::fs::hard_link(&final_tapp, &backup).map_err(io(&final_tapp))?;
                    Some(backup)
                }
                _ => None,
            };
            if let Err(error) = std::fs::rename(&item.stage_tapp, &final_tapp) {
                return Err(io(&final_tapp)(error));
            }
            published_tapps.push((final_tapp.clone(), backup_tapp));

            built.push(BuiltApp {
                app: item.app.app,
                title: item.app.title,
                meta: item.app.meta,
                digest: item.app.digest,
                payload: final_dir.join("payload"),
                manifest: final_dir.join("manifest.toml"),
                tapp: final_tapp,
            });
        }
        tilo_files::native::atomic::sync_directory(out).map_err(io(out))?;
        Ok(())
    })();

    if let Err(error) = result {
        for (final_tapp, backup_tapp) in published_tapps.into_iter().rev() {
            if let Some(backup) = backup_tapp {
                let _ = std::fs::rename(&backup, &final_tapp);
            } else {
                let _ = std::fs::remove_file(&final_tapp);
            }
        }
        for (final_dir, backup_dir) in published_dirs.into_iter().rev() {
            let _ = std::fs::remove_dir_all(&final_dir);
            if let Some(backup) = backup_dir {
                let _ = std::fs::rename(&backup, &final_dir);
            }
        }
        for dir in created_dirs.into_iter().rev() {
            let _ = std::fs::remove_dir(&dir);
        }
        return Err(error);
    }

    Ok(built)
}

/// Compile the packages of `options` in dependency order, writing each one's ids lock when it
/// changed: what every delivery starts from, so a build leaves the package's ids where its sources
/// say they are. A package's source catalogue is written from the sources into the module, never as
/// a file. # Errors A package does not compile (the message lists the errors by file), or an ids
/// lock cannot be written.
fn compiled(
    options: &BuildOptions,
    compile_as: &[(String, String)],
) -> Result<Vec<tilolang::source::BuiltPackage>, String> {
    let (built, _) = crate::compile_for(
        &options.dir,
        &options.search,
        compile_as,
        &mut OsEntropy::default(),
    )
    .map_err(|e| e.to_string())?;
    for package in &built {
        if options.locked {
            refuse_stale(package)?;
            continue;
        }
        if write_ids(package)? {
            eprintln!("{}: ids lock updated", package.dir.display());
        }
    }
    Ok(built)
}

/// `Err` naming what of `package` is not what its sources write, for a `--locked` build.
fn refuse_stale(package: &tilolang::source::BuiltPackage) -> Result<(), String> {
    if tilolang::source::ids_current(package) {
        return Ok(());
    }
    let dir = package.dir.display();
    Err(format!(
        "{dir}: the ids lock (app.ids) is out of date with its sources; write it with `tilolang \
         check --write-ids {dir}` and commit it with them"
    ))
}

struct Assembled {
    app: String,
    title: String,
    meta: PackageMeta,
    manifest: String,
    bundle: Bundle,
    /// The program's files, each relative to the payload and where it is read from; empty for a
    /// package that carries none.
    files: Vec<(PathBuf, PathBuf)>,
    /// SHA-256 (hex) of the manifest, the bundle and the program's files, as an install of the
    /// same build gets it.
    digest: String,
}

/// Compile the package in `options.dir` and those it uses, its bundle carrying those it uses. A
/// package the compile-as pairs name is compiled as the id they give: a development build is the
/// device's own. # Errors A package does not compile (the message lists the errors by file).
fn assembled(
    options: &BuildOptions,
    publisher: Option<&str>,
    compile_as: &[(String, String)],
) -> Result<Vec<Assembled>, String> {
    let built = compiled(options, compile_as)?;
    let mut apps = Vec::new();
    for (package, bundle) in built.iter().zip(tilolang_tilo::bundle::of_build(&built)) {
        let meta = &package.compiled.meta;
        // The id the manifest names, which a development build's module does not: it is the
        // device's own app, installed under the id its marking derives from the app it builds.
        let id = package
            .source
            .header()
            .0
            .unwrap_or_else(|| meta.app.clone());
        let accessibility = source_accessibility(&package.dir)?;
        let program = source_program(&package.dir, meta)?;
        let files = match &program {
            Some(program) => program_files(&package.dir, program)?,
            None => Vec::new(),
        };
        let manifest = manifest_text(
            meta,
            &id,
            accessibility.as_ref(),
            program.as_ref(),
            &bundle.catalogues(),
            options,
            publisher,
        );
        let digest = digest(&manifest, &bundle, &files)?;
        apps.push(Assembled {
            app: id,
            title: meta.title.clone(),
            meta: meta.clone(),
            manifest,
            bundle,
            files,
            digest,
        });
    }
    Ok(apps)
}

/// The files of the program `program` declares in `dir`, each relative to the package directory
/// and where it is read from: everything the program runs from, and none of the language's own
/// sources or what a build writes. The SDK's native addon is not a file of a package: the runtime
/// package ships it (`node_modules/tilo-sdk-native`), so a copy beside an app would be a second
/// one.
fn program_files(dir: &Path, program: &SourceProgram) -> Result<Vec<(PathBuf, PathBuf)>, String> {
    let Some(entry) = &program.entrypoint else {
        return Ok(Vec::new());
    };
    let mut files = Vec::new();
    collect_program_files(dir, dir, &mut files)?;
    if !files.iter().any(|(relative, _)| relative == &entry.script) {
        return Err(format!(
            "{}: the script {} is not a file of the package",
            dir.join("app.toml").display(),
            entry.script.display()
        ));
    }
    Ok(files)
}

/// [`program_files`]' walk of `dir`, `root` being the package directory.
fn collect_program_files(
    root: &Path,
    dir: &Path,
    files: &mut Vec<(PathBuf, PathBuf)>,
) -> Result<(), String> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(io(dir))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<PathBuf>>>()
        .map_err(io(dir))?;
    entries.sort();
    for path in entries {
        let relative = path
            .strip_prefix(root)
            .map_err(|error| format!("{}: {error}", path.display()))?
            .to_path_buf();
        if is_language_source(&relative) || is_sdk_module(&relative) {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&path).map_err(io(&path))?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "{}: a program ships files, not links: its link is not in the package",
                path.display()
            ));
        }
        if metadata.is_dir() {
            collect_program_files(root, &path, files)?;
        } else {
            files.push((relative, path));
        }
    }
    Ok(())
}

/// Whether `relative` is the language's own source or what a build writes: never part of the
/// program a package carries.
fn is_language_source(relative: &Path) -> bool {
    if relative.components().count() == 1 {
        let name = relative.file_name().and_then(|name| name.to_str());
        if matches!(
            name,
            Some(
                "app.tl"
                    | "app.ids"
                    | "app.toml"
                    | "tilo-app.toml"
                    | "app.wasm"
                    | "surface.json"
                    | "icon.svg"
                    | "imports"
            )
        ) {
            return true;
        }
    }
    relative
        .extension()
        .is_some_and(|extension| extension == "tl")
}

/// Whether `relative` is the SDK's native addon, which the runtime package ships.
fn is_sdk_module(relative: &Path) -> bool {
    let components: Vec<&str> = relative
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect();
    components
        .windows(2)
        .any(|pair| matches!(pair, ["node_modules", "tilo-sdk-native"]))
}

/// Stage, pack and sign one already-compiled package as the `.tapp` a session installs: the same
/// delivery [`build`] makes, for a caller that compiled its package itself against packages it
/// publishes (a style package compiled against the shell a session runs, which is no directory this
/// build searches). # Errors The icon, manifest, packing or signing fails.
pub fn deliver_compiled(
    compiled: &tilolang::Compiled,
    imports: &[&tilolang::Compiled],
    options: &BuildOptions,
    identity: &SigningIdentity,
) -> Result<BuiltApp, String> {
    let meta = &compiled.meta;
    let publisher = hex::encode(identity.public_key_bytes());
    let bundle = Bundle::new(compiled, imports.iter().copied());
    let manifest = manifest_text(
        meta,
        &meta.app,
        None,
        None,
        &bundle.catalogues(),
        options,
        Some(publisher.as_str()),
    );
    let digest = digest(&manifest, &bundle, &[])?;
    let mut apps = deliver_all(
        vec![Assembled {
            app: meta.app.clone(),
            title: meta.title.clone(),
            meta: meta.clone(),
            manifest,
            bundle,
            files: Vec::new(),
            digest,
        }],
        options,
        identity,
    )?;
    apps.pop()
        .ok_or_else(|| format!("{}: nothing was delivered", meta.app))
}

/// A build staged for the device running it to install as its development build of the app: the
/// app's manifest and the files of its payload, unsigned, where the cache's `install-dev` action
/// reads them. The device marks it, packs it and signs it with its own key, under the id its
/// marking derives; the tool never sees that key and never signs.
pub struct StagedDev {
    /// The app's own id, the one its sources name and its manifest carries; the device derives
    /// the id it installs the build under from it.
    pub app: String,
    /// The id the module is compiled as: the device's own development build of the app.
    pub dev: String,
    pub title: String,
    pub meta: PackageMeta,
    /// The manifest the device packs: the app's own, with no publisher filled in yet.
    pub manifest: PathBuf,
    /// The directory holding the manifest and the payload, as the cache walks it.
    pub dir: PathBuf,
    /// Each file of the payload, by its path in it and where it is on disk.
    pub payload: Vec<(String, PathBuf)>,
    pub digest: String,
}

/// Compile the packages of `options` and stage each as the directory the running device installs
/// as its development build, under `options.out/dev`. Each package the compile-as pairs name is
/// compiled as the id they give, while its manifest names the app it builds. Nothing is packed,
/// signed or verified: the device does that, with its own key.
///
/// # Errors
/// A package does not compile (the message lists the errors by file), or a file cannot be written.
pub fn stage_dev(
    options: &BuildOptions,
    compile_as: &[(String, String)],
) -> Result<Vec<StagedDev>, String> {
    let staged_root = options.out.join("dev");
    let mut staged = Vec::new();
    for app in assembled(options, None, compile_as)? {
        let dir = staged_root.join(short_name(&app.app));
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(io(&dir))?;
        }
        std::fs::create_dir_all(&dir).map_err(io(&dir))?;
        app.bundle
            .write_installed(&dir)
            .map_err(|error| error.to_string())?;
        if let Some(icon) = &app.meta.icon {
            let path = dir.join(ICON_FILE);
            std::fs::write(&path, icon_artwork(icon)?).map_err(io(&path))?;
        }
        for (relative, source) in &app.files {
            let path = dir.join(relative);
            if path.exists() {
                return Err(format!(
                    "{}: the program's file {} would replace what the build wrote",
                    app.app,
                    relative.display()
                ));
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(io(parent))?;
            }
            std::fs::copy(source, &path).map_err(io(source))?;
        }
        let manifest = dir.join("tilo-app.toml");
        std::fs::write(&manifest, &app.manifest).map_err(io(&manifest))?;
        let payload = payload_of(&dir)?;
        staged.push(StagedDev {
            app: app.app,
            dev: app.meta.app.clone(),
            title: app.title,
            meta: app.meta,
            manifest,
            dir,
            payload,
            digest: app.digest,
        });
    }
    Ok(staged)
}

/// Every file below `dir`, by its path below it, sorted: the payload a device packs, without the
/// manifest it reads apart.
fn payload_of(dir: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(at) = pending.pop() {
        let entries = std::fs::read_dir(&at).map_err(io(&at))?;
        for entry in entries {
            let entry = entry.map_err(io(&at))?;
            let path = entry.path();
            let kind = entry.file_type().map_err(io(&path))?;
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file() {
                let relative = path
                    .strip_prefix(dir)
                    .map_err(|error| format!("{}: {error}", path.display()))?
                    .to_string_lossy()
                    .replace('\\', "/");
                if relative != "tilo-app.toml" {
                    files.push((relative, path));
                }
            }
        }
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(files)
}

/// SHA-256 (hex) of a manifest, the bundle staged with it and the program's files.
fn digest(manifest: &str, bundle: &Bundle, files: &[(PathBuf, PathBuf)]) -> Result<String, String> {
    use sha2::Digest as _;
    let mut hasher = sha2::Sha256::new();
    for part in [
        manifest.as_bytes(),
        &bundle.wasm,
        bundle.ids.as_bytes(),
        &bundle.surface().map_err(|error| error.to_string())?,
    ] {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    for import in &bundle.imports {
        hasher.update((import.wasm.len() as u64).to_le_bytes());
        hasher.update(&import.wasm);
    }
    // The files the package ships that an install carries: what it holds is part of what it is.
    for asset in bundle.assets.iter().filter(|asset| !asset.scenario_only) {
        hasher.update((asset.name.len() as u64).to_le_bytes());
        hasher.update(asset.name.as_bytes());
        hasher.update((asset.bytes.len() as u64).to_le_bytes());
        hasher.update(&asset.bytes);
    }
    for (relative, source) in files {
        let path = relative.to_string_lossy();
        let bytes = std::fs::read(source).map_err(io(source))?;
        hasher.update((path.len() as u64).to_le_bytes());
        hasher.update(path.as_bytes());
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_app_that_reads_the_devices_files_declares_that_it_opens_them() {
        let text = |required: Vec<String>, optional: Vec<String>| {
            SandboxPlan {
                required,
                optional,
                namespaces: Vec::new(),
                net: String::new(),
                seccomp: "tilo-default",
            }
            .capabilities()
        };
        let generated = |origins: &[&str], uses_files: bool| {
            let (required, optional) = capabilities(origins, uses_files, false, &[], false);
            text(required, optional)
        };
        assert_eq!(
            generated(&[], true),
            "required = [\"files.open\"]\noptional = []",
            "an app reading folders opens the files it lists"
        );
        assert_eq!(generated(&[], false), "required = []\noptional = []");
        assert_eq!(
            generated(&["youtube"], false),
            "required = [\"network.http\"]\noptional = []"
        );
        assert_eq!(
            generated(&["imap"], true),
            "required = [\"network.http\", \"files.open\"]\noptional = [\"account-sign-ins\"]"
        );
        assert_eq!(
            text(vec!["secrets".to_owned()], vec!["network.http".to_owned()]),
            "required = [\"secrets\"]\noptional = [\"network.http\"]",
            "a declared program adds its own capabilities, each once"
        );
        assert!(
            sandbox(&[], true, None, &[], false)
                .capabilities()
                .contains("\"files.open\"")
        );
        // A package showing a page names its engine and the confinement the launcher runs it in.
        let page = sandbox(
            &[],
            false,
            Some("tilo-runtime/2026.10.23-chromium"),
            &[],
            false,
        );
        assert!(page.capabilities().contains("\"webview\""));
        assert!(page.namespaces().contains("\"net\""));
        assert_eq!(page.seccomp, "tilo-web-engine");
    }

    #[test]
    fn an_app_that_reaches_the_web_says_which_hosts() {
        let listed = sandbox(&[], false, None, &["example.org".to_owned()], false);
        assert!(listed.capabilities().contains("\"network.http\""));
        assert!(listed.namespaces().contains("\"net\""));
        assert!(listed.net.contains("mode = \"allowlist\""));
        assert!(listed.net.contains("\"example.org\""));
        // `*` is any host a person points the app at: what a reader of feeds and a browser ask.
        let any = sandbox(&[], false, None, &["*".to_owned()], false);
        assert!(any.net.contains("mode = \"host\""));
        // An app reaching nothing declares no network at all.
        let none = sandbox(&[], false, None, &[], false);
        assert!(!none.capabilities().contains("network.http"));
        assert_eq!(none.net, "mode = \"off\"");
    }

    #[test]
    fn packages_showing_a_page_bind_their_window_to_the_device() {
        let manifest = manifest_text(
            &PackageMeta {
                app: "so.tilo.example".into(),
                engine: Some("tilo-runtime/2026.10.23-chromium".into()),
                ..PackageMeta::default()
            },
            "so.tilo.example",
            None,
            None,
            &[],
            &BuildOptions::default(),
            Some("00"),
        );
        assert!(
            manifest.contains("type = \"gui\"\nwindow = \"device\""),
            "a package showing a page declares a window that runs where its engine is: {manifest}"
        );
        assert!(
            !manifest_text(
                &PackageMeta::default(),
                "",
                None,
                None,
                &[],
                &BuildOptions::default(),
                Some("00")
            )
            .contains("window = \"device\""),
            "an app with no engine has views of records"
        );
    }

    /// A package's held connections run in its connection process, launched once per kind, and
    /// reach the hosts the package names and no other: a held connection's network is the
    /// package's grant, never the host network an origin's connector has.
    #[test]
    fn held_connections_run_in_the_apps_process_under_its_own_network_grant() {
        let manifest = |web: &[&str]| {
            manifest_text(
                &PackageMeta {
                    app: "so.tilo.example".into(),
                    connections: vec!["lsp".into()],
                    web: web.iter().map(|host| (*host).to_owned()).collect(),
                    ..PackageMeta::default()
                },
                "so.tilo.example",
                None,
                None,
                &[],
                &BuildOptions::default(),
                Some("00"),
            )
        };
        let local = manifest(&[]);
        assert!(local.contains("origins = [\"lsp\"]"), "{local}");
        assert!(local.contains("mode = \"off\""), "{local}");
        assert!(!local.contains("network.http"), "{local}");
        assert!(!local.contains("account-sign-ins"), "{local}");
        let reaching = manifest(&["gateway.example.org"]);
        assert!(reaching.contains("mode = \"allowlist\""), "{reaching}");
        assert!(reaching.contains("\"gateway.example.org\""), "{reaching}");
        assert!(!reaching.contains("mode = \"host\""), "{reaching}");
    }
}
