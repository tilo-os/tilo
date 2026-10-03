//! In-sandbox Landlock enforcement — the hidden `__sandbox-exec`
//! subcommand.
//!
//! ## Why a shim
//!
//! `bwrap` needs broad filesystem access to *build* the sandbox (mount,
//! bind, `pivot_root`), so it can't be Landlocked. But Landlock is
//! path-based and irrevocable for the calling process and all its
//! descendants, so it must be applied *inside* the namespace, after
//! mount setup finishes and before the app binary takes over.
//!
//! Achieved by having bwrap `exec` this launcher again (via a bind of
//! its own binary), with a hidden `__sandbox-exec` first argument. The
//! re-entered launcher parses the argv following the token, builds a
//! Landlock ruleset against the *sandbox-side* paths, calls
//! `restrict_self()`, then `execvp`s the target app.
//!
//! tapp applications require this layer.  An unsupported kernel, a partial
//! ABI, or ruleset construction failure refuses the launch instead of quietly
//! running an untrusted package with a weaker boundary than the UI promises.
//! Bubblewrap, seccomp and the network namespace remain independent layers;
//! they are not a reason to make this one optional by accident.

use std::ffi::CString;
use std::path::PathBuf;

use landlock::RestrictionStatus;
pub use sandbox_core::landlock::access_plan;

use crate::landlock::{
    LandlockBuildError, LandlockOutcome, NetRestriction, RulesetSpec, TARGET_ABI,
    outcome_from_status,
};

/// Hidden subcommand token. Dispatched by `main.rs` before clap.
pub const SANDBOX_EXEC_SUBCOMMAND: &str = "__sandbox-exec";

/// Parsed form of the shim argv.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShimArgs {
    /// Read-only paths (read + exec) — sandbox-side.
    pub ro: Vec<PathBuf>,
    /// Read-write paths (full FS access) — sandbox-side.
    pub rw: Vec<PathBuf>,
    /// Deny all TCP bind/connect (ABI ≥ 4).
    pub net_restricted: bool,
    /// Egress gate socket, sandbox-side — when set, the shim starts the
    /// loopback relay before restricting itself.
    pub net_gate: Option<PathBuf>,
    /// Loopback port the relay listens on, and the only TCP port the
    /// app is permitted to connect to.
    pub net_gate_port: Option<u16>,
    /// Skip Landlock entirely. Used when the shim is in the argv only to
    /// host the relay — an empty ruleset denies everything, so "no FS
    /// policy" must be said explicitly.
    pub no_landlock: bool,
    /// Accept Landlock not, or only partially, enforced: a system-store app
    /// on a provider-sandboxed machine ([`crate::provider_sandbox`]).
    pub landlock_best_effort: bool,
    /// Connected broker descriptors intentionally handed to the app. Every
    /// other descriptor above stderr is closed before the entrypoint executes.
    pub keep_fds: Vec<i32>,
    /// The app argv passed to `execvp` after restricting.
    pub command: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ShimParseError {
    DanglingValue(String),
    UnknownFlag(String),
    MissingSeparator,
    EmptyCommand,
}

impl std::fmt::Display for ShimParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShimParseError::DanglingValue(flag) => {
                write!(f, "flag `{flag}` expects a path argument")
            }
            ShimParseError::UnknownFlag(flag) => write!(f, "unknown sandbox flag `{flag}`"),
            ShimParseError::MissingSeparator => write!(f, "missing `--` separator"),
            ShimParseError::EmptyCommand => write!(f, "no command after `--`"),
        }
    }
}

impl std::error::Error for ShimParseError {}

