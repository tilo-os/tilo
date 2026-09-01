//! Launcher-side Landlock helpers.
//!
//! Landlock is layered on top of bwrap + seccomp, the same as the
//! compositor's per-workspace sandbox. The launcher's job: derive an FS
//! plan from the manifest (read-only paths get the read access set,
//! read-write paths get the full access set), then install the ruleset
//! on the *child* process after bwrap has set up the mount namespace
//! but before the entrypoint takes over — bwrap itself needs broad FS
//! access to `mount/bind/pivot_root`, so it can't be Landlocked.
//!
//! Today [`apply_plan`] is the in-process restriction point. Real
//! wiring through bwrap-via-shim is the compositor's pattern (see
//! `shell/src/sandbox/landlock_shim.rs`): bwrap execs a small shim, the
//! shim restricts itself, then execs the real binary. The launcher
//! doesn't yet ship a separate shim — it installs Landlock on the
//! parent via `pre_exec` right before bwrap takes over, and since
//! Landlock is inherited by every descendant, the bwrap-spawned app
//! inherits the policy.
//!
//! ### Open design question — bwrap's mount namespace
//!
//! Landlock policies are path-based, evaluated against the *kernel
//! inode*, so the path at `PathFd` time must exist on the host. Bwrap's
//! new mount namespace means the *destination* paths inside it might
//! not match the policy. The compositor solves this with an
//! in-namespace shim exec; TODO to wire the same here.

use std::os::fd::{AsFd, AsRawFd};
use std::path::PathBuf;

use landlock::{
    ABI, Access, AccessFs, AccessNet, BitFlags, CompatLevel, Compatible, PathBeneath, PathFd,
    RestrictionStatus, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetError, RulesetStatus,
};

/// qOS requires ABI 5 so device-node ioctls are policy-controlled. ABI 6 scope
/// enforcement remains blocked on kernels that cannot attest the relevant
/// Landlock errata; accepting a `PartiallyEnforced` result would be ambiguous.
pub const TARGET_ABI: ABI = ABI::V5;
pub const FS_ABI: ABI = TARGET_ABI;

/// FS plan element. Pure type so `plan_from_manifest` is unit-testable.
#[derive(Debug, Clone)]
pub struct FsRule {
    pub path: PathBuf,
    pub access: BitFlags<AccessFs>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum LandlockBuildError {
    #[error(transparent)]
    Ruleset(#[from] RulesetError),
    #[error("could not inspect an opened policy object: {0}")]
    Inspect(#[from] std::io::Error),
}

/// Remove directory-only rights when a rule names one concrete file. The
/// Landlock crate's best-effort mode otherwise drops these bits and reports a
/// partial policy; hard-requirement mode correctly refuses the mismatch. Use
/// `fstat` on the opened object so a path replacement cannot change the answer.
///
/// # Errors
///
/// Returns the operating-system error when the already-open policy object
/// cannot be inspected with `fstat`.
pub fn access_for_fd(
    fd: &PathFd,
    requested: BitFlags<AccessFs>,
) -> std::io::Result<BitFlags<AccessFs>> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `stat` points to writable storage of the exact libc type and fd
    // remains live for the syscall.
    let rc = unsafe { libc::fstat(fd.as_fd().as_raw_fd(), stat.as_mut_ptr()) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: successful fstat initialized the structure.
    let stat = unsafe { stat.assume_init() };
    if stat.st_mode & libc::S_IFMT == libc::S_IFDIR {
        Ok(requested)
    } else {
        Ok(requested & AccessFs::from_file(FS_ABI))
    }
}

/// Outcome enum mirroring the shim's, used for logging.
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
            LandlockOutcome::FullyEnforced => "landlock: fully enforced",
            LandlockOutcome::PartiallyEnforced => {
                "landlock: partially enforced (kernel ABI dropped some rights)"
            }
            LandlockOutcome::NotEnforced => "landlock: NOT enforced (kernel lacks Landlock)",
            LandlockOutcome::Errored => "landlock: ruleset construction failed",
        }
    }
}

pub(crate) fn outcome_from_status(status: &RulesetStatus) -> LandlockOutcome {
    match status {
        RulesetStatus::FullyEnforced => LandlockOutcome::FullyEnforced,
        RulesetStatus::PartiallyEnforced => LandlockOutcome::PartiallyEnforced,
        RulesetStatus::NotEnforced => LandlockOutcome::NotEnforced,
    }
}

