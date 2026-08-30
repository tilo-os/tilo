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
//! `restrict_self()`, then `execvp`s the real app.
//!
//! Landlock is strictly additive: on any error (kernel too old, ruleset
//! construction failed) it logs and still execs the app — bwrap +
//! seccomp + netns remain in force. Never fail a launch on Landlock alone.

use std::ffi::CString;
use std::path::PathBuf;

use landlock::{
    ABI, Access, AccessFs, AccessNet, NetPort, PathBeneath, PathFd, RestrictionStatus, Ruleset,
    RulesetAttr, RulesetCreatedAttr, RulesetError, RulesetStatus,
};

use crate::landlock::{FsRule, LandlockOutcome, TARGET_ABI};

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
    /// The real app argv to `execvp` after restricting.
    pub command: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ShimParseError {
    DanglingValue(String),
    MissingSeparator,
    EmptyCommand,
}

impl std::fmt::Display for ShimParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShimParseError::DanglingValue(flag) => {
                write!(f, "flag `{flag}` expects a path argument")
            }
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
    /// `[--ro <p>]* [--rw <p>]* [--net-restricted] [--no-landlock]
    ///  [--net-gate <sock> --net-gate-port <n>] -- <cmd> [args]`.
    /// Unknown flags before `--` are silently skipped (forward-compat).
    pub fn parse(args: &[String]) -> Result<ShimArgs, ShimParseError> {
        let mut parsed = ShimArgs::default();
        let mut i = 0;
        let mut saw_separator = false;

        while i < args.len() {
            let arg = &args[i];
            match arg.as_str() {
                "--" => {
                    saw_separator = true;
                    parsed.command.extend_from_slice(&args[i + 1..]);
                    break;
                }
                "--net-restricted" => parsed.net_restricted = true,
                "--no-landlock" => parsed.no_landlock = true,
                "--ro" | "--rw" | "--net-gate" | "--net-gate-port" => {
                    let value = args
                        .get(i + 1)
                        .ok_or_else(|| ShimParseError::DanglingValue(arg.clone()))?;
                    match arg.as_str() {
                        "--ro" => parsed.ro.push(PathBuf::from(value)),
                        "--rw" => parsed.rw.push(PathBuf::from(value)),
                        "--net-gate" => parsed.net_gate = Some(PathBuf::from(value)),
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
                    i += 1;
                }
                _ => {}
            }
            i += 1;
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

/// Map ro/rw to FS rules — pure, unit-testable.
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

pub(crate) fn outcome_from_status(status: RulesetStatus) -> LandlockOutcome {
    match status {
        RulesetStatus::FullyEnforced => LandlockOutcome::FullyEnforced,
        RulesetStatus::PartiallyEnforced => LandlockOutcome::PartiallyEnforced,
        RulesetStatus::NotEnforced => LandlockOutcome::NotEnforced,
    }
}

/// Build + `restrict_self()`. The one function that permanently locks
/// the calling process — never call it from unit tests.
fn build_and_restrict(args: &ShimArgs) -> Result<RestrictionStatus, RulesetError> {
    let abi = TARGET_ABI;

    let mut ruleset = Ruleset::default().handle_access(AccessFs::from_all(abi))?;
    // Two mutually exclusive shapes of network restriction:
    // `net_restricted` denies every TCP bind/connect (belt and braces over
    // an already-empty namespace); `net_gate_port` denies every TCP
    // bind/connect except that one port, so even a future change that
    // accidentally gave this namespace a route still can't open a socket
    // to anywhere but the gate.
    let gate_port = args.net_gate_port.filter(|_| args.net_gate.is_some());
    if args.net_restricted || gate_port.is_some() {
        ruleset = ruleset.handle_access(AccessNet::from_all(abi))?;
    }

    let mut created = ruleset.create()?;

    if let Some(port) = gate_port {
        created = created.add_rule(NetPort::new(port, AccessNet::ConnectTcp))?;
    }

    for rule in access_plan(&args.ro, &args.rw, abi) {
        match PathFd::new(&rule.path) {
            Ok(fd) => {
                created = created.add_rule(PathBeneath::new(fd, rule.access))?;
            }
            Err(e) => {
                eprintln!(
                    "qos-sandbox-exec: skipping rule for {} (open failed: {e})",
                    rule.path.display()
                );
            }
        }
    }

    created.restrict_self()
}

pub fn apply(args: &ShimArgs) -> LandlockOutcome {
    if args.no_landlock {
        // Not "apply an empty policy" — an empty ruleset denies everything.
        // The shim is in this argv only to host the egress relay.
        return LandlockOutcome::NotEnforced;
    }
    match build_and_restrict(args) {
        Ok(status) => outcome_from_status(status.ruleset),
        Err(e) => {
            eprintln!("qos-sandbox-exec: failed to build/apply ruleset: {e}");
            LandlockOutcome::Errored
        }
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
fn spawn_relay(gate: &std::path::Path, port: u16) {
    // SAFETY: `fork` in a single-threaded process. The shim has not
    // started any threads at this point — it is straight-line argv
    // parsing — so the child inherits a consistent address space.
    let pid = unsafe { libc::fork() };
    match pid {
        -1 => {
            let err = std::io::Error::last_os_error();
            eprintln!("qos-sandbox-exec: could not fork the egress relay: {err}");
        }
        0 => {
            // Child: serve until the sandbox goes away.
            if let Err(e) = crate::netgate::run_relay(gate, port) {
                eprintln!("qos-sandbox-exec: egress relay stopped: {e}");
            }
            std::process::exit(0);
        }
        _ => {
            eprintln!(
                "qos-sandbox-exec: egress relay on 127.0.0.1:{port} → {}",
                gate.display()
            );
        }
    }
}

/// Entry point for `qos-launcher __sandbox-exec ...`. Diverges:
/// either `execvp`s the real command or exits non-zero.
///
/// `args` is the slice AFTER the subcommand token.
pub fn run_sandbox_exec(args: &[String]) -> ! {
    let parsed = match ShimArgs::parse(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("qos-sandbox-exec: {e}");
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
        "qos-sandbox-exec: {} ({} ro, {} rw, net_restricted={}, gate_port={:?})",
        outcome.describe(),
        parsed.ro.len(),
        parsed.rw.len(),
        parsed.net_restricted,
        parsed.net_gate_port,
    );

    exec_real_command(&parsed.command)
}

fn exec_real_command(command: &[String]) -> ! {
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

    let cargv: Vec<*const libc::c_char> = cargs
        .iter()
        .map(|c| c.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();

    // SAFETY: cprogram + cargv are valid null-terminated strings; cargv
    // is null-terminated. execvp either replaces the image or returns -1.
    unsafe {
        libc::execvp(cprogram.as_ptr(), cargv.as_ptr());
    }
    let err = std::io::Error::last_os_error();
    eprintln!("qos-sandbox-exec: execvp {program:?} failed: {err}");
    std::process::exit(2);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_command() {
        let args = vec!["--".into(), "/bin/echo".into(), "hi".into()];
        let parsed = ShimArgs::parse(&args).expect("parse");
        assert!(parsed.ro.is_empty());
        assert!(parsed.rw.is_empty());
        assert!(!parsed.net_restricted);
        assert_eq!(parsed.command, vec!["/bin/echo".to_string(), "hi".into()]);
    }

    #[test]
    fn parses_rules_and_flags() {
        let args = vec![
            "--ro".into(),
            "/usr".into(),
            "--rw".into(),
            "/tmp/data".into(),
            "--net-restricted".into(),
            "--".into(),
            "/app".into(),
        ];
        let parsed = ShimArgs::parse(&args).expect("parse");
        assert_eq!(parsed.ro, vec![PathBuf::from("/usr")]);
        assert_eq!(parsed.rw, vec![PathBuf::from("/tmp/data")]);
        assert!(parsed.net_restricted);
        assert_eq!(parsed.command, vec!["/app".to_string()]);
    }

    #[test]
    fn missing_separator_is_error() {
        let args = vec!["--ro".into(), "/usr".into()];
        assert_eq!(
            ShimArgs::parse(&args),
            Err(ShimParseError::MissingSeparator)
        );
    }

    #[test]
    fn dangling_flag_is_error() {
        let args = vec!["--ro".into()];
        assert!(matches!(
            ShimArgs::parse(&args),
            Err(ShimParseError::DanglingValue(_))
        ));
    }

    #[test]
    fn empty_command_is_error() {
        let args = vec!["--".into()];
        assert_eq!(ShimArgs::parse(&args), Err(ShimParseError::EmptyCommand));
    }

    #[test]
    fn access_plan_ro_and_rw() {
        let plan = access_plan(
            &[PathBuf::from("/usr")],
            &[PathBuf::from("/tmp/data")],
            TARGET_ABI,
        );
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].path, PathBuf::from("/usr"));
        assert_eq!(plan[1].path, PathBuf::from("/tmp/data"));
        assert_ne!(plan[0].access, plan[1].access); // rw has strictly more rights
    }
}
