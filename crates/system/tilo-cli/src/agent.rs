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

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Tool {
    /// Claude Code.
    Claude,
    /// Codex.
    Codex,
}

impl Tool {
    fn command(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
        }
    }

    /// The tool's own command that registers `door` as the server `tilo`,
    /// or, for another device, as `tilo-<device>` carried to that device's
    /// door.
    fn registration(self, door: &std::path::Path, remote: Option<&Remote>) -> Vec<OsString> {
        let name = remote.map_or_else(|| "tilo".to_owned(), Remote::server_name);
        let mut args: Vec<OsString> = match self {
            // In every directory, not only this one.
            Self::Claude => ["mcp", "add", "--scope", "user"]
                .map(OsString::from)
                .to_vec(),
            Self::Codex => ["mcp", "add"].map(OsString::from).to_vec(),
        };
        args.push(name.into());
        args.push("--".into());
        args.push(door.as_os_str().to_owned());
        args.push("--connect".into());
        if let Some(remote) = remote {
            args.push("--device".into());
            args.push(remote.device.clone().into());
            if let Some(workspace) = &remote.workspace {
                args.push("--workspace".into());
                args.push(workspace.clone().into());
            }
        }
        args
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

/// The agent tool's own command that registers `tilo mcp` (this program)
/// as the server `tilo`.
fn login_registration(tool: Tool, tilo: &std::path::Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = match tool {
        Tool::Claude => ["mcp", "add", "--scope", "user"]
            .map(OsString::from)
            .to_vec(),
        Tool::Codex => ["mcp", "add"].map(OsString::from).to_vec(),
    };
    args.extend([
        "tilo".into(),
        "--".into(),
        tilo.as_os_str().to_owned(),
        "mcp".into(),
    ]);
    args
}

fn connect_login(tool: Tool) -> Result<ExitCode> {
    if nouns::program(tool.command()).is_none() {
        bail!(
            "{} isn't installed here; install it, then run this again",
            tool.title()
        );
    }
    let tilo = std::env::current_exe().context("finding this program")?;
    let output = Command::new(tool.command())
        .args(login_registration(tool, &tilo))
        .output()
        .with_context(|| format!("running {}", tool.command()))?;
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if output.status.success() || said.contains("already exists") {
        println!(
            "{} now has Tilo's tools (MCP server tilo). If it loads tools only when it starts, restart it; \
             then its tilo_status tool says whom it acts as.",
            tool.title()
        );
        return Ok(ExitCode::SUCCESS);
    }
    eprint!("{said}");
    Ok(crate::exit_code(output.status.code()))
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
    use super::*;

    #[test]
    fn a_logged_in_computer_registers_tilo_mcp_as_the_server_tilo() {
        let tilo = std::path::Path::new("/home/me/.local/bin/tilo");
        assert_eq!(
            login_registration(Tool::Claude, tilo),
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
            .map(OsString::from)
        );
        assert_eq!(
            login_registration(Tool::Codex, tilo),
            [
                "mcp",
                "add",
                "tilo",
                "--",
                "/home/me/.local/bin/tilo",
                "mcp"
            ]
            .map(OsString::from)
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
