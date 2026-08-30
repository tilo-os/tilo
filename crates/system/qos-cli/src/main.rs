//! `qos` — what a person types to ask this machine about the image it booted.
//!
//! ```text
//! qos status               what is booted, what is staged, what it can go back to
//! qos rollback             boot the previous deployment from now on
//! qos update               fetch and stage the current image
//! qos repair               explain how to reach repair mode
//! ```
//!
//! ## Why this is thin
//!
//! The work lives in `/usr/libexec/qos-deployment`, a few renames on the ESP and one EFI
//! variable write. This is the part that decides *how to become root to run it*, which has
//! three answers and no good way to guess between them:
//!
//! * already root — run it directly
//! * a desktop session — `systemctl start qos-deployment-rollback.service`, which polkit
//!   authorises for the local active user without a password
//!   (`os/etc/polkit-1/rules.d/49-qos-updates.rules`)
//! * anything else — say to use sudo, rather than failing with a permission error from three
//!   layers down
//!
//! Settings' rollback control takes the second of those, deliberately: one privileged
//! implementation, reached the same way from the button and the command line, so the two
//! cannot drift.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "a test that unwraps is stating an invariant"
    )
)]

use std::process::{Command, ExitCode, Stdio};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

/// Where the privileged half lives. Overridable so the CLI can be exercised
/// against a stub in a test without a machine to break.
fn helper() -> String {
    std::env::var("QOS_DEPLOYMENT_HELPER")
        .unwrap_or_else(|_| "/usr/libexec/qos-deployment".to_string())
}

