//! `tilo` — one command for apps, actions and this device.
//!
//! ```text
//! tilo open calculator      open an app (getting it first when it isn't here)
//! tilo action list          what this desktop can do
//! tilo do mute              do one of those things
//! tilo app u browser        update one app
//! tilo system update        move onto the newest system made for this machine
//! tilo update               update apps and the system
//! ```
//!
//! `tilo --help` lists only what works where it runs (see [`Here`]): no desktop commands without a
//! desktop session, no `system` off Tilo OS. A command typed anyway answers with one sentence
//! saying what it needs. The same program answers as `system` and `action` (`argv[0]`), and as
//! `tilo-<noun>`.
//!
//! The system half is thin because the work lives in `/usr/libexec/tilo-deployment`. What lives
//! here is the decision how to become root: run it directly when already root, start
//! `tilo-deployment-rollback.service` from a desktop session (polkit authorises the local active
//! user, see `os/etc/polkit-1/rules.d/49-tilo-updates.rules`), else say to use sudo rather than
//! fail with a permission error from three layers down. Settings' rollback control takes the
//! second, so the button and the command line cannot drift.

use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
mod agent;
mod aliases;
use tilo_api::calls as api;
mod cleanup;
mod desktop;
mod device;
mod docs;
mod folders;
mod health;
mod hosted;
mod identity;
mod login;
mod machine;
mod mcp;
mod migrate;
mod nouns;
mod os;
mod person;
mod sdk;
mod server;
mod settings;
mod status;

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;

/// Where the privileged half lives. Overridable so the CLI can be exercised
/// against a stub in a test without a machine to break.
fn helper() -> String {
    std::env::var("TILO_DEPLOYMENT_HELPER")
        .unwrap_or_else(|_| "/usr/libexec/tilo-deployment".to_string())
}

/// What `tilo --version` prints: the release this program was built for, which
/// `cli-release.sh build` sets from the tag it builds (`TILO_CLI_VERSION`), else the crate's own
/// version, for a build from the tree.
fn release_version() -> &'static str {
    option_env!("TILO_CLI_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}

#[derive(Parser)]
#[command(
    name = "tilo",
    about = "Use apps and actions, and look after your devices",
    long_about = None,
    version = release_version()
)]
struct Cli {
    #[arg(
        short = 'w',
        long,
        global = true,
        help = "A hosted workspace to act in, by name or id; this computer's own when not given"
    )]
    workspace: Option<String>,
    #[arg(
        long,
        global = true,
        value_name = "FILE",
        help = "A key file to act as (`tilo do account/keys#create --out FILE`; also TILO_KEY_FILE)"
    )]
    key_file: Option<std::path::PathBuf>,
    #[arg(
        long,
        global = true,
        help = "The answer as JSON, typed as the SDK gets it"
    )]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

