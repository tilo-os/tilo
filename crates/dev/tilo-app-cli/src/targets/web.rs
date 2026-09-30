//! The web target: a bundle the browser runtime's page host loads.
//!
//! A bundle is `crates/browser/tilo-browser-runtime/scripts/build.sh`'s
//! layout: `pkg/` (wasm-bindgen output under the runtime's module name), the
//! page host's `*.mjs` and `index.html`, the product fonts with
//! `fonts.json`, and `apps.json`. Two bundles exist:
//!
//! - **desktop**: `crates/browser/tilo-web-desktop`, every web-capable Tilo
//!   app, opened at `index.html?app=<id>`. Used for Tilo's own apps, so they
//!   share one build.
//! - **app**: the runtime with only this app registered. `wasm32` has no
//!   dynamic linking, so the runtime and the app are one module: a small
//!   generated crate under the target directory links both and registers the
//!   app at start. It is its own Cargo workspace (so it can live anywhere)
//!   with the app workspace's `[patch]`, profiles and lock file, and builds
//!   into the app workspace's target directory.
//!
//! [`package`] writes the immutable `versions/<digest>/` layout Cloud
//! serves, where the digest covers exactly the files Cloud checks.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};

use anyhow::{Context as _, Result, bail};
use sha2::{Digest as _, Sha256};

use super::{
    BuildOutcome, Project, Target, TargetPackageRequest, TargetRequest, WebBundle, cargo_program,
    link_or_copy, read_toml,
};

const WASM_TARGET: &str = "wasm32-unknown-unknown";
const RUNTIME: &str = "crates/browser/tilo-browser-runtime";
const DESKTOP: &str = "crates/browser/tilo-web-desktop";
const FONTS: &str = "crates/sdk/ui-core/assets/fonts";
const MOQ_BROWSER: &str = "scripts/moq-browser";
const MOQ_LIBRARY: &str = "media-moq-library.mjs";

/// The crate a web build compiles, and how to open the result.
struct WebCrate {
    manifest: PathBuf,
    package: String,
    /// Passed as `--target-dir` when the crate is its own workspace.
    target_dir: Option<PathBuf>,
    apps_json: String,
    entry: String,
}

pub(super) fn build(project: &Project, request: &TargetRequest) -> Result<BuildOutcome> {
    let bindgen = wasm_bindgen(project)?;
    let web_crate = match resolve_bundle(project, request.bundle)? {
        WebBundle::Desktop => desktop_crate(project)?,
        WebBundle::App | WebBundle::Auto => app_crate(project)?,
    };
    let profile = web_profile(project, request.release)?;
    let wasm = cargo_build_wasm(project, &web_crate, &profile, request)?;
    let bundle = project.output_dir(Target::Web).join("bundle");
    std::fs::create_dir_all(bundle.join("pkg"))
        .with_context(|| format!("creating {}", bundle.display()))?;
    // Cargo leaves an up-to-date module untouched; so does this step, so an
    // unchanged rebuild writes nothing.
    let stamp = bundle.join(".wasm-input");
    let input = wasm_stamp(&wasm, request.release)?;
    let output = bundle.join("pkg/tilo_browser_runtime_bg.wasm");
    if !output.is_file() || std::fs::read_to_string(&stamp).ok().as_deref() != Some(&input) {
        run(
            ProcessCommand::new(&bindgen)
                .arg(&wasm)
                .args([
                    "--target",
                    "web",
                    "--out-name",
                    "tilo_browser_runtime",
                    "--out-dir",
                ])
                .arg(bundle.join("pkg")),
            "wasm-bindgen",
        )?;
        if request.release {
            optimize(&output)?;
        }
        std::fs::write(&stamp, &input).context("recording the bundled module")?;
    }
    copy_page_host(&project.sdk_root, &bundle)?;
    bundle_moq_library(&project.sdk_root, &bundle)?;
    copy_fonts(&project.sdk_root, &bundle)?;
    std::fs::write(bundle.join("apps.json"), &web_crate.apps_json).context("writing apps.json")?;
    let build_json = serde_json::json!({
        "schema": 1,
        "app": project.manifest.package.id,
        "bundle": if web_crate.package == "tilo-web-desktop" { "desktop" } else { "app" },
        "profile": profile,
        "target": WASM_TARGET,
        "revision": git_revision(&project.sdk_root),
    });
    std::fs::write(
        bundle.join("build.json"),
        serde_json::to_string_pretty(&build_json)? + "\n",
    )
    .context("writing build.json")?;
    let size = std::fs::metadata(bundle.join("pkg/tilo_browser_runtime_bg.wasm"))
        .map_or(0, |metadata| metadata.len());
    eprintln!(
        "[tilo-app] web bundle {} ({} KB wasm, {profile})",
        bundle.display(),
        size / 1000
    );
    Ok(BuildOutcome {
        target: Target::Web,
        artifact: bundle,
        entry: web_crate.entry,
    })
}

fn wasm_stamp(wasm: &Path, release: bool) -> Result<String> {
    let metadata =
        std::fs::metadata(wasm).with_context(|| format!("reading {}", wasm.display()))?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_nanos());
    Ok(format!(
        "{}\n{}\n{modified}\n{release}\n",
        wasm.display(),
        metadata.len()
    ))
}

