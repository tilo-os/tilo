//! How a program of the person's (`app`, `tilo-install`) asks this computer's daemon for a
//! change over its native endpoint: an app installed, updated, kept or removed is the action of
//! the workspace's tree (`workspace/apps…`), whose every device then follows it, and a package
//! file it names is handed to the workspace first. So a change from a terminal is the same
//! action, admitted the same way, as one from an agent or another device.
//!
//! The change is made in the workspace the person named ([`act_in`], [`WORKSPACE_ENV`]), over
//! the daemon's action wire, which reaches every workspace this computer holds for them: a
//! company's workspace is changed whatever stands with the computer's own. With none named it
//! is made in the daemon's own workspace, over its MCP session.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{Map, Value, json};
use tilo_action_line::LineClient;
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, ContentPut as _, InvokeArgument, InvokeRequest,
    MAX_REMOTE_WAIT, Pending, ReceiptStatus, RemoteActionTarget, UpdateMark,
};
use tilo_actions::{ActionAddress, InvocationId, InvocationUpdate};

/// The variable naming the workspace, by its id, a change from a terminal is made in: `tilo -w
/// <workspace> app …` hands it on, and `app --workspace` and `tilo-install --workspace` set it
/// for their own process.
pub const WORKSPACE_ENV: &str = "TILO_APP_WORKSPACE";

/// The workspace this process's changes are made in, once named ([`act_in`]).
static NAMED: OnceLock<Option<String>> = OnceLock::new();

/// Make this process's changes in `workspace` (its id), or, with none, in the one
/// [`WORKSPACE_ENV`] names, else the daemon's own. The first naming holds.
pub fn act_in(workspace: Option<String>) {
    let _ = NAMED.set(workspace.or_else(from_env));
}

fn from_env() -> Option<String> {
    std::env::var(WORKSPACE_ENV)
        .ok()
        .map(|named| named.trim().to_owned())
        .filter(|named| !named.is_empty())
}

/// The workspace this process's changes are made in, when one was named.
pub(crate) fn named() -> Option<&'static str> {
    NAMED.get_or_init(from_env).as_deref()
}

/// How long a change may take before its caller stops waiting: a runtime built, a registry
/// fetched.
pub const CHANGE_WAIT_MS: u64 = 30 * 60 * 1000;

/// The most bytes one step of a put carries.
const PUT_CHUNK: usize = 512 * 1024;

/// The workspace's install of the package file at `file`, put first: the version it is, bound
/// to its package root, its publisher and its surface, `dev` for a developer's own build, kept
/// at that version when `pinned`.
///
/// # Errors
/// The file is no signed package, the daemon could not be reached, or the workspace refused.
pub fn install_file(file: &Path, dev: bool, pinned: bool) -> Result<Value, String> {
    let bytes = std::fs::read(file).map_err(|error| format!("{}: {error}", file.display()))?;
    let tapp =
        tilo_app_package::tapp::read(bytes.as_slice(), &mut tilo_app_package::blobs::DiscardBlobs)
            .map_err(|error| format!("{}: {error}", file.display()))?;
    if tapp.meta.root.signature.is_none() {
        return Err(format!("{} is not signed", file.display()));
    }
    let surface = app_registry::publisher::entry::surface_hex(&bytes)
        .map_err(|error| format!("{}: {error:#}", file.display()))?;
    let manifest = &tapp.meta.manifest.package;
    let mut daemon = Daemon::reach()?;
    let hash = daemon.upload_app_file(&bytes)?;
    let installed = daemon.workspace(
        "workspace/apps#install",
        &json!({
            "app": manifest.id,
            "version": manifest.version,
            "surface": surface,
            "package": hex::encode(tapp.meta.id.0),
            "publisher": hex::encode(tapp.meta.root.publisher_identity),
            "file": hash,
            "dev": dev,
        }),
    )?;
    if pinned {
        daemon.workspace("workspace/apps#pin", &json!({ "app": manifest.id }))?;
    }
    Ok(installed)
}

/// The workspace's install of a version a registry offers (`release`: its `app`, `version`,
/// `surface`, `package` and `publisher`), with no file: every device fetches it from the
/// registries.
///
/// # Errors
/// The daemon could not be reached, or the workspace refused.
pub fn install_offered(release: &Value) -> Result<Value, String> {
    Daemon::reach()?.workspace("workspace/apps#install", release)
}

