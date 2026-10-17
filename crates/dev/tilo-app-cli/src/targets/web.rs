//! The web target: a bundle the browser runtime's page host loads.
//!
//! A bundle is `crates/browser/tilo-browser-runtime/scripts/build.sh`'s
//! layout: `pkg/` (wasm-bindgen output under the runtime's module name), the
//! page host's `*.mjs` and `index.html`, the entry pages' `locales/` (the
//! arrival screen's lines), the product fonts with `fonts.json`, and
//! `apps.json`. Two bundles exist:
//!
//! - **desktop**: `crates/browser/tilo-web-desktop`, every web-capable Tilo
//!   app, opened at `app/#/me/<id>`. Used for Tilo's own apps, so they
//!   share one build. Its module is split as the page's is
//!   (`tools/web/wasm-split/split.sh`): a start module, and one module per
//!   app fetched when the app first opens.
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
const MOQ_BROWSER: &str = "scripts/moq-browser";
const MOQ_LIBRARY: &str = "media-moq-library.mjs";
const SPLIT: &str = "tools/web/wasm-split/split.sh";

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
    let split = (web_crate.package == "tilo-web-desktop").then(|| project.sdk_root.join(SPLIT));
    let profile = match web_profile(project, request.release)? {
        // The splitter reads the symbol table `wasm-release` strips.
        profile if split.is_some() && profile == "wasm-release" => "wasm-split".to_owned(),
        profile => profile,
    };
    let wasm = cargo_build_wasm(project, &web_crate, &profile, request)?;
    let bundle = project.output_dir(Target::Web).join("bundle");
    std::fs::create_dir_all(bundle.join("pkg"))
        .with_context(|| format!("creating {}", bundle.display()))?;
    let optimizer = Optimizer::discover(request.release);
    bundle_runtime(
        &wasm,
        request.release,
        &bindgen,
        &bundle,
        optimizer.as_ref(),
        split.as_deref(),
    )?;
    let compute = build_compute(project, request, &bindgen, &bundle, optimizer.as_ref())?;
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
        "compute": compute,
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

const COMPUTE: &str = "crates/browser/tilo-browser-compute";
const COMPUTE_ASSETS: [&str; 5] = [
    "compute-worker.mjs",
    "compute-domains.mjs",
    "compute-protocol.mjs",
    "pkg/compute/tilo_browser_compute.js",
    "pkg/compute/tilo_browser_compute_bg.wasm",
];
const WASM_OPT_FLAGS: [&str; 5] = [
    "-Oz",
    "--enable-bulk-memory",
    "--enable-nontrapping-float-to-int",
    // A split module keeps the name section the splitter read.
    "--strip-debug",
    // Sound for rustc's stack-first wasm32 layout: the first KiB is the
    // bottom of the stack, reached only by a stack about to overflow.
    "--low-memory-unused",
];

/// The optimizer identity and arguments are shared by the cache and invocation.
struct Optimizer {
    program: PathBuf,
    version: String,
    flags: &'static [&'static str],
}

impl Optimizer {
    fn discover(release: bool) -> Option<Self> {
        let program =
            PathBuf::from(std::env::var_os("WASM_OPT").unwrap_or_else(|| "wasm-opt".into()));
        Self::probe(program, release)
    }

    fn probe(program: PathBuf, release: bool) -> Option<Self> {
        release
            .then(|| tool_version(&program).ok())
            .flatten()
            .map(|version| Self {
                program,
                version,
                flags: &WASM_OPT_FLAGS,
            })
    }

    fn input(&self) -> serde_json::Value {
        serde_json::json!({"program":self.program,"version":self.version,"flags":self.flags})
    }

    fn optimize(&self, wasm: &Path) -> Result<()> {
        run(
            ProcessCommand::new(&self.program)
                .args(self.flags)
                .arg(wasm)
                .arg("-o")
                .arg(wasm),
            "wasm-opt",
        )
    }
}

