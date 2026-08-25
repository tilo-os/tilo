//! `qos-agent` — see the crate docs in `lib.rs`.

use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use qos_agent::{Config, Session};
use ui::permissions::{Actor, CapOverride, PermissionContext, Scope, Subject, Verb};

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
    ///
    /// Empty joins the room rather than one app: the broker narrows its
    /// fan-out to an app only when both ends name one, so a connection that
    /// announces nothing is handed every app's state and the agent can
    /// enumerate the room instead of a single document.
    #[arg(long, default_value = "dev.qos.word-processor")]
    app: String,
    /// The directory the agent is browsing, for the apps whose subject is
    /// the filesystem rather than a document. Its own view state, the
    /// counterpart of the caret it puts on a block.
    #[arg(long)]
    directory: Option<PathBuf>,
    /// An app the agent is allowed to act on, by resource namespace
    /// (`word-processor`, `files`). Repeatable.
    ///
    /// This is the capability its inviter issued, and it is the whole of
    /// what the agent may do: with none it enumerates nothing and performs
    /// nothing. It holds no role — an agent is not a person — so there is no
    /// base authority underneath these. Defaults to the app named by
    /// `--app`, which is the grant a launcher would hand an agent invited to
    /// one document.
    ///
    /// Naming an app here is leave to **open** it as well as to act in it:
    /// the scope is `Scope::App`, and opening asks about the application
    /// itself (`ResourceRef::App`) while an operation asks about the
    /// operation. So an agent that may work in the photo editor may also
    /// bring it into the room, and one granted a single action may not.
    #[arg(long = "grant")]
    grants: Vec<String>,
    /// The agent's name: its caret pill, its history author, and the word
    /// an instruction starts with.
    #[arg(long, default_value = "Claude")]
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
    /// How long deciding may take before the keyword vocabulary decides
    /// instead. The work must never wait on the network indefinitely — but it
    /// does have to wait long enough.
    ///
    /// This was 9000, set when the decision was a one-of-three pick that
    /// came back in under a second. Drafting a table is a real reading of
    /// the document and takes around eight, which put the deadline inside
    /// the spread: the call would land in time most runs and not in
    /// others, and a run that misses falls back to the keyword table —
    /// silently producing the very output the model is there to better,
    /// while the log still says a model was asked. A margin that wide is
    /// cheap; being on the wrong side of it during a demonstration is not.
    ///
    /// It now bounds a **conversation** rather than one call: the room is
    /// asked about through named tools and answered in one turn, and the
    /// answer comes back in the next. Two or three round trips is the
    /// ordinary shape. So the same reasoning that moved this off 9000 moves
    /// it off 30000 — a deadline inside the spread is worse than a generous
    /// one, because what a run that misses produces is the keyword
    /// vocabulary's answer under a log line saying a model was asked.
    #[arg(long, default_value_t = 75_000)]
    model_timeout_ms: u64,
    /// How long an addressed line must stop changing before it is read as
    /// a request.
    ///
    /// A person types the colon several seconds before the request, and
    /// the agent is handed a delta per keystroke — so without a pause here
    /// it answers the half-typed line and then ignores the finished one.
    /// The default outlasts the pauses inside real typing and is still
    /// short enough that nobody watching notices it. `0` reads a line the
    /// moment it arrives, which is right only where the instruction was
    /// written before the agent started.
    #[arg(long, default_value_t = 800)]
    settle_ms: u64,
    /// How long to wait for the room to produce something addressed to
    /// this agent before giving up and saying what it did see — nobody
    /// answered, the page that answered is missing history it was built
    /// on, or the page is whole and simply says nothing to this name.
    /// `0` waits forever, which is right for an agent left running in a
    /// room and wrong in a script.
    #[arg(long, default_value_t = 60_000)]
    await_document_ms: u64,
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
    // Who the agent is, and what it was given leave to do. `Actor::agent`
    // carries no role, so every capability below is one somebody granted
    // explicitly; `decide_capability` denies anything they do not cover.
    let subject = Subject::agent(&cli.name);
    let mut grants: Vec<String> = cli.grants.clone();
    if grants.is_empty() {
        grants.extend(
            qos_agent::surface::for_app_id(&cli.app).map(|c| c.namespace.to_string()),
        );
    }
    let caps: Vec<CapOverride> = grants
        .iter()
        .map(|app| {
            CapOverride::grant(subject.clone(), Scope::App(app.clone()), Verb::Write)
                .with_id(format!("agent-{}-{app}", cli.name))
        })
        .collect();
    tracing::info!(
        agent = %cli.name,
        apps = ?grants,
        "acting under {} capability grant(s)",
        caps.len()
    );
    let permissions =
        PermissionContext::from_capabilities(Actor::agent(&cli.name), caps);
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
        settle: Duration::from_millis(cli.settle_ms),
        await_document: Duration::from_millis(cli.await_document_ms),
        directory: cli.directory,
        permissions,
    };
    let mut session = Session::connect(cfg)?;
    session.run()
}
