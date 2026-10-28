//! `tilo` — one command for apps, actions and this device.
//!
//! ```text
//! tilo open calculator      open an app (getting it first when it isn't here)
//! tilo actions              what this desktop can do
//! tilo do mute              do one of those things
//! tilo app u browser        update one app
//! tilo system update        move onto the newest system made for this machine
//! tilo update               update apps and the system
//! ```
//!
//! `tilo --help` lists only what works where it runs (see [`Here`]): no
//! desktop commands without a desktop session, no `system` off Tilo OS.
//! A command typed anyway answers with one sentence saying what it needs.
//! The same program answers as `system` and `action` (`argv[0]`), and as
//! `tilo-<noun>`.
//!
//! ## Why the system half is thin
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

use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
mod agent;
mod aliases;
mod cleanup;
mod desktop;
mod health;
mod nouns;
mod os;
mod status;

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
    about = "Use apps and actions, and look after your devices",
    long_about = None
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

/// `tilo device` under its own names: `device`, `tilo-device`, and the
/// old `system`.
#[derive(Parser)]
#[command(name = "device", about = DEVICE_ABOUT)]
struct DeviceCli {
    #[command(subcommand)]
    cmd: DeviceCmd,
}

const DEVICE_ABOUT: &str = "Your devices: update them, go back, repair them, develop on them";
const PACKAGE_ABOUT: &str = "Make and sign packages";

/// `tilo agent` under its own names: `agent` and `tilo-agent`.
#[derive(Parser)]
#[command(
    name = "agent",
    about = "Connect agents to this desktop, and choose their models"
)]
struct AgentCli {
    #[command(subcommand)]
    cmd: agent::AgentCmd,
}

/// The standalone `action` command: `tilo action` under its own name.
#[derive(Parser)]
#[command(
    name = "action",
    about = "See what this desktop can do, and do it",
    after_help = "Examples:\n  action list            Everything this desktop can do now\n  action list volume     Only what matches\n  action show mute       What an action does and takes\n  action run mute        Do it"
)]
struct ActionCli {
    #[command(subcommand)]
    cmd: desktop::ActionCmd,
}

/// The nouns are listed; the shortcuts (`open`, `do`, `update`, `status`,
/// `rollback`) work and show in the examples.
#[derive(Subcommand)]
enum Cmd {
    /// Find, get, update and remove apps.
    #[command(disable_help_flag = true)]
    App {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
    /// Open an app, or files and links in it.
    #[command(
        hide = true,
        after_help = "Examples:\n  tilo open calculator\n  tilo open notes ~/Documents/plan.md\n  tilo open browser https://tilo.so\n\n\
                      An app this computer doesn't have yet is downloaded first."
    )]
    Open {
        /// The app: its name, a word of its name, or its id.
        app: String,
        /// Files or links to open in it.
        items: Vec<String>,
    },
    /// See what this desktop can do, and do it.
    #[command(
        after_help = "Examples:\n  tilo actions             Everything this desktop can do now\n  tilo actions volume      Only what matches\n  tilo action show mute    What an action does and takes\n  tilo do mute             Do it"
    )]
    Action {
        #[command(subcommand)]
        cmd: desktop::ActionCmd,
    },
    /// Do one thing this desktop can do.
    #[command(
        hide = true,
        after_help = "Examples:\n  tilo do new-terminal-window\n  tilo do rename-note title=Groceries\n  tilo do set-volume 40\n\n\
                      `tilo actions` lists what there is. Options are written name=value; a single\n\
                      value goes to the one the action needs."
    )]
    Do {
        /// The action's name, as `tilo actions` lists it, or words of its title.
        action: String,
        /// Its options, as name=value.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// List what this desktop can do.
    #[command(hide = true)]
    Actions {
        /// Words to look for.
        filter: Vec<String>,
    },
    /// The same as `tilo app`.
    #[command(hide = true, disable_help_flag = true)]
    Store {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
    /// Your devices: update them, go back, repair them, develop on them.
    #[command(aliases = ["system", "os"])]
    Device {
        #[command(subcommand)]
        cmd: DeviceCmd,
    },
    /// Connect agents to this desktop, and choose their models.
    Agent {
        #[command(subcommand)]
        cmd: agent::AgentCmd,
    },
    /// Make and sign packages.
    // Its verbs are the packaging programs' own (see `nouns::PACKAGE`);
    // `dispatch` runs them before this is ever built.
    #[command(subcommand_required = true, arg_required_else_help = true)]
    Package,
    /// Update apps and this device.
    #[command(hide = true)]
    Update {
        /// Don't ask before changing anything.
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// What runs on this device now, and what it can go back to.
    #[command(hide = true)]
    Status {
        /// The system's own record, as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Start the previous system version from now on.
    #[command(hide = true)]
    Rollback {
        /// Don't ask first.
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Start one system once, then go back to the usual one.
    #[command(hide = true)]
    Boot {
        /// Which one, as `tilo status` names it.
        entry: String,
    },
    /// How to reach Repair Mode, and what it can do.
    #[command(hide = true)]
    Repair,
    /// Shell completions, for packaging.
    #[command(hide = true)]
    Completions {
        #[arg(value_enum)]
        shell: Shell,
    },
    /// Names for shell completion, one per line.
    #[command(hide = true)]
    Complete {
        #[arg(value_enum)]
        what: desktop::Completing,
        prefix: Option<String>,
    },
}

#[derive(Subcommand)]
enum DeviceCmd {
    /// What runs now, what starts next, and what this device can go back to.
    Status {
        /// The system's own record, as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Start the previous system from now on.
    #[command(
        long_about = "Start the previous system from now on. Your files and settings stay as they are.\n\n\
                      What going back does and does not undo: /usr/share/doc/tilo/recovery.md"
    )]
    Rollback {
        /// Don't ask first.
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Move onto the newest system made for this device.
    #[command(
        name = "update",
        alias = "upgrade",
        long_about = "Move onto the newest system made for this device. Only what this device \
                      doesn't have yet is downloaded. The new system starts at the next restart; the \
                      one running now stays as the one to go back to."
    )]
    Upgrade {
        /// A development build to move onto instead (main-1a2b3c4).
        #[arg(long, value_name = "BUILD")]
        to: Option<String>,
        /// Don't ask anything; without --reboot, restart later yourself.
        #[arg(long, short = 'y')]
        yes: bool,
        /// Restart as soon as the new system is ready.
        #[arg(long)]
        reboot: bool,
        /// Say which system this device would move onto, and change nothing.
        #[arg(long)]
        check: bool,
    },
    /// Start one system once, then go back to the usual one.
    Boot {
        /// Which one, as `tilo status` names it.
        entry: String,
    },
    /// How to reach Repair Mode, and what it can do.
    Repair,
    /// Remove downloads and caches this device can get again.
    #[command(
        long_about = "Remove downloads and caches this device can get or make again. Nothing in use \
                      is removed. This also runs every day.\n\n\
                      What is kept is set in /etc/tilo/cache-gc.toml; ~/.config/tilo/cache-gc.toml \
                      replaces it for your own caches.",
        after_help = "Examples:\n  tilo device clean        Clean your own caches now\n  sudo tilo device clean   Clean the system's caches and unused OS image data now"
    )]
    Clean {
        /// Clean only this part of the system (the units that run each part pass it).
        #[arg(long, value_enum, hide = true)]
        only: Option<CleanPart>,
        /// Clean nothing; exit 0 when the disk is below the policy's free-space floor and 1
        /// otherwise (the disk-pressure unit's condition).
        #[arg(long, hide = true)]
        check_low_space: bool,
    },
    /// Check that the desktop draws with this device's graphics chip.
    #[command(
        after_help = "Examples:\n  tilo device health --gpu\n  tilo device health --gpu --json\n\nExits 1 when the graphics chip is there but the desktop or an app isn't using it."
    )]
    Health {
        /// Check graphics.
        #[arg(long)]
        gpu: bool,
        /// The answer as JSON.
        #[arg(long)]
        json: bool,
    },
}

