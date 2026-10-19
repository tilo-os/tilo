//! The machine: state on disk, downloads, assembling parts, activation, and
//! the commands.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use advisory_lock::FileLock;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::host::{Header, Host, HttpError, set_times};
use crate::model::{
    self, ARCHITECTURES, Artifact, Base, Catalogue, MAX_BUNDLE_BYTES, MAX_MANIFEST_BYTES,
    MAX_SIGNATURE_BYTES, MAX_TEXT, Manifest, OFFLINE_MESSAGE, OPENH264_WAITING_MESSAGE,
    PROGRESS_STEPS, Part, RAW_FORMAT, RAW_SUFFIX, UNPUBLISHED_MESSAGE, UPDATE_MESSAGE,
    codec_runtime_complete, is_id, is_online, is_sha256, manifest_part_fits, next_state,
    parse_manifest,
};
use crate::py;
use crate::{At, Error, Result};

const CHUNK: usize = 1024 * 1024;
const RECORD_DIR: &str = "usr/share/tilo-extension";

// ---------------------------------------------------------------------------
// Where everything lives
// ---------------------------------------------------------------------------

/// Every path the helper uses, under one root (`/` on a device).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub root: PathBuf,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            root: PathBuf::from("/"),
        }
    }
}

impl Layout {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn at(&self, path: &str) -> PathBuf {
        self.root.join(path.trim_start_matches('/'))
    }

    /// Whether this is the running system rather than a root under it.
    pub fn is_host_root(&self) -> bool {
        self.root == Path::new("/")
    }

    pub fn state_root(&self) -> PathBuf {
        self.at("/var/lib/tilo/extensions")
    }

    pub fn state(&self, ident: &str) -> PathBuf {
        self.state_root().join(ident)
    }

    pub fn status(&self, ident: &str) -> PathBuf {
        self.state_root().join(format!("{ident}.json"))
    }

    pub fn desired(&self, ident: &str) -> PathBuf {
        self.state(ident).join("enabled")
    }

    pub fn accepted(&self, ident: &str) -> PathBuf {
        self.state(ident).join("accepted.json")
    }

    /// Accepted signed manifests, one directory per base fingerprint.
    pub fn bases(&self, ident: &str) -> PathBuf {
        self.state(ident).join("bases")
    }

    /// Verified artifacts, content-addressed, kept while the extension is on.
    pub fn objects(&self, ident: &str) -> PathBuf {
        self.state(ident).join("objects")
    }

    pub fn seed(&self, ident: &str) -> PathBuf {
        self.at("/etc/tilo/extensions/opt-in").join(ident)
    }

    pub fn seed_adopted(&self, ident: &str) -> PathBuf {
        self.state(ident).join("installer-seed-adopted")
    }

    pub fn cache(&self, ident: &str) -> PathBuf {
        self.at("/var/cache/tilo-extension").join(ident)
    }

    pub fn extensions(&self) -> PathBuf {
        self.at("/var/lib/extensions")
    }

    /// Every directory systemd-sysext merges from that is not sealed /usr.
    pub fn sysext_dirs(&self) -> [PathBuf; 4] {
        [
            self.at("/etc/extensions"),
            self.at("/run/extensions"),
            self.at("/var/lib/extensions"),
            self.at("/usr/local/lib/extensions"),
        ]
    }

    pub fn catalogue_dir(&self) -> PathBuf {
        self.at("/usr/share/tilo/system-extensions/sysext.d")
    }

    pub fn trust_key(&self) -> PathBuf {
        self.at("/usr/share/tilo/extensions.pub")
    }

    /// Tilo's offline release root. An image that carries it verifies
    /// manifests only through an operational certificate the root issued.
    pub fn release_root(&self) -> PathBuf {
        self.at("/usr/share/tilo/release/root.crt")
    }

    /// The transparency log's key, which dates a certified signature.
    pub fn rekor_key(&self) -> PathBuf {
        self.at("/usr/share/tilo/release/rekor.pub")
    }

    /// The revocation epoch and identity domain certificates must name.
    pub fn release_trust(&self) -> PathBuf {
        self.at("/usr/share/tilo/release/trust.env")
    }

    /// Whether manifests must be certified under the release root, rather
    /// than signed by the extension-manifest key alone.
    pub fn certified(&self) -> bool {
        self.release_root().symlink_metadata().is_ok()
    }

    /// The file published beside a manifest, and kept beside a verified one,
    /// that authenticates it: a Rekor bundle with the operational certificate
    /// on an image with the release root, a bare signature otherwise.
    pub fn signature_suffix(&self) -> &'static str {
        if self.certified() { ".bundle" } else { ".sig" }
    }

    pub fn os_release(&self) -> PathBuf {
        self.at("/usr/lib/os-release")
    }
}

// ---------------------------------------------------------------------------
// Small file helpers
// ---------------------------------------------------------------------------

fn now() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (year, month, day) = civil_from_days(seconds / 86_400);
    let rest = seconds % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// The proleptic Gregorian date `days` after 1970-01-01 (Howard Hinnant's
/// algorithm, for days on or after the epoch).
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days.wrapping_add(719_468);
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = doe
        .wrapping_sub(doe / 1460)
        .wrapping_add(doe / 36_524)
        .wrapping_sub(doe / 146_096)
        / 365;
    let doy = doe.wrapping_sub(
        yoe.wrapping_mul(365)
            .wrapping_add(yoe / 4)
            .wrapping_sub(yoe / 100),
    );
    let mp = doy.wrapping_mul(5).wrapping_add(2) / 153;
    let day = doy
        .wrapping_sub(mp.wrapping_mul(153).wrapping_add(2) / 5)
        .wrapping_add(1);
    let month = if mp < 10 {
        mp.wrapping_add(3)
    } else {
        mp.wrapping_sub(9)
    };
    let year = yoe
        .wrapping_add(era.wrapping_mul(400))
        .wrapping_add(u64::from(month <= 2));
    (year, month, day)
}

fn chmod(path: &Path, mode: u32) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).at(path)
}

/// `shutil.rmtree(path, ignore_errors=True)`: a symlink or a file is left
/// alone, as it always was.
fn rmtree(path: &Path) {
    if fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()) {
        let _ = fs::remove_dir_all(path);
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Write `content` next to `path` and rename it over, durably.
fn write_atomic(path: &Path, content: &[u8], mode: u32) -> Result<()> {
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::Builder::new()
        .prefix(&format!(".{}.", file_name(path)))
        .tempfile_in(directory)
        .at(directory)?;
    temporary.write_all(content).at(temporary.path())?;
    temporary.as_file().sync_all().at(temporary.path())?;
    chmod(temporary.path(), mode)?;
    temporary
        .persist(path)
        .map_err(|error| Error::os(&error.error, Some(path)))?;
    Ok(())
}

pub(crate) fn write_json(path: &Path, value: &Value, mode: u32) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).at(parent)?;
    }
    write_atomic(
        path,
        (py::Dumps::INDENTED.dumps(value) + "\n").as_bytes(),
        mode,
    )
}

fn read_json(path: &Path) -> Map<String, Value> {
    fs::read(path)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| match value {
            Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default()
}

fn write_text(path: &Path, text: &str, mode: u32) -> Result<()> {
    fs::write(path, text).at(path)?;
    chmod(path, mode)
}

/// [`write_text`], creating the parent directory first.
fn write_text_creating(path: &Path, text: &str, mode: u32) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).at(parent)?;
    }
    write_text(path, text, mode)
}

/// `int(value)` of a status field.
fn py_int(value: &Value) -> Result<i64> {
    match value {
        Value::Bool(flag) => Ok(i64::from(*flag)),
        Value::Number(number) => number
            .as_i64()
            .ok_or_else(|| Error::Value(format!("cannot convert {number} to an integer"))),
        Value::String(text) => py::strip(text)
            .replace('_', "")
            .parse::<i64>()
            .map_err(|_| {
                Error::Value(format!(
                    "invalid literal for int() with base 10: {}",
                    py::repr_str(text)
                ))
            }),
        other => Err(Error::Value(format!(
            "int() argument must be a string or a real number, not {}",
            py::repr(other)
        ))),
    }
}

