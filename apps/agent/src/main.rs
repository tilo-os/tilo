//! `tilo-agent` — see the crate docs in `lib.rs`.

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.
#![allow(
    clippy::cast_precision_loss,
    reason = "app-local arithmetic; see the note above"
)]
use std::path::PathBuf;
use std::time::Duration;

use tilo_agent::{Config, Session};

/// A participant without a window: joins the session's collaboration broker
/// and carries out the runs of issues assigned to it (`--task-runs`).
///
/// Read by hand rather than through an argument-parser crate: the launcher
/// passes a handful of long options, and a parser's builder, help renderer
/// and suggestion engine are a fifth of this tapp.
#[derive(Debug, PartialEq)]
struct Cli {
    /// The room (workspace) the document lives in — the `TILO_WORKSPACE_ID`
    /// the app was launched with.
    workspace: uuid::Uuid,
    /// The record the app announced for the document, e.g.
    /// `file:/var/workspace/Demo/brief.docx`. Omit for an untitled document.
    record: Option<String>,
    /// The app whose document this is (its manifest package id).
    app: String,
    /// The display name used by name-addressed instructions. Structured
    /// mentions and authorship use the stable --agent-id.
    name: String,
    /// Stable admitted agent id. Defaults to the name for existing launchers.
    agent_id: Option<String>,
    /// Explicit typed Cap'n Proto broker socket for headless diagnostics.
    /// Normal launched sessions inherit `TILO_SVC_COLLAB_FD` instead.
    endpoint: Option<PathBuf>,
    /// Receive the runs of issues assigned to it, the agent's only mode; every
    /// launcher passes it. A task uses only the agent's admitted, permission-
    /// filtered action catalogue and cannot infer work from room traffic.
    task_runs: bool,
    /// Decide by keyword only, without asking a model. The run is then
    /// reproducible and needs no network — and says so in its log.
    no_model: bool,
    /// How long a model may take to choose before the keyword vocabulary
    /// decides instead. Short on purpose: an instruction must never wait on
    /// the network.
    ///
    /// Bounds the retries too: a request an overloaded service turns away is
    /// made again with a short backoff, carved out of this deadline rather
    /// than added to it, so shortening this also shortens how long the agent
    /// will wait out a busy service.
    ///
    /// Default 9000 ms; a managed (launcher-started) agent, or one given
    /// `--provider` by the desktop, defaults to 30000 ms, the whole budget of
    /// a multi-turn action decision.
    model_timeout_ms: Option<u64>,
    /// An application the desktop started beside this agent for its tasks
    /// (web search). Repeatable. The agent plans its first run only once
    /// each has published its actions, waiting a bounded while, so that run
    /// is not planned without them.
    companions: Vec<String>,
    /// The model to plan with, as the desktop's nonsecret
    /// `{"profile_id","model_id","protocol"}` selection. The model gateway
    /// holds the endpoint and the credential; a launched agent cannot read
    /// Settings, so without this it plans by keyword only.
    provider: Option<String>,
}

/// What `tilo-agent --help` prints.
const USAGE: &str = "\
A participant without a window: joins the session's collaboration broker and
carries out the runs of issues assigned to it.

Usage: tilo-agent --workspace <UUID> --task-runs [OPTIONS]

Options:
      --workspace <UUID>          The room the document lives in
      --record <RECORD>           The record the app announced for the document
      --app <APP>                 The app whose document this is [default: so.tilo.word-processor]
      --name <NAME>               The display name instructions address [default: Claude]
      --agent-id <ID>             Stable admitted agent id [default: the name]
      --endpoint <PATH>           Broker socket for headless diagnostics
      --task-runs                 Receive the runs of assigned issues (required)
      --no-model                  Decide by keyword only, without asking a model
      --model-timeout-ms <MS>     How long a model may take to choose
      --companion <APP>           An application started beside this agent (repeatable)
      --provider <JSON>           The model selection the desktop hands over
  -h, --help                      Print this help
";

/// How reading the command line ends when it does not give a [`Cli`].
#[derive(Debug, PartialEq)]
enum CliExit {
    /// `--help`: print [`USAGE`] and succeed.
    Help,
    /// A usage error, said in one sentence.
    Usage(String),
}