/// What this computer offers, which decides what `tilo --help` lists.
#[derive(Clone, Copy)]
struct Here {
    /// A desktop session serves this process: open, action, do.
    desktop: bool,
    /// The app tools are installed: app.
    apps: bool,
    /// This is Tilo OS: device, status, rollback, and the system half of
    /// update.
    system: bool,
    /// Whether this computer has a program another noun's verbs run:
    /// [`nouns::present`], or a stand-in in a test.
    has: fn(&str) -> bool,
}

impl Here {
    fn now() -> Self {
        Self {
            desktop: desktop::available(),
            apps: app_available(),
            system: system_available(),
            has: nouns::present,
        }
    }
}

/// `tilo`'s command tree with only what works `here` listed: the rest is
/// hidden, still parses, and answers with one sentence saying what it needs.
fn command_here(here: Here) -> clap::Command {
    // Changed in place, so help keeps the order the commands are declared in.
    Cli::command()
        .mut_subcommand("device", |device| device_here(device, here))
        .mut_subcommand("agent", |agent| agent_here(agent, here))
        .mut_subcommand("package", |package| {
            nouns::with_verbs(package, nouns::PACKAGE, here.has)
        })
        .mut_subcommands(|sub| {
            let absent = match sub.get_name() {
                "open" | "action" | "do" => !here.desktop,
                "app" => !here.apps,
                "device" => !(here.system || nouns::any_present(nouns::DEVICE, here.has)),
                "status" | "rollback" => !here.system,
                "update" => !(here.apps || here.system),
                "agent" => !((here.has)(agent::DOOR) || (here.has)(agent::MODELS)),
                "package" => !nouns::any_present(nouns::PACKAGE, here.has),
                _ => return sub,
            };
            if absent { sub.hide(true) } else { sub }
        })
        .after_help(examples(here))
}

/// The tree completion offers: every noun and verb, the shortcuts too.
fn completing(here: Here) -> clap::Command {
    command_here(here).mut_subcommands(|sub| match sub.get_name() {
        "open" | "do" | "actions" | "update" | "status" | "rollback" => sub.hide(false),
        _ => sub,
    })
}

/// `device` with the verbs that work here: the system's own on Tilo OS,
/// and those whose program this computer has.
fn device_here(device: clap::Command, here: Here) -> clap::Command {
    let device = device.mut_subcommands(|sub| {
        let absent = match sub.get_name() {
            "status" | "rollback" | "update" | "boot" | "repair" => !here.system,
            _ => return sub,
        };
        if absent { sub.hide(true) } else { sub }
    });
    nouns::with_verbs(device, nouns::DEVICE, here.has).after_help(device_examples(here))
}

fn device_examples(here: Here) -> String {
    let mut lines: Vec<(&str, &str)> = Vec::new();
    if here.system {
        lines.extend([
            (
                "tilo device update",
                "Move onto the newest system for this device",
            ),
            (
                "tilo device update --check",
                "Say which system that is, change nothing",
            ),
            (
                "tilo device status",
                "What runs now, and what this device can go back to",
            ),
            (
                "tilo device rollback",
                "Start the previous system from now on",
            ),
        ]);
    }
    lines.push((
        "tilo device clean",
        "Remove downloads and caches nothing uses",
    ));
    if (here.has)("tilo-vm") {
        lines.extend([
            (
                "tilo device swap shell",
                "Put this checkout's shell onto your slot",
            ),
            ("tilo device shot", "Save a picture of your slot's screen"),
        ]);
    }
    render_examples(&lines)
}