fn resolve_bundle(project: &Project, requested: WebBundle) -> Result<WebBundle> {
    let listed = project.in_sdk_checkout() && desktop_links(project)?;
    match requested {
        WebBundle::Desktop if !project.in_sdk_checkout() => bail!(
            "the desktop bundle links Tilo's own apps; build it inside a Tilo checkout\n  help: use `--bundle app` for this app alone"
        ),
        WebBundle::Desktop if !listed => bail!(
            "{} is not linked into the desktop bundle\n  help: run `python3 tools/web/generate_web_desktop.py` after declaring \"web\" in its platforms, or use `--bundle app`",
            project.package
        ),
        WebBundle::Auto if listed => Ok(WebBundle::Desktop),
        WebBundle::Auto => Ok(WebBundle::App),
        other => Ok(other),
    }
}

fn desktop_links(project: &Project) -> Result<bool> {
    let manifest = read_toml(&project.sdk_root.join(DESKTOP).join("Cargo.toml"))?;
    Ok(manifest
        .get("target")
        .and_then(toml::Value::as_table)
        .into_iter()
        .flat_map(|targets| targets.values())
        .filter_map(|target| target.get("dependencies"))
        .any(|dependencies| dependencies.get(&project.package).is_some()))
}

fn desktop_crate(project: &Project) -> Result<WebCrate> {
    let status = ProcessCommand::new("python3")
        .arg(project.sdk_root.join("tools/web/generate_web_desktop.py"))
        .arg("--check")
        .current_dir(&project.sdk_root)
        .status()
        .context("running tools/web/generate_web_desktop.py --check")?;
    if !status.success() {
        bail!(
            "the desktop bundle is stale\n  help: run `python3 tools/web/generate_web_desktop.py`"
        );
    }
    let crate_dir = project.sdk_root.join(DESKTOP);
    Ok(WebCrate {
        manifest: crate_dir.join("Cargo.toml"),
        package: "tilo-web-desktop".into(),
        target_dir: None,
        apps_json: std::fs::read_to_string(crate_dir.join("apps.json"))
            .context("reading the desktop bundle's apps.json")?,
        entry: format!("index.html?app={}", project.manifest.package.id),
    })
}

/// Generate the single-app bundle crate for `project`.
fn app_crate(project: &Project) -> Result<WebCrate> {
    let crate_dir = project.output_dir(Target::Web).join("crate");
    std::fs::create_dir_all(&crate_dir)
        .with_context(|| format!("creating {}", crate_dir.display()))?;
    let manifest = app_crate_manifest(project)?;
    write_if_changed(&crate_dir.join("Cargo.toml"), &manifest)?;
    write_if_changed(&crate_dir.join("lib.rs"), &app_crate_source(project)?)?;
    let lock = project.workspace_root.join("Cargo.lock");
    let copied = crate_dir.join("Cargo.lock");
    if lock.is_file() && !copied.is_file() {
        std::fs::copy(&lock, &copied).context("seeding the bundle crate's Cargo.lock")?;
    }
    let package = &project.manifest.package;
    let apps = serde_json::json!({
        "schema": 1,
        "applications": [{
            "id": package.id,
            "name": project.display_name(),
            "category": package.category.clone().unwrap_or_default(),
            "version": package.version,
        }],
    });
    Ok(WebCrate {
        manifest: crate_dir.join("Cargo.toml"),
        package: bundle_package_name(project),
        target_dir: Some(project.target_dir.clone()),
        apps_json: serde_json::to_string_pretty(&apps)? + "\n",
        entry: format!("index.html?app={}", package.id),
    })
}

fn bundle_package_name(project: &Project) -> String {
    format!("tilo-web-bundle-{}", project.package)
}