impl ShimArgs {
    /// Parse the slice AFTER the `__sandbox-exec` token.
    ///
    /// Grammar:
    /// `[--ro <p>]* [--rw <p>]* [--keep-fd <n>]* [--net-restricted] [--no-landlock]
    ///  [--net-gate <sock> --net-gate-port <n>] -- <cmd> [args]`.
    /// Unknown flags before `--` are rejected. The shim is a security-policy
    /// parser, so version skew must fail closed rather than silently omitting a
    /// restriction the parent believed it requested.
    ///
    /// # Errors
    ///
    /// Returns [`ShimParseError::DanglingValue`] when a flag that takes a
    /// path/port argument is the last token (or its port value fails to
    /// parse), [`ShimParseError::MissingSeparator`] when there is no `--`,
    /// and [`ShimParseError::EmptyCommand`] when `--` is the last token.
    pub fn parse(args: &[String]) -> Result<ShimArgs, ShimParseError> {
        let mut parsed = ShimArgs::default();
        let mut i: usize = 0;
        let mut saw_separator = false;

        while i < args.len() {
            let Some(arg) = args.get(i) else { break };
            match arg.as_str() {
                "--" => {
                    saw_separator = true;
                    let rest_start = i.saturating_add(1);
                    parsed
                        .command
                        .extend_from_slice(args.get(rest_start..).unwrap_or(&[]));
                    break;
                }
                "--net-restricted" => parsed.net_restricted = true,
                "--no-landlock" => parsed.no_landlock = true,
                "--landlock-best-effort" => parsed.landlock_best_effort = true,
                "--ro" | "--rw" | "--net-gate" | "--net-gate-port" | "--keep-fd" => {
                    let value = args
                        .get(i.saturating_add(1))
                        .ok_or_else(|| ShimParseError::DanglingValue(arg.clone()))?;
                    match arg.as_str() {
                        "--ro" => parsed.ro.push(PathBuf::from(value)),
                        "--rw" => parsed.rw.push(PathBuf::from(value)),
                        "--net-gate" => parsed.net_gate = Some(PathBuf::from(value)),
                        "--keep-fd" => {
                            let fd: i32 = value
                                .parse()
                                .map_err(|_| ShimParseError::DanglingValue(arg.clone()))?;
                            if !(3..=1024).contains(&fd) {
                                return Err(ShimParseError::DanglingValue(arg.clone()));
                            }
                            parsed.keep_fds.push(fd);
                        }
                        // A port we can't parse must not silently become "no port", which
                        // would leave the app pinned by Landlock to nothing, unable to reach
                        // the relay at all.
                        _ => {
                            parsed.net_gate_port = Some(
                                value
                                    .parse()
                                    .map_err(|_| ShimParseError::DanglingValue(arg.clone()))?,
                            );
                        }
                    }
                    i = i.saturating_add(1);
                }
                _ => return Err(ShimParseError::UnknownFlag(arg.clone())),
            }
            i = i.saturating_add(1);
        }

        if !saw_separator {
            return Err(ShimParseError::MissingSeparator);
        }
        if parsed.command.is_empty() {
            return Err(ShimParseError::EmptyCommand);
        }
        Ok(parsed)
    }
}

/// The Landlock ruleset a parsed shim invocation installs — pure, so the
/// exact policy is unit-testable without restricting the test process.
///
/// Two mutually exclusive shapes of network restriction:
/// `net_restricted` denies every TCP bind/connect (belt and braces over
/// an already-empty namespace); `net_gate_port` denies every TCP
/// bind/connect except that one port, so even a future change that
/// accidentally gave this namespace a route still can't open a socket
/// to anywhere but the gate.
pub fn ruleset_spec(args: &ShimArgs) -> RulesetSpec {
    let gate_port = args.net_gate_port.filter(|_| args.net_gate.is_some());
    let net = match gate_port {
        Some(port) => NetRestriction::ConnectOnly(port),
        None if args.net_restricted => NetRestriction::DenyAll,
        None => NetRestriction::Unrestricted,
    };
    let mut spec = RulesetSpec::application(access_plan(&args.ro, &args.rw, TARGET_ABI), net);
    if args.landlock_best_effort {
        // A kernel without Landlock, or with an older ABI, then reports not
        // or partially enforced instead of failing to build the ruleset.
        spec.compat = landlock::CompatLevel::BestEffort;
    }
    spec
}

/// Build + `restrict_self()`. The one function that permanently locks
/// the calling process — never call it from unit tests.
fn build_and_restrict(args: &ShimArgs) -> Result<RestrictionStatus, LandlockBuildError> {
    sandbox_core::landlock::restrict_self(&ruleset_spec(args), |path, e| {
        eprintln!(
            "tilo-sandbox-exec: skipping rule for {} (open failed: {e})",
            path.display()
        );
    })
}

