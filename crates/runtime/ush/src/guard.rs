//! `guard`, `scan` and `quarantine`: what the runtime can establish, reported.
//!
//! `guard status` reads back the enforcement in place from the kernel and the session
//! rather than the configuration that asked for it (a partially applied Landlock ruleset
//! says so; the network line comes from connecting to an address nothing granted and to
//! a granted host, [`network_report`]). `scan <path>` reports what is establishable about
//! a path — its image, executable and setuid bits, granted-dir membership — with no
//! verdict and no claim that clean means safe. `quarantine` moves a file into the
//! environment's quarantine with its executable bits cleared, lists it and restores it;
//! nothing is quarantined automatically, because nothing here is entitled to that
//! judgement.

use std::path::{Path, PathBuf};

use landlock::{ABI, Access, AccessFs, BitFlags, CompatLevel};
pub use sandbox_core::landlock::LandlockOutcome;
use sandbox_core::landlock::{FsRule, RulesetSpec, outcome_from_status};
use tilo_clock::unix_now;

/// An address no grant names, for the network test: a public resolver's,
/// so where egress were not enforced the connection would succeed rather
/// than merely time out.
pub const UNLISTED_PROBE: &str = "1.1.1.1:443";

const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

pub fn network_report(status: &crate::broker::NetworkStatus) -> Vec<String> {
    use crate::broker::NetworkStatus;
    let mut lines = Vec::new();
    let hosts = match status {
        NetworkStatus::Unrestricted => {
            lines.push("network     : unrestricted (a * grant), from its own namespace".into());
            return lines;
        }
        NetworkStatus::Offline => {
            lines.push(
                "network     : none (nothing is granted; `perm allow-host HOST` adds a host)"
                    .into(),
            );
            Vec::new()
        }
        NetworkStatus::Filtered { hosts } if hosts.is_empty() => {
            lines.push(
                "network     : none granted (`perm allow-host HOST` adds a host); \
                 enforced by nftables"
                    .into(),
            );
            Vec::new()
        }
        NetworkStatus::Filtered { hosts } => {
            lines.push(format!(
                "network     : only {} (named by the session's resolver, enforced by nftables)",
                hosts.join(", ")
            ));
            hosts.clone()
        }
    };
    lines.push(format!("  tested    : {}", probe_unlisted()));
    match hosts.iter().find(|h| !h.starts_with('.')) {
        Some(host) => lines.push(format!("  tested    : {}", probe_granted(host))),
        None if !hosts.is_empty() => {
            lines.push("  tested    : no exact host to try (only domain grants)".into());
        }
        None => {}
    }
    lines
}

fn probe_unlisted() -> String {
    let Ok(addr) = UNLISTED_PROBE.parse::<std::net::SocketAddr>() else {
        return "no address to try".into();
    };
    match std::net::TcpStream::connect_timeout(&addr, PROBE_TIMEOUT) {
        Ok(_) => format!("{UNLISTED_PROBE} (not granted) CONNECTED: egress is NOT enforced"),
        Err(e) => match e.kind() {
            std::io::ErrorKind::ConnectionRefused => {
                format!("{UNLISTED_PROBE} (not granted) refused")
            }
            std::io::ErrorKind::NetworkUnreachable => {
                format!("{UNLISTED_PROBE} (not granted) unreachable: no route out")
            }
            _ => format!("{UNLISTED_PROBE} (not granted) failed: {e}"),
        },
    }
}

fn probe_granted(host: &str) -> String {
    use std::net::ToSocketAddrs;
    let addrs: Vec<std::net::SocketAddr> = match (host, 443).to_socket_addrs() {
        Ok(a) => a.collect(),
        Err(e) => return format!("{host} (granted) did not resolve: {e}"),
    };
    let Some(first) = addrs.first() else {
        return format!("{host} (granted) resolved to nothing");
    };
    match std::net::TcpStream::connect_timeout(first, PROBE_TIMEOUT) {
        Ok(_) => format!("{host}:443 (granted, {}) reached", first.ip()),
        Err(e) => format!(
            "{host} (granted) resolved to {}, but port 443 did not connect: {e}",
            first.ip()
        ),
    }
}

