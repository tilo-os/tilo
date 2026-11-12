//! `tilo agent`: the agents you run, and the models they run on.
//!
//! `connect` registers this desktop's agent door with an agent tool you run
//! in a terminal, so it can act on the desktop through the same actions a
//! person has — or, with `--device`, the agent door of another of your own
//! devices, reached from this one. `check` says whether Tilo's own MCP server
//! is in the agent running it, and answers, which is what `tilo login` leaves
//! that agent to confirm: it is the only one that can, since it knows when
//! its own tools load. `models` is the model list Settings shows, through
//! the program that keeps it.
//!
//! Tilo's server reaches an agent tool one of two ways ([`Tool`]): through
//! the tool's own command where it documents one, or by writing the tool's
//! own configuration file, merging with the servers it holds and keeping
//! every other byte of that file as the person left it. A tool Tilo cannot
//! place the server in, or one it does not know, is an agent's own work:
//! `tilo login` prints the server for the agent to add where it keeps its
//! servers, and `check` reads back what it wrote.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use serde_json::{Value, json};

use crate::nouns;

/// The agent door an agent tool starts.
pub const DOOR: &str = "tilo-runtime-mcp";
/// The program that keeps the model list.
pub const MODELS: &str = "tilo-model-manager";
/// The site's MCP server over HTTP, for an agent whose own configuration
/// Tilo cannot write: the same workspaces, without a program on this
/// computer holding the device.
pub const MCP_URL: &str = "https://mcp.tilo.sh";
/// The server name every agent tool knows Tilo's tools by.
pub const SERVER: &str = "tilo";

#[derive(Subcommand)]
pub enum AgentCmd {
    /// Let an agent you run in a terminal act on this desktop, or on another
    /// of your devices.
    #[command(
        after_help = "Examples:\n  tilo agent connect claude\n  tilo agent connect codex\n  \
                      tilo agent connect claude --device build-box\n\n\
                      The agent then reaches this desktop's actions from any terminal in your login. \
                      With --device it reaches that device's instead, as an agent of yours from \
                      this device: that device asks you there, unless you let agents from your \
                      other devices in on it (Settings, \"Agents from your other devices\"). The \
                      agent's tilo_devices tool lists your devices and which answer now."
    )]
    Connect {
        /// The agent tool: claude or codex.
        tool: Tool,
        /// Another of your devices to act on, by its name or endpoint id.
        #[arg(long)]
        device: Option<String>,
        /// The workspace on that device to work in, by its id; the one in
        /// front there when not given.
        #[arg(long, requires = "device", value_parser = workspace_id)]
        workspace: Option<String>,
    },
    /// Say whether Tilo's MCP server is in the agent running this, and
    /// answers: what `tilo login` leaves that agent to confirm.
    #[command(
        after_help = "Examples:\n  tilo agent check\n  tilo agent check --agent cursor\n  \
                      tilo agent check --config ~/.config/my-agent/mcp.json\n\n\
                      With nothing named it looks at the agent running it: which one it is, where \
                      that agent keeps its MCP servers, whether Tilo's server `tilo` is there, and \
                      whether what that would start answers. An agent whose configuration Tilo \
                      does not know hands its file over with --config, and Tilo says whether it \
                      holds the server. The answer is for the agent to read: a line that failed \
                      says how to add the server."
    )]
    Check {
        /// The agent to check for, by name or program; the one running this
        /// when not given.
        #[arg(long)]
        agent: Option<String>,
        /// A configuration file of the agent's own, to look for Tilo's
        /// server in.
        #[arg(long, value_name = "PATH")]
        config: Option<PathBuf>,
    },
    /// The models agents run on, local or hosted.
    Models {
        #[command(subcommand)]
        cmd: ModelsCmd,
    },
}

/// An agent tool Tilo adds its tools to: the ones people run, as each of
/// them names itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Tool {
    /// Claude Code.
    Claude,
    /// Codex.
    Codex,
    /// Gemini CLI.
    #[value(skip)]
    Gemini,
    /// Cursor, the CLI and the editor.
    #[value(skip)]
    Cursor,
    /// VS Code, whose Copilot agent mode runs MCP servers.
    #[value(skip)]
    VsCode,
    /// Windsurf.
    #[value(skip)]
    Windsurf,
    /// Claude Desktop, the app.
    #[value(skip)]
    ClaudeDesktop,
    /// `OpenCode`.
    #[value(skip)]
    OpenCode,
    /// Amp.
    #[value(skip)]
    Amp,
    /// Goose.
    #[value(skip)]
    Goose,
    /// Zed.
    #[value(skip)]
    Zed,
    /// Cline, the VS Code extension.
    #[value(skip)]
    Cline,
    /// Roo Code, the VS Code extension.
    #[value(skip)]
    Roo,
}

impl Tool {
    /// Every tool Tilo knows how to add itself to.
    pub const ALL: &'static [Self] = &[
        Self::Claude,
        Self::Codex,
        Self::Gemini,
        Self::Cursor,
        Self::VsCode,
        Self::Windsurf,
        Self::ClaudeDesktop,
        Self::OpenCode,
        Self::Amp,
        Self::Goose,
        Self::Zed,
        Self::Cline,
        Self::Roo,
    ];

    /// As the person's workspace names it.
    pub fn title(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::Gemini => "Gemini CLI",
            Self::Cursor => "Cursor",
            Self::VsCode => "VS Code",
            Self::Windsurf => "Windsurf",
            Self::ClaudeDesktop => "Claude Desktop",
            Self::OpenCode => "OpenCode",
            Self::Amp => "Amp",
            Self::Goose => "Goose",
            Self::Zed => "Zed",
            Self::Cline => "Cline",
            Self::Roo => "Roo Code",
        }
    }

    /// How the person starts it again with the conversation kept, when it
    /// reads its servers only as it starts. `None` when it reads them while
    /// it runs, so a server added now is there already.
    pub fn restart(self) -> Option<&'static str> {
        match self {
            Self::Claude => Some("/exit, then `claude --continue`"),
            Self::Codex => Some("/quit, then `codex resume --last`"),
            // Gemini CLI leaves user-level servers out of a folder it has
            // not been told to trust.
            Self::Gemini => Some(
                "/quit, then `gemini --resume latest` in a folder you trust: Gemini leaves \
                 user-level servers out of an untrusted one",
            ),
            Self::OpenCode => Some("/exit, then `opencode --continue`"),
            Self::VsCode => Some("reload the window: Command Palette, Developer: Reload Window"),
            Self::Windsurf => Some("open Cascade again; Windsurf keeps the conversation"),
            Self::ClaudeDesktop => Some("quit Claude and open it again; the conversation stays"),
            Self::Zed => Some("reload the window: Command Palette, workspace: reload"),
            Self::Cline => Some("open Cline's MCP Servers panel and press Restart on tilo"),
            Self::Roo => Some("open Roo Code's MCP Servers panel and press Restart on tilo"),
            Self::Goose => Some("start a session again (`goose session --resume` keeps this one)"),
            // These read their servers again as they run: Amp has the
            // `reload_mcp` tool, and Cursor's CLI reads `mcp.json` as it
            // starts and takes the server from it.
            Self::Amp | Self::Cursor => None,
        }
    }

    /// Whether it is an editor whose terminal runs other agents as well:
    /// then the agent Tilo added its tools to may not be the one the person
    /// gave the prompt to.
    pub fn hosts_agents(self) -> bool {
        matches!(
            self,
            Self::Cursor | Self::VsCode | Self::Windsurf | Self::Zed | Self::Cline | Self::Roo
        )
    }

    /// What the agent does before it calls Tilo's tools, for a tool that
    /// holds servers back until it is told to let one run.
    pub fn approval(self) -> Option<&'static str> {
        match self {
            Self::Cursor => Some("run `agent mcp enable tilo`"),
            _ => None,
        }
    }

    /// The programs its process runs under, as a process tree names them.
    pub(crate) fn programs(self) -> &'static [&'static str] {
        match self {
            Self::Claude => &["claude", "claude-code"],
            Self::Codex => &["codex"],
            Self::Gemini => &["gemini", "gemini-cli"],
            Self::Cursor => &["cursor-agent", "cursor"],
            Self::VsCode => &["code", "code-insiders", "codium"],
            Self::Windsurf => &["windsurf"],
            Self::ClaudeDesktop => &["claude"],
            Self::OpenCode => &["opencode"],
            Self::Amp => &["amp"],
            Self::Goose => &["goose"],
            Self::Zed => &["zed"],
            Self::Cline => &["cline"],
            Self::Roo => &["roo-code"],
        }
    }

    /// The tool's own program, for the ones that add servers themselves.
    fn cli(self) -> Option<&'static str> {
        Some(match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::VsCode => "code",
            Self::OpenCode => "opencode",
            Self::Amp => "amp",
            Self::Cline => "cline",
            _ => return None,
        })
    }

    /// The tool's own command that registers the stdio server `name` running
    /// `program args`, for every directory of this login.
    fn cli_add(self, name: &str, program: &Path, args: &[&str]) -> Option<Vec<OsString>> {
        let mut add: Vec<OsString> = match self {
            Self::Claude | Self::Gemini => ["mcp", "add", "--scope", "user"]
                .map(OsString::from)
                .to_vec(),
            // Each takes the server's name, then the command after a `--`;
            // the flags below say where OpenCode keeps it and that Cline
            // skips its wizard.
            Self::Codex | Self::Amp | Self::OpenCode | Self::Cline => {
                ["mcp", "add"].map(OsString::from).to_vec()
            }
            // VS Code takes the server itself, as JSON, into this window's
            // user settings.
            Self::VsCode => {
                let server = json!({
                    "name": name,
                    "type": "stdio",
                    "command": program.to_string_lossy(),
                    "args": args,
                });
                return Some(vec![
                    "--add-mcp".into(),
                    OsString::from(serde_json::to_string(&server).ok()?),
                ]);
            }
            _ => return None,
        };
        add.push(name.into());
        // OpenCode adds the server to every project's configuration with
        // `--global`; Cline adds it without its wizard.
        match self {
            Self::OpenCode => add.push("--global".into()),
            Self::Cline => add.push("--yes".into()),
            _ => {}
        }
        // Gemini CLI takes the command and its arguments positionally; the
        // rest take them after a `--`.
        if self != Self::Gemini {
            add.push("--".into());
        }
        add.push(program.as_os_str().to_owned());
        add.extend(args.iter().map(OsString::from));
        Some(add)
    }

    /// The tool's own command that removes the server `name`.
    fn cli_remove(self, name: &str) -> Option<Vec<OsString>> {
        let mut remove: Vec<OsString> = match self {
            Self::Claude | Self::Gemini => ["mcp", "remove", "--scope", "user"]
                .map(OsString::from)
                .to_vec(),
            Self::Codex | Self::Amp | Self::OpenCode | Self::Cline => {
                ["mcp", "remove"].map(OsString::from).to_vec()
            }
            _ => return None,
        };
        remove.push(name.into());
        Some(remove)
    }

    /// The tool's own command that registers `door` as the server `tilo`,
    /// or, for another device, as `tilo-<device>` carried to that device's
    /// door.
    fn registration(self, door: &Path, remote: Option<&Remote>) -> Vec<OsString> {
        let name = remote.map_or_else(|| SERVER.to_owned(), Remote::server_name);
        let mut door_args = vec!["--connect"];
        if let Some(remote) = remote {
            door_args.extend(["--device", remote.device.as_str()]);
            if let Some(workspace) = &remote.workspace {
                door_args.extend(["--workspace", workspace.as_str()]);
            }
        }
        // `tilo agent connect` offers only the tools whose own command adds
        // servers, so one of them is what this is.
        self.cli_add(&name, door, &door_args).unwrap_or_default()
    }

    /// The files this tool keeps its MCP servers in, the one it reads first.
    fn files(self, runner: &dyn Runner) -> Vec<PathBuf> {
        let home = || runner.home();
        let config = || config_dir(runner);
        match self {
            Self::Claude => paths(home().map(|home| home.join(".claude.json"))),
            Self::Codex => paths(home().map(|home| home.join(".codex/config.toml"))),
            Self::Gemini => paths(home().map(|home| home.join(".gemini/settings.json"))),
            // Cursor's CLI reads the editor's own file.
            Self::Cursor => paths(home().map(|home| home.join(".cursor/mcp.json"))),
            Self::VsCode => paths(vscode_user(runner).map(|dir| dir.join("mcp.json"))),
            Self::Windsurf => {
                let mut files = Vec::new();
                if let Some(home) = home() {
                    files.push(home.join(".codeium/windsurf/mcp_config.json"));
                }
                if let Some(config) = config() {
                    files.push(config.join("devin/mcp_config.json"));
                }
                files
            }
            Self::ClaudeDesktop => paths(claude_desktop(runner)),
            Self::OpenCode => config().map_or_else(Vec::new, |dir| {
                let dir = dir.join("opencode");
                vec![dir.join("opencode.jsonc"), dir.join("opencode.json")]
            }),
            Self::Amp => paths(config().map(|dir| dir.join("amp/settings.json"))),
            Self::Goose => paths(config().map(|dir| dir.join("goose/config.yaml"))),
            Self::Zed => paths(zed_settings(runner)),
            Self::Cline => {
                let mut files = Vec::new();
                if let Some(dir) = vscode_user(runner) {
                    files.push(dir.join(
                        "globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json",
                    ));
                }
                // The CLI keeps its own account of the same servers.
                if let Some(home) = home() {
                    files.push(home.join(".cline/data/settings/cline_mcp_settings.json"));
                }
                files
            }
            Self::Roo => paths(vscode_user(runner).map(|dir| {
                dir.join("globalStorage/rooveterinaryinc.roo-cline/settings/mcp_settings.json")
            })),
        }
    }

    /// The keys above the servers in this tool's file: `mcpServers` for most,
    /// `servers` for VS Code, `context_servers` for Zed, `OpenCode`'s `mcp`, and
    /// Amp's own compound key.
    fn section(self) -> &'static [&'static str] {
        match self {
            Self::VsCode => &["servers"],
            Self::Zed => &["context_servers"],
            Self::OpenCode => &["mcp", "servers"],
            Self::Amp => &["amp.mcpServers"],
            _ => &["mcpServers"],
        }
    }

    /// The entry that runs `program args` as a stdio server named `name`, as
    /// this tool writes it.
    fn entry(self, program: &Path, args: &[&str]) -> Value {
        let mut runs = vec![program.to_string_lossy().into_owned()];
        runs.extend(args.iter().map(|arg| (*arg).to_owned()));
        match self {
            // VS Code says how a server is reached.
            Self::VsCode => {
                json!({"type": "stdio", "command": program.to_string_lossy(), "args": args})
            }
            // Zed tells a server of its own from one of the gallery's.
            Self::Zed => json!({"command": program.to_string_lossy(), "args": args, "env": {}}),
            // OpenCode takes the program and its arguments as one list.
            Self::OpenCode => json!({"type": "local", "command": runs}),
            // Cline and Roo keep whether they may run it beside the command.
            Self::Cline | Self::Roo => json!({"command": program.to_string_lossy(), "args": args,
                                             "disabled": false, "autoApprove": []}),
            _ => json!({"command": program.to_string_lossy(), "args": args}),
        }
    }

    /// `text` — this tool's own configuration — with the stdio server `name`
    /// running `program args` set in it, adding the keys on the way that are
    /// not there and writing every other byte back as it was.
    fn write_into(
        self,
        text: &str,
        name: &str,
        program: &Path,
        args: &[&str],
    ) -> Result<String, String> {
        if text.trim().is_empty() {
            return Ok(fresh(self.section(), name, &self.entry(program, args)));
        }
        match self {
            // Codex keeps its servers in TOML and Goose its extensions in
            // YAML: each names a server its own way.
            Self::Codex => with_toml_server(text, name, program, args),
            Self::Goose => with_goose_extension(text, name, program, args),
            _ => with_member(text, self.section(), name, &self.entry(program, args)),
        }
    }
}