/// Whether the launch may proceed with `outcome`. A ruleset that failed to
/// build refuses even on a provider-sandboxed machine.
fn landlock_accepted(outcome: LandlockOutcome, best_effort: bool) -> bool {
    match outcome {
        LandlockOutcome::FullyEnforced => true,
        LandlockOutcome::NotEnforced | LandlockOutcome::PartiallyEnforced => best_effort,
        LandlockOutcome::Errored => false,
    }
}

pub fn apply(args: &ShimArgs) -> LandlockOutcome {
    if args.no_landlock {
        // Not "apply an empty policy" — an empty ruleset denies everything.
        // The shim is in this argv only to host the egress relay.
        return LandlockOutcome::NotEnforced;
    }
    match build_and_restrict(args) {
        Ok(status) => {
            eprintln!("tilo-sandbox-exec: Landlock restriction status: {status:?}");
            outcome_from_status(&status.ruleset)
        }
        Err(e) => {
            eprintln!("tilo-sandbox-exec: failed to build/apply ruleset: {e}");
            LandlockOutcome::Errored
        }
    }
}

/// Close every inherited descriptor except stdin/out/err and the explicitly
/// connected service descriptors. `close_range` is part of the Tilo kernel
/// contract; inability to close ambient descriptors refuses the launch.
fn close_ambient_fds(keep: &[i32]) -> std::io::Result<()> {
    for (first, last) in ambient_fd_close_ranges(keep)? {
        close_fd_range(first, last)?;
    }
    Ok(())
}

fn ambient_fd_close_ranges(keep: &[i32]) -> std::io::Result<Vec<(u32, u32)>> {
    let mut keep = keep.to_vec();
    keep.sort_unstable();
    keep.dedup();
    let mut ranges = Vec::with_capacity(keep.len().saturating_add(1));
    let mut first = 3_u32;
    for fd in keep {
        let fd = u32::try_from(fd).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "negative kept fd")
        })?;
        if fd > first {
            let last = fd.checked_sub(1).ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "kept fd underflowed its close range",
                )
            })?;
            ranges.push((first, last));
        }
        first = fd.saturating_add(1);
    }
    ranges.push((first, u32::MAX));
    Ok(ranges)
}

fn close_fd_range(first: u32, last: u32) -> std::io::Result<()> {
    if first > last {
        return Ok(());
    }
    // SAFETY: close_range consumes no pointers. It only closes descriptors in
    // the inclusive numeric interval and returns an errno on failure.
    let rc = unsafe { libc::syscall(libc::SYS_close_range, first, last, 0_u32) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Start the in-sandbox half of the egress gate, in a child process.
///
/// Forked *before* `restrict_self`, so the relay isn't subject to the
/// app's Landlock policy — it needs to open the gate socket and bind a
/// loopback port, both denied to the app.
///
/// Costs nothing that the relay is unrestricted: it lives in the same
/// namespaces as the app, so anything it can do the app could do by
/// talking to the gate socket directly — a convenience for unmodified
/// HTTP clients, not a privilege boundary (that's the far end of the
/// unix socket, in the launcher).
///
/// The child dies with the sandbox: `--unshare-all` includes a PID
/// namespace, so when the app (PID 1) exits the kernel tears the
/// namespace down and takes the relay with it.
#[allow(
    clippy::exit,
    reason = "the forked child (pid == 0 branch) must terminate itself \
              directly — it must never fall through to Rust cleanup/atexit \
              paths meant for the parent, nor return into the parent's \
              control flow"
)]
fn spawn_relay(gate: &std::path::Path, port: u16) {
    // SAFETY: `fork` in a single-threaded process. The shim has not
    // started any threads at this point — it is straight-line argv
    // parsing — so the child inherits a consistent address space.
    let pid = unsafe { libc::fork() };
    match pid {
        -1 => {
            let err = std::io::Error::last_os_error();
            eprintln!("tilo-sandbox-exec: could not fork the egress relay: {err}");
        }
        0 => {
            // Child: serve until the sandbox goes away.
            if let Err(e) = crate::netgate::run_relay(gate, port) {
                eprintln!("tilo-sandbox-exec: egress relay stopped: {e}");
            }
            std::process::exit(0);
        }
        _ => {
            eprintln!(
                "tilo-sandbox-exec: egress relay on 127.0.0.1:{port} → {}",
                gate.display()
            );
        }
    }
}

