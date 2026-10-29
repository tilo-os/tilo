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
use tilolang::source::{compile_dirs, dependency_order, write_ids};
use tilolang::{OsEntropy, PackageMeta};
use tilolang_tilo::bundle::{Bundle, ICON_FILE};

/// The record kind of every tilolang app's room record.
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
///
/// # Errors
/// The platform has no glyph of that name.
pub fn icon_artwork(declared: &str) -> Result<String, String> {
    let name = declared.replace('_', "-");
    ui::icons::Icon::from_name(&name)
        .map(ui::icons::icon_svg)
        .ok_or_else(|| format!("`.{declared}` is not an icon the platform has"))
}

fn toml_string(value: &str) -> String {
    toml::Value::String(value.to_owned()).to_string()
}

/// The manifest of the app `meta` is, signed by `publisher` (hex Ed25519 key).
#[must_use]
pub fn manifest_text(meta: &PackageMeta, options: &BuildOptions, publisher: &str) -> String {
    let category = meta.category.as_deref().map_or("Utility", menu_category);
    // A package whose types hold a mail server's resources runs the mail origin: the
    // connector reaches the account's servers and reads its credentials from custody, both in
    // the runtime's native code, never the package's.
    let mail = meta
        .types
        .iter()
        .any(|t| t.origin.as_deref().is_some_and(|o| o.starts_with("imap.")));
    let (capabilities, namespaces, net) = if mail {
        (
            "required = [\"network.http\"]\noptional = [\"secrets\"]",
            "[\"user\", \"pid\", \"uts\", \"ipc\", \"mount\", \"net\"]",
            "# The mail origin speaks IMAP and SMTP to the hosts the account's settings name.\nmode = \"host\"",
        )
    } else {
        (
            "optional = []",
            "[\"user\", \"pid\", \"uts\", \"ipc\", \"mount\"]",
            "mode = \"off\"",
        )
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

[package.isa]
x86_64 = "baseline"
aarch64 = "aarch64"

[capabilities]
{capabilities}

[resources]
kinds = [{record_kind}]

[publisher]
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
# its room has synced, which every empty state says until it has.
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
        record_kind = toml_string(RECORD_KIND),
        bin = toml_string(RUNTIME_BIN),
        shell_ui = ui::floating_surface::FLOATING_SURFACE_PROTOCOL_VERSION,
        services = runtime_portal::RUNTIME_INTERFACE_VERSION,
        capabilities = capabilities,
        namespaces = namespaces,
        net = net,
        // The system runs the mail connector for the app, in this sandbox, window or not.
        origins = if mail { "origins = [\"imap\"]\n" } else { "" },
    )
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
    let dirs = dependency_order(&options.dir, &options.search)?;
    let built = compile_dirs(&dirs, &[], &mut OsEntropy::default()).map_err(|e| e.to_string())?;
    for package in &built {
        if write_ids(package)? {
            eprintln!("{}: ids lock updated", package.dir.display());
        }
    }
    let publisher = hex::encode(identity.public_key_bytes());
    let mut apps = Vec::new();
    for (index, package) in built.iter().enumerate() {
        let meta = &package.compiled.meta;
        let used = &meta.uses;
        let imports = built
            .iter()
            .take(index)
            .map(|earlier| &earlier.compiled)
            .filter(|earlier| used.contains(&earlier.meta.app));
        let bundle = Bundle::new(&package.compiled, imports);
        let staged = options.out.join(short_name(&meta.app));
        let payload = staged.join("payload");
        if payload.exists() {
            std::fs::remove_dir_all(&payload).map_err(io(&payload))?;
        }
        bundle.write(&payload).map_err(|error| error.to_string())?;
        if let Some(icon) = &meta.icon {
            let path = payload.join(ICON_FILE);
            std::fs::write(&path, icon_artwork(icon)?).map_err(io(&path))?;
        }
        let text = manifest_text(meta, options, &publisher);
        let manifest_path = staged.join("manifest.toml");
        std::fs::write(&manifest_path, &text).map_err(io(&manifest_path))?;
        let manifest = Manifest::from_toml_str(&text)
            .map_err(|error| format!("{}: {error}", manifest_path.display()))?;
        let packed = tilo_app_package::pack::pack(&payload, &manifest, identity)
            .map_err(|error| format!("{}: {error}", meta.app))?;
        let tapp = options.out.join(format!("{}.tapp", short_name(&meta.app)));
        write_tapp(&tapp, &packed)?;
        apps.push(BuiltApp {
            app: meta.app.clone(),
            title: meta.title.clone(),
            meta: meta.clone(),
            digest: digest(&text, &bundle)?,
            payload,
            manifest: manifest_path,
            tapp,
        });
    }
    Ok(apps)
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