impl Cli {
    /// Read the arguments after the program name. Options take their value
    /// as the next argument or after `=` (`--name=Claude`).
    fn parse_from(args: impl IntoIterator<Item = String>) -> Result<Self, CliExit> {
        let mut workspace = None;
        let mut cli = Cli {
            workspace: uuid::Uuid::nil(),
            record: None,
            app: "so.tilo.word-processor".to_owned(),
            name: "Claude".to_owned(),
            agent_id: None,
            endpoint: None,
            task_runs: false,
            no_model: false,
            model_timeout_ms: None,
            companions: Vec::new(),
            provider: None,
        };
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            let (option, inline) = match arg.split_once('=') {
                Some((option, value)) if option.starts_with("--") => {
                    (option.to_owned(), Some(value.to_owned()))
                }
                _ => (arg, None),
            };
            let flag = |set: &mut bool| {
                if inline.is_some() {
                    return Err(CliExit::Usage(format!("{option} takes no value")));
                }
                *set = true;
                Ok(())
            };
            match option.as_str() {
                "-h" | "--help" => return Err(CliExit::Help),
                "--task-runs" => flag(&mut cli.task_runs)?,
                "--no-model" => flag(&mut cli.no_model)?,
                "--workspace" | "--record" | "--app" | "--name" | "--agent-id" | "--endpoint"
                | "--model-timeout-ms" | "--companion" | "--provider" => {
                    let value = match inline {
                        Some(value) => value,
                        None => args
                            .next()
                            .ok_or_else(|| CliExit::Usage(format!("{option} needs a value")))?,
                    };
                    match option.as_str() {
                        "--workspace" => {
                            workspace = Some(value.parse().map_err(|error| {
                                CliExit::Usage(format!("--workspace {value}: {error}"))
                            })?);
                        }
                        "--record" => cli.record = Some(value),
                        "--app" => cli.app = value,
                        "--name" => cli.name = value,
                        "--agent-id" => cli.agent_id = Some(value),
                        "--endpoint" => cli.endpoint = Some(PathBuf::from(value)),
                        "--model-timeout-ms" => {
                            cli.model_timeout_ms = Some(value.parse().map_err(|error| {
                                CliExit::Usage(format!("--model-timeout-ms {value}: {error}"))
                            })?);
                        }
                        "--companion" => cli.companions.push(value),
                        _ => cli.provider = Some(value),
                    }
                }
                _ => return Err(CliExit::Usage(format!("unexpected argument {option}"))),
            }
        }
        cli.workspace =
            workspace.ok_or_else(|| CliExit::Usage("--workspace <UUID> is required".to_owned()))?;
        Ok(cli)
    }

    /// The process's own command line; prints the help or the usage error
    /// and ends the process when it does not give a [`Cli`].
    fn parse() -> Self {
        match Self::parse_from(std::env::args().skip(1)) {
            Ok(cli) => cli,
            Err(exit) => {
                let code = match exit {
                    CliExit::Help => {
                        print!("{USAGE}");
                        0
                    }
                    CliExit::Usage(error) => {
                        eprintln!("error: {error}\n\nFor more information, try '--help'.");
                        2
                    }
                };
                #[allow(
                    clippy::exit,
                    reason = "a command line that names no run is the whole run of the binary"
                )]
                std::process::exit(code);
            }
        }
    }
}

/// The model budget of one turn of a managed action decision: the most the
/// model gateway gives one provider call. The launcher passes no
/// `--model-timeout-ms`, so this is the default there. Hosted reasoning
/// models answer most turns in a few seconds but take 15 to 35 seconds on a
/// measurable share of them (Gemini on Vertex AI did on several turns of
/// one desktop session); a shorter cut fails those turns after the provider has
/// already done, and billed, the work.
const MANAGED_MODEL_TIMEOUT_MS: u64 = 60_000;

/// The model deadline when `--model-timeout-ms` is not given. A desktop that
/// hands over its model selection (`provider_given`) launched the agent for
/// its tasks, so it plans on the same multi-turn budget as a managed one.
fn default_model_timeout_ms(managed: bool, provider_given: bool) -> u64 {
    if managed || provider_given {
        MANAGED_MODEL_TIMEOUT_MS
    } else {
        9000
    }
}

#[cfg(feature = "thin")]
extern crate runtime_dylib;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let mut cli = Cli::parse();
    anyhow::ensure!(
        cli.task_runs,
        "tilo-agent runs assigned issues only: pass --task-runs"
    );
    let managed = std::env::var_os("TILO_MANAGED_AGENT_CONTEXT").is_some();
    let model_timeout_ms = cli
        .model_timeout_ms
        .unwrap_or_else(|| default_model_timeout_ms(managed, cli.provider.is_some()));
    if managed {
        anyhow::ensure!(
            cli.task_runs && !cli.no_model && cli.endpoint.is_none() && cli.record.is_none(),
            "managed agent requires task mode and inherited services"
        );
        cli.app = "so.tilo.agent".into();
    }
    // What this run's invitation actually was: write on the app it was
    // asked to join, and nothing else. Widening this is a deployment
    // decision, not something a headless participant grants itself.
    let agent_id = cli.agent_id.clone().unwrap_or_else(|| cli.name.clone());
    let permissions = {
        ui::permissions::PermissionContext::agent_granted_at(
            &agent_id,
            &[ui::permissions::TreePath::root().app(cli.app.clone())],
            ui::permissions::KindSet::READ_UNDOABLE,
        )
    };
    let provider = cli
        .provider
        .as_deref()
        .map(|raw| {
            serde_json::from_str(raw)
                .map_err(|error| error.to_string())
                .and_then(|value| tilo_agent::model::managed_provider_selection(&value))
                .map_err(|error| anyhow::anyhow!("--provider: {error}"))
        })
        .transpose()?;
    let cfg = Config {
        agent_id,
        provider,
        endpoint: cli.endpoint,
        endpoint_identity: None,
        app_id: cli.app,
        cluster: cli.workspace,
        record: cli.record,
        name: cli.name,
        model: !cli.no_model,
        model_timeout: Duration::from_millis(model_timeout_ms),
        permissions,
        content: device_content(),
        companions: cli.companions,
    };
    Session::connect(cfg)?.run_task_runs()
}

