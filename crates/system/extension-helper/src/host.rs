//! What the helper asks of the machine: rpm, the network, the signature
//! check, rpm2archive, systemd-sysext and `SELinux` labels.
//!
//! [`Host`]'s provided methods are the production behaviour; [`System`] uses
//! all of them. Tests implement the trait to stand in for what cannot exist
//! off a Tilo machine and keep the rest.

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

use rustix::fs::{AtFlags, CWD, Timespec, Timestamps, utimensat};

use crate::machine::Layout;
use crate::model::{self, safe_member};
use crate::py;
use crate::{At, Error, Result};

const HTTP_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_REDIRECTS: usize = 10;

/// The name, and name-version-release.arch, an RPM's header gives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub name: String,
    pub nvra: String,
}

/// An HTTP response body being read.
pub trait Response: Read {
    /// The status code of the final response (206 for a range).
    fn status(&self) -> u16;
}

/// Why a request produced no body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpError {
    /// The server answered with an error status (or a refused redirect).
    Status { code: u16, reason: String },
    /// The server could not be reached, or the connection failed.
    Unreachable(String),
}

/// A finished command: exit status and its output as text.
pub struct Output {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Run `argv`, capturing its output.
///
/// # Errors
/// `Os` when it cannot start; with `check`, `Pack` when it fails, with the
/// last lines of what it said.
fn run(argv: &[&OsStr], check: bool) -> Result<Output> {
    let (program, rest) = argv
        .split_first()
        .ok_or_else(|| Error::pack("no command"))?;
    let output = Command::new(program)
        .args(rest)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| Error::os(&error, Some(Path::new(program))))?;
    let result = Output {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    };
    if check && !result.success {
        let said = if result.stderr.is_empty() {
            &result.stdout
        } else {
            &result.stderr
        };
        let lines = py::splitlines(py::strip(said));
        let detail = lines
            .get(lines.len().saturating_sub(5)..)
            .unwrap_or_default()
            .join(" ");
        return Err(Error::Pack(format!(
            "{} failed: {}",
            program.to_string_lossy(),
            py::truncate(&detail, 400)
        )));
    }
    Ok(result)
}

/// `shutil.which`: whether `name` is an executable on `PATH`.
pub fn which(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| {
            fs::metadata(dir.join(name))
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
    })
}

/// The machine, as the helper sees it.
pub trait Host {
    /// `rpm -qa` of the base: one `NAME EPOCHNUM:VERSION-RELEASE.ARCH` line
    /// per package.
    ///
    /// # Errors
    /// rpm cannot run or fails.
    fn rpm_query_all(&self, layout: &Layout) -> Result<Vec<String>> {
        let root = layout.root.as_os_str();
        let output = run(
            &[
                OsStr::new("rpm"),
                OsStr::new("--root"),
                root,
                OsStr::new("-qa"),
                OsStr::new("--qf"),
                OsStr::new("%{NAME} %{EPOCHNUM}:%{VERSION}-%{RELEASE}.%{ARCH}\n"),
            ],
            true,
        )?;
        Ok(py::splitlines(&output.stdout)
            .into_iter()
            .map(str::to_owned)
            .collect())
    }

    /// Open `url`, from byte `offset` on when it is not 0.
    ///
    /// # Errors
    /// The server's error status, or why it could not be reached.
    fn http_open(&self, url: &str, offset: u64) -> Result<Box<dyn Response>, HttpError> {
        http_open(url, offset)
    }

