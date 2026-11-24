//! How a program of the person's (`app`, `tilo-install`) asks this computer's daemon for a
//! change over its native endpoint, as MCP: an app installed, updated, kept or removed is the
//! action of the workspace's tree (`workspace/apps…`), whose every device then follows it, and
//! a package file it names is handed to the workspace first. So a change from a terminal is the
//! same action, admitted the same way, as one from an agent or another device.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;

use base64::Engine as _;
use serde_json::{Value, json};

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
        daemon.workspace(&format!("workspace/apps/{}#pin", manifest.id), &json!({}))?;
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
    update_to(app, &release_of(&newest))
}

/// The workspace's update of `app` to the version `release` names (as [`release_of`] does).
///
/// # Errors
/// The daemon could not be reached, or the workspace refused.
pub fn update_to(app: &str, release: &Value) -> Result<Value, String> {
    Daemon::reach()?.workspace(&format!("workspace/apps/{app}#update"), release)
}

/// The versions of `app` the workspace ran before the one it runs, the most recent first, as its
/// record's history keeps them (`workspace/apps/<app>#earlier`).
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

/// Ask `action` (`uninstall`, `pin`, `unpin`) of the app `app` in the workspace's tree.
///
/// # Errors
/// The daemon could not be reached, or the workspace refused.
pub fn on_app(app: &str, action: &str) -> Result<Value, String> {
    Daemon::reach()?.workspace(&format!("workspace/apps/{app}#{action}"), &json!({}))
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

/// One connection to the daemon's native endpoint, as MCP.
pub struct Daemon {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    id: u64,
}

/// The bytes of a package file one `upload` piece carries.
const UPLOAD_PIECE: usize = 256 * 1024;

impl Daemon {
    /// A connection to this computer's daemon.
    /// # Errors
    /// No daemon answers on this computer.
    pub fn reach() -> Result<Self, String> {
        let socket = tilo_paths::machine::daemon_native()?;
        let stream = UnixStream::connect(&socket).map_err(|error| {
            format!(
                "Tilo's background service isn't answering on this computer ({}: {error}): \
                 changes to what it runs go through it. Run `tilo status` to see how it \
                 stands. An image build writes its store with `--offline-store DIR` instead",
                socket.display()
            )
        })?;
        Ok(Self {
            reader: BufReader::new(stream.try_clone().map_err(|error| error.to_string())?),
            writer: stream,
            id: 0,
        })
    }

    /// Ask `path` of the workspace this computer is logged in to.
    ///
    /// # Errors
    /// The workspace refused, or this computer is logged in to none.
    pub fn workspace(&mut self, path: &str, arguments: &Value) -> Result<Value, String> {
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
        writeln!(self.writer, "{request}").map_err(|error| format!("the daemon: {error}"))?;
        let mut line = String::new();
        self.reader
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
