//! Pure logic: commands, the base fingerprint, the catalogue, the signed
//! manifest and which of its parts fit a base.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::py;
use crate::{Error, Result};

pub const VERBS: [&str; 4] = ["install", "remove", "refresh", "status"];
pub const MANIFEST_TYPE: &str = "tilo.system-extension.sysext-manifest";
const FINGERPRINT_DOMAIN: &[u8] = b"tilo.base-packages.v1\n";

pub const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
pub const MAX_SIGNATURE_BYTES: usize = 4096;
/// A Rekor bundle carries the signature, the certificate and the log entry,
/// which holds the certificate again.
pub const MAX_BUNDLE_BYTES: usize = 64 * 1024;
pub const MAX_ARTIFACT_BYTES: u64 = 1024 * 1024 * 1024;
/// A prebuilt sysext image (format "sysext-raw") is one artifact, and large:
/// the Windows compatibility runtime is a few GiB.
const MAX_RAW_BYTES: u64 = 6 * 1024 * 1024 * 1024;
pub const RAW_FORMAT: &str = "sysext-raw";
pub const RAW_SUFFIX: &str = ".raw";
const FORMATS: [&str; 2] = ["rpm", RAW_FORMAT];
const MAX_ARTIFACTS: usize = 512;
pub const MAX_TEXT: usize = 300;

/// rpm's architecture, and systemd's name for it.
pub const ARCHITECTURES: [(&str, &str); 2] = [("x86_64", "x86-64"), ("aarch64", "arm64")];
const PART_STATES: [&str; 3] = ["available", "waiting", "unsupported"];

pub const OFFLINE_MESSAGE: &str =
    "Waiting for an internet connection. This installs automatically once this device is online.";
pub const UNPUBLISHED_MESSAGE: &str = "Not available for this version of Tilo yet. This device tries again every day and after updates.";
pub const UPDATE_MESSAGE: &str = "Waiting to be downloaded again for this version of Tilo.";
pub const OPENH264_WAITING_MESSAGE: &str = "OpenH264 is waiting for Cisco's codec library; the system compatibility stub cannot encode or decode video.";

/// What an install reports while it runs, in order. The desktop shows the
/// label and "step n of N" (status `progress`).
pub const PROGRESS_STEPS: [&str; 5] = [
    "Checking this device",
    "Fetching the signed manifest",
    "Downloading",
    "Verifying",
    "Building the extension",
];

// ---------------------------------------------------------------------------
// The shapes the original checked with regular expressions
// ---------------------------------------------------------------------------

/// `[a-z0-9][a-z0-9-]{0,62}`
pub fn is_id(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && text.len() <= 63
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// `[0-9a-f]{64}`
pub fn is_sha256(text: &str) -> bool {
    text.len() == 64
        && text
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

fn is_evr_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '~' | '^')
}

/// `[0-9]+:[A-Za-z0-9._+~^]+-[A-Za-z0-9._+~^]+`
fn is_evr(text: &str) -> bool {
    let Some((epoch, rest)) = text.split_once(':') else {
        return false;
    };
    let Some((version, release)) = rest.split_once('-') else {
        return false;
    };
    !epoch.is_empty()
        && epoch.chars().all(|c| c.is_ascii_digit())
        && !version.is_empty()
        && version.chars().all(is_evr_char)
        && !release.is_empty()
        && release.chars().all(is_evr_char)
}

/// `[A-Za-z0-9._+~^-]{1,255}`
fn is_package(text: &str) -> bool {
    (1..=255).contains(&text.len()) && text.chars().all(|c| is_evr_char(c) || c == '-')
}

