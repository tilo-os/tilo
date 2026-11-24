//! This computer's daemon, in the test's own process: the native endpoint a program of the
//! person's finds (`$XDG_RUNTIME_DIR/tilo/native.sock`), answering the MCP tools `app` calls it
//! with, and making the package changes they ask for the way the device's daemon does — through
//! `package_actions::apply` against this device's store. So a test runs `app install <file>` as
//! production runs it: the CLI over the socket to the daemon, never the installer in-process.
//!
//! What is not emulated: the workspace's tree. An install asks for `workspace/apps#install`, and
//! this device follows it at once instead of waiting for a workspace service to record it, which
//! is what the follower would do once its workspace named the version.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use package_actions::{Change, Entry, Replacement, SavedData, Target, Trust, Uploaded};
use serde_json::{Value, json};
use tilo_app_package::trust::{AttestorScope, RevokedKey, TrustedAttestor};
use tilo_test_env::ScopedEnv;

/// This computer's daemon, listening on the native endpoint `env` names to the programs the test
/// starts.
pub(crate) struct Daemon {
    _runtime: tempfile::TempDir,
    _content: tempfile::TempDir,
    /// The runtime directory the test's programs find the daemon in, put back when it drops.
    _env: ScopedEnv,
    stop: Arc<AtomicBool>,
    accepting: Option<std::thread::JoinHandle<()>>,
}

/// What the daemon keeps between calls: the store it changes, the content callers put first, and
/// the puts in flight.
struct State {
    store: PathBuf,
    content: PathBuf,
    uploads: Mutex<BTreeMap<String, Vec<u8>>>,
    puts: Mutex<BTreeMap<u64, Vec<u8>>>,
    next_put: AtomicU64,
    /// The releases each app ran, oldest first, as the workspace's record keeps them.
    history: Mutex<BTreeMap<String, Vec<Value>>>,
}

impl Daemon {
    /// Start the daemon for the store at `store`, in a runtime directory this test's programs
    /// find it in.
    ///
    /// The socket lives in a short directory under the system's temporary directory: a Unix
    /// socket's path is bounded, and a run's temporary directory can be long.
    #[must_use]
    pub(crate) fn start(store: &Path) -> Self {
        let mut env = ScopedEnv::new();
        let runtime = tempfile::Builder::new()
            .prefix("tl")
            .tempdir_in("/tmp")
            .or_else(|_| tempfile::Builder::new().prefix("tl").tempdir())
            .expect("a runtime directory");
        let sockets = runtime.path().join("tilo");
        std::fs::create_dir_all(&sockets).expect("the daemon's socket directory");
        std::fs::set_permissions(&sockets, std::fs::Permissions::from_mode(0o700))
            .expect("the daemon's socket directory is its own");
        env.set("XDG_RUNTIME_DIR", runtime.path());
        let listener = UnixListener::bind(sockets.join(tilo_paths::machine::DAEMON_NATIVE))
            .expect("the daemon's native endpoint");
        listener
            .set_nonblocking(true)
            .expect("the endpoint accepts when it can");
        let content = tempfile::Builder::new()
            .prefix("tl-content")
            .tempdir()
            .expect("a content directory");
        let state = Arc::new(State {
            store: store.to_path_buf(),
            content: content.path().to_path_buf(),
            uploads: Mutex::new(BTreeMap::new()),
            puts: Mutex::new(BTreeMap::new()),
            next_put: AtomicU64::new(1),
            history: Mutex::new(BTreeMap::new()),
        });
        let stop = Arc::new(AtomicBool::new(false));
        let accepting = {
            let stop = Arc::clone(&stop);
            let state = Arc::clone(&state);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let state = Arc::clone(&state);
                            std::thread::spawn(move || serve(stream, &state));
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            })
        };
        Self {
            _runtime: runtime,
            _content: content,
            _env: env,
            stop,
            accepting: Some(accepting),
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(accepting) = self.accepting.take() {
            let _ = accepting.join();
        }
    }
}

/// Answer one connection's requests, one JSON-RPC message a line.
fn serve(stream: UnixStream, state: &Arc<State>) {
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else {
            return;
        };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let reply = answer(state, &request);
        let reply = match reply {
            Ok(value) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{"type": "text", "text": "done"}],
                    "structuredContent": value,
                    "isError": false,
                },
            }),
            Err(message) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{"type": "text", "text": message}],
                    "isError": true,
                },
            }),
        };
        if writeln!(writer, "{reply}").is_err() || writer.flush().is_err() {
            return;
        }
    }
}

