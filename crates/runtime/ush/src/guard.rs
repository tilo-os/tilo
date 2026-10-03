//! `guard`, `scan` and `quarantine`.
//!
//! This module reports and acts on facts the runtime can establish:
//!
//! - `guard status` — the enforcement in place right now, read back
//!   from the kernel and the session, not the configuration that asked for
//!   it. A Landlock ruleset that only partially applied says so.
//! - `scan <path>` — everything establishable about a path (base side, rpm
//!   ownership and match, executable/setuid/setgid, trusted-dir membership)
//!   with no verdict and no claim that clean means safe.
//! - `quarantine` — move a file into the layer's quarantine with its
//!   executable bits cleared, list it, restore it; nothing is quarantined
//!   automatically, because nothing here is entitled to that judgement.
//!
//! A `scan` reports established properties and does not infer safety from
//! the absence of a matching condition.

use std::path::{Path, PathBuf};
use std::time::Duration;

use landlock::{ABI, Access, AccessFs, BitFlags, CompatLevel};
pub use sandbox_core::landlock::LandlockOutcome;
use sandbox_core::landlock::{FsRule, RulesetSpec, outcome_from_status};
use tilo_clock::unix_now;

/// Where quarantined files go, inside the layer.
pub const QUARANTINE_DIR: &str = "/usr/lib/ush/quarantine";

/// What Landlock actually did, as opposed to what was asked for — worded for
/// `guard status`, which reports to the person at the shell rather than to a
/// launch log.
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

/// Cap at V4, matching the app sandbox's shim: V5 adds rights this runtime
/// doesn't use, turning a fully-enforced ruleset partial on some kernels.
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

/// Ask the kernel whether the Landlock ruleset is in force, by
/// trying something it forbids. [`restrict_self`] gives the guest's root
/// read-only access, so creating an entry at `/` is refused when the
/// ruleset applied and succeeds when it did not; either way the probe
/// leaves nothing behind.
///
/// A probe, rather than a read of `/proc/self/status`: not every
/// kernel reports Landlock there, and a status line that says "installed"
/// because the code path ran is exactly the check that passes when it
/// cannot detect kernels that omit enforcement.
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

/// What `scan` found. Every field is something the runtime established, and
/// there is no "verdict".
// Each bool records one independently established fact about the path.
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
    /// The rpm package that owns this path, if any.
    pub owned_by: Option<String>,
    /// `rpm -V` disagreed with the installed package about this file.
    pub modified_since_install: bool,
    /// The path is under a directory the user trusted.
    pub in_trusted_dir: bool,
    /// Human-readable notes, including what was *not* checked.
    pub notes: Vec<String>,
}

/// Scan one path. `trusted` is the policy's list, so the report can say
/// whether writing here would reach the host.
pub fn scan(path: &Path, trusted: &[PathBuf]) -> ScanReport {
    use std::os::unix::fs::PermissionsExt;

    let mut report = ScanReport {
        path: path.to_path_buf(),
        exists: false,
        is_dir: false,
        bytes: 0,
        executable: false,
        setuid: false,
        setgid: false,
        owned_by: None,
        modified_since_install: false,
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

    if let Some(owner) = rpm_owner(path) {
        report.modified_since_install = rpm_verify_failed(&owner, path);
        report.owned_by = Some(owner);
    }

    if report.setuid || report.setgid {
        report.notes.push(
            "setuid/setgid bits are set. Inside a guest they escalate only to \
             the user who started it — the file is owned by that user on the \
             host — but they are worth knowing about."
                .into(),
        );
    }
    if report.owned_by.is_none() && report.executable {
        report.notes.push(
            "an executable no package owns: it was written into your layer \
             rather than installed."
                .into(),
        );
    }
    if report.modified_since_install {
        report
            .notes
            .push("rpm says this file no longer matches what its package installed.".into());
    }
    if report.in_trusted_dir {
        report
            .notes
            .push("inside a directory you trusted, so a write here reaches the host.".into());
    }
    // The limit, stated every time rather than in a footnote.
    report.notes.push(NO_DATABASE.into());
    report
}

/// The sentence every [`ScanReport`] carries, on every path through [`scan`].
/// Without it, a `scan` would sooner or later be read as a clean bill of health.
pub const NO_DATABASE: &str = "no signature or reputation check was performed: \
     this runtime ships no such database. A clean report means nothing above \
     was true, not that the file is safe.";

fn rpm_owner(path: &Path) -> Option<String> {
    let args = [std::ffi::OsStr::new("-qf"), path.as_os_str()];
    let out =
        bounded_command::run_os("rpm", &args, None, 64 * 1024, Duration::from_secs(10)).ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty() && !s.contains("not owned"))
}

