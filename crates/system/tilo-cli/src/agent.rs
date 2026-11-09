//! `tilo agent`: the agents you run, and the models they run on.
//!
//! `connect` registers this desktop's agent door with an agent tool you run
//! in a terminal, so it can act on the desktop through the same actions a
//! person has — or, with `--device`, the agent door of another of your own
//! devices, reached from this one. `models` is the model list Settings shows,
//! through the program that keeps it.

use std::ffi::OsString;
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};
use clap::Subcommand;

use crate::nouns;

/// The agent door an agent tool starts.
pub const DOOR: &str = "tilo-runtime-mcp";
/// The program that keeps the model list.
pub const MODELS: &str = "tilo-model-manager";

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
    /// The models agents run on, local or hosted.
    Models {
        #[command(subcommand)]
        cmd: ModelsCmd,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Tool {
    /// Claude Code.
    Claude,
    /// Codex.
    Codex,
    /// Gemini CLI: `tilo login` adds Tilo's tools to it; a desktop's door is
    /// not offered to it.
    #[value(skip)]
    Gemini,
}

impl Tool {
    fn command(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::Gemini => "Gemini CLI",
        }
    }

    /// How the person restarts this agent with the conversation kept: each
    /// loads its MCP servers only when it starts.
    pub fn restart(self) -> &'static str {
        match self {
            Self::Claude => "/exit, then `claude --continue`",
            Self::Codex => "/quit, then `codex resume --last`",
            Self::Gemini => "/quit, then `gemini --resume latest`",
        }
    }

    /// The tool's own command that registers `program args` as the stdio
    /// MCP server `name`, for every directory of this login.
    fn add(self, name: &str, program: &std::path::Path, args: &[&str]) -> Vec<OsString> {
        let mut add: Vec<OsString> = match self {
            Self::Claude | Self::Gemini => ["mcp", "add", "--scope", "user"]
                .map(OsString::from)
                .to_vec(),
            Self::Codex => ["mcp", "add"].map(OsString::from).to_vec(),
        };
        add.push(name.into());
        // Gemini CLI takes the command and its arguments positionally.
        if self != Self::Gemini {
            add.push("--".into());
        }
        add.push(program.as_os_str().to_owned());
        add.extend(args.iter().map(OsString::from));
        add
    }

    /// The tool's own command that removes the server `name`.
    fn remove(self, name: &str) -> Vec<OsString> {
        let mut remove: Vec<OsString> = match self {
            Self::Claude | Self::Gemini => ["mcp", "remove", "--scope", "user"]
                .map(OsString::from)
                .to_vec(),
            Self::Codex => ["mcp", "remove"].map(OsString::from).to_vec(),
        };
        remove.push(name.into());
        remove
    }

    /// The tool's own command that registers `door` as the server `tilo`,
    /// or, for another device, as `tilo-<device>` carried to that device's
    /// door.
    fn registration(self, door: &std::path::Path, remote: Option<&Remote>) -> Vec<OsString> {
        let name = remote.map_or_else(|| "tilo".to_owned(), Remote::server_name);
        let mut door_args = vec!["--connect"];
        if let Some(remote) = remote {
            door_args.extend(["--device", remote.device.as_str()]);
            if let Some(workspace) = &remote.workspace {
                door_args.extend(["--workspace", workspace.as_str()]);
            }
        }
        self.add(&name, door, &door_args)
    }
}

/// A workspace id, as `--workspace` takes it: a UUID in its hyphenated form.
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
    if nouns::program(tool.command()).is_none() {
        bail!(
            "{} isn't installed here; install it, then run this again",
            tool.title()
        );
    }
    let output = Command::new(tool.command())
        .args(tool.registration(&door, remote))
        .output()
        .with_context(|| format!("running {}", tool.command()))?;
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
                "{} now has Tilo's tools (MCP server tilo). They load when it starts: restart it \
                 ({}); then its tilo_ls tool lists your workspace's apps.",
                tool.title(),
                tool.restart()
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

/// Runs an agent tool's own commands: the real ones, or a test's.
pub trait Runner {
    /// Where `program` is, when this computer has it.
    fn find(&self, program: &str) -> Option<std::path::PathBuf>;
    /// Run `program args`: whether it succeeded, and what it said.
    fn run(&self, program: &std::path::Path, args: &[OsString]) -> Result<(bool, String)>;
}

/// The programs of this computer.
pub struct System;

impl Runner for System {
    fn find(&self, program: &str) -> Option<std::path::PathBuf> {
        nouns::program(program)
    }

    fn run(&self, program: &std::path::Path, args: &[OsString]) -> Result<(bool, String)> {
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
}

/// What registering Tilo's tools with an agent came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Registered {
    /// Added now.
    Added,
    /// It had them already, for this program.
    Already,
    /// The agent's own program isn't here.
    Missing,
    /// Its program refused; what it said.
    Failed(String),
}

