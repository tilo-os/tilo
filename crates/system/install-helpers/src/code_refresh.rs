//! `tilo-vm-install-proof code-refresh`: refreshes allowlisted installer
//! helpers inside an explicitly attached development VM.
//!
//! The host (`os/dev/refresh-installer-code.py`) sends one bounded,
//! named-file packet over the `org.tilo.installer-code` virtio port; the
//! daemon validates it, stages every file beside the runtime state and
//! bind-mounts each over the helper it replaces, read-only, with the
//! original's mode, owner and `SELinux` label. The receipt is the canonical
//! packet's SHA-256, its file manifest and the host's source revision.
//!
//! The Python host side and the seed old CI media run
//! (`os/dev/installer-code-bootstrap/`) speak the same protocol; the receipt
//! and its digest must stay byte-identical with theirs.

use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value, json};

use crate::py::Dumps;
use crate::{Error, Result, require};

pub const SCHEMA: &str = "tilo.installer-code-refresh.v1";
pub const LIMIT: usize = 2 * 1024 * 1024;
const FILE_LIMIT: usize = 512 * 1024;
/// Text helpers only. The compiled helpers (tilo-install-handoff-copy,
/// tilo-install-dev-ssh, tilo-vm-bootc-copy-cache,
/// tilo-installer-offline-target with its development target UKI,
/// tilo-staging-handoff, tilo-vm-install-proof with this daemon) come from
/// the base image's install-helpers component; a checkout holds no file to
/// send for them.
pub const ALLOWLIST: [&str; 2] = [
    "usr/libexec/tilo-install",
    "usr/libexec/tilo-boot-mount-contract",
];
const TIMEOUT: Duration = Duration::from_secs(30);
const HELLO: &[u8] = b"TILO-CODE-HELLO-1\n";
const PROBE_MARK: &[u8] = b"# TILO_VM_CODE_REFRESH_PROBE_V1:";

/// The validated files, in packet order.
pub type Files = Vec<(String, Vec<u8>)>;

/// `json.dumps(value, sort_keys=True, separators=(",", ":"))` as bytes.
#[must_use]
pub fn canonical(value: &Value) -> Vec<u8> {
    Dumps::CANONICAL.dumps(value).into_bytes()
}

/// A JSON value whose objects refuse a repeated key at any depth, as the
/// protocol's `object_pairs_hook` did.
struct Unique(Value);

impl<'de> serde::Deserialize<'de> for Unique {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        deserializer.deserialize_any(UniqueVisitor).map(Unique)
    }
}

struct UniqueVisitor;

impl<'de> Visitor<'de> for UniqueVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON value")
    }
    fn visit_bool<E>(self, v: bool) -> std::result::Result<Value, E> {
        Ok(Value::Bool(v))
    }
    fn visit_i64<E>(self, v: i64) -> std::result::Result<Value, E> {
        Ok(Value::from(v))
    }
    fn visit_u64<E>(self, v: u64) -> std::result::Result<Value, E> {
        Ok(Value::from(v))
    }
    fn visit_f64<E>(self, v: f64) -> std::result::Result<Value, E> {
        Ok(Value::from(v))
    }
    fn visit_str<E>(self, v: &str) -> std::result::Result<Value, E> {
        Ok(Value::String(v.to_owned()))
    }
    fn visit_string<E>(self, v: String) -> std::result::Result<Value, E> {
        Ok(Value::String(v))
    }
    fn visit_unit<E>(self) -> std::result::Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(Unique(item)) = seq.next_element()? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<Value, A::Error> {
        let mut result = Map::new();
        while let Some((key, Unique(value))) = map.next_entry::<String, Unique>()? {
            if result.contains_key(&key) {
                return Err(de::Error::custom("duplicate code refresh field"));
            }
            result.insert(key, value);
        }
        Ok(Value::Object(result))
    }
}

fn parse_unique(raw: &[u8]) -> Result<Value> {
    match serde_json::from_slice::<Unique>(raw) {
        Ok(Unique(value)) => Ok(value),
        Err(error)
            if error
                .to_string()
                .starts_with("duplicate code refresh field") =>
        {
            Err(Error::new("duplicate code refresh field"))
        }
        Err(error) => Err(Error::new(error.to_string())),
    }
}