pub const QUARANTINE_DIR: &str = "/var/lib/ush/quarantine";

pub fn describe(outcome: LandlockOutcome) -> &'static str {
    match outcome {
        LandlockOutcome::FullyEnforced => "enforcing (every requested right honoured)",
        LandlockOutcome::PartiallyEnforced => {
            "partially enforcing (the running kernel dropped some rights)"
        }
        LandlockOutcome::NotEnforced => "NOT enforcing (this kernel has no Landlock)",
        LandlockOutcome::Errored => "NOT enforcing (the ruleset would not build)",
    }
}

pub const TARGET_ABI: ABI = ABI::V4;

/// The ruleset [`restrict_self`] installs: every ABI-4 filesystem right
/// handled best-effort, `rw` paths granted all of them and `ro` paths the
/// read set, in that order. The network is not handled — the guest's
/// network namespace is its own boundary.
pub fn ruleset_spec(rw: &[PathBuf], ro: &[PathBuf]) -> RulesetSpec {
    let all = AccessFs::from_all(TARGET_ABI);
    let read = AccessFs::from_read(TARGET_ABI);
    let rules = rw
        .iter()
        .map(|path| (path, all))
        .chain(ro.iter().map(|path| (path, read)))
        .map(|(path, access)| FsRule {
            path: path.clone(),
            access,
        })
        .collect();
    RulesetSpec {
        compat: CompatLevel::BestEffort,
        handled_fs: all,
        handled_net: BitFlags::EMPTY,
        connect_ports: Vec::new(),
        rules,
        trim_file_rights: false,
        scoped: BitFlags::EMPTY,
    }
}

/// Restrict the calling process, and every descendant, to `rw` and `ro`.
/// Called by the guest's first process inside the namespace, immediately
/// before it execs the shell. Irreversible, which is the point.
///
/// Landlock is defence in depth, not the primary boundary — the mount
/// namespace already contains only what was bound, so an ungranted path is
/// absent rather than unreadable. What this adds: a read-only grant stays
/// read-only even if something later remounts it, and the enforcement is a
/// kernel fact `guard status` can report, not a claim about the argv.
pub fn restrict_self(rw: &[PathBuf], ro: &[PathBuf]) -> LandlockOutcome {
    // A path not in the namespace needs no rule — it's already unreachable;
    // refusing to start over it would make an absent optional directory fatal.
    let restricted = sandbox_core::landlock::restrict_self(&ruleset_spec(rw, ro), |path, e| {
        tracing::debug!(path = %path.display(), error = %e, "landlock: skipping absent path");
    });
    match restricted {
        Ok(status) => outcome_from_status(&status.ruleset),
        Err(e) => {
            tracing::warn!(error = %e, "landlock: ruleset refused");
            LandlockOutcome::Errored
        }
    }
}