impl Cli {
    /// The command `cmd`, with no hosted workspace or key file named.
    const fn of(cmd: Cmd) -> Self {
        Self {
            workspace: None,
            key_file: None,
            json: false,
            cmd,
        }
    }
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
        after_help = "Examples:\n  tilo action list         Everything this desktop can do now\n  tilo action list volume  Only what matches\n  tilo action show mute    What an action does and takes\n  tilo do mute             Do it"
    )]
    Action {
        #[command(subcommand)]
        cmd: desktop::ActionCmd,
    },
    /// Do one thing this desktop, or a hosted workspace (-w), can do.
    #[command(
        after_help = "Examples:\n  tilo do new-terminal-window\n  tilo do rename-note title=Groceries\n  tilo do set-volume 40\n  \
                      tilo -w jane-doe do pdf-viewer/application.pdf#read-form-fields\n\n\
                      `tilo action list` shows what this desktop can do; in a hosted workspace,\n\
                      `tilo ls -w <workspace>` lists what it holds. Options are written name=value; a\n\
                      single value goes to the one the action needs; name=@- reads that value from\n\
                      standard input, for a secret that should stand in no command line. With -w, the path is\n\
                      app/resource#action in that hosted workspace, as this computer's own login\n\
                      or the key file --key-file names; an account/… path acts on your account\n\
                      with no -w (tilo do account/keys#create name=\"My backend\"). An argument\n\
                      taking a workspace takes its name. --out FILE writes a secret the action\n\
                      answers (a key) to FILE, readable by you only, and never prints it.\n\n\
                      On a computer that holds the person's identity, --in-person does it as the\n\
                      person themselves rather than as the computer's agent: what only they\n\
                      answer, such as a question their agent asks them\n\
                      (chat/Messages#answer-question message=<id> answer=allow)."
    )]
    Do {
        /// Do it as the person themselves, on a computer that holds their identity, rather
        /// than as this computer's agent: what only they answer.
        #[arg(long)]
        in_person: bool,
        /// The action's name, as `tilo action list` lists it, or words of its title.
        action: String,
        /// Its options, as name=value.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// List what a hosted workspace (-w) holds: its apps, or what a path holds.
    #[command(
        after_help = "Examples:\n  tilo ls -w jane-doe\n  tilo ls notes -w jane-doe\n  tilo ls account/keys\n  \
                      tilo ls notes/Notebook -w jane-doe\n\n\
                      Each line is a path the other hosted commands take: `tilo read`, `tilo do`,\n\
                      and `tilo ls` again."
    )]
    Ls {
        /// What to list: an app, a resource or an action, as `tilo ls` printed it.
        what: Option<String>,
        /// The cursor a long listing ended with, for the rest of it.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Read one resource of a hosted workspace (-w).
    #[command(
        after_help = "Example:\n  tilo read notes/Notebook/Todo -w jane-doe\n\n\
                      A long read ends with a cursor: `tilo read notes/Todo --cursor <cursor> -w`\n\
                      reads the rest."
    )]
    Read {
        /// The resource to read: an app, a record, or what a listing printed.
        resource: String,
        /// The cursor a long read ended with, for the rest of it.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// What an app shows of a resource: its view, line by line.
    #[command(
        after_help = "Examples:\n  tilo view notes/Notebook/Todo -w jane-doe   What the Notes window shows for the note\n                        tilo view tasks -w jane-doe                  What the Tasks window shows\n                        tilo view notes/Todo --picture todo.png -w   And a picture of it\n\n                      Every label, value and control, in reading order, each control's action."
    )]
    View {
        /// The app or resource whose view to read: a path `tilo ls` printed.
        resource: String,
        /// Also write a picture of the view to this file.
        #[arg(long, value_name = "FILE")]
        picture: Option<std::path::PathBuf>,
    },
    /// What your servers are, and what you may ask them to do from here.
    #[command(
        after_help = "Examples:\n  tilo server list\n  tilo server status nas\n  tilo server logs nas tilo-server.service\n  \
                      tilo server restart nas tilo-server.service\n  tilo server update nas\n\n\
                      A server is reached over Tilo's owner-only route from this computer's own \
                      device: nothing here needs ssh. `tilo server list` names the servers this \
                      computer enrolled."
    )]
    Server {
        #[command(subcommand)]
        cmd: server::ServerCmd,
    },
    /// Machines you prepared to install, and how they reach them.
    #[command(after_help = "Examples:\n  tilo machine add --name cupboard\n  \
                      tilo machine add --os os --name spare-laptop --disk largest\n  \
                      tilo machine list\n  tilo machine show 3f2b1c00d4e5f60718293a4b5c6d7e8f\n\n\
                      A plan's choices are made here or left open; an open choice is answered \
                      from your own devices against the machine's real hardware. The token an \
                      installation enrolls with is written to a file, never printed.")]
    Machine {
        #[command(subcommand)]
        cmd: machine::MachineCmd,
    },
    /// Files into a hosted workspace.
    #[command(
        after_help = "Examples:\n  tilo files put passport.png -w jane-doe\n  \
                            tilo files put ./contracts -w jane-doe --to docs\n  \
                            tilo files put scan.pdf -w jane-doe --store\n  \
                            tilo files get tilo.content:… -w jane-doe -o cover-letter.docx"
    )]
    Files {
        #[command(subcommand)]
        cmd: hosted::FilesCmd,
    },
    /// Run a tilolang program in a hosted workspace, as one undoable step.
    #[command(after_help = "Example:\n  tilo run visa.tl -w jane-doe")]
    Run {
        /// The program to run, by its path here.
        file: std::path::PathBuf,
    },
    /// The invocations of a hosted workspace (-w) in records you may read, newest first.
    #[command(
        after_help = "Examples:\n  tilo invocations -w jane-doe\n  tilo invocations -w jane-doe --limit 10\n\n\
                      Each line is an invocation: its id, how it stands (waiting for you, running,\n\
                      ended, failed, cancelled, undone), who made it and what it changed. A long list\n\
                      ends with a cursor: `tilo invocations --cursor <cursor> -w` reads on."
    )]
    Invocations {
        /// How many invocations at most: 50 unless said, at most 200.
        #[arg(long)]
        limit: Option<u32>,
        /// The cursor a long list ended with, for the older invocations.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Write the SDK's types for a workspace's apps, from their surfaces.
    Sdk {
        #[command(subcommand)]
        cmd: sdk::SdkCmd,
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
    /// Every command as a page of documentation, for tilo.sh/docs.
    #[command(hide = true)]
    HelpMarkdown,
    /// Log this computer in as one of your devices, with the ticket one of
    /// your devices gave you.
    #[command(after_help = "Example:\n  tilo login tilo1…\n\n\
                      On a device of yours, run `tilo device add`: it prints a ticket. This computer reaches that \
                      device directly with it, and that device certifies this computer's own key once you approve. \
                      A ticket works once, for 10 minutes, while that device waits for it.")]
    Login {
        /// The ticket `tilo device add` printed on one of your devices, or an invitation link
        /// a member sent you, which joins their workspace over iroh. `-` reads it from standard
        /// input, so a script's ticket never shows in the process list.
        ticket: Option<String>,
        /// Instead: make a Tilo identity whose root this computer holds (or
        /// say it has one), and print its user public key. A host's operator
        /// declares that key an owner.
        #[arg(long, conflicts_with_all = ["ticket", "agent"])]
        new_identity: bool,
        /// Instead: become again the person whose 24-word recovery key is typed in (read from
        /// standard input, never from the command line), on a computer that holds no identity
        /// or holds theirs. It logs in with a ticket from one of their devices afterwards.
        #[arg(long, conflicts_with_all = ["ticket", "new_identity", "name"])]
        recovery_key: bool,
        /// The new identity's name, as workspaces show it (with `--new-identity`, or with an
        /// invitation link on a computer that holds no identity yet).
        #[arg(long)]
        name: Option<String>,
        /// Print the identity as one line of JSON, `{"user_public_key": …,
        /// "name": …}`, for a script that declares the key and wants the name
        /// the person goes by with it.
        #[arg(long, requires = "new_identity")]
        json: bool,
        /// Which of your agents runs here, by the name your workspace shows
        /// it under: every computer logged in under one name is one agent,
        /// with its history; another name is another agent. "Your agent"
        /// when not given.
        #[arg(long)]
        agent: Option<String>,
        /// How your other devices name this computer. The agent and the
        /// hostname when not given.
        #[arg(long, conflicts_with_all = ["new_identity", "recovery_key"])]
        label: Option<String>,
        /// Instead, on a computer that holds your Tilo identity: log in at the server whose
        /// `web-host.json` line this is (the JSON itself, or the file's path). Your identity here
        /// certifies this computer, so no ticket is needed; the computer then takes in every
        /// workspace of yours that server keeps, and serves the files it holds of them.
        #[arg(long, conflicts_with_all = ["ticket", "new_identity", "recovery_key", "agent"])]
        server: Option<String>,
    },
    /// Tilo's MCP server over stdio, for the agent on this computer
    /// (an agent adds it to its own MCP configuration).
    Mcp,
    /// Your workspaces' synced folders on this computer: each workspace's
    /// one folder, at ~/<workspace> unless moved (`tilo login` adds the first).
    #[command(
        after_help = "Examples:\n  tilo folders\n  cp -r ~/Documents/contracts ~/Acme/\n  tilo folders add \"Book club\"\n  tilo folders free ~/Acme/videos\n  tilo folders pause Acme"
    )]
    Folders {
        /// The answer as JSON.
        #[arg(long, global = true)]
        json: bool,
        #[command(subcommand)]
        cmd: Option<folders::FoldersCmd>,
    },
    /// Bring your things over from this computer and the services you use: Thunderbird's
    /// mail, contacts and calendars, Outline, Linear and Forgejo.
    #[command(
        after_help = "Examples:\n  tilo migrate sources\n  tilo migrate review thunderbird\n  \
                            tilo migrate choose thunderbird ada@example.com --only\n  \
                            tilo migrate start thunderbird\n  tilo migrate start thunderbird -w \"Jane Doe\"\n  \
                            tilo migrate status\n\n\
                      Each step is its own command, so an agent can show the person what a source holds \
                      before anything moves. What comes over appears in the workspace as one entry in \
                      History, which undoes it. Running it again brings only what is not there yet. \
                      Keys and passwords are never arguments: `connect` asks the person for a key.\n\n\
                      Without -w a run lands in this computer's own workspace. With it, the run lands in \
                      that hosted workspace (a name or an id), as the key this computer keeps: the \
                      file --key-file names, or the key `tilo login` kept."
    )]
    Migrate {
        /// The answer as JSON.
        #[arg(long, global = true)]
        json: bool,
        #[command(subcommand)]
        cmd: migrate::MigrateCmd,
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
    /// Read or change the settings, for scripts.
    #[command(hide = true)]
    Settings {
        #[command(subcommand)]
        cmd: settings::SettingsCmd,
    },
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
    /// Add a computer as one of your devices: print a ticket for `tilo login` there, and wait
    /// for it.
    #[command(
        after_help = "Example:\n  tilo device add\n\nThe ticket works once, for 10 minutes. The computer that \
                      logs in with it reaches this one directly, and this one certifies it as your device."
    )]
    Add {
        /// The ticket a browser's sign-in prompt carried: sign that browser in as you, instead
        /// of printing a ticket for a computer.
        ticket: Option<String>,
        /// Print the ticket and the device's certificate as JSON lines.
        #[arg(long)]
        json: bool,
        /// Print the ticket and return: the daemon keeps serving it while it lasts, for a
        /// provisioner that hands it on rather than waiting for the computer itself.
        #[arg(long)]
        no_wait: bool,
        /// The planned machine this computer will be, by its plan's id in your Servers
        /// workspace: its certification takes that entry into its own, under its device key.
        #[arg(long)]
        plan: Option<String>,
        /// The computer is a server, which keeps your workspaces: its certification leaves a
        /// new entry for it in your Servers workspace.
        #[arg(long)]
        server: bool,
    },
    /// Your devices, as your workspaces know them.
    Ls {
        /// As JSON.
        #[arg(long)]
        json: bool,
    },
    /// This computer's device key, the person it is certified for and its label, as one JSON
    /// line, read from its login without reaching any host; with no login, the workspace its
    /// daemon founded.
    #[command(hide = true)]
    Key,
    /// Name one of your devices, in every list.
    Rename {
        /// The device, by its key (a prefix of it) or its name.
        device: String,
        /// Its new name.
        name: String,
    },
    /// End the login of one of your devices, everywhere it reaches.
    Revoke {
        /// The device, by its key (a prefix of it) or its name.
        device: String,
        /// Why, kept with the revocation.
        #[arg(long, value_enum, default_value_t = device::Why::Retired)]
        reason: device::Why,
        /// Don't ask first.
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Which account on this device is the person its identity belongs to: the one whose
    /// proof, signed by the identity's key, names its uid. Prints `NAME UID:GID`; exits 2 when
    /// several prove it. The system asks this as root when it prepares the store.
    #[command(hide = true)]
    LocalPerson {
        /// The device's identity tree.
        #[arg(long, value_name = "DIR", default_value = "/var/lib/tilo")]
        identity_root: std::path::PathBuf,
        /// The accounts, in passwd format; default `getent passwd`.
        #[arg(long, value_name = "FILE")]
        passwd: Option<std::path::PathBuf>,
    },
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
                "open" | "action" => !here.desktop,
                "app" => !here.apps,
                "device" => !(here.system || nouns::any_present(nouns::DEVICE, here.has)),
                // A computer logged in as one of the person's devices tells of its folders.
                "status" => !(here.system || login::logged_in()),
                // Its servers are reached as the device it enrolled them with.
                "server" | "machine" => !login::logged_in(),
                "rollback" => !here.system,
                "update" => !(here.apps || here.system),
                "agent" => {
                    !((here.has)(agent::MCP_SERVER)
                        || (here.has)(agent::MODELS)
                        || login::logged_in())
                }
                "package" => !nouns::any_present(nouns::PACKAGE, here.has),
                // A desktop's agents use its own MCP server; elsewhere this
                // computer logs in to the person's workspace instead, and its
                // folders are what a Tilo desktop's own files are there, and
                // its migrations land through that login (a desktop has its
                // own, in its setup).
                "login" | "folders" | "migrate" => here.desktop || (here.has)(agent::MCP_SERVER),
                // Started by agent tools, not typed.
                "mcp" => true,
                _ => return sub,
            };
            if absent { sub.hide(true) } else { sub }
        })
        .after_help(examples(here))
}

