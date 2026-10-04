//! Read-only Linux host discovery and exact Desktop release selection.
//!
//! The worker does not grant installation authority. The privileged adapter must
//! independently re-probe immediately before any future package operation.

use super::desktop_catalog::{DesktopRelease, PackageFormat};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OsRelease {
    pub id: String,
    /// Missing is distinct from a version; only exact ID=arch means rolling.
    pub version_id: Option<String>,
    /// The distribution's own name for people ("Fedora Linux"), when given.
    pub name: Option<String>,
}

/// Parse os-release as data, without expansion, execution, or `ID_LIKE` inheritance.
///
/// # Errors
/// Rejects duplicate keys, malformed assignments/quoting and invalid identities.
pub fn parse_os_release(input: &str) -> Result<OsRelease, String> {
    let mut fields = HashMap::new();
    for line in input.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, raw) = line
            .split_once('=')
            .ok_or("Malformed os-release assignment.")?;
        if key.is_empty()
            || !key
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            || key.as_bytes().first().is_some_and(u8::is_ascii_digit)
        {
            return Err("Invalid os-release key.".into());
        }
        let value = parse_value(raw)?;
        if fields.insert(key, value).is_some() {
            return Err("Duplicate os-release key.".into());
        }
    }
    let id = fields.remove("ID").ok_or("Missing os-release ID.")?;
    if id.is_empty()
        || !id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b))
    {
        return Err("Invalid os-release ID.".into());
    }
    let version_id = fields.remove("VERSION_ID");
    if version_id
        .as_ref()
        .is_some_and(|v| v.is_empty() || v.trim() != v || v.chars().any(char::is_control))
    {
        return Err("Invalid os-release VERSION_ID.".into());
    }
    // Only a display name: never used to choose a release.
    let name = fields
        .remove("NAME")
        .filter(|name| !name.trim().is_empty() && !name.chars().any(char::is_control));
    Ok(OsRelease {
        id,
        version_id,
        name,
    })
}

fn parse_value(raw: &str) -> Result<String, String> {
    let mut chars = raw.chars();
    let first = chars.next();
    let quote = first.filter(|c| matches!(c, '\'' | '"'));
    if let Some(quote) = quote {
        let mut result = String::new();
        while let Some(c) = chars.next() {
            if c == quote {
                return if chars.next().is_none() {
                    Ok(result)
                } else {
                    Err("Trailing os-release data.".into())
                };
            }
            if c.is_control() {
                return Err("Control character in os-release.".into());
            }
            if c == '\\' && quote == '"' {
                let escaped = chars.next().ok_or("Unterminated os-release escape.")?;
                if !matches!(escaped, '$' | '`' | '"' | '\\') {
                    result.push('\\');
                }
                result.push(escaped);
            } else {
                result.push(c);
            }
        }
        Err("Unterminated os-release quote.".into())
    } else if raw
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"._-/".contains(&b))
    {
        Ok(raw.into())
    } else {
        Err("Malformed unquoted os-release value.".into())
    }
}

