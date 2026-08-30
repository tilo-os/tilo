//! Landlock launcher shim — in-kernel per-capability FS/net enforcement
//! layered on top of the bwrap + seccomp + netns sandbox.
//!
//! `bwrap` needs broad filesystem access to build the sandbox (mount, bind,
//! `pivot_root`), so it can't be Landlocked itself. Landlock is path-based
//! and irrevocable for a process and its descendants, so it has to apply
//! to the *app* process after bwrap has finished setting up the mount
//! namespace — hence bwrap execs this shim (via the hidden
//! `shell __sandbox-exec` subcommand) instead of the app directly, and the
//! shim execs the app itself once restricted.
//!
//! Landlock is never allowed to fail a launch: the ruleset is built
//! best-effort, and if Landlock is missing or any step errors, the shim
//! logs and still execs the app — bwrap + seccomp + netns remain in force
//! regardless.
//!
//! Paths here are the sandbox-side (destination) paths of the profile
//! binds: the shim runs inside bwrap's mount namespace, where those are
//! what actually exist, not the host source paths.

use std::ffi::CString;
use std::path::PathBuf;

use landlock::{
    ABI, Access, AccessFs, AccessNet, BitFlags, PathBeneath, PathFd, RestrictionStatus, Ruleset,
    RulesetAttr, RulesetCreatedAttr, RulesetError, RulesetStatus,
};

/// Hidden subcommand token the `shell` binary dispatches on to enter the
/// shim (see `src/main.rs`). bwrap is told to exec
/// `<shell> __sandbox-exec …` as its command.
pub const SANDBOX_EXEC_SUBCOMMAND: &str = "__sandbox-exec";

/// Highest Landlock ABI this shim targets. We deliberately cap at V4
/// rather than chasing the newest ABI for two reasons:
///
/// * V4 is the ABI that introduces network (TCP bind/connect) control,
///   which is the highest right we actively *use* here.
/// * Capping below the `IoctlDev` right (V5) means device ioctls stay
///   unrestricted, so granted hardware (`/dev/video*`, `/dev/snd`) keeps
///   working without us having to special-case `VIDIOC_*`/ALSA ioctls.
///
/// Best-effort downgrade handles older kernels: on a V1–V3 kernel the
/// network access set is empty (so `--net-restricted` silently becomes a
/// no-op) and any FS rights newer than that kernel are dropped.
pub const TARGET_ABI: ABI = ABI::V4;

/// Parsed form of the shim argv: the FS rule set, the net policy, and the
/// real command to exec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShimArgs {
    /// Read-only (read + execute) paths — the sandbox-side targets of the
    /// profile's `binds_ro`.
    pub ro: Vec<PathBuf>,
    /// Read-write (read + write + create) paths — the sandbox-side
    /// targets of the profile's `binds_rw`.
    pub rw: Vec<PathBuf>,
    /// When true, deny all TCP bind/connect (ABI ≥ 4); mirrors the
    /// profile's `net_isolated` (`--unshare-net`) flag.
    pub net_restricted: bool,
    /// The real app argv to `execvp` after the ruleset is applied.
    pub command: Vec<String>,
}

/// Failures parsing the shim argv. These abort the shim with a non-zero
/// exit *before* any app is launched — a malformed shim invocation is a
/// compositor bug, not a sandbox-policy decision.
#[derive(Debug, PartialEq, Eq)]
pub enum ShimParseError {
    /// A `--ro`/`--rw` flag had no path following it.
    DanglingValue(String),
    /// No `--` separator was found; we can't tell rules from command.
    MissingSeparator,
    /// The `--` separator was present but no command followed it.
    EmptyCommand,
}

impl std::fmt::Display for ShimParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShimParseError::DanglingValue(flag) => {
                write!(f, "flag `{flag}` expects a path argument")
            }
            ShimParseError::MissingSeparator => {
                write!(f, "missing `--` separator before the command")
            }
            ShimParseError::EmptyCommand => write!(f, "no command after `--`"),
        }
    }
}

impl std::error::Error for ShimParseError {}

