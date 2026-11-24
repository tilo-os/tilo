//! `tilolang build`: each package of a build becomes a signed Tilo app, a `.tapp`.
//!
//! The package's payload is its [`Bundle`] (module with metadata, ids lock, the metadata of the
//! packages it uses, the generated surface) and its icon. Its manifest is generated from the
//! `app` declaration: the app id is the package id, the name and category are the declaration's,
//! and the entrypoint is the tilolang runtime bundle's binary (`[entrypoint] runtime`), which
//! the launcher points at the payload.

use std::path::{Path, PathBuf};

use tilo_app_package::manifest::{AtspiAcceptance, Manifest};
use tilo_app_package::pack::SigningIdentity;
use tilolang::source::{write_catalogue, write_ids};
use tilolang::{OsEntropy, PackageMeta};
use tilolang_tilo::bundle::{Bundle, ICON_FILE};

pub const RUNTIME_BIN: &str = "bin/tilolang-process";
pub const DEFAULT_TILOLANG_RUNTIME: &str =
    concat!("tilolang/", env!("CARGO_PKG_VERSION"), "+tilolang");
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
    ui::icons::Icon::from_name(&name)
        .map(ui::icons::icon_svg)
        .ok_or_else(|| format!("`.{declared}` is not an icon the platform has"))
}

fn toml_string(value: &str) -> String {
    toml::Value::String(value.to_owned()).to_string()
}