/// Entry point for `tilo-launcher __sandbox-exec ...`. Diverges:
/// either `execvp`s the real command or exits non-zero.
///
/// `args` is the slice AFTER the subcommand token. Never returns: diverges
/// by `exec`ing the real command or calling [`std::process::exit`] on a
/// parse failure or `execvp` failure.
#[allow(
    clippy::exit,
    reason = "this function's contract is to diverge: either exec the real \
              command or terminate the process. There is no caller to \
              return an error to"
)]
pub fn run_sandbox_exec(args: &[String]) -> ! {
    let parsed = match ShimArgs::parse(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("tilo-sandbox-exec: {e}");
            std::process::exit(2);
        }
    };

    // Before Landlock: the relay needs accesses the app is about to be
    // denied, and it must exist before the app's first connect.
    if let (Some(gate), Some(port)) = (parsed.net_gate.as_deref(), parsed.net_gate_port) {
        spawn_relay(gate, port);
    }

    let outcome = apply(&parsed);
    eprintln!(
        "tilo-sandbox-exec: {} ({} ro, {} rw, net_restricted={}, gate_port={:?})",
        outcome.describe(),
        parsed.ro.len(),
        parsed.rw.len(),
        parsed.net_restricted,
        parsed.net_gate_port,
    );

    if !parsed.no_landlock && !landlock_accepted(outcome, parsed.landlock_best_effort) {
        eprintln!(
            "tilo-sandbox-exec: refusing tapp launch because the complete Landlock v5 policy is not enforced"
        );
        std::process::exit(126);
    }
    if !parsed.no_landlock && outcome != LandlockOutcome::FullyEnforced {
        eprintln!(
            "tilo-sandbox-exec: provider-sandboxed machine: running a system app with {}",
            outcome.describe()
        );
    }

    if let Err(error) = close_ambient_fds(&parsed.keep_fds) {
        eprintln!(
            "tilo-sandbox-exec: refusing tapp launch: could not close ambient descriptors: {error}"
        );
        std::process::exit(126);
    }

    exec_real_command(&parsed.command)
}