    /// Check the manifest bytes against the image's extension-manifest key.
    ///
    /// # Errors
    /// `Pack` when there is no key or the signature does not verify.
    fn verify_signature(
        &self,
        layout: &Layout,
        data: &[u8],
        signature: &[u8],
        workdir: &Path,
    ) -> Result<()> {
        let key = layout.trust_key();
        if !key.is_file() {
            return Err(Error::pack("this image has no extension-manifest key"));
        }
        fs::create_dir_all(workdir).at(workdir)?;
        let manifest = workdir.join("manifest.json");
        let sig = workdir.join("manifest.json.sig");
        fs::write(&manifest, data).at(&manifest)?;
        fs::write(&sig, signature).at(&sig)?;
        let crypto = self.tilo_crypto();
        let result = run(
            &[
                crypto.as_os_str(),
                OsStr::new("verify-p256-sha256"),
                key.as_os_str(),
                manifest.as_os_str(),
                sig.as_os_str(),
            ],
            false,
        )?;
        if !result.success {
            // The reason goes to the journal: a sandbox refusing to open the
            // key must not look like a forged manifest to whoever reads the logs.
            let said = if result.stderr.is_empty() {
                &result.stdout
            } else {
                &result.stderr
            };
            eprintln!(
                "tilo-extension: tilo-crypto: {}",
                py::truncate(py::strip(said), 300)
            );
            return Err(Error::pack(
                "the manifest's signature is not valid for this image; nothing was changed",
            ));
        }
        Ok(())
    }

    /// The signature checker.
    fn tilo_crypto(&self) -> PathBuf {
        PathBuf::from("/usr/bin/tilo-crypto")
    }

    /// `NetworkManager`'s cached connectivity state (no probe is sent). An
    /// empty string means unknown.
    fn connectivity(&self) -> String {
        if !which("nmcli") {
            return String::new();
        }
        match run(
            &[
                OsStr::new("nmcli"),
                OsStr::new("-t"),
                OsStr::new("-f"),
                OsStr::new("CONNECTIVITY"),
                OsStr::new("general"),
            ],
            false,
        ) {
            Ok(output) if output.success => py::strip(&output.stdout).to_owned(),
            _ => String::new(),
        }
    }

    /// The header of the RPM at `path`.
    ///
    /// # Errors
    /// rpm cannot read it; `Value` when its answer has the wrong shape.
    fn rpm_header(&self, path: &Path) -> Result<Header> {
        let output = run(
            &[
                OsStr::new("rpm"),
                OsStr::new("-qp"),
                OsStr::new("--nosignature"),
                OsStr::new("--nodigest"),
                OsStr::new("--qf"),
                OsStr::new("%{NAME}\t%{VERSION}\t%{RELEASE}\t%{ARCH}\n"),
                path.as_os_str(),
            ],
            true,
        )?;
        let fields: Vec<&str> = output.stdout.trim_end_matches('\n').split('\t').collect();
        match fields.as_slice() {
            [name, version, release, arch] => Ok(Header {
                name: (*name).to_owned(),
                nvra: format!("{name}-{version}-{release}.{arch}"),
            }),
            short if short.len() < 4 => Err(Error::Value(format!(
                "not enough values to unpack (expected 4, got {})",
                short.len()
            ))),
            _ => Err(Error::Value(
                "too many values to unpack (expected 4)".to_owned(),
            )),
        }
    }

    /// Unpack the /usr of the RPM at `path` into `tree`, collecting the text
    /// of its /etc/ld.so.conf.d files in `ld_conf`.
    ///
    /// # Errors
    /// A member that would land outside the tree, one of an unsupported kind,
    /// or rpm2archive failing.
    fn extract_rpm(&self, path: &Path, tree: &Path, ld_conf: &mut Vec<String>) -> Result<()> {
        extract_rpm(path, tree, ld_conf)
    }

    /// Merge exactly what is in the search path now (or unmerge if empty).
    ///
    /// Runs systemd-sysext directly: its boot-time service stays masked on
    /// the sealed image, so only this activation, right after rebuilding every
    /// part from verified bytes, ever merges.
    ///
    /// # Errors
    /// systemd-sysext fails.
    fn sysext_refresh(&self, layout: &Layout) -> Result<()> {
        if !layout.is_host_root() || !Path::new("/run/systemd/system").is_dir() {
            return Ok(());
        }
        run(&[OsStr::new("systemd-sysext"), OsStr::new("refresh")], true).map(drop)
    }