fn rpm_verify_failed(package: &str, path: &Path) -> bool {
    let Ok(out) = bounded_command::run(
        "rpm",
        &["-V", package],
        None,
        1024 * 1024,
        Duration::from_secs(15),
    ) else {
        return false;
    };
    let needle = path.to_string_lossy();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|line| line.ends_with(needle.as_ref()) && !line.starts_with("......."))
}

/// Move a file into the layer's quarantine, clearing its executable bits.
/// The original path stops resolving, so nothing can find it to run —
/// that's the whole mechanism.
///
/// # Errors
///
/// Returns an error if `quarantine_dir` can't be created, the file can't be
/// moved or copied into it, or its permissions can't be cleared.
pub fn quarantine_add(path: &Path, quarantine_dir: &Path) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(quarantine_dir)?;
    let name = path
        .file_name()
        .map_or_else(|| "unnamed".into(), |n| n.to_string_lossy().into_owned());
    // Keep the original location in the stored name: restoring has to put it
    // back where it came from, and a bare basename cannot say where that was.
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

/// One quarantined file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantineEntry {
    pub stored_at: PathBuf,
    pub original: PathBuf,
    pub at: u64,
    pub bytes: u64,
}

/// What is in quarantine.
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

/// Put a quarantined file back where it came from.
///
/// # Errors
///
/// Returns an error if the original path's parent can't be created, or the
/// file can't be moved or copied back.
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
        // phase: scanning_an_absent_path_says_so_rather_than_guessing
        {
            let dir = tempfile::tempdir().unwrap();
            let report = scan(&dir.path().join("nothing"), &[]);
            assert!(!report.exists);
            assert!(report.notes.iter().any(|n| n.contains("not present")));
        }
        // phase: every_report_states_that_nothing_was_checked_against_a_database
        // Every report carries a note describing unchecked conditions.
        {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("thing");
            std::fs::write(&file, b"data").unwrap();
            for path in [file, dir.path().join("absent")] {
                let report = scan(&path, &[]);
                assert!(
                    report
                        .notes
                        .iter()
                        .any(|n| n.contains("no signature or reputation check")),
                    "a scan must not read as a clean bill of health: {report:?}"
                );
            }
        }
        // phase: scan_reports_the_bits_that_matter
        {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("tool");
            std::fs::write(&file, b"#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o4755)).unwrap();

            let report = scan(&file, &[dir.path().to_path_buf()]);
            assert!(report.exists && report.executable && report.setuid);
            assert!(report.in_trusted_dir);
            assert!(report.notes.iter().any(|n| n.contains("setuid")));
            assert!(report.notes.iter().any(|n| n.contains("reaches the host")));
        }
        // phase: a_path_outside_every_trusted_directory_is_not_reported_as_inside_one
        {
            let dir = tempfile::tempdir().unwrap();
            let trusted = dir.path().join("src");
            let other = dir.path().join("srcextra");
            std::fs::create_dir_all(&trusted).unwrap();
            std::fs::create_dir_all(&other).unwrap();
            let file = other.join("f");
            std::fs::write(&file, b"x").unwrap();
            assert!(!scan(&file, &[trusted]).in_trusted_dir);
        }
    }

    #[test]
    fn quarantine_moves_files_into_the_layer_and_restores_them() {
        // phase: quarantine_moves_the_file_and_clears_its_executable_bits
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
        // phase: listing_an_empty_or_absent_quarantine_is_not_an_error
        {
            let dir = tempfile::tempdir().unwrap();
            assert!(quarantine_list(&dir.path().join("absent")).is_empty());
            std::fs::create_dir_all(dir.path().join("q")).unwrap();
            assert!(quarantine_list(&dir.path().join("q")).is_empty());
        }
        // phase: the_quarantine_lives_inside_the_layer
        {
            assert!(
                QUARANTINE_DIR.starts_with("/usr/"),
                "quarantining must land in the layer, not on the host"
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
        // phase: the_landlock_outcomes_describe_themselves_honestly
        {
            assert!(describe(LandlockOutcome::NotEnforced).contains("NOT"));
            assert!(describe(LandlockOutcome::Errored).contains("NOT"));
            assert!(describe(LandlockOutcome::PartiallyEnforced).contains("partially"));
        }
    }
}
