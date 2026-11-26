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