/// `[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+`
fn is_host(text: &str) -> bool {
    let labels: Vec<&str> = text.split('.').collect();
    labels.len() >= 2
        && labels.iter().all(|label| {
            let edge =
                |c: Option<char>| c.is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
            edge(label.chars().next())
                && edge(label.chars().last())
                && label
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
}

/// Whether every character of a non-empty `text` satisfies `allowed`.
pub fn all_of(text: &str, allowed: impl Fn(char) -> bool) -> bool {
    !text.is_empty() && text.chars().all(allowed)
}

// ---------------------------------------------------------------------------
// Commands and the base
// ---------------------------------------------------------------------------

/// `("activate", "")` or `(verb, id)`; id "all" only for refresh.
///
/// # Errors
/// A usage error, reported with exit status 2.
pub fn parse_command(argv: &[String]) -> Result<(String, String)> {
    let [argument] = argv else {
        return Err(Error::pack(
            "expected exactly one argument: activate, refresh:all, live:<phase> or <verb>:<id>",
        ));
    };
    if argument == "activate" {
        return Ok(("activate".to_owned(), String::new()));
    }
    if let Some(phase) = argument.strip_prefix("live:") {
        if !matches!(phase, "system" | "session" | "status") {
            return Err(Error::pack("live takes system, session or status"));
        }
        return Ok(("live".to_owned(), phase.to_owned()));
    }
    let Some((verb, ident)) = argument
        .split_once(':')
        .filter(|(verb, _)| VERBS.contains(verb))
    else {
        return Err(Error::Pack(format!(
            "unknown command; expected one of {} as <verb>:<id>",
            VERBS.join(", ")
        )));
    };
    if ident == "all" {
        if verb != "refresh" {
            return Err(Error::pack("only refresh applies to all extensions"));
        }
        return Ok((verb.to_owned(), ident.to_owned()));
    }
    if !is_id(ident) {
        return Err(Error::pack("invalid extension id"));
    }
    Ok((verb.to_owned(), ident.to_owned()))
}

/// The base package fingerprint (see the system-extensions README).
///
/// `lines` are `rpm -qa --qf '%{NAME} %{EPOCHNUM}:%{VERSION}-%{RELEASE}.%{ARCH}\n'`
/// lines; gpg-pubkey entries are not packages and are left out.
pub fn base_packages_sha256<S: AsRef<str>>(lines: &[S]) -> String {
    let mut kept: Vec<String> = lines
        .iter()
        .map(|line| line.as_ref().trim_end_matches('\n'))
        .filter(|line| !line.is_empty() && !line.starts_with("gpg-pubkey "))
        .map(|line| format!("{line}\n"))
        .collect();
    kept.sort();
    let mut digest = Sha256::new();
    digest.update(FINGERPRINT_DOMAIN);
    for line in kept {
        digest.update(line.as_bytes());
    }
    hex::encode(digest.finalize())
}

/// name -> epochnum:version-release, from the same rpm -qa lines.
pub fn parse_installed<S: AsRef<str>>(lines: &[S]) -> BTreeMap<String, String> {
    let mut installed = BTreeMap::new();
    for line in lines {
        let fields = py::split(line.as_ref());
        let [name, evra] = fields.as_slice() else {
            continue;
        };
        if *name == "gpg-pubkey" {
            continue;
        }
        let evr = evra.rsplit_once('.').map_or("", |(evr, _)| evr);
        installed.insert((*name).to_owned(), evr.to_owned());
    }
    installed
}

/// The booted system a manifest has to fit.
#[derive(Debug, Clone)]
pub struct Base {
    pub os_id: String,
    pub version_id: String,
    pub rpm_arch: String,
    pub fingerprint: String,
    pub installed: BTreeMap<String, String>,
}

impl Base {
    /// systemd's name for the architecture.
    pub fn architecture(&self) -> &'static str {
        ARCHITECTURES
            .iter()
            .find(|(rpm, _)| *rpm == self.rpm_arch)
            .map_or("", |(_, systemd)| *systemd)
    }
}

pub fn parse_os_release(text: &str) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    for line in py::splitlines(text) {
        let line = py::strip(line);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        values.insert(
            py::strip(key).to_owned(),
            py::strip(value).trim_matches(['"', '\'']).to_owned(),
        );
    }
    values
}

// ---------------------------------------------------------------------------
// Catalogue and manifest
// ---------------------------------------------------------------------------

fn bounded(value: Option<&Value>, field: &str, limit: usize) -> Result<String> {
    match value {
        Some(Value::String(text))
            if py::char_len(text) <= limit && !text.chars().any(|c| c < ' ') =>
        {
            Ok(text.clone())
        }
        _ => Err(Error::Pack(format!(
            "manifest field {field} is not a short line of text"
        ))),
    }
}