fn is_str(value: Option<&Value>) -> bool {
    matches!(value, Some(Value::String(_)))
}

fn keys_are(map: &Map<String, Value>, expected: &[&str]) -> bool {
    map.len() == expected.len() && expected.iter().all(|key| map.contains_key(*key))
}

fn metadata_ok(packet: &Value) -> bool {
    let Some(packet) = packet.as_object() else {
        return false;
    };
    if !keys_are(packet, &["schema", "files", "source"])
        || packet.get("schema").and_then(Value::as_str) != Some(SCHEMA)
    {
        return false;
    }
    let Some(source) = packet.get("source").and_then(Value::as_object) else {
        return false;
    };
    keys_are(source, &["revision", "dirty"])
        && source
            .get("revision")
            .and_then(Value::as_str)
            .is_some_and(|revision| {
                revision.chars().count() == 40
                    && revision.chars().all(|c| "0123456789abcdef".contains(c))
            })
        && source.get("dirty").is_some_and(Value::is_boolean)
}

/// `validate(raw)`: the files and the receipt binding them.
///
/// # Errors
/// An oversized packet, malformed or duplicated JSON, wrong metadata, an
/// unlisted or repeated path, or content that differs from its hash.
pub fn validate(raw: &[u8]) -> Result<(Files, Value)> {
    require(
        !raw.is_empty() && raw.len() <= LIMIT,
        "code refresh packet exceeds its bound",
    )?;
    let packet = parse_unique(raw)?;
    require(metadata_ok(&packet), "invalid code refresh metadata")?;
    let rows = packet.get("files").and_then(Value::as_array);
    require(
        rows.is_some_and(|rows| !rows.is_empty() && rows.len() <= ALLOWLIST.len()),
        "invalid code refresh file inventory",
    )?;
    let mut files: Files = Vec::new();
    let mut manifest = Vec::new();
    for row in rows.into_iter().flatten() {
        let fields = row.as_object();
        let path = fields.and_then(|f| f.get("path")).and_then(Value::as_str);
        require(
            fields.is_some_and(|f| {
                keys_are(f, &["path", "sha256", "data"]) && f.values().all(|v| is_str(Some(v)))
            }) && path.is_some_and(|path| {
                ALLOWLIST.contains(&path) && !files.iter().any(|(seen, _)| seen == path)
            }),
            "code refresh path is not uniquely allowlisted",
        )?;
        let text = |key: &str| {
            fields
                .and_then(|f| f.get(key))
                .and_then(Value::as_str)
                .unwrap_or_default()
        };
        let data = crate::py::b64decode(text("data")).map_err(|e| Error::new(e.to_string()))?;
        require(
            !data.is_empty()
                && data.len() <= FILE_LIMIT
                && !data.contains(&0)
                && crate::py::sha256_hex(&data) == text("sha256"),
            "code refresh content differs from its bounded hash",
        )?;
        let path = text("path").to_owned();
        manifest.push(json!({"path": path, "sha256": text("sha256"), "bytes": data.len()}));
        files.push((path, data));
    }
    // Canonical representation binds every selected file and source receipt;
    // insignificant JSON formatting never changes the evidence identity.
    let digest = crate::py::sha256_hex(&canonical(&packet));
    let source = packet.get("source").cloned().unwrap_or(Value::Null);
    Ok((
        files,
        json!({"schema": SCHEMA, "sha256": digest, "files": manifest, "source": source}),
    ))
}

/// `read_exact`: exactly `count` bytes, however the stream fragments them.
///
/// # Errors
/// The stream ending first, or failing.
pub fn read_exact(stream: &mut dyn Read, count: usize) -> Result<Vec<u8>> {
    let mut output = vec![0; count];
    let mut filled = 0;
    while filled < count {
        let Some(rest) = output.get_mut(filled..) else {
            break;
        };
        let read = stream.read(rest)?;
        require(read > 0, "code refresh stream ended early")?;
        filled = filled.saturating_add(read);
    }
    Ok(output)
}

fn bounded_packet(stream: &mut dyn Read, prefix: &[u8]) -> Result<(Files, Value)> {
    let size = prefix
        .try_into()
        .map(u32::from_be_bytes)
        .map_err(|_| Error::new("code refresh stream ended early"))?;
    let size = usize::try_from(size).unwrap_or(usize::MAX);
    require(
        size > 0 && size <= LIMIT,
        "code refresh packet exceeds its bound",
    )?;
    validate(&read_exact(stream, size)?)
}

