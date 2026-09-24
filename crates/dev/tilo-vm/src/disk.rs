//! `vm create` without `--image`: fetch the dev disk CI published to the
//! forge's generic packages (`.forgejo/workflows/dev-disk.yml`), resume and
//! verify the download, and cache it under the platform cache dir.
//!
//! ```text
//! <PACKAGES>/<tag>/tilo-vm-disk.json   the manifest: disks per arch and format, sha256
//! <PACKAGES>/<tag>/<file>               one disk (qcow2 or vhdx)
//! ```
//!
//! HTTP goes through `curl`, present on every supported host; the token
//! travels in a header file, never on the command line.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::error::CliError;
use crate::hypervisor::{CommandLine, Runner};

pub const FORGE: &str = "code.podesta.ai";
pub const PACKAGES: &str = "https://code.podesta.ai/api/packages/qos/generic/tilo-vm-disk";
/// Lists the package's published versions.
pub const VERSIONS: &str =
    "https://code.podesta.ai/api/v1/packages/qos?type=generic&q=tilo-vm-disk&limit=50";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskManifest {
    pub tag: String,
    #[serde(default)]
    pub source_revision: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
    pub disks: Vec<Disk>,
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

pub fn manifest_url(tag: &str) -> String {
    format!("{PACKAGES}/{tag}/tilo-vm-disk.json")
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
    if let Some(home) = xdg_paths::home() {
        dirs.push(home.join(".config"));
    }
    if let Some(config) = xdg_paths::config_dir() {
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
    #[derive(Deserialize)]
    struct Published {
        name: String,
        version: String,
        #[serde(default)]
        created_at: String,
    }
    let mut versions: Vec<Published> = serde_json::from_str(versions_json).ok()?;
    versions.retain(|v| v.name == "tilo-vm-disk");
    versions.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let channel = |v: &&Published| {
        v.version.starts_with("main-") || v.version.starts_with(|c: char| c.is_ascii_digit())
    };
    versions
        .iter()
        .find(channel)
        .or_else(|| versions.first())
        .map(|v| v.version.clone())
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
    xdg_paths::cache_dir()
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

    const MANIFEST: &str = r#"{
      "schema": "https://tilo.sh/dev/disk/v1",
      "tag": "main-1a2b3c4",
      "sourceRevision": "1a2b3c4d",
      "image": "code.podesta.ai/qos/tilo-vm:main-1a2b3c4",
      "imageDigest": "sha256:aa",
      "disks": [
        {"arch": "x86_64", "format": "qcow2", "file": "tilo-vm-main-1a2b3c4-x86_64.qcow2",
         "url": "https://code.podesta.ai/api/packages/qos/generic/tilo-vm-disk/main-1a2b3c4/tilo-vm-main-1a2b3c4-x86_64.qcow2",
         "sha256": "ab", "size": 3, "virtualSize": 10}
      ]
    }"#;

    #[test]
    fn the_published_manifest_parses_and_picks_by_arch_and_format() {
        let manifest: DiskManifest = serde_json::from_str(MANIFEST).unwrap();
        assert_eq!(manifest.tag, "main-1a2b3c4");
        assert!(manifest.pick("x86_64", "qcow2").is_some());
        assert!(manifest.pick("aarch64", "qcow2").is_none());
        assert!(manifest.pick("x86_64", format_for("hyperv")).is_none());
        assert_eq!(manifest.offered(), ["x86_64 qcow2"]);
        assert_eq!(
            manifest_url("main-1a2b3c4"),
            "https://code.podesta.ai/api/packages/qos/generic/tilo-vm-disk/main-1a2b3c4/tilo-vm-disk.json"
        );
    }

    #[test]
    fn tokens_are_found_in_fgj_and_tea_configs() {
        let fgj = "default_host: code.podesta.ai\nhosts:\n    code.podesta.ai:\n        hostname: code.podesta.ai\n        token: abc123\n    other.example:\n        token: nope\n";
        assert_eq!(fgj_token(fgj, "code.podesta.ai").as_deref(), Some("abc123"));
        assert_eq!(fgj_token(fgj, "missing.example"), None);
        let other_first = "hosts:\n  other.example:\n    token: nope\n  code.podesta.ai:\n    user: q\n    token: \"xyz\"\n";
        assert_eq!(
            fgj_token(other_first, "code.podesta.ai").as_deref(),
            Some("xyz")
        );
        let tea = "logins:\n  - name: other\n    url: https://git.example\n    token: nope\n  - name: podesta\n    url: https://code.podesta.ai\n    token: t0k\n    default: true\n";
        assert_eq!(tea_token(tea, "code.podesta.ai").as_deref(), Some("t0k"));
        assert_eq!(tea_token(tea, "missing.example"), None);
    }

    #[test]
    fn tokens_never_reach_the_command_line() {
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

    #[test]
    fn status_is_split_from_the_body() {
        assert_eq!(split_status("{\"a\":1}\n200"), (200, "{\"a\":1}"));
        assert_eq!(split_status("\n404"), (404, ""));
        assert_eq!(split_status("garbage"), (0, ""));
    }

    #[test]
    fn newest_channel_tag_wins() {
        let versions = r#"[
          {"name": "tilo-vm-disk", "version": "feature-x-aaaaaaa", "created_at": "2026-09-22T10:00:00Z"},
          {"name": "tilo-vm-disk", "version": "main-bbbbbbb", "created_at": "2026-09-21T10:00:00Z"},
          {"name": "tilo-vm-disk", "version": "main-ccccccc", "created_at": "2026-09-20T10:00:00Z"},
          {"name": "other", "version": "main-ddddddd", "created_at": "2026-09-23T10:00:00Z"}
        ]"#;
        assert_eq!(newest_tag(versions).as_deref(), Some("main-bbbbbbb"));
        assert_eq!(newest_tag("[]"), None);
    }

    #[test]
    fn downloads_are_verified_and_cached() {
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