    /// Take every extension off /usr without merging anything.
    ///
    /// For the paths that refuse: a refresh there would merge whatever else
    /// is in the search path, which is exactly what was refused.
    ///
    /// # Errors
    /// systemd-sysext fails.
    fn sysext_unmerge(&self, layout: &Layout) -> Result<()> {
        if !layout.is_host_root() || !Path::new("/run/systemd/system").is_dir() {
            return Ok(());
        }
        run(&[OsStr::new("systemd-sysext"), OsStr::new("unmerge")], true).map(drop)
    }

    /// Whether the image keeps systemd-sysext from merging anything by itself.
    ///
    /// The sealed image masks it (docs/security/boot-chain.md); the unsealed
    /// development image unmasks it for its own layers.
    ///
    /// # Errors
    /// systemctl cannot run.
    fn sysext_masked(&self) -> Result<bool> {
        let output = run(
            &[
                OsStr::new("systemctl"),
                OsStr::new("is-enabled"),
                OsStr::new("systemd-sysext.service"),
            ],
            false,
        )?;
        Ok(py::strip(&output.stdout).starts_with("masked"))
    }

    /// Have PID 1 run the activation unit: it merges in the host's mount
    /// namespace, which the sandboxed install unit does not share.
    ///
    /// # Errors
    /// systemctl fails.
    fn start_activation_unit(&self) -> Result<()> {
        run(
            &[
                OsStr::new("systemctl"),
                OsStr::new("start"),
                OsStr::new("tilo-extension-activate.service"),
            ],
            true,
        )
        .map(drop)
    }

    /// Label the tree as the files it overlays would be, when `SELinux` is on.
    ///
    /// # Errors
    /// setfiles fails.
    fn relabel(&self, tree: &Path) -> Result<()> {
        let contexts = Path::new("/etc/selinux/targeted/contexts/files/file_contexts");
        if !Path::new("/sys/fs/selinux/enforce").exists()
            || !contexts.exists()
            || !which("setfiles")
        {
            return Ok(());
        }
        run(
            &[
                OsStr::new("setfiles"),
                OsStr::new("-F"),
                OsStr::new("-r"),
                tree.as_os_str(),
                contexts.as_os_str(),
                tree.as_os_str(),
            ],
            true,
        )
        .map(drop)
    }

    /// The machine's architecture, as `uname -m` names it.
    fn machine(&self) -> String {
        rustix::system::uname()
            .machine()
            .to_string_lossy()
            .into_owned()
    }

    /// The effective user id.
    fn euid(&self) -> u32 {
        rustix::process::geteuid().as_raw()
    }

    /// `os.replace`: rename over `to`.
    ///
    /// # Errors
    /// The rename's error, unworded (callers check for EXDEV).
    fn replace(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }
}

/// The production machine.
#[derive(Debug, Default, Clone, Copy)]
pub struct System;

impl Host for System {}

struct HttpResponse(reqwest::blocking::Response);

impl Read for HttpResponse {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl Response for HttpResponse {
    fn status(&self) -> u16 {
        self.0.status().as_u16()
    }
}

/// A redirect away from HTTPS, refused.
#[derive(Debug)]
struct RedirectRefused(u16, &'static str);

impl std::fmt::Display for RedirectRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.1)
    }
}

impl std::error::Error for RedirectRefused {}

/// The one client, built on first download: boot activation never needs
/// the HTTP or TLS stack.
fn client() -> Result<&'static reqwest::blocking::Client, HttpError> {
    static CLIENT: OnceLock<Result<reqwest::blocking::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            tilo_net::http::blocking_client()
                .user_agent("tilo-extension/1")
                .timeout(HTTP_TIMEOUT)
                // Follow a publisher's mirror redirects, but never to plain HTTP.
                .redirect(reqwest::redirect::Policy::custom(|attempt| {
                    let code = attempt.status().as_u16();
                    if attempt.url().scheme() != "https" {
                        attempt.error(RedirectRefused(code, "redirect away from HTTPS refused"))
                    } else if attempt.previous().len() > MAX_REDIRECTS {
                        attempt.error(RedirectRefused(
                            code,
                            "The HTTP server returned a redirect error that would lead to an infinite loop.",
                        ))
                    } else {
                        attempt.follow()
                    }
                }))
                .build()
                .map_err(|error| error_chain(&error))
        })
        .as_ref()
        .map_err(|message| HttpError::Unreachable(message.clone()))
}

