//! The web build of an app: `tilo-app build --platform web`, `package
//! --platform web` and `dev --platform web`.
//!
//! The browser runs a toolkit core (a WebAssembly main module) and links each
//! app into it as a small side module. The SDK checkout the app builds against
//! carries the build (`crates/browser/tilo-browser-runtime/scripts/
//! build-web-standalone.sh`); this module runs it, reads what it wrote, signs
//! the module as a `web` package, and serves the page while it rebuilds.

use std::ffi::OsString;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use notify::{Event, RecommendedWatcher};

use crate::{
    DEV_STOP_REQUESTED, DevRequest, DevSignalGuard, DevWatcher, PackageOutcome, PackageRequest,
    WatchTick, changed_sources, dev_watch_roots, doctor_resolved_app, drain_until_quiet,
    load_app_package_spec, local_ui_dependency, resolve_app_dir, sdk_root_from_metadata,
    snapshot_restart_sources, watch_tick,
};

/// The build script, relative to the SDK checkout.
const BUILD_SCRIPT: &str = "crates/browser/tilo-browser-runtime/scripts/build-web-standalone.sh";
/// A command prefix the build runs under, split on whitespace: for example
/// `rcargo --fetch web exec --`, which runs it on a remote builder and brings
/// `target/web` back.
pub const RUNNER_ENV: &str = "TILO_WEB_BUILD_RUNNER";
/// Where the page polls for the build it should be showing.
const DEV_BUILD_PATH: &str = "/__tilo-dev/build";

/// What to build.
#[derive(Debug, Default)]
pub struct WebBuildRequest {
    /// The page directory; defaults to `target/web` in the app.
    pub output: Option<PathBuf>,
    /// Relink the app against the core already built, without rebuilding it.
    pub app_only: bool,
}

/// A finished web build: the page directory and what is in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebBuild {
    pub app_dir: PathBuf,
    pub output: PathBuf,
    pub app_id: String,
    /// The toolkit core's runtime ABI id, which the app module carries too.
    pub abi: String,
    pub core_bytes: u64,
    /// The app's side module.
    pub module: PathBuf,
    pub module_bytes: u64,
    pub module_sha256: String,
    pub elapsed: Duration,
}

impl WebBuild {
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "app_dir": self.app_dir,
            "output": self.output,
            "app_id": self.app_id,
            "runtime_abi": self.abi,
            "core_bytes": self.core_bytes,
            "module": self.module,
            "module_bytes": self.module_bytes,
            "module_sha256": self.module_sha256,
            "elapsed_ms": u64::try_from(self.elapsed.as_millis()).unwrap_or(u64::MAX),
        })
    }
}

/// Build the app at `start` for the web: the toolkit core, the app as a side
/// module of it, and a page that mounts the app, in one directory a static
/// server can serve.
///
/// # Errors
///
/// When the app's SDK has no web build, the build fails, or it leaves no app
/// module behind.
pub fn build_web(start: &Path, request: &WebBuildRequest) -> Result<WebBuild> {
    let app_dir = resolve_app_dir(start)?;
    let script = web_build_script(&app_dir)?;
    let output = request.output.as_ref().map_or_else(
        || app_dir.join("target/web"),
        |path| crate::absolute_from(&app_dir, path),
    );
    let work = app_dir.join("target/web-build");
    let started = Instant::now();
    let mut command = runner_command(std::env::var(RUNNER_ENV).ok().as_deref());
    command
        .arg("bash")
        .arg(&script)
        .arg(&output)
        .arg(&work)
        .arg(&app_dir);
    if request.app_only {
        command.arg("--app-only");
    }
    eprintln!(
        "[tilo-app] building {} for the web{}",
        app_dir.display(),
        if request.app_only {
            " (the app only, against the core already built)"
        } else {
            ""
        }
    );
    let status = command
        .current_dir(&app_dir)
        .stdin(Stdio::null())
        .status()
        .with_context(|| format!("starting {}", script.display()))?;
    if !status.success() {
        bail!("the web build failed with {status}");
    }
    read_web_build(&app_dir, &output, started.elapsed())
}