/// `path` as a list, when there is one.
fn paths(path: Option<PathBuf>) -> Vec<PathBuf> {
    path.into_iter().collect()
}

/// What a workspace id looks like, as `--workspace` takes it: a UUID in its
/// hyphenated form.
fn workspace_id(text: &str) -> Result<String, String> {
    let groups: Vec<&str> = text.split('-').collect();
    let lengths: Vec<usize> = groups.iter().map(|group| group.len()).collect();
    if lengths == [8, 4, 4, 4, 12]
        && groups
            .iter()
            .all(|group| group.chars().all(|character| character.is_ascii_hexdigit()))
    {
        Ok(text.to_ascii_lowercase())
    } else {
        Err(format!("{text} is not a workspace id"))
    }
}

/// Another of the person's devices an agent is connected to.
pub struct Remote {
    /// The device, by its name or endpoint id.
    device: String,
    /// The workspace on it, when chosen.
    workspace: Option<String>,
}

impl Remote {
    /// The server name the agent tool lists it under: `tilo-` and the
    /// device, folded to what a server name may hold.
    fn server_name(&self) -> String {
        let mut name = String::from("tilo-");
        for character in self.device.chars().flat_map(char::to_lowercase) {
            if character.is_ascii_alphanumeric() {
                name.push(character);
            } else if !name.ends_with('-') {
                name.push('-');
            }
            if name.len() >= 48 {
                break;
            }
        }
        name.trim_end_matches('-').to_owned()
    }
}

#[derive(Subcommand)]
pub enum ModelsCmd {
    /// The models here, installed or available, as JSON.
    #[command(visible_alias = "ls")]
    List,
    /// Install a model.
    Add {
        /// The model, as `list` names it.
        model: String,
        /// An exact revision instead of the catalogue's.
        #[arg(long)]
        revision: Option<String>,
        /// Accept the model's licence where it asks for that.
        #[arg(long)]
        accept_license: bool,
    },
    /// Remove an installed model.
    #[command(visible_alias = "rm")]
    Remove {
        /// The model, as `list` names it.
        model: String,
    },
}

pub fn run(cmd: AgentCmd) -> Result<ExitCode> {
    match cmd {
        AgentCmd::Connect {
            tool,
            device,
            workspace,
        } => connect(
            tool,
            device.map(|device| Remote { device, workspace }).as_ref(),
        ),
        AgentCmd::Check { agent, config } => check(
            &agent
                .filter(|name| !name.trim().is_empty())
                .map_or_else(detect_here, |name| named(&name)),
            config.as_deref(),
        ),
        AgentCmd::Models { cmd } => models(cmd),
    }
}

fn connect(tool: Tool, remote: Option<&Remote>) -> Result<ExitCode> {
    let Some(door) = nouns::program(DOOR) else {
        // No desktop here: a computer logged in to the person's workspace
        // (`tilo login`) serves the agent itself, as `tilo mcp`.
        if remote.is_none() && crate::login::logged_in() {
            return connect_login(tool);
        }
        bail!(
            "connecting an agent needs a Tilo desktop, or this computer logged in first: \
             tilo login '<code from your Tilo tab>'"
        );
    };
    let Some(program) = tool.cli() else {
        bail!(
            "{} reads Tilo's tools from its own configuration; `tilo login` writes them there",
            tool.title()
        );
    };
    if nouns::program(program).is_none() {
        bail!(
            "{} isn't installed here; install it, then run this again",
            tool.title()
        );
    }
    let output = Command::new(program)
        .args(tool.registration(&door, remote))
        .output()
        .with_context(|| format!("running {program}"))?;
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let place = remote.map_or_else(
        || "this desktop".to_owned(),
        |remote| format!("your device {}", remote.device),
    );
    if output.status.success() {
        println!(
            "{} can now act on {place} from any terminal in your login.",
            tool.title()
        );
        if remote.is_some() {
            println!(
                "That device asks you before the agent first works there, unless you let agents \
                 from your other devices in on it."
            );
        }
        return Ok(ExitCode::SUCCESS);
    }
    if said.contains("already exists") {
        println!("{} is already connected to {place}.", tool.title());
        return Ok(ExitCode::SUCCESS);
    }
    eprint!("{said}");
    Ok(crate::exit_code(output.status.code()))
}

fn connect_login(tool: Tool) -> Result<ExitCode> {
    let tilo = std::env::current_exe().context("finding this program")?;
    match register(tool, &tilo, &System) {
        Registered::Added | Registered::Already => {
            println!(
                "{} now has Tilo's tools (MCP server {SERVER}).\n{}",
                tool.title(),
                crate::login::restart_of(tool)
            );
            Ok(ExitCode::SUCCESS)
        }
        Registered::Missing => bail!(
            "{} isn't installed here; install it, then run this again",
            tool.title()
        ),
        Registered::Failed(said) => bail!("adding Tilo's tools to {} failed: {said}", tool.title()),
    }
}

/// `tilo agent check [--agent NAME] [--config PATH]`: whether Tilo's MCP
/// server is in the agent running this, and answers.
fn check(agent: &Agent, config: Option<&Path>) -> Result<ExitCode> {
    let tilo = std::env::current_exe().context("finding this program")?;
    if let Some(config) = config {
        return Ok(check_config(agent, config, &tilo));
    }
    let Some(tool) = agent.tool else {
        // An agent Tilo does not know: it can say what Tilo's own server
        // answers, and hand the agent the server to add, but it cannot read
        // a configuration it has never seen.
        println!(
            "I don't know {}: I cannot read its MCP configuration. I can tell you what my own \
             server answers.",
            agent.name
        );
        match answers(&tilo, &["mcp".to_owned()]) {
            Ok(answered) => println!(
                "The Tilo server on this computer answers as tilo: {}.",
                tools_of(&answered)
            ),
            Err(why) => {
                println!("The Tilo server on this computer did not answer: {why}.");
                return Ok(ExitCode::FAILURE);
            }
        }
        println!();
        println!("{}", how_to_add_any(&agent.name, &tilo));
        println!(
            "Then run `tilo agent check --config <that file>` and I will say whether it is there."
        );
        return Ok(ExitCode::FAILURE);
    };
    match verify(tool, &System) {
        Configured::Yes {
            place,
            answered,
            program,
        } => {
            println!(
                "{}: the MCP server {SERVER} is in {place}, running {}, and it answers: {}.",
                agent.name,
                program.display(),
                tools_of(&answered)
            );
            if program != tilo {
                println!(
                    "It runs another copy of Tilo than this one ({}): `tilo login` points it at \
                     this one.",
                    tilo.display()
                );
            }
            crate::login::report_tools_added();
            println!();
            println!("{}", crate::login::first_act(agent));
            Ok(ExitCode::SUCCESS)
        }
        Configured::No { place } => {
            println!(
                "{}: {place} holds no MCP server named {SERVER}.",
                agent.name
            );
            println!();
            println!("{}", how_to_add(tool, &tilo));
            Ok(ExitCode::FAILURE)
        }
        Configured::Unknown { why } => {
            println!(
                "{}: its configuration was not read like Tilo expected: {why}.",
                agent.name
            );
            println!();
            println!("{}", how_to_add(tool, &tilo));
            Ok(ExitCode::FAILURE)
        }
    }
}

