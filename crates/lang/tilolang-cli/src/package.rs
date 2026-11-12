//! `tilolang build`: each package of a build becomes a signed Tilo app, a `.tapp`.
//!
//! The package's payload is its [`Bundle`] (module with metadata, ids lock, the metadata of the
//! packages it uses, the generated surface) and its icon. Its manifest is generated from the
//! `app` declaration: the app id is the package id, the name and category are the declaration's,
//! and the entrypoint is the tilolang runtime bundle's binary (`[entrypoint] runtime`), which
//! the launcher points at the payload.

use std::path::{Path, PathBuf};

use tilo_app_package::manifest::Manifest;
use tilo_app_package::pack::SigningIdentity;
use tilolang::source::{write_catalogue, write_ids};
use tilolang::{OsEntropy, PackageMeta};
use tilolang_tilo::bundle::{Bundle, ICON_FILE};

/// The record kind of every tilolang app's workspace record.
pub const RECORD_KIND: &str = "tilolang.room";
/// The binary of the tilolang runtime bundle, relative to the bundle.
pub const RUNTIME_BIN: &str = "bin/tilolang-app";
/// The tilolang runtime bundle a package built by this tool runs on.
pub const DEFAULT_TILOLANG_RUNTIME: &str =
    concat!("tilolang/", env!("CARGO_PKG_VERSION"), "+tilolang");
/// The Tilo runtime release an app names when the build is not told the exact one.
pub const DEFAULT_TILO_RUNTIME: &str = "tilo-runtime/2026.07";

/// What a build makes of the package in a directory and the packages it uses.
#[derive(Clone, Debug)]
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
    /// The packages' version.
    pub version: String,
}

/// One app a build made.
#[derive(Clone, Debug)]
pub struct BuiltApp {
    pub app: String,
    pub title: String,
    /// The compiled package's metadata: what decides whether a build takes over a running one
    /// in place ([`tilolang_tilo::upgrade`]).
    pub meta: PackageMeta,
    /// SHA-256 (hex) of what the app is: its manifest and its payload. Two builds with the same
    /// digest are the same app.
    pub digest: String,
    /// The staged payload directory.
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

/// The outside origins a package's types hold, each once, in a fixed order: `imap`, `youtube`.
///
/// A type names the kind of an outside service's resource it holds (`type Message (origin:
/// imap.message)`), and the system runs a connector process for the origin its manifest
/// declares ([`tilolang::origin`]). The manifest's `[entrypoint] origins` is what the launcher
/// admits a connector with, so a package whose manifest leaves one out is refused the connector
/// it needs.
#[must_use]
pub fn origins_of(meta: &PackageMeta) -> Vec<&'static str> {
    let mut origins: Vec<&'static str> = Vec::new();
    for ty in &meta.types {
        let Some(origin) = ty.origin.as_deref().and_then(|kind| kind.split('.').next()) else {
            continue;
        };
        // Only the origins the platform runs a connector for: the session's own kinds
        // (`session.desk`) are written by the session, never by a process.
        let connector = ["imap", "youtube"].into_iter().find(|name| *name == origin);
        if let Some(name) = connector
            && !origins.contains(&name)
        {
            origins.push(name);
        }
    }
    origins.sort_unstable();
    origins
}

/// A package's `[sandbox]`: the capabilities, namespaces and network mode of a package holding
/// the outside `origins`.
///
/// A package whose types hold an outside service's resources runs that origin's connector: it
/// reaches the service, and reads the credentials from custody, both in the runtime's native
/// code and never the package's. The origins are the ones the platform runs a connector for; a
/// package with none opens no socket of its own.
fn sandbox(origins: &[&str]) -> (&'static str, &'static str, &'static str) {
    if origins.is_empty() {
        return (
            "optional = []",
            "[\"user\", \"pid\", \"uts\", \"ipc\", \"mount\"]",
            "mode = \"off\"",
        );
    }
    // The mail origin also reads the account's credentials from custody; the others hold none.
    let capabilities = if origins.contains(&"imap") {
        "required = [\"network.http\"]\noptional = [\"secrets\"]"
    } else {
        "required = [\"network.http\"]\noptional = []"
    };
    (
        capabilities,
        "[\"user\", \"pid\", \"uts\", \"ipc\", \"mount\", \"net\"]",
        "# Each origin's connector speaks to the hosts its service names, in the sandbox\n\
         # the launcher runs it in.\nmode = \"host\"",
    )
}