/// Register `tilo mcp` (`tilo` is this program) with `tool` as the server
/// `tilo`. A server `tilo` it has already is replaced, so it runs this
/// program wherever it was before.
pub fn register(tool: Tool, tilo: &std::path::Path, runner: &dyn Runner) -> Registered {
    let Some(program) = runner.find(tool.command()) else {
        return Registered::Missing;
    };
    let add = tool.add("tilo", tilo, &["mcp"]);
    let said = match runner.run(&program, &add) {
        Ok((true, _)) => return Registered::Added,
        Ok((false, said)) => said,
        Err(error) => return Registered::Failed(format!("{error:#}")),
    };
    if !said.contains("already exists") {
        return Registered::Failed(said);
    }
    if let Err(error) = runner.run(&program, &tool.remove("tilo")) {
        return Registered::Failed(format!("{error:#}"));
    }
    match runner.run(&program, &add) {
        Ok((true, _)) => Registered::Already,
        Ok((false, said)) => Registered::Failed(said),
        Err(error) => Registered::Failed(format!("{error:#}")),
    }
}

/// An agent `tilo login` found running it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agent {
    /// As the person's workspace names it: `Claude Code`.
    pub name: String,
    /// The tool, when Tilo adds its tools to it itself.
    pub tool: Option<Tool>,
}

/// Agents `tilo login` recognizes: the name, the tool, and the program names
/// its process runs under.
const AGENTS: &[(&str, Option<Tool>, &[&str])] = &[
    (
        "Claude Code",
        Some(Tool::Claude),
        &["claude", "claude-code"],
    ),
    ("Codex", Some(Tool::Codex), &["codex"]),
    ("Gemini CLI", Some(Tool::Gemini), &["gemini", "gemini-cli"]),
    ("Cursor", None, &["cursor-agent"]),
    ("opencode", None, &["opencode"]),
    ("Amp", None, &["amp"]),
    ("Goose", None, &["goose"]),
];

/// What `tilo login` calls an agent it found nothing about.
pub const UNKNOWN_AGENT: &str = "Your agent";

fn known(name: &str) -> Agent {
    AGENTS
        .iter()
        .find(|(title, _, _)| *title == name)
        .map_or_else(
            || Agent {
                name: name.to_owned(),
                tool: None,
            },
            |(title, tool, _)| Agent {
                name: (*title).to_owned(),
                tool: *tool,
            },
        )
}

/// The agent `--agent` names: a known one by its name or program
/// (`claude`, `Claude Code`), else any name, as given.
#[must_use]
pub fn named(name: &str) -> Agent {
    let folded = name.trim().to_lowercase();
    AGENTS
        .iter()
        .find(|(title, _, programs)| {
            title.to_lowercase() == folded || programs.contains(&folded.as_str())
        })
        .map_or_else(
            || Agent {
                name: name.trim().chars().take(64).collect(),
                tool: None,
            },
            |(title, _, _)| known(title),
        )
}

/// The agent the environment says runs this: each sets its own markers in
/// the commands it runs.
fn from_env(var: &dyn Fn(&str) -> Option<String>) -> Option<Agent> {
    let set = |name: &str| var(name).is_some_and(|value| !value.is_empty());
    if var("CLAUDECODE").as_deref() == Some("1") || set("CLAUDE_CODE_ENTRYPOINT") {
        return Some(known("Claude Code"));
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
        return Some(known("Codex"));
    }
    if set("GEMINI_CLI") {
        return Some(known("Gemini CLI"));
    }
    None
}

/// One process this one runs under.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Process {
    /// Its program's name (`comm`), or path.
    pub comm: String,
    /// Its arguments, the program first.
    pub args: Vec<String>,
}

/// The agent a process is: by its program's name, or, for an agent that
/// runs on Node, by its script's (`…/@anthropic-ai/claude-code/cli.js`).
fn from_process(process: &Process) -> Option<Agent> {
    let base = |path: &str| -> String {
        let name = path.rsplit('/').next().unwrap_or(path);
        name.strip_suffix(".js")
            .or_else(|| name.strip_suffix(".mjs"))
            .or_else(|| name.strip_suffix(".cjs"))
            .unwrap_or(name)
            .to_lowercase()
    };
    let mut names = vec![base(&process.comm)];
    names.extend(process.args.iter().take(2).map(|arg| base(arg)));
    for (title, _, programs) in AGENTS {
        if names.iter().any(|name| programs.contains(&name.as_str())) {
            return Some(known(title));
        }
    }
    let packages = [
        ("@anthropic-ai/claude-code/", "Claude Code"),
        ("@openai/codex/", "Codex"),
        ("@google/gemini-cli/", "Gemini CLI"),
        ("@sourcegraph/amp/", "Amp"),
    ];
    process.args.iter().take(2).find_map(|arg| {
        packages
            .iter()
            .find(|(package, _)| arg.contains(package))
            .map(|(_, title)| known(title))
    })
}