fn app_crate_manifest(project: &Project) -> Result<String> {
    let app_manifest = read_toml(&project.cargo_manifest)?;
    let root_manifest_path = project.workspace_root.join("Cargo.toml");
    let root_manifest = read_toml(&root_manifest_path)?;
    let (ui, ui_base) = match app_manifest
        .get("dependencies")
        .and_then(|dependencies| dependencies.get("ui"))
    {
        Some(dependency)
            if dependency.get("workspace").and_then(toml::Value::as_bool) == Some(true) =>
        {
            let inherited = root_manifest
                .get("workspace")
                .and_then(|workspace| workspace.get("dependencies"))
                .and_then(|dependencies| dependencies.get("ui"))
                .context("`ui.workspace = true` but the workspace declares no `ui`")?;
            (inherited.clone(), project.workspace_root.clone())
        }
        Some(dependency) => (dependency.clone(), project.app_dir.clone()),
        None => bail!(
            "{} has no `ui` dependency\n  help: apps depend on the SDK as `ui = {{ package = \"tilo-app\", … }}`",
            project.cargo_manifest.display()
        ),
    };
    let mut ui = source_only(&ui, &ui_base);
    let runtime = sibling_source(&ui, &project.sdk_root, "tilo-browser-runtime", RUNTIME);
    if let Some(table) = ui.as_table_mut() {
        table.insert("package".into(), "tilo-app".into());
    }
    let bindgen_version = pinned_wasm_bindgen(&project.sdk_root)?;
    let mut dependencies = toml::Table::new();
    dependencies.insert("tilo-browser-runtime".into(), runtime);
    dependencies.insert("ui".into(), ui);
    dependencies.insert("wasm-bindgen".into(), format!("={bindgen_version}").into());
    let mut app = toml::Table::new();
    app.insert("package".into(), project.package.clone().into());
    app.insert("path".into(), project.app_dir.display().to_string().into());
    dependencies.insert("app".into(), app.into());

    let mut target = toml::Table::new();
    let mut wasm = toml::Table::new();
    wasm.insert("dependencies".into(), dependencies.into());
    target.insert("cfg(target_arch = \"wasm32\")".into(), wasm.into());

    let mut manifest = toml::Table::new();
    let mut package = toml::Table::new();
    package.insert("name".into(), bundle_package_name(project).into());
    package.insert("version".into(), "0.0.0".into());
    package.insert("edition".into(), "2024".into());
    package.insert("publish".into(), false.into());
    manifest.insert("package".into(), package.into());
    let mut lib = toml::Table::new();
    lib.insert("path".into(), "lib.rs".into());
    lib.insert(
        "crate-type".into(),
        vec![toml::Value::from("cdylib")].into(),
    );
    manifest.insert("lib".into(), lib.into());
    manifest.insert("target".into(), target.into());
    manifest.insert("workspace".into(), toml::Table::new().into());
    if let Some(patch) = root_manifest.get("patch") {
        manifest.insert(
            "patch".into(),
            absolutize_paths(patch, &project.workspace_root),
        );
    }
    if let Some(profile) = root_manifest.get("profile") {
        let locked = locked_packages(&project.workspace_root.join("Cargo.lock"));
        manifest.insert(
            "profile".into(),
            without_unknown_overrides(profile, &locked),
        );
    }
    Ok(format!(
        "# Generated by `tilo-app build --target web` for {}; do not edit.\n{}",
        project.package,
        toml::to_string(&manifest).context("rendering the bundle crate manifest")?
    ))
}

fn locked_packages(lock: &Path) -> std::collections::BTreeSet<String> {
    read_toml(lock)
        .ok()
        .and_then(|lock| lock.get("package").and_then(toml::Value::as_array).cloned())
        .into_iter()
        .flatten()
        .filter(|package| package.get("source").is_some())
        .filter_map(|package| {
            package
                .get("name")
                .and_then(toml::Value::as_str)
                .map(str::to_owned)
        })
        .collect()
}

/// Profiles keeping per-package overrides only for published dependencies:
/// overrides naming the app workspace's own members (Cargo warns about
/// those outside that workspace) are dropped.
fn without_unknown_overrides(
    profiles: &toml::Value,
    locked: &std::collections::BTreeSet<String>,
) -> toml::Value {
    let mut profiles = profiles.clone();
    if let Some(profiles) = profiles.as_table_mut() {
        for (_, profile) in profiles.iter_mut() {
            if let Some(packages) = profile
                .get_mut("package")
                .and_then(toml::Value::as_table_mut)
            {
                packages.retain(|name, _| name == "*" || locked.contains(name));
            }
        }
    }
    profiles
}

/// A dependency with only its source keys, paths made absolute.
fn source_only(dependency: &toml::Value, base: &Path) -> toml::Value {
    let mut source = toml::Table::new();
    if let Some(table) = dependency.as_table() {
        for key in ["path", "git", "rev", "tag", "branch", "version", "registry"] {
            if let Some(value) = table.get(key) {
                source.insert(key.into(), value.clone());
            }
        }
        if let Some(path) = table.get("path").and_then(toml::Value::as_str) {
            source.insert("path".into(), absolute(base, path).into());
        }
    } else if let Some(version) = dependency.as_str() {
        source.insert("version".into(), version.into());
    }
    source.into()
}

/// The same source as `ui` for another SDK crate: the same git revision, or
/// its path in the same checkout.
fn sibling_source(ui: &toml::Value, sdk_root: &Path, package: &str, path: &str) -> toml::Value {
    let mut source = toml::Table::new();
    if let Some(table) = ui.as_table() {
        if table.contains_key("path") {
            source.insert(
                "path".into(),
                sdk_root.join(path).display().to_string().into(),
            );
        } else {
            for key in ["git", "rev", "tag", "branch", "version", "registry"] {
                if let Some(value) = table.get(key) {
                    source.insert(key.into(), value.clone());
                }
            }
            source.insert("package".into(), package.into());
        }
    }
    source.into()
}

fn absolute(base: &Path, path: &str) -> String {
    let path = Path::new(path);
    if path.is_absolute() {
        path.display().to_string()
    } else {
        base.join(path).display().to_string()
    }
}

