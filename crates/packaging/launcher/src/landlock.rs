//! Launcher-side Landlock helpers.
//!
//! Landlock is layered on top of bwrap + seccomp. The launcher's job:
//! derive an FS plan (read-only paths get the read access set, read-write
//! paths get the full access set), then install the ruleset on the *child*
//! process after bwrap has set up the mount namespace but before the
//! entrypoint takes over — bwrap itself needs broad FS access to
//! `mount/bind/pivot_root`, so it can't be Landlocked.
//!
//! Ruleset construction itself lives in `sandbox_core::landlock`, shared
//! with every other confinement path; this module is the launcher's
//! vocabulary over it.
//!
//! Production launches use the in-namespace `__sandbox-exec` path in
//! [`crate::landlock_shim`]: bwrap creates the mount namespace, execs the
//! launcher at its private read-only bind, and that shim installs Landlock
//! before replacing itself with the application. [`apply_plan_or_log`] remains the
//! small in-process primitive used by focused probes and tests.
//!
//! Landlock policies are path-based and evaluated against the kernel inode.
//! This is why the production rule plan is resolved after bwrap: policy paths
//! name the sandbox-side mounts the application will actually see, rather than
//! similarly spelled host paths.

use std::path::PathBuf;

pub use sandbox_core::landlock::{
    FS_ABI, FsRule, LandlockBuildError, LandlockOutcome, NetRestriction, RulesetSpec, TARGET_ABI,
    access_for_fd, outcome_from_status,
};

/// Produce the FS access plan for a read list and a write list.
///
/// The lists come from the launcher, not a manifest: the system read
/// profile ([`crate::profile`]) and the app's own directory
/// ([`crate::appdir`]). Paths are absolute and literal.
pub fn plan_from_paths(ro: &[PathBuf], rw: &[PathBuf]) -> Vec<FsRule> {
    sandbox_core::landlock::access_plan(ro, rw, FS_ABI)
}

/// Build a ruleset from `plan`, restrict the calling process to it, and
/// report the kernel's actual enforcement status.
///
/// `net_restricted` mirrors `sandbox.net.mode == "off"` (deny TCP
/// bind/connect when set).
///
/// **Caveat re: bwrap.** This irreversibly restricts the *current*
/// process and every descendant, so policy paths must exist in the
/// calling process's mount namespace.
///
/// The in-sandbox shim treats anything short of `FullyEnforced` as a launch
/// refusal.  Keeping the status as data here makes that fail-closed decision
/// independently testable and prevents logging from being confused with
/// enforcement.
pub fn apply_plan_or_log(plan: &[FsRule], net_restricted: bool) -> LandlockOutcome {
    let net = if net_restricted {
        NetRestriction::DenyAll
    } else {
        NetRestriction::Unrestricted
    };
    let spec = RulesetSpec::application(plan.to_vec(), net);
    let restricted = sandbox_core::landlock::restrict_self(&spec, |path, e| {
        tracing::debug!(
            "landlock: skipping rule for {} (cannot open: {e})",
            path.display()
        );
    });
    match restricted {
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
    use landlock::{ABI, Access, AccessFs, AccessNet, CompatLevel, PathFd};

    #[test]
    fn landlock_rules_and_rulesets_are_pinned() {
        {
            let ro = vec![PathBuf::from("/usr")];
            let rw = vec![];
            let plan = plan_from_paths(&ro, &rw);
            assert_eq!(plan.len(), 1);
            let abi = FS_ABI;
            assert_eq!(plan[0].access, AccessFs::from_read(abi));
            assert!(!plan[0].access.contains(AccessFs::WriteFile));
        }
        {
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
        {
            let temp = tempfile::NamedTempFile::new().expect("temp file");
            let fd = PathFd::new(temp.path()).expect("open temp file");
            let access = access_for_fd(&fd, AccessFs::from_all(FS_ABI)).expect("fstat");
            assert!(!access.contains(AccessFs::ReadDir));
            assert!(!access.contains(AccessFs::MakeDir));
            assert!(access.contains(AccessFs::ReadFile));
            assert!(access.contains(AccessFs::IoctlDev));
        }
        // Phase `pinned_ruleset_access_sets`.
        // The access sets that make up every launcher ruleset, as raw bits.
        // A dependency bump or refactor that changes which rights are handled
        // or granted is a policy change and must fail here rather than pass
        // through a comparison of two expressions that moved together.
        // Bits (landlock ABI order): Execute 1, WriteFile 2, ReadFile 4,
        // ReadDir 8, RemoveDir 16, RemoveFile 32, MakeChar 64, MakeDir 128,
        // MakeReg 256, MakeSock 512, MakeFifo 1024, MakeBlock 2048,
        // MakeSym 4096, Refer 8192, Truncate 16384, IoctlDev 32768;
        // BindTcp 1, ConnectTcp 2.
        {
            assert_eq!(AccessFs::from_all(FS_ABI).bits(), 0xffff, "handled FS");
            assert_eq!(AccessNet::from_all(ABI::V4).bits(), 0b11, "handled net");
            assert_eq!(AccessFs::from_file(FS_ABI).bits(), 0xc007, "file rights");

            let plan = plan_from_paths(
                &[PathBuf::from("/usr"), PathBuf::from("/etc")],
                &[PathBuf::from("/app")],
            );
            let pinned: Vec<(PathBuf, u64)> = plan
                .iter()
                .map(|rule| (rule.path.clone(), rule.access.bits()))
                .collect();
            assert_eq!(
                pinned,
                vec![
                    (PathBuf::from("/usr"), 0b1101),
                    (PathBuf::from("/etc"), 0b1101),
                    (PathBuf::from("/app"), 0xffff),
                ]
            );
        }
        // Phase `pinned_in_process_ruleset`.
        // The ruleset [`apply_plan_or_log`] installs: the plan verbatim, every
        // ABI-5 filesystem right handled as a hard requirement, file rules
        // trimmed to file rights, and TCP handled only when restricted.
        {
            let plan = plan_from_paths(&[PathBuf::from("/usr")], &[PathBuf::from("/app")]);
            let open = RulesetSpec::application(plan.clone(), NetRestriction::Unrestricted);
            assert_eq!(open.compat, CompatLevel::HardRequirement);
            assert_eq!(open.handled_fs.bits(), 0xffff);
            assert!(open.handled_net.is_empty());
            assert!(open.connect_ports.is_empty());
            assert!(open.trim_file_rights);
            assert_eq!(open.rules, plan);

            let closed = RulesetSpec::application(plan, NetRestriction::DenyAll);
            assert_eq!(closed.handled_net.bits(), 0b11);
            assert!(closed.connect_ports.is_empty());
        }
    }
}