/// `receive`: one length-prefixed packet, validated.
///
/// # Errors
/// As [`read_exact`] and [`validate`].
pub fn receive(stream: &mut dyn Read) -> Result<(Files, Value)> {
    let prefix = read_exact(stream, 4)?;
    bounded_packet(stream, &prefix)
}

/// `write_all`: every byte, then a flush.
///
/// # Errors
/// The stream refusing data or failing.
pub fn write_all(stream: &mut dyn Write, data: &[u8]) -> Result<()> {
    let mut remaining = data;
    while !remaining.is_empty() {
        let count = stream.write(remaining)?;
        require(count > 0, "code refresh stream stopped accepting data")?;
        remaining = remaining.get(count..).unwrap_or_default();
    }
    stream.flush()?;
    Ok(())
}

/// `stream.readline(limit)` on an unbuffered port: one byte at a time, so
/// nothing after the line is consumed.
fn readline(stream: &mut dyn Read, limit: usize) -> Result<Vec<u8>> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    while line.len() < limit {
        if stream.read(&mut byte)? == 0 {
            break;
        }
        line.push(byte[0]);
        if byte[0] == b'\n' {
            break;
        }
    }
    Ok(line)
}

/// A duplex stream: the virtio port, or a test's.
pub trait Stream: Read + Write {}

impl<T: Read + Write> Stream for T {}

/// `bootstrap_receive`: the old-media bootstrap's handshake. Leftover boot
/// output and repeated hellos are skipped, each answered with readiness,
/// within bounds.
///
/// # Errors
/// No recognizable hello, too many repeats, or as [`receive`].
pub fn bootstrap_receive(stream: &mut dyn Stream) -> Result<(Files, Value)> {
    let mut greeted = false;
    for _ in 0..64 {
        let hello = readline(stream, 1025)?;
        require(
            !hello.is_empty() && hello.len() <= 1024,
            "bootstrap hello missing or oversized",
        )?;
        if hello == HELLO {
            greeted = true;
            break;
        }
    }
    require(greeted, "bootstrap hello was not recognized")?;
    for _ in 0..64 {
        write_all(stream, b"{\"schema\":\"tilo.installer-code-ready.v1\"}\n")?;
        let prefix = read_exact(stream, 4)?;
        if prefix == b"TILO" {
            require(
                read_exact(stream, 14)? == b"-CODE-HELLO-1\n",
                "invalid repeated bootstrap hello",
            )?;
            continue;
        }
        return bounded_packet(stream, &prefix);
    }
    Err(Error::new("too many repeated bootstrap hellos"))
}

/// The development VM the daemon refreshes. The provided methods are the
/// production behaviour; tests replace the seams.
pub trait Guest {
    /// Where generations, the lock and the receipts live.
    fn base(&self) -> PathBuf {
        PathBuf::from("/run/tilo-installer/code-refresh")
    }

    /// The root the allowlisted paths are relative to.
    fn root(&self) -> PathBuf {
        PathBuf::from("/")
    }

    /// Where the running processes are listed.
    fn proc_root(&self) -> PathBuf {
        PathBuf::from("/proc")
    }

    /// A tool's stripped standard output: 30 s, nonzero exit refused.
    ///
    /// # Errors
    /// The tool failing, missing or timing out.
    fn command(&self, argv: &[&str]) -> Result<String> {
        let name = crate::process::name(argv.first().copied().unwrap_or_default());
        let output = match crate::process::run(argv, None, TIMEOUT) {
            Ok(output) => output,
            // process::run reports only its timeout as "Command '…'"; a
            // missing program carries the OS error instead.
            Err(error) if error.0.starts_with("Command '") => {
                return Err(Error::new(format!(
                    "{name} timed out refreshing installer code"
                )));
            }
            Err(error) => return Err(error),
        };
        require(
            output.success(),
            &format!("{name} failed refreshing installer code"),
        )?;
        Ok(crate::py::strip(&String::from_utf8_lossy(&output.stdout)).to_owned())
    }