/// What `request` asks, as the daemon answers it.
fn answer(state: &Arc<State>, request: &Value) -> Result<Value, String> {
    let params = request.get("params").unwrap_or(&Value::Null);
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let arguments = params.get("arguments").unwrap_or(&Value::Null);
    match name {
        "tilo_put_begin" => Ok(json!({"upload": state.next_put.fetch_add(1, Ordering::Relaxed)})),
        "tilo_put_append" => {
            let upload = arguments
                .get("upload")
                .and_then(Value::as_u64)
                .ok_or("no upload")?;
            let offset = arguments
                .get("offset")
                .and_then(Value::as_u64)
                .ok_or("no offset")?;
            let chunk = arguments
                .get("chunk")
                .and_then(Value::as_str)
                .ok_or("no chunk")?;
            let bytes = base64_decode(chunk)?;
            append(
                &mut state.puts.lock().expect("puts"),
                upload,
                offset,
                &bytes,
            )?;
            Ok(json!({}))
        }
        "tilo_put_finish" => {
            let upload = arguments
                .get("upload")
                .and_then(Value::as_u64)
                .ok_or("no upload")?;
            let bytes = state
                .puts
                .lock()
                .expect("puts")
                .remove(&upload)
                .unwrap_or_default();
            let hash = blake3::hash(&bytes).to_hex().to_string();
            write_content(state, &hash, &bytes)?;
            Ok(json!(format!("tilo.content:{hash}")))
        }
        "tilo_do" => {
            let path = arguments
                .get("path")
                .and_then(Value::as_str)
                .ok_or("no path")?;
            let args = arguments.get("args").unwrap_or(&Value::Null);
            do_action(state, path, args)
        }
        other => Err(format!("the daemon offers no {other}")),
    }
}

/// `tilo_do` of `path` with `args`.
fn do_action(state: &Arc<State>, path: &str, args: &Value) -> Result<Value, String> {
    if path == "workspace/apps#upload" {
        let file = args.get("file").and_then(Value::as_str).ok_or("no file")?;
        let offset = args
            .get("offset")
            .and_then(Value::as_u64)
            .ok_or("no offset")?;
        let data = args.get("data").and_then(Value::as_str).ok_or("no data")?;
        let bytes = hex::decode(data).map_err(|error| error.to_string())?;
        append(
            &mut state.uploads.lock().expect("uploads"),
            file.to_owned(),
            offset,
            &bytes,
        )?;
        Ok(json!({}))
    } else if path == "workspace/apps#install" {
        install(state, args)
    } else if let Some(rest) = path.strip_prefix("workspace/apps/") {
        let (app, action) = rest.split_once('#').ok_or("an app's action names it")?;
        match action {
            "uninstall" => {
                let answer = apply(
                    state,
                    Change::Uninstall {
                        app: app.to_owned(),
                        saved_data: SavedData::Delete,
                        dry_run: false,
                    },
                )?;
                state.history.lock().expect("history").remove(app);
                Ok(answer)
            }
            "pin" | "unpin" => apply(
                state,
                Change::SetPinned {
                    app: app.to_owned(),
                    pinned: action == "pin",
                },
            ),
            "update" => install(state, args),
            "earlier" => Ok(json!({ "releases": earlier(state, app) })),
            other => Err(format!("the daemon offers no {other} of an app")),
        }
    } else {
        Err(format!("the daemon offers no {path}"))
    }
}

/// The workspace's install or update of the version `args` names: the bytes a caller put first,
/// else the package the configured registries offer for the root and publisher the workspace
/// recorded, after the runtimes it runs on.
fn install(state: &Arc<State>, args: &Value) -> Result<Value, String> {
    let text = |name: &str| {
        args.get(name)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("the install names no {name}"))
    };
    let app = text("app")?;
    let version = text("version")?;
    let package = text("package")?;
    let publisher = text("publisher")?;
    let surface = text("surface")?;
    let dev = args.get("dev").and_then(Value::as_bool).unwrap_or(false);
    let (bytes, payload_from) = if let Some(file) = args.get("file").and_then(Value::as_str) {
        (
            state
                .uploads
                .lock()
                .expect("uploads")
                .get(file)
                .cloned()
                .ok_or_else(|| format!("{file} is no file this device holds"))?,
            None,
        )
    } else {
        let (bytes, registry) =
            fetch_offered(&app, &package, &publisher, &surface, &state.content)?;
        (bytes, Some(registry))
    };
    keep_runtimes(state, &bytes)?;
    let answer = install_package(
        state,
        &bytes,
        &package,
        &publisher,
        &surface,
        dev,
        payload_from,
    )?;
    state
        .history
        .lock()
        .expect("history")
        .entry(app)
        .or_default()
        .push(json!({
            "version": version,
            "surface": surface,
            "package": package,
            "publisher": publisher,
        }));
    Ok(answer)
}