#[allow(
    clippy::exit,
    reason = "this function's contract is to diverge: either exec the real \
              command or terminate the process. There is no caller to \
              return an error to"
)]
fn exec_real_command(command: &[String]) -> ! {
    let Some(program) = command.first() else {
        eprintln!("tilo-sandbox-exec: empty command");
        std::process::exit(2);
    };

    let Ok(cprogram) = CString::new(program.as_bytes()) else {
        eprintln!("tilo-sandbox-exec: program path contains an interior NUL");
        std::process::exit(2);
    };

    let cargs: Vec<CString> = if let Ok(v) = command
        .iter()
        .map(|a| CString::new(a.as_bytes()))
        .collect::<Result<_, _>>()
    {
        v
    } else {
        eprintln!("tilo-sandbox-exec: argument contains an interior NUL");
        std::process::exit(2);
    };

    let argv_ptrs: Vec<*const libc::c_char> = cargs
        .iter()
        .map(|c| c.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();

    // SAFETY: cprogram + argv_ptrs are valid null-terminated strings; argv_ptrs
    // is null-terminated. execvp either replaces the image or returns -1.
    unsafe {
        libc::execvp(cprogram.as_ptr(), argv_ptrs.as_ptr());
    }
    let err = std::io::Error::last_os_error();
    eprintln!("tilo-sandbox-exec: execvp {program:?} failed: {err}");
    std::process::exit(2);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::landlock::FS_ABI;
    use landlock::CompatLevel;

    #[test]
    fn shim_command_line_parses_strictly() {
        // Phase `parses_minimal_command`.
        {
            let args = vec!["--".into(), "/bin/echo".into(), "hi".into()];
            let parsed = ShimArgs::parse(&args).expect("parse");
            assert!(parsed.ro.is_empty());
            assert!(parsed.rw.is_empty());
            assert!(!parsed.net_restricted);
            assert_eq!(parsed.command, vec!["/bin/echo".to_string(), "hi".into()]);
        }
        // Phase `parses_rules_and_flags`.
        {
            let args = vec![
                "--ro".into(),
                "/usr".into(),
                "--rw".into(),
                "/tmp/data".into(),
                "--net-restricted".into(),
                "--keep-fd".into(),
                "17".into(),
                "--".into(),
                "/app".into(),
            ];
            let parsed = ShimArgs::parse(&args).expect("parse");
            assert_eq!(parsed.ro, vec![PathBuf::from("/usr")]);
            assert_eq!(parsed.rw, vec![PathBuf::from("/tmp/data")]);
            assert!(parsed.net_restricted);
            assert_eq!(parsed.keep_fds, vec![17]);
            assert_eq!(parsed.command, vec!["/app".to_string()]);
        }
        // Phase `missing_separator_is_error`.
        {
            let args = vec!["--ro".into(), "/usr".into()];
            assert_eq!(
                ShimArgs::parse(&args),
                Err(ShimParseError::MissingSeparator)
            );
        }
        // Phase `dangling_flag_is_error`.
        {
            let args = vec!["--ro".into()];
            assert!(matches!(
                ShimArgs::parse(&args),
                Err(ShimParseError::DanglingValue(_))
            ));
        }
        // Phase `unknown_policy_flag_is_error_instead_of_a_silent_downgrade`.
        {
            let args = vec!["--future-restriction".into(), "--".into(), "/app".into()];
            assert_eq!(
                ShimArgs::parse(&args),
                Err(ShimParseError::UnknownFlag("--future-restriction".into()))
            );
        }
        // Phase `invalid_or_standard_keep_fd_is_rejected`.
        {
            for value in ["no", "-1", "2", "1025"] {
                let args = vec!["--keep-fd".into(), value.into(), "--".into(), "/app".into()];
                assert!(matches!(
                    ShimArgs::parse(&args),
                    Err(ShimParseError::DanglingValue(_))
                ));
            }
        }
        // Phase `empty_command_is_error`.
        {
            let args = vec!["--".into()];
            assert_eq!(ShimArgs::parse(&args), Err(ShimParseError::EmptyCommand));
        }
    }

    #[test]
    fn shim_fd_and_access_plans_are_pinned() {
        // Phase `ambient_fd_plan_preserves_only_named_brokers`.
        {
            assert_eq!(
                ambient_fd_close_ranges(&[18, 17, 18]).expect("plan"),
                vec![(3, 16), (19, u32::MAX)]
            );
            assert_eq!(
                ambient_fd_close_ranges(&[]).expect("plan"),
                vec![(3, u32::MAX)]
            );
        }
        // Phase `access_plan_ro_and_rw`.
        {
            let plan = access_plan(
                &[PathBuf::from("/usr")],
                &[PathBuf::from("/tmp/data")],
                FS_ABI,
            );
            assert_eq!(plan.len(), 2);
            assert_eq!(plan[0].path, PathBuf::from("/usr"));
            assert_eq!(plan[1].path, PathBuf::from("/tmp/data"));
            assert_ne!(plan[0].access, plan[1].access); // rw has strictly more rights
        }
        // Phase `pinned_shim_rule_list`.
        // The shim's rule list as raw access bits (see
        // `landlock::tests::pinned_ruleset_access_sets` for the bit names):
        // read-only paths get Execute|ReadFile|ReadDir, read-write paths get
        // every ABI-5 right, in argv order with read-only first.
        {
            let plan = access_plan(
                &[PathBuf::from("/usr"), PathBuf::from("/lib")],
                &[PathBuf::from("/app"), PathBuf::from("/dev/dri")],
                TARGET_ABI,
            );
            let pinned: Vec<(PathBuf, u64)> = plan
                .iter()
                .map(|rule| (rule.path.clone(), rule.access.bits()))
                .collect();
            assert_eq!(
                pinned,
                vec![
                    (PathBuf::from("/usr"), 0b1101),
                    (PathBuf::from("/lib"), 0b1101),
                    (PathBuf::from("/app"), 0xffff),
                    (PathBuf::from("/dev/dri"), 0xffff),
                ]
            );
        }
        // Phase `pinned_shim_rulesets`.
        // Each network shape of the shim argv, and the ruleset it installs.
        {
            let base = ShimArgs::parse(&[
                "--ro".into(),
                "/usr".into(),
                "--rw".into(),
                "/app".into(),
                "--".into(),
                "/app/bin".into(),
            ])
            .expect("parse");
            let rules = access_plan(&base.ro, &base.rw, TARGET_ABI);

            let open = ruleset_spec(&base);
            assert_eq!(open.compat, CompatLevel::HardRequirement);
            assert_eq!(open.handled_fs.bits(), 0xffff);
            assert!(open.handled_net.is_empty());
            assert!(open.connect_ports.is_empty());
            assert!(open.trim_file_rights);
            assert_eq!(open.rules, rules);

            let restricted = ruleset_spec(&ShimArgs {
                net_restricted: true,
                ..base.clone()
            });
            assert_eq!(restricted.handled_net.bits(), 0b11);
            assert!(restricted.connect_ports.is_empty());
            assert_eq!(restricted.rules, rules);

            let gated = ruleset_spec(&ShimArgs {
                net_gate: Some(PathBuf::from("/run/gate.sock")),
                net_gate_port: Some(8118),
                ..base.clone()
            });
            assert_eq!(gated.handled_net.bits(), 0b11);
            assert_eq!(gated.connect_ports, vec![8118]);

            // Restricted and gated together: still only the gate port.
            let both = ruleset_spec(&ShimArgs {
                net_restricted: true,
                net_gate: Some(PathBuf::from("/run/gate.sock")),
                net_gate_port: Some(8118),
                ..base.clone()
            });
            assert_eq!(both, gated);

            // A port without a gate socket opens nothing.
            let portless = ruleset_spec(&ShimArgs {
                net_gate_port: Some(8118),
                ..base
            });
            assert!(portless.handled_net.is_empty());
            assert!(portless.connect_ports.is_empty());
        }
    }

    #[test]
    fn only_the_internal_relay_mode_runs_without_landlock() {
        // Phase `ordinary_shim_invocations_require_landlock`.
        {
            let parsed = ShimArgs::parse(&["--".into(), "/app".into()]).expect("parse");
            assert!(!parsed.no_landlock);
        }
        // Phase `only_the_explicit_internal_relay_mode_skips_landlock`.
        {
            let parsed = ShimArgs::parse(&["--no-landlock".into(), "--".into(), "/app".into()])
                .expect("parse");
            assert!(parsed.no_landlock);
        }
    }
}

#[cfg(test)]
mod provider_sandbox_tests {
    use super::*;

    #[test]
    fn best_effort_accepts_a_missing_or_partial_landlock_but_never_an_error() {
        use LandlockOutcome::{Errored, FullyEnforced, NotEnforced, PartiallyEnforced};
        for (outcome, strict, best_effort) in [
            (FullyEnforced, true, true),
            (NotEnforced, false, true),
            (PartiallyEnforced, false, true),
            (Errored, false, false),
        ] {
            assert_eq!(
                landlock_accepted(outcome, false),
                strict,
                "{outcome:?} strict"
            );
            assert_eq!(
                landlock_accepted(outcome, true),
                best_effort,
                "{outcome:?} best effort"
            );
        }
    }

    #[test]
    fn best_effort_builds_the_ruleset_best_effort() {
        let strict = ShimArgs::parse(&["--".into(), "/app".into()]).unwrap();
        assert_eq!(
            ruleset_spec(&strict).compat,
            landlock::CompatLevel::HardRequirement
        );
        let lenient =
            ShimArgs::parse(&["--landlock-best-effort".into(), "--".into(), "/app".into()])
                .unwrap();
        assert_eq!(
            ruleset_spec(&lenient).compat,
            landlock::CompatLevel::BestEffort
        );
    }

    #[test]
    fn the_best_effort_flag_is_parsed() {
        let parsed =
            ShimArgs::parse(&["--landlock-best-effort".into(), "--".into(), "/app".into()])
                .unwrap();
        assert!(parsed.landlock_best_effort);
        assert!(!parsed.no_landlock);
        let parsed = ShimArgs::parse(&["--".into(), "/app".into()]).unwrap();
        assert!(!parsed.landlock_best_effort);
    }
}