/// The manifest's `[entrypoint]` line naming each of the outside `origins`' connectors: one
/// launch per origin the package's types hold, and nothing when it holds none.
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
    catalogues: &[(String, String)],
    options: &BuildOptions,
    publisher: &str,
) -> String {
    let category = meta.category.as_deref().map_or("Utility", menu_category);
    let origins = origins_of(meta);
    let (capabilities, namespaces, net) = sandbox(&origins);
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
    // What the app may do on the session's action door beyond its own actions: an action or a
    // view of it that reaches another app's published action asks it at that app's door, under
    // its own identity. Nothing else is declared: what a workspace grants the app still decides
    // each call.
    let invokes = if calls_another_app(meta) {
        "[actions]\ninvoke = true\n"
    } else {
        ""
    };
    // The name the app gives itself, in each language it ships: what a person reads in the
    // launcher, in search and where a change it made is named ("Heute"), never the words its
    // id makes.
    let names = names_table(meta, catalogues);
    format!(
        r#"# Generated by `tilolang build` from the package's `app` declaration.

[package]
id = {id}
name = {name}
category = {category}
version = {version}
kind = "app"
runtime = {runtime}
{extra}optional-runtimes = [{tilolang}]
{names}

[package.isa]
x86_64 = "baseline"
aarch64 = "aarch64"

[capabilities]
{capabilities}

[resources]
kinds = [{record_kinds}]

{invokes}[publisher]
identity = "ed25519:{publisher}"
signature = ""

[sandbox]
# Without the mail origin no `net` namespace: the app opens no socket of its own. Collaboration reaches the
# broker over a service descriptor.
namespaces = {namespaces}
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
{net}

[sandbox.portals]

[sandbox.services]
# Collaboration and the filesystem, at the version the runtime serves its session services.
collab = "optional@{services}"
tilo-fs = "optional@{services}"
# The shell's side of the window: where it is shown (the workspace's appearance), and whether
# its workspace has synced, which every empty state says until it has.
shell-ui = "optional@{shell_ui}"

[entrypoint]
runtime = {tilolang}
bin = {bin}
type = "gui"
{origins}
[providers]
mime = []
ext = []
"#,
        id = toml_string(&meta.app),
        name = toml_string(&meta.title),
        category = toml_string(category),
        version = toml_string(&options.version),
        runtime = toml_string(&options.tilo_runtime),
        tilolang = toml_string(&options.tilolang_runtime),
        // The app's workspace record, and the workspace's record of each person's own state
        // of it, which every app of the language opens.
        record_kinds = [
            toml_string(RECORD_KIND),
            toml_string(tilolang::PERSONAL_RECORD_KIND),
        ]
        .join(", "),
        bin = toml_string(RUNTIME_BIN),
        invokes = invokes,
        shell_ui = ui::floating_surface::FLOATING_SURFACE_PROTOCOL_VERSION,
        services = runtime_portal::RUNTIME_INTERFACE_VERSION,
        capabilities = capabilities,
        namespaces = namespaces,
        net = net,
        // The system runs one connector per outside origin the app's types hold, in this
        // sandbox, window or not.
        origins = connector_origins(&origins),
    )
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
        table.push_str(&format!(
            "{} = {}\n",
            toml_string(locale),
            toml_string(&name)
        ));
    }
    table
}

/// Whether an action of the app, or a view of it, reaches an action another app publishes: what
/// the manifest declares as `[actions] invoke`.
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

/// Compile the package in `options.dir` and those it uses, and stage and sign each as an app.
///
/// # Errors
/// A package does not compile (the message lists the errors by file), or a file cannot be
/// written, or a generated manifest does not validate.
pub fn build(options: &BuildOptions, identity: &SigningIdentity) -> Result<Vec<BuiltApp>, String> {
    assembled(options, identity)?
        .into_iter()
        .map(|app| deliver(app, options, identity))
        .collect()
}