    /// `gate`: only the opted-in development scenario VM, in PID 1's mount
    /// namespace.
    ///
    /// # Errors
    /// Anywhere else.
    fn gate(&self) -> Result<()> {
        let regular = |path: &str| {
            let path = Path::new(path);
            path.is_file() && !path.is_symlink()
        };
        let char_device =
            |path: &str| fs::metadata(path).is_ok_and(|m| m.file_type().is_char_device());
        let flavour = "/usr/lib/tilo/image-flavour";
        require(
            rustix::process::geteuid().is_root()
                && regular("/usr/share/tilo/unverified-vm-install")
                && regular(flavour)
                && fs::read(flavour)? == b"dev\n"
                && char_device("/dev/virtio-ports/org.tilo.installer-code")
                && char_device("/dev/virtio-ports/org.tilo.vm-install-proof")
                && crate::py::split(&String::from_utf8_lossy(&fs::read("/proc/cmdline")?))
                    .contains(&"tilo.install"),
            "installer code refresh requires the opted-in development scenario VM",
        )?;
        self.command(&["systemd-detect-virt", "--vm", "--quiet"])?;
        require(
            fs::read_link("/proc/self/ns/mnt")? == fs::read_link("/proc/1/ns/mnt")?,
            "installer code refresh must run in the PID1 mount namespace",
        )
    }

    /// `lock`: the root-private runtime directory and its lock file, opened.
    fn lock(&self) -> Result<File> {
        let base = self.base();
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&base)?;
        let info = fs::metadata(&base)?;
        require(
            !base.is_symlink() && info.uid() == 0 && info.mode() & 0o7777 == 0o700,
            "unsafe code refresh runtime directory",
        )?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(base.join("lock"))?;
        let info = file.metadata()?;
        require(
            info.file_type().is_file() && info.uid() == 0,
            "unsafe installer code refresh lock",
        )?;
        Ok(file)
    }

    /// Copies `from`'s `SELinux` label, if it has one, to `to`.
    ///
    /// # Errors
    /// The label cannot be read or set.
    fn copy_label(&self, from: &Path, to: &Path) -> Result<()> {
        let mut names = vec![0u8; 65536];
        let length = rustix::fs::listxattr(from, &mut names)?;
        let has_label = names
            .get(..length)
            .unwrap_or_default()
            .split(|byte| *byte == 0)
            .any(|name| name == b"security.selinux");
        if has_label {
            let mut label = vec![0u8; 4096];
            let length = rustix::fs::getxattr(from, "security.selinux", &mut label)?;
            rustix::fs::setxattr(
                to,
                "security.selinux",
                label.get(..length).unwrap_or_default(),
                rustix::fs::XattrFlags::empty(),
            )?;
        }
        Ok(())
    }

    /// Runs the refreshed backend's execution probe with empty input.
    ///
    /// # Errors
    /// The backend missing or timing out.
    fn run_probe(&self, argv: &[&str]) -> Result<crate::process::Output> {
        crate::process::run(argv, Some(b""), TIMEOUT)
    }

    /// Whether the cached-target fixture disk is attached.
    fn fixture_disk(&self) -> bool {
        Path::new("/dev/disk/by-id/virtio-tilo-code-fixture").exists()
    }

    /// Opens the host channel.
    ///
    /// # Errors
    /// The port cannot be opened.
    fn open_port(&self) -> Result<Box<dyn Stream>> {
        let port = File::options()
            .read(true)
            .write(true)
            .open("/dev/virtio-ports/org.tilo.installer-code")?;
        Ok(Box::new(port))
    }

    /// Tells systemd the first refresh is in place.
    ///
    /// # Errors
    /// The notification socket refuses it.
    fn ready(&self) -> Result<()> {
        notify_ready()
    }

    /// Whether the old-media bootstrap's handshake precedes the packet.
    fn bootstrap_handshake(&self) -> bool {
        std::env::var_os("TILO_CODE_BOOTSTRAP_HANDSHAKE").is_some_and(|v| v == "1")
    }

    /// The pause before the port is reopened; an error ends the loop.
    ///
    /// # Errors
    /// Never in production.
    fn pause(&self) -> Result<()> {
        std::thread::sleep(Duration::from_millis(100));
        Ok(())
    }
}

/// The development VM as it is.
#[derive(Debug, Clone, Copy, Default)]
pub struct System;

impl Guest for System {}