fn expect_keys<'a>(
    value: &'a Value,
    required: &[&str],
    optional: &[&str],
    place: &str,
) -> Result<&'a Map<String, Value>> {
    let Value::Object(map) = value else {
        return Err(Error::Pack(format!("manifest {place} is not an object")));
    };
    let mut missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|key| !map.contains_key(*key))
        .collect();
    missing.sort_unstable();
    let mut unknown: Vec<&str> = map
        .keys()
        .map(String::as_str)
        .filter(|key| !required.contains(key) && !optional.contains(key))
        .collect();
    unknown.sort_unstable();
    if !missing.is_empty() {
        return Err(Error::Pack(format!(
            "manifest {place} lacks {}",
            missing.join(", ")
        )));
    }
    if !unknown.is_empty() {
        return Err(Error::Pack(format!(
            "manifest {place} has unknown fields {}",
            unknown.join(", ")
        )));
    }
    Ok(map)
}

fn field<'a>(map: &'a Map<String, Value>, key: &str) -> &'a Value {
    map.get(key).unwrap_or(&Value::Null)
}

/// The image-owned catalogue entry: what this image accepts for an id.
#[derive(Debug, Clone)]
pub struct Catalogue {
    pub id: String,
    pub kind: String,
    pub manifest: String,
    pub hosts: BTreeSet<String>,
    pub parts: Vec<String>,
    /// Display only, never trusted for anything: what the owner is told
    /// before opting in. The manifest's pinned sizes are what is enforced.
    pub download_bytes: u64,
    pub installed_bytes: u64,
}

impl Catalogue {
    /// # Errors
    /// An entry this helper cannot trust, worded for the journal.
    pub fn parse(text: &[u8], ident: &str) -> Result<Self> {
        let value: Value = serde_json::from_slice(text)
            .map_err(|_| Error::Pack(format!("catalogue entry {ident} is not JSON")))?;
        let map = expect_keys(
            &value,
            &["schema", "id", "kind", "manifest", "hosts", "parts"],
            &["size"],
            "catalogue",
        )?;
        let empty = Value::Object(Map::new());
        let size = expect_keys(
            map.get("size").unwrap_or(&empty),
            &[],
            &["download_bytes", "installed_bytes"],
            "catalogue size",
        )?;
        for (key, number) in size {
            if positive_int(number).is_none_or(|n| n > MAX_RAW_BYTES.saturating_mul(4)) {
                return Err(Error::Pack(format!(
                    "catalogue entry {ident} has an invalid size {key}"
                )));
            }
        }
        let size_of = |key: &str| size.get(key).and_then(Value::as_u64).unwrap_or(0);
        if !py::equals_one(field(map, "schema")) || field(map, "id").as_str() != Some(ident) {
            return Err(Error::Pack(format!(
                "catalogue entry {ident} has the wrong schema or id"
            )));
        }
        let manifest = match field(map, "manifest") {
            Value::String(manifest)
                if manifest.starts_with("https://")
                    && manifest.contains("{arch}")
                    && manifest.contains("{base}") =>
            {
                manifest.clone()
            }
            _ => {
                return Err(Error::Pack(format!(
                    "catalogue entry {ident} has an invalid manifest location"
                )));
            }
        };
        let hosts = match field(map, "hosts") {
            Value::Array(hosts)
                if !hosts.is_empty() && hosts.iter().all(|h| h.as_str().is_some_and(is_host)) =>
            {
                hosts
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            }
            _ => {
                return Err(Error::Pack(format!(
                    "catalogue entry {ident} has invalid hosts"
                )));
            }
        };
        let parts: Vec<String> = match field(map, "parts") {
            Value::Array(parts)
                if !parts.is_empty() && parts.iter().all(|p| p.as_str().is_some_and(is_id)) =>
            {
                parts
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            }
            _ => {
                return Err(Error::Pack(format!(
                    "catalogue entry {ident} has invalid parts"
                )));
            }
        };
        if parts.iter().collect::<BTreeSet<_>>().len() != parts.len() {
            return Err(Error::Pack(format!(
                "catalogue entry {ident} has invalid parts"
            )));
        }
        let kind = bounded(map.get("kind"), "kind", MAX_TEXT)?;
        Ok(Self {
            id: ident.to_owned(),
            kind,
            manifest,
            hosts,
            parts,
            download_bytes: size_of("download_bytes"),
            installed_bytes: size_of("installed_bytes"),
        })
    }