/// What a server answered with, as a line reads it.
fn tools_of(answered: &Answers) -> String {
    if !answered.tools.is_empty() {
        return format!("it has the tools {}", answered.tools.join(", "));
    }
    match &answered.note {
        Some(note) => format!(
            "it has no tools yet: {}",
            note.lines().next().unwrap_or(note)
        ),
        None => "it has no tools yet".to_owned(),
    }
}

/// `tilo agent check --config`: whether a file the agent named holds Tilo's
/// server.
fn check_config(agent: &Agent, config: &Path, tilo: &Path) -> ExitCode {
    let Some(text) = System.read(config) else {
        println!("{}: {} was not read.", agent.name, config.display());
        return ExitCode::FAILURE;
    };
    let Some(server) = find_server(&text, SERVER) else {
        println!(
            "{}: {} holds no MCP server named {SERVER}.",
            agent.name,
            config.display()
        );
        println!();
        println!("{}", how_to_add_any(&agent.name, tilo));
        return ExitCode::FAILURE;
    };
    println!(
        "{}: {} holds the MCP server {SERVER}, running {}.",
        agent.name,
        config.display(),
        server.program.display()
    );
    if server.program != tilo {
        println!(
            "It runs another copy of Tilo than this one ({}): point it at this one.",
            tilo.display()
        );
    }
    crate::login::report_tools_added();
    println!();
    println!("{}", crate::login::first_act(agent));
    ExitCode::SUCCESS
}

/// The line an agent runs or writes to add Tilo's server itself, for a tool
/// Tilo knows: its own command where it has one, else the entry to put in
/// its configuration.
fn how_to_add(tool: Tool, tilo: &Path) -> String {
    if let (Some(program_name), Some(add)) = (tool.cli(), tool.cli_add(SERVER, tilo, &["mcp"])) {
        return format!(
            "Add it: {program_name} {}\nthen run `tilo agent check` again.",
            words(&add).join(" ")
        );
    }
    format!(
        "Add it to {} under {}:\n  \"{SERVER}\": {}\nthen run `tilo agent check` again.",
        tool.files(&System).first().map_or_else(
            || "its configuration".to_owned(),
            |path| path.display().to_string()
        ),
        tool.section().join(" → "),
        serde_json::to_string(&tool.entry(tilo, &["mcp"])).unwrap_or_default()
    )
}

/// The server to add, for an agent whose configuration Tilo does not know.
fn how_to_add_any(name: &str, tilo: &Path) -> String {
    format!(
        "Add Tilo's MCP server to {name}, as the server {SERVER}: the stdio server {} with the \
         argument mcp, or the streamable HTTP server {MCP_URL}. Put it where your own MCP \
         configuration keeps servers, beside the ones already there, with your own tooling.",
        tilo.display()
    )
}

/// `args` as words a person or an agent reads.
fn words(args: &[OsString]) -> Vec<String> {
    args.iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}

fn models(cmd: ModelsCmd) -> Result<ExitCode> {
    let args: Vec<String> = match cmd {
        ModelsCmd::List => vec!["snapshot".into()],
        ModelsCmd::Add {
            model,
            revision,
            accept_license,
        } => {
            let mut args = vec!["install".into(), "--model".into(), model];
            if let Some(revision) = revision {
                args.extend(["--revision".into(), revision]);
            }
            if accept_license {
                args.push("--accept-license".into());
            }
            args
        }
        ModelsCmd::Remove { model } => vec!["remove".into(), "--model".into(), model],
    };
    let Some(manager) = nouns::program(MODELS) else {
        bail!("agent models need Tilo's model service, which this computer doesn't have");
    };
    let status = Command::new(&manager)
        .args(&args)
        .status()
        .with_context(|| format!("running {}", manager.display()))?;
    Ok(crate::exit_code(status.code()))
}

/// Runs an agent tool's own commands and reads its files: the real ones, or
/// a test's.
pub trait Runner {
    /// Where `program` is, when this computer has it.
    fn find(&self, program: &str) -> Option<PathBuf>;
    /// Run `program args`: whether it succeeded, and what it said.
    fn run(&self, program: &Path, args: &[OsString]) -> Result<(bool, String)>;
    /// This account's home, where agent tools keep their own files.
    fn home(&self) -> Option<PathBuf>;
    /// The kind of computer this is, as agent tools' paths differ by it.
    fn os(&self) -> Os {
        Os::here()
    }
    /// An environment variable's value, as agent tools' paths name it.
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|value| !value.is_empty())
    }
    /// `path`'s text, when it is there.
    fn read(&self, path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }
    /// Write `text` to `path`, making the directories above it.
    fn write(&self, path: &Path, text: &str) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("making {}", dir.display()))?;
        }
        std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
    }
}

/// The programs of this computer.
pub struct System;

impl Runner for System {
    fn find(&self, program: &str) -> Option<PathBuf> {
        nouns::program(program)
    }

    fn run(&self, program: &Path, args: &[OsString]) -> Result<(bool, String)> {
        let output = Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .with_context(|| format!("running {}", program.display()))?;
        Ok((
            output.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
            .trim()
            .to_owned(),
        ))
    }

    fn home(&self) -> Option<PathBuf> {
        std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(PathBuf::from)
    }
}

/// The kind of computer an agent tool keeps its configuration on: each keeps
/// it in a different place on each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Linux,
    Mac,
    Windows,
}

impl Os {
    /// This build's.
    #[must_use]
    pub fn here() -> Self {
        if cfg!(target_os = "macos") {
            Self::Mac
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}

/// `$XDG_CONFIG_HOME`, else this account's `~/.config`, where most agent
/// tools keep theirs on Linux and macOS.
fn config_dir(runner: &dyn Runner) -> Option<PathBuf> {
    if let Some(dir) = runner.var("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(dir));
    }
    Some(runner.home()?.join(".config"))
}

/// Where the macOS programs that keep theirs nowhere else put them.
fn application_support(runner: &dyn Runner) -> Option<PathBuf> {
    Some(runner.home()?.join("Library/Application Support"))
}

/// `%APPDATA%`, where Windows programs keep theirs.
fn app_data(runner: &dyn Runner) -> Option<PathBuf> {
    runner
        .var("APPDATA")
        .map(PathBuf::from)
        .or_else(|| Some(runner.home()?.join("AppData/Roaming")))
}

/// VS Code's own directory, `…/Code/User`, which holds its settings and
/// everything its extensions keep under them.
fn vscode_user(runner: &dyn Runner) -> Option<PathBuf> {
    Some(match runner.os() {
        Os::Windows => app_data(runner)?.join("Code/User"),
        Os::Mac => application_support(runner)?.join("Code/User"),
        Os::Linux => config_dir(runner)?.join("Code/User"),
    })
}

/// Claude Desktop's own configuration, where each kind of computer keeps its
/// programs' settings.
fn claude_desktop(runner: &dyn Runner) -> Option<PathBuf> {
    Some(match runner.os() {
        Os::Windows => app_data(runner)?.join("Claude/claude_desktop_config.json"),
        Os::Mac => application_support(runner)?.join("Claude/claude_desktop_config.json"),
        Os::Linux => config_dir(runner)?.join("Claude/claude_desktop_config.json"),
    })
}

/// Zed's settings, which it keeps under `~/.config` on every kind of
/// computer but Windows.
fn zed_settings(runner: &dyn Runner) -> Option<PathBuf> {
    Some(match runner.os() {
        Os::Windows => app_data(runner)?.join("Zed/settings.json"),
        _ => config_dir(runner)?.join("zed/settings.json"),
    })
}

/// What registering Tilo's tools with an agent came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Registered {
    /// Added now.
    Added,
    /// It had them already, for this program.
    Already,
    /// There is no way from here: its program isn't here and its
    /// configuration is not Tilo's to write.
    Missing,
    /// Its program or its configuration refused; what it said.
    Failed(String),
}

/// Add Tilo's server — the stdio server `program mcp`, named `tilo` — to
/// `tool`: through the tool's own command where it has one, else by writing
/// its configuration file, merging with the servers it holds and writing
/// every other byte back as it was.
pub fn register(tool: Tool, tilo: &Path, runner: &dyn Runner) -> Registered {
    if let Some(program) = tool.cli().and_then(|program| runner.find(program)) {
        let Some(add) = tool.cli_add(SERVER, tilo, &["mcp"]) else {
            return Registered::Missing;
        };
        return match runner.run(&program, &add) {
            Ok((true, _)) => Registered::Added,
            // It has one already, and it must run this program, wherever it
            // was before.
            Ok((false, said)) if said.contains("already exists") => {
                let Some(remove) = tool.cli_remove(SERVER) else {
                    return Registered::Already;
                };
                if let Err(error) = runner.run(&program, &remove) {
                    return Registered::Failed(format!("{error:#}"));
                }
                match runner.run(&program, &add) {
                    Ok((true, _)) => Registered::Already,
                    Ok((false, said)) => Registered::Failed(said),
                    Err(error) => Registered::Failed(format!("{error:#}")),
                }
            }
            Ok((false, said)) => Registered::Failed(said),
            Err(error) => Registered::Failed(format!("{error:#}")),
        };
    }
    let files = tool.files(runner);
    let Some(path) = files
        .iter()
        .find(|path| path.exists())
        .or_else(|| files.first())
    else {
        return Registered::Missing;
    };
    let present = runner.read(path);
    let text = present.clone().unwrap_or_default();
    let merged = match tool.write_into(&text, SERVER, tilo, &["mcp"]) {
        Ok(merged) => merged,
        Err(why) => return Registered::Failed(why),
    };
    if present.as_deref() == Some(merged.as_str()) {
        return Registered::Already;
    }
    match runner.write(path, &merged) {
        Ok(()) => Registered::Added,
        Err(error) => Registered::Failed(format!("{error:#}")),
    }
}

/// A configuration file holding nothing but this server, as a tool that was
/// never asked for one reads it.
fn fresh(section: &[&str], name: &str, entry: &Value) -> String {
    let mut value = json!({name: entry});
    for key in section.iter().rev() {
        value = json!({*key: value});
    }
    format!(
        "{}\n",
        serde_json::to_string_pretty(&value).unwrap_or_default()
    )
}

/// What `tool`'s own configuration says about Tilo's server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Configured {
    /// Its configuration holds the server, and what that would start
    /// answers as an MCP server.
    Yes {
        /// Where the server is, as the person reads it.
        place: String,
        /// What the server answered.
        answered: Answers,
        /// The program the configuration runs.
        program: PathBuf,
    },
    /// Its configuration was read and holds no such server.
    No {
        /// Where Tilo looked, as the person reads it.
        place: String,
    },
    /// Its configuration cannot be read here; why.
    Unknown {
        /// What stopped the reading.
        why: String,
    },
}