fn notify_ready() -> Result<()> {
    use std::os::linux::net::SocketAddrExt as _;
    use std::os::unix::net::{SocketAddr, UnixDatagram};
    let Some(address) = std::env::var_os("NOTIFY_SOCKET").filter(|a| !a.is_empty()) else {
        return Ok(());
    };
    let client = UnixDatagram::unbound()?;
    match address.as_bytes().strip_prefix(b"@") {
        Some(name) => client.connect_addr(&SocketAddr::from_abstract_name(name)?)?,
        None => client.connect(&address)?,
    }
    client.send(b"READY=1")?;
    Ok(())
}

/// `idle`: no installer transient unit and no `tilo-install` process.
/// Independent of the replaceable helper's flock protocol: older helpers
/// must not be refreshed while an installer launcher or backend is alive.
pub fn idle(guest: &dyn Guest) -> Result<()> {
    let units = guest.command(&[
        "systemctl",
        "list-units",
        "--all",
        "--plain",
        "--no-legend",
        "tilo-install-*.service",
    ])?;
    for row in tilo_pycompat::splitlines(&units) {
        let fields = crate::py::split(row);
        require(
            fields.len() >= 4
                && !matches!(
                    fields.get(2).copied(),
                    Some("active" | "activating" | "deactivating")
                ),
            "installer transient unit is running; code refresh refused",
        )?;
    }
    let proc_root = guest.proc_root();
    let Ok(entries) = fs::read_dir(&proc_root) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        if !entry
            .file_name()
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_digit)
        {
            continue;
        }
        let arguments = match fs::read(entry.path().join("cmdline")) {
            Ok(arguments) => arguments,
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::NotFound | ErrorKind::PermissionDenied | ErrorKind::NotADirectory
                ) || error.raw_os_error() == Some(libc::ESRCH) =>
            {
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        let running = arguments
            .split(|byte| *byte == 0)
            .filter(|argument| !argument.is_empty())
            .any(|argument| {
                Path::new(OsStr::from_bytes(argument)).file_name()
                    == Some(OsStr::new("tilo-install"))
            });
        require(
            !running,
            "installer helper process is running; code refresh refused",
        )?;
    }
    Ok(())
}

fn marker_text(value: &Value) -> String {
    format!("{}\n", Dumps::SORTED.dumps(value))
}

fn with_state(receipt: &Value, state: &str) -> Value {
    let mut value = receipt.clone();
    if let Some(map) = value.as_object_mut() {
        map.insert("state".into(), Value::String(state.into()));
    }
    value
}

fn is_generation(name: &OsStr) -> bool {
    let name = name.as_bytes();
    name.len() == 64
        && name
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
}