impl ShimArgs {
    /// Parse the slice that follows the `__sandbox-exec` token, i.e.
    /// `[--ro <p>]… [--rw <p>]… [--net-restricted] -- <cmd> [args…]`.
    ///
    /// Unknown flags before `--` are ignored (forward-compat: a newer
    /// compositor may emit flags an older shim doesn't grok — better to
    /// drop the extra rule than refuse the launch).
    pub fn parse(args: &[String]) -> Result<ShimArgs, ShimParseError> {
        let mut ro = Vec::new();
        let mut rw = Vec::new();
        let mut net_restricted = false;
        let mut i = 0;
        let mut saw_separator = false;
        let mut command = Vec::new();

        while i < args.len() {
            let arg = &args[i];
            match arg.as_str() {
                "--" => {
                    saw_separator = true;
                    command.extend_from_slice(&args[i + 1..]);
                    break;
                }
                "--net-restricted" => net_restricted = true,
                "--ro" | "--rw" => {
                    let value = args
                        .get(i + 1)
                        .ok_or_else(|| ShimParseError::DanglingValue(arg.clone()))?;
                    if arg == "--ro" {
                        ro.push(PathBuf::from(value));
                    } else {
                        rw.push(PathBuf::from(value));
                    }
                    i += 1;
                }
                _ => {}
            }
            i += 1;
        }

        if !saw_separator {
            return Err(ShimParseError::MissingSeparator);
        }
        if command.is_empty() {
            return Err(ShimParseError::EmptyCommand);
        }
        Ok(ShimArgs {
            ro,
            rw,
            net_restricted,
            command,
        })
    }
}

/// Outcome of applying the Landlock ruleset, used for logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LandlockOutcome {
    /// Every requested access right was enforced by the kernel.
    FullyEnforced,
    /// The kernel enforced a subset (older ABI dropped some rights via
    /// best-effort). Still a real restriction.
    PartiallyEnforced,
    /// The kernel has no Landlock support — nothing was enforced.
    NotEnforced,
    /// Ruleset construction errored; we logged and fell through to exec.
    Errored,
}

impl LandlockOutcome {
    pub fn describe(self) -> &'static str {
        match self {
            LandlockOutcome::FullyEnforced => "landlock: fully enforced",
            LandlockOutcome::PartiallyEnforced => {
                "landlock: partially enforced (kernel ABI dropped some rights); \
                 bwrap + seccomp + netns still apply"
            }
            LandlockOutcome::NotEnforced => {
                "landlock: NOT enforced (kernel lacks Landlock); relying on \
                 bwrap + seccomp + netns"
            }
            LandlockOutcome::Errored => {
                "landlock: ruleset construction failed; relying on bwrap + \
                 seccomp + netns"
            }
        }
    }
}

/// Map the crate's `RulesetStatus` to our log-friendly outcome. Split out
/// (and `pub(crate)`) so it is unit-testable without touching the kernel —
/// the `RulesetStatus` variants are constructible directly.
pub(crate) fn outcome_from_status(status: RulesetStatus) -> LandlockOutcome {
    match status {
        RulesetStatus::FullyEnforced => LandlockOutcome::FullyEnforced,
        RulesetStatus::PartiallyEnforced => LandlockOutcome::PartiallyEnforced,
        RulesetStatus::NotEnforced => LandlockOutcome::NotEnforced,
    }
}

/// A single resolved Landlock FS rule: a path plus the access rights it
/// is granted. Produced by [`access_plan`] and consumed by
/// [`build_and_restrict`]. Kept as its own type so the *derivation* of
/// rights from the capability grants is unit-testable without performing
/// any Landlock syscall.
#[derive(Debug, Clone)]
pub struct FsRule {
    pub path: PathBuf,
    pub access: BitFlags<AccessFs>,
}