fn changes<const N: usize>(pairs: [(&str, Value); N]) -> Map<String, Value> {
    pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
}

fn off(catalogue: &Catalogue) -> Value {
    Value::Object(
        catalogue
            .parts
            .iter()
            .map(|p| (p.clone(), json!("off")))
            .collect(),
    )
}

// ---------------------------------------------------------------------------
// Catalogue and status
// ---------------------------------------------------------------------------

fn catalogue_ids(layout: &Layout) -> Result<Vec<String>> {
    let directory = layout.catalogue_dir();
    if !directory.is_dir() {
        return Ok(Vec::new());
    }
    let mut ids = Vec::new();
    for entry in fs::read_dir(&directory).at(&directory)? {
        let name = entry.at(&directory)?.file_name();
        if let Some(stem) = name.to_str().and_then(|n| n.strip_suffix(".json"))
            && is_id(stem)
        {
            ids.push(stem.to_owned());
        }
    }
    ids.sort();
    Ok(ids)
}

fn load_catalogue(layout: &Layout, ident: &str) -> Result<Catalogue> {
    let path = layout.catalogue_dir().join(format!("{ident}.json"));
    let text =
        fs::read(&path).map_err(|_| Error::Pack(format!("this image has no extension {ident}")))?;
    Catalogue::parse(&text, ident)
}

/// The world-readable status file of one extension.
struct Status<'a> {
    layout: &'a Layout,
    ident: String,
    last_progress_write: Option<Instant>,
}

impl<'a> Status<'a> {
    fn new(layout: &'a Layout, ident: &str) -> Self {
        Self {
            layout,
            ident: ident.to_owned(),
            last_progress_write: None,
        }
    }

    fn read(&self) -> Map<String, Value> {
        read_json(&self.layout.status(&self.ident))
    }

    fn state(&self) -> Option<String> {
        self.read()
            .get("state")
            .and_then(Value::as_str)
            .map(str::to_owned)
    }

    fn write(&self, changes: Map<String, Value>) -> Result<()> {
        let mut status = self.read();
        status.extend(changes);
        if status.get("state").and_then(Value::as_str) != Some("installing") {
            status.remove("progress");
        }
        status.insert("schema".to_owned(), json!(1));
        status.insert("id".to_owned(), json!(self.ident));
        status.insert("updated".to_owned(), json!(now()));
        status.insert(
            "desired".to_owned(),
            json!(self.layout.desired(&self.ident).exists()),
        );
        let root = self.layout.state_root();
        fs::create_dir_all(&root).at(&root)?;
        chmod(&root, 0o755)?;
        write_json(
            &self.layout.status(&self.ident),
            &Value::Object(status),
            0o644,
        )
    }

    fn progress(&self, label: &str, extra: &[(&str, Value)]) -> Result<()> {
        let step = PROGRESS_STEPS
            .iter()
            .position(|l| *l == label)
            .unwrap_or_default()
            .saturating_add(1);
        let mut progress = changes([
            ("step", json!(step)),
            ("steps", json!(PROGRESS_STEPS.len())),
            ("label", json!(label)),
        ]);
        for (key, value) in extra {
            progress.insert((*key).to_owned(), value.clone());
        }
        self.write(changes([("progress", Value::Object(progress))]))
    }

    /// At most once a second: the desktop shows it, nothing depends on it.
    fn download_progress(&mut self, done: u64, total: u64) -> Result<()> {
        let moment = Instant::now();
        if done < total
            && self
                .last_progress_write
                .is_some_and(|last| moment.duration_since(last).as_secs_f64() < 1.0)
        {
            return Ok(());
        }
        self.last_progress_write = Some(moment);
        self.progress(
            "Downloading",
            &[("bytes", json!(done)), ("total", json!(total))],
        )
    }
}

fn locked(layout: &Layout, ident: &str) -> Result<FileLock> {
    let state = layout.state(ident);
    fs::create_dir_all(&state).at(&state)?;
    chmod(&state, 0o755)?;
    let path = state.join("lock");
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .at(&path)?;
    FileLock::exclusive(file).at(&path)
}

/// Turn the installer's opt-in into the owner's recorded choice, once.
fn adopt_installer_seed(layout: &Layout, ident: &str) -> Result<bool> {
    if !layout.seed(ident).is_file() || layout.seed_adopted(ident).exists() {
        return Ok(false);
    }
    let state = layout.state(ident);
    fs::create_dir_all(&state).at(&state)?;
    write_text(
        &layout.desired(ident),
        "owner opted in during installation\n",
        0o644,
    )?;
    write_text(&layout.seed_adopted(ident), &format!("{}\n", now()), 0o644)?;
    Ok(true)
}

fn probe_base(host: &dyn Host, layout: &Layout) -> Result<Base> {
    let path = layout.os_release();
    let bytes = fs::read(&path).at(&path)?;
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::Value(format!("'utf-8' codec can't decode: {error}")))?;
    let release = model::parse_os_release(&text);
    let os_id = release.get("ID").cloned().unwrap_or_default();
    let version_id = release.get("VERSION_ID").cloned().unwrap_or_default();
    let id_ok = model::all_of(&os_id, |c| {
        c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')
    });
    let version_ok = model::all_of(&version_id, |c| {
        c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')
    });
    if !id_ok || !version_ok {
        return Err(Error::pack(
            "this system's os-release has no usable ID and VERSION_ID",
        ));
    }
    let rpm_arch = host.machine();
    if !ARCHITECTURES.iter().any(|(rpm, _)| *rpm == rpm_arch) {
        return Err(Error::Pack(format!(
            "extensions are not built for {rpm_arch}"
        )));
    }
    let lines = host.rpm_query_all(layout)?;
    Ok(Base {
        os_id,
        version_id,
        rpm_arch,
        fingerprint: model::base_packages_sha256(&lines),
        installed: model::parse_installed(&lines),
    })
}

// ---------------------------------------------------------------------------
// Network and verified objects
// ---------------------------------------------------------------------------

fn fetch_small(host: &dyn Host, url: &str, limit: usize) -> Result<Vec<u8>> {
    let hostname = py::hostname_for_message(url);
    let mut response = match host.http_open(url, 0) {
        Ok(response) => response,
        Err(HttpError::Status { code: 404, .. }) => {
            return Err(Error::Unpublished(UNPUBLISHED_MESSAGE.to_owned()));
        }
        Err(HttpError::Status { code, .. }) => {
            return Err(Error::Pack(format!("{hostname} answered {code}")));
        }
        Err(HttpError::Unreachable(reason)) => {
            return Err(Error::Pack(format!(
                "could not reach {hostname}: <urlopen error {reason}>"
            )));
        }
    };
    let mut data = Vec::new();
    response
        .by_ref()
        .take(u64::try_from(limit.saturating_add(1)).unwrap_or(u64::MAX))
        .read_to_end(&mut data)
        .map_err(|error| {
            Error::Pack(format!(
                "could not reach {hostname}: {}",
                crate::os_message(&error, None)
            ))
        })?;
    if data.len() > limit {
        let name = url.rsplit('/').next().unwrap_or(url);
        return Err(Error::Pack(format!("{name} is larger than allowed")));
    }
    Ok(data)
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).at(path)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0u8; CHUNK];
    loop {
        let count = file.read(&mut buffer).at(path)?;
        let Some(chunk) = buffer.get(..count).filter(|c| !c.is_empty()) else {
            break;
        };
        digest.update(chunk);
    }
    Ok(hex::encode(digest.finalize()))
}

/// Verified artifacts, content-addressed, with resumable partial downloads.
struct Store {
    root: PathBuf,
    objects: PathBuf,
    partial: PathBuf,
}