    /// The manifest location for an rpm architecture and base fingerprint.
    pub fn manifest_url(&self, rpm_arch: &str, fingerprint: &str) -> String {
        self.manifest
            .replace("{arch}", rpm_arch)
            .replace("{base}", fingerprint)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    pub package: String,
    pub urls: Vec<String>,
    pub size: u64,
    pub sha256: String,
    pub publisher: String,
    pub license: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub name: String,
    pub state: String,
    pub message: String,
    pub requires_base: BTreeMap<String, String>,
    pub artifacts: Vec<Artifact>,
    /// "rpm", or "sysext-raw" for a prebuilt image.
    pub format: String,
}

impl Part {
    /// Whether this part is one prebuilt sysext image rather than RPMs.
    pub fn is_raw(&self) -> bool {
        self.format == RAW_FORMAT
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    pub id: String,
    pub revision: u64,
    /// The base the manifest pins, as signed.
    pub base: Map<String, Value>,
    pub parts: Vec<Part>,
}

impl Manifest {
    pub fn part(&self, name: &str) -> Option<&Part> {
        self.parts.iter().find(|p| p.name == name)
    }

    /// The pinned base fingerprint, when it is a string.
    pub fn packages_sha256(&self) -> Option<&str> {
        self.base.get("packages_sha256").and_then(Value::as_str)
    }