/// The device content store the launcher handed this agent: where the
/// content addresses the room's apps hand it resolve. An agent started
/// without one reads only what it stores itself, in memory.
fn device_content() -> ui::content::ContentStore {
    if let Some(device) = ui::content::device() {
        return device;
    }
    ui::content::ContentStore::memory()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, CliExit> {
        Cli::parse_from(args.iter().map(|arg| (*arg).to_owned()))
    }

    /// The launcher's own command line reads as before, with the defaults the
    /// flags had.
    #[test]
    fn the_launchers_command_line_reads_with_its_defaults() {
        let room = "6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b";
        let cli = parse(&[
            "--workspace",
            room,
            "--app",
            "so.tilo.agent",
            "--name",
            "agent-1",
            "--task-runs",
        ])
        .unwrap();
        assert_eq!(cli.workspace, room.parse::<uuid::Uuid>().unwrap());
        assert_eq!(cli.app, "so.tilo.agent");
        assert_eq!(cli.name, "agent-1");
        assert!(cli.task_runs);
        assert!(!cli.no_model);
        assert_eq!(cli.agent_id, None);
        assert_eq!(cli.model_timeout_ms, None);

        let defaults = parse(&["--workspace", room]).unwrap();
        assert_eq!(defaults.app, "so.tilo.word-processor");
        assert_eq!(defaults.name, "Claude");
        assert!(!defaults.task_runs);
    }

    /// Values after `=`, repeated companions and every option the flags took.
    #[test]
    fn every_option_reads_in_both_spellings() {
        let cli = parse(&[
            "--workspace=6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b",
            "--record=file:/var/workspace/Demo/brief.docx",
            "--agent-id",
            "stable",
            "--endpoint",
            "/run/broker.sock",
            "--no-model",
            "--model-timeout-ms=1500",
            "--companion",
            "so.tilo.web-search",
            "--companion=so.tilo.notes",
            "--provider",
            r#"{"profile_id":"p"}"#,
        ])
        .unwrap();
        assert_eq!(
            cli.record.as_deref(),
            Some("file:/var/workspace/Demo/brief.docx")
        );
        assert_eq!(cli.agent_id.as_deref(), Some("stable"));
        assert_eq!(cli.endpoint, Some(PathBuf::from("/run/broker.sock")));
        assert!(cli.no_model);
        assert_eq!(cli.model_timeout_ms, Some(1500));
        assert_eq!(cli.companions, ["so.tilo.web-search", "so.tilo.notes"]);
        assert_eq!(cli.provider.as_deref(), Some(r#"{"profile_id":"p"}"#));
    }

    /// A missing room, a missing value, a bad number or an unknown argument
    /// is a usage error, and `--help` asks for the help.
    #[test]
    fn a_wrong_command_line_is_a_usage_error() {
        let room = "6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b";
        assert!(matches!(parse(&["--task-runs"]), Err(CliExit::Usage(_))));
        assert!(matches!(parse(&["--workspace"]), Err(CliExit::Usage(_))));
        assert!(matches!(
            parse(&["--workspace", "not-a-uuid"]),
            Err(CliExit::Usage(_))
        ));
        assert!(matches!(
            parse(&["--workspace", room, "--model-timeout-ms", "soon"]),
            Err(CliExit::Usage(_))
        ));
        assert!(matches!(
            parse(&["--workspace", room, "--frobnicate"]),
            Err(CliExit::Usage(_))
        ));
        assert!(matches!(
            parse(&["--workspace", room, "--task-runs=yes"]),
            Err(CliExit::Usage(_))
        ));
        assert_eq!(parse(&["--help"]), Err(CliExit::Help));
        assert_eq!(parse(&["--workspace", room, "-h"]), Err(CliExit::Help));
    }

    /// A shell-launched agent given `--provider` gets a managed agent's
    /// budget: the short default would cut off a multi-turn plan midway.
    #[test]
    fn a_desktop_launched_agent_plans_on_the_managed_budget() {
        assert_eq!(default_model_timeout_ms(false, false), 9000);
        assert_eq!(
            default_model_timeout_ms(false, true),
            MANAGED_MODEL_TIMEOUT_MS
        );
        assert_eq!(
            default_model_timeout_ms(true, false),
            MANAGED_MODEL_TIMEOUT_MS
        );
    }
}