impl Store {
    fn new(layout: &Layout, ident: &str) -> Self {
        let root = layout.cache(ident);
        let partial = root.join("partial");
        Self {
            root,
            objects: layout.objects(ident),
            partial,
        }
    }

    fn prepare(&self) -> Result<()> {
        let state = self
            .objects
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        for directory in [&self.root, &state, &self.objects, &self.partial] {
            fs::create_dir_all(directory).at(directory)?;
            chmod(directory, if *directory == state { 0o755 } else { 0o700 })?;
        }
        Ok(())
    }

    fn object(&self, artifact: &Artifact) -> PathBuf {
        self.objects.join(&artifact.sha256)
    }

    fn have(&self, artifact: &Artifact) -> Result<bool> {
        let path = self.object(artifact);
        if !path.is_file() {
            return Ok(false);
        }
        let size = fs::metadata(&path).at(&path)?.len();
        if size == artifact.size && sha256_file(&path)? == artifact.sha256 {
            return Ok(true);
        }
        fs::remove_file(&path).at(&path)?;
        Ok(false)
    }

    /// Download from the first URL that yields exactly the pinned bytes.
    fn fetch(
        &self,
        host: &dyn Host,
        artifact: &Artifact,
        report: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<PathBuf> {
        if self.have(artifact)? {
            return Ok(self.object(artifact));
        }
        let part = self.partial.join(&artifact.sha256);
        let mut problems = Vec::new();
        for url in &artifact.urls {
            if let Err(problem) = Self::download(host, url, &part, artifact, report)? {
                problems.push(problem);
                continue;
            }
            if sha256_file(&part)? == artifact.sha256 {
                promote(host, &part, &self.object(artifact))?;
                return Ok(self.object(artifact));
            }
            fs::remove_file(&part).at(&part)?;
            problems.push(format!(
                "{} from {} does not match the signed manifest",
                artifact.package,
                py::hostname_for_message(url)
            ));
        }
        let joined = problems.join("; ");
        let message = py::truncate(&joined, 600);
        Err(Error::Pack(if message.is_empty() {
            format!("{} could not be downloaded", artifact.package)
        } else {
            message.to_owned()
        }))
    }

    /// One attempt. The inner error is the URL's problem, recorded before the
    /// next URL is tried; the outer one stops the download altogether.
    fn download(
        host: &dyn Host,
        url: &str,
        part: &Path,
        artifact: &Artifact,
        report: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<Result<(), String>> {
        let hostname = py::hostname_for_message(url);
        let os = |error: &io::Error, path: &Path| {
            format!("{hostname}: {}", crate::os_message(error, Some(path)))
        };
        let mut offset = if part.exists() {
            match fs::metadata(part) {
                Ok(meta) => meta.len(),
                Err(error) => return Ok(Err(os(&error, part))),
            }
        } else {
            0
        };
        if offset >= artifact.size {
            if let Err(error) = fs::remove_file(part) {
                return Ok(Err(os(&error, part)));
            }
            offset = 0;
        }
        let mut response = match host.http_open(url, offset) {
            Ok(response) => response,
            Err(HttpError::Status { reason, .. }) => {
                return Ok(Err(format!("{hostname}: {reason}")));
            }
            Err(HttpError::Unreachable(reason)) => return Ok(Err(format!("{hostname}: {reason}"))),
        };
        if offset > 0 && response.status() != 206 {
            offset = 0;
        }
        let opened = if offset > 0 {
            OpenOptions::new().append(true).create(true).open(part)
        } else {
            File::create(part)
        };
        let mut out = match opened {
            Ok(out) => out,
            Err(error) => return Ok(Err(os(&error, part))),
        };
        let mut done = offset;
        let mut buffer = vec![0u8; CHUNK];
        loop {
            let want = usize::try_from(artifact.size.saturating_sub(done).saturating_add(1))
                .unwrap_or(CHUNK)
                .min(CHUNK);
            let count = match buffer.get_mut(..want).map(|window| response.read(window)) {
                Some(Ok(count)) => count,
                Some(Err(error)) if error.kind() == io::ErrorKind::Interrupted => continue,
                Some(Err(error)) => {
                    return Ok(Err(format!(
                        "{hostname}: {}",
                        crate::os_message(&error, None)
                    )));
                }
                None => 0,
            };
            let Some(chunk) = buffer.get(..count).filter(|c| !c.is_empty()) else {
                break;
            };
            done = done.saturating_add(count as u64);
            if done > artifact.size {
                drop(out);
                if let Err(error) = fs::remove_file(part) {
                    return Ok(Err(os(&error, part)));
                }
                return Ok(Err(format!(
                    "{} is larger than the signed manifest says",
                    artifact.package
                )));
            }
            if let Err(error) = out.write_all(chunk) {
                return Ok(Err(os(&error, part)));
            }
            match report(count as u64) {
                Ok(()) => {}
                Err(Error::Os(message)) => return Ok(Err(format!("{hostname}: {message}"))),
                Err(Error::Pack(message) | Error::Unpublished(message)) => return Ok(Err(message)),
                Err(error) => return Err(error),
            }
        }
        if let Err(error) = out.flush().and_then(|()| out.sync_all()) {
            return Ok(Err(os(&error, part)));
        }
        drop(out);
        drop(response);
        match fs::metadata(part) {
            Ok(meta) if meta.len() != artifact.size => Ok(Err(format!(
                "{}: the download stopped early",
                artifact.package
            ))),
            Ok(_) => Ok(Ok(())),
            Err(error) => Ok(Err(os(&error, part))),
        }
    }

    /// Delete every verified object no kept manifest needs, and partials.
    fn keep_only(&self, wanted: &BTreeSet<String>) -> Result<()> {
        if self.objects.is_dir() {
            for entry in fs::read_dir(&self.objects).at(&self.objects)? {
                let path = entry.at(&self.objects)?.path();
                if !path
                    .file_name()
                    .and_then(OsStr::to_str)
                    .is_some_and(|name| wanted.contains(name))
                {
                    fs::remove_file(&path).at(&path)?;
                }
            }
        }
        rmtree(&self.partial);
        Ok(())
    }
}

/// Move a verified download into the object store.
///
/// The cache and the state directory are separate mounts inside the install
/// unit (`CacheDirectory=`, `StateDirectory=` under `ProtectSystem=strict`), so a
/// rename between them fails with EXDEV even though both live on /var. Copy
/// next to the object, then rename.
fn promote(host: &dyn Host, part: &Path, target: &Path) -> Result<()> {
    match host.replace(part, target) {
        Ok(()) => return Ok(()),
        Err(error) if error.raw_os_error() == Some(rustix::io::Errno::XDEV.raw_os_error()) => {}
        Err(error) => return Err(Error::os(&error, Some(part))),
    }
    let staging = target.with_file_name(format!(".{}.new", file_name(target)));
    let mut source = File::open(part).at(part)?;
    let mut out = File::create(&staging).at(&staging)?;
    io::copy(&mut source, &mut out).at(&staging)?;
    out.flush().at(&staging)?;
    out.sync_all().at(&staging)?;
    drop(out);
    host.replace(&staging, target).at(&staging)?;
    fs::remove_file(part).at(part)
}

// ---------------------------------------------------------------------------
// Assembling a part
// ---------------------------------------------------------------------------

fn relocate_private_libraries(tree: &Path, ld_conf: &[String]) -> Result<Vec<String>> {
    let mut moved = Vec::new();
    for text in ld_conf {
        for directory in model::relocation_dirs(text) {
            let source = directory
                .iter()
                .fold(tree.to_path_buf(), |path, part| path.join(part));
            if !source.is_dir() {
                continue;
            }
            let destination = source.parent().map(Path::to_path_buf).unwrap_or_default();
            let mut entries = fs::read_dir(&source)
                .at(&source)?
                .map(|entry| entry.map(|e| e.path()))
                .collect::<io::Result<Vec<_>>>()
                .at(&source)?;
            entries.sort();
            for entry in entries {
                let name = file_name(&entry);
                if !name.starts_with("lib") || !name.contains(".so") {
                    continue;
                }
                let target = destination.join(&name);
                if target.is_symlink() || target.exists() {
                    fs::remove_file(&target).at(&target)?;
                }
                fs::rename(&entry, &target).at(&entry)?;
                // Whatever pointed at the private copy (the VA drivers are
                // symlinks to libgallium) keeps resolving, to the moved file.
                symlink(format!("../{name}"), &entry).at(&entry)?;
                let relative = target.strip_prefix(tree).unwrap_or(&target);
                moved.push(format!("/{}", relative.display()));
            }
        }
    }
    Ok(moved)
}

fn record_path(tree: &Path, part: &str) -> PathBuf {
    tree.join(RECORD_DIR).join(format!("{part}.json"))
}

fn read_record(tree: &Path, part: &str) -> Map<String, Value> {
    read_json(&record_path(tree, part))
}

/// Where a part that is a prebuilt sysext image lives.
fn raw_path(layout: &Layout, name: &str) -> PathBuf {
    layout.extensions().join(format!("{name}{RAW_SUFFIX}"))
}

/// The record of a sysext image part. Dot-prefixed, so `foreign_extensions`
/// and systemd-sysext both ignore it.
fn raw_record_path(layout: &Layout, name: &str) -> PathBuf {
    layout.extensions().join(format!(".{name}.record.json"))
}

/// Whether a part is live, as a directory or as a sysext image.
fn live_exists(layout: &Layout, name: &str) -> bool {
    layout.extensions().join(name).exists() || raw_path(layout, name).exists()
}

/// What a live part (directory or sysext image) was assembled from.
fn live_record(layout: &Layout, name: &str) -> Map<String, Value> {
    if raw_path(layout, name).exists() {
        return read_json(&raw_record_path(layout, name));
    }
    read_record(&layout.extensions().join(name), name)
}

/// Take a part out of the sysext search path, in either form.
fn live_remove(layout: &Layout, name: &str) -> Result<()> {
    rmtree(&layout.extensions().join(name));
    for path in [raw_path(layout, name), raw_record_path(layout, name)] {
        remove_if_present(&path)?;
    }
    Ok(())
}

/// `unlink`, where a missing file is not an error.
fn remove_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(Error::os(&error, Some(path))),
        _ => Ok(()),
    }
}