    /// The four base keys every record and status repeats.
    pub fn base_summary(&self) -> Value {
        let mut summary = Map::new();
        for key in ["os_id", "version_id", "architecture", "packages_sha256"] {
            summary.insert(
                key.to_owned(),
                self.base.get(key).cloned().unwrap_or(Value::Null),
            );
        }
        Value::Object(summary)
    }
}

/// An artifact URL the catalogue allows, unchanged.
///
/// # Errors
/// `Pack` for a URL the image does not allow; `Value` for one that does not
/// split (an invalid port or unbalanced brackets).
pub fn check_url(url: &Value, hosts: &BTreeSet<String>) -> Result<String> {
    let url = match url {
        Value::String(url) if py::char_len(url) <= 2048 => url,
        _ => return Err(Error::pack("manifest artifact URL is not a string")),
    };
    let value_error = |error: py::UrlValueError| Error::Value(error.0);
    let parsed = py::urlsplit(url).map_err(value_error)?;
    let plain = parsed.scheme == "https"
        && parsed.username().is_none_or(str::is_empty)
        && parsed.password().is_none_or(str::is_empty)
        && matches!(parsed.port().map_err(value_error)?, None | Some(443));
    if !plain {
        return Err(Error::Pack(format!(
            "manifest artifact URL is not plain HTTPS: {}",
            py::truncate(url, 200)
        )));
    }
    let hostname = parsed.hostname();
    if !hostname.as_ref().is_some_and(|host| hosts.contains(host)) {
        return Err(Error::Pack(format!(
            "manifest artifact host {} is not allowed for this extension",
            hostname.as_deref().unwrap_or("None")
        )));
    }
    let climbs = py::unquote(&parsed.path)
        .split('/')
        .any(|part| part == "..");
    if !parsed.query.is_empty() || !parsed.fragment.is_empty() || climbs {
        return Err(Error::Pack(format!(
            "manifest artifact URL has an unexpected form: {}",
            py::truncate(url, 200)
        )));
    }
    Ok(url.clone())
}

fn positive_int(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64().filter(|n| *n > 0),
        _ => None,
    }
}

/// Validate signed manifest bytes against the catalogue and this base.
///
/// Only call this on bytes whose signature has been verified. `exact` requires
/// the manifest to be for exactly this base; without it (a manifest kept from
/// the deployment before an update) the OS release and architecture must still
/// match, and each part is then checked on its own (`manifest_part_fits`).
///
/// # Errors
/// The first way the manifest does not fit, worded for the owner.
pub fn parse_manifest(
    data: &[u8],
    catalogue: &Catalogue,
    base: &Base,
    exact: bool,
) -> Result<Manifest> {
    let data = data.strip_prefix(b"\xef\xbb\xbf").unwrap_or(data);
    let value: Value =
        serde_json::from_slice(data).map_err(|_| Error::pack("the signed manifest is not JSON"))?;
    let map = expect_keys(
        &value,
        &[
            "type", "schema", "id", "revision", "created", "base", "parts",
        ],
        &[],
        "",
    )?;
    if field(map, "type").as_str() != Some(MANIFEST_TYPE) || !py::equals_one(field(map, "schema")) {
        return Err(Error::pack(
            "the signed manifest has an unsupported type or schema",
        ));
    }
    if field(map, "id").as_str() != Some(catalogue.id.as_str()) {
        return Err(Error::pack("the signed manifest is for another extension"));
    }
    let revision = positive_int(field(map, "revision"))
        .ok_or_else(|| Error::pack("the signed manifest has an invalid revision"))?;
    bounded(map.get("created"), "created", MAX_TEXT)?;
    let pinned = expect_keys(
        field(map, "base"),
        &["os_id", "version_id", "architecture", "packages_sha256"],
        &["image"],
        "base",
    )?;
    let expected = [
        ("os_id", &base.os_id),
        ("version_id", &base.version_id),
        ("architecture", &base.rpm_arch),
        ("packages_sha256", &base.fingerprint),
    ];
    for (key, mine) in expected {
        if !exact && key == "packages_sha256" {
            continue;
        }
        let theirs = field(pinned, key);
        if theirs.as_str() != Some(mine.as_str()) {
            return Err(Error::Pack(format!(
                "the signed manifest is for another base ({key} {}, this device has {mine})",
                py::truncate(&py::str_of(theirs), 80)
            )));
        }
    }
    if pinned.contains_key("image") {
        bounded(pinned.get("image"), "base.image", MAX_TEXT)?;
    }
    let raw_parts = match field(map, "parts") {
        Value::Array(parts) if !parts.is_empty() => parts,
        _ => return Err(Error::pack("the signed manifest has no parts")),
    };
    let mut parts = Vec::new();
    let mut seen = BTreeSet::new();
    let mut total: u64 = 0;
    for raw in raw_parts {
        let (part, size) = parse_part(raw, catalogue, &mut seen)?;
        total = total.saturating_add(size);
        parts.push(part);
    }
    if total > MAX_ARTIFACT_BYTES.saturating_mul(4).max(MAX_RAW_BYTES) {
        return Err(Error::pack(
            "the signed manifest is larger than any extension may be",
        ));
    }
    Ok(Manifest {
        id: catalogue.id.clone(),
        revision,
        base: pinned.clone(),
        parts,
    })
}

/// One part of a manifest, and the bytes its artifacts add up to.
fn parse_part(
    raw: &Value,
    catalogue: &Catalogue,
    seen: &mut BTreeSet<String>,
) -> Result<(Part, u64)> {
    let raw = expect_keys(
        raw,
        &["name", "format", "state", "requires_base", "artifacts"],
        &["message"],
        "part",
    )?;
    let name_value = field(raw, "name");
    let Some(name) = name_value
        .as_str()
        .filter(|name| catalogue.parts.iter().any(|p| p == name))
    else {
        return Err(Error::Pack(format!(
            "the signed manifest names a part this image does not allow: {}",
            py::truncate(&py::str_of(name_value), 64)
        )));
    };
    if !seen.insert(name.to_owned()) {
        return Err(Error::Pack(format!(
            "the signed manifest names part {name} twice"
        )));
    }
    let Some(format) = field(raw, "format")
        .as_str()
        .filter(|f| FORMATS.contains(f))
    else {
        return Err(Error::Pack(format!(
            "part {name} has an unsupported format"
        )));
    };
    let Some(state) = field(raw, "state")
        .as_str()
        .filter(|s| PART_STATES.contains(s))
    else {
        return Err(Error::Pack(format!("part {name} has an unknown state")));
    };
    let requires = match field(raw, "requires_base") {
        Value::Object(requires)
            if requires
                .iter()
                .all(|(k, v)| is_package(k) && v.as_str().is_some_and(is_evr)) =>
        {
            requires
                .iter()
                .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned())))
                .collect::<BTreeMap<_, _>>()
        }
        _ => {
            return Err(Error::Pack(format!(
                "part {name} has invalid base requirements"
            )));
        }
    };
    let raw_artifacts = match field(raw, "artifacts") {
        Value::Array(items) if items.len() <= MAX_ARTIFACTS => items,
        _ => {
            return Err(Error::Pack(format!(
                "part {name} has an invalid artifact list"
            )));
        }
    };
    if (state == "available") == raw_artifacts.is_empty() {
        return Err(Error::Pack(format!(
            "part {name}: only an available part has artifacts"
        )));
    }
    if format == RAW_FORMAT && raw_artifacts.len() > 1 {
        return Err(Error::Pack(format!(
            "part {name}: a sysext image part is exactly one artifact"
        )));
    }
    let size_limit = if format == RAW_FORMAT {
        MAX_RAW_BYTES
    } else {
        MAX_ARTIFACT_BYTES
    };
    let mut artifacts = Vec::new();
    let mut total: u64 = 0;
    for item in raw_artifacts {
        let artifact = parse_artifact(item, name, &catalogue.hosts, size_limit)?;
        total = total.saturating_add(artifact.size);
        artifacts.push(artifact);
    }
    let message = match raw.get("message") {
        None => String::new(),
        present => bounded(present, "message", MAX_TEXT)?,
    };
    Ok((
        Part {
            name: name.to_owned(),
            state: state.to_owned(),
            message,
            requires_base: requires,
            artifacts,
            format: format.to_owned(),
        },
        total,
    ))
}

