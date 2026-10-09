//! Narrow authenticated Desktop package transaction; no disk authority.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// Data-only request; all authority comes from the helper's embedded catalog.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopInstallRequest {
    pub schema: u32,
    pub release_id: String,
    pub package_input: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InstalledDesktop {
    pub version: String,
    pub session_entry: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "phase", content = "detail", deny_unknown_fields)]
pub enum DesktopInstallEvent {
    Preparing,
    Installing,
    Verifying,
    /// Location of bounded diagnostics readable only by the privileged host.
    DiagnosticLog(PathBuf),
    Complete(InstalledDesktop),
    Failed(String),
    AuthorizationCancelled,
    AuthorizationRefused,
}

pub(super) const REQUEST_LIMIT: usize = 16 * 1024;
pub(super) const SESSION_ENTRY: &str = "/usr/share/wayland-sessions/tilo.desktop";

pub(super) fn request_digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(any(target_os = "linux", all(test, unix)))]
use transaction_core::{Backend, transaction};

#[cfg(test)]
#[cfg(unix)]
use transaction_core::{pin_input, stage_package};

#[cfg(test)]
#[cfg(target_os = "linux")]
use transaction_core::run_transaction;

#[cfg(any(target_os = "linux", all(test, unix)))]
mod transaction_core {
    use super::super::desktop_catalog::DesktopRelease;
    use super::super::desktop_probe::{
        DesktopHost, DesktopReadiness, PackageManager, desktop_readiness,
    };
    use super::{
        DesktopInstallEvent, DesktopInstallRequest, InstalledDesktop, REQUEST_LIMIT, request_digest,
    };
    use sha2::{Digest, Sha256};
    use std::{
        fs::{File, OpenOptions},
        io::{Read, Write},
        os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        path::{Path, PathBuf},
    };

    pub(super) trait Backend {
        fn probe(&mut self) -> Result<DesktopHost, String>;
        fn check_chroot(&mut self) -> Result<(), String>;
        fn validate_package(
            &mut self,
            manager: PackageManager,
            staged: &Path,
            release: &DesktopRelease,
        ) -> Result<(), String>;
        fn install(&mut self, manager: PackageManager, staged: &Path) -> Result<(), String>;
        fn verify(
            &mut self,
            manager: PackageManager,
            release: &DesktopRelease,
        ) -> Result<InstalledDesktop, String>;
    }

    fn parse_request(bytes: &[u8], digest: &str) -> Result<DesktopInstallRequest, String> {
        if bytes.len() > REQUEST_LIMIT || request_digest(bytes) != digest {
            return Err("Desktop request changed or exceeds its bound.".into());
        }
        let request: DesktopInstallRequest =
            serde_json::from_slice(bytes).map_err(|_| "Malformed Desktop request.")?;
        if request.schema != 1
            || request.release_id.is_empty()
            || !request.package_input.is_absolute()
        {
            return Err("Unsupported Desktop request.".into());
        }
        Ok(request)
    }

    pub(super) fn authorize(
        host: &DesktopHost,
        releases: &[DesktopRelease],
        id: &str,
    ) -> Result<(DesktopRelease, bool), String> {
        match desktop_readiness(host, releases) {
            DesktopReadiness::Ready(release) if release.id == id => Ok((release, false)),
            DesktopReadiness::AlreadyInstalled(release) if release.id == id => Ok((release, true)),
            DesktopReadiness::Blocked(reasons) => Err(reasons.join(" ")),
            _ => Err(
                "The requested release does not match this host's authenticated selection.".into(),
            ),
        }
    }

    pub(super) fn transaction(
        bytes: &[u8],
        digest: &str,
        releases: &[DesktopRelease],
        staging_parent: &Path,
        backend: &mut impl Backend,
        events: &mut impl FnMut(DesktopInstallEvent) -> Result<(), String>,
    ) -> Result<InstalledDesktop, String> {
        run_transaction(
            bytes,
            digest,
            staging_parent,
            backend,
            events,
            &|host, id| authorize(host, releases, id),
        )
    }

    // The only production caller supplies authorize() above. Test-only native
    // proof injects a local artifact policy here, never into the shipped helper.
    pub(super) fn run_transaction(
        bytes: &[u8],
        digest: &str,
        staging_parent: &Path,
        backend: &mut impl Backend,
        events: &mut impl FnMut(DesktopInstallEvent) -> Result<(), String>,
        authorize_host: &impl Fn(&DesktopHost, &str) -> Result<(DesktopRelease, bool), String>,
    ) -> Result<InstalledDesktop, String> {
        events(DesktopInstallEvent::Preparing)?;
        let request = parse_request(bytes, digest)?;
        let initial_host = backend.probe()?;
        let (release, _) = authorize_host(&initial_host, &request.release_id)?;
        backend.check_chroot()?;
        let mut staging = stage_package(&request.package_input, &release, staging_parent)?;
        backend.validate_package(initial_host.package_manager?, &staging.path, &release)?;
        verify_bytes(
            File::open(&staging.path).map_err(|_| "Cannot reopen private package.")?,
            &release,
        )?;
        // Re-read all authority just before the mutation; cached UI readiness
        // and the unprivileged input pathname never authorize a package command.
        let request = parse_request(bytes, digest)?;
        let host = backend.probe()?;
        let (current, installed) = authorize_host(&host, &request.release_id)?;
        if current.sha256 != release.sha256
            || current.version != release.version
            || current.byte_size != release.byte_size
        {
            return Err("Desktop release changed during preparation.".into());
        }
        backend.check_chroot()?;
        let manager = host.package_manager?;
        let mut diagnostic_channel = Ok(());
        if !installed {
            // A disconnected sink before mutation aborts. After this point a
            // sink failure is remembered, but MUST NOT cancel the transaction.
            events(DesktopInstallEvent::Installing)?;
            staging.preserve_diagnostics();
            let outcome = backend.install(manager, &staging.path);
            if let Some(parent) = staging.path.parent() {
                let log = parent.join("native.log");
                if log.is_file() {
                    // Losing diagnostics must not interfere with verification
                    // or the terminal native transaction result.
                    diagnostic_channel = events(DesktopInstallEvent::DiagnosticLog(log));
                }
            }
            outcome?;
        }
        let channel = events(DesktopInstallEvent::Verifying);
        let installed = backend.verify(manager, &release)?;
        diagnostic_channel?;
        channel?;
        events(DesktopInstallEvent::Complete(installed.clone()))?;
        Ok(installed)
    }