/// The version a registry offers, as the workspace's install names it: no file, so every
/// device fetches it from the registries and checks it against this.
#[must_use]
pub fn release_of(offer: &crate::CatalogueApp) -> Value {
    release(
        &offer.package_id,
        &offer.version,
        &offer.tapp_surface_hex,
        &offer.tapp_root_hash_hex,
        &offer.publisher_pubkey_hex,
    )
}

/// A version as the workspace's install and update name it, no file: every device fetches it
/// from the registries and checks it against `package` and `publisher`, a package that
/// publishes none being signed with no `surface`.
#[must_use]
pub fn release(app: &str, version: &str, surface: &str, package: &str, publisher: &str) -> Value {
    json!({
        "app": app,
        "version": version,
        "surface": surface,
        "package": package,
        "publisher": publisher,
    })
}

/// The workspace's update of `app` to the newest version the enabled registries offer that
/// `requirement` (a semver requirement) allows.
///
/// # Errors
/// No registry offers such a version, the daemon could not be reached, or the workspace
/// refused.
pub fn update_offered(app: &str, requirement: Option<&str>) -> Result<Value, String> {
    let requirement = requirement
        .map(semver::VersionReq::parse)
        .transpose()
        .map_err(|error| format!("{error}"))?;
    let newest = crate::all_offers()
        .apps
        .into_iter()
        .filter(|offer| offer.package_id == app)
        .filter_map(|offer| Some((semver::Version::parse(&offer.version).ok()?, offer)))
        .filter(|(version, _)| {
            requirement
                .as_ref()
                .is_none_or(|wanted| wanted.matches(version))
        })
        .max_by(|(left, _), (right, _)| left.cmp(right))
        .map(|(_, offer)| offer)
        .ok_or_else(|| format!("no repository offers a version of {app} to update to"))?;
    install_offered(&release_of(&newest))
}