fn parse_artifact(
    item: &Value,
    part: &str,
    hosts: &BTreeSet<String>,
    size_limit: u64,
) -> Result<Artifact> {
    let item = expect_keys(
        item,
        &["package", "urls", "size", "sha256", "publisher", "license"],
        &[],
        "artifact",
    )?;
    let Some(package) = field(item, "package").as_str().filter(|p| is_package(p)) else {
        return Err(Error::Pack(format!(
            "part {part} has an invalid package name"
        )));
    };
    let urls = match field(item, "urls") {
        Value::Array(urls) if !urls.is_empty() && urls.len() <= 8 => urls,
        _ => return Err(Error::Pack(format!("{package} has no URLs"))),
    };
    let Some(size) = positive_int(field(item, "size")).filter(|size| *size <= size_limit) else {
        return Err(Error::Pack(format!("{package} has an invalid size")));
    };
    let Some(sha256) = field(item, "sha256").as_str().filter(|s| is_sha256(s)) else {
        return Err(Error::Pack(format!("{package} has an invalid SHA-256")));
    };
    let urls = urls
        .iter()
        .map(|url| check_url(url, hosts))
        .collect::<Result<Vec<_>>>()?;
    Ok(Artifact {
        package: package.to_owned(),
        urls,
        size,
        sha256: sha256.to_owned(),
        publisher: bounded(item.get("publisher"), "publisher", MAX_TEXT)?,
        license: bounded(item.get("license"), "license", 2000)?,
    })
}

/// The name in an RPM name-version-release.arch.
pub fn package_name(nvra: &str) -> &str {
    let stem = nvra.rsplit_once('.').map_or(nvra, |(stem, _)| stem);
    let mut cut = stem;
    for _ in 0..2 {
        match cut.rsplit_once('-') {
            Some((head, _)) => cut = head,
            None => break,
        }
    }
    cut
}