fn error_chain(error: &dyn std::error::Error) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(inner) = source {
        let text = inner.to_string();
        if !message.contains(&text) {
            message.push_str(": ");
            message.push_str(&text);
        }
        source = inner.source();
    }
    message
}

fn http_open(url: &str, offset: u64) -> Result<Box<dyn Response>, HttpError> {
    let mut request = client()?.get(url);
    if offset > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={offset}-"));
    }
    let response = request.send().map_err(|error| {
        let mut source: Option<&dyn std::error::Error> = Some(&error);
        while let Some(inner) = source {
            if let Some(refused) = inner.downcast_ref::<RedirectRefused>() {
                return HttpError::Status {
                    code: refused.0,
                    reason: refused.1.to_owned(),
                };
            }
            source = inner.source();
        }
        HttpError::Unreachable(error_chain(&error))
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(HttpError::Status {
            code: status.as_u16(),
            reason: status.canonical_reason().unwrap_or_default().to_owned(),
        });
    }
    Ok(Box::new(HttpResponse(response)))
}

/// `Path.resolve()` without `strict`: symlinks that exist are followed, the
/// rest of the path is taken as written.
fn resolve(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut pending: Vec<PathBuf> = absolute
        .components()
        .rev()
        .filter_map(|c| match c {
            Component::Normal(part) => Some(PathBuf::from(part)),
            Component::ParentDir => Some(PathBuf::from("..")),
            _ => None,
        })
        .collect();
    let mut resolved = PathBuf::from("/");
    let mut hops = 0u32;
    while let Some(part) = pending.pop() {
        if part.as_os_str() == ".." {
            resolved.pop();
            continue;
        }
        let candidate = resolved.join(&part);
        let link = fs::symlink_metadata(&candidate).is_ok_and(|m| m.file_type().is_symlink());
        if link
            && hops < 40
            && let Ok(target) = fs::read_link(&candidate)
        {
            hops = hops.saturating_add(1);
            if target.is_absolute() {
                resolved = PathBuf::from("/");
            }
            for c in target.components().rev() {
                match c {
                    Component::Normal(p) => pending.push(PathBuf::from(p)),
                    Component::ParentDir => pending.push(PathBuf::from("..")),
                    _ => {}
                }
            }
            continue;
        }
        resolved = candidate;
    }
    resolved
}

/// Whether writing `target` stays in `tree`, even through earlier symlinks.
fn inside(tree: &Path, target: &Path) -> bool {
    let parent = target.parent().unwrap_or(target);
    resolve(parent).starts_with(resolve(tree))
}

fn join_parts(tree: &Path, parts: &[&[u8]]) -> PathBuf {
    let mut path = tree.to_path_buf();
    for part in parts {
        path.push(OsStr::from_bytes(part));
    }
    path
}

pub(crate) fn set_times(path: &Path, seconds: i64, nanos: i64, follow: bool) -> Result<()> {
    let stamp = Timespec {
        tv_sec: seconds,
        tv_nsec: nanos,
    };
    let flags = if follow {
        AtFlags::empty()
    } else {
        AtFlags::SYMLINK_NOFOLLOW
    };
    utimensat(
        CWD,
        path,
        &Timestamps {
            last_access: stamp,
            last_modification: stamp,
        },
        flags,
    )
    .map_err(io::Error::from)
    .at(path)
}