    pub(super) struct StagedPackage {
        pub path: PathBuf,
        directory: Option<tempfile::TempDir>,
        retain_diagnostics: bool,
    }

    impl StagedPackage {
        pub(super) fn preserve_diagnostics(&mut self) {
            self.retain_diagnostics = true;
        }
    }

    impl Drop for StagedPackage {
        fn drop(&mut self) {
            if self.retain_diagnostics {
                // Keep the small protected log, not a second package cache.
                let _ = std::fs::remove_file(&self.path);
                if let Some(directory) = self.directory.take() {
                    let _ = directory.keep();
                }
            }
        }
    }

    pub(super) fn pin_input(input: &Path) -> Result<File, String> {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            // O_PATH pins the inode without opening a device or FIFO for IO.
            // Only a proven regular inode is subsequently opened for reading.
            let inode = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(input)
                .map_err(|_| "Cannot pin Desktop package inode.")?;
            if !inode
                .metadata()
                .map_err(|_| "Cannot inspect pinned package inode.")?
                .is_file()
            {
                return Err("Desktop package input is not a regular file.".into());
            }
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(format!("/proc/self/fd/{}", inode.as_raw_fd()))
                .map_err(|_| "Cannot read pinned regular package inode.".into())
        }
        #[cfg(not(target_os = "linux"))]
        {
            // This branch exists only for Unix unit tests; production helper
            // execution is Linux-only and always uses O_PATH above.
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(input)
                .map_err(|_| "Cannot pin test package input.")?;
            if !file
                .metadata()
                .map_err(|_| "Cannot inspect test package input.")?
                .is_file()
            {
                return Err("Desktop package input is not a regular file.".into());
            }
            Ok(file)
        }
    }

    pub(super) fn stage_package(
        input: &Path,
        release: &DesktopRelease,
        parent: &Path,
    ) -> Result<StagedPackage, String> {
        let mut pinned = pin_input(input)?;
        let metadata = pinned
            .metadata()
            .map_err(|_| "Cannot inspect Desktop package input.")?;
        if !metadata.is_file() || metadata.len() != release.byte_size {
            return Err("Desktop input is not a regular package of the authenticated size.".into());
        }
        let directory = tempfile::Builder::new()
            .prefix("tilo-desktop-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in(parent)
            .map_err(|_| "Cannot create private package staging.")?;
        let metadata = directory
            .path()
            .symlink_metadata()
            .map_err(|_| "Cannot inspect private staging.")?;
        // Production calls this only after asserting effective uid zero.
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err("Package staging ownership or permissions are unsafe.".into());
        }
        let path = directory
            .path()
            .join(format!("package.{}", release.package_format.extension()));
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|_| "Cannot create private package.")?;
        let mut hash = Sha256::new();
        let mut count = 0u64;
        let mut buffer = [0u8; 8192];
        loop {
            let n = pinned
                .read(&mut buffer)
                .map_err(|_| "Cannot read pinned package.")?;
            if n == 0 {
                break;
            }
            count = count
                .checked_add(n as u64)
                .ok_or("Package size overflow.")?;
            if count > release.byte_size {
                return Err("Package exceeds authenticated size.".into());
            }
            let chunk = buffer.get(..n).ok_or("Invalid package read size.")?;
            hash.update(chunk);
            output
                .write_all(chunk)
                .map_err(|_| "Cannot write private package.")?;
        }
        if count != release.byte_size || hex::encode(hash.finalize()) != release.sha256 {
            return Err(
                "Desktop package digest or size does not match the authenticated release.".into(),
            );
        }
        output
            .sync_all()
            .map_err(|_| "Cannot synchronize private package.")?;
        Ok(StagedPackage {
            path,
            directory: Some(directory),
            retain_diagnostics: false,
        })
    }

    fn verify_bytes(mut input: File, release: &DesktopRelease) -> Result<(), String> {
        /// `sha2` hashers do not implement `io::Write`; stream into one here.
        struct HashWriter<'a>(&'a mut Sha256);
        impl std::io::Write for HashWriter<'_> {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.update(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let mut hash = Sha256::new();
        let size = std::io::copy(
            &mut Read::by_ref(&mut input).take(release.byte_size + 1),
            &mut HashWriter(&mut hash),
        )
        .map_err(|_| "Cannot verify private package.")?;
        if size != release.byte_size || hex::encode(hash.finalize()) != release.sha256 {
            return Err("Private package failed final authentication.".into());
        }
        Ok(())
    }
}

#[cfg(any(target_os = "linux", all(test, unix)))]
#[cfg_attr(
    not(target_os = "linux"),
    allow(
        dead_code,
        reason = "Linux installer adapters are compiled for Unix tests even when their production entry points are unavailable"
    )
)]
mod native {
    use super::super::desktop_catalog::DesktopRelease;
    use super::super::desktop_probe::{
        DesktopHost, InstalledPackage, PackageManager, probe_desktop_host,
    };
    use super::{Backend, InstalledDesktop, SESSION_ENTRY};
    use std::{
        ffi::OsString,
        fs::OpenOptions,
        io::Read,
        os::{
            fd::AsRawFd,
            unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        },
        path::Path,
        process::{Command, Stdio},
        time::Duration,
    };

    const WRAPPER: &str = "/usr/libexec/tilo-session";
    const RUNNER: &str = "/usr/libexec/tilo-session-runner";
    const OUTPUT_LIMIT: usize = 64 * 1024;
    pub(super) struct NativeBackend;