/// What `tool`'s own configuration says about Tilo's server, and whether what
/// it would start answers.
pub fn verify(tool: Tool, runner: &dyn Runner) -> Configured {
    let files = tool.files(runner);
    // A tool may keep several of the same file, one per way it is run: the
    // server is in whichever of them holds it.
    let mut looked = files.first().map_or_else(
        || "its configuration".to_owned(),
        |path| path.display().to_string(),
    );
    let mut unread: Option<String> = None;
    for path in files.iter().filter(|path| path.exists()) {
        let place = path.display().to_string();
        let Some(text) = runner.read(path) else {
            unread = unread.or_else(|| Some(format!("{place} was not read")));
            continue;
        };
        let found = match tool {
            Tool::Codex => toml_server(&text, SERVER),
            Tool::Goose => goose_extension(&text, SERVER),
            _ => {
                let mut at: Vec<&str> = tool.section().to_vec();
                at.push(SERVER);
                at_path(&text, &at)
                    .and_then(|value| serde_json::from_str::<Value>(&value).ok())
                    .and_then(|value| server_of(&value))
            }
        };
        let Some(server) = found else {
            looked = place;
            continue;
        };
        return match answers(&server.program, &server.args) {
            Ok(answered) => Configured::Yes {
                place,
                answered,
                program: server.program,
            },
            Err(why) => Configured::Unknown {
                why: format!("{} did not answer: {why}", server.program.display()),
            },
        };
    }
    if let Some(why) = unread {
        return Configured::Unknown { why };
    }
    Configured::No { place: looked }
}

/// A server as an agent tool's configuration writes it down.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Server {
    /// The program it runs.
    pub program: PathBuf,
    /// The arguments it runs it with.
    pub args: Vec<String>,
}

/// What an MCP server said about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answers {
    /// The name it gives itself.
    pub name: String,
    /// The tools it lists, as far as it got: none while the workspace behind
    /// it cannot be reached.
    pub tools: Vec<String>,
    /// What it said instead of tools, when it had none to list: why.
    pub note: Option<String>,
}

/// The server a configuration's JSON entry describes: its program, and its
/// arguments.
fn server_of(value: &Value) -> Option<Server> {
    // OpenCode names the program and its arguments in one list.
    if let Some(argv) = value.get("command").and_then(Value::as_array) {
        let mut argv = argv.iter().filter_map(Value::as_str);
        let program = argv.next()?;
        return Some(Server {
            program: PathBuf::from(program),
            args: argv.map(str::to_owned).collect(),
        });
    }
    let program = value.get("command").and_then(Value::as_str)?;
    let args = value
        .get("args")
        .and_then(Value::as_array)
        .map(|args| {
            args.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Some(Server {
        program: PathBuf::from(program),
        args,
    })
}

/// How long a server gets to answer: entering the workspace behind it may
/// take as long as its own entry allows.
const ANSWERS_MS: u64 = 45_000;

/// What `program args` says as an MCP server: the name it gives itself, and
/// the tools it lists, when it answers within [`ANSWERS_MS`].
pub fn answers(program: &Path, args: &[String]) -> Result<Answers, String> {
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::sync::mpsc;

    let mut child = Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|error| format!("it did not start: {error}"))?;
    let mut stdin = child.stdin.take().ok_or("it took no input")?;
    let stdout = child.stdout.take().ok_or("it made no answer")?;
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let asked = [
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                   "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                              "clientInfo": {"name": "tilo", "version": "1"}}}),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
        ];
        for line in &asked {
            if writeln!(stdin, "{line}").is_err() {
                let _ = sender.send(Err("it stopped reading".to_owned()));
                return;
            }
        }
        let mut name: Option<String> = None;
        let mut note: Option<String> = None;
        let mut tools: Option<Vec<String>> = None;
        // A server answers each request as it gets to it, so its name and its
        // tools arrive in whichever order it found them.
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let Ok(answer) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            match answer.get("id").and_then(Value::as_u64) {
                Some(1) => {
                    name = answer
                        .pointer("/result/serverInfo/name")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    note = answer
                        .pointer("/result/instructions")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    if name.is_none() {
                        let _ = sender.send(Err(format!("it said {answer}")));
                        return;
                    }
                }
                Some(2) => {
                    tools = Some(
                        answer
                            .pointer("/result/tools")
                            .and_then(Value::as_array)
                            .map(|tools| {
                                tools
                                    .iter()
                                    .filter_map(|tool| tool.get("name").and_then(Value::as_str))
                                    .map(str::to_owned)
                                    .collect()
                            })
                            .unwrap_or_default(),
                    );
                }
                _ => {}
            }
            if name.is_some() && tools.is_some() {
                break;
            }
        }
        let _ = sender.send(match name {
            Some(name) => Ok(Answers {
                name,
                tools: tools.unwrap_or_default(),
                note,
            }),
            None => Err("it answered nothing".to_owned()),
        });
    });
    let answer = receiver
        .recv_timeout(std::time::Duration::from_millis(ANSWERS_MS))
        .unwrap_or_else(|_| {
            Err(format!(
                "it did not answer in {} seconds",
                ANSWERS_MS / 1000
            ))
        });
    let _ = child.kill();
    let _ = child.wait();
    answer
}

// ---------------------------------------------------------------------------
// Configuration files
//
// An agent tool's file is the person's: other servers, their own settings
// and, in the formats that allow them, their comments. Adding Tilo's server
// reads as much of the file as finds the place for it and writes back
// everything else untouched. A file whose shape Tilo cannot place the server
// in is a file it leaves alone: the agent then adds the server itself.
// ---------------------------------------------------------------------------

/// `text` split at `at`: what lies before it, and what lies from it on.
fn split(text: &str, at: usize) -> (&str, &str) {
    (
        text.get(..at).unwrap_or_default(),
        text.get(at..).unwrap_or_default(),
    )
}

/// `text` with `at..end` written over by `with`.
fn splice(text: &str, at: usize, end: usize, with: &str) -> String {
    let (before, rest) = split(text, at);
    let (_, after) = split(rest, end.saturating_sub(at));
    format!("{before}{with}{after}")
}

/// The value at `path` in `text`, as JSON text: how a server's entry reads
/// in a configuration that may carry comments.
fn at_path(text: &str, path: &[&str]) -> Option<String> {
    let mut at = Cursor::new(text, 0);
    if at.skip()? != '{' {
        return None;
    }
    let mut open = at.at;
    for (index, key) in path.iter().enumerate() {
        let (members, _) = object(text, open)?;
        let member = members.into_iter().find(|member| member.key == *key)?;
        if index.saturating_add(1) == path.len() {
            return Some(text.get(member.value)?.to_owned());
        }
        let mut inner = Cursor::new(text, member.value.start);
        if inner.skip()? != '{' {
            return None;
        }
        open = inner.at;
    }
    None
}

/// The server `name` anywhere in a configuration, for a file whose shape
/// Tilo does not know: the answer an agent gets of its own work.
pub fn find_server(text: &str, name: &str) -> Option<Server> {
    let mut found = None;
    let mut stack = vec![0];
    while let Some(open) = stack.pop() {
        let Some((members, _)) = object(text, open) else {
            continue;
        };
        for member in members {
            let mut at = Cursor::new(text, member.value.start);
            if at.skip() != Some('{') {
                continue;
            }
            if member.key == name {
                found = found.or_else(|| {
                    text.get(member.value)
                        .and_then(|value| serde_json::from_str::<Value>(value).ok())
                        .and_then(|value| server_of(&value))
                });
            }
            stack.push(at.at);
        }
    }
    found
}

/// `text` with `name` set to `entry` under `path`, adding the keys on the way
/// that are not there.
fn with_member(text: &str, path: &[&str], name: &str, entry: &Value) -> Result<String, String> {
    let mut at = Cursor::new(text, 0);
    if at.skip() != Some('{') {
        return Err("it is not a JSON object".to_owned());
    }
    let (members, close) =
        object(text, at.at).ok_or_else(|| "it is not JSON Tilo reads".to_owned())?;
    merge(text, at.at, close, &members, path, name, entry)
}

/// `text` with `name` set to `entry` in the object at `open`, `path` naming
/// the keys above it.
fn merge(
    text: &str,
    open: usize,
    close: usize,
    members: &[Member],
    path: &[&str],
    name: &str,
    entry: &Value,
) -> Result<String, String> {
    let Some((key, rest)) = path.split_first() else {
        return Ok(set_member(text, open, close, members, name, entry));
    };
    match members.iter().find(|member| member.key == *key) {
        Some(member) => {
            let mut at = Cursor::new(text, member.value.start);
            if at.skip() != Some('{') {
                return Err(format!("\"{key}\" is not a JSON object"));
            }
            let (inner, inner_close) =
                object(text, at.at).ok_or_else(|| format!("\"{key}\" is not JSON Tilo reads"))?;
            merge(text, at.at, inner_close, &inner, rest, name, entry)
        }
        None => Ok(insert_member(
            text,
            open,
            close,
            members,
            &format!("\"{key}\": {}", fresh(rest, name, entry).trim_end()),
        )),
    }
}

/// `text` with `"name": entry` set in the object that closes at `close`,
/// replacing the entry it holds.
fn set_member(
    text: &str,
    open: usize,
    close: usize,
    members: &[Member],
    name: &str,
    entry: &Value,
) -> String {
    let value = serde_json::to_string(entry).unwrap_or_default();
    if let Some(member) = members.iter().find(|member| member.key == name) {
        return splice(text, member.value.start, member.value.end, &value);
    }
    insert_member(text, open, close, members, &format!("\"{name}\": {value}"))
}

/// `text` with `member` placed in the object that closes at `close`, after a
/// comma when it holds members already, indented as they are.
fn insert_member(
    text: &str,
    open: usize,
    close: usize,
    members: &[Member],
    member: &str,
) -> String {
    let (before, after) = split(text, close);
    // The object's own indentation, and how much deeper its members sit.
    let close_indent = line_indent(text, close);
    let close_line = line_indent(text, open);
    let unit = members.first().map_or_else(
        || "  ".to_owned(),
        |first| match line_indent(text, first.value.start).strip_prefix(&close_line) {
            Some(deeper) if !deeper.is_empty() => deeper.to_owned(),
            _ => "  ".to_owned(),
        },
    );
    let indent = format!("{close_indent}{unit}");
    let comma = if members.is_empty() { "" } else { "," };
    format!(
        "{}{comma}\n{indent}{member}\n{close_indent}{after}",
        before.trim_end()
    )
}

/// The whitespace `at`'s line starts with.
fn line_indent(text: &str, at: usize) -> String {
    let (before, _) = split(text, at);
    before
        .rsplit('\n')
        .next()
        .unwrap_or_default()
        .chars()
        .take_while(|character| *character == ' ' || *character == '\t')
        .collect()
}