/// Install a prebuilt, manifest-pinned sysext image as `<part>.raw`.
///
/// The bytes were pinned by the signed manifest and re-hashed by the caller
/// (`Store::have`); the image is linked (copied across filesystems) beside the
/// live one and renamed over it. systemd-sysext itself refuses an image whose
/// extension-release does not match this OS release and architecture.
fn build_raw_part(layout: &Layout, assembly: &Assembly<'_>) -> Result<()> {
    let (manifest, part) = (assembly.manifest, assembly.part);
    let [(artifact, path)] = assembly.files.as_slice() else {
        return Err(Error::Value(format!(
            "a sysext image part is exactly one artifact, not {}",
            assembly.files.len()
        )));
    };
    let extensions = layout.extensions();
    fs::create_dir_all(&extensions).at(&extensions)?;
    let live = raw_path(layout, &part.name);
    let staging = extensions.join(format!(".{}{RAW_SUFFIX}.new", part.name));
    remove_if_present(&staging)?;
    let installed = (|| {
        if fs::hard_link(path, &staging).is_err() {
            fs::copy(path, &staging).at(path)?;
        }
        chmod(&staging, 0o644)?;
        let record = json!({
            "schema": 1,
            "id": manifest.id,
            "part": part.name,
            "format": RAW_FORMAT,
            "revision": manifest.revision,
            "base": manifest.base_summary(),
            "requires_base": part.requires_base,
            "packages": [],
            "artifacts": [{"package": artifact.package, "sha256": artifact.sha256,
                           "publisher": artifact.publisher, "license": artifact.license}],
            "relocated": [],
        });
        fs::rename(&staging, &live).at(&staging)?;
        write_json(&raw_record_path(layout, &part.name), &record, 0o644)
    })();
    if let Err(error) = installed {
        remove_if_present(&staging)?;
        return Err(error);
    }
    rmtree(&extensions.join(&part.name));
    Ok(())
}

/// What one part is assembled from: the signed manifest, the part, and each
/// artifact with its verified object.
struct Assembly<'a> {
    manifest: &'a Manifest,
    part: &'a Part,
    files: Vec<(&'a Artifact, PathBuf)>,
}

/// Assemble a directory sysext next to the live one, then swap it in.
fn build_part(
    host: &dyn Host,
    layout: &Layout,
    base: &Base,
    assembly: &Assembly<'_>,
) -> Result<()> {
    let part = assembly.part;
    if part.is_raw() {
        return build_raw_part(layout, assembly);
    }
    if !codec_runtime_complete(part, base) {
        return Err(Error::pack(OPENH264_WAITING_MESSAGE));
    }
    let extensions = layout.extensions();
    fs::create_dir_all(&extensions).at(&extensions)?;
    let staging = extensions.join(format!(".{}.new", part.name));
    rmtree(&staging);
    fs::DirBuilder::new()
        .mode(0o755)
        .create(&staging)
        .at(&staging)?;
    if let Err(error) = fill_part(host, base, assembly, &staging) {
        rmtree(&staging);
        return Err(error);
    }
    let live = extensions.join(&part.name);
    let old = extensions.join(format!(".{}.old", part.name));
    rmtree(&old);
    if live.exists() {
        fs::rename(&live, &old).at(&live)?;
    }
    fs::rename(&staging, &live).at(&staging)?;
    rmtree(&old);
    Ok(())
}

fn fill_part(host: &dyn Host, base: &Base, assembly: &Assembly<'_>, staging: &Path) -> Result<()> {
    let (manifest, part) = (assembly.manifest, assembly.part);
    let mut ld_conf = Vec::new();
    let mut packages = Vec::new();
    for (artifact, path) in &assembly.files {
        let header = host.rpm_header(path)?;
        check_header(&header, artifact, base)?;
        packages.push(header.name);
        host.extract_rpm(path, staging, &mut ld_conf)?;
    }
    let relocated = relocate_private_libraries(staging, &ld_conf)?;
    let release_dir = staging.join("usr/lib/extension-release.d");
    fs::create_dir_all(&release_dir).at(&release_dir)?;
    let release = release_dir.join(format!("extension-release.{}", part.name));
    let extra = BTreeMap::from([
        ("TILO_EXTENSION".to_owned(), manifest.id.clone()),
        (
            "TILO_EXTENSION_REVISION".to_owned(),
            manifest.revision.to_string(),
        ),
        (
            "TILO_BASE_PACKAGES".to_owned(),
            manifest.packages_sha256().unwrap_or_default().to_owned(),
        ),
    ]);
    let text =
        model::extension_release(&base.os_id, &base.version_id, base.architecture(), &extra)?;
    write_text(&release, &text, 0o644)?;
    // Media processes rescan plugins when extension-release.d changes (names
    // and times); the time changes only with what was built.
    let mut digests: Vec<&str> = assembly
        .files
        .iter()
        .map(|(a, _)| a.sha256.as_str())
        .collect();
    digests.sort_unstable();
    let built_from = hex::encode(Sha256::digest(digests.concat().as_bytes()));
    let stamp = 1_000_000_000_i64
        .saturating_add(i64::from_str_radix(py::truncate(&built_from, 7), 16).unwrap_or(0));
    set_times(&release, stamp, 0, true)?;
    packages.sort();
    let record = json!({
        "schema": 1,
        "id": manifest.id,
        "part": part.name,
        "revision": manifest.revision,
        "base": manifest.base_summary(),
        "requires_base": part.requires_base,
        "packages": packages,
        "artifacts": assembly.files.iter().map(|(a, _)| json!({
            "package": a.package, "sha256": a.sha256, "publisher": a.publisher, "license": a.license,
        })).collect::<Vec<_>>(),
        "relocated": relocated,
    });
    let path = record_path(staging, &part.name);
    write_json(&path, &record, 0o644)?;
    set_times(&path, stamp, 0, true)?;
    for directory in [
        staging.join(RECORD_DIR),
        staging.join("usr/share"),
        staging.join("usr"),
    ] {
        chmod(&directory, 0o755)?;
    }
    host.relabel(staging)
}

