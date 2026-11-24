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
}

impl Daemon {
    /// Start the daemon for `env`: a runtime directory the test's programs find it in, and the
    /// store at `store` the changes are made to.
    ///
    /// The socket lives in a short directory under the system's temporary directory: a Unix
    /// socket's path is bounded, and a run's temporary directory can be long.
    #[must_use]
    pub(crate) fn start(env: &mut ScopedEnv, store: &Path) -> Self {
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
            "uninstall" => apply(
                state,
                Change::Uninstall {
                    app: app.to_owned(),
                    saved_data: SavedData::Delete,
                    dry_run: false,
                },
            ),
            "pin" | "unpin" => apply(
                state,
                Change::SetPinned {
                    app: app.to_owned(),
                    pinned: action == "pin",
                },
            ),
            other => Err(format!("the daemon offers no {other} of an app")),
        }
    } else {
        Err(format!("the daemon offers no {path}"))
    }
}

/// The workspace's install of the package file `args` names: its bytes were put first, and this
/// device installs them against the publisher and root the workspace recorded.
fn install(state: &Arc<State>, args: &Value) -> Result<Value, String> {
    let text = |name: &str| {
        args.get(name)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("the install names no {name}"))
    };
    let app = text("app")?;
    let package = text("package")?;
    let publisher = text("publisher")?;
    let file = text("file")?;
    let dev = args.get("dev").and_then(Value::as_bool).unwrap_or(false);
    let surface = text("surface")?;
    let bytes = state
        .uploads
        .lock()
        .expect("uploads")
        .get(&file)
        .cloned()
        .ok_or_else(|| format!("{file} is no file this device holds"))?;
    let found = app_registry::publisher::entry::surface_hex(&bytes)
        .map_err(|error| format!("{error:#}"))?;
    if !found.eq_ignore_ascii_case(&surface) {
        return Err(format!(
            "{app} installs a package whose surface is not the one the workspace records"
        ));
    }
    // The bytes a caller put are content this device holds, named by their hash: what the
    // install names as `tilo.content:<hash>`.
    write_content(state, &file, &bytes)?;
    let (attestors, revoked) = trust();
    let change = Change::Install {
        file: Uploaded {
            source: format!("tilo.content:{file}"),
        },
        trust: Trust {
            require_package_root: Some(package),
            require_publisher: Some(publisher),
            require_attestor: None,
            allow_self_signed: dev,
            attestors,
            revoked,
        },
        pinned: false,
        replacement: Replacement::Replace,
        ignore_missing_runtime: false,
        dry_run: false,
        payload_from: None,
    };
    apply(state, change)
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
    let held = parts.entry(name).or_default();
    let at = usize::try_from(offset).map_err(|error| error.to_string())?;
    if at != held.len() {
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