/// `split` is the splitter script when the module is split
/// (`tools/web/wasm-split/split.sh`).
fn bundle_runtime(
    wasm: &Path,
    release: bool,
    bindgen: &Path,
    bundle: &Path,
    optimizer: Option<&Optimizer>,
    split: Option<&Path>,
) -> Result<()> {
    // Cargo leaves an up-to-date module untouched; so does this step.
    let stamp = bundle.join(".wasm-input");
    let input = serde_json::to_string(&serde_json::json!({
        "wasm":wasm_stamp(wasm, release)?,
        "optimizer":optimizer.map(Optimizer::input),
        "split":split,
    }))?;
    let pkg = bundle.join("pkg");
    let output = pkg.join("tilo_browser_runtime_bg.wasm");
    if !output.is_file() || std::fs::read_to_string(&stamp).ok().as_deref() != Some(&input) {
        // Modules split off an earlier build would linger beside this one's.
        for entry in
            std::fs::read_dir(&pkg).with_context(|| format!("reading {}", pkg.display()))?
        {
            let path = entry?.path();
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if name.starts_with("tilo_browser_runtime.")
                && path
                    .extension()
                    .is_some_and(|extension| extension == "wasm")
            {
                std::fs::remove_file(&path)
                    .with_context(|| format!("removing {}", path.display()))?;
            }
        }
        let mut command = ProcessCommand::new(bindgen);
        command
            .arg(wasm)
            .args([
                "--target",
                "web",
                "--out-name",
                "tilo_browser_runtime",
                "--out-dir",
            ])
            .arg(&pkg);
        if split.is_some() {
            command.args(["--keep-lld-exports", "--no-demangle"]);
        }
        run(&mut command, "wasm-bindgen")?;
        if let Some(split) = split {
            run(
                ProcessCommand::new("bash").arg(split).arg(wasm).arg(&pkg),
                "wasm-split",
            )?;
        }
        if let Some(optimizer) = optimizer {
            for entry in
                std::fs::read_dir(&pkg).with_context(|| format!("reading {}", pkg.display()))?
            {
                let path = entry?.path();
                if path
                    .extension()
                    .is_some_and(|extension| extension == "wasm")
                {
                    optimizer.optimize(&path)?;
                }
            }
        } else if release {
            eprintln!("[tilo-app] wasm-opt not found; the release bundle is not size-optimized");
        }
        std::fs::write(&stamp, input).context("recording the bundled module")?;
    }
    Ok(())
}

fn compute_memory(wasm: &[u8], release: bool) -> Result<serde_json::Value> {
    if release && wasm.len() > 1024 * 1024 {
        bail!("compute exceeds its separate initial 1 MiB release budget");
    }
    if !wasm.starts_with(b"\0asm\x01\0\0\0") {
        bail!("invalid compute WASM header");
    }
    // Validate all sections and function bodies before any output can be stamped.
    wasmparser::Validator::new()
        .validate_all(wasm)
        .context("validating complete final compute WASM")?;
    let mut sections = wasm.get(8..).context("invalid compute WASM header")?;
    let mut memory = None;
    while !sections.is_empty() {
        let (&section, remaining) = sections
            .split_first()
            .context("truncated compute WASM section")?;
        sections = remaining;
        let length = usize::try_from(wasm_leb(&mut sections)?)?;
        let payload = sections
            .get(..length)
            .context("truncated compute WASM section")?;
        sections = sections
            .get(length..)
            .context("truncated compute WASM section")?;
        let mut payload = payload;
        if section == 2 {
            let count = wasm_leb(&mut payload)?;
            if count > if release { 2 } else { 3 } {
                bail!("unexpected compute imports");
            }
            for _ in 0..count {
                let module = wasm_name(&mut payload)?;
                let name = wasm_name(&mut payload)?;
                let (&kind, remaining) =
                    payload.split_first().context("truncated compute import")?;
                payload = remaining;
                if kind != 0
                    || module != "./tilo_browser_compute_bg.js"
                    || !(name.starts_with("__wbg___wbindgen_throw_")
                        || name.starts_with("__wbindgen_cast_")
                        || (!release && name == "__wbindgen_init_externref_table"))
                {
                    bail!("compute imports must be bindgen string/error functions");
                }
                wasm_leb(&mut payload)?;
            }
            if !payload.is_empty() {
                bail!("invalid compute import section");
            }
        } else if section == 5 {
            if memory.is_some() || wasm_leb(&mut payload)? != 1 {
                bail!("compute must define one private memory");
            }
            let flags = wasm_leb(&mut payload)?;
            let initial = wasm_leb(&mut payload)?;
            let maximum = wasm_leb(&mut payload)?;
            if flags != 1 || maximum != 4096 || initial > maximum || !payload.is_empty() {
                bail!("compute must retain private memory with maximum 4096 pages");
            }
            memory = Some(serde_json::json!({"flags":flags,"initial":initial,"maximum":maximum}));
        }
    }
    memory.context("compute WASM has no bounded private memory")
}