fn check_header(header: &Header, artifact: &Artifact, base: &Base) -> Result<()> {
    if header.nvra != artifact.package {
        return Err(Error::Pack(format!(
            "{}: the file is {}",
            artifact.package, header.nvra
        )));
    }
    if base.installed.contains_key(&header.name) {
        return Err(Error::Pack(format!(
            "{} would replace a package of this system",
            header.nvra
        )));
    }
    Ok(())
}

/// What a live part was assembled from: revision and artifact digests.
type Selection = Option<(Value, Vec<Value>)>;

fn selection_of(record: &Map<String, Value>) -> Selection {
    if record.is_empty() {
        return None;
    }
    let mut digests: Vec<Value> = match record.get("artifacts") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|a| {
                a.as_object()
                    .map(|a| a.get("sha256").cloned().unwrap_or_else(|| json!("")))
            })
            .collect(),
        _ => Vec::new(),
    };
    digests.sort_by_key(ToString::to_string);
    Some((
        record.get("revision").cloned().unwrap_or_else(|| json!(0)),
        digests,
    ))
}

fn selections(layout: &Layout, catalogue: &Catalogue) -> BTreeMap<String, Selection> {
    catalogue
        .parts
        .iter()
        .map(|name| (name.clone(), selection_of(&live_record(layout, name))))
        .collect()
}

/// Refuse a manifest older than one already accepted for this base.
fn accept_revision(layout: &Layout, ident: &str, base: &Base, revision: u64) -> Result<()> {
    let path = layout.accepted(ident);
    let mut accepted = read_json(&path);
    let floor = accepted
        .get(&base.fingerprint)
        .cloned()
        .unwrap_or_else(|| json!(0));
    let older = match &floor {
        Value::Number(number) => number.as_u64().is_some_and(|f| revision < f),
        _ => false,
    };
    if older {
        return Err(Error::Pack(format!(
            "the published manifest (revision {revision}) is older than the one this device already accepted \
             ({floor}); refusing a downgrade"
        )));
    }
    accepted.insert(base.fingerprint.clone(), json!(revision));
    write_json(&path, &Value::Object(accepted), 0o644)
}

/// Offline: remember the choice and wait; this is not a failure.
fn waiting_for_network(status: &Status<'_>, installed: bool) -> Result<()> {
    if installed {
        return status.write(changes([("state", json!("installed"))]));
    }
    status.write(changes([
        ("state", json!(next_state("", "offline", false))),
        ("message", json!(OFFLINE_MESSAGE)),
        ("last_error", json!("")),
    ]))
}

fn installed_parts(layout: &Layout, catalogue: &Catalogue) -> bool {
    catalogue.parts.iter().any(|name| live_exists(layout, name))
}

/// A signed manifest and the exact bytes it was verified from.
struct Fetched {
    manifest: Manifest,
    data: Vec<u8>,
    signature: Vec<u8>,
}

/// Fetch, authenticate and validate the manifest for this base.
fn fetch_manifest(
    host: &dyn Host,
    layout: &Layout,
    catalogue: &Catalogue,
    base: &Base,
    status: &Status<'_>,
) -> Result<Fetched> {
    status.progress("Fetching the signed manifest", &[])?;
    let url = catalogue.manifest_url(&base.rpm_arch, &base.fingerprint);
    let data = fetch_small(host, &url, MAX_MANIFEST_BYTES)?;
    let limit = if layout.certified() {
        MAX_BUNDLE_BYTES
    } else {
        MAX_SIGNATURE_BYTES
    };
    let signature = fetch_small(host, &format!("{url}{}", layout.signature_suffix()), limit)?;
    host.verify_signature(
        layout,
        &data,
        &signature,
        &layout.cache(&catalogue.id).join("verify"),
    )?;
    let manifest = parse_manifest(&data, catalogue, base, true)?;
    accept_revision(layout, &catalogue.id, base, manifest.revision)?;
    Ok(Fetched {
        manifest,
        data,
        signature,
    })
}

/// Keep the signed bytes: every activation verifies them again.
fn keep_manifest(layout: &Layout, ident: &str, base: &Base, fetched: &Fetched) -> Result<()> {
    let directory = layout.bases(ident).join(&base.fingerprint);
    fs::create_dir_all(&directory).at(&directory)?;
    chmod(&layout.bases(ident), 0o755)?;
    chmod(&directory, 0o755)?;
    write_atomic(
        &directory.join(format!("manifest.json{}", layout.signature_suffix())),
        &fetched.signature,
        0o644,
    )?;
    write_atomic(&directory.join("manifest.json"), &fetched.data, 0o644)
}

/// Every kept manifest that still verifies, newest first.
///
/// A manifest for exactly this base comes first. Others (kept from the
/// deployments before an update, or after a rollback) follow by revision.
fn kept_manifests(
    host: &dyn Host,
    layout: &Layout,
    catalogue: &Catalogue,
    base: &Base,
) -> Result<Vec<Manifest>> {
    let directory = layout.bases(&catalogue.id);
    if !directory.is_dir() {
        return Ok(Vec::new());
    }
    let mut entries = fs::read_dir(&directory)
        .at(&directory)?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<io::Result<Vec<_>>>()
        .at(&directory)?;
    entries.sort();
    let mut found = Vec::new();
    for entry in entries {
        let name = file_name(&entry);
        if !is_sha256(&name) {
            continue;
        }
        let loaded = (|| {
            let manifest_path = entry.join("manifest.json");
            let data = fs::read(&manifest_path).at(&manifest_path)?;
            let signature_path = entry.join(format!("manifest.json{}", layout.signature_suffix()));
            let signature = fs::read(&signature_path).at(&signature_path)?;
            host.verify_signature(
                layout,
                &data,
                &signature,
                &layout.cache(&catalogue.id).join("verify"),
            )?;
            parse_manifest(&data, catalogue, base, false)
        })();
        let manifest = match loaded {
            Ok(manifest) => manifest,
            Err(error) if error.is_pack_or_os() => {
                eprintln!(
                    "tilo-extension: {}: ignoring the kept manifest for {}: {error}",
                    catalogue.id,
                    py::truncate(&name, 16)
                );
                continue;
            }
            Err(error) => return Err(error),
        };
        if manifest.packages_sha256() != Some(name.as_str()) {
            continue;
        }
        found.push(manifest);
    }
    let key = |m: &Manifest| {
        (
            m.packages_sha256() == Some(base.fingerprint.as_str()),
            m.revision,
        )
    };
    found.sort_by_key(|m| std::cmp::Reverse(key(m)));
    Ok(found)
}

/// Keep the manifests of the last few bases (rollback) and their objects.
fn prune(host: &dyn Host, layout: &Layout, catalogue: &Catalogue, base: &Base) -> Result<()> {
    let manifests = kept_manifests(host, layout, catalogue, base)?;
    let kept = manifests.get(..3).unwrap_or(&manifests);
    let names: BTreeSet<&str> = kept.iter().filter_map(Manifest::packages_sha256).collect();
    let directory = layout.bases(&catalogue.id);
    if directory.is_dir() {
        for entry in fs::read_dir(&directory).at(&directory)? {
            let path = entry.at(&directory)?.path();
            if !names.contains(file_name(&path).as_str()) {
                rmtree(&path);
            }
        }
    }
    let wanted: BTreeSet<String> = kept
        .iter()
        .flat_map(|m| m.parts.iter())
        .filter(|p| p.state == "available")
        .flat_map(|p| p.artifacts.iter().map(|a| a.sha256.clone()))
        .collect();
    Store::new(layout, &catalogue.id).keep_only(&wanted)
}