    impl Backend for NativeBackend {
        fn probe(&mut self) -> Result<DesktopHost, String> {
            probe_desktop_host()
                .map_err(|_| "Cannot start independent host probe.")?
                .join()
                .map_err(|_| "Independent host probe failed.".into())
        }
        fn check_chroot(&mut self) -> Result<(), String> {
            let output = run_command("/usr/bin/systemd-detect-virt", &["--chroot".into()], true)?;
            if output.code == 1 && output.stderr.is_empty() && output.stdout.is_empty() {
                Ok(())
            } else {
                Err("Privileged helper cannot establish absence of chroot confinement.".into())
            }
        }
        fn validate_package(
            &mut self,
            manager: PackageManager,
            staged: &Path,
            release: &DesktopRelease,
        ) -> Result<(), String> {
            let (program, mut args): (&str, Vec<std::ffi::OsString>) = match manager {
                PackageManager::Dnf => (
                    "/usr/bin/rpm",
                    [
                        "-qp",
                        "--qf",
                        "%{NAME}\t%{EPOCHNUM}\t%{VERSION}\t%{RELEASE}\t%{ARCH}\n",
                        "--",
                    ]
                    .map(Into::into)
                    .to_vec(),
                ),
                PackageManager::Apt => (
                    "/usr/bin/dpkg-deb",
                    [
                        "--show",
                        "--showformat=${Package}\t${Version}\t${Architecture}\n",
                        "--",
                    ]
                    .map(Into::into)
                    .to_vec(),
                ),
                PackageManager::Pacman => {
                    ("/usr/bin/pacman", ["-Qip", "--"].map(Into::into).to_vec())
                }
            };
            args.push(staged.as_os_str().into());
            let output = run_command(program, &args, true)?;
            // RPM may warn about an unknown signing key while reading headers;
            // installation still enforces the unchanged native signature policy.
            if output.code != 0 {
                return Err("Cannot query staged native package identity.".into());
            }
            validate_package_identity(manager, &output.stdout, release)
        }
        fn install(&mut self, manager: PackageManager, staged: &Path) -> Result<(), String> {
            let (program, args) = install_command(manager, staged);
            let log = staged
                .parent()
                .ok_or("Missing private operation directory.")?
                .join("native.log");
            run_install_command(program, &args, &log)
        }
        fn verify(
            &mut self,
            manager: PackageManager,
            release: &DesktopRelease,
        ) -> Result<InstalledDesktop, String> {
            let host = self.probe()?;
            if host.installed_package
                != Ok(InstalledPackage::Installed {
                    version: release.version.clone(),
                })
            {
                return Err(
                    "Installed native package version does not match the selected release.".into(),
                );
            }
            for path in [SESSION_ENTRY, WRAPPER, RUNNER] {
                let (program, args): (&str, Vec<OsString>) = match manager {
                    PackageManager::Dnf => (
                        "/usr/bin/rpm",
                        ["-qf", "--qf", "%{NAME}\n", "--", path]
                            .map(Into::into)
                            .to_vec(),
                    ),
                    PackageManager::Apt => {
                        ("/usr/bin/dpkg-query", ["-S", path].map(Into::into).to_vec())
                    }
                    PackageManager::Pacman => (
                        "/usr/bin/pacman",
                        ["-Qqo", "--", path].map(Into::into).to_vec(),
                    ),
                };
                let result = run_command(program, &args, true)?;
                let expected = if manager == PackageManager::Apt {
                    format!("tilo-session: {path}\n")
                } else {
                    "tilo-session\n".into()
                };
                if result.code != 0 || !result.stderr.is_empty() || result.stdout != expected {
                    return Err(
                        "The Tilo session entry or wrapper is not exclusively package-owned."
                            .into(),
                    );
                }
            }
            let (program, args) = match manager {
                PackageManager::Dnf => ("/usr/bin/rpm", vec!["-V".into(), "tilo-session".into()]),
                PackageManager::Apt => (
                    "/usr/bin/dpkg",
                    vec!["--verify".into(), "tilo-session".into()],
                ),
                PackageManager::Pacman => (
                    "/usr/bin/pacman",
                    vec!["-Qkk".into(), "tilo-session".into()],
                ),
            };
            let result = run_command(program, &args, true)?;
            if result.code != 0
                || !result.stderr.is_empty()
                || (manager != PackageManager::Pacman && !result.stdout.is_empty())
            {
                return Err("Native verification found altered or missing package files.".into());
            }
            validate_session(
                &read_installed(SESSION_ENTRY, false)?,
                &read_installed(WRAPPER, true)?,
                &read_installed(RUNNER, true)?,
            )?;
            Ok(InstalledDesktop {
                version: release.version.clone(),
                session_entry: SESSION_ENTRY.into(),
            })
        }
    }

    pub(super) fn validate_package_identity(
        manager: PackageManager,
        text: &str,
        release: &DesktopRelease,
    ) -> Result<(), String> {
        let valid = match manager {
            PackageManager::Dnf => {
                if text.lines().count() == 1 {
                    let parts: Vec<_> = text.trim_end_matches('\n').split('\t').collect();
                    match parts.as_slice() {
                        ["tilo-session", epoch, version, revision, arch]
                            if !epoch.is_empty() && epoch.bytes().all(|b| b.is_ascii_digit()) =>
                        {
                            let actual = if *epoch == "0" {
                                format!("{version}-{revision}")
                            } else {
                                format!("{epoch}:{version}-{revision}")
                            };
                            actual == release.version && *arch == release.architecture
                        }
                        _ => false,
                    }
                } else {
                    false
                }
            }
            PackageManager::Apt => {
                let arch = match release.architecture.as_str() {
                    "aarch64" => "arm64",
                    "x86_64" => "amd64",
                    _ => return Err("Unsupported Debian architecture.".into()),
                };
                text == format!("tilo-session\t{}\t{arch}\n", release.version)
            }
            PackageManager::Pacman => {
                let mut fields = std::collections::HashMap::new();
                for line in text.lines() {
                    if let Some((key, value)) = line.split_once(':')
                        && fields.insert(key.trim(), value.trim()).is_some()
                    {
                        return Err("Ambiguous Arch package identity.".into());
                    }
                }
                fields.get("Name") == Some(&"tilo-session")
                    && fields.get("Version") == Some(&release.version.as_str())
                    && fields.get("Architecture") == Some(&release.architecture.as_str())
            }
        };
        if valid {
            Ok(())
        } else {
            Err("Native package name, version or architecture disagrees with the authenticated release.".into())
        }
    }