/// The versions of `app` the workspace ran before the one it runs, the most recent first, as its
/// record's history keeps them (`workspace/apps#earlier`).
///
/// # Errors
/// The daemon could not be reached, or the workspace refused.
pub fn earlier_versions(app: &str) -> Result<Vec<Value>, String> {
    let answer = on_app(app, "earlier")?;
    Ok(answer
        .get("releases")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

/// Ask `action` (`uninstall`, `pin`, `unpin`, `earlier`) of the workspace's apps for `app`.
///
/// # Errors
/// The daemon could not be reached, or the workspace refused.
pub fn on_app(app: &str, action: &str) -> Result<Value, String> {
    Daemon::reach()?.workspace(&format!("workspace/apps#{action}"), &json!({ "app": app }))
}

/// The workspace's registry action `action` (`registry.list`, `registry.add`,
/// `registry.remove`, `registry.enable`, `registry.disable`, `registry.channel`) with
/// `arguments`: the registries a workspace fetches apps from are its records, and every device
/// asks the daemon for them rather than keeping a list of its own.
///
/// # Errors
/// `action` is no registry action, the daemon could not be reached, or the workspace refused.
pub fn registries(action: &str, arguments: &Value) -> Result<Value, String> {
    let verb = action.strip_prefix("registry.").filter(|verb| {
        matches!(
            *verb,
            "list" | "add" | "remove" | "enable" | "disable" | "channel"
        )
    });
    let verb = verb.ok_or_else(|| format!("{action} is no registry action"))?;
    Daemon::reach()?.workspace(&format!("workspace/registries#{verb}"), arguments)
}

/// The connection to the daemon serving `socket`, or what to tell the person when none does.
///
/// # Errors
/// No daemon answers on `socket`.
fn stream_at(socket: &Path) -> Result<tilo_files::native::socket::Stream, String> {
    tilo_files::native::socket::connect(socket).map_err(|error| {
        format!(
            "Tilo's background service isn't answering on this computer ({}: {error}): \
             changes to what it runs go through it. Run `tilo status` to see how it \
             stands. An image build writes its store with `--offline-store DIR` instead",
            socket.display()
        )
    })
}

/// The action wire to the daemon serving `socket`: what a change from this computer's own
/// account asks it over, so the daemon decides it as that person's own call and never as an
/// agent's.
///
/// # Errors
/// No daemon answers on `socket`.
pub fn line_at(socket: &Path) -> Result<tilo_action_line::LineClient, String> {
    let stream = stream_at(socket)?;
    tilo_action_line::LineClient::of_stream(stream).map_err(|error| error.to_string())
}

/// The action wire to this computer's daemon ([`line_at`] at its socket).
///
/// # Errors
/// No daemon answers on this computer.
pub fn line() -> Result<tilo_action_line::LineClient, String> {
    line_at(&tilo_machine::daemon_native()?)
}

/// One connection to the daemon's native endpoint: over the action wire in the workspace the
/// person named, else as MCP in the daemon's own.
pub struct Daemon {
    /// Where the daemon answers, and its MCP session there, once a call opened it.
    socket: std::path::PathBuf,
    mcp: Option<(
        BufReader<tilo_files::native::socket::Stream>,
        tilo_files::native::socket::Stream,
    )>,
    id: u64,
    /// The action wire, in the workspace named, when one was.
    wire: Option<Wire>,
}

/// The bytes of a package file one `upload` piece carries.
const UPLOAD_PIECE: usize = 256 * 1024;

impl Daemon {
    /// A connection to this computer's daemon.
    /// # Errors
    /// No daemon answers on this computer.
    pub fn reach() -> Result<Self, String> {
        let socket = tilo_machine::daemon_native()?;
        Self::at(&socket)
    }

    /// A connection to the daemon serving `socket`: this computer's own, or one a program names
    /// (a runtime's profile, a test's).
    ///
    /// # Errors
    /// No daemon answers on `socket`.
    pub fn at(socket: &Path) -> Result<Self, String> {
        let (wire, mcp) = match named() {
            Some(workspace) => (Some(Wire::in_workspace(line_at(socket)?, workspace)), None),
            None => (None, Some(Self::session(socket)?)),
        };
        Ok(Self {
            socket: socket.to_owned(),
            mcp,
            id: 0,
            wire,
        })
    }

    /// An MCP session with the daemon on `socket`.
    fn session(
        socket: &Path,
    ) -> Result<
        (
            BufReader<tilo_files::native::socket::Stream>,
            tilo_files::native::socket::Stream,
        ),
        String,
    > {
        let stream = stream_at(socket)?;
        let reader = BufReader::new(stream.try_clone().map_err(|error| error.to_string())?);
        Ok((reader, stream))
    }

    /// Ask `path` (`workspace/<resource>#<action>`) of the workspace the person named, else of
    /// the daemon's own.
    ///
    /// # Errors
    /// The workspace refused, or this computer is logged in to none.
    pub fn workspace(&mut self, path: &str, arguments: &Value) -> Result<Value, String> {
        if let Some(wire) = &self.wire {
            return wire.workspace(path, arguments);
        }
        self.call(
            "tilo_do",
            &json!({"path": path, "wait_ms": CHANGE_WAIT_MS, "args": arguments}),
        )
        .map_err(|error| {
            if error.contains("serves no workspace") {
                format!(
                    "{error}\nAn app is installed in a workspace: log this computer in first \
                     (`tilo login`) when it is not."
                )
            } else {
                error
            }
        })
    }

    /// Hand the package `bytes` to the workspace in the pieces it takes: their BLAKE3, which
    /// an install names as the version's file.
    fn upload_app_file(&mut self, bytes: &[u8]) -> Result<String, String> {
        let hash = blake3::hash(bytes).to_hex().to_string();
        let total = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let mut offset = 0_u64;
        for piece in bytes.chunks(UPLOAD_PIECE) {
            self.workspace(
                "workspace/apps#upload",
                &json!({"file": hash, "offset": offset, "total": total, "data": hex::encode(piece)}),
            )?;
            offset = offset.saturating_add(u64::try_from(piece.len()).unwrap_or(u64::MAX));
        }
        Ok(hash)
    }

    /// Call the tool `tool`: its answer, or what it failed with.
    ///
    /// # Errors
    /// The tool failed, or the daemon answered no MCP.
    pub fn call(&mut self, tool: &str, arguments: &Value) -> Result<Value, String> {
        self.id = self.id.saturating_add(1);
        let request = json!({"jsonrpc": "2.0", "id": self.id, "method": "tools/call",
            "params": {"name": tool, "arguments": arguments}});
        if self.mcp.is_none() {
            self.mcp = Some(Self::session(&self.socket)?);
        }
        let (reader, writer) = self.mcp.as_mut().ok_or("no MCP session")?;
        writeln!(writer, "{request}").map_err(|error| format!("the daemon: {error}"))?;
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .map_err(|error| format!("the daemon: {error}"))?;
        let reply: Value = serde_json::from_str(&line)
            .map_err(|error| format!("the daemon answered no MCP: {error}"))?;
        if let Some(error) = reply.get("error") {
            return Err(error
                .get("message")
                .and_then(Value::as_str)
                .map_or_else(|| error.to_string(), str::to_owned));
        }
        let result = reply.get("result").cloned().unwrap_or(Value::Null);
        let text = result
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        if result.get("isError") == Some(&Value::Bool(true)) {
            return Err(text);
        }
        Ok(result
            .get("structuredContent")
            .cloned()
            .or_else(|| serde_json::from_str(&text).ok())
            .unwrap_or(Value::String(text)))
    }

    /// Put the file at `path` into the device's content: its reference there.
    ///
    /// # Errors
    /// The file could not be read, or the daemon refused it.
    pub fn put(&mut self, path: &Path) -> Result<String, String> {
        let unreadable = |error: std::io::Error| format!("{}: {error}", path.display());
        let mut file = std::fs::File::open(path).map_err(unreadable)?;
        let size = file.metadata().map_err(unreadable)?.len();
        let begun = self.call("tilo_put_begin", &json!({"size": size}))?;
        let upload = begun
            .get("upload")
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("the daemon began no upload: {begun}"))?;
        let mut hasher = blake3::Hasher::new();
        let mut offset = 0_u64;
        let mut chunk = vec![0_u8; PUT_CHUNK];
        loop {
            let read = file.read(&mut chunk).map_err(unreadable)?;
            let Some(bytes) = chunk.get(..read).filter(|bytes| !bytes.is_empty()) else {
                break;
            };
            hasher.update(bytes);
            self.call(
                "tilo_put_append",
                &json!({"upload": upload, "offset": offset,
                    "chunk": base64::engine::general_purpose::STANDARD.encode(bytes)}),
            )?;
            offset = offset.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        }
        self.call("tilo_put_finish", &json!({"upload": upload}))?;
        Ok(format!("tilo.content:{}", hasher.finalize().to_hex()))
    }
}

/// One connection to the daemon over its action wire, as this computer's own account: who a call
/// is, the daemon decides from the connection, and the workspace a call names is all a frame
/// carries of its caller.
pub struct Wire {
    line: LineClient,
    caller: Caller,
    /// The workspace its calls are made in; `None`: the daemon's own.
    workspace: Option<String>,
}

impl Wire {
    /// The action wire `line`, its calls made in the daemon's own workspace.
    #[must_use]
    pub fn of(line: LineClient) -> Self {
        Self {
            line,
            caller: Caller::local_person(""),
            workspace: None,
        }
    }

    /// The action wire `line`, its calls made in `workspace`.
    #[must_use]
    pub fn in_workspace(line: LineClient, workspace: &str) -> Self {
        Self {
            line,
            caller: Caller::local_person("")
                .in_workspace(workspace)
                .in_new_execution(),
            workspace: Some(workspace.to_owned()),
        }
    }

    /// The line its calls go over.
    #[must_use]
    pub fn line(&self) -> &LineClient {
        &self.line
    }

    /// Who its calls are named as.
    #[must_use]
    pub fn caller(&self) -> &Caller {
        &self.caller
    }

    /// The workspace action `path` names (`workspace/<resource>#<action>`) with `arguments`, in
    /// the workspace these calls are made in, to its end: what it answered.
    ///
    /// # Errors
    /// `path` names no workspace action, no workspace was named, or the workspace refused or
    /// failed it.
    pub fn workspace(&self, path: &str, arguments: &Value) -> Result<Value, String> {
        let workspace = self
            .workspace
            .as_deref()
            .ok_or("a workspace's action is asked in the workspace it names")?;
        let (resource, action) = path
            .strip_prefix("workspace/")
            .and_then(|path| path.rsplit_once('#'))
            .ok_or_else(|| format!("{path} names no action of a workspace"))?;
        // A workspace's own actions are its governance provider's, each on the child of its tree
        // the path names (`apps`, `apps/<app>`, `trust`).
        let target = RemoteActionTarget {
            provider: tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER.to_owned(),
            address: ActionAddress::new(
                tilo_actions::remote::workspace_governance_child(workspace, Some(resource)),
                action,
            ),
        };
        let arguments = match arguments {
            Value::Object(arguments) => arguments.clone(),
            Value::Null => Map::new(),
            other => return Err(format!("{path} takes named arguments, not {other}")),
        };
        self.invoke(&target, arguments)
    }

    /// `target`'s action with `arguments`, to its end: the value it answered.
    ///
    /// # Errors
    /// The daemon refused the call, it failed or was stopped, or it did not end in time.
    pub fn invoke(
        &self,
        target: &RemoteActionTarget,
        arguments: Map<String, Value>,
    ) -> Result<Value, String> {
        let request = InvokeRequest {
            asks: false,
            invocation: InvocationId::new(),
            target: target.clone(),
            argument: InvokeArgument::Json(arguments),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let id = request.invocation;
        let receipt = waited(self.line.invoke(&self.caller, request), CHANGE_WAIT)?;
        let deadline = Instant::now().checked_add(CHANGE_WAIT);
        let mut status = receipt.status;
        let mut seen = UpdateMark::Accepted;
        loop {
            match status {
                ReceiptStatus::Accepted => {}
                ReceiptStatus::Completed { output } => {
                    return Ok(output.value.unwrap_or_default());
                }
                ReceiptStatus::Failed { error } => {
                    return Err(format!("{}: {}", error.code, error.message));
                }
                ReceiptStatus::Rejected { error } => return Err(error.said()),
                ReceiptStatus::Cancelled { message } => {
                    return Err(message.unwrap_or_else(|| "the change was stopped".to_owned()));
                }
            }
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err("the change did not finish in time".to_owned());
            }
            // The daemon answers once the change moves on, or after its longest wait: a change
            // that takes minutes is followed until it ends.
            match self.line.next_change(&self.caller, &id) {
                Some(moved) => {
                    let _ = moved.wait_timeout(MAX_REMOTE_WAIT);
                }
                None => std::thread::sleep(PAUSE),
            }
            let Some(state) = self.line.observe(&self.caller, &id) else {
                return Err("the daemon no longer keeps the change".to_owned());
            };
            let at = UpdateMark::of(&state.update);
            if at == seen {
                // A provider that answers at once leaves the wait nothing to wait for: the
                // change is followed at a pace rather than spun on.
                std::thread::sleep(PAUSE);
            }
            seen = at;
            status = match state.update {
                InvocationUpdate::Succeeded { output, .. } => ReceiptStatus::Completed { output },
                InvocationUpdate::Failed { error, .. } => ReceiptStatus::Failed { error },
                InvocationUpdate::Cancelled { message, .. } => ReceiptStatus::Cancelled { message },
                InvocationUpdate::Accepted { .. }
                | InvocationUpdate::Progress { .. }
                | InvocationUpdate::Item { .. } => ReceiptStatus::Accepted,
            };
        }
    }

    /// Put the file at `path` into the store the action `target` names reads from: its
    /// reference there.
    ///
    /// # Errors
    /// The file could not be read, or the daemon refused it.
    pub fn put(&self, path: &Path, target: &RemoteActionTarget) -> Result<String, String> {
        let unreadable = |error: std::io::Error| format!("{}: {error}", path.display());
        let mut file = std::fs::File::open(path).map_err(unreadable)?;
        let size = file.metadata().map_err(unreadable)?.len();
        let mut put = self
            .line
            .begin_put_for(size, Some(target.clone()))
            .map_err(|error| error.said())?;
        let mut offset = 0_u64;
        let mut piece = vec![0_u8; PUT_PIECE];
        loop {
            let read = file.read(&mut piece).map_err(unreadable)?;
            let Some(bytes) = piece.get(..read).filter(|bytes| !bytes.is_empty()) else {
                break;
            };
            put.append(offset, bytes).map_err(|error| error.said())?;
            offset = offset.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        }
        Box::new(put).finish().map_err(|error| error.said())
    }
}

/// How long a change made through the daemon may take to run.
const CHANGE_WAIT: Duration = Duration::from_millis(CHANGE_WAIT_MS);

/// The bytes of a package file one piece of a put over the action wire carries.
const PUT_PIECE: usize = 512 * 1024;

/// How long a change that has not moved on is left before it is looked at again.
const PAUSE: Duration = Duration::from_millis(100);

/// `pending`, waited for `within`.
///
/// # Errors
/// The daemon refused, or did not answer in time.
pub fn waited<T>(
    pending: Pending<Result<T, ActionCallError>>,
    within: Duration,
) -> Result<T, String> {
    match pending.wait_timeout(within) {
        Ok(Some(Ok(answer))) => Ok(answer),
        Ok(Some(Err(refused))) => Err(refused.said()),
        Ok(None) | Err(_) => Err("the daemon did not answer in time".to_owned()),
    }
}
