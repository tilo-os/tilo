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
use std::io::{IsTerminal, Write};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;

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

#[derive(Parser)]
#[command(
    name = "system",
    about = "Manage the Tilo OS image",
    after_help = "Examples:\n  system status\n  system update\n  system update --yes\n  system rollback"
)]
struct SystemCli {
    #[command(subcommand)]
    cmd: SystemCmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Generate shell completions on standard output.
    Completions {
        #[arg(value_enum)]
        shell: Shell,
    },
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
    let args: Vec<OsString> = std::env::args_os().collect();
    let result = if invoked_as(&args, "system") {
        let cli = SystemCli::parse_from(args);
        require_system_provider().and_then(|()| run_system(cli.cmd))
    } else {
        run(Cli::parse_from(args))
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("tilo: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn invoked_as(args: &[OsString], name: &str) -> bool {
    args.first()
        .and_then(|arg| std::path::Path::new(arg).file_stem())
        .is_some_and(|invoked| invoked == name)
}

fn run(cli: Cli) -> Result<ExitCode> {
    match cli.cmd {
        Cmd::Completions { shell } => {
            let mut output = Vec::new();
            clap_complete::generate(shell, &mut Cli::command(), "tilo", &mut output);
            let mut system = SystemCli::command();
            clap_complete::generate(shell, &mut system, "system", &mut output);
            append_app_completions(shell, &mut output)?;
            if let Err(error) = std::io::stdout().write_all(&output)
                && error.kind() != std::io::ErrorKind::BrokenPipe
            {
                return Err(error).context("writing completions");
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::System { cmd } => {
            require_system_provider()?;
            run_system(cmd)
        }
        Cmd::App { args } => run_app(&args, "tilo app"),
        Cmd::Store { args } => run_app(&args, "tilo store"),
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

fn append_app_completions(shell: Shell, output: &mut impl Write) -> Result<()> {
    let configured = app_binary();
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join("app")));
    let binary = if command_available(&configured) {
        std::path::PathBuf::from(configured)
    } else if let Some(path) = sibling.filter(|path| executable_file(path)) {
        path
    } else {
        return Ok(());
    };
    let generated = Command::new(binary)
        .args(["completions", &shell.to_string()])
        .output()
        .context("generating app completions")?;
    let mut app_script = String::from_utf8_lossy(&generated.stdout).into_owned();
    if shell == Shell::Fish {
        app_script = app_script.replace(
            "    set -e cmd[1]\n    argparse",
            "    set -e cmd[1]\n    if contains -- $cmd[1] app store\n        set -e cmd[1]\n    end\n    argparse",
        );
    }
    if generated.status.success() {
        output.write_all(app_script.as_bytes())?;
    }
    match shell {
        Shell::Bash => output.write_all(
            br#"
_tilo_with_app_completion() {
    if [[ "${COMP_WORDS[1]}" == "app" || "${COMP_WORDS[1]}" == "store" ]]; then
        local saved_cword=$COMP_CWORD
        local -a saved_words=("${COMP_WORDS[@]}")
        COMP_WORDS=(app "${COMP_WORDS[@]:2}")
        COMP_CWORD=$((COMP_CWORD - 1))
        _app app "${COMP_WORDS[COMP_CWORD]}" "${COMP_WORDS[COMP_CWORD-1]}"
        COMP_WORDS=("${saved_words[@]}")
        COMP_CWORD=$saved_cword
    else
        _tilo
    fi
}
complete -F _tilo_with_app_completion -o bashdefault -o default tilo
"#,
        )?,
        Shell::Fish => {
            output.write_all(
                b"\n# Reuse the app/store grammar below `tilo app` and `tilo store`.\n",
            )?;
            for line in app_script.lines().filter(|line| {
                line.starts_with("complete -c app ") || line.starts_with("complete -c store ")
            }) {
                let nested = line
                    .replacen("complete -c app ", "complete -c tilo ", 1)
                    .replacen("complete -c store ", "complete -c tilo ", 1);
                writeln!(output, "{nested}")?;
            }
        }
        Shell::Zsh => output.write_all(
            br#"
_tilo_with_app_completion() {
    if (( CURRENT > 2 )) && [[ "${words[2]}" == "app" || "${words[2]}" == "store" ]]; then
        local -a saved_words=("${words[@]}")
        local saved_current=$CURRENT
        words=(app "${words[@]:2}")
        CURRENT=$((CURRENT - 1))
        _app
        words=("${saved_words[@]}")
        CURRENT=$saved_current
    else
        _tilo
    fi
}
compdef _tilo_with_app_completion tilo
"#,
        )?,
        _ => {}
    }
    Ok(())
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

fn run_app(args: &[OsString], invocation: &str) -> Result<ExitCode> {
    if is_root() && !is_help_request(args) {
        bail!(
            "app management belongs to the signed-in user, not root; \
             leave the root shell and run `{invocation} {}`",
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
        .env("TILO_APP_DISPLAY_NAME", invocation)
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
    let app_skip_reason = if is_root() && app_available() {
        "user app updates skipped while running as root"
    } else {
        "app updates skipped (`app` is not installed or not on PATH)"
    };
    update_available_providers(
        app_provider,
        system_available(),
        app_skip_reason,
        || run_app(&[OsString::from("update")], "tilo app"),
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
    app_skip_reason: &str,
    mut update_apps: impl FnMut() -> Result<ExitCode>,
    mut update_system: impl FnMut() -> Result<ExitCode>,
) -> Result<ExitCode> {
    let mut attempted = false;
    let mut failed = false;
    if app_is_available {
        attempted = true;
        let started = provider_started("Apps");
        match update_apps() {
            Ok(status) => {
                provider_finished("Apps", started, status == ExitCode::SUCCESS);
                failed |= status != ExitCode::SUCCESS;
            }
            Err(error) => {
                eprintln!("tilo: app update failed: {error:#}");
                provider_finished("Apps", started, false);
                failed = true;
            }
        }
    } else {
        eprintln!("tilo: {app_skip_reason}");
    }
    if system_is_available {
        attempted = true;
        let started = provider_started("System image");
        match update_system() {
            Ok(status) => {
                provider_finished("System image", started, status == ExitCode::SUCCESS);
                failed |= status != ExitCode::SUCCESS;
            }
            Err(error) => {
                eprintln!("tilo: system update failed: {error:#}");
                provider_finished("System image", started, false);
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

fn provider_started(name: &str) -> Instant {
    eprintln!("\n{name}: checking for updates…");
    Instant::now()
}

fn provider_finished(name: &str, started: Instant, succeeded: bool) {
    eprintln!(
        "{name}: {} ({})",
        if succeeded { "done" } else { "failed" },
        human_duration(started.elapsed())
    );
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
        return run_helper_with_feedback(&a, what);
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
    let mut command = Command::new("systemctl");
    command.args(["start", "--wait", unit]);
    let status = wait_with_feedback(command, what).context("running systemctl start")?;
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

fn run_helper_with_feedback(args: &[&str], what: &str) -> Result<ExitCode> {
    let helper = helper();
    let mut command = Command::new(&helper);
    command.args(args).stdin(Stdio::inherit());
    let status = wait_with_feedback(command, what).with_context(|| format!("running {helper}"))?;
    Ok(exit_code(status.code()))
}

fn wait_with_feedback(
    mut command: Command,
    what: &str,
) -> std::io::Result<std::process::ExitStatus> {
    let mut child = command.spawn()?;
    let started = Instant::now();
    let terminal = std::io::stderr().is_terminal();
    let mut spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
        .into_iter()
        .cycle();
    let mut last_non_terminal_report = Instant::now();
    eprintln!("{what}…");
    loop {
        if let Some(status) = child.try_wait()? {
            if terminal {
                eprint!("\r\x1b[2K");
            }
            eprintln!(
                "{} {} ({})",
                if status.success() { "✓" } else { "✗" },
                what,
                human_duration(started.elapsed())
            );
            return Ok(status);
        }
        if terminal {
            let frame = spinner.next().unwrap_or("•");
            eprint!(
                "\r\x1b[2K{} {}… ({})",
                frame,
                what,
                human_duration(started.elapsed())
            );
            let _ = std::io::stderr().flush();
        } else if last_non_terminal_report.elapsed() >= Duration::from_secs(15) {
            eprintln!(
                "Still working: {what}… ({})",
                human_duration(started.elapsed())
            );
            last_non_terminal_report = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn human_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    }
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

    #[test]
    fn the_command_tree_parses_namespaces_and_completes_both_argv0s() {
        // Phase `the_command_tree_is_well_formed`.
        // clap's own consistency check: duplicate names, bad arg definitions, a subcommand with no
        // help. Fails the build rather than a user's first attempt to use it.
        {
            Cli::command().debug_assert();
            SystemCli::command().debug_assert();
        }
        // Phase `completion_scripts_cover_tilo_and_system`.
        {
            let mut output = Vec::new();
            clap_complete::generate(Shell::Bash, &mut Cli::command(), "tilo", &mut output);
            clap_complete::generate(
                Shell::Bash,
                &mut SystemCli::command(),
                "system",
                &mut output,
            );
            let script = String::from_utf8(output).expect("completion is UTF-8");
            assert!(script.contains("_tilo()"), "{script}");
            assert!(script.contains("default tilo"), "{script}");
            assert!(script.contains("_system()"), "{script}");
            assert!(script.contains("default system"), "{script}");
        }
        // Phase `elapsed_time_is_compact_and_stable`.
        {
            assert_eq!(human_duration(Duration::from_secs(9)), "9s");
            assert_eq!(human_duration(Duration::from_secs(125)), "2m 05s");
        }
        // Phase `status_takes_the_unprivileged_path`.
        // `status` must not require root. A person diagnosing a machine that won't boot properly
        // shouldn't have to authenticate to be told what's on it, which requiring it would push
        // them towards `sudo` for a read.
        {
            let cli = Cli::try_parse_from(["tilo", "status"]).expect("parses");
            assert!(
                matches!(cli.cmd, Cmd::Status { json: false }),
                "status must parse without flags"
            );
        }
        // Phase `provider_namespaces_parse_independently`.
        {
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
        // Phase `system_argv0_is_the_same_system_namespace`.
        {
            let args = vec![
                OsString::from("system"),
                OsString::from("update"),
                OsString::from("-y"),
            ];
            assert!(invoked_as(&args, "system"));
            let cli = SystemCli::try_parse_from(args).expect("the system alias parses");
            assert!(matches!(cli.cmd, SystemCmd::Update { yes: true }));
        }
    }

    #[test]
    fn providers_are_discovered_forwarded_and_overridable() {
        // Phase `missing_optional_provider_is_detectable`.
        {
            assert!(!command_available(std::ffi::OsStr::new(
                "tilo-provider-that-does-not-exist"
            )));
        }
        // Phase `app_provider_defaults_to_path_for_partial_non_tilo_installations`.
        {
            assert_eq!(app_binary_from(None), OsString::from("app"));
            assert_eq!(
                app_binary_from(Some(OsString::from("/custom/app"))),
                OsString::from("/custom/app")
            );
        }
        // Phase `app_help_is_forwarded_to_the_app_provider`.
        {
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
        // Phase `the_helper_path_is_overridable_and_defaults_to_libexec`.
        // The helper path is a single named constant, not a string repeated at each call site — the
        // failure mode being one verb pointing at a path the image does not have.
        {
            // Not asserting on a process-global env var here (tests run in parallel); assert the
            // default, which is the part that ships.
            assert!(
                helper().ends_with("tilo-deployment"),
                "the helper must be tilo-deployment, got {}",
                helper()
            );
        }
    }

    #[test]
    fn umbrella_update_runs_available_providers_and_fails_on_any_failure() {
        // Phase `umbrella_update_uses_whichever_providers_are_available`.
        {
            let mut app_runs = 0;
            let mut system_runs = 0;
            let status = update_available_providers(
                true,
                false,
                "app unavailable",
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
                "app unavailable",
                || Ok(ExitCode::SUCCESS),
                || Ok(ExitCode::SUCCESS),
            )
            .unwrap();
            assert_eq!(status, ExitCode::SUCCESS);
        }
        // Phase `umbrella_update_fails_for_no_provider_or_any_failed_provider`.
        {
            assert!(
                update_available_providers(
                    false,
                    false,
                    "app unavailable",
                    || Ok(ExitCode::SUCCESS),
                    || Ok(ExitCode::SUCCESS)
                )
                .is_err()
            );
            assert_eq!(
                update_available_providers(
                    true,
                    true,
                    "app unavailable",
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
                    "app unavailable",
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
    }
}
