//! `tilo agent`: the agents you run, and the models they run on.
//!
//! An agent adds Tilo's MCP server to itself, where its own configuration keeps servers: `tilo
//! login` tells it the server, and `check` reads back what it wrote and asks the server it names
//! whether it answers. `models` is the model list Settings shows, through the program that keeps
//! it.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use serde_json::{Value, json};

use crate::nouns;

/// The MCP server an agent tool starts.
pub const MCP_SERVER: &str = "tilo-runtime";
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
    /// Say whether an agent's MCP configuration holds Tilo's server, and
    /// whether it answers: what `tilo login` leaves that agent to confirm.
    #[command(
        after_help = "Examples:\n  tilo agent check --config ~/.config/my-agent/mcp.json\n  \
                      tilo agent check\n\n\
                      With --config Tilo reads that file for its server `tilo`, says what it runs, \
                      and tells the agent what to do first. Without it, Tilo says whether its own \
                      server answers here and how to add it. The answer is for the agent to read: \
                      a line that failed says how to add the server."
    )]
    Check {
        /// The agent checking, by the name the person's workspace shows it
        /// under.
        #[arg(long)]
        agent: Option<String>,
        /// The agent's own configuration file, to look for Tilo's server in.
        #[arg(long, value_name = "PATH")]
        config: Option<PathBuf>,
    },
    /// The models agents run on, local or hosted.
    Models {
        #[command(subcommand)]
        cmd: ModelsCmd,
    },
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
        AgentCmd::Check { agent, config } => check(&given(agent.as_deref()), config.as_deref()),
        AgentCmd::Models { cmd } => models(cmd),
    }
}

/// `tilo agent check [--agent NAME] [--config PATH]`: whether Tilo's MCP
/// server is in the agent's configuration, and answers.
fn check(agent: &Agent, config: Option<&Path>) -> Result<ExitCode> {
    let tilo = std::env::current_exe().context("finding this program")?;
    if let Some(config) = config {
        return Ok(check_config(agent, config, &tilo));
    }
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
    Ok(ExitCode::FAILURE)
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
    let Ok(text) = std::fs::read_to_string(config) else {
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
    println!();
    println!("{}", crate::login::first_act(agent));
    ExitCode::SUCCESS
}

/// The server to add, for the agent to put where its own configuration keeps servers.
fn how_to_add_any(name: &str, tilo: &Path) -> String {
    format!(
        "Add Tilo's MCP server to {name}, as the server {SERVER}: the stdio server {} with the \
         argument mcp, or the streamable HTTP server {MCP_URL}. Put it where your own MCP \
         configuration keeps servers, beside the ones already there, with your own tooling.",
        tilo.display()
    )
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

/// A server as an agent's configuration writes it down.
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
    // Some configurations name the program and its arguments in one list.
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

/// The server `name` anywhere in a configuration: the answer an agent gets
/// of its own work.
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

// The agents

/// A tool a person runs an agent in, whose own command adds MCP servers: what `tilo login` adds
/// Tilo's server with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    /// Claude Code.
    Claude,
    /// Codex.
    Codex,
    /// Gemini CLI.
    Gemini,
    /// Cursor, the CLI and the editor.
    Cursor,
    /// `OpenCode`.
    OpenCode,
    /// Amp.
    Amp,
}

impl Tool {
    /// Every tool Tilo adds itself to.
    pub const ALL: &'static [Self] = &[
        Self::Claude,
        Self::Codex,
        Self::Gemini,
        Self::Cursor,
        Self::OpenCode,
        Self::Amp,
    ];

    /// As the person's workspace names it.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::Gemini => "Gemini CLI",
            Self::Cursor => "Cursor",
            Self::OpenCode => "OpenCode",
            Self::Amp => "Amp",
        }
    }

    /// How the person starts it again with the conversation kept, since it reads its servers
    /// only as it starts.
    #[must_use]
    pub fn restart(self) -> &'static str {
        match self {
            Self::Claude => "/exit, then `claude --continue`",
            Self::Codex => "/quit, then `codex resume --last`",
            Self::Gemini => "/quit, then `gemini --resume latest`",
            Self::OpenCode => "/exit, then `opencode --continue`",
            Self::Cursor => "start `cursor-agent` again; it reads its servers as it starts",
            Self::Amp => "nothing: Amp reloads its servers as it runs",
        }
    }

    /// The programs its process runs under, as a process tree names them.
    fn programs(self) -> &'static [&'static str] {
        match self {
            Self::Claude => &["claude", "claude-code"],
            Self::Codex => &["codex"],
            Self::Gemini => &["gemini", "gemini-cli"],
            Self::Cursor => &["cursor-agent", "cursor"],
            Self::OpenCode => &["opencode"],
            Self::Amp => &["amp"],
        }
    }

    /// The program that adds a server with its own command.
    fn cli(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::Cursor => "cursor-agent",
            Self::OpenCode => "opencode",
            Self::Amp => "amp",
        }
    }

    /// The tool's own command that registers the stdio server `name` running `program args`.
    fn cli_add(self, name: &str, program: &Path, args: &[&str]) -> Vec<std::ffi::OsString> {
        let mut add: Vec<std::ffi::OsString> = match self {
            Self::Claude | Self::Gemini => ["mcp", "add", "--scope", "user"]
                .map(std::ffi::OsString::from)
                .to_vec(),
            Self::Codex | Self::Amp | Self::OpenCode | Self::Cursor => {
                ["mcp", "add"].map(std::ffi::OsString::from).to_vec()
            }
        };
        add.push(name.into());
        if self == Self::OpenCode {
            add.push("--global".into());
        }
        // Gemini CLI takes the command and its arguments positionally; the rest after a `--`.
        if self != Self::Gemini {
            add.push("--".into());
        }
        add.push(program.as_os_str().to_owned());
        add.extend(args.iter().map(std::ffi::OsString::from));
        add
    }

    /// The file this tool keeps its MCP servers in, where Tilo can read it back: `None` for one
    /// that keeps them in a shape `tilo agent check --config` does not read.
    fn config(self) -> Option<PathBuf> {
        let home = PathBuf::from(std::env::var_os("HOME")?);
        let config =
            std::env::var_os("XDG_CONFIG_HOME").map_or_else(|| home.join(".config"), PathBuf::from);
        Some(match self {
            Self::Claude => home.join(".claude.json"),
            Self::Gemini => home.join(".gemini/settings.json"),
            Self::Cursor => home.join(".cursor/mcp.json"),
            Self::OpenCode => config.join("opencode/opencode.json"),
            Self::Amp => config.join("amp/settings.json"),
            // Codex keeps its servers in TOML, which the check does not read.
            Self::Codex => return None,
        })
    }
}

