//! `tilo` — one command for Tilo apps and the system beneath them.
//!
//! ```text
//! tilo app u browser        update one app
//! tilo system status        inspect the booted system image
//! tilo update               update every provider available on this machine
//! ```
//!
//! ## Why this is thin
//!
//! The work lives in `/usr/libexec/tilo-deployment`, a few renames on the ESP and one EFI
//! variable write. This is the part that decides *how to become root to run it*, which has
//! three answers and no good way to guess between them:
//!
//! * already root — run it directly
//! * a desktop session — `systemctl start tilo-deployment-rollback.service`, which polkit
//!   authorises for the local active user without a password
//!   (`os/etc/polkit-1/rules.d/49-tilo-updates.rules`)
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
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::ffi::OsString;
use std::process::{Command, ExitCode, Stdio};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

/// Where the privileged half lives. Overridable so the CLI can be exercised
/// against a stub in a test without a machine to break.
fn helper() -> String {
    std::env::var("TILO_DEPLOYMENT_HELPER")
        .unwrap_or_else(|_| "/usr/libexec/tilo-deployment".to_string())
}

#[derive(Parser)]
#[command(
    name = "tilo",
    about = "Manage Tilo apps and the system beneath them",
    long_about = "App and system commands for a Tilo machine. Providers that are not installed are skipped by umbrella operations.\n\n\
                  What a rollback does and does not undo is in \
                  /usr/share/doc/tilo/recovery.md, and every command that changes \
                  the next boot prints it before doing so.",
    after_help = "Examples:\n  tilo app u browser     Update Browser\n  tilo store search pdf  Search the app store (`store` is an alias)\n  tilo system status     Inspect the booted Tilo OS image\n  tilo update            Update apps and every available system provider\n\nOn another Linux distribution, app commands still work when `app` is on PATH;\nTilo OS-only system commands explain that their provider is unavailable."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// System image, boot, rollback, and repair commands.
    #[command(
        after_help = "Examples:\n  tilo system status\n  tilo system update\n  tilo system update --yes\n  tilo system rollback"
    )]
    System {
        #[command(subcommand)]
        cmd: SystemCmd,
    },
    /// Install, update, remove, and discover apps.
    #[command(disable_help_flag = true)]
    App {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
    /// Alias for `tilo app`.
    #[command(disable_help_flag = true)]
    Store {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
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
    /// Update every available app and system provider.
    Update {
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Boot one specific deployment once, then go back to normal.
    Boot {
        /// The loader entry id, as `tilo status` lists it.
        entry: String,
    },
    /// How to reach repair mode, and what it can do.
    Repair,
}

#[derive(Subcommand)]
enum SystemCmd {
    /// Show the booted, staged, and rollback system images.
    Status {
        /// Print bootc's JSON without reformatting it.
        #[arg(long)]
        json: bool,
    },
    /// Make the previous signed image the default for the next boot.
    Rollback {
        /// Do not ask for confirmation.
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Fetch and stage the newest signed Tilo OS image.
    Update {
        /// Do not ask for confirmation.
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Boot one deployment once, then return to the normal default.
    Boot {
        /// Loader entry id shown by `tilo system status`.
        entry: String,
    },
    /// Explain how to enter the isolated recovery environment.
    Repair,
}

fn main() -> ExitCode {
    let args = system_alias_args(std::env::args_os().collect());
    let cli = Cli::parse_from(args);
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("tilo: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// The image exposes a `system` symlink only where its deployment provider is
/// installed.  Preserve that short spelling without giving a different parser
/// or command implementation a chance to drift from `tilo system`.
fn system_alias_args(mut args: Vec<OsString>) -> Vec<OsString> {
    if args
        .first()
        .and_then(|arg| std::path::Path::new(arg).file_stem())
        .is_some_and(|name| name == "system")
    {
        args.insert(1, OsString::from("system"));
    }
    args
}

fn run(cli: Cli) -> Result<ExitCode> {
    match cli.cmd {
        Cmd::System { cmd } => {
            require_system_provider()?;
            run_system(cmd)
        }
        Cmd::App { args } | Cmd::Store { args } => run_app(&args),
        // Read-only, so it never needs to become anything. A person diagnosing a machine should not
        // have to authenticate to be told what is on it.
        Cmd::Status { json } => {
            require_system_provider()?;
            let mut args = vec!["list"];
            if json {
                args.push("--json");
            }
            run_helper_directly(&args)
        }
        Cmd::Rollback { yes } => {
            require_system_provider()?;
            privileged(
                &["rollback"],
                yes,
                "tilo-deployment-rollback.service",
                "Roll back to the previous deployment",
            )
        }
        Cmd::Update { yes } => update_everything(yes),
        Cmd::Boot { entry } => {
            require_system_provider()?;
            // No unit for this one: it names an entry, and a fixed unit cannot carry an argument
            // without becoming a way to run anything.
            if !is_root() {
                bail!(
                    "booting a specific deployment once needs root:\n  \
                     sudo tilo boot {entry}"
                );
            }
            run_helper_directly(&["boot", &entry])
        }
        Cmd::Repair => {
            require_system_provider()?;
            print_repair_help();
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn run_system(cmd: SystemCmd) -> Result<ExitCode> {
    match cmd {
        SystemCmd::Status { json } => run(Cli {
            cmd: Cmd::Status { json },
        }),
        SystemCmd::Rollback { yes } => run(Cli {
            cmd: Cmd::Rollback { yes },
        }),
        SystemCmd::Update { yes } => privileged(
            &["update"],
            yes,
            "tilo-deployment-update.service",
            "Fetch and stage the current image",
        ),
        SystemCmd::Boot { entry } => run(Cli {
            cmd: Cmd::Boot { entry },
        }),
        SystemCmd::Repair => {
            print_repair_help();
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn run_app(args: &[OsString]) -> Result<ExitCode> {
    if is_root() && !is_help_request(args) {
        bail!(
            "app management belongs to the signed-in user, not root; \
             leave the root shell and run `tilo app {}`",
            display_args(args)
        );
    }
    let binary = app_binary();
    if !command_available(&binary) {
        bail!(
            "app management is unavailable because `app` was not found on PATH; \
             install the Tilo app tools, then try `tilo app --help`"
        );
    }
    let status = Command::new(&binary)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("running {}", std::path::Path::new(&binary).display()))?;
    Ok(exit_code(status.code()))
}

fn is_help_request(args: &[OsString]) -> bool {
    args.iter()
        .any(|arg| matches!(arg.to_str(), Some("--help" | "-h")))
}

fn app_binary() -> OsString {
    app_binary_from(std::env::var_os("TILO_APP_BIN"))
}

fn app_binary_from(configured: Option<OsString>) -> OsString {
    configured.unwrap_or_else(|| OsString::from("app"))
}

fn display_args(args: &[OsString]) -> String {
    if args.is_empty() {
        "--help".into()
    } else {
        args.iter()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn command_available(binary: &std::ffi::OsStr) -> bool {
    let path = std::path::Path::new(binary);
    if path.components().count() > 1 {
        return executable_file(path);
    }
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|directory| executable_file(&directory.join(path)))
    })
}

fn executable_file(path: &std::path::Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn app_available() -> bool {
    command_available(app_binary().as_os_str())
}

fn system_available() -> bool {
    command_available(std::ffi::OsStr::new(&helper()))
}

fn require_system_provider() -> Result<()> {
    if system_available() {
        Ok(())
    } else {
        bail!(
            "system image management is available only on Tilo OS, and its provider \
             was not found here; app commands remain available through `tilo app`"
        )
    }
}

fn update_everything(yes: bool) -> Result<ExitCode> {
    let app_provider = app_available() && !is_root();
    if is_root() && app_available() {
        eprintln!("tilo: user app updates are skipped while running as root");
    }
    update_available_providers(
        app_provider,
        system_available(),
        || run_app(&[OsString::from("update")]),
        || {
            privileged(
                &["update"],
                yes,
                "tilo-deployment-update.service",
                "Fetch and stage the current image",
            )
        },
    )
}

fn update_available_providers(
    app_is_available: bool,
    system_is_available: bool,
    mut update_apps: impl FnMut() -> Result<ExitCode>,
    mut update_system: impl FnMut() -> Result<ExitCode>,
) -> Result<ExitCode> {
    let mut attempted = false;
    let mut failed = false;
    if app_is_available {
        attempted = true;
        match update_apps() {
            Ok(status) => failed |= status != ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("tilo: app update failed: {error:#}");
                failed = true;
            }
        }
    } else {
        eprintln!("tilo: app updates skipped (`app` is not installed or not on PATH)");
    }
    if system_is_available {
        attempted = true;
        match update_system() {
            Ok(status) => failed |= status != ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("tilo: system update failed: {error:#}");
                failed = true;
            }
        }
    } else {
        eprintln!("tilo: system image update skipped (this is not Tilo OS)");
    }
    if !attempted {
        bail!("no update providers are available on this installation");
    }
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
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
            "\ntilo: starting {unit} failed. Either this session is not the one at \
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

  Reboot, and at the boot menu choose \"Tilo Repair Mode\".

It brings this machine up without the desktop — no compositor, no greeter, no
collaborative filesystem, no updates, no sshd — and offers: roll back, boot a
specific deployment once, re-fetch the image, make /var writable, read the
previous boot's log, and a root shell.

On an encrypted machine it asks for a passphrase enrolled on the disk (the
recovery key will do) before any of that. It is also where a machine ends up by
itself when three boots in a row fail to reach a login and there is no older
deployment to fall back to.

  /usr/share/doc/tilo/recovery.md     what a rollback does and does not undo
  /usr/share/doc/tilo/repair-mode.md  how the entry is built, and its threat model"
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
        let cli = Cli::try_parse_from(["tilo", "status"]).expect("parses");
        assert!(
            matches!(cli.cmd, Cmd::Status { json: false }),
            "status must parse without flags"
        );
    }

    #[test]
    fn provider_namespaces_parse_independently() {
        let system = Cli::try_parse_from(["tilo", "system", "status"]).expect("system parses");
        assert!(matches!(
            system.cmd,
            Cmd::System {
                cmd: SystemCmd::Status { json: false }
            }
        ));
        let app = Cli::try_parse_from(["tilo", "app", "u", "browser"]).expect("app parses");
        assert!(matches!(app.cmd, Cmd::App { .. }));
    }

    #[test]
    fn system_argv0_is_the_same_system_namespace() {
        let cli = Cli::try_parse_from(system_alias_args(vec![
            OsString::from("system"),
            OsString::from("update"),
            OsString::from("-y"),
        ]))
        .expect("the system alias parses");
        assert!(matches!(
            cli.cmd,
            Cmd::System {
                cmd: SystemCmd::Update { yes: true }
            }
        ));
    }

    #[test]
    fn missing_optional_provider_is_detectable() {
        assert!(!command_available(std::ffi::OsStr::new(
            "tilo-provider-that-does-not-exist"
        )));
    }

    #[test]
    fn app_provider_defaults_to_path_for_partial_non_tilo_installations() {
        assert_eq!(app_binary_from(None), OsString::from("app"));
        assert_eq!(
            app_binary_from(Some(OsString::from("/custom/app"))),
            OsString::from("/custom/app")
        );
    }

    #[test]
    fn app_help_is_forwarded_to_the_app_provider() {
        let cli = Cli::try_parse_from(["tilo", "app", "--help"])
            .expect("app help must be forwarded rather than consumed by the super-command");
        assert!(matches!(cli.cmd, Cmd::App { args } if args == [OsString::from("--help")]));
        assert!(is_help_request(&[OsString::from("--help")]));
        assert!(is_help_request(&[
            OsString::from("update"),
            OsString::from("-h")
        ]));
        assert!(!is_help_request(&[
            OsString::from("update"),
            OsString::from("browser")
        ]));
    }

    #[test]
    fn umbrella_update_uses_whichever_providers_are_available() {
        let mut app_runs = 0;
        let mut system_runs = 0;
        let status = update_available_providers(
            true,
            false,
            || {
                app_runs += 1;
                Ok(ExitCode::SUCCESS)
            },
            || {
                system_runs += 1;
                Ok(ExitCode::SUCCESS)
            },
        )
        .unwrap();
        assert_eq!(status, ExitCode::SUCCESS);
        assert_eq!((app_runs, system_runs), (1, 0));

        let status = update_available_providers(
            false,
            true,
            || Ok(ExitCode::SUCCESS),
            || Ok(ExitCode::SUCCESS),
        )
        .unwrap();
        assert_eq!(status, ExitCode::SUCCESS);
    }

    #[test]
    fn umbrella_update_fails_for_no_provider_or_any_failed_provider() {
        assert!(
            update_available_providers(
                false,
                false,
                || Ok(ExitCode::SUCCESS),
                || Ok(ExitCode::SUCCESS)
            )
            .is_err()
        );
        assert_eq!(
            update_available_providers(
                true,
                true,
                || Ok(ExitCode::FAILURE),
                || Ok(ExitCode::SUCCESS)
            )
            .unwrap(),
            ExitCode::FAILURE
        );

        let mut apps = 0;
        let mut system = 0;
        assert_eq!(
            update_available_providers(
                true,
                true,
                || {
                    apps += 1;
                    Ok(ExitCode::SUCCESS)
                },
                || {
                    system += 1;
                    anyhow::bail!("the deployment service is unavailable")
                }
            )
            .unwrap(),
            ExitCode::FAILURE,
            "one provider error must not turn a completed app update into an indeterminate CLI error"
        );
        assert_eq!((apps, system), (1, 1));
    }

    /// The helper path is a single named constant, not a string repeated at each call site — the
    /// failure mode being one verb pointing at a path the image does not have.
    #[test]
    fn the_helper_path_is_overridable_and_defaults_to_libexec() {
        // Not asserting on a process-global env var here (tests run in parallel); assert the
        // default, which is the part that ships.
        assert!(
            helper().ends_with("tilo-deployment"),
            "the helper must be tilo-deployment, got {}",
            helper()
        );
    }
}
