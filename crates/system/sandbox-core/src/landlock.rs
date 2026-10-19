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

use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::path::{Path, PathBuf};

use landlock::{
    ABI, Access, AccessFs, AccessNet, BitFlags, CompatLevel, Compatible, Erratum, NetPort,
    PathBeneath, PathFd, PathFdError, RestrictionStatus, Ruleset, RulesetAttr, RulesetCreatedAttr,
    RulesetError, RulesetStatus, Scope,
};

/// Tilo requires ABI 5 so device-node ioctls are policy-controlled. It is the
/// minimum: the ABI 6 scopes ([`SCOPE_ABI`]) are added on top only where the
/// running kernel offers them (see [`kernel_scopes`]).
pub const TARGET_ABI: ABI = ABI::V5;
/// The ABI whose filesystem rights an application ruleset handles.
pub const FS_ABI: ABI = TARGET_ABI;
/// The ABI that introduced IPC scoping (Linux 6.12): abstract Unix sockets
/// and signals are confined to the sandbox's own Landlock domain.
pub const SCOPE_ABI: ABI = ABI::V6;
/// `LANDLOCK_CREATE_RULESET_VERSION`: ask the kernel for its Landlock ABI.
const CREATE_RULESET_VERSION: u32 = 1 << 0;

/// The running kernel's Landlock ABI version; 0 without Landlock.
pub fn kernel_abi() -> i32 {
    // SAFETY: the version query reads no memory (a null attribute, size 0).
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<u8>(),
            0_usize,
            CREATE_RULESET_VERSION,
        )
    };
    i32::try_from(abi).unwrap_or(0).max(0)
}

/// The scopes a kernel at `abi`, with the `fixed` errata, enforces exactly.
///
/// Below ABI 6 there are none: such a kernel keeps today's ABI 5 policy, and
/// the ruleset's `FullyEnforced` status still means everything requested was
/// enforced. The signal scope additionally needs erratum 2 fixed: before it,
/// a restricted thread could not signal sibling threads of its own process
/// that sit in another domain, which breaks a confined process that restricts
/// one thread while others run. Every 6.12 stable kernel since the backport,
/// and every kernel from 6.15, reports it.
pub fn kernel_scopes(abi: i32, fixed: BitFlags<Erratum>) -> BitFlags<Scope> {
    if abi < SCOPE_ABI as i32 {
        return BitFlags::EMPTY;
    }
    let mut scopes = Scope::from_all(SCOPE_ABI);
    if !fixed.contains(Erratum::ScopedSignalHandling) {
        scopes.remove(Scope::Signal);
    }
    scopes
}

/// The scopes of `spec` the running kernel will enforce.
pub fn effective_scopes(spec: &RulesetSpec) -> BitFlags<Scope> {
    spec.scoped & kernel_scopes(kernel_abi(), Erratum::current())
}

/// One log line saying which of the requested `scoped` scopes are enforced
/// (`enforced`) and which this kernel cannot provide.
pub fn describe_scopes(scoped: BitFlags<Scope>, enforced: BitFlags<Scope>) -> String {
    let name = |scope: Scope| match scope {
        Scope::AbstractUnixSocket => "abstract-unix-socket",
        Scope::Signal => "signal",
        _ => "unknown",
    };
    let list = |set: BitFlags<Scope>| set.iter().map(name).collect::<Vec<_>>().join(", ");
    let missing = scoped & !enforced;
    match (enforced.is_empty(), missing.is_empty()) {
        (_, true) if scoped.is_empty() => "landlock scopes: none requested".to_string(),
        (_, true) => format!("landlock scopes: {} enforced", list(enforced)),
        (true, false) => format!(
            "landlock scopes: unavailable on this kernel (Landlock ABI {}, scopes need ABI 6): {} not enforced",
            kernel_abi(),
            list(missing)
        ),
        (false, false) => format!(
            "landlock scopes: {} enforced; {} unavailable on this kernel",
            list(enforced),
            list(missing)
        ),
    }
}

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
    /// IPC scopes: connecting to an abstract Unix socket, or signalling a
    /// process, outside this ruleset's Landlock domain is denied. Enforced
    /// where the kernel offers them ([`kernel_scopes`]) and left out
    /// otherwise, under the same compatibility level as everything else.
    pub scoped: BitFlags<Scope>,
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
            scoped: Scope::from_all(SCOPE_ABI),
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
            scoped: Scope::from_all(SCOPE_ABI),
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
    on_skip: impl FnMut(&Path, &PathFdError),
) -> Result<RestrictionStatus, LandlockBuildError> {
    restrict_self_inner(spec, None, on_skip)
}