/// The tree completion offers: every noun and verb, the shortcuts too.
fn completing(here: Here) -> clap::Command {
    command_here(here).mut_subcommands(|sub| match sub.get_name() {
        "open" | "do" | "ls" | "read" | "view" | "update" | "status" | "rollback" => {
            sub.hide(false)
        }
        _ => sub,
    })
}

/// `device` with the verbs that work here: the system's own on Tilo OS,
/// and those whose program this computer has.
fn device_here(device: clap::Command, here: Here) -> clap::Command {
    let device = device.mut_subcommands(|sub| {
        let absent = match sub.get_name() {
            "status" | "rollback" | "update" | "boot" | "repair" => !here.system,
            // The person's devices are reached through this computer's login.
            "add" | "ls" | "revoke" => !login::logged_in(),
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
                "tilo device swap tilo-session",
                "Put this checkout's session onto your slot",
            ),
            ("tilo device shot", "Save a picture of your slot's screen"),
        ]);
    }
    render_examples(&lines)
}

fn agent_here(agent: clap::Command, here: Here) -> clap::Command {
    agent
        .mut_subcommand("models", |sub| sub.hide(!(here.has)(agent::MODELS)))
        // Checking says whether Tilo's server is in the agent, which only a
        // desktop or a computer logged in to the person's workspace has one of.
        .mut_subcommand("check", |sub| {
            sub.hide(!((here.has)(agent::MCP_SERVER) || login::logged_in()))
        })
}

