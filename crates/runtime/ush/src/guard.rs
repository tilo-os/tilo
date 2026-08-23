//! `guard`, `scan` and `quarantine` — and what they honestly are.
//!
//! The reference runtime backs these with a resident daemon that carries
//! signatures. qOS has no such thing and this module does not pretend to one.
//! What it does instead is report and act on facts the runtime actually knows:
//!
//! - `guard status` — the enforcement that is really in place right now, read
//!   back from the kernel and the session rather than from the configuration
//!   that asked for it. A Landlock ruleset that only partially applied says so.
//! - `scan <path>` — everything the runtime can establish about a path: which
//!   side of the read-only base it came from, whether rpm owns it and whether
//!   it still matches what rpm installed, whether it is executable, setuid or
//!   setgid, and whether it is inside a directory you trusted. No verdict, and
//!   no claim that a clean result means the file is safe.
//! - `quarantine` — a real, user-driven mechanism: move a file into the layer's
//!   quarantine directory with its executable bits cleared, list what is there,
//!   restore it. Nothing puts a file there automatically, because nothing here
//!   is entitled to that judgement.
//!
//! Stating the limit is the feature. A `scan` that returned "clean" from a
//! runtime with no signatures would be worse than no `scan` at all.

use std::path::{Path, PathBuf};

use landlock::{
    ABI, Access, AccessFs, PathBeneath, PathFd, RulesetAttr, RulesetCreatedAttr, RulesetStatus,
};

/// Where quarantined files go, inside the layer.
pub const QUARANTINE_DIR: &str = "/usr/lib/ush/quarantine";

/// What Landlock actually did, as opposed to what was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LandlockOutcome {
    FullyEnforced,
    PartiallyEnforced,
    NotEnforced,
    Errored,
}

impl LandlockOutcome {
    pub fn describe(self) -> &'static str {
        match self {
            LandlockOutcome::FullyEnforced => "enforcing (every requested right honoured)",
            LandlockOutcome::PartiallyEnforced => {
                "partially enforcing (the running kernel dropped some rights)"
            }
            LandlockOutcome::NotEnforced => "NOT enforcing (this kernel has no Landlock)",
            LandlockOutcome::Errored => "NOT enforcing (the ruleset would not build)",
        }
    }
}

/// Cap at V4, the same as the app sandbox's shim: V5 adds rights this runtime
/// has no use for, and asking for them turns a fully-enforced ruleset into a
/// partially-enforced one on kernels that are otherwise fine.
pub const TARGET_ABI: ABI = ABI::V4;

/// Restrict the calling process, and every descendant, to `rw` and `ro`.
///
/// Called by the guest's first process inside the namespace, immediately before
/// it execs the shell. Irreversible, which is the point.
///
/// Landlock is defence in depth here and not the primary boundary — the mount
/// namespace already contains only what was bound, so a path the guest was not
/// granted is absent rather than unreadable. What this adds is that a directory
/// granted read-only stays read-only even if something later remounts it, and
/// that the enforcement is a kernel fact `guard status` can report rather than
/// a claim about the argv.
pub fn restrict_self(rw: &[PathBuf], ro: &[PathBuf]) -> LandlockOutcome {
    let abi = TARGET_ABI;
    let all = AccessFs::from_all(abi);
    let read = AccessFs::from_read(abi);

    let ruleset = match landlock::Ruleset::default().handle_access(all) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "landlock: could not handle the access set");
            return LandlockOutcome::Errored;
        }
    };
    let mut created = match ruleset.create() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "landlock: could not create the ruleset");
            return LandlockOutcome::Errored;
        }
    };
    for (paths, access) in [(rw, all), (ro, read)] {
        for path in paths {
            match PathFd::new(path) {
                Ok(fd) => match created.add_rule(PathBeneath::new(fd, access)) {
                    Ok(next) => created = next,
                    Err(e) => {
                        tracing::warn!(path = %path.display(), error = %e, "landlock: rule refused");
                        return LandlockOutcome::Errored;
                    }
                },
                // A path that is not in the namespace needs no rule; it is
                // already unreachable. Refusing to start over it would make an
                // absent optional directory fatal.
                Err(e) => {
                    tracing::debug!(path = %path.display(), error = %e, "landlock: skipping absent path")
                }
            }
        }
    }
    match created.restrict_self() {
        Ok(status) => match status.ruleset {
            RulesetStatus::FullyEnforced => LandlockOutcome::FullyEnforced,
            RulesetStatus::PartiallyEnforced => LandlockOutcome::PartiallyEnforced,
            RulesetStatus::NotEnforced => LandlockOutcome::NotEnforced,
        },
        Err(e) => {
            tracing::warn!(error = %e, "landlock: restrict_self failed");
            LandlockOutcome::Errored
        }
    }
}

/// Ask the kernel whether the Landlock ruleset is really in force, by trying
/// something it forbids.
///
/// [`restrict_self`] gives the guest's root read-only access and read-write
/// access to the trees below it, so creating an entry directly at `/` is
/// refused when the ruleset applied and succeeds when it did not — the root is
/// a tmpfs and the guest is uid 0 in its own namespace. Nothing survives either
/// way: on the success path the probe removes what it made.
///
/// This is deliberately a probe and not a reading of `/proc/self/status`. Not
/// every kernel reports Landlock there, and a status line that says "installed"
/// because the code path ran is exactly the kind of check that passes when it
/// cannot see.
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
        // A read-only or otherwise unusual root cannot tell the two apart, and
        // saying so is better than picking the flattering answer.
        Err(_) => LandlockOutcome::Errored,
    }
}