fn wasm_leb(bytes: &mut &[u8]) -> Result<u32> {
    let mut value = 0;
    for shift in (0..35).step_by(7) {
        let (&byte, remaining) = bytes
            .split_first()
            .context("truncated compute WASM integer")?;
        *bytes = remaining;
        if shift == 28 && byte > 15 {
            bail!("invalid compute WASM integer");
        }
        value |= u32::from(byte & 127) << shift;
        if byte & 128 == 0 {
            return Ok(value);
        }
    }
    bail!("invalid compute WASM integer")
}

fn wasm_name<'a>(bytes: &mut &'a [u8]) -> Result<&'a str> {
    let length = usize::try_from(wasm_leb(bytes)?)?;
    let name = std::str::from_utf8(bytes.get(..length).context("truncated compute WASM name")?)?;
    *bytes = bytes.get(length..).context("truncated compute WASM name")?;
    Ok(name)
}

fn compute_hashes(bundle: &Path) -> Result<serde_json::Value> {
    let mut outputs = serde_json::Map::new();
    for name in COMPUTE_ASSETS {
        outputs.insert(
            name.into(),
            hex_digest(&std::fs::read(bundle.join(name))?).into(),
        );
    }
    Ok(outputs.into())
}

fn compute_cache_valid(bundle: &Path, input: &serde_json::Value) -> bool {
    let stamp = std::fs::read(bundle.join(".compute-input"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
    stamp.is_some_and(|stamp| {
        stamp.get("input") == Some(input)
            && compute_hashes(bundle).is_ok_and(|outputs| stamp.get("outputs") == Some(&outputs))
    })
}

fn tool_version(program: &Path) -> Result<String> {
    let output = ProcessCommand::new(program)
        .arg("--version")
        .output()
        .with_context(|| format!("checking {}", program.display()))?;
    if !output.status.success() {
        bail!("{} --version failed", program.display());
    }
    Ok(String::from_utf8(output.stdout)?.trim().into())
}

fn compute_crate(sdk_root: &Path) -> WebCrate {
    WebCrate {
        manifest: sdk_root.join(COMPUTE).join("Cargo.toml"),
        package: "tilo-browser-compute".into(),
        target_dir: None,
        apps_json: String::new(),
        entry: String::new(),
    }
}

/// Build from the canonical SDK's manifest/lock/profile, never the generated app workspace.
fn build_compute(
    project: &Project,
    request: &TargetRequest,
    bindgen: &Path,
    bundle: &Path,
    optimizer: Option<&Optimizer>,
) -> Result<serde_json::Value> {
    let mut sdk = project.clone();
    sdk.workspace_root.clone_from(&project.sdk_root);
    let profile = web_profile(&sdk, request.release)?;
    let wasm = cargo_build_wasm(&sdk, &compute_crate(&project.sdk_root), &profile, request)?;
    let modules: serde_json::Map<String, serde_json::Value> = COMPUTE_ASSETS[..3]
        .iter()
        .map(|name| {
            Ok((
                (*name).into(),
                hex_digest(&std::fs::read(
                    project.sdk_root.join(COMPUTE).join("web").join(name),
                )?)
                .into(),
            ))
        })
        .collect::<Result<_>>()?;
    let input = serde_json::json!({"schema":1,"profile":profile,"release":request.release,
        "wasmSha256":hex_digest(&std::fs::read(&wasm)?),
        "bindgen":{"program":bindgen,"version":tool_version(bindgen)?},
        "optimizer":optimizer.map(Optimizer::input),
        "modules":modules});
    if !compute_cache_valid(bundle, &input) {
        let staging = tempfile::tempdir_in(bundle).context("staging compute assets")?;
        let output = staging.path().join("pkg/compute");
        std::fs::create_dir_all(&output)?;
        run(
            ProcessCommand::new(bindgen)
                .arg(&wasm)
                .args([
                    "--target",
                    "web",
                    "--out-name",
                    "tilo_browser_compute",
                    "--out-dir",
                ])
                .arg(&output),
            "compute wasm-bindgen",
        )?;
        let emitted = output.join("tilo_browser_compute_bg.wasm");
        if let Some(optimizer) = optimizer {
            optimizer.optimize(&emitted)?;
        }
        compute_memory(&std::fs::read(&emitted)?, request.release)?;
        // Validate all five assets before replacing the prior output or stamping success.
        for name in &COMPUTE_ASSETS[..3] {
            std::fs::copy(
                project.sdk_root.join(COMPUTE).join("web").join(name),
                staging.path().join(name),
            )?;
        }
        compute_hashes(staging.path())?;
        let destination = bundle.join("pkg/compute");
        if destination.exists() {
            std::fs::remove_dir_all(&destination)?;
        }
        std::fs::rename(&output, &destination)?;
        for name in &COMPUTE_ASSETS[..3] {
            std::fs::copy(staging.path().join(name), bundle.join(name))?;
        }
        std::fs::write(
            bundle.join(".compute-input"),
            serde_json::to_vec(
                &serde_json::json!({"input":input,"outputs":compute_hashes(bundle)?}),
            )?,
        )?;
    }
    let memory = compute_memory(
        &std::fs::read(bundle.join(COMPUTE_ASSETS[4]))?,
        request.release,
    )?;
    let mut files = serde_json::Map::new();
    for name in COMPUTE_ASSETS {
        let bytes = std::fs::read(bundle.join(name))?;
        files.insert(
            name.into(),
            serde_json::json!({"bytes":bytes.len(),"sha256":hex_digest(&bytes)}),
        );
    }
    Ok(
        serde_json::json!({"input":input,"files":files,"memory":memory,
        "releaseByteLimit": if request.release { Some(1024*1024) } else { None }}),
    )
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
        entry: app_entry(&project.manifest.package.id),
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
        entry: app_entry(&package.id),
    })
}