/// The part to merge on this base, or why there is none.
struct Choice<'a> {
    name: String,
    state: String,
    message: String,
    chosen: Option<(&'a Manifest, &'a Part)>,
}

impl<'a> Choice<'a> {
    fn without(name: &str, state: &str, message: &str) -> Self {
        Self {
            name: name.to_owned(),
            state: state.to_owned(),
            message: message.to_owned(),
            chosen: None,
        }
    }

    fn enabled(name: &str, manifest: &'a Manifest, part: &'a Part) -> Self {
        Self {
            name: name.to_owned(),
            state: "enabled".to_owned(),
            message: String::new(),
            chosen: Some((manifest, part)),
        }
    }
}

/// For each part: the manifest for exactly this base decides; failing that,
/// the newest kept manifest whose part still fits this base.
fn choose_parts<'a>(
    catalogue: &Catalogue,
    base: &Base,
    manifests: &'a [Manifest],
) -> Vec<Choice<'a>> {
    let exact = manifests
        .iter()
        .find(|m| m.packages_sha256() == Some(base.fingerprint.as_str()));
    let mut choices = Vec::new();
    for name in &catalogue.parts {
        if let Some(exact) = exact {
            choices.push(match exact.part(name) {
                None => Choice::without(name, "unsupported", ""),
                Some(part) if part.state == "available" => {
                    if codec_runtime_complete(part, base) {
                        Choice::enabled(name, exact, part)
                    } else {
                        Choice::without(name, "waiting", OPENH264_WAITING_MESSAGE)
                    }
                }
                Some(part) => Choice::without(name, &part.state, &part.message),
            });
            continue;
        }
        let fitting = manifests.iter().find_map(|m| {
            m.part(name)
                .filter(|p| manifest_part_fits(m, p, base))
                .map(|p| (m, p))
        });
        choices.push(match fitting {
            None => Choice::without(name, "waiting", UPDATE_MESSAGE),
            Some((manifest, part)) => Choice::enabled(name, manifest, part),
        });
    }
    choices
}

/// Anything systemd-sysext would merge that this helper did not build, or
/// (`signed`) did not link this activation after verifying it.
fn foreign_extensions(
    layout: &Layout,
    ours: &BTreeSet<String>,
    signed: &BTreeSet<String>,
) -> Result<Vec<String>> {
    let mut found = Vec::new();
    let extensions = layout.extensions();
    for directory in layout.sysext_dirs() {
        if !directory.is_dir() {
            continue;
        }
        let mut entries = fs::read_dir(&directory)
            .at(&directory)?
            .map(|entry| entry.map(|e| e.path()))
            .collect::<io::Result<Vec<_>>>()
            .at(&directory)?;
        entries.sort();
        for entry in entries {
            let name = file_name(&entry);
            if name.starts_with('.') {
                continue;
            }
            let our_image = name
                .strip_suffix(RAW_SUFFIX)
                .is_some_and(|stem| ours.contains(stem))
                && entry.is_file();
            // The live update's layer, linked from /run where only it links,
            // to where only it builds layers from the verified staged image.
            if entry == crate::live::link(layout) && crate::live::linked(layout) {
                continue;
            }
            // A signed layer this activation verified and linked.
            if signed.contains(&name)
                && entry == crate::signed::link(layout, &name)
                && entry.is_symlink()
            {
                continue;
            }
            if directory == extensions
                && !entry.is_symlink()
                && ((ours.contains(&name) && entry.is_dir()) || our_image)
            {
                continue;
            }
            found.push(entry.display().to_string());
        }
    }
    Ok(found)
}

// ---------------------------------------------------------------------------
// Activation
// ---------------------------------------------------------------------------

struct Activation {
    states: Map<String, Value>,
    changed: bool,
    messages: Vec<String>,
    errors: Vec<String>,
}

/// Rebuild the parts of one extension for the booted base.
fn activate_one(
    host: &dyn Host,
    layout: &Layout,
    catalogue: &Catalogue,
    base: &Base,
) -> Result<Activation> {
    let ident = &catalogue.id;
    let previous = selections(layout, catalogue);
    if !layout.desired(ident).exists() {
        for name in &catalogue.parts {
            live_remove(layout, name)?;
        }
        return Ok(Activation {
            states: catalogue
                .parts
                .iter()
                .map(|p| (p.clone(), json!("off")))
                .collect(),
            changed: previous.values().any(Option::is_some),
            messages: Vec::new(),
            errors: Vec::new(),
        });
    }
    let mut states = Map::new();
    let mut messages: Vec<String> = Vec::new();
    let mut errors = Vec::new();
    let store = Store::new(layout, ident);
    let manifests = kept_manifests(host, layout, catalogue, base)?;
    for choice in choose_parts(catalogue, base, &manifests) {
        let Some((manifest, part)) = choice.chosen else {
            live_remove(layout, &choice.name)?;
            states.insert(choice.name.clone(), json!(choice.state));
            if !choice.message.is_empty() {
                messages.push(choice.message.clone());
            }
            continue;
        };
        let mut missing = false;
        for artifact in &part.artifacts {
            missing |= !store.have(artifact)?;
        }
        if missing {
            live_remove(layout, &choice.name)?;
            states.insert(choice.name.clone(), json!("waiting"));
            messages.push(UPDATE_MESSAGE.to_owned());
            continue;
        }
        let files = part
            .artifacts
            .iter()
            .map(|a| (a, store.object(a)))
            .collect();
        match build_part(
            host,
            layout,
            base,
            &Assembly {
                manifest,
                part,
                files,
            },
        ) {
            Ok(()) => {
                states.insert(choice.name.clone(), json!("enabled"));
            }
            Err(error) if error.is_pack_or_os() => {
                live_remove(layout, &choice.name)?;
                states.insert(choice.name.clone(), json!("waiting"));
                errors.push(format!("{}: {error}", choice.name));
            }
            Err(error) => return Err(error),
        }
    }
    let mut unique: Vec<String> = Vec::new();
    for message in messages {
        if !unique.contains(&message) {
            unique.push(message);
        }
    }
    Ok(Activation {
        states,
        changed: selections(layout, catalogue) != previous,
        messages: unique,
        errors,
    })
}