/// What became of Tilo's tools for an agent.
#[derive(Debug, PartialEq, Eq)]
pub enum Added {
    /// The tool's own configuration holds the server, and its check confirmed it.
    Verified,
    /// The command was run, and Tilo could not read the configuration back.
    Unverified,
    /// Adding the server did not work, for the reason given.
    Failed(String),
}

/// Add Tilo's MCP server to `tool` with the tool's own command, and confirm it where Tilo can
/// read the configuration back (`tilo agent check --config`).
#[must_use]
pub fn add_tools(tool: Tool, tilo: &Path) -> Added {
    let program = tool.cli();
    let output = Command::new(program)
        .args(tool.cli_add(SERVER, tilo, &["mcp"]))
        .output();
    match output {
        Ok(output) => {
            let said = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            if !output.status.success() && !said.contains("already exists") {
                return Added::Failed(
                    said.trim()
                        .lines()
                        .next()
                        .unwrap_or("its own command did not add it")
                        .to_owned(),
                );
            }
        }
        Err(error) => return Added::Failed(format!("{program} did not run: {error}")),
    }
    let Some(config) = tool.config() else {
        return Added::Unverified;
    };
    // What the tool's own configuration holds is what counts, not that a command said it
    // worked: the check reads it back and asks the server it names.
    let checked = Command::new(tilo)
        .args(["agent", "check", "--config"])
        .arg(&config)
        .output();
    match checked {
        Ok(checked) => {
            print!("{}", String::from_utf8_lossy(&checked.stdout));
            if checked.status.success() {
                Added::Verified
            } else {
                Added::Failed(format!(
                    "{} does not hold Tilo's server yet",
                    config.display()
                ))
            }
        }
        Err(error) => Added::Failed(format!("tilo agent check did not run: {error}")),
    }
}

/// The tool running this command: its environment's markers first, then the processes it runs
/// under, nearest first.
#[must_use]
pub fn detect_here() -> Option<Tool> {
    from_env().or_else(|| {
        ancestors()
            .into_iter()
            .find_map(|process| from_process(&process))
    })
}

/// The tool the environment says runs this: each sets its own markers in the commands it runs.
fn from_env() -> Option<Tool> {
    let set = |name: &str| std::env::var(name).is_ok_and(|value| !value.is_empty());
    if std::env::var("CLAUDECODE").as_deref() == Ok("1") || set("CLAUDE_CODE_ENTRYPOINT") {
        return Some(Tool::Claude);
    }
    if [
        "CODEX_SANDBOX",
        "CODEX_MANAGED_BY_NPM",
        "CODEX_MANAGED_BY_BUN",
    ]
    .into_iter()
    .any(set)
    {
        return Some(Tool::Codex);
    }
    if set("GEMINI_CLI") {
        return Some(Tool::Gemini);
    }
    if set("CURSOR_AGENT") {
        return Some(Tool::Cursor);
    }
    if set("OPENCODE") {
        return Some(Tool::OpenCode);
    }
    None
}

/// The tool a process is: by its program's name, or, for an agent that runs on Node, by its
/// script's.
fn from_process(process: &Process) -> Option<Tool> {
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
    let found = Tool::ALL.iter().copied().find(|tool| {
        names
            .iter()
            .any(|name| tool.programs().contains(&name.as_str()))
    });
    // An agent that runs on Node names its tool in the script it runs
    // (`…/@anthropic-ai/claude-code/cli.js`).
    found.or_else(|| {
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
                .map(|(_, tool)| *tool)
        })
    })
}