/// Codex's `config.toml` with the server `name` set to run `program args`.
fn with_toml_server(
    text: &str,
    name: &str,
    program: &Path,
    args: &[&str],
) -> Result<String, String> {
    let table = format!(
        "[mcp_servers.{name}]\ncommand = {}\nargs = {}\n",
        toml_string(&program.to_string_lossy()),
        toml_list(args)
    );
    let header = format!("[mcp_servers.{name}]");
    let lines: Vec<&str> = text.lines().collect();
    if let Some(at) = lines.iter().position(|line| line.trim() == header) {
        // Its table runs to the next one: a line that opens a table.
        let end = lines
            .iter()
            .enumerate()
            .skip(at.saturating_add(1))
            .find(|(_, line)| line.trim_start().starts_with('['))
            .map_or(lines.len(), |(end, _)| end);
        let mut out: Vec<String> = lines
            .iter()
            .take(at)
            .map(|line| (*line).to_owned())
            .collect();
        out.extend(table.lines().map(str::to_owned));
        out.extend(lines.iter().skip(end).map(|line| (*line).to_owned()));
        return Ok(format!("{}\n", out.join("\n").trim_end()));
    }
    // Its servers written some other way — `mcp_servers = { … }`, or the
    // table with its own keys — are not Tilo's to reshape.
    let shaped = lines.iter().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("mcp_servers") && !trimmed.starts_with("[mcp_servers")
    });
    if shaped {
        return Err("its mcp_servers are written in a shape Tilo does not edit".to_owned());
    }
    Ok(format!("{}\n\n{table}", text.trim_end()))
}

/// `text` as a TOML string.
fn toml_string(text: &str) -> String {
    format!("{text:?}")
}

/// `args` as a TOML list of strings.
fn toml_list(args: &[&str]) -> String {
    let items: Vec<String> = args.iter().map(|arg| toml_string(arg)).collect();
    format!("[{}]", items.join(", "))
}

/// The server `name` in Codex's `config.toml`.
fn toml_server(text: &str, name: &str) -> Option<Server> {
    let config: toml::Value = toml::from_str(text).ok()?;
    let table = config.get("mcp_servers")?.get(name)?;
    let program = table.get("command")?.as_str()?;
    let args = table
        .get("args")
        .and_then(toml::Value::as_array)
        .map(|args| {
            args.iter()
                .filter_map(toml::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Some(Server {
        program: PathBuf::from(program),
        args,
    })
}

/// Goose's `config.yaml` with the extension `name` set to run `program args`:
/// Goose keeps its servers as extensions, two spaces in under `extensions`.
fn with_goose_extension(
    text: &str,
    name: &str,
    program: &Path,
    args: &[&str],
) -> Result<String, String> {
    let entry = format!(
        "  {name}:\n    name: {name}\n    type: stdio\n    cmd: {}\n    args: [{}]\n    enabled: true\n",
        yaml_string(&program.to_string_lossy()),
        args.iter()
            .map(|arg| yaml_string(arg))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let entry_lines: Vec<String> = entry.lines().map(str::to_owned).collect();
    let mut lines = text.lines().map(str::to_owned).collect::<Vec<_>>();
    let header = lines.iter().position(|line| {
        let trimmed = line.trim_end();
        trimmed == "extensions:" || trimmed == "extensions: {}"
    });
    let Some(at) = header else {
        if text.contains("extensions:") {
            return Err("its extensions are written in a shape Tilo does not edit".to_owned());
        }
        return Ok(format!("{}extensions:\n{entry}", join_before(&lines)));
    };
    lines.splice(at..=at, ["extensions:".to_owned()]);
    // An extension is a key two spaces in, with its own fields under it: the
    // block runs to the next key at the left margin.
    let start = at.saturating_add(1);
    let end = lines
        .iter()
        .enumerate()
        .skip(start)
        .find(|(_, line)| !line.trim().is_empty() && !line.starts_with(' '))
        .map_or(lines.len(), |(end, _)| end);
    let existing = lines
        .get(start..end)
        .unwrap_or_default()
        .iter()
        .position(|line| line.starts_with(&format!("  {name}:")));
    match existing {
        Some(offset) => {
            let ours = start.saturating_add(offset);
            let until = lines
                .iter()
                .enumerate()
                .skip(ours.saturating_add(1))
                .take(end.saturating_sub(ours.saturating_add(1)))
                .find(|(_, line)| !line.starts_with("    "))
                .map_or(end, |(until, _)| until);
            lines.splice(ours..until, entry_lines);
        }
        None => {
            lines.splice(start..start, entry_lines);
        }
    }
    Ok(format!("{}\n", lines.join("\n").trim_matches('\n')))
}

/// `lines` joined, with a blank line after them when they hold something.
fn join_before(lines: &[String]) -> String {
    if lines.iter().all(|line| line.trim().is_empty()) {
        return String::new();
    }
    format!("{}\n\n", lines.join("\n").trim_end())
}

/// `text` as a YAML string.
fn yaml_string(text: &str) -> String {
    format!("{text:?}")
}

/// The extension `name` in Goose's `config.yaml`.
fn goose_extension(text: &str, name: &str) -> Option<Server> {
    let mut inside = false;
    let mut ours = false;
    let mut program = None;
    let mut args: Vec<String> = Vec::new();
    for line in text.lines() {
        if !line.starts_with(' ') {
            inside = line.trim_end() == "extensions:";
            ours = false;
            continue;
        }
        if !inside {
            continue;
        }
        if !line.starts_with("    ") {
            ours = line.trim_start().starts_with(&format!("{name}:"));
            continue;
        }
        if !ours {
            continue;
        }
        let (key, value) = line.trim_start().split_once(':')?;
        let value = value.trim().trim_matches(['"', '\'']);
        match key {
            "cmd" => program = Some(PathBuf::from(value)),
            "args" => {
                args = value
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .split(',')
                    .map(|arg| arg.trim().trim_matches(['"', '\'']).to_owned())
                    .filter(|arg| !arg.is_empty())
                    .collect();
            }
            _ => {}
        }
    }
    Some(Server {
        program: program?,
        args,
    })
}

/// Where reading a JSON document has got to.
struct Cursor<'a> {
    text: &'a str,
    at: usize,
}

impl<'a> Cursor<'a> {
    fn new(text: &'a str, at: usize) -> Self {
        Self { text, at }
    }

    /// What is left to read.
    fn rest(&self) -> &'a str {
        self.text.get(self.at..).unwrap_or_default()
    }

    /// Step `by` characters, which the caller has read from [`Self::rest`].
    fn step(&mut self, by: usize) {
        self.at = self.at.saturating_add(by);
    }

    /// The next character that means something: whitespace and, since agent
    /// tools' files allow them, `//` and `/* */` comments are stepped over.
    fn skip(&mut self) -> Option<char> {
        loop {
            let rest = self.rest();
            let trimmed = rest.trim_start();
            self.step(rest.len().saturating_sub(trimmed.len()));
            if trimmed.starts_with("//") {
                self.step(
                    trimmed
                        .find('\n')
                        .map_or(trimmed.len(), |newline| newline.saturating_add(1)),
                );
            } else if trimmed.starts_with("/*") {
                self.step(trimmed.find("*/")?.saturating_add(2));
            } else {
                return trimmed.chars().next();
            }
        }
    }

    /// Past the quoted string that starts here.
    fn string(&mut self) -> Option<std::ops::Range<usize>> {
        let start = self.at;
        let mut chars = self.rest().char_indices();
        if chars.next()?.1 != '"' {
            return None;
        }
        let mut escaped = false;
        for (offset, character) in chars {
            if escaped {
                escaped = false;
                continue;
            }
            match character {
                '\\' => escaped = true,
                '"' => {
                    self.step(offset.saturating_add(1));
                    return Some(start..self.at);
                }
                _ => {}
            }
        }
        None
    }
}

/// One member of a JSON object: its key, and where its value sits.
struct Member {
    key: String,
    value: std::ops::Range<usize>,
}

/// The members of the object whose `{` is at `open`, and where its `}` is.
fn object(text: &str, open: usize) -> Option<(Vec<Member>, usize)> {
    let mut at = Cursor::new(text, open);
    if at.skip()? != '{' {
        return None;
    }
    at.step(1);
    let mut members = Vec::new();
    loop {
        match at.skip()? {
            '}' => return Some((members, at.at)),
            '"' => {}
            _ => return None,
        }
        let key = at.string()?;
        let key: String = serde_json::from_str(text.get(key)?).ok()?;
        if at.skip()? != ':' {
            return None;
        }
        at.step(1);
        at.skip()?;
        let end = value_end(text, at.at)?;
        members.push(Member {
            key,
            value: at.at..end,
        });
        at.at = end;
        match at.skip()? {
            ',' => at.step(1),
            '}' => return Some((members, at.at)),
            _ => return None,
        }
    }
}

/// Where the JSON value at `start` ends.
fn value_end(text: &str, start: usize) -> Option<usize> {
    let mut at = Cursor::new(text, start);
    match at.skip()? {
        '"' => at.string().map(|span| span.end),
        open @ ('{' | '[') => {
            let close = if open == '{' { '}' } else { ']' };
            let mut depth = 0usize;
            loop {
                let character = at.skip()?;
                match character {
                    '[' | '{' => {
                        depth = depth.saturating_add(1);
                        at.step(1);
                    }
                    ']' | '}' => {
                        depth = depth.saturating_sub(1);
                        at.step(1);
                        if depth == 0 {
                            return (character == close).then_some(at.at);
                        }
                    }
                    '"' => {
                        at.string()?;
                    }
                    _ => at.step(character.len_utf8()),
                }
            }
        }
        _ => {
            let end = at
                .rest()
                .find([',', '}', ']'])
                .map_or(text.len(), |offset| at.at.saturating_add(offset));
            Some(end)
        }
    }
}

// ---------------------------------------------------------------------------
// The agents
// ---------------------------------------------------------------------------

/// An agent `tilo login` found running it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agent {
    /// As the person's workspace names it: `Claude Code`.
    pub name: String,
    /// The tool, when Tilo knows how to add its tools to it.
    pub tool: Option<Tool>,
}

/// What `tilo login` calls an agent it found nothing about.
pub const UNKNOWN_AGENT: &str = "Your agent";

/// The agent a tool is, as the person's workspace names it.
fn known(tool: Tool) -> Agent {
    Agent {
        name: tool.title().to_owned(),
        tool: Some(tool),
    }
}

/// The agent `--agent` names: a known one by its name or program
/// (`claude`, `Claude Code`), else any name, as given.
#[must_use]
pub fn named(name: &str) -> Agent {
    let folded = name.trim().to_lowercase();
    Tool::ALL
        .iter()
        .find(|tool| {
            tool.title().to_lowercase() == folded
                || tool.programs().contains(&folded.as_str())
                || tool.cli() == Some(folded.as_str())
        })
        .map_or_else(
            || Agent {
                name: name.trim().chars().take(64).collect(),
                tool: None,
            },
            |tool| known(*tool),
        )
}

/// The agent the environment says runs this: each sets its own markers in
/// the commands it runs.
fn from_env(var: &dyn Fn(&str) -> Option<String>) -> Option<Agent> {
    let set = |name: &str| var(name).is_some_and(|value| !value.is_empty());
    if var("CLAUDECODE").as_deref() == Some("1") || set("CLAUDE_CODE_ENTRYPOINT") {
        return Some(known(Tool::Claude));
    }
    if [
        "CODEX_SANDBOX",
        "CODEX_SANDBOX_NETWORK_DISABLED",
        "CODEX_MANAGED_BY_NPM",
        "CODEX_MANAGED_BY_BUN",
    ]
    .into_iter()
    .any(set)
    {
        return Some(known(Tool::Codex));
    }
    if set("GEMINI_CLI") {
        return Some(known(Tool::Gemini));
    }
    if set("CURSOR_AGENT") {
        return Some(known(Tool::Cursor));
    }
    if set("OPENCODE") {
        return Some(known(Tool::OpenCode));
    }
    // A terminal inside an editor says which: an agent running in one is
    // added to that editor's configuration, which its agent mode reads.
    match var("TERM_PROGRAM").as_deref() {
        Some("vscode") => Some(known(Tool::VsCode)),
        Some("cursor") => Some(known(Tool::Cursor)),
        Some("Windsurf" | "windsurf") => Some(known(Tool::Windsurf)),
        Some("zed") => Some(known(Tool::Zed)),
        _ => None,
    }
}

/// One process this one runs under.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Process {
    /// Its program's name (`comm`), or path.
    pub comm: String,
    /// Its arguments, the program first.
    pub args: Vec<String>,
}

