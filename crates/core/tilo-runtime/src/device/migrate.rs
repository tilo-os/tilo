//! This device's migrations: the one migration engine served as the device's own actions,
//! provider `so.tilo.migrate` on `devices/<this device>/migrate` ([`tilo_migrate::provider`]).
//!
//! A migration runs where the data it reads is: this device's folders, browsers and Thunderbird,
//! and the services it connects to. People, their agents, `tilo migrate` and a migration's view
//! call the same actions through the device's one registry of providers. A run lands in the
//! device's workspace as the migration's own agent acting for the person, each run one History
//! entry, through a session this daemon opens for it in its own process; what only the person
//! may do (keeping a mail account and its sign-in) goes through a session of theirs.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Weak};

use anyhow::{Result, anyhow};
use serde_json::Value;
use tilo_actions::remote::Caller;
use tilo_migrate::engine::mcp::{Mcp, Rpc};
use tilo_migrate::engine::state::Place;
use tilo_migrate::engine::writer::Workspace;
use tilo_migrate::provider::{Host, Migrations, PROVIDER, resource};

use super::services::Daemon;
use crate::RuntimeServices as _;

/// The name the migration's agent acts under in the workspace.
const AGENT: &str = "migrate";

impl Daemon {
    /// Serve this device's migrations from now on.
    pub fn serve_migrations(self: &Arc<Self>) {
        let host = DaemonHost {
            daemon: Arc::downgrade(self),
            workspace: self.workspace().to_owned(),
            dir: self.dir(),
        };
        self.provide(
            PROVIDER,
            Arc::new(Migrations::new(
                resource(&self.device_key()),
                Arc::new(host),
            )),
        );
    }
}

/// What this daemon gives its migrations.
struct DaemonHost {
    daemon: Weak<Daemon>,
    workspace: String,
    /// The device's directory: its synced folders' settings, and its migrations.
    dir: PathBuf,
}

impl DaemonHost {
    fn daemon(&self) -> Result<Arc<Daemon>> {
        self.daemon
            .upgrade()
            .ok_or_else(|| anyhow!("this device's daemon is stopping"))
    }
}

impl Host for DaemonHost {
    fn place(&self) -> Place {
        Place::at(&self.scratch())
    }

    fn workspace(&self) -> String {
        self.workspace.clone()
    }

    fn home(&self) -> PathBuf {
        std::env::var_os("HOME").map_or_else(|| self.dir.clone(), PathBuf::from)
    }

    fn folders(&self) -> BTreeMap<String, PathBuf> {
        tilo_api::daemon::folders::Folders::load(&self.dir)
            .map(|folders| {
                folders
                    .workspaces
                    .into_iter()
                    .filter(|(_, folder)| !folder.removed)
                    .map(|(id, folder)| (id, folder.path))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn scratch(&self) -> PathBuf {
        self.dir.join("migrate").join(&self.workspace)
    }

    fn open(&self, _: &Caller) -> Result<Box<dyn Workspace>> {
        let daemon = self.daemon()?;
        let session = (daemon.agent_sessions(AGENT))()?;
        let own = Arc::downgrade(&daemon);
        Ok(Box::new(Mcp::open(
            Box::new(InProcess(session)),
            Box::new(move || {
                let daemon = own
                    .upgrade()
                    .ok_or_else(|| anyhow!("this device's daemon is stopping"))?;
                let (tree, person) = daemon.person_tree()?;
                Ok(
                    Box::new(InProcess(tilo_mcp::McpServer::admitted(tree, person)?))
                        as Box<dyn Rpc>,
                )
            }),
        )?))
    }
}

/// A session of this daemon's, asked in its own process.
struct InProcess<H>(tilo_mcp::McpServer<H>);

impl<H> Rpc for InProcess<H>
where
    H: tilo_actions::remote::ActionProvider + Send + Sync + 'static,
{
    fn exchange(&mut self, request: Value) -> Result<Option<Value>> {
        // A step of a migration runs on a thread of its own, never one of the runtime's.
        tilo_rt::app()
            .block_on(self.0.handle(request))
            .map_err(|error| anyhow!("the session could not be asked: {error}"))
    }
}
