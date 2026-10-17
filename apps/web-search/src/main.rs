//! `tilo-web-search` — see the crate docs in `lib.rs`.

use clap::Parser;
use tilo_files::{Dir, FsError, Lock, LockMode, VPath};
use tilo_web_search_app::actions::APP_ID;

#[cfg(feature = "thin")]
extern crate runtime_dylib;

/// Publishes web search and page reading as actions for the workspace's
/// agents and shortcuts.
#[derive(Parser, Debug)]
#[command(name = "tilo-web-search")]
struct Cli {
    /// The room (workspace) to publish in — the `TILO_WORKSPACE_ID` the
    /// service was launched with.
    #[arg(long)]
    workspace: uuid::Uuid,
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
    // It is started alongside each agent; one per workspace is enough, and
    // a second publishing the same actions would only compete for them.
    let _instance = match single_instance(cli.workspace) {
        Ok(Some(lock)) => Some(lock),
        Ok(None) => {
            tracing::info!("web search already runs in this workspace");
            return Ok(());
        }
        Err(error) => {
            tracing::warn!(%error, "cannot take the instance lock; running anyway");
            None
        }
    };
    tilo_web_search_app::service::run(cli.workspace)
}

/// The instance lock for `workspace`, held for as long as the process lives;
/// `None` when another instance holds it. Named by workspace because outside
/// a sandbox every workspace shares one state directory.
fn single_instance(workspace: uuid::Uuid) -> Result<Option<Lock>, FsError> {
    let dir = Dir::open(tilo_paths::AppPaths::for_app(APP_ID).state())?;
    let locks = VPath::new("locks").map_err(|_| FsError::InvalidPath("not a lock path"))?;
    tilo_files::blocking::create_dir_all(&dir, &locks)?;
    let path = VPath::new(&format!("locks/{workspace}.lock"))
        .map_err(|_| FsError::InvalidPath("not a lock path"))?;
    tilo_files::blocking::try_lock(&dir, &path, LockMode::Exclusive)
}