/// The bytes the enabled registries offer for the root and publisher the workspace recorded: the
/// package of that exact version, downloaded and checked against its signed entry.
fn fetch_offered(
    app: &str,
    package: &str,
    publisher: &str,
    surface: &str,
    content: &Path,
) -> Result<(Vec<u8>, String), String> {
    let registries = app_cli::config::load_registries();
    for registry in registries.enabled() {
        let Ok(offers) = app_cli::version_offers(app, &registry.name) else {
            continue;
        };
        let Some(offer) = offers.into_iter().find(|offer| {
            offer.tapp_root_hash_hex.eq_ignore_ascii_case(package)
                && offer.publisher_pubkey_hex.eq_ignore_ascii_case(publisher)
        }) else {
            continue;
        };
        if !offer.tapp_surface_hex.eq_ignore_ascii_case(surface) {
            return Err(format!(
                "{} offers version {} of {app} with another surface than the workspace records",
                registry.name, offer.version
            ));
        }
        let staged = tempfile::NamedTempFile::new_in(content).map_err(|error| error.to_string())?;
        app_cli::install::fetch_verified(&offer, staged.path(), &mut |_| {})
            .map_err(|error| error.user_message())?;
        let bytes = std::fs::read(staged.path()).map_err(|error| error.to_string())?;
        return Ok((bytes, registry.base_url.clone()));
    }
    Err(format!(
        "no repository offers the version of {app} the workspace records"
    ))
}

/// Install the runtimes the app of `bytes` runs on and requires, each that this device lacks,
/// from the enabled registries: what the follower does before an app lands without them.
fn keep_runtimes(state: &Arc<State>, bytes: &[u8]) -> Result<(), String> {
    let tapp = tilo_app_package::tapp::read(
        std::io::Cursor::new(bytes),
        &mut tilo_app_package::blobs::DiscardBlobs,
    )
    .map_err(|error| error.to_string())?;
    let manifest = &tapp.meta.manifest;
    let mut wanted: Vec<String> = manifest
        .target
        .as_ref()
        .map(|target| target.runtime.clone())
        .filter(|runtime| !runtime.is_empty())
        .into_iter()
        .collect();
    wanted.extend(
        tilo_app_package::desktop::required_runtime_dependencies(manifest)
            .iter()
            .cloned(),
    );
    for runtime in wanted {
        if app_cli::runtime_installed(&runtime) {
            continue;
        }
        let offer = app_cli::runtime_offer_from_enabled(&runtime)?
            .ok_or_else(|| format!("no repository offers the runtime {runtime}"))?;
        let staged =
            tempfile::NamedTempFile::new_in(&state.content).map_err(|error| error.to_string())?;
        app_cli::install::fetch_verified(&offer, staged.path(), &mut |_| {})
            .map_err(|error| error.user_message())?;
        let bytes = std::fs::read(staged.path()).map_err(|error| error.to_string())?;
        let payload_from = app_cli::config::load_registries()
            .enabled()
            .find(|registry| registry.name == offer.registry)
            .map(|registry| registry.base_url.clone());
        install_package(
            state,
            &bytes,
            &offer.tapp_root_hash_hex,
            &offer.publisher_pubkey_hex,
            &offer.tapp_surface_hex,
            false,
            payload_from,
        )?;
    }
    Ok(())
}