/// Whether a part resolved for another base may be used on `base`.
///
/// Offline and exact: the same OS release and architecture, every base package
/// the part was built against at the same version, and none of its own
/// packages now shipped by the base itself.
pub fn manifest_part_fits(manifest: &Manifest, part: &Part, base: &Base) -> bool {
    if part.state != "available" || !codec_runtime_complete(part, base) {
        return false;
    }
    let pinned = |key: &str| manifest.base.get(key).and_then(Value::as_str);
    if (
        pinned("os_id"),
        pinned("version_id"),
        pinned("architecture"),
    ) != (
        Some(base.os_id.as_str()),
        Some(base.version_id.as_str()),
        Some(base.rpm_arch.as_str()),
    ) {
        return false;
    }
    if manifest.packages_sha256() == Some(base.fingerprint.as_str()) {
        return true;
    }
    if part
        .requires_base
        .iter()
        .any(|(name, evr)| base.installed.get(name) != Some(evr))
    {
        return false;
    }
    part.is_raw()
        || !part
            .artifacts
            .iter()
            .any(|a| base.installed.contains_key(package_name(&a.package)))
}

/// A plugin linked against noopenh264 is discoverable but cannot run.
///
/// Old signed parts can contain the plugin without Cisco's implementation.
/// The stub satisfies the SONAME dependency, not the codec capability. Check
/// exact-base parts too; authenticity alone does not make them usable.
pub fn codec_runtime_complete(part: &Part, base: &Base) -> bool {
    if part.is_raw() {
        return true;
    }
    let names: BTreeSet<&str> = part
        .artifacts
        .iter()
        .map(|a| package_name(&a.package))
        .collect();
    !names.contains("gstreamer1-plugin-openh264")
        || names.contains("openh264")
        || base.installed.contains_key("openh264")
}

/// The contents of usr/lib/extension-release.d/extension-release.<name>.
///
/// # Errors
/// A value or key that could change the file's meaning.
pub fn extension_release(
    os_id: &str,
    version_id: &str,
    architecture: &str,
    extra: &BTreeMap<String, String>,
) -> Result<String> {
    let safe =
        |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '~' | '^' | '+' | '-');
    for value in [os_id, version_id, architecture]
        .into_iter()
        .chain(extra.values().map(String::as_str))
    {
        if !all_of(value, safe) {
            return Err(Error::Pack(format!(
                "refusing unsafe extension-release value {}",
                py::repr_str(value)
            )));
        }
    }
    let mut lines = vec![
        format!("ID={os_id}"),
        format!("VERSION_ID={version_id}"),
        format!("ARCHITECTURE={architecture}"),
        "SYSEXT_SCOPE=system".to_owned(),
    ];
    for (key, value) in extra {
        let suffix_ok = key.strip_prefix("TILO_").is_some_and(|rest| {
            all_of(rest, |c| {
                c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'
            })
        });
        if !suffix_ok {
            return Err(Error::Pack(format!(
                "refusing extension-release key {}",
                py::repr_str(key)
            )));
        }
        lines.push(format!("{key}={value}"));
    }
    Ok(lines.join("\n") + "\n")
}

/// The user-visible state after an event.
///
/// A failed background refresh of a working extension leaves it installed
/// (the error is reported separately); a failed install or removal is
/// "failed".
pub fn next_state(current: &str, event: &str, installed: bool) -> &'static str {
    // An unknown current state counts as "not-installed", which only matters
    // for a failed removal.
    if event == "failed" && current == "removing" {
        return "failed";
    }
    match (event, installed) {
        ("install-started", _) | ("refresh-started", false) => "installing",
        ("refresh-started" | "offline" | "failed", true) | ("succeeded", _) => "installed",
        ("remove-started", _) => "removing",
        ("removed", _) => "not-installed",
        ("offline", false) => "waiting-for-network",
        _ => "failed",
    }
}

/// Whether `NetworkManager`'s cached connectivity allows downloading.
///
/// "limited" and "portal" have a link but no internet (or one behind a login
/// page); "unknown", an empty answer or no `NetworkManager` at all count as
/// online, so a system without `NetworkManager`'s connectivity check still tries.
pub fn is_online(connectivity: &str) -> bool {
    !matches!(
        py::strip(connectivity).to_lowercase().as_str(),
        "none" | "limited" | "portal"
    )
}

/// An rpm2archive member name ("./usr/...") as relative path components,
/// normalised as a POSIX path is (empty and "." components dropped).
pub fn archive_parts(name: &[u8]) -> Vec<&[u8]> {
    let name = name.strip_prefix(b"./").unwrap_or(name);
    let start = name.iter().position(|b| *b != b'/').unwrap_or(name.len());
    let name = name.get(start..).unwrap_or_default();
    name.split(|b| *b == b'/')
        .filter(|part| !part.is_empty() && *part != b".")
        .collect()
}