/// Derive the FS rule set from the resolved ro/rw paths at a given ABI.
///
/// `--ro` paths get the *read* access set (read + execute + read-dir) so
/// the dynamic linker can still map `/usr` libraries and exec the app;
/// `--rw` paths get the *full* access set (read + write + create + remove
/// …). This is the "ro→read, rw→read+write" capability mapping, pure and
/// side-effect free.
pub fn access_plan(ro: &[PathBuf], rw: &[PathBuf], abi: ABI) -> Vec<FsRule> {
    let read = AccessFs::from_read(abi);
    let all = AccessFs::from_all(abi);
    let mut plan = Vec::with_capacity(ro.len() + rw.len());
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

/// Applies the Landlock ruleset described by `args` to the current process.
/// Best-effort: unsupported rights are downgraded by the crate.
///
/// The only function here that performs Landlock syscalls and permanently
/// restricts the calling process — never call this from a unit test (it
/// would lock down the test runner). The pure pieces it composes
/// ([`access_plan`], [`outcome_from_status`], [`ShimArgs::parse`]) carry
/// the testable logic.
fn build_and_restrict(args: &ShimArgs) -> Result<RestrictionStatus, RulesetError> {
    let abi = TARGET_ABI;

    let mut ruleset = Ruleset::default().handle_access(AccessFs::from_all(abi))?;

    // ABI < 4 makes AccessNet::from_all empty, so this is a no-op there.
    if args.net_restricted {
        ruleset = ruleset.handle_access(AccessNet::from_all(abi))?;
    }

    let mut created = ruleset.create()?;

    // FS rules. We open each path with `PathFd` (O_PATH); a path that
    // doesn't exist inside the namespace simply can't be granted, so we
    // skip it rather than abort the whole ruleset.
    for rule in access_plan(&args.ro, &args.rw, abi) {
        match PathFd::new(&rule.path) {
            Ok(fd) => {
                created = created.add_rule(PathBeneath::new(fd, rule.access))?;
            }
            Err(e) => {
                tracing::debug!(
                    "landlock: skipping rule for {} (cannot open: {e})",
                    rule.path.display()
                );
            }
        }
    }

    // No `NetPort` rules added: handling the net access space (above)
    // with an empty rule set denies all TCP bind/connect on its own.

    created.restrict_self()
}

/// Never panics; on any error returns [`LandlockOutcome::Errored`] after
/// logging.
pub fn apply(args: &ShimArgs) -> LandlockOutcome {
    match build_and_restrict(args) {
        Ok(status) => outcome_from_status(status.ruleset),
        Err(e) => {
            tracing::warn!("landlock: failed to build/apply ruleset: {e}");
            LandlockOutcome::Errored
        }
    }
}

/// Entry point for the `shell __sandbox-exec …` hidden subcommand. `args`
/// is the slice after the `__sandbox-exec` token itself.
pub fn run_sandbox_exec(args: &[String]) -> ! {
    let parsed = match ShimArgs::parse(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("qos-sandbox-exec: {e}");
            std::process::exit(2);
        }
    };

    let outcome = apply(&parsed);
    match outcome {
        LandlockOutcome::FullyEnforced | LandlockOutcome::PartiallyEnforced => {
            tracing::info!(
                "{} ({} ro, {} rw, net_restricted={})",
                outcome.describe(),
                parsed.ro.len(),
                parsed.rw.len(),
                parsed.net_restricted
            );
        }
        LandlockOutcome::NotEnforced | LandlockOutcome::Errored => {
            // Surface on stderr too: tracing may be filtered, and a
            // missing in-kernel layer is worth a visible line.
            eprintln!("qos-sandbox-exec: {}", outcome.describe());
            tracing::warn!("{}", outcome.describe());
        }
    }

    exec_real_command(&parsed.command)
}