    pub(super) fn install_command(
        manager: PackageManager,
        staged: &Path,
    ) -> (&'static str, Vec<OsString>) {
        let (program, flags): (&str, &[&str]) = match manager {
            // Clearing key sources is restrictive: preserve every configured
            // signature-check setting but refuse importing new trust with -y.
            PackageManager::Dnf => (
                "/usr/bin/dnf",
                &["--assumeyes", "--setopt=*.gpgkey=", "install", "--"],
            ),
            PackageManager::Apt => (
                "/usr/bin/apt-get",
                &["--assume-yes", "--no-remove", "install", "--"],
            ),
            // Upstream cb_question inverses default YES for this fixed mask:
            // IMPORT_KEY (64) | REPLACE_PKG (2) | INSTALL_IGNOREPKG (1).
            PackageManager::Pacman => (
                "/usr/bin/pacman",
                &["--noconfirm", "--ask=67", "--upgrade", "--"],
            ),
        };
        let mut args: Vec<OsString> = flags.iter().map(Into::into).collect();
        args.push(staged.as_os_str().into());
        (program, args)
    }

    pub(super) fn validate_session(entry: &str, wrapper: &str, runner: &str) -> Result<(), String> {
        let mut desktop = false;
        let mut exec = None;
        let mut kind = None;
        for line in entry
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
        {
            if line.starts_with('[') {
                desktop = line == "[Desktop Entry]";
                continue;
            }
            if desktop {
                if let Some(value) = line.strip_prefix("Exec=")
                    && exec.replace(value).is_some()
                {
                    return Err("Ambiguous session Exec.".into());
                }
                if let Some(value) = line.strip_prefix("Type=")
                    && kind.replace(value).is_some()
                {
                    return Err("Ambiguous session Type.".into());
                }
            }
        }
        if exec != Some(WRAPPER)
            || kind != Some("Application")
            || !wrapper
                .lines()
                .any(|line| line.trim() == "TILO_SHELL=/usr/bin/tilo-shell")
            || !wrapper
                .lines()
                .any(|line| line.trim() == "SESSION_RUNNER=/usr/libexec/tilo-session-runner")
            || !wrapper
                .lines()
                .any(|line| line.trim() == r#"exec "$SESSION_RUNNER" "$TILO_SHELL" "$@""#)
            || !runner.lines().collect::<Vec<_>>().windows(2).any(|lines| {
                lines[0].trim()
                    == r#""$TILO_SHELL" --host-desktop-session --firstboot --backend drm "$@" \"#
                    && lines[1].trim() == r#">"$flight_pipe" 2>&1 &"#
            })
        {
            return Err(
                "Installed session does not use the reviewed Tilo host-session wrapper contract."
                    .into(),
            );
        }
        Ok(())
    }

    fn read_installed(path: &str, executable: bool) -> Result<String, String> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(|_| "Cannot pin installed session file.")?;
        let metadata = file
            .metadata()
            .map_err(|_| "Cannot inspect installed session file.")?;
        let mode = metadata.permissions().mode();
        if !metadata.is_file()
            || metadata.uid() != 0
            || mode & 0o022 != 0
            || (executable && mode & 0o111 == 0)
        {
            return Err("Installed session file ownership or permissions are unsafe.".into());
        }
        let mut text = String::new();
        file.take(OUTPUT_LIMIT as u64 + 1)
            .read_to_string(&mut text)
            .map_err(|_| "Cannot read installed session contract.")?;
        if text.len() > OUTPUT_LIMIT {
            return Err("Installed session file exceeds its bound.".into());
        }
        Ok(text)
    }

    pub(super) struct Output {
        pub code: i32,
        pub stdout: String,
        pub stderr: String,
    }

    pub(super) fn run_command(
        program: &str,
        args: &[OsString],
        read_only: bool,
    ) -> Result<Output, String> {
        run_command_with_capture_check(program, args, read_only, || Ok(()))
    }