/// The command a web build runs under: `runner`'s words, else plain `env`.
fn runner_command(runner: Option<&str>) -> ProcessCommand {
    let mut words = runner.unwrap_or_default().split_whitespace();
    match words.next() {
        Some(program) => {
            let mut command = ProcessCommand::new(program);
            command.args(words);
            command
        }
        None => ProcessCommand::new("env"),
    }
}

/// The SDK checkout's web build script: from the app's path dependency on the
/// SDK, else from `cargo metadata` (a git SDK's checkout).
fn web_build_script(app_dir: &Path) -> Result<PathBuf> {
    let root = match local_ui_dependency(app_dir)? {
        Some(sdk) => sdk
            .ancestors()
            .find(|root| root.join(BUILD_SCRIPT).is_file())
            .map(Path::to_path_buf),
        None => sdk_root_via_metadata(app_dir)?,
    };
    let script = root.map(|root| root.join(BUILD_SCRIPT));
    match script {
        Some(script) if script.is_file() => Ok(script),
        _ => bail!(
            "the Tilo SDK {} builds against has no web build ({BUILD_SCRIPT}); update the SDK",
            app_dir.display()
        ),
    }
}

fn sdk_root_via_metadata(app_dir: &Path) -> Result<Option<PathBuf>> {
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
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("parsing cargo metadata")?;
    Ok(sdk_root_from_metadata(&metadata))
}

/// What the build wrote to `output`: `runtime.json`, `apps/index.json` naming
/// the app, and the app's `app.json` and module.
fn read_web_build(app_dir: &Path, output: &Path, elapsed: Duration) -> Result<WebBuild> {
    let json = |relative: &str| -> Result<serde_json::Value> {
        let path = output.join(relative);
        let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))
    };
    let field = |value: &serde_json::Value, pointer: &str, file: &str| -> Result<String> {
        value
            .pointer(pointer)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("{file} has no {pointer}"))
    };
    let runtime = json("runtime.json")?;
    let index = json("apps/index.json")?;
    let apps = index
        .get("apps")
        .and_then(serde_json::Value::as_object)
        .context("apps/index.json lists no apps")?;
    let [(app_id, manifest)] = apps.iter().collect::<Vec<_>>()[..] else {
        bail!(
            "apps/index.json lists {} apps; the build makes one",
            apps.len()
        );
    };
    let manifest = manifest
        .as_str()
        .context("apps/index.json names no app.json")?;
    let app = json(manifest)?;
    let module_dir = Path::new(manifest)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let module = output
        .join(module_dir)
        .join(field(&app, "/module/path", manifest)?);
    let abi = field(&runtime, "/abi", "runtime.json")?;
    let app_abi = field(&app, "/abi", manifest)?;
    if app_abi != abi {
        bail!(
            "{manifest} is built for {app_abi}, the core in {} is {abi}",
            output.display()
        );
    }
    Ok(WebBuild {
        app_dir: app_dir.to_path_buf(),
        output: output.to_path_buf(),
        app_id: app_id.clone(),
        abi,
        core_bytes: runtime
            .pointer("/core/bytes")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default(),
        module_bytes: std::fs::metadata(&module)
            .with_context(|| format!("reading {}", module.display()))?
            .len(),
        module,
        module_sha256: field(&app, "/module/sha256", manifest)?,
        elapsed,
    })
}