/// The in-tree path for an archive member, or `None` to drop it.
///
/// Only /usr is part of a system extension. Build-id links are debugging
/// metadata the base owns for its own files, and an rpm database would make
/// the base's look different. Anything that escapes the tree is rejected
/// outright rather than dropped.
///
/// # Errors
/// A member that climbs out of the tree.
pub fn safe_member(name: &[u8]) -> Result<Option<Vec<&[u8]>>> {
    let parts = archive_parts(name);
    if parts.contains(&b"..".as_slice()) {
        return Err(Error::Pack(format!(
            "archive member escapes the tree: {}",
            String::from_utf8_lossy(name)
        )));
    }
    if parts.first() != Some(&b"usr".as_slice()) {
        return Ok(None);
    }
    if matches!(parts.get(1..3), Some([b"lib", b".build-id" | b"sysimage"])) {
        return Ok(None);
    }
    Ok(Some(parts))
}

/// Private library directories a package gives precedence via ld.so.conf.d.
///
/// RPM Fusion's freeworld builds put a same-soname library (libgallium,
/// libavcodec) into a private directory and list it in /etc/ld.so.conf.d so it
/// wins over the base's copy. A system extension cannot change /etc; putting
/// the library at the base's own path in /usr/lib64 has the same effect.
///
/// Each directory comes back relative to the root, as normalised components.
pub fn relocation_dirs(ld_conf_text: &str) -> Vec<Vec<String>> {
    let mut dirs = Vec::new();
    for line in py::splitlines(ld_conf_text) {
        let line = py::strip(line.split('#').next().unwrap_or_default());
        // A path starting with exactly two slashes keeps them as its root, so
        // its parent is never /usr/lib64 or /usr/lib.
        if !line.starts_with('/') || (line.starts_with("//") && !line.starts_with("///")) {
            continue;
        }
        let parts: Vec<String> = line
            .split('/')
            .filter(|p| !p.is_empty() && *p != ".")
            .map(str::to_owned)
            .collect();
        let Some((_, parent)) = parts.split_last() else {
            continue;
        };
        if matches!(parent, [usr, lib] if usr == "usr" && (lib == "lib64" || lib == "lib")) {
            dirs.push(parts);
        }
    }
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_match_the_original_patterns() {
        assert!(
            is_id("media-codecs")
                && is_id("a")
                && !is_id("-a")
                && !is_id("A")
                && !is_id(&"a".repeat(64))
        );
        assert!(
            is_evr("0:1.26.5-1.fc44") && !is_evr("0:1-2-3") && !is_evr(":1-2") && !is_evr("0:1")
        );
        assert!(
            is_host("download1.rpmfusion.org")
                && !is_host("localhost")
                && !is_host("a-.b")
                && !is_host("a..b")
        );
        assert!(is_package("libavcodec-freeworld-7.1.1-1.fc44.x86_64") && !is_package("a b"));
    }

    #[test]
    fn package_names_and_members() {
        assert_eq!(
            package_name("libavcodec-freeworld-7.1.1-3.fc44.x86_64"),
            "libavcodec-freeworld"
        );
        assert_eq!(package_name("openh264"), "openh264");
        assert_eq!(
            safe_member(b"./usr/lib64/x.so").unwrap(),
            Some(vec![&b"usr"[..], b"lib64", b"x.so"])
        );
        assert_eq!(safe_member(b"./etc/x").unwrap(), None);
        assert_eq!(safe_member(b"./usr/lib/.build-id/aa").unwrap(), None);
        assert!(safe_member(b"./usr/../etc/x").is_err());
        assert_eq!(
            relocation_dirs(
                "# c\n/usr/lib64/mesa-freeworld\n//usr/lib64/x\n/opt/x\n/usr/lib64/a/./\n"
            ),
            vec![
                vec!["usr", "lib64", "mesa-freeworld"],
                vec!["usr", "lib64", "a"]
            ]
        );
    }
}