/// Add exactly `ReadFile` beneath an already held authenticated directory.
/// This only confines the calling task; filesystem/source ACLs still decide
/// access. There is no pathname reopen or FUSE metadata operation here.
///
/// # Errors
///
/// Returns [`LandlockBuildError`] if the spec lacks complete mandatory filesystem
/// enforcement, the kernel refuses the ruleset or held-directory rule, a rule is
/// rejected, or an opened rule path cannot be inspected.
pub fn restrict_self_with_readonly_directory(
    spec: &RulesetSpec,
    directory: BorrowedFd<'_>,
    on_skip: impl FnMut(&Path, &PathFdError),
) -> Result<RestrictionStatus, LandlockBuildError> {
    if spec.compat != CompatLevel::HardRequirement
        || !spec.handled_fs.contains(AccessFs::from_all(FS_ABI))
    {
        return Err(LandlockBuildError::Inspect(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "Held read rule requires complete mandatory ABI5 policy",
        )));
    }
    restrict_self_inner(spec, Some(directory), on_skip)
}
fn restrict_self_inner(
    spec: &RulesetSpec,
    directory: Option<BorrowedFd<'_>>,
    mut on_skip: impl FnMut(&Path, &PathFdError),
) -> Result<RestrictionStatus, LandlockBuildError> {
    let mut ruleset = Ruleset::default()
        .set_compatibility(spec.compat)
        .handle_access(spec.handled_fs)?;
    if !spec.handled_net.is_empty() {
        ruleset = ruleset.handle_access(spec.handled_net)?;
    }
    let scoped = effective_scopes(spec);
    if !scoped.is_empty() {
        ruleset = ruleset.scope(scoped)?;
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

    if let Some(directory) = directory {
        add_held_read_rule(&created, directory)?;
    }
    Ok(created.restrict_self()?)
}

#[repr(C, packed)]
struct HeldReadAttribute {
    allowed_access: u64,
    parent_fd: i32,
}
fn held_read_attribute(directory: BorrowedFd<'_>) -> HeldReadAttribute {
    HeldReadAttribute {
        allowed_access: AccessFs::ReadFile as u64,
        parent_fd: directory.as_raw_fd(),
    }
}
fn add_held_read_rule(
    created: &landlock::RulesetCreated,
    directory: BorrowedFd<'_>,
) -> Result<(), LandlockBuildError> {
    // PathBeneath compatibility processing fstats its parent, which cannot
    // observe an Owner-only FUSE mount before the dropped worker services it.
    // Public clone/conversion borrows the SAME kernel ruleset, never a second
    // restriction or private dependency layout. Original restrict_self retains
    // canonical ABI/compatibility/NNP/network enforcement and status reporting.
    let clone: Option<OwnedFd> = created.try_clone()?.into();
    let ruleset = clone.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Held read rule has no enforced ruleset",
        )
    })?;
    let attr = held_read_attribute(directory);
    if unsafe {
        libc::syscall(
            libc::SYS_landlock_add_rule,
            ruleset.as_raw_fd(),
            1u32,
            &raw const attr,
            0u32,
        )
    } != 0
    {
        return Err(LandlockBuildError::Inspect(std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::OpenOptionsExt;

    #[test]
    fn held_read_rule_has_only_readfile_on_exact_borrowed_fd_and_requires_mandatory_policy() {
        let directory = std::fs::File::options()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open("/")
            .unwrap();
        let attr = held_read_attribute(directory.as_fd());
        let access = attr.allowed_access;
        let fd = attr.parent_fd;
        assert_eq!(access, AccessFs::ReadFile as u64);
        assert_eq!(fd, directory.as_raw_fd());
        // Deny-all uses best effort. Refusal occurs before any restriction,
        // and does not fabricate kernel/native enforcement from a mask test.
        assert!(
            restrict_self_with_readonly_directory(
                &RulesetSpec::deny_all(),
                directory.as_fd(),
                |_, _| {}
            )
            .is_err()
        );
    }
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
    fn abi_6_scopes_are_enforced_exactly_where_the_kernel_offers_them() {
        let both = Scope::AbstractUnixSocket | Scope::Signal;
        let all_errata = BitFlags::<Erratum>::all();
        // ABI 5 is the floor and keeps today's policy: no scope is asked for,
        // so a hard-requirement ruleset still builds and is fully enforced.
        for abi in 0..=5 {
            assert!(kernel_scopes(abi, all_errata).is_empty(), "ABI {abi}");
        }
        assert_eq!(kernel_scopes(6, all_errata), both);
        assert_eq!(kernel_scopes(7, all_errata), both);
        assert_eq!(kernel_scopes(9, all_errata), both);
        // A kernel still carrying erratum 2 gets the socket scope only.
        assert_eq!(
            kernel_scopes(6, BitFlags::EMPTY),
            BitFlags::from(Scope::AbstractUnixSocket)
        );

        // Every confining ruleset asks for both scopes.
        let app = RulesetSpec::application(Vec::new(), NetRestriction::Unrestricted);
        assert_eq!(app.scoped, both);
        assert_eq!(RulesetSpec::deny_all().scoped, both);

        // The launch log says what happened, including on an ABI 5 kernel.
        assert_eq!(
            describe_scopes(both, both),
            "landlock scopes: abstract-unix-socket, signal enforced"
        );
        let unavailable = describe_scopes(both, BitFlags::EMPTY);
        assert!(unavailable.contains("unavailable"), "{unavailable}");
        assert!(unavailable.contains("abstract-unix-socket, signal"));
        let partial = describe_scopes(both, Scope::AbstractUnixSocket.into());
        assert!(partial.contains("abstract-unix-socket enforced; signal unavailable"));
    }

    /// Live kernel: a forked child installs the application ruleset (what
    /// the launcher's sandbox shim installs) and then tries to reach
    /// processes outside its domain. On a kernel with ABI 6 the host's
    /// abstract socket and a signal to the host process are refused; on an
    /// ABI 5 kernel both still work as before. Either way a path socket
    /// bound outside the sandbox (how Wayland, the private D-Bus, the
    /// brokers and the X11 bridge are reached) still connects, and abstract
    /// sockets and signals inside the app's own domain keep working.
    #[test]
    fn application_scopes_confine_abstract_sockets_and_signals_to_the_sandbox() {
        use std::os::linux::net::SocketAddrExt as _;
        use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};

        let abi = kernel_abi();
        if abi < FS_ABI as i32 {
            eprintln!("skipped: kernel Landlock ABI {abi} is below the ABI 5 floor");
            return;
        }
        let spec = RulesetSpec::application(Vec::new(), NetRestriction::Unrestricted);
        let enforced = effective_scopes(&spec);
        let id = std::process::id();

        let host_name = format!("tilo-landlock-scope-host-{id}");
        let host_addr = SocketAddr::from_abstract_name(host_name.as_bytes()).unwrap();
        let _host = UnixListener::bind_addr(&host_addr).unwrap();
        // Outside any domain the abstract socket is reachable.
        UnixStream::connect_addr(&host_addr).expect("unconfined connect");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("display.sock");
        let _path_listener = UnixListener::bind(&path).unwrap();
        let path_addr = SocketAddr::from_pathname(&path).unwrap();

        let own_name = format!("tilo-landlock-scope-own-{id}");
        let own_addr = SocketAddr::from_abstract_name(own_name.as_bytes()).unwrap();
        let socket_scoped = enforced.contains(Scope::AbstractUnixSocket);
        let signal_scoped = enforced.contains(Scope::Signal);

        // SAFETY: the child performs Landlock syscalls, socket calls, fork,
        // kill and `_exit` on pre-built addresses, and never returns into
        // the test harness.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            let exit = |code: i32| -> ! {
                // SAFETY: `_exit` does not unwind or run destructors.
                unsafe { libc::_exit(code) }
            };
            match restrict_self(&spec, |_, _| {}) {
                Ok(status) if status.ruleset == RulesetStatus::FullyEnforced => {}
                _ => exit(20),
            }
            let eperm = |e: &std::io::Error| e.raw_os_error() == Some(libc::EPERM);
            match (UnixStream::connect_addr(&host_addr), socket_scoped) {
                (Err(e), true) if eperm(&e) => {}
                (Ok(_), false) => {}
                (Ok(_), true) => exit(21),
                _ => exit(22),
            }
            // SAFETY: plain syscall; SIGCONT to a running process is a no-op.
            let signalled = unsafe { libc::kill(libc::getppid(), libc::SIGCONT) };
            match (signalled, signal_scoped) {
                (-1, true) if eperm(&std::io::Error::last_os_error()) => {}
                (0, false) => {}
                (0, true) => exit(23),
                _ => exit(24),
            }
            if UnixStream::connect_addr(&path_addr).is_err() {
                exit(25);
            }
            let Ok(_own) = UnixListener::bind_addr(&own_addr) else {
                exit(26)
            };
            if UnixStream::connect_addr(&own_addr).is_err() {
                exit(27);
            }
            // SAFETY: the grandchild only pauses until it is killed.
            let child = unsafe { libc::fork() };
            if child == 0 {
                loop {
                    // SAFETY: waits for a signal; never returns normally.
                    unsafe { libc::pause() };
                }
            }
            // SAFETY: `child` is this process's own child, in its domain.
            if child < 0 || unsafe { libc::kill(child, libc::SIGKILL) } != 0 {
                exit(28);
            }
            // SAFETY: reaping our own child.
            unsafe { libc::waitpid(child, std::ptr::null_mut(), 0) };
            exit(0);
        }

        let mut status: libc::c_int = 0;
        // SAFETY: `pid` is our own child.
        unsafe { libc::waitpid(pid, &raw mut status, 0) };
        assert!(libc::WIFEXITED(status), "child did not exit normally");
        assert_eq!(
            libc::WEXITSTATUS(status),
            0,
            "kernel ABI {abi}, enforced {enforced:?}: 20 = not fully enforced, \
             21 = host abstract socket reachable under the scope, 22 = abstract connect \
             failed unexpectedly, 23 = host signalled under the scope, 24 = signal failed \
             unexpectedly, 25 = path socket refused, 26/27 = own abstract socket refused, \
             28 = own child could not be signalled"
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