#[derive(Parser)]
#[command(
    name = "qos",
    about = "Ask this machine about the image it booted, and change it",
    long_about = "Recovery and update commands for a qOS machine.\n\n\
                  What a rollback does and does not undo is in \
                  /usr/share/doc/qos/recovery.md, and every command that changes \
                  the next boot prints it before doing so."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// What is booted, what is staged, and what this machine can go back to.
    Status {
        /// bootc's own JSON, unmodified.
        #[arg(long)]
        json: bool,
    },
    /// Boot the previous deployment from now on.
    Rollback {
        /// Do not ask for confirmation.
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Fetch the image this machine tracks and stage it for the next boot.
    Update {
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Boot one specific deployment once, then go back to normal.
    Boot {
        /// The loader entry id, as `qos status` lists it.
        entry: String,
    },
    /// How to reach repair mode, and what it can do.
    Repair,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("qos: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    match cli.cmd {
        // Read-only, so it never needs to become anything. A person diagnosing a machine should not
        // have to authenticate to be told what is on it.
        Cmd::Status { json } => {
            let mut args = vec!["list"];
            if json {
                args.push("--json");
            }
            run_helper_directly(&args)
        }
        Cmd::Rollback { yes } => privileged(
            &["rollback"],
            yes,
            "qos-deployment-rollback.service",
            "Roll back to the previous deployment",
        ),
        Cmd::Update { yes } => privileged(
            &["update"],
            yes,
            "qos-deployment-update.service",
            "Fetch and stage the current image",
        ),
        Cmd::Boot { entry } => {
            // No unit for this one: it names an entry, and a fixed unit cannot carry an argument
            // without becoming a way to run anything.
            if !is_root() {
                bail!(
                    "booting a specific deployment once needs root:\n  \
                     sudo qos boot {entry}"
                );
            }
            run_helper_directly(&["boot", &entry])
        }
        Cmd::Repair => {
            print_repair_help();
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn is_root() -> bool {
    // No libc dependency for one number: /proc/self/status is where it is anyway, and this
    // only runs on Linux.
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("Uid:"))
                .and_then(|v| v.split_whitespace().next().map(str::to_string))
        })
        .is_some_and(|uid| uid == "0")
}

fn run_helper_directly(args: &[&str]) -> Result<ExitCode> {
    let helper = helper();
    let status = Command::new(&helper)
        .args(args)
        .stdin(Stdio::inherit())
        .status()
        .with_context(|| format!("running {helper}"))?;
    Ok(exit_code(status.code()))
}

/// Run a privileged verb by whichever route this session can actually take.
fn privileged(args: &[&str], yes: bool, unit: &str, what: &str) -> Result<ExitCode> {
    if is_root() {
        let mut a = args.to_vec();
        if yes {
            a.push("--yes");
        }
        return run_helper_directly(&a);
    }

    // Not root. The caveats belong in front of the confirmation, not behind it, so print them
    // here rather than letting the unit print them into a journal nobody is reading.
    let _ = Command::new(helper()).arg("caveats").status();
    if !yes {
        eprint!("\n{what}? [y/N] ");
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .context("reading the confirmation")?;
        if !matches!(line.trim(), "y" | "Y" | "yes" | "YES") {
            eprintln!("cancelled");
            return Ok(ExitCode::FAILURE);
        }
    }

    // polkit authorises exactly this unit for a local active session in wheel; if it doesn't,
    // systemctl says so far more usefully than a guess would.
    let status = Command::new("systemctl")
        .args(["start", "--wait", unit])
        .status()
        .context("running systemctl start")?;
    if !status.success() {
        eprintln!(
            "\nqos: starting {unit} failed. Either this session is not the one at \
             the machine's own seat, or polkit did not authorise it. From a \
             terminal:\n  sudo {} {}",
            helper(),
            args.join(" ")
        );
        // The unit's own output is the useful part; point at it rather than reprinting a guess.
        let _ = Command::new("journalctl")
            .args(["-u", unit, "-n", "20", "--no-pager"])
            .status();
        return Ok(exit_code(status.code()));
    }
    let _ = Command::new("journalctl")
        .args(["-u", unit, "-n", "40", "--no-pager", "-o", "cat"])
        .status();
    Ok(ExitCode::SUCCESS)
}

fn exit_code(code: Option<i32>) -> ExitCode {
    match code {
        Some(0) => ExitCode::SUCCESS,
        Some(c) => ExitCode::from(u8::try_from(c).unwrap_or(1)),
        None => ExitCode::FAILURE,
    }
}

fn print_repair_help() {
    println!(
        "\
Repair Mode is a boot menu entry, not a command.

  Reboot, and at the boot menu choose \"qOS Repair Mode\".

It brings this machine up without the desktop — no compositor, no greeter, no
collaborative filesystem, no updates, no sshd — and offers: roll back, boot a
specific deployment once, re-fetch the image, make /var writable, read the
previous boot's log, and a root shell.

On an encrypted machine it asks for a passphrase enrolled on the disk (the
recovery key will do) before any of that. It is also where a machine ends up by
itself when three boots in a row fail to reach a login and there is no older
deployment to fall back to.

  /usr/share/doc/qos/recovery.md     what a rollback does and does not undo
  /usr/share/doc/qos/repair-mode.md  how the entry is built, and its threat model"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// clap's own consistency check: duplicate names, bad arg definitions, a subcommand with no
    /// help. Fails the build rather than a user's first attempt to use it.
    #[test]
    fn the_command_tree_is_well_formed() {
        Cli::command().debug_assert();
    }

    /// `status` must not require root. A person diagnosing a machine that won't boot properly
    /// shouldn't have to authenticate to be told what's on it, which requiring it would push
    /// them towards `sudo` for a read.
    #[test]
    fn status_takes_the_unprivileged_path() {
        let cli = Cli::try_parse_from(["qos", "status"]).expect("parses");
        assert!(
            matches!(cli.cmd, Cmd::Status { json: false }),
            "status must parse without flags"
        );
    }

    /// The helper path is a single named constant, not a string repeated at each call site — the
    /// failure mode being one verb pointing at a path the image does not have.
    #[test]
    fn the_helper_path_is_overridable_and_defaults_to_libexec() {
        // Not asserting on a process-global env var here (tests run in parallel); assert the
        // default, which is the part that ships.
        assert!(
            helper().ends_with("qos-deployment"),
            "the helper must be qos-deployment, got {}",
            helper()
        );
    }
}