/// Stage, pack and sign an app as the `.tapp` a session installs.
fn deliver(
    app: Assembled,
    options: &BuildOptions,
    identity: &SigningIdentity,
) -> Result<BuiltApp, String> {
    let staged = options.out.join(short_name(&app.app));
    let payload = staged.join("payload");
    if payload.exists() {
        std::fs::remove_dir_all(&payload).map_err(io(&payload))?;
    }
    app.bundle
        .write(&payload)
        .map_err(|error| error.to_string())?;
    if let Some(icon) = &app.meta.icon {
        let path = payload.join(ICON_FILE);
        std::fs::write(&path, icon_artwork(icon)?).map_err(io(&path))?;
    }
    let manifest_path = staged.join("manifest.toml");
    std::fs::write(&manifest_path, &app.manifest).map_err(io(&manifest_path))?;
    let manifest = Manifest::from_toml_str(&app.manifest)
        .map_err(|error| format!("{}: {error}", manifest_path.display()))?;
    let packed = tilo_app_package::pack::pack(&payload, &manifest, identity)
        .map_err(|error| format!("{}: {error}", app.app))?;
    let tapp = options.out.join(format!("{}.tapp", short_name(&app.app)));
    write_tapp(&tapp, &packed)?;
    Ok(BuiltApp {
        app: app.app,
        title: app.title,
        meta: app.meta,
        digest: app.digest,
        payload,
        manifest: manifest_path,
        tapp,
    })
}

/// Compile the packages of `options` in dependency order, writing each one's ids lock and source
/// catalogue when it changed: what every delivery starts from, so a build leaves the package's
/// words where its sources say they are.
///
/// # Errors
/// A package does not compile (the message lists the errors by file), or an ids lock or a
/// catalogue cannot be written.
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

/// One app of a build, as every delivery takes it.
struct Assembled {
    app: String,
    title: String,
    meta: PackageMeta,
    /// Its manifest: the description of the app an install signs, and what identifies it here.
    manifest: String,
    bundle: Bundle,
    /// SHA-256 (hex) of the manifest and the bundle, as an install of the same build gets it.
    digest: String,
}

/// Compile the package in `options.dir` and those it uses, its bundle carrying those it uses.
///
/// # Errors
/// A package does not compile (the message lists the errors by file).
fn assembled(options: &BuildOptions, identity: &SigningIdentity) -> Result<Vec<Assembled>, String> {
    let built = compiled(options)?;
    let publisher = hex::encode(identity.public_key_bytes());
    let mut apps = Vec::new();
    for (index, package) in built.iter().enumerate() {
        let meta = &package.compiled.meta;
        let used = &meta.uses;
        // The standard library first: every module's code may call into it, and it is compiled
        // with Tilo rather than with the packages of this build.
        let imports = std::iter::once(tilolang::stdlib::compiled())
            .chain(built.iter().take(index).map(|earlier| &earlier.compiled))
            .filter(|earlier| used.contains(&earlier.meta.app));
        let bundle = Bundle::new(&package.compiled, imports);
        let manifest = manifest_text(meta, &bundle.catalogues(), options, &publisher);
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

/// A build of one app as the directory a running desktop app follows it takes it: the bundle
/// itself, and what decides whether the running build takes it in place.
pub struct Published {
    pub app: String,
    pub title: String,
    pub meta: PackageMeta,
    pub digest: String,
    pub bundle: Bundle,
}

/// Compile the package in `options.dir` and those it uses into the bundles a desktop app reads
/// from its generations, with nothing staged, packed or signed: a local run signs nothing.
///
/// # Errors
/// A package does not compile (the message lists the errors by file).
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
    use std::io::Write as _;
    let temp = path.with_extension("tapp.tmp");
    let file = std::fs::File::create(&temp).map_err(io(&temp))?;
    let mut file = tilo_app_package::tapp::write(
        std::io::BufWriter::new(file),
        &packed.meta,
        Some(packed as &dyn tilo_app_package::BlobSource),
    )
    .map_err(|error| format!("{}: {error}", path.display()))?;
    file.flush().map_err(io(&temp))?;
    drop(file);
    std::fs::rename(&temp, path).map_err(io(path))
}