/// `apply`: stage every file, then bind each over its helper read-only,
/// recording the transaction so a backend never runs on a partial view.
///
/// # Errors
/// A gate, lock or idleness refusal, an earlier incomplete or repeated
/// generation, an unsafe destination, or a failed mount (rolled back).
pub fn apply(guest: &dyn Guest, files: &Files, receipt: &Value) -> Result<Value> {
    guest.gate()?;
    // Held until this returns, so the backend's own flock waits for the
    // whole transaction.
    let _lock = match advisory_lock::FileLock::try_exclusive(guest.lock()?) {
        Ok(lock) => lock,
        Err(advisory_lock::TryLockError::WouldBlock) => {
            return Err(Error::new(
                "installer backend is running; code refresh refused",
            ));
        }
        Err(advisory_lock::TryLockError::Error(error)) => return Err(error.into()),
    };
    idle(guest)?;
    let base = guest.base();
    let marker = base.join("active.json");
    let mut previous = None;
    if marker.is_file() {
        let value: Value = serde_json::from_slice(&fs::read(&marker)?)
            .map_err(|error| Error::new(error.to_string()))?;
        require(
            value.get("state").and_then(Value::as_str) == Some("applied"),
            "previous refresh is incomplete; restart this VM",
        )?;
        let mut without_state = value.clone();
        if let Some(map) = without_state.as_object_mut() {
            map.remove("state");
        }
        if crate::py::equal(&without_state, receipt) {
            return Ok(receipt.clone());
        }
        previous = Some(value);
    }
    let retained = fs::read_dir(&base)?
        .flatten()
        .filter(|entry| is_generation(&entry.file_name()))
        .count();
    require(
        retained < 32,
        "32 helper generations retained; restart the disposable installer VM",
    )?;
    let digest = receipt
        .get("sha256")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::new("'sha256'"))?;
    let generation = base.join(digest);
    require(
        !generation.exists() && !generation.is_symlink(),
        "this installer code generation was already attempted; restart this VM",
    )?;
    fs::DirBuilder::new().mode(0o700).create(&generation)?;
    let root = guest.root();
    let mut staged = Vec::new();
    // Validate and prepare ALL files before changing any executable view.
    for (relative, data) in files {
        let destination = root.join(relative);
        require(
            destination.is_file() && !destination.is_symlink(),
            "code refresh destination is not an existing regular helper",
        )?;
        let original = fs::metadata(&destination)?;
        let source = generation.join(destination.file_name().unwrap_or_default());
        fs::write(&source, data)?;
        fs::set_permissions(
            &source,
            fs::Permissions::from_mode(original.mode() & 0o7777),
        )?;
        std::os::unix::fs::chown(&source, Some(original.uid()), Some(original.gid()))?;
        guest.copy_label(&destination, &source)?;
        staged.push((source, destination));
    }
    idle(guest)?;
    fs::write(&marker, marker_text(&with_state(receipt, "applying")))?;
    fs::set_permissions(&marker, fs::Permissions::from_mode(0o600))?;
    let mut mounted = 0usize;
    if let Err(error) = bind_all(guest, &staged, &marker, receipt, &mut mounted) {
        let rollback = unbind(
            guest,
            staged.get(..mounted).unwrap_or_default(),
            &marker,
            previous.as_ref(),
        );
        if rollback.is_err() {
            // Do not let an old UI/backend lacking the new lock protocol
            // operate with a partial view. This is a disposable dev VM.
            guest.command(&["systemctl", "stop", "greetd.service"])?;
        }
        return Err(error);
    }
    Ok(receipt.clone())
}

fn bind_all(
    guest: &dyn Guest,
    staged: &[(PathBuf, PathBuf)],
    marker: &Path,
    receipt: &Value,
    mounted: &mut usize,
) -> Result<()> {
    for (source, destination) in staged {
        let destination = text(destination);
        guest.command(&["mount", "--bind", &text(source), &destination])?;
        *mounted = mounted.saturating_add(1);
        guest.command(&["mount", "-o", "remount,bind,ro", &destination])?;
    }
    fs::write(marker, marker_text(&with_state(receipt, "applied")))?;
    Ok(())
}

fn unbind(
    guest: &dyn Guest,
    mounted: &[(PathBuf, PathBuf)],
    marker: &Path,
    previous: Option<&Value>,
) -> Result<()> {
    for (_, destination) in mounted.iter().rev() {
        guest.command(&["umount", &text(destination)])?;
    }
    match previous {
        Some(previous) => fs::write(marker, marker_text(previous))?,
        None => fs::remove_file(marker)?,
    }
    Ok(())
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn lossy_tail(bytes: &[u8]) -> &[u8] {
    bytes
        .get(bytes.len().saturating_sub(4096)..)
        .unwrap_or(bytes)
}

/// `bytes.splitlines()`: `\n`, `\r` and `\r\n` end a line.
fn byte_lines(bytes: &[u8]) -> Vec<&[u8]> {
    let mut lines = Vec::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        let Some(at) = rest.iter().position(|b| *b == b'\n' || *b == b'\r') else {
            lines.push(rest);
            break;
        };
        lines.push(rest.get(..at).unwrap_or_default());
        let skip = if rest.get(at..at.saturating_add(2)) == Some(b"\r\n") {
            2
        } else {
            1
        };
        rest = rest.get(at.saturating_add(skip)..).unwrap_or_default();
    }
    lines
}