/// Rebuild every part from verified bytes, then merge exactly those.
///
/// Runs at every boot before the desktop starts, and after every install,
/// refresh and removal. Nothing assembled earlier is trusted: what was written
/// to /var since then is replaced by what the kept signed manifests pin.
///
/// # Errors
/// Anything that stops activation as a whole; the exit status is 1 then.
pub fn activate(host: &dyn Host, layout: &Layout) -> Result<i32> {
    let mut catalogues = Vec::new();
    for ident in catalogue_ids(layout)? {
        match load_catalogue(layout, &ident) {
            Ok(catalogue) => catalogues.push(catalogue),
            Err(error) => eprintln!("tilo-extension: {error}"),
        }
    }
    let ours: BTreeSet<String> = catalogues
        .iter()
        .flat_map(|c| c.parts.iter().cloned())
        .collect();
    let any_part_present = || ours.iter().any(|name| live_exists(layout, name));
    let wanted = catalogues.iter().any(|c| layout.desired(&c.id).exists()) || any_part_present();
    // The live update's layer (crate::live): merged when linked, and merged
    // away when it was and no longer is.
    let live_linked = crate::live::linked(layout);
    let live_marker = crate::live::merged_marker(layout);
    let live_was_merged = live_marker.exists();
    // Signed layers (crate::signed): verified and linked, or unlinked when
    // they are no longer desired.
    let signed_wanted = crate::signed::wanted(layout);
    if !wanted && !live_linked && !live_was_merged && !signed_wanted {
        return Ok(0);
    }
    let mut outcome = Vec::new();
    if wanted {
        let base = match probe_base(host, layout) {
            Ok(base) => base,
            Err(error) => {
                // Without knowing the base nothing can be shown to fit it: merge none.
                for name in &ours {
                    live_remove(layout, name)?;
                }
                host.sysext_unmerge(layout)?;
                let _ = fs::remove_file(&live_marker);
                return Err(error);
            }
        };
        for catalogue in &catalogues {
            let lock = locked(layout, &catalogue.id)?;
            let result = activate_one(host, layout, catalogue, &base);
            drop(lock);
            outcome.push(result?);
        }
    }
    let mut signed = if signed_wanted {
        Some(crate::signed::activate(layout)?)
    } else {
        None
    };
    let signed_linked = signed
        .as_ref()
        .map(|signed| signed.linked.clone())
        .unwrap_or_default();
    if outcome.iter().any(|result| result.changed)
        || any_part_present()
        || live_linked
        || live_was_merged
        || signed.is_some()
    {
        let foreign = if host.sysext_masked()? {
            foreign_extensions(layout, &ours, &signed_linked)?
        } else {
            Vec::new()
        };
        if !foreign.is_empty() {
            // The sealed image merges only what this helper verified; something
            // else in the search path means /var or /etc was written behind it.
            for name in &ours {
                live_remove(layout, name)?;
            }
            let listed = foreign.join(", ");
            if let Some(signed) = signed.as_mut() {
                crate::signed::refuse_all(
                    layout,
                    signed,
                    &format!(
                        "not merged: unexpected system extensions are present ({})",
                        py::truncate(&listed, 200)
                    ),
                )?;
            }
            for catalogue in &catalogues {
                if !layout.desired(&catalogue.id).exists() {
                    continue;
                }
                Status::new(layout, &catalogue.id).write(changes([
                    ("state", json!("failed")),
                    ("parts", off(catalogue)),
                    (
                        "last_error",
                        json!(format!(
                            "Not merged: unexpected system extensions are present ({}).",
                            py::truncate(&listed, 200)
                        )),
                    ),
                ]))?;
            }
            host.sysext_unmerge(layout)?;
            let _ = fs::remove_file(&live_marker);
            return Ok(1);
        }
        host.sysext_refresh(layout)?;
        if live_linked {
            write_text_creating(&live_marker, "merged\n", 0o644)?;
        } else {
            let _ = fs::remove_file(&live_marker);
        }
    }
    let reported = report_activation(layout, &catalogues, outcome)?;
    // A refused signed layer fails the activation, so the unit says so.
    let refused = signed
        .as_ref()
        .is_some_and(crate::signed::Outcome::refused_any);
    Ok(if refused { 1 } else { reported })
}

/// What each extension's status says after an activation; 1 when a part
/// could not be built.
fn report_activation(
    layout: &Layout,
    catalogues: &[Catalogue],
    outcome: Vec<Activation>,
) -> Result<i32> {
    let mut failed = false;
    for (catalogue, result) in catalogues.iter().zip(outcome) {
        let status = Status::new(layout, &catalogue.id);
        let desired = layout.desired(&catalogue.id).exists();
        if status.read().is_empty() && !desired {
            continue;
        }
        let mut update = changes([
            ("parts", Value::Object(result.states.clone())),
            ("message", json!(result.messages.join(" "))),
        ]);
        if result.changed {
            let generation = py_int(
                &status
                    .read()
                    .get("generation")
                    .cloned()
                    .unwrap_or_else(|| json!(0)),
            )?;
            update.insert("generation".to_owned(), json!(generation.saturating_add(1)));
        }
        update.insert(
            "last_error".to_owned(),
            json!(py::truncate(&result.errors.join("; "), MAX_TEXT)),
        );
        if desired
            && status.state().as_deref() == Some("failed")
            && result
                .states
                .values()
                .any(|v| v.as_str() == Some("enabled"))
        {
            // Merged again: an earlier refusal no longer applies.
            update.insert("state".to_owned(), json!("installed"));
        }
        status.write(update)?;
        failed = failed || !result.errors.is_empty();
    }
    Ok(i32::from(failed))
}