/// The agent a process is: by its program's name, or, for an agent that runs
/// on Node, by its script's (`…/@anthropic-ai/claude-code/cli.js`).
fn from_process(process: &Process) -> Option<Agent> {
    let base = |path: &str| -> String {
        let name = path.rsplit('/').next().unwrap_or(path);
        name.strip_suffix(".js")
            .or_else(|| name.strip_suffix(".mjs"))
            .or_else(|| name.strip_suffix(".cjs"))
            .unwrap_or(name)
            .to_lowercase()
    };
    // Claude Desktop and Claude Code share the name `claude`: the app spells
    // its own process with a capital, so that is the app and nothing else.
    if process.comm.rsplit('/').next() == Some("Claude") {
        return Some(known(Tool::ClaudeDesktop));
    }
    let mut names = vec![base(&process.comm)];
    names.extend(process.args.iter().take(2).map(|arg| base(arg)));
    for tool in Tool::ALL {
        if matches!(tool, Tool::ClaudeDesktop) {
            continue;
        }
        if names
            .iter()
            .any(|name| tool.programs().contains(&name.as_str()))
        {
            return Some(known(*tool));
        }
    }
    let packages = [
        ("@anthropic-ai/claude-code/", Tool::Claude),
        ("@openai/codex/", Tool::Codex),
        ("@google/gemini-cli/", Tool::Gemini),
        ("@sourcegraph/amp/", Tool::Amp),
    ];
    process.args.iter().take(2).find_map(|arg| {
        packages
            .iter()
            .find(|(package, _)| arg.contains(package))
            .map(|(_, tool)| known(*tool))
    })
}

/// The agent running this command: its environment's markers first, then the
/// processes it runs under, nearest first; [`UNKNOWN_AGENT`] when neither
/// says.
pub fn detect(
    var: &dyn Fn(&str) -> Option<String>,
    ancestors: impl IntoIterator<Item = Process>,
) -> Agent {
    from_env(var)
        .or_else(|| {
            ancestors
                .into_iter()
                .find_map(|process| from_process(&process))
        })
        .unwrap_or_else(|| Agent {
            name: UNKNOWN_AGENT.to_owned(),
            tool: None,
        })
}

/// The agent running this command, on this computer.
pub fn detect_here() -> Agent {
    detect(&|name| std::env::var(name).ok(), ancestors())
}

/// How far up the process tree to look.
const ANCESTORS: usize = 24;

/// The processes this one runs under, its parent first.
#[cfg(target_os = "linux")]
fn ancestors() -> Vec<Process> {
    let mut found = Vec::new();
    let mut pid = std::os::unix::process::parent_id();
    while pid > 1 && found.len() < ANCESTORS {
        let proc = std::path::PathBuf::from(format!("/proc/{pid}"));
        let Ok(stat) = std::fs::read_to_string(proc.join("stat")) else {
            break;
        };
        let comm = std::fs::read_to_string(proc.join("comm"))
            .unwrap_or_default()
            .trim()
            .to_owned();
        let args = std::fs::read(proc.join("cmdline"))
            .unwrap_or_default()
            .split(|byte| *byte == 0)
            .filter(|arg| !arg.is_empty())
            .map(|arg| String::from_utf8_lossy(arg).into_owned())
            .collect();
        found.push(Process { comm, args });
        // `pid (comm) state ppid …`: the name may hold spaces and parentheses.
        let parent = stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().nth(1))
            .and_then(|ppid| ppid.parse().ok());
        match parent {
            Some(parent) if parent != pid => pid = parent,
            _ => break,
        }
    }
    found
}