/// The agent running this command: its environment's markers first, then
/// the processes it runs under, nearest first; [`UNKNOWN_AGENT`] when
/// neither says.
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

#[cfg(test)]
mod login_tests {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    use super::*;

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    /// An agent tool that keeps its servers, as each keeps them in its own
    /// configuration, and refuses to add one it has.
    #[derive(Default)]
    struct FakeTool {
        installed: bool,
        servers: RefCell<HashMap<String, Vec<String>>>,
        ran: RefCell<Vec<Vec<String>>>,
    }

    impl Runner for FakeTool {
        fn find(&self, program: &str) -> Option<PathBuf> {
            self.installed
                .then(|| PathBuf::from(format!("/usr/bin/{program}")))
        }

        fn run(&self, _: &Path, args: &[OsString]) -> Result<(bool, String)> {
            let args = strings(args);
            self.ran.borrow_mut().push(args.clone());
            let mut servers = self.servers.borrow_mut();
            let rest: Vec<String> = args
                .iter()
                .skip(2)
                .filter(|arg| !arg.starts_with("--"))
                .filter(|arg| *arg != "user")
                .cloned()
                .collect();
            match args.get(1).map(String::as_str) {
                Some("add") => {
                    let name = rest[0].clone();
                    if servers.contains_key(&name) {
                        return Ok((
                            false,
                            format!("MCP server {name} already exists in user config"),
                        ));
                    }
                    servers.insert(name, rest[1..].to_vec());
                    Ok((true, String::new()))
                }
                Some("remove") => Ok((servers.remove(&rest[0]).is_some(), String::new())),
                _ => Ok((false, "unknown command".into())),
            }
        }
    }

    #[test]
    fn tilo_mcp_is_registered_as_the_server_tilo_in_each_tools_own_words() {
        let tilo = Path::new("/home/me/.local/bin/tilo");
        assert_eq!(
            strings(&Tool::Claude.add("tilo", tilo, &["mcp"])),
            [
                "mcp",
                "add",
                "--scope",
                "user",
                "tilo",
                "--",
                "/home/me/.local/bin/tilo",
                "mcp"
            ]
        );
        assert_eq!(
            strings(&Tool::Codex.add("tilo", tilo, &["mcp"])),
            [
                "mcp",
                "add",
                "tilo",
                "--",
                "/home/me/.local/bin/tilo",
                "mcp"
            ]
        );
        assert_eq!(
            strings(&Tool::Gemini.add("tilo", tilo, &["mcp"])),
            [
                "mcp",
                "add",
                "--scope",
                "user",
                "tilo",
                "/home/me/.local/bin/tilo",
                "mcp"
            ]
        );
        assert_eq!(
            strings(&Tool::Codex.remove("tilo")),
            ["mcp", "remove", "tilo"]
        );
    }

    #[test]
    fn registering_again_points_the_server_at_this_program() {
        let tool = FakeTool {
            installed: true,
            ..FakeTool::default()
        };
        let old = Path::new("/opt/old/tilo");
        let new = Path::new("/home/me/.local/bin/tilo");
        assert_eq!(register(Tool::Claude, old, &tool), Registered::Added);
        assert_eq!(register(Tool::Claude, new, &tool), Registered::Already);
        assert_eq!(
            tool.servers.borrow()["tilo"],
            ["/home/me/.local/bin/tilo", "mcp"]
        );
        assert_eq!(register(Tool::Claude, new, &tool), Registered::Already);
        assert_eq!(
            register(Tool::Codex, new, &FakeTool::default()),
            Registered::Missing
        );
        assert!(
            tool.ran
                .borrow()
                .iter()
                .any(|args| args[..2] == ["mcp", "remove"])
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
        assert_eq!(found, known("Claude Code"));
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
        let cursor = Process {
            comm: "cursor-agent".into(),
            args: Vec::new(),
        };
        let found = detect(&none, vec![shell.clone(), cursor]);
        assert_eq!((found.name.as_str(), found.tool), ("Cursor", None));
        let goose = Process {
            comm: "goose".into(),
            args: vec!["/Users/me/.local/bin/goose".into(), "session".into()],
        };
        assert_eq!(detect(&none, vec![goose]).name, "Goose");
        assert_eq!(detect(&none, vec![shell]).name, UNKNOWN_AGENT);
    }

    #[test]
    fn agent_names_the_known_ones_by_name_or_program_and_keeps_others() {
        assert_eq!(named("claude"), known("Claude Code"));
        assert_eq!(named("codex").tool, Some(Tool::Codex));
        assert_eq!(named(" Gemini CLI ").tool, Some(Tool::Gemini));
        assert_eq!(
            named("Hermes"),
            Agent {
                name: "Hermes".into(),
                tool: None
            }
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{Remote, Tool, workspace_id};

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
