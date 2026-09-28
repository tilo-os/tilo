//! Landlock ruleset construction.
//!
//! A ruleset is described as data first — [`RulesetSpec`] — and installed by
//! one function, [`restrict_self`]. Keeping the description pure means the
//! exact policy a caller builds (handled rights, compatibility level, every
//! rule) can be asserted in a unit test without performing the irreversible
//! syscall, and the imperative part exists once.
//!
//! Landlock is path-based and irrevocable for the calling process and every
//! descendant: [`restrict_self`] must run in the process about to exec the
//! confined program, after any mount namespace it relies on is complete.

use std::os::fd::{AsFd, AsRawFd};
use std::path::{Path, PathBuf};

use landlock::{
    ABI, Access, AccessFs, AccessNet, BitFlags, CompatLevel, Compatible, NetPort, PathBeneath,
    PathFd, PathFdError, RestrictionStatus, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetError,
    RulesetStatus,
};

/// Tilo requires ABI 5 so device-node ioctls are policy-controlled. ABI 6 scope
/// enforcement remains blocked on kernels that cannot attest the relevant
/// Landlock errata; accepting a `PartiallyEnforced` result would be ambiguous.
pub const TARGET_ABI: ABI = ABI::V5;
/// The ABI whose filesystem rights an application ruleset handles.
pub const FS_ABI: ABI = TARGET_ABI;

/// One filesystem rule: a path and the rights granted beneath it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsRule {
    pub path: PathBuf,
    pub access: BitFlags<AccessFs>,
}

/// Why a ruleset could not be built or installed.
#[derive(Debug, thiserror::Error)]
pub enum LandlockBuildError {
    #[error(transparent)]
    Ruleset(#[from] RulesetError),
    #[error("could not inspect an opened policy object: {0}")]
    Inspect(#[from] std::io::Error),
}

/// What the kernel actually enforced, as opposed to what was asked for.
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

/// Map the kernel's ruleset status onto [`LandlockOutcome`].
pub fn outcome_from_status(status: &RulesetStatus) -> LandlockOutcome {
    match status {
        RulesetStatus::FullyEnforced => LandlockOutcome::FullyEnforced,
        RulesetStatus::PartiallyEnforced => LandlockOutcome::PartiallyEnforced,
        RulesetStatus::NotEnforced => LandlockOutcome::NotEnforced,
    }
}

/// Map read-only and read-write path lists to filesystem rules.
///
/// Read-only paths get the read set (execute, read file, read directory) so
/// the dynamic linker can still map libraries and exec the program;
/// read-write paths get every right of the ABI. Read-only rules come first,
/// each list in its given order. An `abi` at or above [`FS_ABI`] is capped
/// to it, so a newer kernel never widens what an application is granted.
pub fn access_plan(ro: &[PathBuf], rw: &[PathBuf], abi: ABI) -> Vec<FsRule> {
    let fs_abi = if abi >= ABI::V5 { FS_ABI } else { abi };
    let read = AccessFs::from_read(fs_abi);
    let all = AccessFs::from_all(fs_abi);
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

/// The network half of an application ruleset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetRestriction {
    /// TCP is not handled by the ruleset at all.
    Unrestricted,
    /// Every TCP bind and connect is denied.
    DenyAll,
    /// Every TCP bind and connect is denied except connecting to this port.
    ConnectOnly(u16),
}

/// A complete Landlock ruleset, described as data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RulesetSpec {
    /// How the ruleset reacts to a kernel that cannot enforce all of it.
    pub compat: CompatLevel,
    /// Filesystem rights the ruleset handles; anything handled and not
    /// granted by a rule is denied.
    pub handled_fs: BitFlags<AccessFs>,
    /// Network rights the ruleset handles. Empty means the network access
    /// space is not handled at all.
    pub handled_net: BitFlags<AccessNet>,
    /// TCP ports the process may connect to.
    pub connect_ports: Vec<u16>,
    /// Filesystem grants, applied in order.
    pub rules: Vec<FsRule>,
    /// Narrow a rule on a non-directory to the rights a file can carry
    /// (see [`access_for_fd`]) instead of letting the kernel reject it.
    pub trim_file_rights: bool,
}

impl RulesetSpec {
    /// The ruleset every sandboxed application runs under: all ABI-5
    /// filesystem rights handled as a hard requirement, so a kernel that
    /// cannot enforce the whole policy fails the build rather than
    /// installing less than was asked for.
    pub fn application(rules: Vec<FsRule>, net: NetRestriction) -> Self {
        let (handled_net, connect_ports) = match net {
            NetRestriction::Unrestricted => (BitFlags::EMPTY, Vec::new()),
            NetRestriction::DenyAll => (AccessNet::from_all(ABI::V4), Vec::new()),
            NetRestriction::ConnectOnly(port) => (AccessNet::from_all(ABI::V4), vec![port]),
        };
        RulesetSpec {
            compat: CompatLevel::HardRequirement,
            handled_fs: AccessFs::from_all(FS_ABI),
            handled_net,
            connect_ports,
            rules,
            trim_file_rights: true,
        }
    }