/// One process this one runs under.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Process {
    /// Its program's name (`comm`).
    pub comm: String,
    /// Its arguments, the program first.
    pub args: Vec<String>,
}

/// How far up the process tree to look.
const ANCESTORS: usize = 24;

/// The processes this one runs under, its parent first.
#[cfg(target_os = "linux")]
fn ancestors() -> Vec<Process> {
    let mut found = Vec::new();
    let mut pid = std::os::unix::process::parent_id();
    while pid > 1 && found.len() < ANCESTORS {
        let proc = PathBuf::from(format!("/proc/{pid}"));
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

/// The agent `tilo login` and `tilo agent check` run for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agent {
    /// As the person's workspace names it: `Reviewer`.
    pub name: String,
    /// The tool it runs in, when this command runs inside one, whose own configuration Tilo adds
    /// its server to.
    pub tool: Option<Tool>,
}

/// What `tilo login` calls an agent that gave no name.
pub const UNKNOWN_AGENT: &str = "Your agent";

/// The agent `--agent` names, as given.
#[must_use]
pub fn named(name: &str) -> Agent {
    Agent {
        name: name.trim().chars().take(64).collect(),
        tool: None,
    }
}

/// The agent `--agent` names, or [`UNKNOWN_AGENT`] when it names none, run by the tool this
/// command runs in.
#[must_use]
pub fn given(name: Option<&str>) -> Agent {
    let tool = detect_here();
    match name.filter(|name| !name.trim().is_empty()) {
        Some(name) => Agent {
            tool,
            ..named(name)
        },
        None => Agent {
            tool,
            ..named(UNKNOWN_AGENT)
        },
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

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

    /// A server a configuration names is asked, and says its name and tools.
    #[cfg(unix)]
    #[test]
    fn a_configured_server_is_asked_what_it_has() {
        let root = tempfile::tempdir().unwrap().keep();
        let server = answering_server(&root);
        assert_eq!(
            answers(&server, &[]),
            Ok(Answers {
                name: "tilo".to_owned(),
                tools: vec!["tilo_ls".to_owned(), "tilo_do".to_owned()],
                note: None
            })
        );
        assert!(answers(Path::new("/nonexistent/tilo"), &[]).is_err());
    }

    #[test]
    fn the_server_is_found_wherever_a_configuration_keeps_it() {
        let text = "{\n  \"tools\": {\n    \"mcp\": {\n      \"tilo\": {\"command\": \"/opt/tilo\", \
                    \"args\": [\"mcp\"]}\n    }\n  }\n}\n";
        assert_eq!(
            find_server(text, SERVER),
            Some(Server {
                program: PathBuf::from("/opt/tilo"),
                args: vec!["mcp".to_owned()]
            })
        );
        // A configuration that names the program and its arguments in one list.
        let listed = "{\"mcp\": {\"tilo\": {\"command\": [\"/opt/tilo\", \"mcp\"]}}}";
        assert_eq!(
            find_server(listed, SERVER).map(|server| server.args),
            Some(vec!["mcp".to_owned()])
        );
        assert_eq!(find_server("{}", SERVER), None);
    }

    #[test]
    fn a_tool_adds_tilos_server_with_its_own_command() {
        let words = |tool: Tool| -> Vec<String> {
            tool.cli_add(SERVER, Path::new("/opt/tilo"), &["mcp"])
                .iter()
                .map(|word| word.to_string_lossy().into_owned())
                .collect()
        };
        assert_eq!(
            words(Tool::Claude),
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
            words(Tool::Codex),
            ["mcp", "add", "tilo", "--", "/opt/tilo", "mcp"]
        );
        // Gemini CLI takes the command positionally, without the `--`.
        assert_eq!(
            words(Tool::Gemini),
            ["mcp", "add", "--scope", "user", "tilo", "/opt/tilo", "mcp"]
        );
    }

    #[test]
    fn a_tool_is_recognized_by_the_process_it_runs_as() {
        let as_claude = Process {
            comm: "claude".into(),
            args: vec!["claude".into()],
        };
        assert_eq!(from_process(&as_claude), Some(Tool::Claude));
        let on_node = Process {
            comm: "node".into(),
            args: vec![
                "node".into(),
                "/usr/lib/node_modules/@anthropic-ai/claude-code/cli.js".into(),
            ],
        };
        assert_eq!(from_process(&on_node), Some(Tool::Claude));
        assert_eq!(
            from_process(&Process {
                comm: "bash".into(),
                args: vec!["bash".into()],
            }),
            None
        );
    }

    #[test]
    fn an_agent_is_named_as_given() {
        assert_eq!(given(Some("  Reviewer ")).name, "Reviewer");
        assert_eq!(given(Some("  ")).name, UNKNOWN_AGENT);
        assert_eq!(given(None).name, UNKNOWN_AGENT);
    }
}
