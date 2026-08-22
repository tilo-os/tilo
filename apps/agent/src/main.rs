//! `qos-agent` — see the crate docs in `lib.rs`.

use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use qos_agent::{Config, Session};

/// A participant without a window: joins a word-processor document through
/// the session's collaboration broker and carries out instructions written
/// to it ("Scribe: turn the bullets into a table").
#[derive(Parser, Debug)]
#[command(name = "qos-agent")]
struct Cli {
    /// The room (workspace) the document lives in — the `QOS_WORKSPACE_ID`
    /// the app was launched with.
    #[arg(long)]
    workspace: uuid::Uuid,
    /// The record the app announced for the document, e.g.
    /// `file:/var/workspace/Demo/brief.docx`. Omit for an untitled document.
    #[arg(long)]
    record: Option<String>,
    /// The app whose document this is (its manifest package id).
    #[arg(long, default_value = "dev.qos.word-processor")]
    app: String,
    /// The agent's name: its caret pill, its history author, and the word
    /// an instruction starts with.
    #[arg(long, default_value = "Scribe")]
    name: String,
    /// The broker socket. Defaults to the session's `collab-json.sock`
    /// (or `$QUICK_COLLAB_SOCK`).
    #[arg(long)]
    socket: Option<PathBuf>,
    /// Pause between edits, in milliseconds, so the caret can be watched.
    #[arg(long, default_value_t = 250)]
    pace_ms: u64,
    /// Exit after the first instruction has been carried out.
    #[arg(long)]
    once: bool,
    /// Decide by keyword only, without asking a model. The run is then
    /// reproducible and needs no network — and says so in its log.
    #[arg(long)]
    no_model: bool,
    /// How long a model may take to choose before the keyword vocabulary
    /// decides instead. Short on purpose: an instruction must never wait on
    /// the network.
    #[arg(long, default_value_t = 6000)]
    model_timeout_ms: u64,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let cfg = Config {
        socket: cli.socket,
        app_id: cli.app,
        cluster: cli.workspace,
        record: cli.record,
        name: cli.name,
        pace: Duration::from_millis(cli.pace_ms),
        once: cli.once,
        model: !cli.no_model,
        model_timeout: Duration::from_millis(cli.model_timeout_ms),
    };
    let mut session = Session::connect(cfg)?;
    session.run()
}