/// `tilo-app package --platform web`: build the side module and sign it as a
/// `web` package for the core's runtime, `dist/<package>-web-wasm32.tapp`.
pub(crate) fn package_web(start: &Path, request: &PackageRequest) -> Result<PackageOutcome> {
    let app_dir = resolve_app_dir(start)?;
    let report = doctor_resolved_app(app_dir.clone());
    if !report.is_ready() {
        bail!("app preflight failed\n\n{}", report.summary());
    }
    let spec = load_app_package_spec(app_dir)?;
    let output = request.output.as_ref().map_or_else(
        || {
            spec.app_dir.join("dist").join(format!(
                "{}-{}-{}.tapp",
                spec.cargo_package,
                registry_types::WEB_PLATFORM,
                registry_types::WASM32_ARCHITECTURE
            ))
        },
        |path| crate::absolute_from(&spec.app_dir, path),
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
    std::fs::create_dir_all(output_parent)
        .with_context(|| format!("creating {}", output_parent.display()))?;
    let build = build_web(&spec.app_dir, &WebBuildRequest::default())?;

    let manifest_source = std::fs::read_to_string(&spec.manifest_path)
        .with_context(|| format!("reading {}", spec.manifest_path.display()))?;
    let authored = tilo_app_package::Manifest::from_toml_str(&manifest_source)
        .with_context(|| format!("parsing {}", spec.manifest_path.display()))?;
    let budget = crate::size::Budget::from_manifest(&manifest_source)
        .with_context(|| format!("reading [budget] from {}", spec.manifest_path.display()))?;
    let module_bytes = std::fs::read(&build.module)
        .with_context(|| format!("reading {}", build.module.display()))?;
    let module_name = "app.wasm";
    let info = tilo_app_package::web::inspect_wasm_module(&module_bytes)
        .with_context(|| format!("inspecting {}", build.module.display()))?;
    let manifest = tilo_app_package::web::web_manifest(
        &authored,
        module_name,
        &info,
        &tilo_app_package::web::WebBuildOptions::default(),
    )
    .with_context(|| spec.manifest_path.display().to_string())?;

    let staging = tempfile::Builder::new()
        .prefix(".tilo-app-package-")
        .tempdir_in(output_parent)
        .with_context(|| format!("creating a staging area in {}", output_parent.display()))?;
    let payload = staging.path().join("payload");
    std::fs::create_dir(&payload).context("creating staged payload")?;
    std::fs::write(payload.join(module_name), &module_bytes).context("staging the module")?;
    let web_assets = spec.app_dir.join("assets/web");
    if web_assets.is_dir() {
        let assets = payload.join("assets");
        std::fs::create_dir(&assets).context("creating staged assets")?;
        crate::copy_payload_tree(&web_assets, &assets)?;
    }
    let staged = staging.path().join("package.tapp");
    let identity = crate::signing_identity(&spec.app_dir, request.identity.as_deref())?;
    let resource_kinds = crate::resource_kinds::declared_in(&spec.app_dir)?;
    let packed =
        crate::pack::pack_manifest(manifest, resource_kinds, &payload, &staged, &identity)?;
    let entrypoints = vec![PathBuf::from(module_name)];
    let size = crate::within_budget(&payload, &entrypoints, packed.bytes, budget, &output)?;
    std::fs::rename(&staged, &output)
        .with_context(|| format!("publishing package to {}", output.display()))?;
    eprintln!(
        "[tilo-app] packaged {} ({}, {} bytes, for {})",
        output.display(),
        packed.id,
        packed.bytes,
        build.abi
    );
    Ok(PackageOutcome {
        output,
        entrypoints,
        size,
    })
}

/// How `dev --platform web` serves the page.
#[derive(Clone, Debug)]
pub struct WebDevOptions {
    /// The address the page is served on.
    pub bind: String,
}

/// `tilo-app dev --platform web`: build, serve the page, and rebuild on every
/// change. An edit to the app relinks only its side module; the page notices
/// the new build and reloads it into the same record, whose state lives in the
/// browser's storage, not in the module. An edit to the SDK rebuilds the core.
///
/// # Errors
///
/// When the first build fails, the address cannot be bound, or watching
/// cannot start. A later failed build keeps the last good page served.
pub fn dev_web(start: &Path, request: &DevRequest, options: &WebDevOptions) -> Result<()> {
    let app_dir = resolve_app_dir(start)?;
    let roots = dev_watch_roots(&app_dir, request.watch_ui)?;
    let (event_tx, event_rx) = mpsc::channel();
    let watcher: RecommendedWatcher =
        notify::recommended_watcher(move |event: notify::Result<Event>| {
            let _ = event_tx.send(event);
        })
        .context("creating app file watcher")?;
    let mut watcher = DevWatcher::new(watcher);
    for root in &roots {
        watcher.watch_tree(root)?;
    }
    let _signals = DevSignalGuard::install()?;

    let first = build_web(&app_dir, &WebBuildRequest::default())?;
    report_build(&first, "built");
    let generation = Arc::new(AtomicU64::new(1));
    let listener =
        TcpListener::bind(&options.bind).with_context(|| format!("binding {}", options.bind))?;
    let address = listener.local_addr()?;
    let served = first.output.clone();
    let server_generation = Arc::clone(&generation);
    // A page left open across two `dev` sessions still sees a new build.
    let session = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    std::thread::spawn(move || serve(&listener, &served, session, &server_generation));
    eprintln!(
        "[tilo-app] serving {} at http://{address}/ (open it in a browser with WebGPU)",
        first.app_id
    );
    eprintln!(
        "[tilo-app] watching {}",
        roots
            .iter()
            .map(|root| root.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );

    let mut sources = snapshot_restart_sources(&roots).unwrap_or_default();
    let mut next_source_poll = Instant::now();
    while !DEV_STOP_REQUESTED.load(Ordering::Relaxed) {
        match watch_tick(
            &event_rx,
            &roots,
            &mut watcher,
            &mut sources,
            &mut next_source_poll,
        ) {
            WatchTick::Stop => bail!("app file watcher stopped"),
            WatchTick::Idle => {}
            WatchTick::Rebuild { .. } => {
                drain_until_quiet(&event_rx, &roots, request.debounce, &mut watcher);
                let next = snapshot_restart_sources(&roots).unwrap_or_default();
                let app_only = changed_sources(&sources, &next)
                    .iter()
                    .all(|path| path.starts_with(&app_dir));
                sources = next;
                eprintln!("[tilo-app] change detected; rebuilding");
                let request = WebBuildRequest {
                    output: Some(first.output.clone()),
                    app_only,
                };
                match build_web(&app_dir, &request) {
                    Ok(build) => {
                        generation.fetch_add(1, Ordering::Release);
                        report_build(&build, "rebuilt; the page reloads it");
                    }
                    Err(error) => eprintln!(
                        "[tilo-app] build failed; the page keeps the last good build\n{error:#}"
                    ),
                }
            }
        }
    }
    eprintln!("[tilo-app] stopped");
    Ok(())
}

fn report_build(build: &WebBuild, what: &str) {
    eprintln!(
        "[tilo-app] {} {what} in {:.1} s: module {} bytes, core {} bytes, runtime {}",
        build.app_id,
        build.elapsed.as_secs_f64(),
        build.module_bytes,
        build.core_bytes,
        build.abi
    );
}

/// Serve `root` until the process ends: GET and HEAD of its files, and the
/// current build generation at [`DEV_BUILD_PATH`] for the page to poll.
fn serve(listener: &TcpListener, root: &Path, session: u128, generation: &Arc<AtomicU64>) {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    for stream in listener.incoming().flatten() {
        let root = root.clone();
        let generation = format!("{session}.{}", generation.load(Ordering::Acquire));
        std::thread::spawn(move || {
            let _ = respond(stream, &root, &generation);
        });
    }
}

fn respond(mut stream: TcpStream, root: &Path, generation: &str) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 || header.trim_end().is_empty() {
            break;
        }
    }
    let mut words = request_line.split_whitespace();
    let (method, target) = (words.next().unwrap_or(""), words.next().unwrap_or("/"));
    let head = method == "HEAD";
    if method != "GET" && !head {
        return reply(
            &mut stream,
            "405 Method Not Allowed",
            "text/plain",
            b"",
            head,
        );
    }
    let path = target.split(['?', '#']).next().unwrap_or("/");
    if path == DEV_BUILD_PATH {
        let body = format!("{{\"generation\":\"{generation}\"}}");
        return reply(
            &mut stream,
            "200 OK",
            "application/json",
            body.as_bytes(),
            head,
        );
    }
    match resolve_served(root, path).and_then(|file| Some((std::fs::read(&file).ok()?, file))) {
        Some((body, file)) => reply(&mut stream, "200 OK", content_type(&file), &body, head),
        None => reply(
            &mut stream,
            "404 Not Found",
            "text/plain",
            b"not found",
            head,
        ),
    }
}

