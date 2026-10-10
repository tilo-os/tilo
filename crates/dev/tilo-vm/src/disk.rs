//! `vm create` without `--image`: fetch the dev disk CI published to the
//! forge's generic packages (`os/dev/publish-dev-disk.sh`), resume and
//! verify the download, and cache it under the platform cache dir.
//!
//! ```text
//! <PACKAGES>/<tag>/tilo-vm-disk-<arch>.json   one architecture's manifest: its disks per format, sha256
//! <PACKAGES>/<tag>/<file>                      one disk (qcow2 or vhdx)
//! ```
//!
//! Each architecture's CI lane publishes on its own, so a tag can hold one
//! architecture before, or without, the other; a tag is offered to a host
//! once that host's manifest exists.
//!
//! HTTP goes through `curl`, present on every supported host; the token
//! travels in a header file, never on the command line.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::error::CliError;
use crate::hypervisor::{CommandLine, Runner};

pub const FORGE: &str = "code.tilo.so";
pub const PACKAGES: &str = "https://code.tilo.so/api/packages/tilo/generic/tilo-vm-disk";
/// Lists the package's published versions.
pub const VERSIONS: &str =
    "https://code.tilo.so/api/v1/packages/tilo?type=generic&q=tilo-vm-disk&limit=50";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskManifest {
    pub tag: String,
    #[serde(default)]
    pub source_revision: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub direct_boot: Vec<DirectBoot>,
    pub disks: Vec<Disk>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DirectBoot {
    pub arch: String,
    pub manifest: String,
    pub files: Vec<DirectBootFile>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DirectBootFile {
    pub file: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Disk {
    pub arch: String,
    pub format: String,
    pub file: String,
    pub url: String,
    pub sha256: String,
    #[serde(default)]
    pub size: u64,
}

/// How many of the newest main or release tags `vm create` without `--tag`
/// looks through for one that has this host's architecture.
pub const TAGS_SEARCHED: usize = 8;

/// The manifest `arch`'s lane publishes for `tag`.
pub fn manifest_url(tag: &str, arch: &str) -> String {
    format!("{PACKAGES}/{tag}/tilo-vm-disk-{arch}.json")
}

/// Lists the files of one published tag.
pub fn files_url(tag: &str) -> String {
    format!("https://code.tilo.so/api/v1/packages/tilo/generic/tilo-vm-disk/{tag}/files")
}

/// What a tag offers, as `<arch> <format>`, from its file listing: the disks
/// of every architecture whose manifest (the commit point) exists.
pub fn offered_in_files(tag: &str, files_json: &str) -> Vec<String> {
    #[derive(Deserialize)]
    struct File {
        name: String,
    }
    let files: Vec<File> = serde_json::from_str(files_json).unwrap_or_default();
    let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
    let mut offered = Vec::new();
    for name in &names {
        let Some(arch) = name
            .strip_prefix("tilo-vm-disk-")
            .and_then(|rest| rest.strip_suffix(".json"))
        else {
            continue;
        };
        for format in ["qcow2", "vhdx"] {
            if names.contains(&format!("tilo-vm-{tag}-{arch}.{format}").as_str()) {
                offered.push(format!("{arch} {format}"));
            }
        }
    }
    offered.sort();
    offered
}

/// The disk format a hypervisor imports.
pub fn format_for(hypervisor: &str) -> &'static str {
    if hypervisor == "hyperv" {
        "vhdx"
    } else {
        "qcow2"
    }
}

impl DiskManifest {
    pub fn pick(&self, arch: &str, format: &str) -> Option<&Disk> {
        self.disks
            .iter()
            .find(|d| d.arch == arch && d.format == format)
    }

    pub fn offered(&self) -> Vec<String> {
        self.disks
            .iter()
            .map(|d| format!("{} {}", d.arch, d.format))
            .collect()
    }

    /// Select only the bundle published in this disk manifest, with exact
    /// package URLs and file names. A bundle from another tag or architecture
    /// must never be paired with the selected disk.
    ///
    /// # Errors
    /// The manifest is incomplete or disagrees with the selected disk.
    pub fn direct_boot_for(
        &self,
        tag: &str,
        arch: &str,
        disk: &Disk,
    ) -> Result<DirectBoot, CliError> {
        let invalid = |detail: &str| {
            CliError::InvalidInput(format!(
                "published dev disk {tag} has an invalid direct boot bundle: {detail}"
            ))
        };
        let source = self
            .source_revision
            .as_deref()
            .ok_or_else(|| invalid("missing source revision"))?;
        if tag.is_empty()
            || tag.len() > 120
            || tag.contains("..")
            || !tag
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
            || self.tag != tag
            || source.len() != 40
            || !source
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(invalid(
                "tag or source revision does not match the selected disk",
            ));
        }
        if let Some(short) = tag.strip_prefix("main-").and_then(|s| s.get(..7))
            && Some(short) != source.get(..7)
        {
            return Err(invalid("main tag differs from the source revision"));
        }
        let package = format!("{PACKAGES}/{tag}/");
        if disk.arch != arch
            || disk.format != "qcow2"
            || disk.file != format!("tilo-vm-{tag}-{arch}.qcow2")
            || disk.url != format!("{package}{}", disk.file)
            || !valid_sha256(&disk.sha256)
        {
            return Err(invalid(
                "disk is not a verified qcow2 from the same package",
            ));
        }
        let mut bundles = self.direct_boot.iter().filter(|bundle| bundle.arch == arch);
        let bundle = bundles
            .next()
            .ok_or_else(|| invalid("missing architecture companion"))?;
        if bundles.next().is_some() {
            return Err(invalid("duplicate architecture companions"));
        }
        let expected = [
            format!("tilo-vm-{tag}-{arch}-direct-Image"),
            format!("tilo-vm-{tag}-{arch}-direct-initrd"),
            format!("tilo-vm-{tag}-{arch}-direct-boot.json"),
        ];
        if bundle.manifest != expected[2] || bundle.files.len() != expected.len() {
            return Err(invalid("incomplete companion file list"));
        }
        for name in &expected {
            let mut files = bundle.files.iter().filter(|file| file.file == *name);
            let file = files
                .next()
                .ok_or_else(|| invalid("missing companion file"))?;
            if files.next().is_some()
                || file.url != format!("{package}{name}")
                || !valid_sha256(&file.sha256)
                || file.size == 0
            {
                return Err(invalid("companion URL, checksum or size is invalid"));
            }
        }
        Ok(bundle.clone())
    }
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Cache each small companion by the manifest's checksum and verify it on
/// every use, including when a marker already exists.
///
/// # Errors
/// Download, file access or checksum validation fails.
pub fn fetch_direct_boot(
    runner: &mut dyn Runner,
    cache_dir: &Path,
    bundle: &DirectBoot,
    auth: &AuthHeader,
) -> Result<PathBuf, CliError> {
    for file in &bundle.files {
        let dest = cache_dir.join(&file.file);
        let disk = Disk {
            arch: bundle.arch.clone(),
            format: "companion".into(),
            file: file.file.clone(),
            url: file.url.clone(),
            sha256: file.sha256.clone(),
            size: file.size,
        };
        fetch(runner, &disk, &dest, auth)?;
        let actual = file_sha256(&dest)?;
        if actual != file.sha256 {
            // Preserve the failure, but make the next attempt re-download it.
            let _ = std::fs::remove_file(&dest);
            return Err(CliError::ChecksumMismatch {
                file: file.file.clone(),
                expected: file.sha256.clone(),
                actual,
            });
        }
    }
    Ok(cache_dir.join(&bundle.manifest))
}

/// A forge token and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub value: String,
    pub source: String,
}

/// `hosts.<host>.token` from an fgj `config.yaml`.
pub fn fgj_token(text: &str, host: &str) -> Option<String> {
    let mut host_indent: Option<usize> = None;
    for line in text.lines() {
        let indent = line.len().saturating_sub(line.trim_start().len());
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        match host_indent {
            None if trimmed == format!("{host}:") => host_indent = Some(indent),
            Some(outer) if indent <= outer => host_indent = None,
            Some(_) => {
                if let Some(value) = trimmed.strip_prefix("token:") {
                    return yaml_scalar(value);
                }
            }
            None => {}
        }
    }
    None
}

/// The `token` of the tea login whose `url` names `host`.
pub fn tea_token(text: &str, host: &str) -> Option<String> {
    let mut url: Option<String> = None;
    let mut token: Option<String> = None;
    let mut found: Option<String> = None;
    let mut flush = |url: &mut Option<String>, token: &mut Option<String>| {
        if found.is_none() && url.as_deref().is_some_and(|u| u.contains(host)) {
            found = token.take();
        }
        *url = None;
        *token = None;
    };
    for line in text.lines() {
        let trimmed = line.trim();
        let item = trimmed.strip_prefix("- ");
        if item.is_some() {
            flush(&mut url, &mut token);
        }
        let field = item.unwrap_or(trimmed);
        if let Some(value) = field.strip_prefix("url:") {
            url = yaml_scalar(value);
        } else if let Some(value) = field.strip_prefix("token:") {
            token = yaml_scalar(value);
        }
    }
    flush(&mut url, &mut token);
    found
}

fn yaml_scalar(value: &str) -> Option<String> {
    let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
    (!value.is_empty()).then(|| value.to_owned())
}

/// Where a token for the forge can be found: `FORGEJO_TOKEN`, then an fgj
/// or tea login for [`FORGE`].
pub fn find_token() -> Option<Token> {
    if let Ok(value) = std::env::var("FORGEJO_TOKEN")
        && !value.trim().is_empty()
    {
        return Some(Token {
            value: value.trim().to_owned(),
            source: "FORGEJO_TOKEN".into(),
        });
    }
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(home) = tilo_paths::home() {
        dirs.push(home.join(".config"));
    }
    if let Some(config) = tilo_paths::config_dir() {
        dirs.push(config);
    }
    for dir in &dirs {
        let fgj = dir.join("fgj/config.yaml");
        if let Some(value) = std::fs::read_to_string(&fgj)
            .ok()
            .and_then(|t| fgj_token(&t, FORGE))
        {
            return Some(Token {
                value,
                source: fgj.display().to_string(),
            });
        }
        let tea = dir.join("tea/config.yml");
        if let Some(value) = std::fs::read_to_string(&tea)
            .ok()
            .and_then(|t| tea_token(&t, FORGE))
        {
            return Some(Token {
                value,
                source: tea.display().to_string(),
            });
        }
    }
    None
}

/// A header file holding the token, removed on drop.
pub struct AuthHeader {
    file: Option<tempfile::NamedTempFile>,
}

impl AuthHeader {
    /// # Errors
    /// Temp file creation failures.
    pub fn new(token: Option<&Token>) -> Result<Self, CliError> {
        let Some(token) = token else {
            return Ok(Self { file: None });
        };
        let mut file =
            tempfile::NamedTempFile::new().map_err(CliError::io("create a header file"))?;
        std::io::Write::write_all(
            &mut file,
            format!("Authorization: token {}\n", token.value).as_bytes(),
        )
        .map_err(CliError::io("write a header file"))?;
        Ok(Self { file: Some(file) })
    }

    fn add(&self, command: CommandLine) -> CommandLine {
        match &self.file {
            Some(file) => command
                .arg("--header")
                .arg(format!("@{}", file.path().display())),
            None => command,
        }
    }
}

/// `curl` that prints the body, then the HTTP status on its own last line.
pub fn get_command(url: &str, auth: &AuthHeader) -> CommandLine {
    auth.add(CommandLine::new(
        "curl",
        &[
            "--silent",
            "--show-error",
            "--location",
            "--write-out",
            "\n%{http_code}",
        ],
    ))
    .arg(url)
}

/// `curl` that resumes a download into `part`, with a progress bar.
pub fn download_command(url: &str, part: &Path, auth: &AuthHeader) -> CommandLine {
    auth.add(CommandLine::new(
        "curl",
        &[
            "--fail",
            "--location",
            "--progress-bar",
            "--retry",
            "3",
            "--continue-at",
            "-",
        ],
    ))
    .arg("--output")
    .arg(part.display().to_string())
    .arg(url)
}

/// Split `curl --write-out '\n%{http_code}'` output into status and body.
pub fn split_status(stdout: &str) -> (u16, &str) {
    match stdout.rsplit_once('\n') {
        Some((body, code)) => (code.trim().parse().unwrap_or(0), body),
        None => (stdout.trim().parse().unwrap_or(0), ""),
    }
}

/// GET `url`; returns status and body.
///
/// # Errors
/// curl missing or unable to connect.
pub fn get(
    runner: &mut dyn Runner,
    url: &str,
    auth: &AuthHeader,
) -> Result<(u16, String), CliError> {
    let command = get_command(url, auth);
    let output = runner.output(&command).map_err(|e| match e {
        CliError::ToolMissing { .. } => CliError::ToolMissing {
            tool: "curl".into(),
            purpose: "download the dev disk".into(),
        },
        other => other,
    })?;
    let (status, body) = split_status(&output.stdout);
    if status == 0 {
        return Err(CliError::CommandFailed {
            command: format!("curl {url}"),
            status: output.status,
            stderr: output.stderr.trim().to_owned(),
        });
    }
    Ok((status, body.to_owned()))
}

/// The newest published tag of a main or release build, from the forge's
/// package list (newest first by creation time).
pub fn newest_tag(versions_json: &str) -> Option<String> {
    candidate_tags(versions_json).into_iter().next()
}

/// Published tags in the order `vm create` without `--tag` tries them: main
/// and release builds newest first; branch builds only when there are none.
pub fn candidate_tags(versions_json: &str) -> Vec<String> {
    #[derive(Deserialize)]
    struct Published {
        name: String,
        version: String,
        #[serde(default)]
        created_at: String,
    }
    let Ok(mut versions) = serde_json::from_str::<Vec<Published>>(versions_json) else {
        return Vec::new();
    };
    versions.retain(|v| v.name == "tilo-vm-disk");
    versions.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let channel = |v: &Published| {
        v.version.starts_with("main-") || v.version.starts_with(|c: char| c.is_ascii_digit())
    };
    let (channels, rest): (Vec<_>, Vec<_>) = versions.into_iter().partition(|v| channel(v));
    let chosen = if channels.is_empty() { rest } else { channels };
    chosen.into_iter().map(|v| v.version).collect()
}

/// Hex SHA-256 of a file.
///
/// # Errors
/// Read failures.
pub fn file_sha256(path: &Path) -> Result<String, CliError> {
    let mut file =
        std::fs::File::open(path).map_err(CliError::io(format!("read {}", path.display())))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1 << 20];
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(CliError::io(format!("read {}", path.display())))?;
        if n == 0 {
            break;
        }
        hasher.update(buffer.get(..n).unwrap_or_default());
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Where a disk is cached: `<cache>/tilo-vm/disks/<tag>/<file>`.
pub fn cache_path(tag: &str, file: &str) -> PathBuf {
    tilo_paths::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("tilo-vm")
        .join("disks")
        .join(tag)
        .join(file)
}

/// Is `path` already a verified copy of `disk`? A `<file>.sha256` marker
/// written after verification spares rehashing gigabytes every time.
pub fn cached(path: &Path, disk: &Disk) -> bool {
    let marker = path.with_extension(format!(
        "{}.sha256",
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
    ));
    let size_ok = std::fs::metadata(path).is_ok_and(|m| disk.size == 0 || m.len() == disk.size);
    size_ok && std::fs::read_to_string(marker).is_ok_and(|m| m.trim() == disk.sha256)
}

/// Download `disk` to `dest` (resuming a partial download), verify its
/// checksum, and mark it verified.
///
/// # Errors
/// Download failures and checksum mismatches.
pub fn fetch(
    runner: &mut dyn Runner,
    disk: &Disk,
    dest: &Path,
    auth: &AuthHeader,
) -> Result<(), CliError> {
    if cached(dest, disk) {
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(CliError::io(format!("create {}", parent.display())))?;
    }
    let part = dest.with_extension("part");
    let complete = disk.size > 0 && std::fs::metadata(&part).is_ok_and(|m| m.len() == disk.size);
    if !complete {
        eprintln!(
            "downloading {} ({})",
            disk.url,
            crate::app::human_bytes(disk.size)
        );
        if !runner.interactive(&download_command(&disk.url, &part, auth))? {
            return Err(CliError::DownloadFailed {
                url: disk.url.clone(),
            });
        }
    }
    let actual = file_sha256(&part)?;
    if actual != disk.sha256 {
        let _ = std::fs::remove_file(&part);
        return Err(CliError::ChecksumMismatch {
            file: disk.file.clone(),
            expected: disk.sha256.clone(),
            actual,
        });
    }
    std::fs::rename(&part, dest).map_err(CliError::io(format!("write {}", dest.display())))?;
    let marker = dest.with_extension(format!(
        "{}.sha256",
        dest.extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
    ));
    std::fs::write(marker, format!("{}\n", disk.sha256))
        .map_err(CliError::io("write the checksum marker"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_boot_companions_are_bound_to_the_disk_manifest_and_rehashed() {
        struct Writes(Vec<(String, Vec<u8>)>);
        impl Runner for Writes {
            fn output(&mut self, _: &CommandLine) -> Result<crate::hypervisor::Output, CliError> {
                Err(CliError::InvalidInput("not used".into()))
            }
            fn interactive(&mut self, command: &CommandLine) -> Result<bool, CliError> {
                let file = command.args.last().unwrap().rsplit('/').next().unwrap();
                let dest =
                    &command.args[command.args.iter().position(|a| a == "--output").unwrap() + 1];
                let data = &self.0.iter().find(|(name, _)| name == file).unwrap().1;
                std::fs::write(dest, data).unwrap();
                Ok(true)
            }
        }
        let tag = "main-1234567";
        let arch = "aarch64";
        let package = format!("{PACKAGES}/{tag}/");
        let names = [
            format!("tilo-vm-{tag}-{arch}-direct-Image"),
            format!("tilo-vm-{tag}-{arch}-direct-initrd"),
            format!("tilo-vm-{tag}-{arch}-direct-boot.json"),
        ];
        let payloads = [
            b"kernel".as_slice(),
            b"initrd".as_slice(),
            b"boot-json".as_slice(),
        ];
        let files: Vec<_> = names
            .iter()
            .zip(payloads)
            .map(|(name, data)| {
                serde_json::json!({
                    "file": name, "url": format!("{package}{name}"),
                    "sha256": hex::encode(Sha256::digest(data)), "size": data.len(),
                })
            })
            .collect();
        let disk_name = format!("tilo-vm-{tag}-{arch}.qcow2");
        let manifest: DiskManifest = serde_json::from_value(serde_json::json!({
            "tag": tag, "sourceRevision": format!("1234567{}", "a".repeat(33)),
            "disks": [{"arch": arch, "format": "qcow2", "file": disk_name,
                       "url": format!("{package}{disk_name}"), "sha256": "a".repeat(64), "size": 4}],
            "directBoot": [{"arch": arch, "manifest": names[2], "files": files}],
        })).unwrap();
        let disk = manifest.pick(arch, "qcow2").unwrap();
        let bundle = manifest.direct_boot_for(tag, arch, disk).unwrap();
        assert!(manifest.direct_boot_for(tag, "x86_64", disk).is_err());

        let mut wrong_url = manifest.clone();
        wrong_url.direct_boot[0].files[0].url = "https://elsewhere.invalid/kernel".into();
        assert!(wrong_url.direct_boot_for(tag, arch, disk).is_err());
        let mut wrong_source = manifest.clone();
        wrong_source.source_revision = Some("f".repeat(40));
        assert!(wrong_source.direct_boot_for(tag, arch, disk).is_err());

        let temp = tempfile::tempdir().unwrap();
        let mut runner = Writes(
            names
                .iter()
                .zip(payloads)
                .map(|(name, data)| (name.clone(), data.to_vec()))
                .collect(),
        );
        let auth = AuthHeader::new(None).unwrap();
        let path = fetch_direct_boot(&mut runner, temp.path(), &bundle, &auth).unwrap();
        assert_eq!(path, temp.path().join(&names[2]));
        std::fs::write(temp.path().join(&names[0]), b"tamper").unwrap();
        assert!(matches!(
            fetch_direct_boot(&mut runner, temp.path(), &bundle, &auth),
            Err(CliError::ChecksumMismatch { .. })
        ));
        assert!(!temp.path().join(&names[0]).exists());
        assert!(fetch_direct_boot(&mut runner, temp.path(), &bundle, &auth).is_ok());
    }

    const MANIFEST: &str = r#"{
      "schema": "https://tilo.sh/dev/disk/v1",
      "tag": "main-1a2b3c4",
      "sourceRevision": "1a2b3c4d",
      "image": "registry.tilo.so/tilo/tilo-vm:main-1a2b3c4",
      "imageDigest": "sha256:aa",
      "disks": [
        {"arch": "x86_64", "format": "qcow2", "file": "tilo-vm-main-1a2b3c4-x86_64.qcow2",
         "url": "https://code.tilo.so/api/packages/tilo/generic/tilo-vm-disk/main-1a2b3c4/tilo-vm-main-1a2b3c4-x86_64.qcow2",
         "sha256": "ab", "size": 3, "virtualSize": 10}
      ]
    }"#;

    /// The published manifest parses and picks by arch and format; status is split from the body; the
    /// newest channel tag wins; downloads are verified and cached.
    #[test]
    fn published_disks_are_picked_verified_and_cached() {
        // Phase: the_published_manifest_parses_and_picks_by_arch_and_format.
        {
            let manifest: DiskManifest = serde_json::from_str(MANIFEST).unwrap();
            assert_eq!(manifest.tag, "main-1a2b3c4");
            assert!(manifest.pick("x86_64", "qcow2").is_some());
            assert!(manifest.pick("aarch64", "qcow2").is_none());
            assert!(manifest.pick("x86_64", format_for("hyperv")).is_none());
            assert_eq!(manifest.offered(), ["x86_64 qcow2"]);
            assert_eq!(
                manifest_url("main-1a2b3c4", "x86_64"),
                "https://code.tilo.so/api/packages/tilo/generic/tilo-vm-disk/main-1a2b3c4/tilo-vm-disk-x86_64.json"
            );
            assert_eq!(
                files_url("main-1a2b3c4"),
                "https://code.tilo.so/api/v1/packages/tilo/generic/tilo-vm-disk/main-1a2b3c4/files"
            );
        }

        // Phase: a_tag_offers_only_architectures_whose_manifest_is_committed.
        {
            let files = r#"[
              {"name": "tilo-vm-main-1a2b3c4-ci-aarch64.qcow2"},
              {"name": "tilo-vm-main-1a2b3c4-ci-aarch64.qcow2.sha256"},
              {"name": "tilo-vm-disk-aarch64.json"},
              {"name": "tilo-vm-main-1a2b3c4-ci-x86_64.qcow2"}
            ]"#;
            // x86_64's disk is uploaded but its lane has not committed it yet.
            assert_eq!(
                offered_in_files("main-1a2b3c4-ci", files),
                ["aarch64 qcow2"]
            );
            assert!(offered_in_files("main-1a2b3c4-ci", "[]").is_empty());
            assert!(offered_in_files("main-1a2b3c4-ci", "not json").is_empty());
        }

        // Phase: status_is_split_from_the_body.
        {
            assert_eq!(split_status("{\"a\":1}\n200"), (200, "{\"a\":1}"));
            assert_eq!(split_status("\n404"), (404, ""));
            assert_eq!(split_status("garbage"), (0, ""));
        }

        // Phase: newest_channel_tag_wins.
        {
            let versions = r#"[
              {"name": "tilo-vm-disk", "version": "feature-x-aaaaaaa", "created_at": "2026-09-22T10:00:00Z"},
              {"name": "tilo-vm-disk", "version": "main-bbbbbbb", "created_at": "2026-09-21T10:00:00Z"},
              {"name": "tilo-vm-disk", "version": "main-ccccccc", "created_at": "2026-09-20T10:00:00Z"},
              {"name": "other", "version": "main-ddddddd", "created_at": "2026-09-23T10:00:00Z"}
            ]"#;
            assert_eq!(newest_tag(versions).as_deref(), Some("main-bbbbbbb"));
            assert_eq!(newest_tag("[]"), None);
            assert_eq!(candidate_tags(versions), ["main-bbbbbbb", "main-ccccccc"]);
            let branches_only = r#"[
              {"name": "tilo-vm-disk", "version": "feature-x-aaaaaaa-ci", "created_at": "2026-09-22T10:00:00Z"}
            ]"#;
            assert_eq!(candidate_tags(branches_only), ["feature-x-aaaaaaa-ci"]);
        }

        // Phase: downloads_are_verified_and_cached.
        {
            struct Writes(Vec<u8>);
            impl Runner for Writes {
                fn output(
                    &mut self,
                    _command: &CommandLine,
                ) -> Result<crate::hypervisor::Output, CliError> {
                    unimplemented_output()
                }
                fn interactive(&mut self, command: &CommandLine) -> Result<bool, CliError> {
                    let at = command.args.iter().position(|a| a == "--output").unwrap();
                    std::fs::write(&command.args[at + 1], &self.0).unwrap();
                    Ok(true)
                }
            }
            fn unimplemented_output() -> Result<crate::hypervisor::Output, CliError> {
                Err(CliError::InvalidInput("not used".into()))
            }
            let dir = tempfile::tempdir().unwrap();
            let dest = dir.path().join("d.qcow2");
            let good = Disk {
                arch: "x86_64".into(),
                format: "qcow2".into(),
                file: "d.qcow2".into(),
                url: "https://x/d.qcow2".into(),
                sha256: hex::encode(Sha256::digest(b"abc")),
                size: 3,
            };
            let auth = AuthHeader::new(None).unwrap();
            let bad = Disk {
                sha256: "00".into(),
                ..good.clone()
            };
            assert!(matches!(
                fetch(&mut Writes(b"abc".to_vec()), &bad, &dest, &auth),
                Err(CliError::ChecksumMismatch { .. })
            ));
            assert!(
                !dest.with_extension("part").exists(),
                "a bad download is deleted"
            );
            fetch(&mut Writes(b"abc".to_vec()), &good, &dest, &auth).unwrap();
            assert!(cached(&dest, &good));
            // Cached: no second download (this runner would write wrong bytes).
            fetch(&mut Writes(b"zzz".to_vec()), &good, &dest, &auth).unwrap();
            assert_eq!(std::fs::read(&dest).unwrap(), b"abc");
        }
    }

    /// Tokens are found in fgj and tea configs and never reach the command line.
    #[test]
    fn registry_tokens_are_found_but_never_reach_the_command_line() {
        // Phase: tokens_are_found_in_fgj_and_tea_configs.
        {
            let fgj = "default_host: code.tilo.so\nhosts:\n    code.tilo.so:\n        hostname: code.tilo.so\n        token: abc123\n    other.example:\n        token: nope\n";
            assert_eq!(fgj_token(fgj, "code.tilo.so").as_deref(), Some("abc123"));
            assert_eq!(fgj_token(fgj, "missing.example"), None);
            let other_first = "hosts:\n  other.example:\n    token: nope\n  code.tilo.so:\n    user: q\n    token: \"xyz\"\n";
            assert_eq!(
                fgj_token(other_first, "code.tilo.so").as_deref(),
                Some("xyz")
            );
            let tea = "logins:\n  - name: other\n    url: https://git.example\n    token: nope\n  - name: tilo\n    url: https://code.tilo.so\n    token: t0k\n    default: true\n";
            assert_eq!(tea_token(tea, "code.tilo.so").as_deref(), Some("t0k"));
            assert_eq!(tea_token(tea, "missing.example"), None);
        }

        // Phase: tokens_never_reach_the_command_line.
        {
            let token = Token {
                value: "s3cret".into(),
                source: "test".into(),
            };
            let auth = AuthHeader::new(Some(&token)).unwrap();
            let get = get_command("https://x/y", &auth);
            let download = download_command("https://x/y", Path::new("/c/d.part"), &auth);
            for command in [&get, &download] {
                assert!(
                    !command.display().contains("s3cret"),
                    "{}",
                    command.display()
                );
                assert!(command.args.iter().any(|a| a.starts_with('@')));
            }
            assert!(download.args.contains(&"--continue-at".to_owned()));
            let anonymous = get_command("https://x/y", &AuthHeader::new(None).unwrap());
            assert!(!anonymous.args.contains(&"--header".to_owned()));
        }
    }
}