/// A member's modification time: the pax record when there is one (it can be
/// fractional), else the header's.
fn member_mtime<R: Read>(entry: &mut tar::Entry<'_, R>) -> (i64, i64) {
    if let Ok(Some(extensions)) = entry.pax_extensions() {
        for extension in extensions.flatten() {
            if extension.key() == Ok("mtime")
                && let Ok(text) = extension.value()
            {
                let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
                if let Ok(seconds) = whole.parse::<i64>() {
                    let digits: String = fraction.chars().take(9).collect();
                    let nanos = format!("{digits:0<9}").parse::<i64>().unwrap_or(0);
                    if text.starts_with('-') && nanos > 0 {
                        return (
                            seconds.saturating_sub(1),
                            1_000_000_000_i64.saturating_sub(nanos),
                        );
                    }
                    return (seconds, nanos);
                }
            }
        }
    }
    (
        i64::try_from(entry.header().mtime().unwrap_or(0)).unwrap_or(i64::MAX),
        0,
    )
}

fn extract_rpm(path: &Path, tree: &Path, ld_conf: &mut Vec<String>) -> Result<()> {
    let source = File::open(path).at(path)?;
    let mut archive = Command::new("rpm2archive")
        .args(["-n", "-"])
        .stdin(source)
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| Error::os(&error, Some(Path::new("rpm2archive"))))?;
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let unpacked = match archive.stdout.take() {
        Some(stdout) => unpack(tar::Archive::new(stdout), &file_name, tree, ld_conf),
        None => Ok(()),
    };
    let status = archive.wait().at(Path::new("rpm2archive"))?;
    if !status.success() {
        return Err(Error::Pack(format!("rpm2archive failed on {file_name}")));
    }
    unpacked
}

fn unpack<R: Read>(
    mut tar: tar::Archive<R>,
    file_name: &str,
    tree: &Path,
    ld_conf: &mut Vec<String>,
) -> Result<()> {
    for entry in tar.entries().os()? {
        let mut member = entry.os()?;
        let name = member.path_bytes().into_owned();
        let shown = String::from_utf8_lossy(&name).into_owned();
        let kind = member.header().entry_type();
        let is_file = kind.is_file() || kind.is_contiguous() || kind.is_gnu_sparse();
        if model::archive_parts(&name).get(..2)
            == Some(&[b"etc".as_slice(), b"ld.so.conf.d".as_slice()])
            && is_file
        {
            let mut text = Vec::new();
            member.read_to_end(&mut text).os()?;
            ld_conf.push(String::from_utf8_lossy(&text).into_owned());
            continue;
        }
        let Some(relative) = safe_member(&name)? else {
            continue;
        };
        let target = join_parts(tree, &relative);
        if !inside(tree, &target) {
            return Err(Error::Pack(format!(
                "{file_name}: {shown} would be written outside the tree"
            )));
        }
        let parent = target.parent().unwrap_or(tree).to_path_buf();
        if kind.is_dir() {
            fs::create_dir_all(&target).at(&target)?;
            fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).at(&target)?;
        } else if kind.is_symlink() {
            let link = member
                .link_name_bytes()
                .map(std::borrow::Cow::into_owned)
                .unwrap_or_default();
            fs::create_dir_all(&parent).at(&parent)?;
            if target.is_symlink() || target.exists() {
                fs::remove_file(&target).at(&target)?;
            }
            symlink(OsStr::from_bytes(&link), &target).at(&target)?;
            let (seconds, nanos) = member_mtime(&mut member);
            set_times(&target, seconds, nanos, false)?;
        } else if is_file {
            fs::create_dir_all(&parent).at(&parent)?;
            if target.is_symlink() {
                fs::remove_file(&target).at(&target)?;
            }
            let (seconds, nanos) = member_mtime(&mut member);
            let executable = member.header().mode().unwrap_or(0) & 0o111 != 0;
            let mut out = File::create(&target).at(&target)?;
            io::copy(&mut member, &mut out).at(&target)?;
            out.flush().at(&target)?;
            drop(out);
            // No set-id bits and no writable-by-others files, ever.
            let mode = if executable { 0o755 } else { 0o644 };
            fs::set_permissions(&target, fs::Permissions::from_mode(mode)).at(&target)?;
            // The package's own times: rebuilding a part at every boot must
            // not look like new plugins to GStreamer's registry.
            set_times(&target, seconds, nanos, true)?;
        } else if kind.is_hard_link() {
            let link = member
                .link_name_bytes()
                .map(std::borrow::Cow::into_owned)
                .unwrap_or_default();
            let Some(source) = safe_member(&link)? else {
                return Err(Error::Pack(format!(
                    "{file_name}: hard link outside /usr: {shown}"
                )));
            };
            fs::create_dir_all(&parent).at(&parent)?;
            copy_with_times(&join_parts(tree, &source), &target)?;
        } else {
            return Err(Error::Pack(format!(
                "{file_name}: unsupported member type {shown}"
            )));
        }
    }
    Ok(())
}