fn agent_here(agent: clap::Command, here: Here) -> clap::Command {
    agent
        .mut_subcommand("connect", |sub| sub.hide(!(here.has)(agent::DOOR)))
        .mut_subcommand("models", |sub| sub.hide(!(here.has)(agent::MODELS)))
}

/// The examples under `tilo --help`: real commands, only ones that work here.
fn examples(here: Here) -> String {
    let mut lines: Vec<(&str, &str)> = Vec::new();
    if here.desktop {
        lines.extend([
            ("tilo open calculator", "Open Calculator"),
            ("tilo open notes ~/plan.md", "Open a file in Notes"),
            ("tilo actions", "What this desktop can do"),
            ("tilo do new-terminal-window", "Do one of those things"),
        ]);
    }
    if here.apps {
        lines.extend([
            ("tilo app search pdf", "Find apps"),
            ("tilo app update", "Update every app"),
        ]);
    }
    if here.system {
        lines.extend([
            ("tilo status", "What runs on this device"),
            ("tilo device update", "Move onto the newest system"),
        ]);
    }
    if here.apps || here.system {
        lines.push(("tilo update", "Update apps and this device"));
    }
    if (here.has)(agent::DOOR) {
        lines.push((
            "tilo agent connect claude",
            "Let Claude Code act on this desktop",
        ));
    }
    if nouns::any_present(nouns::PACKAGE, here.has) {
        lines.push(("tilo package pack", "Make a package of your app"));
    }
    render_examples(&lines)
}

/// `lines` as an `Examples:` block, commands aligned; nothing for none.
fn render_examples(lines: &[(&str, &str)]) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let width = lines
        .iter()
        .map(|(command, _)| command.len())
        .max()
        .unwrap_or(0);
    lines
        .iter()
        .fold(String::from("Examples:"), |mut text, (command, what)| {
            use std::fmt::Write as _;
            let _ = write!(text, "\n  {command:<width$}  {what}");
            text
        })
}

fn parse_with<T: clap::FromArgMatches>(command: clap::Command, args: Vec<OsString>) -> T {
    let matches = command.get_matches_from(args);
    T::from_arg_matches(&matches).unwrap_or_else(|error| error.exit())
}

/// `device`'s, `package`'s or `tilo`'s matches: a verb another program
/// owns runs it; everything else is `T`'s.
///
/// `own` is the verbs another program owns at the top of `command` itself:
/// `device`'s for `device`, none for `tilo`, whose nouns hold theirs.
fn dispatch<T: clap::FromArgMatches>(
    command: clap::Command,
    args: Vec<OsString>,
    own: &[nouns::Verb],
    run: impl FnOnce(T) -> Result<ExitCode>,
) -> Result<ExitCode> {
    let matches = command.get_matches_from(args);
    let handed_on = match matches.subcommand() {
        Some(("device", device)) => nouns::picked(device, nouns::DEVICE),
        Some(("package", package)) => nouns::picked(package, nouns::PACKAGE),
        _ => nouns::picked(&matches, own),
    };
    if let Some((verb, args)) = handed_on {
        return nouns::run(verb, &args);
    }
    run(T::from_arg_matches(&matches).unwrap_or_else(|error| error.exit()))
}

/// `package` under its own names.
fn package_command(here: Here) -> clap::Command {
    nouns::with_verbs(
        clap::Command::new("package")
            .about(PACKAGE_ABOUT)
            .subcommand_required(true)
            .arg_required_else_help(true),
        nouns::PACKAGE,
        here.has,
    )
}