    /// A ruleset for a process that needs no path and no network at all:
    /// every filesystem right and TCP bind and connect handled, none
    /// granted.
    ///
    /// Best effort, unlike [`RulesetSpec::application`]: a kernel without
    /// some of these rights still enforces the rest. Meant for a process
    /// whose seccomp allow-list already excludes every path and socket
    /// syscall, so Landlock is a second layer there, not the only one.
    pub fn deny_all() -> Self {
        RulesetSpec {
            compat: CompatLevel::BestEffort,
            handled_fs: AccessFs::from_all(FS_ABI),
            handled_net: AccessNet::from_all(ABI::V4),
            connect_ports: Vec::new(),
            rules: Vec::new(),
            trim_file_rights: true,
        }
    }
}

/// Build the ruleset `spec` describes and restrict the calling process to it.
///
/// A rule whose path cannot be opened is reported to `on_skip` and left out:
/// a path that does not exist in the process's mount namespace is already
/// unreachable, and cannot be granted anyway.
///
/// Irreversible for the calling process and every descendant — never call
/// this from a test process that must keep running.
///
/// # Errors
///
/// Returns [`LandlockBuildError`] when the kernel refuses the ruleset (with
/// a hard-requirement spec, including a kernel that cannot enforce all of
/// it), a rule is rejected, or an opened rule path cannot be inspected.
pub fn restrict_self(
    spec: &RulesetSpec,
    mut on_skip: impl FnMut(&Path, &PathFdError),
) -> Result<RestrictionStatus, LandlockBuildError> {
    let mut ruleset = Ruleset::default()
        .set_compatibility(spec.compat)
        .handle_access(spec.handled_fs)?;
    if !spec.handled_net.is_empty() {
        ruleset = ruleset.handle_access(spec.handled_net)?;
    }

    let mut created = ruleset.create()?;

    for port in &spec.connect_ports {
        created = created.add_rule(NetPort::new(*port, AccessNet::ConnectTcp))?;
    }

    for rule in &spec.rules {
        match PathFd::new(&rule.path) {
            Ok(fd) => {
                let access = if spec.trim_file_rights {
                    access_for_fd(&fd, rule.access)?
                } else {
                    rule.access
                };
                created = created.add_rule(PathBeneath::new(fd, access))?;
            }
            Err(e) => on_skip(&rule.path, &e),
        }
    }

    Ok(created.restrict_self()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn landlock_plans_map_ro_and_rw_rules_in_order_without_widening() {
        // Phase `plan_maps_ro_to_read_only_access`.
        {
            let plan = access_plan(&[PathBuf::from("/usr")], &[], FS_ABI);
            assert_eq!(plan.len(), 1);
            assert_eq!(plan[0].access, AccessFs::from_read(FS_ABI));
            assert!(!plan[0].access.contains(AccessFs::WriteFile));
        }
        // Phase `plan_maps_rw_to_full_access_superset`.
        {
            let plan = access_plan(&[PathBuf::from("/a")], &[PathBuf::from("/b")], FS_ABI);
            let ro = plan.iter().find(|r| r.path == *"/a").unwrap().access;
            let rw = plan.iter().find(|r| r.path == *"/b").unwrap().access;
            assert!(rw.contains(ro), "rw is superset of ro");
            assert_eq!(rw, AccessFs::from_all(FS_ABI));
            assert!(rw.contains(AccessFs::MakeReg));
        }
        // Phase `plan_preserves_order_ro_then_rw`.
        {
            let plan = access_plan(
                &[PathBuf::from("/a"), PathBuf::from("/b")],
                &[PathBuf::from("/c")],
                FS_ABI,
            );
            let paths: Vec<_> = plan.iter().map(|r| r.path.clone()).collect();
            assert_eq!(
                paths,
                vec![
                    PathBuf::from("/a"),
                    PathBuf::from("/b"),
                    PathBuf::from("/c")
                ]
            );
        }
        // Phase `a_newer_abi_never_widens_an_application_plan`.
        {
            let newer = access_plan(&[], &[PathBuf::from("/a")], ABI::V6);
            assert_eq!(newer[0].access, AccessFs::from_all(FS_ABI));
        }
    }

    #[test]
    fn landlock_rules_request_exactly_their_rights_and_the_ruleset_is_pinned() {
        // Phase `concrete_file_rules_never_request_directory_rights`.
        {
            let temp = tempfile::NamedTempFile::new().expect("temp file");
            let fd = PathFd::new(temp.path()).expect("open temp file");
            let access = access_for_fd(&fd, AccessFs::from_all(FS_ABI)).expect("fstat");
            assert!(!access.contains(AccessFs::ReadDir));
            assert!(!access.contains(AccessFs::MakeDir));
            assert!(access.contains(AccessFs::ReadFile));
            assert!(access.contains(AccessFs::IoctlDev));
        }
        // Phase `directory_rules_keep_every_requested_right`.
        {
            let dir = tempfile::tempdir().expect("temp dir");
            let fd = PathFd::new(dir.path()).expect("open temp dir");
            let access = access_for_fd(&fd, AccessFs::from_all(FS_ABI)).expect("fstat");
            assert_eq!(access, AccessFs::from_all(FS_ABI));
        }
        // Phase `the_application_ruleset_is_pinned`.
        // Bits (landlock ABI order): Execute 1, WriteFile 2, ReadFile 4,
        // ReadDir 8, RemoveDir 16, RemoveFile 32, MakeChar 64, MakeDir 128,
        // MakeReg 256, MakeSock 512, MakeFifo 1024, MakeBlock 2048,
        // MakeSym 4096, Refer 8192, Truncate 16384, IoctlDev 32768;
        // BindTcp 1, ConnectTcp 2.
        {
            let rules = vec![FsRule {
                path: PathBuf::from("/usr"),
                access: AccessFs::from_read(FS_ABI),
            }];

            let offline = RulesetSpec::application(rules.clone(), NetRestriction::Unrestricted);
            assert_eq!(offline.compat, CompatLevel::HardRequirement);
            assert_eq!(offline.handled_fs.bits(), 0xffff);
            assert!(offline.handled_net.is_empty());
            assert!(offline.connect_ports.is_empty());
            assert_eq!(offline.rules, rules);
            assert!(offline.trim_file_rights);

            let denied = RulesetSpec::application(rules.clone(), NetRestriction::DenyAll);
            assert_eq!(denied.handled_net.bits(), 0b11);
            assert!(denied.connect_ports.is_empty());

            let gated = RulesetSpec::application(rules, NetRestriction::ConnectOnly(4711));
            assert_eq!(gated.handled_net.bits(), 0b11);
            assert_eq!(gated.connect_ports, vec![4711]);
        }
    }

    #[test]
    fn landlock_outcomes_cover_every_status_and_report_partial_enforcement() {
        // Phase `outcome_mapping_covers_all_ruleset_statuses`.
        {
            assert_eq!(
                outcome_from_status(&RulesetStatus::FullyEnforced),
                LandlockOutcome::FullyEnforced
            );
            assert_eq!(
                outcome_from_status(&RulesetStatus::PartiallyEnforced),
                LandlockOutcome::PartiallyEnforced
            );
            assert_eq!(
                outcome_from_status(&RulesetStatus::NotEnforced),
                LandlockOutcome::NotEnforced
            );
        }
        // Phase `outcomes_short_of_full_enforcement_say_so`.
        {
            assert!(LandlockOutcome::NotEnforced.describe().contains("NOT"));
            assert!(LandlockOutcome::Errored.describe().contains("failed"));
            assert!(
                LandlockOutcome::PartiallyEnforced
                    .describe()
                    .contains("partially")
            );
        }
    }

    /// Live-kernel enforcement: a forked child installs an application
    /// ruleset granting one directory read-only, then proves a granted read
    /// succeeds and a write beneath it is refused. Forked because
    /// `restrict_self` cannot be undone.
    ///
    /// ```bash
    /// cargo test -p tilo-sandbox-core --lib -- --ignored application_ruleset_enforces
    /// ```
    #[test]
    #[ignore = "requires a live Landlock ABI 5 kernel; run with --ignored"]
    fn application_ruleset_enforces_read_only_in_forked_child() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let base = tempfile::tempdir().unwrap();
        let ro_dir = base.path().join("ro");
        std::fs::create_dir_all(&ro_dir).unwrap();
        std::fs::write(ro_dir.join("readable.txt"), b"hello").unwrap();
        let read_path = CString::new(ro_dir.join("readable.txt").as_os_str().as_bytes()).unwrap();
        let write_path = CString::new(ro_dir.join("nope.txt").as_os_str().as_bytes()).unwrap();
        let spec = RulesetSpec::application(
            access_plan(std::slice::from_ref(&ro_dir), &[], FS_ABI),
            NetRestriction::DenyAll,
        );

        // SAFETY: the child performs Landlock syscalls, open/close and
        // `_exit`, and never returns into the test harness.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            let exit = |code: i32| -> ! {
                // SAFETY: `_exit` does not unwind or run destructors.
                unsafe { libc::_exit(code) }
            };
            match restrict_self(&spec, |_, _| {}) {
                Ok(status) if status.ruleset == RulesetStatus::FullyEnforced => {}
                _ => exit(12),
            }
            // SAFETY: NUL-terminated path; the fd is closed below.
            let rfd = unsafe { libc::open(read_path.as_ptr(), libc::O_RDONLY) };
            if rfd < 0 {
                exit(11);
            }
            // SAFETY: `rfd` is a descriptor this child just opened.
            unsafe { libc::close(rfd) };
            // SAFETY: NUL-terminated path; a success is closed before exiting.
            let wfd =
                unsafe { libc::open(write_path.as_ptr(), libc::O_WRONLY | libc::O_CREAT, 0o600) };
            if wfd >= 0 {
                exit(10);
            }
            exit(0);
        }

        let mut status: libc::c_int = 0;
        // SAFETY: `pid` is our own child.
        unsafe { libc::waitpid(pid, &raw mut status, 0) };
        assert!(libc::WIFEXITED(status), "child did not exit normally");
        assert_eq!(
            libc::WEXITSTATUS(status),
            0,
            "10 = write not blocked, 11 = granted read denied, \
             12 = the ruleset was not fully enforced"
        );
    }

    #[test]
    fn deny_all_grants_nothing_it_handles() {
        let spec = RulesetSpec::deny_all();
        assert!(spec.rules.is_empty() && spec.connect_ports.is_empty());
        assert_eq!(spec.handled_fs, AccessFs::from_all(FS_ABI));
        assert_eq!(spec.handled_net, AccessNet::from_all(ABI::V4));
    }
}