/// Produce the FS access plan for a read list and a write list.
///
/// The lists come from the launcher, not a manifest: the system read
/// profile ([`crate::profile`]) and the app's own directory
/// ([`crate::appdir`]). Paths are absolute and literal.
pub fn plan_from_paths(ro: &[PathBuf], rw: &[PathBuf]) -> Vec<FsRule> {
    let abi = FS_ABI;
    let read = AccessFs::from_read(abi);
    let all = AccessFs::from_all(abi);
    let mut plan = Vec::with_capacity(ro.len().saturating_add(rw.len()));
    for p in ro {
        plan.push(FsRule {
            path: p.clone(),
            access: read,
        });
    }
    for p in rw {
        plan.push(FsRule {
            path: p.clone(),
            access: all,
        });
    }
    plan
}

/// Build a ruleset and restrict the calling process to it.
///
/// `net_restricted` mirrors `sandbox.net.mode == "off"` (deny TCP
/// bind/connect when set, no-op on ABI < 4).
///
/// **Caveat re: bwrap.** This irreversibly restricts the *current*
/// process and every descendant. Called from the launcher's parent
/// before exec, so policy paths must exist on the host root — see
/// `apply_plan_or_log` for the safe wrapper.
fn build_and_restrict(
    plan: &[FsRule],
    net_restricted: bool,
) -> Result<RestrictionStatus, LandlockBuildError> {
    let mut ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(AccessFs::from_all(FS_ABI))?;
    if net_restricted {
        ruleset = ruleset.handle_access(AccessNet::from_all(ABI::V4))?;
    }
    let mut created = ruleset.create()?;
    for rule in plan {
        match PathFd::new(&rule.path) {
            Ok(fd) => {
                let access = access_for_fd(&fd, rule.access)?;
                created = created.add_rule(PathBeneath::new(fd, access))?;
            }
            Err(e) => {
                tracing::debug!(
                    "landlock: skipping rule for {} (cannot open: {e})",
                    rule.path.display()
                );
            }
        }
    }
    Ok(created.restrict_self()?)
}

/// Apply a plan and report the kernel's actual enforcement status.
///
/// The in-sandbox shim treats anything short of `FullyEnforced` as a launch
/// refusal.  Keeping the status as data here makes that fail-closed decision
/// independently testable and prevents logging from being confused with
/// enforcement.
pub fn apply_plan_or_log(plan: &[FsRule], net_restricted: bool) -> LandlockOutcome {
    match build_and_restrict(plan, net_restricted) {
        Ok(status) => {
            let outcome = outcome_from_status(&status.ruleset);
            tracing::info!(
                "{} ({} rules, net_restricted={net_restricted})",
                outcome.describe(),
                plan.len()
            );
            outcome
        }
        Err(e) => {
            tracing::warn!("landlock: failed to build/apply ruleset: {e}");
            LandlockOutcome::Errored
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn plan_maps_ro_to_read_only_access() {
        let ro = vec![PathBuf::from("/usr")];
        let rw = vec![];
        let plan = plan_from_paths(&ro, &rw);
        assert_eq!(plan.len(), 1);
        let abi = FS_ABI;
        assert_eq!(plan[0].access, AccessFs::from_read(abi));
        assert!(!plan[0].access.contains(AccessFs::WriteFile));
    }

    #[test]
    fn plan_maps_rw_to_full_access_superset() {
        let ro = vec![PathBuf::from("/a")];
        let rw = vec![PathBuf::from("/b")];
        let plan = plan_from_paths(&ro, &rw);
        let abi = FS_ABI;
        let read_only_access = plan.iter().find(|r| r.path == *"/a").unwrap().access;
        let read_write_access = plan.iter().find(|r| r.path == *"/b").unwrap().access;
        assert!(
            read_write_access.contains(read_only_access),
            "rw is superset of ro"
        );
        assert_eq!(read_write_access, AccessFs::from_all(abi));
    }

    #[test]
    fn concrete_file_rules_never_request_directory_rights() {
        let temp = tempfile::NamedTempFile::new().expect("temp file");
        let fd = PathFd::new(temp.path()).expect("open temp file");
        let access = access_for_fd(&fd, AccessFs::from_all(FS_ABI)).expect("fstat");
        assert!(!access.contains(AccessFs::ReadDir));
        assert!(!access.contains(AccessFs::MakeDir));
        assert!(access.contains(AccessFs::ReadFile));
        assert!(access.contains(AccessFs::IoctlDev));
    }
}