/// `execvp` the real command, replacing the process image. Only returns
/// (via `exit`) on failure.
fn exec_real_command(command: &[String]) -> ! {
    // `ShimArgs::parse` guarantees a non-empty command, but guard anyway.
    let Some(program) = command.first() else {
        eprintln!("qos-sandbox-exec: empty command");
        std::process::exit(2);
    };

    let cprogram = if let Ok(c) = CString::new(program.as_bytes()) {
        c
    } else {
        eprintln!("qos-sandbox-exec: program path contains an interior NUL");
        std::process::exit(2);
    };

    let cargs: Vec<CString> = if let Ok(v) = command
        .iter()
        .map(|a| CString::new(a.as_bytes()))
        .collect::<Result<_, _>>()
    {
        v
    } else {
        eprintln!("qos-sandbox-exec: argument contains an interior NUL");
        std::process::exit(2);
    };

    let mut argv_ptrs: Vec<*const libc::c_char> = cargs.iter().map(|c| c.as_ptr()).collect();
    argv_ptrs.push(std::ptr::null());

    // SAFETY: `cprogram` and `cargs` outlive the call; `argv_ptrs` is
    // NULL-terminated. `execvp` only returns on error.
    unsafe {
        libc::execvp(cprogram.as_ptr(), argv_ptrs.as_ptr());
    }

    let err = std::io::Error::last_os_error();
    eprintln!("qos-sandbox-exec: failed to exec {program:?}: {err}");
    std::process::exit(127);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(std::string::ToString::to_string).collect()
    }

    #[test]
    fn parse_extracts_ro_rw_net_and_command() {
        let args = s(&[
            "--ro",
            "/usr",
            "--ro",
            "/run/user/1000/wayland-0",
            "--rw",
            "/work",
            "--net-restricted",
            "--",
            "/usr/bin/terminal",
            "--flag",
        ]);
        let parsed = ShimArgs::parse(&args).expect("parse");
        assert_eq!(
            parsed.ro,
            vec![
                PathBuf::from("/usr"),
                PathBuf::from("/run/user/1000/wayland-0")
            ]
        );
        assert_eq!(parsed.rw, vec![PathBuf::from("/work")]);
        assert!(parsed.net_restricted);
        assert_eq!(parsed.command, s(&["/usr/bin/terminal", "--flag"]));
    }

    #[test]
    fn parse_without_net_flag_leaves_net_unrestricted() {
        let args = s(&["--rw", "/work", "--", "/bin/true"]);
        let parsed = ShimArgs::parse(&args).expect("parse");
        assert!(!parsed.net_restricted);
        assert!(parsed.ro.is_empty());
        assert_eq!(parsed.rw, vec![PathBuf::from("/work")]);
    }

    #[test]
    fn parse_requires_separator() {
        let args = s(&["--ro", "/usr", "/bin/true"]);
        assert_eq!(
            ShimArgs::parse(&args),
            Err(ShimParseError::MissingSeparator)
        );
    }

    #[test]
    fn parse_rejects_empty_command() {
        let args = s(&["--ro", "/usr", "--"]);
        assert_eq!(ShimArgs::parse(&args), Err(ShimParseError::EmptyCommand));
    }

    #[test]
    fn parse_rejects_dangling_flag_value() {
        let args = s(&["--ro"]);
        assert_eq!(
            ShimArgs::parse(&args),
            Err(ShimParseError::DanglingValue("--ro".to_string()))
        );
    }

    #[test]
    fn parse_ignores_unknown_flags_before_separator() {
        let args = s(&["--future-thing", "--rw", "/work", "--", "/bin/true"]);
        let parsed = ShimArgs::parse(&args).expect("parse");
        assert_eq!(parsed.rw, vec![PathBuf::from("/work")]);
        assert_eq!(parsed.command, s(&["/bin/true"]));
    }

    #[test]
    fn parse_preserves_command_args_including_double_dash_lookalikes() {
        let args = s(&["--rw", "/work", "--", "/bin/sh", "-c", "echo --ro"]);
        let parsed = ShimArgs::parse(&args).expect("parse");
        assert_eq!(parsed.command, s(&["/bin/sh", "-c", "echo --ro"]));
    }

    #[test]
    fn access_plan_maps_ro_to_read_and_rw_to_readwrite() {
        let abi = TARGET_ABI;
        let ro = vec![PathBuf::from("/usr")];
        let rw = vec![PathBuf::from("/work")];
        let plan = access_plan(&ro, &rw, abi);
        assert_eq!(plan.len(), 2);

        let ro_rule = plan.iter().find(|r| r.path == *"/usr").unwrap();
        assert_eq!(ro_rule.access, AccessFs::from_read(abi));
        assert!(
            !ro_rule.access.contains(AccessFs::WriteFile),
            "read-only rule must not grant WriteFile"
        );

        let rw_rule = plan.iter().find(|r| r.path == *"/work").unwrap();
        assert_eq!(rw_rule.access, AccessFs::from_all(abi));
        assert!(
            rw_rule.access.contains(AccessFs::WriteFile),
            "read-write rule must grant WriteFile"
        );
        assert!(
            rw_rule.access.contains(AccessFs::MakeReg),
            "read-write rule must grant create (MakeReg)"
        );
        assert!(
            rw_rule.access.contains(ro_rule.access),
            "read-write access must be a superset of read-only access"
        );
    }

    #[test]
    fn access_plan_preserves_order_ro_then_rw() {
        let ro = vec![PathBuf::from("/a"), PathBuf::from("/b")];
        let rw = vec![PathBuf::from("/c")];
        let plan = access_plan(&ro, &rw, TARGET_ABI);
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

    #[test]
    fn net_access_is_present_at_abi_v4_and_empty_below() {
        assert!(
            AccessNet::from_all(ABI::V4).contains(AccessNet::ConnectTcp),
            "ABI V4 must expose TCP connect control"
        );
        assert!(
            AccessNet::from_all(ABI::V4).contains(AccessNet::BindTcp),
            "ABI V4 must expose TCP bind control"
        );
        assert!(
            AccessNet::from_all(ABI::V1).is_empty(),
            "ABI V1 has no network access space (degrades to no-op)"
        );
    }

    #[test]
    fn fs_access_degrades_monotonically_with_abi() {
        // Best-effort relies on newer ABIs being supersets of older ones,
        // so downgrading on old kernels never *adds* an unhonourable right.
        assert!(
            AccessFs::from_all(ABI::V4).contains(AccessFs::from_all(ABI::V1)),
            "V4 FS access must be a superset of V1"
        );
        assert!(
            !AccessFs::from_all(ABI::V1).is_empty(),
            "even the oldest ABI grants basic FS rights"
        );
    }

    #[test]
    fn outcome_mapping_covers_all_ruleset_statuses() {
        assert_eq!(
            outcome_from_status(RulesetStatus::FullyEnforced),
            LandlockOutcome::FullyEnforced
        );
        assert_eq!(
            outcome_from_status(RulesetStatus::PartiallyEnforced),
            LandlockOutcome::PartiallyEnforced
        );
        assert_eq!(
            outcome_from_status(RulesetStatus::NotEnforced),
            LandlockOutcome::NotEnforced
        );
    }

    #[test]
    fn outcome_describe_is_nonempty_for_every_variant() {
        for o in [
            LandlockOutcome::FullyEnforced,
            LandlockOutcome::PartiallyEnforced,
            LandlockOutcome::NotEnforced,
            LandlockOutcome::Errored,
        ] {
            assert!(!o.describe().is_empty());
        }
    }

    /// Live-kernel enforcement test. Forks a child so the irreversible
    /// `restrict_self()` can't lock down the test runner. The child
    /// restricts itself to a single read-only temp dir, then proves that
    /// (a) a granted read succeeds and (b) an out-of-policy *write* into
    /// the ro dir is denied with EACCES. Requires a kernel with Landlock
    /// enabled, so it is `#[ignore]`d by default:
    ///
    /// ```bash
    /// cargo test -p shell --lib -- --ignored landlock_enforces
    /// ```
    #[test]
    #[ignore = "requires a live Landlock-enabled kernel; run with --ignored"]
    fn landlock_enforces_readonly_in_forked_child() {
        use std::os::unix::ffi::OsStrExt;

        let base = std::env::temp_dir().join(format!(
            "qos-landlock-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let ro_dir = base.join("ro");
        std::fs::create_dir_all(&ro_dir).unwrap();
        std::fs::write(ro_dir.join("readable.txt"), b"hello").unwrap();

        // SAFETY: single-threaded test context around the fork; the child
        // performs only async-signal-safe-ish work (Landlock syscalls +
        // libc open) and exits via _exit.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");

        if pid == 0 {
            // Child: restrict to the ro dir (read only), then probe.
            let args = ShimArgs {
                ro: vec![ro_dir.clone()],
                rw: vec![],
                net_restricted: false,
                command: vec!["unused".to_string()],
            };
            let outcome = apply(&args);
            if !matches!(
                outcome,
                LandlockOutcome::FullyEnforced | LandlockOutcome::PartiallyEnforced
            ) {
                // No enforcement on this kernel — report a distinct code
                // so the parent can skip rather than fail.
                unsafe { libc::_exit(42) };
            }

            let read_path =
                CString::new(ro_dir.join("readable.txt").as_os_str().as_bytes()).unwrap();
            let rfd = unsafe { libc::open(read_path.as_ptr(), libc::O_RDONLY) };
            if rfd < 0 {
                unsafe { libc::_exit(10) }; // granted read unexpectedly denied
            }
            unsafe { libc::close(rfd) };

            let write_path = CString::new(ro_dir.join("nope.txt").as_os_str().as_bytes()).unwrap();
            let wfd =
                unsafe { libc::open(write_path.as_ptr(), libc::O_WRONLY | libc::O_CREAT, 0o600) };
            if wfd >= 0 {
                unsafe { libc::close(wfd) };
                unsafe { libc::_exit(11) }; // write should have been denied!
            }
            // Success: read worked, write was blocked.
            unsafe { libc::_exit(0) };
        }

        // Parent: reap and assert.
        let mut status: libc::c_int = 0;
        unsafe { libc::waitpid(pid, &raw mut status, 0) };
        let _ = std::fs::remove_dir_all(&base);

        assert!(libc::WIFEXITED(status), "child did not exit normally");
        let code = libc::WEXITSTATUS(status);
        if code == 42 {
            eprintln!("landlock not enforced on this kernel; skipping assertion");
            return;
        }
        assert_eq!(
            code, 0,
            "child exit code {code}: 10=granted-read-denied, 11=write-not-blocked"
        );
    }
}