/// Install `bytes` against the package root, publisher and surface the workspace recorded.
fn install_package(
    state: &Arc<State>,
    bytes: &[u8],
    package: &str,
    publisher: &str,
    surface: &str,
    dev: bool,
    payload_from: Option<String>,
) -> Result<Value, String> {
    let found =
        app_registry::publisher::entry::surface_hex(bytes).map_err(|error| format!("{error:#}"))?;
    if !surface.is_empty() && !found.eq_ignore_ascii_case(surface) {
        return Err("a package installs that is not the surface the workspace records".to_owned());
    }
    let hash = blake3::hash(bytes).to_hex().to_string();
    write_content(state, &hash, bytes)?;
    let (attestors, revoked) = trust();
    apply(
        state,
        Change::Install {
            file: Uploaded {
                source: format!("tilo.content:{hash}"),
            },
            trust: Trust {
                require_package_root: Some(package.to_owned()),
                require_publisher: Some(publisher.to_owned()),
                require_attestor: None,
                allow_self_signed: dev,
                attestors,
                revoked,
            },
            pinned: false,
            replacement: Replacement::Replace,
            ignore_missing_runtime: false,
            dry_run: false,
            payload_from,
        },
    )
}

/// The releases `app` ran before the one it runs now, the most recent first: what going back a
/// version goes back to.
fn earlier(state: &Arc<State>, app: &str) -> Vec<Value> {
    let ran = state
        .history
        .lock()
        .expect("history")
        .get(app)
        .cloned()
        .unwrap_or_default();
    let current = ran.last().cloned();
    let mut out: Vec<Value> = Vec::new();
    for release in ran.iter().rev() {
        if Some(release) == current.as_ref() {
            continue;
        }
        if !out.contains(release) {
            out.push(release.clone());
        }
    }
    out
}

/// Make `change` to this device's store, answering what it said.
fn apply(state: &Arc<State>, change: Change) -> Result<Value, String> {
    let store = package_store::PackageStore::at(&state.store);
    let catalog = package_store::Catalog::at(&state.store, package_store::SystemStore::from_env());
    let content = state.content.clone();
    let files = move |source: &str| {
        source
            .strip_prefix("tilo.content:")
            .map(|hash| content.join(hash))
            .filter(|path| path.is_file())
    };
    let report = package_actions::apply(
        &Target {
            store: &store,
            catalog: &catalog,
            max_store_bytes: package_store::DEFAULT_MAX_STORE_BYTES,
        },
        change,
        &Entry::Device(&files),
    )?;
    Ok(json!({"lines": report.lines, "warnings": report.warnings}))
}

/// The workspace's package trust, as the test states it in `TILO_WORKSPACE_TRUST` (the same
/// reading `tilo-install check` and a program of the person's take).
fn trust() -> (Vec<TrustedAttestor>, Vec<RevokedKey>) {
    let Ok(text) = std::env::var("TILO_WORKSPACE_TRUST") else {
        return (Vec::new(), Vec::new());
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return (Vec::new(), Vec::new());
    };
    let rows = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let attestors = rows("attestors")
        .into_iter()
        .filter_map(|row| {
            Some(TrustedAttestor {
                id: row.get("id")?.as_str()?.to_owned(),
                pubkey_hex: row.get("pubkey_hex")?.as_str()?.to_owned(),
                scope: AttestorScope::parse(
                    row.get("scope").and_then(Value::as_str).unwrap_or("all"),
                )
                .ok()?,
            })
        })
        .collect();
    let revoked = rows("revoked")
        .into_iter()
        .filter_map(|row| {
            Some(RevokedKey {
                pubkey_hex: row.get("pubkey_hex")?.as_str()?.to_owned(),
                reason: row
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            })
        })
        .collect();
    (attestors, revoked)
}

/// Keep `bytes` as the content `hash`.
fn write_content(state: &Arc<State>, hash: &str, bytes: &[u8]) -> Result<(), String> {
    std::fs::create_dir_all(&state.content).map_err(|error| error.to_string())?;
    std::fs::write(state.content.join(hash), bytes).map_err(|error| error.to_string())
}

/// Append `bytes` at `offset` to `parts[name]`.
fn append<K: Ord>(
    parts: &mut BTreeMap<K, Vec<u8>>,
    name: K,
    offset: u64,
    bytes: &[u8],
) -> Result<(), String> {
    let at = usize::try_from(offset).map_err(|error| error.to_string())?;
    let held = parts.entry(name).or_default();
    // The same content may be put again: a piece at zero starts it over, which is the same
    // bytes for the same hash. Any other piece must follow where the file so far ends.
    if at == 0 {
        held.clear();
    } else if at != held.len() {
        return Err(format!(
            "a piece starts at {at}, not where {} ends",
            held.len()
        ));
    }
    held.extend_from_slice(bytes);
    Ok(())
}

/// Standard base64, as a put's chunk is written.
fn base64_decode(text: &str) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .map_err(|error| error.to_string())
}