/// The noun an `argv[0]` names: `device` (also the old `system`), `action`,
/// `package`, `agent`, each also as `tilo-<noun>` (and `tilo-app`, `app`
/// being its own program). `None` for `tilo` itself.
fn noun_of(args: &[OsString]) -> Option<String> {
    let stem = args
        .first()
        .and_then(|arg| std::path::Path::new(arg).file_stem())?
        .to_string_lossy()
        .into_owned();
    let noun = stem.strip_prefix("tilo-").unwrap_or(&stem);
    (noun != "tilo").then(|| noun.to_string())
}

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().collect();
    if let Some(name) = aliases::moved(&args) {
        return match aliases::exec(name, &args) {
            Ok(never) => match never {},
            Err(error) => {
                eprintln!("{name}: {error:#}");
                ExitCode::FAILURE
            }
        };
    }
    let invoked = args
        .first()
        .and_then(|arg| std::path::Path::new(arg).file_name())
        .map_or_else(
            || "tilo".to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
    let here = Here::now();
    let result = match noun_of(&args).as_deref() {
        Some("device" | "system" | "os") => dispatch(
            device_here(DeviceCli::command(), here).bin_name(&invoked),
            args,
            nouns::DEVICE,
            |cli: DeviceCli| run_system_command(cli.cmd),
        ),
        Some("package") => {
            let matches = package_command(here)
                .bin_name(&invoked)
                .get_matches_from(args);
            match nouns::picked(&matches, nouns::PACKAGE) {
                Some((verb, args)) => nouns::run(verb, &args),
                // A verb is required, so clap has answered already.
                None => Ok(ExitCode::FAILURE),
            }
        }
        Some("agent") => {
            let cli: AgentCli = parse_with(
                agent_here(AgentCli::command(), here).bin_name(&invoked),
                args,
            );
            agent::run(cli.cmd)
        }
        Some("app") => run_app(args.get(1..).unwrap_or_default(), &invoked),
        Some("action") => {
            let cli: ActionCli = parse_with(ActionCli::command().bin_name(&invoked), args);
            Ok(desktop::run_action(
                cli.cmd,
                &tilo_control::front::Spelling::standalone(),
            ))
        }
        _ => dispatch(command_here(here), args, &[], run),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("tilo: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// One part of the root cleanup.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum CleanPart {
    /// Flatpak runtimes and the developer image copy.
    Caches,
    /// OS image data no deployment uses.
    OsImages,
}

/// One `device` command: the system's own need Tilo OS; health and cleaning
/// work wherever a person runs them.
fn run_system_command(cmd: DeviceCmd) -> Result<ExitCode> {
    if !matches!(cmd, DeviceCmd::Health { .. } | DeviceCmd::Clean { .. }) {
        require_system_provider()?;
    }
    run_system(cmd)
}

/// `tilo app open APP [file|link…]`, parsed as `tilo open` is.
fn app_open(args: &[OsString], spelling: &tilo_control::front::Spelling) -> Result<ExitCode> {
    let Some(command) = Cli::command().find_subcommand("open").cloned() else {
        bail!("open is missing from the command tree");
    };
    let matches = command
        .name("open")
        .bin_name("tilo app open")
        .get_matches_from(
            std::iter::once(OsString::from("tilo app open")).chain(args.iter().cloned()),
        );
    let app = matches
        .get_one::<String>("app")
        .cloned()
        .unwrap_or_default();
    let items: Vec<String> = matches
        .get_many::<String>("items")
        .map(|items| items.cloned().collect())
        .unwrap_or_default();
    Ok(desktop::run_open(&app, &items, spelling))
}

fn run(cli: Cli) -> Result<ExitCode> {
    let spelling = tilo_control::front::Spelling::tilo();
    match cli.cmd {
        Cmd::Open { app, items } => Ok(desktop::run_open(&app, &items, &spelling)),
        Cmd::Action { cmd } => Ok(desktop::run_action(cmd, &spelling)),
        Cmd::Do { action, args } => Ok(desktop::run_action(
            desktop::ActionCmd::Run { action, args },
            &spelling,
        )),
        Cmd::Actions { filter } => Ok(desktop::run_action(
            desktop::ActionCmd::List { filter },
            &spelling,
        )),
        Cmd::Complete { what, prefix } => Ok(desktop::complete(
            what,
            prefix.as_deref().unwrap_or_default(),
        )),
        Cmd::Completions { shell } => {
            let mut output = Vec::new();
            let everything = Here {
                desktop: true,
                apps: true,
                system: true,
                has: |_| true,
            };
            clap_complete::generate(shell, &mut completing(everything), "tilo", &mut output);
            for name in ["device", "system"] {
                let mut device = device_here(DeviceCli::command(), everything);
                clap_complete::generate(shell, &mut device, name, &mut output);
            }
            let mut package = package_command(everything);
            clap_complete::generate(shell, &mut package, "package", &mut output);
            let mut agent = AgentCli::command();
            clap_complete::generate(shell, &mut agent, "agent", &mut output);
            clap_complete::generate(shell, &mut ActionCli::command(), "action", &mut output);
            append_app_completions(shell, &mut output)?;
            output.extend_from_slice(desktop::dynamic_completions(shell).as_bytes());
            if shell == Shell::Zsh && output.starts_with(b"#compdef tilo\n") {
                // One file answers every name this program goes by.
                output.splice(
                    ..b"#compdef tilo\n".len(),
                    b"#compdef tilo device system action package agent\n"
                        .iter()
                        .copied(),
                );
            }
            if let Err(error) = std::io::stdout().write_all(&output)
                && error.kind() != std::io::ErrorKind::BrokenPipe
            {
                return Err(error).context("writing completions");
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Device { cmd } => run_system_command(cmd),
        Cmd::Agent { cmd } => agent::run(cmd),
        Cmd::Package => bail!("package needs a verb; `tilo package --help` lists them"),
        Cmd::App { args } if args.first().is_some_and(|verb| verb == "open") => {
            app_open(args.get(1..).unwrap_or_default(), &spelling)
        }
        Cmd::App { args } => run_app(&args, "tilo app"),
        Cmd::Store { args } => run_app(&args, "tilo store"),
        // Read-only, so it never needs to become anything. A person diagnosing a machine should not
        // have to authenticate to be told what is on it.
        Cmd::Status { json } => {
            require_system_provider()?;
            let code = deployment_status(json)?;
            if !json {
                print_cleanup_status();
            }
            Ok(code)
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

fn run_system(cmd: DeviceCmd) -> Result<ExitCode> {
    match cmd {
        DeviceCmd::Status { json } => run(Cli {
            cmd: Cmd::Status { json },
        }),
        DeviceCmd::Rollback { yes } => run(Cli {
            cmd: Cmd::Rollback { yes },
        }),
        DeviceCmd::Upgrade {
            to,
            yes,
            reboot,
            check,
        } => os_upgrade(to.as_deref(), yes, reboot, check),
        DeviceCmd::Boot { entry } => run(Cli {
            cmd: Cmd::Boot { entry },
        }),
        DeviceCmd::Repair => {
            print_repair_help();
            Ok(ExitCode::SUCCESS)
        }
        DeviceCmd::Health { gpu: _, json } => health::run_gpu(json),
        DeviceCmd::Clean {
            only,
            check_low_space,
        } => {
            if check_low_space {
                return Ok(if cleanup::system_low_on_space()? {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                });
            }
            clean(only)
        }
    }
}

/// The root run cleans the system's caches and the OS image data no deployment uses, or
/// the one part `only` names; a person's run cleans their own.
fn clean(only: Option<CleanPart>) -> Result<ExitCode> {
    let now = std::time::SystemTime::now();
    let mut failed = false;
    if is_root() {
        let parts = match only {
            None => vec![cleanup::Scope::Caches, cleanup::Scope::OsImages],
            Some(CleanPart::Caches) => vec![cleanup::Scope::Caches],
            Some(CleanPart::OsImages) => vec![cleanup::Scope::OsImages],
        };
        for scope in parts {
            let record = cleanup::run_system(scope)?;
            print!(
                "{}",
                cleanup::render(scope_title(scope), Some(&record), now)
            );
            failed |= !record.failed.is_empty();
        }
    } else {
        let record = cleanup::run_user()?;
        print!("{}", cleanup::render("Test caches", Some(&record), now));
        println!(
            "System caches are cleaned daily by tilo-cache-gc.timer; `sudo tilo os clean` cleans them now."
        );
        failed = !record.failed.is_empty();
    }
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn scope_title(scope: cleanup::Scope) -> &'static str {
    match scope {
        cleanup::Scope::Caches => "Disk cleanup",
        cleanup::Scope::OsImages => "OS image cleanup",
    }
}

/// What the last cleanup runs freed and kept, after the deployments.
fn print_cleanup_status() {
    let now = std::time::SystemTime::now();
    let system = cleanup::read_record(std::path::Path::new(cleanup::SYSTEM_RECORD));
    print!(
        "\n{}",
        cleanup::render("Disk cleanup", system.as_ref(), now)
    );
    // Only on a machine that collected OS image data at least once: elsewhere there is none.
    if let Some(images) = cleanup::read_record(std::path::Path::new(cleanup::OS_IMAGE_RECORD)) {
        print!(
            "{}",
            cleanup::render("OS image cleanup", Some(&images), now)
        );
    }
    // A person's own run, once it has run at least once: on most machines nobody runs tests.
    if let Some(user) = cleanup::user_record().and_then(|path| cleanup::read_record(&path)) {
        print!("{}", cleanup::render("Test caches", Some(&user), now));
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
        bail!("managing apps needs Tilo's app tools, which this computer doesn't have");
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
        bail!("this works only on Tilo OS")
    }
}

fn update_everything(yes: bool) -> Result<ExitCode> {
    let app_provider = app_available() && !is_root();
    // Apps belong to each person; root updates only the system. Nothing to
    // say about tools this computer simply doesn't have.
    let app_skip_reason = if is_root() && app_available() {
        "apps are updated by each person, not by root"
    } else {
        ""
    };
    update_available_providers(
        app_provider,
        system_available(),
        app_skip_reason,
        || run_app(&[OsString::from("update")], "tilo app"),
        || os_upgrade(None, yes, false, false),
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
    } else if !app_skip_reason.is_empty() {
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
    }
    if !attempted {
        bail!("there is nothing to update on this computer");
    }
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// `tilo os upgrade`; see [`os`].
fn os_upgrade(to: Option<&str>, yes: bool, reboot: bool, check: bool) -> Result<ExitCode> {
    let facts = os::Facts::read_image(std::path::Path::new("/"));
    if !facts.development {
        os::plan(&facts, to)?;
        if check {
            println!(
                "This machine runs a signed Tilo OS image: `tilo os upgrade` fetches the newest signed release."
            );
            return Ok(ExitCode::SUCCESS);
        }
        return privileged(
            &["update"],
            yes,
            "tilo-deployment-update.service",
            "Fetch and stage the newest signed image",
        );
    }
    if !is_root() {
        let exe = std::env::current_exe().context("finding the tilo program to run as root")?;
        eprintln!("tilo: changing the OS image needs root; asking sudo");
        let status = Command::new("sudo")
            .args(os::sudo_args(&exe, to, yes, reboot, check))
            .status()
            .context("running sudo")?;
        return Ok(exit_code(status.code()));
    }
    let bootc = os::bootc_program();
    let facts = facts.with_bootc_status(&os::bootc_status(&bootc));
    let plan = os::plan(&facts, to)?;
    println!(
        "This machine runs {}",
        facts
            .booted
            .as_deref()
            .unwrap_or("an image bootc did not name")
    );
    println!(
        "Moving onto       {}{}",
        plan.image().unwrap_or("the newest signed release"),
        if matches!(plan, os::Plan::Upgrade(_)) {
            " (its newest build)"
        } else {
            ""
        }
    );
    if check {
        println!("Nothing changed (--check).");
        return Ok(ExitCode::SUCCESS);
    }
    let terminal = std::io::stderr().is_terminal();
    match os::stage(&bootc, &plan, &mut std::io::stderr().lock(), terminal)? {
        Ok(()) => {}
        Err(os::Failure::UpToDate(text)) => {
            println!("{text}");
            return Ok(ExitCode::SUCCESS);
        }
        Err(failure) => {
            eprintln!("tilo: {}", failure.sentence());
            return Ok(ExitCode::FAILURE);
        }
    }
    println!("{}", os::restart_note(&plan));
    let later = "Restart when you are ready; the new image boots then.";
    let restart =
        match os::restart_decision(reboot, yes, terminal && std::io::stdin().is_terminal()) {
            os::Restart::Now => true,
            os::Restart::Later => false,
            os::Restart::Ask => {
                eprint!("Restart now? [y/N] ");
                let mut line = String::new();
                std::io::stdin()
                    .read_line(&mut line)
                    .context("reading the answer")?;
                matches!(line.trim(), "y" | "Y" | "yes" | "YES")
            }
        };
    if !restart {
        println!("{later}");
        return Ok(ExitCode::SUCCESS);
    }
    let status = Command::new("systemctl")
        .arg("reboot")
        .status()
        .context("running systemctl reboot")?;
    Ok(exit_code(status.code()))
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

/// Root reads the disk itself; anyone else asks the status socket, where there is one.
fn deployment_status(json: bool) -> Result<ExitCode> {
    if let Some(socket) = status::socket().filter(|_| !is_root()) {
        return Ok(status::print(&status::ask(&socket, json)?));
    }
    let mut args = vec!["list"];
    if json {
        args.push("--json");
    }
    run_helper_directly(&args)
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

    run_privileged_unit(
        Command::new("systemctl"),
        Command::new("journalctl"),
        unit,
        what,
        &mut std::io::stderr().lock(),
    )
}

/// Keep the unit job's exit status and original stderr: a failed deployment
/// helper is different from a denied start request, and systemd reports both.
fn run_privileged_unit(
    mut start: Command,
    mut journal: Command,
    unit: &str,
    what: &str,
    diagnostics: &mut impl Write,
) -> Result<ExitCode> {
    start.args(["start", "--wait", unit]);
    let status = wait_with_feedback(start, what).context("running systemctl start")?;
    let lines = if status.success() { "40" } else { "80" };
    if !status.success() {
        writeln!(
            diagnostics,
            "\ntilo: {unit} failed ({status}). Unit output follows.\n\
             Inspect the update log with:\n  journalctl -u {unit} -n 80 --no-pager"
        )?;
    }
    journal.args(["-u", unit, "-n", lines, "--no-pager", "-o", "cat"]);
    match journal.status() {
        Ok(result) if result.success() => {}
        Ok(result) => writeln!(
            diagnostics,
            "tilo: reading {unit}'s journal failed ({result})"
        )?,
        Err(error) => writeln!(
            diagnostics,
            "tilo: could not read {unit}'s journal: {error}"
        )?,
    }
    Ok(exit_code(status.code()))
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
    use clap::{CommandFactory, FromArgMatches};

    #[test]
    fn failed_deployment_preserves_the_job_reason_journal_and_exit_status() {
        let directory = std::env::temp_dir().join(format!(
            "tilo-deployment-diagnostics-{}-{}",
            std::process::id(),
            tilo_clock::try_unix_now_ns().unwrap()
        ));
        std::fs::create_dir(&directory).unwrap();
        let stderr_path = directory.join("job.stderr");
        let journal_path = directory.join("journal.stdout");
        let mut start = Command::new("/bin/sh");
        start.args([
            "-c",
            "test \"$1\" = start && test \"$2\" = --wait && test \"$3\" = tilo-deployment-update.service || exit 99; printf '%s\\n' 'Job for tilo-deployment-update.service failed because the control process exited with error code.' >&2; exit 7",
            "systemctl-fixture",
        ]);
        start.stderr(std::fs::File::create(&stderr_path).unwrap());
        let mut journal = Command::new("/bin/sh");
        journal.args([
            "-c",
            "test \"$1\" = -u && test \"$2\" = tilo-deployment-update.service && test \"$4\" = 80 || exit 99; printf '%s\\n' 'tilo-deployment: bootc upgrade failed: registry unavailable'",
            "journalctl-fixture",
        ]);
        journal.stdout(std::fs::File::create(&journal_path).unwrap());
        let mut diagnostics = Vec::new();
        assert_eq!(
            run_privileged_unit(
                start,
                journal,
                "tilo-deployment-update.service",
                "Stage image",
                &mut diagnostics
            )
            .unwrap(),
            exit_code(Some(7))
        );
        assert_eq!(
            std::fs::read_to_string(&stderr_path).unwrap(),
            "Job for tilo-deployment-update.service failed because the control process exited with error code.\n"
        );
        assert_eq!(
            std::fs::read_to_string(&journal_path).unwrap(),
            "tilo-deployment: bootc upgrade failed: registry unavailable\n"
        );
        let diagnostics = String::from_utf8(diagnostics).unwrap();
        assert!(
            diagnostics.contains("journalctl -u tilo-deployment-update.service -n 80 --no-pager")
        );
        assert!(!diagnostics.contains("polkit"));
        assert!(!diagnostics.contains("seat"));
        assert!(!diagnostics.contains("sudo"));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn unreadable_journal_is_reported_without_replacing_the_unit_failure() {
        let mut start = Command::new("/bin/sh");
        start.args(["-c", "exit 7"]);
        let mut journal = Command::new("/bin/sh");
        journal.args(["-c", "exit 1"]);
        let mut diagnostics = Vec::new();
        assert_eq!(
            run_privileged_unit(
                start,
                journal,
                "tilo-deployment-update.service",
                "Stage image",
                &mut diagnostics,
            )
            .unwrap(),
            exit_code(Some(7))
        );
        let diagnostics = String::from_utf8(diagnostics).unwrap();
        assert!(diagnostics.contains("reading tilo-deployment-update.service's journal failed"));
        assert!(!diagnostics.contains("polkit"));
    }

    #[test]
    fn the_command_tree_parses_namespaces_and_completes_both_argv0s() {
        // Phase `the_command_tree_is_well_formed`.
        // clap's own consistency check: duplicate names, bad arg definitions, a subcommand with no
        // help. Fails the build rather than a user's first attempt to use it.
        {
            Cli::command().debug_assert();
            DeviceCli::command().debug_assert();
        }
        {
            let mut output = Vec::new();
            clap_complete::generate(Shell::Bash, &mut Cli::command(), "tilo", &mut output);
            clap_complete::generate(
                Shell::Bash,
                &mut DeviceCli::command(),
                "system",
                &mut output,
            );
            let script = String::from_utf8(output).expect("completion is UTF-8");
            assert!(script.contains("_tilo()"), "{script}");
            assert!(script.contains("default tilo"), "{script}");
            assert!(script.contains("_system()"), "{script}");
            assert!(script.contains("default system"), "{script}");
        }
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
        {
            let system = Cli::try_parse_from(["tilo", "system", "status"]).expect("system parses");
            assert!(matches!(
                system.cmd,
                Cmd::Device {
                    cmd: DeviceCmd::Status { json: false }
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
            assert_eq!(noun_of(&args).as_deref(), Some("system"));
            let cli = DeviceCli::try_parse_from(args).expect("the system alias parses");
            assert!(matches!(
                cli.cmd,
                DeviceCmd::Upgrade {
                    yes: true,
                    reboot: false,
                    ..
                }
            ));
            let os =
                Cli::try_parse_from(["tilo", "os", "upgrade", "--to", "main-1a2b3c4", "--check"])
                    .expect("os upgrade parses");
            assert!(matches!(
                os.cmd,
                Cmd::Device {
                    cmd: DeviceCmd::Upgrade { check: true, to: Some(ref to), .. }
                } if to == "main-1a2b3c4"
            ));
        }
    }

    #[test]
    fn the_cleanup_units_command_lines_parse() {
        // tilo-cache-gc.service, tilo-os-image-gc.service, and the disk-pressure condition.
        let caches = Cli::try_parse_from(["tilo", "os", "clean", "--only", "caches"]).unwrap();
        assert!(matches!(
            caches.cmd,
            Cmd::Device {
                cmd: DeviceCmd::Clean {
                    only: Some(CleanPart::Caches),
                    check_low_space: false
                }
            }
        ));
        let images = Cli::try_parse_from(["tilo", "os", "clean", "--only", "os-images"]).unwrap();
        assert!(matches!(
            images.cmd,
            Cmd::Device {
                cmd: DeviceCmd::Clean {
                    only: Some(CleanPart::OsImages),
                    ..
                }
            }
        ));
        let pressure = Cli::try_parse_from(["tilo", "os", "clean", "--check-low-space"]).unwrap();
        assert!(matches!(
            pressure.cmd,
            Cmd::Device {
                cmd: DeviceCmd::Clean {
                    only: None,
                    check_low_space: true
                }
            }
        ));
    }

    #[test]
    fn providers_are_discovered_forwarded_and_overridable() {
        {
            assert!(!command_available(std::ffi::OsStr::new(
                "tilo-provider-that-does-not-exist"
            )));
        }
        {
            assert_eq!(app_binary_from(None), OsString::from("app"));
            assert_eq!(
                app_binary_from(Some(OsString::from("/custom/app"))),
                OsString::from("/custom/app")
            );
        }
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

    const EVERYWHERE: Here = Here {
        desktop: true,
        apps: true,
        system: true,
        has: |_| true,
    };

    fn listed(here: Here) -> Vec<String> {
        command_here(here)
            .get_subcommands()
            .filter(|sub| !sub.is_hide_set())
            .map(|sub| sub.get_name().to_string())
            .collect()
    }

    /// The help is about the person: on a Tilo laptop in a desktop session it
    /// lists exactly what works there, with real examples and no narration
    /// about providers or other distributions.
    #[test]
    fn help_on_a_tilo_desktop_lists_what_works_there_verbatim() {
        assert_eq!(
            listed(EVERYWHERE),
            ["app", "action", "device", "agent", "package"]
        );
        let help = command_here(EVERYWHERE).render_help().to_string();
        assert_eq!(
            help,
            concat!(
                "Use apps and actions, and look after your devices\n",
                "\n",
                "Usage: tilo <COMMAND>\n",
                "\n",
                "Commands:\n",
                "  app      Find, get, update and remove apps\n",
                "  action   See what this desktop can do, and do it\n",
                "  device   Your devices: update them, go back, repair them, develop on them\n",
                "  agent    Connect agents to this desktop, and choose their models\n",
                "  package  Make and sign packages\n",
                "  help     Print this message or the help of the given subcommand(s)\n",
                "\n",
                "Options:\n",
                "  -h, --help  Print help\n",
                "\n",
                "Examples:\n",
                "  tilo open calculator         Open Calculator\n",
                "  tilo open notes ~/plan.md    Open a file in Notes\n",
                "  tilo actions                 What this desktop can do\n",
                "  tilo do new-terminal-window  Do one of those things\n",
                "  tilo app search pdf          Find apps\n",
                "  tilo app update              Update every app\n",
                "  tilo status                  What runs on this device\n",
                "  tilo device update           Move onto the newest system\n",
                "  tilo update                  Update apps and this device\n",
                "  tilo agent connect claude    Let Claude Code act on this desktop\n",
                "  tilo package pack            Make a package of your app\n",
            )
        );
    }

    /// On another distribution, what can't work there is simply absent: no
    /// desktop commands without a desktop, no `system` off Tilo OS. Typing one
    /// anyway still parses, and answers with what it needs.
    #[test]
    fn help_elsewhere_leaves_out_what_cannot_work() {
        let elsewhere = Here {
            desktop: false,
            apps: true,
            system: false,
            has: |_| false,
        };
        assert_eq!(listed(elsewhere), ["app"]);
        let help = command_here(elsewhere).render_help().to_string();
        for absent in [
            "tilo open",
            "tilo do",
            "tilo actions",
            "device update",
            "status",
        ] {
            assert!(!help.contains(absent), "{absent} in:\n{help}");
        }
        let nothing = Here {
            desktop: false,
            apps: false,
            system: false,
            has: |_| false,
        };
        assert!(listed(nothing).is_empty(), "{:?}", listed(nothing));
        assert!(
            !command_here(nothing)
                .render_help()
                .to_string()
                .contains("Examples")
        );
        // Hidden, not removed: a typed command reaches its one plain sentence.
        let typed = command_here(nothing)
            .try_get_matches_from(["tilo", "device", "status"])
            .expect("a hidden command still parses");
        assert!(Cli::from_arg_matches(&typed).is_ok());
        // No narration about the program in any help it prints.
        for here in [EVERYWHERE, elsewhere, nothing] {
            let mut command = command_here(here);
            let mut texts = vec![command.render_long_help().to_string()];
            for sub in command.get_subcommands_mut() {
                texts.push(sub.render_long_help().to_string());
            }
            for text in texts {
                for word in [
                    "provider",
                    "distribution",
                    "PATH",
                    "bootc",
                    "OS image",
                    "Tilo OS-only",
                ] {
                    assert!(!text.contains(word), "{word} in:\n{text}");
                }
            }
        }
    }

    #[test]
    fn open_action_and_do_parse_under_every_name() {
        let open = Cli::try_parse_from(["tilo", "open", "calc", "a.txt", "https://tilo.so"])
            .expect("open parses");
        assert!(matches!(
            open.cmd,
            Cmd::Open { ref app, ref items } if app == "calc" && items == &["a.txt", "https://tilo.so"]
        ));
        let run = Cli::try_parse_from(["tilo", "do", "rename-note", "title=Groceries", "--force"])
            .expect("do parses");
        assert!(matches!(
            run.cmd,
            Cmd::Do { ref action, ref args } if action == "rename-note" && args == &["title=Groceries", "--force"]
        ));
        let list = Cli::try_parse_from(["tilo", "actions", "volume"]).expect("actions parses");
        assert!(matches!(list.cmd, Cmd::Actions { ref filter } if filter == &["volume"]));
        let show = Cli::try_parse_from(["tilo", "action", "show", "mute"]).expect("show parses");
        assert!(matches!(
            show.cmd,
            Cmd::Action { cmd: desktop::ActionCmd::Show { ref action } } if action == "mute"
        ));
        let app_open = Cli::try_parse_from(["tilo", "app", "open", "notes", "plan.md"])
            .expect("app open parses");
        assert!(matches!(
            app_open.cmd,
            Cmd::App { ref args } if args.first().is_some_and(|verb| verb == "open")
        ));
        let standalone = ActionCli::try_parse_from(["action", "run", "mute"]).expect("action run");
        assert!(matches!(
            standalone.cmd,
            desktop::ActionCmd::Run { ref action, .. } if action == "mute"
        ));
        ActionCli::command().debug_assert();
        for (argv0, noun) in [
            ("/usr/bin/action", Some("action")),
            ("tilo-action", Some("action")),
            ("/usr/bin/system", Some("system")),
            ("tilo-device", Some("device")),
            ("tilo-package", Some("package")),
            ("/usr/bin/agent", Some("agent")),
            ("device", Some("device")),
            ("/usr/bin/tilo", None),
        ] {
            assert_eq!(
                noun_of(&[OsString::from(argv0)]).as_deref(),
                noun,
                "{argv0}"
            );
        }
    }

    /// `device` and `package` verbs another program owns parse with every
    /// argument kept for it, `--help` included, and leave help where the
    /// program is missing.
    #[test]
    fn handed_on_verbs_keep_their_arguments_and_show_only_where_they_work() {
        let matches = command_here(EVERYWHERE)
            .try_get_matches_from(["tilo", "package", "inspect", "x.tapp", "--help"])
            .expect("package inspect parses");
        let (_, package) = matches.subcommand().expect("a noun");
        let (verb, args) = nouns::picked(package, nouns::PACKAGE).expect("a handed-on verb");
        assert_eq!(
            (verb.program, verb.prefix),
            ("tilo-app-package", &["inspect"][..])
        );
        assert_eq!(args, ["x.tapp", "--help"]);
        let matches = command_here(EVERYWHERE)
            .try_get_matches_from(["tilo", "device", "swap", "shell", "--target", "lab"])
            .expect("device swap parses");
        let (_, device) = matches.subcommand().expect("a noun");
        let (verb, args) = nouns::picked(device, nouns::DEVICE).expect("a handed-on verb");
        assert_eq!((verb.program, verb.prefix), ("tilo-vm", &["swap"][..]));
        assert_eq!(args, ["shell", "--target", "lab"]);
        let matches = command_here(EVERYWHERE)
            .try_get_matches_from(["tilo", "device", "status"])
            .expect("device status parses");
        let (_, device) = matches.subcommand().expect("a noun");
        assert!(nouns::picked(device, nouns::DEVICE).is_none());

        let bare = Here {
            desktop: false,
            apps: false,
            system: false,
            has: |_| false,
        };
        let device = device_here(DeviceCli::command(), bare);
        let shown: Vec<&str> = device
            .get_subcommands()
            .filter(|sub| !sub.is_hide_set())
            .map(clap::Command::get_name)
            .collect();
        assert_eq!(shown, ["clean", "health"]);
        assert!(!device_examples(bare).contains("swap"));
        let agent = agent_here(AgentCli::command(), bare);
        assert!(agent.get_subcommands().all(clap::Command::is_hide_set));
        let all: Vec<String> = completing(EVERYWHERE)
            .get_subcommands()
            .filter(|sub| !sub.is_hide_set())
            .map(|sub| sub.get_name().to_owned())
            .collect();
        for name in ["open", "do", "app", "device", "agent", "package", "update"] {
            assert!(all.iter().any(|listed| listed == name), "{name} in {all:?}");
        }
    }

    #[test]
    fn completions_ask_the_desktop_for_app_and_action_names() {
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let glue = desktop::dynamic_completions(shell);
            assert!(
                glue.contains("tilo complete actions") || glue.contains("tilo complete \"$what\""),
                "{shell}"
            );
            assert!(glue.contains("open"), "{shell}");
        }
        let typed =
            Cli::try_parse_from(["tilo", "complete", "apps", "cal"]).expect("complete parses");
        assert!(matches!(
            typed.cmd,
            Cmd::Complete { what: desktop::Completing::Apps, prefix: Some(ref prefix) } if prefix == "cal"
        ));
    }
}