/// The page that shows `app` full tab in the person's own room, relative to
/// the bundle: the link grammar's `app/#/me/<app-id>`.
fn app_entry(app: &str) -> String {
    format!("app/#/me/{app}")
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
    let definition = if lib_source.contains("pub fn definition(")
        || lib_source.contains("ui::dev_reload_entry!(")
    {
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

fn cargo_wasm_command(
    project: &Project,
    web_crate: &WebCrate,
    profile: &str,
) -> Result<ProcessCommand> {
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
    Ok(command)
}

fn cargo_build_wasm(
    project: &Project,
    web_crate: &WebCrate,
    profile: &str,
    request: &TargetRequest,
) -> Result<PathBuf> {
    let mut command = cargo_wasm_command(project, web_crate, profile)?;
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

fn copy_page_host(sdk_root: &Path, bundle: &Path) -> Result<()> {
    let web = sdk_root.join(RUNTIME).join("web");
    for entry in std::fs::read_dir(&web).with_context(|| format!("reading {}", web.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if runtime_module(name) || matches!(name, "index.html" | "apple-touch-icon.png") {
            link_or_copy(&entry.path(), &bundle.join(name))?;
        }
    }
    // The full-tab page that `app/#…` links load.
    let app_page = web.join("app").join("index.html");
    if app_page.is_file() {
        let app_dir = bundle.join("app");
        std::fs::create_dir_all(&app_dir)
            .with_context(|| format!("creating {}", app_dir.display()))?;
        link_or_copy(&app_page, &app_dir.join("index.html"))?;
    }
    // The arrival screen's lines: `arrival.mjs` fetches `locales/<locale>.ftl`.
    let locales = web.join("locales");
    if locales.is_dir() {
        let into = bundle.join("locales");
        std::fs::create_dir_all(&into).with_context(|| format!("creating {}", into.display()))?;
        for entry in
            std::fs::read_dir(&locales).with_context(|| format!("reading {}", locales.display()))?
        {
            let entry = entry?;
            if entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "ftl")
            {
                link_or_copy(&entry.path(), &into.join(entry.file_name()))?;
            }
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
    let source = crate::font_assets::prepared_fonts(sdk_root, "product")?;
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
    link_or_copy(&source.join("PROVENANCE.md"), &fonts.join("PROVENANCE.md"))?;
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
/// names sorted, `pkg/`, `fonts/` and `locales/` expanded in place,
/// recursively sorted.
pub(super) fn packaged_files(bundle: &Path) -> Result<Vec<VersionFile>> {
    let mut files = Vec::new();
    for name in sorted_names(bundle)? {
        let path = bundle.join(&name);
        if name == "pkg" || name == "fonts" || name == "locales" {
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
    hex::encode(Sha256::digest(bytes))
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
        "compute-worker.mjs",
        "compute-domains.mjs",
        "compute-protocol.mjs",
        "pkg/compute/tilo_browser_compute.js",
        "pkg/compute/tilo_browser_compute_bg.wasm",
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
    let compute = std::fs::read(bundle.join("build.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|build| build.get("compute").cloned());
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
        "compute": compute,
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

    #[test]
    fn web_font_bundle_preserves_the_pinned_product_bytes_and_notices() {
        let sdk = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let bundle = tempfile::tempdir().unwrap();
        copy_fonts(&sdk, bundle.path()).unwrap();
        let descriptor: serde_json::Value =
            serde_json::from_slice(&std::fs::read(bundle.path().join("fonts.json")).unwrap())
                .unwrap();
        let assets = descriptor["assets"].as_array().unwrap();
        assert_eq!(assets.len(), 35);
        for asset in assets {
            let name = asset["name"].as_str().unwrap();
            let bytes = std::fs::read(bundle.path().join("fonts").join(name)).unwrap();
            assert_eq!(
                bytes.len() as u64,
                asset["bytes"].as_u64().unwrap(),
                "{name}"
            );
            assert_eq!(
                hex_digest(&bytes),
                asset["sha256"].as_str().unwrap(),
                "{name}"
            );
        }
        let source = sdk.join("crates/sdk/ui-core/assets/fonts");
        for name in ["manifest.toml", "PROVENANCE.md", "LICENSES/UPSTREAM.md"] {
            assert_eq!(
                std::fs::read(bundle.path().join("fonts").join(name)).unwrap(),
                std::fs::read(source.join(name)).unwrap(),
                "{name}"
            );
        }
        assert!(!bundle.path().join("fonts/NewCM10-Regular.otf").exists());
        assert!(!bundle.path().join("fonts/AdwaitaSans-Regular.ttf").exists());
    }

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
            "locales/en-US.ftl",
            "untrusted.html",
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
                "locales/en-US.ftl",
                "pkg/tilo_browser_runtime.js"
            ]
        );
    }

    #[test]
    fn a_packaged_bundle_carries_the_arrival_screens_locales() {
        let sdk = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let bundle = tempfile::tempdir().unwrap();
        copy_page_host(&sdk, bundle.path()).unwrap();
        let packaged: Vec<String> = packaged_files(bundle.path())
            .unwrap()
            .into_iter()
            .map(|file| file.name)
            .collect();
        assert!(packaged.iter().any(|name| name == "arrival.mjs"));
        // Every locale `arrivalLocale` picks, and each one shipped beside it.
        let shipped = std::fs::read_dir(sdk.join(RUNTIME).join("web/locales"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap());
        let mut wanted: Vec<String> = shipped.map(|name| format!("locales/{name}")).collect();
        wanted.sort();
        assert!(wanted.iter().any(|name| name == "locales/en-US.ftl"));
        assert!(wanted.iter().any(|name| name == "locales/de-DE.ftl"));
        for name in &wanted {
            assert!(
                packaged.contains(name),
                "{name} is not packaged: {packaged:?}"
            );
        }
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
        let mut names: Vec<String> = zip
            .file_names()
            .map(|name| name.unwrap().into_owned())
            .collect();
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
    fn compute_final_artifact_requires_private_bounded_memory() {
        let valid = [0, 97, 115, 109, 1, 0, 0, 0, 5, 5, 1, 1, 18, 128, 32];
        assert_eq!(
            compute_memory(&valid, true).unwrap(),
            serde_json::json!({"flags":1,"initial":18,"maximum":4096})
        );
        for bytes in [
            &valid[..8],
            &[0, 97, 115, 109, 1, 0, 0, 0, 5, 5, 1, 3, 18, 128, 32],
            &[0, 97, 115, 109, 1, 0, 0, 0, 5, 5, 1, 1, 18, 129, 32],
            &[0, 97, 115, 109, 1, 0, 0, 0, 5, 5, 1, 1, 18],
        ] {
            assert!(compute_memory(bytes, true).is_err());
        }
        assert!(compute_memory(&vec![0; 1024 * 1024 + 1], true).is_err());
    }

    #[test]
    fn compute_uses_sdk_manifest_and_profile_outside_generated_app_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let mut project = packaging_project(dir.path());
        project.sdk_root = dir.path().join("sdk");
        std::fs::create_dir_all(&project.sdk_root).unwrap();
        std::fs::write(project.workspace_root.join("Cargo.toml"), "[workspace]\n").unwrap();
        std::fs::write(
            project.sdk_root.join("Cargo.toml"),
            "[workspace]\n[profile.wasm-release]\ninherits='release'\n",
        )
        .unwrap();
        let compute = compute_crate(&project.sdk_root);
        assert_eq!(
            compute.manifest,
            project.sdk_root.join(COMPUTE).join("Cargo.toml")
        );
        let command = cargo_wasm_command(&project, &compute, "wasm-release").unwrap();
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(command.get_current_dir(), Some(project.sdk_root.as_path()));
        assert!(args.iter().any(|arg| arg == "--locked"));
        assert!(!args.iter().any(|arg| arg == "--target-dir"));
        assert_eq!(args[2], compute.manifest.to_string_lossy());
        assert!(
            compute.target_dir.is_none(),
            "canonical SDK default target and locked build"
        );
        assert_eq!(web_profile(&project, true).unwrap(), "release");
        project.workspace_root.clone_from(&project.sdk_root);
        assert_eq!(web_profile(&project, true).unwrap(), "wasm-release");
        assert_eq!(web_profile(&project, false).unwrap(), "dev");
        // A large custom section followed by valid memory: dev has no release byte gate.
        let mut dev = valid_header().to_vec();
        dev.extend([0, 0x80, 0x80, 0x40]);
        dev.extend(vec![0; 1024 * 1024]);
        dev.extend([5, 5, 1, 1, 18, 128, 32]);
        assert!(compute_memory(&dev, false).is_ok());
        assert!(compute_memory(&dev, true).is_err());
    }

    fn valid_header() -> &'static [u8] {
        b"\0asm\x01\0\0\0"
    }

    #[test]
    fn dev_allows_only_observed_bindgen_externref_initialization() {
        let mut imports = vec![1];
        for name in [
            "./tilo_browser_compute_bg.js",
            "__wbindgen_init_externref_table",
        ] {
            imports.push(u8::try_from(name.len()).unwrap());
            imports.extend(name.as_bytes());
        }
        imports.extend([0, 0]);
        let mut wasm = valid_header().to_vec();
        wasm.extend([1, 4, 1, 96, 0, 0, 2, u8::try_from(imports.len()).unwrap()]);
        wasm.extend(imports);
        wasm.extend([5, 5, 1, 1, 18, 128, 32]);
        assert!(compute_memory(&wasm, false).is_ok());
        assert!(compute_memory(&wasm, true).is_err());
    }

    #[test]
    fn compute_inspector_rejects_malformed_sections_and_invalid_function_bodies() {
        let memory = [5, 5, 1, 1, 18, 128, 32];
        let types = [1, 4, 1, 96, 0, 0];
        let functions = [3, 2, 1, 0];
        let mut examples = Vec::new();
        for extra in [&[255, 0][..], &[1, 1, 1][..], &types[..]] {
            let mut bytes = valid_header().to_vec();
            bytes.extend(memory);
            bytes.extend(extra);
            examples.push(bytes);
        }
        for code in [
            &[10, 5, 1, 3, 0, 255, 11][..],
            &[10, 6, 1, 4, 0, 65, 0, 11][..],
            &[][..],
        ] {
            let mut bytes = valid_header().to_vec();
            bytes.extend(types);
            bytes.extend(functions);
            bytes.extend(memory);
            bytes.extend(code);
            examples.push(bytes);
        }
        let accepted: Vec<_> = examples
            .iter()
            .enumerate()
            .filter_map(|(index, bytes)| compute_memory(bytes, false).is_ok().then_some(index))
            .collect();
        assert!(
            accepted.is_empty(),
            "accepted malformed modules {accepted:?}"
        );
    }

    #[cfg(unix)]
    fn executable(path: &Path, script: &str) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::write(path, script).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn runtime_bundle_invalidates_optimizer_changes_and_preserves_no_op_builds() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let wasm = root.join("input.wasm");
        std::fs::write(&wasm, b"unoptimized").unwrap();
        let bundle = root.join("bundle");
        std::fs::create_dir_all(bundle.join("pkg")).unwrap();
        let bindgen = root.join("bindgen");
        executable(
            &bindgen,
            r#"#!/bin/sh
set -eu
input=$1
for output; do :; done
cp "$input" "$output/tilo_browser_runtime_bg.wasm"
printf 'bindgen\n' >> "$output/../calls"
"#,
        );
        let program = root.join("optimizer");
        let version = root.join("version");
        std::fs::write(&version, "optimizer 1").unwrap();
        let script = r#"#!/bin/sh
set -eu
if [ "$1" = --version ]; then
    cat "$(dirname "$0")/version"
    exit 0
fi
for output; do :; done
printf 'optimized %s\n' "$*" >> "$output"
printf 'optimizer\n' >> "$(dirname "$output")/../calls"
"#;
        let calls = bundle.join("calls");
        let output = bundle.join("pkg/tilo_browser_runtime_bg.wasm");
        let stamp = bundle.join(".wasm-input");
        // A release built before the optimizer is installed must be revisited.
        assert!(Optimizer::probe(program.clone(), true).is_none());
        bundle_runtime(&wasm, true, &bindgen, &bundle, None, None).unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), b"unoptimized");
        executable(&program, script);
        let mut optimizer = Optimizer::probe(program.clone(), true).unwrap();
        assert!(Optimizer::probe(program.clone(), false).is_none());
        bundle_runtime(&wasm, true, &bindgen, &bundle, Some(&optimizer), None).unwrap();
        assert_eq!(
            std::fs::read_to_string(&calls).unwrap(),
            "bindgen\nbindgen\noptimizer\n"
        );
        assert!(
            std::fs::read_to_string(&output)
                .unwrap()
                .starts_with("unoptimizedoptimized -Oz")
        );
        let unchanged_stamp = std::fs::metadata(&stamp).unwrap().modified().unwrap();
        let unchanged_output = std::fs::metadata(&output).unwrap().modified().unwrap();
        bundle_runtime(&wasm, true, &bindgen, &bundle, Some(&optimizer), None).unwrap();
        assert_eq!(
            std::fs::read_to_string(&calls).unwrap(),
            "bindgen\nbindgen\noptimizer\n"
        );
        assert_eq!(
            std::fs::metadata(&stamp).unwrap().modified().unwrap(),
            unchanged_stamp
        );
        assert_eq!(
            std::fs::metadata(&output).unwrap().modified().unwrap(),
            unchanged_output
        );

        std::fs::write(&version, "optimizer 2").unwrap();
        optimizer = Optimizer::probe(program.clone(), true).unwrap();
        bundle_runtime(&wasm, true, &bindgen, &bundle, Some(&optimizer), None).unwrap();
        let other_program = root.join("other-optimizer");
        executable(&other_program, script);
        optimizer = Optimizer::probe(other_program, true).unwrap();
        bundle_runtime(&wasm, true, &bindgen, &bundle, Some(&optimizer), None).unwrap();
        optimizer.flags = &["-O2"];
        bundle_runtime(&wasm, true, &bindgen, &bundle, Some(&optimizer), None).unwrap();
        assert!(
            std::fs::read_to_string(&output)
                .unwrap()
                .contains("optimized -O2")
        );
        assert_eq!(
            std::fs::read_to_string(&calls).unwrap(),
            "bindgen\nbindgen\noptimizer\nbindgen\noptimizer\nbindgen\noptimizer\nbindgen\noptimizer\n"
        );
        // Removing the optimizer restores bindgen output, not stale optimized bytes.
        std::fs::remove_file(program).unwrap();
        bundle_runtime(&wasm, true, &bindgen, &bundle, None, None).unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), b"unoptimized");
        let absent_calls = std::fs::read(&calls).unwrap();
        bundle_runtime(&wasm, true, &bindgen, &bundle, None, None).unwrap();
        assert_eq!(std::fs::read(&calls).unwrap(), absent_calls);
        // A missing output must rebuild even with a matching input stamp.
        std::fs::remove_file(&output).unwrap();
        bundle_runtime(&wasm, true, &bindgen, &bundle, None, None).unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), b"unoptimized");
        assert_ne!(std::fs::read(&calls).unwrap(), absent_calls);
    }

    #[test]
    fn compute_cache_refuses_partial_corrupt_or_changed_input() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path();
        worker_bundle(bundle);
        let input = serde_json::json!({"profile":"dev","bindgen":"0.2.108","optimizer":null});
        let mut outputs = serde_json::Map::new();
        for name in [
            "compute-worker.mjs",
            "compute-domains.mjs",
            "compute-protocol.mjs",
            "pkg/compute/tilo_browser_compute.js",
            "pkg/compute/tilo_browser_compute_bg.wasm",
        ] {
            outputs.insert(
                name.into(),
                hex_digest(&std::fs::read(bundle.join(name)).unwrap()).into(),
            );
        }
        let stamp = serde_json::json!({"input":input,"outputs":outputs});
        std::fs::write(
            bundle.join(".compute-input"),
            serde_json::to_vec(&stamp).unwrap(),
        )
        .unwrap();
        assert!(compute_cache_valid(bundle, &input));
        assert!(!compute_cache_valid(
            bundle,
            &serde_json::json!({"profile":"release"})
        ));
        for name in [
            "compute-worker.mjs",
            "compute-domains.mjs",
            "compute-protocol.mjs",
            "pkg/compute/tilo_browser_compute.js",
            "pkg/compute/tilo_browser_compute_bg.wasm",
        ] {
            let path = bundle.join(name);
            let bytes = std::fs::read(&path).unwrap();
            std::fs::write(&path, b"changed").unwrap();
            assert!(!compute_cache_valid(bundle, &input), "corrupted {name}");
            std::fs::remove_file(&path).unwrap();
            assert!(!compute_cache_valid(bundle, &input), "missing {name}");
            std::fs::write(&path, bytes).unwrap();
        }
    }

    fn packaging_project(root: &Path) -> Project {
        let manifest_path = root.join("tilo-app.toml");
        let source = include_str!("../../../../../apps/calculator/tilo-app.toml");
        std::fs::write(&manifest_path, source).unwrap();
        Project {
            app_dir: root.into(),
            cargo_manifest: root.join("Cargo.toml"),
            package: "calculator".into(),
            lib_name: "calculator".into(),
            bin: "calculator".into(),
            manifest_path,
            manifest: tilo_app_package::Manifest::from_toml_str(source).unwrap(),
            workspace_root: root.into(),
            target_dir: root.join("target"),
            sdk_root: root.into(),
        }
    }

    #[test]
    fn web_registers_the_reload_macros_existing_definition() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let project = packaging_project(root);
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "ui::dev_reload_entry!(ChatShell);\n",
        )
        .unwrap();
        let entry = app_crate_source(&project).unwrap();
        assert!(entry.contains("registry::register(app::definition())"));
        assert!(!entry.contains("definition::<app::App>()"));
    }

    fn packaging_request(root: &Path) -> TargetPackageRequest {
        TargetPackageRequest {
            build: TargetRequest::new(Target::Web),
            output: Some(root.join("dist")),
            identity: None,
            force: false,
        }
    }

    fn worker_bundle(root: &Path) {
        for name in [
            "pkg/tilo_browser_runtime.js",
            "pkg/tilo_browser_runtime_bg.wasm",
            "host.mjs",
            "fonts.json",
            "apps.json",
            "compute-worker.mjs",
            "compute-domains.mjs",
            "compute-protocol.mjs",
            "pkg/compute/tilo_browser_compute.js",
            "pkg/compute/tilo_browser_compute_bg.wasm",
        ] {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, name.as_bytes()).unwrap();
        }
    }

    #[test]
    fn packaging_refuses_every_missing_compute_asset_before_publication() {
        for missing in [
            "compute-worker.mjs",
            "compute-domains.mjs",
            "compute-protocol.mjs",
            "pkg/compute/tilo_browser_compute.js",
            "pkg/compute/tilo_browser_compute_bg.wasm",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let project = packaging_project(dir.path());
            let bundle = dir.path().join("bundle");
            worker_bundle(&bundle);
            std::fs::remove_file(bundle.join(missing)).unwrap();
            let result = package(&project, &bundle, &packaging_request(dir.path()));
            assert!(result.is_err(), "shipped without {missing}");
            assert!(!dir.path().join("dist").exists(), "no partial publication");
        }
    }

    #[test]
    fn compute_assets_are_hash_bound_immutable_and_in_registry_archive() {
        let dir = tempfile::tempdir().unwrap();
        let project = packaging_project(dir.path());
        let bundle = dir.path().join("bundle");
        worker_bundle(&bundle);
        let request = packaging_request(dir.path());
        let output = package(&project, &bundle, &request).unwrap();
        let files = packaged_files(&bundle).unwrap();
        let digest = hex_digest(files_json(&files).unwrap().as_bytes());
        let version = output.join("versions").join(&digest);
        let assets: serde_json::Value =
            serde_json::from_slice(&std::fs::read(version.join("assets.json")).unwrap()).unwrap();
        assert_eq!(assets["digest"], digest);
        let archive = registry_zip_path(&output, &project.manifest.package.version);
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&archive).unwrap()).unwrap();
        for file in files.iter().filter(|file| file.name.contains("compute")) {
            assert_eq!(assets["files"][&file.name]["sha256"], file.sha256);
            assert_eq!(assets["files"][&file.name]["size"], file.size);
            let mut entry = zip
                .by_name(&format!("versions/{digest}/{}", file.name))
                .unwrap();
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut bytes).unwrap();
            assert_eq!(hex_digest(&bytes), file.sha256);
        }
        drop(zip);
        let original = std::fs::read(&archive).unwrap();
        package(&project, &bundle, &request).unwrap();
        assert_eq!(std::fs::read(&archive).unwrap(), original);
        std::fs::write(version.join("compute-worker.mjs"), b"corrupt").unwrap();
        assert!(package(&project, &bundle, &request).is_err());
        assert_eq!(
            std::fs::read(version.join("compute-worker.mjs")).unwrap(),
            b"corrupt"
        );
        std::fs::write(bundle.join("compute-worker.mjs"), b"changed").unwrap();
        assert_ne!(
            hex_digest(
                files_json(&packaged_files(&bundle).unwrap())
                    .unwrap()
                    .as_bytes()
            ),
            digest
        );
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
