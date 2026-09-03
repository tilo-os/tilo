//! `qos-agent` — see the crate docs in `lib.rs`.

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
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
use qos_agent::{Config, Session};

/// A participant without a window: joins a word-processor document through
/// the session's collaboration broker and carries out instructions written
/// to it ("Claude: turn the bullets into a table").
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
    #[arg(long, default_value = "Claude")]
    name: String,
    /// Explicit typed Cap'n Proto broker socket for headless diagnostics.
    /// Normal launched sessions inherit `QOS_SVC_COLLAB_FD` instead.
    #[arg(long)]
    endpoint: Option<PathBuf>,
    /// Pause between edits, in milliseconds, so the caret can be watched.
    #[arg(long, default_value_t = 250)]
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
    #[arg(long, default_value_t = 9000)]
    model_timeout_ms: u64,
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
    let cli = Cli::parse();
    // What this run's invitation actually was: write on the app it was
    // asked to join, and nothing else. Widening this is a deployment
    // decision, not something a headless participant grants itself.
    let permissions = {
        use ui::permissions::{Actor, CapOverride, PermissionContext, Scope, Subject, Verb};
        PermissionContext::from_capabilities(
            Actor::agent(&cli.name),
            vec![
                CapOverride::grant(
                    Subject::agent(&cli.name),
                    Scope::App(cli.app.clone()),
                    Verb::Write,
                )
                .with_id("invited"),
            ],
        )
    };
    let cfg = Config {
        endpoint: cli.endpoint,
        endpoint_identity: None,
        app_id: cli.app,
        cluster: cli.workspace,
        record: cli.record,
        name: cli.name,
        pace: Duration::from_millis(cli.pace_ms),
        once: cli.once,
        model: !cli.no_model,
        model_timeout: Duration::from_millis(cli.model_timeout_ms),
        await_document: Duration::from_millis(cli.await_document_ms),
        settle: Duration::from_millis(cli.settle_ms),
        directory: None,
        permissions,
    };
    let mut session = Session::connect(cfg)?;
    if cli.task_runs {
        session.run_task_runs()
    } else {
        session.run()
    }
}
