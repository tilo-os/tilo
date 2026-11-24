//! Terminal's environment records, read and written for the device's shell runtime.
//!
//! The shell runtime (`ush`) keeps each environment as one of Terminal's documents in its
//! workspace ([`runtime_portal::workspace_documents`]). It holds no connection to the workspace
//! of its own, so it asks on the MCP socket, proving that it holds the device identity, and this
//! session has the device's daemon carry the command out as Terminal's: the app is fixed here and
//! never the asker's to name, and only the record commands naming a record are carried out. The
//! daemon keeps them and syncs them as it does every app's, offline included.

use std::io::Write as _;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use runtime_portal::workspace_documents::{self, Answer, Request};
use tilo_lock::MutexRecover as _;
use tilo_resource_store::wire::Command;
use tilo_session_client::daemon_documents::DaemonShell;

/// The app whose documents the environments are.
const TERMINAL: &str = tilo_app_id::apps::TERMINAL;

/// Where the session carries out the shell runtime's commands on Terminal's documents.
#[derive(Clone)]
pub(crate) struct EnvironmentRecords {
    /// The device's daemon, which keeps the workspaces and carries each command out as the
    /// device's person.
    daemon: DaemonShell,
    /// The workspace in front of the person, as the loop last saw it: where a command that names
    /// no workspace (the runtime's primary workspace, which the session knows by no id) is
    /// carried out.
    front: Arc<Mutex<Option<String>>>,
}

impl EnvironmentRecords {
    /// Carry the commands out at `daemon`.
    pub(crate) fn new(daemon: DaemonShell) -> Self {
        Self {
            daemon,
            front: Arc::default(),
        }
    }

    /// The workspace in front of the person is `workspace` now.
    pub(crate) fn in_front(&self, workspace: Option<String>) {
        *self.front.lock_recover() = workspace;
    }

    /// Answer `request`, which the runtime asked on `stream`, on a thread of its own.
    pub(crate) fn open(&self, stream: UnixStream, request: Request) {
        let records = self.clone();
        let spawned = std::thread::Builder::new()
            .name("tilo-environment-records".into())
            .spawn(move || {
                tilo_failure::ignored!(
                    "a shell runtime that left before its answer asks again when it runs next",
                    workspace_documents::serve(stream, |command| {
                        records.carry_out(request.workspace.as_deref(), command)
                    })
                    .ok()
                );
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "a command on Terminal's environments could not be served");
        }
    }

    fn carry_out(&self, workspace: Option<&str>, command: &[u8]) -> Result<Vec<u8>, String> {
        let command: Command = serde_json::from_slice(command)
            .map_err(|error| format!("that is no command of the workspace's store: {error}"))?;
        // Terminal's own records, and nothing else of the workspace's.
        if !matches!(
            command,
            Command::RecordChanges {
                record: Some(_),
                ..
            } | Command::RecordChange {
                record: Some(_),
                commit: None,
                ..
            } | Command::Records
        ) {
            return Err("only Terminal's environment records are read and written here".into());
        }
        let workspace = match workspace {
            Some(workspace) => workspace.to_owned(),
            None => self
                .front
                .lock_recover()
                .clone()
                .ok_or("no workspace is in front of the person to keep the environment in")?,
        };
        use runtime_collab::wire::shell::{ShellCall, ShellReply};
        let command = serde_json::to_string(&command).map_err(|error| error.to_string())?;
        // A first write can wait at the daemon for this device's keys of the workspace.
        match self.daemon.ask_blocking(ShellCall::Resources {
            workspace,
            app: TERMINAL.to_owned(),
            command,
        })? {
            ShellReply::Resources(reply) => Ok(reply?.into_bytes()),
            other => Err(format!(
                "the daemon answered a resource command with {other:?}"
            )),
        }
    }
}

/// Tell the asker on `stream` that its command is not carried out, and why.
pub(crate) fn refuse(mut stream: UnixStream, reason: &str) {
    let mut line = serde_json::to_vec(&Answer::Refused {
        reason: reason.to_owned(),
    })
    .unwrap_or_default();
    line.push(b'\n');
    tilo_failure::ignored!(
        "an asker of Terminal's records that left needs no refusal",
        stream.write_all(&line).ok()
    );
}

#[cfg(test)]
mod tests {
    use tilo_resource_store::wire::{Bytes, Command, Reply};

    use super::EnvironmentRecords;

    const WORKSPACE: &str = "5a1b2c3d-0000-4000-8000-00000000e17e";

    fn carried_out(
        records: &EnvironmentRecords,
        workspace: Option<&str>,
        command: &Command,
    ) -> Result<Reply, String> {
        let reply = records.carry_out(workspace, &serde_json::to_vec(command).unwrap())?;
        Ok(serde_json::from_slice(&reply).unwrap())
    }

    /// The shell runtime's commands on Terminal's documents are carried out by the device's
    /// daemon, in the workspace they name or the one in front of the person: what is
    /// written there is read back whole, and nothing but Terminal's own documents is reached.
    #[test]
    fn terminals_environments_are_written_to_and_read_from_the_workspace() {
        let daemon = crate::daemon_fixture::TestDaemon::start();
        daemon.found(
            tilo_desktop_model::WorkspaceId::parse(WORKSPACE).unwrap(),
            "Studio",
        );
        let records = EnvironmentRecords::new(daemon.shell());

        let mut doc = automerge::AutoCommit::new();
        automerge::transaction::Transactable::put(&mut doc, automerge::ROOT, "name", "Dev")
            .unwrap();
        let write = Command::RecordChange {
            record: Some("environment:dev".into()),
            changes: Bytes(doc.save()),
            more: false,
            commit: None,
        };
        let written = carried_out(&records, Some(WORKSPACE), &write);
        assert!(matches!(written, Ok(Reply::Written)), "{written:?}");
        let read = Command::RecordChanges {
            record: Some("environment:dev".into()),
            after: Vec::new(),
            from: 0,
        };
        let Ok(Reply::RecordChanges { changes, .. }) =
            carried_out(&records, Some(WORKSPACE), &read)
        else {
            panic!("the workspace answered no record");
        };
        let read_back = automerge::AutoCommit::load(&changes.0).unwrap();
        assert_eq!(
            automerge::ReadDoc::get(&read_back, automerge::ROOT, "name")
                .unwrap()
                .map(|(value, _)| value.to_string()),
            Some("\"Dev\"".to_owned())
        );
        assert!(matches!(
            carried_out(&records, Some(WORKSPACE), &Command::Records),
            Ok(Reply::Ids(ids)) if ids == ["environment:dev"]
        ));

        // A command naming no workspace is the one in front of the person's, and there is none
        // until the loop says which.
        assert!(carried_out(&records, None, &read).is_err());
        records.in_front(Some(WORKSPACE.to_owned()));
        assert!(matches!(
            carried_out(&records, None, &read),
            Ok(Reply::RecordChanges { changes, .. }) if !changes.0.is_empty()
        ));

        // Terminal's records alone, and no resource of the store.
        let query = Command::Query(tilo_resource_store::Query::of("notes.note", 10));
        assert!(carried_out(&records, Some(WORKSPACE), &query).is_err());
    }
}