/// The examples under `tilo --help`: real commands, only ones that work here.
fn examples(here: Here) -> String {
    let mut lines: Vec<(&str, &str)> = Vec::new();
    if here.desktop {
        lines.extend([
            ("tilo open calculator", "Open Calculator"),
            ("tilo open notes ~/plan.md", "Open a file in Notes"),
            ("tilo action list", "What this desktop can do"),
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
    if nouns::any_present(nouns::PACKAGE, here.has) {
        lines.push(("tilo package pack", "Make a package of your app"));
    }
    // Hosted workspaces answer from anywhere, with a key.
    lines.extend([
        (
            "tilo do account/keys#create name=\"Visa backend\"",
            "A key for your own backend: you approve it in your Tilo",
        ),
        (
            "tilo do account/workspaces#create name=\"Jane Doe\"",
            "A hosted workspace, one per customer",
        ),
        (
            "tilo do account/keys#mint workspace=jane-doe",
            "An MCP URL and token for an agent in that workspace alone",
        ),
        ("tilo ls notes -w jane-doe", "What a hosted workspace holds"),
    ]);
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

/// Log to standard error as `RUST_LOG` names targets and levels (`debug`,
/// `tilo_mcp::timing=debug`), else at `default`. Read as targets and levels: a
/// filter of regular expressions would link a regex engine into every `tilo`,
/// which `tilo mcp` holds in memory for as long as an agent runs.
pub(crate) fn log_to_stderr(default: tracing_subscriber::filter::LevelFilter) {
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;
    let targets = std::env::var("RUST_LOG")
        .ok()
        .and_then(|filter| filter.parse::<tracing_subscriber::filter::Targets>().ok())
        .unwrap_or_else(|| tracing_subscriber::filter::Targets::new().with_default(default));
    let _ = tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_ansi(false),
        )
        .with(targets)
        .try_init();
}

fn main() -> ExitCode {
    // Started again as the worker that applies documents (`tilo login`'s
    // and `tilo mcp`'s peer): that, before anything else runs.
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        return ExitCode::from(code);
    }
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
    // `tilo mcp`, as an agent's tools start it, runs for as long as the agent does. It is
    // answered without building the whole command tree: that tree's parser is the
    // largest frame the program has, and its strings and arguments are allocations
    // the process would keep holding for nothing. Anything else typed after `mcp`
    // (`--help`, a global flag) is parsed as every command is.
    if noun_of(&args).is_none()
        && matches!(args.get(1..).unwrap_or_default(), [mcp] if mcp == "mcp")
    {
        return mcp::serve().unwrap_or_else(|e| {
            eprintln!("tilo: {e:#}");
            ExitCode::FAILURE
        });
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
                false,
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
    if !matches!(
        cmd,
        DeviceCmd::Health { .. }
            | DeviceCmd::Clean { .. }
            | DeviceCmd::Add { .. }
            | DeviceCmd::Ls { .. }
            | DeviceCmd::Key
            | DeviceCmd::Rename { .. }
            | DeviceCmd::Revoke { .. }
            | DeviceCmd::LocalPerson { .. }
    ) {
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

/// `tilo do`'s option that does the action as the person, not as this computer's agent
/// ([`person`]).
const IN_PERSON: &str = "--in-person";

/// `tilo do`: in the hosted workspace `-w` names, else on this desktop. A
/// `-w` or `--json` after the action lands in its options, and is taken out.
/// Both answer the one typed shape of Tilo's API, said by one renderer
/// ([`tilo_control::front::say`]).
fn run_do(
    reach: api::Reach<'_>,
    mut workspace: Option<String>,
    (action, in_person): (String, bool),
    mut args: Vec<String>,
    spelling: &tilo_control::front::Spelling,
) -> ExitCode {
    if let Some(at) = args
        .iter()
        .position(|arg| arg == "-w" || arg == "--workspace")
        && let Some(named) = args.get(at.saturating_add(1)).cloned()
    {
        workspace = Some(named);
        args.drain(at..=at.saturating_add(1));
    }
    let json = reach.json | args.iter().any(|arg| arg == "--json");
    args.retain(|arg| arg != "--json");
    let in_person = in_person || args.iter().any(|arg| arg == IN_PERSON);
    args.retain(|arg| arg != IN_PERSON);
    let reach = api::Reach { json, ..reach };
    if workspace.is_none() && hosted::on_account(&action) {
        return hosted::act(reach, None, &action, &args);
    }
    let Some(workspace) = workspace else {
        return desktop::run_action(desktop::ActionCmd::Run { action, args }, json, spelling);
    };
    if person::answers(reach.key_file.is_some(), in_person) {
        return person::run(&workspace, &action, &args, json);
    }
    if in_person {
        eprintln!(
            "tilo: {IN_PERSON} acts as the person whose identity this computer holds, with no key named; \
             this computer holds none (tilo login --new-identity), or a key was named"
        );
        return ExitCode::FAILURE;
    }
    hosted::act(reach, Some(&workspace), &action, &args)
}

/// `tilo completions <shell>`: every name this program goes by.
fn completions(shell: Shell) -> Result<ExitCode> {
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

/// The commands that act in hosted workspaces (and `do`, which acts here or
/// there): their answer, or the command back when it is none of them.
fn run_hosted(
    key_file: Option<&std::path::Path>,
    json: bool,
    workspace: Option<String>,
    cmd: Cmd,
    spelling: &tilo_control::front::Spelling,
) -> Result<Result<ExitCode>, Cmd> {
    let needs_workspace = || anyhow::anyhow!("name the workspace: -w <name or id>");
    let reach = api::Reach { key_file, json };
    // Acting as this computer's own device (no key, or the device's own key named), each command
    // is a call to this computer's daemon, which keeps the device's one session.
    let as_device = !(key_file.is_none() && api::desktop_socket().is_some())
        && reach.as_device().unwrap_or(false)
        && api::entered_workspace(api::Reach::default())
            .ok()
            .flatten()
            .is_some()
        && acts_as_the_login(&cmd, workspace.is_some());
    Ok(match cmd {
        Cmd::Do {
            in_person,
            action,
            args,
        } => Ok(run_do(
            reach,
            workspace,
            (action, in_person),
            args,
            spelling,
        )),
        Cmd::Ls { what, cursor } => (workspace.is_some()
            || what.as_deref().is_some_and(hosted::on_account))
        .then(|| {
            hosted::ls(
                reach,
                workspace.as_deref(),
                what.as_deref(),
                cursor.as_deref(),
            )
        })
        .ok_or_else(needs_workspace),
        Cmd::Read { resource, cursor } => (workspace.is_some() || hosted::on_account(&resource))
            .then(|| hosted::read(reach, workspace.as_deref(), &resource, cursor.as_deref()))
            .ok_or_else(needs_workspace),
        Cmd::View { resource, picture } => workspace
            .ok_or_else(needs_workspace)
            .map(|workspace| hosted::view(reach, &workspace, &resource, picture.as_deref())),
        Cmd::Files { cmd } => workspace
            .ok_or_else(needs_workspace)
            .map(|workspace| hosted::files(reach, &workspace, cmd)),
        Cmd::Run { file } => workspace
            .ok_or_else(needs_workspace)
            .map(|workspace| hosted::program(reach, &workspace, &file)),
        Cmd::Invocations { limit, cursor } => workspace
            .ok_or_else(needs_workspace)
            .map(|workspace| hosted::invocations(reach, &workspace, limit, cursor.as_deref())),
        Cmd::Sdk { cmd } => Ok(sdk::generate(reach, workspace.as_deref(), cmd)),
        // Only an explicit `-w` lands a migration in a hosted workspace: without one it comes
        // over this computer's own, through the session `tilo login` holds.
        Cmd::Migrate { json, cmd } if workspace.is_some() => {
            let target = migrate::Target::Hosted {
                key_file: key_file.map(std::path::Path::to_path_buf),
                workspace: workspace.unwrap_or_default(),
            };
            migrate::run_into(cmd, json, &target)
        }
        other => return Err(other),
    })
}

/// Whether `cmd` calls Tilo's API, which without a key is as this computer's login: a `do`
/// in a named workspace or on the account (else it acts on this desktop), an `sdk` or `migrate`
/// naming one, and every other workspace command.
fn acts_as_the_login(cmd: &Cmd, named: bool) -> bool {
    match cmd {
        Cmd::Do { action, args, .. } => {
            named
                || hosted::on_account(action)
                || args.iter().any(|arg| arg == "-w" || arg == "--workspace")
        }
        // A new key is asked of the API server, never of this computer's session: the session
        // only names the workspace a key is to reach, when `-w` names it by its name.
        Cmd::Sdk { .. } | Cmd::Migrate { .. } => named,
        Cmd::View { .. } => true,
        other => acts_in_a_workspace(other),
    }
}

/// Every command as a page of documentation (`tilo help-markdown`).
fn help_markdown() -> Result<ExitCode> {
    print!("{}", docs::markdown().map_err(anyhow::Error::msg)?);
    Ok(ExitCode::SUCCESS)
}

/// The line that says which workspace `-w` meant when nobody named one, so
/// an answer is never read without knowing where it came from. Standard
/// error, so a `--json` answer stays the typed answer alone.
fn entered_workspace_note(name: &str) -> String {
    format!("acting in the workspace {name:?} (-w <name or id> names another)")
}

/// Which commands act in a workspace, so `-w` means something to them. `do` is
/// not one: without `-w` it acts on this desktop, which is what a person
/// typing it there means. An `ls` or `read` of the account's (`account/keys`) acts in none.
fn acts_in_a_workspace(cmd: &Cmd) -> bool {
    matches!(
        cmd,
        Cmd::Files { .. } | Cmd::Run { .. } | Cmd::Invocations { .. }
    ) || matches!(cmd, Cmd::Ls { what, .. } if !what.as_deref().is_some_and(hosted::on_account))
        || matches!(cmd, Cmd::Read { resource, .. } if !hosted::on_account(resource))
}

/// `-w` when it was not given: the workspace this computer's `tilo login`
/// entered, which is the one its device is in, said as it is used. A command
/// that acts on no workspace never reads the login: `sdk` writes the apps the
/// checkout commits unless `-w` names a workspace, so it does not either.
fn workspace_or_entered(cli: &Cli) -> Result<Option<String>> {
    if cli.workspace.is_some() || !acts_in_a_workspace(&cli.cmd) {
        return Ok(cli.workspace.clone());
    }
    let entered = api::entered_workspace(api::Reach {
        key_file: cli.key_file.as_deref(),
        json: false,
    })?;
    Ok(entered.map(|(workspace, name)| {
        eprintln!("{}", entered_workspace_note(&name));
        workspace
    }))
}

/// The recovery words `tilo login --recovery-key` is given on standard input: asked for on a
/// terminal, read to the end from a pipe. Never an argument, which other accounts and the shell's
/// history would see.
fn recovery_words() -> Result<zeroize::Zeroizing<String>> {
    use std::io::{IsTerminal as _, Read as _};
    let stdin = std::io::stdin();
    let words = if stdin.is_terminal() {
        let mut tty = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .context("opening the terminal to read the recovery words")?;
        migrate::read_hidden(&mut tty, "Type your 24 recovery words, then press Enter: ")?
    } else {
        let mut words = zeroize::Zeroizing::new(String::new());
        stdin
            .lock()
            .take(64 * 1024)
            .read_to_string(&mut words)
            .context("reading the recovery words from standard input")?;
        words
    };
    if words.trim().is_empty() {
        bail!(
            "`tilo login --recovery-key` reads your 24 recovery words from standard input, and got none."
        );
    }
    Ok(words)
}

/// `tilo login` with no ticket, or with an invitation link. `None` for every other command.
fn login_without_a_tab(cli: &Cli) -> Option<Result<ExitCode>> {
    if let Cmd::Login {
        server: Some(host), ..
    } = &cli.cmd
    {
        return Some(login::login_at_server(host));
    }
    if let Cmd::Login {
        ticket: None,
        new_identity: false,
        recovery_key,
        ..
    } = &cli.cmd
    {
        if *recovery_key {
            return Some(
                recovery_words()
                    .and_then(|words| identity::restore(&words))
                    .map(|person| {
                        eprintln!(
                            "This computer holds {} Tilo identity again. Run `tilo device add` on \
                             a device of yours that is logged in, and `tilo login` here with the \
                             ticket it prints.",
                            person.whose()
                        );
                        ExitCode::SUCCESS
                    }),
            );
        }
        return Some(Err(anyhow!(
            "Give the ticket one of your devices printed, like `tilo login tilo1…`: run \
             `tilo device add` on a device of yours to get one."
        )));
    }
    if let Cmd::Login {
        ticket: Some(link),
        new_identity: false,
        name,
        agent,
        ..
    } = &cli.cmd
        && link.contains("invite=")
    {
        return Some(login::login_invited(
            link,
            name.as_deref(),
            agent.as_deref(),
        ));
    }
    None
}

/// `tilo status` on a computer with a system provider: the peer's state when logged in, then
/// the deployment's.
fn system_status(json: bool) -> Result<ExitCode> {
    if login::logged_in() && !json {
        peer_status(false);
    }
    require_system_provider()?;
    let code = deployment_status(json)?;
    if !json {
        print_cleanup_status();
    }
    Ok(code)
}

/// `tilo login -`: the ticket from standard input, so a script's ticket never shows in the
/// process list.
fn ticket_from_stdin(cmd: &mut Cmd) -> Result<()> {
    if let Cmd::Login {
        ticket: Some(ticket),
        ..
    } = cmd
        && ticket == "-"
    {
        let mut read = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin().lock(), &mut read)
            .context("reading the ticket from standard input")?;
        *ticket = read.split_whitespace().collect();
    }
    Ok(())
}

fn run(mut cli: Cli) -> Result<ExitCode> {
    ticket_from_stdin(&mut cli.cmd)?;
    if cli.key_file.is_none() {
        cli.key_file = std::env::var_os(api::KEY_FILE_ENV).map(std::path::PathBuf::from);
    }
    let spelling = tilo_control::front::Spelling::tilo();
    if let Some(logged_in) = login_without_a_tab(&cli) {
        return logged_in;
    }
    let workspace = workspace_or_entered(&cli)?;
    let cmd = match run_hosted(
        cli.key_file.as_deref(),
        cli.json,
        workspace,
        cli.cmd,
        &spelling,
    ) {
        Ok(answered) => return answered,
        Err(cmd) => cmd,
    };
    match cmd {
        Cmd::Do { .. }
        | Cmd::Ls { .. }
        | Cmd::Read { .. }
        | Cmd::View { .. }
        | Cmd::Files { .. }
        | Cmd::Run { .. }
        | Cmd::Invocations { .. }
        | Cmd::Sdk { .. } => bail!("run_hosted takes the hosted commands"),
        Cmd::Open { app, items } => Ok(desktop::run_open(&app, &items, &spelling)),
        Cmd::Action { cmd } => Ok(desktop::run_action(cmd, cli.json, &spelling)),
        Cmd::Server { cmd } => server::serve(cmd),
        Cmd::Machine { cmd } => machine::serve(cmd),
        Cmd::Complete { what, prefix } => Ok(desktop::complete(
            what,
            prefix.as_deref().unwrap_or_default(),
        )),
        Cmd::Completions { shell } => completions(shell),
        Cmd::Device { cmd } => run_system_command(cmd),
        Cmd::HelpMarkdown => help_markdown(),
        Cmd::Agent { cmd } => agent::run(cmd),
        Cmd::Migrate { json, cmd } => migrate::run(cmd, json),
        Cmd::Login {
            new_identity: true,
            name,
            json,
            ..
        } => identity::new_identity(name.as_deref(), json),
        Cmd::Login {
            ticket,
            agent,
            label,
            ..
        } => login::login(
            ticket.as_deref().unwrap_or_default(),
            agent.as_deref(),
            label.as_deref(),
        ),
        Cmd::Mcp => mcp::serve(),
        Cmd::Folders { json, cmd } => folders::run(cmd.unwrap_or(folders::FoldersCmd::List), json),
        Cmd::Settings { cmd } => settings::run(cmd).map(|()| ExitCode::SUCCESS),
        Cmd::Package => bail!("package needs a verb; `tilo package --help` lists them"),
        Cmd::App { args } if args.first().is_some_and(|verb| verb == "open") => {
            app_open(args.get(1..).unwrap_or_default(), &spelling)
        }
        Cmd::App { args } => run_app(&args, "tilo app"),
        Cmd::Store { args } => run_app(&args, "tilo store"),
        // Read-only, so it never needs to become anything. A person diagnosing a machine should not
        // have to authenticate to be told what is on it.
        Cmd::Status { json } if !system_available() && login::logged_in() => Ok(peer_status(json)),
        Cmd::Status { json } => system_status(json),
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
        DeviceCmd::Status { json } => run(Cli::of(Cmd::Status { json })),
        DeviceCmd::Rollback { yes } => run(Cli::of(Cmd::Rollback { yes })),
        DeviceCmd::Upgrade {
            to,
            yes,
            reboot,
            check,
        } => os_upgrade(to.as_deref(), yes, reboot, check),
        DeviceCmd::Boot { entry } => run(Cli::of(Cmd::Boot { entry })),
        DeviceCmd::Repair => {
            print_repair_help();
            Ok(ExitCode::SUCCESS)
        }
        DeviceCmd::Health { gpu: _, json } => health::run_gpu(json),
        DeviceCmd::Add {
            ticket,
            json,
            no_wait,
            plan,
            server,
        } => device::add(ticket.as_deref(), json, no_wait, plan.as_deref(), server),
        DeviceCmd::Ls { json } => device::list(json),
        DeviceCmd::Key => device::key(),
        DeviceCmd::Rename {
            device: which,
            name,
        } => device::rename(&which, &name),
        DeviceCmd::Revoke {
            device: which,
            reason,
            yes,
        } => device::revoke(&which, reason, yes),
        DeviceCmd::LocalPerson {
            identity_root,
            passwd,
        } => Ok(device::local_person(&identity_root, passwd.as_deref())),
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

/// `tilo status` on a computer logged in as one of the person's devices:
/// who it is, which workspaces it shows where, and whether they are in sync.
fn peer_status(json: bool) -> ExitCode {
    let status = match folders::sync() {
        Ok(status) => status,
        Err(error) => {
            if json {
                println!("{}", serde_json::json!({"running": false}));
            } else {
                eprintln!("{error:#}");
            }
            return ExitCode::from(1);
        }
    };
    if json {
        println!("{status}");
        return ExitCode::SUCCESS;
    }
    print!("{}", folders::describe(&status));
    ExitCode::SUCCESS
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

// Workspaces the binary's own tests hold keep their documents in workers this test executable
// serves, as `main` does through `dispatch_early`.
#[cfg(test)]
tilo_automerge_isolate::test_worker!();

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, FromArgMatches};

    /// The line a command prints when `-w` was not given says which
    /// workspace it acted in, and how to name another.
    #[test]
    fn an_unnamed_workspace_is_said() {
        assert_eq!(
            entered_workspace_note("Studio"),
            "acting in the workspace \"Studio\" (-w <name or id> names another)"
        );
    }

    /// `sdk gen` writes the types of the apps the checkout commits unless `-w` names a
    /// workspace, so it reads no login and needs no host.
    #[test]
    fn sdk_gen_needs_no_login() {
        let command = Cmd::Sdk {
            cmd: sdk::SdkCmd::Gen {
                lang: sdk::Lang::Python,
                out: None,
                check: true,
            },
        };
        assert!(!acts_in_a_workspace(&command));
    }

    /// A command that takes `-w` acts in the workspace this computer logged
    /// into when nobody names one; `-w` still names another, and `do` without
    /// it stays on this desktop.
    #[test]
    fn an_unnamed_workspace_is_the_one_this_computer_entered() {
        const ENTERED: &str = "6f1c2b9e-1c1f-4f0e-9a51-3c4d5e6f7a8b";

        /// A command line with `cmd`, `-w` and `--key-file` as given.
        fn with(cmd: Cmd, workspace: Option<&str>, key_file: Option<&str>) -> Cli {
            Cli {
                workspace: workspace.map(str::to_owned),
                key_file: key_file.map(std::path::PathBuf::from),
                json: false,
                cmd,
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[4; 32]);
        let key = ed25519_dalek::SigningKey::from_bytes(&[5; 32]);
        let cert = crate::login::certified_for_test(&root, &key, "Claude Code on build-box");
        let device = crate::login::Device {
            site: "https://tilo.sh/".into(),
            host_origin: "https://eu1.tilo.test".into(),
            workspace: ENTERED.into(),
            workspace_name: Some("Studio".into()),
            agent: "Claude Code".into(),
            server_cert: cert.clone(),
            cert,
            server_address: None,
        };
        let mut env = tilo_test_env::ScopedEnv::new();
        crate::login::keep_for_test(&mut env, dir.path(), &key, &device);
        env.remove(api::KEY_FILE_ENV);

        let members = || Cmd::Ls {
            what: None,
            cursor: None,
        };
        assert_eq!(
            workspace_or_entered(&with(members(), None, None)).unwrap(),
            Some(ENTERED.to_owned()),
            "the workspace the device entered is the one acted in"
        );
        assert_eq!(
            workspace_or_entered(&with(members(), Some("jane-doe"), None)).unwrap(),
            Some("jane-doe".to_owned()),
            "`-w` names another"
        );
        let act = Cmd::Do {
            in_person: false,
            action: "mute".into(),
            args: Vec::new(),
        };
        assert_eq!(
            workspace_or_entered(&with(act, None, None)).unwrap(),
            None,
            "`do` without `-w` acts on this desktop"
        );
        assert_eq!(
            workspace_or_entered(&with(members(), None, Some("visa.json"))).unwrap(),
            None,
            "a key file is someone else's key: it names no workspace"
        );
    }

    tilo_test_env::scenario! {
        fn the_command_takes_json_anywhere_and_shows_what_it_does;

        phase json_is_taken_after_a_verb_and_its_arguments;
        {
            // `tilo --json ls` worked and `tilo ls --json` did not: the answer's shape is the
            // command's, wherever it is typed.
            for args in [
                &["tilo", "--json", "ls", "account/keys"][..],
                &["tilo", "files", "put", "a.json", "--json"][..],
                &["tilo", "ls", "notes", "-w", "jane-doe", "--json"][..],
                &["tilo", "read", "notes/Todo", "-w", "jane-doe", "--json"][..],
                &["tilo", "run", "visa.tl", "-w", "jane-doe", "--json"][..],
                &["tilo", "invocations", "-w", "jane-doe", "--limit", "5", "--json"][..],
            ] {
                let cli = Cli::try_parse_from(args)
                    .unwrap_or_else(|error| panic!("tilo {args:?}: {error}"));
                assert!(cli.json, "tilo {args:?} names JSON");
            }
            // A `do` takes its options after the action, `--json` among them:
            // wherever the parser puts it, the call asks for JSON.
            let cli = Cli::try_parse_from(["tilo", "-w", "jane-doe", "do", "so.tilo.tasks#new_task", "--json"])
                .expect("do takes --json after the action");
            let Cmd::Do { args, .. } = cli.cmd else {
                panic!("do");
            };
            assert!(
                cli.json || args.iter().any(|arg| arg == "--json"),
                "do asks for JSON: json={} args={args:?}",
                cli.json
            );

            // A command with a meaning of its own for --json keeps it: the
            // system's record, the health check, a new identity.
            let cli = Cli::try_parse_from(["tilo", "status", "--json"]).expect("status --json");
            assert!(
                matches!(cli.cmd, Cmd::Status { json: true }),
                "status --json still asks for the system's record"
            );
            let cli = Cli::try_parse_from(["tilo", "login", "--new-identity", "--json"])
                .expect("login --json");
            assert!(
                matches!(cli.cmd, Cmd::Login { json: true, .. }),
                "login --json still asks for the identity as one line"
            );
        }

        phase the_help_lists_do_ls_and_read;
        {
            let everything = Here {
                desktop: true,
                apps: true,
                system: true,
                has: |_| true,
            };
            let command = command_here(everything);
            for name in ["do", "ls", "read", "view", "files"] {
                let sub = command
                    .get_subcommands()
                    .find(|sub| sub.get_name() == name)
                    .unwrap_or_else(|| panic!("{name} is in the command tree"));
                assert!(!sub.is_hide_set(), "{name} is listed by --help");
            }
        }
    }

    tilo_test_env::scenario! {
        fn a_failed_deployment_keeps_the_units_failure_and_its_journal;

        phase failed_deployment_preserves_the_job_reason_journal_and_exit_status;
        {
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

        phase unreadable_journal_is_reported_without_replacing_the_unit_failure;
        {
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
    }

    tilo_test_env::scenario! {
        fn providers_are_found_and_an_update_runs_the_ones_there_are;

        phase providers_are_discovered_forwarded_and_overridable;
        {
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

        phase umbrella_update_runs_available_providers_and_fails_on_any_failure;
        {
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

    tilo_test_env::scenario! {
        fn help_lists_what_works_where_it_is_asked;

        phase help_on_a_tilo_desktop_lists_what_works_there_verbatim;
        // The help is about the person: on a Tilo laptop in a desktop session it
        // lists exactly what works there, with real examples and no narration
        // about providers or other distributions.
        {
            assert_eq!(
                listed(EVERYWHERE),
                [
                    "app",
                    "action",
                    "do",
                    "ls",
                    "read",
                    "view",
                    "workspace",
                    "keys",
                    "files",
                    "run",
                    "invocations",
                    "sdk",
                    "device",
                    "agent",
                    "package"
                ]
            );
            // Each listed noun has its own line saying what it does, under a title a person reads,
            // and the help shows examples rather than narrating the program.
            let help = command_here(EVERYWHERE).render_help().to_string();
            assert!(
                help.starts_with("Use apps and actions, and look after your devices\n"),
                "{help}"
            );
            for name in listed(EVERYWHERE) {
                assert!(
                    help.lines()
                        .any(|line| line.trim_start().starts_with(&format!("{name} "))),
                    "{name} is described in:\n{help}"
                );
            }
            assert!(help.contains("\nExamples:\n"), "{help}");
            // `do` names commands that exist: this desktop's own listing,
            // and the one a hosted workspace answers.
            let mut everywhere = command_here(EVERYWHERE);
            let doing = everywhere
                .find_subcommand_mut("do")
                .expect("do")
                .render_help()
                .to_string();
            assert!(doing.contains("`tilo action list`"), "{doing}");
            assert!(doing.contains("`tilo ls -w <workspace>`"), "{doing}");
            assert!(!doing.contains("tilo actions"), "{doing}");
        }

        phase help_elsewhere_leaves_out_what_cannot_work;
        // On another distribution, what can't work there is simply absent: no
        // desktop commands without a desktop, no `system` off Tilo OS. Typing one
        // anyway still parses, and answers with what it needs.
        {
            let elsewhere = Here {
                desktop: false,
                apps: true,
                system: false,
                has: |_| false,
            };
            assert_eq!(
                listed(elsewhere),
                [
                    "app", "do", "ls", "read", "view", "files", "run", "invocations", "sdk", "login", "folders", "migrate"
                ]
            );
            let help = command_here(elsewhere).render_help().to_string();
            // `do` stays: acting in a hosted workspace (-w) needs no desktop.
            for absent in ["tilo open", "tilo action list", "device update", "status"] {
                assert!(!help.contains(absent), "{absent} in:\n{help}");
            }
            let nothing = Here {
                desktop: false,
                apps: false,
                system: false,
                has: |_| false,
            };
            // A bare computer (the tilo.sh installer's) logs in to the person's
            // workspace, and reaches hosted workspaces with a key.
            assert_eq!(
                listed(nothing),
                [
                    "do", "ls", "read", "view", "files", "run", "invocations", "sdk", "login", "folders", "migrate"
                ]
            );
            let bare = command_here(nothing).render_help().to_string();
            assert!(bare.contains("tilo do account/keys#create"), "{bare}");
            assert!(!bare.contains("tilo open"), "{bare}");
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
    }

    tilo_test_env::scenario! {
        fn the_command_line_parses_under_every_name_it_goes_by;

        phase the_command_tree_parses_namespaces_and_completes_both_argv0s;
        {
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

        phase the_cleanup_units_command_lines_parse;
        {
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

        phase open_action_and_do_parse_under_every_name;
        {
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
                Cmd::Do { ref action, ref args, .. } if action == "rename-note" && args == &["title=Groceries", "--force"]
            ));
            let in_person = Cli::try_parse_from([
                "tilo",
                "do",
                "--in-person",
                "chat/Messages#answer-question",
                "answer=allow",
            ])
            .expect("do --in-person parses");
            assert!(matches!(
                in_person.cmd,
                Cmd::Do { in_person: true, ref action, .. } if action == "chat/Messages#answer-question"
            ));
            let list = Cli::try_parse_from(["tilo", "action", "list", "volume"]).expect("action list parses");
            assert!(matches!(
                list.cmd,
                Cmd::Action {
                    cmd: desktop::ActionCmd::List { ref filter }
                } if filter == &["volume"]
            ));
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

        phase handed_on_verbs_keep_their_arguments_and_show_only_where_they_work;
        // `device` and `package` verbs another program owns parse with every
        // argument kept for it, `--help` included, and leave help where the
        // program is missing.
        {
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
                .try_get_matches_from(["tilo", "device", "swap", "tilo-session", "--target", "lab"])
                .expect("device swap parses");
            let (_, device) = matches.subcommand().expect("a noun");
            let (verb, args) = nouns::picked(device, nouns::DEVICE).expect("a handed-on verb");
            assert_eq!((verb.program, verb.prefix), ("tilo-vm", &["swap"][..]));
            assert_eq!(args, ["tilo-session", "--target", "lab"]);
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

        phase completions_ask_the_desktop_for_app_and_action_names;
        {
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
}
