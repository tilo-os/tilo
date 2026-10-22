//! `tilo agent`: the agents you run, and the models they run on.
//!
//! `connect` registers this desktop's agent door with an agent tool you run
//! in a terminal, so it can act on the desktop through the same actions a
//! person has. `models` is the model list Settings shows, through the
//! program that keeps it.

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
    /// Let an agent you run in a terminal act on this desktop.
    #[command(
        after_help = "Examples:\n  tilo agent connect claude\n  tilo agent connect codex\n\n\
                      The agent then reaches this desktop's actions from any terminal in your login."
    )]
    Connect {
        /// The agent tool: claude or codex.
        tool: Tool,
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

    /// The tool's own command that registers `door` as the server `tilo`.
    fn registration(self, door: &std::path::Path) -> Vec<OsString> {
        let mut args: Vec<OsString> = match self {
            // In every directory, not only this one.
            Self::Claude => ["mcp", "add", "--scope", "user", "tilo", "--"]
                .map(OsString::from)
                .to_vec(),
            Self::Codex => ["mcp", "add", "tilo", "--"].map(OsString::from).to_vec(),
        };
        args.push(door.as_os_str().to_owned());
        args.push("--connect".into());
        args
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
        AgentCmd::Connect { tool } => connect(tool),
        AgentCmd::Models { cmd } => models(cmd),
    }
}

fn connect(tool: Tool) -> Result<ExitCode> {
    let Some(door) = nouns::program(DOOR) else {
        bail!("connecting an agent needs a Tilo desktop, which this computer doesn't have");
    };
    if nouns::program(tool.command()).is_none() {
        bail!(
            "{} isn't installed here; install it, then run this again",
            tool.title()
        );
    }
    let output = Command::new(tool.command())
        .args(tool.registration(&door))
        .output()
        .with_context(|| format!("running {}", tool.command()))?;
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if output.status.success() {
        println!(
            "{} can now act on this desktop from any terminal in your login.",
            tool.title()
        );
        return Ok(ExitCode::SUCCESS);
    }
    if said.contains("already exists") {
        println!("{} is already connected to this desktop.", tool.title());
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
mod tests {
    use super::Tool;

    /// Connecting registers the door, in every directory for Claude Code,
    /// under the name `tilo` with `--connect`, so it reaches the login's
    /// own desktop and never starts one.
    #[test]
    fn connecting_registers_the_desktops_door() {
        let door = std::path::Path::new("/usr/libexec/tilo/tilo-runtime-mcp");
        let claude: Vec<String> = Tool::Claude
            .registration(door)
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
            .registration(door)
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
}