/// What `scan` found. Every field is something the runtime established, and
/// there is deliberately no "verdict".
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

/// Scan one path.
///
/// `trusted` is the list from the policy, so the report can say whether writing
/// here would reach the host.
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
        report.notes.push(
            "rpm says this file no longer matches what its package installed."
                .into(),
        );
    }
    if report.in_trusted_dir {
        report.notes.push(
            "inside a directory you trusted, so a write here reaches the host."
                .into(),
        );
    }
    // The limit, stated every time rather than in a footnote.
    report.notes.push(NO_DATABASE.into());
    report
}

/// The sentence every [`ScanReport`] carries, on every path through [`scan`].
///
/// A `scan` that could return without it would sooner or later be read as a
/// clean bill of health, which this runtime is in no position to give.
pub const NO_DATABASE: &str = "no signature or reputation check was performed: \
     this runtime ships no such database. A clean report means nothing above \
     was true, not that the file is safe.";

fn rpm_owner(path: &Path) -> Option<String> {
    let out = std::process::Command::new("rpm")
        .arg("-qf")
        .arg(path)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty() && !s.contains("not owned"))
}

fn rpm_verify_failed(package: &str, path: &Path) -> bool {
    let Ok(out) = std::process::Command::new("rpm")
        .arg("-V")
        .arg(package)
        .output()
    else {
        return false;
    };
    let needle = path.to_string_lossy();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|line| line.ends_with(needle.as_ref()) && !line.starts_with("......."))
}

/// Move a file into the layer's quarantine, clearing its executable bits.
///
/// Returns where it went. The original path stops resolving, so anything that
/// was going to run it now fails to find it — which is the whole mechanism.
pub fn quarantine_add(path: &Path, quarantine_dir: &Path) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(quarantine_dir)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unnamed".into());
    // Keep the original location in the stored name: restoring has to put it
    // back where it came from, and a bare basename cannot say where that was.
    let encoded = path.to_string_lossy().replace('/', "%2F");
    let dest = quarantine_dir.join(format!("{}-{encoded}", crate::audit::now_unix()));
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
                bytes: e.metadata().ok().map(|m| m.len()).unwrap_or(0),
                stored_at,
            })
        })
        .collect();
    out.sort_by_key(|e| e.at);
    out
}

/// Put a quarantined file back where it came from.
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
    fn scanning_an_absent_path_says_so_rather_than_guessing() {
        let dir = tempfile::tempdir().unwrap();
        let report = scan(&dir.path().join("nothing"), &[]);
        assert!(!report.exists);
        assert!(report.notes.iter().any(|n| n.contains("not present")));
    }

    /// The honesty requirement, asserted rather than documented: every report
    /// carries the limit.
    #[test]
    fn every_report_states_that_nothing_was_checked_against_a_database() {
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

    #[test]
    fn scan_reports_the_bits_that_matter() {
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

    #[test]
    fn a_path_outside_every_trusted_directory_is_not_reported_as_inside_one() {
        let dir = tempfile::tempdir().unwrap();
        let trusted = dir.path().join("src");
        let other = dir.path().join("srcextra");
        std::fs::create_dir_all(&trusted).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let file = other.join("f");
        std::fs::write(&file, b"x").unwrap();
        assert!(!scan(&file, &[trusted]).in_trusted_dir);
    }

    #[test]
    fn quarantine_moves_the_file_and_clears_its_executable_bits() {
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
        assert_eq!(mode & 0o7111, 0, "quarantined file is still runnable: {mode:o}");

        let listed = quarantine_list(&q);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].original, file);

        quarantine_restore(&listed[0]).unwrap();
        assert!(file.exists());
        assert!(quarantine_list(&q).is_empty());
    }

    #[test]
    fn listing_an_empty_or_absent_quarantine_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(quarantine_list(&dir.path().join("absent")).is_empty());
        std::fs::create_dir_all(dir.path().join("q")).unwrap();
        assert!(quarantine_list(&dir.path().join("q")).is_empty());
    }

    #[test]
    fn the_quarantine_lives_inside_the_layer() {
        assert!(
            QUARANTINE_DIR.starts_with("/usr/"),
            "quarantining must land in the layer, not on the host"
        );
    }

    /// The probe's answer has to follow from what `/` actually allows, and
    /// it has to leave nothing behind either way.
    ///
    /// Which arm it takes is a property of the machine, not of the code, so
    /// the precondition is established here rather than assumed. This used
    /// to assume the unprivileged one — "`/` is not writable, so this
    /// exercises the third arm" — and that is false wherever the suite runs
    /// as root, which is every container. There, creating an entry at `/`
    /// succeeds and `NotEnforced` is simply the true answer: nothing is
    /// restricting this process. Failing on it asserted that the machine
    /// was a workstation, not that the probe was right.
    #[test]
    fn the_landlock_probe_answers_for_what_root_actually_allows() {
        // Ask `/` the same question the probe will, under a different name,
        // so the two cannot collide.
        let canary =
            std::path::Path::new("/").join(format!(".ush-landlock-canary-{}", std::process::id()));
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

    #[test]
    fn the_landlock_outcomes_describe_themselves_honestly() {
        assert!(LandlockOutcome::NotEnforced.describe().contains("NOT"));
        assert!(LandlockOutcome::Errored.describe().contains("NOT"));
        assert!(LandlockOutcome::PartiallyEnforced.describe().contains("partially"));
    }
}