    pub(super) fn run_command_with_capture_check(
        program: &str,
        args: &[OsString],
        read_only: bool,
        mut capture_check: impl FnMut() -> Result<(), String>,
    ) -> Result<Output, String> {
        // Configure capture before spawning: a setup failure must never leave
        // a begun package transaction with closed or undrained output sinks.
        let (mut stdout, stdout_writer) =
            std::io::pipe().map_err(|_| "Cannot create native stdout capture.")?;
        let (mut stderr, stderr_writer) =
            std::io::pipe().map_err(|_| "Cannot create native stderr capture.")?;
        for fd in [stdout.as_raw_fd(), stderr.as_raw_fd()] {
            // SAFETY: both descriptors belong to live pipe readers held here.
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
            {
                return Err("Cannot configure command output capture.".into());
            }
        }
        let mut child = Command::new(program)
            .args(args)
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .env("LC_ALL", "C")
            .env("HOME", "/root")
            .env("DEBIAN_FRONTEND", "noninteractive")
            .stdin(Stdio::null())
            .stdout(stdout_writer)
            .stderr(stderr_writer)
            .spawn()
            .map_err(|_| format!("Cannot execute {program}."))?;
        // Readers belong to this outer scope, not the fallible capture work.
        // Keep draining after errors and do not close either pipe until the
        // child is reaped: closing is an implicit SIGPIPE/EPIPE cancellation.
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut overflow = false;
        let mut capture_error = None;
        let start = ui::time::now();
        let capture = loop {
            for result in [
                capture_check(),
                drain(&mut stdout, &mut out, &mut overflow),
                drain(&mut stderr, &mut err, &mut overflow),
            ] {
                if let Err(error) = result {
                    capture_error.get_or_insert(error);
                }
            }
            if read_only && let Some(error) = capture_error.take() {
                break Err(error);
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    for result in [
                        drain(&mut stdout, &mut out, &mut overflow),
                        drain(&mut stderr, &mut err, &mut overflow),
                    ] {
                        if let Err(error) = result {
                            capture_error.get_or_insert(error);
                        }
                    }
                    if let Some(error) = capture_error {
                        break Err(error);
                    }
                    if read_only && overflow {
                        break Err("Read-only command exceeded its output bound.".into());
                    }
                    break Ok(Output {
                        code: status.code().unwrap_or(-1),
                        stdout: String::from_utf8_lossy(&out).into_owned(),
                        stderr: String::from_utf8_lossy(&err).into_owned(),
                    });
                }
                Ok(None) => (),
                Err(_) => {
                    capture_error.get_or_insert_with(|| "Cannot track native command.".into());
                }
            }
            if read_only && (start.elapsed() > Duration::from_secs(15) || overflow) {
                break Err("Read-only command exceeded its time/output bound.".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        if capture.is_err() {
            // Only read-only queries may be killed. Native errors reach this
            // point only after try_wait has established the terminal status.
            if read_only {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
        capture
    }

    fn drain(
        reader: &mut impl Read,
        output: &mut Vec<u8>,
        overflow: &mut bool,
    ) -> Result<(), String> {
        let mut buffer = [0; 4096];
        // Bound each pass even if the writer emits continuously.
        for _ in 0..64 {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    let retain = n.min(OUTPUT_LIMIT.saturating_sub(output.len()));
                    output.extend_from_slice(buffer.get(..retain).ok_or("Invalid output length.")?);
                    *overflow |= retain != n;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => (),
                Err(_) => return Err("Cannot capture native command diagnostics.".into()),
            }
        }
        Ok(())
    }

    pub(super) fn run_install_command(
        program: &str,
        args: &[OsString],
        log: &Path,
    ) -> Result<(), String> {
        use std::io::Write;
        // The path is derived solely from the helper-created private operation
        // directory. Exclusive creation prevents overwriting any existing name.
        let mut log_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(log)
            .map_err(|_| "Cannot create protected native diagnostics.")?;
        if let Ok(output) = run_command(program, args, false) {
            // Both captures are independently bounded at OUTPUT_LIMIT;
            // excess output is drained/discarded, never used to kill a
            // transaction or grow the protected log without bound.
            let saved = log_file
                .write_all(b"stdout (bounded):\n")
                .and_then(|()| log_file.write_all(output.stdout.as_bytes()))
                .and_then(|()| log_file.write_all(b"\nstderr (bounded):\n"))
                .and_then(|()| log_file.write_all(output.stderr.as_bytes()))
                .and_then(|()| log_file.sync_all());
            if output.code != 0 {
                return Err(format!(
                    "Native package transaction failed with exit code {}. See protected diagnostics.",
                    output.code
                ));
            }
            saved.map_err(|_| {
                "Native transaction finished but protected diagnostics could not be saved.".into()
            })
        } else {
            let _ = log_file
                .write_all(b"Native command capture failed; terminal success is unconfirmed.\n");
            Err("Native package transaction could not be tracked to verified success. See protected diagnostics.".into())
        }
    }
}

#[cfg(target_os = "linux")]
pub(super) fn run_native_helper(bytes: &[u8], digest: &str) -> i32 {
    use std::io::Write;
    // SAFETY: geteuid has no preconditions and does not mutate process state.
    if unsafe { libc::geteuid() } != 0 {
        return 1;
    }
    // A stopped/lost UI must not block the privileged transaction on progress.
    // SAFETY: fd 1 is the process's stdout. Failure refuses before mutation.
    let flags = unsafe { libc::fcntl(libc::STDOUT_FILENO, libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(libc::STDOUT_FILENO, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return 1;
    }
    let mut stdout = std::io::stdout().lock();
    let mut emit = |event| {
        let mut frame = serde_json::to_vec(&event)
            .map_err(|_| "Cannot encode Desktop progress.".to_string())?;
        frame.push(b'\n');
        stdout
            .write_all(&frame)
            .and_then(|()| stdout.flush())
            .map_err(|_| "Desktop progress channel closed.".to_string())
    };
    let result = super::desktop_catalog::bundled_releases().and_then(|releases| {
        transaction(
            bytes,
            digest,
            &releases,
            std::path::Path::new("/var/tmp"),
            &mut native::NativeBackend,
            &mut emit,
        )
    });
    match result {
        Ok(_) => 0,
        Err(reason) => {
            let _ = emit(DesktopInstallEvent::Failed(reason));
            1
        }
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::super::desktop_catalog::{DesktopRelease, PackageFormat, RuntimeEvidence};
    use super::super::desktop_probe::{
        DesktopHost, InstalledPackage, OsRelease, PackageManager, SessionSupport,
    };
    use super::*;
    use sha2::{Digest, Sha256};
    use std::path::Path;

    fn release() -> DesktopRelease {
        let digest = hex::encode(Sha256::digest(b"package"));
        DesktopRelease {
            id: "fedora-test".into(),
            version: "0.1.9-1.fc44".into(),
            distro_id: "fedora".into(),
            distro_version: "44".into(),
            architecture: "aarch64".into(),
            package_format: PackageFormat::Rpm,
            url: "https://code.tilo.so/tilo/test.rpm".into(),
            byte_size: 7,
            sha256: digest.clone(),
            verified_runtime: Some(RuntimeEvidence {
                schema: 1,
                reference: "test fixture only".into(),
                package_sha256: digest,
                graphical_login: true,
                return_to_prior_session: true,
                identity_setup: true,
                selected_folder_import: true,
                source_intact: true,
            }),
        }
    }
    fn host() -> DesktopHost {
        DesktopHost {
            operating_system: "linux".into(),
            os_release: Ok(OsRelease {
                id: "fedora".into(),
                version_id: Some("44".into()),
                name: Some("Fedora Linux".into()),
            }),
            architecture: Ok("aarch64".into()),
            package_manager: Ok(PackageManager::Dnf),
            immutable: Ok(false),
            sandboxed: Ok(false),
            session: Ok(SessionSupport {
                display_manager: "gdm.service".into(),
                other_wayland_sessions: vec!["gnome.desktop".into()],
            }),
            installed_package: Ok(InstalledPackage::Absent),
        }
    }
    struct Recording {
        host: DesktopHost,
        installs: usize,
        verifications: usize,
        reject_install: bool,
        change_after_metadata: bool,
        log_diagnostics: bool,
    }
    impl Recording {
        fn new() -> Self {
            Self {
                host: host(),
                installs: 0,
                verifications: 0,
                reject_install: false,
                change_after_metadata: false,
                log_diagnostics: false,
            }
        }
    }
    impl Backend for Recording {
        fn probe(&mut self) -> Result<DesktopHost, String> {
            Ok(self.host.clone())
        }
        fn check_chroot(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn validate_package(
            &mut self,
            _: PackageManager,
            _: &Path,
            _: &DesktopRelease,
        ) -> Result<(), String> {
            if self.change_after_metadata {
                self.host.sandboxed = Ok(true);
            }
            Ok(())
        }
        fn install(&mut self, _: PackageManager, path: &Path) -> Result<(), String> {
            if std::fs::read(path).map_err(|e| e.to_string())? != b"package" {
                return Err("Wrong staged bytes".into());
            }
            self.installs += 1;
            if self.log_diagnostics {
                std::fs::write(path.with_file_name("native.log"), "protected")
                    .map_err(|e| e.to_string())?;
            }
            if self.reject_install {
                return Err("Native exit 7".into());
            }
            Ok(())
        }
        fn verify(
            &mut self,
            _: PackageManager,
            _: &DesktopRelease,
        ) -> Result<InstalledDesktop, String> {
            self.verifications += 1;
            Ok(InstalledDesktop {
                version: "0.1.9-1.fc44".into(),
                session_entry: SESSION_ENTRY.into(),
            })
        }
    }
    /// Refusals, production catalog, package identity and pacman policy.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn install_refuses_untrusted_artifacts_and_identity_before_mutation() {
        // Phase `refusal_never_invokes_package_manager`.
        {
            for case in [
                "unknown",
                "digest",
                "wrong-host",
                "altered",
                "truncated",
                "sandbox",
                "no-runtime",
            ] {
                let temp = tempfile::tempdir().unwrap();
                let input = temp.path().join("input.rpm");
                std::fs::write(
                    &input,
                    if case == "altered" {
                        b"altered".as_slice()
                    } else if case == "truncated" {
                        b"pack".as_slice()
                    } else {
                        b"package".as_slice()
                    },
                )
                .unwrap();
                let mut release = release();
                if case == "no-runtime" {
                    release.verified_runtime = None;
                }
                let request = DesktopInstallRequest {
                    schema: 1,
                    release_id: if case == "unknown" {
                        "unknown".into()
                    } else {
                        release.id.clone()
                    },
                    package_input: input,
                };
                let bytes = serde_json::to_vec(&request).unwrap();
                let digest = if case == "digest" {
                    "0".repeat(64)
                } else {
                    request_digest(&bytes)
                };
                let mut backend = Recording::new();
                if case == "wrong-host" {
                    backend.host.architecture = Ok("x86_64".into());
                }
                if case == "sandbox" {
                    backend.host.sandboxed = Ok(true);
                }
                assert!(
                    transaction(
                        &bytes,
                        &digest,
                        &[release],
                        temp.path(),
                        &mut backend,
                        &mut |_| Ok(())
                    )
                    .is_err(),
                    "accepted {case}"
                );
                assert_eq!(backend.installs, 0, "mutated for {case}");
            }
        }

        // Phase `production_catalog_refuses_local_artifact`.
        {
            let mut backend = Recording::new();
            let request =
                br#"{"schema":1,"release_id":"local-artifact","package_input":"/tmp/local.rpm"}"#;
            assert!(
                transaction(
                    request,
                    &request_digest(request),
                    &super::super::desktop_catalog::bundled_releases().unwrap(),
                    Path::new("/tmp"),
                    &mut backend,
                    &mut |_| Ok(())
                )
                .is_err()
            );
            assert_eq!(backend.installs, 0);
        }

        // Phase `package_identity_must_match_catalog_before_mutation`.
        {
            let release = release();
            assert!(
                native::validate_package_identity(
                    PackageManager::Dnf,
                    "tilo-session\t0\t0.1.9\t1.fc44\taarch64\n",
                    &release
                )
                .is_ok()
            );
            for wrong in [
                "other\t0\t0.1.9\t1.fc44\taarch64\n",
                "tilo-session\t0\t0.1.8\t1.fc44\taarch64\n",
                "tilo-session\t0\t0.1.9\t1.fc44\tx86_64\n",
                "tilo-session\t0\t0.1.9\t1.fc44\taarch64\nextra",
            ] {
                assert!(
                    native::validate_package_identity(PackageManager::Dnf, wrong, &release)
                        .is_err()
                );
            }
        }

        // Phase `fixed_pacman_policy_refuses_default_yes_key_imports_and_replacements`.
        {
            let (_, args) = native::install_command(
                PackageManager::Pacman,
                Path::new("/private/package.pkg.tar.zst"),
            );
            // Upstream cb_question inverses defaults for the --ask mask. These
            // bits are IMPORT_KEY(64), REPLACE_PKG(2), INSTALL_IGNOREPKG(1).
            assert_eq!(
                args,
                [
                    "--noconfirm",
                    "--ask=67",
                    "--upgrade",
                    "--",
                    "/private/package.pkg.tar.zst"
                ]
            );
        }
    }
    /// Pinned staging inputs.
    #[test]
    fn pinned_staging_accepts_only_regular_nonsymlink_files() {
        // Phase `pinned_staging_rejects_symlinks_and_special_files`.
        {
            use std::os::unix::fs::symlink;
            let temp = tempfile::tempdir().unwrap();
            let input = temp.path().join("input");
            std::fs::write(&input, b"package").unwrap();
            let link = temp.path().join("link");
            symlink(&input, &link).unwrap();
            assert!(stage_package(&link, &release(), temp.path()).is_err());
            assert!(stage_package(temp.path(), &release(), temp.path()).is_err());
            let staged = stage_package(&input, &release(), temp.path()).unwrap();
            std::fs::write(&input, b"changed").unwrap();
            assert_eq!(std::fs::read(&staged.path).unwrap(), b"package");
        }

        // Phase `pinned_input_accepts_only_regular_nonsymlink_files`.
        {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("input");
            std::fs::write(&path, b"package").unwrap();
            assert!(pin_input(&path).is_ok());
            assert!(pin_input(Path::new("/dev/null")).is_err());
            assert!(pin_input(temp.path()).is_err());
            let link = temp.path().join("link");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            assert!(pin_input(&link).is_err());
        }
    }

    /// Lost progress/diagnostic channels and final probe ordering.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn install_verification_survives_lost_channels_and_never_fakes_success() {
        // Phase `install_finishes_verification_after_lost_progress_channel`.
        {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("input");
            std::fs::write(&path, b"package").unwrap();
            let bytes = serde_json::to_vec(&DesktopInstallRequest {
                schema: 1,
                release_id: release().id,
                package_input: path,
            })
            .unwrap();
            let mut backend = Recording::new();
            let mut phases = Vec::new();
            let outcome = transaction(
                &bytes,
                &request_digest(&bytes),
                &[release()],
                temp.path(),
                &mut backend,
                &mut |event| {
                    phases.push(event.clone());
                    if event == DesktopInstallEvent::Verifying {
                        Err("closed".into())
                    } else {
                        Ok(())
                    }
                },
            );
            assert!(outcome.is_err());
            assert_eq!(backend.installs, 1);
            assert_eq!(backend.verifications, 1);
            assert_eq!(
                phases,
                vec![
                    DesktopInstallEvent::Preparing,
                    DesktopInstallEvent::Installing,
                    DesktopInstallEvent::Verifying
                ]
            );
        }

        // Phase `final_probe_happens_after_package_metadata_and_no_failure_becomes_success`.
        {
            for changed in [true, false] {
                let temp = tempfile::tempdir().unwrap();
                let input = temp.path().join("input");
                std::fs::write(&input, b"package").unwrap();
                let bytes = serde_json::to_vec(&DesktopInstallRequest {
                    schema: 1,
                    release_id: release().id,
                    package_input: input,
                })
                .unwrap();
                let mut backend = Recording::new();
                backend.change_after_metadata = changed;
                backend.reject_install = !changed;
                let mut events = Vec::new();
                assert!(
                    transaction(
                        &bytes,
                        &request_digest(&bytes),
                        &[release()],
                        temp.path(),
                        &mut backend,
                        &mut |event| {
                            events.push(event);
                            Ok(())
                        }
                    )
                    .is_err()
                );
                assert_eq!(backend.installs, usize::from(!changed));
                assert_eq!(backend.verifications, 0);
                assert!(
                    !events
                        .iter()
                        .any(|event| matches!(event, DesktopInstallEvent::Complete(_)))
                );
            }
        }

        // Phase `a_lost_diagnostic_channel_still_verifies_but_never_completes`.
        {
            let temp = tempfile::tempdir().unwrap();
            let input = temp.path().join("input");
            std::fs::write(&input, b"package").unwrap();
            let bytes = serde_json::to_vec(&DesktopInstallRequest {
                schema: 1,
                release_id: release().id,
                package_input: input,
            })
            .unwrap();
            let mut backend = Recording::new();
            backend.log_diagnostics = true;
            let mut complete = false;
            let result = transaction(
                &bytes,
                &request_digest(&bytes),
                &[release()],
                temp.path(),
                &mut backend,
                &mut |event| {
                    if matches!(event, DesktopInstallEvent::Complete(_)) {
                        complete = true;
                    }
                    if matches!(event, DesktopInstallEvent::DiagnosticLog(_)) {
                        Err("lost channel".into())
                    } else {
                        Ok(())
                    }
                },
            );
            assert!(result.is_err());
            assert_eq!(backend.installs, 1);
            assert_eq!(backend.verifications, 1);
            assert!(!complete);
        }

        // Phase `capture_error_keeps_begun_transaction_pipes_alive_through_terminal_work`.
        {
            let temp = tempfile::tempdir().unwrap();
            let marker = temp.path().join("terminal-work-completed");
            let script = "set -e; sleep 0.05; /usr/bin/yes credential-bearing-stdout | /usr/bin/head -c 262144; /usr/bin/yes credential-bearing-stderr | /usr/bin/head -c 262144 >&2; : > \"$1\"";
            let mut injected = false;
            let result = native::run_command_with_capture_check(
                "/bin/sh",
                &[
                    "-c".into(),
                    script.into(),
                    "recording-transaction".into(),
                    marker.as_os_str().into(),
                ],
                false,
                || {
                    if std::mem::replace(&mut injected, true) {
                        Ok(())
                    } else {
                        Err("Injected capture failure.".into())
                    }
                },
            );
            assert!(injected);
            assert!(result.is_err(), "capture failure must never become success");
            assert!(
                marker.is_file(),
                "capture failure must not interrupt terminal native work"
            );
        }
    }

    /// Fixed native adapters, process recording and installed contract.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn native_commands_use_fixed_adapters_and_verify_the_installed_contract() {
        // Phase `native_commands_use_fixed_adapters_and_exact_staged_argument`.
        {
            let path = Path::new("/var/tmp/private/package with spaces.rpm");
            let (program, args) = native::install_command(PackageManager::Dnf, path);
            assert_eq!(program, "/usr/bin/dnf");
            assert_eq!(
                args,
                [
                    "--assumeyes",
                    "--setopt=*.gpgkey=",
                    "install",
                    "--",
                    "/var/tmp/private/package with spaces.rpm"
                ]
            );
            let (program, args) = native::install_command(PackageManager::Apt, path);
            assert_eq!(program, "/usr/bin/apt-get");
            assert_eq!(
                args,
                [
                    "--assume-yes",
                    "--no-remove",
                    "install",
                    "--",
                    "/var/tmp/private/package with spaces.rpm"
                ]
            );
            let (program, args) = native::install_command(PackageManager::Pacman, path);
            assert_eq!(program, "/usr/bin/pacman");
            assert_eq!(
                args,
                [
                    "--noconfirm",
                    "--ask=67",
                    "--upgrade",
                    "--",
                    "/var/tmp/private/package with spaces.rpm"
                ]
            );
        }

        // Phase `installed_contract_rejects_wrong_exec_or_missing_wrapper_flags`.
        {
            assert!(
                native::validate_session(
                    include_str!("../../../../dist/session/tilo.desktop"),
                    include_str!("../../../../dist/session/tilo-session"),
                    "#!/bin/sh\nexec tilo-shell --host-desktop-session"
                )
                .is_err()
            );

            assert!(
                native::validate_session(
                    "[Desktop Entry]\nExec=/usr/libexec/tilo-session\nType=Application\n",
                    include_str!("../../../../dist/session/tilo-session"),
                    include_str!("../../../../dist/session/tilo-session-runner")
                )
                .is_ok()
            );
            assert!(
                native::validate_session(
                    "[Desktop Entry]\nExec=/tmp/tilo-session\nType=Application\n",
                    include_str!("../../../../dist/session/tilo-session"),
                    include_str!("../../../../dist/session/tilo-session-runner")
                )
                .is_err()
            );
            assert!(native::validate_session("[Desktop Entry]\nExec=/usr/libexec/tilo-session\nExec=/tmp/evil\nType=Application\n", "--host-desktop-session --firstboot", "--host-desktop-session --firstboot").is_err());
            assert!(
                native::validate_session(
                    "[Desktop Entry]\nExec=/usr/libexec/tilo-session\nType=Application\n",
                    "#!/bin/sh\nexec tilo-shell --host-desktop-session",
                    include_str!("../../../../dist/session/tilo-session-runner")
                )
                .is_err()
            );
        }

        // Phase `recording_process_captures_terminal_status_without_shell_interpolation`.
        {
            let result =
                native::run_command("/usr/bin/printf", &["%s".into(), "a;$(false)".into()], true)
                    .unwrap();
            assert_eq!(result.stdout, "a;$(false)");
            assert_eq!(result.code, 0);
            let result = native::run_command("/usr/bin/false", &[], false).unwrap();
            assert_ne!(result.code, 0);
        }
    }

    /// Real native mutation: controller runs this only in a disposable guest.
    /// No runtime evidence is invented; the production catalog refusal is also
    /// checked. This test-only injection cannot be called by the binary helper.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires disposable Linux guest, root and explicit local artifact metadata"]
    fn native_desktop_install_local_artifact() {
        assert_eq!(unsafe { libc::geteuid() }, 0);
        let path = std::path::PathBuf::from(
            std::env::var_os("TILO_DESKTOP_TEST_ARTIFACT").expect("explicit local artifact"),
        );
        let digest =
            std::env::var("TILO_DESKTOP_TEST_SHA256").expect("independent artifact SHA256");
        let version = std::env::var("TILO_DESKTOP_TEST_VERSION").expect("native package version");
        let mut backend = native::NativeBackend;
        let host = backend.probe().unwrap();
        let os = host.os_release.clone().unwrap();
        let release = DesktopRelease {
            id: "local-native-proof".into(),
            version,
            distro_id: os.id,
            distro_version: os.version_id.unwrap_or_else(|| "rolling".into()),
            architecture: host.architecture.unwrap(),
            package_format: match host.package_manager.unwrap() {
                PackageManager::Dnf => PackageFormat::Rpm,
                PackageManager::Apt => PackageFormat::Deb,
                PackageManager::Pacman => PackageFormat::Arch,
            },
            url: "https://code.tilo.so/test-only-local-artifact".into(),
            byte_size: std::fs::metadata(&path).unwrap().len(),
            sha256: digest,
            verified_runtime: None,
        };
        let bytes = serde_json::to_vec(&DesktopInstallRequest {
            schema: 1,
            release_id: release.id.clone(),
            package_input: path,
        })
        .unwrap();
        assert!(
            transaction(
                &bytes,
                &request_digest(&bytes),
                &super::super::desktop_catalog::bundled_releases().unwrap(),
                Path::new("/var/tmp"),
                &mut backend,
                &mut |_| Ok(())
            )
            .is_err()
        );
        let outcome = run_transaction(
            &bytes,
            &request_digest(&bytes),
            Path::new("/var/tmp"),
            &mut backend,
            &mut |event| {
                println!("{}", serde_json::to_string(&event).unwrap());
                Ok(())
            },
            &|host, id| {
                // Reuse every production host/metadata gate, accepting ONLY the
                // deliberately absent runtime record for this local proof fixture.
                match super::super::desktop_probe::desktop_readiness(
                    host,
                    std::slice::from_ref(&release),
                ) {
                    super::super::desktop_probe::DesktopReadiness::Blocked(reasons)
                        if reasons
                            == [
                                "This package has no verified native graphical-session evidence.",
                            ]
                            && id == release.id =>
                    {
                        Ok((
                            release.clone(),
                            matches!(
                                host.installed_package,
                                Ok(InstalledPackage::Installed { .. })
                            ),
                        ))
                    }
                    state => Err(format!(
                        "Native proof fixture failed other readiness gates: {state:?}"
                    )),
                }
            },
        )
        .unwrap();
        assert_eq!(outcome.version, release.version);
        assert_eq!(outcome.session_entry, Path::new(SESSION_ENTRY));
        println!("Native package transaction verified; graphical login remains UNPROVED.");
    }
    /// Diagnostics privacy.
    #[test]
    fn native_diagnostics_stay_private_on_success_and_failure() {
        // Phase `native_diagnostics_stay_private_and_do_not_leak_credentials_to_failure`.
        {
            use std::os::unix::fs::PermissionsExt;
            let temp = tempfile::tempdir().unwrap();
            let log = temp.path().join("native.log");
            let error = native::run_install_command(
                "/bin/sh",
                &[
                    "-c".into(),
                    "printf 'credential=top-secret-value\\n' >&2; exit 7".into(),
                ],
                &log,
            )
            .unwrap_err();
            assert!(!error.contains("top-secret-value"));
            assert!(!error.contains("credential="));
            assert!(
                std::fs::read_to_string(&log)
                    .unwrap()
                    .contains("credential=top-secret-value")
            );
            assert_eq!(
                std::fs::metadata(log).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        // Phase `successful_transaction_preserves_only_private_diagnostics`.
        {
            let temp = tempfile::tempdir().unwrap();
            let input = temp.path().join("input");
            std::fs::write(&input, b"package").unwrap();
            let mut staged = stage_package(&input, &release(), temp.path()).unwrap();
            let directory = staged.path.parent().unwrap().to_owned();
            std::fs::write(directory.join("native.log"), "test diagnostic").unwrap();
            staged.preserve_diagnostics();
            drop(staged);
            assert!(directory.join("native.log").is_file());
            assert!(!directory.join("package.rpm").exists());
        }
    }
}
