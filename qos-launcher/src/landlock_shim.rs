//! In-sandbox Landlock enforcement — the hidden `__sandbox-exec`
//! subcommand.
//!
//! ## Why a shim
//!
//! `bwrap` needs broad filesystem access to *build* the sandbox
//! (mount, bind, pivot_root). We can't Landlock the bwrap process.
//! But Landlock is path-based and irrevocable for the calling process
//! and all its descendants — so it must be applied *inside* the
//! namespace, after the mount setup finishes, before the app binary
//! takes over.
//!
//! We achieve that by having bwrap `exec` this launcher again (via a
//! bind of its own binary), with a hidden `__sandbox-exec` first
//! argument. The re-entered launcher parses the argv following the
//! token, builds a Landlock ruleset against the *sandbox-side* paths,
//! calls `restrict_self()`, and then `execvp`s the real app.
//!
//! Landlock is strictly additive: on any error (kernel too old, ruleset
//! construction failed) we log and still exec the app — bwrap + seccomp
//! + netns remain in force. The mantra matches the compositor shim:
//! never fail a launch on Landlock alone.

use std::ffi::CString;
use std::path::PathBuf;

use landlock::{
    ABI, Access, AccessFs, AccessNet, PathBeneath, PathFd, RestrictionStatus, Ruleset,
    RulesetAttr, RulesetCreatedAttr, RulesetError, RulesetStatus,
};

use crate::landlock::{FsRule, LandlockOutcome, TARGET_ABI};

/// Hidden subcommand token. Dispatched by `main.rs` before clap.
pub const SANDBOX_EXEC_SUBCOMMAND: &str = "__sandbox-exec";

/// Parsed form of the shim argv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShimArgs {
    /// Read-only paths (read + exec) — sandbox-side.
    pub ro: Vec<PathBuf>,
    /// Read-write paths (full FS access) — sandbox-side.
    pub rw: Vec<PathBuf>,
    /// Deny all TCP bind/connect (ABI ≥ 4).
    pub net_restricted: bool,
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
            ShimParseError::DanglingValue(flag) => write!(f, "flag `{flag}` expects a path argument"),
            ShimParseError::MissingSeparator => write!(f, "missing `--` separator"),
            ShimParseError::EmptyCommand => write!(f, "no command after `--`"),
        }
    }
}

impl std::error::Error for ShimParseError {}

impl ShimArgs {
    /// Parse the slice AFTER the `__sandbox-exec` token.
    /// Grammar: `[--ro <p>]* [--rw <p>]* [--net-restricted] -- <cmd> [args]`.
    /// Unknown flags before `--` are silently skipped (forward-compat).
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
    if args.net_restricted {
        ruleset = ruleset.handle_access(AccessNet::from_all(abi))?;
    }

    let mut created = ruleset.create()?;

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
    match build_and_restrict(args) {
        Ok(status) => outcome_from_status(status.ruleset),
        Err(e) => {
            eprintln!("qos-sandbox-exec: failed to build/apply ruleset: {e}");
            LandlockOutcome::Errored
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

    let outcome = apply(&parsed);
    eprintln!(
        "qos-sandbox-exec: {} ({} ro, {} rw, net_restricted={})",
        outcome.describe(),
        parsed.ro.len(),
        parsed.rw.len(),
        parsed.net_restricted
    );

    exec_real_command(&parsed.command)
}

fn exec_real_command(command: &[String]) -> ! {
    let Some(program) = command.first() else {
        eprintln!("qos-sandbox-exec: empty command");
        std::process::exit(2);
    };

    let cprogram = match CString::new(program.as_bytes()) {
        Ok(c) => c,
        Err(_) => {
            eprintln!("qos-sandbox-exec: program path contains an interior NUL");
            std::process::exit(2);
        }
    };

    let cargs: Vec<CString> = match command
        .iter()
        .map(|a| CString::new(a.as_bytes()))
        .collect::<Result<_, _>>()
    {
        Ok(v) => v,
        Err(_) => {
            eprintln!("qos-sandbox-exec: argument contains an interior NUL");
            std::process::exit(2);
        }
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
        assert_eq!(ShimArgs::parse(&args), Err(ShimParseError::MissingSeparator));
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