/// Ask the kernel whether the Landlock ruleset is in force, by trying something it
/// forbids: [`restrict_self`] gives the guest's root read-only access, so creating an entry
/// there is refused when the ruleset applied and succeeds when it did not.
///
/// A probe rather than a read of `/proc/self/status`: not every kernel reports Landlock
/// there, and a status line that says "installed" because the code path ran passes exactly
/// when it cannot detect a kernel that omits enforcement.
pub fn landlock_probe() -> LandlockOutcome {
    let probe = Path::new("/").join(format!(".ush-landlock-probe-{}", std::process::id()));
    match std::fs::create_dir(&probe) {
        Ok(()) => {
            let _ = std::fs::remove_dir(&probe);
            LandlockOutcome::NotEnforced
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            LandlockOutcome::FullyEnforced
        }
        // A read-only or otherwise unusual root cannot distinguish the two
        // outcomes.
        Err(_) => LandlockOutcome::Errored,
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is one independently-established scan fact, not a state machine"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanReport {
    pub path: PathBuf,
    pub exists: bool,
    pub is_dir: bool,
    pub bytes: u64,
    pub executable: bool,
    pub setuid: bool,
    pub setgid: bool,
    pub image: Option<String>,
    pub in_trusted_dir: bool,
    /// Human-readable notes, including what was *not* checked.
    pub notes: Vec<String>,
}

/// Scan one path. `trusted` is the environment's writable grants, so the
/// report can say whether writing here would reach the host; `image` is the
/// broker's answer to [`provenance`].
pub fn scan(path: &Path, trusted: &[PathBuf], image: Option<String>) -> ScanReport {
    use std::os::unix::fs::PermissionsExt;

    let mut report = ScanReport {
        path: path.to_path_buf(),
        exists: false,
        is_dir: false,
        bytes: 0,
        executable: false,
        setuid: false,
        setgid: false,
        image,
        in_trusted_dir: trusted.iter().any(|t| path.starts_with(t)),
        notes: Vec::new(),
    };

    let Ok(meta) = path.symlink_metadata() else {
        report.notes.push("not present in this guest".into());
        report.notes.push(NO_DATABASE.into());
        return report;
    };
    report.exists = true;
    report.is_dir = meta.is_dir();
    report.bytes = meta.len();
    let mode = meta.permissions().mode();
    report.executable = mode & 0o111 != 0 && !meta.is_dir();
    report.setuid = mode & 0o4000 != 0;
    report.setgid = mode & 0o2000 != 0;

    if report.setuid || report.setgid {
        report.notes.push(
            "setuid/setgid bits are set. Inside a guest they escalate only to \
             the user who started it — the file is owned by that user on the \
             host — but they are worth knowing about."
                .into(),
        );
    }
    if report.image.is_some() {
        report.notes.push(
            "it comes from a read-only image, verified by its content address when \
             it was materialised; nothing in the guest can change it."
                .into(),
        );
    } else if report.executable {
        report.notes.push(
            "an executable no image provides: it was written in this environment \
             rather than installed."
                .into(),
        );
    }
    if report.in_trusted_dir {
        report
            .notes
            .push("inside a directory you trusted, so a write here reaches the host.".into());
    }
    report.notes.push(NO_DATABASE.into());
    report
}

pub const NO_DATABASE: &str = "no signature or reputation check was performed: \
     this runtime ships no such database. A clean report means nothing above \
     was true, not that the file is safe.";

pub fn provenance(path: &Path, images: &[(String, PathBuf)]) -> Option<String> {
    let relative = path.strip_prefix("/").ok()?;
    let subtree = relative.components().next()?.as_os_str();
    if !crate::guest::IMAGE_SUBTREES
        .iter()
        .any(|s| std::ffi::OsStr::new(s) == subtree)
    {
        return None;
    }
    images
        .iter()
        .find(|(_, root)| root.join(relative).symlink_metadata().is_ok())
        .map(|(label, _)| label.clone())
}

/// Move a file into the quarantine, clearing its executable bits.
/// The original path stops resolving, so nothing can find it to run —
/// that's the whole mechanism.
pub fn quarantine_add(path: &Path, quarantine_dir: &Path) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(quarantine_dir)?;
    let name = path
        .file_name()
        .map_or_else(|| "unnamed".into(), |n| n.to_string_lossy().into_owned());
    let encoded = path.to_string_lossy().replace('/', "%2F");
    let dest = quarantine_dir.join(format!("{}-{encoded}", unix_now()));
    std::fs::rename(path, &dest).or_else(|_| {
        // Across filesystems `rename` fails; copy and unlink instead.
        std::fs::copy(path, &dest)?;
        std::fs::remove_file(path)
    })?;
    let mut perms = std::fs::metadata(&dest)?.permissions();
    perms.set_mode(perms.mode() & !0o7111);
    std::fs::set_permissions(&dest, perms)?;
    let _ = name;
    Ok(dest)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantineEntry {
    pub stored_at: PathBuf,
    pub original: PathBuf,
    pub at: u64,
    pub bytes: u64,
}

pub fn quarantine_list(quarantine_dir: &Path) -> Vec<QuarantineEntry> {
    let Ok(entries) = std::fs::read_dir(quarantine_dir) else {
        return Vec::new();
    };
    let mut out: Vec<QuarantineEntry> = entries
        .flatten()
        .filter_map(|e| {
            let stored_at = e.path();
            let name = stored_at.file_name()?.to_string_lossy().into_owned();
            let (at, encoded) = name.split_once('-')?;
            Some(QuarantineEntry {
                at: at.parse().ok()?,
                original: PathBuf::from(encoded.replace("%2F", "/")),
                bytes: e.metadata().ok().map_or(0, |m| m.len()),
                stored_at,
            })
        })
        .collect();
    out.sort_by_key(|e| e.at);
    out
}

pub fn quarantine_restore(entry: &QuarantineEntry) -> std::io::Result<()> {
    if let Some(parent) = entry.original.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(&entry.stored_at, &entry.original).or_else(|_| {
        std::fs::copy(&entry.stored_at, &entry.original)?;
        std::fs::remove_file(&entry.stored_at)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn scan_reports_what_it_saw_and_what_it_did_not_check() {
        {
            let dir = tempfile::tempdir().unwrap();
            let report = scan(&dir.path().join("nothing"), &[], None);
            assert!(!report.exists);
            assert!(report.notes.iter().any(|n| n.contains("not present")));
        }
        {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("thing");
            std::fs::write(&file, b"data").unwrap();
            for path in [file, dir.path().join("absent")] {
                let report = scan(&path, &[], None);
                assert!(
                    report
                        .notes
                        .iter()
                        .any(|n| n.contains("no signature or reputation check")),
                    "a scan must not read as a clean bill of health: {report:?}"
                );
            }
        }
        {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("tool");
            std::fs::write(&file, b"#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o4755)).unwrap();

            let report = scan(&file, &[dir.path().to_path_buf()], None);
            assert!(report.exists && report.executable && report.setuid);
            assert!(report.in_trusted_dir);
            assert!(report.notes.iter().any(|n| n.contains("setuid")));
            assert!(report.notes.iter().any(|n| n.contains("reaches the host")));
        }
        {
            let dir = tempfile::tempdir().unwrap();
            let trusted = dir.path().join("src");
            let other = dir.path().join("srcextra");
            std::fs::create_dir_all(&trusted).unwrap();
            std::fs::create_dir_all(&other).unwrap();
            let file = other.join("f");
            std::fs::write(&file, b"x").unwrap();
            assert!(!scan(&file, &[trusted], None).in_trusted_dir);
        }
        {
            let dir = tempfile::tempdir().unwrap();
            let tool = dir.path().join("tool");
            let runtime = dir.path().join("runtime");
            std::fs::create_dir_all(tool.join("usr/bin")).unwrap();
            std::fs::create_dir_all(runtime.join("usr/bin")).unwrap();
            std::fs::write(tool.join("usr/bin/pandoc"), b"").unwrap();
            std::fs::write(tool.join("usr/bin/sh"), b"").unwrap();
            std::fs::write(runtime.join("usr/bin/sh"), b"").unwrap();
            let images = vec![("tool".to_string(), tool), ("runtime".to_string(), runtime)];
            assert_eq!(
                provenance(Path::new("/usr/bin/pandoc"), &images).as_deref(),
                Some("tool")
            );
            assert_eq!(
                provenance(Path::new("/usr/bin/sh"), &images).as_deref(),
                Some("tool"),
                "the topmost image shadows the ones below"
            );
            assert_eq!(provenance(Path::new("/usr/bin/absent"), &images), None);
            assert_eq!(provenance(Path::new("/home/you/usr/bin/sh"), &images), None);
            assert_eq!(provenance(Path::new("relative"), &images), None);
        }
    }

    #[test]
    fn quarantine_moves_files_aside_and_restores_them() {
        {
            let dir = tempfile::tempdir().unwrap();
            let q = dir.path().join("quarantine");
            let file = dir.path().join("suspicious");
            std::fs::write(&file, b"#!/bin/sh\nrm -rf /\n").unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o4755)).unwrap();

            let stored = quarantine_add(&file, &q).unwrap();
            assert!(
                !file.exists(),
                "the original must stop resolving, or nothing has been quarantined"
            );
            let mode = std::fs::metadata(&stored).unwrap().permissions().mode();
            assert_eq!(
                mode & 0o7111,
                0,
                "quarantined file is still runnable: {mode:o}"
            );

            let listed = quarantine_list(&q);
            assert_eq!(listed.len(), 1);
            assert_eq!(listed[0].original, file);

            quarantine_restore(&listed[0]).unwrap();
            assert!(file.exists());
            assert!(quarantine_list(&q).is_empty());
        }
        {
            let dir = tempfile::tempdir().unwrap();
            assert!(quarantine_list(&dir.path().join("absent")).is_empty());
            std::fs::create_dir_all(dir.path().join("q")).unwrap();
            assert!(quarantine_list(&dir.path().join("q")).is_empty());
        }
        {
            assert!(
                QUARANTINE_DIR.starts_with("/var/lib/"),
                "quarantining must land in the environment's /var/lib, not on the host"
            );
        }
    }

    #[test]
    fn the_landlock_probe_ruleset_and_outcomes_are_honest() {
        // phase: the_landlock_probe_answers_for_what_root_actually_allows
        // The probe's answer must follow from what `/` actually allows, and leave
        // nothing behind either way. Which arm it takes is a property of the
        // machine, not the code — asserted here rather than assumed, since running
        // as root (every container) makes `/` writable and `NotEnforced` the true
        // answer; failing on that would assert the machine is a workstation, not
        // that the probe is right.
        {
            // Ask `/` the same question the probe will, under a different name,
            // so the two cannot collide.
            let canary = std::path::Path::new("/")
                .join(format!(".ush-landlock-canary-{}", std::process::id()));
            let root_is_writable = match std::fs::create_dir(&canary) {
                Ok(()) => {
                    let _ = std::fs::remove_dir(&canary);
                    true
                }
                Err(_) => false,
            };

            let outcome = landlock_probe();
            if root_is_writable {
                // Nothing stopped the write, so nothing is enforcing. Claiming
                // otherwise on an unconfined process is the failure this guards.
                assert_eq!(
                    outcome,
                    LandlockOutcome::NotEnforced,
                    "`/` accepted a new directory, so the probe must not claim \
                 anything is enforcing: {outcome:?}"
                );
            } else {
                // The write was refused. Whatever refused it, the probe must not
                // read the refusal as "unconfined".
                assert!(
                    matches!(
                        outcome,
                        LandlockOutcome::FullyEnforced | LandlockOutcome::Errored
                    ),
                    "`/` refused a new directory, so the probe must not report \
                 NotEnforced: {outcome:?}"
                );
            }

            assert!(
                !std::path::Path::new("/")
                    .join(format!(".ush-landlock-probe-{}", std::process::id()))
                    .exists(),
                "the probe left something behind at /"
            );
            assert!(!canary.exists(), "the canary was left behind at /");
        }
        // phase: the_guest_ruleset_is_pinned
        // The guest ruleset, pinned as raw bits (see
        // `sandbox_core::landlock` tests for the bit names): best-effort, every
        // ABI-4 filesystem right handled, TCP not handled, `rw` before `ro`,
        // and no file-right trimming.
        {
            let spec = ruleset_spec(
                &[PathBuf::from("/home/me"), PathBuf::from("/tmp")],
                &[PathBuf::from("/")],
            );
            assert_eq!(spec.compat, CompatLevel::BestEffort);
            assert_eq!(spec.handled_fs.bits(), 0x7fff);
            assert!(spec.handled_net.is_empty());
            assert!(spec.connect_ports.is_empty());
            assert!(!spec.trim_file_rights);
            let rules: Vec<(PathBuf, u64)> = spec
                .rules
                .iter()
                .map(|rule| (rule.path.clone(), rule.access.bits()))
                .collect();
            assert_eq!(
                rules,
                vec![
                    (PathBuf::from("/home/me"), 0x7fff),
                    (PathBuf::from("/tmp"), 0x7fff),
                    (PathBuf::from("/"), 0b1101),
                ]
            );
        }
        {
            assert!(describe(LandlockOutcome::NotEnforced).contains("NOT"));
            assert!(describe(LandlockOutcome::Errored).contains("NOT"));
            assert!(describe(LandlockOutcome::PartiallyEnforced).contains("partially"));
        }
    }
}