/// Have PID 1 run the activation unit: it merges in the host's mount
/// namespace, which this sandboxed unit does not share.
fn request_activation(host: &dyn Host, layout: &Layout) -> Result<()> {
    if !layout.is_host_root() || !Path::new("/run/systemd/system").is_dir() {
        if activate(host, layout)? != 0 {
            return Err(Error::pack("activation failed"));
        }
        return Ok(());
    }
    host.start_activation_unit()
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Fetch what this base needs. Returns whether activation should run.
fn refresh(host: &dyn Host, layout: &Layout, ident: &str, explicit: bool) -> Result<bool> {
    let mut status = Status::new(layout, ident);
    let catalogue = load_catalogue(layout, ident)?;
    if !layout.desired(ident).exists() {
        status.write(changes([
            ("state", json!("not-installed")),
            ("parts", off(&catalogue)),
            ("message", json!("")),
            ("last_error", json!("")),
        ]))?;
        return Ok(false);
    }
    let previous = status.state().unwrap_or_else(|| "not-installed".to_owned());
    let installed = installed_parts(layout, &catalogue);
    if !is_online(&host.connectivity()) {
        waiting_for_network(&status, installed)?;
        return Ok(false);
    }
    let event = if explicit {
        "install-started"
    } else {
        "refresh-started"
    };
    status.write(changes([
        ("state", json!(next_state(&previous, event, installed))),
        ("last_error", json!("")),
    ]))?;
    if !installed {
        status.write(changes([("message", json!(""))]))?;
    }
    match fetch_and_verify(host, layout, &catalogue, &mut status) {
        Ok(needs_activation) => Ok(needs_activation),
        Err(error) => {
            let still_installed = installed_parts(layout, &catalogue);
            // A download that failed because the connection went away is the
            // same situation as starting offline, not an error to show the owner.
            if !matches!(error, Error::Unpublished(_)) && !is_online(&host.connectivity()) {
                waiting_for_network(&status, still_installed)?;
                return Ok(false);
            }
            let current = status.state().unwrap_or_default();
            status.write(changes([
                (
                    "state",
                    json!(next_state(&current, "failed", still_installed)),
                ),
                (
                    "last_error",
                    json!(py::truncate(&error.to_string(), MAX_TEXT)),
                ),
            ]))?;
            Err(error)
        }
    }
}

fn fetch_and_verify(
    host: &dyn Host,
    layout: &Layout,
    catalogue: &Catalogue,
    status: &mut Status<'_>,
) -> Result<bool> {
    let ident = &catalogue.id;
    let store = Store::new(layout, ident);
    status.progress("Checking this device", &[])?;
    let base = probe_base(host, layout)?;
    store.prepare()?;
    let fetched = fetch_manifest(host, layout, catalogue, &base, status)?;
    let manifest = &fetched.manifest;
    let wanted: Vec<&Part> = manifest
        .parts
        .iter()
        .filter(|p| p.state == "available")
        .collect();
    if wanted
        .iter()
        .any(|part| !codec_runtime_complete(part, &base))
    {
        return Err(Error::pack(OPENH264_WAITING_MESSAGE));
    }
    let mut total: u64 = 0;
    for artifact in wanted.iter().flat_map(|p| p.artifacts.iter()) {
        if !store.have(artifact)? {
            total = total.saturating_add(artifact.size);
        }
    }
    let mut done: u64 = 0;
    status.progress(
        "Downloading",
        &[("bytes", json!(0)), ("total", json!(total))],
    )?;
    for artifact in wanted.iter().flat_map(|p| p.artifacts.iter()) {
        let mut report = |count: u64| {
            done = done.saturating_add(count);
            status.download_progress(done, total)
        };
        store.fetch(host, artifact, &mut report)?;
    }
    status.progress("Verifying", &[])?;
    for part in &wanted {
        for artifact in &part.artifacts {
            if !store.have(artifact)? {
                return Err(Error::Pack(format!(
                    "{} changed after it was verified",
                    artifact.package
                )));
            }
            if part.is_raw() {
                continue;
            }
            let header = host.rpm_header(&store.object(artifact))?;
            check_header(&header, artifact, &base)?;
        }
    }
    keep_manifest(layout, ident, &base, &fetched)?;
    prune(host, layout, catalogue, &base)?;
    let current = selections(layout, catalogue);
    let unchanged = catalogue.parts.iter().all(|name| {
        let live: Vec<Value> = current
            .get(name)
            .cloned()
            .flatten()
            .map(|(_, d)| d)
            .unwrap_or_default();
        let mut target: Vec<Value> = wanted
            .iter()
            .find(|p| p.name == *name)
            .map(|p| p.artifacts.iter().map(|a| json!(a.sha256)).collect())
            .unwrap_or_default();
        target.sort_by_key(ToString::to_string);
        live == target
    });
    let packages: Map<String, Value> = wanted
        .iter()
        .map(|p| {
            (
                p.name.clone(),
                json!(
                    p.artifacts
                        .iter()
                        .map(|a| a.package.clone())
                        .collect::<Vec<_>>()
                ),
            )
        })
        .collect();
    status.write(changes([
        ("revision", json!(manifest.revision)),
        ("base", manifest.base_summary()),
        ("packages", Value::Object(packages)),
    ]))?;
    Ok(!unchanged || !status.read().get("parts").is_some_and(py::truthy))
}

/// After activation: the state the owner sees.
fn finish(layout: &Layout, ident: &str) -> Result<()> {
    let status = Status::new(layout, ident);
    let current = status.read();
    let state = current.get("state").and_then(Value::as_str);
    if !layout.desired(ident).exists() || !matches!(state, Some("installing" | "installed")) {
        return Ok(());
    }
    let enabled = current
        .get("parts")
        .and_then(Value::as_object)
        .is_some_and(|parts| parts.values().any(|v| v.as_str() == Some("enabled")));
    if enabled {
        return status.write(changes([("state", json!("installed"))]));
    }
    let reason = [current.get("last_error"), current.get("message")]
        .into_iter()
        .find(|v| v.is_some_and(py::truthy))
        .flatten()
        .cloned()
        .unwrap_or_else(|| json!("Nothing could be installed for this version of Tilo."));
    status.write(changes([
        ("state", json!("failed")),
        ("last_error", reason),
    ]))
}

fn remove(layout: &Layout, ident: &str) -> Result<()> {
    let status = Status::new(layout, ident);
    let catalogue = load_catalogue(layout, ident)?;
    status.write(changes([("state", json!("removing"))]))?;
    remove_if_present(&layout.desired(ident))?;
    for path in [
        layout.bases(ident),
        layout.objects(ident),
        layout.cache(ident),
    ] {
        rmtree(&path);
    }
    let extensions = layout.extensions();
    for name in &catalogue.parts {
        rmtree(&extensions.join(format!(".{name}.new")));
        rmtree(&extensions.join(format!(".{name}.old")));
        remove_if_present(&extensions.join(format!(".{name}{RAW_SUFFIX}.new")))?;
    }
    status.write(changes([
        ("packages", json!({})),
        ("message", json!("")),
        ("last_error", json!("")),
        ("revision", Value::Null),
        ("base", Value::Null),
    ]))
}

/// Run one command line (without the program name) against `layout`.
/// Returns the exit status: 0 done, 1 failed, 2 usage or not root.
pub fn main(host: &dyn Host, layout: &Layout, argv: &[String]) -> i32 {
    let (verb, ident) = match model::parse_command(argv) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("tilo-extension: {error}");
            return 2;
        }
    };
    if verb == "status" {
        if !layout
            .catalogue_dir()
            .join(format!("{ident}.json"))
            .is_file()
        {
            eprintln!("tilo-extension: this image has no extension {ident}");
            return 2;
        }
        let mut status = Status::new(layout, &ident).read();
        if status.is_empty() {
            status = changes([
                ("schema", json!(1)),
                ("id", json!(ident)),
                ("state", json!("not-installed")),
            ]);
        }
        println!("{}", py::Dumps::INDENTED.dumps(&Value::Object(status)));
        return 0;
    }
    if verb == "live" && ident == "status" {
        return crate::live::status(layout);
    }
    if host.euid() != 0 {
        eprintln!("tilo-extension: must run as root (start tilo-extension@<verb>:<id>.service)");
        return 2;
    }
    rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o022));
    if verb == "live" {
        let Some(phase) = crate::live::Phase::parse(&ident) else {
            eprintln!("tilo-extension: unknown live phase {ident}");
            return 2;
        };
        let lock = match locked(layout, crate::live::LIVE_NAME) {
            Ok(lock) => lock,
            Err(error) => {
                eprintln!("tilo-extension: {error}");
                return 1;
            }
        };
        let code = match crate::live::apply(host, &crate::System, layout, phase) {
            Ok(code) => code,
            Err(error) => {
                eprintln!("tilo-extension: live update: {error}");
                1
            }
        };
        drop(lock);
        return code;
    }
    let ids = match select(host, layout, &verb, &ident) {
        Ok(Some(ids)) => ids,
        Ok(None) => return 0,
        Err(Exit(code)) => return code,
    };
    match run_command(host, layout, &verb, &ids) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("tilo-extension: {error}");
            1
        }
    }
}

/// An exit status already reported.
struct Exit(i32);

/// The extensions a command applies to; `None` when it was `activate`, which
/// has already run.
fn select(
    host: &dyn Host,
    layout: &Layout,
    verb: &str,
    ident: &str,
) -> Result<Option<Vec<String>>, Exit> {
    let selected = (|| {
        if verb == "activate" {
            return activate(host, layout).map(Err);
        }
        if ident == "all" {
            let ids = catalogue_ids(layout)?
                .into_iter()
                .filter(|i| layout.desired(i).exists() || layout.seed(i).is_file())
                .collect();
            return Ok(Ok(ids));
        }
        load_catalogue(layout, ident)?;
        Ok(Ok(vec![ident.to_owned()]))
    })();
    match selected {
        Ok(Ok(ids)) => Ok(Some(ids)),
        Ok(Err(0)) => Ok(None),
        Ok(Err(code)) => Err(Exit(code)),
        Err(error) => {
            eprintln!("tilo-extension: {error}");
            Err(Exit(1))
        }
    }
}

fn run_command(host: &dyn Host, layout: &Layout, verb: &str, ids: &[String]) -> Result<i32> {
    let mut failed = false;
    let mut needs_activation = false;
    let mut touched = Vec::new();
    for one in ids {
        let outcome = (|| {
            let _lock = locked(layout, one)?;
            match verb {
                "install" => {
                    // The installer's seed is consumed by an explicit opt-in
                    // too, so it cannot re-enable the extension after a later
                    // Remove.
                    adopt_installer_seed(layout, one)?;
                    write_text(&layout.desired(one), "owner opted in\n", 0o644)?;
                    refresh(host, layout, one, true)
                }
                "refresh" => {
                    adopt_installer_seed(layout, one)?;
                    refresh(host, layout, one, false)
                }
                "remove" => remove(layout, one).map(|()| true),
                _ => Ok(false),
            }
        })();
        match outcome {
            Ok(needs) => {
                needs_activation |= needs;
                touched.push(one.clone());
            }
            Err(error) => {
                eprintln!("tilo-extension: {one}: {error}");
                failed = true;
            }
        }
    }
    if needs_activation {
        for one in &touched {
            let status = Status::new(layout, one);
            if status.state().as_deref() == Some("installing") {
                status.progress("Building the extension", &[])?;
            }
        }
        if let Err(error) = request_activation(host, layout) {
            eprintln!("tilo-extension: {error}");
            failed = true;
        }
    }
    for one in &touched {
        let _lock = locked(layout, one)?;
        if verb == "remove" {
            Status::new(layout, one).write(changes([("state", json!("not-installed"))]))?;
        } else {
            finish(layout, one)?;
        }
    }
    Ok(i32::from(failed))
}