/// Catalog architecture names are deliberate, not loose package-name aliases.
pub fn catalog_architecture(rust_architecture: &str) -> Option<&'static str> {
    match rust_architecture {
        "aarch64" => Some("aarch64"),
        "x86_64" => Some("x86_64"),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageManager {
    Dnf,
    Apt,
    Pacman,
}

impl PackageManager {
    fn format(self) -> PackageFormat {
        match self {
            Self::Dnf => PackageFormat::Rpm,
            Self::Apt => PackageFormat::Deb,
            Self::Pacman => PackageFormat::Arch,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstalledPackage {
    Absent,
    /// Exact native version: RPM [nonzero epoch:]version-release,
    /// Debian epoch:upstream-revision as emitted by dpkg, Arch pacman version.
    Installed {
        version: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSupport {
    pub display_manager: String,
    pub other_wayland_sessions: Vec<String>,
}

/// Fail-closed evidence snapshot. An Err is unavailable/ambiguous evidence, never false.
#[derive(Clone, Debug)]
pub struct DesktopHost {
    pub operating_system: String,
    pub os_release: Result<OsRelease, String>,
    pub architecture: Result<String, String>,
    pub package_manager: Result<PackageManager, String>,
    pub immutable: Result<bool, String>,
    pub sandboxed: Result<bool, String>,
    pub session: Result<SessionSupport, String>,
    pub installed_package: Result<InstalledPackage, String>,
}

#[derive(Clone, Debug)]
pub enum DesktopReadiness {
    Ready(DesktopRelease),
    AlreadyInstalled(DesktopRelease),
    Blocked(Vec<String>),
}

fn adapter(id: &str) -> Option<PackageManager> {
    match id {
        "fedora" => Some(PackageManager::Dnf),
        "ubuntu" | "debian" => Some(PackageManager::Apt),
        "arch" => Some(PackageManager::Pacman),
        _ => None,
    }
}

/// Select an exact authenticated release using only the supplied snapshot.
/// No IO is performed. Callers must supply the bundled authenticated catalog;
/// a successful result is informational and never privileged authorization.
pub fn desktop_readiness(host: &DesktopHost, releases: &[DesktopRelease]) -> DesktopReadiness {
    let mut blockers = Vec::new();
    if host.operating_system != "linux" {
        blockers.push("Desktop installation is available only on Linux.".into());
    }
    for (evidence, label) in [
        (&host.immutable, "immutable deployment"),
        (&host.sandboxed, "sandbox confinement"),
    ] {
        match evidence {
            Ok(false) => (),
            Ok(true) => blockers.push(format!("Desktop installation is unavailable with {label}; a reviewed host adapter is required.")),
            Err(reason) => blockers.push(format!("Could not establish {label}: {reason}")),
        }
    }
    match &host.session {
        Ok(session) if matches!(session.display_manager.as_str(), "gdm.service" | "gdm3.service" | "sddm.service" | "lightdm.service") && !session.other_wayland_sessions.is_empty() => (),
        Ok(_) => blockers.push("No supported login manager with an existing alternative Wayland session was established.".into()),
        Err(reason) => blockers.push(format!("Login session support could not be established: {reason}")),
    }
    if let Err(reason) = &host.installed_package {
        blockers.push(format!("Existing Tilo package status is unknown: {reason}"));
    }
    if let Err(reason) = &host.package_manager {
        blockers.push(format!("Native package manager is unavailable: {reason}"));
    }
    let selected = select_release(host, releases, &mut blockers);
    if let (Some(release), Ok(InstalledPackage::Installed { version })) =
        (selected, &host.installed_package)
        && version != &release.version
    {
        blockers.push(format!("Tilo package {version} is installed; selected release {} requires a reviewed upgrade/downgrade adapter.", release.version));
    }
    if !blockers.is_empty() {
        return DesktopReadiness::Blocked(blockers);
    }
    match (selected, &host.installed_package) {
        (Some(release), Ok(InstalledPackage::Absent)) => DesktopReadiness::Ready(release.clone()),
        (Some(release), Ok(InstalledPackage::Installed { .. })) => {
            DesktopReadiness::AlreadyInstalled(release.clone())
        }
        _ => DesktopReadiness::Blocked(vec!["Desktop readiness evidence is incomplete.".into()]),
    }
}

fn select_release<'a>(
    host: &DesktopHost,
    releases: &'a [DesktopRelease],
    blockers: &mut Vec<String>,
) -> Option<&'a DesktopRelease> {
    match (&host.os_release, &host.architecture) {
        (Ok(os), Ok(architecture)) => {
            let expected_manager = adapter(&os.id);
            if expected_manager.is_none() {
                blockers.push(if os.id == "nixos" {
                    "NixOS requires a reviewed declarative integration path.".into()
                } else {
                    format!(
                        "No Desktop installation adapter supports distribution {}.",
                        os.id
                    )
                });
            }
            if expected_manager != host.package_manager.as_ref().ok().copied() {
                blockers
                    .push("The native package manager does not match this distribution.".into());
            }
            let version = os.version_id.as_deref().or_else(|| {
                if os.id == "arch" {
                    Some("rolling")
                } else {
                    None
                }
            });
            if version.is_none() {
                blockers.push(
                    "The distribution has no VERSION_ID; its exact release cannot be established."
                        .into(),
                );
            }
            if catalog_architecture(architecture).is_none() {
                blockers.push("This native architecture is unsupported.".into());
            }
            let matches: Vec<_> = releases
                .iter()
                .filter(|r| r.matches_platform(&os.id, version.unwrap_or(""), architecture))
                .collect();
            match matches.as_slice() {
                [release] => {
                    if release.validate_metadata().is_err()
                        || super::desktop_catalog::trusted_url(&release.url).is_err()
                    {
                        blockers.push("The selected Desktop release metadata is invalid.".into());
                    }
                    if release.verified_runtime.is_none() {
                        blockers.push(
                            "This package has no verified native graphical-session evidence."
                                .into(),
                        );
                    }
                    if expected_manager.map(PackageManager::format) != Some(release.package_format)
                    {
                        blockers.push(
                            "The Desktop package format does not match the native package manager."
                                .into(),
                        );
                    }
                    Some(*release)
                }
                [] => {
                    blockers.push("No authenticated Desktop release matches this exact distribution, version, and architecture.".into());
                    None
                }
                _ => {
                    blockers.push(
                        "More than one Desktop release matches this host; selection is ambiguous."
                            .into(),
                    );
                    None
                }
            }
        }
        (os, arch) => {
            if let Err(reason) = os {
                blockers.push(format!("Distribution identity is unavailable: {reason}"));
            }
            if let Err(reason) = arch {
                blockers.push(format!("Native architecture is unavailable: {reason}"));
            }
            None
        }
    }
}

/// Start read-only discovery on a named worker. Poll `is_finished` or receive the
/// result on a controller worker; never join a running probe on the UI thread.
///
/// # Errors
/// Returns a thread creation error without doing host IO on the caller's thread.
pub fn probe_desktop_host() -> std::io::Result<std::thread::JoinHandle<DesktopHost>> {
    std::thread::Builder::new()
        .name("desktop-readiness".into())
        .spawn(|| {
            #[cfg(target_os = "linux")]
            {
                native::probe()
            }
            #[cfg(not(target_os = "linux"))]
            {
                let unavailable = || Err("Desktop host discovery requires Linux.".into());
                DesktopHost {
                    operating_system: std::env::consts::OS.into(),
                    os_release: unavailable(),
                    architecture: Err("Not a Linux host.".into()),
                    package_manager: Err("Not a Linux host.".into()),
                    immutable: Err("Not a Linux host.".into()),
                    sandboxed: Err("Not a Linux host.".into()),
                    session: Err("Not a Linux host.".into()),
                    installed_package: Err("Not a Linux host.".into()),
                }
            }
        })
}

#[cfg(any(target_os = "linux", test))]
fn parse_package_status(
    manager: PackageManager,
    code: i32,
    stdout: &str,
    stderr: &str,
) -> Result<InstalledPackage, String> {
    let absent = match manager {
        PackageManager::Dnf => {
            code == 1 && stdout == "package tilo-session is not installed\n" && stderr.is_empty()
        }
        PackageManager::Apt => {
            code == 1
                && stdout.is_empty()
                && stderr == "dpkg-query: no packages found matching tilo-session\n"
        }
        PackageManager::Pacman => {
            code == 1
                && stdout.is_empty()
                && stderr == "error: package 'tilo-session' was not found\n"
        }
    };
    if absent {
        return Ok(InstalledPackage::Absent);
    }
    if code != 0 || !stderr.is_empty() || stdout.lines().count() != 1 {
        return Err("Native package query failed or returned ambiguous results.".into());
    }
    let line = stdout.trim_end_matches('\n');
    let version = match manager {
        PackageManager::Dnf => {
            let fields: Vec<_> = line.split('\t').collect();
            match fields.as_slice() {
                [epoch, version, revision]
                    if !epoch.is_empty()
                        && epoch.bytes().all(|b| b.is_ascii_digit())
                        && !version.is_empty()
                        && !revision.is_empty() =>
                {
                    if *epoch == "0" {
                        format!("{version}-{revision}")
                    } else {
                        format!("{epoch}:{version}-{revision}")
                    }
                }
                _ => return Err("Invalid RPM version response.".into()),
            }
        }
        PackageManager::Apt => line
            .strip_prefix("installed\t")
            .ok_or("The Debian package is not fully installed.")?
            .into(),
        PackageManager::Pacman => line
            .strip_prefix("tilo-session ")
            .ok_or("Invalid pacman response.")?
            .into(),
    };
    if version.is_empty()
        || version.chars().any(char::is_whitespace)
        || version.chars().any(char::is_control)
    {
        return Err("Invalid native package version.".into());
    }
    Ok(InstalledPackage::Installed { version })
}

#[cfg(any(target_os = "linux", test))]
fn sandbox_environment_marker_present(mut present: impl FnMut(&str) -> bool) -> bool {
    [
        "FLATPAK_ID",
        "SNAP",
        "SNAP_NAME",
        "container",
        "TILO_IMAGER_FLATPAK",
    ]
    .into_iter()
    .any(&mut present)
}

// Also compile the read-only primitives on Unix test hosts for bounded IO tests.
#[cfg(any(target_os = "linux", all(test, unix)))]
#[cfg_attr(
    not(target_os = "linux"),
    allow(
        dead_code,
        reason = "non-Linux Unix tests compile these bounded IO primitives without using every Linux desktop probe"
    )
)]
mod native {
    use super::{
        DesktopHost, HashMap, InstalledPackage, OsRelease, PackageManager, SessionSupport, adapter,
        catalog_architecture, parse_os_release, parse_package_status,
    };
    use std::fs::{self, OpenOptions};
    use std::io::Read;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    use std::path::Path;
    use std::process::Command;
    use std::time::Duration;

    pub(super) struct QueryOutput {
        pub code: i32,
        pub stdout: String,
        pub stderr: String,
    }

    /// No shell or PATH resolution. Drain bounded, nonblocking pipes so a hung
    /// command or inherited pipe cannot strand the readiness worker.
    pub(super) fn query(
        program: &str,
        args: &[&str],
        timeout: Duration,
        limit: usize,
    ) -> Result<QueryOutput, String> {
        let mut command = Command::new(program);
        command
            .args(args)
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .env("LC_ALL", "C")
            .env("HOME", "/");
        let output =
            bounded_command::run_prepared(command, None, limit, timeout).map_err(|error| {
                match error {
                    bounded_command::Error::Timeout(_) => {
                        format!("Read-only query timed out: {program}.")
                    }
                    bounded_command::Error::OutputTooLarge { .. } => {
                        "Read-only query output exceeded its limit.".into()
                    }
                    _ => format!("Could not execute {program}."),
                }
            })?;
        Ok(QueryOutput {
            code: output
                .status
                .code()
                .ok_or("Query terminated without an exit code.")?,
            stdout: String::from_utf8(output.stdout)
                .map_err(|_| "Invalid query output encoding.")?,
            stderr: String::from_utf8(output.stderr)
                .map_err(|_| "Invalid query error encoding.")?,
        })
    }

    fn run(program: &str, args: &[&str]) -> Result<QueryOutput, String> {
        query(program, args, Duration::from_secs(5), 64 * 1024)
    }

    fn successful(program: &str, args: &[&str]) -> Result<String, String> {
        let output = run(program, args)?;
        if output.code != 0 || !output.stderr.is_empty() {
            return Err(format!("Read-only query failed: {program}."));
        }
        Ok(output.stdout)
    }

    pub(super) fn read_text(path: &Path) -> Result<String, String> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
            .map_err(|_| format!("Cannot read {}.", path.display()))?;
        if !file
            .metadata()
            .map_err(|_| "Cannot inspect host file.")?
            .is_file()
        {
            return Err("Host evidence is not a regular file.".into());
        }
        let mut text = String::new();
        file.take(64 * 1024 + 1)
            .read_to_string(&mut text)
            .map_err(|_| "Cannot decode host evidence.")?;
        if text.len() > 64 * 1024 {
            return Err("Host evidence exceeds the size limit.".into());
        }
        Ok(text)
    }

    fn marker(path: &str) -> Result<bool, String> {
        Path::new(path)
            .try_exists()
            .map_err(|_| format!("Cannot inspect {path}."))
    }

    fn manager(os: &Result<OsRelease, String>) -> Result<PackageManager, String> {
        let os = os.as_ref().map_err(Clone::clone)?;
        let manager = adapter(&os.id).ok_or("No reviewed native package-manager adapter.")?;
        let binaries: &[&str] = match manager {
            PackageManager::Dnf => &["/usr/bin/dnf", "/usr/bin/rpm"],
            PackageManager::Apt => &["/usr/bin/apt-get", "/usr/bin/dpkg-query"],
            PackageManager::Pacman => &["/usr/bin/pacman"],
        };
        for binary in binaries {
            let metadata = fs::metadata(binary)
                .map_err(|_| format!("Missing native package manager {binary}."))?;
            if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
                return Err(format!(
                    "Native package manager {binary} is not executable."
                ));
            }
        }
        Ok(manager)
    }

    fn installed(manager: &Result<PackageManager, String>) -> Result<InstalledPackage, String> {
        let manager = *manager.as_ref().map_err(Clone::clone)?;
        let result = match manager {
            PackageManager::Dnf => run(
                "/usr/bin/rpm",
                &[
                    "-q",
                    "--qf",
                    "%{EPOCHNUM}\t%{VERSION}\t%{RELEASE}\n",
                    "tilo-session",
                ],
            ),
            PackageManager::Apt => run(
                "/usr/bin/dpkg-query",
                &["-W", "-f=${db:Status-Status}\t${Version}\n", "tilo-session"],
            ),
            PackageManager::Pacman => run("/usr/bin/pacman", &["-Q", "tilo-session"]),
        }?;
        parse_package_status(manager, result.code, &result.stdout, &result.stderr)
    }

    fn immutable() -> Result<bool, String> {
        for path in [
            "/run/ostree-booted",
            "/ostree",
            "/sysroot/ostree",
            "/etc/NIXOS",
            "/run/current-system",
            "/run/booted-system",
        ] {
            if marker(path)? {
                return Ok(true);
            }
        }
        for path in [c"/", c"/usr"] {
            let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
            // SAFETY: path is NUL terminated and stat is valid writable storage.
            if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
                return Err("Cannot establish filesystem mutability.".into());
            }
            // SAFETY: a successful statvfs initializes the structure.
            if unsafe { stat.assume_init() }.f_flag & libc::ST_RDONLY != 0 {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(super) fn process_confined(text: &str) -> Result<bool, String> {
        let mut fields = HashMap::new();
        for line in text.lines() {
            if let Some((key, value)) = line.split_once(':')
                && fields.insert(key, value.trim()).is_some()
            {
                return Err("Ambiguous process confinement evidence.".into());
            }
        }
        let nnp = *fields
            .get("NoNewPrivs")
            .ok_or("Missing NoNewPrivs evidence.")?;
        let seccomp = *fields.get("Seccomp").ok_or("Missing Seccomp evidence.")?;
        if !matches!(nnp, "0" | "1") || !matches!(seccomp, "0" | "1" | "2") {
            return Err("Invalid process confinement evidence.".into());
        }
        Ok(nnp != "0" || seccomp != "0")
    }

    #[derive(Debug, PartialEq, Eq)]
    pub(super) struct RootMount {
        mount_id: u64,
        parent_id: u64,
        device: (u64, u64),
        root: String,
    }

    pub(super) fn mount_path(value: &str) -> Result<String, String> {
        let mut decoded = String::new();
        let mut chars = value.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                let escape: String = chars.by_ref().take(3).collect();
                decoded.push(match escape.as_str() {
                    "040" => ' ',
                    "011" => '\t',
                    "012" => '\n',
                    "134" => '\\',
                    _ => return Err("Invalid mount table path escape.".into()),
                });
            } else {
                decoded.push(c);
            }
        }
        Ok(decoded)
    }

    /// Kernel mountinfo mountpoints are relative to each process's root. Require
    /// one identical root mount for this process and PID 1, without following the
    /// permission-restricted /proc/1/root link. Missing evidence is not absence.
    pub(super) fn root_mount(text: &str) -> Result<RootMount, String> {
        let number = |value: &str| {
            value
                .parse::<u64>()
                .map_err(|_| "Invalid mount table number.".to_string())
        };
        let mut root = None;
        for line in text.lines() {
            let (before, after) = line
                .split_once(" - ")
                .ok_or("Missing mount table separator.")?;
            let fields: Vec<_> = before.split_ascii_whitespace().collect();
            let tail_count = after.split_ascii_whitespace().count();
            let [id, parent, device, mount_root, mountpoint, _options, ..] = fields.as_slice()
            else {
                return Err("Incomplete mount table record.".into());
            };
            if tail_count != 3 {
                return Err("Incomplete mount table filesystem record.".into());
            }
            let (major, minor) = device
                .split_once(':')
                .ok_or("Invalid mount table device.")?;
            let candidate = RootMount {
                mount_id: number(id)?,
                parent_id: number(parent)?,
                device: (number(major)?, number(minor)?),
                root: mount_path(mount_root)?,
            };
            let mountpoint = mount_path(mountpoint)?;
            if !candidate.root.starts_with('/') || !mountpoint.starts_with('/') {
                return Err("Invalid mount table path.".into());
            }
            if mountpoint == "/" && root.replace(candidate).is_some() {
                return Err("Ambiguous root mount evidence.".into());
            }
        }
        root.ok_or_else(|| "No visible root mount establishes the native host.".into())
    }

    fn sandboxed() -> Result<bool, String> {
        if super::sandbox_environment_marker_present(|variable| {
            std::env::var_os(variable).is_some()
        }) {
            return Ok(true);
        }
        for path in [
            "/.flatpak-info",
            "/.dockerenv",
            "/run/.containerenv",
            "/run/systemd/container",
        ] {
            if marker(path)? {
                return Ok(true);
            }
        }
        if process_confined(&read_text(Path::new("/proc/self/status"))?)? {
            return Ok(true);
        }
        if root_mount(&read_text(Path::new("/proc/self/mountinfo"))?)?
            != root_mount(&read_text(Path::new("/proc/1/mountinfo"))?)?
        {
            return Ok(true);
        }
        let output = run("/usr/bin/systemd-detect-virt", &["--container"])?;
        match output.code {
            0 => return Ok(true),
            1 if output.stderr.is_empty() && output.stdout.trim() == "none" => (),
            _ => {
                return Err(
                    "Could not establish whether this is the native host environment.".into(),
                );
            }
        }
        Ok(false)
    }

    pub(super) fn display_manager(text: &str) -> Result<String, String> {
        let mut fields = HashMap::new();
        for line in text.lines() {
            let (key, value) = line
                .split_once('=')
                .ok_or("Invalid display-manager evidence.")?;
            if fields.insert(key, value).is_some() {
                return Err("Ambiguous display-manager evidence.".into());
            }
        }
        let id = fields
            .get("Id")
            .ok_or("Missing display-manager identity.")?;
        if fields.get("LoadState") != Some(&"loaded")
            || fields.get("ActiveState") != Some(&"active")
            || !matches!(
                *id,
                "gdm.service" | "gdm3.service" | "sddm.service" | "lightdm.service"
            )
        {
            return Err("An active GDM, SDDM, or LightDM login manager is required.".into());
        }
        Ok((*id).into())
    }

    pub(super) fn wayland_sessions(directory: &Path) -> Result<Vec<String>, String> {
        let entries = fs::read_dir(directory).map_err(|_| "Cannot read Wayland login sessions.")?;
        let mut sessions = Vec::new();
        for (index, entry) in entries.enumerate() {
            if index >= 512 {
                return Err("Too many login entries to establish session support.".into());
            }
            let path = entry.map_err(|_| "Cannot inspect a login session.")?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("desktop")
                || path.file_name().and_then(|s| s.to_str()) == Some("tilo.desktop")
            {
                continue;
            }
            let text = read_text(&path)?;
            let mut in_entry = false;
            let mut fields = HashMap::new();
            for line in text.lines().map(str::trim) {
                if line.starts_with('[') {
                    in_entry = line == "[Desktop Entry]";
                } else if in_entry && !line.is_empty() && !line.starts_with('#') {
                    let (key, value) = line
                        .split_once('=')
                        .ok_or("Malformed Wayland session entry.")?;
                    if fields.insert(key, value).is_some() {
                        return Err("Ambiguous Wayland session entry.".into());
                    }
                }
            }
            if fields.get("Type") == Some(&"Application")
                && fields.get("Hidden") != Some(&"true")
                && fields.get("NoDisplay") != Some(&"true")
                && fields
                    .get("Exec")
                    .is_some_and(|v| !v.is_empty() && !v.contains("tilo-session"))
            {
                sessions.push(
                    path.file_name()
                        .and_then(|s| s.to_str())
                        .ok_or("Invalid session filename.")?
                        .into(),
                );
            }
        }
        sessions.sort();
        if sessions.is_empty() {
            return Err("No existing alternative Wayland login session was found.".into());
        }
        Ok(sessions)
    }

    pub(super) fn probe() -> DesktopHost {
        let os_release = (|| {
            let path = if marker("/etc/os-release")? {
                "/etc/os-release"
            } else {
                "/usr/lib/os-release"
            };
            parse_os_release(&read_text(Path::new(path))?)
        })();
        let architecture = (|| {
            let native = successful("/usr/bin/uname", &["-m"])?;
            let arch =
                catalog_architecture(native.trim()).ok_or("Unsupported native architecture.")?;
            if catalog_architecture(std::env::consts::ARCH) != Some(arch) {
                return Err(
                    "The setup application architecture differs from the native host.".into(),
                );
            }
            Ok(arch.into())
        })();
        let package_manager = manager(&os_release);
        let installed_package = installed(&package_manager);
        let session = (|| {
            let display_manager = display_manager(&successful(
                "/usr/bin/systemctl",
                &[
                    "show",
                    "display-manager.service",
                    "--property=Id,LoadState,ActiveState",
                    "--no-pager",
                ],
            )?)?;
            Ok(SessionSupport {
                display_manager,
                other_wayland_sessions: wayland_sessions(Path::new("/usr/share/wayland-sessions"))?,
            })
        })();
        DesktopHost {
            operating_system: std::env::consts::OS.into(),
            os_release,
            architecture,
            package_manager,
            immutable: immutable(),
            sandboxed: sandboxed(),
            session,
            installed_package,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sandbox markers, mount evidence, special files, runner bounds and confinement.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn host_evidence_refuses_sandboxes_chroots_and_special_files() {
        // Phase `sandbox_environment_markers_refuse_even_empty_present_values`.
        {
            for marker in [
                "FLATPAK_ID",
                "SNAP",
                "SNAP_NAME",
                "container",
                "TILO_IMAGER_FLATPAK",
            ] {
                let observations = HashMap::from([(marker, "")]);
                assert!(
                    sandbox_environment_marker_present(
                        |candidate| observations.contains_key(candidate)
                    ),
                    "ignored present sandbox marker {marker}"
                );
            }
            assert!(!sandbox_environment_marker_present(|_| false));
        }

        // Phase `mount_evidence_rejects_chroot_namespace_and_ambiguous_roots`.
        #[cfg(unix)]
        {
            let host = "45 1 8:3 / / rw,relatime shared:1 - xfs /dev/sda3 rw,seclabel\n";
            assert_eq!(
                native::root_mount(host).unwrap(),
                native::root_mount("45 1 8:3 / / rw - xfs /dev/sda3 rw\n").unwrap()
            );
            assert_ne!(
                native::root_mount(host).unwrap(),
                native::root_mount("45 1 8:3 /chroot / rw - xfs /dev/sda3 rw\n").unwrap()
            );
            assert_ne!(
                native::root_mount(host).unwrap(),
                native::root_mount("99 1 8:3 / / rw - xfs /dev/sda3 rw\n").unwrap()
            );
            assert_ne!(
                native::root_mount(host).unwrap(),
                native::root_mount("45 1 8:4 / / rw - xfs /dev/sda4 rw\n").unwrap()
            );
            for text in [
                "",
                "bad",
                "45 1 8:3 / /other rw - xfs /dev/sda3 rw\n",
                "45 1 8:3 /bad\\999 / rw - xfs /dev/sda3 rw\n",
                "45 1 8:3 / / rw - xfs\n",
                "xx 1 8:3 / / rw - xfs /dev/sda3 rw\n",
            ] {
                assert!(native::root_mount(text).is_err(), "accepted {text}");
            }
            assert!(native::root_mount(&format!("{host}{host}")).is_err());
            assert_eq!(
                native::mount_path(r"/hello\040world\134path").unwrap(),
                "/hello world\\path"
            );
        }

        // Phase `host_evidence_reader_refuses_special_files_without_waiting`.
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("fifo");
            let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
            // SAFETY: cpath is a live NUL-terminated filename in a private test directory.
            assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(native::read_text(&path));
            });
            assert!(
                rx.recv_timeout(tilo_test_env::hang_bound())
                    .expect("special host files must not block the probe")
                    .is_err()
            );
        }

        // Phase `read_only_command_runner_bounds_time_and_output`.
        #[cfg(unix)]
        {
            use std::time::Duration;
            let bound = tilo_test_env::hang_bound();
            let result = native::query("/usr/bin/printf", &["hello"], bound, 32).unwrap();
            assert_eq!(result.stdout, "hello");
            assert!(native::query("/usr/bin/printf", &["123456789"], bound, 4).is_err());
            let start = ui::time::now();
            let sleep = (bound.as_secs() * 2).to_string();
            assert!(native::query("/bin/sleep", &[&sleep], Duration::from_millis(25), 32).is_err());
            assert!(start.elapsed() < bound);
        }

        // Phase `session_and_confinement_evidence_fail_closed`.
        #[cfg(unix)]
        {
            assert_eq!(
                native::display_manager("Id=gdm.service\nLoadState=loaded\nActiveState=active\n")
                    .unwrap(),
                "gdm.service"
            );
            for text in [
                "",
                "Id=unknown.service\nLoadState=loaded\nActiveState=active",
                "Id=gdm.service\nLoadState=loaded\nActiveState=inactive",
                "Id=gdm.service\nId=sddm.service\nLoadState=loaded\nActiveState=active",
            ] {
                assert!(native::display_manager(text).is_err());
            }
            assert!(
                !native::process_confined("Name:\timager\nNoNewPrivs:\t0\nSeccomp:\t0\n").unwrap()
            );
            assert!(native::process_confined("NoNewPrivs:\t1\nSeccomp:\t0\n").unwrap());
            assert!(native::process_confined("NoNewPrivs:\t0\nSeccomp:\t2\n").unwrap());
            assert!(native::process_confined("Name:\timager\n").is_err());
            let temp = tempfile::tempdir().unwrap();
            assert!(native::wayland_sessions(temp.path()).is_err());
            std::fs::write(
                temp.path().join("tilo.desktop"),
                "[Desktop Entry]\nType=Application\nName=Tilo\nExec=tilo-session\n",
            )
            .unwrap();
            assert!(native::wayland_sessions(temp.path()).is_err());
            std::fs::write(
                temp.path().join("gnome.desktop"),
                "[Desktop Entry]\nType=Application\nName=GNOME\nExec=gnome-session\n",
            )
            .unwrap();
            assert_eq!(
                native::wayland_sessions(temp.path()).unwrap(),
                vec!["gnome.desktop"]
            );
        }
    }

    /// Package queries and installed version matching.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn native_package_status_and_installed_version_are_exact() {
        // Phase `native_package_status_refuses_query_errors_and_ambiguous_versions`.
        {
            assert_eq!(
                parse_package_status(PackageManager::Dnf, 0, "0\t1.0\t2.fc44\n", "").unwrap(),
                InstalledPackage::Installed {
                    version: "1.0-2.fc44".into()
                }
            );
            assert_eq!(
                parse_package_status(PackageManager::Dnf, 0, "2\t1.0\t2.fc44\n", "").unwrap(),
                InstalledPackage::Installed {
                    version: "2:1.0-2.fc44".into()
                }
            );
            assert_eq!(
                parse_package_status(
                    PackageManager::Dnf,
                    1,
                    "package tilo-session is not installed\n",
                    ""
                )
                .unwrap(),
                InstalledPackage::Absent
            );
            assert_eq!(
                parse_package_status(PackageManager::Apt, 0, "installed\t1:1.0-2\n", "").unwrap(),
                InstalledPackage::Installed {
                    version: "1:1.0-2".into()
                }
            );
            assert_eq!(
                parse_package_status(
                    PackageManager::Apt,
                    1,
                    "",
                    "dpkg-query: no packages found matching tilo-session\n"
                )
                .unwrap(),
                InstalledPackage::Absent
            );
            assert_eq!(
                parse_package_status(PackageManager::Pacman, 0, "tilo-session 1:1.0-2\n", "")
                    .unwrap(),
                InstalledPackage::Installed {
                    version: "1:1.0-2".into()
                }
            );
            assert_eq!(
                parse_package_status(
                    PackageManager::Pacman,
                    1,
                    "",
                    "error: package 'tilo-session' was not found\n"
                )
                .unwrap(),
                InstalledPackage::Absent
            );
            for (manager, status, stdout, stderr) in [
                (PackageManager::Dnf, 1, "", "error: rpmdb failed"),
                (PackageManager::Dnf, 0, "0\t1.0\t1\n0\t2.0\t1\n", ""),
                (PackageManager::Apt, 0, "config-files\t1.0-1\n", ""),
                (PackageManager::Apt, 0, "installed\t\n", ""),
                (PackageManager::Pacman, 0, "other 1.0-1\n", ""),
                (PackageManager::Pacman, 1, "", "error: database unavailable"),
            ] {
                assert!(parse_package_status(manager, status, stdout, stderr).is_err());
            }
        }

        // Phase `installed_version_is_exact_and_existing_different_versions_block`.
        {
            let mut host = fedora();
            host.installed_package = Ok(InstalledPackage::Installed {
                version: "1.0-1".into(),
            });
            assert!(matches!(
                desktop_readiness(&host, &[release()]),
                DesktopReadiness::AlreadyInstalled(_)
            ));
            host.installed_package = Ok(InstalledPackage::Installed {
                version: "1.0-2".into(),
            });
            blocked(&host, &[release()]);
        }
    }

    /// Readiness decisions.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn readiness_accepts_only_verified_ordinary_supported_hosts() {
        // Phase `worker_reports_real_platform_without_gui`.
        {
            let host = probe_desktop_host().unwrap().join().unwrap();
            assert_eq!(host.operating_system, std::env::consts::OS);
            if !cfg!(target_os = "linux") {
                blocked(&host, &[release()]);
            }
        }

        // Phase `readiness_requires_exact_release_and_verified_runtime`.
        {
            let host = fedora();
            let good = release();
            assert!(matches!(
                desktop_readiness(&host, &[good.clone()]),
                DesktopReadiness::Ready(_)
            ));
            blocked(
                &host,
                &super::super::desktop_catalog::bundled_releases().unwrap(),
            );
            let mut no_runtime = good.clone();
            no_runtime.verified_runtime = None;
            blocked(&host, &[no_runtime]);
            let mut wrong_runtime = good.clone();
            wrong_runtime
                .verified_runtime
                .as_mut()
                .unwrap()
                .package_sha256 = "b".repeat(64);
            blocked(&host, &[wrong_runtime]);
            blocked(&host, &[good.clone(), good.clone()]);
            for (id, version, arch) in [
                ("example", "44", "aarch64"),
                ("fedora", "44.0", "aarch64"),
                ("fedora", "44", "x86_64"),
                ("fedora", "44", "arm64"),
            ] {
                let mut wrong = host.clone();
                wrong.os_release =
                    parse_os_release(&format!("ID={id}\nVERSION_ID={version}\nID_LIKE=fedora"));
                wrong.architecture = Ok(arch.into());
                blocked(&wrong, &[good.clone()]);
            }
        }

        // Phase `readiness_rejects_unknown_immutable_sandboxed_and_non_linux_hosts`.
        {
            let release = release();
            for change in 0..11 {
                let mut host = fedora();
                match change {
                    0 => host.immutable = Ok(true),
                    1 => host.sandboxed = Ok(true),
                    2 => host.immutable = Err("unknown".into()),
                    3 => host.sandboxed = Err("unknown".into()),
                    4 => host.session = Err("unknown".into()),
                    5 => host.package_manager = Err("missing".into()),
                    6 => host.installed_package = Err("query failed".into()),
                    7 => host.operating_system = "windows".into(),
                    8 => host.os_release = parse_os_release("ID=fedora"),
                    9 => host.package_manager = Ok(PackageManager::Apt),
                    _ => host.architecture = Err("unknown".into()),
                }
                blocked(&host, &[release.clone()]);
            }
        }

        // Phase `readiness_accepts_only_explicit_ordinary_distribution_adapters`.
        {
            for (id, version, manager, format) in [
                (
                    "ubuntu",
                    Some("24.04"),
                    PackageManager::Apt,
                    PackageFormat::Deb,
                ),
                (
                    "debian",
                    Some("13"),
                    PackageManager::Apt,
                    PackageFormat::Deb,
                ),
                ("arch", None, PackageManager::Pacman, PackageFormat::Arch),
            ] {
                let mut host = fedora();
                host.os_release = parse_os_release(&format!(
                    "ID={id}\n{}",
                    version.map_or(String::new(), |v| format!("VERSION_ID={v}"))
                ));
                host.package_manager = Ok(manager);
                let mut release = release();
                release.distro_id = id.into();
                release.distro_version = version.unwrap_or("rolling").into();
                release.package_format = format;
                assert!(matches!(
                    desktop_readiness(&host, &[release]),
                    DesktopReadiness::Ready(_)
                ));
            }
            for id in ["nixos", "unknown", "fedora-derivative"] {
                let mut host = fedora();
                host.os_release = parse_os_release(&format!("ID={id}\nVERSION_ID=44"));
                let mut release = release();
                release.distro_id = id.into();
                blocked(&host, &[release]);
            }
        }

        // Phase `derivative_does_not_inherit_fedora_support`.
        {
            let host = parse_os_release("ID=example\nID_LIKE=fedora\nVERSION_ID=44\n").unwrap();
            assert_eq!(host.id, "example");
        }
    }

    /// Diagnostic only: uses the actual worker and authenticated bundled catalog.
    /// No fixture can be substituted into this path.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "read-only native host diagnostic, run explicitly"]
    fn native_desktop_probe() {
        let host = probe_desktop_host().unwrap().join().unwrap();
        println!("Desktop host evidence: {host:#?}");
        let releases = super::super::desktop_catalog::bundled_releases().unwrap();
        println!(
            "Desktop readiness: {:#?}",
            desktop_readiness(&host, &releases)
        );
    }

    fn fedora() -> DesktopHost {
        DesktopHost {
            operating_system: "linux".into(),
            os_release: parse_os_release(
                "NAME=\"Fedora Linux\"\nVERSION=\"44 (Workstation Edition)\"\nID=fedora\nVERSION_ID=44\nPLATFORM_ID=\"platform:f44\"\n",
            ),
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

    fn release() -> DesktopRelease {
        super::super::desktop_catalog::parse_catalog(&serde_json::json!({
            "schema":1,"tag":"test", "releases":[{
                "id":"fedora-44-aarch64", "version":"1.0-1", "distro_id":"fedora", "distro_version":"44", "architecture":"aarch64", "package_format":"rpm",
                "url":"https://code.tilo.so/tilo/test.rpm", "byte_size":7, "sha256":"a".repeat(64),
                "verified_runtime":{"schema":1,"reference":"test-only-report", "package_sha256":"a".repeat(64), "graphical_login":true, "return_to_prior_session":true,"identity_setup":true,"selected_folder_import":true,"source_intact":true}
            }]
        }).to_string()).unwrap().remove(0)
    }

    fn blocked(host: &DesktopHost, releases: &[DesktopRelease]) {
        assert!(
            matches!(desktop_readiness(host, releases), DesktopReadiness::Blocked(ref reasons) if !reasons.is_empty())
        );
    }

    /// os-release parsing and arch mapping.
    #[test]
    fn os_release_parses_literally_and_rejects_ambiguity() {
        // Phase `os_release_parses_quoted_values_without_shell_evaluation`.
        {
            let host = parse_os_release("# Fedora fixture\nNAME=\"Fedora Linux\"\nID='fedora'\nVERSION_ID=\"44\"\nPRETTY_NAME=\"Fedora \\\"44\\\" \\$HOME\"\n").unwrap();
            assert_eq!(host.id, "fedora");
            assert_eq!(host.version_id.as_deref(), Some("44"));
        }

        // Phase `os_release_rejects_ambiguous_or_malformed_data`.
        {
            for value in [
                "",
                "ID=fedora\nID=fedora\n",
                "ID=fedora\nVERSION_ID=44\nVERSION_ID=45\n",
                "ID=\"fedora",
                "ID=fedora extra",
                "export ID=fedora",
                "ID=$(whoami)",
                "ID=fedora;true",
                "ID=fedora\nBROKEN",
                "ID=Fedora",
                "ID=fedora\nVERSION_ID=\"44\"junk",
            ] {
                assert!(parse_os_release(value).is_err(), "accepted {value:?}");
            }
        }

        // Phase `missing_version_is_preserved_and_arch_mapping_is_explicit`.
        {
            assert_eq!(parse_os_release("ID=fedora\n").unwrap().version_id, None);
            assert_eq!(catalog_architecture("aarch64"), Some("aarch64"));
            assert_eq!(catalog_architecture("x86_64"), Some("x86_64"));
            assert_eq!(catalog_architecture("arm64"), None);
            assert_eq!(catalog_architecture("x86"), None);
        }
    }
}