/// The processes this one runs under, its parent first, as `ps` lists them.
#[cfg(target_os = "macos")]
fn ancestors() -> Vec<Process> {
    let ps = |pid: u32, field: &str| -> Option<String> {
        let output = Command::new("/bin/ps")
            .args(["-o", field, "-p", &pid.to_string()])
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    let mut found = Vec::new();
    let mut pid = std::os::unix::process::parent_id();
    while pid > 1 && found.len() < ANCESTORS {
        let Some(comm) = ps(pid, "comm=") else { break };
        let args = ps(pid, "args=")
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        found.push(Process { comm, args });
        match ps(pid, "ppid=").and_then(|ppid| ppid.parse().ok()) {
            Some(parent) if parent != pid => pid = parent,
            _ => break,
        }
    }
    found
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn ancestors() -> Vec<Process> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    use super::*;

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    /// A computer with an account's home, the agent tools' files already in
    /// it, and the programs that are installed.
    struct FakeHome {
        root: PathBuf,
        home: PathBuf,
        config: PathBuf,
        os: Os,
        programs: Vec<String>,
        ran: RefCell<Vec<Vec<String>>>,
    }

    impl FakeHome {
        fn new(files: &[(&str, &str)], programs: &[&str]) -> Self {
            let root = tempfile::tempdir().unwrap().keep();
            let home = root.join("home");
            let config = root.join("config");
            for dir in [&home, &config] {
                std::fs::create_dir_all(dir).unwrap();
            }
            let this = Self {
                root,
                home,
                config,
                os: Os::Linux,
                programs: programs
                    .iter()
                    .map(|program| (*program).to_owned())
                    .collect(),
                ran: RefCell::new(Vec::new()),
            };
            for (path, text) in files {
                this.write(&this.home.join(path), text).unwrap();
            }
            this
        }

        fn at(&self, path: &str) -> PathBuf {
            self.home.join(path)
        }

        fn text(&self, path: &str) -> String {
            std::fs::read_to_string(self.at(path)).unwrap()
        }
    }

    impl Runner for FakeHome {
        fn find(&self, program: &str) -> Option<PathBuf> {
            self.programs
                .iter()
                .any(|installed| installed == program)
                .then(|| PathBuf::from(format!("/usr/bin/{program}")))
        }

        fn run(&self, program: &Path, args: &[OsString]) -> Result<(bool, String)> {
            let mut ran = strings(args);
            ran.insert(0, program.display().to_string());
            self.ran.borrow_mut().push(ran);
            Ok((true, String::new()))
        }

        fn home(&self) -> Option<PathBuf> {
            Some(self.home.clone())
        }

        fn os(&self) -> Os {
            self.os
        }

        fn var(&self, name: &str) -> Option<String> {
            match name {
                "XDG_CONFIG_HOME" => Some(self.config.display().to_string()),
                "APPDATA" => Some(self.root.join("appdata").display().to_string()),
                _ => None,
            }
        }
    }

    /// A program that answers as Tilo's MCP server does.
    #[cfg(unix)]
    fn answering_server(root: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let path = root.join("tilo-server");
        let initialized = r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"tilo","version":"1"}}}"#;
        let tools = r#"{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"tilo_ls"},{"name":"tilo_do"}]}}"#;
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nprintf '%s\\n' '{initialized}'\nprintf '%s\\n' '{tools}'\nsleep 5\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// The server Tilo's entry in a tool's file names, as Tilo reads it back.
    fn server_at(tool: Tool, fake: &FakeHome) -> Option<Server> {
        let files = tool.files(fake);
        let path = files
            .iter()
            .find(|path| path.exists())
            .or_else(|| files.first())?;
        let text = fake.read(path)?;
        match tool {
            Tool::Codex => toml_server(&text, SERVER),
            Tool::Goose => goose_extension(&text, SERVER),
            _ => {
                let mut at: Vec<&str> = tool.section().to_vec();
                at.push(SERVER);
                at_path(&text, &at)
                    .and_then(|value| serde_json::from_str::<Value>(&value).ok())
                    .and_then(|value| server_of(&value))
            }
        }
    }

    #[test]
    fn every_tool_that_has_its_own_command_adds_the_server_through_it() {
        let tilo = Path::new("/opt/tilo");
        assert_eq!(
            strings(&Tool::Claude.cli_add(SERVER, tilo, &["mcp"]).unwrap()),
            [
                "mcp",
                "add",
                "--scope",
                "user",
                "tilo",
                "--",
                "/opt/tilo",
                "mcp"
            ]
        );
        assert_eq!(
            strings(&Tool::Codex.cli_add(SERVER, tilo, &["mcp"]).unwrap()),
            ["mcp", "add", "tilo", "--", "/opt/tilo", "mcp"]
        );
        // Gemini CLI takes the command positionally.
        assert_eq!(
            strings(&Tool::Gemini.cli_add(SERVER, tilo, &["mcp"]).unwrap()),
            ["mcp", "add", "--scope", "user", "tilo", "/opt/tilo", "mcp"]
        );
        assert_eq!(
            strings(&Tool::Amp.cli_add(SERVER, tilo, &["mcp"]).unwrap()),
            ["mcp", "add", "tilo", "--", "/opt/tilo", "mcp"]
        );
        // OpenCode adds it to every project's configuration.
        assert_eq!(
            strings(&Tool::OpenCode.cli_add(SERVER, tilo, &["mcp"]).unwrap()),
            ["mcp", "add", "tilo", "--global", "--", "/opt/tilo", "mcp"]
        );
        // Cline's own wizard is skipped.
        assert_eq!(
            strings(&Tool::Cline.cli_add(SERVER, tilo, &["mcp"]).unwrap()),
            ["mcp", "add", "tilo", "--yes", "--", "/opt/tilo", "mcp"]
        );
        // VS Code takes the server itself, as JSON.
        assert_eq!(
            strings(&Tool::VsCode.cli_add(SERVER, tilo, &["mcp"]).unwrap()),
            [
                "--add-mcp",
                "{\"name\":\"tilo\",\"type\":\"stdio\",\"command\":\"/opt/tilo\",\"args\":[\"mcp\"]}"
            ]
        );
        // Cursor's CLI only reads the editor's file: there is no command to
        // add a server with.
        assert!(Tool::Cursor.cli_add(SERVER, tilo, &["mcp"]).is_none());
        // The rest have no command of their own: their file is the way in.
        assert!(Tool::Zed.cli_add(SERVER, tilo, &["mcp"]).is_none());
        assert!(Tool::Goose.cli_add(SERVER, tilo, &["mcp"]).is_none());
        assert_eq!(
            strings(&Tool::Codex.cli_remove(SERVER).unwrap()),
            ["mcp", "remove", "tilo"]
        );
    }

    #[test]
    fn a_command_that_refuses_the_server_is_not_the_one_that_adds_it() {
        // `claude` is here and adds it: Tilo trusts the tool it asked.
        let fake = FakeHome::new(&[], &["claude"]);
        assert_eq!(
            register(Tool::Claude, Path::new("/opt/tilo"), &fake),
            Registered::Added
        );
        assert_eq!(
            fake.ran.borrow()[0][1..],
            [
                "mcp",
                "add",
                "--scope",
                "user",
                "tilo",
                "--",
                "/opt/tilo",
                "mcp"
            ][..]
        );
        // Its own command is not here: its file is what Tilo writes.
        let fake = FakeHome::new(&[], &[]);
        assert_eq!(
            register(Tool::Cursor, Path::new("/opt/tilo"), &fake),
            Registered::Added
        );
        assert_eq!(
            server_at(Tool::Cursor, &fake).unwrap(),
            Server {
                program: PathBuf::from("/opt/tilo"),
                args: vec!["mcp".to_owned()]
            }
        );
    }

    #[test]
    fn a_tools_file_keeps_every_other_server_and_its_comments() {
        let fake = FakeHome::new(
            &[(
                ".cursor/mcp.json",
                "{\n  // the person's own servers\n  \"mcpServers\": {\n    \"canvas\": {\n      \
                 \"command\": \"python3\",\n      \"args\": [\"session.py\", \"mcp\"]\n    }\n  }\n}\n",
            )],
            &[],
        );
        assert_eq!(
            register(Tool::Cursor, Path::new("/opt/tilo"), &fake),
            Registered::Added
        );
        let text = fake.text(".cursor/mcp.json");
        assert!(text.contains("// the person's own servers"), "{text}");
        assert!(text.contains("\"canvas\""), "{text}");
        assert!(text.contains("\"tilo\""), "{text}");
        assert_eq!(
            server_at(Tool::Cursor, &fake).unwrap().program,
            PathBuf::from("/opt/tilo")
        );
        // The second time is the same file: nothing to add.
        assert_eq!(
            register(Tool::Cursor, Path::new("/opt/tilo"), &fake),
            Registered::Already
        );
        assert_eq!(fake.text(".cursor/mcp.json"), text);
    }

    #[test]
    fn a_section_a_tool_has_none_of_is_added_beside_its_other_settings() {
        // `../config` from the home is the account's configuration directory,
        // where OpenCode keeps its file.
        let fake = FakeHome::new(
            &[(
                "../config/opencode/opencode.json",
                "{\n  \"$schema\": \"https://opencode.ai/config.json\",\n  \"theme\": \"tokyonight\"\n}\n",
            )],
            &[],
        );
        assert_eq!(
            register(Tool::OpenCode, Path::new("/opt/tilo"), &fake),
            Registered::Added
        );
        let text = fake.text("../config/opencode/opencode.json");
        assert!(text.contains("https://opencode.ai/config.json"), "{text}");
        assert!(text.contains("tokyonight"), "{text}");
        assert_eq!(
            server_at(Tool::OpenCode, &fake).unwrap(),
            // OpenCode takes the program and its arguments as one list.
            Server {
                program: PathBuf::from("/opt/tilo"),
                args: vec!["mcp".to_owned()]
            }
        );
        let entry: Value =
            serde_json::from_str(&at_path(&text, &["mcp", "servers", "tilo"]).unwrap()).unwrap();
        assert_eq!(entry["type"], "local");
        assert_eq!(entry["command"], json!(["/opt/tilo", "mcp"]));
    }

    #[test]
    fn amp_keeps_its_servers_under_its_own_key() {
        let fake = FakeHome::new(
            &[(
                "../config/amp/settings.json",
                "{\n  \"amp.mcpServers\": {\n    \"other\": {\"command\": \"other\"}\n  }\n}\n",
            )],
            &[],
        );
        assert_eq!(
            register(Tool::Amp, Path::new("/opt/tilo"), &fake),
            Registered::Added
        );
        let text = fake.text("../config/amp/settings.json");
        assert!(text.contains("\"other\""), "{text}");
        assert_eq!(
            server_at(Tool::Amp, &fake).unwrap(),
            Server {
                program: PathBuf::from("/opt/tilo"),
                args: vec!["mcp".to_owned()]
            }
        );
    }

    #[test]
    fn vs_code_gets_a_stdio_server_under_servers() {
        let fake = FakeHome::new(
            &[(
                "../config/Code/User/mcp.json",
                "{\n  \"servers\": {\n    \"other\": {\"type\": \"stdio\", \"command\": \"other\"}\n  }\n}\n",
            )],
            &[],
        );
        assert_eq!(
            register(Tool::VsCode, Path::new("/opt/tilo"), &fake),
            Registered::Added
        );
        let text = fake.text("../config/Code/User/mcp.json");
        let entry: Value =
            serde_json::from_str(&at_path(&text, &["servers", "tilo"]).unwrap()).unwrap();
        assert_eq!(entry["type"], "stdio");
        assert_eq!(entry["command"], "/opt/tilo");
        assert_eq!(entry["args"], json!(["mcp"]));
        assert!(text.contains("\"other\""), "{text}");
    }

    #[test]
    fn zed_settings_keep_their_comments_and_gain_a_context_server() {
        let fake = FakeHome::new(
            &[(
                "../config/zed/settings.json",
                "// Zed settings\n{\n  // how it looks\n  \"theme\": \"One Dark\",\n  \
                 \"context_servers\": {}\n}\n",
            )],
            &[],
        );
        assert_eq!(
            register(Tool::Zed, Path::new("/opt/tilo"), &fake),
            Registered::Added
        );
        let text = fake.text("../config/zed/settings.json");
        assert!(text.contains("// Zed settings"), "{text}");
        assert!(text.contains("// how it looks"), "{text}");
        assert!(text.contains("\"One Dark\""), "{text}");
        let entry: Value =
            serde_json::from_str(&at_path(&text, &["context_servers", "tilo"]).unwrap()).unwrap();
        assert_eq!(entry["command"], "/opt/tilo");
        assert_eq!(entry["args"], json!(["mcp"]));
        assert_eq!(entry["env"], json!({}));
    }

    #[test]
    fn the_apps_that_keep_servers_under_mcpservers_take_one_beside_the_others() {
        for (tool, path) in [
            (Tool::ClaudeDesktop, "Claude/claude_desktop_config.json"),
            (Tool::Windsurf, ".codeium/windsurf/mcp_config.json"),
            (
                Tool::Cline,
                "Code/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json",
            ),
            (
                Tool::Roo,
                "Code/User/globalStorage/rooveterinaryinc.roo-cline/settings/mcp_settings.json",
            ),
        ] {
            let fake = FakeHome::new(&[], &[]);
            // Each keeps its file under its own configuration directory.
            let file = tool.files(&fake).into_iter().next().unwrap();
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(
                &file,
                "{\n  \"mcpServers\": {\n    \"other\": {\"command\": \"other\"}\n  }\n}\n",
            )
            .unwrap();
            assert_eq!(
                register(tool, Path::new("/opt/tilo"), &fake),
                Registered::Added,
                "{path}"
            );
            let text = std::fs::read_to_string(&file).unwrap();
            assert!(text.contains("\"other\""), "{tool:?}: {text}");
            assert_eq!(
                server_at(tool, &fake).unwrap().program,
                PathBuf::from("/opt/tilo"),
                "{path}"
            );
        }
    }

    #[test]
    fn codex_keeps_its_other_tables_and_its_server_is_replaced_where_it_stands() {
        let fake = FakeHome::new(
            &[(
                ".codex/config.toml",
                "model = \"gpt\"\n\n[mcp_servers.other]\ncommand = \"/opt/other\"\n",
            )],
            &[],
        );
        assert_eq!(
            register(Tool::Codex, Path::new("/opt/tilo"), &fake),
            Registered::Added
        );
        let text = fake.text(".codex/config.toml");
        let config: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(config["model"].as_str(), Some("gpt"));
        assert_eq!(
            config["mcp_servers"]["other"]["command"].as_str(),
            Some("/opt/other")
        );
        assert_eq!(
            config["mcp_servers"]["tilo"]["command"].as_str(),
            Some("/opt/tilo")
        );
        assert_eq!(
            config["mcp_servers"]["tilo"]["args"][0].as_str(),
            Some("mcp")
        );
        // Pointed at another copy of Tilo, the table is replaced in place and
        // the tables after it stay.
        let fake = FakeHome::new(
            &[(
                ".codex/config.toml",
                "model = \"gpt\"\n\n[mcp_servers.tilo]\ncommand = \"/old/tilo\"\nargs = [\"mcp\"]\n\n[something]\nkey = 1\n",
            )],
            &[],
        );
        assert_eq!(
            register(Tool::Codex, Path::new("/opt/tilo"), &fake),
            Registered::Added
        );
        let text = fake.text(".codex/config.toml");
        let config: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(
            config["mcp_servers"]["tilo"]["command"].as_str(),
            Some("/opt/tilo")
        );
        assert_eq!(config["something"]["key"].as_integer(), Some(1));
        assert_eq!(config["model"].as_str(), Some("gpt"));
    }

    #[test]
    fn goose_keeps_its_extensions_and_gains_one_more() {
        let fake = FakeHome::new(
            &[(
                "../config/goose/config.yaml",
                "GOOSE_MODEL: gpt\nextensions:\n  developer:\n    enabled: true\n    type: builtin\n    name: developer\n",
            )],
            &[],
        );
        assert_eq!(
            register(Tool::Goose, Path::new("/opt/tilo"), &fake),
            Registered::Added
        );
        let text = fake.text("../config/goose/config.yaml");
        assert!(text.contains("GOOSE_MODEL: gpt"), "{text}");
        assert!(text.contains("developer:"), "{text}");
        assert_eq!(
            server_at(Tool::Goose, &fake).unwrap(),
            Server {
                program: PathBuf::from("/opt/tilo"),
                args: vec!["mcp".to_owned()]
            }
        );
    }

    #[test]
    fn a_file_tilo_cannot_place_the_server_in_is_left_alone() {
        let fake = FakeHome::new(&[(".cursor/mcp.json", "this is not JSON\n")], &[]);
        assert!(
            matches!(
                register(Tool::Cursor, Path::new("/opt/tilo"), &fake),
                Registered::Failed(_)
            ),
            "a file Tilo cannot read is the agent's to write"
        );
        assert_eq!(fake.text(".cursor/mcp.json"), "this is not JSON\n");
        // Nor does it reshape a section that is not an object.
        let fake = FakeHome::new(&[(".cursor/mcp.json", "{\"mcpServers\": []}\n")], &[]);
        assert!(matches!(
            register(Tool::Cursor, Path::new("/opt/tilo"), &fake),
            Registered::Failed(_)
        ));
    }

    #[test]
    fn a_configuration_tilo_does_not_know_is_read_for_the_server() {
        let text = "{\n  \"tools\": {\n    \"mcp\": {\n      \"tilo\": {\"command\": \"/opt/tilo\", \
                    \"args\": [\"mcp\"]}\n    }\n  }\n}\n";
        assert_eq!(
            find_server(text, SERVER),
            Some(Server {
                program: PathBuf::from("/opt/tilo"),
                args: vec!["mcp".to_owned()]
            })
        );
        assert_eq!(find_server("{}", SERVER), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_configured_server_is_verified_by_asking_it() {
        let root = tempfile::tempdir().unwrap().keep();
        let server = answering_server(&root);
        let configured = format!(
            "{{\"mcpServers\": {{\"tilo\": {{\"command\": \"{}\", \"args\": [\"mcp\"]}}}}}}",
            server.display()
        );
        let fake = FakeHome::new(&[(".cursor/mcp.json", configured.as_str())], &[]);
        assert_eq!(
            verify(Tool::Cursor, &fake),
            Configured::Yes {
                place: fake.at(".cursor/mcp.json").display().to_string(),
                answered: Answers {
                    name: "tilo".to_owned(),
                    tools: vec!["tilo_ls".to_owned(), "tilo_do".to_owned()],
                    note: None
                },
                program: server,
            }
        );
        // A file with no Tilo in it says so, naming where Tilo looked.
        let empty = FakeHome::new(&[(".cursor/mcp.json", "{\"mcpServers\": {}}\n")], &[]);
        assert_eq!(
            verify(Tool::Cursor, &empty),
            Configured::No {
                place: empty.at(".cursor/mcp.json").display().to_string()
            }
        );
        // A server that does not answer is not a server Tilo counted.
        let silent = FakeHome::new(
            &[(
                ".cursor/mcp.json",
                "{\"mcpServers\": {\"tilo\": {\"command\": \"/nonexistent/tilo\"}}}",
            )],
            &[],
        );
        assert!(matches!(
            verify(Tool::Cursor, &silent),
            Configured::Unknown { .. }
        ));
    }

    /// A tool that keeps the same file in two places — Cline, whose CLI has
    /// its own account of the servers its editor extension holds — is right
    /// when either of them holds the server.
    #[cfg(unix)]
    #[test]
    fn the_server_is_found_in_whichever_of_a_tools_files_holds_it() {
        let root = tempfile::tempdir().unwrap().keep();
        let server = answering_server(&root);
        let fake = FakeHome::new(
            &[(
                ".cline/data/settings/cline_mcp_settings.json",
                &format!(
                    "{{\"mcpServers\": {{\"tilo\": {{\"command\": \"{}\"}}}}}}",
                    server.display()
                ),
            )],
            &[],
        );
        let verified = verify(Tool::Cline, &fake);
        assert!(matches!(verified, Configured::Yes { .. }), "{verified:?}");
        assert_eq!(
            Tool::Cline.files(&fake).first().map(|path| path.exists()),
            Some(false),
            "the first of its files is not there"
        );
    }

    #[test]
    fn where_each_tool_keeps_its_file_depends_on_the_kind_of_computer() {
        let mut fake = FakeHome::new(&[], &[]);
        let files = |fake: &FakeHome, tool: Tool| -> Vec<String> {
            tool.files(fake)
                .into_iter()
                .map(|path| path.display().to_string())
                .collect()
        };
        assert_eq!(
            files(&fake, Tool::Cursor),
            [fake.home.join(".cursor/mcp.json").display().to_string()]
        );
        assert_eq!(
            files(&fake, Tool::Claude),
            [fake.home.join(".claude.json").display().to_string()]
        );
        assert_eq!(
            files(&fake, Tool::VsCode),
            [fake.config.join("Code/User/mcp.json").display().to_string()]
        );
        fake.os = Os::Mac;
        assert_eq!(
            files(&fake, Tool::ClaudeDesktop),
            [fake
                .home
                .join("Library/Application Support/Claude/claude_desktop_config.json")
                .display()
                .to_string()]
        );
        assert_eq!(
            files(&fake, Tool::VsCode),
            [fake
                .home
                .join("Library/Application Support/Code/User/mcp.json")
                .display()
                .to_string()]
        );
        fake.os = Os::Windows;
        assert_eq!(
            files(&fake, Tool::Zed),
            [fake
                .root
                .join("appdata/Zed/settings.json")
                .display()
                .to_string()]
        );
        assert_eq!(
            files(&fake, Tool::ClaudeDesktop),
            [fake
                .root
                .join("appdata/Claude/claude_desktop_config.json")
                .display()
                .to_string()]
        );
        assert_eq!(
            files(&fake, Tool::Cline),
            [
                fake.root
                    .join("appdata/Code/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json")
                    .display()
                    .to_string(),
                fake.home
                    .join(".cline/data/settings/cline_mcp_settings.json")
                    .display()
                    .to_string()
            ]
        );
        assert_eq!(
            files(&fake, Tool::OpenCode),
            [
                fake.config
                    .join("opencode/opencode.jsonc")
                    .display()
                    .to_string(),
                fake.config
                    .join("opencode/opencode.json")
                    .display()
                    .to_string()
            ]
        );
    }

    #[test]
    fn an_agent_is_found_by_its_environment_first() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| (*value).to_owned())
            }
        };
        let codex_parent = vec![Process {
            comm: "codex".into(),
            args: vec!["codex".into()],
        }];
        let found = detect(&env(&[("CLAUDECODE", "1")]), codex_parent.clone());
        assert_eq!(found, known(Tool::Claude));
        assert_eq!(found.tool, Some(Tool::Claude));
        assert_eq!(
            detect(&env(&[("CLAUDE_CODE_ENTRYPOINT", "cli")]), Vec::new()).name,
            "Claude Code"
        );
        assert_eq!(
            detect(&env(&[("CODEX_SANDBOX", "seatbelt")]), Vec::new()).name,
            "Codex"
        );
        assert_eq!(
            detect(&env(&[("GEMINI_CLI", "1")]), Vec::new()).tool,
            Some(Tool::Gemini)
        );
        assert_eq!(
            detect(&env(&[("CURSOR_AGENT", "1")]), Vec::new()).tool,
            Some(Tool::Cursor)
        );
        // An editor's own terminal says which agent mode reads the server.
        assert_eq!(
            detect(&env(&[("TERM_PROGRAM", "vscode")]), Vec::new()).tool,
            Some(Tool::VsCode)
        );
        assert_eq!(
            detect(&env(&[("TERM_PROGRAM", "zed")]), Vec::new()).tool,
            Some(Tool::Zed)
        );
        // Nothing in the environment: the nearest agent process says.
        assert_eq!(detect(&env(&[]), codex_parent).name, "Codex");
    }

    #[test]
    fn an_agent_is_found_among_the_processes_it_runs_under() {
        let none = |_: &str| None;
        let shell = Process {
            comm: "bash".into(),
            args: vec![
                "/bin/bash".into(),
                "-c".into(),
                "tilo login K7Q-MX2P".into(),
            ],
        };
        let node_claude = Process {
            comm: "node".into(),
            args: vec![
                "node".into(),
                "/usr/lib/node_modules/@anthropic-ai/claude-code/cli.js".into(),
            ],
        };
        assert_eq!(
            detect(&none, vec![shell.clone(), node_claude]).name,
            "Claude Code"
        );
        let found = detect(
            &none,
            vec![
                shell.clone(),
                Process {
                    comm: "cursor-agent".into(),
                    args: Vec::new(),
                },
            ],
        );
        assert_eq!(
            (found.name.as_str(), found.tool),
            ("Cursor", Some(Tool::Cursor))
        );
        assert_eq!(
            detect(
                &none,
                vec![Process {
                    comm: "goose".into(),
                    args: vec!["/Users/you/.local/bin/goose".into(), "session".into()],
                }]
            )
            .tool,
            Some(Tool::Goose)
        );
        // A terminal inside VS Code: its agent mode is what reads the server.
        assert_eq!(
            detect(
                &none,
                vec![
                    shell.clone(),
                    Process {
                        comm: "code".into(),
                        args: vec!["/usr/share/code/code".into()],
                    }
                ]
            )
            .tool,
            Some(Tool::VsCode)
        );
        // Claude Desktop spells its process with a capital; Claude Code does
        // not, and is the one whose name is found first.
        assert_eq!(
            detect(
                &none,
                vec![Process {
                    comm: "Claude".into(),
                    args: vec!["/opt/claude-desktop/claude".into()],
                }]
            )
            .tool,
            Some(Tool::ClaudeDesktop)
        );
        assert_eq!(detect(&none, vec![shell]).name, UNKNOWN_AGENT);
    }

    #[test]
    fn agent_names_the_known_ones_by_name_or_program_and_keeps_others() {
        assert_eq!(named("claude"), known(Tool::Claude));
        assert_eq!(named("codex").tool, Some(Tool::Codex));
        assert_eq!(named(" Gemini CLI ").tool, Some(Tool::Gemini));
        assert_eq!(named("cursor").tool, Some(Tool::Cursor));
        assert_eq!(named("VS Code").tool, Some(Tool::VsCode));
        assert_eq!(named("roo-code").tool, Some(Tool::Roo));
        assert_eq!(
            named("Hermes"),
            Agent {
                name: "Hermes".into(),
                tool: None
            }
        );
    }

    #[test]
    fn the_agent_is_told_where_its_own_command_and_file_are() {
        let tilo = Path::new("/opt/tilo");
        // A tool whose own command adds servers is told that command.
        assert!(
            how_to_add(Tool::Codex, tilo)
                .starts_with("Add it: codex mcp add tilo -- /opt/tilo mcp"),
            "{}",
            how_to_add(Tool::Codex, tilo)
        );
        // One that keeps a file is told the entry to write, and where.
        let told = how_to_add(Tool::Zed, tilo);
        assert!(told.contains("zed/settings.json"), "{told}");
        assert!(told.contains("context_servers"), "{told}");
        assert!(told.contains("\"tilo\": "), "{told}");
        assert!(told.contains("\"command\":\"/opt/tilo\""), "{told}");
        // An agent Tilo does not know is told the server itself, both ways.
        let told = how_to_add_any("Hermes", tilo);
        assert!(told.contains("/opt/tilo"), "{told}");
        assert!(told.contains(MCP_URL), "{told}");
    }

    /// Connecting registers the door, in every directory for Claude Code,
    /// under the name `tilo` with `--connect`, so it reaches the login's
    /// own desktop and never starts one.
    #[test]
    fn connecting_registers_the_desktops_door() {
        let door = std::path::Path::new("/usr/libexec/tilo/tilo-runtime-mcp");
        let claude: Vec<String> = Tool::Claude
            .registration(door, None)
            .into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            claude,
            [
                "mcp",
                "add",
                "--scope",
                "user",
                "tilo",
                "--",
                "/usr/libexec/tilo/tilo-runtime-mcp",
                "--connect"
            ]
        );
        let codex: Vec<String> = Tool::Codex
            .registration(door, None)
            .into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            codex,
            [
                "mcp",
                "add",
                "tilo",
                "--",
                "/usr/libexec/tilo/tilo-runtime-mcp",
                "--connect"
            ]
        );
    }

    /// Connecting to another device registers its own server, named after
    /// the device, carried to that device's door.
    #[test]
    fn connecting_to_another_device_registers_its_door_beside_this_ones() {
        let door = std::path::Path::new("/usr/libexec/tilo/tilo-runtime-mcp");
        let remote = Remote {
            device: "Build Box".into(),
            workspace: Some("cccccccc-cccc-4ccc-8ccc-cccccccccccc".into()),
        };
        let claude: Vec<String> = Tool::Claude
            .registration(door, Some(&remote))
            .into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            claude,
            [
                "mcp",
                "add",
                "--scope",
                "user",
                "tilo-build-box",
                "--",
                "/usr/libexec/tilo/tilo-runtime-mcp",
                "--connect",
                "--device",
                "Build Box",
                "--workspace",
                "cccccccc-cccc-4ccc-8ccc-cccccccccccc"
            ]
        );
    }

    #[test]
    fn a_workspace_is_named_by_its_id() {
        assert!(workspace_id("CCCCCCCC-cccc-4ccc-8ccc-cccccccccccc").is_ok());
        assert!(workspace_id("Studio").is_err());
        assert!(workspace_id("cccccccc-cccc-4ccc-8ccc-ccccccccccc").is_err());
    }
}