/// `[patch]` with every relative `path` resolved against `base`.
fn absolutize_paths(value: &toml::Value, base: &Path) -> toml::Value {
    match value {
        toml::Value::Table(table) => toml::Value::Table(
            table
                .iter()
                .map(|(key, value)| {
                    let value = match (key.as_str(), value.as_str()) {
                        ("path", Some(path)) => absolute(base, path).into(),
                        _ => absolutize_paths(value, base),
                    };
                    (key.clone(), value)
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

fn app_crate_source(project: &Project) -> Result<String> {
    let app_manifest = read_toml(&project.cargo_manifest)?;
    let lib_path = app_manifest
        .get("lib")
        .and_then(|lib| lib.get("path"))
        .and_then(toml::Value::as_str)
        .unwrap_or("src/lib.rs");
    let lib_source = std::fs::read_to_string(project.app_dir.join(lib_path))
        .with_context(|| format!("reading {lib_path}; the web target links the app's library"))?;
    let definition = if lib_source.contains("pub fn definition(") {
        "app::definition()"
    } else if lib_source.contains("impl ResourceApp for App") {
        "ui::framework::definition::<app::App>()"
    } else {
        bail!(
            "{} exports neither `pub fn definition()` nor a `ResourceApp` named `App`\n  help: the web page runtime mounts an application definition; export `pub fn definition() -> &'static ui::application::ApplicationDefinition`",
            project.package
        );
    };
    Ok(format!(
        "//! Generated by `tilo-app build --target web`; do not edit.\n//! The browser runtime with {package} registered before the page mounts it.\n#![cfg(target_arch = \"wasm32\")]\n\n// Linked for its `#[wasm_bindgen]` exports, not named.\nextern crate tilo_browser_runtime;\n\n#[wasm_bindgen::prelude::wasm_bindgen(start)]\nfn start() {{\n    ui::application::registry::register({definition});\n}}\n",
        package = project.package,
    ))
}

fn write_if_changed(path: &Path, contents: &str) -> Result<()> {
    if std::fs::read_to_string(path).is_ok_and(|existing| existing == contents) {
        return Ok(());
    }
    std::fs::write(path, contents).with_context(|| format!("writing {}", path.display()))
}

fn pinned_wasm_bindgen(sdk_root: &Path) -> Result<String> {
    let manifest = read_toml(&sdk_root.join(RUNTIME).join("Cargo.toml"))?;
    let targets = manifest
        .get("target")
        .and_then(toml::Value::as_table)
        .into_iter()
        .flat_map(|targets| targets.values())
        .filter_map(|target| target.get("dependencies"));
    manifest
        .get("dependencies")
        .into_iter()
        .chain(targets)
        .find_map(|dependencies| dependencies.get("wasm-bindgen"))
        .and_then(|dependency| {
            dependency
                .as_str()
                .or_else(|| dependency.get("version").and_then(toml::Value::as_str))
        })
        .map(|version| version.trim_start_matches('=').to_owned())
        .context("the browser runtime does not pin wasm-bindgen")
}

/// The wasm-bindgen CLI, checked against the runtime's pinned crate.
fn wasm_bindgen(project: &Project) -> Result<PathBuf> {
    let program = std::env::var_os("WASM_BINDGEN")
        .map_or_else(|| PathBuf::from("wasm-bindgen"), PathBuf::from);
    let pinned = pinned_wasm_bindgen(&project.sdk_root)?;
    let output = ProcessCommand::new(&program).arg("--version").output();
    let found = output
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
    match found {
        Some(version) if version == format!("wasm-bindgen {pinned}") => Ok(program),
        Some(version) => bail!(
            "{version} does not match the SDK's wasm-bindgen {pinned}\n  help: cargo install wasm-bindgen-cli --version {pinned} --locked"
        ),
        None => bail!(
            "the web target needs the wasm-bindgen CLI {pinned}\n  help: cargo install wasm-bindgen-cli --version {pinned} --locked\n        rustup target add {WASM_TARGET}"
        ),
    }
}

/// `wasm-release` when the workspace defines it (small, for shipping),
/// otherwise `release`; `dev` for iteration.
fn web_profile(project: &Project, release: bool) -> Result<String> {
    if !release {
        return Ok("dev".into());
    }
    let root = read_toml(&project.workspace_root.join("Cargo.toml"))?;
    let has_wasm_release = root
        .get("profile")
        .and_then(|profiles| profiles.get("wasm-release"))
        .is_some();
    Ok(if has_wasm_release {
        "wasm-release"
    } else {
        "release"
    }
    .into())
}

fn cargo_build_wasm(
    project: &Project,
    web_crate: &WebCrate,
    profile: &str,
    request: &TargetRequest,
) -> Result<PathBuf> {
    let crate_dir = web_crate
        .manifest
        .parent()
        .context("bundle crate manifest has no directory")?;
    let mut command = ProcessCommand::new(cargo_program());
    command
        .arg("build")
        .arg("--manifest-path")
        .arg(&web_crate.manifest)
        .args(["--lib", "--target", WASM_TARGET, "--profile", profile])
        .arg("--message-format=json-render-diagnostics")
        .current_dir(if web_crate.target_dir.is_some() {
            crate_dir
        } else {
            project.sdk_root.as_path()
        })
        .stdout(Stdio::piped());
    if web_crate.target_dir.is_none() {
        command.arg("--locked");
    }
    if let Some(target_dir) = &web_crate.target_dir {
        command.arg("--target-dir").arg(target_dir);
    }
    if request.no_default_features {
        eprintln!("[tilo-app] --no-default-features does not apply to web bundles");
    }
    let mut child = command.spawn().context("starting the wasm build")?;
    let stdout = child
        .stdout
        .take()
        .context("cargo has no artifact stream")?;
    let expected = web_crate
        .manifest
        .canonicalize()
        .unwrap_or_else(|_| web_crate.manifest.clone());
    let mut wasm = None;
    for line in std::io::BufRead::lines(std::io::BufReader::new(stdout)) {
        let line = line.context("reading cargo artifact stream")?;
        let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) else {
            eprintln!("{line}");
            continue;
        };
        match message.get("reason").and_then(serde_json::Value::as_str) {
            Some("compiler-message") => {
                if let Some(rendered) = message
                    .pointer("/message/rendered")
                    .and_then(serde_json::Value::as_str)
                {
                    eprint!("{rendered}");
                }
            }
            Some("compiler-artifact")
                if message
                    .get("manifest_path")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|path| Path::new(path) == expected) =>
            {
                wasm = message
                    .get("filenames")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_str)
                    .find(|name| {
                        Path::new(name)
                            .extension()
                            .is_some_and(|extension| extension.eq_ignore_ascii_case("wasm"))
                    })
                    .map(PathBuf::from)
                    .or(wasm);
            }
            _ => {}
        }
    }
    let status = child.wait().context("waiting for the wasm build")?;
    if !status.success() {
        bail!(
            "the wasm build of {} failed with {status}",
            web_crate.package
        );
    }
    wasm.with_context(|| format!("cargo reported no .wasm for {}", web_crate.package))
}

fn optimize(wasm: &Path) -> Result<()> {
    let program = std::env::var_os("WASM_OPT").unwrap_or_else(|| "wasm-opt".into());
    let available = ProcessCommand::new(&program)
        .arg("--version")
        .stdout(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if !available {
        eprintln!("[tilo-app] wasm-opt not found; the release bundle is not size-optimized");
        return Ok(());
    }
    run(
        ProcessCommand::new(&program)
            .args([
                "-Oz",
                "--enable-bulk-memory",
                "--enable-nontrapping-float-to-int",
                // Folds constant address offsets into loads and stores
                // (about 540 KB of code). Sound because rustc links wasm32
                // stack-first: the first KiB of memory is the bottom of the
                // 1 MiB stack, reached only by a stack about to overflow.
                "--low-memory-unused",
            ])
            .arg(wasm)
            .arg("-o")
            .arg(wasm),
        "wasm-opt",
    )
}

fn copy_page_host(sdk_root: &Path, bundle: &Path) -> Result<()> {
    let web = sdk_root.join(RUNTIME).join("web");
    for entry in std::fs::read_dir(&web).with_context(|| format!("reading {}", web.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if runtime_module(name) || name == "index.html" {
            link_or_copy(&entry.path(), &bundle.join(name))?;
        }
    }
    Ok(())
}

/// The live plane's `MoQ` library, bundled from the `@moq/net` pinned in
/// `scripts/moq-browser/package-lock.json` as `build.sh` does; it is never
/// committed. Without it `live-transport.mjs` cannot load and the page keeps
/// to the relay fallback. Rebuilt only when the lock file is newer.
fn bundle_moq_library(sdk_root: &Path, bundle: &Path) -> Result<()> {
    let builder = sdk_root.join(MOQ_BROWSER);
    let lock = builder.join("package-lock.json");
    let output = bundle.join(MOQ_LIBRARY);
    let modified = |path: &Path| std::fs::metadata(path).and_then(|metadata| metadata.modified());
    if let (Ok(built), Ok(pinned)) = (modified(&output), modified(&lock))
        && built >= pinned
    {
        return Ok(());
    }
    if !lock.is_file() {
        eprintln!(
            "[tilo-app] {} is missing; the bundle has no live plane",
            lock.display()
        );
        return Ok(());
    }
    // A development copy may have been linked from the page host's sources.
    if output.exists() {
        std::fs::remove_file(&output).with_context(|| format!("replacing {}", output.display()))?;
    }
    run(
        ProcessCommand::new("npm")
            .args(["ci", "--no-audit", "--no-fund", "--silent"])
            .current_dir(&builder),
        "npm ci (scripts/moq-browser)",
    )?;
    run(
        ProcessCommand::new("node")
            .arg("build.mjs")
            .arg(&output)
            .current_dir(&builder),
        "the MoQ library build",
    )
}

fn runtime_module(name: &str) -> bool {
    !name.contains('/')
        && Path::new(name)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("mjs"))
        && !name.ends_with(".test.mjs")
}

/// The product fonts and `fonts.json`, as `build.sh` writes them. Fonts are
/// linked, not copied: they are 40 MB and never change in place.
fn copy_fonts(sdk_root: &Path, bundle: &Path) -> Result<()> {
    let source = sdk_root.join(FONTS);
    let manifest_source = std::fs::read_to_string(source.join("manifest.toml"))
        .context("reading the font manifest")?;
    let manifest: toml::Table = manifest_source
        .parse()
        .context("parsing the font manifest")?;
    let fonts = bundle.join("fonts");
    std::fs::create_dir_all(&fonts).context("creating fonts/")?;
    let mut assets = Vec::new();
    for font in manifest
        .get("font")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
    {
        if font.get("scope").and_then(toml::Value::as_str) != Some("product") {
            continue;
        }
        let name = font
            .get("file")
            .and_then(toml::Value::as_str)
            .context("font without a file")?;
        let destination = fonts.join(name);
        let bytes = font
            .get("bytes")
            .and_then(toml::Value::as_integer)
            .unwrap_or(0);
        let present = std::fs::metadata(&destination)
            .is_ok_and(|metadata| i64::try_from(metadata.len()).is_ok_and(|len| len == bytes));
        if !present {
            link_or_copy(&source.join(name), &destination)?;
        }
        assets.push(serde_json::json!({
            "name": name,
            "bytes": bytes,
            "sha256": font.get("sha256").and_then(toml::Value::as_str).unwrap_or(""),
            "startup": font.get("browser_startup").and_then(toml::Value::as_bool).unwrap_or(false),
        }));
    }
    if !assets
        .iter()
        .any(|asset| asset.get("startup").and_then(serde_json::Value::as_bool) == Some(true))
    {
        bail!("the font manifest marks no browser startup fonts");
    }
    copy_tree(&source.join("LICENSES"), &fonts.join("LICENSES"))?;
    link_or_copy(&source.join("manifest.toml"), &fonts.join("manifest.toml"))?;
    let catalog = manifest
        .get("catalog_id")
        .and_then(toml::Value::as_str)
        .context("the font manifest has no catalog_id")?;
    let descriptor = serde_json::json!({"schema": 1, "catalog": catalog, "assets": assets});
    std::fs::write(
        bundle.join("fonts.json"),
        serde_json::to_string(&descriptor)? + "\n",
    )
    .context("writing fonts.json")
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    std::fs::create_dir_all(destination)
        .with_context(|| format!("creating {}", destination.display()))?;
    for entry in
        std::fs::read_dir(source).with_context(|| format!("reading {}", source.display()))?
    {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            link_or_copy(&entry.path(), &target)?;
        }
    }
    Ok(())
}

fn git_revision(root: &Path) -> Option<String> {
    let output = ProcessCommand::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn run(command: &mut ProcessCommand, what: &str) -> Result<()> {
    let status = command
        .status()
        .with_context(|| format!("starting {what}"))?;
    if !status.success() {
        bail!("{what} failed with {status}");
    }
    Ok(())
}

/// One file of a web version: its path in the bundle, size and SHA-256.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct VersionFile {
    pub name: String,
    pub size: u64,
    pub sha256: String,
}

/// The files Cloud packages from a bundle, in Cloud's order: top-level
/// names sorted, `pkg/` and `fonts/` expanded in place, recursively sorted.
pub(super) fn packaged_files(bundle: &Path) -> Result<Vec<VersionFile>> {
    let mut files = Vec::new();
    for name in sorted_names(bundle)? {
        let path = bundle.join(&name);
        if name == "pkg" || name == "fonts" {
            if !path.is_dir() {
                bail!("{name} in {} must be a directory", bundle.display());
            }
            collect_tree(&path, &format!("{name}/"), &mut files)?;
        } else if runtime_module(&name) || name == "fonts.json" || name == "apps.json" {
            files.push(version_file(&path, name)?);
        }
    }
    Ok(files)
}

fn sorted_names(directory: &Path) -> Result<Vec<String>> {
    let mut names = std::fs::read_dir(directory)
        .with_context(|| format!("reading {}", directory.display()))?
        .map(|entry| {
            entry.map_err(anyhow::Error::from).and_then(|entry| {
                entry
                    .file_name()
                    .into_string()
                    .map_err(|name| anyhow::anyhow!("non-UTF-8 file name {}", name.display()))
            })
        })
        .collect::<Result<Vec<_>>>()?;
    // JavaScript's default sort compares UTF-16 code units.
    names.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
    Ok(names)
}

fn collect_tree(directory: &Path, prefix: &str, files: &mut Vec<VersionFile>) -> Result<()> {
    for name in sorted_names(directory)? {
        let path = directory.join(&name);
        let relative = format!("{prefix}{name}");
        let kind = std::fs::symlink_metadata(&path)?.file_type();
        if kind.is_dir() {
            collect_tree(&path, &format!("{relative}/"), files)?;
        } else if kind.is_file() {
            files.push(version_file(&path, relative)?);
        } else {
            bail!("web bundle files must be regular files: {relative}");
        }
    }
    Ok(())
}

fn version_file(path: &Path, name: String) -> Result<VersionFile> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(VersionFile {
        name,
        size: bytes.len() as u64,
        sha256: hex_digest(&bytes),
    })
}

fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// `JSON.stringify(files)` exactly, whose SHA-256 is the version digest.
pub(super) fn files_json(files: &[VersionFile]) -> Result<String> {
    let mut out = String::from("{");
    for (index, file) in files.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let _ = write!(
            out,
            "{}:{{\"sha256\":\"{}\",\"size\":{}}}",
            serde_json::to_string(&file.name)?,
            file.sha256,
            file.size
        );
    }
    out.push('}');
    Ok(out)
}