fn reply(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
    head: bool,
) -> std::io::Result<()> {
    // Every response is revalidated: a rebuilt module must never come from cache.
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Cache-Control: no-cache\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    if !head {
        stream.write_all(body)?;
    }
    stream.flush()
}

/// The file under `root` a request path names, never one outside it.
fn resolve_served(root: &Path, path: &str) -> Option<PathBuf> {
    let decoded = percent_decode(path)?;
    let relative = decoded.trim_start_matches('/');
    if relative
        .split('/')
        .any(|segment| segment == ".." || segment.contains('\\'))
    {
        return None;
    }
    let mut file = root.join(relative);
    if file.is_dir() {
        file = file.join("index.html");
    }
    let file = file.canonicalize().ok()?;
    (file.starts_with(root) && file.is_file()).then_some(file)
}

fn percent_decode(path: &str) -> Option<String> {
    let mut out = Vec::with_capacity(path.len());
    let mut bytes = path.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let hex = [bytes.next()?, bytes.next()?];
            out.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
        } else {
            out.push(byte);
        }
    }
    String::from_utf8(out).ok()
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("json") => "application/json",
        Some("wasm") => "application/wasm",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("woff2") => "font/woff2",
        Some("txt" | "md" | "ts") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_runner_prefixes_the_build_command() {
        let command = runner_command(Some("rcargo --fetch web exec --"));
        assert_eq!(command.get_program(), "rcargo");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["--fetch", "web", "exec", "--"]
        );
        assert_eq!(runner_command(None).get_program(), "env");
        assert_eq!(runner_command(Some("  ")).get_program(), "env");
    }

    #[test]
    fn served_paths_stay_inside_the_page() {
        let page = tempfile::tempdir().unwrap();
        let root = page.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("apps/a")).unwrap();
        std::fs::write(root.join("index.html"), "page").unwrap();
        std::fs::write(root.join("apps/a/app.wasm"), "wasm").unwrap();
        assert_eq!(resolve_served(&root, "/"), Some(root.join("index.html")));
        assert_eq!(
            resolve_served(&root, "/apps/a/app%2ewasm"),
            Some(root.join("apps/a/app.wasm"))
        );
        assert_eq!(resolve_served(&root, "/../etc/passwd"), None);
        assert_eq!(resolve_served(&root, "/apps/%2e%2e/%2e%2e/x"), None);
        assert_eq!(resolve_served(&root, "/missing.js"), None);
        assert_eq!(resolve_served(&root, "/index.html%2"), None);
        assert_eq!(content_type(Path::new("a/app.wasm")), "application/wasm");
        assert_eq!(
            content_type(Path::new("host.mjs")),
            "text/javascript; charset=utf-8"
        );
    }

    #[test]
    fn a_build_is_read_from_the_page_it_wrote() {
        let page = tempfile::tempdir().unwrap();
        let out = page.path();
        let abi = "tilo-runtime/2026.07+wasm.0123456789abcdef0123456789abcdef";
        std::fs::create_dir_all(out.join("apps/local.dev.demo")).unwrap();
        std::fs::write(
            out.join("runtime.json"),
            serde_json::json!({"abi": abi, "core": {"bytes": 42}}).to_string(),
        )
        .unwrap();
        std::fs::write(
            out.join("apps/index.json"),
            r#"{"schema":1,"apps":{"local.dev.demo":"apps/local.dev.demo/app.json"}}"#,
        )
        .unwrap();
        std::fs::write(out.join("apps/local.dev.demo/app.wasm"), [0u8; 7]).unwrap();
        let app = |abi: &str| {
            serde_json::json!({"abi": abi, "module": {"path": "app.wasm", "sha256": "ab"}})
                .to_string()
        };
        std::fs::write(out.join("apps/local.dev.demo/app.json"), app(abi)).unwrap();
        let build = read_web_build(Path::new("/app"), out, Duration::from_secs(1)).unwrap();
        assert_eq!(build.app_id, "local.dev.demo");
        assert_eq!(build.module, out.join("apps/local.dev.demo/app.wasm"));
        assert_eq!((build.module_bytes, build.core_bytes), (7, 42));
        assert_eq!(build.module_sha256, "ab");

        let stale = "tilo-runtime/2026.07+wasm.ffffffffffffffffffffffffffffffff";
        std::fs::write(out.join("apps/local.dev.demo/app.json"), app(stale)).unwrap();
        let error = read_web_build(Path::new("/app"), out, Duration::ZERO).unwrap_err();
        assert!(error.to_string().contains("is built for"), "{error:#}");
    }
}