/// The outside origins a package's types hold, each once, in a fixed order: `imap`, `music`,
/// `youtube`.
#[must_use]
pub fn origins_of(meta: &PackageMeta) -> Vec<&'static str> {
    let mut origins: Vec<&'static str> = Vec::new();
    for ty in &meta.types {
        let Some(origin) = ty.origin.as_deref().and_then(|kind| kind.split('.').next()) else {
            continue;
        };
        // Only the origins the platform runs a connector for: the session's own kinds
        // (`session.layout`) are written by the session, never by a process.
        let connector = ["imap", "music", "youtube"]
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

/// A package's `[capabilities]`: the hosts it reaches, and the device's files when one of its
/// actions reads them (`uses files`). An app that reads a folder also opens the files it lists in
/// the app that opens their kind, which is `files.open`; it is a `required` capability since the
/// app's own page offers the open, and the person saw it declared when they installed the app.
fn capabilities(origins: &[&str], uses_files: bool, engine: bool, web: &[String]) -> String {
    let mut required: Vec<&str> = Vec::new();
    let mut optional: Vec<&str> = Vec::new();
    if !origins.is_empty() || engine || !web.is_empty() {
        required.push("network.http");
        // The mail origin signs in to the workspace's accounts (`ACCOUNT_SIGN_INS`); the others
        // don't.
        if origins.contains(&"imap") {
            optional.push("account-sign-ins");
        }
    }
    // The page runs in this device's engine, which keeps its own processes and speaks to the web.
    if engine {
        required.push("webview");
    }
    if uses_files {
        required.push(tilo_app_package::manifest::FILES_OPEN);
    }
    let list = |names: &[&str]| {
        names
            .iter()
            .map(|name| format!("\"{name}\""))
            .collect::<Vec<String>>()
            .join(", ")
    };
    format!(
        "required = [{}]\noptional = [{}]",
        list(&required),
        list(&optional)
    )
}

/// A package's `[sandbox]`: the capabilities, namespaces and
/// network mode of a package holding the outside `origins` and reading the device's files when
/// `uses_files`.
fn sandbox(
    origins: &[&str],
    uses_files: bool,
    engine: Option<&str>,
    web: &[String],
) -> (String, &'static str, String, &'static str) {
    let capabilities = capabilities(origins, uses_files, engine.is_some(), web);
    if engine.is_some() {
        // A page runs in this device's engine, which keeps its own processes and speaks to the
        // web: the launcher grants it the web engine's confinement, which is the JIT a renderer
        // needs and nothing more than the engine's own sandbox around it.
        return (
            capabilities,
            "[\"user\", \"pid\", \"uts\", \"ipc\", \"mount\", \"net\"]",
            "# The engine reaches the web itself, in the sandbox the launcher runs the app in.\n\
             mode = \"host\""
                .to_owned(),
            "tilo-web-engine",
        );
    }
    if !origins.is_empty() {
        return (
            capabilities,
            "[\"user\", \"pid\", \"uts\", \"ipc\", \"mount\", \"net\"]",
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
        return (
            capabilities,
            "[\"user\", \"pid\", \"uts\", \"ipc\", \"mount\", \"net\"]",
            net,
            "tilo-default",
        );
    }
    (
        capabilities,
        "[\"user\", \"pid\", \"uts\", \"ipc\", \"mount\"]",
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

/// The manifest of the app `meta` is, signed by `publisher` (hex Ed25519 key), with the name the
/// app gives itself in every language `catalogues` translates it in.
#[must_use]
pub fn manifest_text(
    meta: &PackageMeta,
    accessibility: Option<&AtspiAcceptance>,
    catalogues: &[(String, String)],
    options: &BuildOptions,
    publisher: &str,
) -> String {
    let category = meta.category.as_deref().map_or("Utility", menu_category);
    let (providers_mime, providers_ext) = opens_of(meta);
    let providers_mime: Vec<String> = providers_mime
        .iter()
        .map(|mime| toml_string(mime))
        .collect();
    let providers_ext: Vec<String> = providers_ext.iter().map(|ext| toml_string(ext)).collect();
    let origins = origins_of(meta);
    let (capabilities, namespaces, net, seccomp) =
        sandbox(&origins, uses_files(meta), meta.engine.as_deref(), &meta.web);
    // The runtimes the package needs beside the language's own: the app's page engine, which
    // installing the app installs in the same step or not at all.
    let mut runtimes = vec![toml_string(&options.tilolang_runtime)];
    if let Some(engine) = &meta.engine {
        runtimes.push(toml_string(engine));
    }
    let runtimes = runtimes.join(", ");
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
    // The scenario the package declared for the external AT-SPI check, as a signed manifest
    // carries it.
    let accessibility = accessibility.map_or_else(String::new, |acceptance| {
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
    });
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
identity = "ed25519:{publisher}"
signature = ""

[sandbox]
# Without the mail origin no `net` namespace: the app opens no socket of its own. Collaboration reaches the
# broker over a service descriptor.
namespaces = {namespaces}
seccomp = {seccomp}
landlock = "strict"

[sandbox.net]
{net}

[sandbox.portals]

[sandbox.services]
# Collaboration and the filesystem, at the version the runtime serves its session services.
collab = "optional@{services}"
tilo-fs = "optional@{services}"

[entrypoint]
runtime = {tilolang}
bin = {bin}
type = "gui"
{splash}{window}{origins}
{accessibility}[providers]
mime = [{providers_mime}]
ext = [{providers_ext}]
schemes = [{providers_schemes}]
{offers}"#,
        id = toml_string(&meta.app),
        name = toml_string(&meta.title),
        category = toml_string(category),
        version = toml_string(&options.version),
        // Where the app runs: where its runtime runs.
        platforms = PLATFORMS,
        runtime = toml_string(&options.tilo_runtime),
        tilolang = toml_string(&options.tilolang_runtime),
        runtimes = runtimes,
        seccomp = toml_string(seccomp),
        // The app's workspace record, and the workspace's record of each person's own state of it,
        // which every app of the language opens.
        resource_types = tilolang_tilo::meta::MetaExt::declared_kinds(meta)
            .iter()
            .map(|kind| toml_string(kind))
            .collect::<Vec<String>>()
            .join(", "),
        bin = toml_string(RUNTIME_BIN),
        window = window,
        // The promise the app's header made (`splash_budget:`), or nothing where it made none,
        // so the manifest's own default stands rather than an invented budget.
        splash = meta
            .splash_budget
            .map_or_else(String::new, |ms| format!("splash-budget = {ms}\n")),
        invokes = invokes,
        services = runtime_portal::RUNTIME_INTERFACE_VERSION,
        capabilities = capabilities,
        namespaces = namespaces,
        net = net,
        // The system runs one connector per outside origin
        // the app's types hold, in this sandbox, window or
        // not.
        origins = connector_origins(&origins),
        providers_mime = providers_mime.join(", "),
        providers_ext = providers_ext.join(", "),
        providers_schemes = scheme_list(meta),
        offers = offers,
        accessibility = accessibility,
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
    let key = tilolang::messages::keywords_key(&meta.app);
    let mut table = String::new();
    for (locale, catalogue) in catalogues {
        if locale == tilolang::messages::SOURCE_LOCALE {
            continue;
        }
        let Some(words) = tilolang::messages::word(catalogue, &key) else {
            continue;
        };
        if tilolang::messages::keywords_word(catalogue, &meta.app)
            .is_some_and(|words| words == meta.keywords)
        {
            continue;
        }
        if table.is_empty() {
            table.push_str("[package.keywords]\n");
        }
        table.push_str(&toml_string(locale));
        table.push_str(" = ");
        table.push_str(&toml_string(&words));
        table.push('\n');
    }
    table
}

/// Whether an action of the app, or a view of it, reaches an action another app publishes: what the
/// manifest declares as `[actions] invoke`.
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
    let apps = assembled(options, identity)?;
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
            .write(&stage_payload)
            .map_err(|error| error.to_string())?;
        if let Some(icon) = &app.meta.icon {
            let path = stage_payload.join(ICON_FILE);
            std::fs::write(&path, icon_artwork(icon)?).map_err(io(&path))?;
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

/// Compile the packages of `options` in dependency order, writing each one's ids lock and source
/// catalogue when it changed: what every delivery starts from, so a build leaves the package's
/// words where its sources say they are. # Errors A package does not compile (the message lists the
/// errors by file), or an ids lock or a catalogue cannot be written.
fn compiled(options: &BuildOptions) -> Result<Vec<tilolang::source::BuiltPackage>, String> {
    let (built, _) = crate::compile(&options.dir, &options.search, &mut OsEntropy::default())
        .map_err(|e| e.to_string())?;
    for package in &built {
        if write_ids(package)? {
            eprintln!("{}: ids lock updated", package.dir.display());
        }
        if write_catalogue(package)? {
            eprintln!("{}: the source catalogue updated", package.dir.display());
        }
    }
    Ok(built)
}

struct Assembled {
    app: String,
    title: String,
    meta: PackageMeta,
    manifest: String,
    bundle: Bundle,
    /// SHA-256 (hex) of the manifest and the bundle, as an install of the same build gets it.
    digest: String,
}

/// Compile the package in `options.dir` and those it uses, its bundle carrying those it uses. #
/// Errors A package does not compile (the message lists the errors by file).
fn assembled(options: &BuildOptions, identity: &SigningIdentity) -> Result<Vec<Assembled>, String> {
    let built = compiled(options)?;
    let publisher = hex::encode(identity.public_key_bytes());
    let mut apps = Vec::new();
    for (package, bundle) in built.iter().zip(bundles_of(&built)) {
        let meta = &package.compiled.meta;
        let accessibility = source_accessibility(&package.dir)?;
        let manifest = manifest_text(
            meta,
            accessibility.as_ref(),
            &bundle.catalogues(),
            options,
            &publisher,
        );
        let digest = digest(&manifest, &bundle)?;
        apps.push(Assembled {
            app: meta.app.clone(),
            title: meta.title.clone(),
            meta: meta.clone(),
            manifest,
            bundle,
            digest,
        });
    }
    Ok(apps)
}

/// The bundle of each compiled package, in the order given: each carries the packages it uses,
/// the standard library first and the earlier packages of the build after it.
fn bundles_of(built: &[tilolang::source::BuiltPackage]) -> Vec<Bundle> {
    built
        .iter()
        .enumerate()
        .map(|(index, package)| {
            let used = &package.compiled.meta.uses;
            // The standard library first: every module's code may call into it, and it is compiled
            // with Tilo rather than with the packages of this build.
            let imports = std::iter::once(tilolang::stdlib::compiled())
                .chain(built.iter().take(index).map(|earlier| &earlier.compiled))
                .filter(|earlier| used.contains(&earlier.meta.app));
            Bundle::new(&package.compiled, imports)
        })
        .collect()
}

/// The bundles of the package in `dir` and the packages it uses, in compilation order: the last is
/// `dir`'s own. What a host that carries packages as assets packs, without writing ids locks.
/// # Errors A package does not compile (the message lists the errors by file).
pub fn bundles(dir: &Path, search: &[PathBuf]) -> Result<Vec<Bundle>, String> {
    let (built, _) =
        crate::compile(dir, search, &mut OsEntropy::default()).map_err(|e| e.to_string())?;
    Ok(bundles_of(&built))
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
    let manifest = manifest_text(meta, None, &bundle.catalogues(), options, &publisher);
    let digest = digest(&manifest, &bundle)?;
    let mut apps = deliver_all(
        vec![Assembled {
            app: meta.app.clone(),
            title: meta.title.clone(),
            meta: meta.clone(),
            manifest,
            bundle,
            digest,
        }],
        options,
        identity,
    )?;
    apps.pop()
        .ok_or_else(|| format!("{}: nothing was delivered", meta.app))
}

/// A build of one app as the directory a running desktop app follows it takes it: the bundle
/// itself, and what decides whether the running build takes it in place.
pub struct Published {
    pub app: String,
    pub title: String,
    pub meta: PackageMeta,
    pub digest: String,
    pub bundle: Bundle,
}

/// Compile the package in `options.dir` and those it uses into the bundles a desktop app reads from
/// its generations, with nothing staged, packed or signed: a local run signs nothing. # Errors A
/// package does not compile (the message lists the errors by file).
pub fn published(
    options: &BuildOptions,
    identity: &SigningIdentity,
) -> Result<Vec<Published>, String> {
    Ok(assembled(options, identity)?
        .into_iter()
        .map(|app| Published {
            app: app.app,
            title: app.title,
            meta: app.meta,
            digest: app.digest,
            bundle: app.bundle,
        })
        .collect())
}

/// SHA-256 (hex) of a manifest and the bundle staged with it.
fn digest(manifest: &str, bundle: &Bundle) -> Result<String, String> {
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
    Ok(hex::encode(hasher.finalize()))
}

pub(crate) fn write_tapp(
    path: &Path,
    packed: &tilo_app_package::pack::Packed,
) -> Result<(), String> {
    tilo_app_package::tapp::write_file(
        path,
        &packed.meta,
        Some(packed as &dyn tilo_app_package::BlobSource),
        tilo_app_package::tapp::WriteOptions::replace(),
    )
    .map(|_| ())
    .map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_app_that_reads_the_devices_files_declares_that_it_opens_them() {
        assert_eq!(
            capabilities(&[], true, false, &[]),
            "required = [\"files.open\"]\noptional = []",
            "an app reading folders opens the files it lists"
        );
        assert_eq!(
            capabilities(&[], false, false, &[]),
            "required = []\noptional = []"
        );
        assert_eq!(
            capabilities(&["youtube"], false, false, &[]),
            "required = [\"network.http\"]\noptional = []"
        );
        assert_eq!(
            capabilities(&["imap"], true, false, &[]),
            "required = [\"network.http\", \"files.open\"]\noptional = [\"account-sign-ins\"]"
        );
        assert!(sandbox(&[], true, None, &[]).0.contains("\"files.open\""));
        // A package showing a page names its engine and the confinement the launcher runs it in.
        let (capabilities, namespaces, _, seccomp) =
            sandbox(&[], false, Some("tilo-runtime/2026.10.23-chromium"), &[]);
        assert!(capabilities.contains("\"webview\""), "{capabilities}");
        assert!(namespaces.contains("\"net\""), "{namespaces}");
        assert_eq!(seccomp, "tilo-web-engine");
    }

    #[test]
    fn an_app_that_reaches_the_web_says_which_hosts() {
        let listed = sandbox(&[], false, None, &["example.org".to_owned()]);
        assert!(listed.0.contains("\"network.http\""), "{}", listed.0);
        assert!(listed.1.contains("\"net\""), "{}", listed.1);
        assert!(listed.2.contains("mode = \"allowlist\""), "{}", listed.2);
        assert!(listed.2.contains("\"example.org\""), "{}", listed.2);
        // `*` is any host a person points the app at: what a reader of feeds and a browser ask.
        let any = sandbox(&[], false, None, &["*".to_owned()]);
        assert!(any.2.contains("mode = \"host\""), "{}", any.2);
        // An app reaching nothing declares no network at all.
        let none = sandbox(&[], false, None, &[]);
        assert!(!none.0.contains("network.http"), "{}", none.0);
        assert_eq!(none.2, "mode = \"off\"");
    }

    #[test]
    fn packages_showing_a_page_bind_their_window_to_the_device() {
        let manifest = manifest_text(
            &PackageMeta {
                app: "so.tilo.example".into(),
                engine: Some("tilo-runtime/2026.10.23-chromium".into()),
                ..PackageMeta::default()
            },
            None,
            &[],
            &BuildOptions::default(),
            "00",
        );
        assert!(
            manifest.contains("type = \"gui\"\nwindow = \"device\""),
            "a package showing a page declares a window that runs where its engine is: {manifest}"
        );
        assert!(
            !manifest_text(
                &PackageMeta::default(),
                None,
                &[],
                &BuildOptions::default(),
                "00"
            )
            .contains("window = \"device\""),
            "an app with no engine has views of records"
        );
    }
}