/// `JSON.stringify({digest, files}, null, 2) + "\n"`.
fn assets_json(digest: &str, files: &[VersionFile]) -> Result<String> {
    let mut out = format!("{{\n  \"digest\": \"{digest}\",\n  \"files\": {{");
    for (index, file) in files.iter().enumerate() {
        out.push_str(if index == 0 { "\n" } else { ",\n" });
        let _ = write!(
            out,
            "    {}: {{\n      \"sha256\": \"{}\",\n      \"size\": {}\n    }}",
            serde_json::to_string(&file.name)?,
            file.sha256,
            file.size
        );
    }
    out.push_str(if files.is_empty() {
        "}\n}\n"
    } else {
        "\n  }\n}\n"
    });
    Ok(out)
}

/// Write `bundle` as an immutable version under the output directory:
/// `versions/<digest>/` (the files plus `assets.json`), a `build.json`
/// naming the digest, and one stable `*.mjs` entry per runtime module that
/// re-exports the version. An existing version is reused only if it is
/// byte-identical; it is never modified.
pub(super) fn package(
    project: &Project,
    bundle: &Path,
    request: &TargetPackageRequest,
) -> Result<PathBuf> {
    let output = request.output.as_ref().map_or_else(
        || {
            project
                .app_dir
                .join("dist/web")
                .join(&project.manifest.package.id)
        },
        |path| crate::absolute_from(&project.app_dir, path),
    );
    let files = packaged_files(bundle)?;
    for required in [
        "pkg/tilo_browser_runtime.js",
        "pkg/tilo_browser_runtime_bg.wasm",
        "host.mjs",
        "fonts.json",
        "apps.json",
    ] {
        if !files.iter().any(|file| file.name == required) {
            bail!("the web bundle has no {required}");
        }
    }
    let digest = hex_digest(files_json(&files)?.as_bytes());
    let version = output.join("versions").join(&digest);
    if version.exists() {
        let mut existing = Vec::new();
        collect_tree(&version, "", &mut existing)?;
        existing.retain(|file| file.name != "assets.json");
        let mut expected = files.clone();
        expected.sort_by(|left, right| left.name.cmp(&right.name));
        existing.sort_by(|left, right| left.name.cmp(&right.name));
        if existing != expected {
            bail!(
                "{} exists with different contents; immutable versions are never modified",
                version.display()
            );
        }
        eprintln!("[tilo-app] version {digest} already packaged; reusing it");
    } else {
        let staging = output.join("versions").join(format!(".staging-{digest}"));
        if staging.exists() {
            std::fs::remove_dir_all(&staging)
                .with_context(|| format!("clearing {}", staging.display()))?;
        }
        for file in &files {
            let destination = staging.join(&file.name);
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            std::fs::copy(bundle.join(&file.name), &destination)
                .with_context(|| format!("copying {}", file.name))?;
        }
        std::fs::write(staging.join("assets.json"), assets_json(&digest, &files)?)
            .context("writing assets.json")?;
        std::fs::rename(&staging, &version)
            .with_context(|| format!("publishing {}", version.display()))?;
    }
    let package = &project.manifest.package;
    let build = serde_json::json!({
        "digest": digest,
        "bundle": if request.build.bundle == WebBundle::Desktop { "desktop" } else { "app" },
        "app": {"id": package.id, "name": project.display_name(), "version": package.version},
        "platforms": package.platforms().iter().map(|platform| platform.as_str()).collect::<Vec<_>>(),
        "capabilities": {
            "required": project.manifest.capabilities.required,
            "optional": project.manifest.capabilities.optional,
        },
        "revision": git_revision(&project.sdk_root),
        "profile": "release",
    });
    std::fs::write(
        output.join("build.json"),
        serde_json::to_string_pretty(&build)? + "\n",
    )
    .context("writing build.json")?;
    for file in files.iter().filter(|file| runtime_module(&file.name)) {
        std::fs::write(
            output.join(&file.name),
            format!("export * from './versions/{digest}/{}';\n", file.name),
        )
        .with_context(|| format!("writing the {} entry", file.name))?;
    }
    let archive = registry_zip_path(&output, &package.version);
    write_registry_zip(&archive, &output, &project.manifest_path, &digest)?;
    eprintln!(
        "[tilo-app] packaged {} (version {digest}; registry artifact {})",
        output.display(),
        archive.display()
    );
    Ok(output)
}