/// `shutil.copy2`: contents, permission bits and times.
fn copy_with_times(source: &Path, target: &Path) -> Result<()> {
    fs::copy(source, target).at(source)?;
    let meta = fs::metadata(source).at(source)?;
    let access = Timespec {
        tv_sec: meta.atime(),
        tv_nsec: meta.atime_nsec(),
    };
    let modification = Timespec {
        tv_sec: meta.mtime(),
        tv_nsec: meta.mtime_nsec(),
    };
    utimensat(
        CWD,
        target,
        &Timestamps {
            last_access: access,
            last_modification: modification,
        },
        AtFlags::empty(),
    )
    .map_err(io::Error::from)
    .at(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_follows_existing_links_and_keeps_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap();
        fs::create_dir_all(root.join("tree/usr")).unwrap();
        symlink("/etc", root.join("tree/usr/out")).unwrap();
        symlink("../usr", root.join("tree/usr/back")).unwrap();
        let tree = root.join("tree");
        assert!(inside(&tree, &tree.join("usr/lib64/new.so")));
        assert!(inside(&tree, &tree.join("usr/back/x")));
        assert!(!inside(&tree, &tree.join("usr/out/passwd")));
    }

    /// A loopback HTTP server answering each connection by the request path.
    fn serve(answers: usize) -> (String, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{BufRead, BufReader};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let next = format!("{base}/next");
        let handle = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for stream in listener.incoming().take(answers) {
                let mut stream = stream.unwrap();
                let mut request = String::new();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    request.push_str(&line);
                }
                let path = request
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_owned();
                let ranged = request.to_ascii_lowercase().contains("range: bytes=4-");
                let answer = match path.as_str() {
                    "/redirect" => format!("HTTP/1.1 302 Found\r\nLocation: {next}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"),
                    "/file" if ranged => "HTTP/1.1 206 Partial Content\r\nConnection: close\r\nContent-Length: 2\r\n\r\nef".to_owned(),
                    "/file" => "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 6\r\n\r\nabcdef".to_owned(),
                    _ => "HTTP/1.1 404 Not Found\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".to_owned(),
                };
                stream.write_all(answer.as_bytes()).unwrap();
                requests.push(path);
            }
            requests
        });
        (base, handle)
    }

    #[test]
    fn downloads_follow_no_redirect_away_from_https_and_resume_by_range() {
        let (base, server) = serve(4);
        let refused = System
            .http_open(&format!("{base}/redirect"), 0)
            .err()
            .unwrap();
        assert_eq!(
            refused,
            HttpError::Status {
                code: 302,
                reason: "redirect away from HTTPS refused".to_owned()
            }
        );
        let missing = System.http_open(&format!("{base}/gone"), 0).err().unwrap();
        assert_eq!(
            missing,
            HttpError::Status {
                code: 404,
                reason: "Not Found".to_owned()
            }
        );
        let mut whole = System.http_open(&format!("{base}/file"), 0).unwrap();
        let mut body = String::new();
        whole.read_to_string(&mut body).unwrap();
        assert_eq!((whole.status(), body.as_str()), (200, "abcdef"));
        let mut rest = System.http_open(&format!("{base}/file"), 4).unwrap();
        let mut tail = String::new();
        rest.read_to_string(&mut tail).unwrap();
        assert_eq!((rest.status(), tail.as_str()), (206, "ef"));
        assert_eq!(
            server.join().unwrap(),
            ["/redirect", "/gone", "/file", "/file"],
            "the refused target is never fetched"
        );
    }
}
