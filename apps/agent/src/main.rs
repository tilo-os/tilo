//! `tilo-agent` — see the crate docs in `lib.rs`.

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
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

use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use tilo_agent::{Config, Session};

/// A participant without a window: joins the session's collaboration broker
/// and handles addressed document instructions or canonical package-primary
/// Chat summons (`--app so.tilo.chat`, no `--record`).
#[derive(Parser, Debug)]
#[command(name = "tilo-agent")]
struct Cli {
    /// The room (workspace) the document lives in — the `TILO_WORKSPACE_ID`
    /// the app was launched with.
    #[arg(long)]
    workspace: uuid::Uuid,
    /// The record the app announced for the document, e.g.
    /// `file:/var/workspace/Demo/brief.docx`. Omit for an untitled document.
    #[arg(long)]
    record: Option<String>,
    /// The app whose document this is (its manifest package id).
    #[arg(long, default_value = "so.tilo.word-processor")]
    app: String,
    /// The display name used by name-addressed instructions. Structured
    /// mentions and authorship use the stable --agent-id.
    #[arg(long, default_value = "Claude")]
    name: String,
    /// Stable admitted agent id. Defaults to the name for existing launchers.
    #[arg(long)]
    agent_id: Option<String>,
    /// Explicit typed Cap'n Proto broker socket for headless diagnostics.
    /// Normal launched sessions inherit `TILO_SVC_COLLAB_FD` instead.
    #[arg(long)]
    endpoint: Option<PathBuf>,
    /// Pause after each action and edit, in milliseconds, so the caret can be
    /// watched. A demo affordance: by default the agent acts at full speed.
    #[arg(long, default_value_t = tilo_agent::session::DEFAULT_PACE_MS)]
    pace_ms: u64,
    /// Exit after the first instruction has been carried out.
    #[arg(long)]
    once: bool,
    /// Receive broker-minted Task Center leases instead of watching a
    /// document. This mode uses only the agent's admitted, permission-
    /// filtered action catalogue and cannot infer work from room traffic.
    #[arg(long)]
    task_runs: bool,
    /// Decide by keyword only, without asking a model. The run is then
    /// reproducible and needs no network — and says so in its log.
    #[arg(long)]
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
    /// Default 9000 ms; a managed (launcher-started) agent defaults to
    /// 30000 ms, the whole budget of a multi-turn action decision.
    #[arg(long)]
    model_timeout_ms: Option<u64>,
    /// How long to wait for the room to answer with the document before
    /// giving up and naming the address nobody answered on. `0` waits
    /// forever, which is right when the document may not be open yet and
    /// wrong in a script.
    #[arg(long, default_value_t = 60_000)]
    await_document_ms: u64,
    /// How long a paragraph must stop changing before it is read as a
    /// finished request, in milliseconds. `0` acts on first sight, right
    /// for a page seeded before the agent joins.
    #[arg(long, default_value_t = 0)]
    settle_ms: u64,
}

/// The total model budget of one managed action decision (all its turns).
/// The launcher passes no `--model-timeout-ms`, so this is the default there.
const MANAGED_MODEL_TIMEOUT_MS: u64 = 30_000;

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
    let managed = std::env::var_os("TILO_MANAGED_AGENT_CONTEXT").is_some();
    let model_timeout_ms = cli.model_timeout_ms.unwrap_or(if managed {
        MANAGED_MODEL_TIMEOUT_MS
    } else {
        9000
    });
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
        use ui::permissions::{Actor, CapOverride, PermissionContext, Scope, Subject, Verb};
        PermissionContext::from_capabilities(
            Actor::agent(&agent_id),
            vec![
                CapOverride::grant(
                    Subject::agent(&agent_id),
                    Scope::App(cli.app.clone()),
                    Verb::Write,
                )
                .with_id("invited"),
            ],
        )
    };
    let cfg = Config {
        agent_id,
        endpoint: cli.endpoint,
        endpoint_identity: None,
        app_id: cli.app,
        cluster: cli.workspace,
        record: cli.record,
        name: cli.name,
        pace: Duration::from_millis(cli.pace_ms),
        once: cli.once,
        model: !cli.no_model,
        model_timeout: Duration::from_millis(model_timeout_ms),
        await_document: Duration::from_millis(cli.await_document_ms),
        settle: Duration::from_millis(cli.settle_ms),
        permissions,
        content: device_content(),
    };
    let mut session = Session::connect(cfg)?;
    if cli.task_runs {
        session.run_task_runs()
    } else {
        session.run()
    }
}

/// The device content store the launcher handed this agent: where the
/// content addresses the room's apps hand it resolve. An agent started
/// without one reads only what it stores itself, in memory.
fn device_content() -> tilo_content::ContentStore {
    if let Some(device) = tilo_content::ContentStore::device() {
        return device;
    }
    tilo_content::ContentStore::memory()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pace is a demo affordance: an agent started without `--pace-ms`
    /// (as every launcher starts it) never pauses after an action.
    #[test]
    fn an_agent_started_without_a_pace_acts_at_full_speed() {
        let cli = Cli::try_parse_from([
            "tilo-agent",
            "--workspace",
            "00000000-0000-0000-0000-000000000001",
        ])
        .unwrap();
        assert_eq!(Duration::from_millis(cli.pace_ms), Duration::ZERO);
        let demo = Cli::try_parse_from([
            "tilo-agent",
            "--workspace",
            "00000000-0000-0000-0000-000000000001",
            "--pace-ms",
            "600",
        ])
        .unwrap();
        assert_eq!(demo.pace_ms, 600, "demos still choose a visible pace");
    }
}