/// `<output>-<version>-web.zip`, beside the version directory.
fn registry_zip_path(output: &Path, version: &str) -> PathBuf {
    let name = output
        .file_name()
        .map_or_else(|| "web".into(), |name| name.to_string_lossy().into_owned());
    output.with_file_name(format!("{name}-{version}-web.zip"))
}

/// The registry's web artifact: `build.json`, the app's `tilo-app.toml` and
/// `versions/<digest>/` (with its `assets.json`), in sorted order with fixed
/// timestamps, so the same version zips to the same bytes.
fn write_registry_zip(archive: &Path, output: &Path, manifest: &Path, digest: &str) -> Result<()> {
    use std::io::Write as _;
    let mut entries = vec![
        ("build.json".to_owned(), output.join("build.json")),
        ("tilo-app.toml".to_owned(), manifest.to_path_buf()),
    ];
    let mut version = Vec::new();
    collect_tree(&output.join("versions").join(digest), "", &mut version)?;
    version.sort_by(|left, right| left.name.cmp(&right.name));
    entries.extend(version.into_iter().map(|file| {
        let path = output.join("versions").join(digest).join(&file.name);
        (format!("versions/{digest}/{}", file.name), path)
    }));
    let staging = archive.with_extension("zip.partial");
    let mut writer = zip::ZipWriter::new(
        std::fs::File::create(&staging)
            .with_context(|| format!("creating {}", staging.display()))?,
    );
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(zip::DateTime::default())
        .unix_permissions(0o644);
    for (name, path) in &entries {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        writer
            .start_file(name.as_str(), options)
            .with_context(|| format!("zipping {name}"))?;
        writer
            .write_all(&bytes)
            .with_context(|| format!("zipping {name}"))?;
    }
    writer.finish().context("finishing the web zip")?;
    std::fs::rename(&staging, archive).with_context(|| format!("publishing {}", archive.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, size: u64) -> VersionFile {
        VersionFile {
            name: name.into(),
            size,
            sha256: "ab".repeat(32),
        }
    }

    #[test]
    fn files_json_matches_json_stringify() {
        let files = [file("apps.json", 3), file("fonts/A \"b\".ttf", 10)];
        assert_eq!(
            files_json(&files).unwrap(),
            format!(
                "{{\"apps.json\":{{\"sha256\":\"{h}\",\"size\":3}},\"fonts/A \\\"b\\\".ttf\":{{\"sha256\":\"{h}\",\"size\":10}}}}",
                h = "ab".repeat(32)
            )
        );
    }

    #[test]
    fn assets_json_matches_pretty_json_stringify() {
        let text = assets_json("d", &[file("a.mjs", 1)]).unwrap();
        assert_eq!(
            text,
            format!(
                "{{\n  \"digest\": \"d\",\n  \"files\": {{\n    \"a.mjs\": {{\n      \"sha256\": \"{}\",\n      \"size\": 1\n    }}\n  }}\n}}\n",
                "ab".repeat(32)
            )
        );
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["files"]["a.mjs"]["size"], 1);
    }

    #[test]
    fn packaged_files_follow_clouds_order_and_selection() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for path in [
            "pkg/tilo_browser_runtime.js",
            "fonts/B.ttf",
            "fonts/LICENSES/a.txt",
            "fonts.json",
            "host.mjs",
            "host.test.mjs",
            "apps.json",
            "index.html",
            "build.json",
        ] {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"x").unwrap();
        }
        let names: Vec<String> = packaged_files(root)
            .unwrap()
            .into_iter()
            .map(|file| file.name)
            .collect();
        assert_eq!(
            names,
            [
                "apps.json",
                "fonts/B.ttf",
                "fonts/LICENSES/a.txt",
                "fonts.json",
                "host.mjs",
                "pkg/tilo_browser_runtime.js"
            ]
        );
    }

    #[test]
    fn the_registry_zip_is_build_json_the_manifest_and_the_version() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("so.tilo.hello");
        let version = output.join("versions/abc");
        std::fs::create_dir_all(version.join("pkg")).unwrap();
        std::fs::write(output.join("build.json"), b"{}").unwrap();
        std::fs::write(output.join("host.mjs"), b"entry").unwrap();
        std::fs::write(version.join("host.mjs"), b"x").unwrap();
        std::fs::write(version.join("assets.json"), b"{}").unwrap();
        std::fs::write(version.join("pkg/a.wasm"), b"y").unwrap();
        let manifest = dir.path().join("tilo-app.toml");
        std::fs::write(&manifest, b"[package]").unwrap();
        let archive = registry_zip_path(&output, "1.2.3");
        assert_eq!(archive, dir.path().join("so.tilo.hello-1.2.3-web.zip"));
        write_registry_zip(&archive, &output, &manifest, "abc").unwrap();
        let first = std::fs::read(&archive).unwrap();
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(first.clone())).unwrap();
        let mut names: Vec<String> = zip.file_names().map(str::to_owned).collect();
        names.sort();
        assert_eq!(
            names,
            [
                "build.json",
                "tilo-app.toml",
                "versions/abc/assets.json",
                "versions/abc/host.mjs",
                "versions/abc/pkg/a.wasm"
            ]
        );
        assert!(
            zip.by_name("host.mjs").is_err(),
            "entry stubs are the host's"
        );
        write_registry_zip(&archive, &output, &manifest, "abc").unwrap();
        assert_eq!(std::fs::read(&archive).unwrap(), first, "reproducible");
    }

    #[test]
    fn patch_paths_become_absolute() {
        let patch: toml::Value = toml::from_str(
            "[crates-io]\nwinit = { path = \"vendor/winit\" }\naccesskit = { git = \"ssh://x\", rev = \"1\" }\n",
        )
        .unwrap();
        let absolute = absolutize_paths(&patch, Path::new("/sdk"));
        assert_eq!(
            absolute["crates-io"]["winit"]["path"].as_str(),
            Some("/sdk/vendor/winit")
        );
        assert_eq!(
            absolute["crates-io"]["accesskit"]["git"].as_str(),
            Some("ssh://x")
        );
    }

    #[test]
    fn the_runtime_follows_the_sdk_source() {
        let git: toml::Value =
            toml::from_str("git = \"https://code.tilo.so/tilo/tilo.git\"\ntag = \"sdk-2026.09\"\n")
                .unwrap();
        let runtime = sibling_source(&git, Path::new("/unused"), "tilo-browser-runtime", RUNTIME);
        assert_eq!(runtime["tag"].as_str(), Some("sdk-2026.09"));
        assert_eq!(runtime["package"].as_str(), Some("tilo-browser-runtime"));
        let path: toml::Value = toml::from_str("path = \"/sdk/crates/sdk/tilo-app\"\n").unwrap();
        let runtime = sibling_source(&path, Path::new("/sdk"), "tilo-browser-runtime", RUNTIME);
        assert_eq!(
            runtime["path"].as_str(),
            Some("/sdk/crates/browser/tilo-browser-runtime")
        );
    }
}