/// `backend_probe`: when the refreshed backend carries the probe, run it
/// through its real transient, enforcing path and require its row to name
/// the exact helper bytes; otherwise `None`.
///
/// # Errors
/// The probe failing, or a row that does not match.
pub fn backend_probe(guest: &dyn Guest, files: &Files) -> Result<Value> {
    let helper = files
        .iter()
        .find(|(path, _)| path == "usr/libexec/tilo-install")
        .map(|(_, data)| data.as_slice())
        .unwrap_or_default();
    if !helper
        .windows(PROBE_MARK.len())
        .any(|window| window == PROBE_MARK)
    {
        return Ok(Value::Null);
    }
    let mut arguments = vec!["/usr/libexec/tilo-install", "--dev-code-refresh-probe"];
    if guest.fixture_disk() {
        arguments.push("--cached-target");
    }
    let result = guest.run_probe(&arguments)?;
    if !(result.success() && result.stdout.len() <= 16384) {
        let mut tail = lossy_tail(&result.stdout).to_vec();
        tail.extend_from_slice(lossy_tail(&result.stderr));
        return Err(Error::new(format!(
            "refreshed backend execution probe failed (exit {}): {}",
            result.returncode(),
            String::from_utf8_lossy(&tail)
        )));
    }
    let mut rows = Vec::new();
    for line in byte_lines(&result.stdout) {
        if line.starts_with(b"{") {
            rows.push(
                serde_json::from_slice::<Value>(line)
                    .map_err(|error| Error::new(error.to_string()))?,
            );
        }
    }
    let expected = crate::py::sha256_hex(helper);
    let matches = |row: &Value| {
        let domain = match row.get("domain") {
            None => Some(""),
            Some(Value::String(domain)) => Some(domain.as_str()),
            Some(_) => None,
        };
        row.get("schema").and_then(Value::as_str) == Some("tilo.installer-code-backend-probe.v1")
            && row
                .get("transient")
                .is_some_and(|v| crate::py::equal(v, &json!(1)))
            && row.get("enforcing") == Some(&Value::Bool(true))
            && row.get("helper_sha256").and_then(Value::as_str) == Some(expected.as_str())
            && domain.is_some_and(|domain| {
                matches!(
                    domain.split(':').nth(2),
                    Some("init_t" | "unconfined_service_t")
                )
            })
    };
    match rows.as_slice() {
        [row] if row.is_object() && matches(row) => Ok(row.clone()),
        _ => Err(Error::new(
            "backend probe does not match the executed enforcing transient helper",
        )),
    }
}

fn exchange(guest: &dyn Guest, stream: &mut dyn Stream, ready_sent: &mut bool) -> Result<Value> {
    let (files, receipt) = if guest.bootstrap_handshake() {
        bootstrap_receive(stream)?
    } else {
        receive(stream)?
    };
    let applied = apply(guest, &files, &receipt)?;
    let backend = backend_probe(guest, &files)?;
    let response = json!({"ok": true, "receipt": applied, "runtime": {
        "schema": "tilo.installer-code-runtime-probe.v1", "backend": backend}});
    if !*ready_sent {
        guest.ready()?;
        *ready_sent = true;
    }
    Ok(response)
}

/// The daemon: gate once, then answer every packet the host sends, with
/// readiness after the first successful refresh. Host connections may
/// close between refreshes, so the port is reopened rather than spinning on
/// end of file or treating it as new code.
///
/// # Errors
/// The gate refusing, the port failing to open, or [`Guest::pause`]
/// ending the loop.
pub fn serve(guest: &dyn Guest) -> Result<()> {
    guest.gate()?;
    let mut ready_sent = false;
    loop {
        let mut stream = guest.open_port()?;
        let response = exchange(guest, stream.as_mut(), &mut ready_sent)
            .unwrap_or_else(|error| json!({"ok": false, "error": error.0}));
        let mut line = canonical(&response);
        line.push(b'\n');
        let _ = write_all(stream.as_mut(), &line);
        drop(stream);
        guest.pause()?;
    }
}

/// `tilo-vm-install-proof code-refresh-check STATE_DIR`: the installer
/// backend, holding the refresh lock, refuses to run over a refresh that
/// did not finish applying.
pub fn check_applied(base: &Path) -> Result<()> {
    let marker = base.join("active.json");
    if !marker.exists() {
        return Ok(());
    }
    let value: Value = serde_json::from_slice(&fs::read(&marker)?)
        .map_err(|error| Error::new(error.to_string()))?;
    require(
        value.get("state").and_then(Value::as_str) == Some("applied"),
        "installer code refresh is incomplete",
    )
}

/// `tilo-vm-install-proof code-refresh`: runs [`serve`] on the VM.
#[must_use]
pub fn main(guest: &dyn Guest) -> std::process::ExitCode {
    match serve(guest) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("tilo-vm-install-proof code-refresh: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
